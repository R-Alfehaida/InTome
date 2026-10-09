//! Application-wide constants and per-user paths.

use std::path::PathBuf;

pub const APP_NAME: &str = "InTome";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_AUTHOR: &str = "Claude by Anthropic";

pub const DEFAULT_PAGE_SIZE: usize = 3000;
pub const SIDEBAR_DEFAULT_WIDTH: i32 = 250;
pub const MIN_WINDOW_WIDTH: i32 = 800;
pub const MIN_WINDOW_HEIGHT: i32 = 600;

/// `%APPDATA%\InTome`. Config, logs, cache and the annotations DB live here.
pub fn appdata_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(APP_NAME)
}

pub fn config_file() -> PathBuf {
    appdata_dir().join("intome.toml")
}

pub fn log_file() -> PathBuf {
    appdata_dir().join("intome.log")
}

pub fn cache_dir() -> PathBuf {
    appdata_dir().join("cache")
}

pub fn annotations_db() -> PathBuf {
    appdata_dir().join("annotations.db")
}

pub fn voice_profiles_file() -> PathBuf {
    appdata_dir().join("voice_profiles.json")
}

/// Read-only bundled assets (user guide, pdfium.dll, screen-reader client DLLs)
/// sit next to the executable. During `cargo run` they are copied there by build.rs.
pub fn resource_path(relative: &str) -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default();
    exe_dir.join(relative)
}
