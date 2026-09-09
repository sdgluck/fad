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

/// Sorting by "modified" has to mean the same thing the age filter and the
/// detail pane mean by it. A directory's own mtime moves when its listing
/// changes — a file added, removed or renamed — so ranking on it puts a folder
/// somebody tidied above a project worked on this morning, and disagrees with
/// the filter sitting next to it on the same screen.
#[test]
fn sorting_by_modified_ranks_on_the_newest_write_below() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let now = fad::app::now_secs();

    // Nothing written in it for years; the directory itself was touched just
    // now, which is what reorganising one does.
    write_aged(&root.join("reorganised/a"), 1024, 900);
    // Written to yesterday, but the directory's own mtime is ancient.
    write_aged(&root.join("worked_on/a"), 1024, 900);
    write_aged(&root.join("worked_on/fresh"), 1024, 1);
    set_mtime(&root.join("reorganised"), now);
    set_mtime(&root.join("worked_on"), now - 900 * DAY);

    let mut tree = scanned(root);
    let r = tree.root();

    // The trap the sort used to fall into is right here in the fixture.
    assert!(
        tree.node(find(&tree, "reorganised")).mtime
            > tree.node(find(&tree, "worked_on")).mtime,
        "fixture failed to make the tidied directory look fresher"
    );

    tree.sort_children(r, fad::tree::Sort::Modified, false);
    let order: Vec<String> =
        tree.node(r).children.iter().map(|c| tree.node(*c).name.to_string()).collect();
    assert_eq!(
        order,
        vec!["worked_on".to_string(), "reorganised".to_string()],
        "sorted by the directory's own mtime rather than the newest write below it"
    );
}

/// The newest-write rollup is a maximum, and a maximum does not subtract, so
/// removing a node cannot walk it back the way `roll_up` walks sizes back.
/// Delete the one recent file in an archive and every directory above it went
/// on claiming to have been written to yesterday — which the age filter then
/// believed, and went on hiding a branch that had become exactly what it was
/// looking for.
#[test]
fn deleting_the_newest_file_makes_its_ancestors_old_again() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_aged(&root.join("archive/deep/ancient"), 1024, 900);
    write_aged(&root.join("archive/deep/fresh"), 1024, 1);

    let mut tree = scanned(root);
    let now = fad::app::now_secs();
    let archive = find(&tree, "archive");
    let deep = find(&tree, "archive/deep");

    assert!(now - tree.node(archive).last_write() < 3 * DAY, "fixture is not fresh to begin with");

    tree.remove(find(&tree, "archive/deep/fresh")).expect("remove returned nothing");

    for (id, what) in [(deep, "the containing directory"), (archive, "its parent")] {
        assert!(
            now - tree.node(id).last_write() > 800 * DAY,
            "{what} still reports the deleted file's write"
        );
    }
}

/// The same thing through the filter, which is where a user would meet it.
#[test]
fn the_age_filter_reveals_a_branch_once_its_fresh_file_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_aged(&root.join("active/old"), 1024, 900);
    write_aged(&root.join("active/fresh"), 1024, 2);

    let opts = ScanOpts::default();
    let (mut tree, scan) = Scan::start(root, opts.clone()).unwrap();
    scan.finish(&mut tree);
    let mut app = App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts);
    app.age_filter = AgeFilter::Y1;

    let shown = |app: &mut App| {
        app.mark_dirty();
        app.rebuild_rows();
        app.rows.iter().any(|r| app.tree.node(r.id).name.as_ref() == "active")
    };

    assert!(!shown(&mut app), "a branch with a fresh file was shown as untouched");
    let fresh = app.tree.find_path(&app.tree.root_path().join("active/fresh")).unwrap();
    app.tree.remove(fresh);
    assert!(shown(&mut app), "the branch stayed hidden after its fresh file went");
}
