//! Reading one directory as cheaply as the OS allows.
//!
//! The obvious `std::fs::read_dir` + `symlink_metadata` pairing costs a full
//! path resolution per entry: the kernel re-walks every component of a path
//! that may be ten deep, once for each of a million files. Holding the
//! directory's own descriptor and calling `fstatat` against it resolves a
//! single component instead, which is most of the walk's cost.

use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::meta::Meta;

pub struct DirEntry {
    /// The name as text. Lossy when `representable` is false, in which case it
    /// is fit for display and for nothing else — see there.
    pub name: Box<str>,
    /// The name survived the trip through UTF-8 unchanged, so `name` can be
    /// joined back onto a path and still refer to this entry.
    ///
    /// Filenames are bytes on Unix, and on Linux they need not be UTF-8. fad
    /// carries names as `str` — the tree, the fuzzy matcher, the snapshot and
    /// every path it rebuilds — so a name that does not round-trip is a name it
    /// cannot act on: `Tree::path` would produce a path that does not exist,
    /// and a delete aimed at it would miss. Rather than find that out at the
    /// point of deleting something, the walk stops at such an entry and the
    /// omissions screen says so. macOS does not arise: APFS and HFS+ reject
    /// these names at creation.
    pub representable: bool,
    pub meta: Meta,
}

/// One directory's worth of entries, and what could not be had.
pub struct Listing {
    pub entries: Vec<DirEntry>,
    /// Entries `readdir` named but that would not `fstatat`, for a reason
    /// other than having been deleted in the meantime.
    pub failed: u32,
    /// Why the first of those failed.
    pub error: Option<io::ErrorKind>,
}

/// Read every entry of `path`, stat'ing each without following symlinks.
///
/// Entries that vanish mid-walk are skipped: a file deleted between `readdir`
/// and `fstatat` is not part of the tree, and saying nothing about it is
/// right. Any other refusal is counted in `failed` rather than skipped in
/// silence. A directory with read but not search permission (mode 644) lists
/// its names and refuses to stat every one of them; reporting that as an
/// empty, complete directory of 0 B was a total that looked authoritative and
/// was not.
pub fn read_dir_stat(path: &Path) -> io::Result<Listing> {
    let guard = DirGuard(open_dir(path)?);
    // SAFETY: `dir` is a live DIR* owned by `guard`.
    let fd = unsafe { libc::dirfd(guard.0) };

    let mut out = Vec::new();
    let mut failed = 0u32;
    let mut error = None;
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
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ENOENT) {
                failed += 1;
                error.get_or_insert(err.kind());
            }
            continue;
        }

        let name = String::from_utf8_lossy(name_bytes);
        out.push(DirEntry {
            representable: name.as_bytes() == name_bytes,
            name: name.into_owned().into_boxed_str(),
            meta: Meta::from_stat(&st),
        });
    }
    Ok(Listing { entries: out, failed, error })
}

/// `opendir`, and when the path is too long for one call, the same directory
/// reached a component at a time.
///
/// `PATH_MAX` is 1024 bytes on macOS and 4096 on Linux, and nothing stops a
/// tree from being deeper than that — a runaway build, a recursive copy, a
/// `node_modules` nested inside itself. The kernel refuses the whole path, but
/// it will happily open each component relative to the last. Only the walk
/// that has already gone too deep pays for that; everything else is one call.
fn open_dir(path: &Path) -> io::Result<*mut libc::DIR> {
    let cpath = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL"))?;
    // SAFETY: cpath is a valid NUL-terminated path for the duration of the call.
    let dir = unsafe { libc::opendir(cpath.as_ptr()) };
    if !dir.is_null() {
        return Ok(dir);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::ENAMETOOLONG) || !path.is_absolute() {
        return Err(err);
    }

    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    // SAFETY: a static NUL-terminated path.
    let mut fd = unsafe { libc::open(c"/".as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    for part in path.components() {
        let std::path::Component::Normal(name) = part else { continue };
        let Ok(cname) = CString::new(name.as_bytes()) else {
            unsafe { libc::close(fd) };
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL"));
        };
        // No following symlinks on the way down: every component here is one
        // the walk lstat'ed and found to be a directory, and a symlink in its
        // place now would lead somewhere the tree does not say.
        // SAFETY: fd is an open directory we own; cname is NUL-terminated.
        let next = unsafe { libc::openat(fd, cname.as_ptr(), flags | libc::O_NOFOLLOW) };
        let open_err = io::Error::last_os_error();
        // SAFETY: fd is ours and is not used again.
        unsafe { libc::close(fd) };
        if next < 0 {
            return Err(open_err);
        }
        fd = next;
    }
    // SAFETY: fd is an open directory; on success the DIR* owns it.
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        let err = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err);
    }
    Ok(dir)
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
