//! Annotations as a semantic HTML5 document.

use crate::annotation::models::{Bookmark, Note, Quote};
use crate::export::util::escape_html as esc;

pub fn export_annotations_html(bookmarks: &[Bookmark], notes: &[Note], quotes: &[Quote], document_title: &str) -> String {
    let title = if document_title.is_empty() { "Untitled Document" } else { document_title };
    let mut p: Vec<String> = vec![
        "<!DOCTYPE html>".into(),
        "<html lang=\"en\">".into(),
        "<head>".into(),
        "  <meta charset=\"utf-8\">".into(),
        format!("  <title>Annotations: {}</title>", esc(title)),
        "  <style>".into(),
        "    body { font-family: sans-serif; max-width: 50em; margin: 2em auto; line-height: 1.5; }".into(),
        "    h1 { border-bottom: 2px solid #333; padding-bottom: .3em; }".into(),
        "    h2 { margin-top: 1.5em; }".into(),
        "    .meta { color: #666; font-size: .9em; }".into(),
        "    blockquote { border-left: 3px solid #888; margin-left: 0; padding-left: 1em; }".into(),
        "    .tags { font-style: italic; color: #555; }".into(),
        "  </style>".into(),
        "</head>".into(),
        "<body>".into(),
        format!("  <h1>Annotations for: {}</h1>", esc(title)),
    ];

    if !bookmarks.is_empty() {
        p.push("  <section aria-label=\"Bookmarks\">".into());
        p.push("    <h2>Bookmarks</h2>".into());
        p.push("    <ul>".into());
        for bm in bookmarks {
            let label = if bm.name.is_empty() { "<em>(unnamed)</em>".to_string() } else { esc(&bm.name) };
            p.push(format!("      <li>Page {}: {label}</li>", bm.page_index + 1));
        }
        p.push("    </ul>".into());
        p.push("  </section>".into());
    }

    if !notes.is_empty() {
        p.push("  <section aria-label=\"Notes\">".into());
        p.push("    <h2>Notes</h2>".into());
        for n in notes {
            p.push(format!("    <div class=\"meta\">Page {}, position {}</div>", n.page_index + 1, n.char_position));
            p.push(format!("    <p>{}</p>", esc(&n.text)));
        }
        p.push("  </section>".into());
    }

    if !quotes.is_empty() {
        p.push("  <section aria-label=\"Quotes\">".into());
        p.push("    <h2>Quotes</h2>".into());
        for q in quotes {
            p.push(format!("    <div class=\"meta\">Page {}, position {}&ndash;{}</div>", q.page_index + 1, q.start_pos, q.end_pos));
            p.push(format!("    <blockquote>{}</blockquote>", esc(&q.text)));
            if !q.tags.is_empty() {
                p.push(format!("    <p class=\"tags\">Tags: {}</p>", esc(&q.tags)));
            }
        }
        p.push("  </section>".into());
    }

    if bookmarks.is_empty() && notes.is_empty() && quotes.is_empty() {
        p.push("  <p>No annotations found.</p>".into());
    }

    p.push("</body>".into());
    p.push("</html>".into());
    p.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_marks_unnamed_bookmarks() {
        let bm = Bookmark { id: 1, document_hash: "h".into(), name: String::new(), page_index: 0, char_position: 0, created_at: None };
        let q = Quote { id: 1, document_hash: "h".into(), page_index: 1, start_pos: 2, end_pos: 5, text: "a <b> & c".into(), tags: String::new(), created_at: None };
        let html = export_annotations_html(&[bm], &[], &[q], "Tom & Jerry");
        assert!(html.contains("<title>Annotations: Tom &amp; Jerry</title>"));
        assert!(html.contains("<li>Page 1: <em>(unnamed)</em></li>"));
        assert!(html.contains("position 2&ndash;5</div>"));
        assert!(html.contains("<blockquote>a &lt;b&gt; &amp; c</blockquote>"));
        assert!(!html.contains("class=\"tags\""));
    }
}
