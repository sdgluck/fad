//! Age is the second dimension: what a directory *is* is its size, what makes
//! it a candidate is that nobody has written to it in two years. The rollup has
//! to report the newest write anywhere in a subtree, not the directory's own
//! mtime — those differ every time a folder is reorganised.

use std::path::Path;

use fad::app::{AgeFilter, App};
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;
use fad::tree::{NodeId, Tree};

const DAY: i64 = 86400;

fn write_aged(path: &Path, size: usize, days_ago: i64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![0u8; size]).unwrap();
    set_mtime(path, fad::app::now_secs() - days_ago * DAY);
}

/// Backdate an entry. `utimes` rather than a crate: one call, no dependency.
fn set_mtime(path: &Path, secs: i64) {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let tv = libc::timeval { tv_sec: secs as libc::time_t, tv_usec: 0 };
    let times = [tv, tv];
    assert_eq!(unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) }, 0);
}

fn scanned(root: &Path) -> Tree {
    let (mut tree, scan) = Scan::start(root, ScanOpts::default()).unwrap();
    scan.finish(&mut tree);
    tree
}

fn find(tree: &Tree, rel: &str) -> NodeId {
    tree.find_path(&tree.root_path().join(rel)).unwrap()
}

#[test]
fn the_newest_write_below_reaches_every_ancestor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_aged(&root.join("old/a"), 1024, 900);
    write_aged(&root.join("old/deep/b"), 1024, 800);
    write_aged(&root.join("mixed/ancient"), 1024, 900);
    write_aged(&root.join("mixed/yesterday"), 1024, 1);

    let tree = scanned(root);
    let now = fad::app::now_secs();

    // A subtree of nothing but old files stays old...
    let old = tree.node(find(&tree, "old")).last_write();
    assert!(now - old > 700 * DAY, "old subtree reported as recent");

    // ...and one recent file anywhere below is enough to make the whole
    // subtree recent, which is the entire point of the rollup.
    let mixed = tree.node(find(&tree, "mixed")).last_write();
    assert!(now - mixed < 3 * DAY, "a fresh file did not reach the parent");
}

#[test]
fn the_age_filter_hides_a_subtree_with_anything_recent_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_aged(&root.join("abandoned/x"), 1024, 900);
    write_aged(&root.join("active/x"), 1024, 900);
    write_aged(&root.join("active/fresh"), 1024, 2);

    let opts = ScanOpts::default();
    let (mut tree, scan) = Scan::start(root, opts.clone()).unwrap();
    scan.finish(&mut tree);
    let mut app = App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts);

    app.age_filter = AgeFilter::Y1;
    app.mark_dirty();
    app.rebuild_rows();

    let names: Vec<String> =
        app.rows.iter().map(|r| app.tree.node(r.id).name.to_string()).collect();
    assert!(names.iter().any(|n| n == "abandoned"), "untouched subtree was hidden: {names:?}");
    assert!(!names.iter().any(|n| n == "active"), "subtree with a fresh file shown: {names:?}");
}
