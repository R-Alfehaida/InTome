//! Persistent user settings, stored as TOML at `%APPDATA%\InTome\intome.toml`.
//!
//! Read with [`get`] (a cheap snapshot clone). Write with [`update`], which saves
//! the file and emits [`AppEvent::ConfigChanged`] with the `"section.key"` you name.
//! Missing or malformed keys fall back to defaults when the file is parsed.

use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::constants;
use crate::events::{self, AppEvent};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub general: General,
    pub display: Display,
    pub document: DocumentSettings,
    pub gemini: Gemini,
    pub ocr: Ocr,
    pub navigation: Navigation,
    pub annotations: Annotations,
    pub export: Export,
    pub window: WindowState,
    pub tts: Tts,
    pub recent_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub continuous_reading: bool,
    pub page_size: usize,
    pub recent_files_limit: usize,
    /// Reopen each file at the page and caret it was closed at.
    pub remember_position: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// The configured face. Use [`Display::effective_font_face`] for rendering.
    pub font_face: String,
    pub font_size: u32,
    pub use_opendyslexic: bool,
    pub line_spacing: f32,
}

impl Display {
    pub fn effective_font_face(&self) -> &str {
        if self.use_opendyslexic { "OpenDyslexic" } else { &self.font_face }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PdfReadingMode {
    #[default]
    Automatic,
    PhysicalLayout,
    ReadingOrder,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DocumentSettings {
    pub pdf_reading_mode: PdfReadingMode,
    pub cache_enabled: bool,
    pub cache_max_size_mb: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GeminiMode {
    #[default]
    Speed,
    Accuracy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gemini {
    /// DPAPI-encrypted, base64. Decrypt via `ai::client::get_api_key`.
    pub api_key_encrypted: String,
    pub default_model: String,
    pub speed_model: String,
    pub accuracy_model: String,
    /// Gemini model for read-aloud and audio export.
    pub tts_model: String,
    pub default_mode: GeminiMode,
    pub max_concurrent_pages: u32,
    pub timeout_seconds: u64,
}

/// OCR backend preference. `Windows` (built-in Windows.Media.Ocr) replaces
/// Prism's Surya local model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OcrBackend {
    #[default]
    Auto,
    Gemini,
    Windows,
    Tesseract,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ocr {
    pub render_dpi: u32,
    pub system_prompt_override: String,
    pub manga_mode: bool,
    pub user_instructions: String,
    #[serde(deserialize_with = "lenient_backend")]
    pub backend: OcrBackend,
}

/// Unknown backends (e.g. a hand-edited value) degrade to `auto` instead of
/// discarding the whole config.
fn lenient_backend<'de, D: serde::Deserializer<'de>>(d: D) -> Result<OcrBackend, D::Error> {
    let s = String::deserialize(d)?;
    Ok(match s.as_str() {
        "gemini" => OcrBackend::Gemini,
        "windows" | "surya" => OcrBackend::Windows,
        "tesseract" => OcrBackend::Tesseract,
        _ => OcrBackend::Auto,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Navigation {
    pub highlight_on_navigate: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Annotations {
    pub play_sound_on_bookmark: bool,
    pub speak_bookmark_confirmation: bool,
    pub highlight_bookmarked_lines: bool,
    pub default_export_format: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Export {
    pub default_format: String,
    pub default_output_dir: String,
    pub batch_concurrent: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    pub sidebar_width: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tts {
    pub engine: String,
    pub voice: String,
    pub rate: u32,
    pub volume: u32,
    pub highlight_spoken_text: bool,
    pub speak_page_numbers: bool,
    pub announce_sections: bool,
    /// "book", "chapter" or "page".
    pub reading_scope: String,
    /// "cursor" or "beginning".
    pub start_from: String,
    pub paragraph_pause_ms: u64,
    pub page_pause_ms: u64,
    pub active_profile: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            general: General::default(),
            display: Display::default(),
            document: DocumentSettings::default(),
            gemini: Gemini::default(),
            ocr: Ocr::default(),
            navigation: Navigation::default(),
            annotations: Annotations::default(),
            export: Export::default(),
            window: WindowState::default(),
            tts: Tts::default(),
            recent_files: Vec::new(),
        }
    }
}

impl Default for General {
    fn default() -> Self {
        Self { continuous_reading: false, page_size: constants::DEFAULT_PAGE_SIZE, recent_files_limit: 10, remember_position: true }
    }
}

impl Default for Display {
    fn default() -> Self {
        Self { font_face: "Consolas".into(), font_size: 14, use_opendyslexic: false, line_spacing: 1.2 }
    }
}

impl Default for DocumentSettings {
    fn default() -> Self {
        Self { pdf_reading_mode: PdfReadingMode::Automatic, cache_enabled: true, cache_max_size_mb: 500 }
    }
}

impl Default for Gemini {
    // Model defaults chosen for OCR cost and latency, following Google's
    // migration table as of October 2026. Keep `RETIRED_MODEL_DEFAULTS` in step.
    fn default() -> Self {
        Self {
            api_key_encrypted: String::new(),
            default_model: "gemini-3.5-flash-lite".into(),
            speed_model: "gemini-3.5-flash-lite".into(),
            accuracy_model: "gemini-3.8-flash".into(),
            tts_model: "gemini-3.8-flash-lite-tts".into(),
            default_mode: GeminiMode::Speed,
            max_concurrent_pages: 3,
            timeout_seconds: 60,
        }
    }
}

impl Default for Ocr {
    fn default() -> Self {
        Self {
            render_dpi: 300,
            system_prompt_override: String::new(),
            manga_mode: false,
            user_instructions: String::new(),
            backend: OcrBackend::Auto,
        }
    }
}

impl Default for Navigation {
    fn default() -> Self {
        Self { highlight_on_navigate: true }
    }
}

impl Default for Annotations {
    fn default() -> Self {
        Self {
            play_sound_on_bookmark: true,
            speak_bookmark_confirmation: true,
            highlight_bookmarked_lines: false,
            default_export_format: "text".into(),
        }
    }
}

impl Default for Export {
    fn default() -> Self {
        Self { default_format: "text".into(), default_output_dir: String::new(), batch_concurrent: 2 }
    }
}

impl Default for WindowState {
    fn default() -> Self {
        Self { x: -1, y: -1, width: 800, height: 600, maximized: false, sidebar_width: constants::SIDEBAR_DEFAULT_WIDTH }
    }
}

impl Default for Tts {
    fn default() -> Self {
        Self {
            engine: "sapi".into(),
            voice: String::new(),
            rate: 50,
            volume: 100,
            highlight_spoken_text: true,
            speak_page_numbers: true,
            announce_sections: true,
            reading_scope: "book".into(),
            start_from: "cursor".into(),
            paragraph_pause_ms: 300,
            page_pause_ms: 500,
            active_profile: "Deep Reading".into(),
        }
    }
}

impl Settings {
    /// Pushes `path` to the top of the MRU list, removing duplicates.
    pub fn add_recent_file(&mut self, path: &str) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_string());
        self.recent_files.truncate(self.general.recent_files_limit);
    }
}

struct Store {
    path: PathBuf,
    settings: Settings,
}

static STORE: LazyLock<RwLock<Store>> = LazyLock::new(|| {
    let path = constants::config_file();
    let settings = load(&path);
    RwLock::new(Store { path, settings })
});

/// Earlier default model IDs and their current defaults. `save` writes every field,
/// so a user who never chose a model would otherwise stay on the old default forever.
const RETIRED_MODEL_DEFAULTS: [(&str, &str); 3] = [
    ("gemini-3.1-flash-lite", "gemini-3.5-flash-lite"),
    ("gemini-3.5-flash", "gemini-3.8-flash"),
    ("gemini-3.1-flash-tts-preview", "gemini-3.8-flash-lite-tts"),
];

impl Gemini {
    /// Moves each model setting still on an earlier default to the current one.
    fn upgrade_retired_defaults(&mut self) {
        for model in [&mut self.default_model, &mut self.speed_model, &mut self.accuracy_model, &mut self.tts_model] {
            if let Some((_, new)) = RETIRED_MODEL_DEFAULTS.iter().find(|(old, _)| model == old) {
                *model = new.to_string();
            }
        }
    }
}

fn load(path: &std::path::Path) -> Settings {
    let mut settings = match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            log::warn!("config {} unreadable ({e}); using defaults", path.display());
            Settings::default()
        }),
        Err(_) => Settings::default(),
    };
    settings.gemini.upgrade_retired_defaults();
    settings
}

fn save(store: &Store) {
    if let Some(dir) = store.path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match toml::to_string_pretty(&store.settings) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&store.path, text) {
                log::error!("failed to save config: {e}");
            }
        }
        Err(e) => log::error!("failed to serialize config: {e}"),
    }
}

/// Snapshot of the current settings.
pub fn get() -> Settings {
    STORE.read().unwrap().settings.clone()
}

/// Mutates settings, saves to disk, and emits `ConfigChanged { key }`.
/// `key` names what changed (`"display.font_size"`), or a section (`"display"`).
pub fn update(key: &str, f: impl FnOnce(&mut Settings)) {
    {
        let mut store = STORE.write().unwrap();
        f(&mut store.settings);
        save(&store);
    }
    events::emit(AppEvent::ConfigChanged { key: key.to_string() });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_fills_defaults_and_unknown_backend_degrades() {
        let s: Settings = toml::from_str("[display]\nfont_size = 20\n[ocr]\nbackend = \"mistral\"\n").unwrap();
        assert_eq!(s.display.font_size, 20);
        assert_eq!(s.display.font_face, "Consolas");
        assert_eq!(s.ocr.backend, OcrBackend::Auto);
        assert_eq!(s.ocr.render_dpi, 300);
    }

    #[test]
    fn retired_default_models_upgrade_and_chosen_ones_stay() {
        let mut g: Gemini = toml::from_str(
            "default_model = \"gemini-3.1-flash-lite\"\naccuracy_model = \"gemini-3.5-flash\"\nspeed_model = \"gemini-3.1-pro-preview\"\ntts_model = \"gemini-3.1-flash-tts-preview\"\n",
        )
        .unwrap();
        g.upgrade_retired_defaults();
        assert_eq!(g.default_model, "gemini-3.5-flash-lite");
        assert_eq!(g.accuracy_model, "gemini-3.8-flash");
        assert_eq!(g.speed_model, "gemini-3.1-pro-preview");
        assert_eq!(g.tts_model, "gemini-3.8-flash-lite-tts");
    }

    #[test]
    fn recent_files_dedupe_and_limit() {
        let mut s = Settings::default();
        s.general.recent_files_limit = 2;
        s.add_recent_file("a");
        s.add_recent_file("b");
        s.add_recent_file("a");
        s.add_recent_file("c");
        assert_eq!(s.recent_files, vec!["c", "a"]);
    }
}
