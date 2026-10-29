mod support;

use serde_json::json;
use support::{Peer, Relay, doc_with, keep_version};

fn end(p: &mut Peer) {
    p.act("caret.docEnd", json!({}));
}

#[test]
fn undo_removes_my_edit_for_everyone() {
    let mut relay = Relay::new(doc_with("Hello"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    end(&mut a);
    a.type_text("!");
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(b.text(), "Hello!");
    assert!(a.replica.can_undo());
    assert!(!b.replica.can_undo(), "Bob has nothing of his to undo");
    a.act("edit.undo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "Hello");
    assert_eq!(b.text(), "Hello");
    assert_eq!(a.confirmed_text(), b.confirmed_text());
}

#[test]
fn undo_keeps_later_edits_of_others_where_they_belong() {
    let mut relay = Relay::new(doc_with("one two"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    a.act("caret.docStart", json!({}));
    a.type_text("AAA ");
    relay.settle(&mut [&mut a, &mut b]);
    end(&mut b);
    b.type_text(" BBB");
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "AAA one two BBB");
    a.act("edit.undo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "one two BBB");
    assert_eq!(b.text(), "one two BBB");
}

#[test]
fn redo_brings_it_back_and_a_new_edit_ends_redo() {
    let mut relay = Relay::new(doc_with("x"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    end(&mut a);
    a.type_text("y");
    relay.settle(&mut [&mut a, &mut b]);
    a.act("edit.undo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    assert!(a.replica.can_redo());
    a.act("edit.redo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "xy");
    assert_eq!(b.text(), "xy");
    a.act("edit.undo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    end(&mut a);
    a.type_text("z");
    relay.settle(&mut [&mut a, &mut b]);
    assert!(!a.replica.can_redo());
    a.act("edit.redo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.replica.take_notes(), ["Nothing of yours to redo"]);
    assert_eq!(b.text(), "xz");
}

#[test]
fn undo_with_nothing_of_mine_leaves_a_note_and_sends_nothing() {
    let mut relay = Relay::new(doc_with("x"));
    let mut a = Peer::join("a", "Ann", &relay);
    a.act("edit.undo", json!({}));
    assert_eq!(relay.collect(&mut [&mut a], &[0]), 0);
    assert_eq!(a.replica.take_notes(), ["Nothing of yours to undo"]);
}

#[test]
fn my_unconfirmed_typing_survives_my_undo() {
    let mut relay = Relay::new(doc_with("ab"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    a.act("caret.docStart", json!({}));
    a.type_text("1");
    relay.settle(&mut [&mut a, &mut b]);
    a.act("edit.undo", json!({}));
    end(&mut a);
    a.type_text("2");
    assert_eq!(a.text(), "1ab2", "undo waits for the relay; typing shows at once");
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "ab2");
    assert_eq!(b.text(), "ab2");
}

#[test]
fn restoring_a_version_and_undoing_the_restore() {
    let mut relay = Relay::new(doc_with("first"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    let v = keep_version(&mut relay, &a);
    end(&mut b);
    b.type_text(" second");
    relay.settle(&mut [&mut a, &mut b]);
    a.replica.restore(v);
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "first");
    assert_eq!(b.text(), "first");
    a.act("edit.undo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "first second");
    assert_eq!(b.text(), "first second");
}

#[test]
fn a_refused_edit_leaves_and_the_next_one_goes() {
    let relay = Relay::new(doc_with("Hi"));
    let mut a = Peer::join("a", "Ann", &relay);
    end(&mut a);
    a.type_text("!");
    a.type_text("?");
    let first = a.replica.next_to_send().unwrap();
    a.replica.refused(first.seq, &mut a.visible);
    assert_eq!(a.text(), "Hi?");
    let next = a.replica.next_to_send().unwrap();
    assert_eq!(next.params, json!({"text": "?"}));
}

#[test]
fn undo_out_of_reach_changes_nothing_and_does_not_panic() {
    let mut relay = Relay::new(doc_with("x"));
    let mut a = Peer::join("a", "Ann", &relay);
    end(&mut a);
    a.type_text("y");
    relay.settle(&mut [&mut a]);
    // Bob joins from a snapshot taken after Ann's edit: it is out of his reach.
    relay.base = a.replica.confirmed().clone();
    relay.base_rev = relay.rev();
    relay.log.clear();
    let mut b = Peer::join("b", "Bob", &relay);
    a.act("edit.undo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "x");
    assert_eq!(b.text(), "xy", "out of reach: the fingerprint check resyncs him");
}

#[test]
fn an_undo_sent_again_after_a_reconnect_keeps_its_edit() {
    let mut relay = Relay::new(doc_with("x"));
    let mut a = Peer::join("a", "Ann", &relay);
    end(&mut a);
    a.type_text("1");
    relay.settle(&mut [&mut a]);
    end(&mut a);
    a.type_text("2");
    relay.settle(&mut [&mut a]);
    a.act("edit.undo", json!({}));
    let first = a.replica.next_to_send().unwrap();
    assert_eq!(first.params, json!({"target": 2, "more": [1]}), "typed together: one step");
    // The connection drops before the relay got it; the Welcome resumes and it goes again.
    let again = a.replica.resume(relay.rev(), None, Vec::new(), None, &mut a.visible).unwrap();
    assert_eq!(again[0].params, json!({"target": 2, "more": [1]}));
    assert!(a.replica.can_redo());
}

#[test]
fn a_run_of_typing_is_one_undo() {
    let mut relay = Relay::new(doc_with("Start"));
    let mut a = Peer::join("a", "Ann", &relay);
    let mut b = Peer::join("b", "Bob", &relay);
    end(&mut a);
    for t in [" ", "two", " ", "words", "."] {
        end(&mut a);
        a.type_text(t);
        relay.settle(&mut [&mut a, &mut b]);
    }
    assert_eq!(b.text(), "Start two words.");
    a.act("edit.undo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(a.text(), "Start");
    assert_eq!(b.text(), "Start");
    a.act("edit.redo", json!({}));
    relay.settle(&mut [&mut a, &mut b]);
    assert_eq!(b.text(), "Start two words.");
}
