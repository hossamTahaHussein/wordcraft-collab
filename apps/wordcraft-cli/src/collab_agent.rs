//! Agents join a co-edited document (integrations part A): `wordcraft-cli collab-mcp --relay URL
//! --token TOKEN` is an MCP server whose session is a participant in a shared document. It runs
//! the same engine as everyone's editor (a headless replica), so its edits sync live under its
//! name, its caret shows, and people see it work. Its changes arrive as tracked suggestions
//! unless it was started with `--no-suggest`. It never holds the autosave lease and is never asked
//! for snapshots, versions or exports (`hello` carries `agent: true`).
//!
//! Each tool call first takes in what the relay sent, runs on the shared document, then sends
//! the agent's edits and waits (up to [`CONFIRM_WAIT`]) for the relay to confirm them, so a reply
//! describes the document as everyone has it.

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use wordcraft_collab::proto::{Inbound, Outbound, decode_inbound};
use wordcraft_collab::replica::{CollabError, Replica};
use wordcraft_doc::Document;
use wordcraft_doc::offsets::offset_of;
use wordcraft_engine::{CollabRole, Session};

use wordcraft_mcp::Backend;

/// How long joining waits for the relay's `Welcome`.
const JOIN_WAIT: Duration = Duration::from_secs(15);
/// How long a call waits for the relay to confirm the agent's edits.
pub const CONFIRM_WAIT: Duration = Duration::from_secs(10);
/// The most `wait_for_changes` waits.
const MAX_WAIT: Duration = Duration::from_secs(120);
/// Changes kept for `wait_for_changes`.
const MAX_CHANGES: usize = 2000;

/// A connection to the relay carrying its JSON messages.
pub trait Wire {
    fn send(&mut self, msg: &str) -> Result<(), String>;
    /// The next message, waiting up to `wait`; `Ok(None)` when none came in time.
    fn recv(&mut self, wait: Duration) -> Result<Option<String>, String>;
}

/// The relay's WebSocket (`ws://` or `wss://`).
pub struct WsWire {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
}

impl WsWire {
    pub fn connect(url: &str) -> Result<WsWire, String> {
        let (ws, _) = tungstenite::connect(url).map_err(|e| format!("can't reach the relay at {url}: {e}"))?;
        Ok(WsWire { ws })
    }

    fn set_timeout(&mut self, wait: Duration) {
        let wait = Some(wait.max(Duration::from_millis(1)));
        let r = match self.ws.get_mut() {
            MaybeTlsStream::Plain(s) => s.set_read_timeout(wait),
            MaybeTlsStream::Rustls(s) => s.get_mut().set_read_timeout(wait),
            _ => Ok(()),
        };
        if let Err(e) = r {
            eprintln!("wordcraft-cli collab-mcp: {e}");
        }
    }
}

impl Wire for WsWire {
    fn send(&mut self, msg: &str) -> Result<(), String> {
        self.ws.send(Message::text(msg)).map_err(|e| format!("the relay connection failed: {e}"))
    }

    fn recv(&mut self, wait: Duration) -> Result<Option<String>, String> {
        self.set_timeout(wait);
        match self.ws.read() {
            Ok(Message::Text(t)) => Ok(Some(t.to_string())),
            Ok(Message::Close(frame)) => Err(match frame {
                Some(f) if !f.reason.is_empty() => format!("the relay closed the connection: {}", f.reason),
                _ => "the relay closed the connection".into(),
            }),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(format!("the relay connection failed: {e}")),
        }
    }
}

/// One confirmed edit, for `wait_for_changes`.
#[derive(Clone, Debug, PartialEq)]
struct Change {
    rev: u64,
    user: String,
    command: String,
}

/// An MCP backend whose session is a participant in a shared document.
pub struct Collab {
    wire: Box<dyn Wire>,
    /// Opens a new connection after the old one failed.
    reconnect: Box<dyn FnMut() -> Result<Box<dyn Wire>, String>>,
    token: String,
    client: String,
    /// The agent's view of the document (confirmed plus its own unconfirmed edits).
    pub session: Session,
    replica: Replica,
    /// The others here: client id → (name, colour).
    peers: BTreeMap<String, (String, u8)>,
    changes: Vec<Change>,
    /// Why the relay refused an edit of this call's.
    refused: Option<String>,
    relay: String,
}

/// A client id for this connection.
fn client_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("agent{:x}{:x}", nanos, std::process::id())
}

impl Collab {
    /// Join the relay at `relay` (a `ws://` or `wss://` URL) with join token `token`, as `name`.
    pub fn connect(relay: &str, token: &str, name: &str, suggesting: bool) -> Result<Collab, String> {
        let url = relay.to_string();
        let wire = WsWire::connect(&url)?;
        let reconnect = Box::new(move || WsWire::connect(&url).map(|w| Box::new(w) as Box<dyn Wire>));
        let mut c = Collab::join(Box::new(wire), reconnect, token, name, suggesting)?;
        c.relay = relay.to_string();
        Ok(c)
    }

    /// Join over `wire` (tests give an in-memory one).
    pub fn join(
        mut wire: Box<dyn Wire>,
        reconnect: Box<dyn FnMut() -> Result<Box<dyn Wire>, String>>,
        token: &str,
        name: &str,
        suggesting: bool,
    ) -> Result<Collab, String> {
        let client = client_id();
        wire.send(&json!({"t": "hello", "token": token, "client": client, "agent": true}).to_string())?;
        let deadline = Instant::now() + JOIN_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // The relay holds an agent back until a person opens the document.
                return Err(
                    "no welcome from the relay in 15 s: nobody has this document open yet (an agent joins a document a person has open)".into()
                );
            }
            let Some(msg) = wire.recv(left)? else { continue };
            let Ok(Inbound::Welcome { rev, snapshot, ops, you, history, peers, role, .. }) = decode_inbound(&msg) else { continue };
            let Some(json) = snapshot else {
                return Err("nobody has opened this document yet: an agent joins a document a person has open".into());
            };
            let doc = wordcraft_engine::io::open_bytes("shared.json", json.as_bytes())?;
            let replica = Replica::join_with_history(you, rev, doc, history, ops).map_err(|e| e.to_string())?;
            let mut session = Session::new(Document::new());
            session.author = name.to_string();
            replica.attach(&mut session);
            session.set_collab_role(CollabRole::parse(role.as_deref().unwrap_or("edit")));
            if session.collab_role() != CollabRole::Suggest {
                session.set_collab_tracking(suggesting);
            }
            let peers = peers.into_iter().map(|p| (p.client, (p.user, p.color))).collect();
            return Ok(Collab {
                wire,
                reconnect,
                token: token.to_string(),
                client,
                session,
                replica,
                peers,
                changes: Vec::new(),
                refused: None,
                relay: String::new(),
            });
        }
    }

    fn send(&mut self, m: &Outbound) -> Result<(), String> {
        let text = serde_json::to_string(m).map_err(|e| e.to_string())?;
        self.wire.send(&text)
    }

    /// Take in one relay message.
    fn handle(&mut self, msg: &str) -> Result<(), String> {
        let message = match decode_inbound(msg) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("wordcraft-cli collab-mcp: ignored a relay message: {e}");
                return Ok(());
            }
        };
        match message {
            Inbound::Op(op) => {
                let (rev, user, command) = (op.rev.unwrap_or(0), op.user.clone().unwrap_or_default(), op.cmd.clone());
                match self.replica.receive(*op, &mut self.session) {
                    Ok(_) => {
                        if self.replica.take_needs_fresh() {
                            self.send(&Outbound::ResyncRequest { have: self.replica.rev(), fresh: true })?;
                        }
                        self.changes.push(Change { rev, user, command });
                        if self.changes.len() > MAX_CHANGES {
                            self.changes.remove(0);
                        }
                    }
                    Err(CollabError::Gap { have, .. }) | Err(CollabError::TooOld { oldest: have, .. }) => {
                        self.send(&Outbound::ResyncRequest { have, fresh: false })?
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
            Inbound::Resync { rev, snapshot, ops, seen, history } => {
                let doc = wordcraft_engine::io::open_bytes("shared.json", snapshot.as_bytes())?;
                for op in self.replica.resume(rev, Some((doc, history)), ops, seen, &mut self.session).map_err(|e| e.to_string())? {
                    self.send(&Outbound::Op(Box::new(op)))?;
                }
            }
            Inbound::Welcome { rev, snapshot, ops, seen, history, peers, .. } => {
                // After a reconnect: catch up, then send what the relay didn't get.
                let doc = snapshot.map(|json| wordcraft_engine::io::open_bytes("shared.json", json.as_bytes())).transpose()?;
                self.peers = peers.into_iter().map(|p| (p.client, (p.user, p.color))).collect();
                for op in self.replica.resume(rev, doc.map(|d| (d, history)), ops, seen, &mut self.session).map_err(|e| e.to_string())? {
                    self.send(&Outbound::Op(Box::new(op)))?;
                }
            }
            Inbound::Join(p) | Inbound::Presence(p) => {
                self.peers.insert(p.client, (p.user, p.color));
            }
            Inbound::Leave { client } => {
                self.peers.remove(&client);
            }
            Inbound::Refused { seq, reason } => {
                self.replica.refused(seq, &mut self.session);
                self.refused = Some(reason);
            }
            // An agent never holds the lease: nothing to save, upload or export.
            _ => {}
        }
        Ok(())
    }

    /// Take in what the relay sent, waiting up to `wait` for the first message.
    fn pump(&mut self, wait: Duration) -> Result<(), String> {
        let mut wait = wait;
        loop {
            match self.wire.recv(wait) {
                Ok(Some(msg)) => self.handle(&msg)?,
                Ok(None) => return Ok(()),
                Err(e) => return self.reconnect_after(e),
            }
            wait = Duration::from_millis(1);
        }
    }

    /// The connection failed: open a new one and catch up (the `Welcome` resumes the replica).
    fn reconnect_after(&mut self, why: String) -> Result<(), String> {
        eprintln!("wordcraft-cli collab-mcp: {why}; reconnecting");
        self.wire = (self.reconnect)().map_err(|e| format!("{why}; reconnecting failed: {e}"))?;
        let hello = json!({"t": "hello", "token": self.token, "client": self.client, "agent": true, "haveRev": self.replica.rev()});
        self.wire.send(&hello.to_string())?;
        let deadline = Instant::now() + JOIN_WAIT;
        while Instant::now() < deadline {
            if let Some(msg) = self.wire.recv(Duration::from_millis(200))? {
                let welcome = msg.contains(r#""t":"Welcome""#);
                self.handle(&msg)?;
                if welcome {
                    return Ok(());
                }
            }
        }
        Err(format!("{why}; the relay didn't welcome the agent back"))
    }

    /// Send the agent's edits and wait until the relay confirmed them; then say where its caret is.
    fn flush(&mut self) -> Result<(), String> {
        self.replica.drain_local(&mut self.session);
        let deadline = Instant::now() + CONFIRM_WAIT;
        loop {
            if let Some(op) = self.replica.next_to_send() {
                self.send(&Outbound::Op(Box::new(op)))?;
            }
            if let Some(h) = self.replica.hash_due() {
                self.send(&h)?;
            }
            if self.replica.pending_len() == 0 {
                break;
            }
            if Instant::now() >= deadline {
                return Err("the relay didn't confirm the edit in 10 s; it will be sent again".into());
            }
            self.pump(Duration::from_millis(50))?;
        }
        let sel = &self.session.sel;
        if sel.anchor.story == sel.focus.story
            && let (Some(anchor), Some(head)) = (offset_of(&self.session.doc, &sel.anchor), offset_of(&self.session.doc, &sel.focus))
        {
            let sel = wordcraft_collab::proto::Anchor { story: sel.anchor.story, anchor, head };
            self.send(&Outbound::Presence { sel, rev: self.replica.rev() })?;
        }
        Ok(())
    }

    fn who(&self) -> Value {
        let people: Vec<Value> = self.peers.values().map(|(name, color)| json!({"name": name, "color": color})).collect();
        json!({"you": self.session.author, "people": people, "rev": self.replica.rev()})
    }

    /// Wait until the document is past revision `since` (or `timeout` passes) and list the
    /// changes after it.
    fn wait_for_changes(&mut self, since: u64, timeout: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + timeout.min(MAX_WAIT);
        while self.replica.rev() <= since && Instant::now() < deadline {
            self.pump(Duration::from_millis(100))?;
        }
        let changes: Vec<Value> =
            self.changes.iter().filter(|c| c.rev > since).map(|c| json!({"rev": c.rev, "by": c.user, "command": c.command})).collect();
        Ok(json!({"rev": self.replica.rev(), "changes": changes}))
    }

    /// Run a method on the shared document (the agent's own Suggesting applies, as a person's
    /// does).
    fn run_here(&mut self, method: &str, params: Value) -> Result<Value, String> {
        wordcraft_mcp::answer_on(&mut self.session, method, params)
    }
}

impl Backend for Collab {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.pump(Duration::from_millis(1))?;
        let result = match method {
            "collab.who" => Ok(self.who()),
            "collab.wait" => {
                let since = params.get("since").and_then(Value::as_u64).unwrap_or_else(|| self.replica.rev());
                let ms = params.get("timeoutMs").and_then(Value::as_u64).unwrap_or(30_000);
                return self.wait_for_changes(since, Duration::from_millis(ms));
            }
            // A shared document stays the one document: the agent can't open or start another.
            "file.new" | "file.open" | "file.close" => Err(format!("{method}: not while joined to a shared document")),
            _ => self.run_here(method, params),
        };
        self.flush()?;
        if let Some(reason) = self.refused.take() {
            return Err(reason);
        }
        result
    }

    fn has_ui(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        format!("joined the shared document at {} as {}", self.relay, self.session.author)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    /// The test plays the relay: what the agent sends lands in `sent`; `inbox` is what it reads.
    #[derive(Clone, Default)]
    struct Fake {
        sent: Rc<RefCell<Vec<Value>>>,
        inbox: Rc<RefCell<VecDeque<String>>>,
        /// Stamp the agent's ops and echo them back, as the relay does.
        echo: bool,
        /// Refuse the agent's ops with this reason instead.
        refuse: Option<String>,
        rev: Rc<RefCell<u64>>,
    }

    impl Wire for Fake {
        fn send(&mut self, msg: &str) -> Result<(), String> {
            let v: Value = serde_json::from_str(msg).map_err(|e| e.to_string())?;
            if let Some(reason) = self.refuse.as_ref().filter(|_| v["t"] == "Op") {
                self.inbox.borrow_mut().push_back(json!({"t": "Refused", "seq": v["seq"], "reason": reason}).to_string());
            } else if self.echo && v["t"] == "Op" {
                let mut rev = self.rev.borrow_mut();
                *rev += 1;
                let mut op = v.clone();
                op["rev"] = json!(*rev);
                op["user"] = json!(v["ctx"]["author"]);
                self.inbox.borrow_mut().push_back(op.to_string());
            }
            self.sent.borrow_mut().push(v);
            Ok(())
        }
        fn recv(&mut self, _wait: Duration) -> Result<Option<String>, String> {
            Ok(self.inbox.borrow_mut().pop_front())
        }
    }

    fn snapshot(text: &str) -> String {
        let mut s = Session::new(Document::new());
        s.run("text.insert", &json!({"text": text})).unwrap();
        String::from_utf8(wordcraft_engine::io::save_bytes("d.json", &s.doc).unwrap()).unwrap()
    }

    fn joined(text: &str, suggesting: bool) -> (Collab, Fake) {
        let fake = Fake { echo: true, ..Fake::default() };
        *fake.rev.borrow_mut() = 5;
        let welcome = json!({"t": "Welcome", "rev": 5, "snapshot": snapshot(text), "ops": [], "you": "agent",
            "peers": [{"client": "p1", "user": "Ann", "color": 0}]});
        fake.inbox.borrow_mut().push_back(welcome.to_string());
        let c = Collab::join(Box::new(fake.clone()), Box::new(|| Err("no".into())), "tok", "Helper", suggesting).unwrap();
        (c, fake)
    }

    #[test]
    fn joins_as_an_agent_and_says_who_is_here() {
        let (mut c, fake) = joined("Hello", true);
        let hello = fake.sent.borrow()[0].clone();
        assert_eq!((hello["t"].as_str(), hello["agent"].as_bool(), hello["token"].as_str()), (Some("hello"), Some(true), Some("tok")));
        let who = c.call("collab.who", json!({})).unwrap();
        assert_eq!(who["people"], json!([{"name": "Ann", "color": 0}]));
        assert_eq!(who["you"], "Helper");
    }

    #[test]
    fn nobody_there_yet_is_an_error() {
        let fake = Fake::default();
        fake.inbox.borrow_mut().push_back(r#"{"t":"Welcome","rev":0,"snapshot":null,"ops":[],"you":"a"}"#.into());
        let err = Collab::join(Box::new(fake), Box::new(|| Err("no".into())), "t", "A", true).err().unwrap();
        assert!(err.contains("nobody has opened"));
    }

    #[test]
    fn an_edit_syncs_as_a_suggestion_and_waits_for_its_echo() {
        let (mut c, fake) = joined("Hello", true);
        c.call("command", json!({"command": "caret.docEnd"})).unwrap();
        c.call("command", json!({"command": "text.insert", "params": {"text": " world"}})).unwrap();
        let sent = fake.sent.borrow();
        let op = sent.iter().find(|m| m["t"] == "Op").unwrap();
        assert_eq!(
            (op["cmd"].as_str(), op["ctx"]["track"].as_bool(), op["ctx"]["author"].as_str()),
            (Some("text.insert"), Some(true), Some("Helper"))
        );
        assert!(sent.iter().any(|m| m["t"] == "Presence"), "people see the agent's caret");
        assert_eq!(c.replica.pending_len(), 0);
        assert_eq!(c.replica.confirmed().plain_text(wordcraft_doc::StoryRef::Body), "Hello world");
        assert!(!c.session.doc.settings.track_changes, "tracking is the agent's, not the document's");
        assert_eq!(c.session.doc.revisions.last().map(|r| r.author.as_str()), Some("Helper"));
    }

    #[test]
    fn without_suggesting_edits_are_direct() {
        let (mut c, fake) = joined("Hello", false);
        c.call("command", json!({"command": "text.insert", "params": {"text": "Hi "}})).unwrap();
        let sent = fake.sent.borrow();
        assert_eq!(sent.iter().find(|m| m["t"] == "Op").unwrap()["ctx"]["track"], false);
    }

    #[test]
    fn others_edits_arrive_and_wait_for_changes_lists_them() {
        let (mut c, fake) = joined("Hello", true);
        let op = json!({"t": "Op", "client": "p1", "seq": 1, "base": 5, "kind": "cmd", "cmd": "text.insert", "params": {"text": "!"},
            "sel": {"story": "body", "anchor": 5, "head": 5}, "ctx": {"author": "Ann", "initials": "", "time": 0, "track": false,
            "editing": {"autocorrect": {}, "typingReplacesSelection": true, "overtype": false}}, "rev": 6, "user": "Ann"});
        fake.inbox.borrow_mut().push_back(op.to_string());
        let got = c.call("collab.wait", json!({"since": 5, "timeoutMs": 100})).unwrap();
        assert_eq!(got["rev"], 6);
        assert_eq!(got["changes"], json!([{"rev": 6, "by": "Ann", "command": "text.insert"}]));
        assert_eq!(c.session.doc.plain_text(wordcraft_doc::StoryRef::Body), "Hello!");
    }

    #[test]
    fn a_refused_edit_is_the_calls_error() {
        let (mut c, fake) = joined("Hello", true);
        let mut f = fake.clone();
        f.refuse = Some("You can only comment on this document".into());
        c.wire = Box::new(f);
        let err = c.call("command", json!({"command": "text.insert", "params": {"text": "x"}})).unwrap_err();
        assert_eq!(err, "You can only comment on this document");
        assert_eq!(c.session.doc.plain_text(wordcraft_doc::StoryRef::Body), "Hello");
    }

    #[test]
    fn comment_and_reply_tools_sync() {
        use wordcraft_mcp::tools::call_tool;
        let (mut c, fake) = joined("Hello world", true);
        let r = call_tool(&mut c, "comment", &json!({"text": "Nice word", "quote": "world"}));
        assert!(!r.is_error, "{r:?}");
        let id = *c.session.doc.comments.keys().next().unwrap();
        let r = call_tool(&mut c, "reply", &json!({"id": id, "text": "Thanks"}));
        assert!(!r.is_error, "{r:?}");
        let cmds: Vec<String> = fake.sent.borrow().iter().filter(|m| m["t"] == "Op").map(|m| m["cmd"].as_str().unwrap().to_string()).collect();
        assert_eq!(cmds, ["review.newComment", "review.reply"]);
        assert_eq!(c.replica.pending_len(), 0);
        let who = call_tool(&mut c, "who_is_here", &json!({}));
        assert!(!who.is_error);
        let mut headless = wordcraft_mcp::Headless::default();
        assert!(call_tool(&mut headless, "who_is_here", &json!({})).is_error);
    }

    #[test]
    fn opening_another_document_is_refused() {
        let (mut c, _) = joined("Hello", true);
        assert!(c.call("file.open", json!({"path": "x.docx"})).unwrap_err().contains("shared document"));
    }
}
