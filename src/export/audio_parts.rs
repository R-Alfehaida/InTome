//! Finished TTS chunks kept on disk while an audio export runs.
//!
//! Each chunk's PCM is written as soon as Gemini returns it, so an export holds
//! about one chunk in memory however long the book is, and an export that
//! fails or is cancelled resumes from the chunks it already made. A chunk's
//! file name is a hash of everything that shaped its audio (model, voice,
//! text), so a changed setting never reuses stale audio. A successful export
//! deletes its chunks; leftovers from abandoned exports expire after
//! [`MAX_AGE`]. Prism kept the whole book in memory instead.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::constants;

/// How long a saved chunk from an unfinished export is kept.
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Bumped when the stored format changes, so older files are never read as current.
const FORMAT: &str = "pcm-s16le-24k-mono-v1";

/// The folder holding saved chunks, inside InTome's cache folder.
pub fn parts_dir() -> PathBuf {
    constants::cache_dir().join("tts")
}

/// One synthesised chunk on disk: raw 16-bit 24 kHz mono PCM.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub path: PathBuf,
    pub bytes: u64,
}

pub struct PartStore {
    dir: PathBuf,
}

impl PartStore {
    /// The store in `dir`, created if missing, with expired parts removed.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let store = Self { dir: dir.into() };
        std::fs::create_dir_all(&store.dir)?;
        store.prune(MAX_AGE);
        Ok(store)
    }

    /// The key for a chunk: SHA-256 over everything that shapes its audio.
    pub fn key(model: &str, voice: &str, text: &str) -> String {
        hex::encode(Sha256::digest(format!("{FORMAT}\0{model}\0{voice}\0{text}").as_bytes()))
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.pcm"))
    }

    /// The saved part for `key`, if a finished one exists.
    pub fn get(&self, key: &str) -> Option<Part> {
        let path = self.path(key);
        let bytes = std::fs::metadata(&path).ok().filter(|m| m.is_file())?.len();
        Some(Part { path, bytes })
    }

    /// Saves `pcm` under `key`. It is written to a temporary name and renamed,
    /// so a crash mid-write never leaves a truncated part that a later export
    /// would reuse.
    pub fn put(&self, key: &str, pcm: &[u8]) -> io::Result<Part> {
        let path = self.path(key);
        let tmp = self.dir.join(format!("{key}.tmp"));
        std::fs::write(&tmp, pcm)?;
        std::fs::rename(&tmp, &path)?;
        Ok(Part { path, bytes: pcm.len() as u64 })
    }

    /// Removes parts and leftover temporary files not modified within `max_age`.
    pub fn prune(&self, max_age: Duration) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).unwrap_or_default() > max_age);
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Deletes `parts`, for when the export that made them has succeeded.
pub fn discard(parts: &[Part]) {
    for part in parts {
        let _ = std::fs::remove_file(&part.path);
    }
}

/// Total size of the saved parts, for the Clear Cache prompt.
pub fn saved_bytes() -> u64 {
    std::fs::read_dir(parts_dir())
        .map(|entries| entries.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

/// Deletes every saved part.
pub fn clear_saved() {
    let _ = std::fs::remove_dir_all(parts_dir());
}

/// Appends each part's PCM to `out`, in order, a buffer at a time.
pub fn copy_parts(parts: &[Part], out: &mut dyn Write) -> io::Result<()> {
    for part in parts {
        io::copy(&mut File::open(&part.path)?, out)?;
    }
    Ok(())
}

/// The 44-byte header of a PCM WAV file holding `data_len` bytes.
pub fn wav_header(data_len: u32, sample_rate: u32, bits_per_sample: u16, channels: u16) -> [u8; 44] {
    let block_align = channels * (bits_per_sample / 8);
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36u32.saturating_add(data_len)).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    h[32..34].copy_from_slice(&block_align.to_le_bytes());
    h[34..36].copy_from_slice(&bits_per_sample.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// Writes `parts` as one 24 kHz 16-bit mono WAV at `path`, streaming from disk.
/// A WAV cannot hold more than 4 GB (about 24 hours of this audio).
pub fn write_wav(path: &Path, parts: &[Part]) -> anyhow::Result<()> {
    use anyhow::Context;
    let total: u64 = parts.iter().map(|p| p.bytes).sum();
    let data_len = u32::try_from(total).ok().filter(|n| *n <= u32::MAX - 36).with_context(|| {
        format!("the audio is {:.1} GB, more than a WAV file can hold; install ffmpeg to export it as M4B", total as f64 / 1e9)
    })?;
    let mut out = io::BufWriter::new(File::create(path).with_context(|| format!("cannot write {}", path.display()))?);
    out.write_all(&wav_header(data_len, super::audio::TTS_SAMPLE_RATE, 16, 1))?;
    copy_parts(parts, &mut out)?;
    out.flush().with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, PartStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = PartStore::open(dir.path().join("tts")).unwrap();
        (dir, store)
    }

    #[test]
    fn key_changes_with_model_voice_and_text() {
        let k = PartStore::key("m", "Kore", "Hello.");
        assert_eq!(k.len(), 64);
        assert_eq!(k, PartStore::key("m", "Kore", "Hello."));
        assert_ne!(k, PartStore::key("m2", "Kore", "Hello."));
        assert_ne!(k, PartStore::key("m", "Puck", "Hello."));
        assert_ne!(k, PartStore::key("m", "Kore", "Hello!"));
    }

    #[test]
    fn put_then_get_and_discard() {
        let (_dir, s) = store();
        assert_eq!(s.get("abc"), None);
        let part = s.put("abc", &[1, 2, 3, 4]).unwrap();
        assert_eq!(s.get("abc"), Some(part.clone()));
        assert_eq!(part.bytes, 4);
        assert!(!part.path.with_extension("tmp").exists());
        discard(&[part]);
        assert_eq!(s.get("abc"), None);
    }

    #[test]
    fn prune_removes_only_old_files() {
        let (_dir, s) = store();
        s.put("new", &[0; 8]).unwrap();
        s.prune(Duration::from_secs(3600));
        assert!(s.get("new").is_some());
        s.prune(Duration::ZERO);
        assert!(s.get("new").is_none());
    }

    #[test]
    fn wav_streams_parts_in_order_under_one_header() {
        let (dir, s) = store();
        let parts = [s.put("a", &[1, 2]).unwrap(), s.put("b", &[3, 4, 5, 6]).unwrap()];
        let path = dir.path().join("out.wav");
        write_wav(&path, &parts).unwrap();
        let wav = std::fs::read(&path).unwrap();
        assert_eq!(&wav[..44], &wav_header(6, 24_000, 16, 1));
        assert_eq!(&wav[44..], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 42);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 48_000);
    }
}
