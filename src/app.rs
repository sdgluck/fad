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

pub struct ExtBreakdown {
    pub id: NodeId,
    pub items: Vec<ExtRow>,
    /// The subtree was larger than the walk budget, so these are a sample.
    pub partial: bool,
    /// The subtree's size when this was computed. A live scan keeps growing the
    /// tree under the cursor, and a breakdown taken when the node was empty
    /// would otherwise stay empty for the rest of the session.
    at_bytes: u64,
    at: Instant,
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

/// One line of the tree pane.
#[derive(Clone, Copy)]
pub struct Row {
    pub id: NodeId,
    pub depth: u16,
    /// Largest sibling total, for scaling this row's bar.
    pub sibling_max: u64,
    /// Category headers in the reclaimable view are rows too, so the cursor
    /// can land on one and stage everything under it.
    pub header: Option<Category>,
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
    pub mode: Mode,
    pub filter: String,
    pub status: Option<String>,
    /// Showing only what the built-in rules consider reclaimable.
    pub reclaim_view: bool,
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
    snapshot_rx: Option<crossbeam_channel::Receiver<Option<Tree>>>,

    /// Extension breakdown for the current selection. Aggregating a subtree of
    /// a million nodes is far too slow to redo every frame, and the answer only
    /// changes when the selection moves.
    pub ext_cache: Option<ExtBreakdown>,

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

        let Some(snapshot) = loaded else { return false };
        if self.scan.is_none() || self.pending.is_some() {
            return false;
        }
        // The tree being filled becomes the pending one; the snapshot goes on screen.
        let live = std::mem::replace(&mut self.tree, snapshot);
        self.pending = Some(live);
        self.from_cache = true;
        self.expanded = HashSet::from([self.tree.root()]);
        self.ext_cache = None;
        self.cursor = 0;
        self.offset = 0;
        self.dirty = true;
        true
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
            mode: Mode::Normal,
            filter: String::new(),
            status: None,
            reclaim_view: false,
            from_cache: false,
            pending: None,
            snapshot_rx: None,
            ext_cache: None,
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
        self.staged.clear();
        self.expanded = HashSet::from([self.tree.root()]);
        self.ext_cache = None;
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

        self.tree = fresh;
        self.from_cache = false;
        self.ext_cache = None;

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
        let anchor = self.selected();

        self.rows.clear();
        if self.reclaim_view {
            self.build_reclaim_rows();
        } else {
            let root = self.tree.root();
            let max = self.tree.node(root).total_bytes;
            self.push_row(root, 0, max);
        }

        if let Some(anchor) = anchor {
            if let Some(i) = self.rows.iter().position(|r| r.id == anchor) {
                self.cursor = i;
            }
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    /// The reclaimable view: every preset match, grouped by category and
    /// ranked by size. The whole point is to make the first screen the answer.
    fn build_reclaim_rows(&mut self) {
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
            items.sort_unstable_by_key(|id| std::cmp::Reverse(self.tree.node(*id).total_bytes));
            let max = self.tree.node(items[0]).total_bytes;

            self.rows.push(Row { id: items[0], depth: 0, sibling_max: max, header: Some(cat) });
            for id in items {
                self.rows.push(Row::node(id, 1, max));
            }
        }
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

    /// Everything the reclaimable view is currently showing, for `A`.
    pub fn reclaim_items(&self, cat: Category) -> Vec<NodeId> {
        self.rows
            .iter()
            .filter(|r| r.header.is_none() && self.tree.node(r.id).preset == Some(cat))
            .map(|r| r.id)
            .collect()
    }

    fn push_row(&mut self, id: NodeId, depth: u16, sibling_max: u64) {
        self.rows.push(Row::node(id, depth, sibling_max));
        if !self.expanded.contains(&id) {
            return;
        }
        self.tree.sort_children(id, self.sort);
        let child_max = self.tree.max_child_bytes(id);
        let children = self.tree.node(id).children.clone();
        for c in children {
            if !self.passes_filter(c) {
                continue;
            }
            self.push_row(c, depth + 1, child_max);
        }
    }

    fn passes_filter(&mut self, id: NodeId) -> bool {
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

    fn fuzzy_matches(&mut self, id: NodeId) -> bool {
        let name = self.tree.node(id).name.clone();
        let mut hb = Vec::new();
        let mut nb = Vec::new();
        let haystack = Utf32Str::new(&name, &mut hb);
        let needle = Utf32Str::new(&self.filter, &mut nb);
        // Smart case, like every other tool with a `/`: a lowercase query is
        // case-insensitive, and typing a capital means you meant it.
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
    pub fn ensure_extensions(&mut self) {
        let Some(id) = self.selected() else {
            self.ext_cache = None;
            return;
        };
        // Recomputing costs a subtree walk, so do it when the selection moves,
        // and otherwise only when the size has actually changed and enough time
        // has passed that we are not doing it on every frame of a live scan.
        const THROTTLE: Duration = Duration::from_millis(250);
        let bytes = self.tree.node(id).total_bytes;
        if let Some(c) = self.ext_cache.as_ref() {
            let fresh = c.id == id && (c.at_bytes == bytes || c.at.elapsed() < THROTTLE);
            if fresh {
                return;
            }
        }
        self.ext_cache = Some(self.extensions(id));
    }

    /// Sizes by extension across the whole subtree, not just one level down:
    /// a directory of directories tells you nothing otherwise.
    fn extensions(&self, id: NodeId) -> ExtBreakdown {
        /// Past this many nodes the answer is already shaped; walking further
        /// would stall the frame for no extra insight.
        const BUDGET: usize = 200_000;

        let mut by_ext: HashMap<&str, (u64, u64)> = HashMap::new();
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
            let key = extension_of(&n.name);
            let e = by_ext.entry(key).or_default();
            e.0 += n.self_bytes;
            e.1 += 1;
        }

        let mut items: Vec<ExtRow> = by_ext
            .into_iter()
            .map(|(ext, (bytes, count))| ExtRow { ext: ext.to_string(), bytes, count })
            .collect();
        items.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));
        items.truncate(8);
        ExtBreakdown {
            id,
            items,
            partial,
            at_bytes: self.tree.node(id).total_bytes,
            at: Instant::now(),
        }
    }

    pub fn staged_bytes(&self) -> u64 {
        self.staged.iter().map(|id| self.tree.node(*id).total_bytes).sum()
    }

    /// Staged items in commit order, largest first, with anything the guard
    /// refuses split off so the modal can show it rather than fail silently.
    pub fn review_batch(&mut self) {
        let root = self.tree.root_path().to_path_buf();
        self.refused.clear();
        let mut keep = HashSet::new();
        for id in self.staged.clone() {
            let path = self.tree.path(id);
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
        ids.sort_unstable_by_key(|id| std::cmp::Reverse(self.tree.node(*id).total_bytes));
        ids.iter().map(|id| (self.tree.path(*id), self.tree.node(*id).total_bytes)).collect()
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

    pub fn finish_job(&mut self) {
        self.job = None;
        self.disposal = Disposal::Trash;
        self.refused.clear();
        self.mode = Mode::Normal;
        self.mark_dirty();
    }
}
