# InTome

InTome is an accessibility-first document reader for Windows, built for blind and visually impaired readers. It is the Rust port of Prism-Document-Reader. It reads PDF, EPUB, DOCX, PPTX, ODT/ODP, MOBI/AZW, FB2, RTF, Markdown, HTML, text, CSV, images, and ZIP/RAR archives. It also has OCR, text-to-speech, form filling, annotations, export and batch conversion.

The full feature and shortcut guide is in [resources/user_guide.md](resources/user_guide.md). Press F1 in the app to open it.

## Build and run

You need Windows 10 or 11, Rust (stable, MSVC toolchain), and the Visual Studio C++ build tools.

```
cargo build --release
target\release\intome.exe [file]
```

The build copies `pdfium.dll` (the PDF engine) and `nvdaControllerClient64.dll` (NVDA speech) from `vendor/` next to the exe. Keep them together when you move the program.

Settings, logs, the document cache and the annotations database are stored in `%APPDATA%\InTome\`.

## Optional tools

InTome works without these. When a tool is missing, the feature that needs it is unavailable and the rest of the app still works.

| Tool | Enables |
|---|---|
| Gemini API key (Preferences > AI Providers) | Cloud OCR, AI form labels, Gemini voices, audio export |
| Tesseract (`tesseract.exe` on PATH) | A third local OCR backend. Windows OCR is built in. |
| ffmpeg on PATH | M4B audiobook export with chapters |
| pandoc on PATH | LaTeX, DocBook and Jupyter notebooks |
| antiword on PATH | Legacy `.doc` files |

## Screen readers

InTome speaks through NVDA directly and through JAWS. Narrator and other screen readers get UI Automation notifications. Every control is a native Win32 control with an accessible name.

`scripts/uia_audit.ps1 -WindowTitle "<title>"` checks a window the way NVDA and JAWS see it. It fails if any focusable control has no name.

## Development

- `cargo test` runs the unit tests.
- `python scripts/parity/compare.py <file>` compares InTome's text extraction against the original Prism. It needs a checkout of Prism-Document-Reader with its virtual environment; set `PRISM_ROOT` if it is not at the default path.
- [docs/PORTING.md](docs/PORTING.md) maps each Python library to its Rust replacement and lists the shared contracts.
- [docs/decisions.tsv](docs/decisions.tsv) logs every porting decision and how it was verified.
