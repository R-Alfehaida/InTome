//! Background OCR, form labelling and flat-form discovery with progress
//! events (port of prism/ai/processing.py).
//!
//! Jobs run on a background thread that fans pages out to at most
//! `gemini.max_concurrent_pages` workers. Progress goes out as [`AppEvent`]s
//! and announcements; caller callbacks run on the UI thread via
//! [`events::run_on_ui`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use crate::ai::client::GeminiClient;
use crate::ai::ocr::{self, FieldLabel, FlatField};
use crate::ai::provider::{GeminiOcrProvider, get_ocr_provider};
use crate::ai::providers::tesseract::TesseractOcrProvider;
use crate::ai::providers::windows_ocr::WindowsOcrProvider;
use crate::ai::structure::ParsedOcr;
use crate::cancel::CancelToken;
use crate::config::{self, GeminiMode, OcrBackend};
use crate::document::FormField;
use crate::events::{self, AppEvent, announce};

/// Called with `(page_index, result)` after each OCR'd page.
pub type PageCompleteCallback = Box<dyn Fn(usize, ParsedOcr) + Send>;
/// Called with `(page_index, error_message)` for a page that failed.
pub type PageErrorCallback = Box<dyn Fn(usize, String) + Send>;
/// Called with `(pages_completed, elapsed_seconds)` when a PDF job ends.
pub type PdfCompleteCallback = Box<dyn Fn(usize, f64) + Send>;
pub type ImageCompleteCallback = Box<dyn Fn(ParsedOcr) + Send>;
pub type ErrorCallback = Box<dyn Fn(String) + Send>;
pub type LabelsCompleteCallback = Box<dyn Fn(BTreeMap<usize, Vec<FieldLabel>>) + Send>;
pub type FlatFieldsCompleteCallback = Box<dyn Fn(BTreeMap<usize, Vec<FlatField>>) + Send>;

/// What a start request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    Started,
    /// Another job is running; the request was ignored.
    AlreadyRunning,
    /// Tesseract is (or would be) the backend but is not installed. The GUI
    /// announces `tesseract::MISSING_ANNOUNCEMENT` and shows the
    /// "Tesseract Not Found" dialog with a Download Tesseract button.
    TesseractMissing,
}

pub struct ProcessingManager {
    cancel: CancelToken,
    busy: Arc<AtomicBool>,
}

pub static PROCESSING_MANAGER: LazyLock<ProcessingManager> =
    LazyLock::new(|| ProcessingManager { cancel: CancelToken::new(), busy: Arc::new(AtomicBool::new(false)) });

/// Clears the busy flag when the job thread ends, even by panic.
struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// A multi-call callback shared by worker threads and invoked on the UI thread.
type Shared<T> = Arc<Mutex<T>>;

fn share<T>(cb: Option<T>) -> Option<Shared<T>> {
    cb.map(|c| Arc::new(Mutex::new(c)))
}

struct RunEnd {
    /// Cancellation stopped the run before every page was started.
    cancelled: bool,
    error: Option<anyhow::Error>,
}

/// Runs `f` for each page on up to `workers` threads. Stops taking pages
/// once cancelled or after the first error.
fn for_each_page(pages: &[usize], workers: usize, cancel: &CancelToken, f: impl Fn(usize) -> anyhow::Result<()> + Sync) -> RunEnd {
    let next = AtomicUsize::new(0);
    let cancelled = AtomicBool::new(false);
    let error = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..workers.clamp(1, pages.len().max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= pages.len() || error.lock().unwrap().is_some() {
                        break;
                    }
                    if cancel.is_cancelled() {
                        cancelled.store(true, Ordering::SeqCst);
                        break;
                    }
                    if let Err(e) = f(pages[i]) {
                        error.lock().unwrap().get_or_insert(e);
                        break;
                    }
                }
            });
        }
    });
    RunEnd { cancelled: cancelled.into_inner(), error: error.into_inner().unwrap() }
}

fn workers() -> usize {
    config::get().gemini.max_concurrent_pages as usize
}

impl ProcessingManager {
    pub fn is_processing(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Requests cancellation of the current job; takes effect between pages
    /// and between Gemini retries.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Claims the manager for a new job, or reports why it cannot start.
    fn claim(&self, check_tesseract: bool) -> Result<BusyGuard, StartOutcome> {
        if self.is_processing() {
            log::warn!("OCR already in progress; ignoring new request");
            return Err(StartOutcome::AlreadyRunning);
        }
        // Tesseract is the only backend whose absence the GUI warns about up
        // front; Gemini and Windows OCR report through the normal error path.
        if check_tesseract {
            let backend = config::get().ocr.backend;
            let tesseract_would_be_used = backend == OcrBackend::Tesseract
                || (backend == OcrBackend::Auto && !GeminiOcrProvider::is_available() && !WindowsOcrProvider::is_available());
            if tesseract_would_be_used && !TesseractOcrProvider::is_available() {
                return Err(StartOutcome::TesseractMissing);
            }
        }
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err(StartOutcome::AlreadyRunning);
        }
        self.cancel.reset();
        Ok(BusyGuard(self.busy.clone()))
    }

    fn spawn(name: &str, job: impl FnOnce() + Send + 'static) {
        if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(job) {
            log::error!("failed to start {name}: {e}");
        }
    }

    /// OCRs `page_indices` (0-based) of a PDF in the background.
    pub fn process_pdf(
        &self,
        pdf_path: PathBuf,
        page_indices: Vec<usize>,
        filename: String,
        mode: GeminiMode,
        on_page_complete: Option<PageCompleteCallback>,
        on_error: Option<PageErrorCallback>,
        on_complete: Option<PdfCompleteCallback>,
    ) -> StartOutcome {
        let guard = match self.claim(true) {
            Ok(g) => g,
            Err(outcome) => return outcome,
        };
        let total = page_indices.len();
        events::emit(AppEvent::OcrStarted { filename: filename.clone(), page_count: total });
        announce(format!("Processing {filename}, {total} pages. This may take a moment."));

        let cancel = self.cancel.clone();
        let (on_page_complete, on_error) = (share(on_page_complete), share(on_error));
        Self::spawn("ocr-worker", move || {
            let _guard = guard;
            let start = Instant::now();
            // Resolve the provider once for the batch; failure aborts every page.
            let provider = match get_ocr_provider(config::get().ocr.backend, mode, Some(cancel.clone())) {
                Ok(p) => p,
                Err(e) => {
                    events::emit(AppEvent::OcrError { filename, page_num: 0, error_message: e.to_string() });
                    announce(format!("OCR error: {e}"));
                    return;
                }
            };

            let completed = AtomicUsize::new(0);
            let run = for_each_page(&page_indices, workers(), &cancel, |page_index| {
                match ocr::ocr_pdf_page(&pdf_path, page_index, provider.as_ref()) {
                    Ok(result) => {
                        let n = completed.fetch_add(1, Ordering::SeqCst) + 1;
                        events::emit(AppEvent::OcrPageCompleted { filename: filename.clone(), page_num: page_index + 1, total_pages: total });
                        if let Some(cb) = on_page_complete.clone() {
                            events::run_on_ui(move || (cb.lock().unwrap())(page_index, result));
                        }
                        if n.is_multiple_of(5) || n == total {
                            announce(format!("Processed {n} of {total} pages"));
                        }
                    }
                    Err(e) => {
                        let error_msg = format!("Error on page {}: {e}", page_index + 1);
                        log::error!("{error_msg}");
                        events::emit(AppEvent::OcrError { filename: filename.clone(), page_num: page_index + 1, error_message: e.to_string() });
                        announce(format!("{error_msg}. Continuing with remaining pages."));
                        if let Some(cb) = on_error.clone() {
                            events::run_on_ui(move || (cb.lock().unwrap())(page_index, e.0));
                        }
                    }
                }
                Ok(())
            });
            if run.cancelled {
                announce("OCR processing cancelled.");
            }

            let (completed, elapsed) = (completed.into_inner(), start.elapsed().as_secs_f64());
            events::emit(AppEvent::OcrCompleted { filename, total_pages: completed, elapsed_seconds: elapsed });
            announce(format!("OCR complete. {completed} pages processed in {elapsed:.1} seconds."));
            if let Some(cb) = on_complete {
                events::run_on_ui(move || cb(completed, elapsed));
            }
        });
        StartOutcome::Started
    }

    /// OCRs a standalone image file in the background.
    pub fn process_image(
        &self,
        image_path: PathBuf,
        filename: String,
        mode: GeminiMode,
        on_complete: Option<ImageCompleteCallback>,
        on_error: Option<ErrorCallback>,
    ) -> StartOutcome {
        let guard = match self.claim(true) {
            Ok(g) => g,
            Err(outcome) => return outcome,
        };
        events::emit(AppEvent::OcrStarted { filename: filename.clone(), page_count: 1 });
        announce(format!("Processing image {filename}. This may take a moment."));

        let cancel = self.cancel.clone();
        Self::spawn("ocr-image-worker", move || {
            let _guard = guard;
            let start = Instant::now();
            let result = get_ocr_provider(config::get().ocr.backend, mode, Some(cancel))
                .and_then(|provider| ocr::ocr_image_file(&image_path, provider.as_ref()));
            match result {
                Ok(result) => {
                    let elapsed = start.elapsed().as_secs_f64();
                    events::emit(AppEvent::OcrCompleted { filename, total_pages: 1, elapsed_seconds: elapsed });
                    announce(format!("Image OCR complete in {elapsed:.1} seconds."));
                    if let Some(cb) = on_complete {
                        events::run_on_ui(move || cb(result));
                    }
                }
                Err(e) => {
                    events::emit(AppEvent::OcrError { filename, page_num: 1, error_message: e.to_string() });
                    announce(format!("Image OCR error: {e}"));
                    if let Some(cb) = on_error {
                        events::run_on_ui(move || cb(e.0));
                    }
                }
            }
        });
        StartOutcome::Started
    }

    /// Labels form fields with Gemini in the background. `force` regenerates
    /// labels instead of reading the cache (the explicit "Label Form Fields" command).
    pub fn label_pdf_form_fields(
        &self,
        pdf_path: PathBuf,
        fields_by_page: BTreeMap<usize, Vec<FormField>>,
        filename: String,
        mode: GeminiMode,
        force: bool,
        on_complete: Option<LabelsCompleteCallback>,
        on_error: Option<ErrorCallback>,
    ) -> StartOutcome {
        let guard = match self.claim(false) {
            Ok(g) => g,
            Err(outcome) => return outcome,
        };
        announce(format!("Analyzing form fields in {filename}."));
        let cancel = self.cancel.clone();
        Self::spawn("form-label-worker", move || {
            let _guard = guard;
            let pages: Vec<usize> = fields_by_page.keys().copied().collect();
            let results = Mutex::new(BTreeMap::new());
            let outcome = GeminiClient::new().and_then(|client| {
                let run = for_each_page(&pages, workers(), &cancel, |page_index| {
                    let labels = ocr::label_form_fields(&pdf_path, page_index, &fields_by_page[&page_index], &client, mode, Some(&cancel), force)?;
                    results.lock().unwrap().insert(page_index, labels);
                    Ok(())
                });
                run.error.map_or(Ok(()), Err)
            });
            finish(outcome, results.into_inner().unwrap(), "Form field analysis", on_complete, on_error);
        });
        StartOutcome::Started
    }

    /// Discovers fill-in blanks on flat (non-AcroForm) pages with Gemini
    /// vision in the background; each page's fields are cached.
    pub fn discover_flat_form_fields(
        &self,
        pdf_path: PathBuf,
        page_indices: Vec<usize>,
        filename: String,
        mode: GeminiMode,
        force: bool,
        on_complete: Option<FlatFieldsCompleteCallback>,
        on_error: Option<ErrorCallback>,
    ) -> StartOutcome {
        let guard = match self.claim(false) {
            Ok(g) => g,
            Err(outcome) => return outcome,
        };
        announce(format!("Scanning {filename} for form blanks."));
        let cancel = self.cancel.clone();
        Self::spawn("flat-form-discover-worker", move || {
            let _guard = guard;
            let results = Mutex::new(BTreeMap::new());
            let outcome = GeminiClient::new().and_then(|client| {
                let run = for_each_page(&page_indices, workers(), &cancel, |page_index| {
                    let fields = ocr::discover_flat_form_fields(&pdf_path, page_index, &client, mode, Some(&cancel), force)?;
                    results.lock().unwrap().insert(page_index, fields);
                    Ok(())
                });
                run.error.map_or(Ok(()), Err)
            });
            finish(outcome, results.into_inner().unwrap(), "Form blank scan", on_complete, on_error);
        });
        StartOutcome::Started
    }
}

/// Announces the end of a Gemini form job (`"{what} complete."` or
/// `"{what} error: ..."`) and hands the result to the matching callback.
fn finish<T: Send + 'static>(
    outcome: anyhow::Result<()>,
    results: T,
    what: &str,
    on_complete: Option<Box<dyn Fn(T) + Send>>,
    on_error: Option<ErrorCallback>,
) {
    match outcome {
        Ok(()) => {
            announce(format!("{what} complete."));
            if let Some(cb) = on_complete {
                events::run_on_ui(move || cb(results));
            }
        }
        Err(e) => {
            announce(format!("{what} error: {e}"));
            if let Some(cb) = on_error {
                let msg = e.to_string();
                events::run_on_ui(move || cb(msg));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_run_once_each_and_stop_on_error_or_cancel() {
        let pages: Vec<usize> = (0..20).collect();
        let seen = Mutex::new(Vec::new());
        let run = for_each_page(&pages, 4, &CancelToken::new(), |p| {
            seen.lock().unwrap().push(p);
            Ok(())
        });
        let mut seen = seen.into_inner().unwrap();
        seen.sort();
        assert_eq!((seen, run.cancelled, run.error.is_none()), (pages.clone(), false, true));

        let seen = Mutex::new(Vec::new());
        let run = for_each_page(&pages, 1, &CancelToken::new(), |p| {
            seen.lock().unwrap().push(p);
            if p == 2 { anyhow::bail!("render failed") } else { Ok(()) }
        });
        assert_eq!(seen.into_inner().unwrap(), vec![0, 1, 2]);
        assert_eq!(run.error.unwrap().to_string(), "render failed");

        let cancel = CancelToken::new();
        let run = for_each_page(&pages, 1, &cancel, |p| {
            if p == 3 {
                cancel.cancel();
            }
            Ok(())
        });
        assert!(run.cancelled && run.error.is_none());
    }
}
