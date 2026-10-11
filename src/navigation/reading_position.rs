//! The reading position of one open document: its current page, and what
//! each move does and speaks. No window code: the reading area applies each
//! [`Outcome`] to its control, which alone knows control offsets and line
//! layout. Positions are char indices into `Page::text`.
//!
//! Every page turn either speaks "Page N of M" or stays quiet, because the
//! move speaks its own message or a screen reader's say-all is reading: two
//! interrupting announcements in a row cut the first off after a syllable.

use std::time::{Duration, Instant};

use crate::document::TocEntry;
use crate::navigation::search::SearchMatch;
use crate::structured_text::elements::{ElementType, SemanticElement};

/// A single end-of-page boundary can raise several caret events as say-all
/// settles; continuous reading turns the page at most once in this time.
const TURN_DEBOUNCE: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, PartialEq)]
pub enum Speech {
    Interrupt(String),
    /// After current speech, so it follows say-all's last words.
    Queued(String),
}

/// Where the caret goes once the page (if any) is shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Caret {
    /// Unchanged, or the page start when a page is shown.
    Stay,
    Select(usize, usize),
    /// Caret at the start, `start..end` painted as a highlight screen readers do not announce.
    Mark(usize, usize),
    /// The start of the page's last visual line, which only the control knows.
    LastLineStart,
}

/// What a move does, in the order the reading area applies it.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// The page to show, its text reloaded with the caret at its start.
    pub show: Option<usize>,
    /// "Page N of M", spoken as soon as the page shows, before the caret moves.
    pub page_number: Option<String>,
    pub caret: Caret,
    pub focus: bool,
    pub speech: Option<Speech>,
}

impl Default for Outcome {
    fn default() -> Self {
        Self { show: None, page_number: None, caret: Caret::Stay, focus: false, speech: None }
    }
}

impl Outcome {
    fn say(message: impl Into<String>) -> Self {
        Self { speech: Some(Speech::Interrupt(message.into())), ..Self::default() }
    }
}

pub struct ReadingPosition {
    /// Chars in each page, so the reading percent is a share of the whole document.
    page_chars: Vec<usize>,
    page: usize,
    end_announced: bool,
    last_turn: Option<Instant>,
    /// `(page, caret, percent)` of the last slider move, so the slider stays
    /// where the user put it until the caret moves.
    slider: Option<(usize, usize, usize)>,
}

impl ReadingPosition {
    pub fn new(page_chars: Vec<usize>) -> Self {
        Self { page_chars, page: 0, end_announced: false, last_turn: None, slider: None }
    }

    pub fn page(&self) -> usize {
        self.page
    }

    /// How far through the document char `caret` of the current page is, as a percent of all its characters.
    pub fn percent(&self, caret: usize) -> usize {
        match self.slider {
            Some((page, at, percent)) if (page, at) == (self.page, caret) => percent,
            _ => percent_at(&self.page_chars, self.page, caret),
        }
    }

    fn show(&mut self, index: usize, page_number: bool) -> Outcome {
        self.page = index;
        self.end_announced = false;
        self.slider = None;
        let page_number = page_number.then(|| format!("Page {} of {}", index + 1, self.page_chars.len()));
        Outcome { show: Some(index), page_number, ..Outcome::default() }
    }

    /// Turns to `index` and says its number: TOC, bookmarks, read-aloud. Nothing when out of range.
    pub fn go_to_page(&mut self, index: usize) -> Outcome {
        if index >= self.page_chars.len() {
            return Outcome::default();
        }
        self.show(index, true)
    }

    fn go_to_page_quietly(&mut self, index: usize) -> Outcome {
        Outcome { page_number: None, ..self.go_to_page(index) }
    }

    pub fn next_page(&mut self) -> Outcome {
        match self.page + 1 < self.page_chars.len() {
            true => self.show(self.page + 1, true),
            false => Outcome::say("Last page"),
        }
    }

    pub fn prev_page(&mut self) -> Outcome {
        match self.page > 0 {
            true => self.show(self.page - 1, true),
            false => Outcome::say("First page"),
        }
    }

    pub fn last_page(&mut self) -> Outcome {
        self.go_to_page(self.page_chars.len().saturating_sub(1))
    }

    /// Up on the first line: the previous page with the caret on its last
    /// line, not its very end. A caret on the trailing line break cannot move
    /// down, so continuous reading would never advance. Nothing on the first page.
    pub fn prev_page_end(&mut self) -> Outcome {
        if self.page == 0 {
            return Outcome::default();
        }
        Outcome { caret: Caret::LastLineStart, ..self.show(self.page - 1, true) }
    }

    /// Shows the current page again and says its number, for new page text (OCR).
    pub fn refresh(&mut self) -> Outcome {
        self.show(self.page, true)
    }

    /// Page `index` now holds `chars` characters; refreshes it when it is the current page.
    pub fn page_text_changed(&mut self, index: usize, chars: usize) -> Outcome {
        if let Some(n) = self.page_chars.get_mut(index) {
            *n = chars;
        }
        if index == self.page { self.refresh() } else { Outcome::default() }
    }

    /// Reopening a document: `page` clamped to the document, the caret at
    /// `caret`, and nothing spoken, as the caller says it opened.
    pub fn restore(&mut self, page: usize, caret: usize) -> Outcome {
        let page = page.min(self.page_chars.len().saturating_sub(1));
        Outcome { caret: Caret::Select(caret, caret), ..self.show(page, false) }
    }

    /// The caret at the start of the line `percent` of the way through the
    /// document. `page_text` gives a page's text, to find that line.
    pub fn go_to_percent(&mut self, percent: usize, page_text: impl FnOnce(usize) -> String) -> Outcome {
        let (page, ch) = place_at(&self.page_chars, percent);
        let mut out = Outcome::default();
        if page != self.page {
            out = self.go_to_page(page);
            if out.show.is_none() {
                return out;
            }
        }
        let text = page_text(page);
        let line_start = text.chars().take(ch).collect::<Vec<_>>().iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1);
        self.slider = Some((page, line_start, percent));
        Outcome { caret: Caret::Select(line_start, line_start), ..out }
    }

    /// Alt+Home: the first page of the current section, `bounds` from `find_section_bounds`.
    pub fn section_start(&mut self, bounds: Option<(usize, usize)>) -> Outcome {
        match bounds {
            Some((start, _)) => Outcome { speech: Some(Speech::Interrupt(format!("Start of section, page {}", start + 1))), ..self.go_to_page_quietly(start) },
            None => self.go_to_page(0),
        }
    }

    /// Alt+End: the last page of the current section.
    pub fn section_end(&mut self, bounds: Option<(usize, usize)>) -> Outcome {
        match bounds {
            Some((_, end)) => Outcome { speech: Some(Speech::Interrupt(format!("End of section, page {}", end + 1))), ..self.go_to_page_quietly(end) },
            None => self.last_page(),
        }
    }

    /// The first TOC entry after the current page, `toc` flattened in page order.
    pub fn next_section(&mut self, toc: &[TocEntry]) -> Outcome {
        if toc.is_empty() {
            return Outcome::say("No table of contents");
        }
        match toc.iter().find(|e| e.page_index > self.page) {
            Some(e) => self.to_section(e),
            None => Outcome::say("Last section"),
        }
    }

    pub fn prev_section(&mut self, toc: &[TocEntry]) -> Outcome {
        if toc.is_empty() {
            return Outcome::say("No table of contents");
        }
        match toc.iter().rev().find(|e| e.page_index < self.page) {
            Some(e) => self.to_section(e),
            None => Outcome::say("First section"),
        }
    }

    fn to_section(&mut self, e: &TocEntry) -> Outcome {
        Outcome { speech: Some(Speech::Interrupt(format!("Section: {}", e.title))), ..self.go_to_page_quietly(e.page_index) }
    }

    /// Shows `found` (page, element), selected when `highlight` is on, and says
    /// it; or "No more ..." for `types` when nothing was found.
    pub fn element(&mut self, found: Option<(usize, &SemanticElement)>, types: &[ElementType], highlight: bool) -> Outcome {
        let Some((page, e)) = found else {
            return Outcome::say(format!("No more {}", plural(types)));
        };
        let out = if page != self.page { self.go_to_page_quietly(page) } else { Outcome::default() };
        let end = if highlight { e.end_pos } else { e.start_pos };
        Outcome { caret: Caret::Mark(e.start_pos, end), focus: true, speech: Some(Speech::Interrupt(element_announcement(e))), ..out }
    }

    /// Selects search match `found`, `(index, total)` being its place among all matches.
    pub fn search_match(&mut self, found: Option<&SearchMatch>, (index, total): (usize, usize)) -> Outcome {
        let Some(m) = found else {
            return Outcome::say("No more matches");
        };
        let out = if m.page_index != self.page { self.go_to_page_quietly(m.page_index) } else { Outcome::default() };
        Outcome {
            caret: Caret::Select(m.start_pos, m.end_pos),
            focus: true,
            speech: Some(Speech::Interrupt(format!("Match {index} of {total}: {}", m.context))),
            ..out
        }
    }

    /// Continuous reading: the user's caret moved, `at_end` being a resting
    /// caret (no selection) at the end of the page. A turn to the next page
    /// is silent; on the last page, the outcome shows no page and queues
    /// "End of document" once, and continuous reading stops.
    pub fn caret_moved(&mut self, at_end: bool, now: Instant, continuous_reading: bool) -> Option<Outcome> {
        if !continuous_reading || !at_end {
            return None;
        }
        if self.last_turn.is_some_and(|t| now.saturating_duration_since(t) <= TURN_DEBOUNCE) {
            return None;
        }
        self.last_turn = Some(now);
        if self.page + 1 < self.page_chars.len() {
            return Some(self.show(self.page + 1, false));
        }
        if self.end_announced {
            return None;
        }
        self.end_announced = true;
        Some(Outcome { speech: Some(Speech::Queued("End of document".into())), ..Outcome::default() })
    }
}

/// Percent (0-100) of the document before char `caret` of page `page`, where
/// `page_chars` holds each page's length. Floor, so the last char reads 100
/// only at the very end.
pub fn percent_at(page_chars: &[usize], page: usize, caret: usize) -> usize {
    let total: usize = page_chars.iter().sum();
    if total == 0 {
        return 0;
    }
    let before: usize = page_chars.iter().take(page).sum::<usize>() + caret.min(page_chars.get(page).copied().unwrap_or(0));
    before * 100 / total
}

/// The page and char `percent` of the way through the document. 100 is the end
/// of the last page.
pub fn place_at(page_chars: &[usize], percent: usize) -> (usize, usize) {
    let total: usize = page_chars.iter().sum();
    let mut target = total * percent.min(100) / 100;
    for (i, &len) in page_chars.iter().enumerate() {
        if target < len || i + 1 == page_chars.len() {
            return (i, target.min(len));
        }
        target -= len;
    }
    (0, 0)
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

/// "Heading 2: Introduction", the element's text cut at 80 chars.
fn element_announcement(e: &SemanticElement) -> String {
    format!("{}: {}", e.element_type.label(), e.text.chars().take(80).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured_text::elements::HEADING_TYPES;

    /// Three pages of 100, 300 and 600 chars.
    fn three_pages() -> ReadingPosition {
        ReadingPosition::new(vec![100, 300, 600])
    }

    fn page_number(page: usize, count: usize) -> Option<String> {
        Some(format!("Page {page} of {count}"))
    }

    fn said(s: &str) -> Option<Speech> {
        Some(Speech::Interrupt(s.into()))
    }

    fn toc(entries: &[(&str, usize)]) -> Vec<TocEntry> {
        entries.iter().map(|&(title, page_index)| TocEntry { title: title.into(), page_index, level: 0, children: vec![] }).collect()
    }

    #[test]
    fn page_keys_stop_at_the_ends_and_say_so() {
        let mut p = three_pages();
        assert_eq!(p.prev_page(), Outcome { speech: said("First page"), ..Outcome::default() });
        p.last_page();
        assert_eq!(p.next_page(), Outcome { speech: said("Last page"), ..Outcome::default() });
        assert_eq!(p.page(), 2);
    }

    #[test]
    fn plain_page_turns_say_the_page_number() {
        let mut p = three_pages();
        assert_eq!(p.next_page(), Outcome { show: Some(1), page_number: page_number(2, 3), ..Outcome::default() });
        // TOC and bookmark "Go to" turn the same way; out of range does nothing.
        assert_eq!(p.go_to_page(2).page_number, page_number(3, 3));
        assert_eq!(p.go_to_page(3), Outcome::default());
        assert_eq!(p.prev_page_end(), Outcome { show: Some(1), page_number: page_number(2, 3), caret: Caret::LastLineStart, ..Outcome::default() });
    }

    #[test]
    fn jumps_that_speak_their_own_message_turn_quietly() {
        let mut p = three_pages();
        let o = p.next_section(&toc(&[("One", 0), ("Two", 2)]));
        assert_eq!((o.show, o.page_number, o.speech), (Some(2), None, said("Section: Two")));

        let e = SemanticElement { element_type: ElementType::Heading2, start_pos: 5, end_pos: 9, text: "Intro".into(), meta: Default::default() };
        let o = p.element(Some((0, &e)), &HEADING_TYPES, true);
        assert_eq!(o, Outcome { show: Some(0), caret: Caret::Mark(5, 9), focus: true, speech: said("Heading 2: Intro"), ..Outcome::default() });
        assert_eq!(p.element(None, &[ElementType::Table], true).speech, said("No more tables"));

        let m = SearchMatch { page_index: 1, start_pos: 3, end_pos: 7, context: "a word b".into() };
        let o = p.search_match(Some(&m), (2, 5));
        assert_eq!((o.show, o.page_number, o.caret), (Some(1), None, Caret::Select(3, 7)));
        assert_eq!(o.speech, said("Match 2 of 5: a word b"));

        let o = p.section_start(Some((0, 1)));
        assert_eq!((o.show, o.page_number, o.speech), (Some(0), None, said("Start of section, page 1")));

        // A TOC entry past the last page turns nowhere but still says its title.
        let o = p.next_section(&toc(&[("Gone", 9)]));
        assert_eq!((o.show, o.page_number, o.speech), (None, None, said("Section: Gone")));
    }

    #[test]
    fn continuous_reading_turns_silently_once_per_debounce() {
        let mut p = three_pages();
        let t = Instant::now();
        assert_eq!(p.caret_moved(true, t, true), Some(Outcome { show: Some(1), ..Outcome::default() }));
        assert_eq!(p.caret_moved(true, t + Duration::from_millis(100), true), None, "say-all settling at the boundary");
        assert_eq!(p.caret_moved(true, t + Duration::from_millis(800), true).and_then(|o| o.show), Some(2));
    }

    #[test]
    fn continuous_reading_queues_end_of_document_once() {
        let mut p = ReadingPosition::new(vec![10]);
        let t = Instant::now();
        let end = Some(Outcome { speech: Some(Speech::Queued("End of document".into())), ..Outcome::default() });
        assert_eq!(p.caret_moved(true, t, true), end);
        assert_eq!(p.caret_moved(true, t + Duration::from_secs(2), true), None);
        // Showing the page again rearms it.
        p.refresh();
        assert_eq!(p.caret_moved(true, t + Duration::from_secs(4), true), end);
    }

    #[test]
    fn continuous_reading_off_or_mid_page_does_nothing() {
        let mut p = three_pages();
        assert_eq!(p.caret_moved(true, Instant::now(), false), None);
        assert_eq!(p.caret_moved(false, Instant::now(), true), None);
        assert_eq!(p.page(), 0);
    }

    #[test]
    fn slider_lands_on_a_line_start_and_keeps_its_percent() {
        let mut p = ReadingPosition::new(vec![10, 20]);
        let o = p.go_to_percent(60, |_| "first line\nsecond line\n".into()); // char 18 of 30: page 1, char 8
        assert_eq!(o, Outcome { show: Some(1), page_number: page_number(2, 2), caret: Caret::Select(0, 0), ..Outcome::default() });
        assert_eq!(p.percent(0), 60, "the slider stays where the user put it");
        assert_eq!(p.percent(11), 70, "a caret move reads the caret again");
        p.prev_page();
        p.next_page();
        assert_eq!(p.percent(0), 33, "a later move back to the same spot reads the caret");
        let o = p.go_to_percent(90, |_| "first line\nsecond line\n".into()); // page 1, char 17
        assert_eq!((o.show, o.caret), (None, Caret::Select(11, 11)));
    }

    #[test]
    fn restore_clamps_the_page_and_speaks_nothing() {
        let mut p = three_pages();
        assert_eq!(p.restore(7, 40), Outcome { show: Some(2), caret: Caret::Select(40, 40), ..Outcome::default() });
    }

    #[test]
    fn reading_percent_counts_characters_across_pages() {
        let pages = [100, 300, 600]; // 1000 chars
        assert_eq!(percent_at(&pages, 0, 0), 0);
        assert_eq!(percent_at(&pages, 0, 50), 5, "moves within a page");
        assert_eq!(percent_at(&pages, 1, 0), 10);
        assert_eq!(percent_at(&pages, 2, 300), 70);
        assert_eq!(percent_at(&pages, 2, 600), 100, "the very end");
        assert_eq!(percent_at(&pages, 2, 9999), 100, "a caret past the page clamps");
        assert_eq!(percent_at(&[], 0, 0), 0);
        // A one-page document still moves as you read.
        assert_eq!(percent_at(&[2300], 0, 1150), 50);
    }

    #[test]
    fn slider_percent_finds_its_page_and_char() {
        let pages = [100, 300, 600];
        assert_eq!(place_at(&pages, 0), (0, 0));
        assert_eq!(place_at(&pages, 5), (0, 50));
        assert_eq!(place_at(&pages, 10), (1, 0));
        assert_eq!(place_at(&pages, 70), (2, 300));
        assert_eq!(place_at(&pages, 100), (2, 600));
        assert_eq!(place_at(&pages, 250), (2, 600), "clamped to 100");
        assert_eq!(place_at(&[0, 0], 50), (1, 0), "an empty document lands on its last page");
    }

    #[test]
    fn no_more_announcements_use_prism_plurals() {
        assert_eq!(plural(&HEADING_TYPES), "headings");
        assert_eq!(plural(&[ElementType::Quote]), "blockquotes");
        assert_eq!(plural(&[ElementType::Heading2]), "elements");
    }
}
