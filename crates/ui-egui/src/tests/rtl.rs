//! The right-to-left interface (an Arabic, Persian or Hebrew interface language): the window
//! mirrors as Word's right-to-left versions do; documents keep their own direction.

use egui::epaint::{ClippedShape, Shape};

use crate::{Services, WordApp};

/// An app on a one-paragraph document with the interface in `lang`, after its first frames.
pub(super) fn app_in(lang: &str) -> (WordApp, egui::Context) {
    let doc = wordcraft_doc::Document::from_text("Hello world");
    let mut app = WordApp::new(wordcraft_engine::Session::new(doc), Services::default());
    app.run("ui.language", serde_json::json!({"value": lang})).unwrap();
    let ctx = egui::Context::default();
    frame(&mut app, &ctx, 1280.0);
    (app, ctx)
}

/// Run a few frames `width` × 800 and return what the last one painted.
pub(super) fn frame(app: &mut WordApp, ctx: &egui::Context, width: f32) -> Vec<ClippedShape> {
    frame_sized(app, ctx, width, 800.0)
}

/// One frame with a click at `at`, then [`frame`]'s frames.
pub(super) fn click(app: &mut WordApp, ctx: &egui::Context, width: f32, at: egui::Pos2) -> Vec<ClippedShape> {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0));
    for (i, pressed) in [true, false].into_iter().enumerate() {
        let events = vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() },
        ];
        let time = ctx.input(|inp| inp.time) + (i + 1) as f64 / 60.0;
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(time), events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
        });
        out.textures_delta.clear();
    }
    frame(app, ctx, width)
}

/// [`frame`] with a window `height` tall.
pub(super) fn frame_sized(app: &mut WordApp, ctx: &egui::Context, width: f32, height: f32) -> Vec<ClippedShape> {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
    let mut shapes = Vec::new();
    for i in 0..4 {
        // The clock only moves on (egui checks in debug builds).
        let time = ctx.input(|inp| inp.time) + f64::from(i + 1) / 60.0;
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(time), ..Default::default() };
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
pub(super) fn text_rect(shapes: &[ClippedShape], text: &str) -> Option<egui::Rect> {
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

#[test]
fn pane_contents_read_from_the_right() {
    let (mut app, ctx) = app_in("ar");
    app.run("view.navigationPane", serde_json::json!({"value": true})).unwrap();
    let shapes = frame(&mut app, &ctx, 1280.0);
    let headings = text_rect(&shapes, crate::tl!("Headings")).expect("Headings tab");
    let results = text_rect(&shapes, crate::tl!("Results")).expect("Results tab");
    assert!(headings.min.x > results.max.x, "the first tab is the rightmost: {headings:?} {results:?}");
}

/// Navigation headings keep their outline indent, from the right.
#[test]
fn navigation_headings_indent_from_the_right() {
    let mut doc = wordcraft_doc::Document::from_text("Top\nSub\nBody");
    let at = |i: usize| wordcraft_doc::Pos::body(i, 0);
    doc.format_paragraphs(&at(0), &at(0), &|p: &mut wordcraft_doc::props::ParaProps| p.style = Some("Heading1".into())).unwrap();
    doc.format_paragraphs(&at(1), &at(1), &|p: &mut wordcraft_doc::props::ParaProps| p.style = Some("Heading2".into())).unwrap();
    let mut app = WordApp::new(wordcraft_engine::Session::new(doc), Services::default());
    app.run("ui.language", serde_json::json!({"value": "ar"})).unwrap();
    app.run("view.navigationPane", serde_json::json!({"value": true})).unwrap();
    let ctx = egui::Context::default();
    let shapes = frame(&mut app, &ctx, 1280.0);
    let top = text_rect(&shapes, "Top").expect("Top heading");
    let sub = text_rect(&shapes, "Sub").expect("Sub heading");
    assert!(top.max.x - sub.max.x > 8.0, "the sub-heading ends further from the right edge: {top:?} {sub:?}");
}

#[test]
fn the_file_page_reads_from_the_right() {
    let (mut app, ctx) = app_in("ar");
    app.run("ui.backstage", serde_json::json!({"value": true, "page": "home"})).unwrap();
    let shapes = frame(&mut app, &ctx, 1280.0);
    let blank = text_rect(&shapes, crate::tl!("Blank document")).expect("Blank document");
    let recent = text_rect(&shapes, crate::tl!("Recent")).expect("Recent heading");
    assert!(blank.center().x > 640.0 && recent.center().x > 640.0, "the page starts at the right, next to the menu: {blank:?} {recent:?}");
}

/// A window too narrow for the title between the toolbar and the controls hides the title, in
/// either direction, instead of drawing it over them.
#[test]
fn a_narrow_window_hides_the_title_rather_than_overlapping() {
    for (lang, width) in ["en", "ar"].into_iter().flat_map(|l| [260.0, 300.0, 340.0, 380.0].map(move |w| (l, w))) {
        let (mut app, ctx) = app_in(lang);
        let shapes = frame(&mut app, &ctx, width);
        let title = shapes.iter().find_map(|c| match &c.shape {
            Shape::Text(t) if t.galley.text().contains(" — ") => Some(t.visual_bounding_rect()),
            _ => None,
        });
        let save = text_rect(&shapes, crate::tl!("AutoSave"));
        if let (Some(title), Some(save)) = (title, save) {
            assert!(!title.intersects(save), "{lang} at {width}: the title over AutoSave: {title:?} {save:?}");
        }
    }
}

/// The table style dialog's alignment grid sits in a row of the mirrored dialog: it lays out
/// (finite positions) and stays inside the dialog.
#[test]
fn the_table_style_dialog_lays_out_right_to_left() {
    let (mut app, ctx) = app_in("ar");
    app.run("ui.dialog", serde_json::json!({"name": "newTableStyle"})).unwrap();
    let shapes = frame_sized(&mut app, &ctx, 1280.0, 1400.0);
    let texts: Vec<String> =
        shapes.iter().filter_map(|c| if let Shape::Text(t) = &c.shape { Some(t.galley.text().to_string()) } else { None }).collect();
    let label = text_rect(&shapes, crate::tl!("Alignment:")).unwrap_or_else(|| panic!("Alignment label in {texts:?}"));
    assert!(label.is_finite());
    let ok = text_rect(&shapes, crate::tl!("OK")).expect("OK");
    assert!(ok.is_finite() && ok.min.x > 0.0 && ok.max.x < 1280.0, "{ok:?}");
    // Every text the dialog drew has a finite place.
    for c in &shapes {
        if let Shape::Text(t) = &c.shape
            && !t.galley.text().is_empty()
        {
            assert!(t.visual_bounding_rect().is_finite(), "{:?} at {:?}", t.galley.text(), t.visual_bounding_rect());
        }
    }
}

/// Dialogs with label/field grids (their rows kept left to right) draw whole right to left.
#[test]
fn dialogs_with_grids_draw_whole() {
    for name in ["paragraph", "font", "pageSetup"] {
        let (mut app, ctx) = app_in("ar");
        app.run("ui.dialog", serde_json::json!({"name": name})).unwrap();
        let shapes = frame_sized(&mut app, &ctx, 1280.0, 1400.0);
        assert!(text_rect(&shapes, crate::tl!("OK")).is_some_and(|r| r.is_finite()), "{name}: OK drawn");
        assert!(text_rect(&shapes, crate::tl!("Cancel")).is_some_and(|r| r.is_finite()), "{name}: Cancel drawn");
    }
}

/// A dialog's label/field grid reads from the right too: Page Setup's first column ("Top:") is
/// at the right of its second ("Bottom:") in Arabic, at the left in English.
#[test]
fn dialog_grids_read_from_the_right() {
    for (lang, rtl) in [("ar", true), ("en", false)] {
        let (mut app, ctx) = app_in(lang);
        app.run("ui.dialog", serde_json::json!({"name": "pageSetup"})).unwrap();
        let shapes = frame_sized(&mut app, &ctx, 1280.0, 1400.0);
        let top = text_rect(&shapes, crate::tl!("Top:")).expect("Top:");
        let bottom = text_rect(&shapes, crate::tl!("Bottom:")).expect("Bottom:");
        assert_eq!(top.min.x > bottom.max.x, rtl, "{lang}: Top {top:?}, Bottom {bottom:?}");
    }
}

/// A mirrored ribbon too wide for the window opens at its start (the right) and its left chevron
/// scrolls on to the groups at the far end.
#[test]
fn a_mirrored_ribbon_scrolls_to_its_far_end() {
    let (mut app, ctx) = app_in("ar");
    let width = 300.0;
    let shapes = frame(&mut app, &ctx, width);
    let visible = |shapes: &[ClippedShape], text: &str| {
        shapes.iter().any(|c| match &c.shape {
            Shape::Text(t) if t.galley.text() == text => {
                let r = t.visual_bounding_rect();
                r.min.x >= 0.0 && r.max.x <= width && c.clip_rect.intersects(r)
            }
            _ => false,
        })
    };
    let first = crate::tl!("Clipboard");
    let last = crate::tl!("Editor");
    assert!(visible(&shapes, first), "the first group shows");
    assert!(!visible(&shapes, last), "the last group is past the left end");
    // The ribbon's left chevron (‹), at the left edge of the group row.
    let ribbon_y = text_rect(&shapes, first).map_or(110.0, |r| r.min.y - 30.0);
    let mut shapes = shapes;
    for _ in 0..12 {
        if visible(&shapes, last) {
            break;
        }
        shapes = click(&mut app, &ctx, width, egui::pos2(14.0, ribbon_y));
    }
    assert!(visible(&shapes, last), "the left chevron reaches the last group");
}

/// Mirrored icons keep their polygons' winding, so their edges feather outwards as left to right.
#[test]
fn mirrored_icon_polygons_keep_their_winding() {
    fn areas(rtl: bool, name: &str) -> Vec<bool> {
        let ctx = egui::Context::default();
        craft_ui::direction::apply(&ctx, if rtl { craft_ui::direction::Direction::Rtl } else { craft_ui::direction::Direction::Ltr });
        let r = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(20.0, 20.0));
        let mut out =
            ctx.run_ui(egui::RawInput::default(), |ui| crate::icons::paint(ui.painter(), r, name, egui::Color32::BLACK, egui::Color32::RED));
        out.textures_delta.clear();
        let mut signs = Vec::new();
        for c in &out.shapes {
            if let Shape::Path(p) = &c.shape
                && p.closed
                && p.points.len() > 2
            {
                let a: f32 = p.points.iter().zip(p.points.iter().cycle().skip(1)).map(|(a, b)| a.x * b.y - b.x * a.y).sum();
                signs.push(a > 0.0);
            }
        }
        signs
    }
    for name in ["bullets", "multilevel", "outdent", "indent"] {
        let ltr = areas(false, name);
        if !ltr.is_empty() {
            assert_eq!(areas(true, name), ltr, "{name}");
        }
    }
}

/// A menu's items read from the right in a right-to-left interface.
#[test]
fn menu_items_read_from_the_right() {
    for (lang, right) in [("ar", true), ("en", false)] {
        let (mut app, ctx) = app_in(lang);
        frame(&mut app, &ctx, 1280.0);
        let more = ctx.read_response(crate::chrome::qat_menu_button_id()).expect("the … button");
        let popup = egui::Popup::default_response_id(&more);
        egui::Popup::open_id(&ctx, popup);
        let shapes = frame(&mut app, &ctx, 1280.0);
        let area = ctx.memory(|m| m.area_rect(popup)).expect("menu shown");
        let labels: Vec<egui::Rect> = shapes
            .iter()
            // The menu's own text is clipped to the screen; the ribbon under it to its strips.
            .filter(|c| c.clip_rect == ctx.content_rect())
            .filter_map(|c| if let Shape::Text(t) = &c.shape { Some(t.visual_bounding_rect()) } else { None })
            .filter(|r| area.contains_rect(*r) && r.width() > 12.0)
            .collect();
        assert!(!labels.is_empty(), "{lang}: menu labels");
        for r in labels {
            let near_right = area.max.x - r.max.x < r.min.x - area.min.x;
            assert_eq!(near_right, right, "{lang}: {r:?} in {area:?}");
        }
    }
}

/// A right-to-left document has its comment balloons on the left of the page, as Word does; a
/// left-to-right one keeps them on the right. Both the floating card (comments alone) and the
/// markup area (with a tracked change too).
#[test]
fn comment_balloons_sit_at_the_documents_reading_end() {
    for tracked in [false, true] {
        for (text, rtl) in [("Hello world and more words here", false), ("مرحبا بالعالم وكلمات أخرى هنا", true)] {
            let doc = wordcraft_doc::Document::from_text(text);
            let mut app = WordApp::new(wordcraft_engine::Session::new(doc), Services::default());
            if rtl {
                app.run("select.all", serde_json::json!({})).unwrap();
                app.run("para.rtl", serde_json::json!({"value": true})).unwrap();
            }
            if tracked {
                app.run("review.trackChanges", serde_json::json!({"value": true})).unwrap();
                app.run("caret.docEnd", serde_json::json!({})).unwrap();
                app.run("text.insert", serde_json::json!({"text": " x"})).unwrap();
            }
            app.run("select.all", serde_json::json!({})).unwrap();
            app.run("review.newComment", serde_json::json!({"text": "A note in the margin"})).unwrap();
            app.run("view.commentsPane", serde_json::json!({"value": false})).unwrap();
            app.run("caret.docStart", serde_json::json!({})).unwrap();
            let ctx = egui::Context::default();
            let shapes = frame(&mut app, &ctx, 1280.0);
            let page = app.canvas.page_rects.first().copied().expect("a page");
            let note = text_rect(&shapes, "A note in the margin").unwrap_or_else(|| panic!("tracked={tracked} rtl={rtl}: the balloon"));
            // Page rects are the canvas's own; compare the balloon with the window's middle,
            // which the page straddles.
            assert_eq!(note.center().x < 640.0, rtl, "tracked={tracked} {text}: the balloon at {note:?}, page {page:?}");
        }
    }
}
