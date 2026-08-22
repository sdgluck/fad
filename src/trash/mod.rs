//! Moving something somewhere recoverable.
//!
//! Both implementations return *where the item landed*, which is what makes
//! fad's own undo possible: restoring is then a plain rename back.

use std::io;
use std::path::Path;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::trash;

#[cfg(all(unix, not(target_os = "macos")))]
mod freedesktop;
#[cfg(all(unix, not(target_os = "macos")))]
pub use freedesktop::trash;

#[cfg(not(unix))]
pub fn trash(_path: &Path) -> io::Result<std::path::PathBuf> {
    Err(io::Error::other("no trash implementation for this platform"))
}

/// Undo the move. On platforms that leave metadata beside the trashed file,
/// this cleans that up too, so the item does not linger as a phantom entry in
/// the desktop's trash UI.
pub fn restore(from_trash: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from_trash, to)?;

    #[cfg(all(unix, not(target_os = "macos")))]
    freedesktop::forget(from_trash);

    Ok(())
}

/// Is this somewhere a trashed item could legitimately be sitting?
///
/// The journal records where each item landed, and emptying works off those
/// paths — so this is the check that stands between a corrupt or hand-edited
/// journal and an unrecoverable delete of something that was never in a trash
/// at all. It is deliberately structural rather than exact: `~/.Trash`,
/// `/Volumes/x/.Trashes/501`, `~/.local/share/Trash/files` and
/// `$topdir/.Trash-1000/files` all have to pass, and nothing else should.
pub fn is_trash_path(path: &Path) -> bool {
    path.components().any(|c| {
        let Some(name) = c.as_os_str().to_str() else { return false };
        name == ".Trash" || name == ".Trashes" || name == "Trash" || name.starts_with(".Trash-")
    })
}

/// Take an item out of the trash for good.
///
/// The counterpart of `restore`: the same cleanup of the metadata beside it,
/// but nothing comes back. Refuses anything that is not in a trash directory,
/// because the only thing calling this is a loop over recorded paths and a
/// wrong path here is a delete nobody asked for.
pub fn erase(from_trash: &Path) -> io::Result<()> {
    if !is_trash_path(from_trash) {
        return Err(io::Error::other("not a path inside a trash directory"));
    }
    let meta = std::fs::symlink_metadata(from_trash)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(from_trash)?;
    } else {
        std::fs::remove_file(from_trash)?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    freedesktop::forget(from_trash);

    Ok(())
}
