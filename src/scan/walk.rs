//! Parallel directory walk.
//!
//! Each directory is one rayon task. A task lstats its entries, ships them as a
//! single [`Batch`], then spawns a task per subdirectory. Subdirectory scan ids
//! are allocated by the discovering task, so a batch always names a parent the
//! tree has already seen (the parent's send happens-before the child task exists,
//! and the channel is FIFO).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crossbeam_channel::Sender;

use super::dir;
use super::meta::Meta;
use super::cloud;

/// Scan-side identity of a directory. Dense from 0; the root is always 0.
pub type ScanId = u32;

pub const ROOT_ID: ScanId = 0;

#[derive(Debug)]
pub struct Entry {
    pub name: Box<str>,
    pub meta: Meta,
    /// `Some` for directories we will descend into; the id their batch will use.
    ///
    /// A directory with neither this nor `skip` is one the walk has already
    /// entered by another path — a macOS firmlink, a bind mount, a hard-linked
    /// directory. Its `meta` sizes are zeroed, because they are counted where
    /// the walk went in, and it has no children of its own to wait for.
    pub descend: Option<ScanId>,
    /// Why we did not descend, if we did not.
    pub skip: Option<Skip>,
}

/// A directory we deliberately stopped at, and the reason to show the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Skip {
    /// A mount point for another filesystem.
    OtherDevice,
    /// A cloud provider's folder. Enumerating it can block on the network.
    CloudStorage,
    /// The name is not valid UTF-8, so fad cannot rebuild a path that reaches
    /// it. See `dir::DirEntry::representable`.
    UnrepresentableName,
}

#[derive(Debug)]
pub struct Batch {
    pub parent: ScanId,
    pub entries: Vec<Entry>,
    /// Set when the directory itself could not be read.
    pub unreadable: Option<std::io::ErrorKind>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanOpts {
    /// Follow mount points into other filesystems.
    pub cross_device: bool,
    /// Descend into cloud-provider folders. Off by default: enumerating one can
    /// stall for minutes on a provider that answers over the network.
    pub cloud: bool,
}

/// What the walk has got through so far. Shared with the UI, which is the
/// difference between a scan that looks stuck and one that says where it is.
#[derive(Debug, Default)]
pub struct Progress {
    pub dirs_done: AtomicU64,
    pub entries_seen: AtomicU64,
    pub unreadable: AtomicU64,
    /// The directory some worker is in right now. Written with `try_lock` and
    /// skipped on contention: this is a status line, and blocking eight walker
    /// threads to keep it exact would cost more than it is worth.
    current: Mutex<PathBuf>,
}

impl Progress {
    fn note(&self, path: &std::path::Path) {
        if let Ok(mut cur) = self.current.try_lock() {
            cur.clear();
            cur.push(path);
        }
    }

    pub fn current(&self) -> PathBuf {
        self.current.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

struct Ctx {
    tx: Sender<Batch>,
    next_id: AtomicU32,
    root_dev: u64,
    opts: ScanOpts,
    progress: std::sync::Arc<Progress>,
    seen: Seen,
    /// Directories held back until everything else has been walked, with the
    /// ids their batches will use. See `DEFER`.
    deferred: Mutex<Vec<(PathBuf, ScanId)>>,
    defer: &'static [&'static str],
}

/// Directory inodes on the root's device that the walk has already entered.
///
/// macOS joins its two boot volumes with firmlinks: `/Users` and
/// `/System/Volumes/Data/Users` are the same directory — same `st_dev`, same
/// `st_ino` — reachable by two paths, and `fad /` walked and counted both, so
/// the whole Data volume appeared twice. A bind mount on Linux, or one of Time
/// Machine's hard-linked directories on HFS+, is the same shape. Claiming each
/// directory inode once, at the moment it is discovered, means the second path
/// to it is listed but not entered or counted.
///
/// Only directories on the root's device go through here: across a mount the
/// same inode number means nothing, and those are skipped or cross-device
/// anyway. Sharded, because every directory in the walk takes one of these
/// locks and a single mutex would put eight walker threads in a queue for it.
struct Seen {
    shards: Vec<Mutex<HashSet<u64>>>,
}

impl Seen {
    const SHARDS: usize = 64;

    fn new() -> Seen {
        Seen { shards: (0..Self::SHARDS).map(|_| Mutex::new(HashSet::new())).collect() }
    }

    /// True the first time an inode is offered, false every time after.
    fn claim(&self, ino: u64) -> bool {
        // Inode numbers are dense and sequential on most filesystems, so the
        // low bits spread well enough on their own.
        let shard = &self.shards[(ino % Self::SHARDS as u64) as usize];
        shard.lock().map(|mut s| s.insert(ino)).unwrap_or(true)
    }
}

/// Paths walked only after the rest of the tree, so that when something under
/// them is also reachable from elsewhere, the elsewhere is the path that keeps
/// the size.
///
/// The Data volume's firmlinked directories are reachable from both `/Users`
/// and `/System/Volumes/Data/Users`, and in a parallel walk whichever task got
/// there first would win — a different one each run, and usually the one
/// nobody would think to look under. Holding the Data mount back until the
/// rest is done means every firmlink has already been claimed by its everyday
/// path, and only what exists solely on the Data side is counted there.
#[cfg(target_os = "macos")]
const DEFER: &[&str] = &["/System/Volumes/Data"];
#[cfg(not(target_os = "macos"))]
const DEFER: &[&str] = &[];

/// Walk `root`, streaming batches into `tx`. Returns once every directory has
/// been visited; `tx` is dropped on return so the receiver sees a clean close.
pub fn walk(
    root: PathBuf,
    root_meta: &Meta,
    opts: ScanOpts,
    tx: Sender<Batch>,
    progress: std::sync::Arc<Progress>,
) {
    walk_deferring(root, root_meta, opts, tx, progress, DEFER, &[]);
}

/// `walk`, with the deferred paths and any already-claimed directory inodes
/// supplied. Split out so a test can stand in for a firmlink, which nothing
/// short of the OS installer can create.
fn walk_deferring(
    root: PathBuf,
    root_meta: &Meta,
    opts: ScanOpts,
    tx: Sender<Batch>,
    progress: std::sync::Arc<Progress>,
    defer: &'static [&'static str],
    preclaimed: &[u64],
) {
    let ctx = Ctx {
        tx,
        next_id: AtomicU32::new(ROOT_ID + 1),
        root_dev: root_meta.dev,
        opts,
        progress,
        seen: Seen::new(),
        deferred: Mutex::new(Vec::new()),
        defer,
    };
    ctx.seen.claim(root_meta.ino);
    for ino in preclaimed {
        ctx.seen.claim(*ino);
    }

    rayon::scope(|s| {
        let ctx = &ctx;
        s.spawn(move |s| scan_dir(s, ctx, root, ROOT_ID));
    });
    // The parent of each deferred directory sent its batch before deferring
    // it, so the tree already knows the ids these batches will name.
    loop {
        let later = std::mem::take(&mut *ctx.deferred.lock().unwrap_or_else(|e| e.into_inner()));
        if later.is_empty() {
            break;
        }
        rayon::scope(|s| {
            let ctx = &ctx;
            for (path, id) in later {
                s.spawn(move |s| scan_dir(s, ctx, path, id));
            }
        });
    }

    drop(ctx.tx);
}

fn scan_dir<'s>(scope: &rayon::Scope<'s>, ctx: &'s Ctx, path: PathBuf, id: ScanId) {
    let read = match dir::read_dir_stat(&path) {
        Ok(r) => r,
        Err(e) => {
            ctx.progress.unreadable.fetch_add(1, Ordering::Relaxed);
            let _ = ctx.tx.send(Batch {
                parent: id,
                entries: Vec::new(),
                unreadable: Some(e.kind()),
            });
            return;
        }
    };

    let mut entries = Vec::new();
    let mut subdirs = Vec::new();

    for item in read {
        let dir::DirEntry { name, representable, mut meta } = item;
        let mut skip = None;
        let mut descend = None;
        // Before anything else: every branch below joins this name onto a path,
        // and one that does not round-trip would name a different file — or
        // nothing at all.
        if !representable {
            skip = Some(Skip::UnrepresentableName);
        } else if meta.is_dir() {
            if !ctx.opts.cross_device && meta.dev != ctx.root_dev {
                skip = Some(Skip::OtherDevice);
            } else if !ctx.opts.cloud && cloud::is_cloud_root(&path.join(&*name)) {
                skip = Some(Skip::CloudStorage);
            } else if meta.dev == ctx.root_dev && !ctx.seen.claim(meta.ino) {
                // Entered already by another path; see `Seen`. Its own inode
                // is counted there too, so it costs nothing here.
                meta.blocks = 0;
                meta.len = 0;
            } else {
                let child_id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
                descend = Some(child_id);
                let child = path.join(&*name);
                if ctx.defer.iter().any(|d| child == Path::new(d)) {
                    ctx.deferred.lock().unwrap_or_else(|e| e.into_inner()).push((child, child_id));
                } else {
                    subdirs.push((child, child_id));
                }
            }
        }

        entries.push(Entry { name, meta, descend, skip });
    }

    ctx.progress.entries_seen.fetch_add(entries.len() as u64, Ordering::Relaxed);
    ctx.progress.dirs_done.fetch_add(1, Ordering::Relaxed);
    ctx.progress.note(&path);

    // Send before spawning: guarantees the tree has this node's children
    // registered before any grandchild batch can arrive.
    if ctx.tx.send(Batch { parent: id, entries, unreadable: None }).is_err() {
        return; // receiver gone; abandon the subtree
    }

    for (child_path, child_id) in subdirs {
        scope.spawn(move |s| scan_dir(s, ctx, child_path, child_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Tree;

    /// `a/f` and `b/g`, so a batch can be told apart by what it holds.
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for (sub, file) in [("a", "f"), ("b", "g")] {
            std::fs::create_dir(root.join(sub)).unwrap();
            std::fs::write(root.join(sub).join(file), vec![1u8; 64 * 1024]).unwrap();
        }
        (dir, root)
    }

    /// Walk, and return the tree along with each batch's entry names in the
    /// order they arrived.
    fn run(
        root: &Path,
        defer: &'static [&'static str],
        preclaimed: &[u64],
    ) -> (Tree, Vec<Vec<String>>) {
        let meta = Meta::from_metadata(&std::fs::symlink_metadata(root).unwrap());
        let (tx, rx) = crossbeam_channel::unbounded();
        let progress = std::sync::Arc::new(Progress::default());
        walk_deferring(root.to_path_buf(), &meta, ScanOpts::default(), tx, progress, defer, preclaimed);
        let mut tree = Tree::new(root.to_path_buf(), &meta);
        let mut order = Vec::new();
        for b in rx.iter() {
            let mut names: Vec<String> = b.entries.iter().map(|e| e.name.to_string()).collect();
            names.sort();
            order.push(names);
            tree.apply(b);
        }
        (tree, order)
    }

    fn ino(p: &Path) -> u64 {
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p).unwrap())
    }

    /// A firmlink cannot be made outside the OS installer, so `b` is handed in
    /// as already entered — exactly what the walk sees on reaching
    /// `/System/Volumes/Data/Users` after `/Users`.
    #[test]
    fn a_directory_entered_by_another_path_is_not_counted_twice() {
        let (_dir, root) = fixture();
        let (tree, _) = run(&root, &[], &[ino(&root.join("b"))]);
        let a = tree.find_path(&root.join("a")).unwrap();
        let b = tree.find_path(&root.join("b")).unwrap();
        assert!(tree.node(b).children.is_empty(), "walked into it a second time");
        assert_eq!(tree.node(b).total_bytes, 0, "counted it a second time");
        assert_ne!(tree.node(b).flags & crate::tree::flags::SCANNED, 0, "left looking unfinished");
        let r = tree.node(tree.root());
        assert_eq!(r.total_bytes, r.self_bytes + tree.node(a).total_bytes);
    }

    #[test]
    fn the_same_inode_is_claimed_once() {
        let seen = Seen::new();
        assert!(seen.claim(42));
        assert!(!seen.claim(42));
        assert!(seen.claim(42 + Seen::SHARDS as u64), "a shard neighbour is not the same inode");
    }

    /// The deferred path is walked last, so anything also reachable from
    /// elsewhere has already been claimed by its everyday path.
    #[test]
    fn a_deferred_directory_is_walked_after_everything_else() {
        let (_dir, root) = fixture();
        std::fs::create_dir_all(root.join("b/deep/er")).unwrap();
        let a: &'static str =
            Box::leak(root.join("a").to_string_lossy().into_owned().into_boxed_str());
        let defer: &'static [&'static str] = Box::leak(vec![a].into_boxed_slice());
        let (tree, order) = run(&root, defer, &[]);

        assert_eq!(order.len(), 5, "root, a, b, deep, er: {order:?}");
        assert_eq!(order.last().unwrap(), &vec!["f".to_string()], "not last: {order:?}");
        let a_id = tree.find_path(&root.join("a")).unwrap();
        assert_eq!(tree.node(a_id).children.len(), 1, "landed somewhere other than `a`");
        assert_eq!(tree.node(tree.root()).file_count, 2);
    }
}
