//! The size tree: a flat arena of nodes linked by index.
//!
//! Nodes are never moved or removed during a scan, so indices stay valid and the
//! UI can hold onto a selection while batches keep landing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::presets::{self, Category};
use crate::scan::meta::{Kind, Meta};
use crate::scan::walk::{Batch, ROOT_ID, ScanId, ScanOpts, Skip};

pub type NodeId = u32;

/// What fixes a directory fad was refused. macOS gates whole areas of a home
/// directory behind a privacy grant that has nothing to do with Unix modes;
/// elsewhere a refusal is the modes, and nothing fad can suggest overrides them.
#[cfg(target_os = "macos")]
pub const PERMISSION_FIX: &str = "grant Full Disk Access to your terminal";
#[cfg(not(target_os = "macos"))]
pub const PERMISSION_FIX: &str = "fad has no permission to read them";

/// A reason a directory could not be read, worded for the person reading it.
pub fn unreadable_reason(kind: std::io::ErrorKind) -> String {
    match kind {
        std::io::ErrorKind::PermissionDenied => format!("permission denied \u{2014} {PERMISSION_FIX}"),
        std::io::ErrorKind::InvalidFilename => "path too long to open".to_string(),
        other => other.to_string(),
    }
}

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

/// The links to one inode that the tree holds, and which of them carries its
/// bytes.
#[derive(Debug)]
struct LinkSet {
    /// `st_nlink` when it was scanned: how many names the file has on disk,
    /// inside the tree or not.
    nlink: u64,
    /// The live links in the tree. Removed ones are taken out.
    members: Vec<NodeId>,
    /// The one counted at full size; every other member is a dupe at zero.
    winner: NodeId,
    /// Links the session has removed, so the last one out can tell whether a
    /// name outside the tree is still holding the data.
    removed: u64,
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
    /// Every multiply-linked inode in the tree, keyed by `(dev, ino)`. Only
    /// files with `nlink > 1` ever land here, so this stays tiny on a normal
    /// filesystem.
    links: HashMap<(u64, u64), LinkSet>,
    /// The other direction, for `remove`: which inode a linked node is.
    link_of: HashMap<NodeId, (u64, u64)>,
    pub unreadable_count: u64,
    /// Why each unreadable directory was, in the order they were found. The
    /// remedy depends on it: Full Disk Access fixes a permission refusal and
    /// does nothing for a path too long to open.
    pub unreadable_why: Vec<(NodeId, std::io::ErrorKind)>,
    /// Every node matching a reclaimable preset, for the `r` view.
    pub reclaimable: Vec<NodeId>,
    /// Directories we stopped at, so the UI can say so rather than quietly
    /// under-reporting. Paths are kept for the "rescan including these" action.
    pub skipped: Vec<(NodeId, Skip)>,
    /// The options the walk ran with. A snapshot's numbers mean something
    /// different with and without `--cross-device` or `--cloud`, so it is
    /// only comparable to a scan that used the same ones.
    scan_opts: ScanOpts,
    /// When the walk that built this tree finished, if it has.
    completed_at: Option<std::time::SystemTime>,
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
            link_of: HashMap::new(),
            unreadable_count: 0,
            unreadable_why: Vec::new(),
            reclaimable: Vec::new(),
            skipped: Vec::new(),
            scan_opts: ScanOpts::default(),
            completed_at: None,
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

    pub fn scan_opts(&self) -> &ScanOpts {
        &self.scan_opts
    }

    pub fn set_scan_opts(&mut self, opts: ScanOpts) {
        self.scan_opts = opts;
    }

    /// When the walk finished. This, not when the snapshot happened to be
    /// written, is what "since" measures from: the TUI saves on the way out,
    /// and a session left open all afternoon would otherwise make its
    /// baseline look hours newer than the numbers in it.
    pub fn completed_at(&self) -> Option<std::time::SystemTime> {
        self.completed_at
    }

    pub fn mark_complete(&mut self, at: std::time::SystemTime) {
        self.completed_at = Some(at);
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

        if let Some(kind) = batch.unreadable {
            self.nodes[parent as usize].flags |= flags::UNREADABLE;
            self.unreadable_count += 1;
            self.unreadable_why.push((parent, kind));
            // With entries, the directory listed and only some of it would not
            // stat: what did is counted, and the flag says it is not all.
            if batch.entries.is_empty() {
                self.nodes[parent as usize].flags |= flags::SCANNED;
                return;
            }
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
        let parent_path = self.path(parent);
        let presets: Vec<Option<Category>> = batch
            .entries
            .iter()
            .map(|e| {
                presets::classify(
                    &e.name,
                    e.meta.is_dir(),
                    e.meta.len,
                    &parent_name,
                    &parent_path,
                    &siblings,
                )
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
                // Entered by another path already (see `Entry::descend`): there
                // is no batch coming for it, so it must not read as pending.
                None if e.meta.is_dir() && e.descend.is_none() => f |= flags::SCANNED,
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
                hardlinks.push((id, e.meta.dev, e.meta.ino, e.meta.nlink));
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
        for (id, dev, ino, nlink) in hardlinks {
            self.resolve_hardlink(id, dev, ino, nlink);
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
    fn resolve_hardlink(&mut self, id: NodeId, dev: u64, ino: u64, nlink: u64) {
        self.link_of.insert(id, (dev, ino));
        let Some(prev) = self.links.get(&(dev, ino)).map(|s| s.winner) else {
            self.links.insert(
                (dev, ino),
                LinkSet { nlink, members: vec![id], winner: id, removed: 0 },
            );
            return;
        };
        let id_wins = self.path(id) < self.path(prev);
        let set = self.links.get_mut(&(dev, ino)).expect("looked up above");
        set.members.push(id);
        let loser = if id_wins {
            set.winner = id;
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
    ///
    /// Returns the bytes actually freed, or None if it was already gone. That
    /// is the subtree's total less whatever another name still holds: a file
    /// counted here whose hard link survives elsewhere in the tree frees
    /// nothing, and its bytes move over to that link rather than vanishing
    /// from the totals — they are still on disk, and the surviving link is
    /// now the only path that reaches them.
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

        // Every node under it goes too, not just the one at the top. Left
        // unmarked, they are detached from the tree but still in the arena
        // looking alive: everything that walks the arena by index — the
        // omissions screen, the snapshot writer — would go on reporting files
        // that are in the trash.
        let mut gone = Vec::new();
        let mut stack = vec![id];
        while let Some(x) = stack.pop() {
            self.nodes[x as usize].flags |= flags::DELETED;
            stack.extend_from_slice(&self.nodes[x as usize].children);
            gone.push(x);
        }
        self.nodes[parent as usize].children.retain(|c| *c != id);
        self.roll_up(parent, -(bytes as i64), -(len as i64), -(files as i64), -(dirs as i64));
        // `roll_up` walks sizes back; the newest-write rollup cannot be walked
        // back the same way, because a maximum does not subtract. Delete the one
        // recent file in an archive and every directory above it would go on
        // reporting itself as freshly written — wrong in the detail pane, in the
        // modified sort, and in the age filter, which would keep hiding a branch
        // that is now exactly what it claims to be looking for.
        self.recompute_newest(parent);

        let mut kept = 0u64;
        if !self.link_of.is_empty() {
            for x in gone {
                kept += self.unlink(x);
            }
        }
        Some(bytes.saturating_sub(kept))
    }

    /// Take a removed node out of its hard-link set, if it is in one. Returns
    /// the bytes that stay on disk because another name still holds them.
    ///
    /// Only the winner carries bytes, so only its going needs handling. If
    /// another link survives in the tree, it takes the bytes over — the same
    /// path the scan would have chosen had the winner never existed — and its
    /// ancestors grow by exactly what the removed subtree's ancestors lost. If
    /// none does but the file had more names than the tree ever saw, one of
    /// them is outside the scan root and the data is still there.
    fn unlink(&mut self, x: NodeId) -> u64 {
        let Some(key) = self.link_of.remove(&x) else { return 0 };
        let Some(set) = self.links.get_mut(&key) else { return 0 };
        set.members.retain(|m| *m != x);
        set.removed += 1;
        if set.winner != x {
            return 0;
        }
        let (bytes, len) = (self.nodes[x as usize].self_bytes, self.nodes[x as usize].self_len);

        // Members still in the set may be inside the subtree being removed and
        // not yet unlinked; they are on their way out and cannot inherit.
        let members = set.members.clone();
        let survivor = members
            .into_iter()
            .filter(|m| self.nodes[*m as usize].flags & flags::DELETED == 0)
            .min_by_key(|m| self.path(*m));
        let set = self.links.get_mut(&key).expect("looked up above");
        match survivor {
            Some(s) => {
                set.winner = s;
                let n = &mut self.nodes[s as usize];
                n.self_bytes = bytes;
                n.self_len = len;
                n.flags &= !flags::HARDLINK_DUPE;
                self.roll_up(s, bytes as i64, len as i64, 0, 0);
                bytes
            }
            None => {
                // Nothing left in the tree to carry it. The set stays only while
                // removed-but-unprocessed members remain in it.
                let outside = set.nlink > set.removed + set.members.len() as u64;
                if set.members.is_empty() {
                    self.links.remove(&key);
                }
                if outside { bytes } else { 0 }
            }
        }
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
        presets::rebuild_command(&self.node(id).name, &self.path(parent), &siblings)
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

    /// What would get the unreadable directories counted, for the line that
    /// reports them. Full Disk Access is the answer to a permission refusal
    /// and to nothing else: telling someone to grant it for a path too long
    /// to open sends them to System Settings for a problem that is not there.
    pub fn unreadable_fix(&self) -> &'static str {
        let denied = self.unreadable_why.is_empty()
            || self.unreadable_why.iter().any(|(_, k)| *k == std::io::ErrorKind::PermissionDenied);
        if denied { PERMISSION_FIX } else { "not a permissions problem \u{2014} see `!` for why" }
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
    /// Raw bytes rather than `PathBuf`: serde writes a path as a string and
    /// refuses one that is not UTF-8, which on Linux a scan root can be.
    root_path: Vec<u8>,
    cross_device: bool,
    cloud: bool,
    /// When the walk finished, as seconds and nanoseconds since the epoch.
    completed_at: (u64, u32),
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
    unreadable_why: Vec<(u32, u8)>,
    /// `(node, dev, ino, nlink)` for every multiply-linked file, so a tree
    /// loaded from disk still knows which links share storage when one of
    /// them is removed.
    links: Vec<(u32, u64, u64, u64)>,
}

const NO_PARENT: u32 = u32::MAX;

/// One path component, and nothing that could make a joined path go
/// anywhere else.
fn is_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

fn why_to_u8(k: std::io::ErrorKind) -> u8 {
    match k {
        std::io::ErrorKind::PermissionDenied => 1,
        std::io::ErrorKind::InvalidFilename => 2,
        _ => 0,
    }
}

fn why_from_u8(v: u8) -> std::io::ErrorKind {
    match v {
        1 => std::io::ErrorKind::PermissionDenied,
        2 => std::io::ErrorKind::InvalidFilename,
        _ => std::io::ErrorKind::Other,
    }
}

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
        // Deleted nodes stay in the arena so live indices keep meaning what
        // they meant; a snapshot has no live indices to protect, and writing
        // them out would bring back, on the next launch, everything this
        // session deleted. Renumber the survivors densely, in order — order
        // is what keeps every parent ahead of its children.
        let mut remap = vec![NO_PARENT; self.nodes.len()];
        let mut n = 0usize;
        for (i, node) in self.nodes.iter().enumerate() {
            if node.flags & flags::DELETED == 0 {
                remap[i] = n as u32;
                n += 1;
            }
        }
        let live = |id: &NodeId| remap[*id as usize] != NO_PARENT;
        use std::os::unix::ffi::OsStrExt;
        let done = self
            .completed_at
            .unwrap_or_else(std::time::SystemTime::now)
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let mut snap = Snapshot {
            root_path: self.root_path.as_os_str().as_bytes().to_vec(),
            cross_device: self.scan_opts.cross_device,
            cloud: self.scan_opts.cloud,
            completed_at: (done.as_secs(), done.subsec_nanos()),
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
            reclaimable: self
                .reclaimable
                .iter()
                .filter(|id| live(id))
                .map(|id| remap[*id as usize])
                .collect(),
            skipped: self
                .skipped
                .iter()
                .filter(|(id, _)| live(id))
                .map(|(id, s)| (remap[*id as usize], skip_to_u8(*s)))
                .collect(),
            unreadable_why: self
                .unreadable_why
                .iter()
                .filter(|(id, _)| live(id))
                .map(|(id, k)| (remap[*id as usize], why_to_u8(*k)))
                .collect(),
            links: self
                .link_of
                .iter()
                .filter(|(id, _)| live(id))
                .filter_map(|(id, key)| {
                    let set = self.links.get(key)?;
                    Some((remap[*id as usize], key.0, key.1, set.nlink))
                })
                .collect(),
        };
        for node in self.nodes.iter().filter(|n| n.flags & flags::DELETED == 0) {
            snap.names.push_str(&node.name);
            snap.name_len.push(node.name.len() as u32);
            snap.parent.push(node.parent.map_or(NO_PARENT, |p| remap[p as usize]));
            let kids = node.children.iter().filter(|c| live(c));
            snap.child_len.push(kids.clone().count() as u32);
            snap.child_ids.extend(kids.map(|c| remap[*c as usize]));
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
    ///
    /// In-bounds is not enough. A parent cycle sends `path` and `roll_up`
    /// round it forever; a node listed under two parents gets its size taken
    /// out twice when deleted; and a name of `..` or `a/b` rebuilds a path
    /// that leaves the directory it claims to be in — which is the path a
    /// delete would be aimed at. So the shape the scan always produces is
    /// required exactly: node 0 is the only root, every parent comes before
    /// its children, each child is listed once and only by its own parent,
    /// and every name is a single path component.
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
        let mut listed = vec![false; n];
        for i in 0..n {
            let len = s.name_len[i] as usize;
            let end = name_at.checked_add(len)?;
            let name = s.names.get(name_at..end)?;
            name_at = end;
            // The root's name is display only (it is `/` for `/`); every other
            // name is joined onto a path.
            if i > 0 && !is_component(name) {
                return None;
            }

            let clen = s.child_len[i] as usize;
            let cend = child_at.checked_add(clen)?;
            let children = s.child_ids.get(child_at..cend)?.to_vec();
            for c in &children {
                let c = *c as usize;
                if c <= i || c >= n || s.parent[c] as usize != i || listed[c] {
                    return None;
                }
                listed[c] = true;
            }
            child_at = cend;

            let parent = match (i, s.parent[i]) {
                (0, NO_PARENT) => None,
                (i, p) if i > 0 && (p as usize) < i => Some(p),
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

        // Rebuilt from the members: the one not marked as a copy is the one
        // carrying the bytes.
        let mut links: HashMap<(u64, u64), LinkSet> = HashMap::new();
        let mut link_of = HashMap::new();
        for (id, dev, ino, nlink) in s.links {
            if id as usize >= n || id == 0 {
                return None;
            }
            link_of.insert(id, (dev, ino));
            let set = links.entry((dev, ino)).or_insert(LinkSet {
                nlink,
                members: Vec::new(),
                winner: id,
                removed: 0,
            });
            set.members.push(id);
            if nodes[id as usize].flags & flags::HARDLINK_DUPE == 0 {
                set.winner = id;
            }
        }

        use std::os::unix::ffi::OsStringExt;
        let root_path = PathBuf::from(std::ffi::OsString::from_vec(s.root_path));
        if !root_path.is_absolute() {
            return None;
        }
        let completed_at = std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::new(s.completed_at.0, s.completed_at.1.min(999_999_999)))?;
        Some(Tree {
            nodes,
            root: 0,
            root_path,
            by_scan_id: HashMap::new(),
            orphans: Vec::new(),
            links,
            link_of,
            unreadable_count: s.unreadable_count,
            unreadable_why: s
                .unreadable_why
                .into_iter()
                .filter(|(i, _)| (*i as usize) < n)
                .map(|(i, v)| (i, why_from_u8(v)))
                .collect(),
            scan_opts: ScanOpts { cross_device: s.cross_device, cloud: s.cloud },
            completed_at: Some(completed_at),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::walk::Entry;

    fn meta(kind: Kind) -> Meta {
        Meta { blocks: 4096, len: 4096, mtime: 0, dev: 1, ino: 0, nlink: 1, kind }
    }

    /// root ─ a ─ f, and root ─ g.
    fn snapshot() -> Snapshot {
        let mut t = Tree::new(PathBuf::from("/r"), &meta(Kind::Dir));
        let e = |name: &str, kind, descend| Entry { name: name.into(), meta: meta(kind), descend, skip: None };
        t.apply(Batch {
            parent: ROOT_ID,
            entries: vec![e("a", Kind::Dir, Some(1)), e("g", Kind::File, None)],
            unreadable: None,
        });
        t.apply(Batch { parent: 1, entries: vec![e("f", Kind::File, None)], unreadable: None });
        t.to_snapshot()
    }

    fn rename(s: &mut Snapshot, i: usize, to: &str) {
        let mut at = 0;
        for l in &s.name_len[..i] {
            at += *l as usize;
        }
        let len = s.name_len[i] as usize;
        s.names.replace_range(at..at + len, to);
        s.name_len[i] = to.len() as u32;
    }

    #[test]
    fn a_sound_snapshot_loads() {
        let t = Tree::from_snapshot(snapshot()).unwrap();
        assert!(t.find_path(Path::new("/r/a/f")).is_some());
    }

    #[test]
    fn a_parent_cycle_is_rejected() {
        let mut s = snapshot();
        // a's parent is f, f's parent is a.
        s.parent[1] = 3;
        assert!(Tree::from_snapshot(s).is_none());
    }

    #[test]
    fn a_second_root_is_rejected() {
        let mut s = snapshot();
        s.parent[2] = NO_PARENT;
        assert!(Tree::from_snapshot(s).is_none());
        let mut s = snapshot();
        s.parent[0] = 1;
        assert!(Tree::from_snapshot(s).is_none());
    }

    #[test]
    fn a_child_listed_by_the_wrong_parent_or_twice_is_rejected() {
        let mut s = snapshot();
        // Root lists f, whose parent is a.
        let at = s.child_ids.iter().position(|c| *c == 2).unwrap();
        s.child_ids[at] = 3;
        assert!(Tree::from_snapshot(s).is_none());

        let mut s = snapshot();
        let at = s.child_ids.iter().position(|c| *c == 2).unwrap();
        s.child_ids[at] = 1;
        assert!(Tree::from_snapshot(s).is_none(), "a listed twice");
    }

    #[test]
    fn a_name_that_is_not_one_component_is_rejected() {
        for bad in ["", ".", "..", "x/y", "nul\0"] {
            let mut s = snapshot();
            rename(&mut s, 3, bad);
            assert!(Tree::from_snapshot(s).is_none(), "accepted {bad:?}");
        }
        let mut s = snapshot();
        rename(&mut s, 3, "fine name");
        assert!(Tree::from_snapshot(s).is_some());
    }

    #[test]
    fn a_relative_root_is_rejected() {
        let mut s = snapshot();
        s.root_path = b"relative".to_vec();
        assert!(Tree::from_snapshot(s).is_none());
    }
}
