//! A deterministic context for running a command: co-editing replays every person's commands
//! on every replica, so a command must give the same document wherever it runs. The context
//! fixes what a command would otherwise read from the session it happens to run in: who is
//! editing, the clock, whether their changes are tracked, and the preferences that change what
//! typing does.

use std::cell::Cell;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cmd::tools::AutoCorrectPrefs;
use crate::{CmdError, Session};

thread_local! {
    static CLOCK: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Run `f` with [`crate::cmd::now_unix`] reading `time` (seconds since the Unix epoch). A
/// nested call keeps the outer time, so commands that run commands see one clock.
pub fn with_clock<R>(time: u64, f: impl FnOnce() -> R) -> R {
    let outer = CLOCK.get();
    if outer.is_none() {
        CLOCK.set(Some(time));
    }
    let result = f();
    CLOCK.set(outer);
    result
}

/// The time [`with_clock`] set, if a command is running under one.
pub(crate) fn clock_override() -> Option<u64> {
    CLOCK.get()
}

/// The editing preferences that change what a command does to the document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EditingPrefs {
    pub autocorrect: AutoCorrectPrefs,
    pub typing_replaces_selection: bool,
    pub overtype: bool,
}

/// Everything a command reads from outside the document, fixed for one run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RunCtx {
    pub author: String,
    pub initials: String,
    /// Seconds since the Unix epoch.
    pub time: u64,
    /// Track this person's changes (their Suggesting mode).
    pub track: bool,
    pub editing: EditingPrefs,
    /// The language new text is written in ([`Session::editing_lang`]): case rules follow it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editing_lang: Option<String>,
    /// The person's own clipboard, for a command that pastes it (otherwise none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<wordcraft_doc::edit::Fragment>,
}

impl Session {
    /// The context a command run now in this session would have.
    pub fn run_ctx(&self) -> RunCtx {
        RunCtx {
            author: self.author.clone(),
            initials: self.initials.clone(),
            time: crate::cmd::now_unix(),
            track: self.doc.settings.track_changes,
            editing: EditingPrefs {
                autocorrect: self.prefs.autocorrect.clone(),
                typing_replaces_selection: self.prefs.typing_replaces_selection,
                overtype: self.prefs.overtype,
            },
            editing_lang: self.editing_lang.clone(),
            clipboard: None,
        }
    }

    /// [`Session::run`] under `ctx`: its author, clock, tracking, language, clipboard and editing preferences apply
    /// for this command only, and the session's own are put back afterwards. Tracking stays as
    /// the command left it when the command itself switched it (Review › Track Changes).
    pub fn run_with(&mut self, ctx: &RunCtx, id: &str, params: &Value) -> Result<Value, CmdError> {
        let author = std::mem::replace(&mut self.author, ctx.author.clone());
        let initials = std::mem::replace(&mut self.initials, ctx.initials.clone());
        let autocorrect = std::mem::replace(&mut self.prefs.autocorrect, ctx.editing.autocorrect.clone());
        let replaces = std::mem::replace(&mut self.prefs.typing_replaces_selection, ctx.editing.typing_replaces_selection);
        let overtype = std::mem::replace(&mut self.prefs.overtype, ctx.editing.overtype);
        let track = std::mem::replace(&mut self.doc.settings.track_changes, ctx.track);
        let lang = std::mem::replace(&mut self.editing_lang, ctx.editing_lang.clone());
        let clipboard = std::mem::replace(&mut self.clipboard, ctx.clipboard.clone());
        let result = with_clock(ctx.time, || self.run(id, params));
        self.editing_lang = lang;
        self.clipboard = clipboard;
        self.author = author;
        self.initials = initials;
        self.prefs.autocorrect = autocorrect;
        self.prefs.typing_replaces_selection = replaces;
        self.prefs.overtype = overtype;
        if self.doc.settings.track_changes == ctx.track {
            self.doc.settings.track_changes = track;
        }
        result
    }
}

/// Commands that don't give the same document on every replica even under [`Session::run_with`]
/// (id, why). Co-editing refuses them.
pub const NONDETERMINISTIC: &[(&str, &str)] = &[];
