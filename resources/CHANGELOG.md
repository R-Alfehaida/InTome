# What's New in InTome

The newest version comes first. Each version lists the features and fixes you'll notice; small internal changes are left out. Press H to move from one version to the next.

## Unreleased

### New

- **What's New.** After InTome updates itself, it opens this list once so you can see what changed. Help > What's New opens it any time.
- **Long audiobooks.** Audio export now saves each finished piece of audio as it goes, so long books no longer run out of memory. If an export is cancelled or Gemini keeps failing, exporting the same document again continues where it stopped instead of starting over, so you don't pay for the same audio twice. Tools > Clear Cache removes saved pieces from exports you never finished.

### Fixed

- **Audio export no longer leaves silent gaps.** If a chapter's audio couldn't be made, it used to become silence with no warning. The export now stops and tells you which chapter failed.
- **Closing a tab with Ctrl+W.** NVDA no longer keeps reading a line from the document you closed. Focus now moves straight to the next open document, at your place in it, or to the table of contents when nothing else is open, and the progress slider goes back to 0 %.
- **The window title and status bar follow the active tab.** After switching or closing tabs, they named the wrong document.

## 0.0.2 (October 11, 2026)

### New

- **Select and copy the whole document.** Ctrl+A now selects every page, not just the one on screen, and Ctrl+C copies the entire document. Copy and Select All are in the Edit menu.
- **Newer Gemini models.** OCR, form labeling and Gemini voices use Google's current models and API. InTome also asks Gemini not to store your pages. Settings still on the old default models move to the new ones automatically.

### Fixed

- Saving a quote with the whole document selected now saves the current page and says so in the dialog title.
- The quote dialogs showed text with words joined across line breaks.

## 0.0.1 (October 10, 2026)

- First release of InTome, the Rust version of Prism Document Reader: an accessible document reader for PDF, EPUB, Word, PowerPoint, OpenDocument, MOBI, FB2, RTF, Markdown, HTML, text, CSV, images and archives, with OCR, text-to-speech, form filling, annotations, export and batch conversion.
