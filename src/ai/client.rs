//! Gemini REST client with retry, model fallback, and DPAPI key storage
//! (port of prism/ai/client.py, which used the google-genai SDK). Requests go
//! through the Interactions API; only the model list still uses `models.list`.

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
/// Rate-limit waits a request may sit through on top of its retries: on a Tier 1
/// key TTS allows 10 requests a minute, which a parallel audio export reaches,
/// and a long book should wait its turn, not fail.
const MAX_RATE_LIMIT_WAITS: u32 = 20;
const BASE_DELAY_SECS: f64 = 1.0;
const INTERACTIONS_URL: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";
const MODELS_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
// Pins the steps-based response schema these parsers read.
const API_REVISION: &str = "2026-05-20";
// Base64 audio for a long TTS chunk passes ureq's 10 MB default body limit.
const MAX_RESPONSE_BYTES: u64 = 256 * 1024 * 1024;

/// What to do after a failed attempt, decided from the lowercased error text.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Failure {
    Auth,
    Quota,
    NextModel,
    Retry { delay_secs: f64 },
    /// Too many requests just now; Google said how long to wait.
    RateLimited { delay_secs: f64 },
}

/// Sleeps `secs`, waking early when `cancel` fires, so Cancel stays prompt during
/// a half-minute rate-limit wait.
fn wait(secs: f64, cancel: Option<&CancelToken>) {
    let until = std::time::Instant::now() + Duration::from_secs_f64(secs);
    while std::time::Instant::now() < until && !cancel.is_some_and(CancelToken::is_cancelled) {
        std::thread::sleep(Duration::from_millis(200).min(until.saturating_duration_since(std::time::Instant::now())));
    }
}

/// Seconds Google asks to wait before retrying ("please retry in 33.6s" or
/// `"retryDelay": "33s"`), capped at a minute.
fn retry_hint(error_lower: &str) -> Option<f64> {
    static HINT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"retry in ([0-9.]+)s|retrydelay"?\s*:\s*"([0-9.]+)s"#).unwrap());
    let caps = HINT.captures(error_lower)?;
    let secs: f64 = caps.get(1).or_else(|| caps.get(2))?.as_str().parse().ok()?;
    Some(secs.clamp(1.0, 60.0))
}

/// How long to wait when the error is a temporary rate limit: Google's hint, or
/// 20 s when it only says "retry later". None for anything else, including a
/// "limit: 0" quota, which no wait fixes.
fn rate_limit_wait(error_lower: &str) -> Option<f64> {
    if error_lower.contains("limit: 0") {
        return None;
    }
    let limited = ["too_many_requests", "rate limit exceeded", "429 too many requests"].iter().any(|s| error_lower.contains(s));
    retry_hint(error_lower).or(limited.then_some(20.0))
}

fn classify_failure(error_lower: &str, attempt: u32) -> Failure {
    if error_lower.contains("invalid api key") || error_lower.contains("permission") {
        Failure::Auth
    // A per-minute rate limit says when to try again; a used-up quota doesn't. Parallel
    // audio export hits per-minute limits sooner than one request at a time did.
    } else if let Some(delay_secs) = rate_limit_wait(error_lower) {
        Failure::RateLimited { delay_secs }
    // Quota exhausted (limit 0 = model has no free tier).
    } else if error_lower.contains("resource_exhausted") || (error_lower.contains("quota") && error_lower.contains("limit: 0")) {
        Failure::Quota
    } else if error_lower.contains("not found") && attempt == 0 {
        Failure::NextModel
    } else {
        Failure::Retry { delay_secs: BASE_DELAY_SECS * 2f64.powi(attempt as i32) }
    }
}

// `store: false` on every request: the Interactions API keeps requests for up to
// 55 days by default, and these carry the user's documents.
// Gemini 3 deprecates `temperature`, so OCR no longer sets Prism's 0.1.
fn image_request(image: &[u8], mime_type: &str, prompt: &str, system_instruction: &str, json_mode: bool) -> Value {
    let mut body = json!({
        "input": [
            { "type": "image", "mime_type": mime_type, "data": B64.encode(image) },
            { "type": "text", "text": prompt },
        ],
        "generation_config": { "max_output_tokens": OCR_MAX_OUTPUT_TOKENS },
        "store": false,
    });
    if json_mode {
        body["response_format"] = json!({ "type": "text", "mime_type": "application/json" });
    }
    if !system_instruction.is_empty() {
        body["system_instruction"] = json!(system_instruction);
    }
    body
}

// No `mime_type` in `response_format`: 3.8 TTS models default to WAV and older ones to
// raw PCM, and the older ones reject an explicit `audio/l16`. Both are 24 kHz 16-bit mono.
/// A plain TTS text item. Expressive narration sends several, each with its own style.
pub fn text_item(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// The `speech_config` that reads every item in one prebuilt voice.
pub fn single_voice(voice: &str) -> Value {
    json!([{ "voice": voice }])
}

fn audio_request(items: Vec<Value>, speech_config: Value) -> Value {
    json!({
        "input": [{ "type": "user_input", "content": items }],
        "response_format": { "type": "audio" },
        "generation_config": { "speech_config": speech_config },
        "store": false,
    })
}

/// Content items of the `model_output` steps, in order. Thought and tool steps are skipped.
fn output_content(response: &Value) -> impl Iterator<Item = &Value> {
    let steps = response["steps"].as_array().map(Vec::as_slice).unwrap_or_default();
    steps
        .iter()
        .filter(|s| s["type"] == "model_output")
        .flat_map(|s| s["content"].as_array().map(Vec::as_slice).unwrap_or_default())
}

/// The model's output text. Empty when the model returned no text, which counts
/// as an empty extraction.
fn response_text(response: &Value) -> String {
    output_content(response).filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect()
}

/// The last audio block's base64 data.
fn response_audio(response: &Value) -> Option<&str> {
    output_content(response).filter(|c| c["type"] == "audio").filter_map(|c| c["data"].as_str()).last()
}

/// True if the interaction stopped early, for OCR at the output-token cap.
fn hit_token_cap(response: &Value) -> bool {
    response["status"] == "incomplete"
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

    /// One interaction. Errors carry the status and API error body so
    /// [`classify_failure`] can match on them as Prism matched SDK exceptions.
    fn post(&self, body: &Value) -> Result<Value, String> {
        let mut resp = self
            .agent
            .post(INTERACTIONS_URL)
            .header("x-goog-api-key", &self.api_key)
            .header("Api-Revision", API_REVISION)
            .send_json(body)
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.body_mut().with_config().limit(MAX_RESPONSE_BYTES).read_to_string().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("{status} {text}"));
        }
        let response: Value = serde_json::from_str(&text).map_err(|e| format!("invalid response JSON: {e}"))?;
        if response["status"] == "failed" {
            return Err(format!("interaction failed: {}", response["errors"]));
        }
        Ok(response)
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
        let mut body = image_request(image, mime_type, prompt, system_instruction, json_mode);

        let mut last_error = String::new();
        for model in &models {
            body["model"] = json!(model);
            for attempt in 0..MAX_RETRIES {
                if cancel.is_some_and(CancelToken::is_cancelled) {
                    bail!("Operation cancelled");
                }
                let err = match self.post(&body) {
                    Ok(response) => {
                        if hit_token_cap(&response) {
                            log::warn!(
                                "OCR output truncated: model '{model}' hit the {OCR_MAX_OUTPUT_TOKENS}-token output cap (status=incomplete); the tail of this page is missing from the extracted text. Re-run OCR on a smaller region or raise OCR_MAX_OUTPUT_TOKENS."
                            );
                        }
                        return Ok(response_text(&response));
                    }
                    Err(e) => e,
                };
                match classify_failure(&err.to_lowercase(), attempt) {
                    Failure::Auth => bail!("Authentication error: {err}"),
                    Failure::Quota => bail!(
                        "Quota exhausted for model '{model}'. This model may not have a free tier. Try switching to a Flash-Lite model such as gemini-3.5-flash-lite in Preferences > Gemini Models, or upgrade to a paid API plan."
                    ),
                    Failure::NextModel => {
                        log::warn!("Model {model} not available, trying next");
                        last_error = err;
                        break;
                    }
                    Failure::Retry { delay_secs } | Failure::RateLimited { delay_secs } => {
                        log::warn!("Gemini request failed (attempt {}/{MAX_RETRIES}): {err}. Retrying in {delay_secs:.1}s", attempt + 1);
                        last_error = err;
                        wait(delay_secs, cancel);
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

    /// Every model this key can call, sorted, without the `models/` prefix.
    pub fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let mut names = Vec::new();
        let mut token = String::new();
        loop {
            let mut req = self.agent.get(MODELS_URL).header("x-goog-api-key", &self.api_key).query("pageSize", "1000");
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
        self.generate_audio_items(vec![text_item(text)], single_voice(voice), model, cancel)
    }

    /// Text items (each with an optional `speech_metadata` style and speaker) to
    /// raw PCM (24 kHz, 16-bit, mono), read as `speech_config` says: one voice
    /// ([`single_voice`]) or up to two named speakers.
    pub fn generate_audio_items(&self, items: Vec<Value>, speech_config: Value, model: &str, cancel: Option<&CancelToken>) -> anyhow::Result<Vec<u8>> {
        let mut body = audio_request(items, speech_config);
        body["model"] = json!(model);
        let data = self.request("TTS", model, &body, cancel, |response| {
            response_audio(response).map(str::to_string).ok_or_else(|| format!("response has no audio data: status {}", response["status"]))
        })?;
        Ok(pcm_from_audio(B64.decode(data)?))
    }

    /// One text request whose reply must match the JSON `schema`, parsed.
    pub fn generate_json(&self, model: &str, system_instruction: &str, input: &str, schema: &Value, cancel: Option<&CancelToken>) -> anyhow::Result<Value> {
        let body = json!({
            "model": model,
            "system_instruction": system_instruction,
            "input": input,
            "response_format": { "type": "text", "mime_type": "application/json", "schema": schema },
            "store": false,
        });
        self.request("text", model, &body, cancel, |response| {
            serde_json::from_str(&strip_code_fence(&response_text(response))).map_err(|e| format!("reply is not the requested JSON: {e}"))
        })
    }

    /// Sends `body` with retries and backoff until `parse` accepts the reply. One
    /// model, no fallback; "not found" just retries, as Prism's TTS did.
    fn request<T>(&self, what: &str, model: &str, body: &Value, cancel: Option<&CancelToken>, parse: impl Fn(&Value) -> Result<T, String>) -> anyhow::Result<T> {
        let (mut attempt, mut rate_waits) = (0, 0);
        let mut last_error = String::new();
        while attempt < MAX_RETRIES {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                bail!("Operation cancelled");
            }
            let err = match self.post(body) {
                Ok(response) => match parse(&response) {
                    Ok(value) => return Ok(value),
                    Err(e) => e,
                },
                Err(e) => e,
            };
            let failure = classify_failure(&err.to_lowercase(), attempt);
            let delay = match failure {
                Failure::Auth => bail!("Authentication error: {err}"),
                Failure::Quota => bail!("Quota exhausted for {what} model '{model}'. Try again later or upgrade to a paid API plan."),
                // Waiting our turn is not a failed attempt, up to a limit.
                Failure::RateLimited { delay_secs } if rate_waits < MAX_RATE_LIMIT_WAITS => {
                    rate_waits += 1;
                    log::info!("Gemini {what} is rate limited; waiting {delay_secs:.0}s as asked ({rate_waits}/{MAX_RATE_LIMIT_WAITS})");
                    last_error = err;
                    wait(delay_secs, cancel);
                    continue;
                }
                Failure::Retry { delay_secs } | Failure::RateLimited { delay_secs } => delay_secs,
                Failure::NextModel => BASE_DELAY_SECS * 2f64.powi(attempt as i32),
            };
            log::warn!("Gemini {what} request failed (attempt {}/{MAX_RETRIES}): {err}. Retrying in {delay:.1}s", attempt + 1);
            last_error = err;
            wait(delay, cancel);
            attempt += 1;
        }
        bail!("Gemini {what} request failed after all retries: {last_error}")
    }
}

/// Names of the models in one `models.list` page that support `generateContent`. The list
/// has no Interactions flag; `generateContent` support marks the models the Interactions
/// API serves and leaves out Live-only and embedding models.
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
            {"name": "models/gemini-3.8-flash", "supportedGenerationMethods": ["generateContent", "countTokens"]},
            {"name": "models/gemini-embedding-2", "supportedGenerationMethods": ["embedContent"]},
            {"name": "models/gemini-3.8-live", "supportedGenerationMethods": ["bidiGenerateContent"]},
            {"name": "models/gemini-3.8-flash-lite-tts", "supportedGenerationMethods": ["generateContent"]},
            {"name": "models/no-methods"}
        ]});
        assert_eq!(generate_content_models(&page), ["gemini-3.8-flash", "gemini-3.8-flash-lite-tts"]);
        assert_eq!(generate_content_models(&json!({})), Vec::<String>::new());
        assert!(is_tts_model("gemini-3.8-flash-lite-tts") && !is_tts_model("gemini-3.8-flash"));
    }

    #[test]
    fn retry_decision_follows_prism_error_matching() {
        assert_eq!(classify_failure("403 {\"status\": \"permission_denied\"}", 0), Failure::Auth);
        assert_eq!(classify_failure("429 {\"status\": \"resource_exhausted\"}", 2), Failure::Quota);
        assert_eq!(classify_failure("quota exceeded, limit: 0", 0), Failure::Quota);
        assert_eq!(classify_failure("404 models/x is not found for api version v1beta", 0), Failure::NextModel);
        // The Interactions API's wording for an unknown model, captured live.
        assert_eq!(classify_failure("404 not found {\"error\":{\"message\":\"model 'x' not found.\",\"code\":\"not_found\"}}", 0), Failure::NextModel);
        assert_eq!(classify_failure("404 models/x is not found", 1), Failure::Retry { delay_secs: 2.0 });
        assert_eq!(classify_failure("503 unavailable", 0), Failure::Retry { delay_secs: 1.0 });
        assert_eq!(classify_failure("timeout", 2), Failure::Retry { delay_secs: 4.0 });
    }

    #[test]
    fn a_rate_limit_that_says_when_to_retry_is_retried_after_that_delay() {
        let per_minute = "429 {\"error\":{\"message\":\"quota exceeded for metric requests per minute, limit: 10. please retry in 33.6s.\",\"status\":\"resource_exhausted\"}}";
        assert_eq!(classify_failure(per_minute, 0), Failure::RateLimited { delay_secs: 33.6 });
        assert_eq!(classify_failure("429 resource_exhausted \"retrydelay\": \"12s\"", 1), Failure::RateLimited { delay_secs: 12.0 });
        assert_eq!(classify_failure("429 resource_exhausted, please retry in 500s", 0), Failure::RateLimited { delay_secs: 60.0 }, "capped");
        // The Interactions API's TTS rate limit on a Tier 1 key, captured live.
        let tier1 = "429 too many requests {\"error\":{\"message\":\"rate limit exceeded for model gemini-3.8-flash-lite-tts (limit: 10 requests per minute on tier 1). please retry in 31s or upgrade your tier at https://ai.dev/rate-limit.\",\"code\":\"too_many_requests\"}}";
        assert_eq!(classify_failure(tier1, 0), Failure::RateLimited { delay_secs: 31.0 });
        // The same limit sometimes says only "retry later".
        let later = "429 too many requests {\"error\":{\"message\":\"rate limit exceeded for model gemini-3.8-flash-lite-tts (limit: 10 requests per minute on tier 1). please retry later or upgrade your tier at https://ai.dev/rate-limit.\",\"code\":\"too_many_requests\"}}";
        assert_eq!(classify_failure(later, 0), Failure::RateLimited { delay_secs: 20.0 });
        // No free tier, or no hint: still a quota error that stops at once.
        assert_eq!(classify_failure("429 resource_exhausted limit: 0, please retry in 30s", 0), Failure::Quota);
        assert_eq!(classify_failure("429 {\"status\": \"resource_exhausted\"}", 0), Failure::Quota);
    }

    #[test]
    fn image_request_matches_interactions_shape() {
        let body = image_request(b"\x89PNG", "image/png", "Extract.", "You are OCR.", true);
        assert_eq!(
            body,
            json!({
                "input": [
                    { "type": "image", "mime_type": "image/png", "data": "iVBORw==" },
                    { "type": "text", "text": "Extract." },
                ],
                "generation_config": { "max_output_tokens": 32768 },
                "response_format": { "type": "text", "mime_type": "application/json" },
                "system_instruction": "You are OCR.",
                "store": false,
            })
        );
        let plain = image_request(b"", "image/png", "p", "", false);
        assert!(plain.get("system_instruction").is_none() && plain.get("response_format").is_none());
    }

    #[test]
    fn audio_request_matches_interactions_shape() {
        assert_eq!(
            audio_request(vec![text_item("Hi.")], single_voice("Kore")),
            json!({
                "input": [{ "type": "user_input", "content": [{ "type": "text", "text": "Hi." }] }],
                "response_format": { "type": "audio" },
                "generation_config": { "speech_config": [{ "voice": "Kore" }] },
                "store": false,
            })
        );
    }

    #[test]
    fn canned_response_text_and_token_cap() {
        // Shape of a live gemini-3.5-flash-lite reply cut off by max_output_tokens.
        let resp = json!({ "status": "incomplete", "steps": [
            { "type": "thought", "signature": "Em==" },
            { "type": "model_output", "content": [{ "type": "text", "text": "[H1]Hi" }] },
            { "type": "model_output", "content": [{ "type": "text", "text": "[/H1]" }] },
        ]});
        assert_eq!(response_text(&resp), "[H1]Hi[/H1]");
        assert!(hit_token_cap(&resp));
        let done = json!({ "status": "completed", "steps": [{ "type": "thought", "signature": "Em==" }] });
        assert_eq!(response_text(&done), "");
        assert!(!hit_token_cap(&done));
    }

    #[test]
    fn canned_response_audio_takes_last_audio_block() {
        let resp = json!({ "status": "completed", "steps": [{ "type": "model_output", "content": [
            { "type": "text", "text": "ignored" },
            { "type": "audio", "mime_type": "audio/wav", "data": "AAA=" },
            { "type": "audio", "mime_type": "audio/wav", "data": "BBB=" },
        ]}]});
        assert_eq!(response_audio(&resp), Some("BBB="));
        assert_eq!(response_audio(&json!({ "status": "completed", "steps": [] })), None);
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
    #[ignore = "live Gemini call; on a Tier 1 key (10 TTS requests a minute) takes about a minute"]
    fn live_requests_over_the_rate_limit_wait_their_turn() {
        let _ = simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default());
        let client = live_client();
        let model = config::get().gemini.tts_model;
        let started = std::time::Instant::now();
        let results: Vec<anyhow::Result<Vec<u8>>> =
            std::thread::scope(|s| (0..14).map(|i| s.spawn({ let (client, model) = (&client, &model); move || client.generate_audio(&format!("Number {i}."), "Kore", model, None) })).collect::<Vec<_>>().into_iter().map(|h| h.join().unwrap()).collect());
        let failed: Vec<String> = results.iter().filter_map(|r| r.as_ref().err().map(|e| format!("{e:#}"))).collect();
        println!("14 requests in {:.0}s", started.elapsed().as_secs_f64());
        assert!(failed.is_empty(), "{failed:#?}");
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
