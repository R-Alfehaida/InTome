//! Keyboard shortcuts reference with a filter (Ctrl+/). Port of
//! prism/gui/shortcuts_dialog.py.

use winsafe::prelude::*;

use crate::events;
use crate::gui::dialog::DialogBuilder;

const SHORTCUTS: &[(&str, &str, &str)] = &[
    ("File", "Ctrl+O", "Open document"),
    ("File", "Ctrl+W", "Close tab"),
    ("File", "Ctrl+Shift+S", "Export document"),
    ("File", "Ctrl+Shift+P", "Batch convert"),
    ("File", "Alt+F4", "Exit"),
    ("Edit", "Ctrl+A", "Select entire document"),
    ("Edit", "Ctrl+C", "Copy selection, or the entire document after Ctrl+A"),
    ("Edit", "Ctrl+F", "Find in document"),
    ("Edit", "F3 / Shift+F3", "Find next / previous"),
    ("Edit", "Ctrl+Shift+E", "Element list"),
    ("Edit", "Ctrl+,", "Preferences"),
    ("Navigation", "Page Down / Page Up", "Next / previous page"),
    ("Navigation", "Ctrl+Home / Ctrl+End", "First / last page"),
    ("Navigation", "Alt+Page Down / Alt+Page Up", "Next / previous section"),
    ("Navigation", "Alt+Home / Alt+End", "Start / end of section"),
    ("Navigation", "H / Shift+H", "Next / previous heading"),
    ("Navigation", "1-6 / Shift+1-6", "Next / previous heading at level"),
    ("Navigation", "K / Shift+K", "Next / previous link"),
    ("Navigation", "T / Shift+T", "Next / previous table"),
    ("Navigation", "L / Shift+L", "Next / previous list"),
    ("Navigation", "Q / Shift+Q", "Next / previous quote"),
    ("Navigation", "G / Shift+G", "Next / previous figure"),
    ("Navigation", "Ctrl+Tab", "Next tab"),
    ("Navigation", "Ctrl+Shift+Tab", "Previous tab"),
    ("Navigation", "F6 / Shift+F6", "Cycle focus areas"),
    ("Navigation", "F9", "Toggle sidebar"),
    ("Text-to-Speech", "F5", "Start reading"),
    ("Text-to-Speech", "Shift+F5", "Pause / Resume"),
    ("Text-to-Speech", "F7", "Stop reading"),
    ("Text-to-Speech", "Alt+Right", "Skip forward (paragraph)"),
    ("Text-to-Speech", "Alt+Left", "Skip backward (paragraph)"),
    ("Text-to-Speech", "Ctrl+Shift+V", "Voice profiles"),
    ("Reference", "Ctrl+B", "Quick bookmark"),
    ("Reference", "Ctrl+Shift+B", "Named bookmark"),
    ("Reference", "Ctrl+M", "Add note"),
    ("Reference", "Ctrl+Shift+H", "Save quote"),
    ("Reference", "Ctrl+Shift+F2", "List bookmarks"),
    ("Reference", "Ctrl+Shift+M", "List notes"),
    ("Reference", "Ctrl+Shift+Q", "List quotes"),
    ("Tools", "Ctrl+Shift+O", "Run OCR"),
    ("Tools", "Ctrl+Shift+L", "Label form fields"),
    ("Tools", "Ctrl+Shift+F", "Fill form fields"),
    ("Help", "F1", "Open user guide"),
    ("Help", "Ctrl+/", "Keyboard shortcuts"),
];

/// Shortcuts whose category, keys or action contain `query` (case-insensitive).
fn matching(query: &str) -> Vec<&'static (&'static str, &'static str, &'static str)> {
    let q = query.to_lowercase();
    SHORTCUTS.iter().filter(|(c, k, a)| format!("{c} {k} {a}").to_lowercase().contains(&q)).collect()
}

pub fn show(parent: &impl GuiParent) {
    let d = DialogBuilder::new("Keyboard Shortcuts", 560);
    let filter = d.edit("&Filter:", "");
    let list = d.list_view("&Keyboard shortcuts:", &[("Category", 110), ("Shortcut", 170), ("Action", 230)], 320);
    d.close_button();

    let refresh = {
        let (filter, list) = (filter.clone(), list.clone());
        move || {
            let _ = list.items().delete_all();
            let rows = matching(&filter.text().unwrap_or_default());
            for (c, k, a) in &rows {
                let _ = list.items().add(&[*c, *k, *a], None, ());
            }
            events::announce(format!("{} shortcuts", rows.len()));
        }
    };
    let r = refresh.clone();
    filter.on().en_change(move || {
        r();
        Ok(())
    });
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown {
            refresh();
        }
        Ok(())
    });
    d.run(parent);
}

#[cfg(test)]
mod tests {
    use super::matching;

    #[test]
    fn filter_matches_category_key_or_action() {
        assert_eq!(matching("").len(), super::SHORTCUTS.len());
        let tts: Vec<_> = matching("pause").iter().map(|r| r.1).collect();
        assert_eq!(tts, vec!["Shift+F5"]);
        assert_eq!(matching("CTRL+/")[0].2, "Keyboard shortcuts");
    }
}
