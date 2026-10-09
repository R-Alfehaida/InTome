//! Markdown exporter.

use std::path::PathBuf;

use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::base::{ExportOptions, Exporter, ProgressFn, write_file};
use crate::export::util::{Block, check_cancel, flat_toc, page_blocks};
use crate::structured_text::elements::ElementType;

pub struct MarkdownExporter;

impl Exporter for MarkdownExporter {
    fn format_name(&self) -> &'static str {
        "Markdown"
    }
    fn format_key(&self) -> &'static str {
        "markdown"
    }
    fn file_extension(&self) -> &'static str {
        ".md"
    }

    fn export(&self, doc: &mut dyn Document, options: &ExportOptions, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<PathBuf> {
        let page_count = doc.page_count();
        let meta = doc.metadata();
        let mut parts: Vec<String> = Vec::new();
        if !meta.title.is_empty() {
            parts.push(format!("# {}", meta.title));
            if !meta.author.is_empty() {
                parts.push(format!("\n*by {}*", meta.author));
            }
            parts.push(String::new());
        }

        let toc = flat_toc(doc.toc());
        if !toc.is_empty() && options.include_structure_markers {
            parts.push("## Table of Contents\n".into());
            for e in toc {
                parts.push(format!("{}- {}", "  ".repeat(e.level), e.title));
            }
            parts.push(String::new());
        }

        for i in 0..page_count {
            check_cancel(cancel)?;
            let page = doc.page(i);
            if page.elements.is_empty() {
                parts.push(page.text.clone());
            } else {
                parts.push(render_blocks(&page_blocks(page)));
            }
            if options.include_page_numbers && page_count > 1 {
                parts.push(format!("\n---\n*Page {} of {page_count}*\n", i + 1));
            }
            progress(i + 1, page_count, &format!("Page {}", i + 1));
        }

        write_file(&options.output_path, parts.join("\n"))?;
        Ok(options.output_path.clone())
    }
}

fn strip_bullet(line: &str) -> &str {
    let rest = line.strip_prefix(['•', '-', '*']).unwrap_or(line);
    if rest.len() == line.len() { line } else { rest.trim_start() }
}

fn render_blocks(blocks: &[Block]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in blocks {
        match b.kind {
            k if k.is_heading() => {
                parts.push(format!("{} {}", "#".repeat(k.heading_level().unwrap_or(1) as usize), b.text));
                parts.push(String::new());
            }
            ElementType::Quote => {
                parts.extend(b.text.split('\n').map(|l| format!("> {l}")));
                parts.push(String::new());
            }
            ElementType::CodeBlock => {
                // The element model carries no code language, so the fence has no info string.
                parts.push("```".into());
                parts.push(b.text.clone());
                parts.push("```".into());
                parts.push(String::new());
            }
            ElementType::List => {
                for line in b.text.split('\n').map(str::trim).filter(|l| !l.is_empty()) {
                    parts.push(format!("- {}", strip_bullet(line)));
                }
                parts.push(String::new());
            }
            ElementType::Link => match b.url.as_deref().filter(|u| !u.is_empty()) {
                Some(href) => parts.push(format!("[{}]({href})", b.text)),
                None => parts.push(b.text.clone()),
            },
            ElementType::Figure => {
                let alt = b.alt.as_deref().unwrap_or(&b.text);
                parts.push(format!("![{alt}]()"));
                parts.push(String::new());
            }
            _ => {
                parts.push(b.text.clone());
                parts.push(String::new());
            }
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_one_bullet_like_prism_regex() {
        assert_eq!(strip_bullet("• item"), "item");
        assert_eq!(strip_bullet("-item"), "item");
        assert_eq!(strip_bullet("plain"), "plain");
    }
}
