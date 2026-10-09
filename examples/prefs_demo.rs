//! Hosts the Preferences dialog for audits and keyboard checks:
//!   cargo run --example prefs_demo
//!   powershell -File scripts/uia_audit.ps1 -WindowTitle "Preferences"
//! Announcements print to stderr as `ANNOUNCE: ...`.
use intome::events::{self, AppEvent};
use winsafe::{gui, prelude::*};

fn main() {
    events::subscribe(|e| {
        if let AppEvent::Announce { message, .. } = e {
            eprintln!("ANNOUNCE: {message}");
        }
    });
    let main = gui::WindowMain::new(gui::WindowMainOpts { title: "Prefs host", ..Default::default() });
    let m2 = main.clone();
    main.on().wm_create(move |_| {
        let m3 = m2.clone();
        m2.run_ui_thread(move || {
            let saved = intome::gui::preferences::show(&m3);
            eprintln!("saved={saved}");
            m3.close();
            Ok(())
        });
        Ok(0)
    });
    main.run_main(None).unwrap();
}
