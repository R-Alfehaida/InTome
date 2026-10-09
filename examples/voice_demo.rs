//! Hosts the Voice Profiles dialog with APPDATA pointed at target/voice_demo,
//! so voice_profiles.json and the config it writes are scratch copies:
//!   cargo run --example voice_demo
//!   powershell -File scripts/uia_audit.ps1 -WindowTitle "Voice Profiles"
//! Announcements print to stderr as `ANNOUNCE: ...`.
use intome::events::{self, AppEvent};
use winsafe::{gui, prelude::*};

fn main() {
    let appdata = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("voice_demo");
    std::fs::create_dir_all(&appdata).unwrap();
    // Single-threaded here, before anything reads the environment.
    unsafe { std::env::set_var("APPDATA", &appdata) };
    events::subscribe(|e| {
        if let AppEvent::Announce { message, .. } = e {
            eprintln!("ANNOUNCE: {message}");
        }
    });
    let main = gui::WindowMain::new(gui::WindowMainOpts { title: "Voice host", ..Default::default() });
    let m2 = main.clone();
    main.on().wm_create(move |_| {
        let m3 = m2.clone();
        m2.run_ui_thread(move || {
            intome::gui::voice_profiles::show(&m3);
            m3.close();
            Ok(())
        });
        Ok(0)
    });
    main.run_main(None).unwrap();
}
