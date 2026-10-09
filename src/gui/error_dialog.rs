//! Accessible error dialog with optional technical details and next steps.
//! Port of prism/gui/error_dialog.py.

use winsafe::prelude::*;

use crate::events;
use crate::gui::dialog::{DialogBuilder, IDOK};

/// Shows a modal error and announces "Error: {title}. {message}". `details`
/// goes in a read-only box; `next_steps` are listed numbered.
pub fn show_error(parent: &impl GuiParent, title: &str, message: &str, details: Option<&str>, next_steps: &[&str]) {
    let d = DialogBuilder::new(title, 440);
    let col = d.column();
    col.text(message, wrapped_lines(message));
    if let Some(details) = details.filter(|d| !d.is_empty()) {
        d.multiline("Technical &details:", &details.replace("\r\n", "\n").replace('\n', "\r\n"), 90, true);
    }
    if !next_steps.is_empty() {
        col.text("Suggested next steps:", 1);
        let steps: Vec<String> = next_steps.iter().enumerate().map(|(i, s)| format!("  {}. {s}", i + 1)).collect();
        let steps = steps.join("\n");
        col.text(&steps, wrapped_lines(&steps));
    }
    d.buttons(&[("OK", IDOK)]);
    events::announce(format!("Error: {title}. {message}"));
    d.run(parent);
}

/// Lines a label of the dialog's width needs, at about 60 characters a line.
pub(crate) fn wrapped_lines(text: &str) -> i32 {
    text.lines().map(|l| l.chars().count() / 60 + 1).sum::<usize>().max(1) as i32
}
