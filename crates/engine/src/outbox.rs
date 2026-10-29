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

/// What a suggester may not do: decide on suggestions, or put back a whole version.
pub const SUGGESTER_REFUSED: &[&str] = &["review.accept", "review.acceptAll", "review.reject", "review.rejectAll"];

/// What a suggester may change besides comments (the relay's `suggest-commands.json`): the
/// commands whose whole effect Track Changes records, so Reject All takes it back.
pub const SUGGEST_COMMANDS: &[&str] = &[
    "text.insert",
    "text.backspace",
    "text.delete",
    "text.deleteWordBack",
    "text.deleteWordForward",
    "text.newParagraph",
    "text.lineBreak",
    "text.tab",
    "text.insertTab",
    "text.nbsp",
    "text.nbHyphen",
    "text.optionalHyphen",
    "text.emDash",
    "text.ellipsis",
    "text.copyright",
    "text.registered",
    "text.trademark",
    "text.columnBreak",
    "text.pageBreak",
    "edit.cut",
    "edit.paste",
    "edit.pasteText",
    "edit.replace",
    "edit.replaceAll",
    "insert.symbol",
    "insert.pageBreak",
    "insert.dateTime",
    "format.bold",
    "format.italic",
    "format.underline",
    "format.strikethrough",
    "format.doubleStrikethrough",
    "format.subscript",
    "format.superscript",
    "format.color",
    "format.highlight",
    "format.font",
    "format.size",
    "format.growFont",
    "format.shrinkFont",
    "format.allCaps",
    "format.smallCaps",
    "format.hidden",
    "format.changeCase",
    "format.charStyle",
    "format.spacing",
    "format.position",
    "format.scale",
];

/// What a commenter may change while co-editing (the relay's `comment-commands.json`).
pub const COMMENT_COMMANDS: &[&str] = &["review.newComment", "review.reply", "review.editComment", "review.deleteComment", "review.resolveComment"];

/// What the person may do in a co-edited document (their join token's role).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollabRole {
    #[default]
    Edit,
    /// Comments only.
    Comment,
    /// Everything, but as tracked suggestions others accept or reject (and no accepting or
    /// rejecting themselves).
    Suggest,
    /// Read only.
    View,
}

impl CollabRole {
    /// The relay's name for a role (`edit`, `comment`, `view`); anything else is `edit`.
    pub fn parse(s: &str) -> CollabRole {
        match s {
            "comment" => CollabRole::Comment,
            "suggest" => CollabRole::Suggest,
            "view" => CollabRole::View,
            _ => CollabRole::Edit,
        }
    }
}

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
    role: CollabRole,
    /// The person's own Track Changes while co-editing (Suggesting); `None` follows the
    /// document's setting. Each person suggests or edits on their own.
    track: Option<bool>,
    recording: bool,
    /// A recorded command is running: the commands it runs aren't recorded again.
    busy: bool,
    outbox: Vec<Recorded>,
}

impl Default for CollabState {
    fn default() -> Self {
        CollabState { on: false, role: CollabRole::Edit, track: None, recording: true, busy: false, outbox: Vec::new() }
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

    /// What the person may do while co-editing; the relay refuses the rest anyway, this says so
    /// before the edit shows.
    pub fn set_collab_role(&mut self, role: CollabRole) {
        self.collab.role = role;
        if role == CollabRole::Suggest {
            self.collab.track = Some(true);
        }
    }

    /// Whether this person's edits are tracked: their own Suggesting while co-editing, otherwise
    /// the document's Track Changes.
    pub fn tracking(&self) -> bool {
        match self.collab.track {
            Some(t) if self.collab.on => t,
            _ => self.doc.settings.track_changes,
        }
    }

    /// Set this person's own Suggesting while co-editing (an agent's, a host's choice).
    pub fn set_collab_tracking(&mut self, on: bool) {
        self.collab.track = Some(on);
    }

    pub fn collab_role(&self) -> CollabRole {
        self.collab.role
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
        let person = self.collab.recording && !self.collab.busy;
        if person && self.collab.role == CollabRole::View && (COLLAB_UNDO.contains(&id) || self.registry.get(id).is_some_and(|spec| spec.mutates)) {
            return Err(CmdError::Disabled(format!("{id}: You can only view this document")));
        }
        if COLLAB_UNDO.contains(&id) && person {
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
        // Track Changes while co-editing is the person's own Suggesting: it changes nothing shared.
        if id == "review.trackChanges" && person {
            if self.collab.role == CollabRole::Suggest {
                return Err(CmdError::Disabled(format!("{id}: You can only suggest changes in this document")));
            }
            let on = params.get("value").and_then(Value::as_bool).unwrap_or(!self.tracking());
            self.collab.track = Some(on);
            self.touch();
            return Ok(serde_json::json!({"value": on}));
        }
        let mutates = self.registry.get(id).is_some_and(|spec| spec.mutates);
        if !mutates || !person {
            return run(self);
        }
        // What a viewer may do (inspect, save, export, print) everyone may.
        let reads = crate::cmd::protect::viewing_allows(id, params);
        if self.collab.role == CollabRole::Suggest
            && !reads
            && (SUGGESTER_REFUSED.contains(&id) || !(SUGGEST_COMMANDS.contains(&id) || COMMENT_COMMANDS.contains(&id)))
        {
            return Err(CmdError::Disabled(format!("{id}: You can only suggest changes in this document")));
        }
        if self.collab.role == CollabRole::Comment && !reads && !COMMENT_COMMANDS.contains(&id) {
            return Err(CmdError::Disabled(format!("{id}: You can only comment on this document")));
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
        // The person's own Suggesting applies to this edit, as `ctx.track` does on every replica.
        let doc_track = std::mem::replace(&mut self.doc.settings.track_changes, ctx.track);
        let result = crate::with_clock(ctx.time, || run(self));
        if self.doc.settings.track_changes == ctx.track {
            self.doc.settings.track_changes = doc_track;
        }
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
