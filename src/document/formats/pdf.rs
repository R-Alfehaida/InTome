//! STUB: port of prism/document/formats/pdf.py. Public signatures are the contract.

use std::path::Path;

use pdfium_render::prelude::Pdfium;

use crate::document::{Document, OpenError, OpenOptions};

pub fn open(_path: &Path, _opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    Err(OpenError::Unavailable("not yet ported".into()))
}

/// True when the bundled pdfium.dll loads.
pub fn available() -> bool {
    false
}

/// Runs `f` with the process-wide pdfium binding. Every pdfium use in the app
/// (OCR rendering, flat-form geometry, form saving) goes through here.
pub fn with_pdfium<R>(_f: impl FnOnce(&Pdfium) -> anyhow::Result<R>) -> anyhow::Result<R> {
    anyhow::bail!("pdfium not yet ported")
}

/// Renders one page to PNG bytes at `dpi`, opening its own handle (thread-safe).
pub fn render_page_png(_path: &Path, _password: Option<&str>, _page_index: usize, _dpi: u32) -> anyhow::Result<Vec<u8>> {
    anyhow::bail!("pdfium not yet ported")
}

/// Marker written into a flat form once it has been filled and saved.
pub const FLAT_FORM_FILLED_MARKER: &str = "intome-flatform-filled";

/// PDF document. Plain data (no pdfium handles) so it is `Send`.
pub struct PdfDocument;
