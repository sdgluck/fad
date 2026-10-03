//! Shared test scaffolding.
//!
//! Each test binary compiles its own copy and uses some of it, so whatever one
//! binary leaves alone would otherwise be reported as dead there.
#![allow(dead_code)]

use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Environment variables are process-global, and `cargo test` runs a binary's
/// tests on several threads at once. Any test that points `HOME`,
/// `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `FAD_STATE_DIR`, `FAD_CONFIG_DIR` or `FAD_CACHE_DIR` at a scratch directory
/// must hold this for its whole body, or it will be reading another test's
/// scratch directory halfway through.
pub fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        // A poisoned lock means another test panicked while holding it. The
        // env is still usable; we just want the guard.
        .unwrap_or_else(|e| e.into_inner())
}

/// Point every directory fad uses at a scratch tree, so a test run can never
/// read or consume the real user's trash, cache, or undo history.
pub fn isolate(dir: &Path) {
    unsafe {
        std::env::set_var("HOME", dir);
        std::env::set_var("XDG_DATA_HOME", dir.join("share"));
        std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
        // The ignore list lives under the config directory, and a user who
        // sets XDG_CONFIG_HOME would otherwise have every test's scratch paths
        // appended to their real one.
        std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        std::env::set_var("FAD_CONFIG_DIR", dir.join("config/fad"));
        std::env::set_var("FAD_STATE_DIR", dir.join("state"));
        std::env::set_var("FAD_CACHE_DIR", dir.join("cache/fad"));
    }
}

/// A directory the presets call a cache on this platform, for fixtures that
/// need one reclaimable category to exist. `Library/Caches` is the macOS app
/// cache; Linux has no name-only equivalent (its `.cache` has to be the home
/// directory's own), so it gets npm's, which every platform recognises.
#[cfg(target_os = "macos")]
pub const CACHE_DIR: &str = "Library/Caches";
#[cfg(not(target_os = "macos"))]
pub const CACHE_DIR: &str = ".npm/_cacache";
