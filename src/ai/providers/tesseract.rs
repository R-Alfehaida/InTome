//! Tesseract OCR provider: runs `tesseract.exe` (port of prism/ai/providers/tesseract.py,
//! which went through pytesseract). The prompt is not forwarded; Tesseract
//! has no guided transcription. Output is plain text.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use crate::ai::provider::{OcrError, OcrProvider};
use crate::cancel::CancelToken;

/// Where the official Windows installer puts the binary; many users never add it to PATH.
pub const DEFAULT_WINDOWS_PATH: &str = r"C:\Program Files\Tesseract-OCR\tesseract.exe";

/// Download page offered when Tesseract is missing.
pub const DOWNLOAD_URL: &str = "https://github.com/UB-Mannheim/tesseract/wiki";

/// What the GUI announces before showing the "Tesseract Not Found" dialog.
pub const MISSING_ANNOUNCEMENT: &str =
    "Tesseract OCR is not installed or could not be found. Press Download Tesseract in the dialog to get the installer.";

pub const MISSING_DIALOG_TITLE: &str = "Tesseract Not Found";

/// Body of the "Tesseract Not Found" dialog (buttons: OK, Download Tesseract).
pub fn missing_dialog_message() -> String {
    format!(
        "Tesseract OCR is not installed or could not be found at either of the following locations:\n\n  \u{2022} System PATH\n  \u{2022} {DEFAULT_WINDOWS_PATH}\n\nInstall Tesseract to enable local (offline) OCR processing. The official Windows installer is available from the Tesseract GitHub releases page."
    )
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct TesseractOcrProvider {
    binary: PathBuf,
    cancel: Option<CancelToken>,
}

impl TesseractOcrProvider {
    pub fn new(binary: PathBuf, cancel: Option<CancelToken>) -> Self {
        Self { binary, cancel }
    }

    /// `tesseract.exe` on PATH, else at the default install location.
    pub fn find_binary() -> Option<PathBuf> {
        let on_path = std::env::var_os("PATH")
            .and_then(|p| std::env::split_paths(&p).map(|d| d.join("tesseract.exe")).find(|f| f.is_file()));
        on_path.or_else(|| Some(PathBuf::from(DEFAULT_WINDOWS_PATH)).filter(|p| p.is_file()))
    }

    pub fn is_available() -> bool {
        Self::find_binary().is_some()
    }
}

impl OcrProvider for TesseractOcrProvider {
    fn transcribe_page(&self, image: &[u8], _prompt: &str, mime_type: &str) -> Result<String, OcrError> {
        if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            return Err(OcrError("Operation cancelled".into()));
        }
        let fail = |e: String| OcrError(format!("Tesseract transcription failed: {e}"));
        let suffix = if mime_type == "image/jpeg" { ".jpg" } else { ".png" };
        // Closed before tesseract opens it; deleted when `input` drops.
        let input = tempfile::Builder::new().suffix(suffix).tempfile().map_err(|e| fail(e.to_string()))?.into_temp_path();
        std::fs::write(&input, image).map_err(|e| fail(e.to_string()))?;
        // --psm 3: fully automatic page segmentation, no orientation detection.
        // --oem 3: default engine (LSTM where available). No -l: pytesseract's default, English.
        let out = Command::new(&self.binary)
            .arg(&input)
            .args(["stdout", "--psm", "3", "--oem", "3"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| fail(e.to_string()))?;
        if !out.status.success() {
            return Err(fail(String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
