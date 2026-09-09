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

use crate::tree::{Snapshot, Tree};

/// Bumped whenever `Node` or `Tree` change shape, or a field changes meaning.
/// An old snapshot is discarded rather than misread. Version 4 added the
/// per-subtree newest-mtime rollup that the age histogram reads; version 5 adds
/// a third `Skip` reason, which an older reader would take for the first one.
const FORMAT: u32 = 5;
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
    std::fs::rename(&tmp, &path)
}

/// The saved tree, and when it was saved. The timestamp is what turns "40G" on
/// screen into "+12G since Tuesday", which is the more actionable of the two.
pub fn load(root: &Path) -> Option<(Tree, std::time::SystemTime)> {
    // Snapshots are keyed by the canonical path, because that is what the scan
    // recorded. Without this, `fad /var/x` and `fad /private/var/x` would each
    // keep their own copy and neither would ever find the other's.
    let root = &std::fs::canonicalize(root).ok()?;
    let path = snapshot_path(root)?;
    let saved_at = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let bytes = std::fs::read(path).ok()?;
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
    Some((tree, saved_at))
}

pub fn clear() -> io::Result<()> {
    let Some(dir) = crate::paths::cache_dir() else { return Ok(()) };
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
