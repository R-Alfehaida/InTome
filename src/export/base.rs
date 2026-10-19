//! Exporter abstraction and options (port of `prism/export/base.py`).

use std::path::{Path, PathBuf};

use crate::cancel::CancelToken;
use crate::document::Document;

/// `(current, total, message)`.
pub type ProgressFn<'a> = &'a mut dyn FnMut(usize, usize, &str);

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub output_path: PathBuf,
    pub include_structure_markers: bool,
    pub include_page_numbers: bool,
    /// PDF: write the tag tree and PDF/UA metadata.
    pub tag_for_accessibility: bool,
    pub include_toc: bool,
    /// EPUB: write schema.org accessibility metadata.
    pub accessibility_metadata: bool,
    /// Gemini TTS voice for the audio formats when `expressive` is Off.
    pub voice: String,
    /// Audio formats: performance direction added before Gemini TTS reads the text.
    pub expressive: crate::config::Expressiveness,
    /// Expressive narration's narrator, which picks both of its voices.
    pub narrator: crate::config::NarratorVoice,
    /// 0-100. Prism accepts it but never applied it; kept for the dialog.
    pub audio_speed: u32,
}

impl ExportOptions {
    pub fn new(output_path: impl Into<PathBuf>) -> Self {
        Self {
            output_path: output_path.into(),
            include_structure_markers: true,
            include_page_numbers: true,
            tag_for_accessibility: true,
            include_toc: true,
            accessibility_metadata: true,
            voice: "Kore".into(),
            expressive: crate::config::Expressiveness::Off,
            narrator: crate::config::NarratorVoice::Feminine,
            audio_speed: 50,
        }
    }
}

/// Returned (as the `anyhow` root cause) when the cancel token fired. Nothing is written.
/// Callers detect it with `err.downcast_ref::<Cancelled>()`.
#[derive(Debug, thiserror::Error)]
#[error("Export cancelled")]
pub struct Cancelled;

pub trait Exporter: Send + Sync {
    /// Human-readable name, e.g. "Plain Text".
    fn format_name(&self) -> &'static str;
    /// Short key, e.g. "text".
    fn format_key(&self) -> &'static str;
    /// Extension including the dot, e.g. ".txt".
    fn file_extension(&self) -> &'static str;
    /// False when a runtime dependency (font, Gemini key) is missing.
    fn is_available(&self) -> bool {
        true
    }
    /// Writes the export and returns the main output path.
    fn export(
        &self,
        document: &mut dyn Document,
        options: &ExportOptions,
        cancel: &CancelToken,
        progress: ProgressFn,
    ) -> anyhow::Result<PathBuf>;
}

pub(crate) fn write_file(path: &Path, data: impl AsRef<[u8]>) -> anyhow::Result<()> {
    use anyhow::Context;
    std::fs::write(path, data).with_context(|| format!("cannot write {}", path.display()))
}
