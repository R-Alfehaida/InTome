# InTome User Guide

Welcome to InTome, the accessibility-first document reader for Windows. This guide covers all of InTome's features and keyboard shortcuts.

## Getting Started

### Opening a Document

Press **Ctrl+O** to open a file. InTome supports over 15 document formats including PDF, EPUB, DOCX, PPTX, ODT, MOBI, Markdown, HTML, plain text, and images.

When you open a document, InTome announces the title and page count. If the document contains fillable form fields or needs OCR processing, you will be notified.

### Recent Files

The **File > Recent Files** submenu shows your recently opened documents for quick access.

### Multiple Tabs

You can have multiple documents open at the same time, each in its own tab. Use **Ctrl+Tab** and **Ctrl+Shift+Tab** to switch between tabs. Close the current tab with **Ctrl+W**.

When you close a tab and another document is still open, InTome moves you to that document's reading area at your place in it, and the progress slider, page count, table of contents and window title switch to it. When you close the last document, focus moves to the table of contents and the progress slider goes back to 0 %.

## The Menu Bar

InTome's menus are organized to keep related commands together and reduce the number of top-level menus you tab through:

- **File** -- Open, Recent Files, Export, Batch Convert, Close Tab, Exit
- **Edit** -- Find, Copy, Select All, View Options (a submenu with the Sidebar toggle and Element List), Preferences
- **Reference** -- bookmarks, notes, quotes, and Export Annotations
- **Text-to-Speech** -- Read, Pause/Resume, Stop Reading, and Voice Profiles
- **Tools** -- Run OCR, Cancel OCR, Label / Scan Form Fields, Fill Form, Clear Cache
- **Help** -- User Guide, What's New, Keyboard Shortcuts, Check for Updates, About

Most menu items also have a keyboard shortcut, listed in the sections below.

## Page Navigation

| Key | Action |
|-----|--------|
| Page Down | Next page |
| Page Up | Previous page |
| Ctrl+Home | First page |
| Ctrl+End | Last page |

The status bar at the bottom shows the current page number and total page count.

## Selecting and Copying

| Key | Action |
|-----|--------|
| Ctrl+A | Select the entire document |
| Ctrl+C or Ctrl+Insert | Copy the selection |

InTome shows one page at a time, but **Ctrl+A** selects every page, not just the one on screen. InTome says "Selected entire document" with the page count, and the current page is highlighted. Press **Ctrl+C** to copy the whole document to the clipboard. If some pages of a scanned document have no text yet because OCR hasn't run on them, InTome tells you how many were left out.

Moving the cursor, turning the page or making a new selection ends the whole-document selection. Shift with the arrow keys still selects text on the current page as usual, and Ctrl+C copies just that.

## Focus Areas

Press **F6** or **Shift+F6** to cycle between three focus areas:

1. **Table of Contents** (sidebar) -- shows the document's structure
2. **Content** -- the main reading area
3. **Progress bar** -- shows your position in the document

Toggle the sidebar on or off with **F9**.

### Reading Progress

The progress slider shows how far through the whole document your cursor is, as a percentage of its text, and the number next to it says the same. It moves as you read, not only when the page turns. On the slider, the Left and Right arrow keys move 1 % back or forward, Home goes to the start of the document and End to the end, and the cursor follows to the start of the line at that point.

## Structured Navigation

When focused in the content area, use these single-key shortcuts to jump between semantic elements:

| Key | Action |
|-----|--------|
| H / Shift+H | Next / previous heading (any level) |
| 1-6 / Shift+1-6 | Next / previous heading at level 1-6 |
| K / Shift+K | Next / previous link |
| T / Shift+T | Next / previous table |
| L / Shift+L | Next / previous list |
| Q / Shift+Q | Next / previous quote |
| G / Shift+G | Next / previous figure |

### Section Navigation

| Key | Action |
|-----|--------|
| Alt+Page Down | Next section (TOC entry) |
| Alt+Page Up | Previous section |
| Alt+Home | Start of current section |
| Alt+End | End of current section |

### Element List

Press **Ctrl+Shift+E** to open the Element List dialog. This shows all elements of a chosen type (headings, links, tables, etc.) across the entire document. You can filter the list and press Enter to navigate to any element.

## Search

Press **Ctrl+F** to open the Find bar. Type your search term and use:

| Key | Action |
|-----|--------|
| F3 | Find next match |
| Shift+F3 | Find previous match |
| Escape | Close find bar |

The find bar shows the current match number and total count (e.g., "3 of 17"). Options include match case and wrap around.

## Text-to-Speech (TTS)

InTome includes a built-in TTS system with three engines: SAPI 5, OneCore (Windows 10+), and Gemini TTS (requires API key).

### Reading Controls

| Key | Action |
|-----|--------|
| F5 | Start reading from cursor |
| Shift+F5 | Pause / Resume |
| F7 | Stop reading |
| Alt+Right | Skip forward (next paragraph) |
| Alt+Left | Skip backward (previous paragraph) |

### Voice Profiles

Press **Ctrl+Shift+V** to open the Voice Profiles dialog. InTome includes three built-in profiles:

- **Human-like** -- natural listening using Gemini TTS
- **Deep Reading** -- slower pace for study using SAPI 5
- **Express** -- fast scanning using OneCore

Each profile stores engine selection, voice, rate, pitch, volume, and pause settings. You can also create custom profiles.

## Annotations

InTome lets you bookmark pages, add notes, and save quotes. These commands live in the **Reference** menu. Annotations are saved per-document and persist across sessions.

### Creating Annotations

| Key | Action |
|-----|--------|
| Ctrl+B | Quick bookmark at current position |
| Ctrl+Shift+B | Named bookmark (prompts for name) |
| Ctrl+M | Add a note at cursor position |
| Ctrl+Shift+H | Save selected text as a quote |

A quote belongs to one page. If you press Ctrl+Shift+H after selecting the entire document with Ctrl+A, InTome saves the current page as the quote, and the dialog's title says so.

### Viewing Annotations

| Key | Action |
|-----|--------|
| Ctrl+Shift+F2 | List all bookmarks for this document |
| Ctrl+Shift+M | List all notes for this document |
| Ctrl+Shift+Q | List all quotes for this document |

In the bookmark and note list dialogs, you can filter, navigate to an annotation (Go To), edit notes, or delete entries. The quote list dialog adds a View button (and Enter) to read a saved quote's full text and tags.

### Exporting Annotations

Use **Reference > Export Annotations** to save all bookmarks, notes, and quotes for the current document to a text or HTML file.

## Form Filling

When you open a PDF with fillable form fields, InTome detects them and announces the count. Use **Ctrl+Shift+F** to open the Form Filling dialog.

The dialog presents all form fields as accessible controls grouped by category:

- Text fields appear as editable text boxes
- Checkboxes appear as checkboxes
- Radio buttons and combo boxes appear as dropdown choices
- List boxes appear as list controls

A status line shows which field you are on (e.g., "Field 3 of 24 -- Personal Information").

### AI-Assisted Field Labels

Many PDF forms have cryptic internal field names like "Textbox5". Press **Ctrl+Shift+L** to use AI (Gemini) to analyze the form and generate descriptive labels for each field. When you open the form dialog with unlabeled fields, InTome will offer to run labeling first.

Generated labels are remembered. Once a form has been labeled, the labels are reused the next time you open it, so labeling normally runs only once per form. If a field already has a label set by the form author, that label is used and the field is left as is during automatic labeling.

Pressing **Ctrl+Shift+L** again is an explicit override: it re-analyzes the form and regenerates labels for every field, replacing the remembered labels, including for fields the author had already labeled. Use it when the existing labels are wrong or unhelpful. Author-provided labels return for their fields the next time you reopen the form, while regenerated labels for author-unlabeled fields are kept. Label memory follows the document cache, so it is cleared if you turn the cache off or clear it from Preferences.

### Saving

- **Save to PDF** -- saves values back to the original file
- **Save As** -- saves to a new PDF file
- **Ctrl+S** -- quick save to original file

## OCR (Optical Character Recognition)

Scanned PDFs and image files have no selectable text. InTome uses Google's Gemini AI to extract text from these documents.

### Automatic OCR

When you open a document that needs OCR and your Gemini API key is configured, InTome automatically starts processing.

### Manual OCR

Press **Ctrl+Shift+O** to manually run OCR on the current document. Progress is shown in the status bar and announced to your screen reader.

### Manga / Comic Mode

InTome includes a specialized mode for manga, comics, and graphic novels that describes panels, transcribes dialogue, and notes sound effects.

### Configuration

Open **Preferences > OCR** to adjust:

- Render DPI (default 300)
- Manga mode toggle
- Custom instructions for the AI
- System prompt override for advanced users

## Export

Press **Ctrl+Shift+S** to export the current document to another format. Available formats:

- **Text** -- plain text
- **HTML** -- semantic HTML5 with ARIA landmarks
- **Markdown** -- standard Markdown
- **PDF** -- tagged, accessible PDF
- **EPUB** -- EPUB 3 with accessibility metadata
- **DOCX** -- Word document with proper heading styles
- **DAISY (text)** -- DAISY 3 DTBook format
- **DAISY (audio)** -- DAISY 3 with synchronized audio
- **M4B Audiobook** -- single file with chapter markers

Audio formats (DAISY audio and M4B) require a configured Gemini API key for text-to-speech.

### Long audio exports

Audio is made a few minutes at a time, and each finished piece is saved on disk as soon as it is ready, so even a very long book doesn't fill your computer's memory. The progress bar counts these pieces.

If an audio export is cancelled, or Gemini keeps failing on a piece, the pieces already made stay saved. When an export fails, InTome tells you which chapter it stopped in. Export the same document again with the same voice and it continues from where it stopped instead of starting over, so you don't pay for the same audio twice. Once an export finishes, its saved pieces are deleted. Pieces from an export you never finish are deleted after 30 days, or straight away with **Tools > Clear Cache**.

M4B export needs ffmpeg for audiobooks of any length. Without ffmpeg, InTome saves a WAV file instead, which can hold at most about 24 hours of audio.

## Batch Conversion

Press **Ctrl+Shift+P** to open the Batch Convert dialog. Add multiple files, choose an output format and destination folder, then start conversion. Features include:

- Add, remove, and reorder files (Alt+Up/Down)
- Drag and drop files from Explorer
- Parallel processing (configurable, 1-5 concurrent)
- Per-file status tracking and log output
- Cancel support

## Preferences

Press **Ctrl+Comma** to open the Preferences dialog. Settings are organized into pages:

- **General** -- continuous reading, recent files limit, remember reading position
- **Display** -- font, font size, OpenDyslexic toggle, line spacing
- **Document** -- PDF reading mode, cache settings
- **Gemini Models** -- model selection, mode, concurrency, timeout
- **OCR** -- render DPI, manga mode, custom prompts
- **AI Providers** -- OCR backend, Gemini cloud API key, local models (Surya, Tesseract)
- **TTS** -- voice profile, reading scope, start-from setting
- **Export** -- default format, output directory, batch concurrency
- **Privacy** -- cache info and clear, annotation management

### Setting Your Gemini API Key

To use OCR, AI form labeling, Gemini TTS, and audio exports:

1. Open Preferences (Ctrl+,)
2. Go to the AI Providers page
3. Enter your key in the Gemini API Key field; it is saved when the field loses focus or when you press OK
4. The key is stored encrypted on your machine using Windows DPAPI

### Gemini Models

The Gemini Models page lists the models your key can use. By default InTome uses Gemini 3.5 Flash-Lite for fast OCR and as the fallback model, Gemini 3.8 Flash for accurate OCR, and Gemini 3.8 Flash-Lite TTS for speech. When InTome starts, it moves any model setting that holds an earlier default (Gemini 3.1 Flash-Lite, Gemini 3.5 Flash or Gemini 3.1 Flash TTS Preview) to its current replacement. Any other model you picked stays as it is.

InTome asks Gemini not to store your requests, so pages and text you send for OCR, form labeling or speech are not kept on Google's servers for later retrieval.

## Accessibility

InTome is designed from the ground up for screen reader users:

- Every control has an accessible name
- All actions are keyboard-accessible -- no mouse required
- Status changes are announced via your screen reader (NVDA, JAWS, Narrator)
- Logical tab order in all dialogs
- Escape closes any dialog
- F6 cycles focus between major areas

## Troubleshooting

### Document won't open

- Check that the file format is supported
- For password-protected PDFs, you will be prompted for the password
- Some formats require optional libraries (the error message will tell you which)

### OCR not working

- Ensure google-genai is installed: `pip install google-genai`
- Set your API key in Preferences > AI Providers
- Check your internet connection

### TTS not speaking

- Open Preferences > TTS and check the active profile
- Ensure the selected TTS engine is available on your system
- SAPI 5 is available on all Windows systems as a fallback

### Cache issues

- Use Tools > Clear Cache to reset all cached data
- Cache is stored in %APPDATA%/InTome/cache/

## Updates

Choose **Help > Check for Updates** to see whether a newer version of InTome is available. InTome checks the project's GitHub releases over HTTPS -- no account, sign-in, or token is required. If a newer version exists, InTome asks whether you want to install it. If you agree, it downloads the new version, verifies its integrity, replaces its own files and restarts. Your settings, bookmarks, notes and reading positions are kept. If you are already running the latest version, InTome simply tells you so.

The first time InTome starts after an update, it opens **What's New**, a list of the new features and fixes in each version, newest first, with a heading per version. Open it again any time with **Help > What's New**.

InTome must be able to write to its own folder to update itself. If it can't, it tells you where it saved the download so you can extract it over the InTome folder yourself.

## Keyboard Shortcuts Reference

Press **Ctrl+/** to view the complete keyboard shortcuts list at any time. The shortcuts dialog includes a filter field to quickly find the shortcut you need.

For the full list, use the Keyboard Shortcuts dialog within InTome.
