//! Semantic HTML5 exporter with ARIA landmarks.

use std::path::PathBuf;

use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::base::{ExportOptions, Exporter, ProgressFn, write_file};
use crate::export::util::{Block, check_cancel, escape_html as esc, flat_toc, list_items, or_default, page_blocks};
use crate::structured_text::elements::ElementType;

pub struct HtmlExporter;

const STYLE: &str = "  body { font-family: system-ui, sans-serif; max-width: 50em; margin: 2em auto; padding: 0 1em; line-height: 1.6; }
  nav { border-bottom: 1px solid #ccc; margin-bottom: 2em; padding-bottom: 1em; }
  blockquote { border-left: 3px solid #ccc; margin-left: 0; padding-left: 1em; color: #555; }
  pre { background: #f4f4f4; padding: 1em; overflow-x: auto; }
  .page-break { border-top: 1px dashed #ccc; margin: 2em 0; text-align: center; color: #999; font-size: 0.85em; }";

impl Exporter for HtmlExporter {
    fn format_name(&self) -> &'static str {
        "HTML"
    }
    fn format_key(&self) -> &'static str {
        "html"
    }
    fn file_extension(&self) -> &'static str {
        ".html"
    }

    fn export(&self, doc: &mut dyn Document, options: &ExportOptions, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<PathBuf> {
        let meta = doc.metadata();
        let title = esc(&or_default(meta.title, "Document"));
        let author = esc(&meta.author);
        let page_count = doc.page_count();

        let mut lines: Vec<String> = vec![
            "<!DOCTYPE html>".into(),
            "<html lang=\"en\">".into(),
            "<head>".into(),
            "<meta charset=\"utf-8\">".into(),
            format!("<title>{title}</title>"),
            "<style>".into(),
            STYLE.into(),
            "</style>".into(),
            "</head>".into(),
            "<body>".into(),
            "<header role=\"banner\">".into(),
            format!("<h1>{title}</h1>"),
        ];
        if !author.is_empty() {
            lines.push(format!("<p class=\"author\">by {author}</p>"));
        }
        lines.push("</header>".into());

        let toc = flat_toc(doc.toc());
        if !toc.is_empty() && options.include_structure_markers {
            lines.push("<nav role=\"navigation\" aria-label=\"Table of Contents\">".into());
            lines.push("<h2>Table of Contents</h2>".into());
            lines.push("<ul>".into());
            for e in toc {
                lines.push(format!("  <li><a href=\"#page-{}\">{}</a></li>", e.page_index + 1, esc(&e.title)));
            }
            lines.push("</ul>".into());
            lines.push("</nav>".into());
        }

        lines.push("<main role=\"main\">".into());
        for i in 0..page_count {
            check_cancel(cancel)?;
            let blocks = page_blocks(doc.page(i));
            lines.push(format!("<section id=\"page-{n}\" aria-label=\"Page {n}\">", n = i + 1));
            for b in &blocks {
                render_block(&mut lines, b);
            }
            lines.push("</section>".into());
            if options.include_page_numbers && i + 1 < page_count {
                lines.push(format!("<div class=\"page-break\" role=\"separator\">Page {}</div>", i + 1));
            }
            progress(i + 1, page_count, &format!("Page {}", i + 1));
        }
        lines.push("</main>".into());
        lines.push("</body>".into());
        lines.push("</html>".into());

        write_file(&options.output_path, lines.join("\n"))?;
        Ok(options.output_path.clone())
    }
}

fn render_block(lines: &mut Vec<String>, b: &Block) {
    let t = esc(&b.text);
    match b.kind {
        k if k.is_heading() => {
            let n = k.heading_level().unwrap_or(1);
            lines.push(format!("<h{n}>{t}</h{n}>"));
        }
        ElementType::Quote => lines.push(format!("<blockquote>{t}</blockquote>")),
        ElementType::CodeBlock => lines.push(format!("<pre><code>{t}</code></pre>")),
        // Prism wrapped these in <table>/<li>/<figure> holding bare text, which is invalid HTML.
        ElementType::Table => lines.push(format!("<pre>{t}</pre>")),
        ElementType::Figure => lines.push(format!("<figure><figcaption>{t}</figcaption></figure>")),
        ElementType::List | ElementType::ListItem => {
            lines.push("<ul>".into());
            for item in list_items(&b.text) {
                lines.push(format!("  <li>{}</li>", esc(&item)));
            }
            lines.push("</ul>".into());
        }
        ElementType::Link => {
            let href = esc(b.url.as_deref().unwrap_or("#"));
            lines.push(format!("<p><a href=\"{href}\">{t}</a></p>"));
        }
        _ => lines.push(format!("<p>{t}</p>")),
    }
}
