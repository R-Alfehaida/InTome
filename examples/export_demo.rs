//! Opens a fixture and hosts the Export dialog on a snapshot of it:
//!   cargo run --example export_demo [path]   (default tests/fixtures/article.md)
//!   powershell -File scripts/uia_audit.ps1 -WindowTitle "Export Document"
//! Announcements print to stderr as `ANNOUNCE: ...`.
use std::path::PathBuf;

use intome::document::{OpenOptions, SnapshotDocument, registry};
use intome::events::{self, AppEvent};
use winsafe::{gui, prelude::*};

fn main() {
    events::subscribe(|e| {
        if let AppEvent::Announce { message, .. } = e {
            eprintln!("ANNOUNCE: {message}");
        }
    });
    let path = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("tests/fixtures/article.md"));
    let mut doc = registry::open_document(&path, &OpenOptions { page_size: 3000, ..Default::default() }).expect("open fixture");
    let snapshot = SnapshotDocument::of(doc.as_mut());
    let filename = path.file_name().unwrap().to_string_lossy().into_owned();

    let main = gui::WindowMain::new(gui::WindowMainOpts { title: "Export host", ..Default::default() });
    let m2 = main.clone();
    let pending = std::cell::RefCell::new(Some((snapshot, filename)));
    main.on().wm_create(move |_| {
        let m3 = m2.clone();
        let (snapshot, filename) = pending.borrow_mut().take().unwrap();
        m2.run_ui_thread(move || {
            intome::gui::export_dialog::show(&m3, snapshot, &filename);
            m3.close();
            Ok(())
        });
        Ok(0)
    });
    main.run_main(None).unwrap();
}
