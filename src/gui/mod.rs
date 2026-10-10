//! The GUI: native Win32 controls through winsafe. `run` is the port of
//! prism/app.py.

pub mod batch_dialog;
pub mod dialog;
pub mod export_dialog;
pub mod preferences;
pub mod annotation_dialogs;
pub mod form_dialog;
pub mod updater_ui;
pub mod voice_profiles;
pub mod content_view;
pub mod element_list_dialog;
pub mod error_dialog;
pub mod find_bar;
pub mod main_window;
pub mod progress_bar;
pub mod shortcuts_dialog;
pub mod toc_tree;

use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;
use winsafe::{self as ws, co, prelude::*};

use crate::constants::{self, APP_NAME};
use crate::events::{self, AppEvent};

fn setup_logging() {
    let _ = std::fs::create_dir_all(constants::appdata_dir());
    let file = std::fs::OpenOptions::new().create(true).append(true).open(constants::log_file());
    if let Ok(file) = file {
        // The HTML parser logs every token at debug level.
        let config = simplelog::ConfigBuilder::new().add_filter_ignore_str("html5ever").add_filter_ignore_str("selectors").build();
        let _ = simplelog::WriteLogger::init(log::LevelFilter::Debug, config, file);
    }
    // A panic inside a window procedure aborts with no console to print to;
    // record it so a "silent" exit leaves a trace in the log.
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
        log::logger().flush();
    }));
}

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Wraps the window for the event dispatcher, which must be `Sync`. It only
/// reads the HWND and sends it a message, which any thread may do.
struct Dispatch(ws::gui::WindowMain);
unsafe impl Sync for Dispatch {}

impl Dispatch {
    fn send(&self, task: events::UiTask) {
        self.0.run_ui_thread(move || {
            task();
            Ok(())
        });
    }
}

/// Starts InTome: logging, the single-instance check, speech, the main
/// window, and the file named on the command line. Returns when the window closes.
pub fn run() {
    setup_logging();
    log::info!("{APP_NAME} starting");

    // Kept alive for the whole run: its name is the single-instance lock.
    let instance = unsafe { CreateMutexW(None, false, w!("InTome-single-instance")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let _ = ws::HWND::NULL.MessageBox(&format!("Another instance of {APP_NAME} is already running."), APP_NAME, co::MB::OK | co::MB::ICONWARNING);
        return;
    }
    if let Ok(exe) = std::env::current_exe() && let Some(dir) = exe.parent() {
        crate::updater::remove_replaced_files(dir);
    }

    let main = match main_window::MainWindow::new() {
        Ok(m) => m,
        Err(e) => {
            log::error!("cannot create the main window: {e:#}");
            return;
        }
    };
    let wnd = Dispatch(main.wnd().clone());
    events::set_ui_dispatcher(move |task| wnd.send(task));
    crate::speech::init();
    // Every announcement also goes to the log, so spoken output can be audited without a screen reader.
    events::subscribe(|e| {
        if let AppEvent::Announce { message, interrupt } = e {
            log::info!("announce (interrupt={interrupt}): {message}");
        }
    });

    log::info!("{APP_NAME} ready");
    if let Err(e) = main.run() {
        log::error!("main loop ended with an error: {e}");
    }
    log::info!("{APP_NAME} shutting down");
    if crate::updater::restart_requested() {
        restart(instance);
    }
}

/// Starts a fresh copy of the exe. The single-instance mutex is released first, or the new
/// process would find it held and refuse to start.
fn restart(instance: windows::core::Result<HANDLE>) {
    if let Ok(handle) = instance {
        let _ = unsafe { CloseHandle(handle) };
    }
    let spawned = std::env::current_exe().and_then(|exe| Command::new(exe).creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).spawn());
    if let Err(e) = spawned {
        log::error!("cannot restart after the update: {e}");
    }
}
