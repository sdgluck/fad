//! What is staged has to keep meaning the same files. Node ids are arena
//! indices, so every tree swap is a chance to point a delete batch at something
//! the user never selected.

use std::path::Path;

use fad::app::App;
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;
use fad::tree::Tree;

fn fixture(root: &Path) {
    let mk = |rel: &str, size: usize| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; size]).unwrap();
    };
    mk("dev/fad/target/debug/huge.rlib", 6 * 1024 * 1024);
    mk("dev/fad/src/main.rs", 4 * 1024);
    mk("Movies/holiday.mov", 9 * 1024 * 1024);
    mk("Library/Caches/big.cache", 3 * 1024 * 1024);
}

fn scanned(root: &Path) -> Tree {
    let (mut tree, scan) = Scan::start(root, ScanOpts::default()).unwrap();
    scan.finish(&mut tree);
    tree
}

fn app_for(root: &Path) -> App {
    let opts = ScanOpts::default();
    let (mut tree, scan) = Scan::start(root, opts.clone()).unwrap();
    scan.finish(&mut tree);
    App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts)
}

fn find(tree: &Tree, rel: &str) -> fad::tree::NodeId {
    tree.find_path(&tree.root_path().join(rel)).unwrap_or_else(|| panic!("no node for {rel}"))
}

/// A tree of the same root with padding entries ahead of the real ones, so no
/// path keeps the arena index it has in a plain scan.
fn shifted_snapshot(root: &Path) -> Tree {
    use fad::scan::meta::{Kind, Meta};
    use fad::scan::walk::{Batch, Entry};

    let dir_meta = Meta {
        blocks: 0,
        len: 0,
        mtime: 0,
        dev: 1,
        ino: 0,
        nlink: 1,
        kind: Kind::Dir,
    };
    let real = scanned(root);
    // The scan canonicalises its root, and paths are compared against it.
    let mut tree = Tree::new(real.root_path().to_path_buf(), &dir_meta);
    let mut entries: Vec<Entry> = (0..4)
        .map(|i| Entry {
            name: format!("pad-{i}").into(),
            meta: dir_meta,
            descend: None,
            skip: None,
        })
        .collect();
    for c in &real.node(real.root()).children {
        entries.push(Entry {
            name: real.node(*c).name.to_string().into(),
            meta: dir_meta,
            descend: None,
            skip: None,
        });
    }
    tree.apply(Batch { parent: 0, entries, unreadable: None });
    tree
}

#[test]
fn staging_survives_the_snapshot_swap_as_paths_not_ids() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    // Stage something before the snapshot lands, as a quick user would.
    let movies = find(&app.tree, "Movies");
    app.stage(movies);
    let staged_path = app.tree.path(movies);

    // A snapshot of the same root whose arena is laid out differently — which
    // is the whole hazard: the same path is a different index in each tree.
    let snapshot = shifted_snapshot(dir.path());
    let there = snapshot.find_path(&staged_path).expect("path missing from the snapshot");
    assert_ne!(there, movies, "fixture failed to shift the arena indices");

    assert!(app.install_snapshot(snapshot).is_ok(), "snapshot was not installed");

    let items = app.batch_items();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, staged_path, "the staged item moved to another path");
}

#[test]
fn nothing_stays_staged_inside_something_else_that_is_staged() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    let dev = find(&app.tree, "dev");
    let target = find(&app.tree, "dev/fad/target");

    // The child first, then the directory that contains it.
    app.stage(target);
    app.stage(dev);
    assert_eq!(app.staged.len(), 1, "a nested item was left staged");
    assert!(app.staged.contains(&dev));
    assert_eq!(app.staged_bytes(), app.tree.node(dev).total_bytes);

    // And the other way round: staging inside an already-staged directory is a
    // no-op rather than a second copy of the same bytes.
    app.stage(target);
    assert_eq!(app.staged.len(), 1);
    assert_eq!(app.staged_bytes(), app.tree.node(dev).total_bytes);
}

#[test]
fn apparent_sizes_reach_the_ui_not_just_json() {
    let dir = tempfile::tempdir().unwrap();
    // A sparse file: long, but costing almost nothing on disk.
    std::fs::create_dir_all(dir.path().join("sparse")).unwrap();
    let f = std::fs::File::create(dir.path().join("sparse/hole.img")).unwrap();
    f.set_len(64 * 1024 * 1024).unwrap();
    drop(f);

    let mut app = app_for(dir.path());
    let hole = find(&app.tree, "sparse/hole.img");
    app.stage(hole);

    let on_disk = app.staged_bytes();
    app.apparent = true;
    let apparent = app.staged_bytes();
    assert_eq!(apparent, 64 * 1024 * 1024);
    assert!(apparent > on_disk, "sparse file reported the same size both ways");
    assert_eq!(app.batch_items()[0].1, apparent, "the batch ignored --apparent");
}

#[test]
fn rescanning_while_the_cached_tree_is_shown_starts_clean() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());

    let opts = ScanOpts::default();
    let (tree, scan) = Scan::start(dir.path(), opts.clone()).unwrap();
    let mut app = App::new(tree, scan, opts);

    // A snapshot goes on screen, pushing the running walk into the background.
    assert!(app.install_snapshot(shifted_snapshot(dir.path())).is_ok());
    assert!(app.from_cache);

    // R throws all of that away. The new walk must feed the visible tree, not
    // the half-built one left over from the walk that was just abandoned.
    app.restart_scan().unwrap();
    assert!(!app.from_cache, "still claiming to show cached sizes");
    while app.scanning() {
        app.poll_scan();
    }

    let fresh = scanned(dir.path());
    assert_eq!(
        app.tree.node(app.tree.root()).total_bytes,
        fresh.node(fresh.root()).total_bytes,
        "the rescan did not land on the displayed tree"
    );
    assert!(app.tree_is_complete(), "a finished rescan is not worth persisting");
}

#[test]
fn the_filter_is_smart_case() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    let rows_for = |app: &mut App, needle: &str| {
        app.filter = needle.to_string();
        app.mark_dirty();
        app.rebuild_rows();
        let names: Vec<String> =
            app.rows.iter().map(|r| app.tree.node(r.id).name.to_string()).collect();
        names
    };

    // Lowercase matches anything.
    assert!(rows_for(&mut app, "movies").iter().any(|n| n == "Movies"));
    // The exact case matches.
    assert!(rows_for(&mut app, "Movies").iter().any(|n| n == "Movies"));
    // A capital the name does not have means you meant it.
    assert!(!rows_for(&mut app, "MOVIES").iter().any(|n| n == "Movies"));
}

/// The basket is the last place a batch can be corrected, so it has to show
/// the whole batch — grouped, but with nothing dropped.
#[test]
fn the_basket_groups_the_batch_and_accounts_for_all_of_it() {
    use fad::app::BasketRow;

    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    let target = find(&app.tree, "dev/fad/target");
    let caches = find(&app.tree, "Library/Caches");
    let movies = find(&app.tree, "Movies/holiday.mov");
    for id in [target, caches, movies] {
        app.stage(id);
    }

    let rows = app.basket_rows();
    let items: Vec<_> = rows
        .iter()
        .filter_map(|r| match r {
            BasketRow::Item(id) => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(items.len(), 3, "the basket must list every staged item");

    let groups: Vec<_> = rows
        .iter()
        .filter_map(|r| match r {
            BasketRow::Group { cat, count, bytes } => Some((*cat, *count, *bytes)),
            _ => None,
        })
        .collect();
    // An app cache, and two things the presets know nothing about — `target`
    // has no `Cargo.toml` beside it, so it is not a build directory. Two
    // headings, and the subtotals have to add up to the batch.
    assert_eq!(groups.len(), 2, "expected one heading per category present");
    let total: u64 = groups.iter().map(|(_, _, b)| b).sum();
    assert_eq!(total, app.staged_bytes(), "group subtotals must account for the batch");
    let uncategorised = groups.iter().find(|(c, _, _)| c.is_none()).expect("uncategorised items need a home");
    assert_eq!(uncategorised.1, 2);
}

/// A snapshot that arrives after the walk has already finished is no use as a
/// display — but it is the only record of the previous scan, and so the only
/// thing the "what grew" comparison can measure against. `install_snapshot`
/// therefore has to hand it back rather than swallow it: taking it by value
/// with nothing to return was how it got dropped on the floor.
#[test]
fn a_snapshot_that_arrives_too_late_is_handed_back() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    // Let the live walk finish, which is the race the snapshot has just lost.
    while app.scanning() {
        app.poll_scan();
    }

    let snapshot = shifted_snapshot(dir.path());
    let root = snapshot.root_path().to_path_buf();
    let handed_back = app
        .install_snapshot(snapshot)
        .expect_err("a finished walk should refuse a snapshot for display");
    assert_eq!(handed_back.root_path(), root, "the snapshot came back damaged");
    assert!(
        handed_back.find_path(&root.join("Movies")).is_some(),
        "the snapshot came back unusable for a path comparison"
    );
}
