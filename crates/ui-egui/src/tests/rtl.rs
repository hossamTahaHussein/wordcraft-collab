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
