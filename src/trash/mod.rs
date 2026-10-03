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
///
/// Never over the top of something: a plain `rename` replaces whatever is at
/// `to`, and checking first leaves a gap for something to arrive in. The
/// check and the move are one step here.
pub fn restore(from_trash: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rename_no_replace(from_trash, to)?;

    #[cfg(all(unix, not(target_os = "macos")))]
    freedesktop::forget(from_trash);

    Ok(())
}

/// `rename`, failing with `AlreadyExists` instead of replacing anything —
/// including a dangling symlink, which `Path::exists` calls absent.
///
/// `renamex_np(RENAME_EXCL)` on macOS and `renameat2(RENAME_NOREPLACE)` on
/// Linux do this atomically. Filesystems that do not support the flag (FAT,
/// some network mounts, kernels before 3.15) say so with `EINVAL`, `ENOSYS` or
/// `ENOTSUP`, and only then is it a check followed by a rename — the best that
/// can be done there, and no worse than before.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    match rename_excl(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EEXIST) => Err(occupied()),
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::ENOTSUP)
            ) =>
        {
            if to.symlink_metadata().is_ok() {
                return Err(occupied());
            }
            std::fs::rename(from, to)
        }
        Err(e) => Err(e),
    }
}

fn occupied() -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, "something is there now")
}

#[cfg(unix)]
fn cpath(p: &Path) -> io::Result<std::ffi::CString> {
    std::ffi::CString::new(p.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::other("path contains a NUL byte"))
}

#[cfg(target_os = "macos")]
fn rename_excl(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (cpath(from)?, cpath(to)?);
    // SAFETY: two NUL-terminated paths, neither retained.
    if unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_excl(from: &Path, to: &Path) -> io::Result<()> {
    // Through `syscall` rather than glibc's wrapper, which only arrived in
    // 2.28 and would otherwise be a link failure on older systems.
    const RENAME_NOREPLACE: libc::c_uint = 1;
    let (f, t) = (cpath(from)?, cpath(to)?);
    // SAFETY: two NUL-terminated paths, neither retained.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rename_excl(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOSYS))
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
