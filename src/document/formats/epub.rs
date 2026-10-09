//! EPUB ebooks. Port of prism/document/formats/epub.py.
//!
//! The `epub` crate reads the container, manifest, spine, metadata and NCX.
//! It does not read the EPUB 3 navigation document, so that is parsed here with
//! the same precedence ebooklib uses: NCX first, nav only when the NCX gave nothing.

use std::collections::HashMap;
use std::path::Path;

use epub::doc::{EpubDoc, NavPoint};
use scraper::{ElementRef, Html, Selector};

use super::word::{PagedDoc, effective_page_size, file_name, join_zip_path, paginate_with_elements, zip_dir};
use crate::document::{DocMetadata, Document, OpenError, OpenOptions, Page, TocEntry};
use crate::structured_text::html_parser::parse_html;

/// A TOC entry before its href is mapped to a page.
#[derive(Debug, PartialEq)]
struct RawToc {
    title: String,
    href: String,
    children: Vec<RawToc>,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Archive-relative, forward-slash, percent-decoded and without a `#fragment`:
/// the form chapter names are stored in.
fn page_key(href: &str) -> String {
    let no_fragment = href.split('#').next().unwrap_or("");
    join_zip_path("", &percent_decode(&no_fragment.replace('\\', "/")))
}

fn ncx_to_raw(points: &[NavPoint]) -> Vec<RawToc> {
    points
        .iter()
        .map(|p| RawToc { title: p.label.clone(), href: p.content.to_string_lossy().into_owned(), children: ncx_to_raw(&p.children) })
        .collect()
}

fn text_of(e: ElementRef) -> String {
    e.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn child_el<'a>(e: ElementRef<'a>, name: &str) -> Option<ElementRef<'a>> {
    e.children().filter_map(ElementRef::wrap).find(|c| c.value().name() == name)
}

fn parse_nav_list(ol: ElementRef, nav_dir: &str) -> Vec<RawToc> {
    let mut items = Vec::new();
    for li in ol.children().filter_map(ElementRef::wrap).filter(|c| c.value().name() == "li") {
        let link = child_el(li, "a");
        let href = link
            .and_then(|a| a.value().attr("href"))
            .filter(|h| !h.is_empty())
            .map(|h| join_zip_path(nav_dir, h));
        if let Some(sub) = child_el(li, "ol") {
            let first = li.children().filter_map(ElementRef::wrap).next();
            items.push(RawToc {
                title: first.map(text_of).unwrap_or_default(),
                href: href.unwrap_or_default(),
                children: parse_nav_list(sub, nav_dir),
            });
        } else if let (Some(a), Some(href)) = (link, href) {
            items.push(RawToc { title: text_of(a), href, children: Vec::new() });
        }
    }
    items
}

/// The `<nav epub:type="toc">` of an EPUB 3 navigation document.
fn parse_nav(html: &str, nav_dir: &str) -> Vec<RawToc> {
    let doc = Html::parse_document(html);
    let Ok(sel) = Selector::parse("nav") else { return Vec::new() };
    let nav = doc.select(&sel).find(|n| n.value().attrs().any(|(_, v)| v == "toc"));
    nav.and_then(|n| child_el(n, "ol")).map(|ol| parse_nav_list(ol, nav_dir)).unwrap_or_default()
}

fn to_entries(raw: &[RawToc], level: usize, pages: &HashMap<String, usize>) -> Vec<TocEntry> {
    raw.iter()
        .map(|r| TocEntry {
            title: if r.title.is_empty() { "Untitled".into() } else { r.title.clone() },
            // Unknown targets (and entries with no href) start at page 0.
            page_index: if r.href.is_empty() { 0 } else { pages.get(&page_key(&r.href)).copied().unwrap_or(0) },
            level,
            children: to_entries(&r.children, level + 1, pages),
        })
        .collect()
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let mut book = EpubDoc::new(path).map_err(|e| OpenError::Other(format!("Could not open {} as an EPUB: {e}", file_name(path))))?;
    let page_size = effective_page_size(opts);

    let value_of = |prop: &str| book.mdata(prop).map(|m| m.value.clone()).unwrap_or_default();
    let title = value_of("title");
    let meta = DocMetadata {
        title: if title.is_empty() { file_name(path) } else { title },
        author: value_of("creator"),
        format: "EPUB".into(),
        extra: Vec::new(),
    };

    // Reading order is the spine; without one, every XHTML item.
    let mut chapter_paths: Vec<std::path::PathBuf> =
        book.spine.iter().filter_map(|s| book.resources.get(&s.idref)).map(|r| r.path.clone()).collect();
    if chapter_paths.is_empty() {
        // shortcut: the crate keeps the manifest in a HashMap, so this fallback is path-sorted, not manifest-ordered.
        chapter_paths = book.resources.values().filter(|r| r.mime == "application/xhtml+xml").map(|r| r.path.clone()).collect();
        chapter_paths.sort();
    }

    let mut pages: Vec<Page> = Vec::new();
    let mut page_of: HashMap<String, usize> = HashMap::new();
    for chapter in chapter_paths {
        let Some(bytes) = book.get_resource_by_path(&chapter) else { continue };
        let parsed = parse_html(&String::from_utf8_lossy(&bytes));
        if parsed.text.trim().is_empty() {
            continue;
        }
        page_of.insert(page_key(&chapter.to_string_lossy()), pages.len());
        pages.extend(paginate_with_elements(parsed.text, parsed.elements, page_size));
    }

    let mut raw = ncx_to_raw(&book.toc);
    if raw.is_empty()
        && let Some(nav) = book.resources.values().find(|r| r.properties.as_deref().is_some_and(|p| p.split(' ').any(|t| t == "nav"))).map(|r| r.path.clone())
        && let Some(html) = book.get_resource_str_by_path(&nav)
    {
        raw = parse_nav(&html, zip_dir(&page_key(&nav.to_string_lossy())));
    }
    let toc = to_entries(&raw, 0, &page_of);
    Ok(Box::new(PagedDoc::new(path, pages, toc, meta)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_toc_maps_to_pages_with_nesting_and_fragments() {
        let nav = r##"<html xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="landmarks"><ol><li><a href="cover.xhtml">Cover</a></li></ol></nav>
<nav epub:type="toc"><ol>
  <li><a href="text/ch1.xhtml">
     Beginning </a></li>
  <li><a href="text/ch2.xhtml#s1">Middle</a>
    <ol><li><a href="text/ch3.xhtml%20b.xhtml">End %</a></li></ol></li>
  <li><span>Appendix</span><ol><li><a href="../gone.xhtml">Lost</a></li></ol></li>
</ol></nav></body></html>"##;
        let raw = parse_nav(nav, "OEBPS");
        let pages: HashMap<String, usize> =
            [("OEBPS/text/ch1.xhtml", 0), ("OEBPS/text/ch2.xhtml", 3), ("OEBPS/text/ch3.xhtml b.xhtml", 5)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
        let toc = to_entries(&raw, 0, &pages);
        let flat: Vec<(String, usize, usize)> = {
            fn walk(e: &[TocEntry], out: &mut Vec<(String, usize, usize)>) {
                for t in e {
                    out.push((t.title.clone(), t.level, t.page_index));
                    walk(&t.children, out);
                }
            }
            let mut v = Vec::new();
            walk(&toc, &mut v);
            v
        };
        assert_eq!(
            flat,
            vec![
                ("Beginning".to_string(), 0, 0),
                ("Middle".to_string(), 0, 3),
                ("End %".to_string(), 1, 5),
                ("Appendix".to_string(), 0, 0),
                ("Lost".to_string(), 1, 0),
            ]
        );
    }

    #[test]
    fn page_key_normalises_separators_fragments_and_escapes() {
        assert_eq!(page_key("OEBPS\\text/ch%201.xhtml#a"), "OEBPS/text/ch 1.xhtml");
    }
}
