//! Single-page OCR pipeline, form-field labelling and flat-form discovery
//! (port of prism/ai/ocr.py).
//!
//! Pages are rendered to images and sent through an [`OcrProvider`]; results
//! are cached per page. Labelling and discovery talk to [`GeminiClient`]
//! directly because they need a JSON response, outside the provider contract.

use std::path::Path;

use pdfium_render::prelude::PdfPageIndex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai::client::GeminiClient;
use crate::ai::prompts::{FLAT_FORM_DISCOVERY_PROMPT, FORM_FIELD_LABELING_PROMPT, get_ocr_prompt};
use crate::ai::provider::{OcrError, OcrProvider};
use crate::ai::structure::{ParsedOcr, parse_ocr_response};
use crate::cancel::CancelToken;
use crate::config::{self, GeminiMode};
use crate::document::cache::{get_cached, set_cached};
use crate::document::formats::pdf;
use crate::document::{FormField, Rect};

#[derive(Serialize, Deserialize)]
struct CachedText {
    text: String,
}

/// Reads a standalone image as `(bytes, mime_type)`. JPEG and PNG pass
/// through; other formats (BMP, TIFF, ...) are converted to PNG.
pub fn render_image_file_to_bytes(path: &Path) -> anyhow::Result<(Vec<u8>, &'static str)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Ok((std::fs::read(path)?, "image/jpeg")),
        "png" => Ok((std::fs::read(path)?, "image/png")),
        _ => {
            let mut png = Vec::new();
            image::open(path)?.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
            Ok((png, "image/png"))
        }
    }
}

/// OCRs one page image with the prompt built from current settings.
pub fn ocr_single_page(image: &[u8], mime_type: &str, provider: &dyn OcrProvider) -> Result<ParsedOcr, OcrError> {
    let ocr = config::get().ocr;
    let prompt = get_ocr_prompt(ocr.manga_mode, &ocr.system_prompt_override, &ocr.user_instructions);
    Ok(parse_ocr_response(&provider.transcribe_page(image, &prompt, mime_type)?))
}

/// OCRs a PDF page, or returns the cached text (cache hits carry no elements, as in Prism).
pub fn ocr_pdf_page(pdf_path: &Path, page_index: usize, provider: &dyn OcrProvider) -> Result<ParsedOcr, OcrError> {
    let key = format!("ocr:page:{page_index}");
    if let Some(c) = get_cached::<CachedText>(pdf_path, &key) {
        log::debug!("OCR cache hit for {} page {page_index}", pdf_path.display());
        return Ok(ParsedOcr { text: c.text, elements: Vec::new() });
    }
    let image = render_page(pdf_path, page_index).map_err(|e| OcrError(format!("Could not render page {}: {e}", page_index + 1)))?;
    let result = ocr_single_page(&image, "image/png", provider)?;
    set_cached(pdf_path, &key, &CachedText { text: result.text.clone() });
    Ok(result)
}

/// OCRs a standalone image file, or returns the cached text.
pub fn ocr_image_file(image_path: &Path, provider: &dyn OcrProvider) -> Result<ParsedOcr, OcrError> {
    let key = "ocr:image";
    if let Some(c) = get_cached::<CachedText>(image_path, key) {
        log::debug!("OCR cache hit for {}", image_path.display());
        return Ok(ParsedOcr { text: c.text, elements: Vec::new() });
    }
    let (image, mime_type) = render_image_file_to_bytes(image_path).map_err(|e| OcrError(format!("Could not read image: {e}")))?;
    let result = ocr_single_page(&image, mime_type, provider)?;
    set_cached(image_path, key, &CachedText { text: result.text.clone() });
    Ok(result)
}

fn render_page(pdf_path: &Path, page_index: usize) -> anyhow::Result<Vec<u8>> {
    pdf::render_page_png(pdf_path, None, page_index, config::get().ocr.render_dpi)
}

// ---------------------------------------------------------------------------
// Form-field labelling (Gemini JSON)
// ---------------------------------------------------------------------------

/// An AI label for one widget. Cached per page as a JSON array under
/// `form_labels:page:{n}`; `pdf.rs` reads it back (AI label beats `/TU`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FieldLabel {
    pub field_name: String,
    pub label: String,
    pub group: Option<String>,
    pub context: Option<String>,
}

pub fn form_labels_cache_key(page_index: usize) -> String {
    format!("form_labels:page:{page_index}")
}

fn field_list_prompt(fields: &[FormField]) -> String {
    let lines: Vec<String> = fields
        .iter()
        .map(|f| {
            let [x0, y0, x1, y1] = f.rect;
            // FieldType's Debug names ("CheckBox", "Text", ...) are the readable widget names Prism sent.
            format!("  - Name: {}, Type: {:?}, Position: ({x0:.0}, {y0:.0}, {x1:.0}, {y1:.0})", f.name, f.field_type)
        })
        .collect();
    format!("Form fields on this page:\n{}", lines.join("\n"))
}

/// Asks Gemini for screen-reader labels for the widgets on one page.
/// `force` skips the cache read and overwrites it (the explicit
/// "Label Form Fields" command). A model failure logs and yields no labels;
/// only a page render failure is an error.
pub fn label_form_fields(
    pdf_path: &Path,
    page_index: usize,
    fields_on_page: &[FormField],
    client: &GeminiClient,
    mode: GeminiMode,
    cancel: Option<&CancelToken>,
    force: bool,
) -> anyhow::Result<Vec<FieldLabel>> {
    let key = form_labels_cache_key(page_index);
    if !force && let Some(cached) = get_cached(pdf_path, &key) {
        return Ok(cached);
    }
    let image = render_page(pdf_path, page_index)?;
    let result = client.generate_json_with_image(&image, "image/png", &field_list_prompt(fields_on_page), FORM_FIELD_LABELING_PROMPT, mode, cancel);
    match result {
        Ok(Value::Array(items)) => {
            let labels: Vec<FieldLabel> = items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
            set_cached(pdf_path, &key, &labels);
            Ok(labels)
        }
        Ok(_) => Ok(Vec::new()),
        Err(e) => {
            log::error!("Form field labeling failed for page {page_index}: {e:#}");
            Ok(Vec::new())
        }
    }
}

// ---------------------------------------------------------------------------
// Flat (non-AcroForm) form discovery
// ---------------------------------------------------------------------------

/// Max vertical gap (points) for snapping a discovered blank to an underscore
/// anchor whose centre is near the AI box's centre.
const FLAT_SNAP_MAX_DY_PTS: f32 = 25.0;

/// Bump when the discovery output shape or coordinate maths changes, so stale
/// cached fields are abandoned. `pdf.rs` must read the same key.
pub const FLAT_FORM_CACHE_VERSION: &str = "v3";

pub fn flat_form_cache_key(page_index: usize) -> String {
    format!("flat_form_fields:{FLAT_FORM_CACHE_VERSION}:page:{page_index}")
}

/// One option of a pick-one flat field: the box to tick or word to mark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlatOption {
    pub label: String,
    pub rect: Rect,
}

/// A blank discovered on a flat page, in top-left PDF points. `field_type` is
/// the model's lowercased kind ("text", "date", "signature", "checkbox",
/// "choice", "select", or anything else, treated as text). Pick-one kinds
/// carry `options` (two or more) and no `rect`; the others carry `rect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlatField {
    pub field_name: String,
    pub label: String,
    pub group: Option<String>,
    pub context: Option<String>,
    pub field_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<FlatOption>,
}

/// Page width and height in points, plus the boxes of underscore-run words
/// (top-left origin): the one geometry that pins a blank's writable span.
/// Drawn rules are deliberately not anchors: on table forms they are
/// full-width cell borders, and snapping a small cell to one drags the value
/// to the row's left edge.
// shortcut: assumes an unrotated page whose media box starts at (0, 0); move into pdf.rs if it grows a page-geometry API.
fn flat_form_page_geometry(pdf_path: &Path, page_index: usize) -> anyhow::Result<(f32, f32, Vec<Rect>)> {
    pdf::with_pdfium(|pdfium| {
        let doc = pdfium.load_pdf_from_file(pdf_path, None)?;
        let page = doc.pages().get(page_index as PdfPageIndex)?;
        let (w, h) = (page.width().value, page.height().value);
        let mut anchors = Vec::new();
        let mut word = String::new();
        let mut bbox: Option<Rect> = None;
        let mut flush = |word: &mut String, bbox: &mut Option<Rect>| {
            if let Some(b) = bbox.take().filter(|_| word.contains("___")) {
                anchors.push(b);
            }
            word.clear();
        };
        let text = page.text()?;
        for ch in text.chars().iter() {
            match ch.unicode_char() {
                Some(c) if !c.is_whitespace() => {
                    word.push(c);
                    if let Ok(r) = ch.loose_bounds() {
                        let cb = [r.left().value, h - r.top().value, r.right().value, h - r.bottom().value];
                        bbox = Some(bbox.map_or(cb, |b| [b[0].min(cb[0]), b[1].min(cb[1]), b[2].max(cb[2]), b[3].max(cb[3])]));
                    }
                }
                _ => flush(&mut word, &mut bbox),
            }
        }
        flush(&mut word, &mut bbox);
        Ok((w, h, anchors))
    })
}

/// Snaps an approximate AI rect to the anchor with the most horizontal overlap
/// (less vertical distance) within [`FLAT_SNAP_MAX_DY_PTS`]; unchanged if none qualifies.
fn snap_rect_to_anchor(rect: Rect, anchors: &[Rect]) -> Rect {
    let rcy = (rect[1] + rect[3]) / 2.0;
    let mut best: Option<(f32, Rect)> = None;
    for a in anchors {
        let dy = ((a[1] + a[3]) / 2.0 - rcy).abs();
        if dy > FLAT_SNAP_MAX_DY_PTS {
            continue;
        }
        let overlap = (rect[2].min(a[2]) - rect[0].max(a[0])).max(0.0);
        let score = overlap - dy;
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, *a));
        }
    }
    best.map_or(rect, |(_, a)| a)
}

/// Converts a Gemini `[x0, y0, x1, y1]` box to page points, or `None` if
/// malformed. Gemini emits its native 0-1000 scale even when asked for 0-1, so
/// any value above 1 means 0-1000. Clamped to the page; no y-flip (top-left origin).
fn norm_box_to_pts(value: &Value, page_w: f32, page_h: f32) -> Option<Rect> {
    let items = value.as_array().filter(|a| a.len() == 4)?;
    let mut c = [0f64; 4];
    for (dst, v) in c.iter_mut().zip(items) {
        *dst = v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())?;
    }
    if c.iter().any(|v| v.abs() > 1.0) {
        c.iter_mut().for_each(|v| *v /= 1000.0);
    }
    let [nx0, ny0, nx1, ny1] = c.map(|v| v.clamp(0.0, 1.0));
    let (w, h) = (page_w as f64, page_h as f64);
    let (x0, x1) = ((nx0 * w).min(nx1 * w), (nx0 * w).max(nx1 * w));
    let (y0, y1) = ((ny0 * h).min(ny1 * h), (ny0 * h).max(ny1 * h));
    Some([x0, y0, x1, y1].map(|v| v as f32))
}

fn non_empty_str(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// Turns the model's JSON array into fields: pick-one kinds keep options with
/// usable rects (dropped if fewer than two); others need a rect, snapped to an
/// underscore anchor unless it is a checkbox.
fn parse_flat_fields(result: &Value, page_index: usize, page_w: f32, page_h: f32, anchors: &[Rect]) -> Vec<FlatField> {
    let Some(items) = result.as_array() else { return Vec::new() };
    let mut fields = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if !item.is_object() {
            continue;
        }
        let field_type = non_empty_str(&item["field_type"]).unwrap_or_else(|| "text".into()).trim().to_lowercase();
        let mut field = FlatField {
            field_name: format!("flat:p{page_index}:f{i}"),
            label: non_empty_str(&item["label"]).unwrap_or_else(|| format!("Blank {}", i + 1)).trim().to_string(),
            group: item["group"].as_str().map(str::to_string),
            context: item["context"].as_str().map(str::to_string),
            rect: None,
            options: Vec::new(),
            field_type,
        };
        if matches!(field.field_type.as_str(), "choice" | "select") {
            field.options = item["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|o| o.is_object())
                .filter_map(|o| {
                    let rect = norm_box_to_pts(&o["rect"], page_w, page_h)?;
                    Some(FlatOption { label: o["label"].as_str().unwrap_or("").trim().to_string(), rect })
                })
                .collect();
            if field.options.len() >= 2 {
                fields.push(field);
            }
            continue;
        }
        let Some(rect) = norm_box_to_pts(&item["rect"], page_w, page_h) else { continue };
        field.rect = Some(if field.field_type == "checkbox" { rect } else { snap_rect_to_anchor(rect, anchors) });
        fields.push(field);
    }
    fields
}

/// Uses Gemini vision to find fill-in blanks on a page with no widgets.
/// Cached per page under [`flat_form_cache_key`]; `force` re-queries. A model
/// failure logs and yields nothing; render or geometry failure is an error.
pub fn discover_flat_form_fields(
    pdf_path: &Path,
    page_index: usize,
    client: &GeminiClient,
    mode: GeminiMode,
    cancel: Option<&CancelToken>,
    force: bool,
) -> anyhow::Result<Vec<FlatField>> {
    let key = flat_form_cache_key(page_index);
    if !force && let Some(cached) = get_cached(pdf_path, &key) {
        return Ok(cached);
    }
    let image = render_page(pdf_path, page_index)?;
    let (page_w, page_h, anchors) = flat_form_page_geometry(pdf_path, page_index)?;
    let result = match client.generate_json_with_image(
        &image,
        "image/png",
        "Find the fill-in blanks on this page and return the JSON array.",
        FLAT_FORM_DISCOVERY_PROMPT,
        mode,
        cancel,
    ) {
        Ok(v) => v,
        Err(e) => {
            log::error!("Flat-form discovery failed for page {page_index}: {e:#}");
            return Ok(Vec::new());
        }
    };
    if !result.is_array() {
        return Ok(Vec::new());
    }
    let fields = parse_flat_fields(&result, page_index, page_w, page_h, &anchors);
    set_cached(pdf_path, &key, &fields);
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::FieldType;
    use serde_json::json;

    #[test]
    fn snapping_picks_best_overlap_within_vertical_reach() {
        let anchors = [[100.0, 200.0, 300.0, 212.0], [90.0, 230.0, 400.0, 242.0], [100.0, 400.0, 300.0, 412.0]];
        // Centre y 210: first anchor overlaps 150 with dy 4; second overlaps 150 with dy 26 (out of reach).
        assert_eq!(snap_rect_to_anchor([150.0, 200.0, 320.0, 220.0], &anchors), [100.0, 200.0, 300.0, 212.0]);
        assert_eq!(snap_rect_to_anchor([150.0, 600.0, 320.0, 620.0], &anchors), [150.0, 600.0, 320.0, 620.0]);
        assert_eq!(snap_rect_to_anchor([1.0, 2.0, 3.0, 4.0], &[]), [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn boxes_scale_from_0_1000_or_fractions_and_clamp() {
        assert_eq!(norm_box_to_pts(&json!([500, 250, 100, 1200]), 600.0, 800.0), Some([60.0, 200.0, 300.0, 800.0]));
        assert_eq!(norm_box_to_pts(&json!([0.5, "0.25", 1, 0.5]), 600.0, 800.0), Some([300.0, 200.0, 600.0, 400.0]));
        assert_eq!(norm_box_to_pts(&json!([1, 2, 3]), 600.0, 800.0), None);
        assert_eq!(norm_box_to_pts(&json!(null), 600.0, 800.0), None);
    }

    #[test]
    fn model_json_becomes_flat_fields() {
        let result = json!([
            { "label": " Full Name ", "field_type": "Text", "rect": [100, 100, 500, 120], "group": "Personal" },
            { "label": "", "field_type": "checkbox", "rect": [100, 100, 120, 120] },
            { "label": "Gender", "field_type": "select", "options": [
                { "label": "Male", "rect": [100, 500, 150, 520] }, { "label": "Female", "rect": [200, 500, 260, 520] } ] },
            { "label": "Lonely", "field_type": "choice", "options": [{ "label": "YES", "rect": [1, 1, 2, 2] }] },
            { "label": "No rect", "field_type": "date" },
            "junk",
        ]);
        let anchor = [55.0, 78.0, 330.0, 92.0];
        let fields = parse_flat_fields(&result, 2, 600.0, 800.0, &[anchor]);
        assert_eq!(fields.len(), 3);
        assert_eq!((fields[0].field_name.as_str(), fields[0].label.as_str(), fields[0].field_type.as_str()), ("flat:p2:f0", "Full Name", "text"));
        assert_eq!(fields[0].rect, Some(anchor), "text blank snaps to the underscore anchor");
        assert_eq!(fields[0].group.as_deref(), Some("Personal"));
        assert_eq!((fields[1].label.as_str(), fields[1].rect), ("Blank 2", Some([60.0, 80.0, 72.0, 96.0])), "checkbox keeps the AI box");
        assert_eq!(fields[2].options, vec![
            FlatOption { label: "Male".into(), rect: [60.0, 400.0, 90.0, 416.0] },
            FlatOption { label: "Female".into(), rect: [120.0, 400.0, 156.0, 416.0] },
        ]);
        assert_eq!(fields[2].rect, None);
    }

    #[test]
    fn labelling_prompt_lists_fields() {
        let field = FormField {
            page_index: 0,
            field_type: FieldType::CheckBox,
            name: "q1_yes".into(),
            value: String::new(),
            rect: [72.4, 100.6, 84.0, 112.0],
            choices: Vec::new(),
            label: None,
            group: None,
            synthetic: false,
            choice_rects: Vec::new(),
            mark_style: Default::default(),
        };
        assert_eq!(field_list_prompt(&[field]), "Form fields on this page:\n  - Name: q1_yes, Type: CheckBox, Position: (72, 101, 84, 112)");
    }
}
