//! An ignored entry is hidden and refused, never subtracted. A size that
//! quietly omits things is the one failure this tool cannot afford.

mod common;

use std::path::Path;

use fad::app::App;
use fad::ignore::Rules;
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;

fn fixture(root: &Path) {
    let mk = |rel: &str, size: usize| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; size]).unwrap();
    };
    mk("keep/a", 4 * 1024 * 1024);
    mk("VMs/disk.raw", 8 * 1024 * 1024);
}

fn app_for(root: &Path) -> App {
    let opts = ScanOpts::default();
    let (mut tree, scan) = Scan::start(root, opts.clone()).unwrap();
    scan.finish(&mut tree);
    App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts)
}

#[test]
fn an_ignored_entry_is_hidden_but_still_counted() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    let before = app.tree.size(app.tree.root(), false);
    // The scan canonicalises its root, so the rule has to name the same path
    // the tree does — on macOS a temp dir is a symlink into /private.
    let vms_path = app.tree.root_path().join("VMs");
    app.ignore = Rules::parse(&format!("{}\n", vms_path.display()));
    app.mark_dirty();
    app.rebuild_rows();

    let names: Vec<String> =
        app.rows.iter().map(|r| app.tree.node(r.id).name.to_string()).collect();
    assert!(!names.iter().any(|n| n == "VMs"), "ignored entry was shown: {names:?}");
    assert!(names.iter().any(|n| n == "keep"));

    assert_eq!(app.ignored.0, 1, "what was hidden has to be reported");
    assert_eq!(
        app.tree.size(app.tree.root(), false),
        before,
        "ignoring must never change a total"
    );
}

#[test]
fn something_staged_before_the_rule_is_refused_at_review() {
    let _guard = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let scan_root = dir.path().join("scan");
    std::fs::create_dir_all(&scan_root).unwrap();
    fixture(&scan_root);

    let mut app = app_for(&scan_root);
    let vms_path = app.tree.root_path().join("VMs");
    let vms = app.tree.find_path(&vms_path).unwrap();
    app.stage(vms);

    app.ignore = Rules::parse(&format!("{}\n", vms_path.display()));
    app.review_batch();

    assert!(app.staged.is_empty(), "an ignored path survived review");
    assert_eq!(app.refused.len(), 1);
}

#[test]
fn a_glob_matches_names_anywhere_in_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    app.ignore = Rules::parse("*.raw\n");
    let vms = app.tree.root_path().join("VMs");
    app.expanded.insert(app.tree.find_path(&vms).unwrap());
    app.mark_dirty();
    app.rebuild_rows();

    let names: Vec<String> =
        app.rows.iter().map(|r| app.tree.node(r.id).name.to_string()).collect();
    assert!(names.iter().any(|n| n == "VMs"), "the directory itself should still show");
    assert!(!names.iter().any(|n| n == "disk.raw"), "the glob did not hide the file");
}
