//! HTML documents (.html, .htm, .xhtml). Port of prism/document/formats/html.py.
//!
//! Prism ran trafilatura and used its plain text, which left a cleaned page
//! with no semantic elements. Here dom_smoothie (a Readability port) picks the
//! article body and `parse_html` renders it, so headings, links, tables and
//! the rest stay navigable. When extraction finds nothing the whole page is parsed.

use std::path::Path;

use dom_smoothie::Readability;
use scraper::{Html, Selector};

use crate::document::utils::{PagedDocument, heading_toc, page_size_of, read_text_file, split_pages};
use crate::document::{Document, OpenError, OpenOptions, Page};
use crate::structured_text::html_parser::{ParseResult, parse_html};

/// Article text and elements, falling back to the whole page.
fn extract(raw: &str) -> ParseResult {
    let article = Readability::new(raw, None, None).and_then(|mut r| r.parse()).ok();
    article
        .map(|a| parse_html(&a.content))
        .filter(|p| !p.text.trim().is_empty())
        .unwrap_or_else(|| parse_html(raw))
}

fn title_of(raw: &str) -> Option<String> {
    let doc = Html::parse_document(raw);
    let sel = Selector::parse("title").ok()?;
    let title = doc.select(&sel).next()?.text().collect::<String>();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_string())
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let raw = read_text_file(path)?;
    let parsed = extract(&raw);
    let mut pages = split_pages(&parsed.text, parsed.elements, page_size_of(opts));
    if parsed.text.is_empty() {
        pages = vec![Page::plain("(Empty document)")];
    }
    let toc = heading_toc(&pages);
    let title = title_of(&raw).unwrap_or_else(|| PagedDocument::file_name(path));
    Ok(Box::new(PagedDocument::new(path, "HTML", title, pages, toc)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_comes_from_the_title_tag() {
        assert_eq!(title_of("<html><head><title>  My Page \n</title></head><body>x</body></html>").as_deref(), Some("My Page"));
        assert_eq!(title_of("<p>no title</p>"), None);
    }

    #[test]
    fn open_keeps_article_text_headings_and_toc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.html");
        let para = "Libraries are quiet places where people read, study, and borrow books for weeks at a time. ".repeat(8);
        std::fs::write(
            &path,
            format!(
                "<html><head><title>T</title></head><body><nav><a href=\"/x\">menu</a></nav><article><h2>Section</h2><p>{para}</p></article></body></html>"
            ),
        )
        .unwrap();
        let mut doc = open(&path, &OpenOptions::default()).unwrap();
        assert_eq!(doc.metadata().title, "T");
        assert!(doc.page(0).text.contains("Libraries are quiet places"));
        assert!(!doc.page(0).text.contains("menu"));
        assert_eq!(doc.toc().iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), vec!["Section"]);
    }
}
