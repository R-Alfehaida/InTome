//! Parse OCR response markup tags into semantic elements (port of prism/ai/structure.py).
//!
//! OCR providers return text with custom tags like `[H1]...[/H1]` and
//! `[TABLE]...[/TABLE]`. This strips the tags to plain text and builds
//! [`SemanticElement`]s with char offsets into that text.

use std::sync::LazyLock;

use regex::Regex;

use crate::structured_text::elements::{ElementMeta, ElementType, SemanticElement};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedOcr {
    pub text: String,
    pub elements: Vec<SemanticElement>,
}

fn tag_type(tag: &str) -> Option<ElementType> {
    Some(match tag {
        "H1" => ElementType::Heading1,
        "H2" => ElementType::Heading2,
        "H3" => ElementType::Heading3,
        "H4" => ElementType::Heading4,
        "H5" => ElementType::Heading5,
        "H6" => ElementType::Heading6,
        "TABLE" => ElementType::Table,
        "LIST" => ElementType::List,
        "ITEM" => ElementType::ListItem,
        "LINK" => ElementType::Link,
        "FIGURE" => ElementType::Figure,
        "QUOTE" => ElementType::Quote,
        _ => return None,
    })
}

// Matches [TAG], [TAG attr="value"], [/TAG]. Same alternation order as Prism, so
// e.g. "[SPEAKER: x]" (colon, no space) is left in the text exactly as Prism does.
static TAG_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[(/?)(H1|H2|H3|H4|H5|H6|TABLE|LIST|ITEM|LINK|FIGURE|QUOTE|ROW|CELL|UNCLEAR|SPEAKER|SCENE|SFX|PAGE)(?:\s+([^\]]*))?\]").unwrap()
});

static ATTR_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(\w+)="([^"]*)""#).unwrap());

/// Strips the tags and returns plain text plus elements sorted by `start_pos`.
pub fn parse_ocr_response(raw_text: &str) -> ParsedOcr {
    let mut text = String::with_capacity(raw_text.len());
    let mut elements = Vec::new();
    // (tag, start char index, start byte index, meta)
    let mut open_stack: Vec<(String, usize, usize, ElementMeta)> = Vec::new();
    let mut current_pos = 0usize;
    let mut last_end = 0usize;

    for caps in TAG_PATTERN.captures_iter(raw_text) {
        let whole = caps.get(0).unwrap();
        let between = &raw_text[last_end..whole.start()];
        text.push_str(between);
        current_pos += between.chars().count();
        last_end = whole.end();

        let tag = caps[2].to_ascii_uppercase();
        if &caps[1] == "/" {
            if let Some(i) = open_stack.iter().rposition(|(t, ..)| *t == tag) {
                let (_, start_pos, start_byte, meta) = open_stack.remove(i);
                if let Some(element_type) = tag_type(&tag) {
                    elements.push(SemanticElement {
                        element_type,
                        start_pos,
                        end_pos: current_pos,
                        text: text[start_byte..].to_string(),
                        meta,
                    });
                }
            }
        } else if tag_type(&tag).is_some() {
            // Non-semantic tags (ROW, CELL, UNCLEAR, SPEAKER, SCENE, SFX, PAGE) are
            // stripped but not tracked as navigable elements.
            let meta = parse_attrs(caps.get(3).map_or("", |m| m.as_str()));
            open_stack.push((tag, current_pos, text.len(), meta));
        }
    }
    text.push_str(&raw_text[last_end..]);
    elements.sort_by_key(|e| e.start_pos);
    ParsedOcr { text, elements }
}

/// `url="..."` and `alt="..."` attributes; other keys have no home in [`ElementMeta`].
fn parse_attrs(attrs: &str) -> ElementMeta {
    let mut meta = ElementMeta::default();
    for c in ATTR_PATTERN.captures_iter(attrs) {
        match &c[1] {
            "url" => meta.url = Some(c[2].to_string()),
            "alt" => meta.alt = Some(c[2].to_string()),
            _ => {}
        }
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(t: ElementType, start: usize, end: usize, text: &str) -> SemanticElement {
        SemanticElement { element_type: t, start_pos: start, end_pos: end, text: text.into(), meta: ElementMeta::default() }
    }

    #[test]
    fn strips_tags_and_records_char_offsets() {
        let p = parse_ocr_response("[H1]Café menu[/H1]\n[LIST][ITEM]Tea[/ITEM][item]Ünd[/item][/LIST]");
        assert_eq!(p.text, "Café menu\nTeaÜnd");
        assert_eq!(
            p.elements,
            vec![
                el(ElementType::Heading1, 0, 9, "Café menu"),
                el(ElementType::ListItem, 10, 13, "Tea"),
                el(ElementType::List, 10, 16, "TeaÜnd"),
                el(ElementType::ListItem, 13, 16, "Ünd"),
            ]
        );
    }

    #[test]
    fn link_attrs_table_cells_and_unmatched_tags() {
        let p = parse_ocr_response(
            "See [LINK url=\"https://x.org\"]x[/LINK]. [TABLE][ROW][CELL]a[/CELL][CELL]b[/CELL][/ROW][/TABLE][UNCLEAR]?[/UNCLEAR][/QUOTE][FIGURE alt=\"cat\"] [SPEAKER: Bo] hi",
        );
        assert_eq!(p.text, "See x. ab? [SPEAKER: Bo] hi");
        let link = &p.elements[0];
        assert_eq!((link.element_type, link.start_pos, link.end_pos, link.meta.url.as_deref()), (ElementType::Link, 4, 5, Some("https://x.org")));
        assert_eq!(p.elements[1], el(ElementType::Table, 7, 9, "ab"));
        assert_eq!(p.elements.len(), 2, "unclosed FIGURE and stray [/QUOTE] make no element");
    }
}
