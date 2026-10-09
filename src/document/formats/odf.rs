//! OpenDocument text and presentations. Port of prism/document/formats/odf.py.
//!
//! Prism runs odfpy's ODF2XHTML and parses the result. This walks content.xml
//! and emits the same HTML shapes ODF2XHTML does (headings, paragraphs, `ul`/`ol`
//! chosen from the list styles, tables, links, one `fieldset` per slide, footnotes
//! as a trailing `ol`), then reuses `parse_html`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;

use super::word::{
    PagedDoc, XNode, Xml, effective_page_size, file_name, paginate_with_elements, read_xml, toc_from_headings,
};
use crate::document::{DocMetadata, Document, OpenError, OpenOptions};
use crate::structured_text::html_parser::parse_html;

/// Subtrees ODF2XHTML skips entirely.
const IGNORED: &[&str] = &[
    "office:meta",
    "office:annotation",
    "office:forms",
    "office:scripts",
    "office:settings",
    "svg:desc",
    "presentation:notes",
    "table:covered-table-cell",
    "draw:layer-set",
    "draw:object",
    "draw:object-ole",
    "text:alphabetical-index-source",
    "text:bibliography-source",
    "text:illustration-index-source",
    "text:object-index-source",
    "text:table-index-source",
    "text:table-of-content-source",
    "text:user-index-source",
];

/// `text:list-style` definitions -> "ul" or "ol" per `<style name>_<level>`.
fn collect_list_types(node: &Xml, out: &mut HashMap<String, &'static str>) {
    if node.name == "text:list-style"
        && let Some(name) = node.attr("style:name")
    {
        for level in node.kids.iter().filter_map(|k| if let XNode::El(e) = k { Some(e) } else { None }) {
            let tag = match level.name.as_str() {
                "text:list-level-style-bullet" => "ul",
                "text:list-level-style-number" => "ol",
                _ => continue,
            };
            if let Some(l) = level.attr("text:level") {
                out.insert(format!("{name}_{l}"), tag);
            }
        }
    }
    for k in &node.kids {
        if let XNode::El(e) = k {
            collect_list_types(e, out);
        }
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

struct Walker {
    list_types: HashMap<String, &'static str>,
    notes: Vec<String>,
}

impl Walker {
    fn children(&mut self, e: &Xml, ancestors: &mut Vec<Xml>, in_block: bool, out: &mut String) {
        // Ancestors are only inspected for list level and style names, so keep attributes, not kids.
        ancestors.push(Xml { name: e.name.clone(), attrs: e.attrs.clone(), kids: Vec::new() });
        for k in &e.kids {
            match k {
                XNode::Text(t) if in_block || !t.trim().is_empty() => esc(t, out),
                XNode::Text(_) => {}
                XNode::El(c) => self.element(c, ancestors, in_block, out),
            }
        }
        ancestors.pop();
    }

    fn open_close(&mut self, tag: &str, attrs: &str, e: &Xml, ancestors: &mut Vec<Xml>, in_block: bool, out: &mut String) {
        out.push_str(&format!("<{tag}{attrs}>"));
        self.children(e, ancestors, in_block, out);
        out.push_str(&format!("</{tag}>"));
    }

    fn element(&mut self, e: &Xml, ancestors: &mut Vec<Xml>, in_block: bool, out: &mut String) {
        if IGNORED.contains(&e.name.as_str()) {
            return;
        }
        match e.name.as_str() {
            "text:h" => {
                let level = e.attr("text:outline-level").and_then(|l| l.parse::<i64>().ok()).unwrap_or(1).clamp(1, 6);
                self.open_close(&format!("h{level}"), "", e, ancestors, true, out);
            }
            "text:p" => {
                let tag = match e.attr("text:style-name").map(|s| s.replace('.', "_")).as_deref() {
                    Some("Heading_20_1") => "h1",
                    Some("Heading_20_2") => "h2",
                    Some("Heading_20_3") => "h3",
                    Some("Heading_20_4") => "h4",
                    Some("Heading_20_5") => "h5",
                    Some("Heading_20_6") => "h6",
                    Some("Addressee") => "address",
                    Some("Preformatted_20_Text") => "pre",
                    _ => "p",
                };
                self.open_close(tag, "", e, ancestors, true, out);
            }
            "text:span" => self.open_close("span", "", e, ancestors, true, out),
            "text:a" => {
                let href = e.attr("xlink:href").unwrap_or("").split('|').next().unwrap_or("");
                let mut attr = String::from(" href=\"");
                esc(href, &mut attr);
                attr.push('"');
                self.open_close("a", &attr, e, ancestors, true, out);
            }
            "text:bookmark-ref" => self.open_close("a", "", e, ancestors, true, out),
            "text:s" => {
                let n: usize = e.attr("text:c").and_then(|c| c.parse().ok()).unwrap_or(1);
                out.push_str(&"\u{a0}".repeat(n.min(10_000)));
            }
            "text:tab" => out.push(' '),
            "text:line-break" => out.push_str("<br />"),
            "text:list" => {
                let level = ancestors.iter().filter(|a| a.name == "text:list").count() + 1;
                let name = e
                    .attr("text:style-name")
                    .or_else(|| ancestors.iter().rev().find_map(|a| a.attr("text:style-name")))
                    .unwrap_or("None");
                let tag = self.list_types.get(&format!("{}_{level}", name.replace('.', "_"))).copied().unwrap_or("ul");
                self.open_close(tag, "", e, ancestors, in_block, out);
            }
            "text:list-item" => self.open_close("li", "", e, ancestors, in_block, out),
            "text:note" => {
                self.notes.push(String::new());
                let n = self.notes.len();
                out.push_str(&format!("<a href=\"#footnote-{n}\"><sup>{n}</sup></a>"));
                for body in e.children("text:note-body") {
                    let mut text = String::new();
                    self.children(body, ancestors, false, &mut text);
                    self.notes[n - 1] = text;
                }
            }
            "table:table" => self.open_close("table", "", e, ancestors, false, out),
            "table:table-column" => {
                let repeated: usize = e.attr("table:number-columns-repeated").and_then(|c| c.parse().ok()).unwrap_or(1);
                out.push_str(&"<col />".repeat(repeated.min(1024)));
            }
            "table:table-row" => self.open_close("tr", "", e, ancestors, false, out),
            "table:table-cell" => {
                let mut attrs = String::new();
                if let Some(r) = e.attr("table:number-rows-spanned").filter(|r| !r.is_empty()) {
                    attrs.push_str(&format!(" rowspan=\"{r}\""));
                }
                if let Some(c) = e.attr("table:number-columns-spanned").filter(|c| !c.is_empty()) {
                    attrs.push_str(&format!(" colspan=\"{c}\""));
                }
                self.open_close("td", &attrs, e, ancestors, false, out);
            }
            "draw:frame" | "draw:custom-shape" | "draw:text-box" => self.open_close("div", "", e, ancestors, false, out),
            "draw:image" => out.push_str("<img alt=\"\" />"),
            "draw:page" => {
                out.push_str("<fieldset><legend>");
                esc(e.attr("draw:name").unwrap_or("NoName"), out);
                out.push_str("</legend>");
                self.children(e, ancestors, false, out);
                out.push_str("</fieldset>");
            }
            _ => self.children(e, ancestors, in_block, out),
        }
    }
}

/// content.xml (plus the optional styles.xml for list styles) -> HTML.
fn odf_to_html(content: &Xml, styles: Option<&Xml>) -> String {
    let mut list_types = HashMap::new();
    if let Some(s) = styles {
        collect_list_types(s, &mut list_types);
    }
    collect_list_types(content, &mut list_types);

    let mut walker = Walker { list_types, notes: Vec::new() };
    let mut body = String::new();
    if let Some(b) = content.child("office:body") {
        walker.children(b, &mut Vec::new(), false, &mut body);
    }
    if !walker.notes.is_empty() {
        body.push_str("<ol>");
        for (i, note) in walker.notes.iter().enumerate() {
            body.push_str(&format!("<li id=\"footnote-{}\">{note}</li>", i + 1));
        }
        body.push_str("</ol>");
    }
    format!("<html><body>{body}</body></html>")
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| OpenError::Other(format!("{} is not a valid OpenDocument file: {e}", file_name(path))))?;
    let content = read_xml(&mut zip, "content.xml").context("The OpenDocument file has no readable content.xml.")?;
    let styles = read_xml(&mut zip, "styles.xml");

    let parsed = parse_html(&odf_to_html(&content, styles.as_ref()));
    let is_odp = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("odp"));
    let meta = DocMetadata {
        title: file_name(path),
        format: if is_odp { "ODP Presentation" } else { "ODT Document" }.into(),
        ..Default::default()
    };
    let toc = toc_from_headings(&parsed.elements);
    let pages = paginate_with_elements(parsed.text, parsed.elements, effective_page_size(opts));
    Ok(Box::new(PagedDoc::new(path, pages, toc, meta)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::formats::word::parse_xml;

    #[test]
    fn odf_walk_matches_odf2xhtml_shapes() {
        let ns = r##"xmlns:office="o" xmlns:text="t" xmlns:table="b" xmlns:draw="d" xmlns:xlink="x" xmlns:style="s""##;
        let content = parse_xml(
            format!(
                r##"<office:document-content {ns}>
<office:automatic-styles><text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style></office:automatic-styles>
<office:body><office:text>
<text:h text:outline-level="2">Intro<text:bookmark text:name="x"/></text:h>
<text:p>A <text:span>b</text:span>  c<text:s text:c="2"/>d <text:a xlink:href="#Sec 1|outline">jump</text:a><text:line-break/>e<text:note><text:note-citation>1</text:note-citation><text:note-body><text:p>Foot</text:p></text:note-body></text:note></text:p>
<text:list text:style-name="L1"><text:list-item><text:p>one</text:p></text:list-item></text:list>
<text:list><text:list-item><text:p>dot</text:p></text:list-item></text:list>
<table:table><table:table-column table:number-columns-repeated="2"/><table:table-row><table:table-cell table:number-columns-spanned="2"><text:p>c</text:p></table:table-cell><table:covered-table-cell><text:p>no</text:p></table:covered-table-cell></table:table-row></table:table>
<draw:page draw:name="Slide 1"><draw:frame><draw:text-box><text:p>Hi</text:p></draw:text-box></draw:frame></draw:page>
<office:annotation><text:p>hidden</text:p></office:annotation>
</office:text></office:body></office:document-content>"##
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(
            odf_to_html(&content, None),
            concat!(
                "<html><body><h2>Intro</h2>",
                "<p>A <span>b</span>  c\u{a0}\u{a0}d <a href=\"#Sec 1\">jump</a><br />e<a href=\"#footnote-1\"><sup>1</sup></a></p>",
                "<ol><li><p>one</p></li></ol>",
                "<ul><li><p>dot</p></li></ul>",
                "<table><col /><col /><tr><td colspan=\"2\"><p>c</p></td></tr></table>",
                "<fieldset><legend>Slide 1</legend><div><div><p>Hi</p></div></div></fieldset>",
                "<ol><li id=\"footnote-1\"><p>Foot</p></li></ol></body></html>",
            )
        );
    }
}
