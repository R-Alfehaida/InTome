//! Gemini neural TTS (port of prism/tts/engines/gemini_tts.py).
//!
//! Each `speak` generates the audio, wraps the PCM in a WAV header and plays it
//! with `PlaySound` from memory. PlaySound cannot pause, so pause stops the
//! sound and resume replays from the byte offset already played.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_PURGE};
use windows::core::PCWSTR;

use crate::ai::client::{DEFAULT_TTS_MODEL, GeminiClient};
use crate::cancel::CancelToken;
use crate::tts::engine::{TtsEngine, VoiceInfo};

pub const NAME: &str = "Gemini TTS";
pub const GEMINI_VOICES: [&str; 5] = ["Kore", "Puck", "Charon", "Fenrir", "Aoede"];

const SAMPLE_RATE: u32 = 24_000;
const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * 2;

#[derive(Default)]
struct Ctl {
    stop: bool,
    paused: bool,
    speaking: bool,
}

pub struct GeminiTtsEngine {
    client: Mutex<GeminiClient>,
    voice: Mutex<String>,
    cancel: CancelToken,
    ctl: Mutex<Ctl>,
    changed: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wraps raw 16-bit mono PCM in a 44-byte WAV header.
pub fn pcm_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let (channels, bits) = (1u16, 16u16);
    let block_align = channels * bits / 8;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

fn purge_sound() {
    let _ = unsafe { PlaySoundW(PCWSTR::null(), None, SND_PURGE) };
}

impl GeminiTtsEngine {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: Mutex::new(GeminiClient::new()?),
            voice: Mutex::new(GEMINI_VOICES[0].to_string()),
            cancel: CancelToken::new(),
            ctl: Mutex::new(Ctl::default()),
            changed: Condvar::new(),
        })
    }

    /// Plays `pcm` to the end, honouring stop and pause. Blocks.
    fn play(&self, pcm: &[u8]) {
        let mut offset = 0;
        loop {
            let mut ctl = lock(&self.ctl);
            while ctl.paused && !ctl.stop {
                ctl = self.changed.wait(ctl).unwrap_or_else(|e| e.into_inner());
            }
            if ctl.stop || offset >= pcm.len() {
                return;
            }
            // The buffer must outlive the async playback; it does, because this iteration waits for it to end or purges it.
            let wav = pcm_to_wav(&pcm[offset..], SAMPLE_RATE);
            if !unsafe { PlaySoundW(PCWSTR(wav.as_ptr() as _), None, SND_MEMORY | SND_ASYNC) }.as_bool() {
                log::warn!("PlaySound could not start (no audio device?)");
                return;
            }
            let started = Instant::now();
            let remaining = Duration::from_secs_f64((pcm.len() - offset) as f64 / BYTES_PER_SECOND as f64) + Duration::from_millis(100);
            loop {
                let left = remaining.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return;
                }
                ctl = self.changed.wait_timeout(ctl, left).unwrap_or_else(|e| e.into_inner()).0;
                if ctl.stop || ctl.paused {
                    break;
                }
            }
            purge_sound();
            if ctl.stop {
                return;
            }
            // Paused mid-sound: resume from the sample boundary reached so far.
            offset += (started.elapsed().as_secs_f64() * BYTES_PER_SECOND as f64) as usize & !1;
        }
    }
}

impl TtsEngine for GeminiTtsEngine {
    fn voices(&self) -> Vec<VoiceInfo> {
        GEMINI_VOICES.iter().map(|v| VoiceInfo { id: v.to_string(), name: v.to_string(), language: "en-US".into() }).collect()
    }

    fn set_voice(&self, voice_id: &str) {
        if GEMINI_VOICES.contains(&voice_id) {
            *lock(&self.voice) = voice_id.to_string();
        } else {
            log::warn!("Gemini voice '{voice_id}' not found");
        }
    }

    // Gemini TTS has no rate control, and volume is the system's.
    fn set_rate(&self, _rate: u32) {}

    fn set_volume(&self, _volume: u32) {}

    fn speak(&self, text: &str) -> anyhow::Result<()> {
        self.cancel.reset();
        {
            let mut ctl = lock(&self.ctl);
            ctl.stop = false;
            ctl.speaking = true;
        }
        let voice = lock(&self.voice).clone();
        let audio = lock(&self.client).generate_audio(text, &voice, DEFAULT_TTS_MODEL, Some(&self.cancel));
        let result = match audio {
            Ok(pcm) => {
                self.play(&pcm);
                Ok(())
            }
            // A stop during generation surfaces as an error from the cancelled request; that is not a failure.
            Err(_) if self.cancel.is_cancelled() => Ok(()),
            Err(e) => Err(e),
        };
        lock(&self.ctl).speaking = false;
        result
    }

    fn stop(&self) {
        self.cancel.cancel();
        {
            let mut ctl = lock(&self.ctl);
            ctl.stop = true;
            ctl.paused = false;
        }
        self.changed.notify_all();
        purge_sound();
    }

    fn pause(&self) {
        lock(&self.ctl).paused = true;
        self.changed.notify_all();
    }

    fn resume(&self) {
        lock(&self.ctl).paused = false;
        self.changed.notify_all();
    }

    fn is_speaking(&self) -> bool {
        lock(&self.ctl).speaking
    }

    fn name(&self) -> &str {
        NAME
    }

    fn is_available() -> bool {
        GeminiClient::is_available()
    }
}

#[cfg(test)]
mod tests {
    use super::pcm_to_wav;

    #[test]
    fn wav_header_describes_24khz_16bit_mono() {
        let wav = pcm_to_wav(&[1, 2, 3, 4], 24_000);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 40); // 36 + data
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1); // PCM
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1); // mono
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 48_000); // byte rate
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 2); // block align
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert_eq!(&wav[44..], &[1, 2, 3, 4]);
    }
}
