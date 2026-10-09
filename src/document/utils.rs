//! Shared helpers for format handlers (port of prism/document/utils.py), plus
//! the small pieces the text-like formats share: file decoding, splitting
//! parsed text into pages with its elements, and a plain paged [`Document`].

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use super::{Capabilities, DocMetadata, Document, OpenOptions, Page, TocEntry};
use crate::constants::DEFAULT_PAGE_SIZE;
use crate::structured_text::elements::SemanticElement;

/// `CREATE_NO_WINDOW`: keeps console tools from flashing a window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Splits `text` at paragraph boundaries into pages of about `page_size` chars.
///
/// Prefers a blank line, then a newline, then a space inside the window, and
/// only hard-breaks when none exists. Pages concatenate back to `text`.
pub fn paginate_text(text: &str, page_size: usize) -> Vec<String> {
    let page_size = page_size.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut pages = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let rest = &chars[start..];
        if rest.len() <= page_size {
            pages.push(rest.iter().collect());
            break;
        }
        let chunk = &rest[..page_size];
        let cut = chunk
            .windows(2)
            .rposition(|w| w == ['\n', '\n'])
            .filter(|&p| p > 0)
            .map(|p| p + 2)
            .or_else(|| chunk.iter().rposition(|&c| c == '\n').filter(|&p| p > 0).map(|p| p + 1))
            .or_else(|| chunk.iter().rposition(|&c| c == ' ').filter(|&p| p > 0).map(|p| p + 1))
            .unwrap_or(page_size);
        pages.push(rest[..cut].iter().collect());
        start += cut;
    }
    if pages.is_empty() {
        pages.push(String::new());
    }
    pages
}

/// Path of `name` on PATH (trying the PATHEXT extensions), if present.
pub fn find_external_tool(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|e| !e.is_empty())
        .map(str::to_lowercase)
        .collect();
    let dirs = std::env::split_paths(&std::env::var_os("PATH")?).collect::<Vec<_>>();
    dirs.iter().find_map(|dir| {
        let plain = dir.join(name);
        if plain.is_file() {
            return Some(plain);
        }
        exts.iter().map(|e| dir.join(format!("{name}{e}"))).find(|p| p.is_file())
    })
}

/// True when an external tool (pandoc, antiword, tesseract, ffmpeg) is on PATH.
pub fn check_external_tool(name: &str) -> bool {
    find_external_tool(name).is_some()
}

/// Runs an external tool with no console window and returns stdout.
///
/// Fails on a non-zero exit (with the start of stderr in the message) and
/// kills the process when `timeout_secs` elapses.
pub fn run_external_tool(name: &str, args: &[&str], timeout_secs: u64) -> anyhow::Result<Vec<u8>> {
    let exe = find_external_tool(name).with_context(|| format!("{name} not available"))?;
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("could not start {name}"))?;

    // Drain both pipes on threads so a chatty tool cannot block on a full pipe.
    let mut out = child.stdout.take().context("no stdout pipe")?;
    let mut err = child.stderr.take().context("no stderr pipe")?;
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{name} timed out after {timeout_secs}s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    if !status.success() {
        let msg: String = String::from_utf8_lossy(&stderr).chars().take(500).collect();
        bail!("{name} failed: {msg}");
    }
    Ok(stdout)
}

/// Characters per synthetic page: the caller's setting, or the default when unset.
pub fn page_size_of(opts: &OpenOptions) -> usize {
    if opts.page_size == 0 { DEFAULT_PAGE_SIZE } else { opts.page_size }
}

/// Decodes file bytes the way a reader expects: UTF-8 (BOM dropped), UTF-16
/// by BOM, otherwise the encoding chardetng guesses (so a cp1252 file keeps
/// its accents instead of turning into replacement characters).
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc.decode(bytes).0.into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    detector.guess(None, chardetng::Utf8Detection::Deny).decode_without_bom_handling(bytes).0.into_owned()
}

/// Python text-mode newline translation: `\r\n` and lone `\r` become `\n`.
pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Reads a text file as a Python `open(path, "r")` would: decoded, newlines normalised.
pub fn read_text_file(path: &Path) -> std::io::Result<String> {
    Ok(normalize_newlines(&decode_text(&std::fs::read(path)?)))
}

/// Paginates `text` and hands each element to the page it starts on, rebasing
/// positions to that page. An element running past its page's end is clipped.
/// `elements` must be sorted by `start_pos`.
pub fn split_pages(text: &str, elements: Vec<SemanticElement>, page_size: usize) -> Vec<Page> {
    let chunks = paginate_text(text, page_size);
    let mut starts = Vec::with_capacity(chunks.len());
    let mut total = 0;
    for chunk in &chunks {
        starts.push(total);
        total += chunk.chars().count();
    }
    let mut pages: Vec<Page> = chunks.into_iter().map(Page::plain).collect();
    for mut e in elements {
        let i = starts.partition_point(|&s| s <= e.start_pos).saturating_sub(1);
        let page_end = starts.get(i + 1).copied().unwrap_or(total);
        e.end_pos = e.end_pos.min(page_end).saturating_sub(starts[i]);
        e.start_pos -= starts[i];
        pages[i].elements.push(e);
    }
    pages
}

/// One TOC entry per heading element, in page order (title capped at 80 chars).
pub fn heading_toc(pages: &[Page]) -> Vec<TocEntry> {
    pages
        .iter()
        .enumerate()
        .flat_map(|(page_index, p)| {
            p.elements.iter().filter_map(move |e| {
                e.element_type.heading_level().map(|level| TocEntry {
                    title: e.text.chars().take(80).collect(),
                    page_index,
                    level: usize::from(level) - 1,
                    children: Vec::new(),
                })
            })
        })
        .collect()
}

/// A fully extracted, in-memory document: the shape every text-like format needs.
pub struct PagedDocument {
    path: PathBuf,
    format: &'static str,
    title: String,
    author: String,
    pages: Vec<Page>,
    toc: Vec<TocEntry>,
}

impl PagedDocument {
    pub fn new(path: &Path, format: &'static str, title: String, mut pages: Vec<Page>, toc: Vec<TocEntry>) -> Self {
        if pages.is_empty() {
            pages.push(Page::plain(""));
        }
        Self { path: path.to_path_buf(), format, title, author: String::new(), pages, toc }
    }

    pub fn with_author(mut self, author: String) -> Self {
        self.author = author;
        self
    }

    /// File name, used as the title by formats that carry none.
    pub fn file_name(path: &Path) -> String {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

impl Document for PagedDocument {
    fn path(&self) -> &Path {
        &self.path
    }

    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::SEARCHABLE_TEXT;
        if !self.toc.is_empty() {
            caps |= Capabilities::TOC;
        }
        caps
    }

    fn toc(&self) -> &[TocEntry] {
        &self.toc
    }

    fn metadata(&self) -> DocMetadata {
        DocMetadata { title: self.title.clone(), author: self.author.clone(), format: self.format.to_string(), extra: Vec::new() }
    }

    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page(&mut self, index: usize) -> &Page {
        &self.pages[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured_text::elements::{ElementMeta, ElementType};

    #[test]
    fn paginate_prefers_blank_line_then_newline_then_space_then_hard_break() {
        assert_eq!(paginate_text("aaaa\n\nbbbb\n\ncccc", 12), vec!["aaaa\n\nbbbb\n\n", "cccc"]);
        assert_eq!(paginate_text("aaaa\nbbbb\ncccc", 12), vec!["aaaa\nbbbb\n", "cccc"]);
        assert_eq!(paginate_text("aaaa bbbb cccc", 12), vec!["aaaa bbbb ", "cccc"]);
        assert_eq!(paginate_text("abcdefghijklmnop", 6), vec!["abcdef", "ghijkl", "mnop"]);
        assert_eq!(paginate_text("", 6), vec![""]);
        assert_eq!(paginate_text("short", 6), vec!["short"]);
    }

    #[test]
    fn paginate_counts_chars_not_bytes() {
        assert_eq!(paginate_text("ééééé ééééé", 7), vec!["ééééé ", "ééééé"]);
    }

    #[test]
    fn split_pages_rebases_elements_onto_their_page() {
        let el = |start, end| SemanticElement {
            element_type: ElementType::Heading2,
            start_pos: start,
            end_pos: end,
            text: "h".into(),
            meta: ElementMeta { level: Some(2), ..Default::default() },
        };
        let pages = split_pages("aaaa\n\nbbbb\n\ncccc", vec![el(0, 4), el(12, 16)], 12);
        assert_eq!(pages.len(), 2);
        assert_eq!((pages[0].elements[0].start_pos, pages[0].elements[0].end_pos), (0, 4));
        assert_eq!((pages[1].elements[0].start_pos, pages[1].elements[0].end_pos), (0, 4));
        let toc = heading_toc(&pages);
        assert_eq!(toc.iter().map(|t| (t.page_index, t.level)).collect::<Vec<_>>(), vec![(0, 1), (1, 1)]);
    }

    #[test]
    fn decode_text_handles_utf8_bom_and_cp1252() {
        assert_eq!(decode_text(b"\xef\xbb\xbfcaf\xc3\xa9"), "café");
        assert_eq!(decode_text(b"Caf\xe9 notes, na\xefve fianc\xe9e and cr\xe8me br\xfbl\xe9e."), "Café notes, naïve fiancée and crème brûlée.");
        assert_eq!(normalize_newlines("a\r\nb\rc"), "a\nb\nc");
    }

    #[test]
    fn external_tool_probe_and_run() {
        assert!(!check_external_tool("definitely-not-a-real-tool-xyz"));
        assert!(run_external_tool("definitely-not-a-real-tool-xyz", &[], 5).is_err());
        let out = run_external_tool("cmd", &["/C", "echo", "hi"], 10).unwrap();
        assert_eq!(String::from_utf8_lossy(&out).trim(), "hi");
        assert!(run_external_tool("cmd", &["/C", "exit", "3"], 10).is_err());
    }
}
