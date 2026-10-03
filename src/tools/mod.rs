//! Disk held by tools that do not put it where a walk can see it.
//!
//! Everything else in `fad` is a path with a size. Docker is not: its storage
//! is a pool of shared layers behind a daemon, and on macOS the whole pool sits
//! inside one VM disk image that a scan can only report as a single opaque
//! forty-gigabyte file. Which image is dangling, and how much of the build
//! cache is cold, are facts that exist only inside the tool. The only way to
//! get them is to ask it.
//!
//! Sizes here are therefore *reported*, not measured, and they sit on a
//! different axis from the tree: they are not under the scan root and are never
//! added to its totals. Three things follow, and together they are the reason
//! this is a module rather than a few more rows in `presets.rs`:
//!
//! * A reported per-item size cannot be summed. Image layers are shared, so two
//!   images each reporting 3.24GB can occupy 3.995GB between them. [`Measure`]
//!   is how a row says which of its bytes are its own.
//! * Removing something may free nothing on the host. See [`Backing`].
//! * Some of this storage is a file under the scan root, so the tree has
//!   already counted it. Presenting it as newly found space is the same lie
//!   inverted. See `App::tool_in_tree`.

pub mod docker;
pub mod exec;
mod job;
#[cfg(target_os = "macos")]
pub mod snapshots;

use std::path::PathBuf;
use std::time::Duration;

pub use job::{Job, Outcome};

/// How long one question to a tool may take before we conclude it is not
/// answering.
///
/// Deliberately generous. `docker system df` is not a lookup — the daemon walks
/// its own store to answer it, and on a machine with a few hundred build-cache
/// records that measured just under sixteen seconds with nothing whatsoever
/// wrong. A tighter budget would report a healthy daemon as dead, which is a
/// worse failure than waiting: the view is asynchronous and says how long it
/// has been asking, so a slow answer reads as work rather than as a hang.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(45);
/// `info` is not the cheap question its name suggests: measured at twenty-two
/// seconds against a healthy daemon on this machine. It gets the same budget as
/// anything else that has to wait for the daemon to think.
pub const INFO_TIMEOUT: Duration = PROBE_TIMEOUT;
/// Removal is real work and is allowed to take longer.
pub const REMOVE_TIMEOUT: Duration = Duration::from_secs(30);

/// A tool that keeps disk of its own.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Docker,
    Podman,
    /// macOS Time Machine local snapshots. Invisible to `du` entirely — they
    /// are below the filesystem, not in it.
    Snapshots,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Docker => "docker",
            Source::Podman => "podman",
            Source::Snapshots => "time machine",
        }
    }

    /// The program to ask.
    pub fn program(self) -> &'static str {
        match self {
            Source::Docker => "docker",
            Source::Podman => "podman",
            Source::Snapshots => "tmutil",
        }
    }

    /// Environment override for the binary, so the whole feature can be tested
    /// against a stub script with no daemon anywhere. This is the hinge the
    /// test plan turns on.
    pub fn bin_var(self) -> &'static str {
        match self {
            Source::Docker => "FAD_DOCKER_BIN",
            Source::Podman => "FAD_PODMAN_BIN",
            Source::Snapshots => "FAD_TMUTIL_BIN",
        }
    }

    /// The binary to actually run, honouring the override.
    pub fn bin(self) -> String {
        std::env::var(self.bin_var()).unwrap_or_else(|_| self.program().to_string())
    }

    pub fn all() -> &'static [Source] {
        #[cfg(target_os = "macos")]
        {
            &[Source::Docker, Source::Podman, Source::Snapshots]
        }
        #[cfg(not(target_os = "macos"))]
        {
            &[Source::Docker, Source::Podman]
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Container,
    Image,
    Volume,
    BuildCache,
    Snapshot,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Container => "containers",
            Kind::Image => "images",
            Kind::Volume => "volumes",
            Kind::BuildCache => "build cache",
            Kind::Snapshot => "local snapshots",
        }
    }

    /// What the user needs to know before staging the whole group, in the same
    /// spirit as `presets::Category::note`.
    pub fn note(self) -> &'static str {
        match self {
            Kind::Container => "stopped ones only; the writable layer goes too",
            Kind::Image => "re-pulled or rebuilt on demand",
            Kind::Volume => "whatever is in them is not backed up anywhere",
            Kind::BuildCache => "your next build is slower, not broken",
            Kind::Snapshot => "Time Machine makes them again on its own schedule",
        }
    }

    /// Order in the view: cheapest to lose first, data last.
    pub fn all() -> [Kind; 5] {
        [Kind::BuildCache, Kind::Image, Kind::Container, Kind::Volume, Kind::Snapshot]
    }
}

/// How literally an item's size may be taken.
///
/// The most important type in this file. `docker image ls` sizes overlap — two
/// images built on the same base each report the whole base — so adding up a
/// column of them produces a number that is simply false. This is how a row
/// says which of its bytes are its own.
///
/// In every variant, `Resource::bytes` is the summable figure. `shared` is
/// never summed and never counted as reclaimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// Removing this returns exactly `bytes`.
    Exact,
    /// `bytes` is this item's own storage; `shared` is what it holds in common
    /// with its siblings and which removing it alone will not free.
    Unique { shared: u64 },
    /// The tool will not tell us what this costs. Shown with no size at all
    /// rather than a guess — `tmutil` has no per-snapshot size and is not going
    /// to be given an invented one.
    Unknown,
}

/// Where the bytes physically are, which decides whether removing something
/// gives the host disk anything back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backing {
    /// Native. Bytes freed are bytes freed.
    Host,
    /// One VM disk image holding the lot. Removing an image frees space
    /// *inside* that file; whether the host gets it back is up to the runtime.
    /// OrbStack trims its own disk, so `shrinks` is true and the space really
    /// does come back. Docker Desktop and Colima grow their disk and never
    /// shrink it, so it does not, until something compacts it.
    ///
    /// Reporting reclaimed bytes as free host space in that second case would
    /// be a lie, so the view says which case it is in and `App::after_commit`
    /// leaves those bytes out of the "free space after this batch" line.
    VmDisk { disk: Option<PathBuf>, host_bytes: Option<u64>, shrinks: bool },
}

impl Backing {
    /// Does freeing bytes here give them back to the user's disk?
    pub fn frees_host_space(&self) -> bool {
        match self {
            Backing::Host => true,
            Backing::VmDisk { shrinks, .. } => *shrinks,
        }
    }

    /// The file the tree has already counted, when there is one.
    pub fn disk(&self) -> Option<&PathBuf> {
        match self {
            Backing::Host => None,
            Backing::VmDisk { disk, .. } => disk.as_ref(),
        }
    }

    /// The caveat to show, when there is one, as one short line per idea.
    ///
    /// Split rather than wrapped, and with the consequence first, because this
    /// is drawn into a pane that is routinely under sixty columns. A single
    /// sentence gets truncated and the clause that gets cut is the one at the
    /// end — which in the first draft of this was "does not shrink", the entire
    /// point of the warning. Every line here is kept under forty-five
    /// characters so that the prefix and the meaning both survive.
    pub fn notes(&self) -> Vec<String> {
        let Backing::VmDisk { disk, host_bytes, shrinks } = self else { return Vec::new() };
        let name = disk
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("a VM disk");
        let size = host_bytes
            .map(|b| format!(", {} here", crate::format::human(b)))
            .unwrap_or_default();
        if *shrinks {
            vec![format!("inside {name}{size}, shrinks itself")]
        } else {
            vec![
                "this will not free space on your disk".to_string(),
                format!("inside {name}{size}, never shrinks"),
            ]
        }
    }
}

/// A stable handle for a staged item.
///
/// Staging cannot be an index into the report: `R` re-probes, the report is
/// replaced wholesale, and an index that meant "the dangling image" a moment
/// ago would silently come to mean something else. An id survives that.
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct ToolKey {
    pub source: Source,
    pub kind: Kind,
    pub id: String,
}

/// One thing a tool is holding.
///
/// The handle is an id, not a path, which is the whole reason this cannot be a
/// `tree::Node`.
#[derive(Clone, Debug)]
pub struct Resource {
    pub source: Source,
    pub kind: Kind,
    /// What its removal command takes: an image id, a container id, a volume
    /// name, a snapshot date. Never a path.
    pub id: String,
    /// What the user recognises it by: `nginx:latest`, `old-db`, `pgdata`.
    pub name: String,
    /// This item's own storage, and the only figure that may be summed.
    pub bytes: u64,
    pub measure: Measure,
    /// The tool's own rendering, e.g. `3.24GB`, so the detail pane can show the
    /// same string the user sees in `docker system df`. Docker counts in SI and
    /// `fad` counts in binary, so the two will not match digit for digit and
    /// showing both is the only way that is not baffling.
    pub reported: String,
    /// Nothing else is using it: a dangling image, a stopped container, an
    /// unreferenced volume, cold build cache.
    pub idle: bool,
    /// Removal is refused, and why. Never touch a running container, an image a
    /// container is using, or a volume something is attached to.
    pub blocked: Option<String>,
    /// The tool's own phrasing for when this was last touched — "9 months ago".
    /// Kept as the tool wrote it rather than parsed into a timestamp: Docker
    /// stamps these in three different formats and a wrong date is worse than
    /// the tool's own words.
    pub last_used: Option<String>,
    /// What puts it back, where we can name it. The same promise
    /// `presets::rebuild_command` makes for a build directory.
    pub restore: Option<String>,
    /// Where it lives, for the things that have a path. Used by the detail pane
    /// and to notice that the tree has already counted it — never for removal,
    /// because the daemon's own command is always the right handle.
    pub path: Option<PathBuf>,
    /// Lines of context for the detail pane, already phrased.
    pub detail: Vec<String>,
}

impl Resource {
    pub fn key(&self) -> ToolKey {
        ToolKey { source: self.source, kind: self.kind, id: self.id.clone() }
    }

    /// Storage this shares with its siblings, which removing it will not free.
    pub fn shared(&self) -> u64 {
        match self.measure {
            Measure::Unique { shared } => shared,
            _ => 0,
        }
    }

    pub fn sized(&self) -> bool {
        self.measure != Measure::Unknown
    }

    /// Can this go?
    pub fn removable(&self) -> bool {
        self.blocked.is_none()
    }
}

/// The command that removes a resource, as program plus arguments.
///
/// Build cache has no per-record removal in any Docker CLI, so a build-cache
/// resource is always the whole cold pool as one item and prunes in one call.
/// The image case deliberately omits `-f`, leaving the daemon's own in-use
/// check as a second gate behind `Resource::blocked`.
pub fn remove_command(key: &ToolKey) -> (String, Vec<String>) {
    let program = key.source.bin();
    let args: Vec<String> = match (key.source, key.kind) {
        (Source::Snapshots, _) | (_, Kind::Snapshot) => {
            vec!["deletelocalsnapshots".into(), key.id.clone()]
        }
        (_, Kind::Container) => vec!["container".into(), "rm".into(), key.id.clone()],
        (_, Kind::Image) => vec!["image".into(), "rm".into(), key.id.clone()],
        (_, Kind::Volume) => vec!["volume".into(), "rm".into(), key.id.clone()],
        (_, Kind::BuildCache) => vec!["builder".into(), "prune".into(), "--force".into()],
    };
    (program, args)
}

/// The removal command as one line, in full.
///
/// What `--tools --json` publishes, what `--tools --yes` prints before it runs,
/// and what `y` puts on the clipboard: all three want something that can be
/// pasted or logged without ambiguity. Uses the tool's real name even when a
/// test override is in force.
pub fn remove_line(key: &ToolKey) -> String {
    let (_, args) = remove_command(key);
    format!("{} {}", key.source.program(), args.join(" "))
}

/// The same command, shortened to fit a pane.
///
/// A `sha256:` digest is cut to the twelve characters Docker itself prints and
/// accepts. `fad` always runs the full id — there is no ambiguity to risk — but
/// seventy characters of hex is not a line anyone reads, and a command nobody
/// can read is one nobody can check.
pub fn remove_display(key: &ToolKey) -> String {
    let (_, args) = remove_command(key);
    let args: Vec<String> = args.iter().map(|a| abbreviate(a)).collect();
    format!("{} {}", key.source.program(), args.join(" "))
}

fn abbreviate(arg: &str) -> String {
    match arg.strip_prefix("sha256:") {
        Some(hex) if hex.len() > 12 => hex[..12].to_string(),
        _ => arg.to_string(),
    }
}

/// Actually remove it. Runs on the removal worker thread, where blocking is
/// already the arrangement.
pub fn remove(key: &ToolKey) -> Result<(), String> {
    let (program, args) = remove_command(key);
    let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match exec::run(&program, &argv, REMOVE_TIMEOUT) {
        Ok(_) => Ok(()),
        Err(exec::ExecErr::NotInstalled) => {
            Err(format!("{} is not installed", key.source.program()))
        }
        Err(exec::ExecErr::TimedOut) => Err(format!("{} did not answer", key.source.program())),
        Err(exec::ExecErr::Failed { stderr, .. }) => Err(tail(&stderr)),
    }
}

/// The last useful line of a tool's complaint. Daemons are wordy and the modal
/// has one line.
pub(crate) fn tail(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("failed")
        .to_string()
}

/// What happened when we asked. A missing tool is not an error, and a stopped
/// daemon is not the same thing as a broken one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not on `PATH`. The common case, and not shown at all.
    Missing,
    /// Installed; the daemon is not answering. Worth saying, because "docker is
    /// holding 20G" is the wrong thing to imply when we do not actually know.
    NotRunning(String),
    Ok,
    Failed(String),
    /// It was still going at the deadline and was killed.
    TimedOut,
    /// Too old for the machine-readable output we rely on. Reported, never
    /// scraped: a number taken off a human-aligned table is not a number to
    /// delete by.
    Unsupported { need: &'static str, found: String },
}

impl Status {
    /// One line for the view.
    pub fn line(&self, source: Source) -> Option<String> {
        let t = source.program();
        match self {
            Status::Missing | Status::Ok => None,
            Status::NotRunning(_) => Some(format!("{t} is installed but not running")),
            Status::TimedOut => Some(format!("{t} did not answer in time")),
            Status::Failed(e) => Some(format!("{t} failed: {e}")),
            Status::Unsupported { need, found } => {
                Some(format!("{t} {found} is too old; {need} or newer reports sizes as JSON"))
            }
        }
    }
}

pub struct SourceReport {
    pub source: Source,
    pub status: Status,
    pub backing: Backing,
    pub items: Vec<Resource>,
    /// The tool's own deduplicated totals: (kind, size, reclaimable). Never
    /// derived from `items`, because summing `items` is exactly the mistake
    /// this field exists to prevent.
    pub totals: Vec<(Kind, u64, u64)>,
}

impl SourceReport {
    pub fn empty(source: Source, status: Status) -> SourceReport {
        SourceReport { source, status, backing: Backing::Host, items: Vec::new(), totals: Vec::new() }
    }

    /// The tool's figures for a kind, or `None` when it does not report any.
    pub fn total(&self, kind: Kind) -> Option<(u64, u64)> {
        self.totals.iter().find(|(k, _, _)| *k == kind).map(|(_, s, r)| (*s, *r))
    }

    /// Indices into `items` for one kind, biggest first.
    pub fn items_of(&self, kind: Kind) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, r)| r.kind == kind)
            .map(|(i, _)| i)
            .collect();
        v.sort_by_key(|i| std::cmp::Reverse(self.items[*i].bytes));
        v
    }

    /// Kinds this source has anything to say about, in display order.
    pub fn kinds(&self) -> Vec<Kind> {
        Kind::all()
            .into_iter()
            .filter(|k| self.total(*k).is_some() || self.items.iter().any(|r| r.kind == *k))
            .collect()
    }
}

pub struct Report {
    pub sources: Vec<SourceReport>,
}

impl Report {
    /// Ask every tool. Sources are probed on their own threads: a wedged Docker
    /// should not hold a healthy Podman up for the full timeout.
    pub fn probe() -> Report {
        let handles: Vec<_> = Source::all()
            .iter()
            .copied()
            .map(|s| std::thread::spawn(move || probe_one(s)))
            .collect();
        let sources = handles
            .into_iter()
            .zip(Source::all())
            .map(|(h, s)| {
                h.join().unwrap_or_else(|_| {
                    SourceReport::empty(*s, Status::Failed("probe panicked".into()))
                })
            })
            // A tool nobody has installed is not news.
            .filter(|r| r.status != Status::Missing)
            .collect();
        Report { sources }
    }

    pub fn get(&self, key: &ToolKey) -> Option<&Resource> {
        self.sources
            .iter()
            .filter(|s| s.source == key.source)
            .flat_map(|s| s.items.iter())
            .find(|r| r.kind == key.kind && r.id == key.id)
    }

    pub fn source(&self, source: Source) -> Option<&SourceReport> {
        self.sources.iter().find(|s| s.source == source)
    }

    pub fn items(&self) -> impl Iterator<Item = &Resource> {
        self.sources.iter().flat_map(|s| s.items.iter())
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Everything worth offering unattended, largest first: idle only, nothing
    /// held by something else, nothing below `min_size`.
    ///
    /// The UI and `--tools --yes` both come through here for the reason
    /// `reclaim::candidates` exists — a flag that removes things the UI never
    /// offered is a flag nobody can trust.
    pub fn candidates(&self, min_size: u64) -> Vec<ToolKey> {
        let mut v: Vec<&Resource> = self
            .items()
            .filter(|r| r.idle && r.removable() && r.sized() && r.bytes >= min_size)
            .collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.bytes));
        v.into_iter().map(|r| r.key()).collect()
    }
}

fn probe_one(source: Source) -> SourceReport {
    match source {
        Source::Docker | Source::Podman => docker::probe(source),
        #[cfg(target_os = "macos")]
        Source::Snapshots => snapshots::probe(),
        #[cfg(not(target_os = "macos"))]
        Source::Snapshots => SourceReport::empty(source, Status::Missing),
    }
}

/// What a staged set of tool items would free.
///
/// Never a single confident figure, because the data does not support one.
/// `UniqueSize` is exactly what removing one image alone gives back, so for a
/// single item it *is* the answer. Remove two images that share a base and you
/// also get the base — a figure that depends on which images share what, which
/// `docker system df` does not say. So:
///
/// * one item, or items that share nothing: [`Freed::Exact`].
/// * every item of a kind at once: [`Freed::Exact`], from the tool's own total
///   for that kind, since nothing is left behind to hold a shared layer.
/// * any other subset of things that share: [`Freed::AtLeast`], the sum of what
///   each owns outright.
///
/// The bound only ever errs low. Overstating what a delete tool will give back
/// is the one direction that cannot be forgiven, and the figure reported after
/// the batch is measured rather than predicted anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freed {
    Exact(u64),
    AtLeast(u64),
}

impl Freed {
    pub fn bytes(self) -> u64 {
        match self {
            Freed::Exact(b) | Freed::AtLeast(b) => b,
        }
    }

    /// For a screen: `724M`, or `at least 1.4G`.
    pub fn label(self) -> String {
        match self {
            Freed::Exact(b) => crate::format::human(b),
            Freed::AtLeast(b) => format!("at least {}", crate::format::human(b)),
        }
    }

    /// For a badge, where there is no room for the qualifier.
    pub fn short(self) -> String {
        match self {
            Freed::Exact(b) => crate::format::human(b),
            Freed::AtLeast(b) => format!("\u{2265}{}", crate::format::human(b)),
        }
    }

    pub fn is_exact(self) -> bool {
        matches!(self, Freed::Exact(_))
    }
}

/// See [`Freed`].
pub fn freed(report: &Report, staged: &std::collections::BTreeSet<ToolKey>) -> Freed {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<(Source, Kind), Vec<&Resource>> = BTreeMap::new();
    for key in staged {
        if let Some(r) = report.get(key) {
            groups.entry((r.source, r.kind)).or_default().push(r);
        }
    }

    let mut total = 0u64;
    let mut exact = true;
    for ((source, kind), items) in groups {
        let sr = report.source(source);
        let held = sr.map(|s| s.items_of(kind).len()).unwrap_or(0);

        // All of them, and they share: nothing is left holding a shared layer,
        // so the tool's own figure for the kind is the whole answer.
        //
        // Gated on there actually being sharing, because the kind total is not
        // always what removing every item frees. Build cache is one aggregate
        // item already sized at what pruning gives back, and its kind total
        // includes the in-use cache that pruning leaves alone — taking the
        // total there would promise storage that is not going anywhere.
        if items.len() == held
            && items.iter().any(|r| r.shared() > 0)
            && let Some((size, _)) = sr.and_then(|s| s.total(kind))
        {
            total += size;
            continue;
        }

        total += items.iter().map(|r| r.bytes).sum::<u64>();
        // One item's own bytes are exact by definition; several that share are
        // a floor.
        if items.len() > 1 && items.iter().any(|r| r.shared() > 0) {
            exact = false;
        }
    }

    if exact { Freed::Exact(total) } else { Freed::AtLeast(total) }
}
