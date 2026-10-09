//! AcroForm reads and every PDF write, via lopdf. Ported from Prism's
//! `form_dialog._write_values_to_pdf` and helpers.
//!
//! lopdf rather than pdfium: pdfium-render only writes a widget's own `/V`
//! (not the field's), regenerates no appearance, and cannot set `/TU` or
//! choice values. lopdf does all of it in one load/save that keeps encryption.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::Path;

use anyhow::Context;
use lopdf::{Dictionary, LoadOptions, Object, ObjectId, decode_text_string, text_string};

use super::FLAT_FORM_FILLED_MARKER;
use crate::document::{FieldType, FormField, MarkStyle, Rect};

/// Checkbox values that mean "checked" (what the form dialog writes is "Yes"/"Off").
const CHECKED_VALUES: [&str; 4] = ["yes", "on", "true", "1"];
const STAMP_FONT: &str = "IntomeHelv";

fn load(path: &Path, password: Option<&str>) -> anyhow::Result<lopdf::Document> {
    let opts = password.map(LoadOptions::with_password).unwrap_or_default();
    let bytes = indirect_encrypt(std::fs::read(path)?);
    lopdf::Document::load_mem_with_options(&bytes, opts).with_context(|| format!("Could not read {}", path.display()))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// Index just past the `>>` closing the dictionary that opens at `start`,
/// skipping literal and hex strings (binary string bytes may look like `>>`).
fn dict_end(pdf: &[u8], start: usize) -> Option<usize> {
    let (mut i, mut depth) = (start, 0usize);
    while i < pdf.len() {
        match pdf[i] {
            b'<' if pdf.get(i + 1) == Some(&b'<') => (depth, i) = (depth + 1, i + 2),
            b'>' if pdf.get(i + 1) == Some(&b'>') => {
                (depth, i) = (depth - 1, i + 2);
                if depth == 0 {
                    return Some(i);
                }
            }
            b'<' => i += find(&pdf[i..], b">")? + 1,
            b'(' => {
                let mut nest = 0;
                while i < pdf.len() {
                    match pdf[i] {
                        b'\\' => i += 1,
                        b'(' => nest += 1,
                        b')' if nest == 1 => break,
                        b')' => nest -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// lopdf 0.45 only follows an indirect `/Encrypt`; MuPDF (so also files Prism
/// saved) writes it inline in the trailer, and lopdf then loads no objects.
/// Appending an incremental update that moves the dictionary into an object of
/// its own makes the same file readable. Returns `pdf` unchanged otherwise.
// shortcut: classic trailers only; an inline /Encrypt in an xref stream still fails to load, with an error.
fn indirect_encrypt(pdf: Vec<u8>) -> Vec<u8> {
    let patch = || -> Option<Vec<u8>> {
        let trailer = rfind(&pdf, b"trailer")?;
        let open = trailer + find(&pdf[trailer..], b"<<")?;
        let close = dict_end(&pdf, open)?;
        let key = open + find(&pdf[open..close], b"/Encrypt")? + b"/Encrypt".len();
        let value = key + pdf[key..].iter().take_while(|b| b.is_ascii_whitespace()).count();
        if !pdf[value..].starts_with(b"<<") {
            return None;
        }
        let value_end = dict_end(&pdf, value)?;
        let num = |re: &str, text: &[u8]| -> Option<u64> {
            let caps = regex::bytes::Regex::new(re).ok()?.captures(text)?;
            std::str::from_utf8(&caps[1]).ok()?.parse().ok()
        };
        let prev = num(r"startxref\s+(\d+)", &pdf[rfind(&pdf, b"startxref")?..])?;
        let size = num(r"/Size\s+(\d+)", &pdf[open..close])?;

        let mut dict = [&pdf[open + 2..value], format!(" {size} 0 R").as_bytes(), &pdf[value_end..close - 2]].concat();
        dict = regex::bytes::Regex::new(r"/(Prev|Size)\s+\d+").ok()?.replace_all(&dict, &b""[..]).into_owned();
        let mut out = pdf.clone();
        let obj_offset = out.len() + 1;
        out.extend_from_slice(format!("\n{size} 0 obj\n").as_bytes());
        out.extend_from_slice(&pdf[value..value_end]);
        out.extend_from_slice(b"\nendobj\n");
        let xref_offset = out.len();
        out.extend_from_slice(
            format!("xref\n0 1\n0000000000 65535 f \n{size} 1\n{obj_offset:010} 00000 n \ntrailer\n<< /Size {} /Prev {prev}", size + 1)
                .as_bytes(),
        );
        out.extend_from_slice(&dict);
        out.extend_from_slice(format!(" >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
        Some(out)
    };
    patch().unwrap_or(pdf)
}

fn deref<'a>(doc: &'a lopdf::Document, obj: &'a Object) -> Option<&'a Object> {
    doc.dereference(obj).ok().map(|(_, o)| o)
}

fn get<'a>(doc: &'a lopdf::Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    dict.get(key).ok().and_then(|o| deref(doc, o))
}

fn refs(doc: &lopdf::Document, dict: &Dictionary, key: &[u8]) -> Vec<ObjectId> {
    get(doc, dict, key)
        .and_then(|o| o.as_array().ok())
        .map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect())
        .unwrap_or_default()
}

fn name(obj: Option<&Object>) -> Option<String> {
    obj.and_then(|o| o.as_name().ok()).map(|n| String::from_utf8_lossy(n).into_owned())
}

fn text(obj: Option<&Object>) -> Option<String> {
    obj.and_then(|o| decode_text_string(o).ok())
}

/// A terminal AcroForm field: the node that carries the value, plus its widgets.
struct Terminal {
    id: ObjectId,
    name: String,
    kind: FieldType,
    widgets: Vec<ObjectId>,
}

fn field_kind(ft: Option<&[u8]>, ff: i64) -> FieldType {
    match ft {
        Some(b"Tx") => FieldType::Text,
        Some(b"Btn") if ff & (1 << 16) != 0 => FieldType::Button,
        Some(b"Btn") if ff & (1 << 15) != 0 => FieldType::RadioButton,
        Some(b"Btn") => FieldType::CheckBox,
        Some(b"Ch") if ff & (1 << 17) != 0 => FieldType::ComboBox,
        Some(b"Ch") => FieldType::ListBox,
        Some(b"Sig") => FieldType::Signature,
        _ => FieldType::Unknown,
    }
}

fn terminals(doc: &lopdf::Document) -> Vec<Terminal> {
    let mut out = Vec::new();
    let Some(acroform) = doc.catalog().ok().and_then(|c| get(doc, c, b"AcroForm")).and_then(|o| o.as_dict().ok()) else {
        return out;
    };
    for id in refs(doc, acroform, b"Fields") {
        walk(doc, id, "", None, 0, 0, &mut out);
    }
    out
}

/// Walks the field tree, building fully qualified names ("parent.child") as
/// PyMuPDF's `field_name` does and inheriting `/FT` and `/Ff`.
fn walk(doc: &lopdf::Document, id: ObjectId, parent: &str, ft: Option<&[u8]>, ff: i64, depth: usize, out: &mut Vec<Terminal>) {
    let Ok(dict) = doc.get_dictionary(id) else { return };
    if depth > 32 {
        return; // cyclic /Kids in a malformed file
    }
    let name = match text(dict.get(b"T").ok()) {
        Some(t) if parent.is_empty() => t,
        Some(t) => format!("{parent}.{t}"),
        None => parent.to_string(),
    };
    let ft = dict.get(b"FT").and_then(Object::as_name).ok().or(ft);
    let ff = dict.get(b"Ff").and_then(Object::as_i64).unwrap_or(ff);
    let kids = refs(doc, dict, b"Kids");
    let (fields, widgets): (Vec<ObjectId>, Vec<ObjectId>) =
        kids.iter().partition(|k| doc.get_dictionary(**k).is_ok_and(|d| d.has(b"T")));
    for k in fields {
        walk(doc, k, &name, ft, ff, depth + 1, out);
    }
    let widgets = if kids.is_empty() { vec![id] } else { widgets };
    if !widgets.is_empty() {
        out.push(Terminal { id, name, kind: field_kind(ft, ff), widgets });
    }
}

/// Widget id -> (page index, position in the page's /Annots).
fn widget_placement(doc: &lopdf::Document, pages: &[ObjectId]) -> HashMap<ObjectId, (usize, usize)> {
    let mut map = HashMap::new();
    for (i, page) in pages.iter().enumerate() {
        if let Ok(dict) = doc.get_dictionary(*page) {
            for (j, a) in refs(doc, dict, b"Annots").into_iter().enumerate() {
                map.entry(a).or_insert((i, j));
            }
        }
    }
    map
}

fn inherited<'a>(doc: &'a lopdf::Document, mut id: ObjectId, key: &[u8]) -> Option<&'a Object> {
    for _ in 0..32 {
        let dict = doc.get_dictionary(id).ok()?;
        if let Some(v) = get(doc, dict, key) {
            return Some(v);
        }
        id = dict.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

fn pdf_rect(obj: Option<&Object>) -> Option<[f32; 4]> {
    let a = obj?.as_array().ok()?;
    let v: Vec<f32> = a.iter().filter_map(|o| o.as_float().ok()).collect();
    (v.len() == 4).then(|| [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

/// Crop box (else media box) in PDF space, matching the pdfium side's origin.
fn page_box(doc: &lopdf::Document, page: ObjectId) -> [f32; 4] {
    pdf_rect(inherited(doc, page, b"CropBox"))
        .or_else(|| pdf_rect(inherited(doc, page, b"MediaBox")))
        .unwrap_or([0.0, 0.0, 612.0, 792.0])
}

fn to_top_left(r: [f32; 4], bx: [f32; 4]) -> Rect {
    [r[0] - bx[0], bx[3] - r[3], r[2] - bx[0], bx[3] - r[1]]
}

/// Name of a checkbox/radio widget's "on" appearance state.
fn on_state(doc: &lopdf::Document, widget: ObjectId) -> Option<String> {
    let dict = doc.get_dictionary(widget).ok()?;
    let normal = get(doc, dict, b"AP").and_then(|o| o.as_dict().ok()).and_then(|ap| get(doc, ap, b"N"))?;
    normal.as_dict().ok()?.iter().map(|(k, _)| k).find(|k| k.as_slice() != b"Off").map(|k| String::from_utf8_lossy(k).into_owned())
}

/// Choice options as (export value, display text).
fn options(doc: &lopdf::Document, field: &Dictionary) -> Vec<(String, String)> {
    let Some(opt) = get(doc, field, b"Opt").and_then(|o| o.as_array().ok()) else { return Vec::new() };
    opt.iter()
        .filter_map(|o| match deref(doc, o)? {
            Object::Array(pair) if pair.len() == 2 => Some((text(Some(&pair[0]))?, text(Some(&pair[1]))?)),
            other => text(Some(other)).map(|s| (s.clone(), s)),
        })
        .collect()
}

/// The document's widget fields, one per terminal field (a radio group is one
/// field whose choices are its buttons' export values), ordered by page and
/// annotation order. `label` holds the author's `/TU`, if any.
pub(super) fn read_form_fields(path: &Path, password: Option<&str>) -> anyhow::Result<Vec<FormField>> {
    let doc = load(path, password)?;
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    let placement = widget_placement(&doc, &pages);
    let mut out: Vec<((usize, usize), FormField)> = Vec::new();
    for t in terminals(&doc) {
        // Widgets not listed in any page's /Annots are invisible; PyMuPDF skips them too.
        let Some(&(page, order)) = t.widgets.iter().filter_map(|w| placement.get(w)).min() else { continue };
        let Ok(dict) = doc.get_dictionary(t.id) else { continue };
        let bx = page_box(&doc, pages[page]);
        let rects: Vec<Rect> = t
            .widgets
            .iter()
            .filter(|w| placement.get(w).is_some_and(|p| p.0 == page))
            .filter_map(|w| pdf_rect(doc.get_dictionary(*w).ok().and_then(|d| get(&doc, d, b"Rect"))))
            .map(|r| to_top_left(r, bx))
            .collect();
        let rect = match t.kind {
            FieldType::RadioButton => rects.iter().copied().reduce(super::union),
            _ => rects.first().copied(),
        }
        .unwrap_or([0.0; 4]);
        let opts = options(&doc, dict);
        let v = get(&doc, dict, b"V");
        let (value, choices) = match t.kind {
            FieldType::Text => (text(v).unwrap_or_default(), Vec::new()),
            FieldType::CheckBox => {
                let first_as = doc.get_dictionary(t.widgets[0]).ok().and_then(|d| name(get(&doc, d, b"AS")));
                (name(v).or(first_as).unwrap_or_else(|| "Off".into()), Vec::new())
            }
            FieldType::RadioButton => {
                let mut on: Vec<String> = t.widgets.iter().filter_map(|w| on_state(&doc, *w)).collect();
                on.dedup();
                (name(v).unwrap_or_else(|| "Off".into()), on)
            }
            FieldType::ComboBox | FieldType::ListBox => {
                let export = match v {
                    Some(Object::Array(a)) => text(a.first()),
                    other => text(other),
                }
                .unwrap_or_default();
                let display = opts.iter().find(|(e, _)| *e == export).map_or(export, |(_, d)| d.clone());
                (display, opts.into_iter().map(|(_, d)| d).collect())
            }
            _ => (String::new(), Vec::new()),
        };
        out.push((
            (page, order),
            FormField {
                page_index: page,
                field_type: t.kind,
                name: t.name,
                value,
                rect,
                choices,
                label: text(dict.get(b"TU").ok()).filter(|s| !s.is_empty()),
                group: None,
                synthetic: false,
                choice_rects: Vec::new(),
                mark_style: MarkStyle::Widget,
            },
        ));
    }
    out.sort_by_key(|(k, _)| *k);
    Ok(out.into_iter().map(|(_, f)| f).collect())
}

fn is_checked(value: &str) -> bool {
    CHECKED_VALUES.contains(&value.trim().to_lowercase().as_str())
}

/// Writes `value` into a widget field. Returns true when viewers must
/// regenerate its appearance (text and choice fields).
fn set_value(doc: &mut lopdf::Document, t: &Terminal, value: &str) -> anyhow::Result<bool> {
    let on: Vec<Option<String>> = t.widgets.iter().map(|w| on_state(doc, *w)).collect();
    let set_as = |doc: &mut lopdf::Document, states: Vec<String>| -> anyhow::Result<()> {
        for (w, s) in t.widgets.iter().zip(states) {
            doc.get_dictionary_mut(*w)?.set("AS", Object::Name(s.into_bytes()));
        }
        Ok(())
    };
    let clear_appearances = |doc: &mut lopdf::Document| -> anyhow::Result<()> {
        for w in &t.widgets {
            doc.get_dictionary_mut(*w)?.remove(b"AP");
        }
        Ok(())
    };
    match t.kind {
        FieldType::Text => {
            doc.get_dictionary_mut(t.id)?.set("V", text_string(value));
            clear_appearances(doc)?;
            Ok(true)
        }
        FieldType::CheckBox => {
            let checked = is_checked(value) || on.iter().flatten().any(|s| s == value);
            let states: Vec<String> =
                on.iter().map(|s| if checked { s.clone().unwrap_or_else(|| "Yes".into()) } else { "Off".into() }).collect();
            doc.get_dictionary_mut(t.id)?.set("V", Object::Name(states[0].clone().into_bytes()));
            set_as(doc, states)?;
            Ok(false)
        }
        FieldType::RadioButton => {
            let states: Vec<String> =
                on.iter().map(|s| s.as_deref().filter(|s| *s == value).unwrap_or("Off").to_string()).collect();
            let v = if states.iter().any(|s| s == value) { value } else { "Off" };
            doc.get_dictionary_mut(t.id)?.set("V", Object::Name(v.as_bytes().to_vec()));
            set_as(doc, states)?;
            Ok(false)
        }
        FieldType::ComboBox | FieldType::ListBox => {
            let opts = options(doc, doc.get_dictionary(t.id)?);
            let index = opts.iter().position(|(_, d)| d == value);
            let export = index.map_or(value, |i| opts[i].0.as_str()).to_string();
            let dict = doc.get_dictionary_mut(t.id)?;
            dict.set("V", text_string(&export));
            // List boxes render their selection from /I.
            match index {
                Some(i) if t.kind == FieldType::ListBox => dict.set("I", vec![Object::Integer(i as i64)]),
                _ => {
                    dict.remove(b"I");
                }
            }
            clear_appearances(doc)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// One drawing operation for a flat-form stamp, in top-left page points.
#[derive(Debug, Clone, PartialEq)]
enum Mark {
    /// White fill over the blank so the value reads as filled in.
    Cover(Rect),
    /// Helvetica text on a baseline starting at (x, y).
    Text { x: f32, y: f32, size: f32, text: String },
    /// 1pt black line from (x0, y) to (x1, y).
    Underline { x0: f32, x1: f32, y: f32 },
}

/// Helvetica advance widths (1/1000 em) for ASCII 32..=126.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556,
    556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833,
    722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556,
    556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334,
    260, 334, 584,
];

fn helvetica_width(text: &str, size: f32) -> f32 {
    let units: u32 = text
        .chars()
        .map(|c| match c as u32 {
            n @ 32..=126 => HELVETICA_WIDTHS[(n - 32) as usize] as u32,
            _ => 556,
        })
        .sum();
    units as f32 * size / 1000.0
}

/// An "X" sized to fit inside a checkbox or option box.
fn x_mark(r: Rect) -> Vec<Mark> {
    let (w, h) = (r[2] - r[0], r[3] - r[1]);
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    vec![Mark::Text { x: r[0] + 0.5, y: r[3] - 1.0, size: w.min(h).max(7.0), text: "X".into() }]
}

/// The marks that fill a synthetic flat-form field with `value`:
/// text stamps the value on a whited-out blank, Checkbox draws an X when
/// checked, ChoiceBox an X in the chosen option's box, SelectWord underlines
/// the chosen printed word.
fn marks_for(field: &FormField, value: &str, page_width: f32) -> Vec<Mark> {
    let option_rect = || {
        let i = field.choices.iter().position(|c| c == value)?;
        field.choice_rects.get(i).copied().flatten()
    };
    match field.mark_style {
        MarkStyle::Checkbox if is_checked(value) => x_mark(field.rect),
        MarkStyle::Checkbox => Vec::new(),
        MarkStyle::ChoiceBox => option_rect().map(x_mark).unwrap_or_default(),
        MarkStyle::SelectWord => {
            option_rect().map(|r| vec![Mark::Underline { x0: r[0], x1: r[2], y: r[3] + 1.0 }]).unwrap_or_default()
        }
        MarkStyle::Text | MarkStyle::Widget => {
            let [x0, y0, x1, y1] = field.rect;
            // A single non-wrapping run: shrink the font before it would run off the page.
            let avail = (page_width - 4.0) - (x0 + 2.0);
            let size = [11.0, 9.0, 7.0].into_iter().find(|s| helvetica_width(value, *s) <= avail).unwrap_or(7.0);
            vec![
                Mark::Cover([x0, y0 - 0.5, x1.max(x0 + 4.0), y1 + 1.5]),
                Mark::Text { x: x0 + 2.0, y: y1 - 3.0, size, text: value.to_string() },
            ]
        }
    }
}

/// PDF literal string bytes in WinAnsi (the stamp font's encoding).
// shortcut: chars outside Latin-1 print as "?" (Prism's base-14 "helv" had the same limit); embed a font if users need them.
fn pdf_literal(text: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    for c in text.chars() {
        let b = match c as u32 {
            n @ (0x20..=0x7E | 0xA0..=0xFF) => n as u8,
            _ => b'?',
        };
        if matches!(b, b'(' | b')' | b'\\') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b')');
    out
}

/// Content-stream operators for `marks`, converting top-left points to PDF space.
fn marks_content(marks: &[Mark], bx: [f32; 4]) -> Vec<u8> {
    let (px, py) = (|x: f32| x + bx[0], |y: f32| bx[3] - y);
    let mut out = Vec::new();
    for m in marks {
        // Writes to a Vec cannot fail.
        let _ = match m {
            Mark::Cover([x0, y0, x1, y1]) => {
                writeln!(out, "q 1 g {} {} {} {} re f Q", px(*x0), py(*y1), x1 - x0, y1 - y0)
            }
            Mark::Text { x, y, size, text } => {
                write!(out, "BT /{STAMP_FONT} {size} Tf 0 g {} {} Td ", px(*x), py(*y))
                    .and_then(|_| out.write_all(&pdf_literal(text)))
                    .and_then(|_| writeln!(out, " Tj ET"))
            }
            Mark::Underline { x0, x1, y } => writeln!(out, "q 0 G 1 w {} {} m {} {} l S Q", px(*x0), py(*y), px(*x1), py(*y)),
        };
    }
    out
}

/// Appends `content` to a page inside a fresh graphics state (the page's own
/// content is wrapped in q/Q so a leftover transform cannot skew the stamp),
/// with the stamp font added to the page's own copy of its resources.
fn stamp_page(doc: &mut lopdf::Document, page: ObjectId, font: ObjectId, content: Vec<u8>) -> anyhow::Result<()> {
    let mut resources = inherited(doc, page, b"Resources").and_then(|o| o.as_dict().ok()).cloned().unwrap_or_default();
    let mut fonts = get(doc, &resources, b"Font").and_then(|o| o.as_dict().ok()).cloned().unwrap_or_default();
    fonts.set(STAMP_FONT, font);
    resources.set("Font", fonts);

    let dict = doc.get_dictionary(page)?;
    let existing: Vec<Object> = match dict.get(b"Contents") {
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(Object::Array(a)) => a.clone(),
            _ => vec![Object::Reference(*id)],
        },
        Ok(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let open = doc.add_object(lopdf::Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let mut stamp = b"\nQ\n".to_vec();
    stamp.extend(content);
    let close = doc.add_object(lopdf::Stream::new(Dictionary::new(), stamp));
    let contents: Vec<Object> =
        std::iter::once(open.into()).chain(existing).chain(std::iter::once(close.into())).collect();

    let dict = doc.get_dictionary_mut(page)?;
    dict.set("Resources", resources);
    dict.set("Contents", contents);
    Ok(())
}

fn add_keyword(doc: &mut lopdf::Document, marker: &str) -> anyhow::Result<()> {
    let info_id = match doc.trailer.get(b"Info").and_then(Object::as_reference) {
        Ok(id) => id,
        Err(_) => {
            // A direct /Info dict (or none) becomes an indirect one.
            let info = doc.trailer.get(b"Info").and_then(Object::as_dict).cloned().unwrap_or_default();
            let id = doc.add_object(info);
            doc.trailer.set("Info", id);
            id
        }
    };
    let info = doc.get_dictionary_mut(info_id)?;
    let keywords = text(info.get(b"Keywords").ok()).unwrap_or_default();
    if !keywords.contains(marker) {
        info.set("Keywords", text_string(format!("{keywords} {marker}").trim()));
    }
    Ok(())
}

/// Saves a filled form to `out` (which may be `src`): widget values for text,
/// checkbox, radio, combo and list fields; each field's resolved label baked
/// into `/TU` so the copy stays accessible in any reader; synthetic flat-form
/// fields stamped per their `MarkStyle`, plus the filled marker. `values[i]`
/// is the value for `fields[i]`; `None` leaves the field's value untouched.
/// The original encryption is kept, and `out` is replaced atomically.
pub fn save_form(
    src: &Path,
    password: Option<&str>,
    fields: &[FormField],
    values: &[Option<String>],
    out: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(fields.len() == values.len(), "save_form: {} fields but {} values", fields.len(), values.len());
    let mut doc = load(src, password)?;
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    let by_name: HashMap<String, Terminal> = terminals(&doc).into_iter().map(|t| (t.name.clone(), t)).collect();

    let mut need_appearances = false;
    let mut stamps: BTreeMap<usize, Vec<Mark>> = BTreeMap::new();
    for (f, v) in fields.iter().zip(values) {
        if f.synthetic {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                let page = *pages.get(f.page_index).with_context(|| format!("No page {} for {}", f.page_index, f.name))?;
                let bx = page_box(&doc, page);
                stamps.entry(f.page_index).or_default().extend(marks_for(f, v, bx[2] - bx[0]));
            }
            continue;
        }
        let Some(t) = by_name.get(&f.name) else {
            log::warn!("Form field {:?} is not in {}", f.name, src.display());
            continue;
        };
        if let Some(v) = v {
            need_appearances |= set_value(&mut doc, t, v)?;
        }
        if let Some(label) = &f.label {
            doc.get_dictionary_mut(t.id)?.set("TU", text_string(label));
        }
    }

    if need_appearances && let Ok(Object::Reference(id)) = doc.catalog()?.get(b"AcroForm").cloned() {
        doc.get_dictionary_mut(id)?.set("NeedAppearances", true);
    } else if need_appearances && let Ok(Object::Dictionary(af)) = doc.catalog_mut()?.get_mut(b"AcroForm") {
        af.set("NeedAppearances", true);
    }

    if !stamps.is_empty() {
        let font = doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())),
            ("BaseFont", Object::Name(b"Helvetica".to_vec())),
            ("Encoding", Object::Name(b"WinAnsiEncoding".to_vec())),
        ]));
        for (page_index, marks) in &stamps {
            let page = pages[*page_index];
            let content = marks_content(marks, page_box(&doc, page));
            stamp_page(&mut doc, page, font, content)?;
        }
    }
    // Mark a saved flat form so reopening it does not re-offer discovery.
    if fields.iter().any(|f| f.synthetic) {
        add_keyword(&mut doc, FLAT_FORM_FILLED_MARKER)?;
    }

    if let Some(state) = doc.encryption_state.clone() {
        doc.encrypt(&state).context("Could not re-encrypt the PDF")?;
    }
    let dir = out.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    let mut writer = std::io::BufWriter::new(tmp.as_file_mut());
    doc.save_to(&mut writer).context("Could not write the PDF")?;
    // Flush explicitly: a failure swallowed by drop would replace `out` with a truncated file.
    writer.into_inner().map_err(|e| e.into_error()).context("Could not write the PDF")?.sync_all()?;
    tmp.persist(out).with_context(|| format!("Could not replace {}", out.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic(style: MarkStyle, rect: Rect) -> FormField {
        FormField {
            page_index: 0,
            field_type: FieldType::Text,
            name: "f".into(),
            value: String::new(),
            rect,
            choices: vec!["".into(), "Yes".into(), "No".into()],
            label: None,
            group: None,
            synthetic: true,
            choice_rects: vec![None, Some([300.0, 100.0, 312.0, 110.0]), Some([350.0, 100.0, 380.0, 112.0])],
            mark_style: style,
        }
    }

    #[test]
    fn helvetica_width_matches_pymupdf() {
        // fitz.get_text_length("Sample Value", "helv", 11) == 68.475
        assert!((helvetica_width("Sample Value", 11.0) - 68.475).abs() < 0.001);
    }

    #[test]
    fn text_stamp_covers_blank_and_shrinks_font_near_page_edge() {
        let f = synthetic(MarkStyle::Text, [100.0, 200.0, 250.0, 214.0]);
        assert_eq!(
            marks_for(&f, "Jane Doe", 595.0),
            [
                Mark::Cover([100.0, 199.5, 250.0, 215.5]),
                Mark::Text { x: 102.0, y: 211.0, size: 11.0, text: "Jane Doe".into() },
            ]
        );
        // 549pt available at x0=40: "Sample Value " x 9 is 644pt at 11pt, 527pt at 9pt.
        let near_edge = synthetic(MarkStyle::Text, [40.0, 10.0, 41.0, 20.0]);
        let marks = marks_for(&near_edge, &"Sample Value ".repeat(9), 595.0);
        assert!(matches!(marks[1], Mark::Text { size: 9.0, .. }));
        assert_eq!(marks[0], Mark::Cover([40.0, 9.5, 44.0, 21.5]));
    }

    #[test]
    fn checkbox_and_choice_marks() {
        let cb = synthetic(MarkStyle::Checkbox, [10.0, 20.0, 22.0, 30.0]);
        assert_eq!(marks_for(&cb, "Yes", 600.0), [Mark::Text { x: 10.5, y: 29.0, size: 10.0, text: "X".into() }]);
        assert_eq!(marks_for(&cb, "Off", 600.0), []);

        let choice = synthetic(MarkStyle::ChoiceBox, [0.0; 4]);
        assert_eq!(marks_for(&choice, "No", 600.0), [Mark::Text { x: 350.5, y: 111.0, size: 12.0, text: "X".into() }]);
        assert_eq!(marks_for(&choice, "", 600.0), []);

        let word = synthetic(MarkStyle::SelectWord, [0.0; 4]);
        assert_eq!(marks_for(&word, "Yes", 600.0), [Mark::Underline { x0: 300.0, x1: 312.0, y: 111.0 }]);
    }

    #[test]
    fn marks_content_flips_to_pdf_space_and_escapes() {
        let content = marks_content(
            &[Mark::Text { x: 102.0, y: 211.0, size: 11.0, text: "a(b)\\ é中".into() }],
            [0.0, 0.0, 595.0, 842.0],
        );
        assert_eq!(content, b"BT /IntomeHelv 11 Tf 0 g 102 631 Td (a\\(b\\)\\\\ \xe9?) Tj ET\n");
    }

    #[test]
    fn dict_end_skips_brackets_inside_strings() {
        let pdf = b"x << /A (a >> \\) (b)) /B <3E3E> /C << /D 1 >> >> tail";
        assert_eq!(&pdf[dict_end(pdf, 2).unwrap()..], b" tail");
    }
}
