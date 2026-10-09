//! Dev tool: dumps what InTome extracts from a document as JSON, in the same
//! shape as scripts/parity/prism_dump.py, for parity checks against Prism.

use std::path::PathBuf;

use intome::document::{OpenOptions, TocEntry, registry};
use serde_json::{Value, json};

fn toc(entries: &[TocEntry]) -> Value {
    entries.iter().map(|e| json!({"title": e.title, "page_index": e.page_index, "level": e.level, "children": toc(&e.children)})).collect()
}

fn main() -> anyhow::Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).expect("usage: intome-dump <file>"));
    let opts = OpenOptions { page_size: intome::config::get().general.page_size, ..Default::default() };
    let mut doc = registry::open_document(&path, &opts)?;
    let pages: Vec<Value> = (0..doc.page_count())
        .map(|i| {
            let p = doc.page(i);
            json!({
                "text": p.text,
                "elements": p.elements.iter().map(|e| json!({
                    "type": format!("{:?}", e.element_type), "start": e.start_pos, "end": e.end_pos, "text": e.text
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let md = doc.metadata();
    let out = json!({
        "metadata": {"title": md.title, "author": md.author, "format": md.format},
        "toc": toc(doc.toc()),
        "pages": pages,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
