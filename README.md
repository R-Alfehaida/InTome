# InTome

InTome is an accessibility-first document reader for Windows, built for blind and visually impaired readers. It is the Rust port of Prism-Document-Reader. It reads PDF, EPUB, DOCX, PPTX, ODT/ODP, MOBI/AZW, FB2, RTF, Markdown, HTML, text, CSV, images, and ZIP/RAR archives. It also has OCR, text-to-speech, form filling, annotations, export and batch conversion.

The full feature and shortcut guide is in [resources/user_guide.md](resources/user_guide.md). Press F1 in the app to open it.

## Build and run

You need Windows 10 or 11, Rust (stable, MSVC toolchain), the Visual Studio 2022 C++ build tools (17.11 or newer) with the "C++ ATL" component, and CMake 4.2 or newer. The build compiles the prism speech library from source (through the `prismer` crate) and links it into the exe. Prism is C23, and CMake only passes MSVC its C23 flag from 4.2 on; earlier versions fail with "requires the language dialect C23". If an older CMake comes first on PATH (Strawberry Perl ships one), set `CMAKE` to the newer `cmake.exe`.

```
cargo build --release
target\release\intome.exe [file]
```

The build copies `pdfium.dll` (the PDF engine) from `vendor/` next to the exe. Keep them together when you move the program. The C++ runtime is linked statically, so no Visual C++ redistributable is needed.

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

InTome speaks and brailles through [prism](https://github.com/ethindp/prism) (the `prismer` crate). Prism uses a running screen reader (NVDA, JAWS, ZDSR, Narrator through UI Automation and others) and falls back to a Windows voice (OneCore, then SAPI) when none is running. Every control is a native Win32 control with an accessible name.

`scripts/uia_audit.ps1 -WindowTitle "<title>"` checks a window the way NVDA and JAWS see it. It fails if any focusable control has no name.

## Development

- `cargo test` runs the unit tests.
- `python scripts/parity/compare.py <file>` compares InTome's text extraction against the original Prism. It needs a checkout of Prism-Document-Reader with its virtual environment; set `PRISM_ROOT` if it is not at the default path.
- [docs/PORTING.md](docs/PORTING.md) maps each Python library to its Rust replacement and lists the shared contracts.
- [docs/decisions.tsv](docs/decisions.tsv) logs every porting decision and how it was verified.
