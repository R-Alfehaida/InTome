# Porting Prism to InTome (Rust)

Source: `C:\Users\khais\Documents\GitHub\Prism-Document-Reader\prism` (Python 3, wxPython, ~17.8k lines).
Target: this crate. InTome must keep every Prism feature and stay screen-reader first.
Read the source module you port in full before writing its Rust counterpart; the
Python is the spec. Prism's `CLAUDE.md` lists hard-won gotchas that still apply.

## Naming

Prism is renamed **InTome** everywhere user-visible: window titles, messages,
paths (`%APPDATA%\InTome`), the user guide. Code identifiers drop the prefix.

## Library mapping

| Prism (Python) | InTome (Rust) | Notes |
|---|---|---|
| wxPython | `winsafe` native Win32 controls | Same native controls wx used, so screen readers behave the same. No sizers: lay out with `gui::layout`. |
| raw RICHEDIT50W via ctypes | RICHEDIT50W via `CreateWindowEx` + `windows` crate | Keep every RichEdit gotcha from Prism `CLAUDE.md`. |
| blinker signals | `events::{AppEvent, emit, subscribe}` | `emit` works from any thread; subscribers run on the UI thread. |
| `wx.CallAfter` | `events::run_on_ui` | |
| configobj INI | `config` (serde + TOML) | `config::get()` snapshot, `config::update("section.key", \|s\| ...)`. |
| diskcache | `document::cache` (SQLite KV, expiry, size cap) | |
| SQLAlchemy | `rusqlite` | |
| accessible_output2 | `speech` over the `prismer` crate (prism, statically linked) | Prism picks NVDA, JAWS and other screen readers, UIA for Narrator, else a OneCore/SAPI voice. |
| comtypes / ctypes | `windows` crate | |
| google-genai | `ureq` + `serde_json` against the Gemini Interactions API (`/v1beta/interactions`) | Same prompts. Model defaults follow Google's migration table, not Prism's literals. |
| PyMuPDF | `pdfium-render` (bundled `vendor/pdfium.dll`, chromium/7881) + `lopdf` | lopdf writes what pdfium cannot (`/TU` labels, choice values). |
| Surya (local OCR model) | Windows.Media.Ocr (`windows` crate) | Built into Windows 10+, local, no download. Backend key `windows`. |
| pytesseract | `tesseract.exe` via `std::process` | |
| mammoth (DOCX to HTML) | `zip` + `quick-xml`, then `structured_text::html_parser` | |
| python-pptx, odfpy | `zip` + `quick-xml` | |
| EbookLib (read) | `epub` | |
| mobi | `mobi` | |
| lxml / inscriptis | `scraper` (html5ever), own text renderer | |
| trafilatura | `dom_smoothie` (Readability port) | |
| mistune | `pulldown-cmark` | |
| striprtf-style parsing | `rtf-parser` | |
| rarfile | `unrar` | |
| Pillow | `image` | |
| fpdf2 | `krilla` | krilla emits tagged PDF/UA, which beats fpdf2's tagging. |
| python-docx (write) | `docx-rs` | |
| EbookLib (write) | `epub-builder` | |
| ffmpeg + mutagen | `ffmpeg` CLI with an ffmetadata chapter file | ffmpeg writes chapters itself. |
| pyttsx3 / SAPI | `windows` crate `ISpVoice` | |
| OneCore (WinRT) | `windows` crate `Media::SpeechSynthesis` | |
| winsound | `PlaySound` (`windows` crate) | |

## Shared contracts (already in the scaffold; do not change without the coordinator)

- `document::{Document, Page, TocEntry, SearchResult, FormField, FieldType, MarkStyle, Capabilities, DocMetadata, OpenOptions, OpenError}` in `src/document/mod.rs`.
- `document::registry::FORMATS`: each format module exposes `pub fn open(&Path, &OpenOptions) -> Result<Box<dyn Document>, OpenError>`.
- `structured_text::elements::{ElementType, SemanticElement, ElementMeta}`.
- `events::AppEvent`: one variant per Prism signal.
- `config::Settings`: typed sections mirroring Prism's `_DEFAULTS`.
- `document::cache::{file_hash, get_cached, set_cached, clear_cache, cache_stats}`.
- `platform::win32::a11y::{set_name, set_description, notify_value_change}`.

**Text positions are char indices** (Unicode scalar values) into `Page::text`,
matching Python string indices. Only the GUI converts to RichEdit offsets.

**Documents are `Send`**: opened on a worker thread, used on the UI thread.
Hold no non-`Send` handles; extract eagerly or reopen by path.

## Conventions

- `anyhow::Result` inside modules, typed errors at module boundaries that callers branch on.
- `log` macros for diagnostics. User-facing failures go through `events::announce` and an error dialog, never a silent drop.
- Optional runtime dependencies (pandoc, antiword, tesseract, ffmpeg, Gemini key) are probed at runtime. Missing means "unavailable", never a crash.
- Comments only for a non-obvious why. Port Prism's why-comments that still apply.
- One focused unit test per non-trivial pure function (parsers, splitters, mappers), asserting literal expected values. No tests that would pass if the function returned a default.
- Keep `cargo build` warning-free in the files you own, and `cargo test --lib` green.

## Accessibility rules (spec, not style)

- Every interactive control has an accessible name: a caption, a static label created immediately before it, or `a11y::set_name`.
- Announce every meaningful state change (page turn, OCR progress, save, error) with `events::announce`.
- Everything works from the keyboard; mouse drag-and-drop is only ever additive.
- Escape closes every dialog. Enter activates the default button. Tab order follows visual order.
- The RichEdit keeps `ES_NOHIDESEL` and gets `notify_value_change` after its text is replaced.
- `scripts/uia_audit.ps1 -WindowTitle <title>` must pass for every window and dialog.
