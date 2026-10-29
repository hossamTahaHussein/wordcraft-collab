//! Random interleavings of people editing: every replica ends with the same document.

mod support;

use proptest::prelude::*;
use serde_json::json;
use support::{Peer, Relay, doc_with};
use wordcraft_collab::hash::doc_hash;
use wordcraft_collab::replica::Replica;

#[derive(Clone, Debug)]
enum Action {
    Type(usize, &'static str),
    /// Typed, but not collected before the next delivery (the browser's frame order).
    TypeUndrained(usize, &'static str),
    Enter(usize),
    Backspace(usize),
    Delete(usize),
    Move(usize, i8),
    BoldWord(usize),
    ReplaceWord(usize, &'static str),
    Comment(usize),
    Table(usize),
    Send(usize),
    Deliver(usize, usize),
}

const TEXTS: &[&str] = &["a", "bc", "Hello ", "مرحبا", "👋", "x y", "1.", "e\u{301}"];

fn action(peers: usize) -> impl Strategy<Value = Action> {
    let p = 0..peers;
    prop_oneof![
        4 => (p.clone(), prop::sample::select(TEXTS)).prop_map(|(i, t)| Action::Type(i, t)),
        2 => (p.clone(), prop::sample::select(TEXTS)).prop_map(|(i, t)| Action::TypeUndrained(i, t)),
        1 => p.clone().prop_map(Action::Enter),
        1 => p.clone().prop_map(Action::Backspace),
        1 => p.clone().prop_map(Action::Delete),
        2 => (p.clone(), -6i8..6).prop_map(|(i, n)| Action::Move(i, n)),
        1 => p.clone().prop_map(Action::BoldWord),
        1 => (p.clone(), prop::sample::select(TEXTS)).prop_map(|(i, t)| Action::ReplaceWord(i, t)),
        1 => p.clone().prop_map(Action::Comment),
        1 => p.clone().prop_map(Action::Table),
        3 => p.clone().prop_map(Action::Send),
        3 => (p, 1usize..4).prop_map(|(i, k)| Action::Deliver(i, k)),
    ]
}

fn play(peers: &mut [Peer], relay: &mut Relay, a: &Action) {
    match a {
        Action::Type(i, t) => peers[*i].type_text(t),
        Action::TypeUndrained(i, t) => {
            let _ = peers[*i].visible.run("text.insert", &json!({ "text": t }));
        }
        Action::Enter(i) => peers[*i].act("text.newParagraph", json!({})),
        Action::Backspace(i) => peers[*i].act("text.backspace", json!({})),
        Action::Delete(i) => peers[*i].act("text.delete", json!({})),
        Action::Move(i, n) => {
            let id = if *n < 0 { "caret.left" } else { "caret.right" };
            for _ in 0..n.unsigned_abs() {
                peers[*i].act(id, json!({}));
            }
        }
        Action::BoldWord(i) => {
            peers[*i].act("select.word", json!({}));
            peers[*i].act("format.bold", json!({}));
        }
        Action::ReplaceWord(i, t) => {
            peers[*i].act("select.word", json!({}));
            peers[*i].type_text(t);
        }
        Action::Comment(i) => {
            peers[*i].act("select.word", json!({}));
            peers[*i].act("review.newComment", json!({"text": "note"}));
        }
        Action::Table(i) => peers[*i].act("insert.table", json!({"rows": 2, "cols": 2})),
        Action::Send(i) => {
            let mut refs: Vec<&mut Peer> = peers.iter_mut().collect();
            relay.collect(&mut refs, &[*i]);
        }
        Action::Deliver(i, k) => peers[*i].deliver(*k),
    }
}

fn settle(peers: &mut [Peer], relay: &mut Relay) {
    let mut refs: Vec<&mut Peer> = peers.iter_mut().collect();
    relay.settle(&mut refs);
}

fn check_converged(peers: &[Peer], relay: &Relay) -> Result<(), TestCaseError> {
    let fresh = Replica::join("fresh".into(), relay.base_rev, relay.base.clone(), relay.log.clone()).unwrap();
    let want = doc_hash(fresh.confirmed());
    for (i, p) in peers.iter().enumerate() {
        prop_assert_eq!(p.replica.pending_len(), 0, "peer {} has pending edits", i);
        prop_assert_eq!(doc_hash(p.replica.confirmed()), want.clone(), "peer {} confirmed differs from a replay of the log", i);
        prop_assert_eq!(p.text(), p.confirmed_text(), "peer {} visible differs from confirmed", i);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64), failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn random_interleavings_converge(actions in prop::collection::vec(action(3), 1..40)) {
        let mut relay = Relay::new(doc_with("The quick brown fox"));
        let mut peers: Vec<Peer> = ["a", "b", "c"].iter().map(|c| Peer::join(c, &c.to_uppercase(), &relay)).collect();
        for a in &actions {
            play(&mut peers, &mut relay, a);
        }
        settle(&mut peers, &mut relay);
        check_converged(&peers, &relay)?;
    }

    #[test]
    fn late_join_converges(actions in prop::collection::vec(action(2), 1..30)) {
        let mut relay = Relay::new(doc_with("Start"));
        let mut peers: Vec<Peer> = ["a", "b"].iter().map(|c| Peer::join(c, c, &relay)).collect();
        for a in &actions {
            play(&mut peers, &mut relay, a);
        }
        settle(&mut peers, &mut relay);
        // A third person joins from a snapshot of the confirmed document plus no tail.
        let snap = peers[0].replica.confirmed().clone();
        let late = Replica::join("late".into(), relay.rev(), snap, Vec::new()).unwrap();
        prop_assert_eq!(doc_hash(late.confirmed()), doc_hash(peers[1].replica.confirmed()));
    }
}

#[test]
fn hash_is_due_every_50_revisions() {
    let mut relay = Relay::new(doc_with("x"));
    let mut a = Peer::join("a", "A", &relay);
    let mut due = Vec::new();
    for i in 0..120 {
        a.type_text(if i % 2 == 0 { " " } else { "." });
        relay.settle(&mut [&mut a]);
        if let Some(wordcraft_collab::proto::Outbound::Hash { rev, h }) = a.replica.hash_due() {
            assert_eq!(h, doc_hash(a.replica.confirmed()));
            due.push(rev);
        }
    }
    assert_eq!(due, [50, 100]);
}

#[test]
fn snapshot_round_trips() {
    let relay = Relay::new(doc_with("Snap"));
    let a = Peer::join("a", "A", &relay);
    let wordcraft_collab::proto::Outbound::Snapshot { rev, doc } = a.replica.snapshot().unwrap() else { panic!("not a snapshot") };
    assert_eq!(rev, 0);
    let back = wordcraft_engine::io::open_bytes("s.json", doc.as_bytes()).unwrap();
    assert_eq!(doc_hash(&back), doc_hash(a.replica.confirmed()));
}
