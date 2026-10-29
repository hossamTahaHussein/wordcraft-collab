//! Arabic spelling: the Ayaspell Hunspell dictionary (`assets/proofing/ar`, MPL), bundled
//! compressed and read with [`spellbook`] — it needs long flags, flag aliases and `IGNORE`, which
//! [`crate::affix`] doesn't read. Inflated and parsed the first time an Arabic word is checked
//! (about 50 MB in memory, so only when there is Arabic text).
//!
//! The dictionary ignores diacritics (tashkeel) and tatweel, reads presentation-form ligatures as
//! their letters, and knows the attached prefixes (`و`, `ف`, `ب`, `ال`…) through its affix rules.

use std::sync::OnceLock;

static AFF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ar.aff.z"));
static DIC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ar.dic.z"));

static DICT: OnceLock<Option<spellbook::Dictionary>> = OnceLock::new();

fn dict() -> Option<&'static spellbook::Dictionary> {
    DICT.get_or_init(load).as_ref()
}

fn load() -> Option<spellbook::Dictionary> {
    let text = |packed: &[u8]| -> Option<String> {
        let bytes = miniz_oxide::inflate::decompress_to_vec(packed).ok()?;
        String::from_utf8(bytes).ok()
    };
    let (aff, dic) = (text(AFF)?, text(DIC)?);
    if dic.is_empty() {
        return None;
    }
    spellbook::Dictionary::new(&aff, &dic).map_err(|e| log::warn!("Arabic dictionary: {e}")).ok()
}

/// Whether the Arabic dictionary is in this build and reads.
pub fn has_dictionary() -> bool {
    dict().is_some()
}

/// Is `word` an Arabic word? Without the dictionary every word is.
pub fn is_correct(word: &str) -> bool {
    dict().is_none_or(|d| d.check(word))
}

/// Up to `max` corrections for a misspelled Arabic word, best first.
pub fn suggest(word: &str, max: usize) -> Vec<String> {
    let Some(d) = dict() else { return Vec::new() };
    let mut out = Vec::new();
    d.suggest(word, &mut out);
    out.truncate(max);
    out
}

/// Does `word` have a letter of the Arabic script (Arabic, Persian, Urdu…)? Such words are
/// checked in the text's right-to-left language.
pub fn is_arabic_script(word: &str) -> bool {
    word.chars().any(|c| {
        c.is_alphabetic() && matches!(c, '\u{0600}'..='\u{06FF}' | '\u{0750}'..='\u{077F}' | '\u{08A0}'..='\u{08FF}' | '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}')
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IssueKind, Options, ProofLang, check_text_bidi, check_text_with, is_correct_in, suggest_in};

    #[test]
    fn arabic_words_are_checked_with_the_arabic_dictionary() {
        for w in ["الكتاب", "كتاب", "والكتاب", "بالمدرسة", "يكتبون", "سيذهبون", "فقالوا"] {
            assert!(is_correct(w), "{w}");
            assert!(is_correct_in(w, ProofLang::Ar), "{w}");
        }
        assert!(!is_correct("الكتبب"));
    }

    #[test]
    fn diacritics_and_tatweel_never_make_a_word_wrong() {
        for w in ["الكِتَابُ", "كَتَبَ", "مُدَرِّسَةٌ", "الكـتاب"] {
            assert!(is_correct(w), "{w}");
        }
    }

    #[test]
    fn presentation_form_ligatures_are_read_as_their_letters() {
        assert!(is_correct("ﻻ"));
    }

    #[test]
    fn misspellings_get_arabic_suggestions() {
        let s = suggest("الكتبب", 5);
        assert!(s.iter().any(|w| w == "الكتب"), "{s:?}");
        assert_eq!(suggest_in("الكتبب", 5, ProofLang::Ar), s);
    }

    #[test]
    fn the_language_tags_and_code() {
        assert_eq!(ProofLang::from_tag(Some("ar-SA")), ProofLang::Ar);
        assert_eq!(ProofLang::from_tag(Some("ar")), ProofLang::Ar);
        assert_eq!(ProofLang::from_tag(Some("ara")), ProofLang::Ar);
        assert_eq!(ProofLang::Ar.code(), "ar");
        assert!(ProofLang::Ar.is_proofed());
    }

    #[test]
    fn an_arabic_word_in_english_text_is_checked_in_arabic() {
        let text = "I read الكتبب and الكتاب today";
        let issues = check_text_with(text, &[(0..text.len(), ProofLang::En)], &Options::default(), false, false);
        assert_eq!(issues.len(), 1, "{issues:?}");
        let i = &issues[0];
        assert_eq!(&text[i.start..i.end], "الكتبب");
        assert_eq!(i.kind, IssueKind::Spelling);
        assert_eq!(i.lang, ProofLang::Ar);
        assert_eq!(i.message, "خطأ إملائي محتمل");
    }

    #[test]
    fn the_right_to_left_language_decides_for_arabic_script_words() {
        // Persian (no dictionary here): its words get no marks, English words still do.
        let text = "کتابها and speling";
        let spans = [(0..text.len(), ProofLang::En)];
        let issues = check_text_bidi(text, &spans, &[(0..text.len(), ProofLang::Other)], &Options::default(), false, false);
        let marked: Vec<&str> = issues.iter().map(|i| &text[i.start..i.end]).collect();
        assert_eq!(marked, ["speling"]);
        // Arabic said explicitly is the same as the default.
        let text = "الكتبب";
        let issues = check_text_bidi(text, &[(0..text.len(), ProofLang::En)], &[(0..text.len(), ProofLang::Ar)], &Options::default(), false, false);
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn a_word_with_diacritics_is_one_word() {
        let text = "الكِتَابُ الجَدِيدُ";
        assert_eq!(crate::words(text).len(), 2);
    }

    #[test]
    fn arabic_is_never_hyphenated() {
        let lim = crate::hyphen::Limits::default();
        assert_eq!(crate::hyphen::hyphenate_word_in("والمستشفيات", &lim, ProofLang::Ar), "والمستشفيات");
    }
}
