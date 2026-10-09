//! Reading controller: orchestrates read-aloud on a background thread (port of prism/tts/reader.py).
//!
//! The reading thread never touches the document or the text control. Every
//! access goes through a [`ReadingTarget`] on the UI thread (`events::run_on_ui`
//! plus a channel for the answer, Prism's `wx.CallAfter` + `Event` handshake).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use regex::Regex;

use super::engine::TtsEngine;
use super::engines::{available_engines, create_engine};
use super::profiles::{ProfileManager, VoiceProfile};
use crate::config;
use crate::events::{self, AppEvent};

/// How long the reading thread waits for the UI thread to answer before skipping a page.
const UI_TIMEOUT: Duration = Duration::from_secs(5);

/// What the reading loop needs from the document view. Implemented by the GUI.
///
/// Every method is called on the UI thread, never on the reading thread, so an
/// implementation may hold UI-thread-only state (behind whatever makes it
/// `Send + Sync`). All text positions are char indices into [`Self::page_text`].
pub trait ReadingTarget: Send + Sync {
    fn page_count(&self) -> usize;
    /// 0-based index of the displayed page.
    fn current_page(&self) -> usize;
    /// `Page::text` of the page at `index` (0-based).
    fn page_text(&self, index: usize) -> String;
    /// Caret position in the displayed page's text, in chars.
    fn cursor_offset(&self) -> usize;
    /// Selects and scrolls to `start..end` (chars) in the displayed page.
    fn highlight(&self, start: usize, end: usize);
    /// Displays the page at `index`.
    fn go_to_page(&self, index: usize);
    /// Title of the section (table-of-contents entry) that starts on this page, if any.
    fn section_title(&self, index: usize) -> Option<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingState {
    Playing,
    Paused,
    Stopped,
}

struct Session {
    state: Mutex<ReadingState>,
    engine: Arc<dyn TtsEngine>,
    stop: AtomicBool,
    skip_forward: AtomicBool,
    skip_backward: AtomicBool,
}

#[derive(Default)]
pub struct ReadingController {
    session: Mutex<Option<Arc<Session>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ReadingController {
    pub fn new() -> Self {
        Self::default()
    }

    fn current(&self) -> Option<Arc<Session>> {
        lock(&self.session).clone()
    }

    pub fn state(&self) -> ReadingState {
        self.current().map_or(ReadingState::Stopped, |s| *lock(&s.state))
    }

    /// Begins reading from the displayed page. Call on the UI thread. `from_cursor`
    /// starts at the paragraph holding the caret instead of the top of the page.
    pub fn start(&self, target: Arc<dyn ReadingTarget>, from_cursor: bool) {
        if self.state() != ReadingState::Stopped {
            self.stop();
        }
        let available = available_engines();
        if available.is_empty() {
            events::announce("No TTS engine available.");
            return;
        }
        let profile = ProfileManager::new().get_active_profile();
        let key = available.iter().find(|e| e.key == profile.engine).unwrap_or(&available[0]).key;
        let engine = match create_engine(key) {
            Ok(e) => e,
            Err(e) => {
                events::announce(format!("Failed to initialize TTS engine: {e}"));
                return;
            }
        };
        engine.set_rate(profile.rate);
        engine.set_volume(profile.volume);
        if !profile.voice.is_empty() {
            engine.set_voice(&profile.voice);
        }

        let cfg = config::get().tts;
        let start_page = target.current_page();
        let page_count = target.page_count();
        let plan = Plan {
            start_page,
            end_page: if cfg.reading_scope == "page" { (start_page + 1).min(page_count) } else { page_count },
            // "chapter" stops where the next section begins; "book" reads to the end.
            stop_at_section: cfg.reading_scope == "chapter",
            caret: if from_cursor { target.cursor_offset() } else { 0 },
            highlight: cfg.highlight_spoken_text,
            speak_page_numbers: cfg.speak_page_numbers,
            announce_sections: cfg.announce_sections,
            profile,
        };

        let session = Arc::new(Session {
            state: Mutex::new(ReadingState::Playing),
            engine,
            stop: AtomicBool::new(false),
            skip_forward: AtomicBool::new(false),
            skip_backward: AtomicBool::new(false),
        });
        *lock(&self.session) = Some(session.clone());
        events::emit(AppEvent::TtsStarted);
        events::announce("Reading started");
        let spawned = std::thread::Builder::new().name("tts-reader".into()).spawn(move || reading_loop(&session, &target, &plan));
        if let Err(e) = spawned {
            log::error!("cannot start the reading thread: {e}");
            self.stop();
        }
    }

    pub fn pause(&self) {
        let Some(s) = self.current() else { return };
        {
            let mut state = lock(&s.state);
            if *state != ReadingState::Playing {
                return;
            }
            *state = ReadingState::Paused;
        }
        s.engine.pause();
        events::emit(AppEvent::TtsPaused);
        events::announce("Reading paused");
    }

    pub fn resume(&self) {
        let Some(s) = self.current() else { return };
        {
            let mut state = lock(&s.state);
            if *state != ReadingState::Paused {
                return;
            }
            *state = ReadingState::Playing;
        }
        s.engine.resume();
        events::emit(AppEvent::TtsResumed);
        events::announce("Reading resumed");
    }

    pub fn stop(&self) {
        let Some(s) = self.current() else { return };
        {
            let mut state = lock(&s.state);
            if *state == ReadingState::Stopped {
                return;
            }
            *state = ReadingState::Stopped;
        }
        s.stop.store(true, Ordering::SeqCst);
        s.engine.stop();
        events::emit(AppEvent::TtsStopped);
        events::announce("Reading stopped");
    }

    /// Skips to the next paragraph (the next page after the last one).
    pub fn skip_forward(&self) {
        self.skip(|s| &s.skip_forward);
    }

    /// Skips back to the previous paragraph of the page.
    pub fn skip_backward(&self) {
        self.skip(|s| &s.skip_backward);
    }

    fn skip(&self, flag: fn(&Session) -> &AtomicBool) {
        let Some(s) = self.current() else { return };
        if *lock(&s.state) == ReadingState::Stopped {
            return;
        }
        flag(&s).store(true, Ordering::SeqCst);
        s.engine.stop();
        // A paused loop must run to see the flag.
        if *lock(&s.state) == ReadingState::Paused {
            self.resume();
        }
    }
}

struct Plan {
    start_page: usize,
    end_page: usize,
    stop_at_section: bool,
    caret: usize,
    highlight: bool,
    speak_page_numbers: bool,
    announce_sections: bool,
    profile: VoiceProfile,
}

/// A paragraph and where it starts in the page text, in chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph {
    pub text: String,
    pub start: usize,
}

/// Splits a page into paragraphs at blank lines. Each keeps its own offset so a
/// repeated paragraph highlights the right occurrence.
pub fn split_paragraphs(page_text: &str) -> Vec<Paragraph> {
    static BLANK_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").unwrap());
    let mut out = Vec::new();
    let mut search_from = 0; // byte offset
    for raw in BLANK_LINE.split(page_text) {
        let stripped = raw.trim();
        if stripped.is_empty() {
            continue;
        }
        if let Some(rel) = page_text[search_from..].find(stripped) {
            let byte = search_from + rel;
            out.push(Paragraph { text: stripped.to_string(), start: page_text[..byte].chars().count() });
            search_from = byte + stripped.len();
        }
    }
    out
}

/// Index of the paragraph to start from for a caret at `caret` chars: the first
/// one that ends at or after it. A caret of 0, or past the last paragraph, starts at the top.
fn start_paragraph(paragraphs: &[Paragraph], caret: usize) -> usize {
    if caret == 0 {
        return 0;
    }
    paragraphs.iter().position(|p| caret <= p.start + p.text.chars().count()).unwrap_or(0)
}

/// Runs `f` on the UI thread and waits for its answer.
fn ui_call<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    events::run_on_ui(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(UI_TIMEOUT).ok()
}

/// Sleeps up to `total`, returning early on stop or skip.
fn pause_for(s: &Session, total: Duration) {
    let end = Instant::now() + total;
    while Instant::now() < end && !interrupted(s) {
        std::thread::sleep(Duration::from_millis(20).min(end.saturating_duration_since(Instant::now())));
    }
}

fn interrupted(s: &Session) -> bool {
    s.stop.load(Ordering::SeqCst) || s.skip_forward.load(Ordering::SeqCst) || s.skip_backward.load(Ordering::SeqCst)
}

fn speak(s: &Session, text: &str) {
    if s.stop.load(Ordering::SeqCst) {
        return;
    }
    if let Err(e) = s.engine.speak(text) {
        log::error!("TTS speak error: {e:#}");
    }
}

fn reading_loop(s: &Session, target: &Arc<dyn ReadingTarget>, plan: &Plan) {
    read_pages(s, target, plan);
    // A user stop already announced itself; only a natural end is reported here.
    let mut state = lock(&s.state);
    if *state != ReadingState::Stopped {
        *state = ReadingState::Stopped;
        drop(state);
        events::emit(AppEvent::TtsStopped);
        events::announce("Reading finished");
    }
}

fn read_pages(s: &Session, target: &Arc<dyn ReadingTarget>, plan: &Plan) {
    let para_pause = Duration::from_millis(plan.profile.paragraph_pause_ms);
    let page_pause = Duration::from_millis(plan.profile.page_pause_ms);

    for page in plan.start_page..plan.end_page {
        if s.stop.load(Ordering::SeqCst) {
            return;
        }
        let t = target.clone();
        let fetched = ui_call(move || {
            if t.current_page() != page {
                t.go_to_page(page);
            }
            (t.page_text(page), t.section_title(page))
        });
        let Some((text, section)) = fetched else {
            log::warn!("TTS: timed out fetching text for page {page}; skipping");
            continue;
        };

        let crossing = page > plan.start_page;
        if crossing && plan.stop_at_section && section.is_some() {
            return;
        }
        if text.trim().is_empty() {
            continue;
        }
        if crossing {
            events::emit(AppEvent::TtsPageCrossed { page_num: page + 1 });
            if plan.speak_page_numbers {
                speak(s, &format!("Page {}", page + 1));
            }
            if let Some(title) = section.filter(|_| plan.announce_sections) {
                speak(s, &format!("Section {title}"));
            }
        }

        let paragraphs = split_paragraphs(&text);
        let mut i = if page == plan.start_page { start_paragraph(&paragraphs, plan.caret) } else { 0 };
        while i < paragraphs.len() {
            if s.stop.load(Ordering::SeqCst) {
                return;
            }
            while *lock(&s.state) == ReadingState::Paused && !s.stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
            if s.skip_forward.swap(false, Ordering::SeqCst) {
                i += 1;
                continue;
            }
            if s.skip_backward.swap(false, Ordering::SeqCst) {
                i = i.saturating_sub(1);
                continue;
            }

            let para = &paragraphs[i];
            if plan.highlight {
                let (t, start, end) = (target.clone(), para.start, para.start + para.text.chars().count());
                events::run_on_ui(move || t.highlight(start, end));
            }
            speak(s, &para.text);

            // Skipping interrupted this paragraph: move relative to it, without the usual pause.
            if s.skip_forward.swap(false, Ordering::SeqCst) {
                i += 1;
            } else if s.skip_backward.swap(false, Ordering::SeqCst) {
                i = i.saturating_sub(1);
            } else {
                if i + 1 < paragraphs.len() {
                    pause_for(s, para_pause);
                }
                i += 1;
            }
        }
        if page + 1 < plan.end_page {
            pause_for(s, page_pause);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str, start: usize) -> Paragraph {
        Paragraph { text: text.into(), start }
    }

    #[test]
    fn splits_on_blank_lines_and_tracks_char_offsets() {
        let text = "  First line\nsecond line \n\n\nThird\n \t\nFirst line\nsecond line\n\n";
        assert_eq!(split_paragraphs(text), vec![p("First line\nsecond line", 2), p("Third", 28), p("First line\nsecond line", 37)]);
    }

    #[test]
    fn repeated_paragraphs_get_their_own_offsets() {
        assert_eq!(split_paragraphs("same\n\nsame\n\nsame"), vec![p("same", 0), p("same", 6), p("same", 12)]);
    }

    #[test]
    fn offsets_count_chars_not_bytes() {
        assert_eq!(split_paragraphs("é€\n\nfin"), vec![p("é€", 0), p("fin", 4)]);
    }

    #[test]
    fn caret_picks_the_paragraph_containing_or_after_it() {
        let paras = vec![p("aaaa", 0), p("bbbb", 6), p("cccc", 12)];
        assert_eq!(start_paragraph(&paras, 0), 0);
        assert_eq!(start_paragraph(&paras, 4), 0); // end of first
        assert_eq!(start_paragraph(&paras, 5), 1); // in the gap: the next paragraph
        assert_eq!(start_paragraph(&paras, 14), 2);
        assert_eq!(start_paragraph(&paras, 99), 0); // past the end restarts, as Prism does
    }
}
