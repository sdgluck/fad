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

/// Every reclaimable entry worth offering, largest first: nothing nested inside
/// another, nothing below `min_size`, nothing on the ignore list, and nothing
/// the delete guard would refuse.
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
        .filter(|id| tree.node(*id).flags & flags::DELETED == 0)
        .filter(|id| !has_reclaimable_ancestor(tree, *id))
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
