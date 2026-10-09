//! LaTeX, DocBook and Jupyter notebooks through the external pandoc binary.
//! Port of prism/document/formats/pandoc.py. RTF and FB2 have their own handlers.

use std::path::Path;

use anyhow::Context;

use crate::document::utils::{PagedDocument, check_external_tool, heading_toc, page_size_of, run_external_tool, split_pages};
use crate::document::{Document, OpenError, OpenOptions, Page};
use crate::structured_text::html_parser::parse_html;

pub fn available() -> bool {
    check_external_tool("pandoc")
}

fn format_name(path: &Path) -> &'static str {
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("tex" | "latex") => "LaTeX",
        Some("docbook") => "DocBook",
        Some("ipynb") => "Jupyter Notebook",
        _ => "Document",
    }
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    if !available() {
        return Err(OpenError::Unavailable("Pandoc".into()));
    }
    let path_str = path.to_str().context("path is not valid Unicode")?;
    let html = run_external_tool("pandoc", &["--to", "html", path_str], 60)?;
    let parsed = parse_html(&String::from_utf8_lossy(&html));
    let mut pages = split_pages(&parsed.text, parsed.elements, page_size_of(opts));
    if parsed.text.is_empty() {
        pages = vec![Page::plain("(Empty document)")];
    }
    let toc = heading_toc(&pages);
    Ok(Box::new(PagedDocument::new(path, format_name(path), PagedDocument::file_name(path), pages, toc)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_names_follow_the_extension() {
        assert_eq!(format_name(Path::new("a.TEX")), "LaTeX");
        assert_eq!(format_name(Path::new("a.docbook")), "DocBook");
        assert_eq!(format_name(Path::new("a.ipynb")), "Jupyter Notebook");
        assert_eq!(format_name(Path::new("a.xyz")), "Document");
    }

    #[test]
    fn open_without_pandoc_reports_unavailable_instead_of_failing() {
        if available() {
            return;
        }
        assert!(matches!(open(Path::new("x.tex"), &OpenOptions::default()), Err(OpenError::Unavailable(_))));
    }
}
