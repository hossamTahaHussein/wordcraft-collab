//! Story offsets: a position as one number, for co-editing.
//!
//! A story's *linear text* is its paragraphs' `text` in reading order, with one byte after
//! each paragraph (`\n`), after each table cell's blocks (`\u{7}`) and after each table row
//! (`\u{1}`). An offset counts bytes of that text, in the units of [`Pos::off`], so a position
//! survives as `(story, offset)` while the paths around it change, and two documents can be
//! compared by their linear text to see where an edit happened.

use crate::{Block, Blocks, Document, Paragraph, Path, Pos, StoryRef};

/// Deeper table nesting than this is not walked (offsets inside it are `None`).
const MAX_DEPTH: usize = 32;

const PARA_END: char = '\n';
const CELL_END: char = '\u{7}';
const ROW_END: char = '\u{1}';

/// The story's linear text (see the module docs); empty for a story that doesn't exist.
pub fn linear_text(doc: &Document, story: StoryRef) -> String {
    let mut out = String::new();
    if let Some(blocks) = doc.story(story) {
        walk(blocks, &mut Vec::new(), &mut 0, 0, &mut |visit| {
            match visit {
                Visit::Para(_, p, _) => {
                    out.push_str(&p.text);
                    out.push(PARA_END);
                }
                Visit::Marker(c) => out.push(c),
            }
            true
        });
    }
    out
}

/// The offset of `pos` in its story's linear text; `None` when the paragraph doesn't exist or
/// `pos.off` is past its end or inside a character.
pub fn offset_of(doc: &Document, pos: &Pos) -> Option<usize> {
    let p = doc.para(pos.story, &pos.path)?;
    if pos.off > p.text.len() || !p.text.is_char_boundary(pos.off) {
        return None;
    }
    let mut found = None;
    walk(doc.story(pos.story)?, &mut Vec::new(), &mut 0, 0, &mut |visit| {
        if let Visit::Para(path, _, start) = visit
            && path == pos.path.0.as_slice()
        {
            found = Some(start + pos.off);
            return false;
        }
        true
    });
    found
}

/// The position at `offset` in the story's linear text. An offset on a paragraph, cell or row
/// end is the end of the paragraph before it; one past the end clamps to the story's last
/// caret position; one inside a character snaps back to its start. A story that doesn't exist
/// (or has no paragraph) gives the body's start.
pub fn pos_at(doc: &Document, story: StoryRef, offset: usize) -> Pos {
    let mut last: Option<(Vec<u32>, usize)> = None;
    let mut hit: Option<Pos> = None;
    if let Some(blocks) = doc.story(story) {
        walk(blocks, &mut Vec::new(), &mut 0, 0, &mut |visit| {
            let Visit::Para(path, p, start) = visit else { return true };
            if offset < start {
                // On the markers between the previous paragraph and this one.
                return false;
            }
            if offset <= start + p.text.len() {
                hit = Some(Pos::new(story, Path(path.to_vec()), floor_boundary(p, offset - start)));
                return false;
            }
            last = Some((path.to_vec(), p.text.len()));
            true
        });
    }
    if let Some(pos) = hit {
        return pos;
    }
    match last {
        Some((path, len)) => Pos::new(story, Path(path), len),
        None => Pos::body(0, 0),
    }
}

fn floor_boundary(p: &Paragraph, mut off: usize) -> usize {
    off = off.min(p.text.len());
    while off > 0 && !p.text.is_char_boundary(off) {
        off -= 1;
    }
    off
}

/// What [`walk`] visits: a paragraph (its path and start offset), or a cell or row end marker.
enum Visit<'a> {
    Para(&'a [u32], &'a Paragraph, usize),
    Marker(char),
}

/// Visit every paragraph and marker in reading order; the visitor returns false to stop.
/// Returns false once stopped.
fn walk(blocks: &Blocks, path: &mut Vec<u32>, offset: &mut usize, depth: usize, visit: &mut dyn FnMut(Visit<'_>) -> bool) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    for (i, block) in blocks.iter().enumerate() {
        path.push(u32::try_from(i).unwrap_or(u32::MAX));
        let go_on = match &**block {
            Block::Para(p) => {
                let go_on = visit(Visit::Para(path, p, *offset));
                *offset = offset.saturating_add(p.text.len()).saturating_add(PARA_END.len_utf8());
                go_on
            }
            Block::Table(t) => {
                let mut go_on = true;
                'rows: for (r, row) in t.rows.iter().enumerate() {
                    for (c, cell) in row.cells.iter().enumerate() {
                        path.push(u32::try_from(r).unwrap_or(u32::MAX));
                        path.push(u32::try_from(c).unwrap_or(u32::MAX));
                        go_on = walk(&cell.blocks, path, offset, depth + 1, visit);
                        path.truncate(path.len() - 2);
                        if !go_on {
                            break 'rows;
                        }
                        visit(Visit::Marker(CELL_END));
                        *offset = offset.saturating_add(1);
                    }
                    visit(Visit::Marker(ROW_END));
                    *offset = offset.saturating_add(1);
                }
                go_on
            }
        };
        path.pop();
        if !go_on {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{Block, Document, Paragraph, PartKind, Path, Pos, StoryRef, Table};

    fn para(s: &str) -> Arc<Block> {
        Arc::new(Block::Para(Paragraph::with_text(s, Default::default())))
    }

    /// "Hi" · a 2×2 table (a, b / c, d) · "Bye" · "مرحبا 👋".
    fn doc() -> Document {
        let mut d = Document::new();
        let mut t = Table::new(2, 2, 100.0);
        for (r, row) in [["a", "b"], ["c", "d"]].iter().enumerate() {
            for (c, text) in row.iter().enumerate() {
                t.rows[r].cells[c] = crate::Cell::with_text(text);
            }
        }
        d.body = vec![para("Hi"), Arc::new(Block::Table(t)), para("Bye"), para("مرحبا 👋")];
        d
    }

    #[test]
    fn table_cells_are_walked_in_order() {
        assert_eq!(linear_text(&doc(), StoryRef::Body), "Hi\na\n\u{7}b\n\u{7}\u{1}c\n\u{7}d\n\u{7}\u{1}Bye\nمرحبا 👋\n");
    }

    #[test]
    fn round_trip_every_caret_position() {
        let d = doc();
        let mut checked = 0;
        let mut paths = vec![Path(vec![0]), Path(vec![2]), Path(vec![3])];
        for r in 0..2 {
            for c in 0..2 {
                paths.push(Path(vec![1, r, c, 0]));
            }
        }
        for path in paths {
            let p = d.para(StoryRef::Body, &path).unwrap();
            for off in (0..=p.text.len()).filter(|o| p.text.is_char_boundary(*o)) {
                let pos = Pos::new(StoryRef::Body, path.clone(), off);
                let o = offset_of(&d, &pos).unwrap();
                assert_eq!(pos_at(&d, StoryRef::Body, o), pos, "offset {o}");
                checked += 1;
            }
        }
        assert!(checked > 20);
    }

    #[test]
    fn offsets_count_paragraph_and_cell_ends() {
        let d = doc();
        assert_eq!(offset_of(&d, &Pos::body(0, 2)), Some(2));
        // "Hi\n" then cell a's paragraph starts at 3.
        assert_eq!(offset_of(&d, &Pos::new(StoryRef::Body, Path(vec![1, 0, 0, 0]), 0)), Some(3));
        assert_eq!(offset_of(&d, &Pos::new(StoryRef::Body, Path(vec![1, 0, 1, 0]), 0)), Some(6));
        assert_eq!(offset_of(&d, &Pos::body(2, 0)), Some(17));
    }

    #[test]
    fn offset_on_a_cell_marker_resolves_to_the_paragraph_before() {
        let d = doc();
        // Offset 5 is the cell end after "a\n": end of cell a's paragraph.
        assert_eq!(pos_at(&d, StoryRef::Body, 5), Pos::new(StoryRef::Body, Path(vec![1, 0, 0, 0]), 1));
    }

    #[test]
    fn offset_past_end_clamps() {
        let d = doc();
        let last = d.para(StoryRef::Body, &Path(vec![3])).unwrap().text.len();
        assert_eq!(pos_at(&d, StoryRef::Body, 10_000), Pos::body(3, last));
    }

    #[test]
    fn offset_inside_a_character_snaps_down() {
        let d = doc();
        let start = offset_of(&d, &Pos::body(3, 0)).unwrap();
        // "م" is 2 bytes: start + 1 is inside it.
        assert_eq!(pos_at(&d, StoryRef::Body, start + 1), Pos::body(3, 0));
    }

    #[test]
    fn header_part_story() {
        let mut d = doc();
        let id = d.add_part(PartKind::Header, vec![para("Head"), para("er")]);
        let s = StoryRef::Part(id);
        assert_eq!(linear_text(&d, s), "Head\ner\n");
        assert_eq!(offset_of(&d, &Pos::new(s, Path(vec![1]), 1)), Some(6));
        assert_eq!(pos_at(&d, s, 6), Pos::new(s, Path(vec![1]), 1));
    }

    #[test]
    fn bad_path_is_none() {
        let d = doc();
        assert_eq!(offset_of(&d, &Pos::body(9, 0)), None);
        assert_eq!(offset_of(&d, &Pos::body(0, 99)), None);
        assert_eq!(offset_of(&d, &Pos::new(StoryRef::Part(77), Path(vec![0]), 0)), None);
    }
}
