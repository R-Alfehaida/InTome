//! The UI half of prism/updater.py over `crate::updater`: check on a worker
//! thread, prompt, download with a cancellable progress dialog, verify, launch
//! the installer, then quit so it can replace the files. The check result
//! comes back through `events::run_on_ui` (Prism's `wx.CallAfter`).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;

use winsafe::{self as w, co, gui, prelude::*};

use crate::cancel::CancelToken;
use crate::constants::{APP_NAME, APP_VERSION};
use crate::events;
use crate::gui::dialog::{DialogBuilder, IDCANCEL};
use crate::updater::{self, CheckError, CheckOutcome, DownloadError, UpdateInfo};

const PROGRESS_TIMER: usize = 1;

type CheckResult = Result<CheckOutcome, CheckError>;

/// Checks for updates on a worker thread, then prompts, downloads with a
/// progress dialog, verifies, and launches the installer. `silent` suppresses
/// the "already up to date" and network-error messages.
pub fn check_for_updates(parent: &impl GuiParent, silent: bool) {
    let parent = parent.clone();
    let resume: Box<dyn FnOnce(CheckResult) + Send + '_> = Box::new(move |result| match next_step(result, silent) {
        Next::Nothing => {}
        Next::Tell { title, text, icon } => message(&parent, title, &text, icon),
        Next::Offer(info) => offer_update(&parent, &info),
    });
    // SAFETY: only lifetimes change. `GuiParent` requires `AsRef` of a type
    // private to winsafe, so only winsafe's window types implement it, and
    // every one is an owned, 'static handle.
    let resume: Box<dyn FnOnce(CheckResult) + Send + 'static> = unsafe { std::mem::transmute(resume) };
    std::thread::spawn(move || {
        let result = updater::check_for_updates();
        if let Err(e) = &result {
            log::warn!("Update check failed: {e}");
        }
        events::run_on_ui(move || resume(result));
    });
}

/// Prompts to install `info`, then downloads, verifies and launches it: what
/// `check_for_updates` does once it finds an update.
pub fn offer_update(parent: &impl GuiParent, info: &UpdateInfo) {
    let ask = format!(
        "A new version of {APP_NAME} ({}) is available.\nYou are running {APP_VERSION}.\n\nWould you like to download and install it now?\n{APP_NAME} will close so the installer can finish.",
        info.version
    );
    events::announce(format!("{APP_NAME} {} is available", info.version));
    if parent.hwnd().MessageBox(&ask, "Update Available", co::MB::YESNO | co::MB::ICONQUESTION) != Ok(co::DLGID::YES) {
        return;
    }
    match download_with_progress(parent, info) {
        Ok(path) => launch(parent, &path),
        Err(DownloadError::Cancelled) => {
            log::info!("Update download cancelled by user");
            events::announce("Update download cancelled");
        }
        Err(e) => {
            log::warn!("Update download failed: {e}");
            message(parent, "Download Failed", "The update could not be downloaded. Please try again later.", co::MB::ICONERROR);
        }
    }
}

/// What the UI does with a check result.
#[derive(Debug, PartialEq)]
enum Next {
    Nothing,
    Tell { title: &'static str, text: String, icon: co::MB },
    Offer(UpdateInfo),
}

fn next_step(result: CheckResult, silent: bool) -> Next {
    let tell = |title, text: &str, icon| if silent { Next::Nothing } else { Next::Tell { title, text: text.to_string(), icon } };
    match result {
        Ok(CheckOutcome::Available(info)) => Next::Offer(info),
        Ok(CheckOutcome::UpToDate) => tell("No Updates Available", &format!("You are running the latest version of {APP_NAME} ({APP_VERSION})."), co::MB::ICONINFORMATION),
        Err(CheckError::Failed(_)) => tell(
            "Update Check Failed",
            "Could not check for updates. Please verify your internet connection and try again later.",
            co::MB::ICONWARNING,
        ),
        Err(CheckError::Incomplete) => tell("Update Check Failed", "The update information is incomplete. Please try again later.", co::MB::ICONWARNING),
        Err(CheckError::InsecureDownloadUrl) => tell(
            "Update Blocked",
            "The update download link is not secure (must be HTTPS). Update aborted for your safety.",
            co::MB::ICONERROR,
        ),
    }
}

fn message(parent: &impl GuiParent, title: &str, text: &str, icon: co::MB) {
    events::announce(format!("{title}. {text}"));
    let _ = parent.hwnd().MessageBox(text, title, co::MB::OK | icon);
}

/// Worker to dialog: progress in bytes, then the result.
enum Msg {
    Progress(u64, u64),
    Done(Result<PathBuf, DownloadError>),
}

/// Downloads `info` on a worker thread behind a modal progress dialog. Cancel,
/// Escape or closing stops the worker at its next chunk, and it deletes the
/// partial file itself; nothing waits for it.
fn download_with_progress(parent: &impl GuiParent, info: &UpdateInfo) -> Result<PathBuf, DownloadError> {
    let d = DialogBuilder::new("Downloading Update", 420);
    let status = d.text(&format!("Downloading {APP_NAME} {}...", info.version));
    let bar = d.column().progress();
    d.buttons(&[("Cancel", IDCANCEL)]);

    let cancel = CancelToken::new();
    let (tx, rx) = mpsc::channel();
    let (url, sha256, worker_cancel) = (info.download_url.clone(), info.sha256.clone(), cancel.clone());
    std::thread::spawn(move || {
        let progress_tx = tx.clone();
        let result = updater::download(&url, sha256.as_deref(), |done, total| drop(progress_tx.send(Msg::Progress(done, total))), &worker_cancel);
        let _ = tx.send(Msg::Done(result));
    });

    let outcome: Rc<RefCell<Option<Result<PathBuf, DownloadError>>>> = Rc::new(RefCell::new(None));
    let wnd = d.wnd.clone();
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown && let Err(e) = wnd.hwnd().SetTimer(PROGRESS_TIMER, 100, None) {
            log::error!("update progress timer failed: {e}");
        }
        Ok(())
    });
    let (wnd, out, milestone) = (d.wnd.clone(), outcome.clone(), Cell::new(0));
    d.wnd.on().wm_timer(PROGRESS_TIMER, move || {
        for msg in rx.try_iter() {
            match msg {
                Msg::Progress(done, total) => show_progress(&status, &bar, done, total, &milestone),
                Msg::Done(result) => {
                    *out.borrow_mut() = Some(result);
                    let _ = wnd.hwnd().KillTimer(PROGRESS_TIMER);
                    wnd.close();
                    break;
                }
            }
        }
        Ok(())
    });
    let (out, close_cancel) = (outcome.clone(), cancel.clone());
    d.wnd.on().wm_destroy(move || {
        if out.borrow().is_none() {
            close_cancel.cancel();
        }
        Ok(())
    });
    events::announce(format!("Downloading {APP_NAME} {}", info.version));
    d.run(parent);
    outcome.take().unwrap_or(Err(DownloadError::Cancelled))
}

fn show_progress(status: &gui::Label, bar: &gui::ProgressBar, done: u64, total: u64, milestone: &Cell<u32>) {
    let (pct, text) = progress_text(done, total);
    if let Some(pct) = pct {
        bar.set_position(pct);
        let quarter = pct / 25;
        if (1..4).contains(&quarter) && quarter > milestone.get() {
            milestone.set(quarter);
            events::announce(format!("Download {} percent", quarter * 25));
        }
    }
    let _ = status.hwnd().SetWindowText(&text);
}

/// Percent (when the size is known) and the status line for `done` of `total` bytes.
fn progress_text(done: u64, total: u64) -> (Option<u32>, String) {
    if total > 0 {
        (Some((done * 100 / total).min(100) as u32), format!("Downloaded {} KB of {} KB...", done / 1024, total / 1024))
    } else {
        (None, format!("Downloaded {} KB...", done / 1024))
    }
}

/// Starts the installer and quits so it can replace the files; on failure says where the download is.
fn launch(parent: &impl GuiParent, path: &Path) {
    match updater::launch_installer(path) {
        Ok(()) => {
            events::announce(format!("Installing the update. {APP_NAME} will close."));
            w::PostQuitMessage(0);
        }
        Err(e) => {
            log::error!("Failed to launch installer {}: {e}", path.display());
            let text = format!("The installer could not be started. The download was saved to:\n{}\n\nYou can run it manually to update.", path.display());
            message(parent, "Update Failed", &text, co::MB::ICONERROR);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> UpdateInfo {
        UpdateInfo { version: "9.9.9".into(), download_url: "https://example.com/InTome_Setup.exe".into(), sha256: None }
    }

    #[test]
    fn silent_checks_only_surface_an_available_update() {
        assert_eq!(next_step(Ok(CheckOutcome::Available(info())), true), Next::Offer(info()));
        assert_eq!(next_step(Ok(CheckOutcome::UpToDate), true), Next::Nothing);
        assert_eq!(next_step(Err(CheckError::Failed(anyhow::anyhow!("offline"))), true), Next::Nothing);
        assert_eq!(next_step(Err(CheckError::InsecureDownloadUrl), true), Next::Nothing);
    }

    #[test]
    fn manual_checks_always_answer() {
        let titles: Vec<&str> = [Ok(CheckOutcome::UpToDate), Err(CheckError::Failed(anyhow::anyhow!("offline"))), Err(CheckError::Incomplete), Err(CheckError::InsecureDownloadUrl)]
            .into_iter()
            .map(|r| match next_step(r, false) {
                Next::Tell { title, .. } => title,
                other => panic!("expected a message, got {other:?}"),
            })
            .collect();
        assert_eq!(titles, ["No Updates Available", "Update Check Failed", "Update Check Failed", "Update Blocked"]);
    }

    #[test]
    fn progress_reports_percent_only_with_a_known_size() {
        assert_eq!(progress_text(512 * 1024, 2048 * 1024), (Some(25), "Downloaded 512 KB of 2048 KB...".to_string()));
        assert_eq!(progress_text(3 * 1024, 0), (None, "Downloaded 3 KB...".to_string()));
    }
}
