//! Everything the UI needs to know, and everything a keypress can change.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::delete::{self, Disposal, Job};
use crate::scan::Scan;
use crate::scan::walk::ScanOpts;
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::presets::Category;
use crate::tree::{NodeId, Sort, Tree, flags};

/// How long ago the last write was, as four buckets. Absolute size is what a
/// directory *is*; how much of it nobody has touched in two years is what makes
/// it a candidate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgeFilter {
    All,
    D90,
    Y1,
    Y2,
}

impl AgeFilter {
    pub fn next(self) -> AgeFilter {
        match self {
            AgeFilter::All => AgeFilter::D90,
            AgeFilter::D90 => AgeFilter::Y1,
            AgeFilter::Y1 => AgeFilter::Y2,
            AgeFilter::Y2 => AgeFilter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgeFilter::All => "any age",
            AgeFilter::D90 => "untouched 90 days",
            AgeFilter::Y1 => "untouched 1 year",
            AgeFilter::Y2 => "untouched 2 years",
        }
    }

    /// How old the newest write in a subtree must be for it to show, in
    /// seconds. `None` means no age filtering at all.
    fn cutoff(self) -> Option<i64> {
        match self {
            AgeFilter::All => None,
            AgeFilter::D90 => Some(90 * 86400),
            AgeFilter::Y1 => Some(365 * 86400),
            AgeFilter::Y2 => Some(2 * 365 * 86400),
        }
    }
}

/// The four buckets the age histogram reports, newest first. The boundaries
/// match `AgeFilter` so the histogram reads as a preview of what each filter
/// step would keep.
pub const AGE_BUCKETS: [(&str, i64); 4] = [
    ("<90d", 90 * 86400),
    ("90d-1y", 365 * 86400),
    ("1-2y", 2 * 365 * 86400),
    (">2y", i64::MAX),
];

/// The four classes the file-size distribution reports, largest first. The
/// boundaries are the ones that change what you would do about a directory: a
/// file over 100M is worth deleting on its own, and a file under 1M is only
/// ever worth deleting in bulk. A directory holding 168G either way is two
/// completely different afternoons.
/// Each entry is a label and the smallest file that belongs in it.
pub const SIZE_CLASSES: [(&str, u64); 4] = [
    (">100M", 100 << 20),
    ("10-100M", 10 << 20),
    ("1-10M", 1 << 20),
    ("<1M", 0),
];

/// Which breakdown leads the lower half of the detail pane. As many as fit are
/// shown, in this order, wrapping round; `S` moves the start on by one, which
/// on a short pane is the only way to reach the ones that did not fit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Panel {
    Extensions,
    Ages,
    Biggest,
    Sizes,
}

impl Panel {
    pub fn next(self) -> Panel {
        match self {
            Panel::Extensions => Panel::Ages,
            Panel::Ages => Panel::Biggest,
            Panel::Biggest => Panel::Sizes,
            Panel::Sizes => Panel::Extensions,
        }
    }

    /// All four, this one first. The order never changes; `S` only chooses
    /// where it starts, so on a pane that fits two the pair rotates and on one
    /// that fits all four nothing is ever hidden.
    pub fn rotation(self) -> [Panel; 4] {
        let mut out = [self; 4];
        for i in 1..4 {
            out[i] = out[i - 1].next();
        }
        out
    }

    pub fn label(self) -> &'static str {
        match self {
            Panel::Extensions => "by extension",
            Panel::Ages => "by age",
            Panel::Biggest => "biggest files",
            Panel::Sizes => "file sizes",
        }
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub struct Breakdown {
    pub id: NodeId,
    pub exts: Vec<ExtRow>,
    /// Bytes per `AGE_BUCKETS` entry, in the same order.
    pub ages: [u64; 4],
    /// How much this subtree has changed since the last scan of this root.
    /// `None` when there is nothing to compare against; `Some(None)` when this
    /// path is new since then.
    pub growth: Option<Option<i64>>,
    /// The subtree was larger than the walk budget, so these are a sample.
    pub partial: bool,
    /// The biggest children of the selection, largest first, and how many
    /// children there are in all. Read straight off the node rather than out of
    /// the walk, so it stays exact even when `partial` is set.
    pub children: Vec<ChildRow>,
    pub child_count: usize,
    /// The largest individual files anywhere in the subtree. One 68G disk image
    /// and 700k small files are the same headline and completely different
    /// work, and nothing else on this pane tells them apart.
    pub biggest: Vec<FileRow>,
    /// Bytes and file count per `SIZE_CLASSES` entry, in the same order.
    pub sizes: [(u64, u64); 4],
    /// The subtree's size when this was computed. A live scan keeps growing the
    /// tree under the cursor, and a breakdown taken when the node was empty
    /// would otherwise stay empty for the rest of the session.
    at_bytes: u64,
    at: Instant,
}

/// Why an entry is not in the numbers, or not on screen.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The directory could not be read. Its contents are missing from every
    /// total above it.
    Unreadable,
    /// A cloud provider's folder, not descended into.
    Cloud,
    /// A mount point for another filesystem.
    OtherVolume,
    /// On the user's ignore list: hidden from the views, but counted.
    Ignored,
}

impl Why {
    pub fn heading(self) -> &'static str {
        match self {
            Why::Unreadable => "could not be read",
            Why::Cloud => "cloud folders",
            Why::OtherVolume => "other filesystems",
            Why::Ignored => "on your ignore list",
        }
    }

    /// What it costs, and what to do about it. The second half is the reason
    /// this screen exists: a list of things fad did not do is only useful if
    /// each line says how to make it do them.
    pub fn note(self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            Why::Unreadable => "not counted \u{2014} grant your terminal Full Disk Access",
            #[cfg(not(target_os = "macos"))]
            Why::Unreadable => "not counted \u{2014} needs different permissions, or root",
            Why::Cloud => "not counted \u{2014} rerun with --cloud",
            Why::OtherVolume => "not counted \u{2014} rerun with --cross-device",
            Why::Ignored => "counted in every total above it, just not shown",
        }
    }

    /// Is what this is hiding missing from the totals?
    pub fn uncounted(self) -> bool {
        !matches!(self, Why::Ignored)
    }
}

/// One thing the scan did not count, or did not show.
pub struct Omission {
    pub path: PathBuf,
    pub why: Why,
    /// What it holds, where that is known at all. It never is for anything
    /// uncounted — that is what uncounted means — and inventing a figure for it
    /// would be the exact failure this screen exists to expose.
    pub bytes: Option<u64>,
}

/// One line of the staging basket.
pub enum BasketRow {
    Group { cat: Option<Category>, count: usize, bytes: u64 },
    Item(NodeId),
    /// The tool half of the batch, which is permanent and is kept visually
    /// apart from the trashable half for exactly that reason.
    ToolGroup { count: usize, freed: crate::tools::Freed },
    ToolItem(crate::tools::ToolKey),
}

pub struct ExtRow {
    pub ext: String,
    pub bytes: u64,
    pub count: u64,
}

/// One child of the selection, for the "where it goes" block.
pub struct ChildRow {
    pub name: String,
    pub bytes: u64,
}

/// One file, for the biggest-files panel.
pub struct FileRow {
    pub name: String,
    pub bytes: u64,
}

/// The trailing extension, lowercased, or a bucket for names without one.
/// Dotfiles have no extension: `.zshrc` is a name, not a `.zshrc` file.
fn extension_of(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 10 => ext,
        _ => "",
    }
}

/// A group heading: a row the cursor can rest on, that opens and closes, and
/// that `A` stages in one go. The reclaimable and duplicate views are both
/// lists of groups, and differ only in what a group means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heading {
    Category(Category),
    /// Index into `dupes.groups`.
    Dupes(usize),
    /// One kind of thing one tool is holding: docker's images, podman's
    /// volumes. Its total comes from the tool, never from adding up the rows
    /// beneath it.
    Tool(crate::tools::Source, crate::tools::Kind),
    /// A tool that is installed but had nothing usable to say. A row rather
    /// than a banner, because "docker is not running" belongs where docker's
    /// numbers would have been.
    ToolStatus(crate::tools::Source),
}

/// One line of the tree pane.
#[derive(Clone, Copy)]
pub struct Row {
    pub id: NodeId,
    pub depth: u16,
    /// Largest sibling total, for scaling this row's bar.
    pub sibling_max: u64,
    /// Group headings are rows too, so the cursor can land on one and stage
    /// everything under it.
    pub header: Option<Heading>,
    /// Index into `tool_flat` for a row that is not a tree node at all.
    ///
    /// `id` is meaningless on such a row and is left at the root. Making `id`
    /// an enum would be tidier and would touch every one of the thirty-odd
    /// places that read it; this touches the five that had to learn about tools
    /// anyway. If a second off-tree source ever needs its own row shape, that
    /// is the moment to widen `id` — not now.
    pub tool: Option<u32>,
}

impl Row {
    fn node(id: NodeId, depth: u16, sibling_max: u64) -> Row {
        Row { id, depth, sibling_max, header: None, tool: None }
    }
}

#[derive(PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Typing into the fuzzy filter.
    Filter,
    /// Typing into the search: every entry in the tree, wherever it is.
    Search,
    Help,
    /// The staging basket: the whole batch on one screen, editable.
    Basket,
    /// The undo journal: every batch this machine still remembers.
    History,
    /// Reviewing the staged batch before committing it.
    Confirm,
    /// Confirming that the trash should be taken out.
    EmptyTrash,
    /// A batch is being deleted, or has just finished.
    Deleting,
    /// Everything the scan did not count, and what to do about each kind.
    Omissions,
}

pub struct App {
    pub tree: Tree,
    pub scan: Option<Scan>,
    pub opts: ScanOpts,

    pub rows: Vec<Row>,
    pub expanded: HashSet<NodeId>,
    pub staged: HashSet<NodeId>,
    /// Index into `rows`.
    pub cursor: usize,
    pub offset: usize,
    pub sort: Sort,
    /// Report `st_size` instead of allocated blocks, everywhere a size is shown
    /// or ranked.
    pub apparent: bool,
    pub mode: Mode,
    pub filter: String,
    /// What is being searched for across the whole tree.
    pub search: String,
    /// What the search found: node and size, biggest first.
    pub search_hits: Vec<(NodeId, u64)>,
    /// Index into `search_hits`.
    pub search_cursor: usize,
    /// There were more matches than the list will hold.
    pub search_more: usize,
    pub status: Option<String>,
    /// Showing only what the built-in rules consider reclaimable.
    pub reclaim_view: bool,
    /// Showing files that exist more than once.
    pub dupe_view: bool,
    /// The finished duplicate report, if one has been asked for and arrived.
    pub dupes: Option<crate::dupes::Report>,
    /// A duplicate hunt in flight. Hashing gigabytes cannot happen on the UI
    /// thread, and the view says so while it runs.
    dupes_rx: Option<crossbeam_channel::Receiver<crate::dupes::Report>>,
    /// Groups whose copies are showing.
    pub dupes_open: HashSet<usize>,
    /// Categories whose items are showing. Headings start closed, so the first
    /// screen of the reclaimable view is the four totals rather than a wall of
    /// paths.
    pub reclaim_open: HashSet<Category>,

    // -- tool storage: disk a walk cannot see. See `crate::tools`.
    /// Showing what Docker and friends are holding.
    pub tools_view: bool,
    /// The last answer the tools gave.
    pub tools: Option<crate::tools::Report>,
    /// A probe in flight. `docker system df` measured just under sixteen
    /// seconds against a healthy daemon on the machine this was written on, so
    /// this can never be on the UI thread and the view has to say it is working.
    tools_rx: Option<crossbeam_channel::Receiver<crate::tools::Report>>,
    /// When the running probe started, so the view can show how long it has
    /// been asking. A twenty-second wait with nothing moving reads as a hang.
    pub tools_started: Option<Instant>,
    /// When the current answer was taken. Docker's numbers go stale in seconds
    /// and the view says how old they are.
    pub tools_at: Option<Instant>,
    /// Which tool/kind headings are open.
    pub tools_open: HashSet<(crate::tools::Source, crate::tools::Kind)>,
    /// Staged tool resources, held apart from `staged` and named by id rather
    /// than by index.
    ///
    /// A parallel set rather than a widened `staged`, because `staged` is
    /// threaded through ancestry checks and two path round-trips that rebuild
    /// it after a tree swap — none of which mean anything for a Docker image,
    /// and all of which would gain a dead arm. It also makes the permanence
    /// rule structural: two sets, two disposals, and it is not possible to
    /// accidentally offer to put a removed image back.
    ///
    /// Named by `ToolKey` rather than by position because `R` re-probes and
    /// replaces the report wholesale; an index that meant "the dangling image"
    /// a moment ago would quietly come to mean something else.
    pub staged_tools: std::collections::BTreeSet<crate::tools::ToolKey>,
    /// `(source index, item index)` for each tool row on screen, which is what
    /// `Row::tool` indexes into.
    tool_flat: Vec<(usize, usize)>,
    /// A tool removal in flight. Separate from `job` because it is permanent,
    /// unjournalled, and measured rather than predicted.
    pub tool_job: Option<crate::tools::Job>,
    /// Staged tool items refused at confirm time, and why.
    pub tools_refused: Vec<(String, String)>,
    /// Every category with something in it, and its items, biggest first. Held
    /// apart from `rows` because a closed category still has to report its
    /// count and total, and `A` still has to stage all of it.
    reclaim_cats: Vec<(Category, Vec<NodeId>)>,
    /// The tree on screen came from a snapshot and a fresh walk is running
    /// behind it. Sizes are last-known, not current, and the UI says so.
    pub from_cache: bool,
    /// The fresh tree being built while a snapshot is displayed.
    pending: Option<Tree>,
    /// A snapshot being read from disk. Deserialising a few million nodes takes
    /// over a second, and the live scan paints a useful first screen in tens of
    /// milliseconds — so the snapshot must never be on the startup path. It
    /// arrives when it arrives, and only replaces the live tree if the walk is
    /// still running by then.
    snapshot_rx: Option<crossbeam_channel::Receiver<Option<(Tree, std::time::SystemTime)>>>,
    /// The previous scan of this root, kept for the one comparison the numbers
    /// on screen cannot make on their own: what grew. A cache that put on 12G
    /// this week is a better target than a stable 20G one.
    previous: Option<Tree>,
    /// When that scan was taken.
    pub previous_at: Option<std::time::SystemTime>,

    /// Extension and age breakdown for the current selection. Aggregating a
    /// subtree of a million nodes is far too slow to redo every frame, and the
    /// answer only changes when the selection moves.
    pub breakdown: Option<Breakdown>,
    /// Which of the four breakdowns the detail pane leads with.
    pub panel: Panel,
    /// Hide subtrees written to more recently than this.
    pub age_filter: AgeFilter,
    /// How many entries the last snapshot of this root held. The only honest
    /// denominator we have for an ETA: the walk cannot know what it has not
    /// reached, but last time is a good guess at this time.
    pub expected_entries: Option<u64>,
    /// The user's persistent ignore list.
    pub ignore: crate::ignore::Rules,
    /// What the ignore list hid on the last rebuild, so the tree can say so
    /// rather than silently omit it.
    pub ignored: (usize, u64),
    /// Capture the mouse. Off makes the terminal's own text selection work
    /// again, which is why it is a flag and not an assumption.
    pub mouse: bool,
    /// Index into `basket_rows`.
    pub basket_cursor: usize,
    /// What the scan did not count, as of the last time it was asked for.
    pub omissions: Vec<Omission>,
    /// Index into `omissions`.
    pub omission_cursor: usize,
    /// The undo journal, newest first, as of the last time it was opened.
    pub history: Vec<delete::Batch>,
    /// Index into `history`.
    pub history_cursor: usize,
    /// What the volume under the scan root holds and what is left of it.
    ///
    /// Cached rather than asked for per frame: it is a syscall against a live
    /// filesystem, the answer moves in whole gigabytes, and the header redraws
    /// thirty times a second during a walk.
    pub volume: Option<crate::platform::Volume>,
    volume_at: Option<Instant>,
    /// What fad has trashed and not yet seen emptied. Trashing reclaims nothing
    /// until the trash goes out, and a headline that ignores that is a lie by
    /// omission.
    pub trash_pending: (usize, u64),
    /// Where the tree list was drawn last frame. Clicks arrive as terminal
    /// coordinates and mean nothing without it.
    pub tree_list: ratatui::layout::Rect,

    /// What the pending commit will do. Reset to Trash after every batch, so a
    /// permanent delete is always a deliberate choice.
    pub disposal: Disposal,
    /// Set while a batch is being deleted, and kept afterwards so the modal can
    /// report what happened.
    pub job: Option<Job>,
    /// The running job is the trash going out, not a batch going in. The same
    /// machinery, and the same modal, but it must not say "deleted" about
    /// things that were deleted some time ago and are only now unrecoverable.
    pub emptying: bool,
    /// Staged items the guard refused, shown in the confirm modal.
    pub refused: Vec<(NodeId, String)>,

    matcher: Matcher,

    /// Rows are rebuilt on demand, not on every frame.
    dirty: bool,
    pub should_quit: bool,
}

impl App {
    /// Start reading the last snapshot in the background. The UI is already
    /// live by the time this returns.
    pub fn load_snapshot_async(&mut self) {
        let root = self.tree.root_path().to_path_buf();
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(crate::cache::load(&root));
        });
        self.snapshot_rx = Some(rx);
    }

    /// Take the snapshot if it beat the walk: complete-but-stale totals now
    /// beat exact ones in twenty seconds. If the walk already finished, throw
    /// it away — we have the truth.
    fn poll_snapshot(&mut self) -> bool {
        let Some(rx) = self.snapshot_rx.as_ref() else { return false };
        let Ok(loaded) = rx.try_recv() else { return false };
        self.snapshot_rx = None;

        let Some((snapshot, saved_at)) = loaded else { return false };
        // Worth keeping even when the snapshot is too late to display: an ETA
        // is the one thing a three-minute walk cannot produce on its own.
        self.expected_entries = Some(snapshot.len() as u64);
        self.previous_at = Some(saved_at);
        if self.install_snapshot(snapshot) {
            // It is on screen now, and `adopt` will move it into `previous`
            // when the fresh walk replaces it.
            return true;
        }
        // The walk beat it. It is no use as a display, but it is exactly what
        // the growth comparison needs.
        false
    }

    /// Put a loaded snapshot on screen and push the tree being filled behind
    /// it. Separate from `poll_snapshot` so the swap — the part with the sharp
    /// edges — can be exercised without racing a real walk.
    pub fn install_snapshot(&mut self, snapshot: Tree) -> bool {
        if self.scan.is_none() || self.pending.is_some() {
            return false;
        }
        // Node ids are arena indices and mean nothing in the other tree, so
        // anything staged in the second before the snapshot landed has to be
        // re-resolved by path — exactly as `adopt` does on the way back. Keeping
        // the raw ids would silently re-point the batch at unrelated files.
        let staged: Vec<PathBuf> = self.staged.iter().map(|id| self.tree.path(*id)).collect();

        // The tree being filled becomes the pending one; the snapshot goes on screen.
        let live = std::mem::replace(&mut self.tree, snapshot);
        self.pending = Some(live);
        self.from_cache = true;
        self.expanded = HashSet::from([self.tree.root()]);
        self.staged = staged.iter().filter_map(|p| self.tree.find_path(p)).collect();
        self.refused.clear();
        self.breakdown = None;
        self.invalidate_dupes();
        self.cursor = 0;
        self.offset = 0;
        self.dirty = true;
        true
    }

    /// Hand the app a previous scan directly. The real path runs through
    /// `poll_snapshot`, which needs a live walk to race; a test wants the
    /// comparison without the race.
    /// Put a tools report in place without asking any daemon, so the view can
    /// be rendered and staged against on a machine that has none.
    pub fn install_tools_for_test(&mut self, report: crate::tools::Report) {
        self.tools = Some(report);
        self.tools_at = Some(Instant::now());
        self.tools_view = true;
        self.mark_dirty();
    }

    pub fn install_snapshot_for_test(&mut self, previous: Tree, at: std::time::SystemTime) {
        self.previous = Some(previous);
        self.previous_at = Some(at);
        self.breakdown = None;
    }

    pub fn new(tree: Tree, scan: Scan, opts: ScanOpts) -> App {
        let root = tree.root();
        let mut app = App {
            tree,
            scan: Some(scan),
            opts,
            rows: Vec::new(),
            expanded: HashSet::from([root]),
            staged: HashSet::new(),
            cursor: 0,
            offset: 0,
            sort: Sort::Size,
            apparent: false,
            mode: Mode::Normal,
            filter: String::new(),
            search: String::new(),
            search_hits: Vec::new(),
            search_cursor: 0,
            search_more: 0,
            status: None,
            reclaim_view: false,
            tools_view: false,
            tools: None,
            tools_rx: None,
            tools_started: None,
            tools_at: None,
            tools_open: HashSet::new(),
            staged_tools: std::collections::BTreeSet::new(),
            tool_flat: Vec::new(),
            tool_job: None,
            tools_refused: Vec::new(),
            dupe_view: false,
            dupes: None,
            dupes_rx: None,
            dupes_open: HashSet::new(),
            reclaim_open: HashSet::new(),
            reclaim_cats: Vec::new(),
            from_cache: false,
            pending: None,
            snapshot_rx: None,
            previous: None,
            previous_at: None,
            breakdown: None,
            panel: Panel::Extensions,
            age_filter: AgeFilter::All,
            expected_entries: None,
            ignore: crate::ignore::Rules::load(),
            ignored: (0, 0),
            mouse: true,
            basket_cursor: 0,
            omissions: Vec::new(),
            omission_cursor: 0,
            history: Vec::new(),
            history_cursor: 0,
            volume: None,
            volume_at: None,
            trash_pending: delete::still_in_trash(),
            tree_list: ratatui::layout::Rect::ZERO,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            disposal: Disposal::Trash,
            job: None,
            emptying: false,
            refused: Vec::new(),
            dirty: true,
            should_quit: false,
        };
        app.rebuild_rows();
        app
    }

    /// Throw the tree away and walk again. Used after an undo, and whenever the
    /// filesystem has moved on underneath us.
    pub fn restart_scan(&mut self) -> std::io::Result<()> {
        let root = self.tree.root_path().to_path_buf();
        let (tree, scan) = Scan::start(&root, self.opts.clone())?;
        self.tree = tree;
        self.scan = Some(scan);
        // The half-built tree from the previous scan must go with it. Left in
        // place it would keep receiving the new walk's batches against the old
        // walk's scan ids, folding fresh sizes into stale nodes — and
        // `from_cache` would pin the session as never-complete, so no snapshot
        // would ever be written again.
        self.pending = None;
        self.from_cache = false;
        self.snapshot_rx = None;
        self.staged.clear();
        self.refused.clear();
        // `staged_tools` deliberately survives this. Rescanning the filesystem
        // says nothing about Docker's image store, and a `ToolKey` is not an
        // arena index that a new tree invalidates. It looks like an omission
        // among all this clearing, so: it is not one.
        self.expanded = HashSet::from([self.tree.root()]);
        self.breakdown = None;
        self.invalidate_dupes();
        self.cursor = 0;
        self.offset = 0;
        self.mark_dirty();
        Ok(())
    }

    pub fn root_path(&self) -> PathBuf {
        self.tree.root_path().to_path_buf()
    }

    /// Pull in whatever the walker has produced. Returns true if the view needs
    /// a redraw.
    pub fn poll_scan(&mut self) -> bool {
        let snapshot_arrived = self.poll_snapshot();
        let Some(scan) = self.scan.as_mut() else { return snapshot_arrived };

        // While a snapshot is on screen the walk feeds the pending tree, and
        // nothing changes visually until it is complete. Half a fresh scan is
        // strictly worse to look at than a whole stale one.
        let target = self.pending.as_mut().unwrap_or(&mut self.tree);
        let n = scan.drain_ready(target);
        if !scan.is_finished() {
            let visible = n > 0 && self.pending.is_none();
            if visible {
                self.dirty = true;
            }
            return visible || snapshot_arrived;
        }

        self.scan = None;
        if let Some(fresh) = self.pending.take() {
            self.adopt(fresh);
            return true;
        }
        self.dirty = true;
        true
    }

    /// Replace the displayed tree, carrying the user's place across. Node ids
    /// do not survive a rebuild, so everything is re-resolved by path.
    fn adopt(&mut self, fresh: Tree) {
        let selected = self.selected().map(|id| self.tree.path(id));
        let expanded: Vec<PathBuf> = self.expanded.iter().map(|id| self.tree.path(*id)).collect();
        let staged: Vec<PathBuf> = self.staged.iter().map(|id| self.tree.path(*id)).collect();

        // The tree coming off screen is the previous scan, which is exactly
        // what the growth comparison wants.
        self.previous = Some(std::mem::replace(&mut self.tree, fresh));
        self.from_cache = false;
        self.breakdown = None;
        self.invalidate_dupes();

        self.expanded = expanded.iter().filter_map(|p| self.tree.find_path(p)).collect();
        self.expanded.insert(self.tree.root());
        // Anything staged that the fresh walk cannot find is gone already;
        // silently dropping it is right, quietly keeping a dead id is not.
        self.staged = staged.iter().filter_map(|p| self.tree.find_path(p)).collect();

        self.dirty = true;
        self.rebuild_rows();
        if let Some(id) = selected.as_ref().and_then(|p| self.tree.find_path(p)) {
            if let Some(i) = self.rows.iter().position(|r| r.id == id) {
                self.cursor = i;
            }
        }
    }

    pub fn scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// How the walk is doing, and how far through it looks. The fraction is
    /// `None` until a previous snapshot gives us something to measure against,
    /// and is capped at 1: this run finding more than last run is normal, and a
    /// bar that reads 140% is worse than no bar.
    pub fn scan_progress(&self) -> Option<(crate::scan::ScanProgress, Option<f64>)> {
        let p = self.scan.as_ref()?.progress();
        let fraction = self
            .expected_entries
            .filter(|n| *n > 0)
            .map(|n| (p.entries as f64 / n as f64).min(1.0));
        Some((p, fraction))
    }

    /// True only when the displayed tree is the result of a walk that ran to
    /// completion. A half-finished scan must never be written back as a
    /// snapshot: next launch would present its undersized totals as fact.
    pub fn tree_is_complete(&self) -> bool {
        self.scan.is_none() && self.pending.is_none() && !self.from_cache
    }

    /// The tree node under the cursor, if the cursor is on one at all.
    ///
    /// A tool row carries the root's id as a placeholder — it has no node —
    /// so this has to return `None` there, or `o`, `e`, `i` and `y` would all
    /// quietly act on the scan root instead.
    pub fn selected(&self) -> Option<NodeId> {
        let row = self.rows.get(self.cursor)?;
        if row.tool.is_some() {
            return None;
        }
        Some(row.id)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Flatten the expanded tree into `rows`, sorting only what is visible.
    pub fn rebuild_rows(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;

        // Keep the cursor on the same row across a rebuild; sizes arriving
        // mid-scan reorder rows underneath it constantly otherwise.
        //
        // The whole row identity, not just the node id. A heading shares its id
        // with the first item under it, so heading-ness has to be part of the
        // match or the cursor could never rest on a heading. And in the tools
        // view *every* row carries the root's id as a placeholder — there are no
        // nodes there — so without the heading and the item index in the key,
        // every rebuild would snap the cursor back to the first row of the same
        // shape, and a rebuild follows every keypress. That is exactly what it
        // did: j and k moved the cursor and this put it straight back.
        let anchor = self.rows.get(self.cursor).map(|r| (r.id, r.header, r.tool));

        self.rows.clear();
        self.ignored = (0, 0);
        if self.tools_view {
            self.reclaim_cats.clear();
            self.build_tool_rows();
        } else if self.dupe_view {
            self.reclaim_cats.clear();
            self.tool_flat.clear();
            self.build_dupe_rows();
        } else if self.reclaim_view {
            self.build_reclaim_rows();
        } else {
            self.reclaim_cats.clear();
            self.tool_flat.clear();
            let root = self.tree.root();
            let max = self.tree.size(root, self.apparent);
            self.push_row(root, 0, max);
        }

        if let Some(key) = anchor {
            if let Some(i) = self.rows.iter().position(|r| (r.id, r.header, r.tool) == key) {
                self.cursor = i;
            }
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    /// The reclaimable view: every preset match, grouped by category and
    /// ranked by size. The whole point is to make the first screen the answer.
    fn build_reclaim_rows(&mut self) {
        self.reclaim_cats.clear();
        for cat in Category::all() {
            // Collected before filtering so the fuzzy matcher, which needs
            // `&mut self`, is not borrowing the tree at the same time.
            let candidates: Vec<NodeId> = self
                .tree
                .reclaimable
                .iter()
                .copied()
                .filter(|id| self.tree.node(*id).preset == Some(cat))
                .filter(|id| self.tree.node(*id).flags & flags::DELETED == 0)
                // A `node_modules` inside a `node_modules` is already covered
                // by its ancestor; listing both would double the headline total.
                .filter(|id| !crate::reclaim::has_reclaimable_ancestor(&self.tree, *id))
                .collect();
            let mut items: Vec<NodeId> =
                candidates.into_iter().filter(|id| self.passes_filter(*id)).collect();
            if items.is_empty() {
                continue;
            }
            let apparent = self.apparent;
            items.sort_unstable_by_key(|id| std::cmp::Reverse(self.tree.size(*id, apparent)));
            let max = self.tree.size(items[0], apparent);

            self.rows.push(Row {
                id: items[0],
                depth: 0,
                sibling_max: max,
                header: Some(Heading::Category(cat)),
                tool: None,
            });
            if self.reclaim_open.contains(&cat) {
                for id in &items {
                    self.rows.push(Row::node(*id, 1, max));
                }
            }
            self.reclaim_cats.push((cat, items));
        }
    }

    // ---------------------------------------------------------------- tools

    /// Ask every tool what it is holding.
    ///
    /// Fired on the first `t` and never at startup: opening a disk-usage tool
    /// is not consent to shell out to a container daemon, and the answer would
    /// be stale by the time anyone looked at it anyway.
    pub fn start_tool_probe(&mut self) {
        if self.tools_rx.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(crate::tools::Report::probe());
        });
        self.tools_rx = Some(rx);
        self.tools_started = Some(Instant::now());
        self.mark_dirty();
    }

    /// Collect a finished probe. Returns true if the view needs a redraw.
    pub fn poll_tools(&mut self) -> bool {
        let Some(rx) = self.tools_rx.as_ref() else { return false };
        let Ok(report) = rx.try_recv() else { return false };
        self.tools_rx = None;
        self.tools_started = None;
        self.tools_at = Some(Instant::now());
        // Anything staged that the fresh answer does not know about is already
        // gone, or was never there. Dropping it is the same rule the tree
        // follows after a rescan.
        self.staged_tools.retain(|k| report.get(k).is_some());
        self.tools = Some(report);
        self.mark_dirty();
        true
    }

    pub fn tool_probe_running(&self) -> bool {
        self.tools_rx.is_some()
    }

    /// The resource a row points at, if it points at one.
    pub fn tool_of(&self, row: &Row) -> Option<&crate::tools::Resource> {
        let (s, i) = *self.tool_flat.get(row.tool? as usize)?;
        self.tools.as_ref()?.sources.get(s)?.items.get(i)
    }

    pub fn tool_at_cursor(&self) -> Option<&crate::tools::Resource> {
        self.tool_of(self.rows.get(self.cursor)?)
    }

    pub fn tools_is_open(&self, source: crate::tools::Source, kind: crate::tools::Kind) -> bool {
        self.tools_open.contains(&(source, kind))
    }

    /// Everything one tool is holding of one kind, whether or not the heading is
    /// open — `A` on a closed heading has to stage the lot.
    pub fn tool_items(
        &self,
        source: crate::tools::Source,
        kind: crate::tools::Kind,
    ) -> Vec<crate::tools::ToolKey> {
        let Some(report) = self.tools.as_ref() else { return Vec::new() };
        let Some(sr) = report.source(source) else { return Vec::new() };
        sr.items_of(kind).into_iter().map(|i| sr.items[i].key()).collect()
    }

    /// This resource's storage is a file under the current scan root, so the
    /// tree has already counted it.
    ///
    /// The inverse of the shared-layer trap and just as easy to fall into: a
    /// VM disk image under `$HOME` is in the headline total already, and a view
    /// that presents it as newly discovered space is the same lie the other way
    /// round.
    pub fn tool_in_tree(&self, path: Option<&std::path::Path>) -> bool {
        path.is_some_and(|p| p.starts_with(self.tree.root_path()))
    }

    /// The tools view: one heading per tool and kind, each carrying the tool's
    /// own deduplicated total.
    fn build_tool_rows(&mut self) {
        self.tool_flat.clear();
        let root = self.tree.root();
        let Some(report) = self.tools.as_ref() else { return };

        // Scale every bar against the largest single figure on screen, so a
        // 17G build cache and a 700M image look as different as they are.
        let widest = report
            .sources
            .iter()
            .flat_map(|s| s.totals.iter().map(|(_, _, r)| *r))
            .chain(report.items().map(|r| r.bytes))
            .max()
            .unwrap_or(0);

        let mut rows: Vec<Row> = Vec::new();
        let mut flat: Vec<(usize, usize)> = Vec::new();

        for (si, sr) in report.sources.iter().enumerate() {
            if sr.status != crate::tools::Status::Ok {
                rows.push(Row {
                    id: root,
                    depth: 0,
                    sibling_max: widest,
                    header: Some(Heading::ToolStatus(sr.source)),
                    tool: None,
                });
                continue;
            }
            for kind in sr.kinds() {
                let items = sr.items_of(kind);
                // A kind the tool reports nothing for and holds nothing of is
                // not worth a line.
                if items.is_empty() && sr.total(kind).is_none_or(|(s, _)| s == 0) {
                    continue;
                }
                rows.push(Row {
                    id: root,
                    depth: 0,
                    sibling_max: widest,
                    header: Some(Heading::Tool(sr.source, kind)),
                    tool: None,
                });
                if !self.tools_is_open(sr.source, kind) {
                    continue;
                }
                for i in items {
                    flat.push((si, i));
                    rows.push(Row {
                        id: root,
                        depth: 1,
                        sibling_max: widest,
                        header: None,
                        tool: Some((flat.len() - 1) as u32),
                    });
                }
            }
        }

        self.rows = rows;
        self.tool_flat = flat;
    }

    /// Stage or unstage the resource under the cursor.
    pub fn toggle_tool_stage(&mut self, key: crate::tools::ToolKey) {
        if !self.staged_tools.remove(&key) {
            self.staged_tools.insert(key);
        }
    }

    /// What a staged batch of tool items would free. See `tools::Freed`.
    pub fn staged_tool_freed(&self) -> crate::tools::Freed {
        match self.tools.as_ref() {
            Some(report) => crate::tools::freed(report, &self.staged_tools),
            None => crate::tools::Freed::Exact(0),
        }
    }

    /// Staged tool bytes that will really come back to the user's disk.
    ///
    /// Anything inside a VM image that does not shrink frees space inside that
    /// image and nothing on the host, so it must not reach the "free space
    /// after this batch" line.
    pub fn staged_tool_host_bytes(&self) -> u64 {
        let Some(report) = self.tools.as_ref() else { return 0 };
        self.staged_tools
            .iter()
            .filter_map(|k| report.get(k).map(|r| (k, r)))
            .filter(|(k, _)| {
                report.source(k.source).is_some_and(|s| s.backing.frees_host_space())
            })
            .map(|(_, r)| r.bytes)
            .sum()
    }

    /// Drop anything the tool now says cannot go, and say why.
    ///
    /// Re-checked here rather than trusted from staging time, because the
    /// daemon's state moves underneath us: a container can start between `t`
    /// and `enter`, and the image it is now using must not be in the batch.
    pub fn review_tool_batch(&mut self) {
        self.tools_refused.clear();
        let Some(report) = self.tools.as_ref() else {
            self.staged_tools.clear();
            return;
        };
        let mut keep = std::collections::BTreeSet::new();
        for key in self.staged_tools.clone() {
            match report.get(&key) {
                Some(r) if r.removable() => {
                    keep.insert(key);
                }
                Some(r) => self
                    .tools_refused
                    .push((r.name.clone(), r.blocked.clone().unwrap_or_default())),
                None => self.tools_refused.push((key.id.clone(), "no longer there".into())),
            }
        }
        self.staged_tools = keep;
    }

    /// The staged tool items in removal order, biggest first.
    pub fn tool_batch_items(&self) -> Vec<(crate::tools::ToolKey, String, u64)> {
        let Some(report) = self.tools.as_ref() else { return Vec::new() };
        let mut v: Vec<_> = self
            .staged_tools
            .iter()
            .filter_map(|k| report.get(k))
            .map(|r| (r.key(), r.name.clone(), r.bytes))
            .collect();
        v.sort_by(|a, b| b.2.cmp(&a.2));
        v
    }

    /// Fold a finished removal back in: whatever went is no longer staged, and
    /// the next `t` will get fresh numbers.
    pub fn poll_tool_job(&mut self) -> bool {
        let Some(job) = self.tool_job.as_mut() else { return false };
        if !job.poll() {
            return false;
        }
        let gone: Vec<crate::tools::ToolKey> = job
            .done
            .iter()
            .filter(|o| o.result.is_ok())
            .map(|o| o.key.clone())
            .collect();
        for key in gone {
            self.staged_tools.remove(&key);
        }
        self.mark_dirty();
        true
    }

    /// The duplicate view: one heading per group of identical files, the
    /// biggest pile of wasted space first.
    fn build_dupe_rows(&mut self) {
        let Some(report) = self.dupes.as_ref() else { return };
        let widest = report.groups.first().map(|g| g.wasted()).unwrap_or(0);
        let groups: Vec<(usize, Vec<NodeId>, u64)> = report
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| (i, g.ids.clone(), g.bytes_each))
            .collect();

        for (i, ids, bytes_each) in groups {
            // A copy already deleted this session leaves the group behind; a
            // group down to one copy is not a duplicate any more.
            let live: Vec<NodeId> = ids
                .into_iter()
                .filter(|id| self.tree.node(*id).flags & flags::DELETED == 0)
                .collect();
            if live.len() < 2 {
                continue;
            }
            let shown: Vec<NodeId> =
                live.iter().copied().filter(|id| self.passes_filter(*id)).collect();
            if shown.is_empty() {
                continue;
            }
            self.rows.push(Row {
                id: shown[0],
                depth: 0,
                sibling_max: widest,
                header: Some(Heading::Dupes(i)),
                tool: None,
            });
            if self.dupes_open.contains(&i) {
                for id in shown {
                    self.rows.push(Row::node(id, 1, bytes_each));
                }
            }
        }
    }

    /// The copies in a group that are still here, newest first. The newest is
    /// the one "keep one" keeps.
    pub fn dupe_items(&self, group: usize) -> Vec<NodeId> {
        self.dupes
            .as_ref()
            .and_then(|r| r.groups.get(group))
            .map(|g| {
                g.ids
                    .iter()
                    .copied()
                    .filter(|id| self.tree.node(*id).flags & flags::DELETED == 0)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Make every copy in a group share one copy of the storage, instead of
    /// deleting all but one of them.
    ///
    /// The other half of what the duplicate view can offer, and the better half
    /// where the filesystem supports it: the same bytes come back, and every
    /// path goes on working. Nothing is staged and nothing goes to the trash,
    /// because nothing is being removed — which is also why this does not go
    /// through the basket.
    ///
    /// The tree is deliberately left alone afterwards. `du` counts a clone at
    /// its full size, `fad`'s sizes are `du`'s, and quietly zeroing one here
    /// would produce a number that jumped back up on the next rescan. What
    /// moves instead is the free-space figure in the header, which is the one
    /// that was ever really the point.
    pub fn clone_group(&mut self, group: usize) -> String {
        let items = self.dupe_items(group);
        let Some((keep, copies)) = items.split_first() else {
            return "nothing to share".into();
        };
        if copies.is_empty() {
            return "only one copy left".into();
        }
        let keep_path = self.tree.path(*keep);
        let bytes_each =
            self.dupes.as_ref().and_then(|r| r.groups.get(group)).map(|g| g.bytes_each);
        let Some(bytes_each) = bytes_each else { return "that group has gone".into() };

        let (mut shared, mut freed) = (0usize, 0u64);
        let mut refused: Option<String> = None;
        for id in copies {
            let path = self.tree.path(*id);
            match crate::clone::share(&keep_path, &path, bytes_each) {
                Ok(()) => {
                    shared += 1;
                    // Allocated blocks, not length: what came back is what the
                    // second copy was costing the volume.
                    freed += self.tree.self_size(*id, false);
                }
                // Nothing on this filesystem will work, so stop rather than
                // fail the same way once per copy.
                Err(crate::clone::Refusal::Unsupported) => {
                    return crate::clone::Refusal::Unsupported.to_string();
                }
                Err(e) => refused = Some(e.to_string()),
            }
        }

        if shared > 0 {
            self.drop_shared_group(group);
            self.mark_dirty();
        }
        match (shared, refused) {
            (0, Some(why)) => why,
            (0, None) => "nothing to share".into(),
            (n, why) => {
                let mut msg = format!(
                    "{n} cop{} now share{} storage with the newest \u{2014} {} back on the volume, though du still counts both",
                    if n == 1 { "y" } else { "ies" },
                    if n == 1 { "s" } else { "" },
                    crate::format::human(freed)
                );
                if let Some(why) = why {
                    msg.push_str(&format!(" \u{b7} one refused: {why}"));
                }
                msg
            }
        }
    }

    /// Take a group off the list once its copies share their storage. They are
    /// still byte-for-byte identical, and there is no longer anything to
    /// reclaim by deleting one — which is the only reason the view lists them.
    fn drop_shared_group(&mut self, group: usize) {
        let Some(report) = self.dupes.as_mut() else { return };
        if group < report.groups.len() {
            report.groups.remove(group);
        }
        // The open set is keyed by position, so every index above the one that
        // went now means a different group.
        self.dupes_open = self
            .dupes_open
            .iter()
            .filter(|i| **i != group)
            .map(|i| if *i > group { i - 1 } else { *i })
            .collect();
    }

    /// Node ids are arena indices, so a duplicate report is only ever about
    /// the tree it was computed from. Any tree swap has to throw it away —
    /// carrying it over would point "delete this copy" at an unrelated file.
    fn invalidate_dupes(&mut self) {
        self.dupes = None;
        self.dupes_rx = None;
        self.dupes_open.clear();
        self.dupe_view = false;
    }

    pub fn dupes_is_open(&self, group: usize) -> bool {
        self.dupes_open.contains(&group)
    }

    pub fn dupe_hunt_running(&self) -> bool {
        self.dupes_rx.is_some()
    }

    /// Start hashing. Only worth doing on a finished tree: half a walk means
    /// half the candidates, and a duplicate whose twin has not been scanned yet
    /// simply does not look like one.
    pub fn start_dupe_hunt(&mut self) {
        if self.dupes_rx.is_some() {
            return;
        }
        let mut candidates = Vec::new();
        let mut stack = vec![self.tree.root()];
        while let Some(id) = stack.pop() {
            let n = self.tree.node(id);
            if n.flags & flags::DELETED != 0 {
                continue;
            }
            if n.flags & flags::IS_DIR != 0 {
                stack.extend_from_slice(&n.children);
                continue;
            }
            // Hardlinked copies already share their storage, so deleting one
            // frees nothing and calling them duplicates would be a lie.
            if n.flags & flags::HARDLINK_DUPE != 0 || n.kind != crate::scan::meta::Kind::File {
                continue;
            }
            if n.self_len < crate::dupes::MIN_SIZE {
                continue;
            }
            candidates.push(crate::dupes::Candidate {
                id,
                path: self.tree.path(id),
                // Content identity is about the bytes in the file, so this is
                // the one size that is never `--apparent`-dependent.
                bytes: n.self_len,
                mtime: n.mtime,
            });
        }

        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send(crate::dupes::find(candidates));
        });
        self.dupes_rx = Some(rx);
    }

    /// Collect a finished hunt. Returns true if the view needs a redraw.
    pub fn poll_dupes(&mut self) -> bool {
        let Some(rx) = self.dupes_rx.as_ref() else { return false };
        let Ok(report) = rx.try_recv() else { return false };
        self.dupes_rx = None;
        self.dupes = Some(report);
        self.dupes_open.clear();
        self.cursor = 0;
        self.offset = 0;
        self.mark_dirty();
        true
    }

    /// Everything in a category, whether or not its heading is open. `A` on a
    /// closed heading stages the lot, and the heading reports the full total.
    pub fn reclaim_items(&self, cat: Category) -> Vec<NodeId> {
        self.reclaim_cats
            .iter()
            .find(|(c, _)| *c == cat)
            .map(|(_, items)| items.clone())
            .unwrap_or_default()
    }

    pub fn reclaim_is_open(&self, cat: Category) -> bool {
        self.reclaim_open.contains(&cat)
    }

    fn push_row(&mut self, id: NodeId, depth: u16, sibling_max: u64) {
        self.rows.push(Row::node(id, depth, sibling_max));
        if !self.expanded.contains(&id) {
            return;
        }
        self.tree.sort_children(id, self.sort, self.apparent);
        let child_max = self.tree.max_child_size(id, self.apparent);
        let children = self.tree.node(id).children.clone();
        for c in children {
            if !self.passes_filter(c) {
                continue;
            }
            self.push_row(c, depth + 1, child_max);
        }
    }

    /// Is this entry on the user's ignore list? Checked before anything else,
    /// and tallied, so the tree can report what it is not showing.
    pub fn is_ignored(&self, id: NodeId) -> bool {
        if self.ignore.is_empty() {
            return false;
        }
        let n = self.tree.node(id);
        self.ignore.matches(&self.tree.path(id), &n.name, n.is_dir())
    }

    fn passes_filter(&mut self, id: NodeId) -> bool {
        if self.is_ignored(id) {
            self.ignored.0 += 1;
            self.ignored.1 += self.tree.size(id, self.apparent);
            return false;
        }
        if !self.passes_age(id) {
            return false;
        }
        if self.filter.is_empty() {
            return true;
        }
        if self.fuzzy_matches(id) {
            return true;
        }
        // A directory whose own name misses still has to show, or the children
        // that do match become unreachable.
        self.tree.node(id).flags & flags::IS_DIR != 0 && self.subtree_matches(id)
    }

    /// A subtree shows only when *nothing* in it has been written since the
    /// cutoff. Testing the newest write rather than the directory's own mtime
    /// is the difference between "abandoned" and "the folder was reorganised".
    fn passes_age(&self, id: NodeId) -> bool {
        let Some(cutoff) = self.age_filter.cutoff() else { return true };
        self.tree.node(id).last_write() <= now_secs() - cutoff
    }

    fn fuzzy_matches(&mut self, id: NodeId) -> bool {
        let name = self.tree.node(id).name.clone();
        let mut hb = Vec::new();
        let mut nb = Vec::new();
        let haystack = Utf32Str::new(&name, &mut hb);
        let needle = Utf32Str::new(&self.filter, &mut nb);
        // Smart case, like every other tool with a `/`: a lowercase query is
        // case-insensitive, and typing a capital means you meant it. The
        // matcher's own default is unconditionally case-insensitive, so the
        // decision has to be made here, per query.
        self.matcher.config.ignore_case = !self.filter.chars().any(char::is_uppercase);
        self.matcher.fuzzy_match(haystack, needle).is_some()
    }

    fn subtree_matches(&mut self, id: NodeId) -> bool {
        // One level only: descending the whole subtree per row would turn every
        // keystroke into a full-tree walk.
        let children = self.tree.node(id).children.clone();
        children.iter().any(|c| self.fuzzy_matches(*c))
    }

    /// Everything the scan did not count, and everything it counted but will
    /// not show.
    ///
    /// The banners along the bottom of the tree say how many of each there are,
    /// which is enough to know a total is short and not enough to do anything
    /// about it. "Why is this smaller than the Finder says" was unanswerable
    /// from inside the UI; this is the answer, path by path.
    pub fn collect_omissions(&mut self) {
        /// A cap, because a permissions problem can produce thousands of these
        /// and a list that long is not a list.
        const KEEP: usize = 500;

        let mut out: Vec<Omission> = Vec::new();
        for (id, reason) in &self.tree.skipped {
            out.push(Omission {
                path: self.tree.path(*id),
                why: match reason {
                    crate::scan::walk::Skip::CloudStorage => Why::Cloud,
                    crate::scan::walk::Skip::OtherDevice => Why::OtherVolume,
                },
                bytes: None,
            });
        }
        for id in 0..self.tree.len() as NodeId {
            let n = self.tree.node(id);
            if n.flags & (flags::UNREADABLE | flags::DELETED) != flags::UNREADABLE {
                continue;
            }
            out.push(Omission { path: self.tree.path(id), why: Why::Unreadable, bytes: None });
        }
        // Only the topmost match of each ignored branch. An absolute path in
        // the ignore list covers everything beneath it, and listing all of that
        // would bury the four rules the user actually wrote.
        if !self.ignore.is_empty() {
            for id in 0..self.tree.len() as NodeId {
                if self.tree.node(id).flags & flags::DELETED != 0 || !self.is_ignored(id) {
                    continue;
                }
                if self.has_ignored_ancestor(id) {
                    continue;
                }
                out.push(Omission {
                    path: self.tree.path(id),
                    why: Why::Ignored,
                    bytes: Some(self.tree.size(id, self.apparent)),
                });
            }
        }

        // Grouped by reason, and within a reason the ones with a known size
        // first and biggest: everything else has nothing to sort by.
        out.sort_by(|a, b| {
            (a.why as u8)
                .cmp(&(b.why as u8))
                .then_with(|| b.bytes.cmp(&a.bytes))
                .then_with(|| a.path.cmp(&b.path))
        });
        out.truncate(KEEP);
        self.omissions = out;
        self.omission_cursor = 0;
    }

    fn has_ignored_ancestor(&self, id: NodeId) -> bool {
        let mut cur = self.tree.node(id).parent;
        while let Some(p) = cur {
            if self.is_ignored(p) {
                return true;
            }
            cur = self.tree.node(p).parent;
        }
        false
    }

    /// How many entries are missing from the totals, and how much is merely
    /// hidden. Never one figure: one of them makes every size on screen wrong
    /// and the other does not.
    pub fn omission_summary(&self) -> (usize, usize, u64) {
        let uncounted = self.omissions.iter().filter(|o| o.why.uncounted()).count();
        let hidden = self.omissions.iter().filter(|o| !o.why.uncounted());
        let bytes = hidden.clone().filter_map(|o| o.bytes).sum();
        (uncounted, hidden.count(), bytes)
    }

    /// The path under the cursor on the omissions screen.
    pub fn omission_at_cursor(&self) -> Option<&Omission> {
        self.omissions.get(self.omission_cursor)
    }

    // --------------------------------------------------------------- search

    /// Everything in the tree whose name matches, wherever it is, biggest
    /// first.
    ///
    /// A different question from the one `/` answers. The filter narrows what
    /// is already on screen and keeps the shape of the tree around it; this
    /// finds the 8G thing called `*Simulator*` that is four levels down a
    /// branch nobody has opened. Ranked by size rather than by match quality,
    /// because "where is the big one" is the question being asked — a tidier
    /// match that costs nothing is not the answer.
    pub fn run_search(&mut self) {
        /// Enough to choose from without turning the list into its own
        /// haystack. Anything past this is reported as a count.
        const KEEP: usize = 100;

        self.search_hits.clear();
        self.search_more = 0;
        self.search_cursor = 0;
        if self.search.is_empty() {
            return;
        }
        let apparent = self.apparent;
        // Smart case, matching `/`: a lowercase query is case-insensitive, and
        // typing a capital means you meant it.
        self.matcher.config.ignore_case = !self.search.chars().any(char::is_uppercase);

        // A bounded min-heap, so the whole arena costs one comparison a node
        // and only a contender costs a push.
        let mut best: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::new();
        let mut found = 0usize;
        for id in 0..self.tree.len() as NodeId {
            let n = self.tree.node(id);
            if n.flags & flags::DELETED != 0 {
                continue;
            }
            if !self.name_matches(id) {
                continue;
            }
            found += 1;
            let bytes = self.tree.size(id, apparent);
            if best.len() < KEEP {
                best.push(Reverse((bytes, id)));
            } else if best.peek().is_some_and(|Reverse((b, _))| bytes > *b) {
                best.pop();
                best.push(Reverse((bytes, id)));
            }
        }
        self.search_more = found.saturating_sub(best.len());
        let mut hits: Vec<(NodeId, u64)> =
            best.into_iter().map(|Reverse((bytes, id))| (id, bytes)).collect();
        hits.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.search_hits = hits;
    }

    /// Does this entry's name match the search?
    ///
    /// The cheap check first. The whole arena is walked on every keystroke, and
    /// a name that does not contain the query's first character cannot match at
    /// all — which throws away almost everything for the price of a byte
    /// comparison, and leaves the matcher to run on what is left.
    fn name_matches(&mut self, id: NodeId) -> bool {
        let name = self.tree.node(id).name.clone();
        let Some(first) = self.search.chars().next() else { return false };
        let ignore_case = self.matcher.config.ignore_case;
        let present = name.chars().any(|c| {
            c == first || (ignore_case && c.eq_ignore_ascii_case(&first))
        });
        if !present {
            return false;
        }
        let (mut hb, mut nb) = (Vec::new(), Vec::new());
        let haystack = Utf32Str::new(&name, &mut hb);
        let needle = Utf32Str::new(&self.search, &mut nb);
        self.matcher.fuzzy_match(haystack, needle).is_some()
    }

    /// Put the cursor on the search hit under the search cursor, opening every
    /// directory above it on the way.
    ///
    /// Returns what to tell the user, because getting there can mean undoing
    /// something they set up: a hit inside a filtered-out branch is not
    /// reachable until the filter goes, and dropping it silently would be as
    /// confusing as refusing to move.
    pub fn jump_to_hit(&mut self) -> Option<String> {
        let (id, _) = *self.search_hits.get(self.search_cursor)?;
        // The group views are lists, not the tree, and the row this is looking
        // for does not exist in any of them.
        self.reclaim_view = false;
        self.dupe_view = false;
        self.tools_view = false;

        let mut cur = self.tree.node(id).parent;
        while let Some(p) = cur {
            self.expanded.insert(p);
            cur = self.tree.node(p).parent;
        }
        self.mark_dirty();
        self.rebuild_rows();

        if let Some(i) = self.rows.iter().position(|r| r.id == id && r.tool.is_none()) {
            self.cursor = i;
            return None;
        }
        // Hidden by something the user turned on. Turn it off rather than
        // leave the cursor somewhere else with no explanation.
        let was = (!self.filter.is_empty(), self.age_filter != AgeFilter::All);
        self.filter.clear();
        self.age_filter = AgeFilter::All;
        self.mark_dirty();
        self.rebuild_rows();
        if let Some(i) = self.rows.iter().position(|r| r.id == id && r.tool.is_none()) {
            self.cursor = i;
        }
        match was {
            (true, true) => Some("cleared the filter and the age filter to get there".into()),
            (true, false) => Some("cleared the filter to get there".into()),
            (false, true) => Some("cleared the age filter to get there".into()),
            // Ignored, then: it is in the tree and will not be shown.
            (false, false) => Some("that is on your ignore list, so the tree will not show it".into()),
        }
    }

    /// Move the detail pane on to the next breakdown. Nothing else depends on
    /// it: the rows are unchanged and the breakdown already holds all four
    /// answers, so this is a pure redraw.
    ///
    /// On a pane tall enough for all four this only reorders them. That is the
    /// honest behaviour — there is nothing left to reveal — and it keeps one
    /// rule for the key rather than two.
    pub fn cycle_panel(&mut self) {
        self.panel = self.panel.next();
    }

    /// Recompute the breakdown if the selection moved. Called once per frame;
    /// the cache makes all but the first call free.
    pub fn ensure_breakdown(&mut self) {
        let Some(id) = self.selected() else {
            self.breakdown = None;
            return;
        };
        // Recomputing costs a subtree walk, so do it when the selection moves,
        // and otherwise only when the size has actually changed and enough time
        // has passed that we are not doing it on every frame of a live scan.
        const THROTTLE: Duration = Duration::from_millis(250);
        let bytes = self.tree.size(id, self.apparent);
        if let Some(c) = self.breakdown.as_ref() {
            let fresh = c.id == id && (c.at_bytes == bytes || c.at.elapsed() < THROTTLE);
            if fresh {
                return;
            }
        }
        self.breakdown = Some(self.analyse(id));
    }

    /// Sizes by extension and by age across the whole subtree, not just one
    /// level down: a directory of directories tells you nothing otherwise.
    /// Both answers come out of a single walk, because the walk is the
    /// expensive part and the two are always shown together.
    fn analyse(&self, id: NodeId) -> Breakdown {
        /// Past this many nodes the answer is already shaped; walking further
        /// would stall the frame for no extra insight.
        const BUDGET: usize = 200_000;

        /// How many of the biggest files to keep. Five fits the pane and is
        /// enough to tell "one huge file" from "a directory of huge files".
        const BIGGEST: usize = 5;

        let mut by_ext: HashMap<&str, (u64, u64)> = HashMap::new();
        let mut ages = [0u64; 4];
        let mut sizes = [(0u64, 0u64); 4];
        // A bounded min-heap: the smallest of the leaders is on top, so each
        // file costs one comparison and only a contender costs a push.
        let mut biggest: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::new();
        let now = now_secs();
        let mut stack = vec![id];
        let mut visited = 0usize;
        let mut partial = false;

        while let Some(cur) = stack.pop() {
            visited += 1;
            if visited > BUDGET {
                partial = true;
                break;
            }
            let n = self.tree.node(cur);
            if n.flags & flags::IS_DIR != 0 {
                stack.extend_from_slice(&n.children);
                continue;
            }
            let bytes = self.tree.self_size(cur, self.apparent);
            let key = extension_of(&n.name);
            let e = by_ext.entry(key).or_default();
            e.0 += bytes;
            e.1 += 1;

            let age = (now - n.mtime).max(0);
            let bucket = AGE_BUCKETS.iter().position(|(_, max)| age < *max).unwrap_or(3);
            ages[bucket] += bytes;

            // SIZE_CLASSES runs largest first, so the first class whose floor
            // this file clears is its class.
            let class = SIZE_CLASSES.iter().position(|(_, min)| bytes >= *min).unwrap_or(3);
            sizes[class].0 += bytes;
            sizes[class].1 += 1;

            if biggest.len() < BIGGEST {
                biggest.push(Reverse((bytes, cur)));
            } else if biggest.peek().is_some_and(|Reverse((b, _))| bytes > *b) {
                biggest.pop();
                biggest.push(Reverse((bytes, cur)));
            }
        }

        let mut biggest: Vec<FileRow> = biggest
            .into_iter()
            .map(|Reverse((bytes, id))| FileRow { name: self.tree.node(id).name.to_string(), bytes })
            .collect();
        biggest.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));

        // Not from the walk: one level down is exact whatever the budget did,
        // and this is the block that answers "where is it".
        let kids = &self.tree.node(id).children;
        let child_count = kids.len();
        let mut children: Vec<ChildRow> = kids
            .iter()
            .map(|c| ChildRow {
                name: self.tree.node(*c).name.to_string(),
                bytes: self.tree.size(*c, self.apparent),
            })
            .collect();
        children.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));
        children.truncate(3);

        let mut items: Vec<ExtRow> = by_ext
            .into_iter()
            .map(|(ext, (bytes, count))| ExtRow { ext: ext.to_string(), bytes, count })
            .collect();
        items.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));
        // The extension list has the lower half of the pane to itself now that
        // `S` cycles the breakdowns, so it can afford more than the six it got
        // when it shared the space with the age histogram. The renderer trims
        // to whatever the terminal actually gives it.
        items.truncate(10);
        Breakdown {
            id,
            exts: items,
            ages,
            growth: self.growth(id),
            partial,
            children,
            child_count,
            biggest,
            sizes,
            at_bytes: self.tree.size(id, self.apparent),
            at: Instant::now(),
        }
    }

    /// The basket, grouped by what the built-in rules make of each item. A
    /// batch of forty directories is unreviewable as a flat list; the same
    /// forty under "build artifacts" and "app caches" can be read at a glance.
    pub fn basket_rows(&self) -> Vec<BasketRow> {
        let mut groups: Vec<(Option<Category>, Vec<NodeId>)> = Vec::new();
        for cat in Category::all().into_iter().map(Some).chain([None]) {
            let mut items: Vec<NodeId> = self
                .staged
                .iter()
                .copied()
                .filter(|id| self.tree.node(*id).preset == cat)
                .collect();
            if items.is_empty() {
                continue;
            }
            items.sort_unstable_by_key(|id| std::cmp::Reverse(self.tree.size(*id, self.apparent)));
            groups.push((cat, items));
        }

        let mut rows = Vec::new();
        for (cat, items) in groups {
            let bytes = items.iter().map(|id| self.tree.size(*id, self.apparent)).sum();
            rows.push(BasketRow::Group { cat, count: items.len(), bytes });
            rows.extend(items.into_iter().map(BasketRow::Item));
        }
        // Last, and under its own heading. These do not go to the trash and `u`
        // will not bring them back, so they are never mixed in among things
        // that will.
        if !self.staged_tools.is_empty() {
            rows.push(BasketRow::ToolGroup {
                count: self.staged_tools.len(),
                freed: self.staged_tool_freed(),
            });
            rows.extend(self.staged_tools.iter().cloned().map(BasketRow::ToolItem));
        }
        rows
    }

    /// The name to show for a staged tool item, or its id if the report has
    /// moved on underneath us.
    pub fn tool_name(&self, key: &crate::tools::ToolKey) -> String {
        self.tools
            .as_ref()
            .and_then(|r| r.get(key))
            .map(|r| r.name.clone())
            .unwrap_or_else(|| key.id.clone())
    }

    /// Take out what fad put in the trash.
    ///
    /// Only fad's own entries: it knows exactly where each one landed because
    /// it wrote them down, and everything else in the user's trash was put
    /// there by someone else for reasons fad does not know.
    ///
    /// Nothing is removed from the journal. Those batches stay on the history
    /// screen and start reporting themselves as emptied and unrestorable,
    /// which is precisely what has happened to them.
    pub fn empty_trash(&mut self) {
        let items: Vec<(PathBuf, u64)> =
            delete::trashed_entries().into_iter().map(|(to, _, bytes)| (to, bytes)).collect();
        if items.is_empty() {
            self.mode = Mode::Normal;
            self.status = Some("nothing of fad's is still in the trash".into());
            return;
        }
        self.emptying = true;
        self.job = Some(Job::erase(items));
        self.mode = Mode::Deleting;
        self.mark_dirty();
    }

    /// Free space once the trash goes out, and what it is now.
    ///
    /// The mirror of `after_commit`, and the reason emptying is worth a screen
    /// of its own: this is the only figure in fad where the bytes are already
    /// deleted and the space still has not moved.
    pub fn after_empty(&self) -> Option<(u64, u64)> {
        let free = crate::platform::free_space(self.tree.root_path())?;
        Some((free.saturating_add(self.trash_pending.1), free))
    }

    /// Free space once this batch lands, and the total, for the one number the
    /// user actually came for.
    pub fn after_commit(&self) -> Option<(u64, u64)> {
        let root = self.tree.root_path();
        let free = crate::platform::free_space(root)?;
        // Only tool bytes that really come back to this disk. Everything inside
        // a VM image that does not shrink frees space inside that image and
        // nothing here, and putting it in this figure would make the one number
        // the user came for the one number that is wrong.
        let gained = self.staged_bytes().saturating_add(self.staged_tool_host_bytes());
        Some((free.saturating_add(gained), free))
    }

    /// What this path did since the last scan. Resolved by path, not by id:
    /// arena indices mean nothing across two trees.
    fn growth(&self, id: NodeId) -> Option<Option<i64>> {
        let previous = self.previous.as_ref()?;
        let path = self.tree.path(id);
        let Some(then) = previous.find_path(&path) else { return Some(None) };
        let (now, was) =
            (self.tree.size(id, self.apparent), previous.size(then, self.apparent));
        Some(Some(now as i64 - was as i64))
    }

    /// Nothing staged on either axis.
    ///
    /// The two sets are never added together — the whole point of keeping them
    /// apart — but "is there a batch" is one question, and every early-return
    /// that used to ask `staged.is_empty()` has to ask this instead or a batch
    /// of nothing but Docker images would look like no batch at all.
    pub fn nothing_staged(&self) -> bool {
        self.staged.is_empty() && self.staged_tools.is_empty()
    }

    pub fn staged_bytes(&self) -> u64 {
        self.staged.iter().map(|id| self.tree.size(*id, self.apparent)).sum()
    }

    /// Stage `id`, dropping anything already staged inside it.
    ///
    /// Without this a directory and something under it can both be staged: the
    /// total double-counts the child, and since the batch runs largest-first the
    /// child is deleted along with its parent and then reported as a failure for
    /// a path that is exactly as gone as the user asked for.
    pub fn stage(&mut self, id: NodeId) {
        if self.is_staged_under(id) {
            return;
        }
        let nested: Vec<NodeId> =
            self.staged.iter().copied().filter(|o| self.is_ancestor(id, *o)).collect();
        for n in nested {
            self.staged.remove(&n);
        }
        self.staged.insert(id);
    }

    /// True when `id` is already covered by a staged ancestor.
    fn is_staged_under(&self, id: NodeId) -> bool {
        self.staged.iter().any(|s| self.is_ancestor(*s, id))
    }

    /// Is `ancestor` strictly above `id`?
    fn is_ancestor(&self, ancestor: NodeId, id: NodeId) -> bool {
        let mut cur = self.tree.node(id).parent;
        while let Some(p) = cur {
            if p == ancestor {
                return true;
            }
            cur = self.tree.node(p).parent;
        }
        false
    }

    /// Staged items in commit order, largest first, with anything the guard
    /// refuses split off so the modal can show it rather than fail silently.
    pub fn review_batch(&mut self) {
        self.review_tool_batch();
        let root = self.tree.root_path().to_path_buf();
        self.refused.clear();
        let mut keep = HashSet::new();
        for id in self.staged.clone() {
            let path = self.tree.path(id);
            // Ignoring something is a standing instruction to leave it alone,
            // so it has to survive a batch that was staged before the rule was
            // added — or staged from a view the rule does not filter.
            if self.is_ignored(id) {
                self.refused.push((id, "on your ignore list".into()));
                continue;
            }
            match delete::guard(&path, &root) {
                Ok(()) => {
                    keep.insert(id);
                }
                Err(why) => self.refused.push((id, why)),
            }
        }
        self.staged = keep;
    }

    /// Items to hand the delete worker, biggest first so the reclaimed space
    /// shows up as early as possible.
    pub fn batch_items(&self) -> Vec<(std::path::PathBuf, u64)> {
        let mut ids: Vec<NodeId> = self.staged.iter().copied().collect();
        ids.sort_unstable_by_key(|id| std::cmp::Reverse(self.tree.size(*id, self.apparent)));
        ids.iter().map(|id| (self.tree.path(*id), self.tree.size(*id, self.apparent))).collect()
    }

    /// Start the batch.
    ///
    /// Up to two jobs, because the two halves are not the same operation and
    /// must not be made to look like it: files go to the trash and can be put
    /// back, tool resources are handed to the daemon that owns them and are
    /// gone. `Mode::Deleting` polls both.
    pub fn commit(&mut self) {
        let items = self.batch_items();
        let tools = self.tool_batch_items();
        if items.is_empty() && tools.is_empty() {
            return;
        }
        if !items.is_empty() {
            self.job = Some(Job::start(items, self.disposal));
        }
        if !tools.is_empty() {
            self.tool_job = Some(crate::tools::Job::start(tools));
        }
        self.mode = Mode::Deleting;
        self.mark_dirty();
    }

    /// Any tool item in the current batch lives inside a VM disk that will not
    /// shrink, so the space it frees does not reach this disk.
    ///
    /// Reads the running job's own items rather than `staged_tools`, which is
    /// emptied as each removal lands — by the time the measured figure is worth
    /// captioning, the staged set is gone.
    pub fn stuck_in_vm(&self) -> bool {
        let Some(report) = self.tools.as_ref() else { return false };
        let touched: Vec<crate::tools::Source> = match self.tool_job.as_ref() {
            Some(job) => job.done.iter().map(|o| o.key.source).collect(),
            None => self.staged_tools.iter().map(|k| k.source).collect(),
        };
        report
            .sources
            .iter()
            .filter(|s| !s.backing.frees_host_space())
            .any(|s| touched.contains(&s.source))
    }

    /// Both jobs have finished, or there were none.
    pub fn batch_finished(&self) -> bool {
        self.job.as_ref().is_none_or(|j| j.is_finished())
            && self.tool_job.as_ref().is_none_or(|j| j.is_finished())
    }

    /// Fold finished deletions into the tree so sizes drop as they land.
    pub fn poll_job(&mut self) -> bool {
        let Some(job) = self.job.as_mut() else { return false };
        if !job.poll() {
            return false;
        }
        // Map the paths that succeeded back onto the nodes that produced them.
        let succeeded: Vec<PathBuf> = job
            .done
            .iter()
            .filter(|o| o.result.is_ok())
            .map(|o| o.path.clone())
            .collect();
        let staged: Vec<NodeId> = self.staged.iter().copied().collect();
        for id in staged {
            let path = self.tree.path(id);
            if succeeded.contains(&path) {
                self.tree.remove(id);
                self.staged.remove(&id);
            }
        }
        self.expanded.retain(|id| self.tree.node(*id).flags & flags::DELETED == 0);
        self.mark_dirty();
        true
    }

    /// Reread how much room is left on the volume. Returns true if the figure
    /// moved, which during a scan it will not: nothing fad has done yet
    /// changes it, and everything the user is about to do is measured against
    /// it.
    pub fn poll_volume(&mut self) -> bool {
        const EVERY: Duration = Duration::from_secs(2);
        if self.volume_at.is_some_and(|t| t.elapsed() < EVERY) {
            return false;
        }
        self.volume_at = Some(Instant::now());
        let fresh = crate::platform::volume(self.tree.root_path());
        let changed = match (&self.volume, &fresh) {
            (Some(a), Some(b)) => a.free != b.free || a.total != b.total,
            (None, None) => false,
            _ => true,
        };
        self.volume = fresh;
        changed
    }

    /// True when the scan spans more than one filesystem, so the free-space
    /// figure in the header describes only the volume the root is on.
    pub fn spans_volumes(&self) -> bool {
        self.opts.cross_device
    }

    /// Reread the journal. Called whenever a batch lands or is put back, which
    /// are the only two things that change it.
    pub fn refresh_history(&mut self) {
        self.history = delete::read_journal();
        self.history.reverse();
        self.history_cursor = self.history_cursor.min(self.history.len().saturating_sub(1));
        self.trash_pending = delete::still_in_trash();
    }

    pub fn finish_job(&mut self) {
        self.refresh_history();
        self.job = None;
        self.emptying = false;
        self.disposal = Disposal::Trash;
        self.refused.clear();
        if self.tool_job.take().is_some() {
            // The report on screen described a store that has just changed
            // underneath it. Keeping it would show sizes for things that are
            // gone, so it is dropped and the next `t` asks again.
            self.tools = None;
            self.tools_at = None;
            self.staged_tools.clear();
            self.tools_refused.clear();
            if self.tools_view {
                self.start_tool_probe();
            }
        }
        self.mode = Mode::Normal;
        self.mark_dirty();
    }
}
