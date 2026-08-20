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
