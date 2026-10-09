//! Image files. Port of prism/document/formats/image.py.
//!
//! The file is never decoded here: the document is one page of instructions
//! until OCR text arrives through `set_ocr_result`, and the OCR stage reads the
//! image itself from `path()`.

use std::path::{Path, PathBuf};

use super::word::file_name;
use crate::document::{Capabilities, DocMetadata, Document, OpenError, OpenOptions, Page, TocEntry};
use crate::structured_text::elements::SemanticElement;

pub struct ImageDocument {
    path: PathBuf,
    page: Page,
}

fn placeholder_text(path: &Path) -> String {
    format!(
        "Image file: {}\n\n\
         This is an image file. OCR processing is required to extract text.\n\
         Press Ctrl+Shift+O to run OCR, or configure an OCR backend \
         (Gemini, Windows OCR, or Tesseract) in \
         Preferences > AI Providers.",
        file_name(path)
    )
}

pub fn open(path: &Path, _opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    Ok(Box::new(ImageDocument { path: path.to_path_buf(), page: Page::plain(placeholder_text(path)) }))
}

impl Document for ImageDocument {
    fn path(&self) -> &Path {
        &self.path
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::NEEDS_OCR | Capabilities::IMAGES
    }
    fn toc(&self) -> &[TocEntry] {
        &[]
    }
    fn metadata(&self) -> DocMetadata {
        DocMetadata { title: file_name(&self.path), format: "Image".into(), ..Default::default() }
    }
    fn page_count(&self) -> usize {
        1
    }
    fn page(&mut self, _index: usize) -> &Page {
        &self.page
    }
    /// Replaces the page text; new elements replace the old ones only when there are any.
    fn set_ocr_result(&mut self, _index: usize, text: String, elements: Vec<SemanticElement>) {
        self.page.text = text;
        if !elements.is_empty() {
            self.page.elements = elements;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured_text::elements::{ElementMeta, ElementType};

    fn heading(text: &str) -> SemanticElement {
        SemanticElement { element_type: ElementType::Heading1, start_pos: 0, end_pos: text.len(), text: text.into(), meta: ElementMeta::default() }
    }

    #[test]
    fn page_shows_ocr_instructions_until_a_result_arrives() {
        let mut doc = open(Path::new("C:/scans/photo.png"), &OpenOptions::default()).unwrap();
        assert_eq!(
            doc.page(0).text,
            "Image file: photo.png\n\nThis is an image file. OCR processing is required to extract text.\n\
             Press Ctrl+Shift+O to run OCR, or configure an OCR backend (Gemini, Windows OCR, or Tesseract) in Preferences > AI Providers."
        );
        assert_eq!(doc.capabilities(), Capabilities::NEEDS_OCR | Capabilities::IMAGES);

        doc.set_ocr_result(0, "Hello".into(), vec![heading("Hello")]);
        doc.set_ocr_result(0, "Hello again".into(), Vec::new());
        let page = doc.page(0);
        assert_eq!((page.text.as_str(), page.elements.len()), ("Hello again", 1));
    }
}
