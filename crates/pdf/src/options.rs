use super::{NumeralMode, WriteError};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PdfError {
    #[error("the document has no pages to export")]
    NoPages,
    #[error("page {0} does not exist")]
    BadPage(usize),
    #[error("PDF writing failed: {0}")]
    Write(String),
}

impl From<WriteError> for PdfError {
    fn from(error: WriteError) -> Self {
        Self::Write(error.to_string())
    }
}

/// Export options.
#[derive(Clone, Debug)]
pub struct PdfOptions {
    /// Pages to export (0-based layout page indexes, in output order); `None` = all.
    pub pages: Option<Vec<usize>>,
    /// Title / author overrides (default: the document properties).
    pub title: Option<String>,
    pub author: Option<String>,
    /// Write a structure tree (tagged PDF, best effort).
    pub tagged: bool,
    /// Show tracked changes as markup (coloured, underlined / struck through).
    pub include_markup: bool,
    /// Print Markup's comments: each page is scaled down to make room beside it (on the right, or
    /// the left for a right-to-left document) for the comments in balloons joined to their
    /// anchors (Word's "Document showing markup").
    pub comments: bool,
    /// Compress content streams.
    pub compress: bool,
    /// Print on this paper (width, height in points) instead of each page's own size: the page
    /// is scaled to fit and centred, the paper turned to the page's orientation (Scale to paper
    /// size, #2108). `None` keeps the document's page sizes.
    pub paper: Option<(f32, f32)>,
    /// How [`export`] shows digits (the Numeral setting; callers resolve `System`, see
    /// [`NumeralMode::resolve`]). [`export_layout`] keeps the layout it is given.
    pub numeral: NumeralMode,
    /// Arabic vowels (harakat, tanween, shadda…) in the text readers copy and search. On, copied
    /// text keeps them (poppler, pdf.js); Chrome's PDFium treats each vowel as a break and splits
    /// a vowelled word. Off, vowelled words copy and search whole everywhere, without vowels.
    pub arabic_vowels: bool,
}

impl Default for PdfOptions {
    fn default() -> Self {
        PdfOptions {
            pages: None,
            title: None,
            author: None,
            tagged: true,
            include_markup: false,
            comments: false,
            compress: true,
            paper: None,
            numeral: NumeralMode::Arabic,
            arabic_vowels: true,
        }
    }
}
