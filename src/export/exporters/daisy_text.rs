//! DAISY 3 DTBook (text only) exporter.
//!
//! Writes a folder: `dtbook.xml` (the DTBook), `ncc.html` (navigation control center) and
//! `package.opf`.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::base::{ExportOptions, Exporter, ProgressFn, write_file};
use crate::export::util::{check_cancel, escape_html as esc, flat_toc, list_items, or_default, page_blocks};
use crate::structured_text::elements::ElementType;

pub struct DaisyTextExporter;

/// Minimal XML tree, serialised like `minidom.toprettyxml(indent="  ")`.
#[derive(Debug, Clone)]
pub(crate) struct Xml {
    name: &'static str,
    attrs: Vec<(&'static str, String)>,
    children: Vec<XmlNode>,
}

#[derive(Debug, Clone)]
enum XmlNode {
    El(Xml),
    Text(String),
}

impl Xml {
    pub(crate) fn new(name: &'static str) -> Self {
        Self { name, attrs: Vec::new(), children: Vec::new() }
    }
    fn attr(mut self, k: &'static str, v: impl Into<String>) -> Self {
        self.attrs.push((k, v.into()));
        self
    }
    fn text(mut self, t: impl Into<String>) -> Self {
        self.children.push(XmlNode::Text(t.into()));
        self
    }
    fn push(&mut self, child: Xml) {
        self.children.push(XmlNode::El(child));
    }

    fn write(&self, out: &mut String, depth: usize) {
        let pad = "  ".repeat(depth);
        out.push_str(&format!("{pad}<{}", self.name));
        for (k, v) in &self.attrs {
            out.push_str(&format!(" {k}=\"{}\"", esc(v)));
        }
        if self.children.is_empty() {
            out.push_str("/>\n");
        } else if let [XmlNode::Text(t)] = self.children.as_slice() {
            out.push_str(&format!(">{}</{}>\n", esc(t), self.name));
        } else {
            out.push_str(">\n");
            for c in &self.children {
                match c {
                    XmlNode::El(e) => e.write(out, depth + 1),
                    XmlNode::Text(t) => out.push_str(&format!("{pad}  {}\n", esc(t))),
                }
            }
            out.push_str(&format!("{pad}</{}>\n", self.name));
        }
    }

    pub(crate) fn to_document(&self) -> String {
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
        self.write(&mut out, 0);
        out
    }
}

/// The files of a DAISY text package, built in memory.
pub(crate) struct DaisyText {
    pub dtbook: String,
    pub ncc: String,
    pub opf: String,
}

fn uid_for(title: &str) -> String {
    // FNV-1a, 32 bit: stable across runs, unlike Python's salted hash().
    let h = title.bytes().fold(0x811c9dc5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x01000193));
    format!("intome-daisy-{h:08x}")
}

fn attach(stack: &mut Vec<(u8, Xml)>, body: &mut Xml) {
    if let Some((_, done)) = stack.pop() {
        match stack.last_mut() {
            Some((_, parent)) => parent.push(done),
            None => body.push(done),
        }
    }
}

/// Builds the DTBook, NCC and OPF without touching the disk.
pub(crate) fn build(doc: &mut dyn Document, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<DaisyText> {
    let meta = doc.metadata();
    let title = or_default(meta.title, "Untitled");
    let author = or_default(meta.author, "Unknown");
    let page_count = doc.page_count();
    let uid = uid_for(&title);

    let mut head = Xml::new("head");
    for (name, content) in [("dtb:uid", uid.as_str()), ("dc:Title", &title), ("dc:Creator", &author), ("dc:Format", "ANSI/NISO Z39.86-2005")] {
        head.push(Xml::new("meta").attr("name", name).attr("content", content));
    }
    let mut front = Xml::new("frontmatter");
    front.push(Xml::new("doctitle").text(&title));
    front.push(Xml::new("docauthor").text(&author));

    // `stack` holds the open levelN elements. A heading closes every open level of equal or
    // deeper rank, then opens a level named by nesting depth, so the output nests as the
    // DTBook DTD requires even when the source skips heading levels.
    let mut body = Xml::new("bodymatter");
    let mut stack: Vec<(u8, Xml)> = Vec::new();
    // pagenum markers wait here until a level exists to hold them.
    let mut pending: Vec<Xml> = Vec::new();
    for i in 0..page_count {
        check_cancel(cancel)?;
        let blocks = page_blocks(doc.page(i));
        if stack.is_empty() && blocks.iter().all(|b| b.kind == ElementType::Paragraph) {
            let mut level = Xml::new("level1");
            level.push(Xml::new("h1").text(format!("Page {}", i + 1)));
            stack.push((1, level));
        }
        // Target for the NCC links.
        let pagenum = Xml::new("pagenum").attr("id", format!("page-{}", i + 1)).attr("page", "normal").text((i + 1).to_string());
        // It goes into whichever level receives the page's first block, so a page that opens
        // with a heading is marked inside that heading's level.
        pending.push(pagenum);
        for b in blocks {
            if let Some(n) = b.kind.heading_level() {
                while stack.last().is_some_and(|(l, _)| *l >= n) {
                    attach(&mut stack, &mut body);
                }
                let (level_tag, heading_tag) = LEVELS[(stack.len()).min(5)];
                let mut level = Xml::new(level_tag);
                level.push(Xml::new(heading_tag).text(&b.text));
                level.children.extend(pending.drain(..).map(XmlNode::El));
                stack.push((n, level));
                continue;
            }
            if stack.is_empty() {
                stack.push((1, Xml::new("level1")));
            }
            let cur = &mut stack.last_mut().expect("level opened above").1;
            cur.children.extend(pending.drain(..).map(XmlNode::El));
            match b.kind {
                ElementType::Quote => {
                    let mut bq = Xml::new("blockquote");
                    bq.push(Xml::new("p").text(&b.text));
                    cur.push(bq);
                }
                ElementType::List => {
                    let mut list = Xml::new("list").attr("type", "ul");
                    for item in list_items(&b.text) {
                        list.push(Xml::new("li").text(item));
                    }
                    cur.push(list);
                }
                _ => cur.push(Xml::new("p").text(&b.text)),
            }
        }
        progress(i + 1, page_count, &format!("Page {}", i + 1));
    }
    if let Some((_, cur)) = stack.last_mut() {
        cur.children.extend(pending.drain(..).map(XmlNode::El));
    }
    while !stack.is_empty() {
        attach(&mut stack, &mut body);
    }

    let mut book = Xml::new("book");
    book.push(front);
    book.push(body);
    let mut root = Xml::new("dtbook")
        .attr("xmlns", "http://www.daisy.org/z3986/2005/dtbook/")
        .attr("version", "2005-3")
        .attr("xml:lang", "en");
    root.push(head);
    root.push(book);

    Ok(DaisyText { dtbook: root.to_document(), ncc: ncc(doc, &title, &author, page_count), opf: opf(&title, &author, &uid) })
}

const LEVELS: [(&str, &str); 6] =
    [("level1", "h1"), ("level2", "h2"), ("level3", "h3"), ("level4", "h4"), ("level5", "h5"), ("level6", "h6")];

fn ncc(doc: &dyn Document, title: &str, author: &str, page_count: usize) -> String {
    let (t, a) = (esc(title), esc(author));
    let mut lines = vec![
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>".to_string(),
        "<!DOCTYPE html>".into(),
        "<html xmlns=\"http://www.w3.org/1999/xhtml\">".into(),
        "<head>".into(),
        format!("<title>{t}</title>"),
        format!("<meta name=\"dc:creator\" content=\"{a}\"/>"),
        format!("<meta name=\"dc:title\" content=\"{t}\"/>"),
        "<meta name=\"ncc:totalTime\" content=\"00:00:00\"/>".into(),
        "<meta name=\"ncc:depth\" content=\"1\"/>".into(),
        format!("<meta name=\"ncc:maxPageNormal\" content=\"{page_count}\"/>"),
        "</head>".into(),
        "<body>".into(),
    ];
    let toc = flat_toc(doc.toc());
    if toc.is_empty() {
        lines.push(format!("<h1 class=\"title\"><a href=\"dtbook.xml\">{t}</a></h1>"));
    }
    for e in toc {
        let h = (e.level + 1).min(6);
        lines.push(format!("<h{h} class=\"section\"><a href=\"dtbook.xml#page-{}\">{}</a></h{h}>", e.page_index + 1, esc(&e.title)));
    }
    lines.push("</body>".into());
    lines.push("</html>".into());
    lines.join("\n")
}

fn opf(title: &str, author: &str, uid: &str) -> String {
    [
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>".to_string(),
        "<package xmlns=\"http://openebook.org/namespaces/oeb-package/1.0/\" unique-identifier=\"uid\">".into(),
        "<metadata>".into(),
        "  <dc-metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">".into(),
        format!("    <dc:Title>{}</dc:Title>", esc(title)),
        format!("    <dc:Creator>{}</dc:Creator>", esc(author)),
        format!("    <dc:Identifier id=\"uid\">{uid}</dc:Identifier>"),
        "    <dc:Format>ANSI/NISO Z39.86-2005</dc:Format>".into(),
        "    <dc:Language>en</dc:Language>".into(),
        "  </dc-metadata>".into(),
        "</metadata>".into(),
        "<manifest>".into(),
        "  <item id=\"dtbook\" href=\"dtbook.xml\" media-type=\"application/x-dtbook+xml\"/>".into(),
        "  <item id=\"ncc\" href=\"ncc.html\" media-type=\"application/xhtml+xml\"/>".into(),
        "</manifest>".into(),
        "<spine>".into(),
        "  <itemref idref=\"dtbook\"/>".into(),
        "</spine>".into(),
        "</package>".into(),
    ]
    .join("\n")
}

/// The output folder for a DAISY export: `output_path`, or its parent if it names an `.xml` file.
pub(crate) fn output_dir(output_path: &Path) -> PathBuf {
    if output_path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xml")) {
        output_path.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        output_path.to_path_buf()
    }
}

pub(crate) fn prepare_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))
}

pub(crate) fn write_package(dir: &Path, files: &DaisyText) -> anyhow::Result<PathBuf> {
    let dtbook = dir.join("dtbook.xml");
    write_file(&dtbook, &files.dtbook)?;
    write_file(&dir.join("ncc.html"), &files.ncc)?;
    write_file(&dir.join("package.opf"), &files.opf)?;
    Ok(dtbook)
}

impl Exporter for DaisyTextExporter {
    fn format_name(&self) -> &'static str {
        "DAISY (Text)"
    }
    fn format_key(&self) -> &'static str {
        "daisy_text"
    }
    fn file_extension(&self) -> &'static str {
        ".xml"
    }

    fn export(&self, doc: &mut dyn Document, options: &ExportOptions, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<PathBuf> {
        let dir = output_dir(&options.output_path);
        prepare_dir(&dir)?;
        let files = build(doc, cancel, progress)?;
        write_package(&dir, &files)
    }
}
