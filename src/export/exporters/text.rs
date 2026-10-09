//! Plain text exporter with optional structure markers.

use std::path::PathBuf;

use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::base::{ExportOptions, Exporter, ProgressFn, write_file};
use crate::export::util::check_cancel;

pub struct TextExporter;

impl Exporter for TextExporter {
    fn format_name(&self) -> &'static str {
        "Plain Text"
    }
    fn format_key(&self) -> &'static str {
        "text"
    }
    fn file_extension(&self) -> &'static str {
        ".txt"
    }

    fn export(&self, doc: &mut dyn Document, options: &ExportOptions, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<PathBuf> {
        let page_count = doc.page_count();
        let mut parts: Vec<String> = Vec::new();
        let meta = doc.metadata();
        if !meta.title.is_empty() {
            let rule = "=".repeat(meta.title.chars().count());
            parts.push(rule.clone());
            parts.push(meta.title.clone());
            parts.push(rule);
            if !meta.author.is_empty() {
                parts.push(format!("by {}", meta.author));
            }
            parts.push(String::new());
        }

        for i in 0..page_count {
            check_cancel(cancel)?;
            let page = doc.page(i);
            let mut text = page.text.clone();
            if options.include_structure_markers {
                for elem in &page.elements {
                    if let Some(level) = elem.element_type.heading_level() {
                        let heading = elem.text.trim();
                        let marked = format!("\n{} {heading}", "#".repeat(level as usize));
                        text = text.replacen(heading, &marked, 1);
                    }
                }
            }
            parts.push(text);
            if options.include_page_numbers && page_count > 1 {
                parts.push(format!("\n--- Page {} of {page_count} ---\n", i + 1));
            }
            progress(i + 1, page_count, &format!("Page {}", i + 1));
        }

        write_file(&options.output_path, parts.join("\n"))?;
        Ok(options.output_path.clone())
    }
}
