pub mod dir;
pub mod meta;
pub mod cloud;
pub mod walk;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::tree::Tree;
use meta::Meta;
use walk::{Batch, Progress, ScanOpts};

/// Handle on a running scan. The tree lives with the caller, not in here, so
/// the UI can render a partial tree on every frame while batches keep landing.
pub struct Scan {
    rx: crossbeam_channel::Receiver<Batch>,
    worker: Option<std::thread::JoinHandle<()>>,
    finished: bool,
    progress: Arc<Progress>,
    started: Instant,
}

/// A snapshot of how the walk is doing, for the status line.
pub struct ScanProgress {
    pub dirs: u64,
    pub entries: u64,
    pub current: PathBuf,
    /// Entries per second since the walk started.
    pub rate: f64,
}

impl Scan {
    /// Start walking `root` on a background thread. Returns immediately with an
    /// empty tree and a handle to feed it.
    pub fn start(root: &Path, opts: ScanOpts) -> std::io::Result<(Tree, Scan)> {
        let root = std::fs::canonicalize(root)?;
        let root_meta = Meta::from_metadata(&std::fs::symlink_metadata(&root)?);
        if !root_meta.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not a directory", root.display()),
            ));
        }

        // Opened once up front, so a root that cannot be read at all is an
        // error here — before a UI opens on it or a report prints 0 B for it —
        // rather than one unreadable directory among the results.
        drop(std::fs::read_dir(&root)?);

        let mut tree = Tree::new(root.clone(), &root_meta);
        tree.set_scan_opts(opts.clone());
        // Bounded so a slow consumer applies backpressure instead of letting the
        // walker buffer an entire filesystem in memory.
        let (tx, rx) = crossbeam_channel::bounded(1024);
        let walk_root: PathBuf = root;
        let progress = Arc::new(Progress::default());
        let theirs = Arc::clone(&progress);
        let worker = std::thread::spawn(move || {
            walk::walk(walk_root, &root_meta, opts, tx, theirs);
        });

        Ok((
            tree,
            Scan { rx, worker: Some(worker), finished: false, progress, started: Instant::now() },
        ))
    }

    /// Apply whatever batches are ready without blocking. Returns how many
    /// landed, so a caller can skip the work of rebuilding an unchanged view.
    pub fn drain_ready(&mut self, tree: &mut Tree) -> usize {
        let mut n = 0;
        loop {
            match self.rx.try_recv() {
                Ok(batch) => {
                    tree.apply(batch);
                    n += 1;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.reap();
                    tree.mark_complete(std::time::SystemTime::now());
                    break;
                }
            }
        }
        n
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn progress(&self) -> ScanProgress {
        let entries = self.progress.entries_seen.load(Ordering::Relaxed);
        let secs = self.started.elapsed().as_secs_f64();
        ScanProgress {
            dirs: self.progress.dirs_done.load(Ordering::Relaxed),
            entries,
            current: self.progress.current(),
            rate: if secs > 0.05 { entries as f64 / secs } else { 0.0 },
        }
    }

    fn reap(&mut self) {
        self.finished = true;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }

    /// Block until the walk finishes, applying every batch.
    pub fn finish(mut self, tree: &mut Tree) {
        while let Ok(batch) = self.rx.recv() {
            tree.apply(batch);
        }
        self.reap();
        tree.mark_complete(std::time::SystemTime::now());
    }
}
