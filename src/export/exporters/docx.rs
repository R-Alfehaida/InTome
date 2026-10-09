//! DOCX exporter with real Word heading styles (docx-rs).

use std::path::PathBuf;

use anyhow::Context;
use docx_rs::*;

use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::base::{ExportOptions, Exporter, ProgressFn};
use crate::export::util::{check_cancel, list_items, page_blocks};
use crate::structured_text::elements::ElementType;

pub struct DocxExporter;

const BULLET_NUMBERING: usize = 1;
/// Half-points: 32 = 16 pt. Mirrors Word's built-in heading scale.
const HEADING_SIZES: [usize; 6] = [32, 26, 24, 22, 22, 22];

fn styles() -> Vec<Style> {
    let mut out = vec![
        Style::new("Title", StyleType::Paragraph).name("Title").based_on("Normal").next("Normal").size(52).bold(),
        Style::new("Subtitle", StyleType::Paragraph).name("Subtitle").based_on("Normal").next("Normal").size(28).italic(),
        Style::new("Quote", StyleType::Paragraph)
            .name("Quote")
            .based_on("Normal")
            .italic()
            .indent(Some(720), None, Some(720), None),
        Style::new("ListBullet", StyleType::Paragraph).name("List Bullet").based_on("Normal"),
        Style::new("Code", StyleType::Paragraph).name("Code").based_on("Normal").fonts(RunFonts::new().ascii("Consolas").hi_ansi("Consolas")),
    ];
    for (i, size) in HEADING_SIZES.iter().enumerate() {
        let n = i + 1;
        out.push(
            Style::new(format!("Heading{n}"), StyleType::Paragraph)
                .name(format!("Heading {n}"))
                .based_on("Normal")
                .next("Normal")
                .size(*size)
                .bold()
                .outline_lvl(i),
        );
    }
    out
}

/// A run of `text` with a line break for each `\n` (like python-docx's `add_paragraph`).
fn run_of(text: &str) -> Run {
    let mut run = Run::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            run = run.add_break(BreakType::TextWrapping);
        }
        run = run.add_text(line);
    }
    run
}

fn plain(text: &str) -> Paragraph {
    Paragraph::new().add_run(run_of(text))
}

impl Exporter for DocxExporter {
    fn format_name(&self) -> &'static str {
        "Word Document"
    }
    fn format_key(&self) -> &'static str {
        "docx"
    }
    fn file_extension(&self) -> &'static str {
        ".docx"
    }

    fn export(&self, doc: &mut dyn Document, options: &ExportOptions, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<PathBuf> {
        let page_count = doc.page_count();
        let meta = doc.metadata();

        let mut docx = Docx::new()
            .add_abstract_numbering(AbstractNumbering::new(BULLET_NUMBERING).add_level(
                Level::new(0, Start::new(1), NumberFormat::new("bullet"), LevelText::new("•"), LevelJc::new("left"))
                    .indent(Some(720), Some(SpecialIndentType::Hanging(360)), None, None),
            ))
            .add_numbering(Numbering::new(BULLET_NUMBERING, BULLET_NUMBERING));
        for s in styles() {
            docx = docx.add_style(s);
        }

        if !meta.title.is_empty() {
            docx = docx.add_paragraph(plain(&meta.title).style("Title"));
        }
        if !meta.author.is_empty() {
            docx = docx.add_paragraph(plain(&format!("by {}", meta.author)).style("Subtitle"));
        }

        for i in 0..page_count {
            check_cancel(cancel)?;
            for b in page_blocks(doc.page(i)) {
                match b.kind {
                    k if k.is_heading() => {
                        let n = k.heading_level().unwrap_or(1);
                        docx = docx.add_paragraph(plain(&b.text).style(&format!("Heading{n}")));
                    }
                    ElementType::Quote => docx = docx.add_paragraph(plain(&b.text).style("Quote")),
                    ElementType::CodeBlock => {
                        let run = run_of(&b.text).fonts(RunFonts::new().ascii("Consolas").hi_ansi("Consolas"));
                        docx = docx.add_paragraph(Paragraph::new().add_run(run).style("Code"));
                    }
                    ElementType::List => {
                        for item in list_items(&b.text) {
                            docx = docx.add_paragraph(
                                plain(&item).style("ListBullet").numbering(NumberingId::new(BULLET_NUMBERING), IndentLevel::new(0)),
                            );
                        }
                    }
                    _ => docx = docx.add_paragraph(plain(&b.text)),
                }
            }
            if i + 1 < page_count {
                docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_break(BreakType::Page)));
            }
            progress(i + 1, page_count, &format!("Page {}", i + 1));
        }

        let file = std::fs::File::create(&options.output_path).with_context(|| format!("cannot write {}", options.output_path.display()))?;
        docx.build().pack(file).context("cannot write DOCX")?;
        Ok(options.output_path.clone())
    }
}
