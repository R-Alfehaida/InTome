//! Plain text documents (.txt, .log, .ini, .cfg) with paragraph-boundary pagination.
//! Port of prism/document/formats/text.py.

use std::path::Path;

use crate::document::utils::{PagedDocument, page_size_of, paginate_text, read_text_file};
use crate::document::{Document, OpenError, OpenOptions, Page};

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let text = read_text_file(path)?;
    let pages = paginate_text(&text, page_size_of(opts)).into_iter().map(Page::plain).collect();
    Ok(Box::new(PagedDocument::new(path, "Plain Text", PagedDocument::file_name(path), pages, Vec::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_utf8_and_cp1252_with_normalised_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let utf8 = dir.path().join("a.txt");
        std::fs::write(&utf8, "Caf\u{e9}\r\n\r\nna\u{ef}ve").unwrap();
        let mut doc = open(&utf8, &OpenOptions::default()).unwrap();
        assert_eq!(doc.page_count(), 1);
        assert_eq!(doc.page(0).text, "Café\n\nnaïve");
        assert_eq!(doc.metadata().title, "a.txt");
        assert_eq!(doc.metadata().format, "Plain Text");

        let cp = dir.path().join("b.txt");
        std::fs::write(&cp, b"Caf\xe9 notes, na\xefve fianc\xe9e and cr\xe8me br\xfbl\xe9e.\r\n").unwrap();
        let mut doc = open(&cp, &OpenOptions { page_size: 20, ..Default::default() }).unwrap();
        assert_eq!(doc.page_count(), 3);
        assert_eq!(doc.page(0).text, "Café notes, naïve ");
    }
}
