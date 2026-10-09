//! Annotations as a plain-text report.

use crate::annotation::models::{Bookmark, Note, Quote};

pub fn export_annotations_text(bookmarks: &[Bookmark], notes: &[Note], quotes: &[Quote], document_title: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let title = if document_title.is_empty() { "Untitled Document" } else { document_title };
    let header = format!("Annotations for: {title}");
    lines.push(header.clone());
    lines.push("=".repeat(header.chars().count()));
    lines.push(String::new());

    if !bookmarks.is_empty() {
        lines.push("BOOKMARKS".into());
        lines.push("---------".into());
        for bm in bookmarks {
            let label = if bm.name.is_empty() { "(unnamed bookmark)" } else { &bm.name };
            lines.push(format!("  Page {}: {label}", bm.page_index + 1));
        }
        lines.push(String::new());
    }

    if !notes.is_empty() {
        lines.push("NOTES".into());
        lines.push("-----".into());
        for note in notes {
            lines.push(format!("  Page {}, position {}:", note.page_index + 1, note.char_position));
            lines.extend(note.text.lines().map(|l| format!("    {l}")));
            lines.push(String::new());
        }
    }

    if !quotes.is_empty() {
        lines.push("QUOTES".into());
        lines.push("------".into());
        for q in quotes {
            lines.push(format!("  Page {}, position {}-{}:", q.page_index + 1, q.start_pos, q.end_pos));
            lines.push(format!("    \"{}\"", q.text));
            if !q.tags.is_empty() {
                lines.push(format!("    Tags: {}", q.tags));
            }
            lines.push(String::new());
        }
    }

    if bookmarks.is_empty() && notes.is_empty() && quotes.is_empty() {
        lines.push("No annotations found.".into());
        lines.push(String::new());
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report() {
        let want = format!("Annotations for: Untitled Document\n{}\n\nNo annotations found.\n", "=".repeat(34));
        assert_eq!(export_annotations_text(&[], &[], &[], ""), want);
    }

    #[test]
    fn full_report_layout() {
        let bm = Bookmark { id: 1, document_hash: "h".into(), name: String::new(), page_index: 0, char_position: 0, created_at: None };
        let note = Note { id: 1, document_hash: "h".into(), page_index: 1, char_position: 7, text: "line one\nline two".into(), created_at: None, updated_at: None };
        let q = Quote { id: 1, document_hash: "h".into(), page_index: 2, start_pos: 3, end_pos: 8, text: "quoted".into(), tags: "a".into(), created_at: None };
        let want = "Annotations for: T\n==================\n\nBOOKMARKS\n---------\n  Page 1: (unnamed bookmark)\n\nNOTES\n-----\n  Page 2, position 7:\n    line one\n    line two\n\nQUOTES\n------\n  Page 3, position 3-8:\n    \"quoted\"\n    Tags: a\n";
        assert_eq!(export_annotations_text(&[bm], &[note], &[q], "T"), want);
    }
}
