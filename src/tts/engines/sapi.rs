//! Windows SAPI 5 engine over `ISpVoice` (port of prism/tts/engines/sapi.py).

use std::sync::{Mutex, MutexGuard};

use windows::Win32::Foundation::WAIT_TIMEOUT;
use windows::Win32::Media::Speech::{
    ISpObjectToken, ISpObjectTokenCategory, ISpVoice, SPCAT_VOICES, SPF_ASYNC, SPF_IS_NOT_XML, SPF_PURGEBEFORESPEAK, SPRS_IS_SPEAKING, SPVOICESTATUS,
    SpObjectTokenCategory, SpVoice,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::{PCWSTR, PWSTR, w};

use crate::tts::engine::{TtsEngine, VoiceInfo, ensure_com, wide};

pub const NAME: &str = "SAPI 5";

pub struct SapiEngine {
    voice: ISpVoice,
    epoch: Mutex<u64>,
}

// SpVoice is registered ThreadingModel=Both and thread-safe by design: Prism likewise
// speaks on the reading thread while pause/stop arrive from the UI thread.
unsafe impl Send for SapiEngine {}
unsafe impl Sync for SapiEngine {}

/// Maps 0-100 to SAPI's -10..10, truncating toward zero like Python's `int()`.
pub fn map_rate(rate: u32) -> i32 {
    ((rate.min(100) as i32) * 20 - 1000) / 100
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn take_string(s: PWSTR) -> String {
    let out = unsafe { s.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(s.0 as _)) };
    out
}

fn description(token: &ISpObjectToken) -> String {
    unsafe { token.GetStringValue(PCWSTR::null()) }.map(take_string).unwrap_or_default()
}

impl SapiEngine {
    pub fn new() -> anyhow::Result<Self> {
        ensure_com();
        Ok(Self { voice: unsafe { CoCreateInstance(&SpVoice, None, CLSCTX_ALL)? }, epoch: Mutex::new(0) })
    }

    fn tokens(&self) -> anyhow::Result<Vec<ISpObjectToken>> {
        unsafe {
            let category: ISpObjectTokenCategory = CoCreateInstance(&SpObjectTokenCategory, None, CLSCTX_ALL)?;
            category.SetId(SPCAT_VOICES, false)?;
            let tokens = category.EnumTokens(PCWSTR::null(), PCWSTR::null())?;
            let mut count = 0;
            tokens.GetCount(&mut count)?;
            Ok((0..count).filter_map(|i| tokens.Item(i).ok()).collect())
        }
    }
}

impl TtsEngine for SapiEngine {
    fn voices(&self) -> Vec<VoiceInfo> {
        let tokens = self.tokens().unwrap_or_else(|e| {
            log::warn!("SAPI voice enumeration failed: {e}");
            Vec::new()
        });
        tokens
            .iter()
            .filter_map(|t| {
                let id = unsafe { t.GetId() }.map(take_string).ok()?;
                let language = unsafe { t.OpenKey(w!("Attributes")).and_then(|k| k.GetStringValue(w!("Language"))) }.map(take_string).unwrap_or_default();
                Some(VoiceInfo { id, name: description(t), language })
            })
            .collect()
    }

    fn set_voice(&self, voice_id: &str) {
        let tokens = self.tokens().unwrap_or_default();
        let by_id = tokens.iter().find(|t| unsafe { t.GetId() }.map(take_string).is_ok_and(|id| id == voice_id));
        let found = by_id.or_else(|| tokens.iter().find(|t| description(t) == voice_id));
        match found {
            Some(token) => {
                if let Err(e) = unsafe { self.voice.SetVoice(token) } {
                    log::warn!("SAPI SetVoice failed: {e}");
                }
            }
            None => log::warn!("SAPI voice '{voice_id}' not found"),
        }
    }

    fn set_rate(&self, rate: u32) {
        let _ = unsafe { self.voice.SetRate(map_rate(rate)) };
    }

    fn set_volume(&self, volume: u32) {
        let _ = unsafe { self.voice.SetVolume(volume.min(100) as u16) };
    }

    fn speak(&self, text: &str) -> anyhow::Result<()> {
        ensure_com();
        let text = wide(text);
        // Async speak plus waiting on the completion event: a blocking Speak holds the voice's
        // lock, which made Pause and Purge from the UI thread ineffective until the text ended.
        // IS_NOT_XML: document text containing '<' must not be parsed as SAPI markup.
        let epoch = {
            let epoch = lock(&self.epoch);
            unsafe { self.voice.Speak(PCWSTR(text.as_ptr()), (SPF_IS_NOT_XML.0 | SPF_ASYNC.0) as u32, None)? };
            *epoch
        };
        unsafe {
            let done = self.voice.SpeakCompleteEvent();
            while WaitForSingleObject(done, 25) == WAIT_TIMEOUT && *lock(&self.epoch) == epoch {}
        }
        Ok(())
    }

    fn stop(&self) {
        // The epoch bump and the purge happen under one lock, so a speak that starts after
        // stop() cannot be purged by it, and one that started before always sees the bump.
        let mut epoch = lock(&self.epoch);
        *epoch += 1;
        // Purge cancels what is queued or playing; Resume clears a paused state so the next speak is audible.
        let _ = unsafe { self.voice.Speak(w!(""), (SPF_PURGEBEFORESPEAK.0 | SPF_ASYNC.0) as u32, None) };
        let _ = unsafe { self.voice.Resume() };
    }

    fn pause(&self) {
        let _ = unsafe { self.voice.Pause() };
    }

    fn resume(&self) {
        let _ = unsafe { self.voice.Resume() };
    }

    fn is_speaking(&self) -> bool {
        let mut status = SPVOICESTATUS::default();
        unsafe { self.voice.GetStatus(&mut status, std::ptr::null_mut()) }.is_ok() && status.dwRunningState == SPRS_IS_SPEAKING.0 as u32
    }

    fn name(&self) -> &str {
        NAME
    }

    fn is_available() -> bool {
        ensure_com();
        unsafe { CoCreateInstance::<_, ISpVoice>(&SpVoice, None, CLSCTX_ALL) }.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::map_rate;

    #[test]
    fn rate_maps_0_100_to_sapi_range_truncating_toward_zero() {
        assert_eq!(map_rate(0), -10);
        assert_eq!(map_rate(35), -3);
        assert_eq!(map_rate(47), 0); // 9.4 - 10 = -0.6 truncates to 0, as Python int() does
        assert_eq!(map_rate(50), 0);
        assert_eq!(map_rate(80), 6);
        assert_eq!(map_rate(100), 10);
        assert_eq!(map_rate(250), 10);
    }
}
