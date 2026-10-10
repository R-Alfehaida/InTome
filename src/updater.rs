//! In-app update core (port of the non-UI half of prism/updater.py).
//!
//! An unauthenticated HTTPS GET of the latest GitHub release names the newest
//! version and its portable zip. The GUI builds the prompts and the progress
//! dialog on top of these functions; everything here is blocking, so call it
//! from a worker thread.
//!
//! Security model: HTTPS only for the release lookup and the zip (also across
//! redirects), TLS verification always on, no credentials, the zip's SHA-256
//! is checked against the digest GitHub reports for the asset, the zip is
//! extracted with path-traversal checks, and the app's files are swapped in
//! place with a rollback so a failed update never leaves a half-replaced install.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::cancel::CancelToken;
use crate::constants::{APP_NAME, APP_VERSION};

/// Public, unauthenticated URL of the latest published release (drafts and prereleases are skipped). Must be https.
pub const RELEASES_URL: &str = "https://api.github.com/repos/R-Alfehaida/InTome/releases/latest";

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// The release body carries the release notes, so this is larger than a bare manifest would need.
const RELEASE_MAX_BYTES: u64 = 1024 * 1024;
const DOWNLOAD_CHUNK: usize = 64 * 1024;

/// Suffix of the copy of a replaced file; Windows lets a running exe or loaded DLL be renamed but not deleted.
const REPLACED_SUFFIX: &str = ".update-old";

static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

fn user_agent() -> String {
    format!("{APP_NAME}/{APP_VERSION} (auto-update)")
}

fn agent(timeout: bool) -> ureq::Agent {
    let builder = ureq::Agent::config_builder().https_only(true).user_agent(user_agent());
    // The download has no overall limit (the zip is large); only connecting and the first response are bounded.
    let builder = if timeout {
        builder.timeout_global(Some(HTTP_TIMEOUT))
    } else {
        builder.timeout_connect(Some(HTTP_TIMEOUT)).timeout_recv_response(Some(HTTP_TIMEOUT))
    };
    builder.build().into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: String,
    pub download_url: String,
    /// Lowercase hex SHA-256 of the zip, when GitHub reports a digest for the asset.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    UpToDate,
    Available(UpdateInfo),
}

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("could not fetch the latest release: {0:#}")]
    Failed(anyhow::Error),
    #[error("the latest release lacks a version tag or its zip asset")]
    Incomplete,
    #[error("the update download link is not HTTPS")]
    InsecureDownloadUrl,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("download cancelled")]
    Cancelled,
    #[error("checksum mismatch (expected {expected}, got {actual})")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("download failed: {0:#}")]
    Failed(anyhow::Error),
}

/// Fetches the latest release at [`RELEASES_URL`] and compares it with the running version.
pub fn check_for_updates() -> Result<CheckOutcome, CheckError> {
    let release = fetch_release(RELEASES_URL).map_err(CheckError::Failed)?;
    evaluate_release(&release, APP_VERSION)
}

/// Decides what a GitHub release means for a build running `local_version`.
pub fn evaluate_release(release: &serde_json::Value, local_version: &str) -> Result<CheckOutcome, CheckError> {
    let tag = release.get("tag_name").and_then(|v| v.as_str()).unwrap_or_default();
    let version = tag.trim().trim_start_matches(['v', 'V']);
    if version.is_empty() {
        return Err(CheckError::Incomplete);
    }
    if !is_newer(version, local_version) {
        return Ok(CheckOutcome::UpToDate);
    }
    let wanted = format!("{APP_NAME}-{version}.zip");
    let asset = release
        .get("assets")
        .and_then(|v| v.as_array())
        .and_then(|assets| assets.iter().find(|a| a["name"].as_str() == Some(&wanted)))
        .ok_or(CheckError::Incomplete)?;
    let url = asset["browser_download_url"].as_str().unwrap_or_default().trim();
    // A plain-http download URL defeats the point even when the release lookup itself came over HTTPS.
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err(CheckError::InsecureDownloadUrl);
    }
    let sha256 = asset["digest"].as_str().and_then(|d| d.trim().strip_prefix("sha256:")).map(str::to_ascii_lowercase).filter(|s| !s.is_empty());
    Ok(CheckOutcome::Available(UpdateInfo { version: version.to_string(), download_url: url.to_string(), sha256 }))
}

/// HTTPS GET of the release JSON. Verifies TLS, sends no credentials, caps the size.
pub fn fetch_release(url: &str) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(url.to_ascii_lowercase().starts_with("https://"), "release URL must be https: {url:?}");
    let mut resp = agent(true).get(url).call()?;
    let raw = resp.body_mut().with_config().limit(RELEASE_MAX_BYTES).read_to_vec()?;
    Ok(serde_json::from_slice(&raw)?)
}

/// Turns `1.2.3` or `v1.2.3-beta` into its leading numeric parts: `[1, 2, 3]`.
pub fn parse_version(version: &str) -> Vec<u64> {
    let mut nums = Vec::new();
    for part in version.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']) {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        match digits.parse() {
            Ok(n) => nums.push(n),
            Err(_) => break,
        }
    }
    nums
}

/// True when `remote` is a higher version than `local`. Missing trailing parts count as 0.
pub fn is_newer(remote: &str, local: &str) -> bool {
    let (mut r, mut l) = (parse_version(remote), parse_version(local));
    let width = r.len().max(l.len());
    r.resize(width, 0);
    l.resize(width, 0);
    r > l
}

/// Downloads the release zip to the temp directory and returns its path. `progress`
/// receives (downloaded, total) bytes; total is 0 when the server sent no length.
pub fn download(url: &str, sha256: Option<&str>, progress: impl FnMut(u64, u64), cancel: &CancelToken) -> Result<PathBuf, DownloadError> {
    // A fixed name: the URL's own file name is never trusted.
    let dest = std::env::temp_dir().join(format!("{APP_NAME}-update.zip"));
    let resp = agent(false).get(url).call().map_err(|e| DownloadError::Failed(e.into()))?;
    let total = resp.body().content_length().unwrap_or(0);
    copy_verified(resp.into_body().into_reader(), total, &dest, sha256, progress, cancel)?;
    Ok(dest)
}

/// Streams `src` into `dest`, hashing as it goes. On cancel, error or checksum mismatch the partial file is removed.
fn copy_verified(mut src: impl Read, total: u64, dest: &Path, sha256: Option<&str>, mut progress: impl FnMut(u64, u64), cancel: &CancelToken) -> Result<(), DownloadError> {
    let result = (|| {
        let mut file = File::create(dest).map_err(|e| DownloadError::Failed(e.into()))?;
        let mut digest = Sha256::new();
        let mut buf = vec![0u8; DOWNLOAD_CHUNK];
        let mut downloaded = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }
            let n = src.read(&mut buf).map_err(|e| DownloadError::Failed(e.into()))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| DownloadError::Failed(e.into()))?;
            digest.update(&buf[..n]);
            downloaded += n as u64;
            progress(downloaded, total);
        }
        file.flush().map_err(|e| DownloadError::Failed(e.into()))?;
        match sha256 {
            Some(expected) => {
                let actual = hex::encode(digest.finalize());
                let expected = expected.trim().to_ascii_lowercase();
                if actual != expected {
                    return Err(DownloadError::ChecksumMismatch { expected, actual });
                }
            }
            None => log::warn!("release provided no sha256; zip integrity is unverified"),
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

/// Installs the package in `zip` over `app_dir`. Each file is renamed to `<name>.update-old` and
/// replaced by the new copy; on any error every change is undone before the error is returned.
pub fn apply_update(zip: &Path, app_dir: &Path) -> anyhow::Result<()> {
    let staging = tempfile::tempdir()?;
    zip::ZipArchive::new(File::open(zip)?)?.extract(staging.path())?;
    let root = package_root(staging.path())?;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            names.push(entry.file_name());
        }
    }
    names.sort();

    // (target, set-aside copy) for every file touched so far, so a failure can restore the old install.
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let result = names.iter().try_for_each(|name| -> anyhow::Result<()> {
        let target = app_dir.join(name);
        let mut old = None;
        if target.exists() {
            let path = app_dir.join(format!("{}{REPLACED_SUFFIX}", name.to_string_lossy()));
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
            std::fs::rename(&target, &path)?;
            old = Some(path);
        }
        done.push((target.clone(), old));
        std::fs::copy(root.join(name), &target)?;
        Ok(())
    });
    if result.is_err() {
        for (target, old) in done.iter().rev() {
            let _ = std::fs::remove_file(target);
            if let Some(old) = old {
                let _ = std::fs::rename(old, target);
            }
        }
    }
    result
}

/// The extracted directory that holds `intome.exe`: `dir` itself or one folder below it.
fn package_root(dir: &Path) -> anyhow::Result<PathBuf> {
    let has_exe = |d: &Path| d.join("intome.exe").is_file();
    if has_exe(dir) {
        return Ok(dir.to_path_buf());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() && has_exe(&path) {
            return Ok(path);
        }
    }
    anyhow::bail!("not an {APP_NAME} package")
}

/// Best-effort delete of the files `apply_update` set aside. The old process still holds them while it runs, so the next start retries.
pub fn remove_replaced_files(app_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(app_dir) else { return };
    for path in entries.flatten().map(|e| e.path()) {
        if path.is_file() && path.to_string_lossy().ends_with(REPLACED_SUFFIX) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Asks the app to start a fresh copy of itself once its window has closed.
pub fn request_restart() {
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn restart_requested() -> bool {
    RESTART_REQUESTED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_parse_leading_numbers_and_compare_with_padding() {
        assert_eq!(parse_version("1.2.3"), [1, 2, 3]);
        assert_eq!(parse_version(" v1.2.3-beta"), [1, 2, 3]);
        assert_eq!(parse_version("2.0.1rc1+build"), [2, 0, 1]);
        assert_eq!(parse_version("beta"), Vec::<u64>::new());
        assert!(is_newer("0.2", "0.1.0"));
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(!is_newer("1.0", "1.0.0"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("v2", "1.99.99"));
    }

    fn release(tag: &str, asset_name: &str, url: &str) -> serde_json::Value {
        json!({"tag_name": tag, "assets": [
            {"name": "other.zip", "browser_download_url": "https://x/other.zip"},
            {"name": asset_name, "browser_download_url": url, "digest": "sha256:ABCD"},
        ]})
    }

    #[test]
    fn release_up_to_date_when_the_tag_is_not_newer() {
        let r = release("v0.1.0", "InTome-0.1.0.zip", "https://x/a.zip");
        assert!(matches!(evaluate_release(&r, "0.1.0"), Ok(CheckOutcome::UpToDate)));
    }

    #[test]
    fn newer_release_offers_its_zip_with_a_lowercase_digest() {
        let r = release("v0.2.0", "InTome-0.2.0.zip", "https://x/InTome-0.2.0.zip");
        let Ok(CheckOutcome::Available(info)) = evaluate_release(&r, "0.1.0") else { panic!() };
        assert_eq!(info, UpdateInfo { version: "0.2.0".into(), download_url: "https://x/InTome-0.2.0.zip".into(), sha256: Some("abcd".into()) });
    }

    #[test]
    fn newer_release_without_the_matching_zip_is_incomplete() {
        let r = release("v0.2.0", "InTome-0.3.0.zip", "https://x/a.zip");
        assert!(matches!(evaluate_release(&r, "0.1.0"), Err(CheckError::Incomplete)));
        assert!(matches!(evaluate_release(&json!({"tag_name": "v0.2.0"}), "0.1.0"), Err(CheckError::Incomplete)));
    }

    #[test]
    fn release_with_an_http_zip_url_is_blocked() {
        let r = release("v0.2.0", "InTome-0.2.0.zip", "http://x/InTome-0.2.0.zip");
        assert!(matches!(evaluate_release(&r, "0.1.0"), Err(CheckError::InsecureDownloadUrl)));
    }

    #[test]
    fn release_without_a_tag_is_incomplete() {
        assert!(matches!(evaluate_release(&json!({"assets": []}), "0.1.0"), Err(CheckError::Incomplete)));
        assert!(matches!(evaluate_release(&json!({"tag_name": " v "}), "0.1.0"), Err(CheckError::Incomplete)));
    }

    // sha256("abc")
    const ABC_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn download_verifies_sha256_and_reports_progress() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("update.zip");
        let mut seen = Vec::new();
        copy_verified(&b"abc"[..], 3, &dest, Some(&ABC_SHA.to_uppercase()), |d, t| seen.push((d, t)), &CancelToken::new()).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        assert_eq!(seen, [(3, 3)]);
    }

    #[test]
    fn checksum_mismatch_and_cancel_remove_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("update.zip");
        let err = copy_verified(&b"abd"[..], 3, &dest, Some(ABC_SHA), |_, _| {}, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, DownloadError::ChecksumMismatch { ref expected, .. } if expected == ABC_SHA));
        assert!(!dest.exists());

        let cancel = CancelToken::new();
        cancel.cancel();
        let err = copy_verified(&b"abc"[..], 3, &dest, None, |_, _| {}, &cancel).unwrap_err();
        assert!(matches!(err, DownloadError::Cancelled));
        assert!(!dest.exists());
    }

    fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn apply_update_swaps_files_in_place_and_keeps_the_old_ones_until_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("update.zip");
        make_zip(&zip, &[("InTome-9.9.9/intome.exe", b"new"), ("InTome-9.9.9/pdfium.dll", b"dll")]);
        let app = tmp.path().join("app");
        std::fs::create_dir(&app).unwrap();
        std::fs::write(app.join("intome.exe"), b"old").unwrap();
        std::fs::write(app.join("intome.toml"), b"settings").unwrap();

        apply_update(&zip, &app).unwrap();
        assert_eq!(std::fs::read(app.join("intome.exe")).unwrap(), b"new");
        assert_eq!(std::fs::read(app.join("pdfium.dll")).unwrap(), b"dll");
        assert_eq!(std::fs::read(app.join("intome.exe.update-old")).unwrap(), b"old");
        assert_eq!(std::fs::read(app.join("intome.toml")).unwrap(), b"settings");

        remove_replaced_files(&app);
        assert_eq!(names_in(&app), ["intome.exe", "intome.toml", "pdfium.dll"]);
    }

    #[test]
    fn apply_update_rolls_back_every_file_when_a_later_one_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("update.zip");
        make_zip(&zip, &[("InTome-9.9.9/intome.exe", b"new"), ("InTome-9.9.9/pdfium.dll", b"dll")]);
        let app = tmp.path().join("app");
        std::fs::create_dir_all(app.join("pdfium.dll.update-old")).unwrap();
        std::fs::write(app.join("pdfium.dll.update-old").join("keep"), b"x").unwrap();
        std::fs::write(app.join("intome.exe"), b"old").unwrap();
        std::fs::write(app.join("pdfium.dll"), b"old dll").unwrap();

        assert!(apply_update(&zip, &app).is_err());
        assert_eq!(std::fs::read(app.join("intome.exe")).unwrap(), b"old");
        assert_eq!(std::fs::read(app.join("pdfium.dll")).unwrap(), b"old dll");
        assert_eq!(names_in(&app), ["intome.exe", "pdfium.dll", "pdfium.dll.update-old"]);
    }

    #[test]
    fn apply_update_refuses_a_zip_without_intome_exe() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("update.zip");
        make_zip(&zip, &[("Other/readme.txt", b"hi")]);
        let app = tmp.path().join("app");
        std::fs::create_dir(&app).unwrap();
        std::fs::write(app.join("intome.exe"), b"old").unwrap();

        let err = apply_update(&zip, &app).unwrap_err();
        assert_eq!(err.to_string(), "not an InTome package");
        assert_eq!(names_in(&app), ["intome.exe"]);
        assert_eq!(std::fs::read(app.join("intome.exe")).unwrap(), b"old");
    }
}
