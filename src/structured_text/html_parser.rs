//! Port of prism/structured_text/html_parser.py: HTML to plain text with
//! semantic element positions.
//!
//! Prism rendered text with inscriptis, then searched that text for each
//! element's content to guess its position. Here one DOM walk renders the text
//! and records every element's char offsets as it emits them, so positions are
//! exact. The text style follows inscriptis: block elements on their own
//! lines with a blank line around paragraphs and headings, `*` and `1.` list
//! markers, tables one row per line, `[text](url)` for links, `[alt]` for images.

use ego_tree::NodeRef;
use scraper::{Html, Node, node::Element};

use super::elements::{ElementMeta, ElementType, SemanticElement};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseResult {
    pub text: String,
    pub elements: Vec<SemanticElement>,
}

/// Converts HTML to plain text, recording semantic elements (headings, links,
/// tables, lists, quotes, figures, code blocks) at char positions in that text.
pub fn parse_html(html: &str) -> ParseResult {
    let doc = Html::parse_document(html);
    let mut r = Renderer::default();
    r.walk(doc.tree.root());
    r.finish()
}

fn is_html_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

fn collapse_ws(chars: &[char]) -> String {
    let s: String = chars.iter().collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Blank lines kept before and after a block element, or `None` for inline.
fn block_margins(name: &str) -> Option<(usize, usize)> {
    match name {
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "pre" | "figure" | "table" | "textarea" => Some((1, 1)),
        "div" | "section" | "article" | "aside" | "header" | "footer" | "nav" | "main" | "address" | "hgroup" | "figcaption"
        | "li" | "ul" | "ol" | "dl" | "dt" | "dd" | "tr" | "form" | "fieldset" | "legend" | "details" | "summary" | "center"
        | "caption" => Some((0, 0)),
        _ => None,
    }
}

fn tracked_type(name: &str, el: &Element) -> Option<(ElementType, ElementMeta)> {
    let mut meta = ElementMeta::default();
    let ty = match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = name.as_bytes()[1] - b'0';
            meta.level = Some(level);
            ElementType::heading(level)
        }
        // An anchor without href (a bookmark target) is not a link to navigate to.
        "a" => {
            meta.url = Some(el.attr("href")?.to_string());
            ElementType::Link
        }
        "table" => ElementType::Table,
        "ul" | "ol" => ElementType::List,
        "li" => ElementType::ListItem,
        "blockquote" => ElementType::Quote,
        "figure" => ElementType::Figure,
        "pre" => ElementType::CodeBlock,
        _ => return None,
    };
    Some((ty, meta))
}

fn is_hidden(el: &Element) -> bool {
    if el.attr("hidden").is_some() {
        return true;
    }
    el.attr("style").is_some_and(|s| s.to_ascii_lowercase().replace(' ', "").contains("display:none"))
}

struct Slot {
    ty: ElementType,
    meta: ElementMeta,
    start: Option<usize>,
    end: usize,
    /// Link target: shown as `[text](url)` around the link's text.
    href: Option<String>,
    text: Option<String>,
}

struct ListCtx {
    ordered: bool,
    next: usize,
}

/// A rendered table cell: flat text with its elements and markup ranges, all
/// relative to the cell's first char.
#[derive(Default)]
struct Cell {
    text: Vec<char>,
    elements: Vec<SemanticElement>,
    marks: Vec<(usize, usize)>,
}

#[derive(Default)]
struct Renderer {
    out: Vec<char>,
    last_end: usize,
    pending_nl: usize,
    pending_space: bool,
    line_open: bool,
    indent: usize,
    bullet: Option<String>,
    pre: bool,
    lists: Vec<ListCtx>,
    slots: Vec<Slot>,
    open: Vec<usize>,
    figure: Option<usize>,
    /// Link brackets and URLs: not part of an element's own text.
    marks: Vec<(usize, usize)>,
    /// Finished elements from table cells, already at final offsets.
    extra: Vec<SemanticElement>,
}

impl Renderer {
    fn walk(&mut self, node: NodeRef<Node>) {
        match node.value() {
            Node::Text(t) => self.text(t),
            Node::Element(el) => self.element(node, el),
            Node::Document | Node::Fragment => self.walk_children(node),
            _ => {}
        }
    }

    fn walk_children(&mut self, node: NodeRef<Node>) {
        for child in node.children() {
            self.walk(child);
        }
    }

    /// Emits `s` as content: flushes pending line breaks and indentation, opens
    /// elements that start here, and returns the char offset where `s` begins.
    fn write(&mut self, s: &str) -> usize {
        if !self.out.is_empty() && self.pending_nl > 0 {
            self.out.extend(std::iter::repeat_n('\n', self.pending_nl));
            self.line_open = false;
        }
        self.pending_nl = 0;
        if !self.line_open {
            let bullet = self.bullet.take();
            let pad = self.indent.saturating_sub(if bullet.is_some() { 4 } else { 0 });
            self.out.extend(std::iter::repeat_n(' ', pad));
            if let Some(b) = bullet {
                self.out.extend(b.chars());
            }
            self.line_open = true;
        } else if self.pending_space {
            self.out.push(' ');
        }
        self.pending_space = false;
        for i in 0..self.open.len() {
            let slot = self.open[i];
            if self.slots[slot].start.is_none() {
                if self.slots[slot].href.as_deref().is_some_and(|h| !h.is_empty()) {
                    self.marks.push((self.out.len(), self.out.len() + 1));
                    self.out.push('[');
                }
                self.slots[slot].start = Some(self.out.len());
            }
        }
        let at = self.out.len();
        self.out.extend(s.chars());
        self.last_end = self.out.len();
        at
    }

    fn text(&mut self, t: &str) {
        if self.pre {
            for (i, seg) in t.split('\n').enumerate() {
                if i > 0 {
                    self.pending_nl += 1;
                }
                if !seg.is_empty() {
                    self.write(seg);
                }
            }
            return;
        }
        if t.starts_with(is_html_ws) {
            self.pending_space = true;
        }
        let mut any = false;
        for word in t.split(is_html_ws).filter(|w| !w.is_empty()) {
            if any {
                self.pending_space = true;
            }
            self.write(word);
            any = true;
        }
        if any && t.ends_with(is_html_ws) {
            self.pending_space = true;
        }
    }

    fn open_slot(&mut self, ty: ElementType, meta: ElementMeta, href: Option<String>) -> usize {
        self.slots.push(Slot { ty, meta, start: None, end: 0, href, text: None });
        let idx = self.slots.len() - 1;
        self.open.push(idx);
        idx
    }

    fn close_slot(&mut self, idx: usize) {
        self.open.pop();
        if self.slots[idx].start.is_none() {
            return;
        }
        self.slots[idx].end = self.last_end;
        if let Some(href) = self.slots[idx].href.clone().filter(|h| !h.is_empty()) {
            let start = self.out.len();
            self.out.extend(format!("]({href})").chars());
            self.marks.push((start, self.out.len()));
            self.last_end = self.out.len();
        }
    }

    fn element(&mut self, node: NodeRef<Node>, el: &Element) {
        let name = el.name();
        if matches!(name, "script" | "style" | "head" | "title" | "template" | "noscript" | "iframe" | "svg") || is_hidden(el) {
            return;
        }
        match name {
            "br" => {
                self.pending_nl += 1;
                return;
            }
            "hr" => {
                self.pending_nl = self.pending_nl.max(2);
                return;
            }
            "img" => return self.image(el),
            "table" => return self.table(node),
            _ => {}
        }

        let margins = block_margins(name);
        if let Some((before, _)) = margins {
            self.pending_nl = self.pending_nl.max(before + 1);
        }
        match name {
            "ul" | "ol" => {
                let next = el.attr("start").and_then(|s| s.trim().parse().ok()).unwrap_or(1);
                self.lists.push(ListCtx { ordered: name == "ol", next });
                self.indent += 4;
            }
            "li" => {
                let depth = self.lists.len();
                let marker = match self.lists.last_mut() {
                    Some(l) if l.ordered => {
                        l.next += 1;
                        format!("{}. ", l.next - 1)
                    }
                    Some(_) => format!("{} ", ["*", "-", "+", "o"][(depth - 1).min(3)]),
                    None => "* ".to_string(),
                };
                self.bullet = Some(format!("{marker:>4}"));
            }
            "dd" => self.indent += 4,
            _ => {}
        }

        let slot = tracked_type(name, el).map(|(ty, meta)| {
            let href = meta.url.clone();
            self.open_slot(ty, meta, href)
        });
        let prev_figure = self.figure;
        if name == "figure" {
            self.figure = slot;
        }
        let was_pre = self.pre;
        if matches!(name, "pre" | "textarea") {
            self.pre = true;
        }

        self.walk_children(node);

        self.pre = was_pre;
        self.figure = prev_figure;
        if let Some(idx) = slot {
            self.close_slot(idx);
        }
        match name {
            "ul" | "ol" => {
                self.lists.pop();
                self.indent -= 4;
            }
            "li" => self.bullet = None,
            "dd" => self.indent -= 4,
            _ => {}
        }
        if let Some((_, after)) = margins {
            self.pending_nl = self.pending_nl.max(after + 1);
        }
    }

    fn image(&mut self, el: &Element) {
        let alt = el.attr("alt").unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
        if alt.is_empty() {
            return;
        }
        let shown = format!("[{alt}]");
        match self.figure {
            Some(fig) => {
                self.slots[fig].meta.alt.get_or_insert(alt);
                self.write(&shown);
            }
            None => {
                let meta = ElementMeta { alt: Some(alt.clone()), ..Default::default() };
                let idx = self.open_slot(ElementType::Figure, meta, None);
                self.slots[idx].text = Some(alt);
                self.write(&shown);
                self.close_slot(idx);
            }
        }
    }

    fn table(&mut self, node: NodeRef<Node>) {
        let mut rows: Vec<Vec<NodeRef<Node>>> = Vec::new();
        let mut caption = None;
        collect_rows(node, &mut rows, &mut caption);

        self.pending_nl = self.pending_nl.max(2);
        let slot = self.open_slot(ElementType::Table, ElementMeta::default(), None);
        if let Some(cap) = caption {
            self.pending_nl = self.pending_nl.max(1);
            self.walk_children(cap);
            self.pending_nl = self.pending_nl.max(1);
        }

        let cells: Vec<Vec<Cell>> = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|&td| {
                        let mut sub = Renderer::default();
                        sub.walk_children(td);
                        sub.into_cell()
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|row| row.iter().any(|c| !c.text.is_empty()))
            .collect();
        let mut widths: Vec<usize> = Vec::new();
        for row in &cells {
            for (i, c) in row.iter().enumerate() {
                if widths.len() <= i {
                    widths.push(0);
                }
                widths[i] = widths[i].max(c.text.len());
            }
        }

        for row in cells {
            let mut line: Vec<char> = Vec::new();
            let mut placed: Vec<(usize, Cell)> = Vec::new();
            for (i, cell) in row.into_iter().enumerate() {
                let at = line.len();
                line.extend(&cell.text);
                line.extend(std::iter::repeat_n(' ', widths[i] - cell.text.len() + 2));
                placed.push((at, cell));
            }
            while line.last() == Some(&' ') {
                line.pop();
            }
            self.pending_nl = self.pending_nl.max(1);
            let base = self.write(&line.iter().collect::<String>());
            for (at, cell) in placed {
                let shift = base + at;
                for mut e in cell.elements {
                    e.start_pos += shift;
                    e.end_pos += shift;
                    self.extra.push(e);
                }
                self.marks.extend(cell.marks.into_iter().map(|(a, b)| (a + shift, b + shift)));
            }
        }

        self.close_slot(slot);
        self.pending_nl = self.pending_nl.max(2);
    }

    /// Element text without link brackets and URLs, whitespace collapsed.
    fn plain_text(&self, start: usize, end: usize) -> String {
        let mut plain: Vec<char> = Vec::new();
        let mut pos = start;
        let first = self.marks.partition_point(|m| m.1 <= start);
        for &(a, b) in &self.marks[first..] {
            if a >= end {
                break;
            }
            plain.extend(&self.out[pos..a.max(pos)]);
            pos = pos.max(b);
        }
        if pos < end {
            plain.extend(&self.out[pos..end]);
        }
        collapse_ws(&plain)
    }

    fn elements(&mut self) -> Vec<SemanticElement> {
        let mut elements = Vec::new();
        for slot in &self.slots {
            let Some(start) = slot.start else { continue };
            let text = slot.text.clone().unwrap_or_else(|| self.plain_text(start, slot.end));
            if text.is_empty() {
                continue;
            }
            elements.push(SemanticElement { element_type: slot.ty, start_pos: start, end_pos: slot.end, text, meta: slot.meta.clone() });
        }
        elements.append(&mut self.extra);
        // Stable: outer elements, recorded first, stay ahead of inner ones at equal starts.
        elements.sort_by_key(|e| e.start_pos);
        elements
    }

    fn into_cell(mut self) -> Cell {
        let elements = self.elements();
        let text = self.out.iter().map(|&c| if c == '\n' { ' ' } else { c }).collect();
        Cell { text, elements, marks: self.marks }
    }

    fn finish(mut self) -> ParseResult {
        let elements = self.elements();
        ParseResult { text: self.out.iter().collect(), elements }
    }
}

/// Gathers `<tr>` rows (through thead/tbody/tfoot) and the caption of a table,
/// leaving nested tables to the cells that contain them.
fn collect_rows<'a>(table: NodeRef<'a, Node>, rows: &mut Vec<Vec<NodeRef<'a, Node>>>, caption: &mut Option<NodeRef<'a, Node>>) {
    for child in table.children() {
        let Node::Element(el) = child.value() else { continue };
        match el.name() {
            "tr" => rows.push(
                child.children().filter(|c| matches!(c.value(), Node::Element(e) if matches!(e.name(), "td" | "th"))).collect(),
            ),
            "thead" | "tbody" | "tfoot" => collect_rows(child, rows, caption),
            "caption" => *caption = Some(child),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(r: &ParseResult, ty: ElementType) -> Vec<(usize, usize, String)> {
        r.elements.iter().filter(|e| e.element_type == ty).map(|e| (e.start_pos, e.end_pos, e.text.clone())).collect()
    }

    fn slice(r: &ParseResult, e: &SemanticElement) -> String {
        r.text.chars().skip(e.start_pos).take(e.end_pos - e.start_pos).collect()
    }

    #[test]
    fn headings_paragraphs_and_links_get_exact_offsets() {
        let r = parse_html(
            "<h1>Title</h1><p>See <a href=\"http://x.org\">the <b>site</b></a>, then <a href=\"/b\">Title</a>.</p>",
        );
        assert_eq!(r.text, "Title\n\nSee [the site](http://x.org), then [Title](/b).");
        assert_eq!(find(&r, ElementType::Heading1), vec![(0, 5, "Title".to_string())]);
        assert_eq!(find(&r, ElementType::Link), vec![(12, 20, "the site".to_string()), (43, 48, "Title".to_string())]);
        assert_eq!(r.elements[0].meta.level, Some(1));
        assert_eq!(r.elements[1].meta.url.as_deref(), Some("http://x.org"));
        for e in &r.elements {
            assert_eq!(slice(&r, e).split_whitespace().collect::<Vec<_>>().join(" "), e.text);
        }
    }

    #[test]
    fn lists_are_marked_and_indented() {
        let r = parse_html("<ul><li>one</li><li>two<ul><li>deep</li></ul></li></ul><ol start=\"3\"><li>x</li><li>y</li></ol>");
        assert_eq!(r.text, "  * one\n  * two\n      - deep\n 3. x\n 4. y");
        let items = find(&r, ElementType::ListItem);
        assert_eq!(items[0], (4, 7, "one".to_string()));
        assert_eq!(items[3].2, "x");
        assert_eq!(find(&r, ElementType::List).len(), 3);
        assert_eq!(find(&r, ElementType::List)[0].0, 4);
    }

    #[test]
    fn tables_render_row_by_row_with_cell_links_positioned() {
        let r = parse_html(
            "<table><tr><th>Day</th><th>Open</th></tr><tr><td>Saturday</td><td><a href=\"/s\">ten</a></td></tr></table><p>after</p>",
        );
        assert_eq!(r.text, "Day       Open\nSaturday  [ten](/s)\n\nafter");
        assert_eq!(find(&r, ElementType::Table), vec![(0, 34, "Day Open Saturday ten".to_string())]);
        assert_eq!(find(&r, ElementType::Link), vec![(26, 29, "ten".to_string())]);
    }

    #[test]
    fn quote_figure_image_and_code_block() {
        let r = parse_html(
            "<blockquote><p>Wise words.</p></blockquote><figure><img src=\"a.png\" alt=\"A  cat\"><figcaption>Cat</figcaption></figure><pre>a\n  b\n</pre><img alt=\"solo\">",
        );
        assert_eq!(r.text, "Wise words.\n\n[A cat]\nCat\n\na\n  b\n\n[solo]");
        assert_eq!(find(&r, ElementType::Quote), vec![(0, 11, "Wise words.".to_string())]);
        assert_eq!(find(&r, ElementType::CodeBlock), vec![(26, 31, "a b".to_string())]);
        let figures = find(&r, ElementType::Figure);
        assert_eq!(figures, vec![(13, 24, "[A cat] Cat".to_string()), (33, 39, "solo".to_string())]);
        assert_eq!(r.elements[1].meta.alt.as_deref(), Some("A cat"));
    }

    #[test]
    fn skips_scripts_hidden_nodes_and_anchors_without_href() {
        let r = parse_html(
            "<head><title>t</title></head><body><script>x()</script><p hidden>no</p><p>a<br>b</p><a name=\"top\">Top</a></body>",
        );
        assert_eq!(r.text, "a\nb\n\nTop");
        assert!(find(&r, ElementType::Link).is_empty());
    }
}
