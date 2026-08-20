//! `NSFileManager.trashItemAtURL:resultingItemURL:error:`.
//!
//! This is the only route that gets both a real Trash move — with the metadata
//! Finder's "Put Back" needs — and the resulting URL handed back to us. The
//! `trash` crate's restore APIs are Linux and Windows only, which is why the
//! journal in `delete.rs` is ours.

use std::io;
use std::path::{Path, PathBuf};

use objc2_foundation::{NSFileManager, NSString, NSURL};

pub fn trash(path: &Path) -> io::Result<PathBuf> {
    let fm = NSFileManager::defaultManager();
    let ns_path = NSString::from_str(&path.to_string_lossy());
    let url = NSURL::fileURLWithPath(&ns_path);

    let mut resulting = None;
    fm.trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
        .map_err(|e| io::Error::other(e.localizedDescription().to_string()))?;

    resulting
        .and_then(|u| u.path())
        .map(|p| PathBuf::from(p.to_string()))
        // The item is trashed either way; we just could not learn where, so we
        // must not claim it can be undone.
        .ok_or_else(|| io::Error::other("macOS did not report where the item was trashed"))
}
