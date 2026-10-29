//! Selective undo and redo (co-editing spec §6): undoing one person's edit for everyone, however
//! many edits came after it.
//!
//! Every replica keeps checkpoints of the confirmed document every [`CHECKPOINT_EVERY`] revisions
//! (the last [`KEEP_CHECKPOINTS`]) and the confirmed edits since the oldest. Undoing edit R
//! rebuilds the confirmed document from the newest checkpoint before R: the edits after it are
//! replayed twice, once as they were (the old chain) and once without R (the new chain), and each
//! replayed edit's selection moves through the difference between its two starting documents, so
//! later edits land where they belong. The relay's order is the same everywhere, so is the result.

use std::collections::{BTreeSet, VecDeque};

use wordcraft_doc::Document;
use wordcraft_doc::offsets::linear_text;
use wordcraft_engine::Session;

use crate::proto::{Op, OpKind};

/// A checkpoint every this many revisions.
pub const CHECKPOINT_EVERY: u64 = 100;
/// Checkpoints kept: undo reaches at least `(KEEP_CHECKPOINTS - 1) * CHECKPOINT_EVERY` edits back.
pub const KEEP_CHECKPOINTS: usize = 5;

/// Typing with pauses up to this long (seconds) is one undo step.
pub const TYPING_GAP_S: u64 = 1;

/// One confirmed edit, its selection as it applies in the current chain.
#[derive(Clone, Debug)]
pub(crate) struct Logged {
    pub rev: u64,
    pub op: Op,
}

pub(crate) struct UndoLog {
    /// The confirmed document after each checkpoint revision, oldest first.
    checkpoints: VecDeque<(u64, Document)>,
    /// The confirmed edits after the oldest checkpoint.
    entries: VecDeque<Logged>,
    /// The edits undone (and not redone), by revision.
    excluded: BTreeSet<u64>,
}

impl UndoLog {
    /// Start at the confirmed document `doc` at `rev`.
    pub fn new(rev: u64, doc: &Document) -> UndoLog {
        UndoLog { checkpoints: VecDeque::from([(rev, doc.clone())]), entries: VecDeque::new(), excluded: BTreeSet::new() }
    }

    /// The same edits undone, from a new starting document (after a resync).
    pub fn restart(&mut self, rev: u64, doc: &Document) {
        self.checkpoints = VecDeque::from([(rev, doc.clone())]);
        self.entries.clear();
    }

    /// The oldest revision an undo can reach is the one after this.
    pub fn oldest(&self) -> u64 {
        self.checkpoints.front().map_or(0, |(r, _)| *r)
    }

    /// Edit `op` (its selection as applied) became revision `rev`, leaving `after`.
    pub fn record(&mut self, rev: u64, op: Op, after: &Document) {
        self.entries.push_back(Logged { rev, op });
        if rev.is_multiple_of(CHECKPOINT_EVERY) {
            self.checkpoints.push_back((rev, after.clone()));
        }
        while self.checkpoints.len() > KEEP_CHECKPOINTS {
            self.checkpoints.pop_front();
        }
        let oldest = self.oldest();
        while self.entries.front().is_some_and(|e| e.rev <= oldest) {
            self.entries.pop_front();
        }
    }

    /// The newest edit by `client` an undo can target: an edit or restore, not undone, after the
    /// oldest checkpoint.
    pub fn newest_own(&self, client: &str) -> Option<u64> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.op.client == client && matches!(e.op.kind, OpKind::Cmd | OpKind::Restore) && !self.excluded.contains(&e.rev))
            .map(|e| e.rev)
    }

    /// What one undo by `client` takes back, newest first: their newest edit and, when it is
    /// typing, the run of typing before it (their `text.insert`s each within [`TYPING_GAP_S`]
    /// of the next, with no other edit of theirs between), as Word undoes a typed run at once.
    pub fn own_group(&self, client: &str) -> Vec<u64> {
        let own: Vec<&Logged> = self
            .entries
            .iter()
            .rev()
            .filter(|e| e.op.client == client && matches!(e.op.kind, OpKind::Cmd | OpKind::Restore) && !self.excluded.contains(&e.rev))
            .collect();
        let Some(first) = own.first() else { return Vec::new() };
        let mut group = vec![first.rev];
        let typing = |e: &Logged| e.op.kind == OpKind::Cmd && e.op.cmd == "text.insert";
        if typing(first) {
            for pair in own.windows(2) {
                let (later, earlier) = (pair[0], pair[1]);
                if !typing(earlier) || later.op.ctx.time.saturating_sub(earlier.op.ctx.time) > TYPING_GAP_S {
                    break;
                }
                group.push(earlier.rev);
            }
        }
        group
    }

    /// Whether `rev` is an edit an undo or redo can reach.
    fn reachable(&self, rev: u64) -> bool {
        rev > self.oldest() && self.entries.iter().any(|e| e.rev == rev && matches!(e.op.kind, OpKind::Cmd | OpKind::Restore))
    }

    /// Undo (`undo: true`) or redo the edits `targets`: returns the rebuilt confirmed document,
    /// or `None` when nothing changes (out of reach, or already in that state).
    pub fn apply(&mut self, targets: &[u64], undo: bool) -> Option<Document> {
        let targets: Vec<u64> = targets.iter().copied().filter(|t| self.reachable(*t) && self.excluded.contains(t) != undo).collect();
        let first = *targets.iter().min()?;
        let old_excluded = self.excluded.clone();
        let changed: BTreeSet<u64> = targets.iter().copied().collect();
        for target in targets {
            if undo {
                self.excluded.insert(target);
            } else {
                self.excluded.remove(&target);
            }
        }
        let ci = self.checkpoints.iter().rposition(|(r, _)| *r < first)?;
        let start = self.checkpoints[ci].0;
        let mut old = headless(self.checkpoints[ci].1.clone());
        let mut new = headless(self.checkpoints[ci].1.clone());
        for entry in self.entries.iter_mut().filter(|e| e.rev > start) {
            let story = entry.op.sel.story;
            let moved = crate::mapping::diff_near(&linear_text(&old.doc, story), &linear_text(&new.doc, story), story, None);
            let mut new_sel = crate::replica::move_anchor(&entry.op.sel, moved.as_ref());
            // Redoing a run of typing: each piece goes after the one redone before it, not at
            // the same place (a position at an insertion stays before it otherwise).
            if !undo
                && changed.contains(&entry.rev)
                && let Some(sp) = moved.as_ref()
                && sp.old_end == sp.start
                && new_sel.anchor == sp.start
                && new_sel.head == sp.start
            {
                new_sel.anchor = sp.new_end;
                new_sel.head = sp.new_end;
            }
            if !old_excluded.contains(&entry.rev) {
                apply_logged(&mut old, &entry.op, &entry.op.sel.clone());
            }
            entry.op.sel = new_sel.clone();
            if !self.excluded.contains(&entry.rev) {
                apply_logged(&mut new, &entry.op, &new_sel);
            }
            // Later checkpoints described the old chain.
            if let Some(cp) = self.checkpoints.iter_mut().find(|(r, _)| *r == entry.rev) {
                cp.1 = new.doc.clone();
            }
        }
        Some(new.doc)
    }
}

/// Replay a logged edit: a command runs at its selection, a restore puts its document back;
/// undo and redo themselves change nothing here (the excluded set already says what they did).
fn apply_logged(s: &mut Session, op: &Op, sel: &crate::proto::Anchor) {
    match op.kind {
        OpKind::Cmd => {
            let _ = crate::replica::run_at(s, op, sel);
        }
        OpKind::Restore => crate::replica::restore_into(s, op),
        OpKind::Undo | OpKind::Redo => {}
    }
}

fn headless(doc: Document) -> Session {
    let mut s = Session::new(doc);
    s.set_recording(false);
    s
}
