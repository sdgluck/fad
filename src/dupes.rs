//! Files that exist more than once.
//!
//! Three passes, each one cheaper than the one it saves. Files are grouped by
//! exact size first, which costs nothing because the scan already knows every
//! size. Groups of two or more are then fingerprinted from their first and last
//! 64K, which is one seek and two reads. Only what survives that is read in
//! full.
//!
//! A group is reported only once every member has been hashed end to end. A
//! same-size, same-fingerprint pair is a *likely* duplicate, and offering to
//! delete one of them on that basis is how a tool like this destroys someone's
//! work. When the read budget runs out, the unverified groups are dropped and
//! counted, never shown.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::hash::Sha256;
use crate::tree::NodeId;

/// Below this a duplicate is not worth the read, and the count of them would
/// bury the ones that matter.
pub const MIN_SIZE: u64 = 1 << 20;

/// How much of each end the fingerprint pass reads.
const EDGE: u64 = 64 << 10;

/// Total bytes the verifying pass will read before giving up. Roughly a few
/// seconds on an SSD; a home directory with more duplicate mass than this has
/// bigger news to deliver first.
const READ_BUDGET: u64 = 8 << 30;

/// Which file a path named, and what state it was in, at the moment its
/// contents were read.
///
/// A hash proves two files were identical *when they were read*. Anything that
/// acts on that proof later — `clone::share` replacing one with a clone of the
/// other — has to be able to tell whether either file has moved on since, and
/// the length alone cannot: an in-place edit that keeps the size is the most
/// ordinary kind there is. So the whole identity is kept, to the nanosecond:
///
/// - `dev` and `ino` say it is still the same file, not a new one saved over
///   the old name;
/// - `mtime` catches an ordinary write;
/// - `ctime` catches the write that hid itself. A program can write through an
///   open handle and put the old `mtime` back afterwards — `rsync -t`, `touch
///   -r`, any editor that preserves timestamps — but it cannot set `ctime`,
///   which the kernel moves on every change to the inode, including that
///   `utimes` itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
}

impl Identity {
    /// The identity of the plain file at `path`, without following a symlink.
    /// `None` for anything that is not a plain file, or not there.
    pub fn of(path: &Path) -> Option<Identity> {
        let m = std::fs::symlink_metadata(path).ok()?;
        m.is_file().then(|| Identity::from_meta(&m))
    }

    pub fn from_meta(m: &std::fs::Metadata) -> Identity {
        use std::os::unix::fs::MetadataExt;
        Identity {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_nsec: m.ctime_nsec(),
        }
    }
}

pub struct Group {
    /// Every copy, newest first, so "keep one" has an obvious default.
    pub ids: Vec<NodeId>,
    /// What each of `ids` was when it was hashed, in the same order. This is
    /// what the hash is a statement about, and acting on the hash without it
    /// would be acting on a statement about some earlier file.
    pub identities: Vec<Identity>,
    pub bytes_each: u64,
}

impl Group {
    /// What deleting all but one copy would give back.
    pub fn wasted(&self) -> u64 {
        self.bytes_each * (self.ids.len() as u64 - 1)
    }

    /// What this copy was when its contents were proved to match the others.
    pub fn identity(&self, id: NodeId) -> Option<Identity> {
        let i = self.ids.iter().position(|x| *x == id)?;
        self.identities.get(i).copied()
    }
}

pub struct Report {
    pub groups: Vec<Group>,
    pub bytes_read: u64,
    /// Groups that looked identical but ran out of budget before they could be
    /// proven identical. Not shown, only counted.
    pub unverified: usize,
}

impl Report {
    pub fn wasted(&self) -> u64 {
        self.groups.iter().map(|g| g.wasted()).sum()
    }

    /// Record a group, unless collapsing shared storage has left it with
    /// nothing to reclaim.
    fn push(&mut self, (ids, identities): (Vec<NodeId>, Vec<Identity>), bytes_each: u64) {
        if ids.len() < 2 {
            return;
        }
        self.groups.push(Group { ids, identities, bytes_each });
    }
}

/// One candidate file: its node, where to read it, its size, and its mtime so
/// the newest copy can be the one that stays.
pub struct Candidate {
    pub id: NodeId,
    pub path: PathBuf,
    pub bytes: u64,
    pub mtime: i64,
}

/// A candidate on its way through the passes, with the identity it had before
/// the first byte of it was read.
struct Suspect {
    c: Candidate,
    before: Identity,
}

impl Suspect {
    /// Still the file it was when reading began? Checked after the last read,
    /// so a write that lands while the hash is running cannot produce a group
    /// whose identities describe one version of a file and whose hash describes
    /// another.
    fn unchanged(&self) -> bool {
        Identity::of(&self.c.path) == Some(self.before)
    }
}

pub fn find(candidates: Vec<Candidate>) -> Report {
    let mut by_size: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for c in candidates {
        by_size.entry(c.bytes).or_default().push(c);
    }

    // Same size and same fingerprint. Still only a candidate.
    let mut suspects: Vec<Vec<Suspect>> = Vec::new();
    for group in by_size.into_values() {
        if group.len() < 2 {
            continue;
        }
        let mut by_print: HashMap<[u8; 32], Vec<Suspect>> = HashMap::new();
        for c in group {
            // A length that no longer matches the scan's is a file that has
            // already moved on; there is nothing to prove about it.
            let Some(before) = Identity::of(&c.path).filter(|i| i.len == c.bytes) else {
                continue;
            };
            let Some(print) = fingerprint(&c) else { continue };
            by_print.entry(print).or_default().push(Suspect { c, before });
        }
        for (_, same) in by_print {
            if same.len() >= 2 {
                suspects.push(same);
            }
        }
    }

    // Spend the budget where it buys the most: the biggest piles first.
    suspects.sort_unstable_by_key(|g| {
        std::cmp::Reverse(g[0].c.bytes * (g.len() as u64 - 1))
    });

    let mut report = Report { groups: Vec::new(), bytes_read: 0, unverified: 0 };
    for group in suspects {
        let bytes = group[0].c.bytes;
        // A file no larger than both edges was read end to end by the
        // fingerprint, so its fingerprint *is* its full hash: it is already
        // verified. See `fingerprint`, which reads the tail from one edge up
        // precisely so that this holds.
        if bytes <= EDGE * 2 {
            report.bytes_read += bytes * group.len() as u64;
            let group: Vec<Suspect> = group.into_iter().filter(Suspect::unchanged).collect();
            report.push(newest_first(group), bytes);
            continue;
        }
        let cost = bytes.saturating_mul(group.len() as u64);
        if report.bytes_read.saturating_add(cost) > READ_BUDGET {
            report.unverified += 1;
            continue;
        }
        report.bytes_read += cost;

        let mut by_hash: HashMap<[u8; 32], Vec<Suspect>> = HashMap::new();
        for s in group {
            let Some(h) = full_hash(&s.c) else { continue };
            if !s.unchanged() {
                continue;
            }
            by_hash.entry(h).or_default().push(s);
        }
        for (_, same) in by_hash {
            if same.len() >= 2 {
                report.push(newest_first(same), bytes);
            }
        }
    }

    report.groups.sort_unstable_by_key(|g| std::cmp::Reverse(g.wasted()));
    report
}

/// Newest first, so the copy the user is most likely still using is the one
/// "keep one" keeps — with copies that already share their storage collapsed
/// down to one entry.
///
/// Two files that share their extents cost what one of them costs, so deleting
/// either frees nothing. That is the same reason hard links are left out of the
/// candidate set entirely, and it matters more here than it looks: on APFS
/// `cp` clones, and so does the standard library's own file copy, so a great
/// many byte-identical pairs on a Mac have never cost anything twice. Listing
/// them as reclaimable would be inviting the user to delete a file for no gain.
///
/// A filesystem that will not report extents says nothing either way, and
/// nothing is collapsed on the strength of not knowing.
fn newest_first(mut group: Vec<Suspect>) -> (Vec<NodeId>, Vec<Identity>) {
    group.sort_unstable_by(|a, b| {
        b.c.mtime.cmp(&a.c.mtime).then_with(|| a.c.path.cmp(&b.c.path))
    });
    let mut seen = std::collections::HashSet::new();
    group.retain(|s| match crate::clone::physical_start(&s.c.path) {
        Some(start) => seen.insert(start),
        None => true,
    });
    group.into_iter().map(|s| (s.c.id, s.before)).unzip()
}

/// The first and last 64K, plus the size. Cheap enough to run on every
/// same-size file and selective enough that almost nothing reaches the full
/// read.
///
/// The tail is read for anything past one edge, not past two, so that a file
/// no larger than both edges is covered end to end — the head and the tail
/// overlap in the middle of that range, and re-reading a few kilobytes is the
/// price of `find` being able to treat such a fingerprint as a full hash. Past
/// two edges the two reads are disjoint and the fingerprint is only ever a
/// filter.
fn fingerprint(c: &Candidate) -> Option<[u8; 32]> {
    let mut f = std::fs::File::open(&c.path).ok()?;
    let mut h = Sha256::default();
    h.update(&c.bytes.to_le_bytes());

    let head = EDGE.min(c.bytes) as usize;
    let mut buf = vec![0u8; head];
    f.read_exact(&mut buf).ok()?;
    h.update(&buf);

    if c.bytes > EDGE {
        f.seek(SeekFrom::End(-(EDGE as i64))).ok()?;
        let mut tail = vec![0u8; EDGE as usize];
        f.read_exact(&mut tail).ok()?;
        h.update(&tail);
    }
    Some(h.finish())
}

fn full_hash(c: &Candidate) -> Option<[u8; 32]> {
    let mut f = std::fs::File::open(&c.path).ok()?;
    let mut h = Sha256::default();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            return Some(h.finish());
        }
        h.update(&buf[..n]);
    }
}
