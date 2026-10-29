//! The co-editing outbox: each edit a person makes, recorded once, with the selection it started
//! from and the context it ran in.

use serde_json::json;
use wordcraft_doc::StoryRef;

use crate::{CmdError, Session};

fn collab() -> Session {
    let mut s = Session::new(wordcraft_doc::Document::new());
    s.set_collab(true);
    s
}

#[test]
fn records_only_top_level_mutations() {
    let mut s = collab();
    s.run("text.insert", &json!({"text": "Hi"})).unwrap();
    s.run("text.insert", &json!({"text": "!"})).unwrap();
    s.run("caret.left", &json!({})).unwrap();
    let out = s.take_outbox();
    assert_eq!(out.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["text.insert", "text.insert"]);
    assert_eq!(out[1].params, json!({"text": "!"}));
    assert!(s.take_outbox().is_empty(), "take_outbox drains");
}

#[test]
fn macro_playback_records_the_commands_it_runs() {
    let mut s = collab();
    s.run(
        "tools.macros",
        &json!({"define": {"name": "m", "steps": [{"command": "text.insert", "params": {"text": "a"}}, {"command": "text.newParagraph"}]}}),
    )
    .unwrap();
    s.take_outbox();
    s.run("tools.macros", &json!({"run": "m"})).unwrap();
    let ids: Vec<String> = s.take_outbox().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, ["text.insert", "text.newParagraph"]);
}

#[test]
fn failed_or_noop_command_records_nothing() {
    let mut s = collab();
    let _ = s.run("text.backspace", &json!({}));
    let _ = s.run("text.insert", &json!({"text": 5}));
    assert!(s.take_outbox().is_empty());
}

#[test]
fn recording_off_records_nothing() {
    let mut s = collab();
    s.set_recording(false);
    s.run("text.insert", &json!({"text": "x"})).unwrap();
    assert!(s.take_outbox().is_empty());
}

#[test]
fn not_collab_records_nothing() {
    let mut s = Session::new(wordcraft_doc::Document::new());
    s.run("text.insert", &json!({"text": "x"})).unwrap();
    assert!(s.take_outbox().is_empty());
    assert!(!s.collab());
}

#[test]
fn collab_refuses_undo_and_file_commands() {
    let mut s = collab();
    s.run("text.insert", &json!({"text": "x"})).unwrap();
    for id in ["edit.undo", "edit.undoTo", "edit.redo", "file.new", "file.open", "file.close"] {
        match s.run(id, &json!({})) {
            Err(CmdError::Disabled(m)) => assert!(m.contains("co-editing"), "{id}: {m}"),
            other => panic!("{id}: {other:?}"),
        }
    }
}

#[test]
fn sel_before_and_anchor_are_the_selection_before_the_run() {
    let mut s = collab();
    s.run("text.insert", &json!({"text": "Hello"})).unwrap();
    s.run("select.text", &json!({"text": "ell"})).unwrap();
    s.take_outbox();
    s.run("format.bold", &json!({})).unwrap();
    let r = s.take_outbox().pop().unwrap();
    assert_eq!(r.sel_before, s.sel);
    assert_eq!(r.anchor, Some((StoryRef::Body, 1, 4)));
}

#[test]
fn recorded_context_is_the_one_the_command_ran_with() {
    let mut s = collab();
    s.author = "Ann".into();
    s.run("text.insert", &json!({"text": "x"})).unwrap();
    s.run("select.all", &json!({})).unwrap();
    s.run("review.newComment", &json!({"text": "n"})).unwrap();
    let r = s.take_outbox().pop().unwrap();
    assert_eq!(r.ctx.author, "Ann");
    let date = &s.doc.comments.values().next().unwrap().date;
    assert_eq!(*date, crate::cmd::iso_from_unix_secs(r.ctx.time));
}

#[test]
fn a_rolled_back_preview_records_nothing() {
    let mut s = collab();
    s.run("text.insert", &json!({"text": "Hello"})).unwrap();
    s.run("select.all", &json!({})).unwrap();
    let snap = s.edit_snapshot();
    s.run("format.bold", &json!({})).unwrap();
    s.restore(snap);
    let ids: Vec<String> = s.take_outbox().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, ["text.insert"]);
}
