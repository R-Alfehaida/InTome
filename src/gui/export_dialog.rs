//! Export dialog (port of `prism/export/dialog.py`): choose a format, output
//! file and format options, then export on a background thread with a
//! progress bar, a status line and screen-reader announcements.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use winsafe::{self as w, co, gui, prelude::*};

use crate::cancel::CancelToken;
use crate::config::{self, Expressiveness};
use crate::document::SnapshotDocument;
use crate::events::{self, AppEvent};
use crate::export::{self, ExportOptions, FormatInfo};
use crate::gui::dialog::{self, Column, DialogBuilder, GAP, IDCANCEL, IDOK, MARGIN, ROW_H};
use crate::platform::win32::a11y;

const GEMINI_VOICES: [&str; 5] = ["Kore", "Puck", "Charon", "Fenrir", "Aoede"];
const AUDIO_FORMATS: [&str; 2] = ["daisy_audio", "m4b"];
/// The expressive narration choices, in `Expressiveness` order.
const EXPRESSIVE: [(&str, Expressiveness); 3] = [("Off", Expressiveness::Off), ("Subtle", Expressiveness::Subtle), ("Dramatic", Expressiveness::Dramatic)];
/// Read with the choice by screen readers, as the box has no room for a note.
const EXPRESSIVE_NOTE: &str = "Subtle gives each character their own pitch and acts out only lines the book describes, such as she whispered. Dramatic also acts out lines whose words carry an emotion. Both make extra Gemini requests, so the export takes longer and costs about a fifth more.";
pub(crate) const TIMER_ID: usize = 1;
pub(crate) const TIMER_MS: u32 = 100;

/// An existing folder to seed output paths. The Desktop is the natural default,
/// but Windows often redirects it (e.g. to OneDrive), so `~\Desktop` may not
/// exist: resolve the real one from the shell's User Shell Folders, then fall
/// back to `~\Desktop` if it exists, else the home folder.
pub(crate) fn default_output_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    resolve_output_dir(registry_desktop(), &home)
}

fn registry_desktop() -> Option<PathBuf> {
    let value = w::HKEY::CURRENT_USER
        .RegGetValue(Some(r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders"), Some("Desktop"), co::RRF::RT_ANY | co::RRF::NOEXPAND)
        .ok()?;
    match value {
        w::RegistryValue::Sz(s) => Some(PathBuf::from(s)),
        w::RegistryValue::ExpandSz(s) => Some(PathBuf::from(w::ExpandEnvironmentStrings(&s).unwrap_or(s))),
        _ => None,
    }
}

fn resolve_output_dir(desktop: Option<PathBuf>, home: &Path) -> PathBuf {
    if let Some(desktop) = desktop.filter(|d| d.is_dir()) {
        return desktop;
    }
    let fallback = home.join("Desktop");
    if fallback.is_dir() { fallback } else { home.to_path_buf() }
}

/// `dir\<filename without its extension><ext>`; `ext` includes the dot.
pub(crate) fn output_path(dir: &Path, filename: &str, ext: &str) -> PathBuf {
    let stem = Path::new(filename).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    dir.join(format!("{stem}{ext}"))
}

pub(crate) fn set_text(wnd: &impl GuiWindow, text: &str) {
    let _ = wnd.hwnd().SetWindowText(text);
}

pub(crate) fn set_visible(hwnd: &w::HWND, visible: bool) {
    hwnd.ShowWindow(if visible { co::SW::SHOW } else { co::SW::HIDE });
}

/// Worker to UI. winsafe controls are not `Send`, so the worker posts these
/// into a channel that a UI timer drains.
enum Msg {
    Step { current: usize, total: usize, message: String },
    Done { path: PathBuf, elapsed: f64 },
    Failed(String),
}

struct ExportUi {
    wnd: gui::WindowModal,
    formats: Vec<FormatInfo>,
    filename: String,
    doc: Arc<Mutex<SnapshotDocument>>,
    format: gui::ComboBox,
    output: gui::Edit,
    structure: gui::CheckBox,
    page_numbers: gui::CheckBox,
    tag_pdf: gui::CheckBox,
    toc: gui::CheckBox,
    a11y_metadata: gui::CheckBox,
    voice: gui::ComboBox,
    expressive: gui::ComboBox,
    status: gui::Label,
    bar: gui::ProgressBar,
    export_btn: gui::Button,
    cancel_btn: gui::Button,
    cancel: CancelToken,
    rx: RefCell<Option<Receiver<Msg>>>,
    /// Last quarter (1-3) announced, so progress is spoken at 25/50/75%, not every step.
    milestone: Cell<usize>,
}

/// Shows the modal export dialog for `doc`; `filename` seeds the output name.
pub fn show(parent: &impl GuiParent, doc: SnapshotDocument, filename: &str) {
    let formats = export::available_formats();
    let names: Vec<&str> = formats.iter().map(|f| f.name).collect();
    let initial = formats.first().map(|f| output_path(&default_output_dir(), filename, f.extension)).unwrap_or_default();

    let d = DialogBuilder::new("Export Document", 500);
    let col = d.column();
    let format = col.combo("&Format:", &names, (!names.is_empty()).then_some(0));
    let (output, browse) = col.edit_button("&Output file:", &initial.to_string_lossy(), co::ES::NoValue, "&Browse...");

    // Each format family's options share the same slots; only the selected family is shown.
    let box_h = 2 * (ROW_H + GAP) + 24;
    let box_top = col.take(box_h);
    let _ = gui::Button::new(&d.wnd, gui::ButtonOpts {
        text: "Options",
        position: gui::dpi(MARGIN, box_top),
        width: gui::dpi_x(d.content_width()),
        height: gui::dpi_y(box_h),
        control_style: co::BS::GROUPBOX,
        window_style: co::WS::CHILD | co::WS::VISIBLE,
        ..Default::default()
    });
    let slot = || Column::new(&d.wnd, MARGIN + 10, box_top + 20, d.content_width() - 20);
    let text_slot = slot();
    let structure = text_slot.check("Include &structure markers", true);
    let page_numbers = text_slot.check("Include &page numbers", true);
    let tag_pdf = slot().check("&Tag for accessibility", true);
    let epub_slot = slot();
    let toc = epub_slot.check("Include &table of contents", true);
    let a11y_metadata = epub_slot.check("Accessibility &metadata", true);
    let audio_slot = slot();
    let voice = audio_slot.combo("&Voice:", &GEMINI_VOICES, Some(0));
    let names: Vec<&str> = EXPRESSIVE.iter().map(|(n, _)| *n).collect();
    let last = EXPRESSIVE.iter().position(|(_, e)| *e == config::get().export.expressive).unwrap_or(0) as u32;
    let expressive = audio_slot.combo("Expressive &narration:", &names, Some(last));

    let status = col.text("Ready to export.", 1);
    let bar = col.progress();
    let mut buttons = d.buttons(&[("&Export", IDOK), ("Cancel", IDCANCEL)]);
    let cancel_btn = buttons.pop().unwrap();
    let export_btn = buttons.pop().unwrap();

    let ui = Rc::new(ExportUi {
        wnd: d.wnd.clone(),
        formats,
        filename: filename.to_string(),
        doc: Arc::new(Mutex::new(doc)),
        format,
        output,
        structure,
        page_numbers,
        tag_pdf,
        toc,
        a11y_metadata,
        voice,
        expressive,
        status,
        bar,
        export_btn,
        cancel_btn,
        cancel: CancelToken::new(),
        rx: RefCell::new(None),
        milestone: Cell::new(0),
    });

    let u = ui.clone();
    ui.format.on().cbn_sel_change(move || {
        u.on_format_changed();
        Ok(())
    });
    let u = ui.clone();
    browse.on().bn_clicked(move || {
        u.browse();
        Ok(())
    });
    let u = ui.clone();
    d.on_ok(move || {
        u.start();
        false
    });
    let u = ui.clone();
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown {
            a11y::set_description(u.expressive.hwnd().ptr() as isize, EXPRESSIVE_NOTE);
            u.update_options();
        }
        Ok(())
    });
    let u = ui.clone();
    d.wnd.on().wm_timer(TIMER_ID, move || {
        u.drain();
        Ok(())
    });
    // Closing (Cancel, Cancel Export, Escape) stops a running export; nothing is written.
    let u = ui.clone();
    d.wnd.on().wm_destroy(move || {
        u.cancel.cancel();
        Ok(())
    });
    d.run(parent);
}

impl ExportUi {
    fn selected(&self) -> Option<&FormatInfo> {
        self.format.items().selected_index().and_then(|i| self.formats.get(i as usize))
    }

    fn on_format_changed(&self) {
        if let Some(f) = self.selected() {
            set_text(&self.output, &output_path(&default_output_dir(), &self.filename, f.extension).to_string_lossy());
        }
        self.update_options();
    }

    fn update_options(&self) {
        let Some(key) = self.selected().map(|f| f.key) else { return };
        let text = matches!(key, "text" | "markdown");
        let epub = key == "epub";
        let audio = AUDIO_FORMATS.contains(&key);
        set_visible(self.structure.hwnd(), text);
        set_visible(self.page_numbers.hwnd(), text);
        set_visible(self.tag_pdf.hwnd(), key == "pdf");
        set_visible(self.toc.hwnd(), epub);
        set_visible(self.a11y_metadata.hwnd(), epub);
        for combo in [&self.voice, &self.expressive] {
            // Column creates each label right before its control.
            if let Ok(label) = combo.hwnd().GetWindow(co::GW::HWNDPREV) {
                set_visible(&label, audio);
            }
            set_visible(combo.hwnd(), audio);
        }
    }

    fn browse(&self) {
        let Some(f) = self.selected() else { return };
        let picked = if f.key.starts_with("daisy") {
            dialog::pick_folder(self.wnd.hwnd(), "Choose output folder", "")
        } else {
            let filter = [(format!("{} (*{})", f.name, f.extension), format!("*{}", f.extension))];
            dialog::pick_save_file(self.wnd.hwnd(), "Export As", &filter, &self.output.text().unwrap_or_default())
        };
        if let Some(path) = picked {
            set_text(&self.output, &path);
        }
    }

    fn warn(&self, text: &str, caption: &str) {
        let _ = self.wnd.hwnd().MessageBox(text, caption, co::MB::OK | co::MB::ICONWARNING);
    }

    fn start(&self) {
        if self.rx.borrow().is_some() {
            return;
        }
        let Some(format) = self.selected().cloned() else { return };
        let output = self.output.text().unwrap_or_default().trim().to_string();
        if output.is_empty() {
            self.warn("Please specify an output file path.", "No Output Path");
            return;
        }
        // Fail fast on a missing folder: audio export writes the output only at the
        // very end, so a bad folder would waste the whole TTS run.
        let dir = Path::new(&output).parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        if !dir.is_dir() {
            self.warn(
                &format!("The output folder does not exist:\n{}\n\nChoose an existing folder (use Browse) and try again.", dir.display()),
                "Output Folder Not Found",
            );
            return;
        }

        let mut options = ExportOptions::new(&output);
        options.include_structure_markers = self.structure.is_checked();
        options.include_page_numbers = self.page_numbers.is_checked();
        options.tag_for_accessibility = self.tag_pdf.is_checked();
        options.include_toc = self.toc.is_checked();
        options.accessibility_metadata = self.a11y_metadata.is_checked();
        options.voice = self.voice.items().selected_text().ok().flatten().unwrap_or_else(|| "Kore".into());
        if AUDIO_FORMATS.contains(&format.key) {
            let chosen = self.expressive.items().selected_index().and_then(|i| EXPRESSIVE.get(i as usize)).map_or(Expressiveness::Off, |(_, e)| *e);
            options.expressive = chosen;
            config::update("export.expressive", |s| s.export.expressive = chosen);
        }

        self.cancel.reset();
        self.milestone.set(0);
        self.bar.set_position(0);
        set_text(&self.cancel_btn, "&Cancel Export");
        // Disabling the focused Export button would leave keyboard focus nowhere.
        self.cancel_btn.hwnd().SetFocus();
        self.export_btn.hwnd().EnableWindow(false);
        set_text(&self.status, &format!("Exporting as {}...", format.name));
        events::announce(format!("Exporting as {}", format.name));
        events::emit(AppEvent::ExportStarted { filename: self.filename.clone(), format: format.name.to_string() });

        let (tx, rx) = mpsc::channel();
        *self.rx.borrow_mut() = Some(rx);
        let (doc, cancel, key) = (self.doc.clone(), self.cancel.clone(), format.key);
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = export::create_exporter(key).and_then(|exporter| {
                let mut doc = doc.lock().unwrap_or_else(|p| p.into_inner());
                exporter.export(&mut *doc, &options, &cancel, &mut |current, total, message| {
                    let _ = tx.send(Msg::Step { current, total, message: message.to_string() });
                })
            });
            let _ = tx.send(match result {
                Ok(path) => Msg::Done { path, elapsed: started.elapsed().as_secs_f64() },
                Err(e) => Msg::Failed(format!("{e:#}")),
            });
        });
        if let Err(e) = self.wnd.hwnd().SetTimer(TIMER_ID, TIMER_MS, None) {
            log::error!("export progress timer failed: {e}");
        }
    }

    fn drain(&self) {
        let msgs: Vec<Msg> = match self.rx.borrow().as_ref() {
            Some(rx) => rx.try_iter().collect(),
            None => return,
        };
        for msg in msgs {
            match msg {
                Msg::Step { current, total, message } => self.step(current, total, &message),
                Msg::Done { path, elapsed } => self.complete(&path, elapsed),
                Msg::Failed(error) => self.failed(&error),
            }
        }
    }

    fn step(&self, current: usize, total: usize, message: &str) {
        let pct = if total > 0 { (current * 100 / total).min(100) } else { 0 };
        self.bar.set_position(pct as u32);
        set_text(&self.status, message);
        events::emit(AppEvent::ExportProgress { filename: self.filename.clone(), current, total, message: message.to_string() });
        let quarter = pct / 25;
        if (1..4).contains(&quarter) && quarter > self.milestone.get() {
            self.milestone.set(quarter);
            events::announce(format!("Export {} percent", quarter * 25));
        }
    }

    /// Back to idle; called before any message box so the timer cannot re-enter.
    fn finish(&self) {
        let _ = self.wnd.hwnd().KillTimer(TIMER_ID);
        *self.rx.borrow_mut() = None;
        self.export_btn.hwnd().EnableWindow(true);
    }

    fn complete(&self, path: &Path, elapsed: f64) {
        self.finish();
        set_text(&self.cancel_btn, "&Close");
        self.bar.set_position(100);
        set_text(&self.status, &format!("Export complete ({elapsed:.1}s)"));
        events::announce(format!("Export complete in {elapsed:.1} seconds"));
        events::emit(AppEvent::ExportCompleted { filename: self.filename.clone(), output_path: path.display().to_string(), elapsed_seconds: elapsed });
        let ask = format!("Export complete.\n\nFile saved to:\n{}\n\nWould you like to open the exported file?", path.display());
        if self.wnd.hwnd().MessageBox(&ask, "Export Complete", co::MB::YESNO | co::MB::ICONINFORMATION) == Ok(co::DLGID::YES)
            && let Err(e) = self.wnd.hwnd().ShellExecute("open", &path.to_string_lossy(), None, None, co::SW::SHOWNORMAL)
        {
            events::announce(format!("Could not open the exported file: {e}"));
            let _ = self.wnd.hwnd().MessageBox(&format!("Could not open the exported file:\n\n{e}"), "Export", co::MB::OK | co::MB::ICONERROR);
        }
    }

    fn failed(&self, error: &str) {
        self.finish();
        set_text(&self.cancel_btn, "Cancel");
        self.bar.set_position(0);
        set_text(&self.status, &format!("Export failed: {error}"));
        events::announce(format!("Export failed: {error}"));
        events::emit(AppEvent::ExportError { filename: self.filename.clone(), error_message: error.to_string() });
        let _ = self.wnd.hwnd().MessageBox(&format!("Export failed:\n\n{error}"), "Export Error", co::MB::OK | co::MB::ICONERROR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_swaps_the_extension() {
        let dir = Path::new(r"C:\out");
        assert_eq!(output_path(dir, "book.epub", ".txt"), PathBuf::from(r"C:\out\book.txt"));
        assert_eq!(output_path(dir, "my.notes.pdf", ".html"), PathBuf::from(r"C:\out\my.notes.html"));
        assert_eq!(output_path(dir, "README", ".md"), PathBuf::from(r"C:\out\README.md"));
    }

    #[test]
    fn default_dir_prefers_registry_desktop_then_home_desktop_then_home() {
        let home = tempfile::tempdir().unwrap();
        let redirected = tempfile::tempdir().unwrap();
        assert_eq!(resolve_output_dir(Some(redirected.path().to_path_buf()), home.path()), redirected.path());
        assert_eq!(resolve_output_dir(Some(home.path().join("missing")), home.path()), home.path());
        std::fs::create_dir(home.path().join("Desktop")).unwrap();
        assert_eq!(resolve_output_dir(None, home.path()), home.path().join("Desktop"));
    }
}
