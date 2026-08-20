//! Where fad keeps its files, per platform.

use std::path::PathBuf;

/// Snapshots of previous scans. Losing these costs a rescan and nothing more,
/// so a cache location is exactly right.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FAD_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(target_os = "macos")]
    {
        Some(home()?.join("Library/Caches/fad"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg("XDG_CACHE_HOME", ".cache")
    }
}

/// The undo journal. Deliberately *not* a cache directory: a cleaner is
/// entitled to wipe a cache, and losing your undo history to one would be a
/// nasty surprise.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FAD_STATE_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(target_os = "macos")]
    {
        Some(home()?.join("Library/Application Support/fad"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg("XDG_DATA_HOME", ".local/share")
    }
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// An XDG base directory, honouring the environment variable only when it is
/// an absolute path, as the spec requires.
#[cfg(not(target_os = "macos"))]
fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(base) = std::env::var_os(var) {
        let base = PathBuf::from(base);
        if base.is_absolute() {
            return Some(base.join("fad"));
        }
    }
    Some(home()?.join(fallback).join("fad"))
}
