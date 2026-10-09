//! STUB: port of prism/ai/client.py. Public signatures are the contract.

use crate::cancel::CancelToken;
use crate::config::GeminiMode;

/// DPAPI-encrypts an API key for storage in settings (base64 output).
pub fn encrypt_api_key(_plain: &str) -> anyhow::Result<String> {
    anyhow::bail!("not yet ported")
}

pub fn decrypt_api_key(_encrypted_b64: &str) -> anyhow::Result<String> {
    anyhow::bail!("not yet ported")
}

/// The Gemini key from `GEMINI_API_KEY`, else the decrypted stored key.
pub fn get_api_key() -> Option<String> {
    None
}

pub const DEFAULT_TTS_MODEL: &str = "gemini-3.1-flash-tts-preview";

pub struct GeminiClient;

impl GeminiClient {
    pub fn new() -> anyhow::Result<Self> {
        anyhow::bail!("not yet ported")
    }

    pub fn is_available() -> bool {
        false
    }

    /// Image + prompt to text. Retries with backoff and falls back to the fallback model.
    pub fn generate_with_image(&self, _image: &[u8], _mime_type: &str, _prompt: &str, _system_instruction: &str, _mode: GeminiMode, _cancel: Option<&CancelToken>) -> anyhow::Result<String> {
        anyhow::bail!("not yet ported")
    }

    pub fn generate_json_with_image(&self, _image: &[u8], _mime_type: &str, _prompt: &str, _system_instruction: &str, _mode: GeminiMode, _cancel: Option<&CancelToken>) -> anyhow::Result<serde_json::Value> {
        anyhow::bail!("not yet ported")
    }

    /// Text to raw PCM (24 kHz, 16-bit, mono).
    pub fn generate_audio(&self, _text: &str, _voice: &str, _model: &str, _cancel: Option<&CancelToken>) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("not yet ported")
    }
}
