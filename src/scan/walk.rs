//! Parallel directory walk.
//!
//! Each directory is one rayon task. A task lstats its entries, ships them as a
//! single [`Batch`], then spawns a task per subdirectory. Subdirectory scan ids
//! are allocated by the discovering task, so a batch always names a parent the
//! tree has already seen (the parent's send happens-before the child task exists,
//! and the channel is FIFO).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crossbeam_channel::Sender;

use super::dir;
use super::meta::Meta;
use super::platform;

/// Scan-side identity of a directory. Dense from 0; the root is always 0.
pub type ScanId = u32;

pub const ROOT_ID: ScanId = 0;

#[derive(Debug)]
pub struct Entry {
    pub name: Box<str>,
    pub meta: Meta,
    /// `Some` for directories we will descend into; the id their batch will use.
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
}

#[derive(Debug)]
pub struct Batch {
    pub parent: ScanId,
    pub entries: Vec<Entry>,
    /// Set when the directory itself could not be read.
    pub unreadable: Option<std::io::ErrorKind>,
}

#[derive(Clone, Debug)]
pub struct ScanOpts {
    /// Follow mount points into other filesystems.
    pub cross_device: bool,
    /// Descend into cloud-provider folders. Off by default: enumerating one can
    /// stall for minutes on a provider that answers over the network.
    pub cloud: bool,
}

impl Default for ScanOpts {
    fn default() -> Self {
        ScanOpts { cross_device: false, cloud: false }
    }
}

#[derive(Debug, Default)]
pub struct Progress {
    pub dirs_done: AtomicU64,
    pub entries_seen: AtomicU64,
    pub unreadable: AtomicU64,
}

struct Ctx {
    tx: Sender<Batch>,
    next_id: AtomicU32,
    root_dev: u64,
    opts: ScanOpts,
    progress: Progress,
}

/// Walk `root`, streaming batches into `tx`. Returns once every directory has
/// been visited; `tx` is dropped on return so the receiver sees a clean close.
pub fn walk(root: PathBuf, root_meta: &Meta, opts: ScanOpts, tx: Sender<Batch>) -> Progress {
    let ctx = Ctx {
        tx,
        next_id: AtomicU32::new(ROOT_ID + 1),
        root_dev: root_meta.dev,
        opts,
        progress: Progress::default(),
    };

    rayon::scope(|s| {
        let ctx = &ctx;
        s.spawn(move |s| scan_dir(s, ctx, root, ROOT_ID));
    });

    drop(ctx.tx);
    ctx.progress
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
        let dir::DirEntry { name, meta } = item;
        let mut skip = None;
        let mut descend = None;
        if meta.is_dir() {
            if !ctx.opts.cross_device && meta.dev != ctx.root_dev {
                skip = Some(Skip::OtherDevice);
            } else if !ctx.opts.cloud && platform::is_cloud_root(&path.join(&*name)) {
                skip = Some(Skip::CloudStorage);
            } else {
                let child_id = ctx.next_id.fetch_add(1, Ordering::Relaxed);
                descend = Some(child_id);
                subdirs.push((path.join(&*name), child_id));
            }
        }

        entries.push(Entry { name, meta, descend, skip });
    }

    ctx.progress.entries_seen.fetch_add(entries.len() as u64, Ordering::Relaxed);
    ctx.progress.dirs_done.fetch_add(1, Ordering::Relaxed);

    // Send before spawning: guarantees the tree has this node's children
    // registered before any grandchild batch can arrive.
    if ctx.tx.send(Batch { parent: id, entries, unreadable: None }).is_err() {
        return; // receiver gone; abandon the subtree
    }

    for (child_path, child_id) in subdirs {
        scope.spawn(move |s| scan_dir(s, ctx, child_path, child_id));
    }
}
