//! Co-editing through the drawn interface: Version history from the status bar to Restore, and
//! the header's save indicator. The host side (the relay's list, preview and restore) is the
//! web app's; here it is played by filling `app.collab` as the host does.

use egui::epaint::{ClippedShape, Shape};

use super::rtl::{app_in, click, frame, text_rect};
use crate::collab_ui::{CollabRequest, SyncState, VersionHistory, VersionPreview, VersionRow};

const W: f32 = 1280.0;

/// The first text shape whose text contains `part`.
fn text_containing(shapes: &[ClippedShape], part: &str) -> Option<egui::Rect> {
    fn find(shape: &Shape, part: &str) -> Option<egui::Rect> {
        match shape {
            Shape::Text(t) if t.galley.text().contains(part) => Some(t.visual_bounding_rect()),
            Shape::Vec(v) => v.iter().find_map(|s| find(s, part)),
            _ => None,
        }
    }
    shapes.iter().find_map(|c| find(&c.shape, part))
}

fn co_editing() -> (crate::WordApp, egui::Context) {
    let (mut app, ctx) = app_in("en");
    app.session.set_collab(true);
    app.collab.active = true;
    frame(&mut app, &ctx, W);
    (app, ctx)
}

#[test]
fn version_history_from_the_status_bar_to_restore() {
    let (mut app, ctx) = co_editing();
    let shapes = frame(&mut app, &ctx, W);
    let open = text_rect(&shapes, "Version history").expect("Version history in the status bar");
    click(&mut app, &ctx, W, open.center());
    assert!(app.take_collab_requests().contains(&CollabRequest::ListVersions), "the list is asked for");
    // The host answers with the relay's list.
    app.collab.history.as_mut().expect("the window is open").rows =
        Some(vec![VersionRow { rev: 7, at: "2026-10-30T09:15:00Z".into(), users: vec!["Ann Lee".into()] }]);
    let shapes = frame(&mut app, &ctx, W);
    let row = text_containing(&shapes, "Ann Lee").expect("the version is listed");
    click(&mut app, &ctx, W, row.center());
    assert_eq!(app.take_collab_requests(), [CollabRequest::Preview(7)], "picking it asks for its document");
    // The host answers with the version's document.
    let doc = wordcraft_doc::Document::from_text("An older draft");
    app.collab.history.as_mut().unwrap().preview = Some((7, Some(VersionPreview::new(Some(doc)))));
    let shapes = frame(&mut app, &ctx, W);
    let restore = text_rect(&shapes, "Restore this version").expect("an editor can restore it");
    click(&mut app, &ctx, W, restore.center());
    assert_eq!(app.take_collab_requests(), [CollabRequest::Restore(7)]);
    assert!(app.collab.history.is_none(), "the window closes");
}

#[test]
fn a_viewer_previews_versions_but_cannot_restore() {
    let (mut app, ctx) = co_editing();
    app.session.set_collab_role(wordcraft_engine::CollabRole::View);
    app.collab.history = Some(VersionHistory { rows: Some(Vec::new()), preview: None });
    let doc = wordcraft_doc::Document::from_text("An older draft");
    app.collab.history.as_mut().unwrap().preview = Some((7, Some(VersionPreview::new(Some(doc)))));
    let shapes = frame(&mut app, &ctx, W);
    assert!(text_rect(&shapes, "Version history").is_some());
    assert!(text_rect(&shapes, "Restore this version").is_none());
}

#[test]
fn the_header_says_whether_changes_are_saved() {
    let (mut app, ctx) = co_editing();
    for (state, label) in
        [(SyncState::Saving, "Saving…"), (SyncState::Saved, "All changes saved"), (SyncState::Offline, "Offline — changes will sync")]
    {
        app.collab.sync = Some(state);
        let shapes = frame(&mut app, &ctx, W);
        let at = text_rect(&shapes, label).unwrap_or_else(|| panic!("{label} shown"));
        assert!(at.max.y < 40.0, "{label} is in the title row: {at:?}");
    }
}
