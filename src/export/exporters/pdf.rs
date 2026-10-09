//! Tagged PDF exporter (krilla). Replaces Prism's fpdf2 exporter.
//!
//! Output: A4 pages, a system font embedded (Segoe UI, else Arial), text wrapped and
//! paginated by this module. With `tag_for_accessibility` the file is PDF/UA-1: a tag tree
//! (H1-H6, P, BlockQuote, L/LI/Lbl/LBody), document title and language, and an outline
//! built from the headings. If PDF/UA validation rejects a font or the content, the next
//! font is tried, and as a last resort the PDF is written tagged but unvalidated.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};
use krilla::Document as PdfDoc;
use krilla::SerializeSettings;
use krilla::configure::{Accessibility, ConfigurationBuilder};
use krilla::destination::XyzDestination;
use krilla::geom::Point;
use krilla::metadata::Metadata;
use krilla::outline::{Outline, OutlineNode};
use krilla::page::PageSettings;
use krilla::tagging::{ContentTag, Identifier, ListNumbering, Node, SpanTag, Tag, TagGroup, TagTree};
use krilla::text::{Font, TextDirection};
use rustybuzz::ttf_parser::Face;

use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::base::{ExportOptions, Exporter, ProgressFn, write_file};
use crate::export::util::{Block, check_cancel, list_items, page_blocks};
use crate::structured_text::elements::ElementType;

pub struct PdfExporter;

const PAGE_W: f32 = 595.276;
const PAGE_H: f32 = 841.89;
const MARGIN: f32 = 56.7;
const LANG: &str = "en";
const HEADING_SIZES: [f32; 6] = [24.0, 20.0, 16.0, 14.0, 12.0, 11.0];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FontKind {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    Mono = 3,
}

/// Font files of one family; regular is required, the rest fall back to it.
struct FamilyFiles {
    name: &'static str,
    data: [Arc<Vec<u8>>; 4],
}

fn fonts_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts")
}

fn load_families() -> Vec<FamilyFiles> {
    let dir = fonts_dir();
    let read = |name: &str| std::fs::read(dir.join(name)).ok().map(Arc::new);
    let mono = read("consola.ttf").or_else(|| read("cour.ttf"));
    [("Segoe UI", ["segoeui.ttf", "segoeuib.ttf", "segoeuii.ttf"]), ("Arial", ["arial.ttf", "arialbd.ttf", "ariali.ttf"])]
        .into_iter()
        .filter_map(|(name, [r, b, i])| {
            let regular = read(r)?;
            Some(FamilyFiles {
                name,
                data: [regular.clone(), read(b).unwrap_or(regular.clone()), read(i).unwrap_or(regular.clone()), mono.clone().unwrap_or(regular)],
            })
        })
        .collect()
}

/// Glyph coverage and advances for one font file.
struct Metrics<'a> {
    face: Face<'a>,
    upem: f32,
    cache: RefCell<HashMap<char, Option<f32>>>,
    /// Characters replaced by `?` because the font has no glyph.
    missing: Cell<usize>,
}

impl<'a> Metrics<'a> {
    fn new(data: &'a [u8]) -> anyhow::Result<Self> {
        let face = Face::parse(data, 0).context("unreadable font file")?;
        Ok(Self { upem: f32::from(face.units_per_em()), face, cache: RefCell::new(HashMap::new()), missing: Cell::new(0) })
    }

    /// Advance in em, or `None` when the font has no glyph for `c`.
    fn advance(&self, c: char) -> Option<f32> {
        *self.cache.borrow_mut().entry(c).or_insert_with(|| {
            let gid = self.face.glyph_index(c)?;
            Some(f32::from(self.face.glyph_hor_advance(gid).unwrap_or(0)) / self.upem)
        })
    }

    /// Text safe to draw: zero-width format chars dropped, tabs spaced, unmapped chars shown as `?`
    /// (PDF/UA forbids .notdef glyphs).
    fn clean(&self, s: &str) -> String {
        s.chars()
            .filter(|c| !matches!(c, '\u{200B}'..='\u{200D}' | '\u{FEFF}' | '\u{00AD}' | '\r'))
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control() || *c == '\n')
            .map(|c| {
                if c == '\n' || self.advance(c).is_some() {
                    c
                } else {
                    self.missing.set(self.missing.get() + 1);
                    '?'
                }
            })
            .collect()
    }

    fn width(&self, s: &str, size: f32) -> f32 {
        s.chars().map(|c| self.advance(c).unwrap_or(0.0)).sum::<f32>() * size
    }
}

#[derive(Clone, Copy)]
struct Style {
    font: FontKind,
    size: f32,
    leading: f32,
    indent: f32,
    before: f32,
    after: f32,
    /// Keep the author's spacing and break only by characters.
    preformatted: bool,
}

impl Style {
    const BODY: Style = Style { font: FontKind::Regular, size: 11.0, leading: 15.0, indent: 0.0, before: 0.0, after: 8.0, preformatted: false };

    fn heading(level: u8) -> Style {
        let size = HEADING_SIZES[usize::from(level.clamp(1, 6)) - 1];
        Style { font: FontKind::Bold, size, leading: size * 1.3, indent: 0.0, before: size * 0.8, after: 6.0, preformatted: false }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Heading(u8),
    Paragraph,
    Quote,
    Code,
    /// List number (items of one list share it).
    ListItem(usize),
}

struct Line {
    x: f32,
    baseline: f32,
    text: String,
    font: FontKind,
    size: f32,
}

struct LBlock {
    kind: Kind,
    text: String,
    label: Option<Line>,
    lines: Vec<Line>,
    label_ids: Vec<Identifier>,
    ids: Vec<Identifier>,
}

#[derive(Clone, Copy)]
enum Slot {
    Label,
    Line(usize),
}

struct HeadingMark {
    level: u8,
    text: String,
    page: usize,
    y: f32,
}

struct Layout<'a> {
    metrics: &'a [Metrics<'a>; 4],
    blocks: Vec<LBlock>,
    /// Per page, the lines in drawing (= reading) order.
    pages: Vec<Vec<(usize, Slot)>>,
    headings: Vec<HeadingMark>,
    page: usize,
    y: f32,
    next_list: usize,
}

const BOTTOM: f32 = PAGE_H - MARGIN;

impl<'a> Layout<'a> {
    fn new(metrics: &'a [Metrics<'a>; 4]) -> Self {
        Self { metrics, blocks: Vec::new(), pages: vec![Vec::new()], headings: Vec::new(), page: 0, y: MARGIN, next_list: 0 }
    }

    fn new_page(&mut self) {
        self.page += 1;
        self.pages.push(Vec::new());
        self.y = MARGIN;
    }

    fn room_for(&mut self, height: f32) {
        if self.y + height > BOTTOM && self.y > MARGIN {
            self.new_page();
        }
    }

    fn wrap(&self, text: &str, st: &Style, width: f32) -> Vec<String> {
        let m = &self.metrics[st.font as usize];
        let mut out = Vec::new();
        for hard in m.clean(text).split('\n') {
            if hard.trim().is_empty() {
                out.push(String::new());
                continue;
            }
            let mut line = String::new();
            let flush = |line: &mut String, out: &mut Vec<String>| {
                out.push(std::mem::take(line).trim_end().to_string());
            };
            if st.preformatted {
                for c in hard.chars() {
                    if m.width(&line, st.size) + m.width(c.encode_utf8(&mut [0; 4]), st.size) > width && !line.is_empty() {
                        flush(&mut line, &mut out);
                    }
                    line.push(c);
                }
            } else {
                for word in hard.split(' ').filter(|w| !w.is_empty()) {
                    let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
                    if m.width(&candidate, st.size) <= width {
                        line = candidate;
                        continue;
                    }
                    if !line.is_empty() {
                        flush(&mut line, &mut out);
                    }
                    // A word wider than the line is broken by characters.
                    for c in word.chars() {
                        if m.width(&line, st.size) + m.width(c.encode_utf8(&mut [0; 4]), st.size) > width && !line.is_empty() {
                            flush(&mut line, &mut out);
                        }
                        line.push(c);
                    }
                }
            }
            if !line.is_empty() {
                flush(&mut line, &mut out);
            }
        }
        out
    }

    /// Places a block; returns its index. `label` is drawn left of the first line (list bullet).
    fn add(&mut self, kind: Kind, st: Style, text: &str, label: Option<&str>) -> Option<usize> {
        let width = PAGE_W - 2.0 * MARGIN - st.indent;
        let lines = self.wrap(text, &st, width);
        if lines.iter().all(String::is_empty) {
            return None;
        }
        let keep_with_next = if matches!(kind, Kind::Heading(_)) { 2.0 * Style::BODY.leading } else { 0.0 };
        if self.y > MARGIN {
            self.y += st.before;
        }
        self.room_for(st.leading + keep_with_next);

        let bi = self.blocks.len();
        self.blocks.push(LBlock { kind: kind.clone(), text: text.to_string(), label: None, lines: Vec::new(), label_ids: Vec::new(), ids: Vec::new() });
        for (n, text) in lines.into_iter().enumerate() {
            self.room_for(st.leading);
            let top = self.y;
            self.y += st.leading;
            if n == 0 {
                if let Kind::Heading(level) = kind {
                    self.headings.push(HeadingMark { level, text: self.blocks[bi].text.trim().to_string(), page: self.page, y: top });
                }
                if let Some(l) = label {
                    let line = Line { x: MARGIN + st.indent - 14.0, baseline: top + st.size * 1.05, text: l.to_string(), font: st.font, size: st.size };
                    self.blocks[bi].label = Some(line);
                    self.pages[self.page].push((bi, Slot::Label));
                }
            }
            if text.is_empty() {
                continue;
            }
            let line = Line { x: MARGIN + st.indent, baseline: top + st.size * 1.05, text, font: st.font, size: st.size };
            let b = &mut self.blocks[bi];
            b.lines.push(line);
            let idx = b.lines.len() - 1;
            self.pages[self.page].push((bi, Slot::Line(idx)));
        }
        self.y += st.after;
        Some(bi)
    }

    fn add_item(&mut self, doc_title: &Option<String>, authors: &str, items: &[Item]) {
        if let Some(t) = doc_title {
            self.add(Kind::Heading(1), Style::heading(1), t, None);
            if !authors.is_empty() {
                let st = Style { font: FontKind::Italic, size: 14.0, leading: 19.0, after: 14.0, ..Style::BODY };
                self.add(Kind::Paragraph, st, &format!("by {authors}"), None);
            }
        }
        for item in items {
            match item {
                Item::PageBreak => {
                    if self.y > MARGIN {
                        self.new_page();
                    }
                }
                Item::Block(b) => self.add_block(b),
            }
        }
    }

    fn add_block(&mut self, b: &Block) {
        match b.kind {
            k if k.is_heading() => {
                let n = k.heading_level().unwrap_or(1);
                self.add(Kind::Heading(n), Style::heading(n), &b.text, None);
            }
            ElementType::Quote => {
                self.add(Kind::Quote, Style { font: FontKind::Italic, indent: 24.0, ..Style::BODY }, &b.text, None);
            }
            ElementType::CodeBlock => {
                self.add(Kind::Code, Style { font: FontKind::Mono, size: 10.0, leading: 13.0, preformatted: true, ..Style::BODY }, &b.text, None);
            }
            ElementType::List => {
                let list = self.next_list;
                self.next_list += 1;
                let items = list_items(&b.text);
                let last = items.len().saturating_sub(1);
                for (i, item) in items.iter().enumerate() {
                    let after = if i == last { Style::BODY.after } else { 3.0 };
                    self.add(Kind::ListItem(list), Style { indent: 22.0, after, ..Style::BODY }, item, Some("\u{2022}"));
                }
            }
            _ => {
                self.add(Kind::Paragraph, Style::BODY, &b.text, None);
            }
        }
    }
}

enum Item {
    PageBreak,
    Block(Block),
}

fn leaves(ids: Vec<Identifier>) -> Vec<Node> {
    ids.into_iter().map(Node::Leaf).collect()
}

fn group(tag: impl Into<krilla::tagging::TagKind>, children: Vec<Node>) -> TagGroup {
    TagGroup::with_children(tag, children)
}

fn tag_tree(blocks: Vec<LBlock>) -> TagTree {
    let mut tree = TagTree::new().with_lang(Some(LANG.to_string()));
    let mut list: Option<(usize, TagGroup)> = None;
    for b in blocks {
        if let Kind::ListItem(n) = b.kind {
            let item = group(Tag::LI, vec![group(Tag::Lbl, leaves(b.label_ids)).into(), group(Tag::LBody, leaves(b.ids)).into()]);
            match &mut list {
                Some((cur, l)) if *cur == n => l.push(item),
                _ => {
                    if let Some((_, done)) = list.take() {
                        tree.push(done);
                    }
                    list = Some((n, group(Tag::L(ListNumbering::Disc), vec![item.into()])));
                }
            }
            continue;
        }
        if let Some((_, done)) = list.take() {
            tree.push(done);
        }
        let g = match b.kind {
            Kind::Heading(n) => group(Tag::Hn(NonZeroU16::new(u16::from(n)).expect("level 1-6"), Some(b.text.trim().to_string())), leaves(b.ids)),
            Kind::Quote => group(Tag::BlockQuote, vec![group(Tag::P, leaves(b.ids)).into()]),
            Kind::Code => group(Tag::P, vec![group(Tag::Code, leaves(b.ids)).into()]),
            _ => group(Tag::P, leaves(b.ids)),
        };
        tree.push(g);
    }
    if let Some((_, done)) = list {
        tree.push(done);
    }
    tree
}

fn outline(headings: &[HeadingMark], fallback_title: &str) -> Outline {
    let dest = |page: usize, y: f32| XyzDestination::new(page, Point::from_xy(MARGIN, y));
    let mut root = Outline::new();
    let mut stack: Vec<(u8, OutlineNode)> = Vec::new();
    let close = |stack: &mut Vec<(u8, OutlineNode)>, root: &mut Outline| {
        if let Some((_, done)) = stack.pop() {
            match stack.last_mut() {
                Some((_, parent)) => parent.push_child(done),
                None => root.push_child(done),
            }
        }
    };
    for h in headings {
        while stack.last().is_some_and(|(l, _)| *l >= h.level) {
            close(&mut stack, &mut root);
        }
        stack.push((h.level, OutlineNode::new(h.text.clone(), dest(h.page, h.y)).with_open(true)));
    }
    while !stack.is_empty() {
        close(&mut stack, &mut root);
    }
    if headings.is_empty() {
        // PDF/UA requires an outline even for a document with no headings.
        root.push_child(OutlineNode::new(fallback_title.to_string(), dest(0, MARGIN)));
    }
    root
}

struct Job<'a> {
    title: Option<String>,
    authors: String,
    items: &'a [Item],
    tagged: bool,
}

/// Lays out and writes the PDF with one font family. `validate` turns on PDF/UA-1 checking.
fn render(family: &FamilyFiles, job: &Job, validate: bool) -> anyhow::Result<Vec<u8>> {
    let metrics = [
        Metrics::new(&family.data[0])?,
        Metrics::new(&family.data[1])?,
        Metrics::new(&family.data[2])?,
        Metrics::new(&family.data[3])?,
    ];
    let fonts: Vec<Font> = family
        .data
        .iter()
        .map(|d| Font::new(Arc::clone(d).into(), 0).context("font cannot be embedded"))
        .collect::<anyhow::Result<_>>()?;

    let mut layout = Layout::new(&metrics);
    layout.add_item(&job.title, &job.authors, job.items);
    let Layout { mut blocks, pages, headings, .. } = layout;
    let missing: usize = metrics.iter().map(|m| m.missing.get()).sum();
    if missing > 0 {
        log::warn!("PDF: {missing} characters have no glyph in {} and were written as '?'", family.name);
    }

    let mut builder = ConfigurationBuilder::new();
    if validate {
        builder = builder.with_accessibility_validator(Accessibility::UA1);
    }
    let settings = SerializeSettings {
        enable_tagging: job.tagged,
        configuration: builder.finish().map_err(|e| anyhow::anyhow!("PDF configuration: {e:?}"))?,
        ..Default::default()
    };
    let mut pdf = PdfDoc::new_with(settings);
    let span = SpanTag::empty().with_lang(Some(LANG));
    for items in &pages {
        let mut page = pdf.start_page_with(PageSettings::from_wh(PAGE_W, PAGE_H).context("page size")?);
        let mut surface = page.surface();
        for &(bi, slot) in items {
            let block = &mut blocks[bi];
            let line = match slot {
                Slot::Label => block.label.as_ref(),
                Slot::Line(i) => block.lines.get(i),
            }
            .expect("slot refers to a placed line");
            let id = surface.start_tagged(ContentTag::Span(span));
            surface.draw_text(Point::from_xy(line.x, line.baseline), fonts[line.font as usize].clone(), line.size, &line.text, false, TextDirection::Auto);
            surface.end_tagged();
            match slot {
                Slot::Label => block.label_ids.push(id),
                Slot::Line(_) => block.ids.push(id),
            }
        }
        surface.finish();
        page.finish();
    }

    let doc_title = job.title.clone().unwrap_or_else(|| "Untitled".into());
    let mut meta = Metadata::new().title(doc_title.clone()).language(LANG.into()).creator("InTome".into());
    if !job.authors.is_empty() {
        meta = meta.authors(vec![job.authors.clone()]);
    }
    pdf.set_metadata(meta);
    if job.tagged {
        pdf.set_outline(outline(&headings, &doc_title));
        pdf.set_tag_tree(tag_tree(blocks));
    }
    pdf.finish().map_err(|e| anyhow::anyhow!("{e:?}"))
}

impl Exporter for PdfExporter {
    fn format_name(&self) -> &'static str {
        "PDF"
    }
    fn format_key(&self) -> &'static str {
        "pdf"
    }
    fn file_extension(&self) -> &'static str {
        ".pdf"
    }
    fn is_available(&self) -> bool {
        ["segoeui.ttf", "arial.ttf"].iter().any(|f| fonts_dir().join(f).is_file())
    }

    fn export(&self, doc: &mut dyn Document, options: &ExportOptions, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<PathBuf> {
        let page_count = doc.page_count();
        let meta = doc.metadata();
        let mut items = Vec::new();
        for i in 0..page_count {
            check_cancel(cancel)?;
            if i > 0 {
                items.push(Item::PageBreak);
            }
            items.extend(page_blocks(doc.page(i)).into_iter().map(Item::Block));
            progress(i + 1, page_count, &format!("Page {}", i + 1));
        }
        let job = Job {
            title: Some(meta.title).filter(|t| !t.is_empty()),
            authors: meta.author,
            items: &items,
            tagged: options.tag_for_accessibility,
        };

        let families = load_families();
        if families.is_empty() {
            bail!("No usable system font found in {} (need Segoe UI or Arial).", fonts_dir().display());
        }
        // (family, PDF/UA validation): UA first with each font, then tagged-but-unvalidated.
        let mut attempts: Vec<(&FamilyFiles, bool)> = Vec::new();
        if job.tagged {
            attempts.extend(families.iter().map(|f| (f, true)));
        }
        attempts.push((&families[0], false));
        let mut last_err = None;
        for (family, validate) in attempts {
            match render(family, &job, validate) {
                Ok(bytes) => {
                    if job.tagged && !validate {
                        log::warn!("PDF/UA validation failed for every font; wrote a tagged PDF without the PDF/UA claim");
                    }
                    write_file(&options.output_path, bytes)?;
                    return Ok(options.output_path.clone());
                }
                Err(e) => {
                    log::warn!("PDF export with {} (PDF/UA {validate}) failed: {e:#}", family.name);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.expect("at least one attempt")).context("PDF export failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str) -> Item {
        Item::Block(Block { kind: ElementType::Paragraph, text: text.into(), url: None, alt: None })
    }

    #[test]
    fn wrap_keeps_lines_within_width_and_breaks_long_words() {
        let Some(family) = load_families().into_iter().next() else { return };
        let metrics = [0, 1, 2, 3].map(|i| Metrics::new(&family.data[i]).unwrap());
        let layout = Layout::new(&metrics);
        let lines = layout.wrap("alpha beta gamma delta epsilon zeta eta theta", &Style::BODY, 80.0);
        assert!(lines.len() > 2);
        assert!(lines.iter().all(|l| metrics[0].width(l, 11.0) <= 80.0), "{lines:?}");
        assert_eq!(lines.join(" "), "alpha beta gamma delta epsilon zeta eta theta");
        let long = layout.wrap(&"x".repeat(100), &Style::BODY, 80.0);
        assert!(long.len() > 1 && long.iter().all(|l| metrics[0].width(l, 11.0) <= 80.0));
        assert_eq!(layout.wrap("日本 ok", &Style::BODY, 80.0), vec!["?? ok"]);
    }

    #[test]
    fn long_document_paginates_and_is_tagged_pdf_ua() {
        let Some(family) = load_families().into_iter().next() else { return };
        let mut items = vec![Item::Block(Block { kind: ElementType::Heading1, text: "Part".into(), url: None, alt: None })];
        for i in 0..150 {
            items.push(para(&format!("Paragraph {i}. {}", "lorem ipsum dolor sit amet ".repeat(30))));
        }
        let job = Job { title: Some("Long".into()), authors: String::new(), items: &items, tagged: true };
        let bytes = render(&family, &job, true).unwrap();
        let pdf = lopdf::Document::load_mem(&bytes).unwrap();
        assert!(pdf.get_pages().len() >= 10, "pages: {}", pdf.get_pages().len());
        let catalog = pdf.catalog().unwrap();
        assert!(catalog.has(b"StructTreeRoot") && catalog.has(b"MarkInfo") && catalog.has(b"Lang"));

        let untagged = Job { tagged: false, ..job };
        let bytes = render(&family, &untagged, false).unwrap();
        let pdf = lopdf::Document::load_mem(&bytes).unwrap();
        assert!(!pdf.catalog().unwrap().has(b"StructTreeRoot"));
    }
}
