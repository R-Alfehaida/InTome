//! STUB: port of prism/gui/annotation_dialogs.py. Signatures are the contract
//! between the main window (caller) and this module.

use winsafe::prelude::*;

use crate::annotation::models::{Bookmark, Note, Quote};

/// What the user did in a list dialog. The caller applies deletions and edits
/// through `AnnotationManager`, then navigates to `go_to_page` if set (Go To).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ListResult {
    pub deleted: Vec<i64>,
    /// Note edits as (note id, new text). Always empty for bookmarks and quotes.
    pub edits: Vec<(i64, String)>,
    /// 0-based page chosen with Go To.
    pub go_to_page: Option<usize>,
}

/// Asks for note text; `None` when cancelled or empty. `page_num` is 1-based.
pub fn add_note(_parent: &impl GuiParent, _page_num: usize) -> Option<String> {
    None
}

/// Confirms saving `selected_text` as a quote and asks for tags; `None` when cancelled.
pub fn save_quote(_parent: &impl GuiParent, _selected_text: &str, _page_num: usize) -> Option<String> {
    None
}

pub fn list_bookmarks(_parent: &impl GuiParent, _bookmarks: Vec<Bookmark>) -> ListResult {
    ListResult::default()
}

pub fn list_notes(_parent: &impl GuiParent, _notes: Vec<Note>) -> ListResult {
    ListResult::default()
}

pub fn list_quotes(_parent: &impl GuiParent, _quotes: Vec<Quote>) -> ListResult {
    ListResult::default()
}
