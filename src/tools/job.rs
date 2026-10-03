//! Removing tool resources, in the background, permanently.
//!
//! This mirrors the shape of `delete::Job` — a worker thread streaming outcomes
//! over a channel — but deliberately does not reuse its type. `delete::Outcome`
//! carries `Result<Option<PathBuf>, String>`, where the `PathBuf` is where the
//! item landed in the trash, and that value is the entire basis of the undo
//! journal. A `docker image rm` has no trash path and no undo. Threading `None`
//! through the journal to say "this one cannot actually come back" is how you
//! end up with a `u` that silently does nothing, so nothing here is journalled
//! and nothing here is offered back.
//!
//! The other difference is the last step. A prediction of what a removal will
//! free cannot be exact while layers are shared, so once the batch is done the
//! job asks the tool again and reports the *measured* difference.

use crossbeam_channel::{Receiver, Sender};

use std::collections::BTreeSet;

use super::{Source, ToolKey, docker};

#[derive(Clone, Debug)]
pub struct Outcome {
    pub key: ToolKey,
    /// What to call it on screen.
    pub label: String,
    /// What we expected this to free, for progress only. The figure reported at
    /// the end is measured, not this.
    pub bytes: u64,
    pub result: Result<(), String>,
}

enum Msg {
    Done(Box<Outcome>),
    /// What the tools' own totals fell by across the whole batch. The honest
    /// number, and the only one shown once it arrives.
    Measured(u64),
}

pub struct Job {
    rx: Receiver<Msg>,
    pub total: usize,
    pub done: Vec<Outcome>,
    /// What the tools' own totals fell by, when they could be read both before
    /// and after. `None` once finished means the figure is `expected`, an
    /// estimate, and must be shown as one.
    pub measured: Option<u64>,
    /// True once the worker has hung up, whether or not it measured.
    finished: bool,
}

impl Job {
    pub fn start(items: Vec<(ToolKey, String, u64)>) -> Job {
        let total = items.len();
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || run_batch(items, tx));
        Job { rx, total, done: Vec::new(), measured: None, finished: false }
    }

    /// Collect what has finished. Returns true if anything new arrived.
    pub fn poll(&mut self) -> bool {
        let before = (self.done.len(), self.measured);
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Done(o)) => self.done.push(*o),
                Ok(Msg::Measured(b)) => self.measured = Some(b),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        (self.done.len(), self.measured) != before
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Every removal has been tried and the job is re-asking the tools for
    /// their totals.
    pub fn measuring(&self) -> bool {
        !self.finished && self.done.len() == self.total
    }

    /// What we expected to free, until the measurement lands.
    pub fn expected(&self) -> u64 {
        self.done.iter().filter(|o| o.result.is_ok()).map(|o| o.bytes).sum()
    }

    pub fn failures(&self) -> Vec<&Outcome> {
        self.done.iter().filter(|o| o.result.is_err()).collect()
    }
}

fn run_batch(items: Vec<(ToolKey, String, u64)>, tx: Sender<Msg>) {
    // Which tools this batch touches, so only those get re-measured.
    let sources: BTreeSet<Source> = items.iter().map(|(k, _, _)| k.source).collect();
    let before: Vec<(Source, Option<u64>)> =
        sources.iter().map(|s| (*s, store_bytes(*s))).collect();

    for (key, label, bytes) in items {
        let result = super::remove(&key);
        if tx.send(Msg::Done(Box::new(Outcome { key, label, bytes, result }))).is_err() {
            // The UI is gone. Stop rather than keep removing unobserved, the
            // same rule `delete::run_batch` follows.
            return;
        }
    }

    // Measured only when every tool the batch touched answered both times.
    // A store that could not be sized is unknown, not empty: reading it as
    // zero turned a slow `df` before the batch into "freed 0, measured", and a
    // slow one after into the whole store "freed". Without both ends the job
    // sends nothing, and the estimate stands, labelled as one.
    let Some(was) = before.iter().map(|(_, b)| *b).sum::<Option<u64>>() else { return };
    let Some(after) = before.iter().map(|(s, _)| store_bytes(*s)).sum::<Option<u64>>() else {
        return;
    };
    let _ = tx.send(Msg::Measured(was.saturating_sub(after)));
}

/// Everything a tool says its store costs right now, or `None` when it would
/// not say.
fn store_bytes(source: Source) -> Option<u64> {
    match source {
        Source::Docker | Source::Podman => {
            docker::measure(source).map(|t| t.iter().map(|(_, size, _)| *size).sum())
        }
        // Snapshots have no size to measure; a batch of them reports what it
        // expected and says as much.
        Source::Snapshots => None,
    }
}
