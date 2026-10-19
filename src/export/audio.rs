//! Shared audio pipeline for generating TTS audio via Gemini (port of `prism/export/audio.py`).

use std::sync::{LazyLock, Mutex};

use anyhow::Context;
use regex::Regex;

use crate::ai::client::GeminiClient;
use crate::ai::expressive::{self, Character, Narrator};
use crate::cancel::CancelToken;
use crate::config::{self, Expressiveness};
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
    narration_model: String,
    expressive: Expressiveness,
    /// The saved cast of an expressive export, deleted with the audio parts.
    cast_file: Mutex<Option<Part>>,
}

impl Default for AudioPipeline {
    fn default() -> Self {
        Self::new(Expressiveness::Off)
    }
}

impl AudioPipeline {
    /// A pipeline that reads the text as it is (`Off`) or marks it up first ([`expressive`]).
    pub fn new(expressive: Expressiveness) -> Self {
        let gemini = config::get().gemini;
        Self { model: gemini.tts_model, narration_model: gemini.narration_model, expressive, cast_file: Mutex::new(None) }
    }

    /// Deletes the saved parts (and cast) behind `audio`, for when the export that
    /// used them has succeeded.
    pub fn discard(&self, audio: &[SectionAudio]) {
        audio_parts::discard(&all_parts(audio));
        if let Some(cast) = self.cast_file.lock().unwrap_or_else(|e| e.into_inner()).take() {
            audio_parts::discard(&[cast]);
        }
    }

    /// The book's characters for expressive narration, cast part by part with the
    /// cast so far. Saved beside the audio parts, so a resumed export keeps the
    /// same characters and pitches and so reuses the audio it already made.
    fn cast(&self, store: &PartStore, client: &GeminiClient, sections: &[(String, String)], cancel: &CancelToken) -> anyhow::Result<Vec<Character>> {
        let text = sections.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join("\n\n");
        let key = PartStore::key(&self.narration_model, "cast-v1", &text);
        let saved = store.get_file(&key, "json").and_then(|part| {
            let cast = std::fs::read(&part.path).ok().and_then(|data| serde_json::from_slice(&data).ok())?;
            Some((part, cast))
        });
        let (part, cast) = match saved {
            Some(found) => found,
            None => {
                let mut cast = Vec::new();
                for part in split_text_for_tts(&text, expressive::CAST_PART_CHARS) {
                    if cancel.is_cancelled() {
                        return Err(Cancelled.into());
                    }
                    let reply = client
                        .generate_json(&self.narration_model, expressive::casting_prompt(), &expressive::cast_input(&cast, &part), &expressive::cast_schema(), Some(cancel))
                        .map_err(|e| if cancel.is_cancelled() { Cancelled.into() } else { e.context("Could not find the book's characters for expressive narration") })?;
                    cast = expressive::merge_cast(&cast, expressive::parse_cast(&reply)?);
                }
                (store.put_file(&key, "json", &serde_json::to_vec(&cast)?).context("cannot save the cast")?, cast)
            }
        };
        log::info!("expressive narration cast: {}", cast.iter().map(|c| format!("{} ({})", c.id, c.register)).collect::<Vec<_>>().join(", "));
        *self.cast_file.lock().unwrap_or_else(|e| e.into_inner()) = Some(part);
        Ok(cast)
    }
    /// Synthesises every section chunk by chunk ([`split_text_for_tts`]) into saved
    /// parts, reusing parts an earlier, unfinished export already made. Returns one
    /// [`SectionAudio`] per section, in order.
    ///
    /// A chunk that still fails after the client's retries stops the export with an
    /// error naming its section: a book with a silently missing chapter is worse than
    /// no book, and the saved parts make trying again cheap. Cancelling returns
    /// [`Cancelled`] and also keeps the parts. `export.audio_concurrent` chunks are
    /// synthesised at once.
    pub fn generate_all_sections(
        &self,
        sections: &[(String, String)],
        voice: &str,
        cancel: &CancelToken,
        progress: &mut dyn FnMut(usize, usize, &str),
    ) -> anyhow::Result<Vec<SectionAudio>> {
        let store = PartStore::open(audio_parts::parts_dir()).context("cannot create the folder for saved audio parts")?;
        let client = GeminiClient::new()?;
        let workers = config::get().export.audio_concurrent.max(1) as usize;
        if self.expressive == Expressiveness::Off {
            let synth = |text: &str, _context: &str| client.generate_audio(text, voice, &self.model, Some(cancel));
            return generate_with(&store, &self.model, "", sections, voice, cancel, progress, &synth, workers);
        }
        progress(0, chunk_count(sections), "Finding the characters in the book...");
        let cast = self.cast(&store, &client, sections, cancel)?;
        // Audio made in another mode, by another narration model or with another cast is never reused.
        let cast_id = PartStore::key(&self.narration_model, "cast", &serde_json::to_string(&cast)?);
        let salt = format!("expressive-{:?}-{}-{cast_id}", self.expressive, self.narration_model);
        let narrator = Narrator::new(&client, &self.narration_model, self.expressive, cast);
        let synth = |text: &str, context: &str| {
            let items = narrator.items(context, text, Some(cancel))?;
            client.generate_audio_items(items, voice, &self.model, Some(cancel))
        };
        generate_with(&store, &self.model, &salt, sections, voice, cancel, progress, &synth, workers)
    }
}

/// One TTS chunk to make: where it sits in the book and the key its saved part has.
struct Job<'a> {
    section: usize,
    /// 0-based chunk within its section, of `of`.
    index: usize,
    of: usize,
    text: &'a str,
    /// Up to two paragraphs before the chunk in its section, for expressive
    /// narration to see the scene and who spoke last. Empty at a section's start.
    context: String,
    key: String,
}

/// The last two paragraphs of `previous`.
fn context_after(previous: &str) -> String {
    let paragraphs: Vec<&str> = PARAGRAPH_BREAK.split(previous).filter(|p| !p.trim().is_empty()).collect();
    paragraphs[paragraphs.len().saturating_sub(2)..].join("\n\n")
}

/// [`AudioPipeline::generate_all_sections`] with the synthesiser passed in, so tests
/// need no network. Saved parts are reused first; the rest go to `workers` threads,
/// which take chunks in reading order and save each as it finishes. A failure or a
/// cancel stops new chunks from starting, and the chunks already in progress finish
/// and are saved, so trying again loses nothing. `progress` counts chunks, not
/// sections, so a long chapter moves the bar too.
/// `salt` names everything besides model, voice and text that shaped the audio
/// (empty for plain reading), so it is part of each saved part's key. `synth`
/// gets a chunk's text and its [`Job::context`].
#[allow(clippy::too_many_arguments)]
fn generate_with(
    store: &PartStore,
    model: &str,
    salt: &str,
    sections: &[(String, String)],
    voice: &str,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(usize, usize, &str),
    synth: &(dyn Fn(&str, &str) -> anyhow::Result<Vec<u8>> + Sync),
    workers: usize,
) -> anyhow::Result<Vec<SectionAudio>> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let chunks: Vec<Vec<String>> = sections.iter().map(|(_, text)| split_text_for_tts(text, TTS_CHUNK_CHAR_BUDGET)).collect();
    let key = |text: &str| if salt.is_empty() { PartStore::key(model, voice, text) } else { PartStore::key(model, voice, &format!("{salt}\0{text}")) };
    let jobs: Vec<Job> = chunks
        .iter()
        .enumerate()
        .flat_map(|(section, list)| {
            list.iter().enumerate().map(move |(index, text)| Job {
                section,
                index,
                of: list.len(),
                text,
                context: index.checked_sub(1).map(|i| context_after(&list[i])).unwrap_or_default(),
                key: key(text),
            })
        })
        .collect();
    let total = jobs.len();
    let mut slots: Vec<Option<Part>> = jobs.iter().map(|j| store.get(&j.key)).collect();
    let pending: Vec<usize> = (0..total).filter(|&i| slots[i].is_none()).collect();
    let mut done = total - pending.len();
    if done > 0 {
        log::info!("audio export reused {done} of {total} saved parts");
        progress(done, total, &format!("Reusing {done} saved parts of {total}"));
    }

    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let mut first_error: Option<(usize, anyhow::Error)> = None;
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel::<(usize, anyhow::Result<Part>)>();
        for _ in 0..workers.max(1).min(pending.len()) {
            let (tx, next, stop, jobs, pending) = (tx.clone(), &next, &stop, &jobs, &pending);
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) && !cancel.is_cancelled() {
                    let Some(&i) = pending.get(next.fetch_add(1, Ordering::Relaxed)) else { break };
                    let job = &jobs[i];
                    let result = synth(job.text, &job.context).and_then(|pcm| store.put(&job.key, &pcm).context("cannot save a finished audio part"));
                    if result.is_err() {
                        stop.store(true, Ordering::Relaxed);
                    }
                    if tx.send((i, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        // Results arrive here, on the calling thread, as each chunk finishes.
        for (i, result) in rx {
            match result {
                Ok(part) => {
                    slots[i] = Some(part);
                    done += 1;
                    progress(done, total, &format!("Audio {done} of {total}: {}", sections[jobs[i].section].0));
                }
                Err(e) => {
                    // The earliest failing chunk in reading order is the one reported.
                    if first_error.as_ref().is_none_or(|(at, _)| i < *at) {
                        first_error = Some((i, e));
                    }
                }
            }
        }
    });

    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    if let Some((i, e)) = first_error {
        let job = &jobs[i];
        return Err(e.context(format!(
            "Could not make the audio for \"{}\" (part {} of {}). The audio made so far is saved, so exporting again continues from here",
            sections[job.section].0,
            job.index + 1,
            job.of
        )));
    }
    let mut parts = slots.into_iter();
    Ok(sections
        .iter()
        .zip(&chunks)
        .map(|((title, _), list)| SectionAudio { title: title.clone(), parts: parts.by_ref().take(list.len()).map(|p| p.expect("every chunk finished")).collect() })
        .collect())
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

    /// A fake synthesiser: 2 bytes of "audio" per character, so durations are
    /// predictable, recording each text it was asked for.
    fn fake(calls: &std::sync::Mutex<Vec<String>>) -> impl Fn(&str, &str) -> anyhow::Result<Vec<u8>> + Sync + '_ {
        move |text: &str, _context: &str| {
            calls.lock().unwrap().push(text.to_string());
            Ok(vec![7; text.chars().count() * 2])
        }
    }

    #[test]
    fn sections_become_saved_parts_and_empty_sections_stay_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        let (calls, mut steps) = (Default::default(), Vec::new());
        let input = sections(&[("One", "Hello there."), ("Blank", "  \n"), ("Two", "Bye.")]);
        let out = generate_with(&store, "m", "", &input, "Kore", &CancelToken::new(), &mut |d, t, m| steps.push(format!("{d}/{t} {m}")), &fake(&calls), 1)
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), ["Hello there.", "Bye."]);
        assert_eq!(steps, ["1/2 Audio 1 of 2: One", "2/2 Audio 2 of 2: Two"]);
        assert_eq!(out.iter().map(|s| (s.title.as_str(), s.parts.len())).collect::<Vec<_>>(), [("One", 1), ("Blank", 0), ("Two", 1)]);
        assert_eq!(out[0].pcm_bytes(), 24);
        assert_eq!(std::fs::read(&out[2].parts[0].path).unwrap(), vec![7; 8]);
        assert!(out[1].is_empty());
    }

    #[test]
    fn parallel_workers_overlap_and_keep_reading_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        let texts: Vec<String> = (0..9).map(|i| format!("Chunk {i}.")).collect();
        let input: Vec<(String, String)> = texts.iter().map(|t| (format!("Section {t}"), t.clone())).collect();
        let (in_flight, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        // Each "synthesis" takes a moment and returns the text's own bytes, so order is checkable.
        let slow = |text: &str, _context: &str| -> anyhow::Result<Vec<u8>> {
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(40));
            in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(text.as_bytes().to_vec())
        };
        let mut steps = 0;
        let out = generate_with(&store, "m", "", &input, "Kore", &CancelToken::new(), &mut |_, _, _| steps += 1, &slow, 3).unwrap();
        assert!(most.load(Ordering::SeqCst) > 1, "chunks ran at the same time");
        assert!(most.load(Ordering::SeqCst) <= 3, "never more than the worker count");
        assert_eq!(steps, 9);
        let read: Vec<String> = out.iter().map(|s| String::from_utf8(std::fs::read(&s.parts[0].path).unwrap()).unwrap()).collect();
        assert_eq!(read, texts, "audio comes back in reading order");
    }

    #[test]
    fn a_failed_chunk_stops_the_export_and_a_rerun_reuses_finished_parts() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        let input = sections(&[("Chapter 1", "First."), ("Chapter 2", "Second."), ("Chapter 3", "Third.")]);
        let first_try = |text: &str, _context: &str| -> anyhow::Result<Vec<u8>> {
            if text == "Second." { anyhow::bail!("503 unavailable") } else { Ok(vec![1; 4]) }
        };
        // One worker, so nothing after the failing chunk starts.
        let err = generate_with(&store, "m", "", &input, "Kore", &CancelToken::new(), &mut |_, _, _| {}, &first_try, 1).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.starts_with("Could not make the audio for \"Chapter 2\" (part 1 of 1)"), "{msg}");
        assert!(msg.ends_with("503 unavailable"), "{msg}");

        let calls = Default::default();
        let mut steps = Vec::new();
        let out = generate_with(&store, "m", "", &input, "Kore", &CancelToken::new(), &mut |d, t, m| steps.push(format!("{d}/{t} {m}")), &fake(&calls), 1).unwrap();
        assert_eq!(*calls.lock().unwrap(), ["Second.", "Third."], "only the chunks without a saved part are synthesised again");
        assert_eq!(steps[0], "1/3 Reusing 1 saved parts of 3");
        assert_eq!(out.iter().map(|s| s.parts.len()).sum::<usize>(), 3);

        let calls = Default::default();
        generate_with(&store, "m", "", &input, "Puck", &CancelToken::new(), &mut |_, _, _| {}, &fake(&calls), 3).unwrap();
        assert_eq!(calls.lock().unwrap().len(), 3, "another voice reuses nothing");
    }

    #[test]
    fn with_parallel_workers_the_earliest_failure_is_reported_and_finished_chunks_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        let input = sections(&[("A", "One."), ("B", "Two."), ("C", "Three."), ("D", "Four.")]);
        let synth = |text: &str, _context: &str| -> anyhow::Result<Vec<u8>> {
            match text {
                "Two." | "Four." => anyhow::bail!("503 unavailable"),
                _ => Ok(vec![1; 2]),
            }
        };
        let err = generate_with(&store, "m", "", &input, "Kore", &CancelToken::new(), &mut |_, _, _| {}, &synth, 4).unwrap_err();
        assert!(format!("{err:#}").starts_with("Could not make the audio for \"B\""), "{err:#}");
        assert!(store.get(&PartStore::key("m", "Kore", "One.")).is_some());
    }

    #[test]
    fn each_chunk_gets_the_two_paragraphs_before_it_in_its_section() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        // 900-char paragraphs: three fill a 3000-char chunk, the fourth starts the next.
        let para = |c: char| c.to_string().repeat(900);
        let long = [para('a'), para('b'), para('c'), para('d')].join("\n\n");
        let input = vec![("One".to_string(), long), ("Two".to_string(), "Fresh start.".to_string())];
        let seen = std::sync::Mutex::new(Vec::new());
        let record = |text: &str, context: &str| -> anyhow::Result<Vec<u8>> {
            seen.lock().unwrap().push((text.chars().next().unwrap(), context.to_string()));
            Ok(vec![0; 2])
        };
        generate_with(&store, "m", "", &input, "Kore", &CancelToken::new(), &mut |_, _, _| {}, &record, 1).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![('a', String::new()), ('d', format!("{}\n\n{}", para('b'), para('c'))), ('F', String::new())]);
    }

    #[test]
    fn audio_made_another_way_is_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        let input = sections(&[("A", "Hello.")]);
        let calls = Default::default();
        generate_with(&store, "m", "", &input, "Kore", &CancelToken::new(), &mut |_, _, _| {}, &fake(&calls), 1).unwrap();
        generate_with(&store, "m", "expressive-Subtle", &input, "Kore", &CancelToken::new(), &mut |_, _, _| {}, &fake(&calls), 1).unwrap();
        generate_with(&store, "m", "expressive-Subtle", &input, "Kore", &CancelToken::new(), &mut |_, _, _| {}, &fake(&calls), 1).unwrap();
        assert_eq!(calls.lock().unwrap().len(), 2, "plain, then expressive once, then the saved expressive part");
    }

    #[test]
    fn cancelling_returns_cancelled_and_keeps_finished_parts() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path()).unwrap();
        let cancel = CancelToken::new();
        let input = sections(&[("A", "One."), ("B", "Two.")]);
        let synth = |text: &str, _context: &str| -> anyhow::Result<Vec<u8>> {
            if text == "Two." {
                cancel.cancel();
                anyhow::bail!("Operation cancelled")
            }
            Ok(vec![1; 2])
        };
        let err = generate_with(&store, "m", "", &input, "Kore", &cancel, &mut |_, _, _| {}, &synth, 1).unwrap_err();
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
