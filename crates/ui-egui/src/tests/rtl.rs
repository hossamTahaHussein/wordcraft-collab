//! The right-to-left interface (an Arabic, Persian or Hebrew interface language): the window
//! mirrors as Word's right-to-left versions do; documents keep their own direction.

use egui::epaint::{ClippedShape, Shape};

use crate::{Services, WordApp};

/// An app on a one-paragraph document with the interface in `lang`, after its first frames.
fn app_in(lang: &str) -> (WordApp, egui::Context) {
    let doc = wordcraft_doc::Document::from_text("Hello world");
    let mut app = WordApp::new(wordcraft_engine::Session::new(doc), Services::default());
    app.run("ui.language", serde_json::json!({"value": lang})).unwrap();
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, 1280.0);
    (app, ctx)
}

/// Run a few frames `width` × 800 and return what the last one painted.
fn frame(app: &mut WordApp, ctx: &egui::Context, width: f32) -> Vec<ClippedShape> {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0));
    let mut shapes = Vec::new();
    for i in 0..4 {
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(f64::from(i) / 60.0), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        out.textures_delta.clear();
        shapes = out.shapes;
    }
    shapes
}

/// The bounding rect of the first text shape showing exactly `text`.
fn text_rect(shapes: &[ClippedShape], text: &str) -> Option<egui::Rect> {
    fn find(shape: &Shape, text: &str) -> Option<egui::Rect> {
        match shape {
            Shape::Text(t) if t.galley.text() == text => Some(t.visual_bounding_rect()),
            Shape::Vec(v) => v.iter().find_map(|s| find(s, text)),
            _ => None,
        }
    }
    shapes.iter().find_map(|c| find(&c.shape, text))
}

#[test]
fn an_arabic_interface_is_right_to_left() {
    let (_app, ctx) = app_in("ar");
    assert!(craft_ui::direction::is_rtl(&ctx));
    assert!(ctx.global_style().right_to_left, "egui's own chrome mirrors too");
    for lang in ["fa", "he"] {
        assert!(craft_ui::direction::is_rtl(&app_in(lang).1), "{lang}");
    }
    assert!(!craft_ui::direction::is_rtl(&app_in("en").1));
}

#[test]
fn switching_back_to_english_restores_left_to_right() {
    let (mut app, ctx) = app_in("ar");
    app.run("ui.language", serde_json::json!({"value": "en"})).unwrap();
    frame(&mut app, &ctx, 1280.0);
    assert!(!craft_ui::direction::is_rtl(&ctx));
    assert!(!ctx.global_style().right_to_left);
}

#[test]
fn a_theme_change_keeps_the_interface_mirrored() {
    let (mut app, ctx) = app_in("ar");
    app.run("view.interfaceTheme", serde_json::json!({"value": "dark"})).unwrap();
    frame(&mut app, &ctx, 1280.0);
    assert!(ctx.global_style().right_to_left);
    assert!(craft_ui::direction::is_rtl(&ctx));
}

#[test]
fn the_title_and_status_bars_mirror() {
    let (mut app, ctx) = app_in("ar");
    let shapes = frame(&mut app, &ctx, 1280.0);
    let save = text_rect(&shapes, crate::tl!("AutoSave")).expect("AutoSave label");
    assert!(save.center().x > 640.0, "AutoSave and the Quick Access Toolbar at the right: {save:?}");
    let zoom = text_rect(&shapes, "100%").expect("zoom");
    assert!(zoom.center().x < 640.0, "the zoom control at the status bar's left: {zoom:?}");
    let page = shapes.iter().find_map(|c| match &c.shape {
        Shape::Text(t) if t.galley.text().contains('1') && t.visual_bounding_rect().min.y > 760.0 && t.galley.text() != "100%" => {
            Some(t.visual_bounding_rect())
        }
        _ => None,
    });
    assert!(page.expect("page count").center().x > 640.0, "page and word counts at the right");
    let (mut app, ctx) = app_in("en");
    let shapes = frame(&mut app, &ctx, 1280.0);
    assert!(text_rect(&shapes, "AutoSave").unwrap().center().x < 640.0, "English unchanged");
    assert!(text_rect(&shapes, "100%").unwrap().center().x > 640.0);
}

#[test]
fn the_ribbon_tabs_and_groups_start_at_the_right() {
    let (mut app, ctx) = app_in("ar");
    let shapes = frame(&mut app, &ctx, 1280.0);
    let file = text_rect(&shapes, crate::tl!("File")).expect("File tab");
    let home = text_rect(&shapes, crate::tl!("Home")).expect("Home tab");
    assert!(file.min.x > home.max.x, "File is the rightmost tab: {file:?} {home:?}");
    let clipboard = text_rect(&shapes, crate::tl!("Clipboard")).expect("Clipboard group");
    let font = text_rect(&shapes, crate::tl!("Font")).expect("Font group");
    assert!(clipboard.min.x > font.max.x, "Clipboard is the first group, at the right");
    let share = text_rect(&shapes, crate::tl!("Share")).expect("Share button");
    assert!(share.center().x < 640.0, "Share at the tab strip's left end");
}

#[test]
fn a_narrow_mirrored_ribbon_keeps_its_first_groups_on_the_right() {
    let (mut app, ctx) = app_in("ar");
    let shapes = frame(&mut app, &ctx, 640.0);
    let clipboard = text_rect(&shapes, crate::tl!("Clipboard")).expect("Clipboard group");
    assert!(clipboard.max.x > 520.0, "first group against the right edge: {clipboard:?}");
}

#[test]
fn group_dialog_launchers_sit_in_the_bottom_left_corner() {
    crate::widgets::LAUNCHERS.with(|l| l.borrow_mut().clear());
    let (mut app, ctx) = app_in("ar");
    frame(&mut app, &ctx, 1280.0);
    let (launcher, row) =
        crate::widgets::LAUNCHERS.with(|l| l.borrow().iter().rev().find(|(id, _, _)| id == "Font").map(|(_, a, b)| (*a, *b))).expect("Font launcher");
    assert!(launcher.center().x < row.center().x, "at the group's left: {launcher:?} in {row:?}");
}

#[test]
fn docked_panes_open_on_the_right() {
    let (mut app, ctx) = app_in("ar");
    app.run("view.navigationPane", serde_json::json!({"value": true})).unwrap();
    let shapes = frame(&mut app, &ctx, 1280.0);
    let nav = text_rect(&shapes, crate::tl!("Navigation")).expect("pane title");
    assert!(nav.center().x > 900.0, "the Navigation pane docks at the reading start, the right: {nav:?}");
    let (mut app, ctx) = app_in("en");
    app.run("view.navigationPane", serde_json::json!({"value": true})).unwrap();
    let shapes = frame(&mut app, &ctx, 1280.0);
    assert!(text_rect(&shapes, "Navigation").unwrap().center().x < 400.0, "English unchanged");
}

#[test]
fn the_file_page_menu_is_on_the_right() {
    let (mut app, ctx) = app_in("ar");
    app.run("ui.backstage", serde_json::json!({"value": true, "page": "home"})).unwrap();
    let shapes = frame(&mut app, &ctx, 1280.0);
    let open = text_rect(&shapes, crate::tl!("Open")).expect("Open in the File menu");
    assert!(open.center().x > 1080.0, "{open:?}");
    // As in English 52 pt from the menu's left edge (the icon before it), 52 pt from its right.
    assert!((open.max.x - (1280.0 - 52.0)).abs() < 2.0, "the label reads from the menu's right edge: {open:?}");
}

#[test]
fn the_vertical_ruler_is_on_the_right() {
    let (mut app, ctx) = app_in("ar");
    let shapes = frame(&mut app, &ctx, 1280.0);
    let page = app.canvas.page_rects[0];
    // Ruler numbers right of the page belong to a vertical ruler at the window's right.
    let right_of_page = shapes.iter().any(|c| match &c.shape {
        Shape::Text(t) => {
            t.galley.text().parse::<u32>().is_ok() && t.visual_bounding_rect().min.x > page.max.x && t.visual_bounding_rect().min.y > page.min.y
        }
        _ => false,
    });
    assert!(right_of_page, "vertical ruler numbers at the right of the page");
    let left_of_page = shapes.iter().any(|c| match &c.shape {
        Shape::Text(t) => t.galley.text().parse::<u32>().is_ok() && t.visual_bounding_rect().max.x < page.min.x,
        _ => false,
    });
    assert!(!left_of_page, "none at the left");
}

#[test]
fn the_document_is_not_mirrored() {
    let (mut app, ctx) = app_in("ar");
    frame(&mut app, &ctx, 1280.0);
    let caret_ar = crate::canvas::caret_screen(&mut app).expect("caret").0.x;
    let (mut app2, ctx2) = app_in("en");
    frame(&mut app2, &ctx2, 1280.0);
    let caret_en = crate::canvas::caret_screen(&mut app2).expect("caret").0.x;
    // The page moves with the ruler, but the English paragraph still starts at its left margin.
    let page_ar = app.canvas.page_rects[0];
    let page_en = app2.canvas.page_rects[0];
    assert!(((caret_ar - page_ar.min.x) - (caret_en - page_en.min.x)).abs() < 1.0, "{caret_ar} in {page_ar:?}, {caret_en} in {page_en:?}");
}

#[test]
fn dialog_buttons_sit_at_the_left_and_rows_start_at_the_right() {
    let (mut app, ctx) = app_in("ar");
    app.run("ui.dialog", serde_json::json!({"name": "zoom"})).unwrap();
    let shapes = frame(&mut app, &ctx, 1280.0);
    let texts: Vec<String> =
        shapes.iter().filter_map(|c| if let Shape::Text(t) = &c.shape { Some(t.galley.text().to_string()) } else { None }).collect();
    let ok = text_rect(&shapes, crate::tl!("OK")).unwrap_or_else(|| panic!("OK in {texts:?}"));
    let cancel = text_rect(&shapes, crate::tl!("Cancel")).expect("Cancel");
    // The first choice (200%) leads its row from the dialog's right; the buttons end at its left.
    let choice = text_rect(&shapes, "200%").unwrap_or_else(|| panic!("200% in {texts:?}"));
    assert!(
        ok.center().x < choice.center().x && cancel.center().x < choice.center().x,
        "buttons at the dialog's left, choices at its right: {ok:?} {cancel:?} {choice:?}"
    );
    assert!(cancel.center().x < ok.center().x, "mirrored order: Cancel left of OK");
}

#[test]
fn a_menu_opens_under_its_buttons_right_edge() {
    let (mut app, ctx) = app_in("ar");
    frame(&mut app, &ctx, 1280.0);
    let more = ctx.read_response(crate::chrome::qat_menu_button_id()).expect("the … button");
    egui::Popup::open_id(&ctx, egui::Popup::default_response_id(&more));
    frame(&mut app, &ctx, 1280.0);
    let popup = ctx.memory(|m| m.area_rect(egui::Popup::default_response_id(&more))).expect("menu shown");
    assert!((popup.max.x - more.rect.max.x).abs() < 4.0, "right edges level: {popup:?} under {:?}", more.rect);
}

/// Every point an icon paints, in `r` (paths, segments, circles, rectangle corners).
fn icon_points(rtl: bool, name: &str) -> Vec<(f32, f32)> {
    let ctx = egui::Context::default();
    craft_ui::direction::apply(&ctx, if rtl { craft_ui::direction::Direction::Rtl } else { craft_ui::direction::Direction::Ltr });
    let r = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(20.0, 20.0));
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| crate::icons::paint(ui.painter(), r, name, egui::Color32::BLACK, egui::Color32::RED));
    out.textures_delta.clear();
    let mut pts = Vec::new();
    fn walk(s: &Shape, pts: &mut Vec<(f32, f32)>) {
        match s {
            Shape::Vec(v) => v.iter().for_each(|s| walk(s, pts)),
            Shape::Path(p) => pts.extend(p.points.iter().map(|q| (q.x, q.y))),
            Shape::LineSegment { points, .. } => pts.extend(points.iter().map(|q| (q.x, q.y))),
            Shape::Circle(c) => pts.push((c.center.x, c.center.y)),
            Shape::Rect(r) => pts.extend([(r.rect.min.x, r.rect.min.y), (r.rect.max.x, r.rect.max.y)]),
            _ => {}
        }
    }
    for c in &out.shapes {
        walk(&c.shape, &mut pts);
    }
    pts
}

/// The points mirrored about the icon's centre line (x = 110), sorted for comparison.
fn mirrored(pts: &[(f32, f32)]) -> Vec<(i32, i32)> {
    let mut v: Vec<(i32, i32)> = pts.iter().map(|(x, y)| (((220.0 - x) * 10.0).round() as i32, (y * 10.0).round() as i32)).collect();
    v.sort_unstable();
    v
}

fn sorted(pts: &[(f32, f32)]) -> Vec<(i32, i32)> {
    let mut v: Vec<(i32, i32)> = pts.iter().map(|(x, y)| ((x * 10.0).round() as i32, (y * 10.0).round() as i32)).collect();
    v.sort_unstable();
    v
}

#[test]
fn bullets_and_indent_icons_flip_right_to_left() {
    for name in ["bullets", "multilevel", "outdent", "indent"] {
        let ltr = icon_points(false, name);
        assert!(!ltr.is_empty(), "{name} paints");
        assert_eq!(sorted(&icon_points(true, name)), mirrored(&ltr), "{name} mirrored");
    }
    // Undo keeps its drawing (history runs left to right).
    assert_eq!(sorted(&icon_points(true, "undo")), sorted(&icon_points(false, "undo")));
}

/// Layout › Paragraph's indent and spacing fields (an egui grid, kept left to right) stay inside
/// their group in a mirrored ribbon.
#[test]
fn a_grid_in_a_mirrored_ribbon_group_stays_in_its_group() {
    let (mut app, ctx) = app_in("ar");
    app.ui.tab = "Layout".into();
    let shapes = frame(&mut app, &ctx, 1280.0);
    let title = text_rect(&shapes, crate::tl!("Paragraph")).expect("Paragraph group title");
    let indent = text_rect(&shapes, crate::tl!("Indent")).expect("Indent heading");
    assert!((indent.center().x - title.center().x).abs() < 120.0, "the fields sit over their title: {indent:?} {title:?}");
}
