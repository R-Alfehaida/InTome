//! Format registry (port of `prism/export/registry.py`).

use crate::export::base::Exporter;
use crate::export::exporters::*;

/// Every built-in exporter in menu order.
fn all() -> Vec<Box<dyn Exporter>> {
    vec![
        Box::new(text::TextExporter),
        Box::new(html::HtmlExporter),
        Box::new(markdown::MarkdownExporter),
        Box::new(docx::DocxExporter),
        Box::new(epub::EpubExporter),
        Box::new(pdf::PdfExporter),
        Box::new(daisy_text::DaisyTextExporter),
        Box::new(daisy_audio::DaisyAudioExporter),
        Box::new(m4b::M4bExporter),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatInfo {
    pub key: &'static str,
    pub name: &'static str,
    /// Including the dot.
    pub extension: &'static str,
}

/// The formats whose dependencies are present, in menu order.
pub fn available_formats() -> Vec<FormatInfo> {
    all()
        .iter()
        .filter(|e| e.is_available())
        .map(|e| FormatInfo { key: e.format_key(), name: e.format_name(), extension: e.file_extension() })
        .collect()
}

/// A new exporter for `key`; errors if the key is unknown or the format unavailable.
pub fn create_exporter(key: &str) -> anyhow::Result<Box<dyn Exporter>> {
    let available: Vec<Box<dyn Exporter>> = all().into_iter().filter(|e| e.is_available()).collect();
    let keys: Vec<&str> = available.iter().map(|e| e.format_key()).collect();
    match available.into_iter().find(|e| e.format_key() == key) {
        Some(e) => Ok(e),
        None => anyhow::bail!("Export format '{key}' is not available. Available: {keys:?}"),
    }
}

/// `(label, pattern)` pairs for a save dialog, e.g. `("Plain Text (*.txt)", "*.txt")`.
pub fn file_dialog_filters() -> Vec<(String, String)> {
    available_formats()
        .into_iter()
        .map(|f| (format!("{} (*{})", f.name, f.extension), format!("*{}", f.extension)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_order_and_lookup() {
        let keys: Vec<_> = all().iter().map(|e| e.format_key()).collect();
        assert_eq!(keys, ["text", "html", "markdown", "docx", "epub", "pdf", "daisy_text", "daisy_audio", "m4b"]);
        assert_eq!(create_exporter("text").unwrap().file_extension(), ".txt");
        assert!(create_exporter("nope").is_err());
        assert!(file_dialog_filters().contains(&("Markdown (*.md)".to_string(), "*.md".to_string())));
    }
}
