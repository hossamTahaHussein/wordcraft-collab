//! Co-editing glue between the page and the replica (`?collab=1`). The page relays messages
//! between this frame and the relay as JSON strings; this turns them into replica calls on the
//! visible session and returns what to send back. No browser APIs here, so it is tested natively.

use std::collections::BTreeMap;

use wordcraft_collab::mapping::{changes, map};
use wordcraft_collab::proto::{Anchor, HistEntry, Inbound, Op, Outbound, Peer, decode_inbound};
use wordcraft_collab::replica::{Applied, CollabError, Replica};
use wordcraft_doc::Document;
use wordcraft_doc::offsets::{offset_of, pos_at};
use wordcraft_engine::{Selection, Session};

/// How often a replica may say where its selection is.
const PRESENCE_EVERY_MS: f64 = 100.0;
/// How long a peer counts as typing after its last edit.
const TYPING_MS: f64 = 3000.0;

/// Someone else in the document, as the UI shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerView {
    pub client: String,
    pub name: String,
    pub color: u8,
    pub sel: Option<Selection>,
    pub typed_at: Option<f64>,
}

impl PeerView {
    /// Whether they edited in the last three seconds (their name flag shows).
    pub fn typing(&self, now_ms: f64) -> bool {
        self.typed_at.is_some_and(|t| now_ms - t < TYPING_MS)
    }
}

struct PeerState {
    name: String,
    color: u8,
    /// Where they are, in the visible document's offsets.
    anchor: Option<Anchor>,
    /// Just placed in the current visible document: the next tick doesn't move it again.
    fresh: bool,
    /// Where they said they are, at a revision this replica hasn't reached yet.
    waiting: Option<(Anchor, u64)>,
    typed_at: Option<f64>,
}

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
    /// A message for the person (an edit of theirs couldn't be applied).
    note: Option<String>,
    /// The others here, by client id.
    peers: BTreeMap<String, PeerState>,
    /// The visible document as the peers' positions last followed it (its revision, a copy).
    followed: Option<(u64, Document)>,
    /// This replica's selection as last sent, and when.
    sent: Option<(Anchor, f64)>,
    /// The selection moved but wasn't sent yet (too soon after the last one).
    presence_waiting: bool,
    /// The time of the last tick, for marking who typed.
    now: f64,
}

impl CollabLink {
    pub fn new(doc_name: Option<String>) -> CollabLink {
        CollabLink {
            replica: None,
            you: None,
            awaiting: None,
            doc_name,
            document_ready: false,
            save_requested: false,
            note: None,
            peers: BTreeMap::new(),
            followed: None,
            sent: None,
            presence_waiting: false,
            now: 0.0,
        }
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

    /// A message for the status bar, once.
    pub fn take_note(&mut self) -> Option<String> {
        self.note.take()
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
                // An edit nobody can read still takes its revision: skip it, as every replica does.
                if let Some(rev) = unreadable_op_rev(json)
                    && let Some(replica) = self.replica.as_mut()
                    && rev == replica.rev() + 1
                    && replica.skip(rev).is_ok()
                {
                    return Vec::new();
                }
                return Vec::new();
            }
        };
        match message {
            Inbound::Welcome { rev, snapshot, ops, you, seen, history, peers } => {
                // The relay's list replaces ours (a reconnect may have missed joins and leaves).
                self.peers.clear();
                let welcome = self.welcome(rev, snapshot, ops, you, seen, history, visible);
                for peer in peers {
                    self.note_peer(peer, visible);
                }
                welcome
            }
            Inbound::Op(op) => self.op(*op, visible),
            Inbound::SnapshotRequest => match self.replica.as_ref().map(Replica::snapshot) {
                Some(Ok(s)) => vec![encode(&s)],
                Some(Err(e)) => {
                    log::warn!("co-editing: no snapshot: {e}");
                    Vec::new()
                }
                None => Vec::new(),
            },
            Inbound::Resync { rev, snapshot, ops, seen, history } => {
                if self.replica.is_none() {
                    // It couldn't join from its Welcome: start from this snapshot instead.
                    let you = self.you.clone().unwrap_or_default();
                    return self.join(rev, snapshot, history, ops, you, visible);
                }
                self.resume(rev, Some(snapshot), ops, seen, history, visible)
            }
            Inbound::Save => {
                self.save_requested = self.replica.is_some();
                Vec::new()
            }
            Inbound::Join(peer) | Inbound::Presence(peer) => {
                self.note_peer(peer, visible);
                Vec::new()
            }
            Inbound::Leave { client } => {
                self.peers.remove(&client);
                Vec::new()
            }
        }
    }

    /// After the person's edits this frame: the next edit to send and a fingerprint when due;
    /// for the first person, the opened document as revision 0.
    pub fn tick(&mut self, visible: &mut Session, now_ms: f64) -> Vec<String> {
        self.now = now_ms;
        self.follow_changes(visible);
        self.place_waiting(visible);
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
        if let Some(p) = self.presence_due(visible, now_ms) {
            out.push(p);
        }
        out
    }

    /// The others here, their selections in the visible document.
    pub fn peers(&self, visible: &Session) -> Vec<PeerView> {
        self.peers
            .iter()
            .map(|(client, p)| PeerView {
                client: client.clone(),
                name: p.name.clone(),
                color: p.color,
                sel: p
                    .anchor
                    .as_ref()
                    .map(|a| Selection { anchor: pos_at(&visible.doc, a.story, a.anchor), focus: pos_at(&visible.doc, a.story, a.head) }),
                typed_at: p.typed_at,
            })
            .collect()
    }

    /// The selection moved but wasn't sent yet: the host should tick again shortly, even
    /// without input.
    pub fn presence_pending(&self) -> bool {
        self.presence_waiting
    }

    fn note_peer(&mut self, peer: Peer, visible: &Session) {
        let entry = self.peers.entry(peer.client.clone()).or_insert(PeerState {
            name: String::new(),
            color: 0,
            anchor: None,
            fresh: false,
            waiting: None,
            typed_at: None,
        });
        entry.name = peer.user;
        entry.color = peer.color;
        if let Some(sel) = peer.sel {
            entry.waiting = Some((sel, peer.rev.unwrap_or(0)));
        }
        self.place_waiting(visible);
    }

    /// Place the positions people sent, once this replica has the revision each is relative to:
    /// moved through the edits since, then through this person's own unconfirmed edits.
    fn place_waiting(&mut self, visible: &Session) {
        let Some(replica) = self.replica.as_ref() else { return };
        let mine = changes(replica.confirmed(), &visible.doc, None);
        for p in self.peers.values_mut() {
            let Some((sel, rev)) = p.waiting.clone() else { continue };
            if rev > replica.rev() {
                continue;
            }
            p.waiting = None;
            if let Some(a) = replica.rebase_anchor(&sel, rev) {
                p.anchor = Some(move_through(&a, &mine));
                p.fresh = true;
            }
        }
    }

    /// Move the peers' positions through whatever changed in the visible document since the last
    /// tick: someone's edit, or this person's own typing.
    fn follow_changes(&mut self, visible: &Session) {
        let rev = visible.rev();
        if let Some((seen, before)) = &self.followed {
            if *seen == rev {
                return;
            }
            let spans = changes(before, &visible.doc, None);
            for p in self.peers.values_mut() {
                // A position placed since the last tick is already in the current document.
                if std::mem::take(&mut p.fresh) {
                    continue;
                }
                if let Some(a) = p.anchor.as_mut() {
                    *a = move_through(a, &spans);
                }
            }
        }
        for p in self.peers.values_mut() {
            p.fresh = false;
        }
        self.followed = Some((rev, visible.doc.clone()));
    }

    /// This person's selection for the relay, when it moved and the last one is 100 ms old.
    fn presence_due(&mut self, visible: &Session, now_ms: f64) -> Option<String> {
        let sel = &visible.sel;
        if sel.anchor.story != sel.focus.story {
            return None;
        }
        // With edits unconfirmed the visible offsets aren't the confirmed revision's: wait.
        let replica = self.replica.as_ref()?;
        if replica.pending_len() > 0 {
            self.presence_waiting = true;
            return None;
        }
        let rev = replica.rev();
        let here = Anchor { story: sel.anchor.story, anchor: offset_of(&visible.doc, &sel.anchor)?, head: offset_of(&visible.doc, &sel.focus)? };
        self.presence_waiting = false;
        if self.sent.as_ref().is_some_and(|(last, _)| *last == here) {
            return None;
        }
        if self.sent.as_ref().is_some_and(|(_, at)| now_ms - at < PRESENCE_EVERY_MS) {
            self.presence_waiting = true;
            return None;
        }
        self.sent = Some((here.clone(), now_ms));
        Some(encode(&Outbound::Presence { sel: here, rev }))
    }

    #[allow(clippy::too_many_arguments)]
    fn welcome(
        &mut self,
        rev: u64,
        snapshot: Option<String>,
        ops: Vec<Op>,
        you: String,
        seen: Option<u64>,
        history: Vec<HistEntry>,
        visible: &mut Session,
    ) -> Vec<String> {
        self.you = Some(you.clone());
        if self.replica.is_some() {
            // A reconnect: catch up and send what the relay didn't get.
            return self.resume(rev, snapshot, ops, seen, history, visible);
        }
        let Some(json) = snapshot else {
            // The first person: the host opens the document, which becomes revision 0.
            self.awaiting = Some(visible.document_id());
            return Vec::new();
        };
        self.join(rev, json, history, ops, you, visible)
    }

    /// Start the replica from a snapshot and show it; on failure ask for a resync.
    fn join(&mut self, rev: u64, json: String, history: Vec<HistEntry>, ops: Vec<Op>, you: String, visible: &mut Session) -> Vec<String> {
        let doc = match wordcraft_engine::io::open_bytes("shared.json", json.as_bytes()) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("co-editing: bad snapshot: {e}");
                return vec![encode(&Outbound::ResyncRequest { have: rev })];
            }
        };
        match Replica::join_with_history(you, rev, doc, history, ops) {
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
        let out = self.apply_op(op, visible);
        // The author's caret is where their edit left it.
        if let Some((client, a)) = self.replica.as_mut().and_then(|r| r.take_author_selection())
            && let Some(replica) = self.replica.as_ref()
        {
            let mine = changes(replica.confirmed(), &visible.doc, None);
            if let Some(p) = self.peers.get_mut(&client) {
                p.anchor = Some(move_through(&a, &mine));
                p.fresh = true;
                p.waiting = None;
                p.typed_at = Some(self.now);
            }
        }
        out
    }

    fn apply_op(&mut self, op: Op, visible: &mut Session) -> Vec<String> {
        let Some(replica) = self.replica.as_mut() else { return Vec::new() };
        match replica.receive(op, visible) {
            Ok(Applied::Remote { dropped_pending }) if dropped_pending > 0 => {
                self.note = Some(dropped_note(dropped_pending));
                Vec::new()
            }
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

    fn resume(
        &mut self,
        rev: u64,
        snapshot: Option<String>,
        ops: Vec<Op>,
        seen: Option<u64>,
        history: Vec<HistEntry>,
        visible: &mut Session,
    ) -> Vec<String> {
        let Some(replica) = self.replica.as_mut() else { return Vec::new() };
        let doc = match snapshot.map(|json| wordcraft_engine::io::open_bytes("shared.json", json.as_bytes())).transpose() {
            Ok(d) => d,
            Err(e) => {
                log::warn!("co-editing: bad snapshot: {e}");
                return Vec::new();
            }
        };
        match replica.resume(rev, doc.map(|d| (d, history)), ops, seen, visible) {
            Ok(resend) => resend.into_iter().map(|op| encode(&Outbound::Op(Box::new(op)))).collect(),
            Err(e) => {
                log::warn!("co-editing: couldn't catch up: {e}");
                vec![encode(&Outbound::ResyncRequest { have: replica.rev() })]
            }
        }
    }
}

/// `a` moved through `spans` (someone else's changes: a position at a change stays before it).
fn move_through(a: &Anchor, spans: &[(wordcraft_doc::StoryRef, wordcraft_collab::mapping::Span)]) -> Anchor {
    let mut out = a.clone();
    for (story, s) in spans {
        if *story == out.story {
            out.anchor = map(s, out.anchor, false);
            out.head = map(s, out.head, false);
        }
    }
    out
}

fn dropped_note(n: usize) -> String {
    if n == 1 {
        "Your last edit couldn't be applied after someone else's change.".to_string()
    } else {
        format!("{n} of your edits couldn't be applied after someone else's change.")
    }
}

/// The revision of a relay op that couldn't be decoded, if it has one.
fn unreadable_op_rev(json: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    (v.get("t")?.as_str()? == "Op").then(|| v.get("rev")?.as_u64())?
}

fn encode(m: &Outbound) -> String {
    serde_json::to_string(m).unwrap_or_default()
}

/// Test helper: the messages from a tick other than presence.
#[cfg(test)]
fn edits_only(out: Vec<String>) -> Vec<String> {
    out.into_iter().filter(|m| !m.contains(r#""t":"Presence""#)).collect()
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
        assert!(super::edits_only(link.tick(&mut s, 0.0)).is_empty(), "waits for the document");
        // The host opens the file.
        let mut opened = Session::new(Document::new());
        opened.run("text.insert", &json!({"text": "From file"})).unwrap();
        s.set_document(opened.doc.clone());
        let out = super::edits_only(link.tick(&mut s, 0.0));
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
        let out = super::edits_only(link.tick(&mut s, 0.0));
        assert_eq!(out.len(), 1);
        assert_eq!(link.pending(), 2);
        let mut op = parse(&out[0]);
        assert_eq!(op["t"], "Op");
        assert!(super::edits_only(link.tick(&mut s, 0.0)).is_empty(), "one in flight");
        op["rev"] = json!(1);
        link.on_message(&op.to_string(), &mut s);
        let next = super::edits_only(link.tick(&mut s, 0.0));
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
        assert_eq!(super::edits_only(link.tick(&mut s, 0.0)).len(), 1);
        // The connection drops before the echo; the relay never got it.
        let out = link.on_message(&welcome(0, None, "c1"), &mut s);
        assert_eq!(out.len(), 1);
        assert_eq!(parse(&out[0])["params"]["text"], "!");
        assert_eq!(s.doc.plain_text(StoryRef::Body), typed);
    }
}

#[cfg(test)]
mod seen_tests {
    use serde_json::json;
    use wordcraft_doc::{Document, StoryRef};
    use wordcraft_engine::Session;

    use super::CollabLink;

    #[test]
    fn a_resync_snapshot_holding_an_edit_does_not_resend_it() {
        let mut s0 = Session::new(Document::new());
        s0.run("text.insert", &json!({"text": "ab"})).unwrap();
        let snap = |s: &Session| String::from_utf8(wordcraft_engine::io::save_bytes("d.json", &s.doc).unwrap()).unwrap();
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        link.on_message(&json!({"t": "Welcome", "rev": 0, "snapshot": snap(&s0), "ops": [], "you": "c1"}).to_string(), &mut s);
        s.run("caret.docEnd", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "X"})).unwrap();
        assert_eq!(super::edits_only(link.tick(&mut s, 0.0)).len(), 1);
        // The relay took it into a snapshot (seen = 1) before this replica heard back.
        let mut s1 = Session::new(Document::new());
        s1.run("text.insert", &json!({"text": "abX"})).unwrap();
        let out = link.on_message(&json!({"t": "Resync", "rev": 1, "snapshot": snap(&s1), "ops": [], "seen": 1}).to_string(), &mut s);
        assert!(out.is_empty(), "nothing to resend: {out:?}");
        assert_eq!(s.doc.plain_text(StoryRef::Body), "abX");
    }
}

#[cfg(test)]
mod recovery_tests {
    use serde_json::json;
    use wordcraft_doc::{Document, StoryRef};
    use wordcraft_engine::Session;

    use super::CollabLink;

    fn snap(text: &str) -> String {
        let mut s = Session::new(Document::new());
        s.run("text.insert", &json!({"text": text})).unwrap();
        String::from_utf8(wordcraft_engine::io::save_bytes("d.json", &s.doc).unwrap()).unwrap()
    }

    /// A joiner whose Welcome couldn't be used asks for a Resync, and the Resync then
    /// starts it (there's no replica yet).
    #[test]
    fn a_resync_starts_a_replica_that_could_not_join() {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        link.on_message(&json!({"t": "Welcome", "rev": 0, "snapshot": "not a document", "ops": [], "you": "c1"}).to_string(), &mut s);
        assert_eq!(link.rev(), None);
        link.on_message(&json!({"t": "Resync", "rev": 2, "snapshot": snap("Shared"), "ops": []}).to_string(), &mut s);
        assert_eq!(link.rev(), Some(2));
        assert_eq!(s.doc.plain_text(StoryRef::Body), "Shared");
        assert!(link.take_document_ready());
    }

    /// An op no replica can decode, at the next revision, is skipped.
    #[test]
    fn an_unreadable_op_at_the_next_revision_is_skipped() {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        link.on_message(&json!({"t": "Welcome", "rev": 0, "snapshot": snap("x"), "ops": [], "you": "c1"}).to_string(), &mut s);
        let out = link.on_message(&json!({"t": "Op", "rev": 1, "sel": "garbage"}).to_string(), &mut s);
        assert!(out.is_empty(), "{out:?}");
        assert_eq!(link.rev(), Some(1));
    }

    /// An edit dropped after someone else's change is reported.
    #[test]
    fn a_dropped_edit_leaves_a_note() {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        link.on_message(&json!({"t": "Welcome", "rev": 0, "snapshot": snap("Intro"), "ops": [], "you": "c1"}).to_string(), &mut s);
        s.run("caret.docEnd", &json!({})).unwrap();
        s.run("text.newParagraph", &json!({})).unwrap();
        s.run("insert.table", &json!({"rows": 2, "cols": 2})).unwrap();
        // Simulate: this replica's two edits are confirmed, then its row edit waits while
        // someone else's delete of everything arrives.
        let mut rev = 0;
        for _ in 0..2 {
            let out = super::edits_only(link.tick(&mut s, 0.0));
            let mut op: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
            rev += 1;
            op["rev"] = json!(rev);
            link.on_message(&op.to_string(), &mut s);
        }
        s.run("table.insertRowBelow", &json!({})).unwrap();
        // Someone else deletes everything (the row edit hasn't been sent yet).
        let other = json!({"t": "Op", "client": "c9", "seq": 1, "base": rev, "kind": "cmd", "cmd": "text.delete", "params": {},
            "sel": {"story": "body", "anchor": 0, "head": 100000}, "ctx": {}, "rev": rev + 1});
        link.on_message(&other.to_string(), &mut s);
        let note = link.take_note().expect("a note");
        assert!(note.contains("couldn't be applied"), "{note}");
    }
}

#[cfg(test)]
mod presence_tests {
    use serde_json::{Value, json};
    use wordcraft_doc::{Document, StoryRef};
    use wordcraft_engine::Session;

    use super::CollabLink;

    fn snap(text: &str) -> String {
        let mut s = Session::new(Document::new());
        s.run("text.insert", &json!({"text": text})).unwrap();
        String::from_utf8(wordcraft_engine::io::save_bytes("d.json", &s.doc).unwrap()).unwrap()
    }

    fn joined(text: &str, peers: Value) -> (CollabLink, Session) {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        link.on_message(&json!({"t": "Welcome", "rev": 0, "snapshot": snap(text), "ops": [], "you": "me", "peers": peers}).to_string(), &mut s);
        link.tick(&mut s, 0.0);
        (link, s)
    }

    fn presence(client: &str, user: &str, anchor: usize) -> String {
        json!({"t": "Presence", "client": client, "user": user, "color": 3, "sel": {"story": "body", "anchor": anchor, "head": anchor}}).to_string()
    }

    fn caret_off(link: &CollabLink, s: &Session, client: &str) -> usize {
        link.peers(s).into_iter().find(|p| p.client == client).and_then(|p| p.sel).map(|sel| sel.focus.off).unwrap()
    }

    #[test]
    fn join_presence_leave_update_the_book() {
        let (mut link, mut s) = joined("Hello world", json!([{"client": "a", "user": "Ann", "color": 0}]));
        assert_eq!(link.peers(&s).len(), 1);
        assert_eq!(link.peers(&s)[0].name, "Ann");
        link.on_message(&json!({"t": "Join", "client": "b", "user": "Bob", "color": 1}).to_string(), &mut s);
        link.on_message(&presence("b", "Bob", 6), &mut s);
        assert_eq!(caret_off(&link, &s, "b"), 6);
        link.on_message(&json!({"t": "Leave", "client": "a"}).to_string(), &mut s);
        let names: Vec<String> = link.peers(&s).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Bob"]);
    }

    #[test]
    fn presence_sent_only_when_selection_changes_and_at_most_ten_per_second() {
        let (mut link, mut s) = joined("Hello world", json!([]));
        let sent = |out: &[String]| out.iter().filter(|m| m.contains(r#""t":"Presence""#)).count();
        s.run("caret.docEnd", &json!({})).unwrap();
        assert_eq!(sent(&link.tick(&mut s, 1000.0)), 1);
        assert_eq!(sent(&link.tick(&mut s, 1500.0)), 0, "unchanged: nothing sent");
        s.run("caret.left", &json!({})).unwrap();
        assert_eq!(sent(&link.tick(&mut s, 1550.0)), 1, "moved, and the last one was 550 ms ago");
        s.run("caret.left", &json!({})).unwrap();
        assert_eq!(sent(&link.tick(&mut s, 1600.0)), 0, "within 100 ms of the last one");
        assert!(link.presence_pending());
        assert_eq!(sent(&link.tick(&mut s, 1651.0)), 1);
        assert!(!link.presence_pending());
    }

    #[test]
    fn peer_caret_moves_with_text_typed_before_it() {
        let (mut link, mut s) =
            joined("Hello world", json!([{"client": "a", "user": "Ann", "color": 0, "sel": {"story": "body", "anchor": 6, "head": 6}}]));
        s.run("caret.docStart", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "Hi "})).unwrap();
        link.tick(&mut s, 10.0);
        assert_eq!(caret_off(&link, &s, "a"), 9);
    }

    #[test]
    fn peer_caret_survives_deletion_around_it() {
        let (mut link, mut s) =
            joined("Hello big world", json!([{"client": "a", "user": "Ann", "color": 0, "sel": {"story": "body", "anchor": 8, "head": 8}}]));
        s.run("select.text", &json!({"text": "big "})).unwrap();
        s.run("text.delete", &json!({})).unwrap();
        link.tick(&mut s, 10.0);
        assert_eq!(caret_off(&link, &s, "a"), 6);
        assert_eq!(s.doc.plain_text(StoryRef::Body), "Hello world");
    }

    #[test]
    fn a_peer_typing_is_marked_for_three_seconds() {
        let (mut link, mut s) = joined("x", json!([{"client": "a", "user": "Ann", "color": 0}]));
        let op = json!({"t": "Op", "client": "a", "seq": 1, "base": 0, "kind": "cmd", "cmd": "text.insert", "params": {"text": "y"},
            "sel": {"story": "body", "anchor": 1, "head": 1}, "ctx": {}, "rev": 1});
        link.tick(&mut s, 5000.0);
        link.on_message(&op.to_string(), &mut s);
        link.tick(&mut s, 5001.0);
        assert!(link.peers(&s)[0].typing(6000.0));
        assert!(!link.peers(&s)[0].typing(8100.0));
    }

    #[test]
    fn peers_survive_a_reconnect_welcome() {
        let (mut link, mut s) = joined("x", json!([{"client": "a", "user": "Ann", "color": 0}]));
        link.on_message(
            &json!({"t": "Welcome", "rev": 0, "snapshot": null, "ops": [], "you": "me", "peers": [{"client": "b", "user": "Bob", "color": 1}]})
                .to_string(),
            &mut s,
        );
        let names: Vec<String> = link.peers(&s).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Bob"], "the reconnect's list replaces the old one");
    }
}

#[cfg(test)]
mod presence_order_tests {
    use serde_json::json;
    use wordcraft_doc::Document;
    use wordcraft_engine::Session;

    use super::CollabLink;

    fn snap(text: &str) -> String {
        let mut s = Session::new(Document::new());
        s.run("text.insert", &json!({"text": text})).unwrap();
        String::from_utf8(wordcraft_engine::io::save_bytes("d.json", &s.doc).unwrap()).unwrap()
    }

    fn joined(text: &str) -> (CollabLink, Session) {
        let mut link = CollabLink::new(None);
        let mut s = Session::new(Document::new());
        let peers = json!([{"client": "a", "user": "Ann", "color": 0}]);
        link.on_message(&json!({"t": "Welcome", "rev": 0, "snapshot": snap(text), "ops": [], "you": "me", "peers": peers}).to_string(), &mut s);
        link.tick(&mut s, 0.0);
        (link, s)
    }

    fn insert_op(rev: u64, at: usize, text: &str) -> String {
        json!({"t": "Op", "client": "a", "seq": rev, "base": rev - 1, "kind": "cmd", "cmd": "text.insert", "params": {"text": text},
            "sel": {"story": "body", "anchor": at, "head": at}, "ctx": {}, "rev": rev})
        .to_string()
    }

    fn ann(link: &CollabLink, s: &Session) -> usize {
        link.peers(s)[0].sel.as_ref().map(|sel| sel.focus.off).unwrap()
    }

    #[test]
    fn a_typing_peer_caret_follows_their_typing() {
        let (mut link, mut s) = joined("Hello");
        link.on_message(&insert_op(1, 5, " there"), &mut s);
        link.tick(&mut s, 10.0);
        assert_eq!(ann(&link, &s), 11, "after what Ann typed");
    }

    #[test]
    fn presence_waits_for_the_edits_it_follows() {
        let (mut link, mut s) = joined("Hello");
        // Ann typed " there" (revision 1) and says she's after it, but her edit hasn't arrived yet.
        link.on_message(
            &json!({"t": "Presence", "client": "a", "user": "Ann", "color": 0, "rev": 1, "sel": {"story": "body", "anchor": 11, "head": 11}})
                .to_string(),
            &mut s,
        );
        link.tick(&mut s, 10.0);
        link.on_message(&insert_op(1, 5, " there"), &mut s);
        link.tick(&mut s, 20.0);
        assert_eq!(ann(&link, &s), 11, "placed once, not shifted again by her own edit");
    }

    #[test]
    fn presence_from_an_older_revision_moves_through_later_edits() {
        let (mut link, mut s) = joined("Hello world");
        // Someone else's edit (revision 1) puts "Big " at the start; Ann said where she was before it.
        let other = json!({"t": "Op", "client": "b", "seq": 1, "base": 0, "kind": "cmd", "cmd": "text.insert", "params": {"text": "Big "},
            "sel": {"story": "body", "anchor": 0, "head": 0}, "ctx": {}, "rev": 1});
        link.on_message(&other.to_string(), &mut s);
        link.tick(&mut s, 10.0);
        link.on_message(
            &json!({"t": "Presence", "client": "a", "user": "Ann", "color": 0, "rev": 0, "sel": {"story": "body", "anchor": 6, "head": 6}})
                .to_string(),
            &mut s,
        );
        link.tick(&mut s, 20.0);
        assert_eq!(ann(&link, &s), 10);
    }

    #[test]
    fn presence_is_not_sent_while_edits_are_unconfirmed() {
        let (mut link, mut s) = joined("Hello");
        let presence = |out: &[String]| out.iter().find(|m| m.contains(r#""t":"Presence""#)).cloned();
        s.run("caret.docEnd", &json!({})).unwrap();
        s.run("text.insert", &json!({"text": "!"})).unwrap();
        let out = link.tick(&mut s, 1000.0);
        assert!(presence(&out).is_none(), "an edit is in flight");
        let mut op: serde_json::Value = serde_json::from_str(out.iter().find(|m| m.contains(r#""t":"Op""#)).unwrap()).unwrap();
        op["rev"] = json!(1);
        link.on_message(&op.to_string(), &mut s);
        let p = presence(&link.tick(&mut s, 1200.0)).expect("sent once confirmed");
        let v: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(v["rev"], 1);
    }
}
