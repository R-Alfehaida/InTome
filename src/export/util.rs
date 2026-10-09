//! Helpers shared by the exporters.

use crate::cancel::CancelToken;
use crate::document::{Page, TocEntry};
use crate::export::base::Cancelled;
use crate::structured_text::elements::ElementType;

/// Python's `html.escape(s, quote=True)`.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

pub fn check_cancel(cancel: &CancelToken) -> anyhow::Result<()> {
    if cancel.is_cancelled() { Err(Cancelled.into()) } else { Ok(()) }
}

/// TOC entries in document order, children after their parent.
pub fn flat_toc(entries: &[TocEntry]) -> Vec<&TocEntry> {
    fn walk<'a>(entries: &'a [TocEntry], out: &mut Vec<&'a TocEntry>) {
        for e in entries {
            out.push(e);
            walk(&e.children, out);
        }
    }
    let mut out = Vec::new();
    walk(entries, &mut out);
    out
}

/// One renderable unit of a page: a semantic element, or the plain text between elements.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: ElementType,
    /// Trimmed.
    pub text: String,
    pub url: Option<String>,
    pub alt: Option<String>,
}

/// Interleaves the text between semantic elements (as `Paragraph` blocks) with the
/// elements, in `start_pos` order. Without elements the page splits on blank lines.
/// Empty blocks are dropped.
pub fn page_blocks(page: &Page) -> Vec<Block> {
    let para = |text: &str| Block { kind: ElementType::Paragraph, text: text.trim().to_string(), url: None, alt: None };
    let mut out = Vec::new();
    if page.elements.is_empty() {
        out.extend(page.text.split("\n\n").map(para));
    } else {
        let chars: Vec<char> = page.text.chars().collect();
        let slice = |a: usize, b: usize| -> String {
            let (a, b) = (a.min(chars.len()), b.min(chars.len()));
            if a < b { chars[a..b].iter().collect() } else { String::new() }
        };
        let mut elems: Vec<_> = page.elements.iter().collect();
        elems.sort_by_key(|e| e.start_pos); // stable, like Python's sorted()
        let mut last_end = 0;
        for e in elems {
            out.push(para(&slice(last_end, e.start_pos)));
            out.push(Block {
                kind: e.element_type,
                text: e.text.trim().to_string(),
                url: e.meta.url.clone(),
                alt: e.meta.alt.clone(),
            });
            last_end = e.end_pos;
        }
        out.push(para(&slice(last_end, usize::MAX)));
    }
    out.retain(|b| !b.text.is_empty() || b.alt.is_some());
    out
}

/// Python's `line.strip().lstrip("•-* ")` over each line of a list element.
pub fn list_items(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| l.trim().trim_start_matches(['•', '-', '*', ' ']).to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

pub fn or_default(value: String, default: &str) -> String {
    if value.is_empty() { default.to_string() } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured_text::elements::{ElementMeta, SemanticElement};

    #[test]
    fn blocks_interleave_gap_text_and_elements() {
        let page = Page {
            text: "Intro\nTitle\nBody tail".into(),
            elements: vec![SemanticElement {
                element_type: ElementType::Heading2,
                start_pos: 6,
                end_pos: 11,
                text: "Title".into(),
                meta: ElementMeta::default(),
            }],
        };
        let kinds: Vec<_> = page_blocks(&page).into_iter().map(|b| (b.kind, b.text)).collect();
        assert_eq!(
            kinds,
            vec![
                (ElementType::Paragraph, "Intro".to_string()),
                (ElementType::Heading2, "Title".to_string()),
                (ElementType::Paragraph, "Body tail".to_string()),
            ]
        );
    }

    #[test]
    fn list_items_strip_bullets() {
        assert_eq!(list_items("• one\n- two\n\n  * three"), vec!["one", "two", "three"]);
    }

    #[test]
    fn escape_matches_python() {
        assert_eq!(escape_html(r#"<a href="x">it's & co</a>"#), "&lt;a href=&quot;x&quot;&gt;it&#x27;s &amp; co&lt;/a&gt;");
    }
}
