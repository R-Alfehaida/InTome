//! Annotation dialogs (port of prism/gui/annotation_dialogs.py): add a note,
//! save a quote, and the bookmark, note and quote lists with filter, Go To,
//! Delete, Edit (notes) and a read-only View (quotes; the manager has no
//! `update_quote`). The caller applies the returned deletions and edits.

use std::cell::RefCell;
use std::rc::Rc;

use winsafe::{self as w, gui, prelude::*};

use crate::annotation::models::{Bookmark, Note, Quote};
use crate::events;
use crate::gui::dialog::{BUTTON_W, DialogBuilder};
use crate::platform::win32::{a11y, to_crlf};

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
pub fn add_note(parent: &impl GuiParent, page_num: usize) -> Option<String> {
    text_dialog(parent, "Add Note", &format!("Adding note on page {page_num}:"), "")
}

/// Confirms saving `selected_text` as a quote and asks for tags; `None` when cancelled.
/// `page_only` says the whole document was selected but only page `page_num` is saved;
/// the title carries it because the screen reader reads the title when the dialog opens.
pub fn save_quote(parent: &impl GuiParent, selected_text: &str, page_num: usize, page_only: bool) -> Option<String> {
    let (title, label) = if page_only {
        (
            format!("Save Quote: page {page_num} only"),
            format!("A quote holds one page. Saving page {page_num}:"),
        )
    } else {
        ("Save Quote".to_string(), format!("Saving quote from page {page_num}:"))
    };
    let d = DialogBuilder::new(&title, 480);
    let quote = d.multiline(&label, &to_crlf(selected_text), 80, true);
    let tags = d.edit("&Tags (comma-separated):", "");
    d.ok_cancel("OK");
    // As in Prism, land on Tags: the quote is context, the tags are the input.
    // The timer fires after activation has focused the first control.
    const FOCUS_TIMER: usize = 1;
    let (wnd, q, t) = (d.wnd.clone(), quote.clone(), tags.clone());
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown {
            a11y::set_name(q.hwnd().ptr() as isize, "Selected text");
            a11y::set_name(t.hwnd().ptr() as isize, "Tags");
            let _ = wnd.hwnd().SetTimer(FOCUS_TIMER, 0, None);
        }
        Ok(())
    });
    let (wnd, t) = (d.wnd.clone(), tags.clone());
    d.wnd.on().wm_timer(FOCUS_TIMER, move || {
        let _ = wnd.hwnd().KillTimer(FOCUS_TIMER);
        t.hwnd().SetFocus();
        Ok(())
    });
    let text = read_on_ok(&d, tags);
    d.run(parent).then(|| text.take())
}

pub fn list_bookmarks(parent: &impl GuiParent, bookmarks: Vec<Bookmark>) -> ListResult {
    let items = bookmarks.into_iter().map(|b| Item { id: b.id, page_index: b.page_index, text: b.name, tags: String::new() });
    show_list(parent, Kind::Bookmarks, items.collect())
}

pub fn list_notes(parent: &impl GuiParent, notes: Vec<Note>) -> ListResult {
    let items = notes.into_iter().map(|n| Item { id: n.id, page_index: n.page_index, text: n.text, tags: String::new() });
    show_list(parent, Kind::Notes, items.collect())
}

pub fn list_quotes(parent: &impl GuiParent, quotes: Vec<Quote>) -> ListResult {
    let items = quotes.into_iter().map(|q| Item { id: q.id, page_index: q.page_index, text: q.text, tags: q.tags });
    show_list(parent, Kind::Quotes, items.collect())
}

/// A multi-line text prompt; the trimmed text, or `None` when cancelled or empty.
fn text_dialog(parent: &impl GuiParent, title: &str, label: &str, value: &str) -> Option<String> {
    let d = DialogBuilder::new(title, 450);
    let edit = d.multiline(label, value, 120, false);
    d.ok_cancel("OK");
    let text = read_on_ok(&d, edit);
    d.run(parent).then(|| text.take()).filter(|t| !t.is_empty())
}

/// The edit's trimmed text, captured on OK while the control still exists.
fn read_on_ok(d: &DialogBuilder, edit: gui::Edit) -> Rc<RefCell<String>> {
    let text = Rc::new(RefCell::new(String::new()));
    let t = text.clone();
    d.on_ok(move || {
        *t.borrow_mut() = edit.text().unwrap_or_default().trim().to_string();
        true
    });
    text
}

/// One row of any annotation list. `text` is the bookmark name, note text or quote text.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    id: i64,
    page_index: usize,
    text: String,
    tags: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Bookmarks,
    Notes,
    Quotes,
}

impl Kind {
    fn title(self) -> &'static str {
        match self {
            Kind::Bookmarks => "Bookmarks",
            Kind::Notes => "Notes",
            Kind::Quotes => "Quotes",
        }
    }

    fn list_label(self) -> &'static str {
        match self {
            Kind::Bookmarks => "Boo&kmarks:",
            Kind::Notes => "N&otes:",
            Kind::Quotes => "&Quotes:",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Kind::Bookmarks => "Bookmark",
            Kind::Notes => "Note",
            Kind::Quotes => "Quote",
        }
    }

    fn columns(self) -> &'static [(&'static str, i32)] {
        match self {
            Kind::Bookmarks => &[("Name", 330), ("Page", 60)],
            Kind::Notes => &[("Preview", 360), ("Page", 60)],
            Kind::Quotes => &[("Preview", 260), ("Page", 60), ("Tags", 130)],
        }
    }

    /// The button between Go To and Delete: Edit for notes, View for quotes.
    fn extra_button(self) -> Option<&'static str> {
        match self {
            Kind::Bookmarks => None,
            Kind::Notes => Some("&Edit"),
            Kind::Quotes => Some("&View"),
        }
    }

    fn cells(self, item: &Item) -> Vec<String> {
        let page = (item.page_index + 1).to_string();
        match self {
            Kind::Bookmarks => vec![bookmark_label(item).to_string(), page],
            Kind::Notes => vec![preview(&item.text), page],
            Kind::Quotes => vec![preview(&item.text), page, item.tags.clone()],
        }
    }

    /// Case-insensitive substring match on what the row shows (quotes also match their tags).
    fn matches(self, item: &Item, filter: &str) -> bool {
        let filter = filter.to_lowercase();
        let haystack = match self {
            Kind::Bookmarks => bookmark_label(item).to_string(),
            Kind::Notes => item.text.clone(),
            Kind::Quotes => format!("{} {}", item.text, item.tags),
        };
        filter.is_empty() || haystack.to_lowercase().contains(&filter)
    }
}

fn bookmark_label(item: &Item) -> &str {
    if item.text.is_empty() { "(unnamed)" } else { &item.text }
}

/// The first 80 characters on one line.
fn preview(text: &str) -> String {
    text.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).take(80).collect()
}

struct ListUi {
    kind: Kind,
    wnd: gui::WindowModal,
    filter: gui::Edit,
    list: gui::ListView,
    items: RefCell<Vec<Item>>,
    /// Indices into `items` of the rows shown, in list order.
    shown: RefCell<Vec<usize>>,
    result: RefCell<ListResult>,
}

fn show_list(parent: &impl GuiParent, kind: Kind, items: Vec<Item>) -> ListResult {
    let d = DialogBuilder::new(kind.title(), 520);
    let filter = d.edit("&Filter:", "");
    let list = d.list_view(kind.list_label(), kind.columns(), 220);
    let labels: Vec<&str> = ["&Go To"].into_iter().chain(kind.extra_button()).chain(["&Delete"]).collect();
    let buttons = d.column().buttons(&labels, BUTTON_W);
    d.close_button();

    let ui = Rc::new(ListUi {
        kind,
        wnd: d.wnd.clone(),
        filter,
        list,
        items: RefCell::new(items),
        shown: RefCell::new(Vec::new()),
        result: RefCell::new(ListResult::default()),
    });

    let u = ui.clone();
    d.wnd.on().wm_show_window(move |p| {
        if p.being_shown && u.shown.borrow().is_empty() {
            a11y::set_name(u.filter.hwnd().ptr() as isize, &format!("Filter {}", u.kind.title().to_lowercase()));
            u.refresh(0);
        }
        Ok(())
    });
    let u = ui.clone();
    ui.filter.on().en_change(move || {
        u.refresh(0);
        Ok(())
    });
    let u = ui.clone();
    ui.list.on().lvn_item_activate(move |_| {
        u.activate();
        Ok(())
    });
    // Enter reaches the dialog as IDOK; in the list it activates the row, elsewhere it does nothing.
    let u = ui.clone();
    d.on_ok(move || {
        if w::HWND::GetFocus().is_some_and(|f| f == *u.list.hwnd()) {
            u.activate();
        }
        false
    });
    let u = ui.clone();
    buttons[0].on().bn_clicked(move || {
        u.go_to();
        Ok(())
    });
    if kind.extra_button().is_some() {
        let u = ui.clone();
        buttons[1].on().bn_clicked(move || {
            match u.kind {
                Kind::Notes => u.edit(),
                _ => u.view(),
            }
            Ok(())
        });
    }
    let u = ui.clone();
    buttons.last().unwrap().on().bn_clicked(move || {
        u.delete();
        Ok(())
    });

    d.run(parent);
    ui.result.take()
}

impl ListUi {
    /// Refills the list from the filter, selecting row `select` (clamped).
    fn refresh(&self, select: usize) {
        let filter = self.filter.text().unwrap_or_default();
        let items = self.items.borrow();
        let shown: Vec<usize> = (0..items.len()).filter(|&i| self.kind.matches(&items[i], &filter)).collect();
        let rows = self.list.items();
        let _ = rows.delete_all();
        for &i in &shown {
            let _ = rows.add(&self.kind.cells(&items[i]), None, ());
        }
        if !shown.is_empty() {
            let _ = rows.get(select.min(shown.len() - 1) as u32).select(true).and_then(|i| i.focus()).and_then(|i| i.ensure_visible());
        }
        events::announce(format!("{} {}", shown.len(), self.kind.title().to_lowercase()));
        *self.shown.borrow_mut() = shown;
    }

    /// (row in the list, index into `items`) of the selected row.
    fn selected(&self) -> Option<(usize, usize)> {
        let row = self.list.items().iter_selected().next()?.index() as usize;
        self.shown.borrow().get(row).map(|&i| (row, i))
    }

    fn activate(&self) {
        match self.kind {
            Kind::Quotes => self.view(),
            _ => self.go_to(),
        }
    }

    fn go_to(&self) {
        let Some((_, i)) = self.selected() else { return };
        self.result.borrow_mut().go_to_page = Some(self.items.borrow()[i].page_index);
        self.wnd.close();
    }

    fn delete(&self) {
        let Some((row, i)) = self.selected() else { return };
        let id = self.items.borrow_mut().remove(i).id;
        self.result.borrow_mut().deleted.push(id);
        self.refresh(row);
        events::announce(format!("{} deleted", self.kind.noun()));
    }

    fn edit(&self) {
        let Some((row, i)) = self.selected() else { return };
        let (id, page, text) = {
            let items = self.items.borrow();
            (items[i].id, items[i].page_index, items[i].text.clone())
        };
        if let Some(new_text) = text_dialog(&self.wnd, "Edit Note", &format!("Edit note (page {}):", page + 1), &text) {
            self.items.borrow_mut()[i].text = new_text.clone();
            self.result.borrow_mut().edits.push((id, new_text));
            self.refresh(row);
            events::announce("Note updated");
        }
    }

    fn view(&self) {
        let Some((_, i)) = self.selected() else { return };
        let quote = self.items.borrow()[i].clone();
        let d = DialogBuilder::new(&format!("Quote \u{2014} page {}", quote.page_index + 1), 480);
        d.multiline("Quote &text:", &to_crlf(&quote.text), 160, true);
        if !quote.tags.is_empty() {
            d.text(&format!("Tags: {}", quote.tags));
        }
        d.close_button();
        d.run(&self.wnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, tags: &str) -> Item {
        Item { id: 1, page_index: 4, text: text.into(), tags: tags.into() }
    }

    #[test]
    fn filter_matches_case_insensitively_on_shown_text_and_quote_tags() {
        assert!(Kind::Notes.matches(&item("Read Chapter 3", ""), "chapter"));
        assert!(!Kind::Notes.matches(&item("Read Chapter 3", "chapter"), "intro"));
        assert!(Kind::Quotes.matches(&item("To be or not", "Hamlet, drama"), "hamlet"));
        assert!(!Kind::Bookmarks.matches(&item("Intro", "hamlet"), "hamlet"));
        assert!(Kind::Bookmarks.matches(&item("", ""), "unnamed"));
        assert!(Kind::Bookmarks.matches(&item("Intro", ""), ""));
    }

    #[test]
    fn rows_show_one_line_previews_and_one_based_pages() {
        let long = format!("line one\nline two {}", "x".repeat(100));
        let cells = Kind::Quotes.cells(&item(&long, "tag"));
        assert_eq!(cells[0], format!("line one line two {}", "x".repeat(62)));
        assert_eq!(cells[1..], ["5".to_string(), "tag".to_string()]);
        assert_eq!(Kind::Bookmarks.cells(&item("", "")), ["(unnamed)", "5"]);
    }
}
