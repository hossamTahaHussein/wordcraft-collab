//! `Session::run_with`: the same command under the same context gives the same document on any
//! replica (co-editing replays commands, so every replica must agree).

use serde_json::json;

use crate::{EditingPrefs, RunCtx, Session, io};

fn native_json(doc: &wordcraft_doc::Document) -> String {
    String::from_utf8(io::save_bytes("x.json", doc).unwrap()).unwrap()
}

fn ctx(author: &str) -> RunCtx {
    RunCtx { author: author.into(), initials: "A".into(), time: 1_800_000_000, track: false, editing: EditingPrefs::default() }
}

#[test]
fn run_with_is_deterministic_across_sessions() {
    let ctx = RunCtx { track: true, ..ctx("Ann") };
    let mut a = Session::new(wordcraft_doc::Document::new());
    let mut b = Session::new(wordcraft_doc::Document::new());
    for s in [&mut a, &mut b] {
        s.run_with(&ctx, "text.insert", &json!({"text": "Hello"})).unwrap();
        s.run_with(&ctx, "select.all", &json!({})).unwrap();
        s.run_with(&ctx, "review.newComment", &json!({"text": "note"})).unwrap();
    }
    assert_eq!(native_json(&a.doc), native_json(&b.doc));
}

#[test]
fn run_with_uses_the_context_clock() {
    let mut s = Session::new(wordcraft_doc::Document::new());
    s.run_with(&ctx("Ann"), "text.insert", &json!({"text": "x"})).unwrap();
    s.run_with(&ctx("Ann"), "select.all", &json!({})).unwrap();
    s.run_with(&ctx("Ann"), "review.newComment", &json!({"text": "note"})).unwrap();
    let comment = s.doc.comments.values().next().unwrap();
    assert!(comment.date.starts_with("2027-01-15"), "{}", comment.date);
    assert_eq!(comment.author, "Ann");
}

#[test]
fn run_with_restores_session_author_and_prefs() {
    let mut s = Session::new(wordcraft_doc::Document::new());
    s.author = "Me".into();
    s.prefs.overtype = false;
    let ctx = RunCtx { editing: EditingPrefs { overtype: true, ..EditingPrefs::default() }, ..ctx("Ann") };
    s.run_with(&ctx, "text.insert", &json!({"text": "x"})).unwrap();
    assert_eq!(s.author, "Me");
    assert!(!s.prefs.overtype);
}

#[test]
fn run_with_track_true_marks_insertion_without_changing_session_setting() {
    let mut s = Session::new(wordcraft_doc::Document::new());
    let ctx = RunCtx { track: true, ..ctx("Ann") };
    s.run_with(&ctx, "text.insert", &json!({"text": "tracked"})).unwrap();
    assert!(!s.doc.settings.track_changes);
    assert!(s.doc.revisions.iter().any(|r| r.author == "Ann"), "{:?}", s.doc.revisions);
}

#[test]
fn run_ctx_reflects_the_session() {
    let mut s = Session::new(wordcraft_doc::Document::new());
    s.author = "Me".into();
    s.doc.settings.track_changes = true;
    let c = s.run_ctx();
    assert_eq!(c.author, "Me");
    assert!(c.track);
}

/// Every editing command, run under one context on two copies of the same document, gives the
/// same document; the few that can't are listed in `NONDETERMINISTIC` (refused while co-editing).
#[test]
fn every_mutating_command_is_deterministic_under_run_with() {
    let ctx = RunCtx { track: true, ..ctx("Ann") };
    let reg = crate::cmd::registry();
    let mut differ = Vec::new();
    for spec in reg.all() {
        if !spec.mutates || spec.id.starts_with("file.") || matches!(spec.id, "insert.picture" | "insert.textFromFile") {
            continue;
        }
        if crate::run_ctx::NONDETERMINISTIC.iter().any(|(id, _)| *id == spec.id) {
            continue;
        }
        // Once at a caret and once with a word selected.
        for select in [None, Some("Membership")] {
            let mut docs = Vec::new();
            for _ in 0..2 {
                let mut s = Session::new(crate::sample::sample_document());
                if let Some(word) = select {
                    let _ = s.run("select.text", &json!({"text": word}));
                }
                let ok = s.run_with(&ctx, spec.id, &json!({})).is_ok();
                docs.push((ok, native_json(&s.doc)));
            }
            if docs[0] != docs[1] {
                differ.push(format!("{} (selection {select:?})", spec.id));
            }
        }
    }
    assert!(differ.is_empty(), "nondeterministic under run_with: {differ:#?}");
}
