//! CSV / TSV documents rendered as an accessible text table, with each page
//! marked as a `Table` element so table navigation and the Element List reach
//! it. Port of prism/document/formats/csv.py.

use std::collections::HashMap;
use std::path::Path;

use crate::document::utils::{PagedDocument, decode_text, page_size_of, paginate_text};
use crate::document::{Document, OpenError, OpenOptions, Page};
use crate::structured_text::elements::{ElementMeta, ElementType, SemanticElement};

/// Cells joined by " | " keep columns audibly distinct without an ASCII grid.
const CELL_SEP: &str = " | ";
const CANDIDATES: [char; 4] = [',', ';', '\t', '|'];
const PREFERRED: [char; 5] = [',', '\t', ';', ' ', ':'];
const SNIFF_CHARS: usize = 8192;

/// Port of Python's `csv.Sniffer.sniff(sample, delimiters=",;\t|")` frequency
/// heuristic: the delimiter whose per-line count is the most consistent wins.
/// Delimiters inside double quotes are not counted. Falls back to a comma.
fn sniff_delimiter(sample: &str) -> char {
    let lines: Vec<&str> = sample.split('\n').filter(|l| !l.is_empty()).collect();
    let chunk = lines.len().min(10);
    let mut freq: HashMap<char, Vec<(usize, i64)>> = HashMap::new();
    let mut delims: HashMap<char, (usize, i64)> = HashMap::new();
    let (mut start, mut iteration) = (0, 0);
    while start < lines.len() {
        iteration += 1;
        let end = (start + chunk).min(lines.len());
        for line in &lines[start..end] {
            for c in CANDIDATES {
                let n = count_outside_quotes(line, c);
                let table = freq.entry(c).or_default();
                match table.iter_mut().find(|(count, _)| *count == n) {
                    Some(entry) => entry.1 += 1,
                    None => table.push((n, 1)),
                }
            }
        }
        let mut modes: Vec<(char, (usize, i64))> = Vec::new();
        for c in CANDIDATES {
            let Some(items) = freq.get(&c) else { continue };
            if items.len() == 1 && items[0].0 == 0 {
                continue;
            }
            let mode = if items.len() > 1 {
                let best = items.iter().copied().reduce(|a, b| if b.1 > a.1 { b } else { a }).unwrap();
                let others: i64 = items.iter().filter(|i| i.0 != best.0).map(|i| i.1).sum();
                (best.0, best.1 - others)
            } else {
                items[0]
            };
            modes.push((c, mode));
        }
        let total = (chunk * iteration).min(lines.len()) as f64;
        let mut consistency = 1.0_f64;
        while delims.is_empty() && consistency >= 0.9 {
            for &(k, v) in &modes {
                if v.0 > 0 && v.1 > 0 && v.1 as f64 / total >= consistency {
                    delims.insert(k, v);
                }
            }
            consistency -= 0.01;
        }
        if delims.len() == 1 {
            return *delims.keys().next().unwrap();
        }
        start = end;
    }
    if let Some(&d) = PREFERRED.iter().find(|d| delims.contains_key(d)) {
        return d;
    }
    delims.iter().max_by_key(|&(&k, &v)| (v, k)).map_or(',', |(&k, _)| k)
}

fn count_outside_quotes(line: &str, delim: char) -> usize {
    let mut in_quotes = false;
    let mut n = 0;
    for ch in line.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
        } else if ch == delim && !in_quotes {
            n += 1;
        }
    }
    n
}

fn parse_rows(text: &str, delimiter: char) -> Vec<Vec<String>> {
    csv::ReaderBuilder::new()
        .delimiter(delimiter as u8)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes())
        .records()
        .filter_map(Result::ok)
        .map(|r| r.iter().map(str::to_string).collect())
        .collect()
}

fn table_element(len: usize, text: String) -> SemanticElement {
    SemanticElement { element_type: ElementType::Table, start_pos: 0, end_pos: len, text, meta: ElementMeta::default() }
}

/// Pages for a table's rows: one page when it fits, else split at row boundaries.
fn render_pages(rows: &[Vec<String>], page_size: usize) -> Vec<Page> {
    if rows.is_empty() {
        return vec![Page::plain("(Empty table)")];
    }
    let lines: Vec<String> = rows.iter().map(|r| r.iter().map(|c| c.trim()).collect::<Vec<_>>().join(CELL_SEP)).collect();
    let full_text = lines.join("\n");
    let len = full_text.chars().count();
    if len <= page_size {
        let label: String = lines[0].chars().take(80).collect();
        return vec![Page { elements: vec![table_element(len, label)], text: full_text }];
    }
    paginate_text(&full_text, page_size)
        .into_iter()
        .map(|chunk| Page { elements: vec![table_element(chunk.chars().count(), "Table (continued)".into())], text: chunk })
        .collect()
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let is_tsv = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tsv"));
    // No newline translation here: line breaks inside quoted cells must survive.
    let text = decode_text(&std::fs::read(path)?);
    let delimiter = if is_tsv { '\t' } else { sniff_delimiter(&text.chars().take(SNIFF_CHARS).collect::<String>()) };
    let pages = render_pages(&parse_rows(&text, delimiter), page_size_of(opts));
    let format = if is_tsv { "Tab-separated values" } else { "CSV" };
    Ok(Box::new(PagedDocument::new(path, format, PagedDocument::file_name(path), pages, Vec::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_the_consistent_delimiter() {
        assert_eq!(sniff_delimiter("a;b;c\n1;2;3\n4;5;6\n"), ';');
        assert_eq!(sniff_delimiter("a|b\n1|2\n"), '|');
        assert_eq!(sniff_delimiter("a\tb\n1\t2\n"), '\t');
        assert_eq!(sniff_delimiter("name,city\nAda,\"London, UK\"\nBob,Paris\n"), ',');
        assert_eq!(sniff_delimiter("just one column\nof words\n"), ',');
    }

    #[test]
    fn renders_rows_with_quoted_cells_and_a_table_element() {
        let rows = parse_rows("name,city\nAda,\"London, UK\"\n\"Bob \"\"B\"\" Smith\", Paris \n", ',');
        let pages = render_pages(&rows, 3000);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].text, "name | city\nAda | London, UK\nBob \"B\" Smith | Paris");
        let e = &pages[0].elements[0];
        assert_eq!((e.element_type, e.start_pos, e.end_pos, e.text.as_str()), (ElementType::Table, 0, 50, "name | city"));
    }

    #[test]
    fn long_tables_split_at_row_boundaries_and_empty_input_says_so() {
        let rows: Vec<Vec<String>> = (0..6).map(|i| vec![format!("row{i}"), "x".into()]).collect();
        let pages = render_pages(&rows, 25);
        assert_eq!(pages.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), vec!["row0 | x\nrow1 | x\n", "row2 | x\nrow3 | x\n", "row4 | x\nrow5 | x"]);
        assert_eq!(pages[1].elements[0].text, "Table (continued)");
        assert_eq!(pages[1].elements[0].end_pos, 18);
        assert_eq!(render_pages(&[], 25)[0].text, "(Empty table)");
    }
}
