//! Application event bus, the Rust replacement for Prism's blinker signals.
//!
//! Components emit [`AppEvent`]s and subscribe to them instead of calling each
//! other for state. Subscribers live on the UI thread. `emit` may be called from
//! any thread: off the UI thread the event is marshalled through the dispatcher
//! the GUI installs with [`set_ui_dispatcher`] (the `wx.CallAfter` equivalent).
//! Without a dispatcher (tests, headless use) events dispatch synchronously to
//! the calling thread's subscribers.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::thread::ThreadId;

#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    DocumentOpened { filename: String, page_count: usize },
    DocumentClosed { filename: String },
    /// `page_num` is 1-based.
    PageChanged { page_num: usize, page_count: usize },
    TabSwitched { tab_index: usize, filename: String },
    ContinuousReadingStarted,
    ContinuousReadingStopped,
    /// Speak (and braille) a message through the active screen reader.
    /// `interrupt: false` queues behind current speech.
    Announce { message: String, interrupt: bool },
    CaretMoved { line: usize, col: usize },
    /// `key` is `"section.key"`, e.g. `"display.font_size"`.
    ConfigChanged { key: String },
    DocumentLoaded { path: String, filename: String },
    OcrStarted { filename: String, page_count: usize },
    OcrPageCompleted { filename: String, page_num: usize, total_pages: usize },
    OcrCompleted { filename: String, total_pages: usize, elapsed_seconds: f64 },
    OcrError { filename: String, page_num: usize, error_message: String },
    TtsStarted,
    TtsPaused,
    TtsResumed,
    TtsStopped,
    TtsPageCrossed { page_num: usize },
    ExportStarted { filename: String, format: String },
    ExportProgress { filename: String, current: usize, total: usize, message: String },
    ExportCompleted { filename: String, output_path: String, elapsed_seconds: f64 },
    ExportError { filename: String, error_message: String },
    BookmarkAdded { document_hash: String, page_index: usize, name: String },
    BookmarkRemoved { document_hash: String, bookmark_id: i64 },
    NoteAdded { document_hash: String, page_index: usize, text: String },
    AnnotationExported { document_hash: String, format: String, count: usize },
    BatchStarted { file_count: usize, format: String },
    BatchFileCompleted { filename: String, status: String },
    BatchCompleted { total: usize, succeeded: usize, failed: usize, elapsed: f64 },
    FormSaved { document_hash: String, field_count: usize, output_path: String },
}

pub type UiTask = Box<dyn FnOnce() + Send>;
type Dispatcher = Box<dyn Fn(UiTask) + Send + Sync>;

struct Ui {
    thread: ThreadId,
    dispatch: Dispatcher,
}

static UI: OnceLock<Ui> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionId(u64);

thread_local! {
    static SUBSCRIBERS: RefCell<Vec<(SubscriptionId, Rc<dyn Fn(&AppEvent)>)>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: RefCell<u64> = const { RefCell::new(0) };
}

/// Installs the UI-thread marshaller. Call once, on the UI thread.
pub fn set_ui_dispatcher(dispatch: impl Fn(UiTask) + Send + Sync + 'static) {
    let _ = UI.set(Ui { thread: std::thread::current().id(), dispatch: Box::new(dispatch) });
}

pub fn on_ui_thread() -> bool {
    UI.get().is_none_or(|ui| ui.thread == std::thread::current().id())
}

/// Runs `f` on the UI thread: immediately if already there, else queued.
pub fn run_on_ui(f: impl FnOnce() + Send + 'static) {
    match UI.get() {
        Some(ui) if ui.thread != std::thread::current().id() => (ui.dispatch)(Box::new(f)),
        _ => f(),
    }
}

/// Registers a subscriber on the current (UI) thread.
pub fn subscribe(f: impl Fn(&AppEvent) + 'static) -> SubscriptionId {
    let id = NEXT_ID.with(|n| {
        let mut n = n.borrow_mut();
        *n += 1;
        SubscriptionId(*n)
    });
    SUBSCRIBERS.with(|s| s.borrow_mut().push((id, Rc::new(f))));
    id
}

pub fn unsubscribe(id: SubscriptionId) {
    SUBSCRIBERS.with(|s| s.borrow_mut().retain(|(i, _)| *i != id));
}

/// Emits an event from any thread.
pub fn emit(event: AppEvent) {
    run_on_ui(move || dispatch(&event));
}

fn dispatch(event: &AppEvent) {
    // Snapshot so subscribers may subscribe, unsubscribe or emit re-entrantly.
    let subs: Vec<_> = SUBSCRIBERS.with(|s| s.borrow().iter().map(|(_, f)| f.clone()).collect());
    for f in subs {
        f(event);
    }
}

/// Speaks `message` through the screen reader, interrupting current speech.
pub fn announce(message: impl Into<String>) {
    emit(AppEvent::Announce { message: message.into(), interrupt: true });
}

/// Speaks `message` after current speech finishes (e.g. end-of-document cue during say-all).
pub fn announce_queued(message: impl Into<String>) {
    emit(AppEvent::Announce { message: message.into(), interrupt: false });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn subscribers_receive_events_and_can_unsubscribe() {
        let seen = Rc::new(Cell::new(0));
        let s2 = seen.clone();
        let id = subscribe(move |e| {
            if let AppEvent::PageChanged { page_num, .. } = e {
                s2.set(*page_num);
            }
        });
        emit(AppEvent::PageChanged { page_num: 3, page_count: 9 });
        assert_eq!(seen.get(), 3);
        unsubscribe(id);
        emit(AppEvent::PageChanged { page_num: 5, page_count: 9 });
        assert_eq!(seen.get(), 3);
    }
}
