//! End-to-end exporter tests over a small structured document. Outputs also land in
//! `target/export-validation/` so external checkers (python-docx, PyMuPDF, ebooklib, lxml)
//! can inspect the real files.

use std::path::{Path, PathBuf};

use crate::cancel::CancelToken;
use crate::document::{Capabilities, DocMetadata, Document, Page, TocEntry};
use crate::export::{ExportOptions, create_exporter};
use crate::structured_text::elements::{ElementMeta, ElementType, SemanticElement};

pub(crate) struct TestDoc {
    pages: Vec<Page>,
    toc: Vec<TocEntry>,
}

impl Document for TestDoc {
    fn path(&self) -> &Path {
        Path::new("test.doc")
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::TOC
    }
    fn toc(&self) -> &[TocEntry] {
        &self.toc
    }
    fn metadata(&self) -> DocMetadata {
        DocMetadata { title: "The Test Book".into(), author: "A. Writer".into(), format: "TEST".into(), extra: Vec::new() }
    }
    fn page_count(&self) -> usize {
        self.pages.len()
    }
    fn page(&mut self, index: usize) -> &Page {
        &self.pages[index]
    }
}

type Part = (Option<ElementType>, &'static str, Option<&'static str>);

/// Builds a page from parts joined by newlines; parts without an element type are plain text.
fn page(parts: &[Part]) -> Page {
    let mut text = String::new();
    let mut elements = Vec::new();
    for (kind, t, url) in parts {
        if !text.is_empty() {
            text.push('\n');
        }
        let start = text.chars().count();
        text.push_str(t);
        if let Some(k) = kind {
            elements.push(SemanticElement {
                element_type: *k,
                start_pos: start,
                end_pos: start + t.chars().count(),
                text: (*t).into(),
                meta: ElementMeta { level: k.heading_level(), url: url.map(String::from), alt: None },
            });
        }
    }
    Page { text, elements }
}

pub(crate) fn sample_doc() -> TestDoc {
    use ElementType::*;
    let pages = vec![
        page(&[
            (Some(Heading1), "Chapter One: Beginnings", None),
            (None, "It was a bright cold day in April, and the clocks were striking thirteen. Winston Smith slipped quickly through the glass doors of Victory Mansions, though not quickly enough to prevent a swirl of gritty dust from entering along with him.", None),
            (Some(Heading2), "A Section & Its \"Quotes\"", None),
            (Some(List), "• first item\n• second item\n• third item", None),
            (Some(Link), "Visit the example site", Some("https://example.com/")),
        ]),
        page(&[
            (Some(Heading1), "Chapter Two: Middles", None),
            (Some(Quote), "To be, or not to be, that is the question.", None),
            (Some(CodeBlock), "fn main() {\n    println!(\"hi\");\n}", None),
            (None, "Unicode check: café, naïve, 日本語, emoji 🎉 and a tab\there.", None),
        ]),
        page(&[
            (Some(Heading1), "Chapter Three: Endings", None),
            (Some(Heading3), "Skipped a level", None),
            (None, "The end. Thanks for reading.", None),
        ]),
    ];
    let toc = vec![
        TocEntry {
            title: "Chapter One: Beginnings".into(),
            page_index: 0,
            level: 0,
            children: vec![TocEntry { title: "A Section".into(), page_index: 0, level: 1, children: vec![] }],
        },
        TocEntry { title: "Chapter Two: Middles".into(), page_index: 1, level: 0, children: vec![] },
        TocEntry { title: "Chapter Three: Endings".into(), page_index: 2, level: 0, children: vec![] },
    ];
    TestDoc { pages, toc }
}

fn out_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("export-validation");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(key: &str, file: &str) -> PathBuf {
    let mut doc = sample_doc();
    let opts = ExportOptions::new(out_dir().join(file));
    let mut steps = Vec::new();
    let path = create_exporter(key)
        .unwrap()
        .export(&mut doc, &opts, &CancelToken::new(), &mut |c, t, _| steps.push((c, t)))
        .unwrap();
    assert_eq!(steps, vec![(1, 3), (2, 3), (3, 3)], "progress for {key}");
    path
}

#[test]
fn text_export() {
    let text = std::fs::read_to_string(run("text", "sample.txt")).unwrap();
    assert!(text.starts_with("=============\nThe Test Book\n=============\nby A. Writer\n"));
    assert!(text.contains("\n# Chapter One: Beginnings"));
    assert!(text.contains("\n## A Section & Its \"Quotes\""));
    assert!(text.contains("--- Page 2 of 3 ---"));
}

#[test]
fn html_export_has_landmarks_and_escapes() {
    let html = std::fs::read_to_string(run("html", "sample.html")).unwrap();
    for needle in [
        "<header role=\"banner\">",
        "<nav role=\"navigation\" aria-label=\"Table of Contents\">",
        "<main role=\"main\">",
        "<section id=\"page-2\" aria-label=\"Page 2\">",
        "<h1>Chapter One: Beginnings</h1>",
        "<h2>A Section &amp; Its &quot;Quotes&quot;</h2>",
        "  <li>second item</li>",
        "<p><a href=\"https://example.com/\">Visit the example site</a></p>",
        "<blockquote>To be, or not to be, that is the question.</blockquote>",
        "<li><a href=\"#page-3\">Chapter Three: Endings</a></li>",
    ] {
        assert!(html.contains(needle), "missing {needle}");
    }
}

#[test]
fn markdown_export() {
    let md = std::fs::read_to_string(run("markdown", "sample.md")).unwrap();
    assert!(md.starts_with("# The Test Book\n\n*by A. Writer*\n"));
    assert!(md.contains("## Table of Contents\n\n- Chapter One: Beginnings\n  - A Section\n- Chapter Two: Middles"));
    assert!(md.contains("# Chapter One: Beginnings\n"));
    assert!(md.contains("- first item\n- second item"));
    assert!(md.contains("[Visit the example site](https://example.com/)"));
    assert!(md.contains("> To be, or not to be"));
    assert!(md.contains("```\nfn main() {"));
}

#[test]
fn binary_exporters_write_files() {
    for (key, file) in [("docx", "sample.docx"), ("epub", "sample.epub"), ("pdf", "sample.pdf")] {
        let path = run(key, file);
        assert!(std::fs::metadata(&path).unwrap().len() > 1000, "{key} output too small");
    }
    let xml = run("daisy_text", "daisy");
    assert!(xml.ends_with("dtbook.xml"));
    assert!(xml.parent().unwrap().join("ncc.html").is_file());
}

#[test]
fn cancel_writes_nothing() {
    let mut doc = sample_doc();
    let out = out_dir().join("cancelled.txt");
    let _ = std::fs::remove_file(&out);
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = create_exporter("text").unwrap().export(&mut doc, &ExportOptions::new(&out), &cancel, &mut |_, _, _| {}).unwrap_err();
    assert!(err.downcast_ref::<crate::export::Cancelled>().is_some());
    assert!(!out.exists());
}
