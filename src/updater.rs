//! In-app update core (port of the non-UI half of prism/updater.py).
//!
//! An unauthenticated HTTPS GET of a small JSON manifest names the latest
//! version and an installer URL. The GUI builds the prompts and the progress
//! dialog on top of these functions; everything here is blocking, so call it
//! from a worker thread.
//!
//! Security model, as in Prism: HTTPS only for the manifest and the installer
//! (also across redirects), TLS verification always on, no credentials, the
//! installer's SHA-256 is checked when the manifest carries one, and the
//! installer is started with an argument list, never through a shell.

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::cancel::CancelToken;
use crate::constants::{APP_NAME, APP_VERSION};

/// Public, unauthenticated URL of the version manifest JSON. Placeholder: replace the
/// host with the real update server. Must be https.
pub const MANIFEST_URL: &str = "https://updates.example.com/intome/latest.json";

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// A manifest is tiny; anything larger is refused.
const MANIFEST_MAX_BYTES: u64 = 64 * 1024;
const DOWNLOAD_CHUNK: usize = 64 * 1024;

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

fn user_agent() -> String {
    format!("{APP_NAME}/{APP_VERSION} (auto-update)")
}

fn agent(timeout: bool) -> ureq::Agent {
    let builder = ureq::Agent::config_builder().https_only(true).user_agent(user_agent());
    // The download has no overall limit (installers are large); only connecting and the first response are bounded.
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
    /// Lowercase hex SHA-256 of the installer, when the manifest provides one.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    UpToDate,
    Available(UpdateInfo),
}

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("could not fetch the update manifest: {0:#}")]
    Failed(anyhow::Error),
    #[error("the update manifest lacks latest_version or download_url")]
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

/// Fetches the manifest at [`MANIFEST_URL`] and compares it with the running version.
pub fn check_for_updates() -> Result<CheckOutcome, CheckError> {
    let manifest = fetch_manifest(MANIFEST_URL).map_err(CheckError::Failed)?;
    evaluate_manifest(&manifest, APP_VERSION)
}

/// Decides what a manifest means for a build running `local_version`.
pub fn evaluate_manifest(manifest: &serde_json::Value, local_version: &str) -> Result<CheckOutcome, CheckError> {
    let field = |key: &str| manifest.get(key).and_then(|v| v.as_str()).map(str::trim).unwrap_or_default();
    let (latest, url) = (field("latest_version"), field("download_url"));
    if latest.is_empty() || url.is_empty() {
        return Err(CheckError::Incomplete);
    }
    if !is_newer(latest, local_version) {
        return Ok(CheckOutcome::UpToDate);
    }
    // A plain-http installer URL defeats the point even when the manifest itself came over HTTPS.
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err(CheckError::InsecureDownloadUrl);
    }
    let sha256 = Some(field("sha256").to_ascii_lowercase()).filter(|s| !s.is_empty());
    Ok(CheckOutcome::Available(UpdateInfo { version: latest.to_string(), download_url: url.to_string(), sha256 }))
}

/// HTTPS GET of the manifest JSON. Verifies TLS, sends no credentials, caps the size.
pub fn fetch_manifest(url: &str) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(url.to_ascii_lowercase().starts_with("https://"), "manifest URL must be https: {url:?}");
    let mut resp = agent(true).get(url).call()?;
    let raw = resp.body_mut().with_config().limit(MANIFEST_MAX_BYTES).read_to_vec()?;
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

/// A safe local `.exe` file name derived from the URL; the URL's own name is never trusted raw.
pub fn safe_installer_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let path = path.split_once("://").map_or(path, |(_, rest)| rest.split_once('/').map_or("", |(_, p)| p));
    let base: String = path.rsplit('/').next().unwrap_or_default().chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect();
    if base.to_ascii_lowercase().ends_with(".exe") { base } else { format!("{APP_NAME}_Setup.exe") }
}

/// Downloads the installer to the temp directory and returns its path. `progress`
/// receives (downloaded, total) bytes; total is 0 when the server sent no length.
pub fn download(url: &str, sha256: Option<&str>, progress: impl FnMut(u64, u64), cancel: &CancelToken) -> Result<PathBuf, DownloadError> {
    let dest = std::env::temp_dir().join(safe_installer_name(url));
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
            None => log::warn!("manifest provided no sha256; installer integrity is unverified"),
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

/// Starts the installer detached so it outlives this process (`/SILENT` is Inno Setup's flag).
/// The caller then exits the app so the installer can replace the files.
pub fn launch_installer(path: &Path) -> std::io::Result<()> {
    Command::new(path).arg("/SILENT").creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).spawn().map(drop)
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

    #[test]
    fn installer_names_are_sanitised() {
        assert_eq!(safe_installer_name("https://x.org/dl/InTome-1.2.exe"), "InTome-1.2.exe");
        assert_eq!(safe_installer_name("https://x.org/dl/in%20tome (1).EXE?token=a/b"), "in_20tome__1_.EXE");
        assert_eq!(safe_installer_name("https://x.org/dl/setup.msi"), "InTome_Setup.exe");
        assert_eq!(safe_installer_name("https://x.org/"), "InTome_Setup.exe");
        assert_eq!(safe_installer_name("https://x.org/a/..%5C..%5Cevil.exe"), ".._5C.._5Cevil.exe");
    }

    #[test]
    fn manifest_evaluation_follows_prisms_order_of_checks() {
        let m = |v: serde_json::Value| evaluate_manifest(&v, "0.1.0");
        assert!(matches!(m(json!({"latest_version": "0.1.0", "download_url": "http://x"})), Ok(CheckOutcome::UpToDate)));
        assert!(matches!(m(json!({"latest_version": "0.2.0"})), Err(CheckError::Incomplete)));
        assert!(matches!(m(json!({"latest_version": "0.2.0", "download_url": "http://x/a.exe"})), Err(CheckError::InsecureDownloadUrl)));
        let Ok(CheckOutcome::Available(info)) = m(json!({"latest_version": "0.2.0", "download_url": "https://x/a.exe", "sha256": "ABCD"})) else { panic!() };
        assert_eq!((info.version.as_str(), info.sha256.as_deref()), ("0.2.0", Some("abcd")));
    }

    // sha256("abc")
    const ABC_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn download_verifies_sha256_and_reports_progress() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("setup.exe");
        let mut seen = Vec::new();
        copy_verified(&b"abc"[..], 3, &dest, Some(&ABC_SHA.to_uppercase()), |d, t| seen.push((d, t)), &CancelToken::new()).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"abc");
        assert_eq!(seen, [(3, 3)]);
    }

    #[test]
    fn checksum_mismatch_and_cancel_remove_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("setup.exe");
        let err = copy_verified(&b"abd"[..], 3, &dest, Some(ABC_SHA), |_, _| {}, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, DownloadError::ChecksumMismatch { ref expected, .. } if expected == ABC_SHA));
        assert!(!dest.exists());

        let cancel = CancelToken::new();
        cancel.cancel();
        let err = copy_verified(&b"abc"[..], 3, &dest, None, |_, _| {}, &cancel).unwrap_err();
        assert!(matches!(err, DownloadError::Cancelled));
        assert!(!dest.exists());
    }
}
