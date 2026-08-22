//! `L` in the duplicate view: keep every copy and stop paying for all but one.
//!
//! The end-to-end version of the promise. What matters is not that the syscall
//! works — `tests/clone.rs` covers that — but that pressing the key over a
//! group leaves every path in place, takes the group off the list, and reports
//! what happened without overstating it.

use std::path::Path;

use fad::app::App;
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;

/// Two byte-identical files, written rather than copied: on macOS the standard
/// library's copy already clones, which would leave nothing to share.
fn fixture(root: &Path) {
    let body = {
        let mut v = vec![0x5Au8; 2 << 20];
        v[0] = b'h';
        *v.last_mut().unwrap() = b't';
        v
    };
    std::fs::create_dir_all(root.join("one")).unwrap();
    std::fs::create_dir_all(root.join("two")).unwrap();
    std::fs::write(root.join("one/asset.bin"), &body).unwrap();
    std::fs::write(root.join("two/asset.bin"), &body).unwrap();
}

fn app_for(root: &Path) -> App {
    let opts = ScanOpts::default();
    let (mut tree, scan) = Scan::start(root, opts.clone()).unwrap();
    scan.finish(&mut tree);
    let mut app = App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts);
    app.dupe_view = true;
    app.start_dupe_hunt();
    for _ in 0..2000 {
        if app.poll_dupes() {
            return app;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the duplicate hunt never finished");
}

#[test]
fn sharing_a_group_keeps_every_path_and_takes_it_off_the_list() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    let groups = app.dupes.as_ref().unwrap().groups.len();
    assert_eq!(groups, 1, "the fixture pair was not found as a duplicate");

    let message = app.clone_group(0);
    if message.contains("cannot share storage") {
        eprintln!("skipped: this filesystem cannot share storage between files");
        return;
    }
    println!("{message}");

    // Both paths still there, both still readable, both still the same bytes.
    let one = std::fs::read(dir.path().join("one/asset.bin")).unwrap();
    let two = std::fs::read(dir.path().join("two/asset.bin")).unwrap();
    assert_eq!(one, two, "the copies stopped matching");
    assert_eq!(one.len(), 2 << 20, "a copy lost its contents");

    // Nothing was staged and nothing was trashed: this is not a deletion.
    assert!(app.nothing_staged(), "sharing storage staged something for deletion");

    // And the group is gone from the view, because there is no longer anything
    // to reclaim by deleting one of them.
    assert!(
        app.dupes.as_ref().unwrap().groups.is_empty(),
        "a group that now shares one copy is still on offer to delete"
    );

    // The message has to say both halves of the truth: what came back, and that
    // the tree above will not show it.
    assert!(message.contains("share"), "message does not say what happened: {message}");
    assert!(message.contains("du still counts both"), "message overstates it: {message}");
}

/// Pressing it outside the duplicate view, or where there is no group, must say
/// so rather than quietly do nothing.
#[test]
fn there_is_nothing_to_share_without_a_group() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    // Group index past the end: the report moved on underneath the keypress.
    let message = app.clone_group(99);
    assert!(!message.is_empty(), "silent no-op");
    assert!(
        app.dupes.as_ref().unwrap().groups.len() == 1,
        "a bad index disturbed the report"
    );
}
