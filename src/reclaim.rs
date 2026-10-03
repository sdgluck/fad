//! Choosing what to reclaim, without a person watching.
//!
//! The interactive views and the scripted `--reclaim --yes` have to agree about
//! what counts, or the flag would delete things the UI never offered. Both go
//! through here.

use crate::ignore::Rules;
use crate::tree::{NodeId, Tree, flags};

/// A `node_modules` inside a `node_modules` is already covered by its ancestor.
/// Listing both would double the headline total and stage the same bytes twice.
pub fn has_reclaimable_ancestor(tree: &Tree, id: NodeId) -> bool {
    let mut cur = tree.node(id).parent;
    while let Some(p) = cur {
        if tree.node(p).preset.is_some() {
            return true;
        }
        cur = tree.node(p).parent;
    }
    false
}

/// Flags that disqualify a node outright. Another filesystem or a cloud
/// folder is not what the rule that matched it was about; an unreadable one
/// was never seen inside; an unnamed one has no real path to delete by; and a
/// deleted one is gone.
const UNFIT: flags::Flags =
    flags::OTHER_DEVICE | flags::CLOUD | flags::UNREADABLE | flags::UNNAMED | flags::DELETED;

/// Flags that disqualify a node when anything *below* it carries them: the
/// delete would reach into another filesystem or a cloud folder, or into a
/// directory the scan could not read and so cannot vouch for.
const UNFIT_INSIDE: flags::Flags = flags::OTHER_DEVICE | flags::CLOUD | flags::UNREADABLE;

/// Is this node, its position, and everything under it fit to hand to an
/// unattended delete?
fn fit(tree: &Tree, id: NodeId) -> bool {
    if tree.node(id).flags & UNFIT != 0 {
        return false;
    }
    // A node whose ancestor was deleted this session keeps its own flags
    // clean — only the detached node is marked — but it is gone just the same.
    let mut cur = tree.node(id).parent;
    while let Some(p) = cur {
        if tree.node(p).flags & flags::DELETED != 0 {
            return false;
        }
        cur = tree.node(p).parent;
    }
    let mut stack = tree.node(id).children.clone();
    while let Some(c) = stack.pop() {
        let n = tree.node(c);
        if n.flags & UNFIT_INSIDE != 0 {
            return false;
        }
        stack.extend_from_slice(&n.children);
    }
    true
}

/// Every reclaimable entry worth offering, largest first: nothing nested inside
/// another, nothing below `min_size`, nothing on the ignore list, nothing the
/// delete guard would refuse, and nothing that is — or holds — another
/// filesystem, a cloud folder, an unreadable directory, an unrepresentable
/// name, or something already deleted.
pub fn candidates(
    tree: &Tree,
    apparent: bool,
    min_size: u64,
    ignore: &Rules,
) -> Vec<(NodeId, u64)> {
    let root = tree.root_path();
    let mut items: Vec<(NodeId, u64)> = tree
        .reclaimable
        .iter()
        .copied()
        .filter(|id| !has_reclaimable_ancestor(tree, *id))
        .filter(|id| fit(tree, *id))
        .filter(|id| {
            let n = tree.node(*id);
            !ignore.matches(&tree.path(*id), &n.name, n.is_dir())
        })
        .filter(|id| crate::delete::guard(&tree.path(*id), root).is_ok())
        .map(|id| (id, tree.size(id, apparent)))
        .filter(|(_, bytes)| *bytes >= min_size)
        .collect();
    items.sort_unstable_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
    items
}

/// What `--reclaim --yes` may take with nobody watching: `candidates`, less
/// every category that has to be chosen by hand (`Category::manual_only`).
pub fn auto_candidates(
    tree: &Tree,
    apparent: bool,
    min_size: u64,
    ignore: &Rules,
) -> Vec<(NodeId, u64)> {
    candidates(tree, apparent, min_size, ignore)
        .into_iter()
        .filter(|(id, _)| !tree.node(*id).preset.is_some_and(|c| c.manual_only()))
        .collect()
}

/// Take from `items` until `max` is reached, largest first.
///
/// Anything that would take the batch over the cap is skipped rather than
/// ending the selection: stopping at the first overshoot would leave a 2G cap
/// unable to reclaim anything at all when the largest candidate is 3G.
pub fn under_cap(items: Vec<(NodeId, u64)>, max: Option<u64>) -> Vec<(NodeId, u64)> {
    let Some(max) = max else { return items };
    let mut total = 0u64;
    items
        .into_iter()
        .filter(|(_, bytes)| {
            if total + bytes > max {
                return false;
            }
            total += bytes;
            true
        })
        .collect()
}
