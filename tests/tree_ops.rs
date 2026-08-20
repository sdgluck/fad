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

/// A batch whose parent has not been registered yet is held back, and must be
/// applied — not quietly dropped — once the parent arrives. The walker's
/// ordering means this should not happen, but a whole subtree silently missing
/// from the totals is not a failure mode worth leaving to chance.
#[test]
fn a_batch_that_arrives_before_its_parent_is_still_counted() {
    use fad::scan::meta::{Kind, Meta};
    use fad::scan::walk::{Batch, Entry};

    let meta = |dir: bool, blocks: u64| Meta {
        blocks,
        len: blocks,
        mtime: 0,
        dev: 1,
        ino: 0,
        nlink: 1,
        kind: if dir { Kind::Dir } else { Kind::File },
    };

    let mut tree = Tree::new("/root".into(), &meta(true, 0));

    // The child directory's contents turn up first, naming a scan id the tree
    // has never seen.
    tree.apply(Batch {
        parent: 7,
        entries: vec![Entry {
            name: "big.bin".into(),
            meta: meta(false, 4096),
            descend: None,
            skip: None,
        }],
        unreadable: None,
    });
    assert_eq!(tree.node(tree.root()).total_bytes, 0);

    // Now the parent arrives and claims scan id 7.
    tree.apply(Batch {
        parent: 0,
        entries: vec![Entry {
            name: "sub".into(),
            meta: meta(true, 0),
            descend: Some(7),
            skip: None,
        }],
        unreadable: None,
    });

    assert_eq!(tree.node(tree.root()).total_bytes, 4096, "the held batch was dropped");
    assert_eq!(tree.node(tree.root()).file_count, 1);
    let sub = find(&tree, "sub");
    assert_eq!(tree.node(sub).total_bytes, 4096);
}

/// Presets are matched against the parent directory's name, and the scan root
/// is a parent like any other: `fad ~/.cargo` still has to recognise
/// `registry`, and `fad ~` still has to recognise the caches directly inside it.
#[test]
fn presets_match_directly_under_the_scan_root() {
    let dir = tempfile::tempdir().unwrap();
    let cargo = dir.path().join(".cargo");
    std::fs::create_dir_all(cargo.join("registry/cache")).unwrap();
    std::fs::write(cargo.join("registry/cache/crate.crate"), vec![0u8; 64 * 1024]).unwrap();

    // Scanned from above, `registry` has an ordinary `.cargo` parent node.
    let from_above = scan(dir.path());
    let nested = find(&from_above, ".cargo/registry");
    let expected = from_above.node(nested).preset;
    assert!(expected.is_some(), "fixture does not match any preset");

    // Scanned with `.cargo` as the root, it must still match.
    let at_root = scan(&cargo);
    let top = find(&at_root, "registry");
    assert_eq!(at_root.node(top).preset, expected, "the scan root did not vouch for its children");
    assert!(at_root.reclaimable.contains(&top), "missing from the reclaimable view");
}
