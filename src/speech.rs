//! Screen-reader output through prism, via the `prismer` crate (prism is linked into the exe).
//!
//! Choice of backend: a running screen reader wins (NVDA, then ZDSR/PCTalker, then JAWS and
//! the other readers, then UI Automation for Narrator), each in prism's own priority order. When
//! none is running, prism's best voice backend (OneCore, then SAPI) speaks instead.
//!
//! The choice is made again when a call fails (the reader exited) and when prism's background
//! availability poll reports that a screen reader appeared (it started after InTome). The poll
//! runs off the UI thread, so no call pays for it. State is per thread; call from the UI thread,
//! as the event subscriber does. No call panics.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use prismer::{Backend, Error, Features, Prism};

use crate::events::{self, AppEvent};

/// Voice backends: used only when no screen reader is running.
const VOICES: [&str; 2] = ["OneCore", "SAPI"];
/// How long to wait before looking again after finding nothing, or a reader that would not start.
const RETRY_AFTER: Duration = Duration::from_secs(5);
/// Quiet-time ceiling for prism's availability poll, which re-reads every backend's state.
const POLL_BACKOFF_MS: u32 = 5000;

/// Set by prism's poll thread when a screen reader becomes available.
static READER_APPEARED: AtomicBool = AtomicBool::new(false);

/// Subscribes to [`AppEvent::Announce`] and chooses the backend now, so the log names it at startup.
/// Call on the UI thread.
pub fn init() -> events::SubscriptionId {
    with_speech(|s| s.refresh());
    events::subscribe(|e| {
        if let AppEvent::Announce { message, interrupt } = e {
            output(message, *interrupt);
        }
    })
}

/// Name of the backend in use, e.g. `"NVDA"`, or `None` when nothing can speak.
pub fn backend_name() -> Option<String> {
    with_speech(|s| s.backend.as_ref().map(|b| b.name()))
}

/// Speaks `text`. With `interrupt` false it queues behind current speech. Returns whether a backend took it.
pub fn speak(text: &str, interrupt: bool) -> bool {
    !text.is_empty() && with_speech(|s| s.call(|b| b.speak(text, interrupt)))
}

/// Sends `text` to a braille display, if the backend has one.
pub fn braille(text: &str) -> bool {
    !text.is_empty() && with_speech(|s| s.call(|b| if b.supports(Features::BRAILLE) { b.braille(text) } else { Err(Error::NotImplemented) }))
}

/// Speaks and brailles `text`. Returns whether it was spoken.
pub fn output(text: &str, interrupt: bool) -> bool {
    let spoken = speak(text, interrupt);
    braille(text);
    spoken
}

struct Speech {
    prism: Option<&'static Prism>,
    backend: Option<Backend<'static>>,
    /// Earliest time to choose again. `None` means do so only when asked.
    retry_at: Option<Instant>,
}

thread_local! {
    static SPEECH: RefCell<Speech> = const { RefCell::new(Speech { prism: None, backend: None, retry_at: None }) };
}

fn with_speech<R>(f: impl FnOnce(&mut Speech) -> R) -> R {
    SPEECH.with(|s| f(&mut s.borrow_mut()))
}

impl Speech {
    /// Runs `op` on the backend, choosing again once if the call fails for a reason other than the request itself.
    fn call(&mut self, op: impl Fn(&Backend<'static>) -> prismer::Result<()>) -> bool {
        for _ in 0..2 {
            self.refresh();
            let Some(backend) = &self.backend else { return false };
            match op(backend) {
                Ok(()) => return true,
                Err(Error::NotImplemented | Error::InvalidParam | Error::InvalidUtf8) => return false,
                Err(e) => {
                    log::warn!("speech: {} failed ({e}); choosing again", backend.name());
                    self.backend = None;
                    self.retry_at = None;
                }
            }
        }
        false
    }

    /// Chooses a backend if there is none, a screen reader has appeared, or a retry is due.
    fn refresh(&mut self) {
        let appeared = READER_APPEARED.swap(false, Ordering::Relaxed);
        let wait = match self.retry_at {
            Some(t) => Instant::now() < t,
            None => self.backend.is_some(),
        };
        if wait && !appeared {
            return;
        }
        let Some(prism) = self.prism() else { return };
        let on_voice = self.backend.as_ref().is_some_and(|b| VOICES.contains(&b.name().as_str()));
        let (reader, unstartable) = screen_reader(prism);
        // Already on a voice and no reader to move to: keep it, starting a voice is slow.
        let picked = reader.or_else(|| if on_voice { None } else { prism.create_best().ok() });
        if let Some(b) = picked {
            log::info!("speech: using {}", b.name());
            self.backend = Some(b);
        } else if self.backend.is_none() {
            log::warn!("speech: no backend available");
        }
        let on_reader = self.backend.as_ref().is_some_and(|b| !VOICES.contains(&b.name().as_str()));
        self.retry_at = (self.backend.is_none() || (unstartable && !on_reader)).then(|| Instant::now() + RETRY_AFTER);
    }

    fn prism(&mut self) -> Option<&'static Prism> {
        if self.prism.is_none() {
            match Prism::builder().backoff_max_ms(POLL_BACKOFF_MS).on_availability(note_availability).build() {
                // One context for the life of the process, so its backends can be 'static.
                Ok(p) => self.prism = Some(Box::leak(Box::new(p))),
                Err(e) => {
                    log::warn!("speech: prism failed to start: {e}");
                    self.retry_at = Some(Instant::now() + RETRY_AFTER);
                }
            }
        }
        self.prism
    }
}

fn note_availability(_: prismer::BackendId, name: &str, available: bool) {
    if available && !VOICES.contains(&name) {
        READER_APPEARED.store(true, Ordering::Relaxed);
    }
}

/// The highest-priority running screen reader that starts, if any. The flag says a reader
/// reported itself running but would not start (UI Automation does so until a window is visible).
fn screen_reader(prism: &'static Prism) -> (Option<Backend<'static>>, bool) {
    let mut unstartable = false;
    let found = prism.backend_ids().into_iter().find_map(|id| {
        if VOICES.contains(&prism.backend_name(id)?.as_str()) {
            return None;
        }
        let b = prism.create(id).ok()?;
        if !b.supports(Features::IS_SUPPORTED_AT_RUNTIME) {
            return None;
        }
        if b.initialize().is_ok() {
            return Some(b);
        }
        unstartable = true;
        None
    });
    (found, unstartable)
}
