//! Voice profile management with persistence to `voice_profiles.json` (port of prism/tts/profiles.py).
//!
//! Built-in profiles are always available. Saving writes every profile, so a
//! customised built-in is stored too and overrides the default on the next load.

use std::path::PathBuf;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::{config, constants};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceProfile {
    pub name: String,
    pub engine: String,
    pub voice: String,
    pub rate: u32,
    pub volume: u32,
    pub paragraph_pause_ms: u64,
    pub page_pause_ms: u64,
    #[serde(skip)]
    pub builtin: bool,
}

fn builtin(name: &str, engine: &str, voice: &str, rate: u32, paragraph_pause_ms: u64, page_pause_ms: u64) -> VoiceProfile {
    VoiceProfile { name: name.into(), engine: engine.into(), voice: voice.into(), rate, volume: 100, paragraph_pause_ms, page_pause_ms, builtin: true }
}

pub static BUILTIN_PROFILES: LazyLock<[VoiceProfile; 3]> = LazyLock::new(|| {
    [
        builtin("Human-like", "gemini", "Kore", 50, 400, 800),
        builtin("Deep Reading", "sapi", "", 35, 500, 1000),
        builtin("Express", "sapi", "", 80, 100, 300),
    ]
});

/// One stored profile. Missing fields fall back to the built-in of the same name, else to the custom-profile defaults.
#[derive(Deserialize)]
struct Stored {
    name: String,
    engine: Option<String>,
    voice: Option<String>,
    rate: Option<u32>,
    volume: Option<u32>,
    paragraph_pause_ms: Option<u64>,
    page_pause_ms: Option<u64>,
}

impl Stored {
    fn resolve(self) -> VoiceProfile {
        let base = BUILTIN_PROFILES.iter().find(|b| b.name == self.name).cloned().unwrap_or_else(|| VoiceProfile {
            name: self.name.clone(),
            engine: "sapi".into(),
            voice: String::new(),
            rate: 50,
            volume: 100,
            paragraph_pause_ms: 300,
            page_pause_ms: 500,
            builtin: false,
        });
        VoiceProfile {
            engine: self.engine.unwrap_or(base.engine),
            voice: self.voice.unwrap_or(base.voice),
            rate: self.rate.unwrap_or(base.rate),
            volume: self.volume.unwrap_or(base.volume),
            paragraph_pause_ms: self.paragraph_pause_ms.unwrap_or(base.paragraph_pause_ms),
            page_pause_ms: self.page_pause_ms.unwrap_or(base.page_pause_ms),
            ..base
        }
    }
}

pub struct ProfileManager {
    profiles: Vec<VoiceProfile>,
    path: PathBuf,
}

impl Default for ProfileManager {
    fn default() -> Self {
        Self::with_path(constants::voice_profiles_file())
    }
}

impl ProfileManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_path(path: PathBuf) -> Self {
        let mut m = Self { profiles: BUILTIN_PROFILES.to_vec(), path };
        m.load();
        m
    }

    fn load(&mut self) {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return };
        let entries = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => v.get("profiles").and_then(|p| p.as_array().cloned()).unwrap_or_default(),
            Err(e) => {
                log::warn!("voice profiles {} unreadable ({e}); using built-ins", self.path.display());
                return;
            }
        };
        // A malformed entry is skipped, like Prism skipping a profile whose numbers do not parse.
        for stored in entries.into_iter().filter_map(|v| serde_json::from_value::<Stored>(v).ok()) {
            self.upsert(stored.resolve());
        }
    }

    fn upsert(&mut self, mut profile: VoiceProfile) {
        profile.builtin = BUILTIN_PROFILES.iter().any(|b| b.name == profile.name);
        match self.profiles.iter_mut().find(|p| p.name == profile.name) {
            Some(slot) => *slot = profile,
            None => self.profiles.push(profile),
        }
    }

    /// All profiles, built-in first.
    pub fn get_profiles(&self) -> Vec<VoiceProfile> {
        let (builtin, custom): (Vec<_>, Vec<_>) = self.profiles.iter().cloned().partition(|p| p.builtin);
        builtin.into_iter().chain(custom).collect()
    }

    pub fn get_profile(&self, name: &str) -> Option<VoiceProfile> {
        self.profiles.iter().find(|p| p.name == name).cloned()
    }

    /// The profile named by `tts.active_profile`, else "Deep Reading".
    pub fn get_active_profile(&self) -> VoiceProfile {
        self.get_profile(&config::get().tts.active_profile).unwrap_or_else(|| self.profiles.iter().find(|p| p.name == BUILTIN_PROFILES[1].name).cloned().unwrap_or_else(|| BUILTIN_PROFILES[1].clone()))
    }

    pub fn set_active_profile(&self, name: &str) {
        if self.profiles.iter().any(|p| p.name == name) {
            config::update("tts.active_profile", |s| s.tts.active_profile = name.to_string());
        }
    }

    /// Adds a profile, or replaces the one with the same name.
    pub fn add_profile(&mut self, profile: VoiceProfile) {
        self.upsert(profile);
        self.save();
    }

    pub fn update_profile(&mut self, profile: VoiceProfile) {
        self.add_profile(profile);
    }

    /// Removes a custom profile. Returns false for built-ins and unknown names.
    pub fn remove_profile(&mut self, name: &str) -> bool {
        let Some(i) = self.profiles.iter().position(|p| p.name == name && !p.builtin) else { return false };
        self.profiles.remove(i);
        self.save();
        true
    }

    fn save(&self) {
        let json = serde_json::json!({ "profiles": self.profiles });
        let written = self
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&self.path, serde_json::to_string_pretty(&json).unwrap_or_default()));
        if let Err(e) = written {
            log::error!("failed to save voice profiles to {}: {e}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(name: &str) -> VoiceProfile {
        VoiceProfile { name: name.into(), engine: "onecore".into(), voice: "MSTTS_V110_enUS_MarkM".into(), rate: 61, volume: 90, paragraph_pause_ms: 250, page_pause_ms: 700, builtin: false }
    }

    #[test]
    fn profiles_round_trip_through_json_with_customised_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice_profiles.json");
        let mut m = ProfileManager::with_path(path.clone());
        assert_eq!(m.get_profiles().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Human-like", "Deep Reading", "Express"]);

        m.add_profile(custom("Bedtime"));
        let mut express = m.get_profile("Express").unwrap();
        express.rate = 95;
        m.update_profile(express);

        let reloaded = ProfileManager::with_path(path);
        let names: Vec<_> = reloaded.get_profiles().iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["Human-like", "Deep Reading", "Express", "Bedtime"]);
        let bedtime = reloaded.get_profile("Bedtime").unwrap();
        assert_eq!(bedtime, custom("Bedtime"));
        let express = reloaded.get_profile("Express").unwrap();
        assert_eq!((express.rate, express.paragraph_pause_ms, express.builtin), (95, 100, true));
    }

    #[test]
    fn partial_entries_use_defaults_and_bad_entries_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice_profiles.json");
        std::fs::write(&path, r#"{"profiles":[{"name":"Mine","rate":20},{"name":"Broken","rate":"fast"},{"name":"Deep Reading","volume":40}]}"#).unwrap();
        let m = ProfileManager::with_path(path);
        let mine = m.get_profile("Mine").unwrap();
        assert_eq!((mine.engine.as_str(), mine.rate, mine.volume, mine.paragraph_pause_ms, mine.page_pause_ms), ("sapi", 20, 100, 300, 500));
        assert!(m.get_profile("Broken").is_none());
        let deep = m.get_profile("Deep Reading").unwrap();
        assert_eq!((deep.rate, deep.volume, deep.page_pause_ms), (35, 40, 1000));
    }

    #[test]
    fn builtins_cannot_be_removed_but_custom_profiles_can() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = ProfileManager::with_path(dir.path().join("p.json"));
        m.add_profile(custom("Temp"));
        assert!(!m.remove_profile("Express"));
        assert!(m.remove_profile("Temp"));
        assert!(!m.remove_profile("Temp"));
    }
}
