//! The co-editing outbox. In collab mode each edit a person makes is recorded once — the
//! outermost command that changed the document, so a macro or Repeat records the commands it
//! ran — with the selection it started from and the [`RunCtx`] it ran under, for the co-editing
//! layer to send to the other replicas.

use serde_json::Value;
use wordcraft_doc::StoryRef;
use wordcraft_doc::offsets::offset_of;

use crate::{CmdError, RunCtx, Selection, Session};

/// Commands refused while co-editing: undo to a point of this session's own history (undo and
/// redo themselves go to the co-editing layer, see [`COLLAB_UNDO`]), a co-edited document can't
/// be swapped for another one, and what pastes from this session's own stores (the Clipboard
/// pane, the Spike, Quick Parts, the format painter) can't be replayed by the other replicas.
pub const COLLAB_DISABLED: &[&str] = &[
    "edit.undoTo",
    "file.new",
    "file.open",
    "file.close",
    "file.newFromTemplate",
    "edit.pasteClipboardItem",
    "edit.pasteAllClipboard",
    "edit.insertSpike",
    "edit.pasteFormat",
    "insert.quickParts",
    "insert.autoText",
];

/// Undo and redo while co-editing: recorded for the co-editing layer, which undoes this person's
/// own most recent edit for everyone (selective undo), instead of running on this session's
/// snapshot history.
pub const COLLAB_UNDO: &[&str] = &["edit.undo", "edit.redo"];

/// Commands that paste this session's clipboard: their record carries it ([`RunCtx::clipboard`]).
const CLIPBOARD_READERS: &[&str] = &["edit.paste", "edit.pasteSpecial", "edit.pasteText", "edit.pasteMerge"];

/// The largest clipboard a recorded paste may carry, in bytes of JSON (the relay takes edits of
/// up to 256 KB).
const MAX_CLIPBOARD_BYTES: usize = 200 * 1024;

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
        if matches!(id, "file.new" | "file.open" | "file.close" | "file.newFromTemplate") {
            return Err(CmdError::Disabled(format!(
                "{id}: Opening or closing a document isn't available while co-editing: it would replace the shared document for everyone"
            )));
        }
        if COLLAB_UNDO.contains(&id) && self.collab.recording && !self.collab.busy {
            if self.viewing {
                return Err(CmdError::Disabled(format!("{id}: {}", crate::cmd::protect::VIEWING)));
            }
            let sel_before = self.sel.clone();
            let anchor = anchor_of(self, &sel_before);
            let ctx = self.run_ctx();
            self.collab.outbox.push(Recorded { id: id.to_string(), params: params.clone(), sel_before, anchor, ctx });
            return Ok(serde_json::json!({"queued": true}));
        }
        if COLLAB_DISABLED.contains(&id) || crate::run_ctx::NONDETERMINISTIC.iter().any(|(nd, _)| *nd == id) {
            return Err(CmdError::Disabled(format!("{id}: Not available while co-editing")));
        }
        let mutates = self.registry.get(id).is_some_and(|spec| spec.mutates);
        if !mutates || !self.collab.recording || self.collab.busy {
            return run(self);
        }
        let refuse = |why: &str| Err(CmdError::Disabled(format!("{id}: {why} isn't available while co-editing")));
        if !self.also_selected.is_empty() || self.column.is_some() {
            return refuse("Editing several selections at once");
        }
        let sel_before = self.sel.clone();
        let Some(anchor) = anchor_of(self, &sel_before) else { return refuse("Editing a selection across stories") };
        let anchor = Some(anchor);
        let mut ctx = self.run_ctx();
        if CLIPBOARD_READERS.contains(&id) {
            ctx.clipboard = self.clipboard.clone();
            let size = ctx.clipboard.as_ref().and_then(|f| serde_json::to_vec(f).ok()).map_or(0, |v| v.len());
            if size > MAX_CLIPBOARD_BYTES {
                return refuse("Pasting this much at once");
            }
        }
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
