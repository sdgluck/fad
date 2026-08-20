//! Reading one directory as cheaply as the OS allows.
//!
//! The obvious `std::fs::read_dir` + `symlink_metadata` pairing costs a full
//! path resolution per entry: the kernel re-walks every component of a path
//! that may be ten deep, once for each of a million files. Holding the
//! directory's own descriptor and calling `fstatat` against it resolves a
//! single component instead, which is most of the walk's cost.

use std::ffi::CStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::meta::Meta;

pub struct DirEntry {
    pub name: Box<str>,
    pub meta: Meta,
}

/// Read every entry of `path`, stat'ing each without following symlinks.
/// Entries that vanish or refuse to stat mid-walk are skipped, not fatal.
pub fn read_dir_stat(path: &Path) -> io::Result<Vec<DirEntry>> {
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL"))?;

    // SAFETY: cpath is a valid NUL-terminated path for the duration of the call.
    let dir = unsafe { libc::opendir(cpath.as_ptr()) };
    if dir.is_null() {
        return Err(io::Error::last_os_error());
    }
    let guard = DirGuard(dir);
    // SAFETY: `dir` is a live DIR* owned by `guard`.
    let fd = unsafe { libc::dirfd(guard.0) };

    let mut out = Vec::new();
    loop {
        // `readdir` returns a pointer into DIR-private storage that is valid
        // until the next call on this same DIR*, which we do not share.
        errno_reset();
        let ent = unsafe { libc::readdir(guard.0) };
        if ent.is_null() {
            let err = io::Error::last_os_error();
            if err.raw_os_error().unwrap_or(0) != 0 {
                return Err(err);
            }
            break; // clean end of directory
        }
        // SAFETY: non-null entry from readdir on a live DIR*.
        let ent = unsafe { &*ent };
        let name_ptr = ent.d_name.as_ptr();
        // SAFETY: d_name is NUL-terminated within the dirent.
        let name_bytes = unsafe { CStr::from_ptr(name_ptr) }.to_bytes();
        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }

        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fd is the open directory, name_ptr is NUL-terminated, st is ours.
        let rc = unsafe {
            libc::fstatat(fd, name_ptr, &mut st, libc::AT_SYMLINK_NOFOLLOW)
        };
        if rc != 0 {
            continue; // raced with a delete, or we cannot stat it; either way, skip
        }

        out.push(DirEntry {
            name: String::from_utf8_lossy(name_bytes).into_owned().into_boxed_str(),
            meta: Meta::from_stat(&st),
        });
    }
    Ok(out)
}

/// `readdir` signals "end of directory" and "error" the same way — a null
/// return — and they are only distinguishable by whether it touched `errno`.
/// The slot has a different name on every libc.
fn errno_reset() {
    #[cfg(target_os = "macos")]
    // SAFETY: __error() returns this thread's errno slot.
    unsafe {
        *libc::__error() = 0
    };
    #[cfg(target_os = "linux")]
    // SAFETY: __errno_location() returns this thread's errno slot.
    unsafe {
        *libc::__errno_location() = 0
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    unsafe {
        *libc::__error() = 0
    };
}

struct DirGuard(*mut libc::DIR);

impl Drop for DirGuard {
    fn drop(&mut self) {
        // SAFETY: we own this DIR* and close it exactly once.
        unsafe { libc::closedir(self.0) };
    }
}
