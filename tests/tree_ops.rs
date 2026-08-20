//! Removing a node has to take its weight back out of every ancestor. Getting
//! this wrong shows up as sizes that drift after each delete, which is exactly
//! the kind of quiet wrongness that makes a disk tool untrustworthy.

use std::path::Path;

use fad::scan::Scan;
use fad::scan::walk::ScanOpts;
use fad::tree::{NodeId, Tree};

fn scan(root: &Path) -> Tree {
    let (mut tree, scan) = Scan::start(root, ScanOpts::default()).unwrap();
    scan.finish(&mut tree);
    tree
}

fn find(tree: &Tree, rel: &str) -> NodeId {
    tree.find_path(&tree.root_path().join(rel)).unwrap_or_else(|| panic!("no node for {rel}"))
}

fn fixture(root: &Path) {
    let mk = |rel: &str, size: usize| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; size]).unwrap();
    };
    mk("a/b/c/deep.bin", 4 * 1024 * 1024);
    mk("a/b/mid.bin", 2 * 1024 * 1024);
    mk("a/top.bin", 1024 * 1024);
    mk("other.bin", 512 * 1024);
}

#[test]
fn removing_a_subtree_updates_every_ancestor() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut tree = scan(dir.path());

    let root = tree.root();
    let a = find(&tree, "a");
    let b = find(&tree, "a/b");
    let c = find(&tree, "a/b/c");

    let root_before = tree.node(root).total_bytes;
    let a_before = tree.node(a).total_bytes;
    let b_before = tree.node(b).total_bytes;
    let c_bytes = tree.node(c).total_bytes;
    let root_files_before = tree.node(root).file_count;
    let root_dirs_before = tree.node(root).dir_count;

    let freed = tree.remove(c).expect("remove returned nothing");
    assert_eq!(freed, c_bytes);

    assert_eq!(tree.node(root).total_bytes, root_before - c_bytes);
    assert_eq!(tree.node(a).total_bytes, a_before - c_bytes);
    assert_eq!(tree.node(b).total_bytes, b_before - c_bytes);

    // `c` held one file and was itself one directory.
    assert_eq!(tree.node(root).file_count, root_files_before - 1);
    assert_eq!(tree.node(root).dir_count, root_dirs_before - 1);

    assert!(!tree.node(b).children.contains(&c), "still listed as a child");
    assert!(tree.find_path(&tree.root_path().join("a/b/c")).is_none());
}

#[test]
fn removing_everything_leaves_the_root_at_its_own_size() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut tree = scan(dir.path());

    let root = tree.root();
    let own = tree.node(root).self_bytes;
    for child in tree.node(root).children.clone() {
        tree.remove(child);
    }
    assert_eq!(tree.node(root).total_bytes, own, "sizes did not fully unwind");
    assert_eq!(tree.node(root).file_count, 0);
    assert_eq!(tree.node(root).dir_count, 0);
}

#[test]
fn the_root_itself_cannot_be_removed() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut tree = scan(dir.path());
    let root = tree.root();
    assert!(tree.remove(root).is_none());
    assert!(tree.node(root).total_bytes > 0);
}
