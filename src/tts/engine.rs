//! Base TTS engine abstraction (port of prism/tts/engine.py).

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceInfo {
    pub id: String,
    pub name: String,
    pub language: String,
}

/// A speech engine. Every method takes `&self`: `speak` blocks on the reading
/// thread while `stop`, `pause` and `resume` arrive from the UI thread, so
/// implementations keep their mutable state behind locks.
pub trait TtsEngine: Send + Sync {
    fn voices(&self) -> Vec<VoiceInfo>;
    /// Selects a voice by engine-specific id (a name is accepted as a fallback). Unknown ids are logged and ignored.
    fn set_voice(&self, voice_id: &str);
    /// 0-100 normalized.
    fn set_rate(&self, rate: u32);
    /// 0-100.
    fn set_volume(&self, volume: u32);
    /// Speaks synchronously: returns when the text finished or `stop` was called.
    fn speak(&self, text: &str) -> anyhow::Result<()>;
    /// Interrupts current speech immediately, including paused speech.
    fn stop(&self);
    fn pause(&self);
    fn resume(&self);
    fn is_speaking(&self) -> bool;
    /// Human-friendly engine name.
    fn name(&self) -> &str;
    /// Whether this engine can be used on this system.
    fn is_available() -> bool
    where
        Self: Sized;
}

/// Makes COM usable on the calling thread (multithreaded unless the thread already chose a mode).
pub(crate) fn ensure_com() {
    thread_local! {
        static DONE: () = unsafe {
            // RPC_E_CHANGED_MODE just means the thread is already an STA, which is fine.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        };
    }
    DONE.with(|_| {});
}

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
