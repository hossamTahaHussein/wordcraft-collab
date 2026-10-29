//! Co-editing UI the host drives (co-editing spec §7, §9): the Version history window (list,
//! read-only preview, "Restore this version" for editors) and the offline notice ("Reconnecting…"
//! with "Save my copy"). The host fills [`CollabUi`] from the relay and takes back
//! [`CollabRequest`]s; this module draws only.

use egui::Context;
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
    /// The header's save indicator; `None` when not co-editing.
    pub sync: Option<SyncState>,
    /// The Version history window, when open.
    pub history: Option<VersionHistory>,
    /// What the person asked for, for the host.
    pub requests: Vec<CollabRequest>,
}

/// Whether this person's edits have reached the others (the header shows it, as Google Docs and
/// Word for the web do).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// Edits on their way to the relay.
    Saving,
    /// Everything confirmed.
    Saved,
    /// Not connected: edits wait and sync on reconnecting.
    Offline,
}

pub(crate) fn sync_label(s: SyncState) -> &'static str {
    match s {
        SyncState::Saving => tl!("Saving…"),
        SyncState::Saved => tl!("All changes saved"),
        SyncState::Offline => tl!("Offline — changes will sync"),
    }
}

#[derive(Debug, Default)]
pub struct VersionHistory {
    /// The versions, newest first; `None` while they load.
    pub rows: Option<Vec<VersionRow>>,
    /// The version being previewed: its revision and its document (`None` while it loads).
    pub preview: Option<(u64, Option<VersionPreview>)>,
}

/// A kept version, shown as its pages (read only).
pub struct VersionPreview {
    /// `None` when the version is gone or can't be read.
    doc: Option<wordcraft_doc::Document>,
    /// Its pages, drawn on first show.
    pages: Option<Vec<egui::TextureHandle>>,
}

impl std::fmt::Debug for VersionPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VersionPreview").field("doc", &self.doc.is_some()).field("pages", &self.pages.as_ref().map(Vec::len)).finish()
    }
}

impl VersionPreview {
    pub fn new(doc: Option<wordcraft_doc::Document>) -> VersionPreview {
        VersionPreview { doc, pages: None }
    }

    /// The pages as pictures `width` points wide (the first [`PREVIEW_PAGES`]).
    fn pages(&mut self, ctx: &Context, width: f32) -> &[egui::TextureHandle] {
        if self.pages.is_none() {
            let mut out = Vec::new();
            if let Some(doc) = &self.doc {
                let s = wordcraft_engine::Session::new(doc.clone());
                let layout = s.export_layout();
                let ppp = ctx.pixels_per_point();
                for (i, page) in layout.pages.iter().take(PREVIEW_PAGES).enumerate() {
                    let img = wordcraft_render::render_page(&s.doc, page, width / page.w.max(1.0) * ppp, &crate::canvas::screen_render_options());
                    let color = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.to_straight());
                    out.push(ctx.load_texture(format!("collab_version_page_{i}"), color, egui::TextureOptions::LINEAR));
                }
            }
            self.pages = Some(out);
        }
        self.pages.as_deref().unwrap_or_default()
    }
}

/// How many pages a version's preview shows.
const PREVIEW_PAGES: usize = 20;
/// The preview's page width, in points.
const PREVIEW_WIDTH: f32 = 380.0;

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
        CollabRole::Suggest => Some(tl!("Suggesting")),
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
            let rows_view = history.rows.clone();
            let picked_rev = history.preview.as_ref().map(|(r, _)| *r);
            match &rows_view {
                None => {
                    ui.label(tl!("Loading…"));
                }
                Some(rows) if rows.is_empty() => {
                    ui.label(tl!("No versions yet. A version is kept when the document is saved."));
                }
                Some(rows) => {
                    egui::ScrollArea::vertical().id_salt("collab_version_rows").max_height(180.0).show(ui, |ui| {
                        for row in rows {
                            let picked = picked_rev == Some(row.rev);
                            let people = if row.users.is_empty() { String::new() } else { row.users.join(", ") };
                            let text = format!("{}  {}", short_time(&row.at), people);
                            if ui.selectable_label(picked, text).clicked() {
                                asked.push(CollabRequest::Preview(row.rev));
                            }
                        }
                    });
                }
            }
            let ctx = ui.ctx().clone();
            if let Some((rev, preview)) = app.collab.history.as_mut().and_then(|h| h.preview.as_mut()) {
                ui.separator();
                match preview {
                    None => {
                        ui.label(tl!("Loading…"));
                    }
                    Some(p) if p.doc.is_none() => {
                        ui.label(tl!("This version is no longer kept."));
                    }
                    Some(p) => {
                        if editor && ui.button(tl!("Restore this version")).clicked() {
                            asked.push(CollabRequest::Restore(*rev));
                        }
                        egui::ScrollArea::vertical().id_salt("collab_version_pages").max_height(420.0).show(ui, |ui| {
                            for tex in p.pages(&ctx, PREVIEW_WIDTH) {
                                let size = tex.size_vec2() / ctx.pixels_per_point();
                                ui.add(egui::Image::new((tex.id(), size)));
                                ui.add_space(6.0);
                            }
                        });
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
    fn the_save_indicator_and_suggesting_label() {
        assert_eq!(sync_label(SyncState::Saving), "Saving…");
        assert_eq!(sync_label(SyncState::Offline), "Offline — changes will sync");
        let mut a = app();
        a.session.set_collab_role(CollabRole::Suggest);
        assert_eq!(role_label(&a), Some("Suggesting"));
        assert!(a.session.tracking());
    }

    #[test]
    fn a_version_previews_as_its_pages() {
        let mut s = Session::new(Document::new());
        s.run("text.insert", &serde_json::json!({"text": "Kept"})).unwrap();
        let ctx = Context::default();
        let mut p = VersionPreview::new(Some(s.doc));
        assert_eq!(p.pages(&ctx, PREVIEW_WIDTH).len(), 1);
        assert!(VersionPreview::new(None).pages(&ctx, PREVIEW_WIDTH).is_empty());
    }

    #[test]
    fn short_times() {
        assert_eq!(short_time("2026-10-29T14:05:09.123Z"), "2026-10-29 14:05");
        assert_eq!(short_time("bad"), "bad");
    }
}
