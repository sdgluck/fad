//! Everything the UI needs to know, and everything a keypress can change.

use std::collections::{HashMap, HashSet};
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
#[derive(Clone, Copy, PartialEq, Eq)]
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
    /// The subtree's size when this was computed. A live scan keeps growing the
    /// tree under the cursor, and a breakdown taken when the node was empty
    /// would otherwise stay empty for the rest of the session.
    at_bytes: u64,
    at: Instant,
}

/// One line of the staging basket.
pub enum BasketRow {
    Group { cat: Option<Category>, count: usize, bytes: u64 },
    Item(NodeId),
}

pub struct ExtRow {
    pub ext: String,
    pub bytes: u64,
    pub count: u64,
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Heading {
    Category(Category),
    /// Index into `dupes.groups`.
    Dupes(usize),
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
}

impl Row {
    fn node(id: NodeId, depth: u16, sibling_max: u64) -> Row {
        Row { id, depth, sibling_max, header: None }
    }
}

#[derive(PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Typing into the fuzzy filter.
    Filter,
    Help,
    /// The staging basket: the whole batch on one screen, editable.
    Basket,
    /// The undo journal: every batch this machine still remembers.
    History,
    /// Reviewing the staged batch before committing it.
    Confirm,
    /// A batch is being deleted, or has just finished.
    Deleting,
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
    /// The undo journal, newest first, as of the last time it was opened.
    pub history: Vec<delete::Batch>,
    /// Index into `history`.
    pub history_cursor: usize,
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
            status: None,
            reclaim_view: false,
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
            age_filter: AgeFilter::All,
            expected_entries: None,
            ignore: crate::ignore::Rules::load(),
            ignored: (0, 0),
            mouse: true,
            basket_cursor: 0,
            history: Vec::new(),
            history_cursor: 0,
            trash_pending: delete::still_in_trash(),
            tree_list: ratatui::layout::Rect::ZERO,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            disposal: Disposal::Trash,
            job: None,
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

    pub fn selected(&self) -> Option<NodeId> {
        self.rows.get(self.cursor).map(|r| r.id)
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

        // Keep the cursor on the same node across a rebuild; sizes arriving
        // mid-scan reorder rows underneath it constantly otherwise.
        let anchor = self.rows.get(self.cursor).map(|r| (r.id, r.header.is_some()));

        self.rows.clear();
        self.ignored = (0, 0);
        if self.dupe_view {
            self.reclaim_cats.clear();
            self.build_dupe_rows();
        } else if self.reclaim_view {
            self.build_reclaim_rows();
        } else {
            self.reclaim_cats.clear();
            let root = self.tree.root();
            let max = self.tree.size(root, self.apparent);
            self.push_row(root, 0, max);
        }

        if let Some((id, was_header)) = anchor {
            // A heading shares its id with the first item under it, so the
            // heading-ness has to be part of the match or the cursor cannot rest
            // on a heading: the restore would keep yanking it onto the item.
            if let Some(i) =
                self.rows.iter().position(|r| r.id == id && r.header.is_some() == was_header)
            {
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
                .filter(|id| !self.has_reclaimable_ancestor(*id))
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
            });
            if self.reclaim_open.contains(&cat) {
                for id in &items {
                    self.rows.push(Row::node(*id, 1, max));
                }
            }
            self.reclaim_cats.push((cat, items));
        }
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

    fn has_reclaimable_ancestor(&self, id: NodeId) -> bool {
        let mut cur = self.tree.node(id).parent;
        while let Some(p) = cur {
            if self.tree.node(p).preset.is_some() {
                return true;
            }
            cur = self.tree.node(p).parent;
        }
        false
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

        let mut by_ext: HashMap<&str, (u64, u64)> = HashMap::new();
        let mut ages = [0u64; 4];
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
        }

        let mut items: Vec<ExtRow> = by_ext
            .into_iter()
            .map(|(ext, (bytes, count))| ExtRow { ext: ext.to_string(), bytes, count })
            .collect();
        items.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));
        // Six, not eight: the age histogram below earns the two rows more than
        // a seventh extension does in a 38-column pane.
        items.truncate(6);
        Breakdown {
            id,
            exts: items,
            ages,
            growth: self.growth(id),
            partial,
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
        rows
    }

    /// Free space once this batch lands, and the total, for the one number the
    /// user actually came for.
    pub fn after_commit(&self) -> Option<(u64, u64)> {
        let root = self.tree.root_path();
        let free = crate::platform::free_space(root)?;
        Some((free.saturating_add(self.staged_bytes()), free))
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

    pub fn commit(&mut self) {
        let items = self.batch_items();
        if items.is_empty() {
            return;
        }
        self.job = Some(Job::start(items, self.disposal));
        self.mode = Mode::Deleting;
        self.mark_dirty();
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
        self.disposal = Disposal::Trash;
        self.refused.clear();
        self.mode = Mode::Normal;
        self.mark_dirty();
    }
}
