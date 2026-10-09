//! Hosts the Fill Form dialog over a copy of a fixture in target/form_demo:
//!   cargo run --example form_demo -- acroform   (tests/fixtures/acroform.pdf)
//!   cargo run --example form_demo -- flat       (tests/fixtures/flat_form.pdf, synthetic fields)
//!   powershell -File scripts/uia_audit.ps1 -WindowTitle "Fill Form: acroform.pdf"
//! Labels and groups set here stand in for the AI label cache. Events print to
//! stderr as `ANNOUNCE: ...` and `FORM_SAVED: ...`.
use intome::document::formats::pdf::{self, PdfDocument};
use intome::document::{Document, FieldType, FormField, MarkStyle, OpenOptions};
use intome::events::{self, AppEvent};
use winsafe::{gui, prelude::*};

fn main() {
    let flat = std::env::args().nth(1).as_deref() == Some("flat");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let name = if flat { "flat_form.pdf" } else { "acroform.pdf" };
    let dir = root.join("target").join("form_demo");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::copy(root.join("tests/fixtures").join(name), &path).unwrap();

    let mut doc = PdfDocument::open(&path, &OpenOptions::default()).unwrap();
    let fields = if flat { flat_fields(&path) } else { acro_fields(&mut doc) };
    for f in &fields {
        eprintln!("FIELD {:?} {:?} label={:?} group={:?} value={:?}", f.field_type, f.name, f.label, f.group, f.value);
    }

    events::subscribe(|e| match e {
        AppEvent::Announce { message, .. } => eprintln!("ANNOUNCE: {message}"),
        AppEvent::FormSaved { field_count, output_path, .. } => eprintln!("FORM_SAVED: {field_count} fields to {output_path}"),
        _ => {}
    });
    let main = gui::WindowMain::new(gui::WindowMainOpts { title: "Form host", ..Default::default() });
    let m2 = main.clone();
    let doc = std::cell::Cell::new(Some(doc));
    main.on().wm_create(move |_| {
        let m3 = m2.clone();
        let mut doc = doc.take().unwrap();
        let fields = fields.clone();
        m2.run_ui_thread(move || {
            intome::gui::form_dialog::show(&m3, &mut doc, fields, name);
            m3.close();
            Ok(())
        });
        Ok(0)
    });
    main.run_main(None).unwrap();
}

fn acro_fields(doc: &mut PdfDocument) -> Vec<FormField> {
    let mut fields = doc.form_fields();
    for f in &mut fields {
        match f.name.as_str() {
            "email" => f.label = Some("Email address".into()),
            "country" => f.label = Some("Country & region".into()),
            _ => {}
        }
        if ["name", "email", "addr.city"].contains(&f.name.as_str()) {
            f.group = Some("Personal details".into());
        }
    }
    fields
}

fn flat_fields(path: &std::path::Path) -> Vec<FormField> {
    let geo = pdf::flat_form_page_geometry(path, None, 0).unwrap();
    let field = |name: &str, label: &str, field_type, rect, mark_style, choices: &[&str], choice_rects| FormField {
        page_index: 0,
        field_type,
        name: name.into(),
        value: String::new(),
        rect,
        choices: choices.iter().map(|c| c.to_string()).collect(),
        label: Some(label.into()),
        group: Some("Membership".into()),
        synthetic: true,
        choice_rects,
        mark_style,
    };
    vec![
        field("full_name", "Full name", FieldType::Text, geo.anchors[0], MarkStyle::Text, &[], vec![]),
        field("dob", "Date of birth", FieldType::Text, geo.anchors[1], MarkStyle::Text, &[], vec![]),
        field("signed", "Signed in person", FieldType::CheckBox, [40.0, 160.0, 52.0, 172.0], MarkStyle::Checkbox, &[], vec![]),
        field(
            "member",
            "Existing member",
            FieldType::RadioButton,
            [300.0, 200.0, 360.0, 212.0],
            MarkStyle::ChoiceBox,
            &["", "Yes", "No"],
            vec![None, Some([300.0, 200.0, 312.0, 212.0]), Some([348.0, 200.0, 360.0, 212.0])],
        ),
    ]
}
