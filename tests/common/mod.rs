//! Shared test scaffolding.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Environment variables are process-global, and `cargo test` runs a binary's
/// tests on several threads at once. Any test that points `HOME`,
/// `XDG_DATA_HOME`, `FAD_STATE_DIR` or `FAD_CACHE_DIR` at a scratch directory
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
        std::env::set_var("FAD_STATE_DIR", dir.join("state"));
        std::env::set_var("FAD_CACHE_DIR", dir.join("cache/fad"));
    }
}
