//! Opens one dialog with every DialogBuilder row type, for the MSAA audit:
//!   cargo run --example dialog_demo
//!   powershell -File scripts/uia_audit.ps1 -WindowTitle "Dialog demo"
use intome::gui::dialog::DialogBuilder;
use winsafe::{co, gui, prelude::*};

fn main() {
    let main = gui::WindowMain::new(gui::WindowMainOpts { title: "Demo host", ..Default::default() });
    let m2 = main.clone();
    main.on().wm_create(move |_| {
        let m3 = m2.clone();
        m2.run_ui_thread(move || {
            let d = DialogBuilder::new("Dialog demo", 420);
            d.text("Every row type, for the screen-reader audit.");
            let name = d.edit("&Name:", "InTome");
            d.edit_with("API &key:", "secret", co::ES::PASSWORD);
            d.check("&Enable cache", true);
            d.combo("&Mode:", &["Speed", "Accuracy"], Some(0));
            d.trackbar("&Rate:", 0, 100, 50);
            d.list_box("&Files:", &["a.pdf", "b.epub"], 60);
            d.list_view("F&ields:", &[("Name", 150), ("Value", 200)], 80);
            d.multiline("&Notes:", "line one", 60, false);
            d.ok_cancel("OK");
            let n2 = name.clone();
            d.on_ok(move || !n2.text().unwrap_or_default().is_empty());
            let ok = d.run(&m3);
            eprintln!("accepted={ok}");
            m3.close();
            Ok(())
        });
        Ok(0)
    });
    main.run_main(None).unwrap();
}
