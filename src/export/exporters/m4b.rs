//! M4B audiobook exporter with chapter markers.
//!
//! Synthesises each chapter with Gemini TTS, joins the PCM, and encodes AAC with the
//! `ffmpeg` CLI. Chapters travel in an ffmetadata file, which ffmpeg turns into MP4
//! chapter atoms (no tag library needed).
//!
//! - ffmpeg present: `.m4b` with chapters. If that encode fails, retry without chapters as `.m4a`.
//! - ffmpeg absent: the joined audio as `.wav`.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::ai::client::GeminiClient;
use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::audio::{AudioPipeline, concatenate_wav, extract_chapters, wav_duration_seconds};
use crate::export::base::{Cancelled, ExportOptions, Exporter, ProgressFn, write_file};
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

fn run_ffmpeg(args: &[&std::ffi::OsStr]) -> anyhow::Result<()> {
    let out = Command::new("ffmpeg").args(args).creation_flags(CREATE_NO_WINDOW).output().context("cannot run ffmpeg")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg failed: {}", err.lines().last().unwrap_or("unknown error"));
    }
    Ok(())
}

/// Encodes `wav` to AAC at `out`. With `meta`, the ffmetadata file supplies tags and chapters.
fn encode(wav: &Path, meta: Option<&Path>, out: &Path) -> anyhow::Result<()> {
    use std::ffi::OsStr;
    let mut args: Vec<&OsStr> = vec!["-y".as_ref(), "-i".as_ref(), wav.as_os_str()];
    if let Some(m) = meta {
        args.extend(["-i".as_ref(), m.as_os_str(), "-map_metadata".as_ref(), "1".as_ref(), "-map_chapters".as_ref(), "1".as_ref()]);
    }
    args.extend(["-c:a".as_ref(), "aac".as_ref(), "-b:a".as_ref(), "64k".as_ref(), out.as_os_str()]);
    run_ffmpeg(&args)
}

/// `.m4b` with chapters; if ffmpeg rejects that, the same audio as a plain `.m4a`.
fn encode_with_fallback(wav: &Path, meta: &Path, m4b: &Path, m4a: &Path) -> anyhow::Result<PathBuf> {
    encode(wav, Some(meta), m4b).map(|()| m4b.to_path_buf()).or_else(|e| {
        log::warn!("M4B with chapters failed ({e:#}); retrying as .m4a without chapters");
        encode(wav, None, m4a).map(|()| m4a.to_path_buf())
    })
}

/// Writes the combined audio next to `output`, encodes it, and returns the file written.
fn write_audiobook(combined_wav: &[u8], output: &Path, title: &str, author: &str, chapters: &[(f64, String)]) -> anyhow::Result<PathBuf> {
    let total = wav_duration_seconds(combined_wav);
    let m4b = output.with_extension("m4b");
    let tmp_wav = PathBuf::from(format!("{}.temp.wav", output.display()));
    let tmp_meta = PathBuf::from(format!("{}.temp.meta.txt", output.display()));
    write_file(&tmp_wav, combined_wav)?;
    write_file(&tmp_meta, ffmetadata(title, author, &build_chapter_intervals(chapters, total)))?;

    let result = encode_with_fallback(&tmp_wav, &tmp_meta, &m4b, &output.with_extension("m4a"));
    let _ = std::fs::remove_file(&tmp_wav);
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
        let total_steps = sections.len() + 1; // audio per chapter, then combining
        let audio = AudioPipeline::default().generate_all_sections(&sections, &options.voice, cancel, &mut |c, _, m| progress(c, total_steps, m))?;
        if cancel.is_cancelled() {
            return Err(Cancelled.into());
        }

        let mut wavs = Vec::new();
        let mut chapters = Vec::new();
        let mut now = 0.0;
        for (section_title, wav) in audio {
            if !wav.is_empty() {
                chapters.push((now, section_title));
                now += wav_duration_seconds(&wav);
                wavs.push(wav);
            }
        }
        if wavs.is_empty() {
            bail!("No audio was generated; every section failed or was empty. See the log for details.");
        }

        progress(sections.len(), total_steps, "Combining audio...");
        // Join PCM under one header, not WAVs with headers, to avoid clicks at the seams.
        let combined = concatenate_wav(&wavs);

        let out = if has_ffmpeg() {
            write_audiobook(&combined, &options.output_path, &title, &author, &chapters)?
        } else {
            let wav_path = options.output_path.with_extension("wav");
            write_file(&wav_path, &combined)?;
            wav_path
        };
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

    fn sine_wav(freq: f64, seconds: f64) -> Vec<u8> {
        let n = (24000.0 * seconds) as usize;
        let pcm: Vec<u8> = (0..n)
            .flat_map(|i| (((i as f64 * freq * std::f64::consts::TAU / 24000.0).sin() * 12000.0) as i16).to_le_bytes())
            .collect();
        crate::export::audio::pcm_to_wav(&pcm)
    }

    #[test]
    fn write_audiobook_produces_chapters_ffprobe_can_read() {
        if !has_ffmpeg() || Command::new("ffprobe").arg("-version").creation_flags(CREATE_NO_WINDOW).output().is_err() {
            return;
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("export-validation");
        std::fs::create_dir_all(&dir).unwrap();
        let wavs = [sine_wav(330.0, 2.0), sine_wav(440.0, 3.0), sine_wav(550.0, 1.5)];
        let combined = concatenate_wav(&wavs);
        let chapters = [(0.0, "Intro".to_string()), (2.0, "Middle = 2; #x".to_string()), (5.0, "End".to_string())];
        let out = write_audiobook(&combined, &dir.join("book.wav"), "My Book", "Me", &chapters).unwrap();
        assert_eq!(out, dir.join("book.m4b"));
        assert!(!dir.join("book.wav.temp.wav").exists() && !dir.join("book.wav.temp.meta.txt").exists());

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
        let wav = dir.join("fallback.wav");
        std::fs::write(&wav, sine_wav(440.0, 1.0)).unwrap();
        let (m4b, m4a) = (dir.join("fallback.m4b"), dir.join("fallback.m4a"));
        let _ = std::fs::remove_file(&m4b);
        // A missing metadata file makes the chapter encode fail.
        let out = encode_with_fallback(&wav, &dir.join("no-such-meta.txt"), &m4b, &m4a).unwrap();
        assert_eq!(out, m4a);
        assert!(m4a.is_file() && !m4b.exists());
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
