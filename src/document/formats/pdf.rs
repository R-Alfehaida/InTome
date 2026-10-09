//! PDF documents, ported from prism/document/formats/pdf.py.
//!
//! pdfium (bundled pdfium.dll) reads text, outline, metadata and geometry and
//! renders pages. AcroForm reads and every write go through lopdf in [`forms`].
//! A [`PdfDocument`] is plain data extracted at open, so it is `Send` and never
//! holds the file open (saving a form in place rewrites the same path).

mod forms;

pub use forms::save_form;

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::anyhow;
use pdfium_render::prelude::*;
use serde_json::Value;

use crate::config::PdfReadingMode;
use crate::document::{
    Capabilities, DocMetadata, Document, FieldType, FormField, MarkStyle, OpenError, OpenOptions, Page, Rect, TocEntry,
    cache,
};
use crate::structured_text::elements::SemanticElement;

// Flat-form heuristic tuning. These thresholds only gate whether to *offer* AI
// discovery (the model decides what is a field), so they are deliberately loose.
const FLAT_FORM_UNDERSCORE_RUN: &str = "___";
const FLAT_FORM_MIN_LINE_LEN_PTS: f32 = 40.0;
const FLAT_FORM_MAX_LINE_THICKNESS_PTS: f32 = 2.0;

/// Pages with fewer trimmed chars than this need OCR.
const MIN_TEXT_CHARS: usize = 50;

/// Marker written into a flat form once it has been filled and saved.
pub const FLAT_FORM_FILLED_MARKER: &str = "intome-flatform-filled";
// Forms Prism filled carry its marker; honour it so they are not re-offered discovery.
const PRISM_FLAT_FORM_FILLED_MARKER: &str = "prism-flatform-filled";

/// Bump when the flat-form discovery output shape or coordinate maths changes,
/// so stale cached field definitions are abandoned rather than re-served.
pub const FLAT_FORM_CACHE_VERSION: &str = "v3";

/// Per-page cache key for AI-discovered flat-form fields.
pub fn flat_form_cache_key(page_index: usize) -> String {
    format!("flat_form_fields:{FLAT_FORM_CACHE_VERSION}:page:{page_index}")
}

/// Per-page cache key for AI-generated form field labels.
pub fn form_labels_cache_key(page_index: usize) -> String {
    format!("form_labels:page:{page_index}")
}

static PDFIUM: OnceLock<Result<Pdfium, String>> = OnceLock::new();

fn pdfium() -> anyhow::Result<&'static Pdfium> {
    PDFIUM.get_or_init(bind_pdfium).as_ref().map_err(|e| anyhow!("pdfium.dll could not be loaded: {e}"))
}

fn bind_pdfium() -> Result<Pdfium, String> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    // `cargo test` binaries run from target/<profile>/deps, one level below the dll build.rs copies.
    for dir in exe_dir.iter().flat_map(|d| [Some(d.as_path()), d.parent()]).flatten() {
        if let Ok(bindings) = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(dir)) {
            return Ok(Pdfium::new(bindings));
        }
    }
    Pdfium::bind_to_system_library().map(Pdfium::new).map_err(|e| e.to_string())
}

/// True when the bundled pdfium.dll loads.
pub fn available() -> bool {
    pdfium().is_ok()
}

/// Runs `f` with the process-wide pdfium binding. Every pdfium use in the app
/// (OCR rendering, flat-form geometry, form saving) goes through here.
pub fn with_pdfium<R>(f: impl FnOnce(&Pdfium) -> anyhow::Result<R>) -> anyhow::Result<R> {
    f(pdfium()?)
}

fn load<'a>(
    pdfium: &'a Pdfium,
    path: &Path,
    password: Option<&str>,
) -> Result<pdfium_render::prelude::PdfDocument<'a>, OpenError> {
    pdfium.load_pdf_from_file(path, password).map_err(|e| match e {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) if password.is_some() => {
            OpenError::WrongPassword
        }
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => OpenError::PasswordRequired,
        PdfiumError::IoError(io) => OpenError::Io(io),
        other => OpenError::Other(format!("Could not open the PDF: {other}")),
    })
}

fn page_at<'a>(doc: &'a pdfium_render::prelude::PdfDocument, index: usize) -> anyhow::Result<PdfPage<'a>> {
    Ok(doc.pages().get(PdfPageIndex::try_from(index)?)?)
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    Ok(Box::new(PdfDocument::open(path, opts)?))
}

/// Renders one page to PNG bytes at `dpi`, opening its own handle (thread-safe).
pub fn render_page_png(path: &Path, password: Option<&str>, page_index: usize, dpi: u32) -> anyhow::Result<Vec<u8>> {
    with_pdfium(|p| {
        let doc = load(p, path, password)?;
        let page = page_at(&doc, page_index)?;
        let bitmap = page.render_with_config(&PdfRenderConfig::new().scale_page_by_factor(dpi as f32 / 72.0))?;
        let mut png = Vec::new();
        bitmap.as_image()?.into_rgb8().write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
        Ok(png)
    })
}

/// Page size in points plus the boxes of underscore-run words on it.
#[derive(Debug, Clone, PartialEq)]
pub struct PageGeometry {
    pub width: f32,
    pub height: f32,
    /// Top-left-origin boxes of words containing "___": the one geometry that
    /// pins a blank's writable span exactly. Drawn rules are deliberately not
    /// anchors: on table forms they are full-width cell borders.
    pub anchors: Vec<Rect>,
}

/// Geometry AI flat-form discovery snaps discovered blanks to.
pub fn flat_form_page_geometry(path: &Path, password: Option<&str>, page_index: usize) -> anyhow::Result<PageGeometry> {
    with_pdfium(|p| {
        let doc = load(p, path, password)?;
        let page = page_at(&doc, page_index)?;
        let page_box = page_box(&page);
        let mut anchors = Vec::new();
        let mut word = String::new();
        let mut bbox: Option<Rect> = None;
        let mut flush = |word: &mut String, bbox: &mut Option<Rect>| {
            if let Some(b) = bbox.take().filter(|_| word.contains(FLAT_FORM_UNDERSCORE_RUN)) {
                anchors.push(b);
            }
            word.clear();
        };
        for ch in page.text()?.chars().iter() {
            let c = ch.unicode_char().unwrap_or(' ');
            if c.is_whitespace() {
                flush(&mut word, &mut bbox);
                continue;
            }
            word.push(c);
            if let Ok(b) = ch.loose_bounds() {
                let r = to_top_left(&b, page_box);
                bbox = Some(bbox.map_or(r, |u| union(u, r)));
            }
        }
        flush(&mut word, &mut bbox);
        Ok(PageGeometry { width: page.width().value, height: page.height().value, anchors })
    })
}

/// The page's crop box (falling back to the media box) in PDF space: [left, bottom, right, top].
fn page_box(page: &PdfPage) -> [f32; 4] {
    let b = page.boundaries();
    b.crop()
        .or_else(|_| b.media())
        .map(|bx| [bx.bounds.left().value, bx.bounds.bottom().value, bx.bounds.right().value, bx.bounds.top().value])
        .unwrap_or([0.0, 0.0, page.width().value, page.height().value])
}

// shortcut: ignores /Rotate, as Prism's geometry effectively does; upgrade if rotated forms show misplaced stamps.
fn to_top_left(r: &PdfRect, page_box: [f32; 4]) -> Rect {
    [r.left().value - page_box[0], page_box[3] - r.top().value, r.right().value - page_box[0], page_box[3] - r.bottom().value]
}

fn union(a: Rect, b: Rect) -> Rect {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

/// pdfium ends lines with "\r\n" and marks a line-end hyphen it kept as U+0002.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").replace('\u{2}', "-").replace('\0', "")
}

/// The widget text MuPDF (so Prism) reads from form appearances, so a filled
/// form's values are heard in the reading view: text and combo values, and
/// every list box option, in annotation order, each at its widget's rect.
fn widget_runs(page: &PdfPage, page_box: [f32; 4]) -> Vec<TextRun> {
    let mut runs = Vec::new();
    for annotation in page.annotations().iter() {
        let Some(field) = annotation.as_form_field() else { continue };
        let text = if let Some(t) = field.as_text_field() {
            t.value()
        } else if let Some(c) = field.as_combo_box_field() {
            c.value()
        } else if let Some(l) = field.as_list_box_field() {
            Some(l.options().iter().filter_map(|o| o.label().cloned()).collect::<Vec<_>>().join("\n"))
        } else {
            None
        };
        if let (Some(text), Ok(bounds)) = (text.filter(|t| !t.trim().is_empty()), annotation.bounds()) {
            // Anchor at the widget's vertical centre: its top edge sits above the label's
            // text, which would sort the value ahead of its own label.
            let [x0, y0, x1, y1] = to_top_left(&bounds, page_box);
            let cy = (y0 + y1) / 2.0;
            runs.push(TextRun { rect: [x0, cy - 0.5, x1, cy + 0.5], text });
        }
    }
    runs
}

fn page_text(page: &PdfPage, mode: PdfReadingMode) -> anyhow::Result<String> {
    let text = page.text()?;
    let page_box = page_box(page);
    let widgets = widget_runs(page, page_box);
    Ok(match mode {
        // PyMuPDF's physical-layout flags differ from its default only by not clipping
        // to the MediaBox; pdfium never clips, so both modes read the same text.
        // shortcut: Automatic keeps off-page text Prism clipped; clip via chars_inside_rect if that shows up.
        PdfReadingMode::Automatic | PdfReadingMode::PhysicalLayout => {
            // MuPDF emits annotation text after the page content.
            std::iter::once(normalize(&text.all())).chain(widgets.into_iter().map(|w| w.text)).collect::<Vec<_>>().join("\n")
        }
        PdfReadingMode::ReadingOrder => {
            let mut runs = line_runs(&text, page_box);
            runs.extend(widgets);
            reading_order_text(runs)
        }
    })
}

/// One run per extracted line, in one pass over the chars. (Segment `text()`
/// rescans the whole page per call: 47 ms/page against 7 ms on a 388-page book.)
fn line_runs(text: &PdfPageText, page_box: [f32; 4]) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut current = TextRun { rect: [0.0; 4], text: String::new() };
    let mut bbox: Option<Rect> = None;
    for ch in text.chars().iter() {
        match ch.unicode_char().unwrap_or(' ') {
            '\r' | '\n' => {
                if let Some(r) = bbox.take() {
                    current.rect = r;
                    runs.push(std::mem::replace(&mut current, TextRun { rect: [0.0; 4], text: String::new() }));
                }
            }
            c => {
                current.text.push(if c == '\u{2}' { '-' } else { c });
                if !c.is_whitespace()
                    && let Ok(b) = ch.loose_bounds()
                {
                    let r = to_top_left(&b, page_box);
                    bbox = Some(bbox.map_or(r, |u| union(u, r)));
                }
            }
        }
    }
    if let Some(r) = bbox {
        current.rect = r;
        runs.push(current);
    }
    runs
}

/// A run of text on one line, top-left-origin rect.
struct TextRun {
    rect: Rect,
    text: String,
}

/// Prism's reading-order mode: text blocks sorted by row (y rounded to 20pt),
/// then x, joined by blank lines. pdfium has no blocks, so runs are grouped
/// into one when they continue the same line or start the next line below it.
fn reading_order_text(runs: Vec<TextRun>) -> String {
    let mut blocks: Vec<(Rect, Rect, String)> = Vec::new(); // (block bbox, last run, text)
    for run in runs.into_iter().filter(|r| !r.text.trim().is_empty()) {
        let r = run.rect;
        let h = (r[3] - r[1]).max(1.0);
        let text = run.text.trim();
        match blocks.last_mut() {
            Some((bbox, last, acc)) if (r[1] - last[1]).abs() < h * 0.5 && (0.0..h * 2.0).contains(&(r[0] - last[2])) => {
                acc.push(' ');
                acc.push_str(text);
                *bbox = union(*bbox, r);
                *last = r;
            }
            Some((bbox, last, acc)) if r[1] > last[1] && r[1] - last[3] < h * 0.8 && r[0] < bbox[2] && r[2] > bbox[0] => {
                acc.push('\n');
                acc.push_str(text);
                *bbox = union(*bbox, r);
                *last = r;
            }
            _ => blocks.push((r, r, text.to_string())),
        }
    }
    blocks.sort_by(|a, b| {
        let row = |r: &Rect| (r[1] / 20.0).round_ties_even() * 20.0;
        row(&a.0).total_cmp(&row(&b.0)).then(a.0[0].total_cmp(&b.0[0]))
    });
    blocks.into_iter().map(|b| b.2).collect::<Vec<_>>().join("\n\n")
}

fn has_horizontal_rule(obj: &PdfPageObject) -> bool {
    match obj {
        PdfPageObject::Path(path) => {
            let Ok(matrix) = path.matrix() else { return false };
            let mut prev: Option<(f32, f32)> = None;
            for seg in path.segments().transform(matrix).iter() {
                let (x, y) = (seg.x().value, seg.y().value);
                if seg.segment_type() == PdfPathSegmentType::LineTo
                    && let Some((px, py)) = prev
                    && (y - py).abs() <= FLAT_FORM_MAX_LINE_THICKNESS_PTS
                    && (x - px).abs() >= FLAT_FORM_MIN_LINE_LEN_PTS
                {
                    return true;
                }
                prev = Some((x, y));
            }
            false
        }
        PdfPageObject::XObjectForm(form) => form.iter().any(|o| has_horizontal_rule(&o)),
        _ => false,
    }
}

/// Cheap heuristic: does this page look like it has static fill-in blanks
/// (an underscore run, or a long thin horizontal rule)? It only decides
/// whether to *offer* AI discovery, so false positives cost one empty AI call.
fn has_flat_form_candidates(page: &PdfPage) -> bool {
    page.text().is_ok_and(|t| t.all().contains(FLAT_FORM_UNDERSCORE_RUN))
        || page.objects().iter().any(|o| has_horizontal_rule(&o))
}

fn toc_entries(first: Option<PdfBookmark>, level: usize, budget: &mut usize) -> Vec<TocEntry> {
    let mut out = Vec::new();
    let mut next = first;
    while let Some(b) = next {
        // Budget and depth cap guard against cyclic outlines in malformed files.
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let children = if level < 64 { toc_entries(b.first_child(), level + 1, budget) } else { Vec::new() };
        out.push(TocEntry { title: b.title().unwrap_or_default(), page_index: bookmark_page(&b), level, children });
        next = b.next_sibling();
    }
    out
}

/// 0-based target page; 0 when the bookmark has no local destination (as Prism's max(0, -2)).
fn bookmark_page(b: &PdfBookmark) -> usize {
    let index = match b.destination() {
        Some(dest) => dest.page_index().ok(),
        None => b.action().and_then(|a| a.as_local_destination_action()?.destination().ok()?.page_index().ok()),
    };
    index.map_or(0, |i| i.max(0) as usize)
}

fn json_rect(v: Option<&Value>) -> Rect {
    let mut r = [0.0; 4];
    if let Some(a) = v.and_then(Value::as_array).filter(|a| a.len() == 4) {
        for (dst, src) in r.iter_mut().zip(a) {
            *dst = src.as_f64().unwrap_or(0.0) as f32;
        }
    }
    r
}

fn json_str(d: &Value, key: &str) -> Option<String> {
    d.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Prism's label precedence: a cached AI label (author-unlabeled field, or an
/// explicit relabel) wins over the author's `/TU`, which `field.label` already
/// holds; the group only ever comes from AI. Later cache entries win, as Prism's dict did.
fn apply_ai_label(field: &mut FormField, cached: &[Value]) {
    let ai = cached.iter().rev().find(|d| d.get("field_name").and_then(Value::as_str).unwrap_or("") == field.name);
    if let Some(d) = ai
        && let Some(label) = json_str(d, "label")
    {
        field.label = Some(label);
        field.group = d.get("group").and_then(Value::as_str).map(str::to_string);
    }
}

/// Maps one page's cached flat-form discovery output to synthetic fields.
fn flat_fields_from_cache(page_index: usize, cached: &[Value]) -> Vec<FormField> {
    cached
        .iter()
        .filter(|d| d.is_object())
        .map(|d| {
            let ftype = d.get("field_type").and_then(Value::as_str).unwrap_or("text").to_lowercase();
            let mut f = FormField {
                page_index,
                field_type: FieldType::Text,
                name: d.get("field_name").and_then(Value::as_str).unwrap_or("").to_string(),
                value: String::new(),
                rect: json_rect(d.get("rect")),
                choices: Vec::new(),
                label: json_str(d, "label"),
                group: d.get("group").and_then(Value::as_str).map(str::to_string),
                synthetic: true,
                choice_rects: Vec::new(),
                mark_style: MarkStyle::Text,
            };
            match ftype.as_str() {
                "choice" | "select" => {
                    let options = d.get("options").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
                    let rects: Vec<Rect> = options.iter().map(|o| json_rect(o.get("rect"))).collect();
                    // A leading blank entry lets the user leave it unanswered.
                    f.choices = std::iter::once(String::new())
                        .chain(options.iter().map(|o| o.get("label").and_then(Value::as_str).unwrap_or("").to_string()))
                        .collect();
                    f.choice_rects = std::iter::once(None).chain(rects.iter().copied().map(Some)).collect();
                    // Place the field at its options' bounding box so reading-order sorting finds it.
                    f.rect = rects.iter().copied().reduce(union).unwrap_or([0.0; 4]);
                    f.field_type = FieldType::RadioButton;
                    f.mark_style = if ftype == "select" { MarkStyle::SelectWord } else { MarkStyle::ChoiceBox };
                }
                "checkbox" => {
                    f.field_type = FieldType::CheckBox;
                    f.mark_style = MarkStyle::Checkbox;
                }
                _ => {} // text / date / signature
            }
            f
        })
        .collect()
}

/// PDF document. Plain data (no pdfium handles) so it is `Send`.
pub struct PdfDocument {
    path: PathBuf,
    password: Option<String>,
    pages: Vec<Page>,
    /// Trimmed char count of each page's extracted text at open (OCR detection).
    text_lens: Vec<usize>,
    toc: Vec<TocEntry>,
    metadata: DocMetadata,
    keywords: String,
    caps: Capabilities,
}

impl PdfDocument {
    /// Extracts everything eagerly on the calling (worker) thread.
    pub fn open(path: &Path, opts: &OpenOptions) -> Result<Self, OpenError> {
        let pdfium = pdfium().map_err(|e| {
            log::error!("{e:#}");
            OpenError::Unavailable("PDF".into())
        })?;
        let doc = load(pdfium, path, opts.password.as_deref())?;

        let mut caps = Capabilities::empty();
        let toc = toc_entries(doc.bookmarks().root(), 0, &mut 100_000);
        if !toc.is_empty() {
            caps |= Capabilities::TOC;
        }

        let count = doc.pages().len() as usize;
        let mut pages = Vec::with_capacity(count);
        let mut text_lens = Vec::with_capacity(count);
        let mut has_images = false;
        let mut has_widgets = false;
        for (i, page) in doc.pages().iter().enumerate() {
            let text = page_text(&page, opts.pdf_reading_mode).unwrap_or_else(|e| {
                log::warn!("PDF page {i} text extraction failed: {e:#}");
                String::new()
            });
            text_lens.push(text.trim().chars().count());
            pages.push(Page::plain(text));
            if i < 3 && !has_images {
                has_images = page.objects().iter().any(|o| o.object_type() == PdfPageObjectType::Image);
            }
            // Only the first pages are checked for widgets, as Prism does, to avoid scanning the whole file.
            if i < 5 && !has_widgets {
                has_widgets = page.annotations().iter().any(|a| a.annotation_type() == PdfPageAnnotationType::Widget);
            }
        }

        if text_lens.iter().take(3).any(|&n| n > MIN_TEXT_CHARS) {
            caps |= Capabilities::SEARCHABLE_TEXT;
        } else {
            caps |= Capabilities::NEEDS_OCR;
        }
        if has_images {
            caps |= Capabilities::IMAGES;
        }
        if has_widgets {
            caps |= Capabilities::FILLABLE_FORM;
        } else if (0..count.min(5)).any(|i| page_at(&doc, i).is_ok_and(|p| has_flat_form_candidates(&p))) {
            // A true AcroForm wins; static blanks only matter when there are no widgets.
            caps |= Capabilities::FLAT_FORM;
        }

        let meta = doc.metadata();
        let tag = |t| meta.get(t).map(|m| m.value().trim().to_string()).unwrap_or_default();
        let title = tag(PdfDocumentMetadataTagType::Title);
        let subject = tag(PdfDocumentMetadataTagType::Subject);
        let metadata = DocMetadata {
            title: if title.is_empty() {
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            } else {
                title
            },
            author: tag(PdfDocumentMetadataTagType::Author),
            format: "PDF".into(),
            extra: if subject.is_empty() { Vec::new() } else { vec![("subject".into(), subject)] },
        };
        let keywords = tag(PdfDocumentMetadataTagType::Keywords);

        Ok(Self {
            path: path.to_path_buf(),
            password: opts.password.clone(),
            pages,
            text_lens,
            toc,
            metadata,
            keywords,
            caps,
        })
    }

    /// The password the document was opened with, for reopening by path.
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// AI-discovered flat-form fields from the per-page cache; empty before
    /// discovery has run (or with caching disabled).
    pub fn flat_form_fields(&self) -> Vec<FormField> {
        (0..self.pages.len())
            .flat_map(|i| {
                let cached: Vec<Value> = cache::get_cached(&self.path, &flat_form_cache_key(i)).unwrap_or_default();
                flat_fields_from_cache(i, &cached)
            })
            .collect()
    }

    /// True if this PDF was already filled as a flat form, so the auto
    /// offer-on-open (and its wasted discovery call) is skipped.
    pub fn flat_form_already_filled(&self) -> bool {
        self.keywords.contains(FLAT_FORM_FILLED_MARKER) || self.keywords.contains(PRISM_FLAT_FORM_FILLED_MARKER)
    }

    /// Pages that look like they hold static fill-in blanks, to scope AI
    /// discovery. Empty unless the document was flagged `FLAT_FORM`. Reopens the file.
    pub fn flat_form_candidate_pages(&self) -> anyhow::Result<Vec<usize>> {
        if !self.caps.contains(Capabilities::FLAT_FORM) {
            return Ok(Vec::new());
        }
        with_pdfium(|p| {
            let doc = load(p, &self.path, self.password())?;
            Ok(doc.pages().iter().enumerate().filter(|(_, page)| has_flat_form_candidates(page)).map(|(i, _)| i).collect())
        })
    }
}

impl Document for PdfDocument {
    fn path(&self) -> &Path {
        &self.path
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    fn toc(&self) -> &[TocEntry] {
        &self.toc
    }

    fn metadata(&self) -> DocMetadata {
        self.metadata.clone()
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page(&mut self, index: usize) -> &Page {
        &self.pages[index]
    }

    fn set_ocr_result(&mut self, index: usize, text: String, elements: Vec<SemanticElement>) {
        if let Some(page) = self.pages.get_mut(index) {
            page.text = text;
            page.elements = elements;
        }
    }

    fn ocr_page_indices(&mut self) -> Vec<usize> {
        if !self.caps.contains(Capabilities::NEEDS_OCR) {
            return Vec::new();
        }
        self.text_lens.iter().enumerate().filter(|(_, n)| **n < MIN_TEXT_CHARS).map(|(i, _)| i).collect()
    }

    fn form_fields(&mut self) -> Vec<FormField> {
        let mut fields = match forms::read_form_fields(&self.path, self.password()) {
            Ok(f) => f,
            Err(e) => {
                log::error!("Reading form fields of {} failed: {e:#}", self.path.display());
                return Vec::new();
            }
        };
        let mut page = None;
        let mut cached: Vec<Value> = Vec::new();
        for f in &mut fields {
            if page != Some(f.page_index) {
                page = Some(f.page_index);
                cached = cache::get_cached(&self.path, &form_labels_cache_key(f.page_index)).unwrap_or_default();
            }
            apply_ai_label(f, &cached);
        }
        fields
    }

    fn as_pdf(&mut self) -> Option<&mut PdfDocument> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(name: &str, tu: Option<&str>) -> FormField {
        FormField {
            page_index: 0,
            field_type: FieldType::Text,
            name: name.into(),
            value: String::new(),
            rect: [0.0; 4],
            choices: Vec::new(),
            label: tu.map(str::to_string),
            group: None,
            synthetic: false,
            choice_rects: Vec::new(),
            mark_style: MarkStyle::Widget,
        }
    }

    #[test]
    fn label_precedence_ai_then_tu_then_none() {
        let cached = vec![
            json!({"field_name": "name", "label": "Old AI label"}),
            json!({"field_name": "name", "label": "Applicant name", "group": "Applicant"}),
            json!({"field_name": "email", "label": ""}),
        ];
        let mut ai_wins = field("name", Some("Your full name"));
        apply_ai_label(&mut ai_wins, &cached);
        assert_eq!((ai_wins.label.as_deref(), ai_wins.group.as_deref()), (Some("Applicant name"), Some("Applicant")));

        let mut tu_kept = field("email", Some("Email address"));
        apply_ai_label(&mut tu_kept, &cached);
        assert_eq!((tu_kept.label.as_deref(), tu_kept.group), (Some("Email address"), None));

        let mut unlabeled = field("phone", None);
        apply_ai_label(&mut unlabeled, &cached);
        assert_eq!(unlabeled.label, None);
    }

    #[test]
    fn flat_choice_field_gets_blank_entry_and_option_bbox() {
        let cached = vec![json!({
            "field_type": "select", "field_name": "title", "label": "Title",
            "options": [{"label": "Mr", "rect": [100, 50, 120, 60]}, {"label": "Ms", "rect": [130, 52, 150, 62]}],
        })];
        let f = &flat_fields_from_cache(2, &cached)[0];
        assert_eq!(f.choices, vec!["", "Mr", "Ms"]);
        assert_eq!(f.choice_rects, vec![None, Some([100.0, 50.0, 120.0, 60.0]), Some([130.0, 52.0, 150.0, 62.0])]);
        assert_eq!(f.rect, [100.0, 50.0, 150.0, 62.0]);
        assert_eq!((f.field_type, f.mark_style, f.page_index, f.synthetic), (FieldType::RadioButton, MarkStyle::SelectWord, 2, true));
    }

    #[test]
    fn reading_order_groups_lines_into_blocks_and_sorts_by_row_then_column() {
        let run = |x0, y0, x1, text: &str| TextRun { rect: [x0, y0, x1, y0 + 10.0], text: text.into() };
        // Content order: right column first, then the left column's two lines.
        let text = reading_order_text(vec![
            run(300.0, 100.0, 400.0, "Right one"),
            run(300.0, 112.0, 400.0, "Right two"),
            run(50.0, 101.0, 150.0, "Left one"),
            run(155.0, 101.0, 200.0, "cont"),
            run(50.0, 113.0, 150.0, "Left two"),
            run(50.0, 400.0, 150.0, "Footer"),
        ]);
        assert_eq!(text, "Left one cont\nLeft two\n\nRight one\nRight two\n\nFooter");
    }
}

/// Tests against the generated fixtures in tests/fixtures (need pdfium.dll next to target/<profile>).
#[cfg(test)]
mod fixture_tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    fn open_fixture(name: &str, password: Option<&str>) -> Result<PdfDocument, OpenError> {
        PdfDocument::open(&fixture(name), &OpenOptions { password: password.map(str::to_string), ..Default::default() })
    }

    fn outline(entries: &[TocEntry]) -> Vec<(usize, String, usize)> {
        entries.iter().flat_map(|e| std::iter::once((e.level, e.title.clone(), e.page_index)).chain(outline(&e.children))).collect()
    }

    #[test]
    fn toc_is_nested_from_bookmarks() {
        let doc = open_fixture("text_toc.pdf", None).unwrap();
        let toc = doc.toc();
        assert_eq!(toc.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), ["Part One", "Part Two"]);
        assert_eq!(toc[0].children[0].children[0].title, "Section 1.1");
        assert_eq!(
            outline(toc),
            [
                (0, "Part One".into(), 0),
                (1, "Chapter 1".into(), 1),
                (2, "Section 1.1".into(), 2),
                (1, "Chapter 2".into(), 3),
                (0, "Part Two".into(), 4),
                (1, "Chapter 3".into(), 5),
            ]
        );
        assert_eq!(doc.capabilities(), Capabilities::TOC | Capabilities::SEARCHABLE_TEXT);
        assert_eq!((doc.metadata.title.as_str(), doc.metadata.author.as_str()), ("Fixture Handbook", "InTome Tests"));
    }

    #[test]
    fn password_errors_are_typed() {
        assert!(matches!(open_fixture("password.pdf", None), Err(OpenError::PasswordRequired)));
        assert!(matches!(open_fixture("password.pdf", Some("nope")), Err(OpenError::WrongPassword)));
        let mut doc = open_fixture("password.pdf", Some("secret")).unwrap();
        assert!(doc.page(0).text.contains("Part One"));
    }

    #[test]
    fn scanned_page_needs_ocr() {
        let mut doc = open_fixture("scanned.pdf", None).unwrap();
        assert_eq!(doc.capabilities(), Capabilities::NEEDS_OCR | Capabilities::IMAGES);
        assert_eq!(doc.ocr_page_indices(), [0]);
    }

    #[test]
    fn flat_form_is_detected_with_underscore_anchors() {
        let doc = open_fixture("flat_form.pdf", None).unwrap();
        assert!(doc.capabilities().contains(Capabilities::FLAT_FORM));
        assert_eq!(doc.flat_form_candidate_pages().unwrap(), [0]);
        let geo = flat_form_page_geometry(&fixture("flat_form.pdf"), None, 0).unwrap();
        assert_eq!((geo.width.round(), geo.height.round(), geo.anchors.len()), (595.0, 842.0, 2));
    }

    #[test]
    fn acroform_fields_resolve_names_types_and_tu() {
        let doc = open_fixture("acroform.pdf", None).unwrap();
        assert!(doc.capabilities().contains(Capabilities::FILLABLE_FORM));
        let fields = forms::read_form_fields(&fixture("acroform.pdf"), None).unwrap();
        let summary: Vec<_> =
            fields.iter().map(|f| (f.name.as_str(), f.field_type, f.value.as_str(), f.label.as_deref())).collect();
        assert_eq!(
            summary,
            [
                ("name", FieldType::Text, "", Some("Your full name")),
                ("email", FieldType::Text, "old@example.com", None),
                ("addr.city", FieldType::Text, "", Some("City of residence")),
                ("agree", FieldType::CheckBox, "Off", Some("I agree to the terms")),
                ("colour", FieldType::RadioButton, "Off", Some("Favourite colour")),
                ("country", FieldType::ComboBox, "Malaysia", None),
                ("fruit", FieldType::ListBox, "", Some("Pick a fruit")),
            ]
        );
        assert_eq!(fields[4].choices, ["Red", "Green", "Blue"]);
        assert_eq!(fields[4].rect, [250.0, 233.0, 364.0, 247.0]);
        assert_eq!(fields[6].choices, ["Apple", "Banana", "Cherry"]);
        assert_eq!(fields[0].rect, [180.0, 67.0, 400.0, 87.0]);
    }

    /// Also leaves its outputs in target/pdf-roundtrip for tests/fixtures/verify_pdf_roundtrip.py.
    #[test]
    fn save_form_round_trips_every_field_type_and_stamps() {
        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/pdf-roundtrip");
        std::fs::create_dir_all(&out_dir).unwrap();

        let src = fixture("acroform.pdf");
        let mut fields = forms::read_form_fields(&src, None).unwrap();
        fields[1].label = Some("Email address".into());
        let values = ["Jane Doe", "jane@example.com", "Kuala Lumpur", "Yes", "Green", "Japan", "Banana"];
        let values: Vec<Option<String>> = values.iter().map(|v| Some(v.to_string())).collect();
        let out = out_dir.join("acroform_filled.pdf");
        save_form(&src, None, &fields, &values, &out).unwrap();
        let saved = forms::read_form_fields(&out, None).unwrap();
        let summary: Vec<_> = saved.iter().map(|f| (f.value.as_str(), f.label.as_deref())).collect();
        assert_eq!(
            summary,
            [
                ("Jane Doe", Some("Your full name")),
                ("jane@example.com", Some("Email address")),
                ("Kuala Lumpur", Some("City of residence")),
                ("Yes", Some("I agree to the terms")),
                ("Green", Some("Favourite colour")),
                ("Japan", None),
                ("Banana", Some("Pick a fruit")),
            ]
        );

        let stamp = |page_index, rect, style, choices: Vec<&str>, choice_rects| FormField {
            page_index,
            field_type: FieldType::Text,
            name: String::new(),
            value: String::new(),
            rect,
            choices: choices.into_iter().map(str::to_string).collect(),
            label: None,
            group: None,
            synthetic: true,
            choice_rects,
            mark_style: style,
        };
        let geo = flat_form_page_geometry(&fixture("flat_form.pdf"), None, 0).unwrap();
        let flat = [
            stamp(0, geo.anchors[0], MarkStyle::Text, vec![], vec![]),
            stamp(0, [40.0, 160.0, 52.0, 172.0], MarkStyle::Checkbox, vec![], vec![]),
        ];
        let flat_values = [Some("Ada Lovelace".to_string()), Some("Yes".to_string())];
        let out = out_dir.join("flat_form_filled.pdf");
        save_form(&fixture("flat_form.pdf"), None, &flat, &flat_values, &out).unwrap();
        let mut reopened = PdfDocument::open(&out, &OpenOptions::default()).unwrap();
        assert!(reopened.flat_form_already_filled());
        assert!(reopened.page(0).text.contains("Ada Lovelace"));

        // The original AES-256 encryption survives a save.
        let out = out_dir.join("password_stamped.pdf");
        let note = [stamp(0, [72.0, 700.0, 300.0, 714.0], MarkStyle::Text, vec![], vec![])];
        save_form(&fixture("password.pdf"), Some("secret"), &note, &[Some("Stamped note".into())], &out).unwrap();
        let open = |pw: Option<&str>| PdfDocument::open(&out, &OpenOptions { password: pw.map(str::to_string), ..Default::default() });
        assert!(matches!(open(None), Err(OpenError::PasswordRequired)));
        assert!(open(Some("secret")).unwrap().page(0).text.contains("Stamped note"));
    }

    #[test]
    fn render_page_png_has_png_signature() {
        let png = render_page_png(&fixture("two_column.pdf"), None, 0, 72).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}
