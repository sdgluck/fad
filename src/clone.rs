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

use crate::dupes::Identity;

/// Up to this size, the two files are compared byte for byte once more before
/// one replaces the other. The identity check already says neither has been
/// touched; this is the belt to its braces, on a filesystem that does not keep
/// `ctime` honestly (a FUSE mount, an SMB share) and for the price of a read
/// that is quick at this size. Larger pairs rely on the identity alone.
const RECHECK_LIMIT: u64 = 32 << 20;

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
/// The caller has to have proved the two files hold identical contents, and
/// hands over what each file was at the moment it was read (`dupes::Group`
/// keeps these). This checks everything else: that both are still exactly
/// those files — same inode, same length, same `mtime` and `ctime` to the
/// nanosecond — that they are plain files on the same filesystem, that they
/// are not the same file, and that they are not already sharing.
///
/// The identity is checked twice: before anything is built, and again after
/// the clone exists and just before it is renamed into place. A write to the
/// source while `clonefile` runs would otherwise be captured into the clone,
/// and a write to the destination would be thrown away by the rename.
///
/// Returns the bytes given back to the volume: what the destination's own
/// inode had allocated at the moment it was replaced. That is the figure the
/// rename releases, not the scan's figure from however long ago — and it is
/// still an upper bound, because a destination that was itself already a
/// clone of some third file shared those blocks with it, and the filesystem
/// will not say so.
pub fn share(src: &Path, src_was: &Identity, dst: &Path, dst_was: &Identity) -> Result<u64, Refusal> {
    use std::os::unix::fs::MetadataExt;

    let sm = std::fs::symlink_metadata(src).map_err(Refusal::Failed)?;
    let dm = std::fs::symlink_metadata(dst).map_err(Refusal::Failed)?;
    if !sm.is_file() || !dm.is_file() {
        return Err(Refusal::Refused("not both plain files".into()));
    }
    if sm.dev() == dm.dev() && sm.ino() == dm.ino() {
        return Err(Refusal::Refused("already the same file".into()));
    }
    // The contents were proved equal at some point in the past. Length alone
    // is no evidence that neither has moved on since — an edit in place that
    // keeps the size is the commonest edit there is, and the rename below
    // would destroy it without a trace.
    unchanged(&sm, src_was, &dm, dst_was)?;
    if src_was.len != dst_was.len {
        return Err(Refusal::Refused("they are not the same size".into()));
    }
    if sm.dev() != dm.dev() {
        return Err(Refusal::Refused("they are on different filesystems".into()));
    }
    // Replacing one name of a multiply-linked file frees nothing — the other
    // names keep the inode and its blocks alive — and it quietly splits the
    // link, so writes through the other names stop showing up here.
    if dm.nlink() > 1 {
        return Err(Refusal::Refused(
            "it has other hard links, so replacing it would free nothing".into(),
        ));
    }
    if already_shared(src, dst) {
        return Err(Refusal::Refused("already sharing their storage".into()));
    }
    if dm.len() <= RECHECK_LIMIT && !same_contents(src, dst).map_err(Refusal::Failed)? {
        return Err(Refusal::Refused("their contents no longer match".into()));
    }

    let freed = dm.blocks() * 512;
    let tmp = temp_beside(dst)?;
    let result = build_clone(src, &tmp).and_then(|()| {
        // The destination keeps its own extended attributes — Finder tags,
        // a resource fork, quarantine, `user.*` — not the source's, which
        // `clonefile` copies across with the data.
        carry_xattrs(dst, &tmp)?;
        // And its own permissions and its own modification time. Only where
        // the bytes live is different, and nothing that looks at the file has
        // any business noticing.
        carry_over(&dm, &tmp)?;
        // The last look before the point of no return. Anything that wrote to
        // either file while the clone was being built shows up here as a moved
        // `ctime`.
        let sm = std::fs::symlink_metadata(src).map_err(Refusal::Failed)?;
        let dm = std::fs::symlink_metadata(dst).map_err(Refusal::Failed)?;
        unchanged(&sm, src_was, &dm, dst_was)?;
        std::fs::rename(&tmp, dst).map_err(Refusal::Failed)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|()| freed)
}

/// Both files still exactly what they were when they were hashed.
fn unchanged(
    sm: &std::fs::Metadata,
    src_was: &Identity,
    dm: &std::fs::Metadata,
    dst_was: &Identity,
) -> Result<(), Refusal> {
    if Identity::from_meta(sm) != *src_was || Identity::from_meta(dm) != *dst_was {
        return Err(Refusal::Refused("one of them changed since it was hashed".into()));
    }
    Ok(())
}

/// Byte-for-byte equality, read in step so a mismatch near the start costs
/// almost nothing.
fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// `read` until the buffer is full or the file ends, so the two sides of
/// `same_contents` are always compared in equal-sized pieces.
fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> io::Result<usize> {
    use std::io::Read;

    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
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

    // `utimensat`, not `utimes`: the latter takes microseconds and both APFS and
    // ext4 keep nanoseconds, so it would round the destination's timestamps on
    // the way through. This is supposed to change where the bytes live and
    // nothing else, and a build system watching mtimes is exactly the kind of
    // thing that would notice.
    let times = [
        libc::timespec {
            tv_sec: dm.atime() as libc::time_t,
            tv_nsec: dm.atime_nsec() as libc::c_long,
        },
        libc::timespec {
            tv_sec: dm.mtime() as libc::time_t,
            tv_nsec: dm.mtime_nsec() as libc::c_long,
        },
    ];
    // SAFETY: a NUL-terminated path and a two-element array of the expected type.
    // The temporary is a regular file we just created; NOFOLLOW says so.
    let rc = unsafe {
        libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW)
    };
    if rc != 0 {
        return Err(Refusal::Failed(io::Error::last_os_error()));
    }
    Ok(())
}

/// Make the clone's extended attributes exactly the destination's own.
///
/// `clonefile` copies the *source's* attributes across with its data, and
/// `FICLONE` copies none at all; either way the rename would have swapped the
/// destination's Finder tags, resource fork, quarantine flag or `user.*` notes
/// for someone else's, or for nothing. The attributes are what a file *is* to
/// the user as much as its bytes are, and "only where the bytes live changes"
/// is the promise this module makes.
///
/// Strip what the clone brought that the destination does not have, write
/// what the destination has that the clone lacks, then read the result back.
/// If it still does not match — a `security.*` label the user may not set, an
/// attribute the filesystem refuses — the share is refused rather than done
/// with the wrong metadata.
fn carry_xattrs(dst: &Path, tmp: &Path) -> Result<(), Refusal> {
    use std::os::unix::fs::PermissionsExt;

    let want = xattr::list(dst).map_err(Refusal::Failed)?;
    let have = xattr::list(tmp).map_err(Refusal::Failed)?;
    if xattr::same(&want, &have) {
        return Ok(());
    }
    // The clone carries the source's mode, which may be read-only, and writing
    // an attribute needs write access. `carry_over` sets the real mode after.
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o600))
        .map_err(Refusal::Failed)?;
    for (name, _) in &have {
        if !want.iter().any(|(n, _)| n == name) {
            let _ = xattr::remove(tmp, name);
        }
    }
    for (name, value) in &want {
        if !have.iter().any(|(n, v)| n == name && v == value) {
            let _ = xattr::set(tmp, name, value);
        }
    }
    if !xattr::same(&want, &xattr::list(tmp).map_err(Refusal::Failed)?) {
        return Err(Refusal::Refused(
            "its extended attributes could not be carried over to the clone".into(),
        ));
    }
    Ok(())
}

/// Extended attributes by path, never following a symlink.
mod xattr {
    use std::ffi::{CStr, CString};
    use std::io;
    use std::path::Path;

    /// Every attribute and its value, sorted by name.
    pub type Attrs = Vec<(CString, Vec<u8>)>;

    /// Attributes the system manages on its own and will not let a user
    /// process remove or set. A mismatch in one of these says where a file
    /// came from, not what it holds, and refusing over it would refuse almost
    /// every pair of downloaded files on a modern Mac.
    const SYSTEM_MANAGED: &[&[u8]] = &[b"com.apple.provenance"];

    pub fn same(a: &Attrs, b: &Attrs) -> bool {
        let user = |x: &Attrs| -> Attrs {
            x.iter().filter(|(n, _)| !SYSTEM_MANAGED.contains(&n.as_bytes())).cloned().collect()
        };
        user(a) == user(b)
    }

    fn c(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::other("path contains a NUL byte"))
    }

    pub fn list(path: &Path) -> io::Result<Attrs> {
        let p = c(path)?;
        let names = fill(|buf, len| sys::list(&p, buf, len))?;
        let mut out = Vec::new();
        for name in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
            // Split on NUL, so there is none left inside.
            let name = CString::new(name).expect("attribute name split on NUL");
            match fill(|buf, len| sys::get(&p, &name, buf, len)) {
                Ok(value) => out.push((name, value)),
                // Removed between the listing and the read: not there.
                Err(e) if e.raw_os_error() == Some(sys::ENOATTR) => {}
                Err(e) => return Err(e),
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn set(path: &Path, name: &CStr, value: &[u8]) -> io::Result<()> {
        if sys::set(&c(path)?, name, value) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn remove(path: &Path, name: &CStr) -> io::Result<()> {
        if sys::remove(&c(path)?, name) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Ask for the size, then read; again if it grew in between.
    fn fill(mut call: impl FnMut(*mut u8, usize) -> isize) -> io::Result<Vec<u8>> {
        loop {
            let n = call(std::ptr::null_mut(), 0);
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buf = vec![0u8; n as usize];
            if n == 0 {
                return Ok(buf);
            }
            let m = call(buf.as_mut_ptr(), buf.len());
            if m < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::ERANGE) {
                    continue;
                }
                return Err(e);
            }
            buf.truncate(m as usize);
            return Ok(buf);
        }
    }

    #[cfg(target_os = "macos")]
    mod sys {
        use std::ffi::CStr;

        pub const ENOATTR: i32 = libc::ENOATTR;
        const NOFOLLOW: libc::c_int = 0x0001;

        // SAFETY (all four): NUL-terminated strings and a buffer of the length
        // passed, none retained past the call.
        pub fn list(p: &CStr, buf: *mut u8, len: usize) -> isize {
            unsafe { libc::listxattr(p.as_ptr(), buf.cast(), len, NOFOLLOW) }
        }
        pub fn get(p: &CStr, name: &CStr, buf: *mut u8, len: usize) -> isize {
            unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), buf.cast(), len, 0, NOFOLLOW) }
        }
        pub fn set(p: &CStr, name: &CStr, v: &[u8]) -> libc::c_int {
            unsafe { libc::setxattr(p.as_ptr(), name.as_ptr(), v.as_ptr().cast(), v.len(), 0, NOFOLLOW) }
        }
        pub fn remove(p: &CStr, name: &CStr) -> libc::c_int {
            unsafe { libc::removexattr(p.as_ptr(), name.as_ptr(), NOFOLLOW) }
        }
    }

    #[cfg(target_os = "linux")]
    mod sys {
        use std::ffi::CStr;

        pub const ENOATTR: i32 = libc::ENODATA;

        // SAFETY (all four): NUL-terminated strings and a buffer of the length
        // passed, none retained past the call.
        pub fn list(p: &CStr, buf: *mut u8, len: usize) -> isize {
            unsafe { libc::llistxattr(p.as_ptr(), buf.cast(), len) }
        }
        pub fn get(p: &CStr, name: &CStr, buf: *mut u8, len: usize) -> isize {
            unsafe { libc::lgetxattr(p.as_ptr(), name.as_ptr(), buf.cast(), len) }
        }
        pub fn set(p: &CStr, name: &CStr, v: &[u8]) -> libc::c_int {
            unsafe { libc::lsetxattr(p.as_ptr(), name.as_ptr(), v.as_ptr().cast(), v.len(), 0) }
        }
        pub fn remove(p: &CStr, name: &CStr) -> libc::c_int {
            unsafe { libc::lremovexattr(p.as_ptr(), name.as_ptr()) }
        }
    }

    /// No attributes to speak of, and no clone operation either: `share` is
    /// refused as unsupported before this is ever reached.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    mod sys {
        use std::ffi::CStr;

        pub const ENOATTR: i32 = 0;
        pub fn list(_: &CStr, _: *mut u8, _: usize) -> isize {
            0
        }
        pub fn get(_: &CStr, _: &CStr, _: *mut u8, _: usize) -> isize {
            -1
        }
        pub fn set(_: &CStr, _: &CStr, _: &[u8]) -> libc::c_int {
            -1
        }
        pub fn remove(_: &CStr, _: &CStr) -> libc::c_int {
            -1
        }
    }
}

fn cstr(path: &Path) -> Result<std::ffi::CString, Refusal> {
    std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| Refusal::Refused("path contains a NUL byte".into()))
}
