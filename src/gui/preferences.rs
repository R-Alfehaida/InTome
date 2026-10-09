//! Preferences dialog (port of `prism/gui/preferences_dialog.py`).
//!
//! Prism used a wx.Listbook because screen readers handle it better than tabs.
//! The equivalent: a list box named "Categories" on the left picks which page
//! window is visible on the right. Hidden pages' controls are not tab stops,
//! so Tab runs list, visible page, OK, Cancel.

use winsafe::{self as w, co, gui, msg, prelude::*};

use crate::annotation::AnnotationManager;
use crate::config::{self, GeminiMode, OcrBackend, PdfReadingMode, Settings};
use crate::document::cache;
use crate::events;
use crate::export;
use crate::gui::dialog::{self, spin_value, Column, DialogBuilder, GAP, MARGIN, ROW_H};
use crate::gui::export_dialog::set_text;
use crate::platform::win32::a11y;
use crate::tts::profiles::ProfileManager;

const PAGES: [&str; 9] = ["General", "Display", "Document", "Gemini Models", "OCR", "AI Providers", "Text-to-Speech", "Export", "Privacy"];
const WIDTH: i32 = 640;
const LIST_W: i32 = 150;
const BODY_H: i32 = 390;

type Page = Column<gui::WindowControl>;
/// Copies a page's controls into the settings draft when OK is pressed.
type Save = Box<dyn Fn(&mut Settings)>;

/// Shows the modal Preferences dialog. True when the user saved with OK.
pub fn show(parent: &impl GuiParent) -> bool {
    let cfg = config::get();
    let d = DialogBuilder::new("Preferences", WIDTH);
    let top = d.column().take(BODY_H);
    let _ = gui::Label::new(&d.wnd, gui::LabelOpts {
        text: "Categories",
        position: gui::dpi(MARGIN, top),
        size: gui::dpi(LIST_W, ROW_H - GAP),
        ..Default::default()
    });
    let list = gui::ListBox::new(&d.wnd, gui::ListBoxOpts {
        position: gui::dpi(MARGIN, top + ROW_H - GAP),
        size: gui::dpi(LIST_W, BODY_H - ROW_H + GAP),
        items: &PAGES,
        ..Default::default()
    });

    let (page_x, page_w) = (MARGIN + LIST_W + 2 * GAP, d.content_width() - LIST_W - 2 * GAP);
    let windows: Vec<gui::WindowControl> = (0..PAGES.len()).map(|i| page_window(&d.wnd, page_x, top, page_w, i == 0)).collect();
    let pages: Vec<Page> = windows.iter().map(|wnd| Column::new(wnd, 0, 0, page_w)).collect();
    let saves: Vec<Save> = vec![
        general(&pages[0], &cfg),
        display(&pages[1], &cfg),
        document(&pages[2], &cfg),
        gemini(&pages[3], &cfg),
        ocr(&pages[4], &cfg),
        ai_providers(&pages[5], &cfg),
        tts(&pages[6], &cfg),
        export_page(&pages[7], &cfg),
        privacy(&pages[8]),
    ];

    for wnd in &windows {
        // A multi-line edit posts WM_CLOSE to its parent on Escape; make that cancel the dialog.
        let dlg = d.wnd.clone();
        wnd.on().wm_close(move || {
            dlg.close();
            Ok(())
        });
        dialog::route_next_dlg_ctl(wnd.on());
    }
    let l = list.clone();
    list.on().lbn_sel_change(move || {
        let selected = unsafe { l.hwnd().SendMessage(msg::LbGetCurSel {}) };
        for (i, wnd) in windows.iter().enumerate() {
            wnd.hwnd().ShowWindow(if selected == Some(i as u32) { co::SW::SHOW } else { co::SW::HIDE });
        }
        Ok(())
    });
    let l = list.clone();
    // Also sent when the owner is restored, so only select General while nothing is selected.
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown && unsafe { l.hwnd().SendMessage(msg::LbGetCurSel {}) }.is_none() {
            let _ = unsafe { l.hwnd().SendMessage(msg::LbSetCurSel { index: Some(0) }) };
        }
        Ok(())
    });

    d.ok_cancel("OK");
    d.on_ok(move || {
        let old = config::get();
        let mut new = old.clone();
        for save in &saves {
            save(&mut new);
        }
        commit(&old, &new);
        events::announce("Preferences saved");
        true
    });
    d.run(parent)
}

fn page_window(dlg: &gui::WindowModal, x: i32, y: i32, width: i32, visible: bool) -> gui::WindowControl {
    gui::WindowControl::new(dlg, gui::WindowControlOpts {
        position: gui::dpi(x, y),
        size: gui::dpi(width, BODY_H),
        class_bg_brush: gui::Brush::Color(co::COLOR::BTNFACE),
        // Not a tab stop itself; CONTROLPARENT makes the dialog manager tab into its controls.
        style: co::WS::CHILD | co::WS::CLIPSIBLINGS | if visible { co::WS::VISIBLE } else { co::WS::NoValue },
        ex_style: co::WS_EX::CONTROLPARENT,
        ..Default::default()
    })
}

/// Saves each changed section once, so `ConfigChanged` names what changed.
fn commit(old: &Settings, new: &Settings) {
    macro_rules! sections {
        ($($section:ident),*) => {$(
            if old.$section != new.$section {
                let value = new.$section.clone();
                config::update(stringify!($section), move |s| s.$section = value);
            }
        )*};
    }
    sections!(general, display, document, gemini, ocr, tts, export);
}

/// The dialog window that owns `page`, for message boxes and pickers.
fn owner(page: &Page) -> w::HWND {
    page.parent().hwnd().GetAncestor(co::GA::ROOT).unwrap_or(w::HWND::NULL)
}

/// Edit controls want CRLF; settings keep LF.
fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn general(p: &Page, cfg: &Settings) -> Save {
    let continuous = p.check("Continuous reading (auto-advance pages)", cfg.general.continuous_reading);
    let limit = cfg.general.recent_files_limit as u32;
    let recent = p.spin("Recent files limit:", limit, 1, 50);
    let remember = p.check("Remember reading position", cfg.general.remember_position);
    Box::new(move |s| {
        s.general.continuous_reading = continuous.is_checked();
        s.general.remember_position = remember.is_checked();
        s.general.recent_files_limit = spin_value(&recent, 1, 50, limit) as usize;
    })
}

fn font_faces() -> Vec<String> {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::Graphics::Gdi::{DEFAULT_CHARSET, EnumFontFamiliesExW, GetDC, LOGFONTW, ReleaseDC, TEXTMETRICW};

    unsafe extern "system" fn collect(lf: *const LOGFONTW, _: *const TEXTMETRICW, _: u32, names: LPARAM) -> i32 {
        let (names, face) = unsafe { (&mut *(names.0 as *mut Vec<String>), &(*lf).lfFaceName) };
        let len = face.iter().position(|&c| c == 0).unwrap_or(face.len());
        let name = String::from_utf16_lossy(&face[..len]);
        // '@' faces are the vertical-writing variants.
        if !name.starts_with('@') {
            names.push(name);
        }
        1
    }

    let mut names: Vec<String> = Vec::new();
    unsafe {
        let hdc = GetDC(None);
        let lf = LOGFONTW { lfCharSet: DEFAULT_CHARSET, ..Default::default() };
        EnumFontFamiliesExW(hdc, &lf, Some(collect), LPARAM(&mut names as *mut _ as isize), 0);
        ReleaseDC(None, hdc);
    }
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// Line spacing 1.0 to 3.0 in steps of 0.1, as combo-box items (wx used a SpinCtrlDouble).
fn spacing_index(spacing: f32) -> u32 {
    ((spacing - 1.0) * 10.0).round().clamp(0.0, 20.0) as u32
}

fn display(p: &Page, cfg: &Settings) -> Save {
    let fonts = font_faces();
    let names: Vec<&str> = fonts.iter().map(String::as_str).collect();
    let font = p.combo("Font:", &names, fonts.iter().position(|f| *f == cfg.display.font_face).map(|i| i as u32));
    let size_default = cfg.display.font_size;
    let size = p.spin("Font size:", size_default, 8, 72);
    let dyslexic = p.check("Use OpenDyslexic font", cfg.display.use_opendyslexic);
    let spacings: Vec<String> = (10..=30).map(|t| format!("{:.1}", t as f32 / 10.0)).collect();
    let spacing_items: Vec<&str> = spacings.iter().map(String::as_str).collect();
    let spacing = p.combo("Line spacing:", &spacing_items, Some(spacing_index(cfg.display.line_spacing)));

    // The face is moot while OpenDyslexic is on.
    let (f, dy) = (font.clone(), dyslexic.clone());
    let sync = move || {
        f.hwnd().EnableWindow(!dy.is_checked());
        Ok(())
    };
    dyslexic.on().bn_clicked(sync.clone());
    p.parent().on().wm_create(move |_| sync().map(|()| 0));

    let (face_default, spacing_default) = (cfg.display.font_face.clone(), cfg.display.line_spacing);
    Box::new(move |s| {
        // Keep the configured face when it is not installed (nothing selected).
        s.display.font_face = font.items().selected_text().ok().flatten().unwrap_or_else(|| face_default.clone());
        s.display.font_size = spin_value(&size, 8, 72, size_default);
        s.display.use_opendyslexic = dyslexic.is_checked();
        s.display.line_spacing = spacing.items().selected_index().map_or(spacing_default, |i| (10 + i) as f32 / 10.0);
    })
}

const READING_MODES: [(PdfReadingMode, &str); 3] =
    [(PdfReadingMode::Automatic, "Automatic"), (PdfReadingMode::PhysicalLayout, "Physical Layout"), (PdfReadingMode::ReadingOrder, "Reading Order")];

fn document(p: &Page, cfg: &Settings) -> Save {
    let labels = READING_MODES.map(|(_, l)| l);
    let current = READING_MODES.iter().position(|(m, _)| *m == cfg.document.pdf_reading_mode).unwrap_or(0);
    let mode = p.combo("PDF reading mode:", &labels, Some(current as u32));
    let cache_enabled = p.check("Enable document cache", cfg.document.cache_enabled);
    let size_default = cfg.document.cache_max_size_mb as u32;
    let cache_size = p.spin("Cache size limit (MB):", size_default, 100, 5000);
    Box::new(move |s| {
        s.document.pdf_reading_mode = mode.items().selected_index().map_or(PdfReadingMode::Automatic, |i| READING_MODES[i as usize].0);
        s.document.cache_enabled = cache_enabled.is_checked();
        s.document.cache_max_size_mb = spin_value(&cache_size, 100, 5000, size_default) as u64;
    })
}

fn gemini(p: &Page, cfg: &Settings) -> Save {
    // The API key lives on the AI Providers page; this page is models, mode and limits only.
    let g = &cfg.gemini;
    let mode = p.combo("Default mode:", &["Accuracy", "Speed"], Some((g.default_mode == GeminiMode::Speed) as u32));
    let accuracy = p.edit("Accuracy model:", &g.accuracy_model, co::ES::NoValue);
    let speed = p.edit("Speed model:", &g.speed_model, co::ES::NoValue);
    let fallback = p.edit("Fallback model:", &g.default_model, co::ES::NoValue);
    let (concurrent_default, timeout_default) = (g.max_concurrent_pages, g.timeout_seconds as u32);
    let concurrent = p.spin("Max concurrent pages:", concurrent_default, 1, 10);
    let timeout = p.spin("Timeout (s):", timeout_default, 10, 300);
    let text = |e: &gui::Edit| e.text().unwrap_or_default().trim().to_string();
    Box::new(move |s| {
        s.gemini.default_mode = if mode.items().selected_index() == Some(1) { GeminiMode::Speed } else { GeminiMode::Accuracy };
        s.gemini.accuracy_model = text(&accuracy);
        s.gemini.speed_model = text(&speed);
        s.gemini.default_model = text(&fallback);
        s.gemini.max_concurrent_pages = spin_value(&concurrent, 1, 10, concurrent_default);
        s.gemini.timeout_seconds = spin_value(&timeout, 10, 300, timeout_default) as u64;
    })
}

fn ocr(p: &Page, cfg: &Settings) -> Save {
    let dpi_default = cfg.ocr.render_dpi;
    let dpi = p.spin("Render DPI:", dpi_default, 72, 600);
    let manga = p.check("Default to manga/comic mode", cfg.ocr.manga_mode);
    let instructions = p.multiline("User instructions (appended to every OCR job):", &to_crlf(&cfg.ocr.user_instructions), 60, false);
    let prompt = p.multiline("System prompt override (leave blank for default):", &to_crlf(&cfg.ocr.system_prompt_override), 80, false);
    let text = |e: &gui::Edit| e.text().unwrap_or_default().replace("\r\n", "\n");
    Box::new(move |s| {
        s.ocr.render_dpi = spin_value(&dpi, 72, 600, dpi_default);
        s.ocr.manga_mode = manga.is_checked();
        s.ocr.user_instructions = text(&instructions);
        s.ocr.system_prompt_override = text(&prompt);
    })
}

/// OCR backends in Prism's order; Windows OCR takes the slot of Prism's Surya.
const BACKENDS: [(OcrBackend, &str); 4] = [
    (OcrBackend::Auto, "Auto (best available)"),
    (OcrBackend::Tesseract, "Tesseract (local)"),
    (OcrBackend::Windows, "Windows OCR (built-in, local)"),
    (OcrBackend::Gemini, "Gemini"),
];

fn windows_ocr_status() -> &'static str {
    let _com = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED);
    let languages = windows::Media::Ocr::OcrEngine::AvailableRecognizerLanguages().and_then(|l| l.Size());
    match languages {
        Ok(n) if n > 0 => "Windows OCR: Available (built into Windows, no download needed)",
        _ => "Windows OCR: No OCR language installed (add one in Windows Settings, Time and language, Language)",
    }
}

fn key_status(configured: bool) -> &'static str {
    if configured { "Key configured" } else { "No key set" }
}

/// The key field's status, shown in a label and read with the field.
fn show_key_status(edit: &gui::Edit, label: &gui::Label, configured: bool) {
    set_text(label, key_status(configured));
    a11y::set_description(edit.hwnd().ptr() as isize, key_status(configured));
}

fn ai_providers(p: &Page, cfg: &Settings) -> Save {
    let labels = BACKENDS.map(|(_, l)| l);
    let current = BACKENDS.iter().position(|(b, _)| *b == cfg.ocr.backend).unwrap_or(0);
    let backend = p.combo("OCR backend:", &labels, Some(current as u32));
    p.text("Cloud API Keys", 1);
    // The plaintext key is never persisted: it is DPAPI-encrypted and saved the
    // moment the field loses focus, and flushed on OK in case focus never left.
    let (key, clear) = p.edit_button("Gemini API Key:", "", co::ES::PASSWORD, "Clear");
    let configured = !cfg.gemini.api_key_encrypted.is_empty();
    let status = p.text(key_status(configured), 1);
    p.text("Local Models", 1);
    p.text(windows_ocr_status(), 2);

    let (k, c) = (key.clone(), clear.clone());
    p.parent().on().wm_create(move |_| {
        a11y::set_name(c.hwnd().ptr() as isize, "Clear Gemini API Key");
        a11y::set_description(k.hwnd().ptr() as isize, key_status(configured));
        Ok(0)
    });

    let (k, st) = (key.clone(), status.clone());
    let dlg = p.parent().clone();
    key.on_subclass().wm_kill_focus(move |msg| {
        // Only when focus moves within the dialog: closing or Cancel must not save the key.
        let root = dlg.hwnd().GetAncestor(co::GA::ROOT).unwrap_or(w::HWND::NULL);
        let within = msg.hwnd.as_ref().is_some_and(|g| root.IsChild(g));
        unsafe { k.hwnd().DefSubclassProc(msg) };
        let value = k.text().unwrap_or_default().trim().to_string();
        if within && !value.is_empty() {
            match crate::ai::client::encrypt_api_key(&value) {
                Ok(encrypted) => {
                    config::update("gemini.api_key_encrypted", |s| s.gemini.api_key_encrypted = encrypted);
                    set_text(&k, "");
                    show_key_status(&k, &st, true);
                    events::announce("Gemini API key saved");
                }
                Err(e) => {
                    let _ = root.MessageBox(&format!("Failed to encrypt key: {e}"), "Encryption Error", co::MB::OK | co::MB::ICONERROR);
                    k.hwnd().SetFocus();
                }
            }
        }
        Ok(())
    });

    let (k, st) = (key.clone(), status.clone());
    clear.on().bn_clicked(move || {
        config::update("gemini.api_key_encrypted", |s| s.gemini.api_key_encrypted.clear());
        set_text(&k, "");
        show_key_status(&k, &st, false);
        events::announce("Gemini API key cleared");
        Ok(())
    });

    Box::new(move |s| {
        let value = key.text().unwrap_or_default().trim().to_string();
        // On failure the focus-loss handler already reported it interactively.
        if !value.is_empty()
            && let Ok(encrypted) = crate::ai::client::encrypt_api_key(&value)
        {
            s.gemini.api_key_encrypted = encrypted;
            set_text(&key, "");
            show_key_status(&key, &status, true);
        }
        if let Some(i) = backend.items().selected_index() {
            s.ocr.backend = BACKENDS[i as usize].0;
        }
    })
}

/// Reading scopes as stored in `tts.reading_scope`; Prism's "section" is InTome's "chapter".
const SCOPES: [(&str, &str); 3] = [("book", "Entire Book"), ("chapter", "Current Section"), ("page", "Current Page")];
const STARTS: [(&str, &str); 2] = [("cursor", "Cursor Position"), ("beginning", "Beginning of Page")];

fn tts(p: &Page, cfg: &Settings) -> Save {
    let t = &cfg.tts;
    let profiles: Vec<String> = ProfileManager::new().get_profiles().into_iter().map(|v| v.name).collect();
    let names: Vec<&str> = profiles.iter().map(String::as_str).collect();
    let profile = p.combo("Active profile:", &names, profiles.iter().position(|n| *n == t.active_profile).map(|i| i as u32));
    let scope_current = match t.reading_scope.as_str() {
        "section" => 1,
        s => SCOPES.iter().position(|(k, _)| *k == s).unwrap_or(0),
    };
    let scope = p.combo("Reading scope:", &SCOPES.map(|(_, l)| l), Some(scope_current as u32));
    let start_current = STARTS.iter().position(|(k, _)| *k == t.start_from).unwrap_or(0);
    let start = p.combo("Start from:", &STARTS.map(|(_, l)| l), Some(start_current as u32));
    let highlight = p.check("Highlight spoken text", t.highlight_spoken_text);
    let page_numbers = p.check("Speak page numbers at boundaries", t.speak_page_numbers);
    let sections = p.check("Announce section endings", t.announce_sections);
    Box::new(move |s| {
        if let Some(name) = profile.items().selected_text().ok().flatten() {
            s.tts.active_profile = name;
        }
        s.tts.reading_scope = SCOPES[scope.items().selected_index().unwrap_or(0) as usize].0.into();
        s.tts.start_from = STARTS[start.items().selected_index().unwrap_or(0) as usize].0.into();
        s.tts.highlight_spoken_text = highlight.is_checked();
        s.tts.speak_page_numbers = page_numbers.is_checked();
        s.tts.announce_sections = sections.is_checked();
    })
}

fn export_page(p: &Page, cfg: &Settings) -> Save {
    let formats = export::available_formats();
    let names: Vec<&str> = formats.iter().map(|f| f.name).collect();
    let current = formats.iter().position(|f| f.key == cfg.export.default_format).or((!formats.is_empty()).then_some(0));
    let format = p.combo("Default format:", &names, current.map(|i| i as u32));
    let (dir, browse) = p.browse("Default output directory:", &cfg.export.default_output_dir);
    let concurrent_default = cfg.export.batch_concurrent;
    let concurrent = p.spin("Batch concurrent files:", concurrent_default, 1, 5);

    let (d, page) = (dir.clone(), p.clone());
    browse.on().bn_clicked(move || {
        if let Some(path) = dialog::pick_folder(&owner(&page), "Select Default Output Directory", &d.text().unwrap_or_default()) {
            set_text(&d, &path);
        }
        Ok(())
    });
    Box::new(move |s| {
        if let Some(f) = format.items().selected_index().and_then(|i| formats.get(i as usize)) {
            s.export.default_format = f.key.into();
        }
        s.export.default_output_dir = dir.text().unwrap_or_default().trim().to_string();
        s.export.batch_concurrent = spin_value(&concurrent, 1, 5, concurrent_default);
    })
}

fn cache_info() -> String {
    let stats = cache::cache_stats();
    format!("Location: {}\nSize: {:.1} MB, Entries: {}", stats.path.display(), stats.size_bytes as f64 / (1024.0 * 1024.0), stats.entry_count)
}

fn annotation_info() -> String {
    match AnnotationManager::global().get_counts(None) {
        Ok(c) => format!("{} bookmarks, {} notes, {} quotes", c.bookmarks, c.notes, c.quotes),
        Err(e) => {
            log::warn!("annotation counts unavailable: {e:#}");
            "Annotation information unavailable".into()
        }
    }
}

/// Info text next to a button is not read on Tab, so it is also the button's description.
fn show_info(label: &gui::Label, button: &gui::Button, info: &str) {
    set_text(label, info);
    a11y::set_description(button.hwnd().ptr() as isize, info);
}

fn privacy(p: &Page) -> Save {
    p.text("Document Cache", 1);
    let cache_label = p.text(&cache_info(), 2);
    let clear_cache = p.buttons(&["&Clear Cache Now"], 150).remove(0);
    p.text("Annotations", 1);
    let annot_label = p.text(&annotation_info(), 1);
    let delete_annot = p.buttons(&["&Delete All Annotations"], 150).remove(0);

    let (cl, cb, al, ab) = (cache_label.clone(), clear_cache.clone(), annot_label.clone(), delete_annot.clone());
    p.parent().on().wm_create(move |_| {
        show_info(&cl, &cb, &cache_info());
        show_info(&al, &ab, &annotation_info());
        Ok(0)
    });

    let page = p.clone();
    let (cl, cb) = (cache_label, clear_cache.clone());
    clear_cache.on().bn_clicked(move || {
        let ask = "Clear all cached document data?\n\nThis will remove OCR results and conversion cache.";
        if owner(&page).MessageBox(ask, "Clear Cache", co::MB::YESNO | co::MB::DEFBUTTON2 | co::MB::ICONQUESTION) == Ok(co::DLGID::YES) {
            cache::clear_cache();
            events::announce("Cache cleared");
            show_info(&cl, &cb, &cache_info());
        }
        Ok(())
    });

    let page = p.clone();
    let (al, ab) = (annot_label, delete_annot.clone());
    delete_annot.on().bn_clicked(move || {
        let owner = owner(&page);
        let ask = "Delete ALL annotations across all documents?\n\nThis cannot be undone.";
        if owner.MessageBox(ask, "Delete Annotations", co::MB::YESNO | co::MB::DEFBUTTON2 | co::MB::ICONWARNING) == Ok(co::DLGID::YES) {
            match AnnotationManager::global().delete_all(None) {
                Ok(count) => events::announce(format!("Deleted {count} annotations")),
                Err(e) => {
                    events::announce("Could not delete annotations");
                    let _ = owner.MessageBox(&format!("Could not delete annotations:\n\n{e:#}"), "Delete Annotations", co::MB::OK | co::MB::ICONERROR);
                }
            }
            show_info(&al, &ab, &annotation_info());
        }
        Ok(())
    });
    Box::new(|_| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_spacing_maps_to_tenths_and_clamps() {
        assert_eq!(spacing_index(1.0), 0);
        assert_eq!(spacing_index(1.2), 2);
        assert_eq!(spacing_index(3.0), 20);
        assert_eq!(spacing_index(7.5), 20);
    }

    #[test]
    fn multiline_text_gets_crlf_once() {
        assert_eq!(to_crlf("a\nb\r\nc"), "a\r\nb\r\nc");
    }
}
