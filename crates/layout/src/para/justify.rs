use super::model::{ClKind, Cluster, StyleRun};
use super::proof::{ProofRun, lang_at};
use super::{ParaLayout, SOFT_HYPHEN};

/// Word's default hyphenation zone (0.25"), points.
pub(super) const HYPHENATION_ZONE: f32 = wordcraft_doc::DEFAULT_HYPHENATION_ZONE;

/// How much of their width the spaces of a justified line may give up to fit more text (Word
/// 2013+, compatibility mode 15).
const MAX_SPACE_SHRINK: f32 = 0.25;

/// How far a word (with the space before it) may overrun a justified line and still be fitted
/// onto it by shrinking the line's spaces, as a share of its own width.
const MAX_WORD_OVERRUN: f32 = 0.355;

/// How far a justified line's text may run past its right edge, its spaces (`spaces` wide in
/// all) shrinking to make room, when the word now ending the line is `word` wide with the space
/// before it. Measured in Word for Mac (compatibility mode 15, synthetic justified paragraphs):
/// at most a quarter of the spaces' width, and at most 0.355 of the word's own (0.351 to 0.360
/// across words of 2.7 to 80 pt and spaces of 3.3 and 6.7 pt).
pub(super) fn space_room(spaces: f32, word: f32) -> f32 {
    (spaces * MAX_SPACE_SHRINK).min(word.max(0.0) * MAX_WORD_OVERRUN)
}

/// Take `-slack` (an overflow) out of the line's inner spaces, in proportion to their widths and
/// never more than [`MAX_SPACE_SHRINK`] of them. Trailing spaces hang and are left alone.
pub(super) fn shrink_line_spaces(pl: &ParaLayout, xs: &mut [f32], c0: usize, c1: usize, content_end: f32, slack: f32) {
    let spaces: Vec<(usize, f32)> = (c0..c1)
        .filter_map(|k| {
            let c = pl.clusters.get(k).filter(|c| c.kind == ClKind::Space)?;
            (xs.get(k - c0).copied().unwrap_or(f32::MAX) < content_end - 0.01).then_some((k, c.adv))
        })
        .collect();
    let total: f32 = spaces.iter().map(|s| s.1).sum();
    if total <= 0.0 || !slack.is_finite() {
        return;
    }
    let ratio = (-slack / total).clamp(0.0, MAX_SPACE_SHRINK);
    let mut take = 0.0;
    let mut next = spaces.iter().peekable();
    for k in c0..c1 {
        if let Some(v) = xs.get_mut(k - c0) {
            *v -= take;
        }
        if let Some((_, adv)) = next.next_if(|s| s.0 == k) {
            take += adv * ratio;
        }
    }
    if let Some(v) = xs.last_mut() {
        *v -= take;
    }
}

/// The hyphen glyph and advance in a style (cached per paragraph).
pub(super) fn hyphen_glyph(pl: &ParaLayout, style: u32, cache: &mut Vec<(u32, u32, f32)>) -> (u32, u32, f32) {
    if let Some(h) = cache.iter().find(|h| h.0 == style) {
        return *h;
    }
    let h = pl
        .styles
        .get(style as usize)
        .and_then(|st| {
            let g = wordcraft_fonts::shape(&st.face, "-", &[], |c| c).into_iter().next()?;
            let k = st.size / st.face.upem.max(1.0) as f32 * st.rc.scale / 100.0;
            Some((style, g.gid, g.x_advance as f32 * k))
        })
        .unwrap_or((style, 0, 0.0));
    cache.push(h);
    h
}

/// Clusters after which a line may end with a hyphen: soft hyphens always, and dictionary or
/// pattern hyphenation points of each word, in the language of its run ([`ProofRun`]), when
/// automatic hyphenation is on (words in capitals only with `caps`). Both are found in `text`, the paragraph as laid out (without what
/// the layout leaves out, [`left_out`]), and `to_para` maps the end of a char there to the
/// paragraph's text.
pub(super) fn hyphenation_points(
    text: &str,
    to_para: impl Fn(usize) -> Option<usize>,
    pl: &ParaLayout,
    auto: bool,
    caps: bool,
    runs: &[ProofRun],
) -> Vec<u32> {
    let mut bytes: Vec<usize> = text.char_indices().filter(|(_, c)| *c == SOFT_HYPHEN).map(|(i, c)| i + c.len_utf8()).collect();
    // Vietnamese words are single syllables and never take a hyphen: no automatic hyphenation in
    // a Vietnamese paragraph, nor in a word written with a Vietnamese-only letter (`Nguyễn`).
    if auto && !wordcraft_proof::vi::looks_vietnamese(text) {
        let lim = wordcraft_proof::hyphen::Limits::default();
        let mut start: Option<usize> = None;
        for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
            let word_char = c.is_alphabetic() || c == '\'' || c == '\u{2019}';
            match (start, word_char) {
                (None, true) => start = Some(i),
                (Some(a), false) => {
                    // The patterns are for Latin-script languages: never hyphenate Persian/Arabic words.
                    // Words in capitals (`NASA`, `README`) only when the document allows it.
                    let all_caps = |w: &str| w.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase);
                    if let Some(w) = text.get(a..i)
                        && w.chars().count() >= lim.min_word
                        && !w.chars().any(wordcraft_fonts::is_rtl)
                        && !wordcraft_proof::vi::is_vietnamese_word(w)
                        && (caps || !all_caps(w))
                    {
                        // The word's language: its first letter's run, found by its place in `p.text`.
                        let first = w.chars().next().map_or(1, char::len_utf8);
                        let lang = to_para(a + first).map(|e| lang_at(runs, e.saturating_sub(first))).unwrap_or_default();
                        let offs: Vec<usize> = w.char_indices().map(|(o, _)| o).collect();
                        for pt in wordcraft_proof::hyphen::hyphen_points_in(w, &lim, lang) {
                            if let Some(o) = offs.get(pt) {
                                bytes.push(a + o);
                            }
                        }
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }
    let mut bytes: Vec<usize> = bytes.into_iter().filter_map(to_para).collect();
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes.sort_unstable();
    let mut out: Vec<u32> = pl
        .clusters
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.kind, ClKind::Text | ClKind::Marker) && bytes.binary_search(&c.end).is_ok())
        .map(|(i, _)| i as u32)
        .collect();
    out.dedup();
    out
}

/// The Arabic tatweel (kashida), drawn across a stretched join.
pub const TATWEEL: char = '\u{0640}';

/// How far one kashida may stretch a word, as a share of its font size; a justified line's room
/// beyond that goes to its spaces.
const MAX_KASHIDA: f32 = 1.0;

/// Does an Arabic-script letter join the letter after it (a dual-joining letter, or the
/// tatweel)? Right-joining letters (alef, dal, reh, waw…) and hamza don't.
fn joins_next(c: char) -> bool {
    joins_prev(c)
        && !matches!(c,
            '\u{0622}'..='\u{0625}' | '\u{0627}' | '\u{0629}' | '\u{062F}'..='\u{0632}' | '\u{0648}' | '\u{0671}'..='\u{0673}'
            | '\u{0675}'..='\u{0677}' | '\u{0688}'..='\u{0699}' | '\u{06C0}' | '\u{06C3}'..='\u{06CB}' | '\u{06CD}' | '\u{06CF}'
            | '\u{06D2}' | '\u{06D3}' | '\u{06EE}' | '\u{06EF}')
}

/// Is `c` an Arabic-script letter joined from the letter before it (any joining letter or the
/// tatweel; not hamza, which joins nothing)?
fn joins_prev(c: char) -> bool {
    matches!(c, '\u{0620}'..='\u{064A}' | '\u{066E}'..='\u{066F}' | '\u{0671}'..='\u{06D3}' | '\u{06D5}' | '\u{06EE}'..='\u{06EF}' | '\u{06FA}'..='\u{06FC}' | '\u{06FF}')
        && c != '\u{0621}'
}

/// An Arabic diacritic (harakat, shadda, superscript alef, Quranic marks).
fn is_arabic_mark(c: char) -> bool {
    matches!(c, '\u{0610}'..='\u{061A}' | '\u{064B}'..='\u{065F}' | '\u{0670}' | '\u{06D6}'..='\u{06DC}' | '\u{06DF}'..='\u{06E4}' | '\u{06E7}' | '\u{06E8}' | '\u{06EA}'..='\u{06ED}' | '\u{08D3}'..='\u{08FF}')
}

/// The clusters a kashida may widen (see [`super::ParaLayout::kashida_at`]): a text cluster
/// whose first letter is joined from the last letter of the cluster before it, in one style
/// whose font has a tatweel.
pub(super) fn kashida_points(text: &str, clusters: &[Cluster], styles: &[StyleRun]) -> Vec<u32> {
    let letter = |c: &&Cluster, last: bool| -> Option<char> {
        let t = text.get(c.start..c.end)?;
        // Diacritics ride on their letter: the letter decides.
        if last { t.chars().rev().find(|ch| !is_arabic_mark(*ch)) } else { t.chars().next() }
    };
    let mut tatweel: Vec<Option<bool>> = vec![None; styles.len()];
    let mut out = Vec::new();
    for (k, pair) in clusters.windows(2).enumerate() {
        let [a, b] = pair else { continue };
        if a.kind != ClKind::Text || b.kind != ClKind::Text || a.end != b.start || a.style != b.style {
            continue;
        }
        if !letter(&a, true).is_some_and(joins_next) || !letter(&b, false).is_some_and(joins_prev) {
            continue;
        }
        let Some(has) = tatweel.get_mut(a.style as usize) else { continue };
        if *has.get_or_insert_with(|| styles.get(a.style as usize).is_some_and(|s| s.face.glyph_for(TATWEEL) != 0)) {
            out.push(k as u32 + 1);
        }
    }
    out
}

/// The kashida of a justified line `c0..c1` (from `align_from`, before `content_end`), taking up
/// to `slack`: one per word, at its last join, each at most [`MAX_KASHIDA`] of its font size.
pub(super) fn line_kashida(
    pl: &super::ParaLayout,
    xs: &[f32],
    (c0, c1, align_from): (usize, usize, usize),
    (content_end, slack): (f32, f32),
) -> Vec<(usize, f32)> {
    let lo = pl.kashida_at.partition_point(|&k| (k as usize) < align_from.max(c0 + 1));
    let hi = pl.kashida_at.partition_point(|&k| (k as usize) < c1);
    let cands: Vec<usize> = pl
        .kashida_at
        .get(lo..hi)
        .unwrap_or_default()
        .iter()
        .map(|&k| k as usize)
        .filter(|&k| xs.get(k - c0).is_some_and(|x| *x < content_end - 0.01))
        .collect();
    // The last join of each word: the next candidate is in another word when something other
    // than text comes between them.
    let mut picked: Vec<usize> = Vec::new();
    for (i, &k) in cands.iter().enumerate() {
        let next_same_word = cands.get(i + 1).is_some_and(|&n| (k..n).all(|j| pl.clusters.get(j).is_some_and(|c| c.kind == ClKind::Text)));
        if !next_same_word {
            picked.push(k);
        }
    }
    if picked.is_empty() || slack <= 0.0 {
        return Vec::new();
    }
    let per = slack / picked.len() as f32;
    picked
        .into_iter()
        .map(|k| {
            let size = pl.clusters.get(k).and_then(|c| pl.styles.get(c.style as usize)).map_or(12.0, |s| s.size);
            (k, per.min(size * MAX_KASHIDA))
        })
        .collect()
}
