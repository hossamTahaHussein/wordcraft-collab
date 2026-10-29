//! The co-editing outbox. In collab mode each edit a person makes is recorded once — the
//! outermost command that changed the document, so a macro or Repeat records the commands it
//! ran — with the selection it started from and the [`RunCtx`] it ran under, for the co-editing
//! layer to send to the other replicas.

use serde_json::Value;
use wordcraft_doc::StoryRef;
use wordcraft_doc::offsets::offset_of;

use crate::{CmdError, RunCtx, Selection, Session};

/// Commands refused while co-editing: undo works per person through the co-editing layer, and
/// a co-edited document can't be swapped for another one.
pub const COLLAB_DISABLED: &[&str] = &["edit.undo", "edit.undoTo", "edit.redo", "file.new", "file.open", "file.close"];

/// One recorded edit.
#[derive(Clone, Debug, PartialEq)]
pub struct Recorded {
    pub id: String,
    pub params: Value,
    pub sel_before: Selection,
    /// `(story, anchor offset, focus offset)` of `sel_before` (see `wordcraft_doc::offsets`);
    /// `None` when the selection isn't a valid position in one story.
    pub anchor: Option<(StoryRef, usize, usize)>,
    pub ctx: RunCtx,
}

#[derive(Debug)]
pub(crate) struct CollabState {
    on: bool,
    recording: bool,
    /// A recorded command is running: the commands it runs aren't recorded again.
    busy: bool,
    outbox: Vec<Recorded>,
}

impl Default for CollabState {
    fn default() -> Self {
        CollabState { on: false, recording: true, busy: false, outbox: Vec::new() }
    }
}

impl CollabState {
    pub(crate) fn outbox_len(&self) -> usize {
        self.outbox.len()
    }

    /// Drop edits recorded after a snapshot that is being put back.
    pub(crate) fn truncate_outbox(&mut self, len: usize) {
        self.outbox.truncate(len);
    }
}

impl Session {
    /// Turn collab mode on or off. While on, edits are recorded and [`COLLAB_DISABLED`] commands
    /// are refused.
    pub fn set_collab(&mut self, on: bool) {
        self.collab.on = on;
        self.collab.outbox.clear();
    }

    pub fn collab(&self) -> bool {
        self.collab.on
    }

    /// Pause recording (while the co-editing layer replays commands it already has).
    pub fn set_recording(&mut self, on: bool) {
        self.collab.recording = on;
    }

    /// The edits recorded since the last call.
    pub fn take_outbox(&mut self) -> Vec<Recorded> {
        std::mem::take(&mut self.collab.outbox)
    }

    /// [`Session::run`]'s collab-mode wrapper around `run`: refuses what co-editing can't do and
    /// records the outermost command that changed the document.
    pub(crate) fn run_collab(
        &mut self,
        id: &str,
        params: &Value,
        run: impl FnOnce(&mut Session) -> Result<Value, CmdError>,
    ) -> Result<Value, CmdError> {
        if !self.collab.on {
            return run(self);
        }
        if COLLAB_DISABLED.contains(&id) || crate::run_ctx::NONDETERMINISTIC.iter().any(|(nd, _)| *nd == id) {
            return Err(CmdError::Disabled(format!("{id}: Not available while co-editing")));
        }
        let mutates = self.registry.get(id).is_some_and(|spec| spec.mutates);
        if !mutates || !self.collab.recording || self.collab.busy {
            return run(self);
        }
        let sel_before = self.sel.clone();
        let anchor = anchor_of(self, &sel_before);
        let ctx = self.run_ctx();
        let rev_before = self.rev();
        self.collab.busy = true;
        let result = crate::with_clock(ctx.time, || run(self));
        self.collab.busy = false;
        if result.is_ok() && self.rev() != rev_before {
            self.collab.outbox.push(Recorded { id: id.to_string(), params: params.clone(), sel_before, anchor, ctx });
        }
        result
    }
}

fn anchor_of(s: &Session, sel: &Selection) -> Option<(StoryRef, usize, usize)> {
    if sel.anchor.story != sel.focus.story {
        return None;
    }
    Some((sel.anchor.story, offset_of(&s.doc, &sel.anchor)?, offset_of(&s.doc, &sel.focus)?))
}
