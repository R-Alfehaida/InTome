//! Document format engine: the [`Document`] trait every format implements,
//! plus the shared value types the rest of the app reads.
//!
//! Contract for implementors:
//! - A `Document` is `Send`: documents are opened on a worker thread and then
//!   handed to the UI thread. Hold no non-`Send` library handles; extract
//!   eagerly at open, or reopen the file by path when work needs the library
//!   again (PDF rendering for OCR does this).
//! - All text positions are char indices (Unicode scalar values) into
//!   [`Page::text`]. See `structured_text::elements`.
//! - Opening reports failure through [`OpenError`]; nothing panics on bad input.

pub mod cache;
pub mod formats;
pub mod registry;
pub mod utils;

use std::path::Path;

use bitflags::bitflags;

use crate::structured_text::elements::SemanticElement;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Capabilities: u32 {
        const TOC = 1 << 0;
        const STRUCTURED_NAV = 1 << 1;
        const IMAGES = 1 << 2;
        const SEARCHABLE_TEXT = 1 << 3;
        const NEEDS_OCR = 1 << 4;
        const FILLABLE_FORM = 1 << 5;
        /// Static/visual blanks (underscores, ruled lines) with no widgets.
        const FLAT_FORM = 1 << 6;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TocEntry {
    pub title: String,
    /// 0-based page this entry starts on.
    pub page_index: usize,
    /// Nesting depth: 0 = top level.
    pub level: usize,
    pub children: Vec<TocEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub page_index: usize,
    pub start_pos: usize,
    pub end_pos: usize,
    /// Surrounding text snippet.
    pub context: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocMetadata {
    pub title: String,
    pub author: String,
    /// Human-readable format name, e.g. "PDF", "EPUB".
    pub format: String,
    /// Any further key/value pairs a format exposes (subject, publisher, ...).
    pub extra: Vec<(String, String)>,
}

/// PDF form widget kinds (PyMuPDF's `PDF_WIDGET_TYPE_*` set, as an enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldType {
    Unknown,
    Button,
    CheckBox,
    ComboBox,
    ListBox,
    RadioButton,
    Signature,
    Text,
}

/// How the save path marks a synthetic (flat-form) field on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkStyle {
    /// A real AcroForm widget; the value is written to the widget.
    #[default]
    Widget,
    /// Stamp the typed value as text inside `rect`.
    Text,
    /// Draw an X in `rect` when checked.
    Checkbox,
    /// Draw an X in the chosen option's box (`choice_rects`).
    ChoiceBox,
    /// Underline the chosen printed word (`choice_rects`).
    SelectWord,
}

/// A page-space rectangle in PDF points: (x0, y0, x1, y1), origin top-left.
pub type Rect = [f32; 4];

#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    pub page_index: usize,
    pub field_type: FieldType,
    pub name: String,
    pub value: String,
    pub rect: Rect,
    pub choices: Vec<String>,
    /// Cached AI label, else author `/TU` label, else none.
    pub label: Option<String>,
    /// AI-generated group name.
    pub group: Option<String>,
    /// True for AI-discovered blanks on a flat (non-AcroForm) form.
    pub synthetic: bool,
    /// For a synthetic pick-one field: the rect to mark per entry in `choices`
    /// (same order, `None` for an empty "unselected" entry).
    pub choice_rects: Vec<Option<Rect>>,
    pub mark_style: MarkStyle,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub text: String,
    pub elements: Vec<SemanticElement>,
}

impl Page {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), elements: Vec::new() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    pub password: Option<String>,
    pub pdf_reading_mode: crate::config::PdfReadingMode,
    /// Characters per synthetic page for unpaginated formats.
    pub page_size: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("Unsupported file format: {0}")]
    Unsupported(String),
    #[error("The support for {0} is not available on this system.")]
    Unavailable(String),
    #[error("The document is password-protected.")]
    PasswordRequired,
    #[error("The password you entered is incorrect.")]
    WrongPassword,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl From<anyhow::Error> for OpenError {
    fn from(e: anyhow::Error) -> Self {
        OpenError::Other(format!("{e:#}"))
    }
}

pub trait Document: Send {
    fn path(&self) -> &Path;
    fn capabilities(&self) -> Capabilities;
    fn toc(&self) -> &[TocEntry];
    fn metadata(&self) -> DocMetadata;
    fn page_count(&self) -> usize;
    /// The page at `index` (0-based). Panics if out of range; callers check `page_count`.
    fn page(&mut self, index: usize) -> &Page;

    /// Replaces a page's text with an OCR result. No-op for formats without OCR.
    fn set_ocr_result(&mut self, _index: usize, _text: String, _elements: Vec<SemanticElement>) {}

    /// Pages that need OCR (no extractable text).
    fn ocr_page_indices(&mut self) -> Vec<usize> {
        Vec::new()
    }

    /// Fillable form fields, if any.
    fn form_fields(&mut self) -> Vec<FormField> {
        Vec::new()
    }

    /// Downcast hook for PDF-only operations (forms, rendering).
    fn as_pdf(&mut self) -> Option<&mut formats::pdf::PdfDocument> {
        None
    }

    /// Case-insensitive full-text search across all pages.
    fn search(&mut self, query: &str) -> Vec<SearchResult> {
        let mut results = Vec::new();
        for i in 0..self.page_count() {
            results.extend(search_text(i, &self.page(i).text, query));
        }
        results
    }
}

/// A frozen, plain-data copy of a document: every page (including OCR results
/// already applied), TOC and metadata. Background work such as export takes a
/// snapshot on the UI thread instead of sharing the live document.
pub struct SnapshotDocument {
    path: std::path::PathBuf,
    capabilities: Capabilities,
    toc: Vec<TocEntry>,
    metadata: DocMetadata,
    pages: Vec<Page>,
}

impl SnapshotDocument {
    pub fn of(doc: &mut dyn Document) -> Self {
        Self {
            path: doc.path().to_path_buf(),
            capabilities: doc.capabilities(),
            toc: doc.toc().to_vec(),
            metadata: doc.metadata(),
            pages: (0..doc.page_count()).map(|i| doc.page(i).clone()).collect(),
        }
    }
}

impl Document for SnapshotDocument {
    fn path(&self) -> &Path {
        &self.path
    }
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }
    fn toc(&self) -> &[TocEntry] {
        &self.toc
    }
    fn metadata(&self) -> DocMetadata {
        self.metadata.clone()
    }
    fn page_count(&self) -> usize {
        self.pages.len()
    }
    fn page(&mut self, index: usize) -> &Page {
        &self.pages[index]
    }
}

/// Case-insensitive search within one page's text; positions are char indices.
pub fn search_text(page_index: usize, text: &str, query: &str) -> Vec<SearchResult> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    // Lowercase per char, keeping a 1:1 index map so positions stay in `text` coordinates.
    let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let qlen = query.chars().count();
    let mut out = Vec::new();
    let mut i = 0;
    while i + q.len() <= lower.len() {
        if lower[i..i + q.len()] == q[..] {
            let ctx_start = i.saturating_sub(40);
            let ctx_end = (i + qlen + 40).min(chars.len());
            out.push(SearchResult {
                page_index,
                start_pos: i,
                end_pos: i + qlen,
                context: chars[ctx_start..ctx_end].iter().collect(),
            });
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_is_case_insensitive_with_char_positions() {
        let hits = search_text(2, "Café au lait, CAFÉ noir", "café");
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].page_index, hits[0].start_pos, hits[0].end_pos), (2, 0, 4));
        assert_eq!((hits[1].start_pos, hits[1].end_pos), (14, 18));
    }
}
