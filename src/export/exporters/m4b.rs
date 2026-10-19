//! M4B audiobook exporter with chapter markers.
//!
//! Synthesises each chapter with Gemini TTS into saved parts on disk, then streams
//! them into the `ffmpeg` CLI, which encodes AAC. Chapters travel in an ffmetadata
//! file, which ffmpeg turns into MP4 chapter atoms (no tag library needed).
//!
//! - ffmpeg present: `.m4b` with chapters. If that encode fails, retry without chapters as `.m4a`.
//! - ffmpeg absent: the joined audio as `.wav` (at most 4 GB, about 24 hours).

use std::io::{self, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

use crate::ai::client::GeminiClient;
use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::audio::{self, AudioPipeline, SectionAudio, TTS_SAMPLE_RATE, all_parts, extract_chapters};
use crate::export::audio_parts::{self, copy_parts};
use crate::export::base::{ExportOptions, Exporter, ProgressFn, write_file};
use crate::export::util::or_default;

pub struct M4bExporter;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn has_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").creation_flags(CREATE_NO_WINDOW).output().is_ok_and(|o| o.status.success())
}

/// `(start_ms, end_ms, title)` chapter intervals.
///
/// Each chapter ends exactly where the next begins, so they tile the timeline with no gaps
/// or overlaps; the last one ends at `total_duration`, the real length of the rendered
/// audio. `chapters` is ordered `(start_seconds, title)`.
pub fn build_chapter_intervals(chapters: &[(f64, String)], total_duration: f64) -> Vec<(i64, i64, String)> {
    chapters
        .iter()
        .enumerate()
        .map(|(i, (start, title))| {
            let end = chapters.get(i + 1).map_or(total_duration, |n| n.0);
            ((start * 1000.0) as i64, (end * 1000.0) as i64, title.clone())
        })
        .collect()
}

/// ffmetadata values escape `=`, `;`, `#`, `\` and newlines with a backslash.
fn ffmeta_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '=' | ';' | '#' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' | '\r' => out.push_str("\\\n"),
            c => out.push(c),
        }
    }
    out
}

fn ffmetadata(title: &str, author: &str, intervals: &[(i64, i64, String)]) -> String {
    let mut s = format!(";FFMETADATA1\ntitle={}\nartist={}\n", ffmeta_escape(title), ffmeta_escape(author));
    for (start, end, chapter) in intervals {
        s.push_str(&format!("[CHAPTER]\nTIMEBASE=1/1000\nSTART={start}\nEND={end}\ntitle={}\n", ffmeta_escape(chapter)));
    }
    s
}

/// Writes the PCM that `feed` produces (24 kHz, 16-bit, mono) through ffmpeg's
/// stdin and encodes it to AAC at `out`. With `meta`, the ffmetadata file supplies
/// tags and chapters. Streaming keeps the whole book out of memory and avoids a
/// temporary WAV, which could not exceed 4 GB.
fn encode(feed: &dyn Fn(&mut dyn Write) -> io::Result<()>, meta: Option<&Path>, out: &Path) -> anyhow::Result<()> {
    use std::ffi::OsStr;
    let rate = TTS_SAMPLE_RATE.to_string();
    let input = ["-y", "-hide_banner", "-loglevel", "error", "-f", "s16le", "-ar", rate.as_str(), "-ac", "1", "-i", "pipe:0"];
    let mut args: Vec<&OsStr> = input.iter().map(OsStr::new).collect();
    if let Some(m) = meta {
        args.extend(["-i".as_ref(), m.as_os_str(), "-map_metadata".as_ref(), "1".as_ref(), "-map_chapters".as_ref(), "1".as_ref()]);
    }
    args.extend(["-c:a".as_ref(), "aac".as_ref(), "-b:a".as_ref(), "64k".as_ref(), out.as_os_str()]);
    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("cannot run ffmpeg")?;
    // Read stderr on its own thread so a full pipe can never stall ffmpeg while we write.
    let mut stderr = child.stderr.take().context("ffmpeg has no stderr")?;
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let fed = {
        let mut stdin = io::BufWriter::with_capacity(1 << 20, child.stdin.take().context("ffmpeg has no stdin")?);
        feed(&mut stdin).and_then(|()| stdin.flush())
        // Dropping stdin here tells ffmpeg the audio has ended.
    };
    let status = child.wait().context("ffmpeg did not finish")?;
    let errors = errors.join().unwrap_or_default();
    if !status.success() {
        bail!("ffmpeg failed: {}", errors.lines().last().unwrap_or("unknown error"));
    }
    fed.context("cannot send the audio to ffmpeg")
}

/// `.m4b` with chapters; if ffmpeg rejects that, the same audio as a plain `.m4a`.
fn encode_with_fallback(feed: &dyn Fn(&mut dyn Write) -> io::Result<()>, meta: &Path, m4b: &Path, m4a: &Path) -> anyhow::Result<PathBuf> {
    encode(feed, Some(meta), m4b).map(|()| m4b.to_path_buf()).or_else(|e| {
        log::warn!("M4B with chapters failed ({e:#}); retrying as .m4a without chapters");
        encode(feed, None, m4a).map(|()| m4a.to_path_buf())
    })
}

/// `(start_seconds, title)` for each section, laid end to end, and the total length.
fn chapter_starts(sections: &[SectionAudio]) -> (Vec<(f64, String)>, f64) {
    let mut now = 0.0;
    let starts = sections
        .iter()
        .map(|s| {
            let start = now;
            now += s.duration_seconds();
            (start, s.title.clone())
        })
        .collect();
    (starts, now)
}

/// Encodes `sections` (each with audio) next to `output`, one chapter each, and returns the file written.
fn write_audiobook(sections: &[SectionAudio], output: &Path, title: &str, author: &str) -> anyhow::Result<PathBuf> {
    let (chapters, total) = chapter_starts(sections);
    let tmp_meta = PathBuf::from(format!("{}.temp.meta.txt", output.display()));
    write_file(&tmp_meta, ffmetadata(title, author, &build_chapter_intervals(&chapters, total)))?;
    let parts = all_parts(sections);
    let feed = |w: &mut dyn Write| copy_parts(&parts, w);
    let result = encode_with_fallback(&feed, &tmp_meta, &output.with_extension("m4b"), &output.with_extension("m4a"));
    let _ = std::fs::remove_file(&tmp_meta);
    result
}

impl Exporter for M4bExporter {
    fn format_name(&self) -> &'static str {
        "M4B Audiobook"
    }
    fn format_key(&self) -> &'static str {
        "m4b"
    }
    fn file_extension(&self) -> &'static str {
        if has_ffmpeg() { ".m4b" } else { ".wav" }
    }
    fn is_available(&self) -> bool {
        GeminiClient::is_available()
    }

    fn export(&self, doc: &mut dyn Document, options: &ExportOptions, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<PathBuf> {
        let meta = doc.metadata();
        let title = or_default(meta.title, "Untitled");
        let author = or_default(meta.author, "Unknown");

        // Chapters come from the document structure; each is one audio segment and one marker.
        let sections = extract_chapters(doc);
        let chunks = audio::chunk_count(&sections);
        let total_steps = chunks + 1; // one per audio chunk, then encoding
        let pipeline = AudioPipeline::new(options.expressive, options.narrator);
        let audio = pipeline.generate_all_sections(&sections, &options.voice, cancel, &mut |c, _, m| progress(c, total_steps, m))?;
        // Chapters without text have no audio and no marker.
        let spoken: Vec<SectionAudio> = audio.iter().filter(|s| !s.is_empty()).cloned().collect();
        if spoken.is_empty() {
            bail!("No audio was generated: the document has no text to read.");
        }

        progress(chunks, total_steps, "Encoding audio...");
        let out = if has_ffmpeg() {
            write_audiobook(&spoken, &options.output_path, &title, &author)?
        } else {
            let wav_path = options.output_path.with_extension("wav");
            audio_parts::write_wav(&wav_path, &all_parts(&spoken))?;
            wav_path
        };
        pipeline.discard(&audio);
        progress(total_steps, total_steps, "Export complete");
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_tile_the_timeline_and_end_at_total() {
        let ch = vec![(0.0, "A".to_string()), (12.5, "B".to_string()), (20.0, "C".to_string())];
        assert_eq!(
            build_chapter_intervals(&ch, 31.25),
            vec![(0, 12500, "A".to_string()), (12500, 20000, "B".to_string()), (20000, 31250, "C".to_string())]
        );
    }

    fn sine_pcm(freq: f64, seconds: f64) -> Vec<u8> {
        let n = (24000.0 * seconds) as usize;
        (0..n).flat_map(|i| (((i as f64 * freq * std::f64::consts::TAU / 24000.0).sin() * 12000.0) as i16).to_le_bytes()).collect()
    }

    /// A section whose audio is `pcms`, saved as parts in `store`.
    fn section(store: &audio_parts::PartStore, title: &str, pcms: &[Vec<u8>]) -> SectionAudio {
        let parts = pcms.iter().enumerate().map(|(i, pcm)| store.put(&format!("{title}-{i}"), pcm).unwrap()).collect();
        SectionAudio { title: title.to_string(), parts }
    }

    #[test]
    fn chapters_start_where_the_previous_section_ends() {
        let dir = tempfile::tempdir().unwrap();
        let store = audio_parts::PartStore::open(dir.path()).unwrap();
        let sections = [section(&store, "A", &[vec![0; 48_000], vec![0; 24_000]]), section(&store, "B", &[vec![0; 96_000]])];
        let (starts, total) = chapter_starts(&sections);
        assert_eq!(starts, vec![(0.0, "A".to_string()), (1.5, "B".to_string())]);
        assert_eq!(total, 3.5);
    }

    #[test]
    fn write_audiobook_produces_chapters_ffprobe_can_read() {
        if !has_ffmpeg() || Command::new("ffprobe").arg("-version").creation_flags(CREATE_NO_WINDOW).output().is_err() {
            return;
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("export-validation");
        std::fs::create_dir_all(&dir).unwrap();
        let parts = tempfile::tempdir().unwrap();
        let store = audio_parts::PartStore::open(parts.path()).unwrap();
        // "Middle" is two parts, so chapter times must add up across part boundaries.
        let sections = [
            section(&store, "Intro", &[sine_pcm(330.0, 2.0)]),
            section(&store, "Middle = 2; #x", &[sine_pcm(440.0, 1.0), sine_pcm(440.0, 2.0)]),
            section(&store, "End", &[sine_pcm(550.0, 1.5)]),
        ];
        let out = write_audiobook(&sections, &dir.join("book.wav"), "My Book", "Me").unwrap();
        assert_eq!(out, dir.join("book.m4b"));
        assert!(!dir.join("book.wav.temp.meta.txt").exists());

        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-show_chapters", "-show_format", "-of", "json"])
            .arg(&out)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let got: Vec<(String, String)> = json["chapters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["tags"]["title"].as_str().unwrap().to_string(), c["start_time"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Intro".to_string(), "0.000000".to_string()),
                ("Middle = 2; #x".to_string(), "2.000000".to_string()),
                ("End".to_string(), "5.000000".to_string()),
            ]
        );
        assert_eq!(json["format"]["tags"]["title"], "My Book");
        let duration: f64 = json["format"]["duration"].as_str().unwrap().parse().unwrap();
        assert!((duration - 6.5).abs() < 0.2, "duration {duration}");
    }

    #[test]
    fn failed_chapter_encode_falls_back_to_m4a() {
        if !has_ffmpeg() {
            return;
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("export-validation");
        std::fs::create_dir_all(&dir).unwrap();
        let pcm = sine_pcm(440.0, 1.0);
        let feed = |w: &mut dyn Write| w.write_all(&pcm);
        let (m4b, m4a) = (dir.join("fallback.m4b"), dir.join("fallback.m4a"));
        let _ = std::fs::remove_file(&m4b);
        // A missing metadata file makes the chapter encode fail; the fallback re-sends the audio.
        let out = encode_with_fallback(&feed, &dir.join("no-such-meta.txt"), &m4b, &m4a).unwrap();
        assert_eq!(out, m4a);
        assert!(m4a.is_file() && !m4b.exists());
    }

    fn saved_parts() -> std::collections::BTreeSet<std::ffi::OsString> {
        std::fs::read_dir(audio_parts::parts_dir()).map(|d| d.flatten().map(|e| e.file_name()).collect()).unwrap_or_default()
    }

    #[test]
    #[ignore = "live Gemini TTS (about 5 minutes of audio); needs ffmpeg and a key"]
    fn live_export_cancels_keeps_its_parts_then_resumes() {
        use crate::document::{OpenOptions, registry};
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/text_toc.pdf");
        let mut doc = registry::open_document(&fixture, &OpenOptions::default()).unwrap();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("live-export");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let options = ExportOptions::new(dir.join("book.m4b"));
        let before = saved_parts();

        // Cancel once the first chunk is done: that chunk's part must stay on disk.
        let cancel = CancelToken::new();
        let err = M4bExporter.export(&mut *doc, &options, &cancel, &mut |c, _, _| if c == 1 { cancel.cancel() }).unwrap_err();
        assert!(err.downcast_ref::<crate::export::base::Cancelled>().is_some(), "{err:#}");
        let kept: Vec<_> = saved_parts().difference(&before).cloned().collect();
        // With parallel workers, a chunk already in flight when the cancel lands also finishes and is kept.
        assert!((1..=2).contains(&kept.len()), "finished parts kept: {kept:?}");

        // Exporting again finishes, reuses that part, and then deletes every part it used.
        let mut steps = Vec::new();
        let out = M4bExporter.export(&mut *doc, &options, &CancelToken::new(), &mut |c, t, m| steps.push(format!("{c}/{t} {m}"))).unwrap();
        assert_eq!(out, dir.join("book.m4b"));
        assert!(saved_parts().difference(&before).next().is_none(), "parts left behind");
        let probe = Command::new("ffprobe").args(["-v", "error", "-show_chapters", "-show_format", "-of", "json"]).arg(&out).output().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let chapters: Vec<&str> = json["chapters"].as_array().unwrap().iter().map(|c| c["tags"]["title"].as_str().unwrap()).collect();
        println!("steps: {steps:#?}\nchapters: {chapters:?}\nduration: {}", json["format"]["duration"]);
        assert!(!chapters.is_empty());
    }

    #[test]
    #[ignore = "live Gemini (casting, markup and TTS for about 3 minutes of audio); needs ffmpeg, a key and the trial's Pride and Prejudice excerpt"]
    fn live_expressive_export_makes_an_audiobook_and_cleans_up() {
        use crate::document::{OpenOptions, registry};
        // Shows the cast and any passage read plainly after failing the checks.
        let _ = simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default());
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let text = std::env::var("INTOME_EXPRESSIVE_TEXT").map(std::path::PathBuf::from).unwrap_or_else(|_| root.join("target/expressive_trial/books/pp_chapter1_short.txt"));
        let mode = match std::env::var("INTOME_EXPRESSIVE_MODE").as_deref() {
            Ok("dramatic") => crate::config::Expressiveness::Dramatic,
            _ => crate::config::Expressiveness::Subtle,
        };
        let narrator = match std::env::var("INTOME_EXPRESSIVE_NARRATOR").as_deref() {
            Ok("masculine") => crate::config::NarratorVoice::Masculine,
            _ => crate::config::NarratorVoice::Feminine,
        };
        let mut doc = registry::open_document(&text, &OpenOptions::default()).unwrap();
        let dir = root.join("target").join("live-export");
        std::fs::create_dir_all(&dir).unwrap();
        let mut options = ExportOptions::new(dir.join(format!("expressive-{mode:?}-{narrator:?}.m4b")));
        options.expressive = mode;
        options.narrator = narrator;
        let before = saved_parts();
        let mut steps = Vec::new();
        let out = M4bExporter.export(&mut *doc, &options, &CancelToken::new(), &mut |c, t, m| steps.push(format!("{c}/{t} {m}"))).unwrap();
        assert!(saved_parts().difference(&before).next().is_none(), "parts or the cast left behind");
        let probe = Command::new("ffprobe").args(["-v", "error", "-show_format", "-of", "json"]).arg(&out).output().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        println!("out: {}\nsteps: {steps:#?}\nduration: {}", out.display(), json["format"]["duration"]);
        assert!(steps[0].ends_with("Finding the characters in the book..."));
    }

    #[test]
    fn ffmetadata_escapes_special_characters() {
        let text = ffmetadata("A=B;C", "x", &[(0, 1000, "Ch #1".to_string())]);
        assert_eq!(
            text,
            ";FFMETADATA1\ntitle=A\\=B\\;C\nartist=x\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=Ch \\#1\n"
        );
    }
}
