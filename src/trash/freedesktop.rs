//! The FreeDesktop.org Trash specification.
//!
//! Two rules drive the whole implementation:
//!
//! 1. Trashing must be a `rename`, never a copy. A trash directory therefore
//!    has to live on the same filesystem as the item — which is why the spec
//!    defines per-filesystem trash directories alongside the home one, and why
//!    we pick between them by device number rather than by path.
//! 2. The `.trashinfo` file is what *claims* a name. It is created with
//!    `O_EXCL`, so two processes trashing `report.pdf` at the same moment
//!    cannot end up fighting over `files/report.pdf`.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::paths;

pub fn trash(path: &Path) -> io::Result<PathBuf> {
    let meta = std::fs::symlink_metadata(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("cannot trash a path with no final component"))?;

    let dir = trash_dir_for(path, meta.dev())?;
    let files = dir.join("files");
    let info = dir.join("info");
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;

    let stem = name.to_string_lossy();
    for attempt in 0..1000 {
        let candidate = if attempt == 0 {
            stem.to_string()
        } else {
            // Suffix before the extension, the way desktop trashes do it, so a
            // restored `notes.md` is not called `notes.md.2`.
            match stem.rsplit_once('.') {
                Some((base, ext)) if !base.is_empty() => format!("{base}.{attempt}.{ext}"),
                _ => format!("{stem}.{attempt}"),
            }
        };

        let info_path = info.join(format!("{candidate}.trashinfo"));
        // O_EXCL is the lock: whoever creates the info file owns the name.
        let mut f = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&info_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };

        let target = files.join(&candidate);
        // The name was free a moment ago; if the file half is somehow taken,
        // give the info file back rather than clobbering someone's data.
        if target.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(&info_path);
            continue;
        }

        let recorded = record_path(path, &dir);
        write!(
            f,
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            url_encode(&recorded),
            deletion_date()
        )?;
        f.sync_all()?;
        drop(f);

        match std::fs::rename(path, &target) {
            Ok(()) => return Ok(target),
            Err(e) => {
                let _ = std::fs::remove_file(&info_path);
                return Err(e);
            }
        }
    }
    Err(io::Error::other("could not find a free name in the trash"))
}

/// Drop the `.trashinfo` that belongs to a trashed file, after it has been
/// restored. Without this the desktop's trash keeps showing an entry whose
/// file is gone.
pub fn forget(from_trash: &Path) {
    let Some(name) = from_trash.file_name() else { return };
    let Some(files) = from_trash.parent() else { return };
    if files.file_name().is_none_or(|n| n != "files") {
        return;
    }
    let Some(dir) = files.parent() else { return };
    let info = dir.join("info").join(format!("{}.trashinfo", name.to_string_lossy()));
    let _ = std::fs::remove_file(info);
}

/// The trash directory that shares a filesystem with `path`.
///
/// The home trash is preferred, but only when it is on the same device —
/// otherwise `rename` would fail with `EXDEV` and the only alternative would be
/// a copy, which is not what a user means by "move to trash".
fn trash_dir_for(path: &Path, dev: u64) -> io::Result<PathBuf> {
    if let Some(home_trash) = home_trash() {
        // Compare against the nearest existing ancestor: the trash directory
        // itself may not have been created yet.
        if let Some(home_dev) = existing_ancestor_dev(&home_trash) {
            if home_dev == dev {
                return Ok(home_trash);
            }
        }
    }

    let top = top_dir(path, dev)?;
    let uid = unsafe { libc::getuid() };

    // Spec order: an admin-provided `$topdir/.Trash` that is sticky and not a
    // symlink, otherwise our own `$topdir/.Trash-$uid`.
    let shared = top.join(".Trash");
    if let Ok(m) = std::fs::symlink_metadata(&shared) {
        const S_ISVTX: u32 = 0o1000;
        if m.is_dir() && !m.file_type().is_symlink() && m.mode() & S_ISVTX != 0 {
            return Ok(shared.join(uid.to_string()));
        }
    }
    Ok(top.join(format!(".Trash-{uid}")))
}

fn home_trash() -> Option<PathBuf> {
    if let Some(base) = std::env::var_os("XDG_DATA_HOME") {
        let base = PathBuf::from(base);
        if base.is_absolute() {
            return Some(base.join("Trash"));
        }
    }
    Some(paths::home()?.join(".local/share/Trash"))
}

fn existing_ancestor_dev(path: &Path) -> Option<u64> {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if let Ok(m) = std::fs::metadata(p) {
            return Some(m.dev());
        }
        cur = p.parent();
    }
    None
}

/// Walk up until the device number changes: that boundary is the mount point.
fn top_dir(path: &Path, dev: u64) -> io::Result<PathBuf> {
    let mut best = PathBuf::from("/");
    let mut cur = path.parent();
    while let Some(p) = cur {
        match std::fs::metadata(p) {
            Ok(m) if m.dev() == dev => {
                best = p.to_path_buf();
                cur = p.parent();
            }
            _ => break,
        }
    }
    Ok(best)
}

/// The spec wants a path relative to the trash directory's top dir when the
/// trash is not the home one, and an absolute path otherwise.
fn record_path(path: &Path, trash_dir: &Path) -> String {
    if home_trash().is_some_and(|h| trash_dir == h) {
        return path.to_string_lossy().into_owned();
    }
    // `$topdir/.Trash-1000` and `$topdir/.Trash/1000` both sit under the top dir.
    let top = match trash_dir.file_name().and_then(|n| n.to_str()) {
        Some(n) if n.starts_with(".Trash-") => trash_dir.parent(),
        _ => trash_dir.parent().and_then(|p| p.parent()),
    };
    match top.and_then(|t| path.strip_prefix(t).ok()) {
        Some(rel) => rel.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

/// Percent-encode everything outside the unreserved set, leaving `/` alone so
/// the value stays readable as a path.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Local time, as the spec asks for, via `localtime_r`.
fn deletion_date() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to live locals, and localtime_r does not retain them.
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return "1970-01-01T00:00:00".into();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}
