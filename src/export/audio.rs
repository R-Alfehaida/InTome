//! Shared audio pipeline for generating TTS audio via Gemini (port of `prism/export/audio.py`).

use std::sync::LazyLock;

use anyhow::Context;
use regex::Regex;

use crate::ai::client::GeminiClient;
use crate::cancel::CancelToken;
use crate::config;
use crate::document::{Document, TocEntry};
use crate::export::audio_parts::{self, Part, PartStore};
use crate::export::base::Cancelled;

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

/// Raw PCM in a 44-byte WAV header at Gemini's 24 kHz / 16-bit / mono. Exports
/// stream from saved parts instead ([`audio_parts::write_wav`]); this is for tests.
#[cfg(test)]
pub fn pcm_to_wav(pcm: &[u8]) -> Vec<u8> {
    let mut wav = audio_parts::wav_header(pcm.len() as u32, TTS_SAMPLE_RATE, 16, 1).to_vec();
    wav.extend_from_slice(pcm);
    wav
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

/// One section's audio: its saved parts on disk, in reading order. Empty when the
/// section has no text.
#[derive(Debug, Clone)]
pub struct SectionAudio {
    pub title: String,
    pub parts: Vec<Part>,
}

impl SectionAudio {
    pub fn pcm_bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.bytes).sum()
    }

    pub fn duration_seconds(&self) -> f64 {
        self.pcm_bytes() as f64 / (f64::from(TTS_SAMPLE_RATE) * 2.0)
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

/// How many TTS chunks `sections` will be synthesised as, for a progress total
/// known before synthesis starts.
pub fn chunk_count(sections: &[(String, String)]) -> usize {
    sections.iter().map(|(_, text)| split_text_for_tts(text, TTS_CHUNK_CHAR_BUDGET).len()).sum()
}

/// Every part of every section, for [`audio_parts::discard`] once the export succeeded.
pub fn all_parts(sections: &[SectionAudio]) -> Vec<Part> {
    sections.iter().flat_map(|s| s.parts.iter().cloned()).collect()
}

/// Generates audio for document sections via Gemini TTS.
pub struct AudioPipeline {
    model: String,
}

impl Default for AudioPipeline {
    fn default() -> Self {
        Self { model: config::get().gemini.tts_model }
    }
}

impl AudioPipeline {
    /// Synthesises every section chunk by chunk ([`split_text_for_tts`]) into saved
    /// parts, reusing parts an earlier, unfinished export already made. Returns one
    /// [`SectionAudio`] per section, in order.
    ///
    /// A chunk that still fails after the client's retries stops the export with an
    /// error naming its section: a book with a silently missing chapter is worse than
    /// no book, and the saved parts make trying again cheap. Cancelling returns
    /// [`Cancelled`] and also keeps the parts.
    pub fn generate_all_sections(
        &self,
        sections: &[(String, String)],
        voice: &str,
        cancel: &CancelToken,
        progress: &mut dyn FnMut(usize, usize, &str),
    ) -> anyhow::Result<Vec<SectionAudio>> {
        let store = PartStore::open(audio_parts::parts_dir()).context("cannot create the folder for saved audio parts")?;
        let mut client = None;
        let mut synth = |text: &str| -> anyhow::Result<Vec<u8>> {
            if client.is_none() {
                client = Some(GeminiClient::new()?);
            }
            client.as_ref().unwrap().generate_audio(text, voice, &self.model, Some(cancel))
        };
        generate_with(&store, &self.model, sections, voice, cancel, progress, &mut synth)
    }
}

/// [`AudioPipeline::generate_all_sections`] with the synthesiser passed in, so tests
/// need no network. `progress` counts chunks, not sections, so a long chapter moves
/// the bar too.
fn generate_with(
    store: &PartStore,
    model: &str,
    sections: &[(String, String)],
    voice: &str,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(usize, usize, &str),
    synth: &mut dyn FnMut(&str) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<Vec<SectionAudio>> {
    let plan: Vec<(&String, Vec<String>)> = sections.iter().map(|(title, text)| (title, split_text_for_tts(text, TTS_CHUNK_CHAR_BUDGET))).collect();
    let total = plan.iter().map(|(_, chunks)| chunks.len()).sum::<usize>();
    let (mut done, mut reused) = (0, 0);
    let mut out = Vec::with_capacity(plan.len());
    for (title, chunks) in plan {
        let mut parts = Vec::with_capacity(chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(Cancelled.into());
            }
            let key = PartStore::key(model, voice, chunk);
            let part = match store.get(&key) {
                Some(part) => {
                    reused += 1;
                    part
                }
                None => {
                    let pcm = synth(chunk).map_err(|e| {
                        if cancel.is_cancelled() {
                            return Cancelled.into();
                        }
                        e.context(format!(
                            "Could not make the audio for \"{title}\" (part {} of {}). The audio made so far is saved, so exporting again continues from here",
                            i + 1,
                            chunks.len()
                        ))
                    })?;
                    store.put(&key, &pcm).context("cannot save a finished audio part")?
                }
            };
            parts.push(part);
            done += 1;
            progress(done, total, &format!("Audio {done} of {total}: {title}"));
        }
        out.push(SectionAudio { title: title.clone(), parts });
    }
    if reused > 0 {
        log::info!("audio export reused {reused} of {total} saved parts");
    }
    Ok(out)
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

    fn sections(texts: &[(&str, &str)]) -> Vec<(String, String)> {
        texts.iter().map(|(t, x)| (t.to_string(), x.to_string())).collect()
    }

    /// A fake synthesiser: 2 bytes of "audio" per character, so durations are predictable.
    fn fake(calls: &mut Vec<String>) -> impl FnMut(&str) -> anyhow::Result<Vec<u8>> + '_ {
        move |text: &str| {
            calls.push(text.to_string());
            Ok(vec![7; text.chars().count() * 2])
        }
    }

    #[test]
    fn sections_become_saved_parts_and_empty_sections_stay_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        let (mut calls, mut steps) = (Vec::new(), Vec::new());
        let input = sections(&[("One", "Hello there."), ("Blank", "  \n"), ("Two", "Bye.")]);
        let out = generate_with(&store, "m", &input, "Kore", &CancelToken::new(), &mut |d, t, m| steps.push(format!("{d}/{t} {m}")), &mut fake(&mut calls))
            .unwrap();
        assert_eq!(calls, ["Hello there.", "Bye."]);
        assert_eq!(steps, ["1/2 Audio 1 of 2: One", "2/2 Audio 2 of 2: Two"]);
        assert_eq!(out.iter().map(|s| (s.title.as_str(), s.parts.len())).collect::<Vec<_>>(), [("One", 1), ("Blank", 0), ("Two", 1)]);
        assert_eq!(out[0].pcm_bytes(), 24);
        assert_eq!(std::fs::read(&out[2].parts[0].path).unwrap(), vec![7; 8]);
        assert!(out[1].is_empty());
    }

    #[test]
    fn a_failed_chunk_stops_the_export_and_a_rerun_reuses_finished_parts() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        let input = sections(&[("Chapter 1", "First."), ("Chapter 2", "Second."), ("Chapter 3", "Third.")]);
        let mut first_try = |text: &str| -> anyhow::Result<Vec<u8>> {
            if text == "Second." { anyhow::bail!("503 unavailable") } else { Ok(vec![1; 4]) }
        };
        let err = generate_with(&store, "m", &input, "Kore", &CancelToken::new(), &mut |_, _, _| {}, &mut first_try).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.starts_with("Could not make the audio for \"Chapter 2\" (part 1 of 1)"), "{msg}");
        assert!(msg.ends_with("503 unavailable"), "{msg}");

        let mut calls = Vec::new();
        let out = generate_with(&store, "m", &input, "Kore", &CancelToken::new(), &mut |_, _, _| {}, &mut fake(&mut calls)).unwrap();
        assert_eq!(calls, ["Second.", "Third."], "only the chunks without a saved part are synthesised again");
        assert_eq!(out.iter().map(|s| s.parts.len()).sum::<usize>(), 3);

        let mut calls = Vec::new();
        generate_with(&store, "m", &input, "Puck", &CancelToken::new(), &mut |_, _, _| {}, &mut fake(&mut calls)).unwrap();
        assert_eq!(calls.len(), 3, "another voice reuses nothing");
    }

    #[test]
    fn cancelling_returns_cancelled_and_keeps_finished_parts() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        let cancel = CancelToken::new();
        let input = sections(&[("A", "One."), ("B", "Two.")]);
        let mut synth = |text: &str| -> anyhow::Result<Vec<u8>> {
            if text == "Two." {
                cancel.cancel();
                anyhow::bail!("Operation cancelled")
            }
            Ok(vec![1; 2])
        };
        let err = generate_with(&store, "m", &input, "Kore", &cancel, &mut |_, _, _| {}, &mut synth).unwrap_err();
        assert!(err.downcast_ref::<Cancelled>().is_some());
        assert!(store.get(&PartStore::key("m", "Kore", "One.")).is_some());
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
