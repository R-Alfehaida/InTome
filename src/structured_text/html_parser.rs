//! STUB: port of prism/structured_text/html_parser.py. The signature is the contract.

use super::elements::SemanticElement;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParseResult {
    pub text: String,
    pub elements: Vec<SemanticElement>,
}

/// Converts HTML to plain text, recording semantic elements (headings, links,
/// tables, lists, quotes, figures, code blocks) at char positions in that text.
pub fn parse_html(html: &str) -> ParseResult {
    ParseResult { text: html.to_string(), elements: Vec::new() }
}
