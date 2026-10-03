//! Getting rid of things, and getting them back.
//!
//! The default is the macOS Trash, which is recoverable and which Finder can
//! "Put Back". `NSFileManager` hands us the resulting Trash URL, and that URL
//! is what makes an in-app undo possible: the `trash` crate's restore APIs are
//! Linux and Windows only, so the journal here is ours.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// not strictly inside the scan root. A tool whose whole job is bulk deletion
/// has to be the one that says no.
///
/// Every check is made against the path as given *and* as the filesystem
/// resolves it, because the same directory has more than one name. `$HOME` can
/// be a symlink (or reached through `/var` → `/private/var`), and on macOS
/// every user directory is also reachable through the Data volume's firmlink
/// root: `/System/Volumes/Data/Users` is `/Users` and must be refused as such.
pub fn guard(path: &Path, root: &Path) -> Result<(), String> {
    use std::path::Component;

    // Mount roots and scratch roots on both, so a scan rooted at `/` cannot
    // offer up `/tmp` or a whole attached disk as one entry.
    const SHARED: &[&str] = &[
        "/Volumes", "/mnt", "/media", "/opt", "/tmp", "/var", "/private/tmp", "/private/var",
    ];
    #[cfg(target_os = "macos")]
    const NEVER: &[&str] = &[
        "/", "/Applications", "/Library", "/System", "/Users", "/bin", "/etc", "/private",
        "/sbin", "/usr", "/cores", "/dev",
    ];
    #[cfg(not(target_os = "macos"))]
    const NEVER: &[&str] = &[
        "/", "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib32", "/lib64", "/proc",
        "/root", "/run", "/sbin", "/srv", "/sys", "/usr",
    ];

    // `..` would make every prefix comparison below a lie.
    if path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err("that path is not in plain form".into());
    }
    let names = [path.to_path_buf(), resolved(path)];
    for p in &names {
        if NEVER.iter().chain(SHARED).any(|n| p == Path::new(n)) {
            return Err(format!("{} is a system directory", path.display()));
        }
        // The root of a mounted volume: the whole disk, not something on it.
        if p.parent() == Some(Path::new("/Volumes")) {
            return Err(format!("{} is a mounted volume", path.display()));
        }
    }
    if path == root {
        return Err("that is the scan root".into());
    }
    if root.starts_with(path) {
        return Err(format!("{} contains the scan root", path.display()));
    }
    // Whatever the tree says, nothing outside what was scanned is fad's to
    // touch.
    if !path.starts_with(root) {
        return Err(format!("{} is outside the scan root", path.display()));
    }
    if let Some(home) = crate::paths::home() {
        let homes = [home.clone(), std::fs::canonicalize(&home).unwrap_or(home)];
        if names.iter().any(|p| homes.contains(p)) {
            return Err("that is your home directory".into());
        }
    }
    if path.components().count() < 2 {
        return Err("that is too close to the filesystem root".into());
    }
    if is_mount_point(path) {
        return Err(format!("{} is a mount point", path.display()));
    }
    Ok(())
}

/// `path` as the filesystem resolves it, without resolving its last component
/// (a symlink being deleted is the link, not what it points at). On macOS the
/// Data volume's firmlink prefix is then dropped when the remainder exists at
/// `/`, so `/System/Volumes/Data/Users/x` compares as `/Users/x`.
fn resolved(path: &Path) -> PathBuf {
    let base = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            std::fs::canonicalize(parent).map(|p| p.join(name)).unwrap_or_else(|_| path.into())
        }
        _ => path.to_path_buf(),
    };
    strip_firmlink(&base)
}

#[cfg(target_os = "macos")]
fn strip_firmlink(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("/System/Volumes/Data") {
        let at_root = Path::new("/").join(rest);
        if !rest.as_os_str().is_empty() && at_root.symlink_metadata().is_ok() {
            return at_root;
        }
    }
    path.to_path_buf()
}

#[cfg(not(target_os = "macos"))]
fn strip_firmlink(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// A directory on a different device from its parent: something is mounted
/// there, and deleting it means emptying a whole filesystem.
fn is_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let (Ok(m), Some(parent)) = (std::fs::symlink_metadata(path), path.parent()) else {
        return false;
    };
    m.is_dir() && std::fs::metadata(parent).is_ok_and(|p| p.dev() != m.dev())
}

/// Refuse a directory that holds another filesystem or a cloud folder,
/// before anything in it is touched.
///
/// `remove_dir_all` does not follow symlinks, but it does walk straight
/// through a mount point: a disk image attached under a build directory, a
/// bind mount, a network share someone mounted inside a cache — all emptied
/// along with the directory around them. A cloud provider's folder reports the
/// boot volume's device, so the device check does not see it, and emptying one
/// deletes the files from every machine that syncs it. Trashing is held to the
/// same rule: the move would carry the mount, or the synced folder, with it.
///
/// So the whole tree is walked first, without following symlinks, and one
/// such directory anywhere refuses the whole entry. A directory that cannot be
/// read refuses it too, since what is under it cannot be vouched for — and
/// the delete would fail there anyway, part-way through.
pub fn contained(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    use crate::scan::cloud::is_cloud_root;

    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Ok(());
    }
    if is_cloud_root(path) {
        return Err("it is a cloud-synced folder".into());
    }
    let dev = meta.dev();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let unreadable = |e: io::Error| format!("could not look inside {}: {e}", dir.display());
        for entry in std::fs::read_dir(&dir).map_err(unreadable)? {
            let entry = entry.map_err(unreadable)?;
            // From the directory entry itself: never follows a symlink.
            if !entry.file_type().map_err(unreadable)?.is_dir() {
                continue;
            }
            let p = entry.path();
            let m = std::fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
            if m.dev() != dev {
                return Err(format!("{} is another filesystem mounted inside it", p.display()));
            }
            if is_cloud_root(&p) {
                return Err(format!("{} inside it is a cloud-synced folder", p.display()));
            }
            stack.push(p);
        }
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
    /// Set from the UI to stop the worker before its next item. Checked
    /// between items, never during one: half a `remove_dir_all` is worse than
    /// either the whole thing or none of it.
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn start(items: Vec<(PathBuf, u64)>, disposal: Disposal) -> Job {
        let total = items.len();
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || run_batch(items, disposal, tx, &stop));
        Job { rx, disposal, total, done: Vec::new(), finished: false, cancel }
    }

    /// Stop after the item in hand. What is done stays done and is reported;
    /// the rest is never attempted.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Items the worker never reached because it was stopped.
    pub fn not_attempted(&self) -> usize {
        if self.finished { self.total - self.done.len() } else { 0 }
    }

    /// Take a set of already-trashed paths out of the trash for good.
    ///
    /// The same machinery as a batch, because it is the same shape of work —
    /// a list of paths, one outcome each, on a thread so a forty-gigabyte
    /// `remove_dir_all` cannot freeze the UI. What differs is only what is
    /// done to each path, and that nothing is journalled: these entries are
    /// already in the journal, and after this they are the record of a batch
    /// that can no longer be put back.
    ///
    /// Each path is checked against the journal immediately before it goes:
    /// it has to be one fad recorded, and the thing there now has to be the
    /// thing fad put there (`Entry::check`). Anything else — a path the
    /// journal does not know, an item replaced since, an entry from a journal
    /// too old to say what it was — is reported as a failure and left alone.
    pub fn erase(items: Vec<(PathBuf, u64)>) -> Job {
        let total = items.len();
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let journal = read_journal();
            for (path, bytes) in items {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // The newest record for this path is the one that describes
                // what is there now.
                let entry = journal.iter().flat_map(|b| &b.entries).rev().find(|e| e.to == path);
                let result = match entry.map(Entry::check) {
                    None => Err("not something fad trashed — left alone".to_string()),
                    Some(Err(why)) => Err(why.reason().to_string()),
                    Some(Ok(())) => contained(&path)
                        .map_err(|why| format!("refused: {why}"))
                        .and_then(|()| {
                            trash::erase(&path).map(|_| None).map_err(|e| e.to_string())
                        }),
                };
                if tx.send(Outcome { path, bytes, result }).is_err() {
                    break;
                }
            }
        });
        Job { rx, disposal: Disposal::Permanent, total, done: Vec::new(), finished: false, cancel }
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

fn run_batch(items: Vec<(PathBuf, u64)>, disposal: Disposal, tx: Sender<Outcome>, stop: &AtomicBool) {
    let mut journal = Recorder::new();
    for (path, bytes) in items {
        // Stopped: everything that did go is already on the journal, entry by
        // entry, so `u` puts back exactly what was trashed.
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let result = contained(&path)
            .map_err(|why| format!("refused: {why}"))
            .and_then(|()| match disposal {
                Disposal::Trash => trash::trash(&path).map(Some).map_err(|e| e.to_string()),
                Disposal::Permanent => permanent(&path).map(|_| None).map_err(|e| e.to_string()),
            });
        // Written down the moment the move lands, not when the batch ends: a
        // fad killed halfway through a two-hundred-item batch has still moved
        // the first hundred, and they have to be on the undo list.
        if let Ok(Some(to)) = &result {
            let _ = journal.record(&path, to, bytes);
        }
        if tx.send(Outcome { path, bytes, result }).is_err() {
            break; // UI is gone; stop rather than keep deleting unobserved
        }
    }
}

// ---------------------------------------------------------------- undo journal
//
// One JSON line per write. A batch starts as one line and grows by one more
// line, carrying the same `id`, for every item that lands after the first, so
// recording an entry is an append rather than a rewrite of the whole file.
// Reading merges lines by `id`. A journal written before batches had ids is a
// line per batch, and reads exactly as it always did.
//
// Two fad instances can be writing at once — a `--reclaim --yes` from cron and
// a TUI, say — so every read-modify-write holds an advisory lock on a file
// beside the journal, and every rewrite goes to a temporary file that is
// renamed over the journal. A reader never takes the lock: the rename means it
// sees either the old journal or the new one, and at worst a torn last line
// from an append in flight, which does not parse and is skipped.

/// How many batches the journal remembers.
const KEEP: usize = 20;

fn journal_path() -> Option<PathBuf> {
    Some(crate::paths::state_dir()?.join("undo.jsonl"))
}

/// An exclusive `flock` on `undo.lock`, released when dropped.
///
/// A separate file rather than the journal itself, because the journal is
/// replaced by rename: a lock on the old inode would not stop a second writer
/// that opened the new one.
struct Lock {
    _file: std::fs::File,
}

fn lock() -> io::Result<Lock> {
    use std::os::unix::io::AsRawFd;

    let dir = crate::paths::state_dir().ok_or_else(|| io::Error::other("no state directory"))?;
    std::fs::create_dir_all(&dir)?;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("undo.lock"))?;
    // SAFETY: a descriptor this function owns, for the life of the `Lock`.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Lock { _file: f })
}

/// One committed batch.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Batch {
    /// What `undo_batch` is asked for. Stable across rewrites, unlike a
    /// position in the list: the history screen can be open while another
    /// batch lands or is put back, and an index read off it would then name a
    /// different batch than the one under the cursor. Zero in a journal from
    /// before ids, until `read_journal` gives it one.
    #[serde(default)]
    pub id: u64,
    pub at: u64,
    pub entries: Vec<Entry>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Entry {
    pub from: PathBuf,
    pub to: PathBuf,
    pub bytes: u64,
    /// What fad left at `to`, taken just after the move. `None` in a journal
    /// from before this was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed: Option<Landed>,
}

/// Enough of an inode to tell that the thing at a trash path is still the
/// thing fad put there.
///
/// The journal is a list of paths, and a path is only a name: the user can
/// empty the trash and trash something else that lands under the same name,
/// or restore an item from Finder and have a different one take its slot.
/// Emptying works off those paths and is irreversible, so it has to know it
/// is erasing what fad trashed and not whatever is called that now.
///
/// Taken *after* the move, because a rename moves `ctime` — which is also why
/// `ctime` and `mtime` are not part of it. A directory's length is left out
/// too: it counts entries, and a Finder window opened on the trash can drop a
/// `.DS_Store` into it.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Landed {
    pub dev: u64,
    pub ino: u64,
    /// `f`ile, `d`irectory, `l`ink or `o`ther.
    pub kind: char,
    pub len: u64,
}

impl Landed {
    pub fn of(path: &Path) -> Option<Landed> {
        use std::os::unix::fs::MetadataExt;

        let m = std::fs::symlink_metadata(path).ok()?;
        let t = m.file_type();
        let kind = if t.is_dir() {
            'd'
        } else if t.is_symlink() {
            'l'
        } else if t.is_file() {
            'f'
        } else {
            'o'
        };
        Some(Landed { dev: m.dev(), ino: m.ino(), kind, len: if kind == 'd' { 0 } else { m.len() } })
    }
}

/// What makes an entry not safe to act on, if anything.
#[derive(Debug, PartialEq, Eq)]
pub enum Unsafe {
    /// Nothing at the recorded path.
    Gone,
    /// Something is there, but not what fad put there.
    Replaced,
    /// Recorded before fad kept identities, so there is no telling.
    Unverifiable,
}

impl Unsafe {
    pub fn reason(&self) -> &'static str {
        match self {
            Unsafe::Gone => "no longer in the Trash",
            Unsafe::Replaced => "no longer the thing fad trashed — left alone",
            Unsafe::Unverifiable => {
                "trashed by an older fad that did not record what it was, so it cannot be \
                 checked — left alone; empty it from the Trash yourself"
            }
        }
    }
}

impl Entry {
    /// Is the thing at `to` still what fad trashed?
    pub fn check(&self) -> Result<(), Unsafe> {
        let now = Landed::of(&self.to).ok_or(Unsafe::Gone)?;
        match self.landed {
            None => Err(Unsafe::Unverifiable),
            Some(was) if was == now => Ok(()),
            Some(_) => Err(Unsafe::Replaced),
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A batch id nothing else will have: the clock, mixed with the process id so
/// two instances starting in the same tick still differ, and a per-process
/// counter so two batches in one process do too — macOS's clock only ticks in
/// microseconds, and two batches can start inside one.
fn new_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    ((nanos as u64) ^ (u64::from(std::process::id()) << 40) ^ seq.rotate_right(8)) | 1
}

/// An id for a batch written before batches had them, derived from what it
/// holds so it comes out the same on every read.
fn legacy_id(b: &Batch) -> u64 {
    let mut h = crate::hash::Sha256::default();
    h.update(&b.at.to_le_bytes());
    for e in &b.entries {
        h.update(e.to.as_os_str().as_encoded_bytes());
        h.update(&[0]);
    }
    let d = h.finish();
    u64::from_le_bytes(d[..8].try_into().unwrap()) | 1
}

/// Records one batch into the journal, an entry at a time, as it happens.
///
/// Public so the journal's behaviour can be tested without moving anything
/// into a real trash; `Job` is the only other user.
pub struct Recorder {
    id: u64,
    at: u64,
    started: bool,
}

impl Default for Recorder {
    fn default() -> Self {
        Recorder::new()
    }
}

impl Recorder {
    pub fn new() -> Recorder {
        Recorder { id: new_id(), at: now(), started: false }
    }

    /// Remember that `from` now lives at `to`.
    ///
    /// The first entry starts the batch, and that is also when the journal is
    /// trimmed back to the last `KEEP` batches — a full rewrite, so it goes
    /// through a temporary file. Every later entry is a single appended line.
    pub fn record(&mut self, from: &Path, to: &Path, bytes: u64) -> io::Result<()> {
        use std::io::Write;

        let Some(path) = journal_path() else { return Ok(()) };
        let _lock = lock()?;
        let line = Batch {
            id: self.id,
            at: self.at,
            entries: vec![Entry {
                from: from.to_path_buf(),
                to: to.to_path_buf(),
                bytes,
                landed: Landed::of(to),
            }],
        };
        if !self.started {
            let mut batches = read_at(&path);
            let start = batches.len().saturating_sub(KEEP - 1);
            batches.drain(..start);
            batches.push(line);
            rewrite(&path, &batches)?;
            self.started = true;
            return Ok(());
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        // One `write` of the whole line, so a reader racing it sees all of it
        // or a torn tail it will skip, never half an entry glued to the next.
        f.write_all(format!("{}\n", serde_json::to_string(&line)?).as_bytes())
    }
}

/// Replace the journal with `batches`, atomically: written beside it under a
/// temporary name, flushed, and renamed over it. A crash part-way leaves the
/// old journal whole, where truncating it in place would have left half of it.
fn rewrite(path: &Path, batches: &[Batch]) -> io::Result<()> {
    use std::io::Write;

    let dir = path.parent().ok_or_else(|| io::Error::other("journal has no directory"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".undo.jsonl.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        let mut text = String::new();
        for b in batches {
            text.push_str(&serde_json::to_string(b)?);
            text.push('\n');
        }
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Every remembered batch, oldest first, with an entry-per-line batch put
/// back together and every batch given an id.
pub fn read_journal() -> Vec<Batch> {
    journal_path().map(|p| read_at(&p)).unwrap_or_default()
}

fn read_at(path: &Path) -> Vec<Batch> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut batches: Vec<Batch> = Vec::new();
    for line in text.lines() {
        let Ok(mut b) = serde_json::from_str::<Batch>(line) else { continue };
        if b.id == 0 {
            b.id = legacy_id(&b);
        }
        match batches.iter_mut().find(|x| x.id == b.id) {
            Some(existing) => existing.entries.append(&mut b.entries),
            None => batches.push(b),
        }
    }
    batches
}

/// Is there anything at all at `path`? Not `Path::exists`, which follows a
/// symlink and calls a dangling one absent — so a trashed dangling link read
/// as already emptied, and a dangling link at the restore destination read as
/// a free slot that the restore then replaced.
fn present(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
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
            .filter(|e| present(&e.to))
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
        .filter(|e| trash::is_trash_path(&e.to) && present(&e.to))
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
    let id = read_journal().last().map(|b| b.id).ok_or("nothing to undo")?;
    undo_batch(id)
}

/// Put one batch back, by its id. The journal is a stack of the last twenty
/// commits, and there is no reason the only one you can reach is the top of
/// it — but it is a stack other things push onto and pop from while the
/// history screen is open, so the batch is named by id rather than position.
pub fn undo_batch(id: u64) -> Result<UndoReport, String> {
    let path = journal_path().ok_or("nothing to undo")?;
    // Held across the whole read-restore-rewrite, so a batch landing from
    // another instance meanwhile is not lost when this one writes back.
    let _lock = lock().map_err(|e| format!("could not lock the undo journal: {e}"))?;
    let mut batches = read_at(&path);
    let index = batches
        .iter()
        .position(|b| b.id == id)
        .ok_or("that batch is no longer in the undo history")?;
    let entries = std::mem::take(&mut batches[index].entries);

    let mut report = UndoReport { restored: 0, bytes: 0, skipped: Vec::new() };
    // What could not come back this time but might next time: the original
    // path is occupied, or the move failed. Those stay in the batch so `u`
    // can be pressed again once the way is clear. What is gone from the trash
    // or was replaced there can never come back, and is let go.
    let mut retry = Vec::new();
    for e in entries {
        // Putting back whatever is at the recorded path now would move some
        // other trashed thing into a place the user never had it. An entry
        // from before identities were kept is let through: a restore moves
        // rather than destroys, and refusing would strand every batch an
        // older fad trashed.
        match e.check() {
            Ok(()) | Err(Unsafe::Unverifiable) => {}
            Err(why) => {
                report.skipped.push((e.from.clone(), why.reason().into()));
                continue;
            }
        }
        if present(&e.from) {
            report.skipped.push((e.from.clone(), "something is there now".into()));
            retry.push(e);
            continue;
        }
        match trash::restore(&e.to, &e.from) {
            Ok(()) => {
                report.restored += 1;
                report.bytes += e.bytes;
            }
            Err(err) => {
                report.skipped.push((e.from.clone(), err.to_string()));
                retry.push(e);
            }
        }
    }

    // Only what came back leaves the batch, and the batch leaves the journal
    // only once nothing in it is left to try.
    if retry.is_empty() {
        batches.remove(index);
    } else {
        batches[index].entries = retry;
    }
    rewrite(&path, &batches).map_err(|e| format!("could not update the undo journal: {e}"))?;
    Ok(report)
}
