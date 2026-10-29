//! An in-memory relay and peers for replica tests: ops travel only when a test says so.

#![allow(dead_code)]

use std::collections::VecDeque;

use serde_json::{Value, json};
use wordcraft_collab::proto::Op;
use wordcraft_collab::replica::{Applied, Replica};
use wordcraft_doc::Document;
use wordcraft_engine::Session;

pub struct Peer {
    pub replica: Replica,
    pub visible: Session,
    /// Relay ops not yet delivered to this peer.
    pub inbox: VecDeque<Op>,
    pub dropped: usize,
}

impl Peer {
    pub fn join(client: &str, author: &str, relay: &Relay) -> Peer {
        let replica = Replica::join(client.into(), relay.base_rev, relay.base.clone(), relay.log.clone()).unwrap();
        let mut visible = Session::new(Document::new());
        visible.author = author.into();
        replica.attach(&mut visible);
        Peer { replica, visible, inbox: VecDeque::new(), dropped: 0 }
    }

    /// Run a command as this person would (on the visible document) and record it.
    pub fn act(&mut self, id: &str, params: Value) {
        let _ = self.visible.run(id, &params);
        self.replica.drain_local(&mut self.visible);
    }

    pub fn type_text(&mut self, text: &str) {
        self.act("text.insert", json!({ "text": text }));
    }

    /// Deliver up to `n` queued relay ops.
    pub fn deliver(&mut self, n: usize) {
        for _ in 0..n {
            let Some(op) = self.inbox.pop_front() else { return };
            match self.replica.receive(op, &mut self.visible).unwrap() {
                Applied::Remote { dropped_pending } => self.dropped += dropped_pending,
                Applied::Echo => {}
            }
        }
    }

    pub fn text(&self) -> String {
        self.visible.doc.plain_text(wordcraft_doc::StoryRef::Body)
    }

    pub fn confirmed_text(&self) -> String {
        self.replica.confirmed().plain_text(wordcraft_doc::StoryRef::Body)
    }
}

pub struct Relay {
    pub base: Document,
    pub base_rev: u64,
    pub log: Vec<Op>,
}

impl Relay {
    pub fn new(doc: Document) -> Relay {
        Relay { base: doc, base_rev: 0, log: Vec::new() }
    }

    pub fn rev(&self) -> u64 {
        self.base_rev + self.log.len() as u64
    }

    /// Take each peer's next op (if any) in the given order, stamp it and queue it for everyone.
    pub fn collect(&mut self, peers: &mut [&mut Peer], order: &[usize]) -> usize {
        let mut n = 0;
        for &i in order {
            let p = &mut *peers[i];
            p.replica.drain_local(&mut p.visible);
            if let Some(mut op) = peers[i].replica.next_to_send() {
                op.rev = Some(self.rev() + 1);
                op.user = Some(op.ctx.author.clone());
                self.log.push(op.clone());
                for p in peers.iter_mut() {
                    p.inbox.push_back(op.clone());
                }
                n += 1;
            }
        }
        n
    }

    /// Send and deliver until nothing moves.
    pub fn settle(&mut self, peers: &mut [&mut Peer]) {
        let order: Vec<usize> = (0..peers.len()).collect();
        loop {
            let sent = self.collect(peers, &order);
            let mut delivered = 0;
            for p in peers.iter_mut() {
                delivered += p.inbox.len();
                p.deliver(usize::MAX);
            }
            if sent == 0 && delivered == 0 {
                return;
            }
        }
    }
}

pub fn doc_with(text: &str) -> Document {
    let mut s = Session::new(Document::new());
    s.run("text.insert", &json!({ "text": text })).unwrap();
    s.doc
}
