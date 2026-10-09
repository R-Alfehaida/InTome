//! Word documents. Port of prism/document/formats/word.py.
//!
//! `.docx` is converted to HTML the way mammoth does (same default style map,
//! list nesting, run formatting, tables, notes), then handed to `parse_html`.
//! Legacy `.doc` goes through antiword (and pandoc when present), like Prism.
//!
//! This file also holds the pieces the other zip/XML formats share:
//! [`PagedDoc`], [`parse_xml`] and the pagination/TOC helpers.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, bail};
use quick_xml::{Reader, XmlVersion};
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use regex::Regex;

use crate::document::utils::{check_external_tool, paginate_text, run_external_tool};
use crate::document::{Capabilities, DocMetadata, Document, OpenError, OpenOptions, Page, TocEntry};
use crate::structured_text::elements::SemanticElement;
use crate::structured_text::html_parser::parse_html;

// ---------------------------------------------------------------------------
// Shared by the zip/XML formats (docx, doc, odf, pptx, epub, mobi)
// ---------------------------------------------------------------------------

/// A fully extracted, paginated document. Capabilities follow Prism: searchable
/// text, plus a TOC when there are entries.
pub(crate) struct PagedDoc {
    path: PathBuf,
    pages: Vec<Page>,
    toc: Vec<TocEntry>,
    meta: DocMetadata,
}

impl PagedDoc {
    pub(crate) fn new(path: &Path, mut pages: Vec<Page>, toc: Vec<TocEntry>, meta: DocMetadata) -> Self {
        if pages.is_empty() {
            pages.push(Page::plain("(Empty document)"));
        }
        Self { path: path.to_path_buf(), pages, toc, meta }
    }
}

impl Document for PagedDoc {
    fn path(&self) -> &Path {
        &self.path
    }
    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::SEARCHABLE_TEXT;
        if !self.toc.is_empty() {
            caps |= Capabilities::TOC;
        }
        caps
    }
    fn toc(&self) -> &[TocEntry] {
        &self.toc
    }
    fn metadata(&self) -> DocMetadata {
        self.meta.clone()
    }
    fn page_count(&self) -> usize {
        self.pages.len()
    }
    fn page(&mut self, index: usize) -> &Page {
        &self.pages[index]
    }
}

/// `settings.page_size`; `OpenOptions::default()` carries 0, so fall back to the configured value.
pub(crate) fn effective_page_size(opts: &OpenOptions) -> usize {
    if opts.page_size > 0 { opts.page_size } else { crate::config::get().general.page_size.max(1) }
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Short text fits one page and keeps its elements. Longer text is split and,
/// as in Prism, the split pages carry no elements.
pub(crate) fn paginate_with_elements(text: String, elements: Vec<SemanticElement>, page_size: usize) -> Vec<Page> {
    if text.chars().count() <= page_size {
        vec![Page { text, elements }]
    } else {
        paginate_text(&text, page_size).into_iter().map(Page::plain).collect()
    }
}

/// Prism builds the TOC from heading elements: title is the first 80 chars,
/// level is heading level - 1, and every entry points at page 0.
pub(crate) fn toc_from_headings(elements: &[SemanticElement]) -> Vec<TocEntry> {
    elements
        .iter()
        .filter_map(|e| {
            e.meta.level.map(|level| TocEntry {
                title: e.text.chars().take(80).collect(),
                page_index: 0,
                level: (level as usize).saturating_sub(1),
                children: Vec::new(),
            })
        })
        .collect()
}

/// A parsed XML element. Names keep the prefix used in the file (`w:p`).
/// shortcut: prefixes are not resolved to namespace URIs; Word, LibreOffice and
/// python-docx/pptx all use the conventional prefixes, upgrade if a producer does not.
#[derive(Debug, Clone, Default)]
pub(crate) struct Xml {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<XNode>,
}

#[derive(Debug, Clone)]
pub(crate) enum XNode {
    El(Xml),
    Text(String),
}

impl Xml {
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub(crate) fn child<'a>(&'a self, name: &str) -> Option<&'a Xml> {
        self.kids.iter().find_map(|k| match k {
            XNode::El(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    pub(crate) fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Xml> {
        self.kids.iter().filter_map(move |k| match k {
            XNode::El(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    /// All text below this element, concatenated.
    pub(crate) fn inner_text(&self) -> String {
        let mut out = String::new();
        self.push_text(&mut out);
        out
    }

    fn push_text(&self, out: &mut String) {
        for k in &self.kids {
            match k {
                XNode::Text(t) => out.push_str(t),
                XNode::El(e) => e.push_text(out),
            }
        }
    }
}

fn open_element(e: &BytesStart) -> Xml {
    let attrs = e
        .attributes()
        .flatten()
        .map(|a| {
            (
                a.key.as_ref().to_string(),
                a.normalized_value(XmlVersion::Implicit1_0).map(|v| v.into_owned()).unwrap_or_default(),
            )
        })
        .collect();
    Xml { name: e.name().as_ref().to_string(), attrs, kids: Vec::new() }
}

fn push_text(stack: &mut [Xml], text: &str) {
    let Some(top) = stack.last_mut() else { return };
    if let Some(XNode::Text(last)) = top.kids.last_mut() {
        last.push_str(text);
    } else {
        top.kids.push(XNode::Text(text.to_string()));
    }
}

/// Parses an XML part into a tree. `None` when the data is not well-formed.
pub(crate) fn parse_xml(data: &[u8]) -> Option<Xml> {
    let text = String::from_utf8_lossy(data);
    let mut reader = Reader::from_str(&text);
    reader.config_mut().expand_empty_elements = true;
    let mut stack = vec![Xml::default()];
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => stack.push(open_element(&e)),
            Event::End(_) => {
                let done = stack.pop()?;
                stack.last_mut()?.kids.push(XNode::El(done));
            }
            Event::Text(t) => push_text(&mut stack, &t.xml10_content()),
            Event::CData(t) => push_text(&mut stack, &t.xml10_content()),
            Event::GeneralRef(r) => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    push_text(&mut stack, c.encode_utf8(&mut [0; 4]));
                } else if let Some(s) = resolve_predefined_entity(&r.xml10_content()) {
                    push_text(&mut stack, s);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return None;
    }
    stack.pop()?.kids.into_iter().find_map(|k| match k {
        XNode::El(e) => Some(e),
        XNode::Text(_) => None,
    })
}

pub(crate) fn read_entry<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let mut entry = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

pub(crate) fn read_xml<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Xml> {
    parse_xml(&read_entry(zip, name)?)
}

/// Joins a zip-internal path and a relationship target, resolving `.`/`..`.
pub(crate) fn join_zip_path(base: &str, target: &str) -> String {
    let full = if target.starts_with('/') { target.to_string() } else { format!("{base}/{target}") };
    let mut parts: Vec<&str> = Vec::new();
    for seg in full.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

pub(crate) fn zip_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// `word/document.xml` -> `word/_rels/document.xml.rels`.
pub(crate) fn rels_path_for(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/_rels/{name}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Rel {
    pub id: String,
    pub target: String,
    pub ty: String,
}

pub(crate) fn read_rels<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Vec<Rel> {
    let Some(root) = read_xml(zip, name) else { return Vec::new() };
    root.children("Relationship")
        .filter_map(|r| {
            Some(Rel { id: r.attr("Id")?.to_string(), target: r.attr("Target")?.to_string(), ty: r.attr("Type")?.to_string() })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// HTML node tree (the part of mammoth's html module the conversion needs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum H {
    Text(String),
    El(HEl),
    /// Keeps an otherwise empty element (tables, rows, cells, bookmarks) from being stripped.
    Force,
}

#[derive(Debug, Clone)]
struct HEl {
    /// `names[0]` is the tag; the rest are the alternatives this element may merge into.
    names: Vec<&'static str>,
    attrs: Vec<(String, String)>,
    collapsible: bool,
    kids: Vec<H>,
}

impl H {
    fn build(names: &[&'static str], attrs: &[(&str, &str)], collapsible: bool, kids: Vec<H>) -> H {
        H::El(HEl {
            names: names.to_vec(),
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            collapsible,
            kids,
        })
    }
    fn el(name: &'static str, attrs: &[(&str, &str)], kids: Vec<H>) -> H {
        Self::build(&[name], attrs, false, kids)
    }
    fn cel(name: &'static str, attrs: &[(&str, &str)], kids: Vec<H>) -> H {
        Self::build(&[name], attrs, true, kids)
    }
}

fn is_void(tag: &str, no_kids: bool) -> bool {
    no_kids && matches!(tag, "br" | "hr" | "img" | "input")
}

fn strip_empty(nodes: Vec<H>) -> Vec<H> {
    nodes
        .into_iter()
        .filter_map(|n| match n {
            H::Text(t) => (!t.is_empty()).then_some(H::Text(t)),
            H::Force => Some(H::Force),
            H::El(mut e) => {
                let void = is_void(e.names[0], e.kids.is_empty());
                e.kids = strip_empty(std::mem::take(&mut e.kids));
                (void || !e.kids.is_empty()).then_some(H::El(e))
            }
        })
        .collect()
}

/// Merges a collapsible element into the preceding element when the preceding
/// tag is among its alternatives and the attributes are equal. This is what
/// turns consecutive list-item paragraphs into one `<ul>`.
fn collapse(nodes: Vec<H>) -> Vec<H> {
    let mut out = Vec::new();
    for n in nodes {
        add_collapsing(&mut out, n);
    }
    out
}

fn add_collapsing(out: &mut Vec<H>, node: H) {
    let node = match node {
        H::El(mut e) => {
            e.kids = collapse(std::mem::take(&mut e.kids));
            H::El(e)
        }
        other => other,
    };
    let merges = match (&node, out.last()) {
        (H::El(new), Some(H::El(last))) => new.collapsible && new.names.contains(&last.names[0]) && last.attrs == new.attrs,
        _ => false,
    };
    if merges {
        if let (H::El(new), Some(H::El(last))) = (node, out.last_mut()) {
            for child in new.kids {
                add_collapsing(&mut last.kids, child);
            }
        }
    } else {
        out.push(node);
    }
}

fn esc(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

fn write_html(nodes: &[H], out: &mut String) {
    for n in nodes {
        match n {
            H::Text(t) => esc(t, out),
            H::Force => {}
            H::El(e) => {
                let tag = e.names[0];
                let _ = write!(out, "<{tag}");
                for (k, v) in &e.attrs {
                    let _ = write!(out, " {k}=\"");
                    esc(v, out);
                    out.push('"');
                }
                if is_void(tag, e.kids.is_empty()) {
                    out.push_str(" />");
                } else {
                    out.push('>');
                    write_html(&e.kids, out);
                    let _ = write!(out, "</{tag}>");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// DOCX: styles, numbering, relationships
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Styles {
    paragraph: HashMap<String, Option<String>>,
    character: HashMap<String, Option<String>>,
    /// numbering style id -> numId
    numbering: HashMap<String, Option<String>>,
}

fn val<'a>(e: Option<&'a Xml>) -> Option<&'a str> {
    e.and_then(|e| e.attr("w:val"))
}

fn read_styles(root: &Xml) -> Styles {
    let mut s = Styles::default();
    for style in root.children("w:style") {
        let (Some(ty), Some(id)) = (style.attr("w:type"), style.attr("w:styleId")) else { continue };
        let name = val(style.child("w:name")).map(str::to_string);
        // The first style with a given id wins (ECMA-376 17.7.4.17).
        match ty {
            "numbering" => {
                let num_id = val(style.child("w:pPr").and_then(|p| p.child("w:numPr")).and_then(|n| n.child("w:numId"))).map(str::to_string);
                s.numbering.entry(id.to_string()).or_insert(num_id);
            }
            "paragraph" => {
                s.paragraph.entry(id.to_string()).or_insert(name);
            }
            "character" => {
                s.character.entry(id.to_string()).or_insert(name);
            }
            _ => {}
        }
    }
    s
}

#[derive(Clone, Debug, PartialEq)]
struct Lvl {
    index: String,
    ordered: bool,
}

struct AbstractNum {
    levels: HashMap<String, Lvl>,
    style_link: Option<String>,
}

#[derive(Default)]
struct Numbering {
    abstracts: HashMap<String, AbstractNum>,
    nums: HashMap<String, String>,
    by_paragraph_style: HashMap<String, Lvl>,
}

fn read_numbering(root: &Xml) -> Numbering {
    let mut n = Numbering::default();
    for an in root.children("w:abstractNum") {
        let Some(id) = an.attr("w:abstractNumId") else { continue };
        let mut levels: HashMap<String, Lvl> = HashMap::new();
        let mut without_index: Option<Lvl> = None;
        let mut pstyles: Vec<(String, Lvl)> = Vec::new();
        for l in an.children("w:lvl") {
            let ordered = val(l.child("w:numFmt")) != Some("bullet");
            let pstyle = val(l.child("w:pStyle")).map(str::to_string);
            let index = l.attr("w:ilvl").map(str::to_string);
            // Malformed documents omit the index; Word treats that as level 0.
            let lvl = Lvl { index: index.clone().unwrap_or_else(|| "0".into()), ordered };
            if index.is_some() {
                levels.insert(lvl.index.clone(), lvl.clone());
            } else {
                without_index = Some(lvl.clone());
            }
            if let Some(p) = pstyle {
                pstyles.push((p, lvl));
            }
        }
        if let Some(l) = without_index {
            levels.entry(l.index.clone()).or_insert(l);
        }
        n.by_paragraph_style.extend(pstyles);
        let style_link = val(an.child("w:numStyleLink")).map(str::to_string);
        n.abstracts.insert(id.to_string(), AbstractNum { levels, style_link });
    }
    for num in root.children("w:num") {
        if let (Some(id), Some(a)) = (num.attr("w:numId"), val(num.child("w:abstractNumId"))) {
            n.nums.insert(id.to_string(), a.to_string());
        }
    }
    n
}

impl Numbering {
    fn find_level(&self, num_id: &str, level: &str, styles: &Styles, depth: u8) -> Option<Lvl> {
        let abstract_num = self.abstracts.get(self.nums.get(num_id)?)?;
        match &abstract_num.style_link {
            None => abstract_num.levels.get(level).cloned(),
            Some(link) if depth < 8 => {
                let linked = styles.numbering.get(link)?.as_deref()?;
                self.find_level(linked, level, styles, depth + 1)
            }
            Some(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// DOCX body -> HTML
// ---------------------------------------------------------------------------

/// Mammoth's default style map: `p:unordered-list(n)` / `p:ordered-list(n)` for n in 1..=5.
fn list_path(level: usize, ordered: bool) -> Vec<HEl> {
    let mut path = Vec::new();
    for _ in 0..level {
        path.push(path_el(&["ul", "ol"], false));
        path.push(path_el(&["li"], false));
    }
    path.push(path_el(if ordered { &["ol"] } else { &["ul"] }, false));
    path.push(path_el(&["li"], true));
    path
}

fn path_el(names: &[&'static str], fresh: bool) -> HEl {
    HEl { names: names.to_vec(), attrs: Vec::new(), collapsible: !fresh, kids: Vec::new() }
}

/// First match wins, in the order of mammoth's default style map: heading ids,
/// heading names, the "p:fresh" aliases, then numbered paragraphs.
fn paragraph_path(style_id: Option<&str>, style_name: Option<&str>, numbering: Option<&Lvl>) -> Vec<HEl> {
    const HEADINGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
    for (i, tag) in HEADINGS.iter().enumerate() {
        if style_id == Some(format!("Heading{}", i + 1).as_str()) {
            return vec![path_el(&[tag], true)];
        }
    }
    for (i, tag) in HEADINGS.iter().enumerate() {
        let n = i + 1;
        if style_name == Some(format!("Heading {n}").as_str()) || style_name == Some(format!("heading {n}").as_str()) {
            return vec![path_el(&[tag], true)];
        }
    }
    if style_id == Some("Heading") || style_name == Some("Heading") {
        return vec![path_el(&["h1"], true)];
    }
    const PLAIN: [&str; 5] = ["footnote text", "endnote text", "annotation text", "Footnote", "Endnote"];
    if style_name.is_some_and(|n| PLAIN.contains(&n)) {
        return vec![path_el(&["p"], true)];
    }
    if let Some(l) = numbering
        && let Ok(idx) = l.index.parse::<usize>()
        && idx < 5
    {
        return list_path(idx, l.ordered);
    }
    vec![path_el(&["p"], true)]
}

fn wrap(path: &[HEl], mut nodes: Vec<H>) -> Vec<H> {
    for el in path.iter().rev() {
        nodes = vec![H::El(HEl { kids: nodes, ..el.clone() })];
    }
    nodes
}

static HYPERLINK_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*HYPERLINK\s+(\\l\s+)?(?:"(.*)"|([^\\]\S*))"#).expect("valid regex"));

enum Field {
    Begin,
    Link { href: Option<String>, anchor: Option<String> },
    Other,
}

fn parse_instr_text(instr: &str) -> Field {
    let Some(c) = HYPERLINK_FIELD.captures(instr) else { return Field::Other };
    let location = c.get(2).or_else(|| c.get(3)).map(|m| m.as_str().to_string());
    if c.get(1).is_some() {
        Field::Link { href: None, anchor: location }
    } else {
        Field::Link { href: location, anchor: None }
    }
}

fn replace_fragment(uri: &str, fragment: &str) -> String {
    format!("{}#{fragment}", uri.split('#').next().unwrap_or(uri))
}

fn is_off(v: Option<&str>) -> bool {
    matches!(v, Some("false" | "0"))
}

fn flag(props: Option<&Xml>, name: &str) -> bool {
    props.and_then(|p| p.child(name)).is_some_and(|e| !is_off(e.attr("w:val")))
}

struct Note {
    body: Vec<XNode>,
    rels: HashMap<String, String>,
}

struct Conv {
    styles: Styles,
    numbering: Numbering,
    rels: HashMap<String, String>,
    notes: HashMap<(String, String), Note>,
    note_refs: Vec<(String, String)>,
    fields: Vec<Field>,
    instr: Vec<String>,
    /// Content of paragraphs marked deleted, merged into the next paragraph.
    deleted: Vec<XNode>,
    /// Text-box content found inside a paragraph, emitted after it.
    extra: Vec<Vec<H>>,
}

impl Conv {
    fn convert(&mut self, nodes: &[XNode]) -> Vec<H> {
        let mut out = Vec::new();
        for n in nodes {
            if let XNode::El(e) = n {
                out.extend(self.read(e));
            }
        }
        out
    }

    fn read(&mut self, e: &Xml) -> Vec<H> {
        match e.name.as_str() {
            "w:t" => vec![H::Text(e.inner_text())],
            "w:r" => self.run(e),
            "w:p" => self.paragraph(e),
            "w:fldChar" => {
                self.fld_char(e);
                Vec::new()
            }
            "w:instrText" => {
                self.instr.push(e.inner_text());
                Vec::new()
            }
            "w:tab" => vec![H::Text("\t".into())],
            "w:noBreakHyphen" => vec![H::Text("\u{2011}".into())],
            "w:softHyphen" => vec![H::Text("\u{ad}".into())],
            "w:tbl" => self.table(e),
            "w:ins" | "w:object" | "w:smartTag" | "w:drawing" | "v:group" | "v:rect" | "v:roundrect" | "v:shape" | "v:textbox"
            | "w:txbxContent" => self.convert(&e.kids),
            "w:pict" => {
                let content = self.convert(&e.kids);
                if let Some(extra) = self.extra.last_mut() {
                    extra.extend(content);
                }
                Vec::new()
            }
            "w:hyperlink" => self.hyperlink(e),
            "w:bookmarkStart" => match e.attr("w:name") {
                Some("_GoBack") | None => Vec::new(),
                Some(name) => vec![H::cel("a", &[("id", name)], vec![H::Force])],
            },
            "w:br" => match e.attr("w:type") {
                None | Some("") | Some("textWrapping") => vec![H::el("br", &[], Vec::new())],
                _ => Vec::new(),
            },
            "wp:inline" | "wp:anchor" => self.inline_image(e),
            "v:imagedata" => {
                if e.attr("r:id").is_some() {
                    vec![self.img(e.attr("o:title"))]
                } else {
                    Vec::new()
                }
            }
            "w:footnoteReference" => self.note_reference("footnote", e),
            "w:endnoteReference" => self.note_reference("endnote", e),
            "w:sdt" => e.child("w:sdtContent").map(|c| self.convert(&c.kids)).unwrap_or_default(),
            // Everything else (properties, proofErr, del, fldSimple, sym, comments, ...) is ignored, as in mammoth.
            _ => Vec::new(),
        }
    }

    fn fld_char(&mut self, e: &Xml) {
        match e.attr("w:fldCharType") {
            Some("begin") => {
                self.fields.push(Field::Begin);
                self.instr.clear();
            }
            Some("end") => {
                self.fields.pop();
            }
            Some("separate") => {
                self.fields.pop();
                let parsed = parse_instr_text(&self.instr.concat());
                self.fields.push(parsed);
            }
            _ => {}
        }
    }

    fn run(&mut self, e: &Xml) -> Vec<H> {
        let props = e.child("w:rPr");
        let mut nodes = self.convert(&e.kids);
        // A run inside a HYPERLINK complex field becomes part of that link.
        if let Some(Field::Link { href, anchor }) = self.fields.iter().rev().find(|f| matches!(f, Field::Link { .. })) {
            let target = match (href, anchor) {
                (Some(h), _) => Some(h.clone()),
                (None, Some(a)) => Some(format!("#{a}")),
                _ => None,
            };
            if let Some(t) = target {
                nodes = vec![H::cel("a", &[("href", &t)], nodes)];
            }
        }
        let run_style = val(props.and_then(|p| p.child("w:rStyle")))
            .and_then(|id| self.styles.character.get(id))
            .and_then(|n| n.as_deref());
        // Innermost first, in mammoth's order.
        if flag(props, "w:strike") {
            nodes = vec![H::cel("s", &[], nodes)];
        }
        match val(props.and_then(|p| p.child("w:vertAlign"))) {
            Some("subscript") => nodes = vec![H::cel("sub", &[], nodes)],
            Some("superscript") => nodes = vec![H::cel("sup", &[], nodes)],
            _ => {}
        }
        if flag(props, "w:i") {
            nodes = vec![H::cel("em", &[], nodes)];
        }
        if flag(props, "w:b") {
            nodes = vec![H::cel("strong", &[], nodes)];
        }
        if run_style == Some("Strong") {
            nodes = vec![H::cel("strong", &[], nodes)];
        }
        nodes
    }

    fn paragraph(&mut self, e: &Xml) -> Vec<H> {
        let ppr = e.child("w:pPr");
        if ppr.and_then(|p| p.child("w:rPr")).and_then(|r| r.child("w:del")).is_some() {
            self.deleted.extend(e.kids.iter().cloned());
            return Vec::new();
        }
        let mut kids_xml = std::mem::take(&mut self.deleted);
        kids_xml.extend(e.kids.iter().cloned());
        self.extra.push(Vec::new());
        let kids = self.convert(&kids_xml);
        let extra = self.extra.pop().unwrap_or_default();

        let style_id = val(ppr.and_then(|p| p.child("w:pStyle")));
        let style_name = style_id.and_then(|id| self.styles.paragraph.get(id)).and_then(|n| n.as_deref());
        let numbering = self.paragraph_numbering(style_id, ppr.and_then(|p| p.child("w:numPr")));
        let mut out = wrap(&paragraph_path(style_id, style_name, numbering.as_ref()), kids);
        out.extend(extra);
        out
    }

    fn paragraph_numbering(&self, style_id: Option<&str>, num_pr: Option<&Xml>) -> Option<Lvl> {
        let num_id = val(num_pr.and_then(|n| n.child("w:numId")));
        let level = val(num_pr.and_then(|n| n.child("w:ilvl")));
        if let (Some(n), Some(l)) = (num_id, level) {
            return self.numbering.find_level(n, l, &self.styles, 0);
        }
        if let Some(l) = style_id.and_then(|id| self.numbering.by_paragraph_style.get(id)) {
            return Some(l.clone());
        }
        // A numPr without ilvl means level 0.
        num_id.and_then(|n| self.numbering.find_level(n, "0", &self.styles, 0))
    }

    fn hyperlink(&mut self, e: &Xml) -> Vec<H> {
        let anchor = e.attr("w:anchor");
        let target_frame = e.attr("w:tgtFrame").filter(|t| !t.is_empty());
        let kids = self.convert(&e.kids);
        let href = match (e.attr("r:id"), anchor) {
            (Some(id), a) => self.rels.get(id).map(|h| a.map_or(h.clone(), |a| replace_fragment(h, a))),
            (None, Some(a)) => Some(format!("#{a}")),
            (None, None) => None,
        };
        match href {
            Some(h) => {
                let mut attrs = vec![("href", h.as_str())];
                if let Some(t) = target_frame {
                    attrs.push(("target", t));
                }
                vec![H::cel("a", &attrs, kids)]
            }
            None => kids,
        }
    }

    /// Images become `<img alt>` only; the pixels are not embedded since the
    /// reader shows alt text.
    fn img(&self, alt: Option<&str>) -> H {
        match alt.filter(|a| !a.is_empty()) {
            Some(a) => H::el("img", &[("alt", a)], Vec::new()),
            None => H::el("img", &[], Vec::new()),
        }
    }

    fn inline_image(&self, e: &Xml) -> Vec<H> {
        let doc_pr = e.child("wp:docPr");
        let alt = match doc_pr.and_then(|d| d.attr("descr")) {
            Some(d) if !d.trim().is_empty() => Some(d),
            _ => doc_pr.and_then(|d| d.attr("title")),
        };
        let href = doc_pr
            .and_then(|d| d.child("a:hlinkClick"))
            .and_then(|h| h.attr("r:id"))
            .and_then(|id| self.rels.get(id))
            .cloned();
        let mut images = Vec::new();
        for graphic in e.children("a:graphic") {
            for data in graphic.children("a:graphicData") {
                for pic in data.children("pic:pic") {
                    for fill in pic.children("pic:blipFill") {
                        for blip in fill.children("a:blip") {
                            if blip.attr("r:embed").is_some() || blip.attr("r:link").is_some() {
                                images.push(self.img(alt));
                            }
                        }
                    }
                }
            }
        }
        match href {
            Some(h) if !images.is_empty() => vec![H::cel("a", &[("href", &h)], images)],
            _ => images,
        }
    }

    fn note_reference(&mut self, ty: &str, e: &Xml) -> Vec<H> {
        let Some(id) = e.attr("w:id") else { return Vec::new() };
        self.note_refs.push((ty.to_string(), id.to_string()));
        let n = self.note_refs.len();
        vec![H::el(
            "sup",
            &[],
            vec![H::el(
                "a",
                &[("href", &format!("#{ty}-{id}")), ("id", &format!("{ty}-ref-{id}"))],
                vec![H::Text(format!("[{n}]"))],
            )],
        )]
    }

    fn table(&mut self, e: &Xml) -> Vec<H> {
        struct Cell {
            kids: Vec<H>,
            colspan: usize,
            rowspan: usize,
            vmerge: bool,
        }
        struct Row {
            header: bool,
            cells: Vec<Cell>,
        }
        let mut rows: Vec<Row> = Vec::new();
        for tr in e.children("w:tr") {
            let tr_pr = tr.child("w:trPr");
            if tr_pr.is_some_and(|p| p.child("w:del").is_some()) {
                continue;
            }
            let header = tr_pr.is_some_and(|p| p.child("w:tblHeader").is_some());
            let mut cells = Vec::new();
            for tc in tr.children("w:tc") {
                let pr = tc.child("w:tcPr");
                let colspan = val(pr.and_then(|p| p.child("w:gridSpan"))).and_then(|v| v.parse().ok()).unwrap_or(1);
                let vmerge = pr
                    .and_then(|p| p.child("w:vMerge"))
                    .is_some_and(|v| matches!(v.attr("w:val"), None | Some("") | Some("continue")));
                cells.push(Cell { kids: self.convert(&tc.kids), colspan, rowspan: 1, vmerge });
            }
            rows.push(Row { header, cells });
        }
        // Vertically merged cells extend the cell above them in the same column.
        let mut columns: HashMap<usize, (usize, usize)> = HashMap::new();
        for ri in 0..rows.len() {
            let mut col = 0;
            for ci in 0..rows[ri].cells.len() {
                match columns.get(&col) {
                    Some(&(r0, c0)) if rows[ri].cells[ci].vmerge => rows[r0].cells[c0].rowspan += 1,
                    _ => {
                        columns.insert(col, (ri, ci));
                        rows[ri].cells[ci].vmerge = false;
                    }
                }
                col += rows[ri].cells[ci].colspan;
            }
        }
        let body_index = rows.iter().position(|r| !r.header).unwrap_or(rows.len());
        let render = |rows: &[Row], header: bool| -> Vec<H> {
            rows.iter()
                .map(|r| {
                    let mut kids = vec![H::Force];
                    for c in r.cells.iter().filter(|c| !c.vmerge) {
                        let mut attrs: Vec<(&str, String)> = Vec::new();
                        if c.colspan != 1 {
                            attrs.push(("colspan", c.colspan.to_string()));
                        }
                        if c.rowspan != 1 {
                            attrs.push(("rowspan", c.rowspan.to_string()));
                        }
                        let attrs: Vec<(&str, &str)> = attrs.iter().map(|(k, v)| (*k, v.as_str())).collect();
                        let mut cell_kids = vec![H::Force];
                        cell_kids.extend(c.kids.iter().cloned());
                        kids.push(H::el(if header { "th" } else { "td" }, &attrs, cell_kids));
                    }
                    H::el("tr", &[], kids)
                })
                .collect()
        };
        let mut kids = vec![H::Force];
        if body_index == 0 {
            kids.extend(render(&rows, false));
        } else {
            kids.push(H::el("thead", &[], render(&rows[..body_index], true)));
            kids.push(H::el("tbody", &[], render(&rows[body_index..], false)));
        }
        vec![H::el("table", &[], kids)]
    }
}

/// Keeps only the `mc:Fallback` branch of `mc:AlternateContent`, which is the
/// VML form (text boxes included) that mammoth reads.
fn collapse_alternate_content(node: &mut Xml) {
    let kids = std::mem::take(&mut node.kids);
    for k in kids {
        match k {
            XNode::El(e) if e.name == "mc:AlternateContent" => {
                if let Some(fallback) = e.child("mc:Fallback") {
                    let mut fb = fallback.clone();
                    collapse_alternate_content(&mut fb);
                    node.kids.extend(fb.kids);
                }
            }
            XNode::El(mut e) => {
                collapse_alternate_content(&mut e);
                node.kids.push(XNode::El(e));
            }
            text => node.kids.push(text),
        }
    }
}

fn read_docx_part<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Xml> {
    let mut root = read_xml(zip, name)?;
    collapse_alternate_content(&mut root);
    Some(root)
}

fn read_notes<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, part: &str, ty: &str, notes: &mut HashMap<(String, String), Note>) {
    let Some(root) = read_docx_part(zip, part) else { return };
    let rels = rel_map(&read_rels(zip, &rels_path_for(part)));
    for n in root.children(&format!("w:{ty}")) {
        if matches!(n.attr("w:type"), Some("continuationSeparator" | "separator")) {
            continue;
        }
        if let Some(id) = n.attr("w:id") {
            notes.insert((ty.to_string(), id.to_string()), Note { body: n.kids.clone(), rels: rels.clone() });
        }
    }
}

fn rel_map(rels: &[Rel]) -> HashMap<String, String> {
    rels.iter().map(|r| (r.id.clone(), r.target.clone())).collect()
}

/// The first existing target of the given relationship type, else the conventional path.
fn find_part<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, rels: &[Rel], suffix: &str, base: &str, fallback: &str) -> String {
    rels.iter()
        .filter(|r| r.ty.ends_with(suffix))
        .map(|r| join_zip_path(base, &r.target))
        .find(|p| zip.by_name(p).is_ok())
        .unwrap_or_else(|| fallback.to_string())
}

/// Converts a `.docx` package to the HTML mammoth would produce (images without pixel data).
fn docx_to_html<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> anyhow::Result<String> {
    let package_rels = read_rels(zip, "_rels/.rels");
    let main = find_part(zip, &package_rels, "/officeDocument", "", "word/document.xml");
    if zip.by_name(&main).is_err() {
        bail!("Could not find main document part. Are you sure this is a valid .docx file?");
    }
    let base = zip_dir(&main).to_string();
    let doc_rels = read_rels(zip, &rels_path_for(&main));
    let mut part = |name: &str| find_part(zip, &doc_rels, &format!("/{name}"), &base, &format!("word/{name}.xml"));
    let (styles_p, numbering_p, footnotes_p, endnotes_p) = (part("styles"), part("numbering"), part("footnotes"), part("endnotes"));

    let styles = read_xml(zip, &styles_p).map(|r| read_styles(&r)).unwrap_or_default();
    let numbering = read_xml(zip, &numbering_p).map(|r| read_numbering(&r)).unwrap_or_default();
    let mut notes = HashMap::new();
    read_notes(zip, &footnotes_p, "footnote", &mut notes);
    read_notes(zip, &endnotes_p, "endnote", &mut notes);

    let root = read_docx_part(zip, &main).context("The main document part is not valid XML.")?;
    let body = root.child("w:body").context("Could not find the body element: are you sure this is a docx file?")?;

    let mut conv = Conv {
        styles,
        numbering,
        rels: rel_map(&doc_rels),
        notes,
        note_refs: Vec::new(),
        fields: Vec::new(),
        instr: Vec::new(),
        deleted: Vec::new(),
        extra: Vec::new(),
    };
    let mut nodes = conv.convert(&body.kids);

    // Notes are listed in reference order; converting one may reference more.
    let mut items = Vec::new();
    let mut i = 0;
    while i < conv.note_refs.len() {
        let (ty, id) = conv.note_refs[i].clone();
        i += 1;
        let Some(note) = conv.notes.get(&(ty.clone(), id.clone())) else { continue };
        let (body_xml, rels) = (note.body.clone(), note.rels.clone());
        let saved = std::mem::replace(&mut conv.rels, rels);
        let mut item = conv.convert(&body_xml);
        conv.rels = saved;
        item.push(H::cel(
            "p",
            &[],
            vec![H::Text(" ".into()), H::el("a", &[("href", &format!("#{ty}-ref-{id}"))], vec![H::Text("\u{2191}".into())])],
        ));
        items.push(H::el("li", &[("id", &format!("{ty}-{id}"))], item));
    }
    nodes.push(H::el("ol", &[], items));

    let mut html = String::new();
    write_html(&collapse(strip_empty(nodes)), &mut html);
    Ok(html)
}

/// Title and author from docProps/core.xml (what python-docx reports).
fn docx_core_properties<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> (String, String) {
    let Some(core) = read_xml(zip, "docProps/core.xml") else { return Default::default() };
    let text = |name| core.child(name).map(|e| e.inner_text()).unwrap_or_default();
    (text("dc:title"), text("dc:creator"))
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| OpenError::Other(format!("{} is not a valid .docx file: {e}", file_name(path))))?;
    let html = docx_to_html(&mut zip)?;
    let parsed = parse_html(&html);

    let (title, author) = docx_core_properties(&mut zip);
    let meta = DocMetadata {
        title: if title.is_empty() { file_name(path) } else { title },
        author,
        format: "DOCX".into(),
        extra: Vec::new(),
    };
    let toc = toc_from_headings(&parsed.elements);
    let pages = paginate_with_elements(parsed.text, parsed.elements, effective_page_size(opts));
    Ok(Box::new(PagedDoc::new(path, pages, toc, meta)))
}

// ---------------------------------------------------------------------------
// Legacy DOC
// ---------------------------------------------------------------------------

/// Antiword prints Latin-1/cp1252 unless told otherwise. Line breaks become `\n`
/// because Prism reads tool output in text mode, which does the same.
fn decode_tool_output(bytes: Vec<u8>) -> String {
    let text = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => encoding_rs::WINDOWS_1252.decode(e.as_bytes()).0.into_owned(),
    };
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub fn doc_available() -> bool {
    check_external_tool("antiword")
}

/// antiword to DocBook, then pandoc to HTML when pandoc exists; plain antiword
/// text if either step fails.
fn doc_text(path: &Path) -> anyhow::Result<String> {
    let path_str = path.to_string_lossy();
    if check_external_tool("pandoc") {
        let via_pandoc = || -> anyhow::Result<String> {
            let docbook = run_external_tool("antiword", &["-x", "db", &path_str], 60)?;
            let mut tmp = tempfile::Builder::new().suffix(".xml").tempfile()?;
            std::io::Write::write_all(&mut tmp, &docbook)?;
            let html = run_external_tool("pandoc", &["--from", "docbook", "--to", "html", &tmp.path().to_string_lossy()], 60)?;
            Ok(parse_html(&decode_tool_output(html)).text)
        };
        if let Ok(text) = via_pandoc() {
            return Ok(text);
        }
    }
    Ok(decode_tool_output(run_external_tool("antiword", &[&path_str], 60)?))
}

pub fn open_doc(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let text = doc_text(path)?;
    let pages = paginate_text(&text, effective_page_size(opts)).into_iter().map(Page::plain).collect();
    let meta = DocMetadata { title: file_name(path), format: "DOC".into(), ..Default::default() };
    Ok(Box::new(PagedDoc::new(path, pages, Vec::new(), meta)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn docx_with(parts: &[(&str, &str)]) -> zip::ZipArchive<Cursor<Vec<u8>>> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in parts {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        zip::ZipArchive::new(w.finish().unwrap()).unwrap()
    }

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    #[test]
    fn docx_maps_headings_lists_formatting_links_and_tables_like_mammoth() {
        let document = format!(
            r#"<w:document {NS}><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Plain </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t> more</w:t></w:r>
<w:hyperlink r:id="rId5"><w:r><w:t>site</w:t></w:r></w:hyperlink></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>one</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>nested</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>first</w:t></w:r></w:p>
<w:tbl><w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:p><w:r><w:t>H</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>wide</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
</w:body></w:document>"#
        );
        let numbering = format!(
            r#"<w:numbering {NS}>
<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl><w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#
        );
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com/a?x=1&amp;y=2" TargetMode="External"/></Relationships>"#;
        let mut zip = docx_with(&[
            ("word/document.xml", &document),
            ("word/numbering.xml", &numbering),
            ("word/_rels/document.xml.rels", rels),
        ]);
        assert_eq!(
            docx_to_html(&mut zip).unwrap(),
            concat!(
                "<h1>Title</h1>",
                "<p>Plain <strong>bold more</strong><a href=\"https://example.com/a?x=1&amp;y=2\">site</a></p>",
                "<ul><li>one<ul><li>nested</li></ul></li></ul>",
                "<ol><li>first</li></ol>",
                "<table><thead><tr><th><p>H</p></th></tr></thead>",
                "<tbody><tr><td colspan=\"2\"><p>wide</p></td></tr></tbody></table>",
            )
        );
    }

    #[test]
    fn docx_notes_images_and_vertical_merge() {
        let document = format!(
            r#"<w:document {NS} xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><w:body>
<w:p><w:r><w:t>Text</w:t></w:r><w:r><w:footnoteReference w:id="2"/></w:r></w:p>
<w:p><w:r><w:drawing><wp:inline><wp:docPr id="1" descr="A chart"/><a:graphic><a:graphicData><pic:pic><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>
<w:tbl><w:tr><w:tc><w:tcPr><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc></w:tr></w:tbl>
</w:body></w:document>"#
        );
        let footnotes = format!(
            r#"<w:footnotes {NS}><w:footnote w:type="separator" w:id="0"><w:p/></w:footnote><w:footnote w:id="2"><w:p><w:r><w:t>Note body</w:t></w:r></w:p></w:footnote></w:footnotes>"#
        );
        let mut zip = docx_with(&[("word/document.xml", &document), ("word/footnotes.xml", &footnotes)]);
        assert_eq!(
            docx_to_html(&mut zip).unwrap(),
            concat!(
                "<p>Text<sup><a href=\"#footnote-2\" id=\"footnote-ref-2\">[1]</a></sup></p>",
                "<p><img alt=\"A chart\" /></p>",
                "<table><tr><td rowspan=\"2\"><p>A</p></td></tr><tr></tr></table>",
                "<ol><li id=\"footnote-2\"><p>Note body <a href=\"#footnote-ref-2\">\u{2191}</a></p></li></ol>",
            )
        );
    }

    #[test]
    fn docx_reads_the_fallback_branch_of_alternate_content_after_its_paragraph() {
        let document = format!(
            r#"<w:document {NS} xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:v="urn:schemas-microsoft-com:vml"><w:body>
<w:p><w:r><w:t>Before</w:t></w:r><w:r><mc:AlternateContent><mc:Choice><w:drawing>modern box</w:drawing></mc:Choice>
<mc:Fallback><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>Boxed</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>
</w:body></w:document>"#
        );
        let mut zip = docx_with(&[("word/document.xml", &document)]);
        assert_eq!(docx_to_html(&mut zip).unwrap(), "<p>Before</p><p>Boxed</p>");
    }

    #[test]
    fn xml_parser_resolves_entities_and_merges_text() {
        let root = parse_xml(br#"<a x="1 &amp; 2">one &lt; two &#x41;<b/></a>"#).unwrap();
        assert_eq!(root.attr("x"), Some("1 & 2"));
        assert_eq!(root.inner_text(), "one < two A");
        assert!(parse_xml(b"<a><b></a>").is_none());
    }

    #[test]
    fn zip_paths_resolve_dot_segments() {
        assert_eq!(join_zip_path("ppt", "../notesSlides/n1.xml"), "notesSlides/n1.xml");
        assert_eq!(join_zip_path("ppt", "/ppt/slides/s1.xml"), "ppt/slides/s1.xml");
        assert_eq!(rels_path_for("ppt/slides/slide1.xml"), "ppt/slides/_rels/slide1.xml.rels");
    }

    #[test]
    fn headings_become_toc_entries_truncated_to_80_chars() {
        use crate::structured_text::elements::{ElementMeta, ElementType};
        let h = |level: u8, text: &str| SemanticElement {
            element_type: ElementType::heading(level),
            start_pos: 0,
            end_pos: text.len(),
            text: text.into(),
            meta: ElementMeta { level: Some(level), ..Default::default() },
        };
        let long = "x".repeat(100);
        let toc = toc_from_headings(&[h(1, "A"), h(3, &long)]);
        assert_eq!((toc[0].title.as_str(), toc[0].level), ("A", 0));
        assert_eq!((toc[1].title.chars().count(), toc[1].level), (80, 2));
    }
}
