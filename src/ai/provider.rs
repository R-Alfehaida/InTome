//! Provider-agnostic OCR abstraction (port of prism/ai/provider.py).
//!
//! Backends: Gemini (cloud, defined here), Windows.Media.Ocr and Tesseract
//! (local, in [`crate::ai::providers`]). `auto` prefers the cloud provider,
//! then Windows OCR, then Tesseract. [`get_ocr_provider`] is the only
//! construction path: it owns availability and unknown-backend errors.

use crate::ai::client::GeminiClient;
use crate::ai::providers::tesseract::TesseractOcrProvider;
use crate::ai::providers::windows_ocr::WindowsOcrProvider;
use crate::cancel::CancelToken;
use crate::config::{self, GeminiMode, OcrBackend};

/// An OCR call failed after any internal retries.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct OcrError(pub String);

/// An OCR backend. Implementations are called from background worker threads,
/// possibly several at once.
pub trait OcrProvider: Send + Sync {
    /// Transcribes one page image. The result may carry `[TAG]` markup for
    /// [`crate::ai::structure::parse_ocr_response`]; local backends return plain text.
    fn transcribe_page(&self, image: &[u8], prompt: &str, mime_type: &str) -> Result<String, OcrError>;
}

/// OCR via Gemini: the prompt goes in as the system instruction.
pub struct GeminiOcrProvider {
    client: GeminiClient,
    mode: GeminiMode,
    cancel: Option<CancelToken>,
}

impl GeminiOcrProvider {
    pub fn is_available() -> bool {
        GeminiClient::is_available()
    }
}

impl OcrProvider for GeminiOcrProvider {
    fn transcribe_page(&self, image: &[u8], prompt: &str, mime_type: &str) -> Result<String, OcrError> {
        self.client
            .generate_with_image(image, mime_type, "Extract all text from this page image.", prompt, self.mode, self.cancel.as_ref())
            .map_err(|e| OcrError(e.to_string()))
    }
}

/// The backend `auto` resolves to right now, or `None` when nothing is available.
pub fn resolve_backend(backend: OcrBackend) -> Option<OcrBackend> {
    match backend {
        OcrBackend::Auto => [
            (OcrBackend::Gemini, GeminiOcrProvider::is_available as fn() -> bool),
            (OcrBackend::Windows, WindowsOcrProvider::is_available),
            (OcrBackend::Tesseract, TesseractOcrProvider::is_available),
        ]
        .into_iter()
        .find_map(|(b, available)| available().then_some(b)),
        other => Some(other),
    }
}

/// Creates the provider for `backend`. `mode` only affects Gemini.
pub fn get_ocr_provider(backend: OcrBackend, mode: GeminiMode, cancel: Option<CancelToken>) -> Result<Box<dyn OcrProvider>, OcrError> {
    let err = |m: &str| Err(OcrError(m.to_string()));
    match resolve_backend(backend) {
        None => err(
            "No OCR backend is available. Configure a Gemini API key in Preferences > AI Providers, or install a Windows OCR language pack or Tesseract.",
        ),
        Some(OcrBackend::Gemini) => match GeminiClient::new() {
            Ok(client) => Ok(Box::new(GeminiOcrProvider { client, mode, cancel })),
            Err(_) => err("Gemini OCR backend is not available. Configure a Gemini API key in Preferences > AI Providers."),
        },
        Some(OcrBackend::Windows) if WindowsOcrProvider::is_available() => Ok(Box::new(WindowsOcrProvider::new(cancel))),
        Some(OcrBackend::Windows) => err(
            "Windows OCR is not available. Add a language with optical character recognition support in Windows Settings > Time & language > Language & region.",
        ),
        Some(OcrBackend::Tesseract) => match TesseractOcrProvider::find_binary() {
            Some(binary) => Ok(Box::new(TesseractOcrProvider::new(binary, cancel))),
            None => err(
                "Tesseract OCR is not installed or could not be found. Install the Tesseract binary, or download the Windows installer from https://github.com/UB-Mannheim/tesseract/wiki",
            ),
        },
        Some(OcrBackend::Auto) => unreachable!("resolve_backend never returns Auto"),
    }
}

/// True if an OCR provider can be resolved for `backend` (default: the
/// configured backend). Gate OCR UI on this, never on Gemini availability alone.
pub fn is_ocr_available(backend: Option<OcrBackend>) -> bool {
    let backend = backend.unwrap_or_else(|| config::get().ocr.backend);
    get_ocr_provider(backend, GeminiMode::default(), None).is_ok()
}
