//! FictionBook 2 (.fb2) documents. Port of prism/document/formats/fb2.py.
//!
//! The first `<body>` is flattened to text: each `<section>` title becomes a
//! heading element and a TOC entry, every other block becomes a paragraph.
//! Tags are matched by local name so any FB2 namespace URI works.

use std::io::Read;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::document::utils::{PagedDocument, page_size_of, split_pages};
use crate::document::{Document, OpenError, OpenOptions, Page, TocEntry};
use crate::structured_text::elements::{ElementMeta, ElementType, SemanticElement};

enum Child {
    Text(String),
    Elem(Elem),
}

#[derive(Default)]
struct Elem {
    name: String,
    children: Vec<Child>,
}

impl Elem {
    fn elems<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Elem> {
        self.children.iter().filter_map(move |c| match c {
            Child::Elem(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    fn child_elems(&self) -> impl Iterator<Item = &Elem> {
        self.children.iter().filter_map(|c| if let Child::Elem(e) = c { Some(e) } else { None })
    }

    /// Text before the first child element (ElementTree's `.text`).
    fn own_text(&self) -> &str {
        match self.children.first() {
            Some(Child::Text(t)) => t,
            _ => "",
        }
    }

    /// Every descendant text piece, stripped and joined by single spaces.
    fn gather_text(&self) -> String {
        fn pieces<'a>(e: &'a Elem, out: &mut Vec<&'a str>) {
            for c in &e.children {
                match c {
                    Child::Text(t) => out.push(t.trim()),
                    Child::Elem(e) => pieces(e, out),
                }
            }
        }
        let mut v = Vec::new();
        pieces(self, &mut v);
        v.retain(|s| !s.is_empty());
        v.join(" ")
    }
}

fn push_text(stack: &mut [Elem], s: &str) {
    let Some(top) = stack.last_mut() else { return };
    match top.children.last_mut() {
        Some(Child::Text(t)) => t.push_str(s),
        _ => top.children.push(Child::Text(s.to_string())),
    }
}

fn parse_xml(xml: &str) -> Result<Elem, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut stack: Vec<Elem> = Vec::new();
    let mut root = None;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => stack.push(Elem { name: e.local_name().as_ref().to_string(), children: Vec::new() }),
            Event::End(_) => {
                let done = stack.pop().ok_or("unbalanced end tag")?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(Child::Elem(done)),
                    None => root = Some(done),
                }
            }
            Event::Text(t) => push_text(&mut stack, &t),
            Event::CData(c) => push_text(&mut stack, &c),
            Event::GeneralRef(r) => {
                let name = r.to_string();
                let resolved = match r.resolve_char_ref().map_err(|e| e.to_string())? {
                    Some(ch) => ch.to_string(),
                    None => match name.as_str() {
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "amp" => "&".into(),
                        "apos" => "'".into(),
                        "quot" => "\"".into(),
                        _ => format!("&{name};"),
                    },
                };
                push_text(&mut stack, &resolved);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("unclosed tag".into());
    }
    root.ok_or_else(|| "no root element".to_string())
}

/// Decodes XML bytes using the encoding the prolog declares (UTF-8 if none).
fn decode_xml(bytes: &[u8]) -> String {
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc.decode(bytes).0.into_owned();
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).into_owned();
    let declared = head
        .find("encoding=")
        .map(|i| &head[i + 9..])
        .and_then(|rest| {
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            rest[1..].split(quote).next().map(str::to_string)
        })
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    declared.decode_without_bom_handling(bytes).0.into_owned()
}

/// A few .fb2 files are really zip archives holding the XML.
fn read_fb2(path: &Path) -> Result<Vec<u8>, OpenError> {
    let data = std::fs::read(path)?;
    if !data.starts_with(b"PK") {
        return Ok(data);
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| OpenError::Other(e.to_string()))?;
    let name = zip
        .file_names()
        .find(|n| n.to_lowercase().ends_with(".fb2"))
        .map(str::to_string)
        .ok_or_else(|| OpenError::Other("Zipped FB2 contains no .fb2 entry".into()))?;
    let mut xml = Vec::new();
    zip.by_name(&name).map_err(|e| OpenError::Other(e.to_string()))?.read_to_end(&mut xml)?;
    Ok(xml)
}

struct Heading {
    level: usize,
    title: String,
    offset: usize,
}

struct Walker {
    text: String,
    len: usize,
    headings: Vec<Heading>,
}

impl Walker {
    fn emit(&mut self, s: &str) {
        self.text.push_str(s);
        self.len += s.chars().count();
    }

    fn paragraph(&mut self, e: &Elem) {
        let para = e.gather_text();
        if !para.is_empty() {
            self.emit(&para);
            self.emit("\n\n");
        }
    }

    fn section(&mut self, section: &Elem, level: usize) {
        let title = section.elems("title").next().map(|t| t.gather_text().trim().to_string()).unwrap_or_default();
        if !title.is_empty() {
            self.headings.push(Heading { level: level.min(5), title: title.clone(), offset: self.len });
            self.emit(&title);
            self.emit("\n\n");
        }
        for child in section.child_elems() {
            match child.name.as_str() {
                "section" => self.section(child, level + 1),
                "title" => {}
                _ => self.paragraph(child),
            }
        }
    }
}

/// (title, author) from `description/title-info`. The last author listed wins.
fn metadata(root: &Elem) -> (Option<String>, String) {
    let (mut title, mut author) = (None, String::new());
    for ti in root.elems("description").flat_map(|d| d.elems("title-info")) {
        for node in ti.child_elems() {
            match node.name.as_str() {
                "book-title" => {
                    let t = node.own_text().trim();
                    if !t.is_empty() {
                        title = Some(t.to_string());
                    }
                }
                "author" => {
                    let part = |n: &str| node.elems(n).last().map(|e| e.own_text().trim().to_string()).unwrap_or_default();
                    author = [part("first-name"), part("last-name")].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
                }
                _ => {}
            }
        }
    }
    (title, author)
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let root = parse_xml(&decode_xml(&read_fb2(path)?)).map_err(|e| OpenError::Other(format!("Invalid FB2 file: {e}")))?;
    let (title, author) = metadata(&root);

    let mut walker = Walker { text: String::new(), len: 0, headings: Vec::new() };
    if let Some(body) = root.elems("body").next() {
        for child in body.child_elems() {
            if child.name == "section" {
                walker.section(child, 0);
            } else {
                walker.paragraph(child);
            }
        }
    }
    let full_text = walker.text.trim().to_string();
    let (pages, toc) = if full_text.is_empty() {
        (vec![Page::plain("(Empty book)")], Vec::new())
    } else {
        // Offsets are exact (not a text search for the title), and every page keeps its headings.
        let elements = walker
            .headings
            .iter()
            .map(|h| SemanticElement {
                element_type: ElementType::heading(h.level as u8 + 1),
                start_pos: h.offset,
                end_pos: h.offset + h.title.chars().count(),
                text: h.title.clone(),
                meta: ElementMeta { level: Some(h.level as u8 + 1), ..Default::default() },
            })
            .collect();
        let pages = split_pages(&full_text, elements, page_size_of(opts));
        let page_of_heading = pages.iter().enumerate().flat_map(|(i, p)| p.elements.iter().map(move |_| i));
        let toc = walker
            .headings
            .iter()
            .zip(page_of_heading)
            .map(|(h, page_index)| TocEntry { title: h.title.clone(), page_index, level: h.level, children: Vec::new() })
            .collect();
        (pages, toc)
    };
    let title = title.unwrap_or_else(|| PagedDocument::file_name(path));
    Ok(Box::new(PagedDocument::new(path, "FictionBook", title, pages, toc).with_author(author)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOK: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
<description><title-info><author><first-name>Ada</first-name><last-name>Reader</last-name></author>
<book-title> The Quiet Book </book-title></title-info></description>
<body>
<title><p>Cover</p></title>
<section><title><p>Chapter One</p></title>
<p>It was <emphasis>very</emphasis> quiet &amp; calm.</p>
<section><title><p>A Part</p></title><p>Deep.</p></section>
</section>
<section><title><p>Chapter Two</p></title><p>End.</p></section>
</body></FictionBook>"#;

    #[test]
    fn sections_become_headings_toc_entries_and_paragraphs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.fb2");
        std::fs::write(&path, BOOK).unwrap();
        let mut doc = open(&path, &OpenOptions::default()).unwrap();
        assert_eq!(doc.page(0).text, "Cover\n\nChapter One\n\nIt was very quiet & calm.\n\nA Part\n\nDeep.\n\nChapter Two\n\nEnd.");
        let toc: Vec<(String, usize, usize)> = doc.toc().iter().map(|t| (t.title.clone(), t.page_index, t.level)).collect();
        assert_eq!(toc, vec![("Chapter One".into(), 0, 0), ("A Part".into(), 0, 1), ("Chapter Two".into(), 0, 0)]);
        let h: Vec<(ElementType, usize, usize)> = doc.page(0).elements.iter().map(|e| (e.element_type, e.start_pos, e.end_pos)).collect();
        assert_eq!(h, vec![(ElementType::Heading1, 7, 18), (ElementType::Heading2, 47, 53), (ElementType::Heading1, 62, 73)]);
        let md = doc.metadata();
        assert_eq!((md.title.as_str(), md.author.as_str(), md.format.as_str()), ("The Quiet Book", "Ada Reader", "FictionBook"));
    }

    #[test]
    fn headings_map_to_pages_and_declared_encoding_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.fb2");
        let xml = "<?xml version=\"1.0\" encoding=\"windows-1251\"?><FictionBook><body><section><title><p>A</p></title><p>text one</p></section><section><title><p>\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}</p></title><p>text two</p></section></body></FictionBook>";
        let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(xml);
        std::fs::write(&path, bytes).unwrap();
        let doc = open(&path, &OpenOptions { page_size: 20, ..Default::default() }).unwrap();
        assert_eq!(doc.page_count(), 2);
        assert_eq!(doc.toc().iter().map(|t| (t.title.as_str(), t.page_index)).collect::<Vec<_>>(), vec![("A", 0), ("Привет", 1)]);
        assert_eq!(doc.metadata().title, "c.fb2");
    }

    #[test]
    fn malformed_xml_fails_to_open_with_a_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.fb2");
        std::fs::write(&path, "<FictionBook><body><p>oops</body></FictionBook>").unwrap();
        assert!(matches!(open(&path, &OpenOptions::default()), Err(OpenError::Other(m)) if m.starts_with("Invalid FB2 file")));
    }
}
