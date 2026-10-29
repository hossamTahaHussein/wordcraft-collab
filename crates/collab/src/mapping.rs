//! Where an edit happened, and where a position moves to because of it (spec §5). A story's
//! linear text (`wordcraft_doc::offsets`) is compared before and after: what the two share at
//! the start and at the end didn't change, and the bytes in between did.

use std::collections::BTreeSet;
use std::sync::Arc;

use wordcraft_doc::{Blocks, Document, StoryRef};

/// The changed part of a text: `start..old_end` before became `start..new_end` after. All three
/// are char boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

/// The changed part of `before` → `after`; `None` when they are the same.
pub fn span(before: &str, after: &str) -> Option<Span> {
    if before == after {
        return None;
    }
    let (a, b) = (before.as_bytes(), after.as_bytes());
    let mut start = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    // The shared start is identical in both texts, so a boundary in one is one in the other.
    while start > 0 && !before.is_char_boundary(start) {
        start -= 1;
    }
    let room = a.len().min(b.len()) - start;
    let suffix = a.iter().rev().zip(b.iter().rev()).take(room).take_while(|(x, y)| x == y).count();
    let mut old_end = a.len() - suffix;
    let mut new_end = b.len() - suffix;
    // Grow the ends to whole characters; the shared tail grows identically in both.
    while !before.is_char_boundary(old_end) || !after.is_char_boundary(new_end) {
        old_end += 1;
        new_end += 1;
    }
    Some(Span { start, old_end, new_end })
}

/// [`span`], placed at `hint` when the change could be there too. Inserting or deleting text that
/// repeats its neighbours ("o" before the "o" of "Hello") reads the same at several places, and
/// the shared-start rule picks the last; `hint` (where the edit's selection started) picks the
/// one that happened.
pub fn span_near(before: &str, after: &str, hint: usize) -> Option<Span> {
    let s = span(before, after)?;
    if hint >= s.start {
        return Some(s);
    }
    let (old_end, new_end) = (hint + (s.old_end - s.start), hint + (s.new_end - s.start));
    let fits = before.is_char_boundary(hint)
        && after.is_char_boundary(hint)
        && before.is_char_boundary(old_end)
        && after.is_char_boundary(new_end)
        && before.as_bytes().get(..hint) == after.as_bytes().get(..hint)
        && before.as_bytes().get(old_end..) == after.as_bytes().get(new_end..);
    Some(if fits { Span { start: hint, old_end, new_end } } else { s })
}

/// Where `offset` (in the text before) is after the change. The author's own caret at the
/// change goes after what they inserted; everyone else's stays before it.
pub fn map(span: &Span, offset: usize, is_author_caret: bool) -> usize {
    if offset < span.start || (offset == span.start && !is_author_caret) {
        offset
    } else if offset >= span.old_end && !(offset == span.start && is_author_caret) {
        offset - span.old_end + span.new_end
    } else {
        span.new_end
    }
}

/// The stories whose content differs between two versions of a document, found by comparing
/// the shared block pointers (an edit copies only the blocks it touches).
pub fn changed_stories(before: &Document, after: &Document) -> Vec<StoryRef> {
    let mut out = Vec::new();
    if !same_blocks(&before.body, &after.body) {
        out.push(StoryRef::Body);
    }
    let ids: BTreeSet<u32> = before.parts.keys().chain(after.parts.keys()).copied().collect();
    for id in ids {
        let same = match (before.parts.get(&id), after.parts.get(&id)) {
            (Some(a), Some(b)) => same_blocks(&a.blocks, &b.blocks),
            _ => false,
        };
        if !same {
            out.push(StoryRef::Part(id));
        }
    }
    out
}

fn same_blocks(a: &Blocks, b: &Blocks) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| Arc::ptr_eq(x, y) || x == y)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wordcraft_doc::{Block, Document, Paragraph, PartKind, StoryRef};

    use super::*;

    fn sp(before: &str, after: &str) -> Span {
        span(before, after).unwrap()
    }

    #[test]
    fn identical_text_has_no_span() {
        assert_eq!(span("abc", "abc"), None);
        assert_eq!(span("", ""), None);
    }

    #[test]
    fn insertion_span() {
        assert_eq!(sp("hello world", "hello big world"), Span { start: 6, old_end: 6, new_end: 10 });
    }

    #[test]
    fn deletion_span() {
        assert_eq!(sp("hello big world", "hello world"), Span { start: 6, old_end: 10, new_end: 6 });
    }

    #[test]
    fn repeated_letters_never_overlap_prefix_and_suffix() {
        // "aaa" → "aaaa": the prefix takes 3, the suffix may not take more than what's left.
        let s = sp("aaa", "aaaa");
        assert_eq!((s.start, s.old_end, s.new_end), (3, 3, 4));
    }

    #[test]
    fn span_near_places_an_ambiguous_insert_at_the_hint() {
        assert_eq!(span_near("Hello", "Helloo", 4), Some(Span { start: 4, old_end: 4, new_end: 5 }));
        assert_eq!(span_near("Hello", "Helloo", 5), Some(Span { start: 5, old_end: 5, new_end: 6 }));
        // A hint where the change can't be keeps the plain span.
        assert_eq!(span_near("Hello", "Helloo", 1), Some(Span { start: 5, old_end: 5, new_end: 6 }));
        assert_eq!(span_near("aaa", "aa", 0), Some(Span { start: 0, old_end: 1, new_end: 0 }));
    }

    #[test]
    fn mapping_table() {
        // "hello world" → "hello big world": insert "big " at 6.
        let ins = sp("hello world", "hello big world");
        let cases = [(0, false, 0), (5, false, 5), (6, false, 6), (6, true, 10), (7, false, 11), (11, false, 15)];
        for (offset, author, want) in cases {
            assert_eq!(map(&ins, offset, author), want, "insert: {offset} author={author}");
        }
        // "hello big world" → "hello world": delete "big " (6..10).
        let del = sp("hello big world", "hello world");
        for (offset, want) in [(5, 5), (6, 6), (8, 6), (10, 6), (15, 11)] {
            assert_eq!(map(&del, offset, false), want, "delete: {offset}");
        }
        // Replace "big" with "tiny": an offset inside the old text goes to the end of the new.
        let rep = sp("a big dog", "a tiny dog");
        assert_eq!(map(&rep, 3, false), 6);
        assert_eq!(map(&rep, 9, false), 10);
    }

    #[test]
    fn map_never_splits_chars() {
        let before = "مرحبا 👋 world";
        let after = "مرحبا 👍👋 world";
        let s = sp(before, after);
        assert!(before.is_char_boundary(s.start) && after.is_char_boundary(s.start));
        assert!(before.is_char_boundary(s.old_end) && after.is_char_boundary(s.new_end));
        for offset in (0..=before.len()).filter(|o| before.is_char_boundary(*o)) {
            for author in [false, true] {
                let m = map(&s, offset, author);
                assert!(after.is_char_boundary(m), "{offset} → {m}");
            }
        }
    }

    #[test]
    fn emoji_sharing_leading_bytes_is_one_change() {
        // 😀 F0 9F 98 80 and 😁 F0 9F 98 81 share three bytes: the span covers the whole char.
        let s = sp("a😀b", "a😁b");
        assert_eq!((s.start, s.old_end, s.new_end), (1, 5, 5));
    }

    #[test]
    fn a_megabyte_with_one_change_is_fast() {
        let before = "x".repeat(1 << 20);
        let mut after = before.clone();
        after.insert(1 << 19, 'y');
        let t = std::time::Instant::now();
        let s = sp(&before, &after);
        assert_eq!(s.start, 1 << 19);
        assert!(t.elapsed().as_millis() < 10, "{:?}", t.elapsed());
    }

    fn para(s: &str) -> Arc<Block> {
        Arc::new(Block::Para(Paragraph::with_text(s, Default::default())))
    }

    #[test]
    fn changed_stories_finds_only_what_changed() {
        let mut a = Document::new();
        a.body = vec![para("one"), para("two")];
        let h = a.add_part(PartKind::Header, vec![para("head")]);
        let mut b = a.clone();
        assert!(changed_stories(&a, &b).is_empty());
        b.body[1] = para("TWO");
        assert_eq!(changed_stories(&a, &b), [StoryRef::Body]);
        let mut c = a.clone();
        if let Some(part) = c.parts.get_mut(&h) {
            part.blocks = vec![para("HEAD")];
        }
        assert_eq!(changed_stories(&a, &c), [StoryRef::Part(h)]);
        let mut d = a.clone();
        let f = d.add_part(PartKind::Footnote, vec![para("note")]);
        assert_eq!(changed_stories(&a, &d), [StoryRef::Part(f)]);
        assert_eq!(changed_stories(&d, &a), [StoryRef::Part(f)]);
    }
}
