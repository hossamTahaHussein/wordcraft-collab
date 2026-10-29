//! One person's copy of a co-edited document (spec §4).
//!
//! The replica keeps the *confirmed* document — the relay's history up to [`Replica::rev`],
//! identical on every replica — beside the *visible* one the person edits, which is confirmed
//! plus their *pending* edits (sent or not yet confirmed). Their own edits show at once; the
//! relay's order decides. When someone else's edit arrives first, the visible document is
//! rebuilt: confirmed, then the pending edits re-run with their selections moved through the
//! change.
//!
//! An edit's selection is recorded against the confirmed document at its `base` revision, and
//! a replica has at most one edit in flight: it sends the next only after the relay has
//! confirmed the last. So the edits between an incoming edit's base and now are all someone
//! else's, and every replica moves its selection through exactly those changes ([`History`])
//! before running it.

use std::collections::VecDeque;

use serde_json::Value;
use wordcraft_doc::offsets::{linear_text, offset_of, pos_at};
use wordcraft_doc::{Document, Pos, StoryRef};
use wordcraft_engine::{Selection, Session};

use crate::hash::{HASH_EVERY, doc_hash};
use crate::mapping::{Span, changes, map, span};
use crate::proto::{Anchor, HistEntry, HistSpan, Op, OpKind, Outbound};

/// How many revisions of changes a replica keeps for moving late edits' selections; an edit
/// whose base is older needs a resync.
const HISTORY_REVS: usize = 2000;

/// The longest merged run of typing, in bytes.
const MAX_MERGED_TEXT: usize = 4096;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CollabError {
    #[error("missed edits: have revision {have}, got {got}")]
    Gap { have: u64, got: u64 },
    #[error("edit based on revision {base}, older than this replica's history (from {oldest})")]
    TooOld { base: u64, oldest: u64 },
    #[error("bad snapshot: {0}")]
    Snapshot(String),
}

/// What [`Replica::receive`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// The relay confirmed this replica's own edit; the visible document already had it.
    Echo,
    /// Someone else's edit; `dropped_pending` of this person's unconfirmed edits could no longer
    /// be applied after it and were dropped.
    Remote { dropped_pending: usize },
    /// An edit this replica already has (sent again after a reconnect): nothing to do.
    Duplicate,
}

/// The changes each recent revision made, per story.
#[derive(Default)]
struct History {
    /// Revisions before this have no recorded changes.
    oldest: u64,
    revs: VecDeque<(u64, Vec<(StoryRef, Span)>)>,
}

impl History {
    /// The history a snapshot at `rev` carried.
    fn from_entries(rev: u64, entries: Vec<HistEntry>) -> History {
        let entries: Vec<HistEntry> = entries.into_iter().filter(|e| e.rev <= rev).collect();
        let oldest = entries.first().map_or(rev, |e| e.rev.saturating_sub(1));
        let revs = entries
            .into_iter()
            .map(|e| (e.rev, e.spans.into_iter().map(|s| (s.story, Span { start: s.start, old_end: s.old_end, new_end: s.new_end })).collect()))
            .collect();
        History { oldest, revs }
    }

    fn entries(&self) -> Vec<HistEntry> {
        self.revs
            .iter()
            .map(|(rev, spans)| HistEntry {
                rev: *rev,
                spans: spans.iter().map(|(story, s)| HistSpan { story: *story, start: s.start, old_end: s.old_end, new_end: s.new_end }).collect(),
            })
            .collect()
    }

    fn push(&mut self, rev: u64, spans: Vec<(StoryRef, Span)>) {
        self.revs.push_back((rev, spans));
        while self.revs.len() > HISTORY_REVS {
            if let Some((r, _)) = self.revs.pop_front() {
                self.oldest = r;
            }
        }
    }

    /// Move a selection made at `base` through every change after it.
    fn rebase(&self, sel: &Anchor, base: u64) -> Result<Anchor, CollabError> {
        if base < self.oldest {
            return Err(CollabError::TooOld { base, oldest: self.oldest });
        }
        let mut out = sel.clone();
        for (_, spans) in self.revs.iter().filter(|(r, _)| *r > base) {
            for (story, s) in spans {
                if *story == out.story {
                    out.anchor = map(s, out.anchor, false);
                    out.head = map(s, out.head, false);
                }
            }
        }
        Ok(out)
    }
}

pub struct Replica {
    client: String,
    rev: u64,
    confirmed: Session,
    /// Replays the old visible chain while rebasing.
    scratch: Session,
    pending: VecDeque<Op>,
    /// Whether `pending[0]` has been sent.
    in_flight: bool,
    next_seq: u64,
    history: History,
    /// The last revision whose fingerprint was reported.
    hashed: u64,
    /// Where the last remote edit left its author's selection (client, offsets in the confirmed
    /// document).
    author_sel: Option<(String, Anchor)>,
}

impl Replica {
    /// Start from the relay's snapshot at `rev` and the edits after it.
    pub fn join(client: String, rev: u64, snapshot: Document, tail: Vec<Op>) -> Result<Replica, CollabError> {
        Self::join_with_history(client, rev, snapshot, Vec::new(), tail)
    }

    /// [`Replica::join`] with the changes of the revisions before the snapshot, so edits in
    /// `tail` based on an earlier revision can be placed.
    pub fn join_with_history(client: String, rev: u64, snapshot: Document, history: Vec<HistEntry>, tail: Vec<Op>) -> Result<Replica, CollabError> {
        let mut replica = Replica {
            client,
            rev,
            confirmed: headless(snapshot),
            scratch: headless(Document::new()),
            pending: VecDeque::new(),
            in_flight: false,
            next_seq: 1,
            history: History::from_entries(rev, history),
            hashed: rev,
            author_sel: None,
        };
        for op in tail {
            replica.check_next(&op)?;
            replica.apply_confirmed(&op)?;
        }
        Ok(replica)
    }

    /// Show the confirmed document in `visible` (at its start) and switch it to collab mode.
    pub fn attach(&self, visible: &mut Session) {
        visible.doc = self.confirmed.doc.clone();
        visible.sel = Selection::caret(Pos::body(0, 0));
        visible.clamp_selection();
        visible.set_collab(true);
        visible.touch();
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn client(&self) -> &str {
        &self.client
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn confirmed(&self) -> &Document {
        &self.confirmed.doc
    }

    /// The confirmed document's fingerprint, once per [`HASH_EVERY`] revisions.
    pub fn hash_due(&mut self) -> Option<Outbound> {
        let at = self.rev - self.rev % HASH_EVERY;
        if at == 0 || at <= self.hashed || at != self.rev {
            return None;
        }
        self.hashed = at;
        Some(Outbound::Hash { rev: at, h: doc_hash(&self.confirmed.doc) })
    }

    /// The confirmed document in WordCraft's native JSON, for the relay to give new joiners.
    pub fn snapshot(&self) -> Result<Outbound, CollabError> {
        let bytes = wordcraft_engine::io::save_bytes("doc.json", &self.confirmed.doc).map_err(CollabError::Snapshot)?;
        let doc = String::from_utf8(bytes).map_err(|e| CollabError::Snapshot(e.to_string()))?;
        Ok(Outbound::Snapshot { rev: self.rev, doc, history: self.history.entries() })
    }

    /// Turn the edits the person just made in `visible` into pending ops.
    pub fn drain_local(&mut self, visible: &mut Session) {
        for r in visible.take_outbox() {
            let sel = match r.anchor {
                Some((story, anchor, head)) => Anchor { story, anchor, head },
                None => Anchor::default(),
            };
            self.pending.push_back(Op {
                client: self.client.clone(),
                seq: self.next_seq,
                base: 0,
                kind: OpKind::Cmd,
                cmd: r.id,
                params: r.params,
                sel,
                ctx: r.ctx,
                rev: None,
                user: None,
            });
            self.next_seq += 1;
        }
    }

    /// The next op to send to the relay, if none is in flight: plain typing still waiting is
    /// merged into one edit first.
    pub fn next_to_send(&mut self) -> Option<Op> {
        if self.in_flight {
            return None;
        }
        self.merge_typing();
        let rev = self.rev;
        let op = self.pending.front_mut()?;
        op.base = rev;
        self.in_flight = true;
        Some(op.clone())
    }

    /// Apply an op from the relay.
    pub fn receive(&mut self, op: Op, visible: &mut Session) -> Result<Applied, CollabError> {
        if op.rev.is_some_and(|r| r <= self.rev) {
            return Ok(Applied::Duplicate);
        }
        self.check_next(&op)?;
        // Edits the person made since the host last collected them were made on the visible
        // document as it is now: take them before it is rebuilt.
        self.drain_local(visible);
        self.apply_from_relay(op, visible)
    }

    /// One relay edit: this replica's own (the first pending one) or someone else's.
    fn apply_from_relay(&mut self, op: Op, visible: &mut Session) -> Result<Applied, CollabError> {
        let own = op.client == self.client && self.pending.front().is_some_and(|p| p.seq == op.seq);
        if own {
            self.apply_confirmed(&op)?;
            self.pending.pop_front();
            self.in_flight = false;
            return Ok(Applied::Echo);
        }
        let before = self.confirmed.doc.clone();
        let spans = self.apply_confirmed(&op)?;
        self.author_sel = anchor_of(&self.confirmed.doc, &self.confirmed.sel).map(|a| (op.client.clone(), a));
        let hint = spans.iter().find(|(story, _)| *story == op.sel.story).map(|(story, s)| (*story, s.start));
        let dropped_pending = if self.pending.is_empty() { self.follow(visible, &spans) } else { self.rebuild(visible, before, hint) };
        Ok(Applied::Remote { dropped_pending })
    }

    /// Where the last remote edit left its author's selection: `(client, offsets in the confirmed
    /// document)`. Taken once.
    pub fn take_author_selection(&mut self) -> Option<(String, Anchor)> {
        self.author_sel.take()
    }

    /// A selection made at confirmed revision `base`, moved through every edit since, in the
    /// confirmed document's offsets; `None` when `base` is newer than this replica or older than
    /// its history.
    pub fn rebase_anchor(&self, sel: &Anchor, base: u64) -> Option<Anchor> {
        if base > self.rev {
            return None;
        }
        self.history.rebase(sel, base).ok()
    }

    /// Skip revision `rev`, which no replica could read (a malformed edit): every replica skips
    /// it the same way, as an edit that changed nothing.
    pub fn skip(&mut self, rev: u64) -> Result<(), CollabError> {
        if rev != self.rev + 1 {
            return Err(CollabError::Gap { have: self.rev, got: rev });
        }
        self.rev = rev;
        self.history.push(rev, Vec::new());
        Ok(())
    }

    /// After a reconnect (`Welcome`) or a `Resync`: start again from `snapshot` at `rev` if
    /// given — `seen` is the last of this replica's edits the snapshot holds, which aren't sent
    /// again — then apply `tail` one edit at a time, exactly as if it had arrived live. Returns
    /// the next edit to send (the rest follow one at a time).
    pub fn resume(
        &mut self,
        rev: u64,
        snapshot: Option<(Document, Vec<HistEntry>)>,
        tail: Vec<Op>,
        seen: Option<u64>,
        visible: &mut Session,
    ) -> Result<Vec<Op>, CollabError> {
        self.drain_local(visible);
        self.in_flight = false;
        if let Some((doc, history)) = snapshot {
            let before = std::mem::replace(&mut self.confirmed, headless(doc)).doc;
            self.rev = rev;
            self.history = History::from_entries(rev, history);
            self.hashed = rev;
            let seen = seen.unwrap_or(0);
            self.rebuild_skipping(visible, before, None, |op| op.seq <= seen);
        }
        for op in tail {
            if op.rev.is_some_and(|r| r <= self.rev) {
                continue;
            }
            self.check_next(&op)?;
            self.apply_from_relay(op, visible)?;
        }
        Ok(self.next_to_send().into_iter().collect())
    }

    fn check_next(&self, op: &Op) -> Result<(), CollabError> {
        let got = op.rev.unwrap_or(0);
        if got != self.rev + 1 {
            return Err(CollabError::Gap { have: self.rev, got });
        }
        Ok(())
    }

    /// Run a relay op on the confirmed document and record what it changed.
    fn apply_confirmed(&mut self, op: &Op) -> Result<Vec<(StoryRef, Span)>, CollabError> {
        let sel = self.history.rebase(&op.sel, op.base)?;
        let before = self.confirmed.doc.clone();
        // A command that fails here fails on every replica: it is skipped everywhere.
        let _ = run_at(&mut self.confirmed, op, &sel);
        let spans = changes(&before, &self.confirmed.doc, Some((sel.story, sel.anchor.min(sel.head))));
        self.rev += 1;
        self.history.push(self.rev, spans.clone());
        Ok(spans)
    }

    /// Nothing pending: the visible document becomes the confirmed one, and the person's
    /// selection moves through the change.
    fn follow(&mut self, visible: &mut Session, spans: &[(StoryRef, Span)]) -> usize {
        let sel = anchor_of(&visible.doc, &visible.sel).map(|mut a| {
            for (story, s) in spans {
                if *story == a.story {
                    a.anchor = map(s, a.anchor, false);
                    a.head = map(s, a.head, false);
                }
            }
            a
        });
        visible.doc = self.confirmed.doc.clone();
        if let Some(a) = sel {
            visible.sel = selection_at(&visible.doc, &a);
        }
        visible.clamp_selection();
        visible.touch();
        0
    }

    /// Rebuild the visible document as confirmed + pending, each pending edit's selection moved
    /// from the old chain (`before` + pending) to the new one. Returns how many pending edits
    /// failed and were dropped (an edit in flight is kept: the relay will confirm it).
    fn rebuild(&mut self, visible: &mut Session, before: Document, hint: Option<(StoryRef, usize)>) -> usize {
        self.rebuild_skipping(visible, before, hint, |_| false)
    }

    /// [`Self::rebuild`], where pending edits matching `confirmed_already` are part of the old
    /// chain but already in the confirmed document: they are replayed on the old chain only and
    /// leave the pending list.
    ///
    /// `hint`: where the remote change happened, in `before`'s offsets — it decides between
    /// places a change of repeated letters could be (`mapping::span_near`), and follows the old
    /// chain's own edits.
    fn rebuild_skipping(
        &mut self,
        visible: &mut Session,
        before: Document,
        mut hint: Option<(StoryRef, usize)>,
        confirmed_already: impl Fn(&Op) -> bool,
    ) -> usize {
        let old_visible = visible.doc.clone();
        let caret = anchor_of(&old_visible, &visible.sel);
        self.scratch.doc = before;
        visible.doc = self.confirmed.doc.clone();
        visible.set_recording(false);
        let mut kept = VecDeque::new();
        let mut dropped = 0;
        for (i, mut op) in std::mem::take(&mut self.pending).into_iter().enumerate() {
            let story = op.sel.story;
            let old_text = linear_text(&self.scratch.doc, story);
            let moved = crate::mapping::diff_near(&old_text, &linear_text(&visible.doc, story), story, hint);
            let new_sel = move_anchor(&op.sel, moved.as_ref());
            let _ = run_at(&mut self.scratch, &op, &op.sel.clone());
            // The remote change's place moves with this edit on the old chain.
            if let Some((hs, h)) = hint
                && hs == story
                && let Some(own) = span(&old_text, &linear_text(&self.scratch.doc, story))
            {
                hint = Some((hs, map(&own, h, false)));
            }
            if confirmed_already(&op) {
                continue;
            }
            let ok = run_at(visible, &op, &new_sel).is_ok();
            op.sel = new_sel;
            if ok || (i == 0 && self.in_flight) {
                kept.push_back(op);
            } else {
                dropped += 1;
            }
        }
        visible.set_recording(true);
        self.pending = kept;
        if let Some(a) = caret {
            let moved = crate::mapping::diff_near(&linear_text(&old_visible, a.story), &linear_text(&visible.doc, a.story), a.story, hint);
            visible.sel = selection_at(&visible.doc, &move_anchor(&a, moved.as_ref()));
        }
        visible.clamp_selection();
        visible.touch();
        dropped
    }

    /// Merge plain typing waiting behind the first pending op into it (not the op in flight):
    /// consecutive `text.insert`s of letters and digits, each right after the last, untracked.
    fn merge_typing(&mut self) {
        while self.pending.len() >= 2 {
            let (Some(a), Some(b)) = (self.pending.front(), self.pending.get(1)) else { return };
            let (Some(ta), Some(tb)) = (plain_typing(a), plain_typing(b)) else { return };
            let start = a.sel.anchor.min(a.sel.head);
            let joins = b.sel.story == a.sel.story
                && b.sel.anchor == b.sel.head
                && b.sel.anchor == start + ta.len()
                && !a.ctx.track
                && !b.ctx.track
                && a.ctx.author == b.ctx.author
                && ta.len() + tb.len() <= MAX_MERGED_TEXT;
            if !joins {
                return;
            }
            let text = format!("{ta}{tb}");
            self.pending.remove(1);
            if let Some(a) = self.pending.front_mut() {
                a.params = serde_json::json!({ "text": text });
            }
        }
    }
}

/// The text of a `text.insert` of letters and digits only (typing that can't trigger
/// AutoCorrect or AutoFormat, so one insert of all of it gives the same document).
fn plain_typing(op: &Op) -> Option<&str> {
    if op.cmd != "text.insert" || op.kind != OpKind::Cmd {
        return None;
    }
    let obj = op.params.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    let text = obj.get("text").and_then(Value::as_str)?;
    (!text.is_empty() && text.chars().all(char::is_alphanumeric)).then_some(text)
}

fn headless(doc: Document) -> Session {
    let mut s = Session::new(doc);
    s.set_recording(false);
    s
}

/// Run `op` in `s` with its context, from `sel`.
fn run_at(s: &mut Session, op: &Op, sel: &Anchor) -> Result<Value, wordcraft_engine::CmdError> {
    s.sel = selection_at(&s.doc, sel);
    s.clamp_selection();
    s.run_with(&op.ctx, &op.cmd, &op.params)
}

fn selection_at(doc: &Document, a: &Anchor) -> Selection {
    Selection { anchor: pos_at(doc, a.story, a.anchor), focus: pos_at(doc, a.story, a.head) }
}

fn anchor_of(doc: &Document, sel: &Selection) -> Option<Anchor> {
    if sel.anchor.story != sel.focus.story {
        return None;
    }
    Some(Anchor { story: sel.anchor.story, anchor: offset_of(doc, &sel.anchor)?, head: offset_of(doc, &sel.focus)? })
}

fn move_anchor(a: &Anchor, s: Option<&Span>) -> Anchor {
    match s {
        Some(s) => Anchor { story: a.story, anchor: map(s, a.anchor, false), head: map(s, a.head, false) },
        None => a.clone(),
    }
}
