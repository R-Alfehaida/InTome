//! Hosts the Batch Convert dialog:
//!   cargo run --example batch_demo
//!   powershell -File scripts/uia_audit.ps1 -WindowTitle "Batch Convert"
//! Announcements print to stderr as `ANNOUNCE: ...`.
use intome::events::{self, AppEvent};
use winsafe::{gui, prelude::*};

fn main() {
    events::subscribe(|e| {
        if let AppEvent::Announce { message, .. } = e {
            eprintln!("ANNOUNCE: {message}");
        }
    });
    let main = gui::WindowMain::new(gui::WindowMainOpts { title: "Batch host", ..Default::default() });
    let m2 = main.clone();
    main.on().wm_create(move |_| {
        let m3 = m2.clone();
        m2.run_ui_thread(move || {
            intome::gui::batch_dialog::show(&m3);
            m3.close();
            Ok(())
        });
        Ok(0)
    });
    main.run_main(None).unwrap();
}
