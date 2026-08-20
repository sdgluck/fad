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
    const NEVER: &[&str] = &[
        "/",
        "/Applications",
        "/Library",
        "/System",
        "/Users",
        "/bin",
        "/etc",
        "/private",
        "/sbin",
        "/usr",
        "/var",
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
    if let Some(home) = std::env::var_os("HOME") {
        if path == Path::new(&home) {
            return Err("that is your home directory".into());
        }
    }
    if path.components().count() < 2 {
        return Err("that is too close to the filesystem root".into());
    }
    Ok(())
}

/// Move one item to the Trash, returning where it landed.
#[cfg(target_os = "macos")]
pub fn trash(path: &Path) -> io::Result<PathBuf> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let fm = NSFileManager::defaultManager();
    let ns_path = NSString::from_str(&path.to_string_lossy());
    let url = NSURL::fileURLWithPath(&ns_path);

    let mut resulting = None;
    fm.trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
        .map_err(|e| io::Error::other(e.localizedDescription().to_string()))?;

    let landed = resulting
        .and_then(|u| u.path())
        .map(|p| PathBuf::from(p.to_string()))
        // Very old systems may not report the URL back. The item is trashed
        // either way; we just cannot offer to undo it.
        .ok_or_else(|| io::Error::other("macOS did not report where the item was trashed"))?;
    Ok(landed)
}

#[cfg(not(target_os = "macos"))]
pub fn trash(_path: &Path) -> io::Result<PathBuf> {
    Err(io::Error::other("Trash is only implemented on macOS"))
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
            Disposal::Trash => trash(&path).map(Some).map_err(|e| e.to_string()),
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

/// The journal lives in Application Support, not Caches: a cache cleaner is
/// entitled to delete a cache, and losing your undo history to one would be a
/// nasty surprise.
pub fn state_dir() -> Option<PathBuf> {
    // Overridable so tests never touch the real journal: a test run must not be
    // able to consume the undo history of an actual session.
    if let Some(dir) = std::env::var_os("FAD_STATE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/fad"))
}

fn journal_path() -> Option<PathBuf> {
    Some(state_dir()?.join("undo.jsonl"))
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

pub struct UndoReport {
    pub restored: usize,
    pub bytes: u64,
    pub skipped: Vec<(PathBuf, String)>,
}

/// Move the most recent trashed batch back where it came from.
pub fn undo_last() -> Result<UndoReport, String> {
    let mut batches = read_journal();
    let batch = batches.pop().ok_or("nothing to undo")?;

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
        if let Some(parent) = e.from.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                report.skipped.push((e.from.clone(), err.to_string()));
                continue;
            }
        }
        match std::fs::rename(&e.to, &e.from) {
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
