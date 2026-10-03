//! The size tree: a flat arena of nodes linked by index.
//!
//! Nodes are never moved or removed during a scan, so indices stay valid and the
//! UI can hold onto a selection while batches keep landing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::presets::{self, Category};
use crate::scan::meta::{Kind, Meta};
use crate::scan::walk::{Batch, ROOT_ID, ScanId, Skip};

pub type NodeId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Size,
    Count,
    Modified,
    Name,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::Size => Sort::Count,
            Sort::Count => Sort::Modified,
            Sort::Modified => Sort::Name,
            Sort::Name => Sort::Size,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sort::Size => "size",
            Sort::Count => "count",
            Sort::Modified => "modified",
            Sort::Name => "name",
        }
    }
}

pub mod flags {
    pub type Flags = u8;
    pub const IS_DIR: Flags = 1 << 0;
    /// This directory's own children have arrived.
    pub const SCANNED: Flags = 1 << 1;
    pub const UNREADABLE: Flags = 1 << 2;
    pub const OTHER_DEVICE: Flags = 1 << 3;
    pub const HARDLINK_DUPE: Flags = 1 << 4;
    pub const CLOUD: Flags = 1 << 5;
    /// Deleted during this session. The arena entry stays so every other index
    /// remains valid; it is simply no longer anyone's child.
    pub const DELETED: Flags = 1 << 6;
    /// The name is not valid UTF-8. Shown lossily, never descended into, and
    /// never staged: the path fad would rebuild for it does not exist.
    pub const UNNAMED: Flags = 1 << 7;
}

#[derive(Debug)]
pub struct Node {
    /// Path component only. Full paths are rebuilt by walking `parent`.
    pub name: Box<str>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// Allocated bytes for this entry alone.
    pub self_bytes: u64,
    /// Allocated bytes for the whole subtree, growing as batches land.
    pub total_bytes: u64,
    /// `st_size` for this entry alone, for the apparent-vs-disk comparison.
    pub self_len: u64,
    pub total_len: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub mtime: i64,
    /// The most recent mtime of any *file* in this subtree, or `i64::MIN` when
    /// there are none. Directory mtimes are deliberately excluded: a directory's
    /// mtime moves whenever a child is added, removed, or renamed, so a project
    /// nobody has opened in two years looks freshly touched the moment it is
    /// reorganised. Read it through `last_write`, which resolves the sentinel.
    pub newest_file_mtime: i64,
    pub kind: Kind,
    pub flags: flags::Flags,
    /// Set when this entry matches a built-in reclaimable rule.
    pub preset: Option<Category>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.flags & flags::IS_DIR != 0
    }

    /// When this subtree was last written to. Falls back to the entry's own
    /// mtime for a directory holding no files at all, which is the only thing
    /// left to report about it.
    pub fn last_write(&self) -> i64 {
        if self.newest_file_mtime == i64::MIN { self.mtime } else { self.newest_file_mtime }
    }
}

pub struct Tree {
    nodes: Vec<Node>,
    root: NodeId,
    root_path: PathBuf,
    /// Scan-side id -> arena index. Meaningless once a scan is over.
    by_scan_id: HashMap<ScanId, NodeId>,
    /// Batches that arrived before their parent was registered. Should stay
    /// empty given the walker's ordering, but correctness should not rest on it.
    orphans: Vec<Batch>,
    /// Winning node for each multiply-linked inode. Only files with `nlink > 1`
    /// ever land here, so this stays tiny on a normal filesystem.
    links: HashMap<(u64, u64), NodeId>,
    pub unreadable_count: u64,
    /// Every node matching a reclaimable preset, for the `r` view.
    pub reclaimable: Vec<NodeId>,
    /// Directories we stopped at, so the UI can say so rather than quietly
    /// under-reporting. Paths are kept for the "rescan including these" action.
    pub skipped: Vec<(NodeId, Skip)>,
}

impl Tree {
    pub fn new(root_path: PathBuf, root_meta: &Meta) -> Self {
        // The basename, not the whole path: this is what `presets::classify`
        // sees as the parent name of every top-level entry, and a rule keyed on
        // `Library` or `.cargo` must still fire when that directory *is* the
        // scan root. The full path is kept in `root_path` and shown in the
        // pane title.
        let name = Tree::root_name(&root_path);
        let root = Node {
            name,
            parent: None,
            children: Vec::new(),
            self_bytes: root_meta.blocks,
            total_bytes: root_meta.blocks,
            self_len: root_meta.len,
            total_len: root_meta.len,
            file_count: 0,
            dir_count: 0,
            mtime: root_meta.mtime,
            newest_file_mtime: i64::MIN,
            kind: root_meta.kind,
            flags: flags::IS_DIR,
            preset: None,
        };
        let mut by_scan_id = HashMap::new();
        by_scan_id.insert(ROOT_ID, 0);
        Tree {
            nodes: vec![root],
            root: 0,
            root_path,
            by_scan_id,
            orphans: Vec::new(),
            links: HashMap::new(),
            unreadable_count: 0,
            reclaimable: Vec::new(),
            skipped: Vec::new(),
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Never true of a real tree, which always holds its root; here because a
    /// public `len` without it is a lint, and a lint is noise in every review.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Resolve an absolute path to a node, walking down from the root by
    /// component. Used to carry a selection across a tree swap; small numbers
    /// of lookups only, so a linear scan per level is the right trade.
    pub fn find_path(&self, path: &Path) -> Option<NodeId> {
        let rel = path.strip_prefix(&self.root_path).ok()?;
        let mut cur = self.root;
        for part in rel.components() {
            let want = part.as_os_str().to_str()?;
            cur = *self
                .node(cur)
                .children
                .iter()
                .find(|c| self.node(**c).name.as_ref() == want)?;
        }
        Some(cur)
    }

    /// Absolute path of a node, rebuilt from the arena.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(n) = cur {
            let node = self.node(n);
            if node.parent.is_none() {
                break;
            }
            parts.push(node.name.as_ref());
            cur = node.parent;
        }
        let mut p = self.root_path.clone();
        for part in parts.iter().rev() {
            p.push(part);
        }
        p
    }

    /// Fold one batch of directory entries into the tree and roll its sizes up
    /// through every ancestor.
    pub fn apply(&mut self, batch: Batch) {
        let Some(&parent) = self.by_scan_id.get(&batch.parent) else {
            self.orphans.push(batch);
            return;
        };

        if let Some(_kind) = batch.unreadable {
            self.nodes[parent as usize].flags |= flags::UNREADABLE | flags::SCANNED;
            self.unreadable_count += 1;
            return;
        }

        let mut added_bytes = 0u64;
        let mut added_len = 0u64;
        let mut added_files = 0u64;
        let mut added_dirs = 0u64;

        // The whole directory arrives in one batch, so every entry can be
        // classified against its actual siblings without a second pass.
        let siblings: std::collections::HashSet<&str> =
            batch.entries.iter().map(|e| e.name.as_ref()).collect();
        let parent_name = self.nodes[parent as usize].name.clone();
        let presets: Vec<Option<Category>> = batch
            .entries
            .iter()
            .map(|e| {
                presets::classify(&e.name, e.meta.is_dir(), e.meta.len, &parent_name, &siblings)
            })
            .collect();

        let mut children = Vec::with_capacity(batch.entries.len());
        let mut hardlinks = Vec::new();
        let mut newest = i64::MIN;
        for (idx, e) in batch.entries.into_iter().enumerate() {
            let mut f = 0u8;
            if e.meta.is_dir() {
                f |= flags::IS_DIR;
                added_dirs += 1;
            } else {
                added_files += 1;
            }
            match e.skip {
                Some(Skip::OtherDevice) => f |= flags::OTHER_DEVICE | flags::SCANNED,
                Some(Skip::CloudStorage) => f |= flags::CLOUD | flags::SCANNED,
                Some(Skip::UnrepresentableName) => f |= flags::UNNAMED | flags::SCANNED,
                None => {}
            }
            let bytes = e.meta.blocks;
            let len = e.meta.len;

            let id = self.nodes.len() as NodeId;
            self.nodes.push(Node {
                name: e.name,
                parent: Some(parent),
                children: Vec::new(),
                self_bytes: bytes,
                total_bytes: bytes,
                self_len: len,
                total_len: len,
                file_count: 0,
                dir_count: 0,
                mtime: e.meta.mtime,
                // A directory's own entry contributes nothing; only the files
                // under it will raise this, once their batches land.
                newest_file_mtime: if e.meta.is_dir() { i64::MIN } else { e.meta.mtime },
                kind: e.meta.kind,
                flags: f,
                preset: presets[idx],
            });
            if presets[idx].is_some() {
                self.reclaimable.push(id);
            }
            if let Some(scan_id) = e.descend {
                self.by_scan_id.insert(scan_id, id);
            }
            if e.meta.nlink > 1 && !e.meta.is_dir() {
                hardlinks.push((id, e.meta.dev, e.meta.ino));
            }
            if let Some(reason) = e.skip {
                self.skipped.push((id, reason));
            }
            children.push(id);
            if !e.meta.is_dir() {
                newest = newest.max(e.meta.mtime);
            }
            added_bytes += bytes;
            added_len += len;
        }

        let p = &mut self.nodes[parent as usize];
        p.children = children;
        p.flags |= flags::SCANNED;
        self.roll_up(
            parent,
            added_bytes as i64,
            added_len as i64,
            added_files as i64,
            added_dirs as i64,
        );
        if newest > i64::MIN {
            self.bump_newest(parent, newest);
        }

        // Resolved after the rollup so the subtraction never has to underflow a
        // total that has not been added yet.
        for (id, dev, ino) in hardlinks {
            self.resolve_hardlink(id, dev, ino);
        }

        if !self.orphans.is_empty() {
            self.drain_orphans();
        }
    }

    /// Raise `newest_file_mtime` on `from` and its ancestors. Stops at the first
    /// ancestor that is already at least this recent — everything above it must
    /// be too, so a deep tree does not pay a full root walk per batch.
    fn bump_newest(&mut self, from: NodeId, mtime: i64) {
        let mut cur = Some(from);
        while let Some(id) = cur {
            let n = &mut self.nodes[id as usize];
            if n.newest_file_mtime >= mtime {
                return;
            }
            n.newest_file_mtime = mtime;
            cur = n.parent;
        }
    }

    fn drain_orphans(&mut self) {
        loop {
            // `partition`, not `retain`: the batches whose parent has now
            // arrived are the ones we must apply, and `retain` would drop them
            // on the floor instead of handing them back.
            let (ready, waiting): (Vec<Batch>, Vec<Batch>) = std::mem::take(&mut self.orphans)
                .into_iter()
                .partition(|b| self.by_scan_id.contains_key(&b.parent));
            self.orphans = waiting;
            if ready.is_empty() {
                return;
            }
            for b in ready {
                self.apply(b);
            }
        }
    }

    /// A file with several links is counted exactly once, and always against
    /// the same path: the lexicographically smallest one. Deciding it that way
    /// rather than first-one-wins keeps subtotals stable across runs, which a
    /// parallel walk would otherwise scramble.
    fn resolve_hardlink(&mut self, id: NodeId, dev: u64, ino: u64) {
        let Some(&prev) = self.links.get(&(dev, ino)) else {
            self.links.insert((dev, ino), id);
            return;
        };
        let loser = if self.path(id) < self.path(prev) {
            self.links.insert((dev, ino), id);
            prev
        } else {
            id
        };
        let n = &mut self.nodes[loser as usize];
        let (bytes, len) = (n.self_bytes, n.self_len);
        n.self_bytes = 0;
        n.self_len = 0;
        n.flags |= flags::HARDLINK_DUPE;
        self.roll_up(loser, -(bytes as i64), -(len as i64), 0, 0);
        // The winner may have been demoted earlier in this same batch; make sure
        // it is not still wearing the dupe flag.
        let winner = if loser == id { prev } else { id };
        self.nodes[winner as usize].flags &= !flags::HARDLINK_DUPE;
    }

    /// Add a subtree's contribution to `from` and every ancestor above it.
    /// Deltas are signed so hardlink corrections can walk back a rollup.
    fn roll_up(&mut self, from: NodeId, bytes: i64, len: i64, files: i64, dirs: i64) {
        let mut cur = Some(from);
        while let Some(id) = cur {
            let n = &mut self.nodes[id as usize];
            n.total_bytes = n.total_bytes.saturating_add_signed(bytes);
            n.total_len = n.total_len.saturating_add_signed(len);
            n.file_count = n.file_count.saturating_add_signed(files);
            n.dir_count = n.dir_count.saturating_add_signed(dirs);
            cur = n.parent;
        }
    }

    /// Detach a node and take its weight back out of every ancestor.
    /// Returns the bytes reclaimed, or None if it was already gone.
    pub fn remove(&mut self, id: NodeId) -> Option<u64> {
        if id == self.root || self.nodes[id as usize].flags & flags::DELETED != 0 {
            return None;
        }
        let n = &self.nodes[id as usize];
        let parent = n.parent?;
        let (bytes, len) = (n.total_bytes, n.total_len);
        // The node counted as one entry in its own right, on top of its subtree.
        let files = n.file_count + u64::from(n.flags & flags::IS_DIR == 0);
        let dirs = n.dir_count + u64::from(n.flags & flags::IS_DIR != 0);

        self.nodes[id as usize].flags |= flags::DELETED;
        self.nodes[parent as usize].children.retain(|c| *c != id);
        self.roll_up(parent, -(bytes as i64), -(len as i64), -(files as i64), -(dirs as i64));
        // `roll_up` walks sizes back; the newest-write rollup cannot be walked
        // back the same way, because a maximum does not subtract. Delete the one
        // recent file in an archive and every directory above it would go on
        // reporting itself as freshly written — wrong in the detail pane, in the
        // modified sort, and in the age filter, which would keep hiding a branch
        // that is now exactly what it claims to be looking for.
        self.recompute_newest(parent);
        Some(bytes)
    }

    /// Rebuild `newest_file_mtime` from the children that are left, upwards.
    ///
    /// Stops at the first ancestor whose value does not move: it is a maximum
    /// over the level below, so if this level did not change, nothing above it
    /// can have.
    fn recompute_newest(&mut self, from: NodeId) {
        let mut cur = Some(from);
        while let Some(id) = cur {
            let newest = self.nodes[id as usize]
                .children
                .iter()
                .map(|c| self.nodes[*c as usize].newest_file_mtime)
                .max()
                .unwrap_or(i64::MIN);
            let n = &mut self.nodes[id as usize];
            // A file's stamp is its own mtime, not a fold over children it
            // does not have.
            if n.flags & flags::IS_DIR == 0 || n.newest_file_mtime == newest {
                return;
            }
            n.newest_file_mtime = newest;
            cur = n.parent;
        }
    }

    /// Order children by `sort`, in place. Called only for nodes that are
    /// actually on screen, so a live scan never pays to order two million
    /// entries nobody is looking at.
    pub fn sort_children(&mut self, id: NodeId, sort: Sort, apparent: bool) {
        let mut kids = std::mem::take(&mut self.nodes[id as usize].children);
        match sort {
            Sort::Size => kids.sort_unstable_by(|a, b| {
                let (sa, sb) = (self.size(*a, apparent), self.size(*b, apparent));
                let (a, b) = (&self.nodes[*a as usize], &self.nodes[*b as usize]);
                sb.cmp(&sa).then_with(|| a.name.cmp(&b.name))
            }),
            Sort::Count => kids.sort_unstable_by(|a, b| {
                let (a, b) = (&self.nodes[*a as usize], &self.nodes[*b as usize]);
                let (ac, bc) = (a.file_count + a.dir_count, b.file_count + b.dir_count);
                bc.cmp(&ac).then_with(|| a.name.cmp(&b.name))
            }),
            // By `last_write`, for the reason the field exists: a directory's
            // own mtime is when its listing last changed, not when anything in
            // it was last written. Sorting on it would rank a reorganised
            // archive above a project worked on this morning — and would
            // disagree with both the detail pane and the age filter, which read
            // through `last_write`.
            Sort::Modified => kids.sort_unstable_by(|a, b| {
                let (a, b) = (&self.nodes[*a as usize], &self.nodes[*b as usize]);
                b.last_write().cmp(&a.last_write()).then_with(|| a.name.cmp(&b.name))
            }),
            Sort::Name => kids.sort_unstable_by(|a, b| {
                self.nodes[*a as usize].name.cmp(&self.nodes[*b as usize].name)
            }),
        }
        self.nodes[id as usize].children = kids;
    }

    /// The largest child total, for scaling a row's size bar against its
    /// siblings rather than against the root (which would flatten every level
    /// below the first into an invisible sliver).
    pub fn max_child_size(&self, id: NodeId, apparent: bool) -> u64 {
        self.node(id)
            .children
            .iter()
            .map(|c| self.size(*c, apparent))
            .max()
            .unwrap_or(0)
    }

    /// What rebuilds this entry, when it is a build directory we can name a
    /// command for. The siblings come from the arena rather than a fresh
    /// `readdir`: the scan already read that directory, and this is called on
    /// whatever the cursor is sitting on.
    pub fn rebuild_command(&self, id: NodeId) -> Option<&'static str> {
        let parent = self.node(id).parent?;
        let siblings: std::collections::HashSet<&str> = self
            .node(parent)
            .children
            .iter()
            .map(|c| self.node(*c).name.as_ref())
            .collect();
        presets::rebuild_command(&self.node(id).name, &siblings)
    }

    /// The subtree size to report: allocated blocks, or `st_size` under
    /// `--apparent`. Everything that shows or ranks a size goes through here,
    /// so the flag cannot end up honoured in one pane and ignored in another.
    pub fn size(&self, id: NodeId, apparent: bool) -> u64 {
        let n = self.node(id);
        if apparent { n.total_len } else { n.total_bytes }
    }

    /// This entry's own contribution, on the same metric.
    pub fn self_size(&self, id: NodeId, apparent: bool) -> u64 {
        let n = self.node(id);
        if apparent { n.self_len } else { n.self_bytes }
    }

    fn root_name(root_path: &Path) -> Box<str> {
        match root_path.file_name() {
            Some(n) => n.to_string_lossy().into_owned().into_boxed_str(),
            // `/` and `C:` style roots have no final component.
            None => root_path.to_string_lossy().into_owned().into_boxed_str(),
        }
    }

    /// Sort every node's children largest-first. Called once for `--json`; the
    /// TUI sorts lazily, only what is visible.
    pub fn sort_all_by_size(&mut self, apparent: bool) {
        for i in 0..self.nodes.len() {
            let mut kids = std::mem::take(&mut self.nodes[i].children);
            kids.sort_unstable_by(|a, b| {
                let (sa, sb) = (self.size(*a, apparent), self.size(*b, apparent));
                let (a, b) = (&self.nodes[*a as usize], &self.nodes[*b as usize]);
                sb.cmp(&sa).then_with(|| a.name.cmp(&b.name))
            });
            self.nodes[i].children = kids;
        }
    }
}

// ------------------------------------------------------------------ snapshots

/// The on-disk shape of a tree.
///
/// Not `Node` with a derive on it: a tree of two and a half million nodes holds
/// as many boxed names and child vectors, and rebuilding those one at a time
/// costs well over a second — long enough that reading the snapshot competes
/// with the live scan instead of helping it. Struct-of-arrays turns that into a
/// handful of large allocations and a memcpy.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    root_path: PathBuf,
    /// Every name concatenated; `name_len` slices it back apart in node order.
    names: String,
    name_len: Vec<u32>,
    /// `u32::MAX` stands in for "no parent".
    parent: Vec<u32>,
    child_len: Vec<u32>,
    child_ids: Vec<u32>,
    self_bytes: Vec<u64>,
    total_bytes: Vec<u64>,
    self_len: Vec<u64>,
    total_len: Vec<u64>,
    file_count: Vec<u64>,
    dir_count: Vec<u64>,
    mtime: Vec<i64>,
    newest_file_mtime: Vec<i64>,
    kind: Vec<u8>,
    flags: Vec<u8>,
    preset: Vec<u8>,
    unreadable_count: u64,
    reclaimable: Vec<u32>,
    skipped: Vec<(u32, u8)>,
}

const NO_PARENT: u32 = u32::MAX;

fn kind_to_u8(k: Kind) -> u8 {
    match k {
        Kind::Dir => 0,
        Kind::File => 1,
        Kind::Symlink => 2,
        Kind::Other => 3,
    }
}

fn kind_from_u8(v: u8) -> Kind {
    match v {
        0 => Kind::Dir,
        1 => Kind::File,
        2 => Kind::Symlink,
        _ => Kind::Other,
    }
}

fn preset_to_u8(c: Option<Category>) -> u8 {
    match c {
        None => 0,
        Some(Category::VmImage) => 1,
        Some(Category::BuildArtifact) => 2,
        Some(Category::PackageCache) => 3,
        Some(Category::AppCache) => 4,
    }
}

fn preset_from_u8(v: u8) -> Option<Category> {
    match v {
        1 => Some(Category::VmImage),
        2 => Some(Category::BuildArtifact),
        3 => Some(Category::PackageCache),
        4 => Some(Category::AppCache),
        _ => None,
    }
}

fn skip_to_u8(s: Skip) -> u8 {
    match s {
        Skip::OtherDevice => 0,
        Skip::CloudStorage => 1,
        Skip::UnrepresentableName => 2,
    }
}

fn skip_from_u8(v: u8) -> Skip {
    match v {
        1 => Skip::CloudStorage,
        2 => Skip::UnrepresentableName,
        _ => Skip::OtherDevice,
    }
}

impl Tree {
    pub fn to_snapshot(&self) -> Snapshot {
        let n = self.nodes.len();
        let mut snap = Snapshot {
            root_path: self.root_path.clone(),
            names: String::with_capacity(n * 12),
            name_len: Vec::with_capacity(n),
            parent: Vec::with_capacity(n),
            child_len: Vec::with_capacity(n),
            child_ids: Vec::with_capacity(n),
            self_bytes: Vec::with_capacity(n),
            total_bytes: Vec::with_capacity(n),
            self_len: Vec::with_capacity(n),
            total_len: Vec::with_capacity(n),
            file_count: Vec::with_capacity(n),
            dir_count: Vec::with_capacity(n),
            mtime: Vec::with_capacity(n),
            newest_file_mtime: Vec::with_capacity(n),
            kind: Vec::with_capacity(n),
            flags: Vec::with_capacity(n),
            preset: Vec::with_capacity(n),
            unreadable_count: self.unreadable_count,
            reclaimable: self.reclaimable.clone(),
            skipped: self.skipped.iter().map(|(id, s)| (*id, skip_to_u8(*s))).collect(),
        };
        for node in &self.nodes {
            snap.names.push_str(&node.name);
            snap.name_len.push(node.name.len() as u32);
            snap.parent.push(node.parent.unwrap_or(NO_PARENT));
            snap.child_len.push(node.children.len() as u32);
            snap.child_ids.extend_from_slice(&node.children);
            snap.self_bytes.push(node.self_bytes);
            snap.total_bytes.push(node.total_bytes);
            snap.self_len.push(node.self_len);
            snap.total_len.push(node.total_len);
            snap.file_count.push(node.file_count);
            snap.dir_count.push(node.dir_count);
            snap.mtime.push(node.mtime);
            snap.newest_file_mtime.push(node.newest_file_mtime);
            snap.kind.push(kind_to_u8(node.kind));
            snap.flags.push(node.flags);
            snap.preset.push(preset_to_u8(node.preset));
        }
        snap
    }

    /// Rebuild a tree from a snapshot, rejecting anything internally
    /// inconsistent rather than indexing off the end of an array later.
    pub fn from_snapshot(s: Snapshot) -> Option<Tree> {
        let n = s.name_len.len();
        if n == 0
            || s.parent.len() != n
            || s.child_len.len() != n
            || s.self_bytes.len() != n
            || s.total_bytes.len() != n
            || s.self_len.len() != n
            || s.total_len.len() != n
            || s.file_count.len() != n
            || s.dir_count.len() != n
            || s.mtime.len() != n
            || s.newest_file_mtime.len() != n
            || s.kind.len() != n
            || s.flags.len() != n
            || s.preset.len() != n
        {
            return None;
        }

        let mut nodes = Vec::with_capacity(n);
        let mut name_at = 0usize;
        let mut child_at = 0usize;
        for i in 0..n {
            let len = s.name_len[i] as usize;
            let end = name_at.checked_add(len)?;
            let name = s.names.get(name_at..end)?;
            name_at = end;

            let clen = s.child_len[i] as usize;
            let cend = child_at.checked_add(clen)?;
            let children = s.child_ids.get(child_at..cend)?.to_vec();
            if children.iter().any(|c| *c as usize >= n) {
                return None;
            }
            child_at = cend;

            let parent = match s.parent[i] {
                NO_PARENT => None,
                p if (p as usize) < n => Some(p),
                _ => return None,
            };

            nodes.push(Node {
                name: name.into(),
                parent,
                children,
                self_bytes: s.self_bytes[i],
                total_bytes: s.total_bytes[i],
                self_len: s.self_len[i],
                total_len: s.total_len[i],
                file_count: s.file_count[i],
                dir_count: s.dir_count[i],
                mtime: s.mtime[i],
                newest_file_mtime: s.newest_file_mtime[i],
                kind: kind_from_u8(s.kind[i]),
                flags: s.flags[i],
                preset: preset_from_u8(s.preset[i]),
            });
        }

        Some(Tree {
            nodes,
            root: 0,
            root_path: s.root_path,
            by_scan_id: HashMap::new(),
            orphans: Vec::new(),
            links: HashMap::new(),
            unreadable_count: s.unreadable_count,
            reclaimable: s.reclaimable.into_iter().filter(|i| (*i as usize) < n).collect(),
            skipped: s
                .skipped
                .into_iter()
                .filter(|(i, _)| (*i as usize) < n)
                .map(|(i, v)| (i, skip_from_u8(v)))
                .collect(),
        })
    }
}
