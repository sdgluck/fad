//! The scan must agree with `du` byte for byte. `du` is the reference
//! implementation for "how much disk is this actually costing me", including
//! the awkward cases: sparse files, hardlinks, and symlinks.

use std::path::Path;
use std::process::Command;

use fad::scan::Scan;
use fad::scan::walk::ScanOpts;
use fad::tree::{NodeId, Tree};

/// `du -sk` in bytes, or None if du refused the path.
fn du_bytes(path: &Path) -> Option<u64> {
    let out = Command::new("du").arg("-sk").arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let kb: u64 = text.split_whitespace().next()?.parse().ok()?;
    Some(kb * 1024)
}

/// `hardlink` adds a second link to `a/big.bin` from `c/`. It is opt-in because
/// a cross-directory hardlink makes `du` on a *subdirectory* disagree with a
/// whole-tree walk by construction: `du -sk c` on its own has never seen `a/`,
/// so it bills the shared inode to `c`. Only the root total is comparable then.
fn build_fixture(root: &Path, hardlink: bool) {
    let a = root.join("a");
    let b = a.join("b");
    let c = root.join("c");
    std::fs::create_dir_all(&b).unwrap();
    std::fs::create_dir_all(&c).unwrap();

    std::fs::write(a.join("big.bin"), vec![7u8; 8 * 1024 * 1024]).unwrap();
    std::fs::write(b.join("small.bin"), vec![3u8; 100 * 1024]).unwrap();
    std::fs::write(root.join("empty"), b"").unwrap();

    // Sparse: 200MB of nothing, which should cost almost no blocks.
    let f = std::fs::File::create(c.join("sparse.img")).unwrap();
    f.set_len(200 * 1024 * 1024).unwrap();
    drop(f);

    if hardlink {
        // Counted once, and always against `a/big.bin` because that path sorts first.
        std::fs::hard_link(a.join("big.bin"), c.join("big-hardlink.bin")).unwrap();
    }

    // A symlink is worth its own few bytes, never its target's.
    std::os::unix::fs::symlink(a.join("big.bin"), c.join("link-to-big")).unwrap();
}

fn scan(root: &Path) -> Tree {
    let (mut tree, scan) = Scan::start(root, ScanOpts::default()).unwrap();
    scan.finish(&mut tree);
    tree
}

fn find(tree: &Tree, rel: &str) -> NodeId {
    let want = tree.root_path().join(rel);
    (0..tree.len() as NodeId)
        .find(|&id| tree.path(id) == want)
        .unwrap_or_else(|| panic!("no node for {rel}"))
}

#[test]
fn totals_match_du_at_every_level() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path(), false);
    let tree = scan(dir.path());

    for rel in ["", "a", "a/b", "c"] {
        let id = if rel.is_empty() { tree.root() } else { find(&tree, rel) };
        let path = tree.path(id);
        let expected = du_bytes(&path).unwrap();
        assert_eq!(
            tree.node(id).total_bytes,
            expected,
            "size mismatch at {:?}: fad {} vs du {}",
            path,
            tree.node(id).total_bytes,
            expected
        );
    }
}

#[test]
fn root_total_matches_du_when_hardlinks_span_directories() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path(), true);
    let tree = scan(dir.path());
    let expected = du_bytes(dir.path()).unwrap();
    assert_eq!(tree.node(tree.root()).total_bytes, expected);
}

#[test]
fn hardlink_is_counted_once_and_always_at_the_same_path() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path(), true);

    // Attribution must not depend on which walker task happened to arrive first.
    for _ in 0..5 {
        let tree = scan(dir.path());
        let winner = find(&tree, "a/big.bin");
        let dupe = find(&tree, "c/big-hardlink.bin");
        assert!(tree.node(winner).self_bytes >= 8 * 1024 * 1024);
        assert_eq!(tree.node(dupe).self_bytes, 0, "dupe should contribute nothing");
    }
}

#[test]
fn sparse_file_costs_blocks_not_length() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path(), false);
    let tree = scan(dir.path());
    let n = tree.node(find(&tree, "c/sparse.img"));
    assert_eq!(n.self_len, 200 * 1024 * 1024, "apparent size is the full length");
    assert!(n.self_bytes < 1024 * 1024, "but it barely costs any disk: {}", n.self_bytes);
}

#[test]
fn symlink_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path(), false);
    let tree = scan(dir.path());
    let n = tree.node(find(&tree, "c/link-to-big"));
    // Not "costs nothing": a filesystem may allocate a block for the link
    // itself (ext4 does for longer targets, APFS stores it inline). The claim
    // is that we recorded the link, not the 8MB it points at.
    assert!(
        n.total_bytes < 64 * 1024,
        "symlink appears to have been followed: {} bytes",
        n.total_bytes
    );
    assert!(n.self_len < 4096, "symlink length is its target's path, not its size");
}
