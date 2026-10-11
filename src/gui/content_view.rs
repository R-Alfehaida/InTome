//! The reading area of one open document (one per tab): a read-only RichEdit
//! showing one page at a time, page turns, single-key structured navigation,
//! section navigation, search, continuous reading and whole-document Select
//! All. Port of prism/gui/content_view.py.
//!
//! Positions are char indices into `Page::text` everywhere except the calls
//! into [`RichEdit`], which take control offsets.
//!
//! Unpaginated formats arrive already split into pages (`OpenOptions::page_size`
//! and `document::utils::paginate_text`), so Prism's `DocumentModel` has no
//! counterpart here.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

use crate::ai::structure::ParsedOcr;
use crate::config;
use crate::document::Document;
use crate::events::{self, AppEvent};
use crate::navigation::element_nav::{self, find_section_bounds, get_toc_entries_flat};
use crate::navigation::search::{SearchEngine, SearchMatch};
use crate::platform::win32::{a11y, clipboard};
use crate::platform::win32::richedit::{RichEdit, char_to_offset, offset_len, offset_to_char};
use crate::structured_text::elements::{ElementType, HEADING_TYPES, SemanticElement};
use crate::tts::reader::ReadingTarget;

/// Control ID shared by every reading control, so the main window routes all
/// their `EN_SELCHANGE` notifications through one handler.
pub const CONTROL_ID: u16 = 3000;

const WM_KEYDOWN: u32 = 0x100;
const WM_CHAR: u32 = 0x102;
const WM_SYSKEYDOWN: u32 = 0x104;
const WM_GETDLGCODE: u32 = 0x87;
// Arrows and characters, but not Tab or Escape: those move focus and close the find bar.
// No DLGC_HASSETSEL, or tabbing in would select the whole page.
const DLGC_READING: isize = 0x1 | 0x80;

const VK_A: u16 = 0x41;
const VK_C: u16 = 0x43;
const VK_INSERT: u16 = 0x2D;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_UP: u16 = 0x26;

/// A key press as the reading area sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub vk: u16,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// What a key does in the reading area. `None` from [`key_action`] leaves the
/// key to the RichEdit (caret movement, selection, copy).
#[derive(Debug, Clone, PartialEq)]
pub enum KeyAction {
    NextPage,
    PrevPage,
    FirstPage,
    LastPage,
    /// Up on the first line: the previous page's last line.
    PrevPageEnd,
    Element { types: Vec<ElementType>, reverse: bool },
    NextSection,
    PrevSection,
    SectionStart,
    SectionEnd,
    /// Ctrl+A: the whole document, not just the page in the control.
    SelectAll,
    /// Ctrl+C or Ctrl+Insert. Left to the RichEdit unless the whole document is selected.
    Copy,
}

/// Maps a key to its reading action. `on_first_line` is whether the caret sits
/// on the first (wrapped) line of the page.
pub fn key_action(k: Key, on_first_line: bool) -> Option<KeyAction> {
    let plain = !k.ctrl && !k.alt && !k.shift;
    match k.vk {
        VK_NEXT if plain => Some(KeyAction::NextPage),
        VK_PRIOR if plain => Some(KeyAction::PrevPage),
        VK_UP if plain && on_first_line => Some(KeyAction::PrevPageEnd),
        VK_HOME if k.ctrl && !k.alt => Some(KeyAction::FirstPage),
        VK_END if k.ctrl && !k.alt => Some(KeyAction::LastPage),
        VK_NEXT if k.alt => Some(KeyAction::NextSection),
        VK_PRIOR if k.alt => Some(KeyAction::PrevSection),
        VK_HOME if k.alt => Some(KeyAction::SectionStart),
        VK_END if k.alt => Some(KeyAction::SectionEnd),
        VK_A if k.ctrl && !k.alt && !k.shift => Some(KeyAction::SelectAll),
        VK_C | VK_INSERT if k.ctrl && !k.alt && !k.shift => Some(KeyAction::Copy),
        _ if k.ctrl || k.alt => None,
        vk => {
            let types = match vk as u8 {
                b'H' => HEADING_TYPES.to_vec(),
                b'K' => vec![ElementType::Link],
                b'T' => vec![ElementType::Table],
                b'L' => vec![ElementType::List],
                b'Q' => vec![ElementType::Quote],
                b'G' => vec![ElementType::Figure],
                d @ b'1'..=b'6' => vec![ElementType::heading(d - b'0')],
                _ => return None,
            };
            Some(KeyAction::Element { types, reverse: k.shift })
        }
    }
}

/// Prism's friendly plural for "No more ..." announcements.
fn plural(types: &[ElementType]) -> &'static str {
    match types {
        [ElementType::Link] => "links",
        [ElementType::Table] => "tables",
        [ElementType::List] => "lists",
        [ElementType::Quote] => "blockquotes",
        [ElementType::Figure] => "figures",
        [ElementType::CodeBlock] => "code blocks",
        [_] => "elements",
        _ => "headings",
    }
}

/// The page as the reading control shows it: ending in a line break, as
/// Prism's pages do. Down on the last line then has an empty line to move to,
/// which is the end of the page; without it the caret cannot reach the end by
/// arrowing and continuous reading never turns the page. Offsets into the
/// page text are unchanged.
fn shown_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.ends_with(['\n', '\r']) { text.into() } else { format!("{text}\n").into() }
}

/// "1 page", "42 pages".
fn pages(n: usize) -> String {
    if n == 1 { "1 page".into() } else { format!("{n} pages") }
}

/// Every page's text as one document. Pages split from one text
/// (`document::utils::paginate_text`) end in whitespace and join back exactly;
/// a page ending mid-line, as PDF pages do, gets a blank line before the next.
fn join_pages(pages: &[String]) -> String {
    let mut out = String::new();
    for page in pages.iter().filter(|p| !p.is_empty()) {
        if !out.is_empty() && !out.ends_with(char::is_whitespace) {
            out.push_str("\n\n");
        }
        out.push_str(page);
    }
    out
}

/// What InTome says after copying the whole document. `missing` is how many
/// pages still have no text because they wait for OCR.
fn copied_message(page_count: usize, missing: usize) -> String {
    let copied = format!("Copied entire document, {}", pages(page_count));
    match missing {
        0 => copied,
        1 => format!("{copied}. 1 page has no text yet and needs OCR."),
        n => format!("{copied}. {n} pages have no text yet and need OCR."),
    }
}

/// "Heading 2: Introduction", the element's text cut at 80 chars.
pub fn element_announcement(e: &SemanticElement) -> String {
    format!("{}: {}", e.element_type.label(), e.text.chars().take(80).collect::<String>())
}

thread_local! {
    static LIVE: RefCell<Vec<Weak<ContentView>>> = const { RefCell::new(Vec::new()) };
}

pub struct ContentView {
    pub edit: RichEdit,
    pub filename: String,
    doc: RefCell<Box<dyn Document>>,
    page: Cell<usize>,
    search: RefCell<SearchEngine>,
    /// True while the page turns without its spoken page number: a continuous
    /// reading turn (the screen reader's say-all is speaking) or a jump that
    /// announces its own message. Guards the re-entrant selection event of the
    /// post-turn caret reset, and tells the main window to stay silent.
    advancing: Cell<bool>,
    end_announced: Cell<bool>,
    /// A single end-of-page boundary can raise several caret events as say-all
    /// settles; the page turns at most once per 0.75 s.
    last_turn: Cell<Option<Instant>>,
    /// The `WM_CHAR` of a key we already handled, so the read-only control does not beep.
    eat_char: Cell<bool>,
    /// Ctrl+A selected every page. The control only holds the current page, so
    /// this is InTome's selection, shown as a highlight; any caret move ends it.
    whole_doc: Cell<bool>,
}

impl ContentView {
    /// Creates the reading control on `parent` for `doc`. Call
    /// [`Self::show_current_page`] once the caller is ready for `PageChanged`.
    pub fn new(parent: isize, doc: Box<dyn Document>, filename: String) -> anyhow::Result<Rc<Self>> {
        let edit = RichEdit::create(parent, CONTROL_ID)?;
        a11y::set_name(edit.0, "Document");
        let view = Rc::new(Self {
            edit,
            filename,
            doc: RefCell::new(doc),
            page: Cell::new(0),
            search: RefCell::default(),
            advancing: Cell::new(false),
            end_announced: Cell::new(false),
            last_turn: Cell::new(None),
            eat_char: Cell::new(false),
            whole_doc: Cell::new(false),
        });
        view.apply_font();
        let weak = Rc::downgrade(&view);
        edit.subclass(Box::new(move |msg, wparam, _| weak.upgrade().and_then(|v| v.on_message(msg, wparam))));
        LIVE.with(|l| {
            let mut l = l.borrow_mut();
            l.retain(|w| w.strong_count() > 0);
            l.push(Rc::downgrade(&view));
        });
        Ok(view)
    }

    /// The live view whose reading control is `hwnd`.
    pub fn find(hwnd: isize) -> Option<Rc<Self>> {
        LIVE.with(|l| l.borrow().iter().filter_map(Weak::upgrade).find(|v| v.edit.0 == hwnd))
    }

    pub fn apply_font(&self) {
        let d = config::get().display;
        self.edit.set_font(d.effective_font_face(), d.font_size);
    }

    pub fn with_doc<R>(&self, f: impl FnOnce(&mut dyn Document) -> R) -> R {
        f(self.doc.borrow_mut().as_mut())
    }

    pub fn page_count(&self) -> usize {
        self.doc.borrow().page_count()
    }

    pub fn current_page(&self) -> usize {
        self.page.get()
    }

    pub fn page_text(&self, index: usize) -> String {
        let mut doc = self.doc.borrow_mut();
        if index < doc.page_count() { doc.page(index).text.clone() } else { String::new() }
    }

    fn current_text(&self) -> String {
        self.page_text(self.page.get())
    }

    pub fn is_advancing(&self) -> bool {
        self.advancing.get()
    }

    pub fn document_hash(&self) -> String {
        crate::document::cache::file_hash(self.doc.borrow().path())
    }

    // Page display

    /// Shows the current page without a "Page N of M" announcement, for a
    /// caller that speaks its own message (opening a document).
    pub fn show_current_page_quietly(&self) {
        self.advancing.set(true);
        self.show_current_page();
        self.advancing.set(false);
    }

    /// Shows `page` with the caret at `caret` (chars), both clamped to the document,
    /// without a page announcement or a continuous-reading turn: reopening a document.
    pub fn restore_position(&self, page: usize, caret: usize) {
        self.page.set(page.min(self.page_count().saturating_sub(1)));
        self.advancing.set(true);
        self.show_current_page();
        self.select(caret, caret);
        self.advancing.set(false);
    }

    pub fn show_current_page(&self) {
        self.end_announced.set(false);
        // New text drops the highlight, and the caret moves to the page start.
        self.whole_doc.set(false);
        self.edit.set_text(&shown_text(&self.current_text()));
        self.edit.set_selection(0, 0);
        events::emit(AppEvent::PageChanged { page_num: self.page.get() + 1, page_count: self.page_count() });
    }

    pub fn go_to_page(&self, index: usize) -> bool {
        if index >= self.page_count() {
            return false;
        }
        self.page.set(index);
        self.show_current_page();
        true
    }

    /// [`Self::go_to_page`] without the spoken page number, for a turn whose
    /// caller announces its own message: two interrupting announcements in a
    /// row cut the first off after a syllable.
    fn go_to_page_quietly(&self, index: usize) -> bool {
        self.advancing.set(true);
        let turned = self.go_to_page(index);
        self.advancing.set(false);
        turned
    }

    pub fn next_page(&self) -> bool {
        self.go_to_page(self.page.get() + 1)
    }

    pub fn prev_page(&self) -> bool {
        self.page.get() > 0 && self.go_to_page(self.page.get() - 1)
    }

    pub fn go_to_end(&self) {
        let count = self.page_count();
        if count > 0 {
            self.go_to_page(count - 1);
        }
    }

    /// Turns to the previous page with the caret at the start of its last
    /// line, not its very end: a caret on the trailing line break cannot move
    /// down, so Down would raise no selection change and continuous reading
    /// would never advance. False on the first page.
    pub fn go_to_prev_page_end(&self) -> bool {
        if !self.prev_page() {
            return false;
        }
        let last = offset_len(&self.current_text()).saturating_sub(1);
        let start = self.edit.line_start(self.edit.line_from_offset(last));
        self.advancing.set(true);
        self.edit.set_selection(start, start);
        self.advancing.set(false);
        true
    }

    pub fn focus(&self) {
        unsafe {
            let _ = SetFocus(Some(HWND(self.edit.0 as _)));
        }
    }

    // Selection, in chars

    pub fn selection(&self) -> (usize, usize) {
        let (s, e) = self.edit.selection();
        let text = self.current_text();
        (offset_to_char(&text, s), offset_to_char(&text, e))
    }

    pub fn caret(&self) -> usize {
        self.selection().0
    }

    pub fn select(&self, start: usize, end: usize) {
        // Set first: an unchanged selection raises no event to end it.
        self.whole_doc.set(false);
        let text = self.current_text();
        self.edit.set_selection(char_to_offset(&text, start), char_to_offset(&text, end));
    }

    /// Puts the caret at `start` and paints `start..end` (chars) as a visual
    /// highlight that screen readers do not announce, unlike a selection.
    pub fn mark(&self, start: usize, end: usize) {
        self.whole_doc.set(false);
        let text = self.current_text();
        let (s, e) = (char_to_offset(&text, start), char_to_offset(&text, end));
        self.edit.set_selection(s, s);
        self.edit.set_highlight(s, e);
    }

    /// Whether Ctrl+A selected the whole document and the caret has not moved since.
    pub fn whole_document_selected(&self) -> bool {
        self.whole_doc.get()
    }

    /// Selects every page. The current page is painted, not selected: a
    /// selection would make the screen reader read the whole page aloud as
    /// "selected" over the announcement.
    pub fn select_all_document(&self) {
        let shown = offset_len(&shown_text(&self.current_text()));
        self.edit.set_highlight(0, shown);
        self.whole_doc.set(true);
        events::announce(format!("Selected entire document, {}", pages(self.page_count())));
    }

    /// Copies the whole document when it is selected, else the control's own selection.
    pub fn copy(&self) {
        if !self.whole_doc.get() {
            self.edit.copy();
            return;
        }
        let (text, count, missing) = {
            let mut doc = self.doc.borrow_mut();
            let count = doc.page_count();
            let texts: Vec<String> = (0..count).map(|i| doc.page(i).text.clone()).collect();
            let missing = doc.ocr_page_indices().into_iter().filter(|&i| texts.get(i).is_some_and(|t| t.trim().is_empty())).count();
            (join_pages(&texts), count, missing)
        };
        match clipboard::set_text(self.edit.0, &text) {
            Ok(()) => events::announce(copied_message(count, missing)),
            Err(e) => {
                log::error!("copying the document to the clipboard failed: {e:#}");
                events::announce("Could not copy to the clipboard.");
            }
        }
    }

    /// The selected text, sliced from the page by char index. Slicing the page
    /// with control offsets would drift by one per preceding `\r\n`.
    pub fn selected_text(&self) -> String {
        let (s, e) = self.selection();
        self.current_text().chars().skip(s).take(e.saturating_sub(s)).collect()
    }

    // Continuous reading: with `general.continuous_reading` on, a caret that
    // reaches the end of the page turns to the next page with the caret at the
    // top, so a screen reader's say-all flows across pages.

    pub fn stop_continuous_reading(&self) {
        self.end_announced.set(false);
        if config::get().general.continuous_reading {
            events::emit(AppEvent::ContinuousReadingStopped);
        }
    }

    /// Called for every `EN_SELCHANGE` with control offsets.
    pub fn on_sel_change(&self, cp_min: usize, cp_max: usize) {
        if self.whole_doc.replace(false) {
            self.edit.set_highlight(0, 0);
        }
        let line = self.edit.line_from_offset(cp_min);
        events::emit(AppEvent::CaretMoved { line, col: cp_min.saturating_sub(self.edit.line_start(line)) });
        self.maybe_advance(cp_min, cp_max);
    }

    fn maybe_advance(&self, cp_min: usize, cp_max: usize) {
        // A selected range is not a resting caret, so selecting to the end (or
        // Select All) never turns the page.
        if !config::get().general.continuous_reading || self.advancing.get() || cp_min != cp_max {
            return;
        }
        if cp_max < offset_len(&shown_text(&self.current_text())) {
            return;
        }
        if self.last_turn.get().is_some_and(|t| t.elapsed() <= Duration::from_millis(750)) {
            return;
        }
        self.last_turn.set(Some(Instant::now()));
        self.advancing.set(true);
        let advanced = self.next_page();
        self.advancing.set(false);
        // No focus() here: a focus event interrupts the screen reader's say-all.
        if !advanced && !self.end_announced.get() {
            self.end_announced.set(true);
            // Queued so it follows the last words instead of cutting them off.
            events::announce_queued("End of document");
            events::emit(AppEvent::ContinuousReadingStopped);
        }
    }

    // Keys

    fn on_message(&self, msg: u32, wparam: usize) -> Option<isize> {
        match msg {
            WM_GETDLGCODE => Some(DLGC_READING),
            WM_CHAR if self.eat_char.replace(false) => Some(0),
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let down = |vk| unsafe { GetKeyState(vk as i32) } < 0;
                let key = Key { vk: wparam as u16, ctrl: down(VK_CONTROL.0), shift: down(VK_SHIFT.0), alt: down(VK_MENU.0) };
                let (caret, _) = self.edit.selection();
                let action = key_action(key, self.edit.line_from_offset(caret) == 0)?;
                if action == KeyAction::Copy && !self.whole_doc.get() {
                    return None;
                }
                self.eat_char.set(matches!(action, KeyAction::Element { .. } | KeyAction::SelectAll | KeyAction::Copy));
                self.run(action);
                Some(0)
            }
            _ => None,
        }
    }

    pub fn run(&self, action: KeyAction) {
        match action {
            KeyAction::NextPage => {
                if !self.next_page() {
                    events::announce("Last page");
                }
            }
            KeyAction::PrevPage => {
                if !self.prev_page() {
                    events::announce("First page");
                }
            }
            KeyAction::SelectAll => self.select_all_document(),
            KeyAction::Copy => self.copy(),
            KeyAction::FirstPage => {
                self.go_to_page(0);
            }
            KeyAction::LastPage => self.go_to_end(),
            KeyAction::PrevPageEnd => {
                self.go_to_prev_page_end();
            }
            KeyAction::Element { types, reverse } => self.navigate_element(&types, reverse),
            KeyAction::NextSection => self.next_section(),
            KeyAction::PrevSection => self.prev_section(),
            KeyAction::SectionStart => match self.with_doc(|d| find_section_bounds(d, self.page.get())) {
                Some((start, _)) => {
                    self.go_to_page_quietly(start);
                    events::announce(format!("Start of section, page {}", start + 1));
                }
                None => {
                    self.go_to_page(0);
                }
            },
            KeyAction::SectionEnd => match self.with_doc(|d| find_section_bounds(d, self.page.get())) {
                Some((_, end)) => {
                    self.go_to_page_quietly(end);
                    events::announce(format!("End of section, page {}", end + 1));
                }
                None => self.go_to_end(),
            },
        }
    }

    // Structured navigation

    fn navigate_element(&self, types: &[ElementType], reverse: bool) {
        let (from, page) = (self.caret(), self.page.get());
        let found = self.with_doc(|d| {
            if reverse { element_nav::find_prev(d, types, from, page) } else { element_nav::find_next(d, types, from, page) }
        });
        match found {
            Some(r) => self.go_to_element(r.page_index, &r.element),
            None => events::announce(format!("No more {}", plural(types))),
        }
    }

    /// Shows `element` on page `page_index`, selected when
    /// `navigation.highlight_on_navigate` is on, and announces it.
    pub fn go_to_element(&self, page_index: usize, element: &SemanticElement) {
        if page_index != self.page.get() {
            self.go_to_page_quietly(page_index);
        }
        let end = if config::get().navigation.highlight_on_navigate { element.end_pos } else { element.start_pos };
        self.mark(element.start_pos, end);
        self.focus();
        events::announce(element_announcement(element));
    }

    fn next_section(&self) {
        let toc = self.with_doc(|d| get_toc_entries_flat(d));
        if toc.is_empty() {
            return events::announce("No table of contents");
        }
        match toc.iter().find(|e| e.page_index > self.page.get()) {
            Some(e) => {
                self.go_to_page_quietly(e.page_index);
                events::announce(format!("Section: {}", e.title));
            }
            None => events::announce("Last section"),
        }
    }

    fn prev_section(&self) {
        let toc = self.with_doc(|d| get_toc_entries_flat(d));
        if toc.is_empty() {
            return events::announce("No table of contents");
        }
        match toc.iter().rev().find(|e| e.page_index < self.page.get()) {
            Some(e) => {
                self.go_to_page_quietly(e.page_index);
                events::announce(format!("Section: {}", e.title));
            }
            None => events::announce("First section"),
        }
    }

    /// Title of the TOC entry that starts on `index`, if any.
    pub fn section_title(&self, index: usize) -> Option<String> {
        self.with_doc(|d| get_toc_entries_flat(d)).into_iter().find(|e| e.page_index == index).map(|e| e.title)
    }

    // Search

    /// Searches every page; the total match count.
    pub fn execute_search(&self, query: &str, case_sensitive: bool, wrap: bool) -> usize {
        let mut search = self.search.borrow_mut();
        self.with_doc(|d| search.search(query, d, case_sensitive, wrap))
    }

    pub fn next_match(&self) -> bool {
        let found = self.search.borrow_mut().next_match(self.page.get(), self.caret());
        self.go_to_match(found)
    }

    pub fn prev_match(&self) -> bool {
        let found = self.search.borrow_mut().prev_match(self.page.get(), self.caret());
        self.go_to_match(found)
    }

    fn go_to_match(&self, found: Option<SearchMatch>) -> bool {
        let Some(m) = found else {
            events::announce("No more matches");
            return false;
        };
        if m.page_index != self.page.get() {
            self.go_to_page_quietly(m.page_index);
        }
        self.select(m.start_pos, m.end_pos);
        self.focus();
        let (idx, total) = self.match_info();
        events::announce(format!("Match {idx} of {total}: {}", m.context));
        true
    }

    /// `(current match, 1-based; total)` for the find bar.
    pub fn match_info(&self) -> (usize, usize) {
        self.search.borrow().match_info()
    }

    // OCR results

    /// Applies an OCR result to a page and refreshes it when it is on screen.
    pub fn update_ocr_page(&self, index: usize, result: ParsedOcr) {
        let Ok(mut doc) = self.doc.try_borrow_mut() else {
            log::warn!("page {} OCR result arrived while the document was busy; it stays in the OCR cache", index + 1);
            return;
        };
        doc.set_ocr_result(index, result.text, result.elements);
        drop(doc);
        if self.page.get() == index {
            self.show_current_page();
        }
    }

    /// Releases the document's continuous-reading state and destroys the control.
    pub fn close(&self) {
        self.stop_continuous_reading();
        self.search.borrow_mut().clear();
        unsafe {
            let _ = DestroyWindow(HWND(self.edit.0 as _));
        }
    }
}

/// The [`ReadingTarget`] for a reading control. The reading thread only holds
/// the control's HWND; every call runs on the UI thread and finds the view there.
pub struct ViewTarget(pub isize);

impl ViewTarget {
    fn with<R: Default>(&self, f: impl FnOnce(&ContentView) -> R) -> R {
        ContentView::find(self.0).map(|v| f(&v)).unwrap_or_default()
    }
}

impl ReadingTarget for ViewTarget {
    fn page_count(&self) -> usize {
        self.with(|v| v.page_count())
    }
    fn current_page(&self) -> usize {
        self.with(|v| v.current_page())
    }
    fn page_text(&self, index: usize) -> String {
        self.with(|v| v.page_text(index))
    }
    fn cursor_offset(&self) -> usize {
        self.with(|v| v.caret())
    }
    fn highlight(&self, start: usize, end: usize) {
        self.with(|v| v.mark(start, end))
    }
    fn go_to_page(&self, index: usize) {
        self.with(|v| {
            v.go_to_page(index);
        })
    }
    fn section_title(&self, index: usize) -> Option<String> {
        self.with(|v| v.section_title(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(vk: u16, ctrl: bool, shift: bool, alt: bool) -> Key {
        Key { vk, ctrl, shift, alt }
    }

    #[test]
    fn keys_map_to_reading_actions() {
        assert_eq!(key_action(key(VK_NEXT, false, false, false), false), Some(KeyAction::NextPage));
        assert_eq!(key_action(key(VK_NEXT, false, true, false), false), None); // Shift+PageDown selects
        assert_eq!(key_action(key(VK_NEXT, false, false, true), false), Some(KeyAction::NextSection));
        assert_eq!(key_action(key(VK_HOME, true, false, false), false), Some(KeyAction::FirstPage));
        assert_eq!(key_action(key(VK_UP, false, false, false), true), Some(KeyAction::PrevPageEnd));
        assert_eq!(key_action(key(VK_UP, false, false, false), false), None);
        assert_eq!(key_action(key(b'H' as u16, false, true, false), false), Some(KeyAction::Element { types: HEADING_TYPES.to_vec(), reverse: true }));
        assert_eq!(key_action(key(b'3' as u16, false, false, false), false), Some(KeyAction::Element { types: vec![ElementType::Heading3], reverse: false }));
        assert_eq!(key_action(key(b'K' as u16, true, false, false), false), None); // Ctrl+K is not a reading key
        assert_eq!(key_action(key(b'Z' as u16, false, false, false), false), None);
    }

    #[test]
    fn select_all_and_copy_keys() {
        assert_eq!(key_action(key(VK_A, true, false, false), false), Some(KeyAction::SelectAll));
        assert_eq!(key_action(key(VK_A, false, false, false), false), None); // plain A is not a reading key
        assert_eq!(key_action(key(VK_A, true, true, false), false), None);
        assert_eq!(key_action(key(VK_C, true, false, false), false), Some(KeyAction::Copy));
        assert_eq!(key_action(key(VK_INSERT, true, false, false), false), Some(KeyAction::Copy));
        assert_eq!(key_action(key(VK_INSERT, false, true, false), false), None); // Shift+Insert pastes
    }

    #[test]
    fn pages_join_into_one_document() {
        let split = ["One para.\n\n", "Two runs on \n", "and on."].map(String::from);
        assert_eq!(join_pages(&split), "One para.\n\nTwo runs on \nand on.");
        let pdf = ["Page one", "", "Page three\n", "Page four"].map(String::from);
        assert_eq!(join_pages(&pdf), "Page one\n\nPage three\nPage four");
        assert_eq!(join_pages(&[]), "");
    }

    #[test]
    fn copy_message_counts_pages_waiting_for_ocr() {
        assert_eq!(copied_message(1, 0), "Copied entire document, 1 page");
        assert_eq!(copied_message(12, 1), "Copied entire document, 12 pages. 1 page has no text yet and needs OCR.");
        assert_eq!(copied_message(12, 3), "Copied entire document, 12 pages. 3 pages have no text yet and need OCR.");
    }

    #[test]
    fn shown_page_always_ends_in_a_line_break() {
        assert_eq!(shown_text("Part One. "), "Part One. \n");
        assert_eq!(shown_text("quiet old.\n\n"), "quiet old.\n\n");
        assert_eq!(shown_text(""), "\n");
    }

    #[test]
    fn no_more_announcements_use_prism_plurals() {
        assert_eq!(plural(&HEADING_TYPES), "headings");
        assert_eq!(plural(&[ElementType::Quote]), "blockquotes");
        assert_eq!(plural(&[ElementType::Heading2]), "elements");
    }
}
