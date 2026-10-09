//! Element navigation with cross-page traversal. Port of
//! prism/navigation/element_nav.py.
//!
//! Prism's `ElementNavigator` held the bound document or text model. Here the
//! document is passed in, so every function is free of state. Prism's
//! `set_current_page` was never read and is dropped.

use crate::document::{Document, TocEntry};
use crate::structured_text::elements::{ElementType, SemanticElement};

/// Result of a navigation lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct NavResult {
    pub page_index: usize,
    pub element: SemanticElement,
    pub page_changed: bool,
}

/// An element with its page, for the Element List dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct DocElement {
    pub page_index: usize,
    pub element: SemanticElement,
}

/// The matching element on `page_index` that `better` ranks first. Ties keep the
/// earlier element in page order, as Prism's stable sort does.
fn pick(
    doc: &mut dyn Document,
    page_index: usize,
    types: &[ElementType],
    keep: impl Fn(&SemanticElement) -> bool,
    better: impl Fn(&SemanticElement, &SemanticElement) -> bool,
) -> Option<SemanticElement> {
    let best = doc.page(page_index).elements.iter().filter(|e| types.contains(&e.element_type) && keep(e)).fold(
        None,
        |best: Option<&SemanticElement>, e| match best {
            Some(b) if !better(e, b) => Some(b),
            _ => Some(e),
        },
    );
    best.cloned()
}

fn find(doc: &mut dyn Document, types: &[ElementType], from_pos: usize, page_index: usize, forward: bool) -> Option<NavResult> {
    let page_count = doc.page_count();
    if page_count == 0 || page_index >= page_count {
        return None;
    }
    let better = |a: &SemanticElement, b: &SemanticElement| if forward { a.start_pos < b.start_pos } else { a.start_pos > b.start_pos };
    let on_this_page = pick(doc, page_index, types, |e| if forward { e.start_pos > from_pos } else { e.start_pos < from_pos }, better);
    if let Some(element) = on_this_page {
        return Some(NavResult { page_index, element, page_changed: false });
    }
    // Then the other pages in reading direction, wrapping around the document.
    (1..page_count).find_map(|offset| {
        let page = if forward { (page_index + offset) % page_count } else { (page_index + page_count - offset) % page_count };
        pick(doc, page, types, |_| true, better).map(|element| NavResult { page_index: page, element, page_changed: true })
    })
}

/// Next element of any of `types` after `from_pos` on `page_index`, else the
/// first one on the following pages (wrapping past the last page).
pub fn find_next(doc: &mut dyn Document, types: &[ElementType], from_pos: usize, page_index: usize) -> Option<NavResult> {
    find(doc, types, from_pos, page_index, true)
}

/// Previous element of any of `types` before `from_pos` on `page_index`, else the
/// last one on the preceding pages (wrapping past the first page).
pub fn find_prev(doc: &mut dyn Document, types: &[ElementType], from_pos: usize, page_index: usize) -> Option<NavResult> {
    find(doc, types, from_pos, page_index, false)
}

/// Every element across all pages in page order, optionally filtered by type.
/// `None` or an empty filter means all elements.
pub fn get_all_elements(doc: &mut dyn Document, types: Option<&[ElementType]>) -> Vec<DocElement> {
    let filter = types.filter(|t| !t.is_empty());
    let mut result = Vec::new();
    for page_index in 0..doc.page_count() {
        for e in &doc.page(page_index).elements {
            if filter.is_none_or(|t| t.contains(&e.element_type)) {
                result.push(DocElement { page_index, element: e.clone() });
            }
        }
    }
    result
}

/// The nested TOC flattened depth-first and sorted by page (stable).
pub fn get_toc_entries_flat(doc: &dyn Document) -> Vec<TocEntry> {
    fn flatten(entries: &[TocEntry], out: &mut Vec<TocEntry>) {
        for e in entries {
            out.push(e.clone());
            flatten(&e.children, out);
        }
    }
    let mut out = Vec::new();
    flatten(doc.toc(), &mut out);
    out.sort_by_key(|e| e.page_index);
    out
}

/// First and last page of the TOC section containing `page_index`, or `None`
/// when the document has no TOC.
pub fn find_section_bounds(doc: &dyn Document, page_index: usize) -> Option<(usize, usize)> {
    let flat = get_toc_entries_flat(doc);
    if flat.is_empty() {
        return None;
    }
    let mut start = 0;
    for e in &flat {
        if e.page_index <= page_index {
            start = e.page_index;
        } else {
            break;
        }
    }
    let end = flat.iter().find(|e| e.page_index > start).map_or(doc.page_count().saturating_sub(1), |e| e.page_index - 1);
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::document::Page;
    use crate::document::utils::PagedDocument;
    use crate::structured_text::elements::ElementMeta;

    fn el(ty: ElementType, start: usize) -> SemanticElement {
        SemanticElement { element_type: ty, start_pos: start, end_pos: start + 3, text: format!("{ty:?}@{start}"), meta: ElementMeta::default() }
    }

    fn doc() -> PagedDocument {
        let page = |elements| Page { text: "x".repeat(100), elements };
        let pages = vec![
            page(vec![el(ElementType::Heading1, 0), el(ElementType::Link, 20), el(ElementType::Heading2, 50)]),
            page(vec![]),
            page(vec![el(ElementType::Heading3, 10), el(ElementType::Heading1, 70)]),
        ];
        let toc = |title: &str, page_index, children| TocEntry { title: title.into(), page_index, level: 0, children };
        let toc = vec![toc("B", 2, vec![]), toc("A", 0, vec![toc("A.1", 1, vec![])])];
        PagedDocument::new(Path::new("t.txt"), "Test", "t".into(), pages, toc)
    }

    const HEADINGS: [ElementType; 3] = [ElementType::Heading1, ElementType::Heading2, ElementType::Heading3];

    #[test]
    fn find_next_stays_on_the_page_then_crosses_pages_and_wraps() {
        let mut d = doc();
        let r = find_next(&mut d, &HEADINGS, 0, 0).unwrap();
        assert_eq!((r.page_index, r.element.start_pos, r.page_changed), (0, 50, false));
        let r = find_next(&mut d, &HEADINGS, 50, 0).unwrap();
        assert_eq!((r.page_index, r.element.start_pos, r.page_changed), (2, 10, true));
        let r = find_next(&mut d, &HEADINGS, 70, 2).unwrap();
        assert_eq!((r.page_index, r.element.start_pos, r.page_changed), (0, 0, true));
        assert_eq!(find_next(&mut d, &[ElementType::Table], 0, 0), None);
    }

    #[test]
    fn find_prev_goes_backwards_across_pages_and_wraps() {
        let mut d = doc();
        let r = find_prev(&mut d, &HEADINGS, 60, 2).unwrap();
        assert_eq!((r.page_index, r.element.start_pos, r.page_changed), (2, 10, false));
        let r = find_prev(&mut d, &HEADINGS, 10, 2).unwrap();
        assert_eq!((r.page_index, r.element.start_pos, r.page_changed), (0, 50, true));
        let r = find_prev(&mut d, &HEADINGS, 0, 0).unwrap();
        assert_eq!((r.page_index, r.element.start_pos, r.page_changed), (2, 70, true));
    }

    #[test]
    fn get_all_elements_filters_by_type_and_keeps_page_order() {
        let mut d = doc();
        assert_eq!(get_all_elements(&mut d, None).len(), 5);
        let links = get_all_elements(&mut d, Some(&[ElementType::Link]));
        assert_eq!(links.iter().map(|e| (e.page_index, e.element.start_pos)).collect::<Vec<_>>(), vec![(0, 20)]);
        let h1 = get_all_elements(&mut d, Some(&[ElementType::Heading1]));
        assert_eq!(h1.iter().map(|e| (e.page_index, e.element.start_pos)).collect::<Vec<_>>(), vec![(0, 0), (2, 70)]);
    }

    #[test]
    fn toc_flattens_sorted_by_page_and_section_bounds_follow_it() {
        let d = doc();
        assert_eq!(get_toc_entries_flat(&d).iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), vec!["A", "A.1", "B"]);
        assert_eq!(find_section_bounds(&d, 0), Some((0, 0)));
        assert_eq!(find_section_bounds(&d, 1), Some((1, 1)));
        assert_eq!(find_section_bounds(&d, 2), Some((2, 2)));
        let no_toc = PagedDocument::new(Path::new("t.txt"), "Test", "t".into(), vec![], vec![]);
        assert_eq!(find_section_bounds(&no_toc, 0), None);
    }
}
