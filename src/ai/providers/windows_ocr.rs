//! Local OCR through Windows.Media.Ocr, replacing Prism's Surya provider.
//!
//! Built into Windows 10+, offline, no model download. It recognizes the
//! languages of the user's profile that have an OCR component installed. Like
//! Surya, it ignores the prompt and returns plain text: one line per
//! recognized line, with a blank line where the vertical gap marks a new
//! paragraph or column, so structure parsing passes it through unchanged.

use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform, ColorManagementMode,
    ExifOrientationMode,
};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::System::Com::CoIncrementMTAUsage;

use crate::ai::provider::{OcrError, OcrProvider};
use crate::cancel::CancelToken;

pub struct WindowsOcrProvider {
    cancel: Option<CancelToken>,
}

/// Keeps a process MTA alive so WinRT works on worker threads that never
/// initialised COM, without changing the caller's apartment (RoInitialize
/// here would make the UI thread an MTA and break its later OLE init).
fn init_apartment() {
    static MTA: std::sync::Once = std::sync::Once::new();
    MTA.call_once(|| {
        if let Err(e) = unsafe { CoIncrementMTAUsage() } {
            log::warn!("CoIncrementMTAUsage failed: {e}");
        }
    });
}

impl WindowsOcrProvider {
    pub fn new(cancel: Option<CancelToken>) -> Self {
        Self { cancel }
    }

    /// True when a user-profile language has an OCR recognizer installed.
    pub fn is_available() -> bool {
        init_apartment();
        OcrEngine::TryCreateFromUserProfileLanguages().is_ok()
    }

    fn recognize(image: &[u8]) -> windows::core::Result<Vec<LineBox>> {
        init_apartment();
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;

        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream)?;
        writer.WriteBytes(image)?;
        writer.StoreAsync()?.join()?;
        writer.DetachStream()?;
        stream.Seek(0)?;

        let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
        // A 300 DPI page exceeds the engine's size limit on some Windows builds; downscale to fit.
        let (w, h) = (decoder.PixelWidth()?, decoder.PixelHeight()?);
        let max = OcrEngine::MaxImageDimension()?;
        let transform = BitmapTransform::new()?;
        if w.max(h) > max {
            let scale = max as f64 / w.max(h) as f64;
            transform.SetScaledWidth(((w as f64 * scale) as u32).max(1))?;
            transform.SetScaledHeight(((h as f64 * scale) as u32).max(1))?;
            transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
        }
        let bitmap = decoder
            .GetSoftwareBitmapTransformedAsync(
                BitmapPixelFormat::Bgra8,
                BitmapAlphaMode::Premultiplied,
                &transform,
                ExifOrientationMode::RespectExifOrientation,
                ColorManagementMode::DoNotColorManage,
            )?
            .join()?;

        let result = engine.RecognizeAsync(&bitmap)?.join()?;
        let mut lines = Vec::new();
        for line in result.Lines()? {
            let (mut top, mut bottom) = (f32::MAX, f32::MIN);
            for word in line.Words()? {
                let r = word.BoundingRect()?;
                top = top.min(r.Y);
                bottom = bottom.max(r.Y + r.Height);
            }
            lines.push(LineBox { text: line.Text()?.to_string(), top, bottom });
        }
        Ok(lines)
    }
}

impl OcrProvider for WindowsOcrProvider {
    fn transcribe_page(&self, image: &[u8], _prompt: &str, _mime_type: &str) -> Result<String, OcrError> {
        if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            return Err(OcrError("Operation cancelled".into()));
        }
        Self::recognize(image)
            .map(|lines| lines_to_text(&lines))
            .map_err(|e| OcrError(format!("Windows OCR transcription failed: {}", e.message())))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct LineBox {
    text: String,
    top: f32,
    bottom: f32,
}

/// Joins lines in reading order. A gap above a line wider than most of a line
/// height, or a jump back up the page (next column), starts a new paragraph.
fn lines_to_text(lines: &[LineBox]) -> String {
    let mut heights: Vec<f32> = lines.iter().map(|l| l.bottom - l.top).collect();
    heights.sort_by(f32::total_cmp);
    let line_height = heights.get(heights.len() / 2).copied().unwrap_or(0.0);
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            let gap = line.top - lines[i - 1].bottom;
            out.push_str(if gap > 0.8 * line_height || gap < -line_height { "\n\n" } else { "\n" });
        }
        out.push_str(&line.text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, top: f32) -> LineBox {
        LineBox { text: text.into(), top, bottom: top + 20.0 }
    }

    #[test]
    fn vertical_gaps_and_column_jumps_become_paragraph_breaks() {
        let lines = [line("Title", 0.0), line("First para", 40.0), line("continues", 65.0), line("Next column", 0.0), line("more", 26.0)];
        assert_eq!(lines_to_text(&lines), "Title\n\nFirst para\ncontinues\n\nNext column\nmore");
        assert_eq!(lines_to_text(&[]), "");
    }

    #[test]
    #[ignore = "needs target/ai_scratch/page.png from the OCR accuracy script"]
    fn live_windows_ocr_on_rendered_page() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ai_scratch");
        let png = std::fs::read(dir.join("page.png")).unwrap();
        assert!(WindowsOcrProvider::is_available());
        let text = WindowsOcrProvider::new(None).transcribe_page(&png, "", "image/png").unwrap();
        std::fs::write(dir.join("winocr.txt"), &text).unwrap();
        println!("{text}");
    }
}
