//! Cross-page text search with match tracking. Port of prism/navigation/search.py.
//!
//! Prism's `model` argument (the GUI's paginated text) is replaced by the
//! [`Document`] itself, which already exposes page text.

use crate::document::Document;

/// A single search hit with page context. Positions are char indices.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchMatch {
    pub page_index: usize,
    pub start_pos: usize,
    pub end_pos: usize,
    /// About 40 chars on each side of the match.
    pub context: String,
}

#[derive(Debug, Default)]
pub struct SearchEngine {
    matches: Vec<SearchMatch>,
    current: Option<usize>,
    wrap: bool,
}

/// One lowercase char per char, so indices in folded text equal indices in the original.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

impl SearchEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Searches every page and returns the total match count. Overlapping
    /// matches are all reported. An empty query matches nothing.
    pub fn search(&mut self, query: &str, doc: &mut dyn Document, case_sensitive: bool, wrap: bool) -> usize {
        self.clear();
        self.wrap = wrap;
        if query.is_empty() {
            return 0;
        }
        let needle: Vec<char> = query.chars().map(|c| if case_sensitive { c } else { fold(c) }).collect();
        for page_index in 0..doc.page_count() {
            let page: Vec<char> = doc.page(page_index).text.chars().collect();
            if page.len() < needle.len() {
                continue;
            }
            let haystack: Vec<char> = if case_sensitive { page.clone() } else { page.iter().map(|&c| fold(c)).collect() };
            for pos in 0..=haystack.len() - needle.len() {
                if haystack[pos..pos + needle.len()] == needle[..] {
                    let from = pos.saturating_sub(40);
                    let to = (pos + needle.len() + 40).min(page.len());
                    self.matches.push(SearchMatch {
                        page_index,
                        start_pos: pos,
                        end_pos: pos + needle.len(),
                        context: page[from..to].iter().collect(),
                    });
                }
            }
        }
        self.matches.len()
    }

    /// First match after (`current_page`, `current_pos`), wrapping to the first when enabled.
    pub fn next_match(&mut self, current_page: usize, current_pos: usize) -> Option<SearchMatch> {
        let found = self.matches.iter().position(|m| (m.page_index, m.start_pos) > (current_page, current_pos));
        self.settle(found, 0)
    }

    /// Last match before (`current_page`, `current_pos`), wrapping to the last when enabled.
    pub fn prev_match(&mut self, current_page: usize, current_pos: usize) -> Option<SearchMatch> {
        let found = self.matches.iter().rposition(|m| (m.page_index, m.start_pos) < (current_page, current_pos));
        self.settle(found, self.matches.len().wrapping_sub(1))
    }

    fn settle(&mut self, found: Option<usize>, wrap_to: usize) -> Option<SearchMatch> {
        let index = found.or((self.wrap && !self.matches.is_empty()).then_some(wrap_to))?;
        self.current = Some(index);
        Some(self.matches[index].clone())
    }

    /// `(current match, 1-based; total matches)`. The current match is 0 before navigating.
    pub fn match_info(&self) -> (usize, usize) {
        (self.current.map_or(0, |i| i + 1), self.matches.len())
    }

    pub fn clear(&mut self) {
        self.matches.clear();
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::document::Page;
    use crate::document::utils::PagedDocument;

    fn doc() -> PagedDocument {
        let pages = ["Hello world, hello.", "nothing here", "HELLO again"].map(Page::plain).to_vec();
        PagedDocument::new(Path::new("t.txt"), "Test", "t".into(), pages, vec![])
    }

    fn pos(m: Option<SearchMatch>) -> Option<(usize, usize)> {
        m.map(|m| (m.page_index, m.start_pos))
    }

    #[test]
    fn counts_matches_case_insensitively_with_char_positions_and_context() {
        let mut s = SearchEngine::new();
        assert_eq!(s.search("hello", &mut doc(), false, true), 3);
        let first = s.next_match(0, 0).unwrap();
        assert_eq!((first.page_index, first.start_pos, first.end_pos), (0, 13, 18));
        assert_eq!(first.context, "Hello world, hello.");
        assert_eq!(s.search("hello", &mut doc(), true, true), 1);
        assert_eq!(s.search("", &mut doc(), true, true), 0);
        assert_eq!(s.search("aa", &mut PagedDocument::new(Path::new("t"), "T", "t".into(), vec![Page::plain("aaaa")], vec![]), true, true), 3);
    }

    #[test]
    fn next_and_prev_walk_matches_and_wrap_only_when_enabled() {
        let mut s = SearchEngine::new();
        s.search("hello", &mut doc(), false, true);
        assert_eq!(pos(s.next_match(0, 0)), Some((0, 13)));
        assert_eq!(pos(s.next_match(0, 13)), Some((2, 0)));
        assert_eq!(s.match_info(), (3, 3));
        assert_eq!(pos(s.next_match(2, 0)), Some((0, 0)));
        assert_eq!(pos(s.prev_match(0, 0)), Some((2, 0)));
        assert_eq!(pos(s.prev_match(2, 0)), Some((0, 13)));

        s.search("hello", &mut doc(), false, false);
        assert_eq!(s.match_info(), (0, 3));
        assert_eq!(s.next_match(2, 0), None);
        assert_eq!(s.prev_match(0, 0), None);
    }
}
