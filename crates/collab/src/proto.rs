//! The messages between a replica and the relay (see the co-editing spec, §3 and §8). Each is a
//! JSON object whose `t` names its type; field names are camelCase. The host page carries them
//! as strings, so a replica only ever sees `decode_inbound` and `serde_json::to_string`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wordcraft_doc::StoryRef;
use wordcraft_engine::RunCtx;

/// The largest message that isn't a snapshot, in bytes.
pub const MAX_OP_BYTES: usize = 256 * 1024;
/// The largest message carrying a whole document (`Welcome`, `Resync`, `VersionDoc`, a restore
/// `Op`), in bytes.
pub const MAX_SNAPSHOT_BYTES: usize = 50 * 1024 * 1024;

/// A selection as story offsets (`wordcraft_doc::offsets`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
    pub story: StoryRef,
    pub anchor: usize,
    pub head: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OpKind {
    #[default]
    Cmd,
    Undo,
    Redo,
    Restore,
}

/// One edit: a command, the selection it ran at and the context it ran under.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Op {
    /// The replica that made it.
    pub client: String,
    /// Its number among that replica's edits.
    pub seq: u64,
    /// The last relay revision its replica had applied.
    pub base: u64,
    pub kind: OpKind,
    pub cmd: String,
    #[serde(default)]
    pub params: Value,
    pub sel: Anchor,
    pub ctx: RunCtx,
    /// Set by the relay: the edit's place in the document's history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
    /// Set by the relay: who made it (the join token's name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// One revision's changes (`mapping::Span`s, per story): what a replica needs to place an edit
/// based on an earlier revision. Snapshots carry the recent ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistEntry {
    pub rev: u64,
    pub spans: Vec<HistSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistSpan {
    pub story: StoryRef,
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

/// Someone else in the document: who, their colour (an index into the palette) and, once they
/// sent one, where their selection is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub client: String,
    pub user: String,
    pub color: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sel: Option<Anchor>,
    /// The confirmed revision `sel` is relative to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
}

/// Relay → replica.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Inbound {
    /// After joining: the document at `rev` (a native JSON snapshot, or none when this is the
    /// first person and their editor provides the document), the edits after it, and this
    /// replica's client id.
    Welcome {
        rev: u64,
        snapshot: Option<String>,
        ops: Vec<Op>,
        you: String,
        /// With a snapshot: the last of this replica's edits it holds (`Replica::resume`).
        #[serde(default)]
        seen: Option<u64>,
        /// With a snapshot: the changes of the revisions before it.
        #[serde(default)]
        history: Vec<HistEntry>,
        /// The people already here (one that can't be read is left out, not the whole message).
        #[serde(default, deserialize_with = "readable_peers")]
        peers: Vec<Peer>,
        /// The join token's role: `edit`, `comment` or `view` (an older relay sends none: `edit`).
        #[serde(default)]
        role: Option<String>,
    },
    Op(Box<Op>),
    /// Upload a snapshot of the confirmed document.
    SnapshotRequest,
    /// Start again from this snapshot and the edits after it.
    Resync {
        rev: u64,
        snapshot: String,
        ops: Vec<Op>,
        #[serde(default)]
        seen: Option<u64>,
        #[serde(default)]
        history: Vec<HistEntry>,
    },
    /// Save the document now (autosave).
    Save,
    /// Someone joined.
    Join(Peer),
    /// Someone left.
    Leave {
        client: String,
    },
    /// Someone's selection moved.
    Presence(Peer),
    /// The relay didn't take this replica's edit `seq` (its role doesn't allow it, or an undo
    /// names someone else's edit); it was not sequenced.
    Refused {
        seq: u64,
        reason: String,
    },
    /// Keep the confirmed document at `rev` as a version (answer with `Outbound::Version` only
    /// when the confirmed revision is `rev`).
    VersionRequest {
        rev: u64,
    },
    /// The document's versions, newest first.
    VersionList {
        versions: Vec<VersionInfo>,
    },
    /// One version's document (native JSON), or none when it is gone.
    VersionDoc {
        rev: u64,
        doc: Option<String>,
    },
    /// The relay's HTTP API wants the document as a file of `format`.
    Export {
        id: String,
        format: String,
    },
    /// From the page, not the relay: whether it is connected to the relay now.
    Connection {
        online: bool,
    },
    /// The relay's HTTP API runs command `cmd` as an edit by `author`.
    Run {
        id: String,
        cmd: String,
        #[serde(default)]
        params: Value,
        author: String,
    },
}

/// One kept version: its revision, when it was kept (ISO time) and who edited since the one
/// before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    pub rev: u64,
    pub at: String,
    #[serde(default)]
    pub users: Vec<String>,
}

/// Replica → relay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Outbound {
    Op(Box<Op>),
    /// The confirmed document's fingerprint at `rev` (`hash::doc_hash`).
    Hash {
        rev: u64,
        h: String,
    },
    Snapshot {
        rev: u64,
        doc: String,
        #[serde(default)]
        history: Vec<HistEntry>,
    },
    Saved {
        rev: u64,
    },
    /// This replica missed edits after `have`.
    ResyncRequest {
        have: u64,
    },
    /// Where this person's selection is, relative to the confirmed revision `rev` (sent at most
    /// ten times a second, and only with no edits unconfirmed; never logged).
    Presence {
        sel: Anchor,
        rev: u64,
    },
    /// The confirmed document at `rev` (native JSON), answering `VersionRequest`.
    Version {
        rev: u64,
        doc: String,
    },
    /// List the versions (`VersionList`).
    Versions,
    /// One version's document (`VersionDoc`).
    GetVersion {
        rev: u64,
    },
    /// Answers `Export`: the file's bytes in base64, or why there are none.
    Exported {
        id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Answers `Run`.
    RunResult {
        id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

fn readable_peers<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Peer>, D::Error> {
    let all = Vec::<Value>::deserialize(d)?;
    Ok(all.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("message too large: {size} bytes (limit {limit})")]
    TooLarge { size: usize, limit: usize },
    #[error("bad message: {0}")]
    Json(#[from] serde_json::Error),
}

/// Parse a relay message, refusing one over its size limit.
pub fn decode_inbound(json: &str) -> Result<Inbound, ProtoError> {
    if json.len() > MAX_SNAPSHOT_BYTES {
        return Err(ProtoError::TooLarge { size: json.len(), limit: MAX_SNAPSHOT_BYTES });
    }
    let message: Inbound = serde_json::from_str(json)?;
    let carries_snapshot = match &message {
        Inbound::Welcome { .. } | Inbound::Resync { .. } | Inbound::VersionDoc { .. } => true,
        Inbound::Op(op) => op.kind == OpKind::Restore,
        _ => false,
    };
    if !carries_snapshot && json.len() > MAX_OP_BYTES {
        return Err(ProtoError::TooLarge { size: json.len(), limit: MAX_OP_BYTES });
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wordcraft_doc::StoryRef;
    use wordcraft_engine::RunCtx;

    use super::*;

    fn op() -> Op {
        Op {
            client: "c1".into(),
            seq: 42,
            base: 1187,
            kind: OpKind::Cmd,
            cmd: "text.insert".into(),
            params: json!({"text": "a"}),
            sel: Anchor { story: StoryRef::Body, anchor: 5120, head: 5120 },
            ctx: RunCtx { author: "Jane Doe".into(), ..RunCtx::default() },
            rev: None,
            user: None,
        }
    }

    #[test]
    fn op_round_trips_json() {
        let text = serde_json::to_string(&Outbound::Op(Box::new(op()))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["t"], "Op");
        assert_eq!(v["cmd"], "text.insert");
        assert_eq!(v["sel"]["anchor"], 5120);
        assert_eq!(v["ctx"]["author"], "Jane Doe");
        let back = decode_inbound(&text).unwrap();
        assert_eq!(back, Inbound::Op(Box::new(op())));
    }

    #[test]
    fn relay_stamped_op_keeps_rev_and_user() {
        let mut v = serde_json::to_value(Outbound::Op(Box::new(op()))).unwrap();
        v["rev"] = json!(1188);
        v["user"] = json!("Jane Doe");
        let Inbound::Op(o) = decode_inbound(&v.to_string()).unwrap() else { panic!("not an op") };
        assert_eq!((o.rev, o.user.as_deref()), (Some(1188), Some("Jane Doe")));
    }

    #[test]
    fn welcome_and_resync_decode() {
        let w = decode_inbound(r#"{"t":"Welcome","rev":3,"snapshot":null,"ops":[],"you":"c1"}"#).unwrap();
        assert_eq!(
            w,
            Inbound::Welcome { rev: 3, snapshot: None, ops: vec![], you: "c1".into(), seen: None, history: vec![], peers: vec![], role: None }
        );
        assert_eq!(decode_inbound(r#"{"t":"Save"}"#).unwrap(), Inbound::Save);
        assert_eq!(decode_inbound(r#"{"t":"SnapshotRequest"}"#).unwrap(), Inbound::SnapshotRequest);
        let r = decode_inbound(r#"{"t":"Resync","rev":9,"snapshot":"{}","ops":[]}"#).unwrap();
        assert!(matches!(r, Inbound::Resync { rev: 9, seen: None, .. }));
        let r = decode_inbound(r#"{"t":"Resync","rev":9,"snapshot":"{}","ops":[],"seen":4}"#).unwrap();
        assert!(matches!(r, Inbound::Resync { seen: Some(4), .. }));
    }

    #[test]
    fn outbound_messages_have_their_tags() {
        let t = |m: &Outbound| serde_json::to_value(m).unwrap()["t"].as_str().unwrap().to_string();
        assert_eq!(t(&Outbound::Hash { rev: 50, h: "ab".into() }), "Hash");
        assert_eq!(t(&Outbound::Snapshot { rev: 1, doc: "{}".into(), history: vec![] }), "Snapshot");
        assert_eq!(t(&Outbound::Saved { rev: 1 }), "Saved");
        assert_eq!(serde_json::to_value(Outbound::ResyncRequest { have: 7 }).unwrap(), json!({"t": "ResyncRequest", "have": 7}));
    }

    #[test]
    fn presence_messages_decode() {
        let j = decode_inbound(r#"{"t":"Join","client":"b","user":"B","color":1}"#).unwrap();
        assert_eq!(j, Inbound::Join(Peer { client: "b".into(), user: "B".into(), color: 1, sel: None, rev: None }));
        assert_eq!(decode_inbound(r#"{"t":"Leave","client":"b"}"#).unwrap(), Inbound::Leave { client: "b".into() });
        let p = decode_inbound(r#"{"t":"Presence","client":"a","user":"A","color":0,"sel":{"story":"body","anchor":3,"head":5}}"#).unwrap();
        let Inbound::Presence(peer) = p else { panic!("not presence") };
        assert_eq!(peer.sel, Some(Anchor { story: StoryRef::Body, anchor: 3, head: 5 }));
        let w =
            decode_inbound(r#"{"t":"Welcome","rev":0,"snapshot":null,"ops":[],"you":"c","peers":[{"client":"a","user":"A","color":0}]}"#).unwrap();
        let Inbound::Welcome { peers, .. } = w else { panic!("not welcome") };
        assert_eq!(peers.len(), 1);
        assert_eq!(
            serde_json::to_value(Outbound::Presence { sel: Anchor { story: StoryRef::Body, anchor: 2, head: 2 }, rev: 7 }).unwrap(),
            json!({"t": "Presence", "sel": {"story": "body", "anchor": 2, "head": 2}, "rev": 7})
        );
    }

    #[test]
    fn oversized_op_rejected() {
        let mut o = op();
        o.params = json!({"text": "x".repeat(MAX_OP_BYTES)});
        let text = serde_json::to_string(&Outbound::Op(Box::new(o))).unwrap();
        assert!(matches!(decode_inbound(&text), Err(ProtoError::TooLarge { .. })));
    }

    #[test]
    fn large_snapshot_is_allowed_up_to_its_own_limit() {
        let doc = "x".repeat(MAX_OP_BYTES * 2);
        let text = serde_json::json!({"t": "Resync", "rev": 1, "snapshot": doc, "ops": []}).to_string();
        assert!(decode_inbound(&text).is_ok());
    }

    #[test]
    fn protocol_v2_inbound_messages_decode() {
        let w = decode_inbound(r#"{"t":"Welcome","rev":0,"snapshot":null,"ops":[],"you":"c","role":"view"}"#).unwrap();
        assert!(matches!(w, Inbound::Welcome { role: Some(ref r), .. } if r == "view"));
        assert_eq!(
            decode_inbound(r#"{"t":"Refused","seq":4,"reason":"You can only view this document"}"#).unwrap(),
            Inbound::Refused { seq: 4, reason: "You can only view this document".into() }
        );
        assert_eq!(decode_inbound(r#"{"t":"VersionRequest","rev":12}"#).unwrap(), Inbound::VersionRequest { rev: 12 });
        assert_eq!(decode_inbound(r#"{"t":"Connection","online":false}"#).unwrap(), Inbound::Connection { online: false });
        assert_eq!(
            decode_inbound(r#"{"t":"VersionList","versions":[{"rev":9,"at":"2026-10-29T10:00:00Z","users":["Ann"]}]}"#).unwrap(),
            Inbound::VersionList { versions: vec![VersionInfo { rev: 9, at: "2026-10-29T10:00:00Z".into(), users: vec!["Ann".into()] }] }
        );
        assert_eq!(decode_inbound(r#"{"t":"VersionDoc","rev":9,"doc":null}"#).unwrap(), Inbound::VersionDoc { rev: 9, doc: None });
        assert_eq!(decode_inbound(r#"{"t":"Export","id":"e1","format":"pdf"}"#).unwrap(), Inbound::Export { id: "e1".into(), format: "pdf".into() });
        assert_eq!(
            decode_inbound(r#"{"t":"Run","id":"r1","cmd":"format.bold","params":{"value":true},"author":"API"}"#).unwrap(),
            Inbound::Run { id: "r1".into(), cmd: "format.bold".into(), params: json!({"value": true}), author: "API".into() }
        );
    }

    #[test]
    fn protocol_v2_outbound_messages_encode() {
        let v = |m: Outbound| serde_json::to_value(m).unwrap();
        assert_eq!(v(Outbound::Version { rev: 3, doc: "{}".into() }), json!({"t": "Version", "rev": 3, "doc": "{}"}));
        assert_eq!(v(Outbound::Versions), json!({"t": "Versions"}));
        assert_eq!(v(Outbound::GetVersion { rev: 3 }), json!({"t": "GetVersion", "rev": 3}));
        assert_eq!(
            v(Outbound::Exported { id: "e".into(), ok: true, data: Some("AA==".into()), error: None }),
            json!({"t": "Exported", "id": "e", "ok": true, "data": "AA=="})
        );
        assert_eq!(
            v(Outbound::RunResult { id: "r".into(), ok: false, result: None, error: Some("Unknown command".into()) }),
            json!({"t": "RunResult", "id": "r", "ok": false, "error": "Unknown command"})
        );
    }

    #[test]
    fn a_restore_op_or_version_may_be_as_large_as_a_snapshot() {
        let mut o = op();
        o.kind = OpKind::Restore;
        o.rev = Some(1);
        o.params = json!({"version": 3, "doc": "x".repeat(MAX_OP_BYTES + 10)});
        assert!(decode_inbound(&serde_json::to_string(&Outbound::Op(Box::new(o))).unwrap()).is_ok());
        let doc = json!({"t": "VersionDoc", "rev": 1, "doc": "x".repeat(MAX_OP_BYTES + 10)}).to_string();
        assert!(decode_inbound(&doc).is_ok());
    }

    #[test]
    fn unknown_type_is_error() {
        assert!(matches!(decode_inbound(r#"{"t":"Nope"}"#), Err(ProtoError::Json(_))));
        assert!(matches!(decode_inbound("not json"), Err(ProtoError::Json(_))));
    }
}
