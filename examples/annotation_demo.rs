//! Hosts the annotation dialogs over a scratch database, applying results the
//! way the main window does, then prints the rows left in it:
//!   cargo run --example annotation_demo -- <add-note|save-quote|bookmarks|notes|quotes> [--fresh]
//!   powershell -File scripts/uia_audit.ps1 -WindowTitle "Notes"
//! The database is target/annotation_demo.db (`--fresh` reseeds it).
//! Announcements print to stderr as `ANNOUNCE: ...`.
use intome::annotation::manager::AnnotationManager;
use intome::events::{self, AppEvent};
use intome::gui::annotation_dialogs as dlg;
use winsafe::{gui, prelude::*};

const DOC: &str = "demo-doc-hash";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().cloned().unwrap_or_else(|| "notes".into());
    let db = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/annotation_demo.db");
    if args.iter().any(|a| a == "--fresh") || !db.exists() {
        let _ = std::fs::remove_file(&db);
        let m = AnnotationManager::new(&db);
        m.add_bookmark(DOC, 0, 0, "Introduction").unwrap();
        m.add_bookmark(DOC, 11, 0, "Chapter 2").unwrap();
        m.add_note(DOC, 2, 0, "Check the citation\nin the footnote").unwrap();
        m.add_note(DOC, 7, 0, "Great argument here").unwrap();
        m.save_quote(DOC, 4, 10, 40, "To be, or not to be, that is the question", "hamlet, drama").unwrap();
    }
    events::subscribe(|e| {
        if let AppEvent::Announce { message, .. } = e {
            eprintln!("ANNOUNCE: {message}");
        }
    });
    let main = gui::WindowMain::new(gui::WindowMainOpts { title: "Annotation host", ..Default::default() });
    let m2 = main.clone();
    main.on().wm_create(move |_| {
        let (m3, db, mode) = (m2.clone(), db.clone(), mode.clone());
        m2.run_ui_thread(move || {
            let m = AnnotationManager::new(&db);
            let result = match mode.as_str() {
                "add-note" => {
                    let text = dlg::add_note(&m3, 3);
                    eprintln!("add_note -> {text:?}");
                    if let Some(text) = text {
                        m.add_note(DOC, 2, 0, &text).unwrap();
                    }
                    None
                }
                "save-quote" => {
                    let tags = dlg::save_quote(&m3, "All the world's a stage", 6);
                    eprintln!("save_quote -> {tags:?}");
                    if let Some(tags) = tags {
                        m.save_quote(DOC, 5, 0, 23, "All the world's a stage", &tags).unwrap();
                    }
                    None
                }
                "bookmarks" => Some(dlg::list_bookmarks(&m3, m.get_bookmarks(DOC).unwrap())),
                "quotes" => Some(dlg::list_quotes(&m3, m.get_quotes(DOC).unwrap())),
                _ => Some(dlg::list_notes(&m3, m.get_notes(DOC).unwrap())),
            };
            if let Some(r) = result {
                eprintln!("result={r:?}");
                for id in &r.deleted {
                    match mode.as_str() {
                        "bookmarks" => m.remove_bookmark(*id).unwrap(),
                        "quotes" => m.remove_quote(*id).unwrap(),
                        _ => m.remove_note(*id).unwrap(),
                    }
                }
                for (id, text) in &r.edits {
                    m.update_note(*id, text).unwrap();
                }
            }
            for b in m.get_bookmarks(DOC).unwrap() {
                eprintln!("ROW bookmark id={} page={} name={:?}", b.id, b.page_index, b.name);
            }
            for n in m.get_notes(DOC).unwrap() {
                eprintln!("ROW note id={} page={} text={:?}", n.id, n.page_index, n.text);
            }
            for q in m.get_quotes(DOC).unwrap() {
                eprintln!("ROW quote id={} page={} text={:?} tags={:?}", q.id, q.page_index, q.text, q.tags);
            }
            m3.close();
            Ok(())
        });
        Ok(0)
    });
    main.run_main(None).unwrap();
}
