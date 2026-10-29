//! Co-editing: the other people in the document, painted where they are. Each gets a caret and a
//! translucent selection in their colour, a thin bar in the page margin beside their line, and a
//! name flag above the caret while they type or the pointer is over it. The host fills
//! `WordApp::peers` each frame; nothing here knows about the relay.

use egui::{Color32, FontId, Painter, Pos2, Rect, pos2, vec2};
use wordcraft_engine::Selection;

use crate::WordApp;

use super::pages::page_screen_scale;

/// Width of a peer's caret on screen, in points.
pub(crate) const CARET_WIDTH: f32 = 2.0;
/// Width of the margin bar, and its distance from the page's left edge.
const MARGIN_BAR: f32 = 3.0;
const MARGIN_INSET: f32 = 6.0;
/// How near the pointer must be to a caret to show its name.
const HOVER_REACH: f32 = 6.0;

/// Someone else in the document.
#[derive(Clone, Debug, PartialEq)]
pub struct RemotePeer {
    pub name: String,
    /// Index into `theme::Tokens::peer_colors`.
    pub color: u8,
    pub sel: Selection,
    /// They edited in the last few seconds: their name shows.
    pub typing: bool,
}

/// Where one peer is drawn, in screen points.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PeerMark {
    pub color: u8,
    pub caret: Option<Rect>,
    pub sel: Vec<Rect>,
    /// The name flag's bottom-left corner and the name.
    pub flag: Option<(Pos2, String)>,
    pub margin: Option<Rect>,
}

/// Each peer's marks on pages laid out at `rects` (screen rectangles, one per page).
pub(crate) fn peer_marks(app: &mut WordApp, rects: &[Rect], scale: f32, hover: Option<Pos2>) -> Vec<PeerMark> {
    let layout = app.session.layout();
    let hint = app.session.page_hint;
    let to_screen = |page: usize, x: f32, y: f32, w: f32, h: f32| {
        let pr = rects.get(page)?;
        let s = layout.pages.get(page).map_or(scale, |p| page_screen_scale(*pr, p, scale));
        Some(Rect::from_min_size(pos2(pr.min.x + x * s, pr.min.y + y * s), vec2(w * s, h * s)))
    };
    app.peers
        .iter()
        .map(|peer| {
            let caret = layout.caret_on(&peer.sel.focus, hint).and_then(|c| {
                let r = to_screen(c.page, c.x, c.top, 0.0, c.height)?;
                Some((c.page, Rect::from_min_size(pos2(r.min.x - CARET_WIDTH / 2.0, r.min.y), vec2(CARET_WIDTH, r.height()))))
            });
            let sel = if peer.sel.is_collapsed() {
                Vec::new()
            } else {
                let (a, b) = peer.sel.ordered();
                layout.selection_rects(&app.session.doc, &a, &b, hint).into_iter().filter_map(|(pi, r)| to_screen(pi, r.x, r.y, r.w, r.h)).collect()
            };
            let hovered = caret.is_some_and(|(_, c)| hover.is_some_and(|p| c.expand(HOVER_REACH).contains(p)));
            let flag = caret.filter(|_| peer.typing || hovered).map(|(_, c)| (c.min, peer.name.clone()));
            let margin = caret.and_then(|(page, c)| {
                let pr = rects.get(page)?;
                Some(Rect::from_min_max(pos2(pr.min.x + MARGIN_INSET, c.min.y), pos2(pr.min.x + MARGIN_INSET + MARGIN_BAR, c.max.y)))
            });
            PeerMark { color: peer.color, caret: caret.map(|(_, c)| c), sel, flag, margin }
        })
        .collect()
}

/// The title bar's avatars: up to five (initials and colour), and how many more there are.
pub(crate) fn avatars(peers: &[RemotePeer]) -> (Vec<(String, u8)>, usize) {
    const SHOWN: usize = 5;
    let shown = peers.iter().take(SHOWN).map(|p| (initials(&p.name), p.color)).collect();
    (shown, peers.len().saturating_sub(SHOWN))
}

/// The first letter of the first and last words of a name ("Ann Lee" → "AL").
fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let first = |w: &&str| w.chars().next();
    let mut out: String = words.first().and_then(first).into_iter().collect();
    if words.len() > 1 {
        out.extend(words.last().and_then(first));
    }
    out.to_uppercase()
}

/// The status bar's count of people in the document, this person included.
pub(crate) fn people_label(peers: usize) -> Option<String> {
    (peers > 0).then(|| crate::i18n::fmt(crate::tl!("{n} people editing"), &[("n", &(peers + 1).to_string())]))
}

/// Paint the peers' marks: selections under carets, flags on top.
pub(crate) fn paint_peers(painter: &Painter, marks: &[PeerMark], colors: &[Color32; 12]) {
    let color = |i: u8| colors.get(usize::from(i) % colors.len()).copied().unwrap_or(Color32::GRAY);
    for m in marks {
        let c = color(m.color);
        for r in &m.sel {
            painter.rect_filled(*r, 0.0, c.gamma_multiply(0.25));
        }
        if let Some(r) = m.margin {
            painter.rect_filled(r, 1.0, c);
        }
        if let Some(r) = m.caret {
            painter.rect_filled(r, 0.0, c);
        }
    }
    let font = FontId::proportional(11.0);
    for m in marks {
        let Some((at, name)) = &m.flag else { continue };
        let galley = painter.layout_no_wrap(name.clone(), font.clone(), Color32::WHITE);
        let size = galley.size() + vec2(8.0, 2.0);
        let pill = Rect::from_min_size(pos2(at.x, at.y - size.y), size);
        painter.rect_filled(pill, 3.0, color(m.color));
        painter.galley(pill.min + vec2(4.0, 1.0), galley, Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2, vec2};
    use serde_json::json;
    use wordcraft_doc::{Document, Path, Pos, StoryRef};
    use wordcraft_engine::{Selection, Session};

    use super::*;
    use crate::WordApp;

    const SCALE: f32 = 1.0;

    fn app(text: &str) -> WordApp {
        WordApp::new(Session::new(Document::from_text(text)), crate::Services::default())
    }

    /// One screen rectangle per page, stacked, at scale 1.
    fn rects(app: &mut WordApp) -> Vec<Rect> {
        let l = app.session.layout();
        let mut y = 0.0;
        l.pages
            .iter()
            .map(|p| {
                let r = Rect::from_min_size(pos2(100.0, y), vec2(p.w, p.h));
                y += p.h + 20.0;
                r
            })
            .collect()
    }

    fn peer(name: &str, a: usize, b: usize, typing: bool) -> RemotePeer {
        RemotePeer { name: name.into(), color: 2, sel: Selection { anchor: Pos::body(0, a), focus: Pos::body(0, b) }, typing }
    }

    #[test]
    fn peer_marks_place_caret_at_peer_position() {
        let mut app = app("Hello world");
        app.peers = vec![peer("Ann", 5, 5, false)];
        let r = rects(&mut app);
        let marks = peer_marks(&mut app, &r, SCALE, None);
        let caret = marks[0].caret.expect("a caret");
        // The same place the engine puts a caret at offset 5 on page 0.
        let c = app.session.layout().caret_on(&Pos::body(0, 5), 0).unwrap();
        assert!((caret.center().x - (r[0].min.x + c.x)).abs() < 1.5, "{caret:?} vs x={}", c.x);
        assert!((caret.min.y - (r[0].min.y + c.top)).abs() < 0.5);
        assert!((caret.width() - CARET_WIDTH).abs() < 0.01);
        assert_eq!(marks[0].color, 2);
        assert!(marks[0].sel.is_empty());
        let bar = marks[0].margin.expect("a margin bar");
        assert!(bar.min.x < r[0].min.x + 20.0 && bar.max.x <= r[0].min.x + 20.0, "in the page's left margin: {bar:?}");
    }

    #[test]
    fn peer_selection_covers_its_range() {
        let mut app = app("Hello world");
        app.peers = vec![peer("Ann", 0, 5, false)];
        let r = rects(&mut app);
        let marks = peer_marks(&mut app, &r, SCALE, None);
        let sel = &marks[0].sel;
        assert_eq!(sel.len(), 1);
        let c0 = app.session.layout().caret_on(&Pos::body(0, 0), 0).unwrap();
        let c5 = app.session.layout().caret_on(&Pos::body(0, 5), 0).unwrap();
        assert!((sel[0].width() - (c5.x - c0.x)).abs() < 1.0, "{:?}", sel[0]);
    }

    #[test]
    fn peer_marks_in_a_header_story() {
        let mut app = app("Body text");
        app.session.run("insert.createHeader", &json!({})).unwrap();
        app.session.run("text.insert", &json!({"text": "Head"})).unwrap();
        let header = app.session.sel.focus.story;
        assert_ne!(header, StoryRef::Body);
        let _ = app.session.run("insert.closeHeader", &json!({}));
        app.peers = vec![RemotePeer { name: "Ann".into(), color: 1, sel: Selection::caret(Pos::new(header, Path(vec![0]), 2)), typing: false }];
        let r = rects(&mut app);
        let marks = peer_marks(&mut app, &r, SCALE, None);
        let caret = marks[0].caret.expect("a caret in the header");
        let body = app.session.layout().caret_on(&Pos::body(0, 0), 0).unwrap();
        assert!(caret.min.y < r[0].min.y + body.top, "above the body text: {caret:?}");
    }

    #[test]
    fn flag_only_while_typing_or_hovered() {
        let mut app = app("Hello world");
        app.peers = vec![peer("Ann", 5, 5, false), peer("Bob", 8, 8, true)];
        let r = rects(&mut app);
        let marks = peer_marks(&mut app, &r, SCALE, None);
        assert!(marks[0].flag.is_none());
        assert_eq!(marks[1].flag.as_ref().map(|(_, n)| n.as_str()), Some("Bob"));
        let hover = marks[0].caret.map(|c| c.center());
        let marks = peer_marks(&mut app, &r, SCALE, hover);
        assert_eq!(marks[0].flag.as_ref().map(|(_, n)| n.as_str()), Some("Ann"));
    }

    #[test]
    fn a_peer_off_the_document_draws_nothing() {
        let mut app = app("Hi");
        app.peers = vec![RemotePeer { name: "Ann".into(), color: 0, sel: Selection::caret(Pos::body(40, 0)), typing: true }];
        let r = rects(&mut app);
        let marks = peer_marks(&mut app, &r, SCALE, None);
        assert!(marks.iter().all(|m| m.caret.is_none() && m.sel.is_empty() && m.flag.is_none()));
    }

    #[test]
    fn peer_marks_for_twenty_peers_reuse_layout() {
        let mut app = app("Lots of people are editing this one line at the same time.");
        app.peers = (0..20).map(|i| peer(&format!("P{i}"), i * 2, i * 2 + 1, i % 2 == 0)).collect();
        let r = rects(&mut app);
        let before = app.session.layout();
        let marks = peer_marks(&mut app, &r, SCALE, None);
        assert_eq!(marks.len(), 20);
        assert!(std::sync::Arc::ptr_eq(&before, &app.session.layout()), "painting peers never lays the document out again");
    }
}

#[cfg(test)]
mod chrome_tests {
    use serde_json::json;
    use wordcraft_doc::{Document, Pos};
    use wordcraft_engine::{Selection, Session};

    use super::*;
    use crate::WordApp;

    fn peers(n: usize) -> Vec<RemotePeer> {
        (0..n).map(|i| RemotePeer { name: format!("Person {i}"), color: i as u8, sel: Selection::caret(Pos::body(0, i)), typing: false }).collect()
    }

    #[test]
    fn avatars_show_up_to_five_and_a_count() {
        let (shown, more) = avatars(&peers(3));
        assert_eq!(shown.len(), 3);
        assert_eq!(more, 0);
        assert_eq!(shown[0], ("P0".to_string(), 0));
        let (shown, more) = avatars(&peers(8));
        assert_eq!((shown.len(), more), (5, 3));
        let ann = vec![RemotePeer { name: "Ann Lee".into(), color: 4, sel: Selection::caret(Pos::body(0, 0)), typing: false }];
        assert_eq!(avatars(&ann).0[0], ("AL".to_string(), 4));
        let arabic = vec![RemotePeer { name: "حسام طه".into(), color: 1, sel: Selection::caret(Pos::body(0, 0)), typing: false }];
        assert_eq!(avatars(&arabic).0[0].0, "حط");
    }

    #[test]
    fn status_bar_counts_people() {
        assert_eq!(people_label(0), None);
        assert_eq!(people_label(1).as_deref(), Some("2 people editing"));
        assert_eq!(people_label(19).as_deref(), Some("20 people editing"));
    }

    #[test]
    fn jump_to_peer_moves_the_view_to_its_caret() {
        let mut app = WordApp::new(Session::new(Document::from_text("Hello world")), crate::Services::default());
        app.peers = peers(1);
        app.peers[0].sel = Selection::caret(Pos::body(0, 7));
        app.canvas.scroll_to_caret = false;
        app.run("ui.jumpToPeer", json!({"index": 0})).unwrap();
        assert_eq!(app.session.sel.focus, Pos::body(0, 7));
        assert!(app.canvas.scroll_to_caret);
        assert!(app.run("ui.jumpToPeer", json!({"index": 5})).is_err(), "no such person");
    }
}
