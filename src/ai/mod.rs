//! AI integration: OCR (Gemini cloud, Windows OCR and Tesseract local),
//! structure parsing, form-field labelling, and Gemini TTS audio.
pub mod client;
pub mod ocr;
pub mod processing;
pub mod prompts;
pub mod provider;
/// Local OCR backends.
pub mod providers {
    pub mod tesseract;
    pub mod windows_ocr;
}
pub mod structure;
