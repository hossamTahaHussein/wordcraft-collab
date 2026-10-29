//! Co-editing UI the host drives (co-editing spec §7, §9): the Version history window (list,
//! read-only preview, "Restore this version" for editors) and the offline notice ("Reconnecting…"
//! with "Save my copy"). The host fills [`CollabUi`] from the relay and takes back
//! [`CollabRequest`]s; this module draws only.

use egui::{Context, RichText};
use wordcraft_engine::CollabRole;

use crate::WordApp;

/// What the co-editing host tells the UI each frame.
#[derive(Debug, Default)]
pub struct CollabUi {
    /// The document is shared (co-editing is on).
    pub active: bool,
    /// The person has an edit of theirs to undo, or an undone one to redo.
    pub can_undo: bool,
    pub can_redo: bool,
    /// Too long offline or too many edits waiting: the document is read only until they sync.
    pub offline: bool,
    /// The Version history window, when open.
    pub history: Option<VersionHistory>,
    /// What the person asked for, for the host.
    pub requests: Vec<CollabRequest>,
}

#[derive(Debug, Default)]
pub struct VersionHistory {
    /// The versions, newest first; `None` while they load.
    pub rows: Option<Vec<VersionRow>>,
    /// The version being previewed: its revision and its text (`None` while it loads, empty
    /// when it is gone).
    pub preview: Option<(u64, Option<String>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionRow {
    pub rev: u64,
    /// When it was kept (ISO time).
    pub at: String,
    /// Who edited since the version before.
    pub users: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollabRequest {
    ListVersions,
    Preview(u64),
    Restore(u64),
    /// Save this person's copy of the document (offline), as "<name> (my copy).docx".
    SaveMyCopy,
}

impl WordApp {
    /// File › Version history while co-editing: open the window and ask for the list.
    pub fn open_version_history(&mut self) {
        if !self.collab.active {
            return;
        }
        self.collab.history = Some(VersionHistory::default());
        self.collab.requests.push(CollabRequest::ListVersions);
    }

    /// The host's requests since the last call.
    pub fn take_collab_requests(&mut self) -> Vec<CollabRequest> {
        std::mem::take(&mut self.collab.requests)
    }
}

/// The role's label in the header and the status bar: `None` for an editor.
pub(crate) fn role_label(app: &WordApp) -> Option<&'static str> {
    if !app.session.collab() {
        return None;
    }
    match app.session.collab_role() {
        CollabRole::Edit => None,
        CollabRole::Comment => Some(tl!("Commenting")),
        CollabRole::View => Some(tl!("View only")),
    }
}

/// "2026-10-29T14:05:09.123Z" as "2026-10-29 14:05".
fn short_time(iso: &str) -> String {
    let t = iso.replace('T', " ");
    t.get(..16).unwrap_or(&t).to_string()
}

pub(crate) fn show(app: &mut WordApp, ctx: &Context) {
    if app.collab.offline {
        offline_notice(app, ctx);
    }
    if app.collab.history.is_some() {
        version_history(app, ctx);
    }
}

fn offline_notice(app: &mut WordApp, ctx: &Context) {
    egui::Window::new(tl!("Reconnecting…"))
        .id(egui::Id::unique("collab_offline"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 90.0))
        .show(ctx, |ui| {
            ui.label(tl!("You're offline. The document is read only until your changes sync."));
            if ui.button(tl!("Save my copy")).clicked() {
                app.collab.requests.push(CollabRequest::SaveMyCopy);
            }
        });
}

fn version_history(app: &mut WordApp, ctx: &Context) {
    let mut open = true;
    let editor = app.session.collab_role() == CollabRole::Edit;
    let mut asked = Vec::new();
    egui::Window::new(tl!("Version history"))
        .id(egui::Id::unique("collab_versions"))
        .open(&mut open)
        .default_width(420.0)
        .default_height(420.0)
        .show(ctx, |ui| {
            let Some(history) = app.collab.history.as_ref() else { return };
            match &history.rows {
                None => {
                    ui.label(tl!("Loading…"));
                }
                Some(rows) if rows.is_empty() => {
                    ui.label(tl!("No versions yet. A version is kept when the document is saved."));
                }
                Some(rows) => {
                    egui::ScrollArea::vertical().id_salt("collab_version_rows").max_height(180.0).show(ui, |ui| {
                        for row in rows {
                            let picked = history.preview.as_ref().is_some_and(|(r, _)| *r == row.rev);
                            let people = if row.users.is_empty() { String::new() } else { row.users.join(", ") };
                            let text = format!("{}  {}", short_time(&row.at), people);
                            if ui.selectable_label(picked, text).clicked() {
                                asked.push(CollabRequest::Preview(row.rev));
                            }
                        }
                    });
                }
            }
            if let Some((rev, text)) = &history.preview {
                ui.separator();
                match text {
                    None => {
                        ui.label(tl!("Loading…"));
                    }
                    Some(t) if t.is_empty() => {
                        ui.label(tl!("This version is no longer kept."));
                    }
                    Some(t) => {
                        egui::ScrollArea::vertical().id_salt("collab_version_text").max_height(200.0).show(ui, |ui| {
                            ui.label(RichText::new(t.as_str()));
                        });
                        if editor && ui.button(tl!("Restore this version")).clicked() {
                            asked.push(CollabRequest::Restore(*rev));
                        }
                    }
                }
            }
        });
    for request in asked {
        if let (CollabRequest::Preview(rev), Some(h)) = (&request, app.collab.history.as_mut()) {
            h.preview = Some((*rev, None));
        }
        if matches!(request, CollabRequest::Restore(_)) {
            app.collab.history = None;
        }
        app.collab.requests.push(request);
    }
    if !open {
        app.collab.history = None;
    }
}

#[cfg(test)]
mod tests {
    use wordcraft_doc::Document;
    use wordcraft_engine::{CollabRole, Session};

    use super::*;
    use crate::Services;

    fn app() -> WordApp {
        let mut a = WordApp::new(Session::new(Document::new()), Services::default());
        a.session.set_collab(true);
        a.collab.active = true;
        a
    }

    #[test]
    fn version_history_asks_for_the_list_only_while_co_editing() {
        let mut a = WordApp::new(Session::new(Document::new()), Services::default());
        a.open_version_history();
        assert!(a.collab.history.is_none());
        let mut a = app();
        a.open_version_history();
        assert!(a.collab.history.is_some());
        assert_eq!(a.take_collab_requests(), [CollabRequest::ListVersions]);
    }

    #[test]
    fn the_role_shows_for_viewers_and_commenters() {
        let mut a = app();
        assert_eq!(role_label(&a), None);
        a.session.set_collab_role(CollabRole::View);
        assert_eq!(role_label(&a), Some("View only"));
        a.session.set_collab_role(CollabRole::Comment);
        assert_eq!(role_label(&a), Some("Commenting"));
        a.session.set_collab(false);
        assert_eq!(role_label(&a), None);
    }

    #[test]
    fn short_times() {
        assert_eq!(short_time("2026-10-29T14:05:09.123Z"), "2026-10-29 14:05");
        assert_eq!(short_time("bad"), "bad");
    }
}
