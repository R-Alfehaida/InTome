//! Batch conversion dialog (port of `prism/gui/batch_dialog.py`): convert many
//! files to one export format on background threads.
//!
//! Every list operation has a keyboard path (Add / Remove / Clear / Move
//! buttons, Delete, Alt+Up / Alt+Down). Dropping files from Explorer onto the
//! list is only an additive shortcut.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

use winsafe::{co, gui, prelude::*};

use crate::cancel::CancelToken;
use crate::config;
use crate::document::{registry, OpenOptions};
use crate::events::{self, AppEvent};
use crate::export::{self, ExportOptions, FormatInfo};
use crate::gui::dialog::{self, DialogBuilder, IDCANCEL, IDOK};
use crate::gui::export_dialog::{default_output_dir, output_path, set_text, TIMER_ID, TIMER_MS};

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    path: PathBuf,
    status: &'static str,
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Appends the paths that are existing files not already listed; returns how many were added.
fn add_paths(files: &mut Vec<Entry>, paths: impl IntoIterator<Item = PathBuf>) -> usize {
    let before = files.len();
    for path in paths {
        if path.is_file() && !files.iter().any(|e| e.path == path) {
            files.push(Entry { path, status: "Pending" });
        }
    }
    files.len() - before
}

/// Swaps item `index` with the one `delta` away; the new index, or `None` past either end.
fn move_item<T>(items: &mut [T], index: usize, delta: isize) -> Option<usize> {
    let to = index.checked_add_signed(delta).filter(|&t| t < items.len() && index < items.len())?;
    items.swap(index, to);
    Some(to)
}

/// One finished file, from a worker. Each file yields exactly one.
struct Done {
    path: PathBuf,
    result: Result<PathBuf, String>,
}

struct Run {
    rx: Receiver<Done>,
    total: usize,
    completed: usize,
    succeeded: usize,
    started: Instant,
}

struct BatchUi {
    wnd: gui::WindowModal,
    formats: Vec<FormatInfo>,
    files: RefCell<Vec<Entry>>,
    list: gui::ListView,
    format: gui::ComboBox,
    dir: gui::Edit,
    concurrent: gui::Edit,
    concurrent_default: u32,
    log: gui::Edit,
    status: gui::Label,
    bar: gui::ProgressBar,
    start_btn: gui::Button,
    cancel: CancelToken,
    run: RefCell<Option<Run>>,
}

/// Shows the modal batch conversion dialog.
pub fn show(parent: &impl GuiParent) {
    let cfg = config::get();
    let formats = export::available_formats();
    let names: Vec<&str> = formats.iter().map(|f| f.name).collect();
    let selected = formats.iter().position(|f| f.key == cfg.export.default_format).or((!formats.is_empty()).then_some(0));
    let out_dir = match cfg.export.default_output_dir.trim() {
        "" => default_output_dir().to_string_lossy().into_owned(),
        dir => dir.to_string(),
    };

    let d = DialogBuilder::new("Batch Convert", 620);
    let col = d.column();
    let list = d.list_view("Files to convert:", &[("Filename", 360), ("Status", 120)], 180);
    let row1 = col.buttons(&["&Add Files", "&Remove Selected", "C&lear All"], 120);
    let row2 = col.buttons(&["Move &Up", "Move &Down"], 120);
    let format = col.combo("Output format:", &names, selected.map(|i| i as u32));
    let (dir, browse) = col.browse("Output folder:", &out_dir);
    let concurrent = col.spin("Concurrent files:", cfg.export.batch_concurrent, 1, 5);
    let log = col.multiline("Log:", "", 80, true);
    let status = col.text("Ready", 1);
    let bar = col.progress();
    let mut buttons = d.buttons(&[("&Start", IDOK), ("&Close", IDCANCEL)]);
    buttons.pop();
    let start_btn = buttons.pop().unwrap();

    let ui = Rc::new(BatchUi {
        wnd: d.wnd.clone(),
        formats,
        files: RefCell::new(Vec::new()),
        list,
        format,
        dir,
        concurrent,
        concurrent_default: cfg.export.batch_concurrent,
        log,
        status,
        bar,
        start_btn,
        cancel: CancelToken::new(),
        run: RefCell::new(None),
    });

    let actions: [(&gui::Button, fn(&BatchUi)); 5] = [
        (&row1[0], BatchUi::on_add),
        (&row1[1], BatchUi::remove_selected),
        (&row1[2], BatchUi::clear),
        (&row2[0], |u| u.move_selected(-1)),
        (&row2[1], |u| u.move_selected(1)),
    ];
    for (button, action) in actions {
        let u = ui.clone();
        button.on().bn_clicked(move || {
            action(&u);
            Ok(())
        });
    }
    let u = ui.clone();
    browse.on().bn_clicked(move || {
        if let Some(path) = dialog::pick_folder(u.wnd.hwnd(), "Select Output Folder", &u.dir.text().unwrap_or_default()) {
            set_text(&u.dir, &path);
        }
        Ok(())
    });
    let u = ui.clone();
    ui.list.on().lvn_key_down(move |p| {
        if p.wVKey == co::VK::DELETE {
            u.remove_selected();
        }
        Ok(())
    });
    let u = ui.clone();
    ui.list.on_subclass().wm_sys_key_down(move |p| {
        match p.vkey_code {
            co::VK::UP => u.move_selected(-1),
            co::VK::DOWN => u.move_selected(1),
            _ => unsafe { u.list.hwnd().DefSubclassProc(p) },
        }
        Ok(())
    });
    let u = ui.clone();
    ui.list.on_subclass().wm_drop_files(move |p| {
        let paths: Vec<PathBuf> = p.hdrop.DragQueryFile()?.filter_map(Result::ok).map(PathBuf::from).collect();
        u.add(paths);
        Ok(())
    });
    let u = ui.clone();
    d.on_ok(move || {
        u.on_start();
        false
    });
    let u = ui.clone();
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown {
            u.list.hwnd().DragAcceptFiles(true);
        }
        Ok(())
    });
    let u = ui.clone();
    d.wnd.on().wm_timer(TIMER_ID, move || {
        u.drain();
        Ok(())
    });
    let u = ui.clone();
    d.wnd.on().wm_destroy(move || {
        u.cancel.cancel();
        Ok(())
    });
    d.run(parent);
}

impl BatchUi {
    fn selected(&self) -> Option<usize> {
        self.list.items().iter_selected().next().map(|i| i.index() as usize)
    }

    fn refresh_list(&self) {
        let items = self.list.items();
        let _ = items.delete_all();
        for e in self.files.borrow().iter() {
            let _ = items.add(&[file_name(&e.path).as_str(), e.status], None, ());
        }
    }

    fn select(&self, index: usize) {
        let item = self.list.items().get(index as u32);
        let _ = item.select(true).and_then(|i| i.focus()).and_then(|i| i.ensure_visible());
    }

    fn add(&self, paths: Vec<PathBuf>) {
        let added = add_paths(&mut self.files.borrow_mut(), paths);
        if added > 0 {
            self.refresh_list();
            events::announce(format!("Added {added} files. {} files in list", self.files.borrow().len()));
        }
    }

    fn on_add(&self) {
        let paths = dialog::pick_open_files(self.wnd.hwnd(), "Add Files", &registry::file_filters());
        self.add(paths.into_iter().map(PathBuf::from).collect());
    }

    fn remove_selected(&self) {
        let Some(index) = self.selected() else { return };
        let removed = self.files.borrow_mut().remove(index);
        self.refresh_list();
        let remaining = self.files.borrow().len();
        // Keep a selection so a keyboard user can go on removing or moving.
        if remaining > 0 {
            self.select(index.min(remaining - 1));
        }
        events::announce(format!("Removed {}. {remaining} files remaining", file_name(&removed.path)));
    }

    fn clear(&self) {
        self.files.borrow_mut().clear();
        self.refresh_list();
        events::announce("File list cleared");
    }

    fn move_selected(&self, delta: isize) {
        let Some(index) = self.selected() else { return };
        let Some(to) = move_item(&mut self.files.borrow_mut(), index, delta) else { return };
        self.refresh_list();
        self.select(to);
        let files = self.files.borrow();
        events::announce(format!("Moved {} to position {} of {}", file_name(&files[to].path), to + 1, files.len()));
    }

    /// Validation problems are spoken and shown in the status line.
    fn refuse(&self, message: &str) {
        set_text(&self.status, message);
        events::announce(message);
    }

    fn on_start(&self) {
        if self.run.borrow().is_some() {
            self.cancel.cancel();
            set_text(&self.start_btn, "&Start");
            set_text(&self.status, "Cancelling...");
            return;
        }
        if self.files.borrow().is_empty() {
            return self.refuse("No files to convert");
        }
        let Some(format) = self.format.items().selected_index().and_then(|i| self.formats.get(i as usize)) else {
            return self.refuse("Select an output format");
        };
        let out_dir = PathBuf::from(self.dir.text().unwrap_or_default().trim());
        if out_dir.as_os_str().is_empty() || !out_dir.is_dir() {
            return self.refuse("Select a valid output folder");
        }
        let concurrent = dialog::spin_value(&self.concurrent, 1, 5, self.concurrent_default) as usize;

        let jobs: Vec<PathBuf> = {
            let mut files = self.files.borrow_mut();
            files.iter_mut().for_each(|e| e.status = "Pending");
            files.iter().map(|e| e.path.clone()).collect()
        };
        self.refresh_list();
        set_text(&self.log, "");
        self.bar.set_range(0, jobs.len() as u32);
        self.bar.set_position(0);
        self.cancel.reset();
        set_text(&self.start_btn, "&Cancel");
        events::emit(AppEvent::BatchStarted { file_count: jobs.len(), format: format.key.to_string() });
        events::announce(format!("Batch conversion started. {} files.", jobs.len()));

        let (tx, rx) = mpsc::channel();
        *self.run.borrow_mut() = Some(Run { rx, total: jobs.len(), completed: 0, succeeded: 0, started: Instant::now() });
        spawn_workers(jobs, format.key, out_dir, concurrent, self.cancel.clone(), tx);
        if let Err(e) = self.wnd.hwnd().SetTimer(TIMER_ID, TIMER_MS, None) {
            log::error!("batch progress timer failed: {e}");
        }
    }

    fn drain(&self) {
        let (done, finished) = {
            let run = self.run.borrow();
            let Some(run) = run.as_ref() else { return };
            let mut done = Vec::new();
            // Workers drop their senders when the queue is empty: that ends the run.
            let finished = loop {
                match run.rx.try_recv() {
                    Ok(d) => done.push(d),
                    Err(TryRecvError::Empty) => break false,
                    Err(TryRecvError::Disconnected) => break true,
                }
            };
            (done, finished)
        };
        for d in done {
            self.file_done(d);
        }
        if finished {
            self.finish();
        }
    }

    fn file_done(&self, done: Done) {
        let name = file_name(&done.path);
        let (status, line) = match &done.result {
            Ok(out) => ("Complete", format!("OK: {name} -> {}", out.display())),
            Err(e) => ("Error", format!("FAIL: {name}: {e}")),
        };
        // Look up by path: the user may reorder or remove files mid-run.
        let index = self.files.borrow().iter().position(|e| e.path == done.path);
        if let Some(i) = index {
            self.files.borrow_mut()[i].status = status;
            let _ = self.list.items().get(i as u32).set_text(1, status);
        }
        let (completed, total) = {
            let mut run = self.run.borrow_mut();
            let run = run.as_mut().unwrap();
            run.completed += 1;
            run.succeeded += done.result.is_ok() as usize;
            (run.completed, run.total)
        };
        self.bar.set_position(completed as u32);
        let end = self.log.text().unwrap_or_default().encode_utf16().count() as i32;
        self.log.set_selection(end, end);
        self.log.replace_selection(&format!("{line}\r\n"));
        set_text(&self.status, &format!("Processing: {completed} of {total}"));
        events::announce(format!("{name}: {status}"));
        events::emit(AppEvent::BatchFileCompleted { filename: name, status: status.to_string() });
    }

    fn finish(&self) {
        let _ = self.wnd.hwnd().KillTimer(TIMER_ID);
        let Some(run) = self.run.borrow_mut().take() else { return };
        set_text(&self.start_btn, "&Start");
        let elapsed = run.started.elapsed().as_secs_f64();
        let failed = run.total - run.succeeded;
        let mut msg = format!("Batch complete. {} of {} files converted in {elapsed:.1}s.", run.succeeded, run.total);
        if failed > 0 {
            msg.push_str(&format!(" {failed} failed."));
        }
        set_text(&self.status, &msg);
        events::announce(msg);
        events::emit(AppEvent::BatchCompleted { total: run.total, succeeded: run.succeeded, failed, elapsed });
    }
}

/// Converts `jobs` on up to `concurrent` threads; each file sends one [`Done`].
fn spawn_workers(jobs: Vec<PathBuf>, format_key: &'static str, out_dir: PathBuf, concurrent: usize, cancel: CancelToken, tx: Sender<Done>) {
    let cfg = config::get();
    let opts = OpenOptions { password: None, pdf_reading_mode: cfg.document.pdf_reading_mode, page_size: cfg.general.page_size };
    let jobs = Arc::new(jobs);
    let next = Arc::new(AtomicUsize::new(0));
    for _ in 0..concurrent.clamp(1, jobs.len().max(1)) {
        let (jobs, next, out_dir, cancel, tx, opts) = (jobs.clone(), next.clone(), out_dir.clone(), cancel.clone(), tx.clone(), opts.clone());
        std::thread::spawn(move || {
            while let Some(path) = jobs.get(next.fetch_add(1, Ordering::SeqCst)) {
                let result = convert(path, format_key, &out_dir, &opts, &cancel);
                if tx.send(Done { path: path.clone(), result }).is_err() {
                    break;
                }
            }
        });
    }
}

fn convert(path: &Path, format_key: &str, out_dir: &Path, opts: &OpenOptions, cancel: &CancelToken) -> Result<PathBuf, String> {
    if cancel.is_cancelled() {
        return Err("Cancelled".into());
    }
    let mut doc = registry::open_document(path, opts).map_err(|e| e.to_string())?;
    let exporter = export::create_exporter(format_key).map_err(|e| format!("{e:#}"))?;
    let out = output_path(out_dir, &file_name(path), exporter.file_extension());
    exporter.export(doc.as_mut(), &ExportOptions::new(out), cancel, &mut |_, _, _| {}).map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_item_swaps_with_neighbour_and_stops_at_the_ends() {
        let mut v = vec!["a", "b", "c"];
        assert_eq!(move_item(&mut v, 2, -1), Some(1));
        assert_eq!(v, ["a", "c", "b"]);
        assert_eq!(move_item(&mut v, 0, -1), None);
        assert_eq!(move_item(&mut v, 2, 1), None);
        assert_eq!(move_item(&mut v, 0, 1), Some(1));
        assert_eq!(v, ["c", "a", "b"]);
    }

    #[test]
    fn add_paths_skips_duplicates_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.md");
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "y").unwrap();
        let mut files = Vec::new();
        assert_eq!(add_paths(&mut files, [a.clone(), dir.path().join("gone.txt"), a.clone()]), 1);
        assert_eq!(add_paths(&mut files, [b.clone(), a.clone(), dir.path().to_path_buf()]), 1);
        assert_eq!(files.iter().map(|e| e.path.clone()).collect::<Vec<_>>(), [a, b]);
        assert!(files.iter().all(|e| e.status == "Pending"));
    }
}
