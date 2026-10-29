use super::{
    Exporter, FaceRef, Fill, FillRule, GlyphId, KrillaGlyph, Point, Rgb, Stroke, Surface, Transform, baseline, cluster_ends, fill, glyph_outlines,
    left_to_right, norm, ok, paint_of, rgb, split_clusters, visual_order,
};

/// How a run of glyphs is painted: its colour, opacity and synthetic bold and italic.
#[derive(Clone, Copy)]
pub(super) struct GlyphPaint {
    pub color: Rgb,
    pub alpha: f32,
    pub bold: bool,
    pub italic: bool,
}

/// Within each character's glyphs (one text range: a letter drawn as a dotless body and its dots,
/// a base and its marks), the glyph with the widest advance carries the text and the marks (no
/// advance) none.
/// Readers place an extracted character between the first and last glyph that carry it: with the
/// dots inside, an Arabic letter (Noto draws ب ت ن ي… as a body and dots) started or ended
/// mid-letter, and poppler (pdftotext, Evince, Okular) saw gaps there and split words with spaces.
fn text_on_base_glyphs(face: &FaceRef, placed: &mut [(u32, f32, f32, std::ops::Range<usize>)]) {
    let mut i = 0;
    while i < placed.len() {
        let range = placed[i].3.clone();
        let mut j = i + 1;
        while placed.get(j).is_some_and(|p| p.3 == range) {
            j += 1;
        }
        if j - i > 1
            && let Some(cluster) = placed.get_mut(i..j)
        {
            // Only marks (no advance) give their text up: several full glyphs for one character
            // (ß shown as SS) keep it, and the reader's ActualText names them together.
            let base = (0..cluster.len()).max_by(|a, b| face.advance(cluster[*a].0).total_cmp(&face.advance(cluster[*b].0))).unwrap_or(0);
            if face.advance(cluster[base].0) > 0.0 {
                for (k, g) in cluster.iter_mut().enumerate() {
                    if k != base && face.advance(g.0) <= 0.0 {
                        g.3 = range.end..range.end;
                    }
                }
            }
        }
        i = j;
    }
}

/// An Arabic diacritic: harakat, tanween, shadda, sukun, superscript alef, Quranic marks.
fn is_arabic_mark(c: char) -> bool {
    matches!(c, '\u{0610}'..='\u{061A}' | '\u{064B}'..='\u{065F}' | '\u{0670}' | '\u{06D6}'..='\u{06DC}' | '\u{06DF}'..='\u{06E4}' | '\u{06E7}' | '\u{06E8}' | '\u{06EA}'..='\u{06ED}' | '\u{08D3}'..='\u{08FF}')
}

impl Exporter<'_> {
    /// Unicode text for each glyph: (text, per-glyph byte range). Glyphs and the run's text are
    /// aligned through the font's reverse character map (ligatures take several characters,
    /// extra glyphs share their cluster's text).
    fn map_text(&mut self, face: &FaceRef, glyphs: &[(u32, f32, f32)], text: &str) -> (String, Vec<std::ops::Range<usize>>) {
        let cmap = self.cmap(face);
        let rev = |g: u32| cmap.get(&g).copied();
        if text.is_empty() {
            let mut t = String::new();
            let ranges = glyphs
                .iter()
                .map(|(g, _, _)| {
                    let a = t.len();
                    t.push(rev(*g).unwrap_or(' '));
                    a..t.len()
                })
                .collect();
            return (t, ranges);
        }
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let n = chars.len();
        let byte = |i: usize| chars.get(i).map(|c| c.0).unwrap_or(text.len());
        let mut ranges = Vec::with_capacity(glyphs.len());
        let mut i = 0usize;
        for (k, (g, _, _)) in glyphs.iter().enumerate() {
            let left_g = glyphs.len() - k;
            let left_c = n.saturating_sub(i);
            if left_c == 0 {
                let r = ranges.last().cloned().unwrap_or(0..text.len());
                ranges.push(r);
                continue;
            }
            if k + 1 == glyphs.len() {
                ranges.push(byte(i)..text.len());
                i = n;
                continue;
            }
            let this = chars.get(i).map(|c| c.1);
            let take = if left_c <= left_g || rev(*g) == this {
                if left_g > left_c && rev(*g) != this && k > 0 {
                    // More glyphs than characters: this one shares the previous cluster.
                    let r = ranges.last().cloned().unwrap_or(byte(i)..byte(i + 1));
                    ranges.push(r);
                    continue;
                }
                1
            } else {
                // A ligature (or substituted glyph): consume until the next glyph's character.
                let next = glyphs.get(k + 1).and_then(|(ng, _, _)| rev(*ng));
                let max = left_c - (left_g - 1);
                let mut t = 1;
                while t < max && chars.get(i + t).map(|c| c.1) != next {
                    t += 1;
                }
                if t == max && next.is_some() && chars.get(i + t).map(|c| c.1) != next {
                    // The next glyph doesn't match anything: assume one character each.
                    t = 1;
                }
                t
            };
            ranges.push(byte(i)..byte(i + take));
            i += take;
        }
        (text.to_string(), ranges)
    }

    pub(super) fn glyphs(
        &mut self,
        s: &mut Surface,
        face: &FaceRef,
        size: f32,
        glyphs: &[(u32, f32, f32)],
        style: GlyphPaint,
        (text, known): (&str, &[std::ops::Range<usize>]),
    ) {
        let GlyphPaint { color, alpha, bold, italic } = style;
        if !size.is_finite() || size <= 0.0 || size > 5000.0 {
            return;
        }
        if glyphs.is_empty() {
            return;
        }
        if !glyphs.iter().all(|(_, x, y)| ok(*x) && ok(*y)) {
            return;
        }
        let font = self.font(face);
        if font.is_none() && !self.outlined.contains(&face.id()) {
            log::warn!("PDF: font {} {} could not be embedded", face.family, face.style);
            return;
        }
        // Layout's glyph → text mapping when it has one (right-to-left text, contextual forms and
        // ligatures can't be guessed back from the font's cmap), else a guess.
        let valid = known.len() == glyphs.len() && known.iter().all(|r| r.start < r.end && text.get(r.clone()).is_some());
        let (txt, ranges) = if valid { (text.to_string(), known.to_vec()) } else { self.map_text(face, glyphs, text) };
        let upem = face.upem.max(1.0) as f32;
        // Right-to-left text is written left to right as it appears (see `left_to_right`); glyphs
        // already in that order are written as they are.
        let (order, ranges) = if left_to_right(glyphs) {
            ((0..glyphs.len()).collect(), ranges)
        } else {
            let parts = split_clusters(&txt, glyphs, &ranges, &self.cmap(face));
            (visual_order(&txt, glyphs, &ranges, &parts), parts)
        };
        let mut placed: Vec<(u32, f32, f32, std::ops::Range<usize>)> =
            order.iter().filter_map(|&i| Some((glyphs.get(i).copied()?, ranges.get(i).cloned()?))).map(|((g, x, y), r)| (g, x, y, r)).collect();
        text_on_base_glyphs(face, &mut placed);
        // The glyphs that carry no text (a letter's dots, marks) are drawn as shapes, so readers
        // see only the glyphs that carry characters.
        // Arabic vowel marks (fatha, tanween, shadda…) are characters of their own, drawn over or
        // under their letter. In the text they broke words apart (Chrome's PDFium took a raised
        // mark for another line: الحلًوة was found as الحل … ًوة), so search failed. They are
        // drawn as shapes and left out of the text, as most readers' search ignores them anyway:
        // the words copy and search whole, without their vowels.
        let vowel = |p: &(u32, f32, f32, std::ops::Range<usize>)| {
            face.advance(p.0) <= 0.0 && txt.get(p.3.clone()).is_some_and(|t| !t.is_empty() && t.chars().all(is_arabic_mark))
        };
        let marks: Vec<(u32, f32, f32)> = placed.iter().filter(|p| p.3.is_empty() || vowel(p)).map(|p| (p.0, p.1, p.2)).collect();
        placed.retain(|p| !p.3.is_empty() && !vowel(p));
        // A letter whose text holds its vowels too (one glyph group) keeps only the letter.
        for p in &mut placed {
            if let Some(t) = txt.get(p.3.clone()) {
                let kept = t.trim_end_matches(is_arabic_mark).len();
                if kept > 0 && kept < t.len() {
                    p.3 = p.3.start..p.3.start + kept;
                }
            }
        }
        let Some(&(_, x0, y0, _)) = placed.first() else { return };
        let kg: Vec<KrillaGlyph> = placed
            .iter()
            .enumerate()
            .map(|(k, (g, x, y, r))| {
                let adv = match placed.get(k + 1) {
                    Some((_, nx, _, _)) => (nx - x) / size,
                    None => face.advance(*g) as f32 / upem,
                };
                KrillaGlyph::new(GlyphId::new(*g), adv, 0.0, (y0 - y) / size, 0.0, r.clone(), None)
            })
            .collect();
        let pushed = italic;
        if italic {
            // The run's baseline: the y most of its glyphs share (marks sit higher or lower).
            let base = baseline(glyphs).unwrap_or(y0);
            s.push_transform(&Transform::from_row(1.0, 0.0, -0.21, 1.0, 0.21 * base, 0.0));
        }
        s.set_fill(Some(fill(color, alpha)));
        s.set_stroke(if bold {
            Some(Stroke { paint: rgb::Color::new(color.0, color.1, color.2).into(), width: size * 0.03, opacity: norm(alpha), ..Default::default() })
        } else {
            None
        });
        match font {
            Some(font) => {
                s.draw_glyphs(Point::from_xy(x0, y0), &kg, font, &txt, size, false);
                // One shape per mark, each where layout put it.
                for mark in &marks {
                    if let Some(path) = glyph_outlines(face, size, std::slice::from_ref(mark)) {
                        s.draw_path(&path);
                    }
                }
            }
            None => {
                if let Some(path) = glyph_outlines(face, size, glyphs) {
                    s.draw_path(&path);
                }
            }
        }
        s.set_fill(None);
        s.set_stroke(None);
        if pushed {
            s.pop();
        }
    }

    pub(super) fn link_rect(&mut self, face: &FaceRef, size: f32, glyphs: &[(u32, f32, f32)], link: &str) {
        let (Some(first), Some(last)) = (glyphs.first(), glyphs.last()) else { return };
        let adv = face.advance(last.0) as f32 * size / face.upem.max(1.0) as f32;
        let (x0, x1) = (first.1.min(last.1), first.1.max(last.1) + adv);
        let (y0, y1) = (first.2 - size * 0.85, first.2 + size * 0.25);
        if ![x0, x1, y0, y1].iter().all(|v| ok(*v)) {
            return;
        }
        if let Some(prev) = self.links.last_mut()
            && prev.4 == link
            && (prev.1 - y0).abs() < 0.5
            && x0 - prev.2 < size
        {
            prev.0 = prev.0.min(x0);
            prev.2 = prev.2.max(x1);
            return;
        }
        if self.links.len() < 10_000 {
            self.links.push((x0, y0, x1, y1, link.to_string()));
        }
    }

    /// Draws a metafile text run as PDF text (outlines when the font cannot be embedded). `tf` maps
    /// the run's own space (see [`PlacedItem::Text`]) to the page.
    pub(crate) fn vector_text(&mut self, s: &mut Surface, t: &wordcraft_metafile::Text, tf: kurbo::Affine) {
        let face = wordcraft_fonts::word::resolve(&t.font, t.bold, t.italic).face;
        let shaped = wordcraft_fonts::shape(&face, &t.text, &[], |c| c);
        let glyphs: Vec<_> = shaped.iter().map(|g| (g.cluster, f64::from(g.x_advance), f64::from(g.x_offset), f64::from(g.y_offset))).collect();
        let Some(lay) = t.layout(face.upem, face.ascent, face.descent, &glyphs) else { return };
        let font = self.font(&face);
        if font.is_none() && !self.outlined.contains(&face.id()) {
            return;
        }
        let em = lay.em as f32;
        let c = tf.as_coeffs().map(|v| v as f32);
        if !c.iter().all(|v| v.is_finite()) || !em.is_finite() {
            return;
        }
        let (paint, opacity) = paint_of(t.color);
        s.push_transform(&Transform::from_row(c[0], c[1], c[2], c[3], c[4], c[5]));
        s.set_stroke(None);
        s.set_fill(Some(Fill { paint: paint.into(), opacity, rule: FillRule::NonZero }));
        match font {
            Some(font) => {
                let upem = face.upem.max(1.0) as f32;
                let ends = cluster_ends(&shaped.iter().map(|g| g.cluster).collect::<Vec<_>>(), t.text.len());
                let kg: Vec<KrillaGlyph> = shaped
                    .iter()
                    .enumerate()
                    .map(|(i, g)| {
                        let end = ends.get(i).copied().unwrap_or(t.text.len());
                        let start = g.cluster.min(end);
                        let x = lay.glyphs.get(i).map_or(0.0, |p| p.0);
                        let adv = match lay.glyphs.get(i + 1) {
                            Some(n) => ((n.0 - x) / lay.em) as f32,
                            None => g.x_advance as f32 / upem,
                        };
                        let dy = lay.glyphs.get(i).map_or(0.0, |p| ((lay.baseline - p.1) / lay.em) as f32);
                        KrillaGlyph::new(GlyphId::new(g.gid), adv, 0.0, dy, 0.0, start..end, None)
                    })
                    .collect();
                let x0 = lay.glyphs.first().map_or(0.0, |p| p.0) as f32;
                s.draw_glyphs(Point::from_xy(x0, lay.baseline as f32), &kg, font, &t.text, em, false);
            }
            None => {
                let pen: Vec<(u32, f32, f32)> = shaped.iter().zip(&lay.glyphs).map(|(g, (x, y))| (g.gid, *x as f32, *y as f32)).collect();
                if let Some(path) = glyph_outlines(&face, em, &pen) {
                    s.draw_path(&path);
                }
            }
        }
        s.set_fill(None);
        s.pop();
    }
}
