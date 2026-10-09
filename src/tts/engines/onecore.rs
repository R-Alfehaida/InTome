//! OneCore voices through Windows.Media.SpeechSynthesis, played by a MediaPlayer
//! (replaces Prism's pyttsx3 path, which could not pause).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use windows::Foundation::{IClosable, TypedEventHandler};
use windows::Media::Core::MediaSource;
use windows::Media::Playback::MediaPlayer;
use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceInformation};
use windows::core::{HSTRING, Interface};

use crate::tts::engine::{TtsEngine, VoiceInfo, ensure_com};

pub const NAME: &str = "OneCore";

pub struct OneCoreEngine {
    synth: Mutex<SpeechSynthesizer>,
    player: Mutex<Option<MediaPlayer>>,
    /// Playback volume 0.0-1.0.
    volume: Mutex<f64>,
    stop: AtomicBool,
    speaking: AtomicBool,
}

/// Maps 0-100 to a synthesizer speaking rate. Prism mapped 0-100 to 100-300 wpm
/// with 200 wpm as normal; as a multiplier that is 0.5 (0), 1.0 (50), 1.5 (100).
pub fn map_rate(rate: u32) -> f64 {
    0.5 + rate.min(100) as f64 / 100.0
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn all_voices() -> Vec<VoiceInformation> {
    SpeechSynthesizer::AllVoices().map(|v| v.into_iter().collect()).unwrap_or_default()
}

impl OneCoreEngine {
    pub fn new() -> anyhow::Result<Self> {
        ensure_com();
        Ok(Self {
            synth: Mutex::new(SpeechSynthesizer::new()?),
            player: Mutex::new(None),
            volume: Mutex::new(1.0),
            stop: AtomicBool::new(false),
            speaking: AtomicBool::new(false),
        })
    }

    fn play(&self, text: &str) -> anyhow::Result<()> {
        let stream = {
            let synth = lock(&self.synth);
            synth.SynthesizeTextToStreamAsync(&HSTRING::from(text))?.join()?
        };
        if self.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let player = MediaPlayer::new()?;
        player.SetVolume(*lock(&self.volume))?;
        let (tx, rx) = mpsc::channel::<Option<String>>();
        let ended = tx.clone();
        player.MediaEnded(&TypedEventHandler::new(move |_, _| {
            let _ = ended.send(None);
            Ok(())
        }))?;
        player.MediaFailed(&TypedEventHandler::new(move |_, args: windows::core::Ref<'_, windows::Media::Playback::MediaPlayerFailedEventArgs>| {
            let msg = args.as_ref().and_then(|a| a.ErrorMessage().ok()).map(|m| m.to_string()).unwrap_or_default();
            let _ = tx.send(Some(msg));
            Ok(())
        }))?;
        player.SetSource(&MediaSource::CreateFromStream(&stream, &stream.ContentType()?)?)?;
        *lock(&self.player) = Some(player.clone());
        // stop() may have run between the check above and publishing the player.
        if !self.stop.load(Ordering::SeqCst) {
            player.Play()?;
        }
        let outcome = loop {
            if self.stop.load(Ordering::SeqCst) {
                break Ok(());
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(None) => break Ok(()),
                Ok(Some(msg)) => break Err(anyhow::anyhow!("OneCore playback failed: {msg}")),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break Ok(()),
            }
        };
        *lock(&self.player) = None;
        let _ = player.Pause();
        let _ = player.cast::<IClosable>().and_then(|c| c.Close());
        outcome
    }
}

impl TtsEngine for OneCoreEngine {
    fn voices(&self) -> Vec<VoiceInfo> {
        all_voices()
            .into_iter()
            .filter_map(|v| Some(VoiceInfo { id: v.Id().ok()?.to_string(), name: v.DisplayName().ok()?.to_string(), language: v.Language().ok()?.to_string() }))
            .collect()
    }

    fn set_voice(&self, voice_id: &str) {
        let matches = |v: &VoiceInformation| {
            let id = v.Id().map(|s| s.to_string()).unwrap_or_default();
            // Prism's profiles hold full registry paths; WinRT reports only the last segment.
            id == voice_id || voice_id.rsplit('\\').next() == Some(id.as_str()) || v.DisplayName().is_ok_and(|n| n.to_string() == voice_id)
        };
        match all_voices().into_iter().find(matches) {
            Some(v) => {
                if let Err(e) = lock(&self.synth).SetVoice(&v) {
                    log::warn!("OneCore SetVoice failed: {e}");
                }
            }
            None => log::warn!("OneCore voice '{voice_id}' not found"),
        }
    }

    fn set_rate(&self, rate: u32) {
        let _ = lock(&self.synth).Options().and_then(|o| o.SetSpeakingRate(map_rate(rate)));
    }

    fn set_volume(&self, volume: u32) {
        let v = volume.min(100) as f64 / 100.0;
        *lock(&self.volume) = v;
        if let Some(p) = lock(&self.player).as_ref() {
            let _ = p.SetVolume(v);
        }
    }

    fn speak(&self, text: &str) -> anyhow::Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        ensure_com();
        self.stop.store(false, Ordering::SeqCst);
        self.speaking.store(true, Ordering::SeqCst);
        let result = self.play(text);
        self.speaking.store(false, Ordering::SeqCst);
        result
    }

    fn stop(&self) {
        // Flag and pause under the player lock: the speak that sees the flag cannot finish, and
        // so the next one cannot start, before this pause has hit the player it was meant for.
        let player = lock(&self.player);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(p) = player.as_ref() {
            let _ = p.Pause();
        }
    }

    fn pause(&self) {
        if let Some(p) = lock(&self.player).as_ref() {
            let _ = p.Pause();
        }
    }

    fn resume(&self) {
        if let Some(p) = lock(&self.player).as_ref() {
            let _ = p.Play();
        }
    }

    fn is_speaking(&self) -> bool {
        self.speaking.load(Ordering::SeqCst)
    }

    fn name(&self) -> &str {
        NAME
    }

    fn is_available() -> bool {
        ensure_com();
        !all_voices().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::map_rate;

    #[test]
    fn rate_maps_prisms_wpm_scale_to_a_speaking_rate_multiplier() {
        assert_eq!(map_rate(0), 0.5);
        assert_eq!(map_rate(50), 1.0);
        assert_eq!(map_rate(100), 1.5);
    }
}
