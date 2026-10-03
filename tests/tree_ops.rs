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

/// Every other preset rule needs corroboration before it fires — a `target`
/// wants a `Cargo.toml` beside it — because, as the module says, name-only
/// matching would eventually stage someone's photos. `.raw` and `.img` were the
/// exception: matched on the extension alone, into the category that
/// `--reclaim --yes` deletes unattended, and `.raw` is what Panasonic and Leica
/// cameras write.
#[test]
fn an_ambiguous_image_extension_needs_a_size_to_vouch_for_it() {
    use fad::presets::Category;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // A camera raw and a disc image, at the sizes those things really are.
    std::fs::write(root.join("P1000123.raw"), vec![0u8; 40 * 1024 * 1024]).unwrap();
    std::fs::write(root.join("notes.img"), vec![0u8; 8 * 1024 * 1024]).unwrap();
    // A VM disk, provisioned large and sparse — which is how they arrive.
    std::fs::File::create(root.join("Docker.raw")).unwrap().set_len(60 << 30).unwrap();
    // And one whose extension is proof on its own, at any size.
    std::fs::write(root.join("box.qcow2"), vec![0u8; 1024]).unwrap();

    let tree = scan(root);
    let preset = |rel: &str| tree.node(find(&tree, rel)).preset;

    assert_eq!(preset("P1000123.raw"), None, "a camera raw was staged as a VM image");
    assert_eq!(preset("notes.img"), None, "a small .img was staged as a VM image");
    assert_eq!(preset("Docker.raw"), Some(Category::VmImage), "a real VM disk was missed");
    assert_eq!(preset("box.qcow2"), Some(Category::VmImage), "an unambiguous format was missed");

    // And nothing that was not offered can reach the unattended path.
    let ignore = fad::ignore::Rules::default();
    let offered: Vec<String> = fad::reclaim::candidates(&tree, false, 0, &ignore)
        .into_iter()
        .map(|(id, _)| tree.node(id).name.to_string())
        .collect();
    assert!(!offered.iter().any(|n| n == "P1000123.raw"), "offered to --reclaim: {offered:?}");
}

/// Everything under a removed directory is gone too, not only the directory.
/// Left looking alive in the arena, the descendants went on being reported by
/// anything that walks it by index, and were written back into the snapshot.
#[test]
fn removing_a_directory_takes_everything_under_it() {
    use fad::tree::flags;
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut tree = scan(dir.path());
    let a = find(&tree, "a");
    let deep = find(&tree, "a/b/c/deep.bin");
    let other = find(&tree, "other.bin");

    tree.remove(a).unwrap();
    assert_ne!(tree.node(deep).flags & flags::DELETED, 0, "a descendant still looks alive");
    assert_eq!(tree.node(other).flags & flags::DELETED, 0, "a bystander was marked");
    // Removing a descendant of something already removed frees nothing more.
    assert!(tree.remove(deep).is_none());

    let back = Tree::from_snapshot(tree.to_snapshot()).expect("the snapshot did not load");
    assert!(back.find_path(&back.root_path().join("a")).is_none(), "came back from the snapshot");
    let names: Vec<_> = (0..back.len() as NodeId).map(|i| back.node(i).name.to_string()).collect();
    assert!(!names.iter().any(|n| n == "deep.bin" || n == "mid.bin"), "{names:?}");
    assert!(back.find_path(&back.root_path().join("other.bin")).is_some());
    assert_eq!(back.node(back.root()).total_bytes, tree.node(tree.root()).total_bytes);
}

/// Two names for one file, both in the tree. Removing the one that carries the
/// bytes frees nothing — the other name still holds the data — and the bytes
/// have to move to the survivor rather than vanish from the totals.
#[test]
fn removing_the_counted_link_moves_its_bytes_to_the_survivor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    std::fs::write(root.join("a/one"), vec![1u8; 1024 * 1024]).unwrap();
    std::fs::hard_link(root.join("a/one"), root.join("b/two")).unwrap();
    let mut tree = scan(root);

    let one = find(&tree, "a/one");
    let two = find(&tree, "b/two");
    let b = find(&tree, "b");
    let size = tree.node(one).self_bytes;
    assert!(size > 0 && tree.node(two).self_bytes == 0, "the scan did not pick `a/one`");
    let root_before = tree.node(tree.root()).total_bytes;

    assert_eq!(tree.remove(one), Some(0), "reported bytes freed that are still on disk");
    assert_eq!(tree.node(two).self_bytes, size, "the survivor did not inherit the bytes");
    assert_eq!(tree.node(b).total_bytes, tree.node(b).self_bytes + size);
    assert_eq!(tree.node(tree.root()).total_bytes, root_before, "the bytes left the totals");
    assert_eq!(
        tree.node(two).flags & fad::tree::flags::HARDLINK_DUPE,
        0,
        "the survivor is still marked as a copy"
    );

    // Now it is the last name, and removing it does free the data.
    assert_eq!(tree.remove(two), Some(size));
}

#[test]
fn removing_an_uncounted_link_frees_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a"), vec![1u8; 512 * 1024]).unwrap();
    std::fs::hard_link(root.join("a"), root.join("b")).unwrap();
    let mut tree = scan(root);
    let before = tree.node(tree.root()).total_bytes;
    assert_eq!(tree.remove(find(&tree, "b")), Some(0));
    assert_eq!(tree.node(tree.root()).total_bytes, before);
}

/// Both links inside one removed directory: the whole file goes.
#[test]
fn removing_every_link_at_once_frees_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("d")).unwrap();
    std::fs::write(root.join("d/a"), vec![1u8; 512 * 1024]).unwrap();
    std::fs::hard_link(root.join("d/a"), root.join("d/b")).unwrap();
    let mut tree = scan(root);
    let d = find(&tree, "d");
    let total = tree.node(d).total_bytes;
    assert_eq!(tree.remove(d), Some(total));
}

/// A link outside the scan root keeps the data alive however many of the
/// inside ones go.
#[test]
fn a_link_outside_the_tree_keeps_the_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("scan");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("inside"), vec![1u8; 512 * 1024]).unwrap();
    std::fs::hard_link(root.join("inside"), dir.path().join("outside")).unwrap();
    let mut tree = scan(&root);
    assert_eq!(tree.remove(find(&tree, "inside")), Some(0));
}

/// A tree loaded from a snapshot is on screen while the fresh walk runs, and
/// can be deleted from. It has to know its hard links as well as a scanned one.
#[test]
fn a_snapshot_remembers_which_links_share_storage() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a"), vec![1u8; 512 * 1024]).unwrap();
    std::fs::hard_link(root.join("a"), root.join("b")).unwrap();
    let scanned = scan(root);
    let mut tree = Tree::from_snapshot(scanned.to_snapshot()).unwrap();
    let a = find(&tree, "a");
    let size = tree.node(a).self_bytes;
    assert_eq!(tree.remove(a), Some(0), "the loaded tree forgot `b` holds the same data");
    assert_eq!(tree.node(find(&tree, "b")).self_bytes, size);
}
