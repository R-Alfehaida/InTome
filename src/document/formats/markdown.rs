//! Markdown documents: rendered to HTML with pulldown-cmark, then parsed to
//! structured text. Port of prism/document/formats/markdown.py.

use std::path::Path;

use pulldown_cmark::{Options, Parser, html};

use crate::document::utils::{PagedDocument, heading_toc, page_size_of, read_text_file, split_pages};
use crate::document::{Document, OpenError, OpenOptions, Page};
use crate::structured_text::html_parser::parse_html;

fn to_html(md: &str) -> String {
    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_FOOTNOTES));
    out
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let parsed = parse_html(&to_html(&read_text_file(path)?));
    let mut pages = split_pages(&parsed.text, parsed.elements, page_size_of(opts));
    if parsed.text.is_empty() {
        pages = vec![Page::plain("(Empty document)")];
    }
    let toc = heading_toc(&pages);
    Ok(Box::new(PagedDocument::new(path, "Markdown", PagedDocument::file_name(path), pages, toc)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured_text::elements::ElementType;

    #[test]
    fn headings_links_and_tables_survive_the_html_round_trip() {
        let md = "# Title\n\nSee [docs](https://x.org).\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n## Part two\n\n- one\n- two\n";
        let parsed = parse_html(&to_html(md));
        assert_eq!(parsed.text, "Title\n\nSee [docs](https://x.org).\n\na  b\n1  2\n\nPart two\n\n  * one\n  * two");
        let types: Vec<ElementType> = parsed.elements.iter().map(|e| e.element_type).collect();
        assert_eq!(
            types,
            vec![
                ElementType::Heading1,
                ElementType::Link,
                ElementType::Table,
                ElementType::Heading2,
                ElementType::List,
                ElementType::ListItem,
                ElementType::ListItem
            ]
        );
    }

    #[test]
    fn open_builds_toc_from_headings_with_page_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.md");
        std::fs::write(&path, "# One\n\nalpha beta\n\n## Two\n\ngamma delta\n").unwrap();
        let mut doc = open(&path, &OpenOptions { page_size: 20, ..Default::default() }).unwrap();
        assert_eq!(doc.page(0).text, "One\n\nalpha beta\n\n");
        let toc: Vec<(String, usize, usize)> = doc.toc().iter().map(|t| (t.title.clone(), t.page_index, t.level)).collect();
        assert_eq!(toc, vec![("One".to_string(), 0, 0), ("Two".to_string(), 1, 1)]);
    }
}
