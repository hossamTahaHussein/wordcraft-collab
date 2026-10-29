mod support;

use serde_json::json;
use support::{Peer, Relay, doc_with};
use wordcraft_collab::replica::{Applied, CollabError};

#[test]
fn local_typing_is_visible_immediately_and_echo_changes_nothing() {
    let mut relay = Relay::new(doc_with("Hello"));
    let mut a = Peer::join("a", "Ann", &relay);
    a.act("caret.docEnd", json!({}));
    a.type_text("!");
    assert_eq!(a.text(), "Hello!");
    assert_eq!(a.confirmed_text(), "Hello");
    assert_eq!(a.replica.pending_len(), 1);
    relay.settle(&mut [&mut a]);
    assert_eq!(a.text(), "Hello!");
    assert_eq!(a.confirmed_text(), "Hello!");
    assert_eq!(a.replica.pending_len(), 0);
    assert_eq!(a.replica.rev(), 1);
}

#[test]
fn remote_op_applies_with_its_authors_selection() {
    let mut relay = Relay::new(doc_with("Hello world"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    b.act("select.text", json!({"text": "world"}));
    b.act("format.bold", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    let bold = |p: &Peer| {
        let para = p.visible.doc.para(wordcraft_doc::StoryRef::Body, &wordcraft_doc::Path(vec![0])).unwrap().clone();
        (para.props_of_char(0).bold, para.props_of_char(6).bold)
    };
    assert_eq!(bold(&a), bold(&b));
    assert_eq!(bold(&a).1, Some(true));
    // Ann's own caret didn't move to Bob's selection.
    assert!(a.visible.sel.is_collapsed());
}

#[test]
fn concurrent_typing_same_paragraph() {
    let mut relay = Relay::new(doc_with("01234 tail"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    for p in [&mut a, &mut b] {
        p.act("caret.docStart", json!({}));
        for _ in 0..5 {
            p.act("caret.right", json!({}));
        }
    }
    // Both type at offset 5 before anything travels, a keystroke at a time.
    for i in 0..3 {
        a.type_text("a");
        b.type_text("b");
        if i == 1 {
            relay.collect(&mut [&mut a, &mut b], &[0, 1]);
            a.deliver(1);
        }
    }
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), b.text());
    assert_eq!(a.text(), a.confirmed_text());
    assert!(a.text().contains("aaa") && a.text().contains("bbb"), "{}", a.text());
    assert!(a.text().starts_with("01234") && a.text().ends_with(" tail"), "{}", a.text());
    // Each caret is right after its own typing.
    let caret = |p: &Peer| p.visible.sel.focus.off;
    let at = |p: &Peer, s: &str| p.text().find(s).unwrap() + s.len();
    assert_eq!(caret(&a), at(&a, "aaa"));
    assert_eq!(caret(&b), at(&b, "bbb"));
}

#[test]
fn edits_in_different_places_both_land() {
    let mut relay = Relay::new(doc_with("start middle end"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    a.act("caret.docStart", json!({}));
    a.type_text("A-");
    b.act("caret.docEnd", json!({}));
    b.type_text("-B");
    b.act("select.text", json!({"text": "middle"}));
    b.type_text("MID");
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "A-start MID end-B");
    assert_eq!(b.text(), a.text());
}

#[test]
fn pending_edit_after_remote_delete() {
    let mut relay = Relay::new(doc_with("Intro"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    a.act("caret.docEnd", json!({}));
    a.act("text.newParagraph", json!({}));
    a.act("insert.table", json!({"rows": 2, "cols": 2}));
    relay.settle(&mut [&mut a, &mut b]);
    // Bob deletes the table while Ann, in it, adds a row that hasn't travelled yet.
    b.act("caret.docEnd", json!({}));
    b.act("select.all", json!({}));
    b.act("caret.docStart", json!({}));
    let mut found = false;
    for _ in 0..10 {
        if b.visible.run("table.deleteTable", &json!({})).is_ok() {
            found = true;
            break;
        }
        let _ = b.visible.run("caret.down", &json!({}));
    }
    assert!(found, "Bob's caret reached the table");
    b.replica.drain_local(&mut b.visible);
    a.act("table.insertRowBelow", json!({}));
    let rows_before = a.text();
    // Only Bob's delete travels; Ann's row is still unsent when it reaches her.
    relay.collect(&mut [&mut a, &mut b], &[1]);
    a.deliver(usize::MAX);
    assert_eq!(a.dropped, 1, "Ann's row couldn't be added to a deleted table");
    assert_ne!(a.text(), rows_before);
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), b.text());
    assert_eq!(a.text(), a.confirmed_text());
}

#[test]
fn gap_is_reported() {
    let mut relay = Relay::new(doc_with("x"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    b.type_text("1");
    relay.collect(&mut [&mut a, &mut b], &[1]);
    b.type_text("2");
    relay.settle(&mut [&mut b]);
    let second = relay.log[1].clone();
    match a.replica.receive(second, &mut a.visible) {
        Err(CollabError::Gap { have: 0, got: 2 }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn resume_pops_echoed_and_returns_unsent() {
    let mut relay = Relay::new(doc_with("x"));
    let mut a = Peer::join("a", "Ann", &relay);
    a.act("caret.docEnd", json!({}));
    a.type_text(" one");
    relay.collect(&mut [&mut a], &[0]);
    // The connection drops: the relay has op 1, Ann never heard back, and kept typing.
    a.inbox.clear();
    a.type_text(" two");
    assert_eq!(a.replica.pending_len(), 2);
    let tail = relay.log.clone();
    let resend = a.replica.resume(0, None, tail, &mut a.visible).unwrap();
    assert_eq!(resend.len(), 1);
    assert_eq!(resend[0].params, json!({"text": " two"}));
    assert_eq!(a.replica.rev(), 1);
    assert_eq!(a.confirmed_text(), "x one");
    assert_eq!(a.text(), "x one two");
}

#[test]
fn a_replica_sends_one_op_at_a_time_and_merges_plain_typing() {
    let relay = Relay::new(doc_with("x"));
    let mut a = Peer::join("a", "Ann", &relay);
    a.act("caret.docEnd", json!({}));
    for c in ["a", "b", "c"] {
        a.type_text(c);
    }
    let first = a.replica.next_to_send().unwrap();
    assert_eq!(first.params, json!({"text": "abc"}), "plain letters typed in a row travel as one edit");
    assert!(a.replica.next_to_send().is_none(), "one op in flight");
    let _ = Applied::Echo;
}

#[test]
fn spaces_and_punctuation_are_not_merged() {
    let relay = Relay::new(doc_with("x"));
    let mut a = Peer::join("a", "Ann", &relay);
    a.act("caret.docEnd", json!({}));
    for c in ["a", "b", " ", "c"] {
        a.type_text(c);
    }
    let first = a.replica.next_to_send().unwrap();
    assert_eq!(first.params, json!({"text": "ab"}));
}

#[test]
fn an_edit_in_flight_that_fails_after_a_remote_change_fails_everywhere() {
    let mut relay = Relay::new(doc_with("Intro"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    a.act("caret.docEnd", json!({}));
    a.act("text.newParagraph", json!({}));
    a.act("insert.table", json!({"rows": 2, "cols": 2}));
    relay.settle(&mut [&mut a, &mut b]);
    b.act("select.all", json!({}));
    b.act("text.delete", json!({}));
    a.act("table.insertRowBelow", json!({}));
    // Both are sent before either hears of the other.
    relay.collect(&mut [&mut a, &mut b], &[1, 0]);
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), b.text());
    assert_eq!(a.replica.confirmed().body.len(), b.replica.confirmed().body.len());
    assert_eq!(a.replica.pending_len(), 0);
}
