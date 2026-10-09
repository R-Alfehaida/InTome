//! Document conversion cache (replaces diskcache): a SQLite key/value table in
//! `%APPDATA%\InTome\cache\cache.db` with per-entry expiry and a total size cap.
//!
//! Keys are `"{file_hash}:{suffix}"`. Values are JSON. Honours the
//! `document.cache_enabled` setting: when off, lookups miss and stores no-op.
//! The size cap is read at first use, so a change applies after restart.

use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::{config, constants};

pub const DEFAULT_EXPIRE_SECS: i64 = 7 * 24 * 3600;

struct Cache {
    conn: Connection,
    size_limit: i64,
}

static CACHE: LazyLock<Mutex<Option<Cache>>> = LazyLock::new(|| Mutex::new(open()));

fn open() -> Option<Cache> {
    let dir = constants::cache_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let conn = Connection::open(dir.join("cache.db"))
        .map_err(|e| log::warn!("cache disabled: {e}"))
        .ok()?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS entries (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL,
             size INTEGER NOT NULL,
             expires_at INTEGER NOT NULL,
             accessed_at INTEGER NOT NULL
         );",
    )
    .ok()?;
    let size_limit = (config::get().document.cache_max_size_mb as i64) * 1024 * 1024;
    Some(Cache { conn, size_limit })
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Hash key for a file: SHA-256 of absolute path + size + mtime, first 32 hex chars.
/// Also the annotation DB's `document_hash`.
pub fn file_hash(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let (size, mtime) = std::fs::metadata(path)
        .map(|m| {
            let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            (m.len(), mtime)
        })
        .unwrap_or((0, 0.0));
    let digest = Sha256::digest(format!("{}:{}:{}", abs.display(), size, mtime).as_bytes());
    hex::encode(digest)[..32].to_string()
}

fn key(path: &Path, suffix: &str) -> String {
    format!("{}:{}", file_hash(path), suffix)
}

/// Cached value for `path` + `suffix`, or `None` on miss, expiry, or cache off.
pub fn get_cached<T: DeserializeOwned>(path: &Path, suffix: &str) -> Option<T> {
    if !config::get().document.cache_enabled {
        return None;
    }
    let k = key(path, suffix);
    let guard = CACHE.lock().unwrap();
    let cache = guard.as_ref()?;
    let value: Option<String> = cache
        .conn
        .query_row("SELECT value FROM entries WHERE key = ?1 AND expires_at > ?2", params![k, now()], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    let value = value?;
    let _ = cache.conn.execute("UPDATE entries SET accessed_at = ?1 WHERE key = ?2", params![now(), k]);
    serde_json::from_str(&value).ok()
}

pub fn set_cached<T: Serialize>(path: &Path, suffix: &str, data: &T) {
    set_cached_with_expiry(path, suffix, data, DEFAULT_EXPIRE_SECS);
}

pub fn set_cached_with_expiry<T: Serialize>(path: &Path, suffix: &str, data: &T, expire_secs: i64) {
    if !config::get().document.cache_enabled {
        return;
    }
    let Ok(value) = serde_json::to_string(data) else { return };
    let k = key(path, suffix);
    let guard = CACHE.lock().unwrap();
    let Some(cache) = guard.as_ref() else { return };
    let t = now();
    let _ = cache.conn.execute(
        "INSERT OR REPLACE INTO entries (key, value, size, expires_at, accessed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![k, value, value.len() as i64, t + expire_secs, t],
    );
    evict(cache, t);
}

/// Drops expired entries, then least-recently-used ones until under the size cap.
fn evict(cache: &Cache, t: i64) {
    let _ = cache.conn.execute("DELETE FROM entries WHERE expires_at <= ?1", params![t]);
    let total: i64 = cache.conn.query_row("SELECT COALESCE(SUM(size), 0) FROM entries", [], |r| r.get(0)).unwrap_or(0);
    if total <= cache.size_limit {
        return;
    }
    let mut excess = total - cache.size_limit;
    let Ok(mut stmt) = cache.conn.prepare("SELECT key, size FROM entries ORDER BY accessed_at ASC") else { return };
    let victims: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default();
    for (k, size) in victims {
        if excess <= 0 {
            break;
        }
        let _ = cache.conn.execute("DELETE FROM entries WHERE key = ?1", params![k]);
        excess -= size;
    }
}

pub fn clear_cache() {
    if let Some(cache) = CACHE.lock().unwrap().as_ref() {
        let _ = cache.conn.execute("DELETE FROM entries", []);
        let _ = cache.conn.execute_batch("VACUUM");
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheStats {
    pub size_bytes: u64,
    pub entry_count: u64,
    pub path: std::path::PathBuf,
}

pub fn cache_stats() -> CacheStats {
    let path = constants::cache_dir();
    let guard = CACHE.lock().unwrap();
    let Some(cache) = guard.as_ref() else {
        return CacheStats { size_bytes: 0, entry_count: 0, path };
    };
    let (size, count): (i64, i64) = cache
        .conn
        .query_row("SELECT COALESCE(SUM(size), 0), COUNT(*) FROM entries", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap_or((0, 0));
    CacheStats { size_bytes: size as u64, entry_count: count as u64, path }
}
