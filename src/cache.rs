//! Persisting a scan so the next launch is instant.
//!
//! The strategy is deliberately blunt: load the last snapshot, show it
//! immediately, and start a fresh walk in the background. When the walk
//! finishes, the exact tree replaces the remembered one.
//!
//! The alternative — revalidating directory by directory against `mtime` and
//! re-walking only what changed — saves CPU but means splicing subtrees into a
//! live arena while rollups and hardlink attribution stay consistent. That is a
//! lot of surgery for a scan that already finishes in seconds, and every bug in
//! it shows up as a wrong number the user has no way to distrust. Whole-tree
//! replacement cannot be subtly wrong.

use std::io;
use std::path::{Path, PathBuf};

use crate::scan::walk::ScanOpts;
use crate::tree::{Snapshot, Tree};

/// Bumped whenever `Node` or `Tree` change shape, or a field changes meaning.
/// An old snapshot is discarded rather than misread. Version 4 added the
/// per-subtree newest-mtime rollup that the age histogram reads; version 5 adds
/// a third `Skip` reason, which an older reader would take for the first one;
/// version 6 records the scan options and completion time, the reasons
/// directories were unreadable, the hard-link sets, and the root as raw bytes.
const FORMAT: u32 = 6;
const MAGIC: &[u8; 4] = b"fad\0";

fn snapshot_path(root: &Path) -> Option<PathBuf> {
    // A stable, readable filename per root: the hash keeps it unique, the
    // basename keeps the directory browsable when something goes wrong.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in root.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let name = root.file_name().and_then(|s| s.to_str()).unwrap_or("root");
    let name: String = name.chars().filter(|c| c.is_alphanumeric() || *c == '-').take(24).collect();
    Some(crate::paths::cache_dir()?.join(format!("{name}-{hash:016x}.snap")))
}

pub fn save(tree: &Tree) -> io::Result<()> {
    let Some(path) = snapshot_path(tree.root_path()) else {
        return Ok(());
    };
    std::fs::create_dir_all(path.parent().unwrap())?;

    let mut buf = Vec::with_capacity(1 << 20);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&FORMAT.to_le_bytes());
    postcard::to_io(&tree.to_snapshot(), &mut buf).map_err(io::Error::other)?;

    // Write beside the target and rename: a snapshot is either the old one or
    // the new one, never a half-written file that panics the next launch.
    let tmp = path.with_extension("snap.tmp");
    std::fs::write(&tmp, &buf)?;
    std::fs::rename(&tmp, &path)?;
    prune(path.parent().unwrap(), &path);
    Ok(())
}

/// The saved tree, and when the walk that produced it finished. The timestamp
/// is what turns "40G" on screen into "+12G since Tuesday", which is the more
/// actionable of the two.
///
/// Only a snapshot taken with the same options counts. The cache is keyed by
/// root alone, and `fad --cross-device /` and `fad /` are measuring different
/// things: comparing one against the other would report a mounted disk as
/// "+2T since yesterday". `--apparent` needs no such care — every snapshot
/// holds both allocated and apparent sizes, and the flag only picks which one
/// is shown.
pub fn load(root: &Path, opts: &ScanOpts) -> Option<(Tree, std::time::SystemTime)> {
    // Snapshots are keyed by the canonical path, because that is what the scan
    // recorded. Without this, `fad /var/x` and `fad /private/var/x` would each
    // keep their own copy and neither would ever find the other's.
    let root = &std::fs::canonicalize(root).ok()?;
    let path = snapshot_path(root)?;
    let bytes = std::fs::read(&path).ok()?;
    if bytes.len() < 8 || &bytes[..4] != MAGIC {
        return None;
    }
    if u32::from_le_bytes(bytes[4..8].try_into().ok()?) != FORMAT {
        return None;
    }
    let snapshot: Snapshot = postcard::from_bytes(&bytes[8..]).ok()?;
    let tree = Tree::from_snapshot(snapshot)?;
    // A snapshot taken of a different directory is not ours, however it got here.
    if tree.root_path() != root.as_path() {
        return None;
    }
    if tree.scan_opts() != opts {
        return None;
    }
    let completed_at = tree.completed_at()?;
    // Reading it counts as using it: a root opened every day but rarely left
    // to finish a walk should not age out of the cache it keeps relying on.
    if let Ok(f) = std::fs::File::options().write(true).open(&path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Some((tree, completed_at))
}

/// Is this a file fad wrote into the cache directory? Only these are ever
/// deleted from it.
fn ours(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    name.ends_with(".snap") || name.ends_with(".snap.tmp")
}

/// Delete every saved snapshot, and the directory if that leaves it empty.
///
/// Not `remove_dir_all`. `FAD_CACHE_DIR` names the directory itself, with no
/// `fad/` appended, so `FAD_CACHE_DIR=~/.cache fad --clear-cache` used to
/// delete the whole of `~/.cache` — every other program's cache along with
/// fad's. Only files fad writes are touched, and the directory goes only if
/// nothing else was in it.
pub fn clear() -> io::Result<()> {
    let Some(dir) = crate::paths::cache_dir() else { return Ok(()) };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        if ours(&path) && std::fs::symlink_metadata(&path)?.is_file() {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    // `remove_dir` refuses a directory with anything left in it, which is
    // exactly the refusal wanted here.
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

/// Snapshots untouched for this long are for roots nobody scans any more.
const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 24 * 60 * 60);
/// A snapshot of a large home directory runs to a hundred megabytes and more,
/// one per root ever scanned. Past this, the least recently used go first.
const MAX_TOTAL: u64 = 1 << 30;

/// Keep the cache from growing without bound: drop snapshots not saved or
/// loaded in `MAX_AGE`, stray temporaries from an interrupted save, and then
/// the oldest until what is left fits in `MAX_TOTAL`. `keep` — the snapshot
/// just written — is never a candidate, however big it is on its own.
///
/// Best effort throughout: a file that will not go is left for next time.
fn prune(dir: &Path, keep: &Path) {
    prune_with(dir, keep, MAX_AGE, MAX_TOTAL)
}

fn prune_with(dir: &Path, keep: &Path, max_age: std::time::Duration, max_total: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    let mut live: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if !ours(&path) || path == keep {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.is_file() {
            continue;
        }
        let touched = meta.modified().unwrap_or(now);
        let age = now.duration_since(touched).unwrap_or_default();
        let stale_tmp = path.extension().is_some_and(|e| e == "tmp")
            && age > std::time::Duration::from_secs(24 * 60 * 60);
        if age > max_age || stale_tmp {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        total += meta.len();
        live.push((touched, meta.len(), path));
    }
    total += std::fs::metadata(keep).map(|m| m.len()).unwrap_or(0);
    live.sort_by_key(|(touched, _, _)| *touched);
    for (_, len, path) in live {
        if total <= max_total {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn put(dir: &Path, name: &str, len: usize, age_days: u64) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, vec![0u8; len]).unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(age_days * 86_400)).unwrap();
        p
    }

    #[test]
    fn old_snapshots_and_stale_temporaries_go() {
        let dir = tempfile::tempdir().unwrap();
        let keep = put(dir.path(), "now.snap", 10, 0);
        let fresh = put(dir.path(), "fresh.snap", 10, 3);
        let old = put(dir.path(), "old.snap", 10, 90);
        let tmp = put(dir.path(), "half.snap.tmp", 10, 2);
        let other = put(dir.path(), "not-ours.txt", 10, 900);
        prune_with(dir.path(), &keep, MAX_AGE, MAX_TOTAL);
        assert!(keep.exists() && fresh.exists());
        assert!(!old.exists(), "a snapshot untouched for 90 days survived");
        assert!(!tmp.exists(), "an interrupted save's temporary survived");
        assert!(other.exists(), "deleted a file fad did not write");
    }

    #[test]
    fn over_the_cap_the_least_recently_used_go_first() {
        let dir = tempfile::tempdir().unwrap();
        let keep = put(dir.path(), "now.snap", 400, 0);
        let newer = put(dir.path(), "newer.snap", 400, 1);
        let older = put(dir.path(), "older.snap", 400, 5);
        let oldest = put(dir.path(), "oldest.snap", 400, 9);
        prune_with(dir.path(), &keep, MAX_AGE, 900);
        assert!(keep.exists(), "evicted the snapshot just written");
        assert!(newer.exists());
        assert!(!older.exists() && !oldest.exists(), "still over the cap");
    }

    #[test]
    fn the_snapshot_just_written_survives_even_alone_over_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let keep = put(dir.path(), "huge.snap", 1000, 0);
        prune_with(dir.path(), &keep, MAX_AGE, 10);
        assert!(keep.exists());
    }
}
