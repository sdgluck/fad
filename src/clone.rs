//! Making two identical files share one copy of their storage.
//!
//! The duplicate view can prove that two files hold the same bytes. Until now
//! the only thing it could offer to do about it was delete one of them, which
//! means the user has to decide which path they can live without. On a
//! filesystem with copy-on-write clones there is nothing to decide: the second
//! path can keep working and stop costing anything, because both names come to
//! point at the same extents and the filesystem splits them again the moment
//! either one is written to.
//!
//! Three things make this safe enough to offer:
//!
//! 1. It is a `rename` over the destination, never a truncate-and-write. The
//!    clone is built beside the target under a temporary name and moved into
//!    place in one step, so an interruption leaves either the old file or the
//!    new one and never half of either.
//! 2. Nothing observable changes. The destination keeps its own permissions and
//!    its own modification time; only where its bytes live is different.
//! 3. It is refused rather than emulated. Where the filesystem has no clone
//!    operation there is no fallback to a hard link — a hard link would make
//!    writing to one path change the other, which is a different thing from
//!    what the user asked for and a much worse one.

use std::io;
use std::path::{Path, PathBuf};

/// Why a clone did not happen.
#[derive(Debug)]
pub enum Refusal {
    /// The filesystem has no clone operation. Not an error: most do not.
    Unsupported,
    /// Something about the pair is not what the duplicate report described, or
    /// the two are not eligible in the first place.
    Refused(String),
    Failed(io::Error),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unsupported => write!(f, "this filesystem cannot share storage between files"),
            Refusal::Refused(why) => write!(f, "{why}"),
            Refusal::Failed(e) => write!(f, "{e}"),
        }
    }
}

/// What a file's bytes start at on the device, when the filesystem will say.
///
/// Two files sharing storage share this. It is what lets the duplicate hunt
/// skip pairs that have already been cloned — deleting one of those frees
/// nothing, exactly as with a hard link — and what stops this module cloning
/// something onto itself.
///
/// `None` means the filesystem would not answer, which is not the same as "not
/// shared" and is treated as "we do not know" everywhere it is used.
pub fn physical_start(path: &Path) -> Option<u64> {
    let f = std::fs::File::open(path).ok()?;
    physical_start_of(&f)
}

#[cfg(target_os = "macos")]
fn physical_start_of(f: &std::fs::File) -> Option<u64> {
    use std::os::unix::io::AsRawFd;

    let mut l: libc::log2phys = unsafe { std::mem::zeroed() };
    // The extended form takes the logical offset to ask about in
    // `l2p_devoffset` and the run length in `l2p_contigbytes`, and writes the
    // physical offset back over the former.
    l.l2p_contigbytes = 1 << 20;
    l.l2p_devoffset = 0;
    // SAFETY: `l` is a live local of the type this command expects, and the
    // kernel does not retain the pointer.
    if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_LOG2PHYS_EXT, &mut l) } != 0 {
        return None;
    }
    Some(l.l2p_devoffset as u64)
}

#[cfg(target_os = "linux")]
fn physical_start_of(f: &std::fs::File) -> Option<u64> {
    use std::os::unix::io::AsRawFd;

    // `FS_IOC_FIEMAP`, with room for exactly one extent: the first one is all
    // this is asking about.
    const FS_IOC_FIEMAP: libc::c_ulong = 0xC020_660B;
    const FIEMAP_FLAG_SYNC: u32 = 0x0001;

    #[repr(C)]
    #[derive(Default)]
    struct Extent {
        logical: u64,
        physical: u64,
        length: u64,
        reserved64: [u64; 2],
        flags: u32,
        reserved: [u32; 3],
    }
    #[repr(C)]
    #[derive(Default)]
    struct Fiemap {
        start: u64,
        length: u64,
        flags: u32,
        mapped_extents: u32,
        extent_count: u32,
        reserved: u32,
        extents: [Extent; 1],
    }

    let mut m = Fiemap { length: u64::MAX, flags: FIEMAP_FLAG_SYNC, extent_count: 1, ..Default::default() };
    // SAFETY: `m` is a live local laid out as the kernel's `struct fiemap`
    // followed by the one extent `extent_count` promises room for.
    if unsafe { libc::ioctl(f.as_raw_fd(), FS_IOC_FIEMAP, &mut m) } != 0 {
        return None;
    }
    (m.mapped_extents > 0).then_some(m.extents[0].physical)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn physical_start_of(_f: &std::fs::File) -> Option<u64> {
    None
}

/// Do these two paths already share their storage?
///
/// `false` when the filesystem will not say, because the only thing this
/// decides is whether to bother, and a wasted check is cheaper than a wrongly
/// skipped one.
pub fn already_shared(a: &Path, b: &Path) -> bool {
    match (physical_start(a), physical_start(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Replace `dst` with a copy-on-write clone of `src`, freeing what `dst`'s own
/// bytes were costing.
///
/// The caller has to have proved the two files hold identical contents. This
/// checks everything else: that they are both plain files on the same
/// filesystem, that neither has changed size since it was hashed, that they are
/// not the same file, and that they are not already sharing.
pub fn share(src: &Path, dst: &Path, expect_bytes: u64) -> Result<(), Refusal> {
    use std::os::unix::fs::MetadataExt;

    let sm = std::fs::symlink_metadata(src).map_err(Refusal::Failed)?;
    let dm = std::fs::symlink_metadata(dst).map_err(Refusal::Failed)?;
    if !sm.is_file() || !dm.is_file() {
        return Err(Refusal::Refused("not both plain files".into()));
    }
    // The contents were proved equal at some point in the past, and this is the
    // cheapest evidence available that neither has moved on since.
    if sm.len() != expect_bytes || dm.len() != expect_bytes {
        return Err(Refusal::Refused("one of them changed since it was hashed".into()));
    }
    if sm.dev() != dm.dev() {
        return Err(Refusal::Refused("they are on different filesystems".into()));
    }
    if sm.ino() == dm.ino() {
        return Err(Refusal::Refused("already the same file".into()));
    }
    if already_shared(src, dst) {
        return Err(Refusal::Refused("already sharing their storage".into()));
    }

    let tmp = temp_beside(dst)?;
    let result = build_clone(src, &tmp).and_then(|()| {
        // The destination keeps its own permissions and its own modification
        // time. Only where the bytes live is different, and nothing that looks
        // at the file has any business noticing.
        carry_over(&dm, &tmp)?;
        std::fs::rename(&tmp, dst).map_err(Refusal::Failed)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A name beside the destination, so the clone lands on the right filesystem
/// and the move into place is a rename rather than a copy.
fn temp_beside(dst: &Path) -> Result<PathBuf, Refusal> {
    let dir = dst.parent().ok_or_else(|| Refusal::Refused("no parent directory".into()))?;
    let name = dst
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Refusal::Refused("unreadable file name".into()))?;
    let pid = std::process::id();
    for attempt in 0..1000u32 {
        let candidate = dir.join(format!(".fad-clone-{pid}-{attempt}-{name}"));
        if candidate.symlink_metadata().is_err() {
            return Ok(candidate);
        }
    }
    Err(Refusal::Refused("could not find a free temporary name".into()))
}

#[cfg(target_os = "macos")]
fn build_clone(src: &Path, tmp: &Path) -> Result<(), Refusal> {
    let (s, t) = (cstr(src)?, cstr(tmp)?);
    // SAFETY: two NUL-terminated paths, neither retained by the call.
    if unsafe { libc::clonefile(s.as_ptr(), t.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        // No clone on this filesystem — HFS+, a network mount, an exFAT stick.
        Some(libc::ENOTSUP) | Some(libc::EXDEV) | Some(libc::EOPNOTSUPP) => Err(Refusal::Unsupported),
        _ => Err(Refusal::Failed(e)),
    }
}

#[cfg(target_os = "linux")]
fn build_clone(src: &Path, tmp: &Path) -> Result<(), Refusal> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;

    // `FICLONE`: make this file's data a reflink of that one's.
    const FICLONE: libc::c_ulong = 0x4004_9409;

    let s = std::fs::File::open(src).map_err(Refusal::Failed)?;
    let t = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
        .map_err(Refusal::Failed)?;
    // SAFETY: both descriptors are open for the duration of the call.
    if unsafe { libc::ioctl(t.as_raw_fd(), FICLONE, s.as_raw_fd()) } == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        // ext4 and friends have no reflink; XFS and btrfs do.
        Some(libc::EOPNOTSUPP) | Some(libc::EINVAL) | Some(libc::EXDEV) => Err(Refusal::Unsupported),
        _ => Err(Refusal::Failed(e)),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn build_clone(_src: &Path, _tmp: &Path) -> Result<(), Refusal> {
    Err(Refusal::Unsupported)
}

/// Give the clone the destination's own permissions, owner and timestamps, so
/// that after the rename nothing about the file has changed but where its
/// bytes are.
fn carry_over(dm: &std::fs::Metadata, tmp: &Path) -> Result<(), Refusal> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(dm.mode() & 0o7777))
        .map_err(Refusal::Failed)?;

    let c = cstr(tmp)?;
    // Best effort: a user who does not own the file cannot give it away, and
    // failing the whole clone over that would be worse than the mismatch.
    // SAFETY: a NUL-terminated path, not retained.
    unsafe { libc::chown(c.as_ptr(), dm.uid(), dm.gid()) };

    let times = [
        libc::timeval { tv_sec: dm.atime() as libc::time_t, tv_usec: 0 },
        libc::timeval { tv_sec: dm.mtime() as libc::time_t, tv_usec: 0 },
    ];
    // SAFETY: a NUL-terminated path and a two-element array of the expected type.
    if unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) } != 0 {
        return Err(Refusal::Failed(io::Error::last_os_error()));
    }
    Ok(())
}

fn cstr(path: &Path) -> Result<std::ffi::CString, Refusal> {
    std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| Refusal::Refused("path contains a NUL byte".into()))
}
