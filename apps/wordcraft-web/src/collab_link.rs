//! Co-editing glue between the page and the replica (`?collab=1`). The page relays messages
//! between this frame and the relay as JSON strings; this turns them into replica calls on the
//! visible session and returns what to send back. No browser APIs here, so it is tested natively.

use wordcraft_collab::proto::{Inbound, Op, Outbound, decode_inbound};
use wordcraft_collab::replica::{CollabError, Replica};
use wordcraft_engine::Session;

pub struct CollabLink {
    replica: Option<Replica>,
    /// This replica's client id (from `Welcome`).
    you: Option<String>,
    /// The first person: waiting for the host to open the document, which was open as this id.
    awaiting: Option<u64>,
    /// The document's file name, for saving (`?docName=`).
    doc_name: Option<String>,
    document_ready: bool,
    save_requested: bool,
}

impl CollabLink {
    pub fn new(doc_name: Option<String>) -> CollabLink {
        CollabLink { replica: None, you: None, awaiting: None, doc_name, document_ready: false, save_requested: false }
    }

    pub fn rev(&self) -> Option<u64> {
        self.replica.as_ref().map(Replica::rev)
    }

    pub fn pending(&self) -> usize {
        self.replica.as_ref().map_or(0, Replica::pending_len)
    }

    /// True once, when the shared document is first shown (the host leaves the start screen).
    pub fn take_document_ready(&mut self) -> bool {
        std::mem::take(&mut self.document_ready)
    }

    /// True once after the relay asked this replica to save.
    pub fn take_save_request(&mut self) -> bool {
        std::mem::take(&mut self.save_requested)
    }

    /// The `Saved` message for the confirmed revision (after the host saved).
    pub fn saved(&self) -> String {
        encode(&Outbound::Saved { rev: self.rev().unwrap_or(0) })
    }

    /// Handle one relay message; returns the messages to send back. Bad messages are logged and
    /// ignored.
    pub fn on_message(&mut self, json: &str, visible: &mut Session) -> Vec<String> {
        let message = match decode_inbound(json) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("co-editing: ignored a relay message: {e}");
                return Vec::new();
            }
        };
        match message {
            Inbound::Welcome { rev, snapshot, ops, you } => self.welcome(rev, snapshot, ops, you, visible),
            Inbound::Op(op) => self.op(*op, visible),
            Inbound::SnapshotRequest => match self.replica.as_ref().map(Replica::snapshot) {
                Some(Ok(s)) => vec![encode(&s)],
                Some(Err(e)) => {
                    log::warn!("co-editing: no snapshot: {e}");
                    Vec::new()
                }
                None => Vec::new(),
            },
            Inbound::Resync { rev, snapshot, ops } => self.resume(rev, Some(snapshot), ops, visible),
            Inbound::Save => {
                self.save_requested = self.replica.is_some();
                Vec::new()
            }
        }
    }

    /// After the person's edits this frame: the next edit to send and a fingerprint when due;
    /// for the first person, the opened document as revision 0.
    pub fn tick(&mut self, visible: &mut Session) -> Vec<String> {
        if let Some(blank) = self.awaiting {
            if visible.document_id() == blank {
                return Vec::new();
            }
            self.awaiting = None;
            let you = self.you.clone().unwrap_or_default();
            match Replica::join(you, 0, visible.doc.clone(), Vec::new()) {
                Ok(r) => {
                    visible.set_collab(true);
                    self.document_ready = true;
                    let snapshot = r.snapshot();
                    self.replica = Some(r);
                    return snapshot.map(|s| vec![encode(&s)]).unwrap_or_default();
                }
                Err(e) => log::warn!("co-editing: couldn't start: {e}"),
            }
        }
        let Some(replica) = self.replica.as_mut() else { return Vec::new() };
        replica.drain_local(visible);
        let mut out = Vec::new();
        if let Some(op) = replica.next_to_send() {
            out.push(encode(&Outbound::Op(Box::new(op))));
        }
        if let Some(h) = replica.hash_due() {
            out.push(encode(&h));
        }
        out
    }

    fn welcome(&mut self, rev: u64, snapshot: Option<String>, ops: Vec<Op>, you: String, visible: &mut Session) -> Vec<String> {
        self.you = Some(you.clone());
        if self.replica.is_some() {
            // A reconnect: catch up and send what the relay didn't get.
            return self.resume(rev, snapshot, ops, visible);
        }
        let Some(json) = snapshot else {
            // The first person: the host opens the document, which becomes revision 0.
            self.awaiting = Some(visible.document_id());
            return Vec::new();
        };
        let doc = match wordcraft_engine::io::open_bytes("shared.json", json.as_bytes()) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("co-editing: bad snapshot: {e}");
                return Vec::new();
            }
        };
        match Replica::join(you, rev, doc, ops) {
            Ok(r) => {
                visible.set_document(r.confirmed().clone());
                visible.path = self.doc_name.as_ref().map(std::path::PathBuf::from);
                visible.dirty = false;
                r.attach(visible);
                self.replica = Some(r);
                self.document_ready = true;
                Vec::new()
            }
            Err(e) => {
                log::warn!("co-editing: couldn't join: {e}");
                vec![encode(&Outbound::ResyncRequest { have: rev })]
            }
        }
    }

    fn op(&mut self, op: Op, visible: &mut Session) -> Vec<String> {
        let Some(replica) = self.replica.as_mut() else { return Vec::new() };
        match replica.receive(op, visible) {
            Ok(_) => Vec::new(),
            Err(CollabError::Gap { have, .. }) | Err(CollabError::TooOld { oldest: have, .. }) => {
                vec![encode(&Outbound::ResyncRequest { have })]
            }
            Err(e) => {
                log::warn!("co-editing: {e}");
                vec![encode(&Outbound::ResyncRequest { have: replica.rev() })]
            }
        }
    }

    fn resume(&mut self, rev: u64, snapshot: Option<String>, ops: Vec<Op>, visible: &mut Session) -> Vec<String> {
        let Some(replica) = self.replica.as_mut() else { return Vec::new() };
        let doc = match snapshot.map(|json| wordcraft_engine::io::open_bytes("shared.json", json.as_bytes())).transpose() {
            Ok(d) => d,
            Err(e) => {
                log::warn!("co-editing: bad snapshot: {e}");
                return Vec::new();
            }
        };
        match replica.resume(rev, doc, ops, visible) {
            Ok(resend) => resend.into_iter().map(|op| encode(&Outbound::Op(Box::new(op)))).collect(),
            Err(e) => {
                log::warn!("co-editing: couldn't catch up: {e}");
                vec![encode(&Outbound::ResyncRequest { have: replica.rev() })]
            }
        }
    }
}

fn encode(m: &Outbound) -> String {
    serde_json::to_string(m).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wordcraft_doc::{Document, StoryRef};
    use wordcraft_engine::Session;

    use super::*;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn snapshot_of(text: &str) -> String {
        let mut s = Session::new(Document::new());
        s.run("text.insert", &json!({"text": text})).unwrap();
        String::from_utf8(wordcraft_engine::io::save_bytes("d.json", &s.doc).unwrap()).unwrap()
    }

    fn welcome(rev: u64, snapshot: Option<String>, you: &str) -> String {
        json!({"t": "Welcome", "rev": rev, "snapshot": snapshot, "ops": [], "you": you}).to_string()
    }

    #[test]
    fn first_person_uploads_the_opened_document_as_revision_0() {
        let mut link = CollabLink::new(Some("report.docx".into()));
        let mut s = Session::new(Document::new());
        assert!(link.on_message(&welcome(0, None, "c1"), &mut s).is_empty());
        assert!(link.tick(&mut s).is_empty(), "waits for the document");
        // The host opens the file.
        let mut opened = Session::new(Document::new());
        opened.run("text.insert", &json!({"text": "From file"})).unwrap();
        s.set_document(opened.doc.clone());
        let out = link.tick(&mut s);
        assert_eq!(out.len(), 1);
        let v = parse(&out[0]);
        assert_eq!((v["t"].as_str(), v["rev"].as_u64()), (Some("Snapshot"), Some(0)));
        assert!(s.collab());
        assert!(link.take_document_ready());
    }

    #[test]
    fn joiner_shows_the_snapshot_named_as_the_document() {
        let mut link = CollabLink::new(Some("report.docx".into()));
        let mut s = Session::new(Document::new());
        let id = s.document_id();
        link.on_message(&welcome(4, Some(snapshot_of("Shared")), "c2"), &mut s);
        assert_eq!(s.doc.plain_text(StoryRef::Body), "Shared");
        assert_ne!(s.document_id(), id, "a real document, not the start screen's blank");
        assert_eq!(s.path.as_deref(), Some(std::path::Path::new("report.docx")));
        assert!(s.collab());
        assert!(link.take_document_ready());
        assert_eq!(link.rev(), Some(4));
    }

    #[test]
    fn local_edits_go_out_one_at_a_time_and_echoes_release_the_next() {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        link.on_message(&welcome(0, Some(snapshot_of("x")), "c1"), &mut s);
        s.run("caret.docEnd", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": " "})).unwrap();
        s.run("text.insert", &json!({"text": "."})).unwrap();
        let out = link.tick(&mut s);
        assert_eq!(out.len(), 1);
        assert_eq!(link.pending(), 2);
        let mut op = parse(&out[0]);
        assert_eq!(op["t"], "Op");
        assert!(link.tick(&mut s).is_empty(), "one in flight");
        op["rev"] = json!(1);
        link.on_message(&op.to_string(), &mut s);
        let next = link.tick(&mut s);
        assert_eq!(parse(&next[0])["params"]["text"], ".");
    }

    #[test]
    fn a_gap_asks_for_a_resync() {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        link.on_message(&welcome(0, Some(snapshot_of("x")), "c1"), &mut s);
        let op = json!({"t": "Op", "client": "c9", "seq": 1, "base": 0, "kind": "cmd", "cmd": "text.insert", "params": {"text": "z"},
            "sel": {"story": "body", "anchor": 0, "head": 0}, "ctx": {}, "rev": 3});
        let out = link.on_message(&op.to_string(), &mut s);
        assert_eq!(parse(&out[0]), json!({"t": "ResyncRequest", "have": 0}));
    }

    #[test]
    fn snapshot_request_and_save_request() {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        link.on_message(&welcome(0, Some(snapshot_of("x")), "c1"), &mut s);
        let out = link.on_message(r#"{"t":"SnapshotRequest"}"#, &mut s);
        assert_eq!(parse(&out[0])["t"], "Snapshot");
        assert!(!link.take_save_request());
        assert!(link.on_message(r#"{"t":"Save"}"#, &mut s).is_empty());
        assert!(link.take_save_request());
        assert!(!link.take_save_request());
        assert_eq!(parse(&link.saved()), json!({"t": "Saved", "rev": 0}));
    }

    #[test]
    fn bad_and_oversized_messages_are_ignored() {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        assert!(link.on_message("nonsense", &mut s).is_empty());
        let big = json!({"t": "Op", "params": "x".repeat(300 * 1024)}).to_string();
        assert!(link.on_message(&big, &mut s).is_empty());
        assert_eq!(link.rev(), None);
    }

    #[test]
    fn reconnect_welcome_resumes_and_resends_pending() {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        link.on_message(&welcome(0, Some(snapshot_of("x")), "c1"), &mut s);
        s.run("caret.docEnd", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "!"})).unwrap();
        let typed = s.doc.plain_text(StoryRef::Body);
        assert_eq!(link.tick(&mut s).len(), 1);
        // The connection drops before the echo; the relay never got it.
        let out = link.on_message(&welcome(0, None, "c1"), &mut s);
        assert_eq!(out.len(), 1);
        assert_eq!(parse(&out[0])["params"]["text"], "!");
        assert_eq!(s.doc.plain_text(StoryRef::Body), typed);
    }
}
