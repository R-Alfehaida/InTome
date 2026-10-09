//! PowerPoint presentations, one slide per page. Port of prism/document/formats/powerpoint.py.
//!
//! Like Prism (python-pptx), only top-level text shapes contribute text: tables,
//! grouped shapes and pictures are skipped. Notes follow the slide text.

use std::path::Path;

use anyhow::Context;

use super::word::{PagedDoc, XNode, Xml, file_name, join_zip_path, read_rels, read_xml, rels_path_for, zip_dir};
use crate::document::{DocMetadata, Document, OpenError, OpenOptions, Page, TocEntry};

/// Text of one `p:sp`: paragraphs joined by newlines; a line break inside a
/// paragraph is a vertical tab, as python-pptx reports it.
fn shape_text(sp: &Xml) -> String {
    let Some(body) = sp.child("p:txBody") else { return String::new() };
    let paragraphs: Vec<String> = body
        .children("a:p")
        .map(|p| {
            let mut text = String::new();
            for k in &p.kids {
                let XNode::El(e) = k else { continue };
                match e.name.as_str() {
                    "a:r" | "a:fld" => text.extend(e.children("a:t").map(|t| t.inner_text())),
                    "a:br" => text.push('\u{b}'),
                    _ => {}
                }
            }
            text
        })
        .collect();
    paragraphs.join("\n")
}

/// The `p:ph` of a top-level shape, whatever kind of shape it is.
fn placeholder(shape: &Xml) -> Option<&Xml> {
    shape.kids.iter().find_map(|k| match k {
        XNode::El(nv) if nv.name.starts_with("p:nv") => nv.child("p:nvPr")?.child("p:ph"),
        _ => None,
    })
}

fn shapes(slide: &Xml) -> impl Iterator<Item = &Xml> {
    const KINDS: [&str; 6] = ["p:sp", "p:grpSp", "p:graphicFrame", "p:cxnSp", "p:pic", "p:contentPart"];
    let tree = slide.child("p:cSld").and_then(|c| c.child("p:spTree"));
    tree.into_iter().flat_map(|t| t.kids.iter()).filter_map(|k| match k {
        XNode::El(e) if KINDS.contains(&e.name.as_str()) => Some(e),
        _ => None,
    })
}

/// Text blocks for one slide plus its title. The title is the placeholder with
/// idx 0 (python-pptx `shapes.title`), else "Slide N".
fn slide_content(slide: &Xml, notes: Option<&Xml>, number: usize) -> (Vec<String>, String) {
    let mut parts = Vec::new();
    for shape in shapes(slide).filter(|s| s.name == "p:sp") {
        let text = shape_text(shape);
        let text = text.trim();
        if !text.is_empty() {
            parts.push(text.to_string());
        }
    }
    let title_shape = shapes(slide).find(|s| placeholder(s).is_some_and(|ph| ph.attr("idx").map_or(true, |i| i == "0")));
    let title = title_shape.filter(|s| s.name == "p:sp").map(|s| shape_text(s).trim().to_string()).unwrap_or_default();
    let title = if title.is_empty() { format!("Slide {number}") } else { title };

    let notes_text = notes.and_then(|n| {
        shapes(n).filter(|s| s.name == "p:sp").find(|s| placeholder(s).and_then(|ph| ph.attr("type")) == Some("body")).map(shape_text)
    });
    if let Some(text) = notes_text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(format!("\n--- Notes ---\n{text}"));
    }
    (parts, title)
}

pub fn open(path: &Path, _opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| OpenError::Other(format!("{} is not a valid .pptx file: {e}", file_name(path))))?;

    let package_rels = read_rels(&mut zip, "_rels/.rels");
    let main = package_rels
        .iter()
        .find(|r| r.ty.ends_with("/officeDocument"))
        .map_or_else(|| "ppt/presentation.xml".to_string(), |r| join_zip_path("", &r.target));
    let presentation = read_xml(&mut zip, &main).context("The presentation part is missing or is not valid XML.")?;
    let base = zip_dir(&main).to_string();
    let rels = read_rels(&mut zip, &rels_path_for(&main));

    // Presentation order is the order of p:sldIdLst, not the slide file numbers.
    let slide_paths: Vec<String> = presentation
        .child("p:sldIdLst")
        .into_iter()
        .flat_map(|l| l.children("p:sldId"))
        .filter_map(|s| s.attr("r:id"))
        .filter_map(|id| rels.iter().find(|r| r.id == id))
        .map(|r| join_zip_path(&base, &r.target))
        .collect();

    let mut pages = Vec::new();
    let mut toc = Vec::new();
    for (i, slide_path) in slide_paths.iter().enumerate() {
        let slide = read_xml(&mut zip, slide_path).with_context(|| format!("Could not read slide {}.", i + 1))?;
        let slide_rels = read_rels(&mut zip, &rels_path_for(slide_path));
        let notes = slide_rels
            .iter()
            .find(|r| r.ty.ends_with("/notesSlide"))
            .and_then(|r| read_xml(&mut zip, &join_zip_path(zip_dir(slide_path), &r.target)));
        let (parts, title) = slide_content(&slide, notes.as_ref(), i + 1);
        let text = if parts.is_empty() { format!("(Slide {} \u{2014} no text content)", i + 1) } else { parts.join("\n\n") };
        pages.push(Page::plain(text));
        toc.push(TocEntry { title, page_index: i, level: 0, children: Vec::new() });
    }
    if pages.is_empty() {
        pages.push(Page::plain("(Empty presentation)"));
    }
    let meta = DocMetadata { title: file_name(path), format: "PowerPoint".into(), ..Default::default() };
    Ok(Box::new(PagedDoc::new(path, pages, toc, meta)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::formats::word::parse_xml;

    const NS: &str = r#"xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#;

    fn sp(ph: &str, paragraphs: &str) -> String {
        format!(r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="x"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:txBody>{paragraphs}</p:txBody></p:sp>"#)
    }

    #[test]
    fn slide_text_title_and_notes_follow_python_pptx() {
        let slide = parse_xml(
            format!(
                r#"<p:sld {NS}><p:cSld><p:spTree>{}{}{}</p:spTree></p:cSld></p:sld>"#,
                sp(r#"<p:ph type="title"/>"#, "<a:p><a:r><a:t>  Results </a:t></a:r></a:p>"),
                sp(r#"<p:ph idx="1"/>"#, "<a:p><a:r><a:t>Revenue</a:t></a:r><a:br/><a:r><a:t>grew</a:t></a:r></a:p><a:p><a:r><a:t>Second</a:t></a:r></a:p>"),
                sp("", "<a:p><a:endParaRPr/></a:p>"),
            )
            .as_bytes(),
        )
        .unwrap();
        let notes = parse_xml(
            format!(
                r#"<p:notes {NS}><p:cSld><p:spTree>{}{}</p:spTree></p:cSld></p:notes>"#,
                sp(r#"<p:ph type="sldImg"/>"#, "<a:p><a:r><a:t>image</a:t></a:r></a:p>"),
                sp(r#"<p:ph type="body" idx="1"/>"#, "<a:p><a:r><a:t>Mention Q3</a:t></a:r></a:p>"),
            )
            .as_bytes(),
        )
        .unwrap();
        let (parts, title) = slide_content(&slide, Some(&notes), 2);
        assert_eq!(title, "Results");
        assert_eq!(parts, vec!["Results", "Revenue\u{b}grew\nSecond", "\n--- Notes ---\nMention Q3"]);
    }

    #[test]
    fn slide_without_title_placeholder_is_named_by_number() {
        let slide = parse_xml(format!(r#"<p:sld {NS}><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:sld>"#, sp("", "<a:p><a:r><a:t>Body</a:t></a:r></a:p>")).as_bytes()).unwrap();
        let (parts, title) = slide_content(&slide, None, 3);
        assert_eq!((parts, title.as_str()), (vec!["Body".to_string()], "Slide 3"));
    }
}
