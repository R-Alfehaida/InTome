//! Gemini REST client with retry, model fallback, and DPAPI key storage
//! (port of prism/ai/client.py, which used the google-genai SDK).

use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData};
use windows::core::w;

use crate::cancel::CancelToken;
use crate::config::{self, GeminiMode};

/// DPAPI-encrypts an API key for storage in settings (base64 output).
pub fn encrypt_api_key(plain: &str) -> anyhow::Result<String> {
    Ok(B64.encode(dpapi(plain.as_bytes(), true).context("CryptProtectData failed")?))
}

/// Decrypts a base64 DPAPI blob, including one written by Prism.
pub fn decrypt_api_key(encrypted_b64: &str) -> anyhow::Result<String> {
    let blob = B64.decode(encrypted_b64.trim())?;
    Ok(String::from_utf8(dpapi(&blob, false).context("CryptUnprotectData failed")?)?)
}

fn dpapi(data: &[u8], protect: bool) -> windows::core::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        if protect {
            CryptProtectData(&input, w!("InTomeAPIKey"), None, None, None, 0, &mut out)?;
        } else {
            CryptUnprotectData(&input, None, None, None, None, 0, &mut out)?;
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(Some(HLOCAL(out.pbData.cast())));
        Ok(bytes)
    }
}

/// The Gemini key from `GEMINI_API_KEY`, else the decrypted stored key.
pub fn get_api_key() -> Option<String> {
    if let Some(key) = std::env::var("GEMINI_API_KEY").ok().filter(|k| !k.is_empty()) {
        return Some(key);
    }
    let encrypted = config::get().gemini.api_key_encrypted;
    if encrypted.is_empty() {
        return None;
    }
    decrypt_api_key(&encrypted).map_err(|_| log::error!("Failed to decrypt API key from config")).ok()
}

/// Output-token cap for OCR. Raised from the original 8192 because a dense
/// page's tagged OCR output can exceed that and be silently truncated; a
/// response that still hits the cap is detected and logged.
pub const OCR_MAX_OUTPUT_TOKENS: u32 = 32768;

const MAX_RETRIES: u32 = 3;
const BASE_DELAY_SECS: f64 = 1.0;
const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";
// Base64 audio for a long TTS chunk passes ureq's 10 MB default body limit.
const MAX_RESPONSE_BYTES: u64 = 256 * 1024 * 1024;

/// What to do after a failed attempt, decided from the lowercased error text.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Failure {
    Auth,
    Quota,
    NextModel,
    Retry { delay_secs: f64 },
}

fn classify_failure(error_lower: &str, attempt: u32) -> Failure {
    if error_lower.contains("invalid api key") || error_lower.contains("permission") {
        Failure::Auth
    // Quota exhausted (limit 0 = model has no free tier).
    } else if error_lower.contains("resource_exhausted") || (error_lower.contains("quota") && error_lower.contains("limit: 0")) {
        Failure::Quota
    } else if error_lower.contains("not found") && attempt == 0 {
        Failure::NextModel
    } else {
        Failure::Retry { delay_secs: BASE_DELAY_SECS * 2f64.powi(attempt as i32) }
    }
}

fn image_request(image: &[u8], mime_type: &str, prompt: &str, system_instruction: &str, json_mode: bool) -> Value {
    let mut generation_config = json!({ "maxOutputTokens": OCR_MAX_OUTPUT_TOKENS, "temperature": 0.1 });
    if json_mode {
        generation_config["responseMimeType"] = json!("application/json");
    }
    let mut body = json!({
        "contents": [{ "role": "user", "parts": [
            { "inlineData": { "mimeType": mime_type, "data": B64.encode(image) } },
            { "text": prompt },
        ]}],
        "generationConfig": generation_config,
    });
    if !system_instruction.is_empty() {
        body["systemInstruction"] = json!({ "parts": [{ "text": system_instruction }] });
    }
    body
}

fn audio_request(text: &str, voice: &str) -> Value {
    json!({
        "contents": [{ "role": "user", "parts": [{ "text": text }] }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": { "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": voice } } },
        },
    })
}

/// The first candidate's text parts, skipping thought summaries. Empty when the
/// model returned no text part, which counts as an empty extraction.
fn response_text(response: &Value) -> String {
    let parts = response["candidates"][0]["content"]["parts"].as_array();
    parts
        .into_iter()
        .flatten()
        .filter(|p| p["thought"] != json!(true))
        .filter_map(|p| p["text"].as_str())
        .collect()
}

/// True if any candidate stopped at the output-token cap. Matched by name so a
/// renamed or prefixed enum value still counts.
fn hit_token_cap(response: &Value) -> bool {
    response["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["finishReason"].as_str())
        .any(|r| r.to_ascii_uppercase().contains("MAX_TOKENS"))
}

/// Strips a markdown code fence the model may wrap JSON in.
fn strip_code_fence(text: &str) -> String {
    let text = text.trim();
    if !text.starts_with("```") {
        return text.to_string();
    }
    text.split('\n').skip(1).filter(|l| l.trim() != "```").collect::<Vec<_>>().join("\n")
}

/// Raw PCM from an audio part: WAV output has its header removed so callers
/// always get bare 16-bit samples.
fn pcm_from_audio(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return bytes;
    }
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
        if &bytes[i..i + 4] == b"data" {
            let end = (i + 8 + size).min(bytes.len());
            return bytes[i + 8..end].to_vec();
        }
        i += 8 + size + (size & 1);
    }
    bytes
}

pub struct GeminiClient {
    api_key: String,
    agent: ureq::Agent,
}

impl GeminiClient {
    pub fn new() -> anyhow::Result<Self> {
        let key = get_api_key().ok_or_else(|| {
            anyhow!(
                "No Gemini API key configured. Set GEMINI_API_KEY environment variable or configure the key in Preferences > AI Providers."
            )
        })?;
        Ok(Self::with_key(key))
    }

    fn with_key(api_key: String) -> Self {
        let timeout = Duration::from_secs(config::get().gemini.timeout_seconds.max(1));
        let agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(timeout)).build().new_agent();
        Self { api_key, agent }
    }

    pub fn is_available() -> bool {
        get_api_key().is_some()
    }

    /// One HTTP call. Errors carry the status and API error body so
    /// [`classify_failure`] can match on them as Prism matched SDK exceptions.
    fn post(&self, model: &str, body: &Value) -> Result<Value, String> {
        let url = format!("{API_BASE}/{model}:generateContent");
        let mut resp = self
            .agent
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .send_json(body)
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.body_mut().with_config().limit(MAX_RESPONSE_BYTES).read_to_string().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("{status} {text}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("invalid response JSON: {e}"))
    }

    fn generate(&self, image: &[u8], mime_type: &str, prompt: &str, system_instruction: &str, mode: GeminiMode, cancel: Option<&CancelToken>, json_mode: bool) -> anyhow::Result<String> {
        let settings = config::get().gemini;
        let model = match mode {
            GeminiMode::Speed => settings.speed_model,
            GeminiMode::Accuracy => settings.accuracy_model,
        };
        let mut models = vec![model];
        if !settings.default_model.is_empty() && settings.default_model != models[0] {
            models.push(settings.default_model);
        }
        let body = image_request(image, mime_type, prompt, system_instruction, json_mode);

        let mut last_error = String::new();
        for model in &models {
            for attempt in 0..MAX_RETRIES {
                if cancel.is_some_and(CancelToken::is_cancelled) {
                    bail!("Operation cancelled");
                }
                let err = match self.post(model, &body) {
                    Ok(response) => {
                        if hit_token_cap(&response) {
                            log::warn!(
                                "OCR output truncated: model '{model}' hit the {OCR_MAX_OUTPUT_TOKENS}-token output cap (finish_reason=MAX_TOKENS); the tail of this page is missing from the extracted text. Re-run OCR on a smaller region or raise OCR_MAX_OUTPUT_TOKENS."
                            );
                        }
                        return Ok(response_text(&response));
                    }
                    Err(e) => e,
                };
                match classify_failure(&err.to_lowercase(), attempt) {
                    Failure::Auth => bail!("Authentication error: {err}"),
                    Failure::Quota => bail!(
                        "Quota exhausted for model '{model}'. This model may not have a free tier. Try switching to gemini-3.5-flash or gemini-3.1-flash-lite in Preferences > Gemini Models, or upgrade to a paid API plan."
                    ),
                    Failure::NextModel => {
                        log::warn!("Model {model} not available, trying next");
                        last_error = err;
                        break;
                    }
                    Failure::Retry { delay_secs } => {
                        log::warn!("Gemini request failed (attempt {}/{MAX_RETRIES}): {err}. Retrying in {delay_secs:.1}s", attempt + 1);
                        last_error = err;
                        std::thread::sleep(Duration::from_secs_f64(delay_secs));
                    }
                }
            }
        }
        bail!("Gemini API request failed after all retries: {last_error}")
    }

    /// Image + prompt to text. Retries with backoff and falls back to the fallback model.
    pub fn generate_with_image(&self, image: &[u8], mime_type: &str, prompt: &str, system_instruction: &str, mode: GeminiMode, cancel: Option<&CancelToken>) -> anyhow::Result<String> {
        self.generate(image, mime_type, prompt, system_instruction, mode, cancel, false)
    }

    /// Like [`Self::generate_with_image`] with JSON response mode, parsed.
    pub fn generate_json_with_image(&self, image: &[u8], mime_type: &str, prompt: &str, system_instruction: &str, mode: GeminiMode, cancel: Option<&CancelToken>) -> anyhow::Result<Value> {
        let text = self.generate(image, mime_type, prompt, system_instruction, mode, cancel, true)?;
        Ok(serde_json::from_str(&strip_code_fence(&text))?)
    }

    /// Every model this key can call with `generateContent`, sorted, without the `models/` prefix.
    pub fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let mut names = Vec::new();
        let mut token = String::new();
        loop {
            let mut req = self.agent.get(API_BASE).header("x-goog-api-key", &self.api_key).query("pageSize", "1000");
            if !token.is_empty() {
                req = req.query("pageToken", &token);
            }
            let mut resp = req.call()?;
            let text = resp.body_mut().read_to_string()?;
            if !resp.status().is_success() {
                bail!("{} {text}", resp.status());
            }
            let page: Value = serde_json::from_str(&text)?;
            names.extend(generate_content_models(&page));
            match page["nextPageToken"].as_str() {
                Some(t) if !t.is_empty() => token = t.to_string(),
                _ => break,
            }
        }
        names.sort();
        Ok(names)
    }

    /// Text to raw PCM (24 kHz, 16-bit, mono).
    pub fn generate_audio(&self, text: &str, voice: &str, model: &str, cancel: Option<&CancelToken>) -> anyhow::Result<Vec<u8>> {
        let body = audio_request(text, voice);
        let mut last_error = String::new();
        for attempt in 0..MAX_RETRIES {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                bail!("Operation cancelled");
            }
            let err = match self.post(model, &body) {
                Ok(response) => match response["candidates"][0]["content"]["parts"][0]["inlineData"]["data"].as_str() {
                    Some(data) => return Ok(pcm_from_audio(B64.decode(data)?)),
                    None => format!("response has no audio data: {}", response["candidates"][0]["finishReason"]),
                },
                Err(e) => e,
            };
            let lower = err.to_lowercase();
            match classify_failure(&lower, attempt) {
                Failure::Auth => bail!("Authentication error: {err}"),
                Failure::Quota => bail!("Quota exhausted for TTS model '{model}'. Try again later or upgrade to a paid API plan."),
                // TTS has no fallback model, so "not found" just retries like Prism.
                Failure::NextModel | Failure::Retry { .. } => {
                    let delay = BASE_DELAY_SECS * 2f64.powi(attempt as i32);
                    log::warn!("Gemini TTS request failed (attempt {}/{MAX_RETRIES}): {err}. Retrying in {delay:.1}s", attempt + 1);
                    last_error = err;
                    std::thread::sleep(Duration::from_secs_f64(delay));
                }
            }
        }
        bail!("Gemini TTS request failed after all retries: {last_error}")
    }
}

/// Names of the models in one `models.list` page that support `generateContent`.
fn generate_content_models(page: &Value) -> Vec<String> {
    let models = page["models"].as_array().map(Vec::as_slice).unwrap_or_default();
    models
        .iter()
        .filter(|m| m["supportedGenerationMethods"].as_array().is_some_and(|a| a.iter().any(|v| v == "generateContent")))
        .filter_map(|m| m["name"].as_str())
        .map(|n| n.trim_start_matches("models/").to_string())
        .collect()
}

/// Whether `model` speaks rather than writes. The API has no field for this, so it goes by
/// Google's naming. shortcut: relies on "tts" in the name, revisit if Google renames TTS models.
pub fn is_tts_model(model: &str) -> bool {
    model.contains("tts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_list_keeps_generate_content_models_without_prefix() {
        let page = json!({"models": [
            {"name": "models/gemini-3.5-flash", "supportedGenerationMethods": ["generateContent", "countTokens"]},
            {"name": "models/text-embedding-004", "supportedGenerationMethods": ["embedContent"]},
            {"name": "models/gemini-3.1-flash-tts-preview", "supportedGenerationMethods": ["generateContent"]},
            {"name": "models/no-methods"}
        ]});
        assert_eq!(generate_content_models(&page), ["gemini-3.5-flash", "gemini-3.1-flash-tts-preview"]);
        assert_eq!(generate_content_models(&json!({})), Vec::<String>::new());
        assert!(is_tts_model("gemini-3.1-flash-tts-preview") && !is_tts_model("gemini-3.5-flash"));
    }

    #[test]
    fn retry_decision_follows_prism_error_matching() {
        assert_eq!(classify_failure("403 {\"status\": \"permission_denied\"}", 0), Failure::Auth);
        assert_eq!(classify_failure("429 {\"status\": \"resource_exhausted\"}", 2), Failure::Quota);
        assert_eq!(classify_failure("quota exceeded, limit: 0", 0), Failure::Quota);
        assert_eq!(classify_failure("404 models/x is not found for api version v1beta", 0), Failure::NextModel);
        assert_eq!(classify_failure("404 models/x is not found", 1), Failure::Retry { delay_secs: 2.0 });
        assert_eq!(classify_failure("503 unavailable", 0), Failure::Retry { delay_secs: 1.0 });
        assert_eq!(classify_failure("timeout", 2), Failure::Retry { delay_secs: 4.0 });
    }

    #[test]
    fn image_request_matches_gemini_rest_shape() {
        let body = image_request(b"\x89PNG", "image/png", "Extract.", "You are OCR.", true);
        assert_eq!(
            body,
            json!({
                "contents": [{ "role": "user", "parts": [
                    { "inlineData": { "mimeType": "image/png", "data": "iVBORw==" } },
                    { "text": "Extract." },
                ]}],
                "generationConfig": { "maxOutputTokens": 32768, "temperature": 0.1, "responseMimeType": "application/json" },
                "systemInstruction": { "parts": [{ "text": "You are OCR." }] },
            })
        );
        assert!(image_request(b"", "image/png", "p", "", false).get("systemInstruction").is_none());
    }

    #[test]
    fn canned_response_text_and_token_cap() {
        let resp = json!({ "candidates": [{
            "content": { "parts": [{ "text": "plan", "thought": true }, { "text": "[H1]Hi" }, { "text": "[/H1]" }] },
            "finishReason": "MAX_TOKENS",
        }]});
        assert_eq!(response_text(&resp), "[H1]Hi[/H1]");
        assert!(hit_token_cap(&resp));
        let done = json!({ "candidates": [{ "content": { "parts": [] }, "finishReason": "STOP" }] });
        assert_eq!(response_text(&done), "");
        assert!(!hit_token_cap(&done));
    }

    #[test]
    fn json_fence_is_stripped() {
        assert_eq!(strip_code_fence("```json\n[{\"a\": 1}]\n```\n"), "[{\"a\": 1}]");
        assert_eq!(strip_code_fence(" [1] "), "[1]");
    }

    #[test]
    fn wav_header_is_removed_from_audio() {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt \x02\0\0\0xxdata\x04\0\0\0".to_vec();
        wav.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(pcm_from_audio(wav), vec![1, 2, 3, 4]);
        assert_eq!(pcm_from_audio(vec![9, 8]), vec![9, 8]);
    }

    #[test]
    fn dpapi_round_trip() {
        let enc = encrypt_api_key("AIza-test-ключ").unwrap();
        assert_ne!(enc, "AIza-test-ключ");
        assert_eq!(decrypt_api_key(&enc).unwrap(), "AIza-test-ключ");
    }

    /// The key Prism stored in `%APPDATA%\Prism\prism.ini`, decrypted with this port.
    fn prism_ini_key() -> Option<String> {
        let ini = std::fs::read_to_string(std::path::Path::new(&std::env::var("APPDATA").ok()?).join("Prism").join("prism.ini")).ok()?;
        let enc = ini.lines().find_map(|l| l.trim().strip_prefix("api_key_encrypted")?.trim().strip_prefix('=').map(str::trim))?;
        Some(decrypt_api_key(enc).expect("Prism's DPAPI blob must decrypt"))
    }

    fn live_client() -> GeminiClient {
        let key = std::env::var("GEMINI_API_KEY").ok().or_else(prism_ini_key).expect("no Gemini key available");
        GeminiClient::with_key(key)
    }

    #[test]
    #[ignore = "live Gemini call; needs target/ai_scratch/page_clean.png"]
    fn live_ocr_page_through_default_prompt() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ai_scratch");
        let png = std::fs::read(dir.join("page_clean.png")).unwrap();
        let prompt = crate::ai::prompts::get_ocr_prompt(false, "", "");
        let raw = live_client()
            .generate_with_image(&png, "image/png", "Extract all text from this page image.", &prompt, GeminiMode::Accuracy, None)
            .unwrap();
        let parsed = crate::ai::structure::parse_ocr_response(&raw);
        std::fs::write(dir.join("gemini_raw.txt"), &raw).unwrap();
        std::fs::write(dir.join("gemini.txt"), &parsed.text).unwrap();
        println!("model: {}\n--- raw ---\n{raw}\n--- elements ---", config::get().gemini.accuracy_model);
        for e in &parsed.elements {
            println!("{:?} {}..{} {:?}", e.element_type, e.start_pos, e.end_pos, e.text);
        }
    }

    #[test]
    #[ignore = "live Gemini call"]
    fn live_tts_returns_pcm() {
        let pcm = live_client().generate_audio("Hello from InTome.", "Kore", &config::get().gemini.tts_model, None).unwrap();
        assert!(pcm.len() > 24_000 && pcm.len() % 2 == 0, "got {} bytes", pcm.len());
        assert_ne!(&pcm[..4], b"RIFF");
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ai_scratch/tts.pcm");
        std::fs::write(&out, &pcm).unwrap();
        println!("TTS: {} bytes = {:.2} s at 24 kHz 16-bit mono -> {}", pcm.len(), pcm.len() as f64 / 48_000.0, out.display());
    }
}
