//! Shared audio pipeline for generating TTS audio via Gemini (port of `prism/export/audio.py`).

use std::sync::LazyLock;

use regex::Regex;

use crate::ai::client::{DEFAULT_TTS_MODEL, GeminiClient};
use crate::cancel::CancelToken;
use crate::document::{Document, TocEntry};

/// Gemini TTS quality can drift past a few minutes of continuous audio, so a whole
/// chapter must not be synthesized in one call. ~3000 characters is roughly two minutes
/// of narration at a typical speaking rate, well under the drift threshold, while
/// keeping the number of API calls reasonable.
pub const TTS_CHUNK_CHAR_BUDGET: usize = 3000;

/// Gemini TTS output format: 24 kHz, 16-bit, mono.
pub const TTS_SAMPLE_RATE: u32 = 24000;

static PARAGRAPH_BREAK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").unwrap());

fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Splits `text` into ordered chunks of at most `budget` chars.
///
/// Paragraphs stay whole and are packed up to the budget; an oversized paragraph is
/// split on sentence boundaries; a single sentence longer than the budget is hard-split
/// as a last resort. Empty or whitespace-only input yields no chunks.
pub fn split_text_for_tts(text: &str, budget: usize) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    if char_len(text) <= budget {
        return vec![text.trim().to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, chunks: &mut Vec<String>| {
        if !current.trim().is_empty() {
            chunks.push(current.trim().to_string());
        }
        current.clear();
    };

    for para in PARAGRAPH_BREAK.split(text).filter(|p| !p.trim().is_empty()) {
        if char_len(para) > budget {
            flush(&mut current, &mut chunks);
            chunks.extend(split_oversized(para, budget));
        } else if char_len(&current) + char_len(para) + 2 > budget {
            flush(&mut current, &mut chunks);
            current = para.to_string();
        } else if current.is_empty() {
            current = para.to_string();
        } else {
            current = format!("{current}\n\n{para}");
        }
    }
    flush(&mut current, &mut chunks);
    chunks
}

/// Sentences end at `.`, `!` or `?` followed by whitespace (Python: `(?<=[.!?])\s+`).
fn split_sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev = ' ';
    let mut ws_start = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if ws_start.is_none() && matches!(prev, '.' | '!' | '?') {
                ws_start = Some(i);
            }
        } else if let Some(w) = ws_start.take() {
            out.push(&text[start..w]);
            start = i;
        }
        prev = c;
    }
    out.push(&text[start..]);
    out
}

fn split_oversized(para: &str, budget: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for sentence in split_sentences(para.trim()).into_iter().filter(|s| !s.is_empty()) {
        if char_len(sentence) > budget {
            if !buf.trim().is_empty() {
                out.push(buf.trim().to_string());
                buf.clear();
            }
            let chars: Vec<char> = sentence.chars().collect();
            out.extend(chars.chunks(budget).map(|c| c.iter().collect::<String>()));
        } else if char_len(&buf) + char_len(sentence) + 1 > budget {
            if !buf.trim().is_empty() {
                out.push(buf.trim().to_string());
            }
            buf = sentence.to_string();
        } else {
            buf = format!("{buf} {sentence}").trim().to_string();
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    out
}

/// Wraps raw PCM in a 44-byte WAV header.
pub fn pcm_to_wav_fmt(pcm: &[u8], sample_rate: u32, bits_per_sample: u16, channels: u16) -> Vec<u8> {
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(block_align);
    let data_size = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// `pcm_to_wav_fmt` at Gemini's 24 kHz / 16-bit / mono.
pub fn pcm_to_wav(pcm: &[u8]) -> Vec<u8> {
    pcm_to_wav_fmt(pcm, TTS_SAMPLE_RATE, 16, 1)
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Duration of a 44-byte-header WAV buffer; 0 when it is too short or the header is empty.
pub fn wav_duration_seconds(wav: &[u8]) -> f64 {
    if wav.len() < 44 {
        return 0.0;
    }
    let (rate, bits, channels, data) = (le32(wav, 24), le16(wav, 34), le16(wav, 22), le32(wav, 40));
    if rate == 0 || bits == 0 || channels == 0 {
        return 0.0;
    }
    f64::from(data) / (f64::from(rate) * f64::from(channels) * f64::from(bits / 8))
}

/// Joins WAV buffers of the same format: strips each 44-byte header, concatenates the PCM
/// and writes one header (taken from the first buffer's format).
pub fn concatenate_wav(wavs: &[Vec<u8>]) -> Vec<u8> {
    match wavs {
        [] => Vec::new(),
        [only] => only.clone(),
        [first, ..] => {
            let pcm: Vec<u8> = wavs.iter().filter(|w| w.len() > 44).flat_map(|w| w[44..].iter().copied()).collect();
            if first.len() >= 44 {
                pcm_to_wav_fmt(&pcm, le32(first, 24), le16(first, 34), le16(first, 22))
            } else {
                pcm_to_wav(&pcm)
            }
        }
    }
}

/// `(title, text)` chapters from the document's structure.
///
/// Chapters come from the TOC / outline (EPUB nav, PDF outline, DOCX/HTML/Markdown/ODF
/// heading styles). Only the top-level entries (the shallowest `level` present) become
/// chapters; sub-headings fold into their parent. Chapter *i* spans the pages from its
/// start to the next top-level entry's start. Leading pages before the first entry become
/// a front-matter chapter titled from the document title ("Beginning" if none). Without
/// usable structure the whole document is one chapter ("Full Document" if untitled).
///
/// Independent of TTS chunking, which only splits a chapter into render-sized pieces.
/// Boundaries are only as fine as `page_index`.
pub fn extract_chapters(doc: &mut dyn Document) -> Vec<(String, String)> {
    let page_count = doc.page_count();
    fn flatten<'a>(entries: &'a [TocEntry], out: &mut Vec<&'a TocEntry>) {
        for e in entries {
            out.push(e);
            flatten(&e.children, out);
        }
    }
    let mut flat = Vec::new();
    flatten(doc.toc(), &mut flat);
    let flat: Vec<(String, usize, usize)> =
        flat.into_iter().filter(|e| e.page_index < page_count).map(|e| (e.title.clone(), e.page_index, e.level)).collect();
    let title = doc.metadata().title;

    let mut range_text = |start: usize, end: usize| -> String {
        (start..end).map(|p| doc.page(p).text.clone()).collect::<Vec<_>>().join("\n\n")
    };

    let Some(min_level) = flat.iter().map(|e| e.2).min() else {
        let t = if title.is_empty() { "Full Document".to_string() } else { title };
        return vec![(t, range_text(0, page_count))];
    };
    let top: Vec<&(String, usize, usize)> = flat.iter().filter(|e| e.2 == min_level).collect();

    let mut chapters = Vec::new();
    if top[0].1 > 0 {
        let front = range_text(0, top[0].1);
        if !front.trim().is_empty() {
            chapters.push((if title.is_empty() { "Beginning".to_string() } else { title }, front));
        }
    }
    for (i, entry) in top.iter().enumerate() {
        let end = top.get(i + 1).map_or(page_count, |n| n.1);
        chapters.push((entry.0.clone(), range_text(entry.1, end)));
    }
    chapters
}

/// Generates audio for document sections via Gemini TTS.
pub struct AudioPipeline {
    model: String,
}

impl Default for AudioPipeline {
    fn default() -> Self {
        Self { model: DEFAULT_TTS_MODEL.into() }
    }
}

impl AudioPipeline {
    /// WAV bytes for one section, or empty if cancelled before any chunk finished.
    ///
    /// The section is split by [`split_text_for_tts`] and synthesised chunk by chunk. The
    /// raw PCM is joined first and wrapped in a single header: joining WAVs that each carry
    /// a header introduces clicks at the seams.
    pub fn generate_section_audio(&self, client: &GeminiClient, text: &str, voice: &str, cancel: &CancelToken) -> anyhow::Result<Vec<u8>> {
        let chunks = split_text_for_tts(text, TTS_CHUNK_CHAR_BUDGET);
        if chunks.is_empty() {
            return Ok(Vec::new());
        }
        let mut pcm = Vec::new();
        for chunk in &chunks {
            if cancel.is_cancelled() {
                break;
            }
            pcm.extend(client.generate_audio(chunk, voice, &self.model, Some(cancel))?);
        }
        Ok(pcm_to_wav(&pcm))
    }

    /// `(title, wav)` per section. Empty sections and sections whose synthesis failed give
    /// an empty buffer (the failure is logged); stops early when cancelled. Fails only if
    /// the Gemini client cannot be created.
    pub fn generate_all_sections(
        &self,
        sections: &[(String, String)],
        voice: &str,
        cancel: &CancelToken,
        progress: &mut dyn FnMut(usize, usize, &str),
    ) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let client = GeminiClient::new()?;
        let total = sections.len();
        let mut results = Vec::with_capacity(total);
        for (i, (title, text)) in sections.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            if text.trim().is_empty() {
                results.push((title.clone(), Vec::new()));
                continue;
            }
            match self.generate_section_audio(&client, text, voice, cancel) {
                Ok(wav) => results.push((title.clone(), wav)),
                Err(e) => {
                    log::error!("Failed to generate audio for section '{title}': {e:#}");
                    results.push((title.clone(), Vec::new()));
                }
            }
            progress(i + 1, total, &format!("Audio: {title}"));
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Capabilities, DocMetadata, Page};
    use std::path::Path;

    #[test]
    fn short_text_is_one_trimmed_chunk() {
        assert_eq!(split_text_for_tts("  hello world \n", 100), vec!["hello world"]);
        assert!(split_text_for_tts(" \n\t", 100).is_empty());
    }

    #[test]
    fn paragraphs_pack_up_to_the_budget() {
        // Each paragraph is 10 chars; two plus the 2-char joiner is 22 <= 25, three would be 34.
        let p = ["aaaaaaaaaa", "bbbbbbbbbb", "cccccccccc", "dddddddddd"].join("\n\n");
        assert_eq!(split_text_for_tts(&p, 25), vec!["aaaaaaaaaa\n\nbbbbbbbbbb", "cccccccccc\n\ndddddddddd"]);
    }

    #[test]
    fn oversized_paragraph_splits_on_sentences() {
        let text = "One two. Three four. Five six.\n\nShort.";
        // The first paragraph is 30 chars (> 22): sentences "One two." + "Three four." fit (20), "Five six." starts a new chunk.
        assert_eq!(split_text_for_tts(text, 22), vec!["One two. Three four.", "Five six.", "Short."]);
    }

    #[test]
    fn monster_sentence_is_hard_split_after_flushing_the_buffer() {
        let text = "Hi. abcdefghijklmnopqrstuvwxyz";
        assert_eq!(split_text_for_tts(text, 10), vec!["Hi.", "abcdefghij", "klmnopqrst", "uvwxyz"]);
    }

    #[test]
    fn wav_header_bytes() {
        let wav = pcm_to_wav(&[1, 2, 3, 4]);
        let expected: Vec<u8> = [
            &b"RIFF"[..], &[40, 0, 0, 0], b"WAVE", b"fmt ", &[16, 0, 0, 0], &[1, 0], &[1, 0],
            &[0xC0, 0x5D, 0, 0],       // 24000 Hz
            &[0x80, 0xBB, 0, 0],       // byte rate 48000
            &[2, 0], &[16, 0], b"data", &[4, 0, 0, 0], &[1, 2, 3, 4],
        ]
        .concat();
        assert_eq!(wav, expected);
    }

    #[test]
    fn duration_from_header() {
        // 48000 bytes of 24 kHz/16-bit/mono = 1 second.
        assert_eq!(wav_duration_seconds(&pcm_to_wav(&vec![0u8; 48000])), 1.0);
        assert_eq!(wav_duration_seconds(&[0u8; 10]), 0.0);
    }

    #[test]
    fn concatenate_joins_pcm_under_one_header() {
        let joined = concatenate_wav(&[pcm_to_wav(&[1, 2]), pcm_to_wav(&[3, 4, 5, 6])]);
        assert_eq!(joined, pcm_to_wav(&[1, 2, 3, 4, 5, 6]));
        assert_eq!(concatenate_wav(&[]), Vec::<u8>::new());
        let one = pcm_to_wav(&[9, 9]);
        assert_eq!(concatenate_wav(std::slice::from_ref(&one)), one);
    }

    struct Doc {
        toc: Vec<TocEntry>,
        pages: Vec<Page>,
        title: String,
    }

    impl Document for Doc {
        fn path(&self) -> &Path {
            Path::new("t")
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::empty()
        }
        fn toc(&self) -> &[TocEntry] {
            &self.toc
        }
        fn metadata(&self) -> DocMetadata {
            DocMetadata { title: self.title.clone(), ..Default::default() }
        }
        fn page_count(&self) -> usize {
            self.pages.len()
        }
        fn page(&mut self, i: usize) -> &Page {
            &self.pages[i]
        }
    }

    fn entry(title: &str, page: usize, level: usize, children: Vec<TocEntry>) -> TocEntry {
        TocEntry { title: title.into(), page_index: page, level, children }
    }

    fn doc(toc: Vec<TocEntry>, title: &str) -> Doc {
        Doc { toc, pages: ["p0", "p1", "p2", "p3"].map(Page::plain).to_vec(), title: title.into() }
    }

    #[test]
    fn chapters_use_top_level_entries_and_front_matter() {
        let toc = vec![entry("Ch1", 1, 0, vec![entry("Sec", 2, 1, vec![])]), entry("Ch2", 3, 0, vec![])];
        let got = extract_chapters(&mut doc(toc, "Book"));
        assert_eq!(
            got,
            vec![
                ("Book".to_string(), "p0".to_string()),
                ("Ch1".to_string(), "p1\n\np2".to_string()),
                ("Ch2".to_string(), "p3".to_string()),
            ]
        );
    }

    #[test]
    fn chapters_fall_back_to_whole_document() {
        let got = extract_chapters(&mut doc(vec![], ""));
        assert_eq!(got, vec![("Full Document".to_string(), "p0\n\np1\n\np2\n\np3".to_string())]);
    }
}
