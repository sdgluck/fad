//! Getting rid of things, and getting them back.
//!
//! The default is the macOS Trash, which is recoverable and which Finder can
//! "Put Back". `NSFileManager` hands us the resulting Trash URL, and that URL
//! is what makes an in-app undo possible: the `trash` crate's restore APIs are
//! Linux and Windows only, so the journal here is ours.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender};

use crate::trash;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposal {
    /// Recoverable. Slower, and cannot cross a volume boundary.
    Trash,
    /// Gone.
    Permanent,
}

impl Disposal {
    pub fn label(self) -> &'static str {
        match self {
            Disposal::Trash => "Trash",
            Disposal::Permanent => "permanently delete",
        }
    }
}

/// Paths that must never be deleted no matter what is selected, plus anything
/// at or above the scan root. A tool whose whole job is bulk deletion has to be
/// the one that says no.
pub fn guard(path: &Path, root: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    const NEVER: &[&str] = &[
        "/", "/Applications", "/Library", "/System", "/Users", "/bin", "/etc", "/private",
        "/sbin", "/usr", "/var",
    ];
    #[cfg(not(target_os = "macos"))]
    const NEVER: &[&str] = &[
        "/", "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib32", "/lib64", "/opt",
        "/proc", "/root", "/run", "/sbin", "/srv", "/sys", "/usr", "/var",
    ];

    if NEVER.iter().any(|p| path == Path::new(p)) {
        return Err(format!("{} is a system directory", path.display()));
    }
    if path == root {
        return Err("that is the scan root".into());
    }
    if root.starts_with(path) {
        return Err(format!("{} contains the scan root", path.display()));
    }
    if let Some(home) = crate::paths::home() {
        if path == home {
            return Err("that is your home directory".into());
        }
    }
    if path.components().count() < 2 {
        return Err("that is too close to the filesystem root".into());
    }
    Ok(())
}

pub fn permanent(path: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub path: PathBuf,
    pub bytes: u64,
    /// `Ok(Some(trash_path))` when it can be undone, `Ok(None)` when it is gone.
    pub result: Result<Option<PathBuf>, String>,
}

/// A running batch. Deleting a 40GB disk image takes seconds, and the UI has to
/// stay alive while it happens.
pub struct Job {
    rx: Receiver<Outcome>,
    pub disposal: Disposal,
    pub total: usize,
    pub done: Vec<Outcome>,
    finished: bool,
}

impl Job {
    pub fn start(items: Vec<(PathBuf, u64)>, disposal: Disposal) -> Job {
        let total = items.len();
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || run_batch(items, disposal, tx));
        Job { rx, disposal, total, done: Vec::new(), finished: false }
    }

    /// Take a set of already-trashed paths out of the trash for good.
    ///
    /// The same machinery as a batch, because it is the same shape of work —
    /// a list of paths, one outcome each, on a thread so a forty-gigabyte
    /// `remove_dir_all` cannot freeze the UI. What differs is only what is
    /// done to each path, and that nothing is journalled: these entries are
    /// already in the journal, and after this they are the record of a batch
    /// that can no longer be put back.
    pub fn erase(items: Vec<(PathBuf, u64)>) -> Job {
        let total = items.len();
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            for (path, bytes) in items {
                let result = trash::erase(&path).map(|_| None).map_err(|e| e.to_string());
                if tx.send(Outcome { path, bytes, result }).is_err() {
                    break;
                }
            }
        });
        Job { rx, disposal: Disposal::Permanent, total, done: Vec::new(), finished: false }
    }

    /// Collect finished items. Returns true if anything new arrived.
    pub fn poll(&mut self) -> bool {
        let before = self.done.len();
        loop {
            match self.rx.try_recv() {
                Ok(o) => self.done.push(o),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        self.done.len() != before
    }

    pub fn is_finished(&self) -> bool {
        self.finished || self.done.len() == self.total
    }

    pub fn freed(&self) -> u64 {
        self.done.iter().filter(|o| o.result.is_ok()).map(|o| o.bytes).sum()
    }

    pub fn failures(&self) -> Vec<&Outcome> {
        self.done.iter().filter(|o| o.result.is_err()).collect()
    }
}

fn run_batch(items: Vec<(PathBuf, u64)>, disposal: Disposal, tx: Sender<Outcome>) {
    let mut journal = Vec::new();
    for (path, bytes) in items {
        let result = match disposal {
            Disposal::Trash => trash::trash(&path).map(Some).map_err(|e| e.to_string()),
            Disposal::Permanent => permanent(&path).map(|_| None).map_err(|e| e.to_string()),
        };
        if let Ok(Some(to)) = &result {
            journal.push((path.clone(), to.clone(), bytes));
        }
        if tx.send(Outcome { path, bytes, result }).is_err() {
            break; // UI is gone; stop rather than keep deleting unobserved
        }
    }
    if !journal.is_empty() {
        let _ = write_journal(&journal);
    }
}

// ---------------------------------------------------------------- undo journal

fn journal_path() -> Option<PathBuf> {
    Some(crate::paths::state_dir()?.join("undo.jsonl"))
}

/// One committed batch, appended as a single JSON line.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct Batch {
    pub at: u64,
    pub entries: Vec<Entry>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct Entry {
    pub from: PathBuf,
    pub to: PathBuf,
    pub bytes: u64,
}

fn write_journal(items: &[(PathBuf, PathBuf, u64)]) -> io::Result<()> {
    use std::io::Write;

    const KEEP: usize = 20;

    let Some(path) = journal_path() else {
        return Ok(());
    };
    std::fs::create_dir_all(path.parent().unwrap())?;

    let batch = Batch {
        at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        entries: items
            .iter()
            .map(|(from, to, bytes)| Entry { from: from.clone(), to: to.clone(), bytes: *bytes })
            .collect(),
    };

    let mut batches = read_journal();
    batches.push(batch);
    // Rewrite rather than append, so trimming is the same operation as writing.
    let start = batches.len().saturating_sub(KEEP);
    let mut f = std::fs::File::create(&path)?;
    for b in &batches[start..] {
        writeln!(f, "{}", serde_json::to_string(b)?)?;
    }
    Ok(())
}

pub fn read_journal() -> Vec<Batch> {
    let Some(path) = journal_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

impl Batch {
    pub fn bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.bytes).sum()
    }

    /// The entries still sitting in the trash. An entry the user has since
    /// emptied is gone for good and must not be counted as recoverable.
    pub fn recoverable(&self) -> (usize, u64) {
        self.entries
            .iter()
            .filter(|e| e.to.exists())
            .fold((0, 0), |(n, b), e| (n + 1, b + e.bytes))
    }
}

/// What fad has put in the trash and not yet seen emptied. Deleting to the
/// trash reclaims nothing until the trash goes out, which is a real enough
/// footgun to say out loud.
/// Every item fad trashed that is still sitting in the trash: where it is now,
/// where it came from, and what it costs.
///
/// Only fad's own entries, and only ones still in a real trash directory. What
/// else is in the user's trash is not fad's to touch, and is never counted here
/// or taken out.
pub fn trashed_entries() -> Vec<(PathBuf, PathBuf, u64)> {
    read_journal()
        .iter()
        .flat_map(|b| b.entries.iter())
        .filter(|e| trash::is_trash_path(&e.to) && e.to.exists())
        .map(|e| (e.to.clone(), e.from.clone(), e.bytes))
        .collect()
}

/// How many remembered batches would stop being restorable if the trash were
/// emptied now. The cost of the keystroke, in the only currency the undo
/// history deals in.
pub fn batches_still_restorable() -> usize {
    read_journal().iter().filter(|b| b.recoverable().0 > 0).count()
}

pub fn still_in_trash() -> (usize, u64) {
    read_journal()
        .iter()
        .map(Batch::recoverable)
        .fold((0, 0), |(n, b), (en, eb)| (n + en, b + eb))
}

pub struct UndoReport {
    pub restored: usize,
    pub bytes: u64,
    pub skipped: Vec<(PathBuf, String)>,
}

/// Move the most recent trashed batch back where it came from.
pub fn undo_last() -> Result<UndoReport, String> {
    let n = read_journal().len();
    undo_batch(n.checked_sub(1).ok_or("nothing to undo")?)
}

/// Put one batch back, by its index in `read_journal`. The journal is a stack
/// of the last twenty commits, and there is no reason the only one you can
/// reach is the top of it.
pub fn undo_batch(index: usize) -> Result<UndoReport, String> {
    let mut batches = read_journal();
    if index >= batches.len() {
        return Err("nothing to undo".into());
    }
    let batch = batches.remove(index);

    let mut report = UndoReport { restored: 0, bytes: 0, skipped: Vec::new() };
    for e in &batch.entries {
        if !e.to.exists() {
            report.skipped.push((e.from.clone(), "no longer in the Trash".into()));
            continue;
        }
        if e.from.exists() {
            report.skipped.push((e.from.clone(), "something is there now".into()));
            continue;
        }
        match trash::restore(&e.to, &e.from) {
            Ok(()) => {
                report.restored += 1;
                report.bytes += e.bytes;
            }
            Err(err) => report.skipped.push((e.from.clone(), err.to_string())),
        }
    }

    // The batch is consumed whether or not every item came back; leaving it
    // would offer to restore the same things again.
    if let Some(path) = journal_path() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::File::create(&path) {
            for b in &batches {
                let _ = writeln!(f, "{}", serde_json::to_string(b).unwrap_or_default());
            }
        }
    }
    Ok(report)
}
