//! Format registry: maps file extensions to document openers.
//!
//! Every format module exposes `pub fn open(path, opts) -> Result<Box<dyn Document>, OpenError>`
//! and is listed once in [`FORMATS`]. `available` reports whether the format's
//! runtime dependency (pdfium.dll, pandoc, antiword, ...) is present; unavailable
//! formats disappear from the open dialog and fail with `OpenError::Unavailable`.

use std::path::Path;

use super::formats::*;
use super::{Document, OpenError, OpenOptions};

pub type Opener = fn(&Path, &OpenOptions) -> Result<Box<dyn Document>, OpenError>;

pub struct FormatEntry {
    pub extensions: &'static [&'static str],
    pub description: &'static str,
    pub open: Opener,
    pub available: fn() -> bool,
}

fn always() -> bool {
    true
}

pub static FORMATS: &[FormatEntry] = &[
    FormatEntry { extensions: &[".pdf"], description: "PDF documents", open: pdf::open, available: pdf::available },
    FormatEntry { extensions: &[".epub"], description: "EPUB ebooks", open: epub::open, available: always },
    FormatEntry { extensions: &[".docx"], description: "Word documents", open: word::open, available: always },
    FormatEntry { extensions: &[".doc"], description: "Legacy Word documents", open: word::open_doc, available: word::doc_available },
    FormatEntry { extensions: &[".pptx"], description: "PowerPoint presentations", open: powerpoint::open, available: always },
    FormatEntry { extensions: &[".odt", ".odp"], description: "OpenDocument files", open: odf::open, available: always },
    FormatEntry { extensions: &[".mobi", ".azw3", ".azw"], description: "MOBI ebooks", open: mobi::open, available: always },
    FormatEntry { extensions: &[".fb2"], description: "FictionBook e-books", open: fb2::open, available: always },
    FormatEntry { extensions: &[".rtf"], description: "Rich Text Format", open: rtf::open, available: always },
    FormatEntry { extensions: &[".md", ".markdown"], description: "Markdown files", open: markdown::open, available: always },
    FormatEntry { extensions: &[".html", ".htm", ".xhtml"], description: "HTML documents", open: html::open, available: always },
    FormatEntry { extensions: &[".txt", ".log", ".ini", ".cfg"], description: "Text files", open: text::open, available: always },
    FormatEntry { extensions: &[".csv", ".tsv"], description: "Tabular data", open: csv::open, available: always },
    FormatEntry { extensions: &[".jpg", ".jpeg", ".png", ".tiff", ".tif", ".bmp"], description: "Image files", open: image::open, available: always },
    FormatEntry { extensions: &[".zip"], description: "ZIP archives", open: archive::open_zip, available: always },
    FormatEntry { extensions: &[".rar"], description: "RAR archives", open: archive::open_rar, available: always },
    FormatEntry { extensions: &[".tex", ".latex"], description: "LaTeX documents", open: pandoc::open, available: pandoc::available },
    FormatEntry { extensions: &[".docbook"], description: "DocBook documents", open: pandoc::open, available: pandoc::available },
    FormatEntry { extensions: &[".ipynb"], description: "Jupyter notebooks", open: pandoc::open, available: pandoc::available },
];

fn extension(path: &Path) -> String {
    path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default()
}

fn entry_for(path: &Path) -> Option<&'static FormatEntry> {
    let ext = extension(path);
    FORMATS.iter().find(|f| f.extensions.contains(&ext.as_str()))
}

/// Opens `path` with the handler registered for its extension.
pub fn open_document(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let entry = entry_for(path).ok_or_else(|| OpenError::Unsupported(extension(path)))?;
    if !(entry.available)() {
        return Err(OpenError::Unavailable(format!("{} ({})", entry.description, extension(path))));
    }
    (entry.open)(path, opts)
}

pub fn is_supported(path: &Path) -> bool {
    entry_for(path).is_some_and(|e| (e.available)())
}

pub fn available_extensions() -> Vec<&'static str> {
    FORMATS.iter().filter(|f| (f.available)()).flat_map(|f| f.extensions.iter().copied()).collect()
}

/// File-dialog filters: "All supported files" first (as `*.*`; a 30+ pattern
/// filter greys out the Open button on Windows), then one entry per format.
pub fn file_filters() -> Vec<(String, String)> {
    let mut out = vec![("All supported files (*.*)".to_string(), "*.*".to_string())];
    for f in FORMATS.iter().filter(|f| (f.available)()) {
        let pats = f.extensions.iter().map(|e| format!("*{e}")).collect::<Vec<_>>().join(";");
        out.push((format!("{} ({pats})", f.description), pats));
    }
    out
}
