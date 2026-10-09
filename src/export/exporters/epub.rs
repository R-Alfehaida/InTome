//! EPUB 3 exporter with accessibility metadata and a nav-document TOC (epub-builder).

use std::path::PathBuf;

use anyhow::Context;
use epub_builder::{EpubBuilder, EpubContent, EpubVersion, MetadataOpfV3, ReferenceType, ZipLibrary};

use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::base::{ExportOptions, Exporter, ProgressFn, write_file};
use crate::export::util::{Block, check_cancel, escape_html as esc, list_items, or_default, page_blocks};
use crate::structured_text::elements::ElementType;

pub struct EpubExporter;

/// epub-builder gives the first `dc:language` and the first `dc:creator` the same id
/// (`epub-creator-0`), and duplicate ids make the package invalid. Renames the language's id
/// in `content.opf`, copying every other entry byte for byte.
fn fix_duplicate_ids(epub: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    use std::io::{Cursor, Read};
    let mut archive = zip::ZipArchive::new(Cursor::new(epub))?;
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        if archive.name_for_index(i).is_some_and(|n| n.ends_with("content.opf")) {
            let mut opf = String::new();
            let mut entry = archive.by_index(i)?;
            let name = entry.name().to_string();
            entry.read_to_string(&mut opf)?;
            drop(entry);
            let opf = opf.replace("<dc:language id=\"epub-creator-", "<dc:language id=\"epub-language-");
            out.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated))?;
            std::io::Write::write_all(&mut out, opf.as_bytes())?;
        } else {
            out.raw_copy_file(archive.by_index_raw(i)?)?;
        }
    }
    Ok(out.finish()?.into_inner())
}

const CSS: &str = "body { font-family: serif; line-height: 1.6; }
blockquote { border-left: 3px solid #ccc; padding-left: 1em; }
pre { background: #f4f4f4; padding: 1em; overflow-x: auto; }
";

impl Exporter for EpubExporter {
    fn format_name(&self) -> &'static str {
        "EPUB"
    }
    fn format_key(&self) -> &'static str {
        "epub"
    }
    fn file_extension(&self) -> &'static str {
        ".epub"
    }

    fn export(&self, doc: &mut dyn Document, options: &ExportOptions, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<PathBuf> {
        let page_count = doc.page_count();
        let meta = doc.metadata();
        let title = or_default(meta.title, "Untitled");

        let mut book = EpubBuilder::new(ZipLibrary::new().map_err(|e| anyhow::anyhow!("{e}"))?).map_err(|e| anyhow::anyhow!("{e}"))?;
        let fail = |e: epub_builder::Error| anyhow::anyhow!("EPUB: {e}");
        book.epub_version(EpubVersion::V30);
        book.metadata("title", &title).map_err(fail)?;
        book.metadata("lang", "en").map_err(fail)?;
        if !meta.author.is_empty() {
            book.metadata("author", &meta.author).map_err(fail)?;
        }
        book.metadata("generator", "InTome").map_err(fail)?;
        if options.accessibility_metadata {
            for (property, value) in [
                ("schema:accessMode", "textual"),
                ("schema:accessModeSufficient", "textual"),
                ("schema:accessibilityFeature", "structuralNavigation"),
                ("schema:accessibilityFeature", "tableOfContents"),
                ("schema:accessibilitySummary", "This publication has been exported with structural markup for accessibility."),
            ] {
                book.add_metadata_opf(Box::new(MetadataOpfV3::new(property.into(), value.into())));
            }
        }
        book.stylesheet(CSS.as_bytes()).map_err(fail)?;

        // One chapter per source page, titled by the page's first level-1 heading.
        for i in 0..page_count {
            check_cancel(cancel)?;
            let page = doc.page(i);
            let chapter_title = page
                .elements
                .iter()
                .find(|e| e.element_type == ElementType::Heading1)
                .map(|e| e.text.trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format!("Page {}", i + 1));
            let blocks = page_blocks(page);

            let mut body = Vec::new();
            for b in &blocks {
                render_block(&mut body, b);
            }
            let xhtml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n\
                 <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" lang=\"en\" xml:lang=\"en\">\n\
                 <head>\n<meta charset=\"utf-8\" />\n<title>{}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"stylesheet.css\" />\n</head>\n\
                 <body>\n{}\n</body>\n</html>\n",
                esc(&chapter_title),
                body.join("\n")
            );

            let mut content = EpubContent::new(format!("page_{:04}.xhtml", i + 1), xhtml.as_bytes());
            // A nav document needs at least one entry; without the TOC option it points at the first page only.
            if options.include_toc {
                content = content.title(chapter_title);
            } else if i == 0 {
                content = content.title(title.clone());
            }
            if i == 0 {
                content = content.reftype(ReferenceType::Text);
            }
            book.add_content(content).map_err(fail)?;
            progress(i + 1, page_count, &format!("Page {}", i + 1));
        }

        let mut bytes = Vec::new();
        book.generate(&mut bytes).map_err(fail)?;
        write_file(&options.output_path, fix_duplicate_ids(bytes).context("cannot finalise EPUB")?)?;
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
        ElementType::Quote => lines.push(format!("<blockquote><p>{t}</p></blockquote>")),
        ElementType::CodeBlock => lines.push(format!("<pre><code>{t}</code></pre>")),
        ElementType::List => {
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
