//! TTS engine registry: discovers and creates engines (port of prism/tts/engines/__init__.py).

pub mod gemini_tts;
pub mod onecore;
pub mod sapi;

use std::sync::Arc;

use super::engine::TtsEngine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineInfo {
    /// Key stored in voice profiles: `sapi`, `onecore` or `gemini`.
    pub key: &'static str,
    pub name: &'static str,
}

/// Engines usable on this system, in preference order (SAPI, OneCore, Gemini).
pub fn available_engines() -> Vec<EngineInfo> {
    let mut engines = Vec::new();
    if sapi::SapiEngine::is_available() {
        engines.push(EngineInfo { key: "sapi", name: sapi::NAME });
    }
    if onecore::OneCoreEngine::is_available() {
        engines.push(EngineInfo { key: "onecore", name: onecore::NAME });
    }
    if gemini_tts::GeminiTtsEngine::is_available() {
        engines.push(EngineInfo { key: "gemini", name: gemini_tts::NAME });
    }
    engines
}

/// Creates an engine by key. Fails for unknown keys and for engines that cannot start here.
pub fn create_engine(key: &str) -> anyhow::Result<Arc<dyn TtsEngine>> {
    Ok(match key {
        "sapi" => Arc::new(sapi::SapiEngine::new()?),
        "onecore" => Arc::new(onecore::OneCoreEngine::new()?),
        "gemini" => Arc::new(gemini_tts::GeminiTtsEngine::new()?),
        _ => anyhow::bail!("TTS engine '{key}' is not available. Available engines: {:?}", available_engines().iter().map(|e| e.key).collect::<Vec<_>>()),
    })
}
