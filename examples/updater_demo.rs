//! Hosts the update UI with a UI dispatcher installed, as the main window does:
//!   cargo run --example updater_demo -- check          (manual check against the GitHub releases URL)
//!   cargo run --example updater_demo -- check-silent   (startup check: quiet unless an update exists)
//!   cargo run --example updater_demo -- offer <https url> [sha256]   (prompt, download, verify)
//! The host window stays open; close it when done. Announcements print to
//! stderr as `ANNOUNCE: ...`.
use std::sync::Mutex;

use intome::events::{self, AppEvent};
use intome::updater::UpdateInfo;
use winsafe::{gui, prelude::*};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    events::subscribe(|e| {
        if let AppEvent::Announce { message, .. } = e {
            eprintln!("ANNOUNCE: {message}");
        }
    });
    let main = gui::WindowMain::new(gui::WindowMainOpts { title: "Updater host", ..Default::default() });
    let host = Mutex::new(main.clone());
    events::set_ui_dispatcher(move |task| {
        host.lock().unwrap().run_ui_thread(move || {
            task();
            Ok(())
        })
    });
    let m2 = main.clone();
    main.on().wm_create(move |_| {
        let (m3, args) = (m2.clone(), args.clone());
        m2.run_ui_thread(move || {
            match args.first().map(String::as_str) {
                Some("offer") => {
                    let info = UpdateInfo { version: "99.0.0".into(), download_url: args[1].clone(), sha256: args.get(2).cloned() };
                    intome::gui::updater_ui::offer_update(&m3, &info);
                    eprintln!("offer_update returned");
                }
                Some("check-silent") => intome::gui::updater_ui::check_for_updates(&m3, true),
                _ => intome::gui::updater_ui::check_for_updates(&m3, false),
            }
            Ok(())
        });
        Ok(0)
    });
    main.run_main(None).unwrap();
}
