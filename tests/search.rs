//! `f`: find an entry anywhere in the tree.
//!
//! The question `/` cannot answer. The filter narrows what is already on
//! screen; this reaches into branches nobody has opened, which means the two
//! things worth testing are that it finds them and that going to one actually
//! puts the cursor on it.

use std::path::Path;

use fad::app::{AgeFilter, App};
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;

fn fixture(root: &Path) {
    let mk = |rel: &str, size: usize| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; size]).unwrap();
    };
    // Four levels down a branch that starts closed.
    mk("dev/app/ios/build/Simulator.runtime", 8 * 1024 * 1024);
    mk("dev/app/node_modules/left-pad/index.js", 1024);
    mk("dev/other/node_modules/big/blob.bin", 4 * 1024 * 1024);
    mk("Movies/holiday.mov", 2 * 1024 * 1024);
}

fn app_for(root: &Path) -> App {
    let opts = ScanOpts::default();
    let (mut tree, scan) = Scan::start(root, opts.clone()).unwrap();
    scan.finish(&mut tree);
    App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts)
}

fn rel(app: &App, id: fad::tree::NodeId) -> String {
    let p = app.tree.path(id);
    p.strip_prefix(app.tree.root_path()).unwrap_or(&p).display().to_string()
}

#[test]
fn it_reaches_into_branches_nobody_has_opened() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    app.search = "Simulator".into();
    app.run_search();

    assert_eq!(app.search_hits.len(), 1, "hits: {:?}", app.search_hits);
    assert_eq!(rel(&app, app.search_hits[0].0), "dev/app/ios/build/Simulator.runtime");
}

/// Ranked by size, not by how tidy the match is: the question being asked is
/// "where is the big one".
#[test]
fn matches_come_back_biggest_first() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    app.search = "node_modules".into();
    app.run_search();

    let names: Vec<String> = app.search_hits.iter().map(|(id, _)| rel(&app, *id)).collect();
    assert_eq!(names, ["dev/other/node_modules", "dev/app/node_modules"], "wrong order: {names:?}");
}

/// A lowercase query is case-insensitive; typing a capital means you meant it.
#[test]
fn case_is_smart_the_way_the_filter_is() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    app.search = "simulator".into();
    app.run_search();
    assert_eq!(app.search_hits.len(), 1, "a lowercase query missed a capitalised name");

    app.search = "SIMULATOR".into();
    app.run_search();
    assert!(app.search_hits.is_empty(), "an uppercase query matched anyway");
}

/// Going to a hit has to open everything above it, or the row it selected does
/// not exist to be selected.
#[test]
fn going_to_a_hit_opens_the_way_down_to_it() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    app.search = "Simulator".into();
    app.run_search();
    let target = app.search_hits[0].0;

    assert!(app.jump_to_hit().is_none(), "needed to undo something to get there");
    app.rebuild_rows();
    assert_eq!(app.selected(), Some(target), "the cursor did not land on the hit");
}

/// The hit can be behind a filter the user set up earlier. Undoing it silently
/// would be as confusing as refusing to move, so it is undone and said.
#[test]
fn a_hit_behind_a_filter_is_reached_and_the_undoing_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    // Everything in the fixture was written a moment ago, so this hides all of
    // it — including whatever the search is about to find.
    app.age_filter = AgeFilter::Y2;
    app.search = "Simulator".into();
    app.run_search();
    let target = app.search_hits[0].0;

    let said = app.jump_to_hit().expect("said nothing about clearing the filter");
    assert!(said.contains("age filter"), "did not say what it undid: {said}");
    app.rebuild_rows();
    assert_eq!(app.selected(), Some(target), "the cursor did not land on the hit");
    assert_eq!(app.age_filter, AgeFilter::All);
}

/// The list is a top hundred. Passing it off as the whole set would make "not
/// found" and "not shown" look the same.
#[test]
fn a_truncated_list_says_how_much_it_is_not_showing() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..150 {
        let p = dir.path().join(format!("bulk/item{i}.log"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; 1024]).unwrap();
    }
    let mut app = app_for(dir.path());

    app.search = "item".into();
    app.run_search();

    assert_eq!(app.search_hits.len(), 100);
    assert_eq!(app.search_more, 50, "did not report the rest");
}
