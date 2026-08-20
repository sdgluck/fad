//! Renders the real UI against a fixture tree. Cheap to eyeball, and it fails
//! loudly if a layout change starts producing garbage.

use std::path::Path;

use fad::app::App;
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

mod common;

fn fixture(root: &Path) {
    let mk = |rel: &str, size: usize| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; size]).unwrap();
    };
    mk("dev/fad/target/debug/huge.rlib", 6 * 1024 * 1024);
    mk("dev/fad/target/debug/deps.o", 2 * 1024 * 1024);
    mk("dev/fad/src/main.rs", 4 * 1024);
    mk("dev/notes/todo.md", 1024);
    mk("Movies/holiday.mov", 9 * 1024 * 1024);
    mk("Library/Caches/big.cache", 3 * 1024 * 1024);
}

fn app_for(root: &Path) -> App {
    let opts = ScanOpts::default();
    let (mut tree, scan) = Scan::start(root, opts.clone()).unwrap();
    scan.finish(&mut tree);
    App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts)
}

fn render(app: &mut App, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    app.rebuild_rows();
    terminal.draw(|f| fad::ui::draw(f, app)).unwrap();
    let buf = terminal.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn renders_a_tree_with_sizes_and_bars() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    // Open the biggest directory so the snapshot shows a nested level.
    let root = app.tree.root();
    let biggest = *app.tree.node(root).children.first().unwrap();
    app.expanded.insert(biggest);
    app.mark_dirty();

    let out = render(&mut app, 100, 20);
    println!("{out}");

    assert!(out.contains("holiday.mov") || out.contains("Movies"), "no content rendered:\n{out}");
    assert!(out.contains("█"), "no size bars rendered:\n{out}");
    assert!(out.contains("selection"), "no detail pane:\n{out}");
    assert!(out.contains("staged"), "no staging pane:\n{out}");
}

#[test]
fn staging_shows_up_in_both_panes() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    app.cursor = 1;
    app.rebuild_rows();
    let id = app.selected().unwrap();
    app.staged.insert(id);
    app.mark_dirty();

    let out = render(&mut app, 100, 20);
    println!("{out}");
    assert!(out.contains("●"), "no staged marker:\n{out}");
    assert!(out.contains("1 staged"), "status bar missing the batch:\n{out}");
}

#[test]
fn confirm_modal_states_the_whole_truth() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    // Stage every top-level directory.
    let root = app.tree.root();
    for c in app.tree.node(root).children.clone() {
        app.staged.insert(c);
    }
    app.review_batch();
    app.mode = fad::app::Mode::Confirm;
    app.mark_dirty();

    let trash_view = render(&mut app, 100, 24);
    println!("--- Trash ---\n{trash_view}");
    assert!(trash_view.contains("move to Trash"), "{trash_view}");
    assert!(trash_view.contains("u puts them back"), "{trash_view}");

    app.disposal = fad::delete::Disposal::Permanent;
    let perm_view = render(&mut app, 100, 24);
    println!("--- Permanent ---\n{perm_view}");
    assert!(perm_view.contains("cannot be undone"), "{perm_view}");
    assert!(perm_view.contains("permanently delete"), "{perm_view}");
}

#[test]
fn snapshot_round_trips_and_keeps_the_users_place() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let _env = common::env_lock();
    common::isolate(dir.path());

    let mut app = app_for(dir.path());
    let root = app.tree.root();
    let biggest = *app.tree.node(root).children.first().unwrap();
    app.expanded.insert(biggest);
    app.staged.insert(biggest);
    let staged_path = app.tree.path(biggest);

    fad::cache::save(&app.tree).unwrap();
    let (loaded, _at) = fad::cache::load(dir.path()).expect("snapshot did not load");
    assert_eq!(
        loaded.node(loaded.root()).total_bytes,
        app.tree.node(root).total_bytes,
        "sizes did not survive the round trip"
    );

    // Node ids are not stable across a rebuild; paths are what carry a
    // selection from one tree to the next.
    let again = loaded.find_path(&staged_path).expect("staged path lost");
    assert_eq!(loaded.node(again).name, app.tree.node(biggest).name);
    assert_eq!(loaded.node(again).preset, app.tree.node(biggest).preset);
}

#[test]
fn a_snapshot_of_another_directory_is_rejected() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fixture(a.path());
    let _env = common::env_lock();
    common::isolate(a.path());

    let app = app_for(a.path());
    fad::cache::save(&app.tree).unwrap();
    assert!(fad::cache::load(b.path()).is_none(), "loaded a foreign snapshot");
}

#[test]
fn the_cursor_can_step_up_onto_a_reclaimable_heading() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |rel: &str, size: usize| {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; size]).unwrap();
    };
    // Two categories, both matched the same way on macOS and Linux: a Cargo
    // build directory and an npm package cache.
    mk("proj/Cargo.toml", 64);
    mk("proj/target/debug/big.rlib", 4 * 1024 * 1024);
    mk("npm/_cacache/chunk.bin", 2 * 1024 * 1024);
    let mut app = app_for(dir.path());

    app.reclaim_view = true;
    for cat in fad::presets::Category::all() {
        app.reclaim_open.insert(cat);
    }
    app.mark_dirty();
    app.rebuild_rows();

    // The second heading down, so there is a row above it to have come from.
    let heading = app
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.header.is_some())
        .nth(1)
        .map(|(i, _)| i)
        .expect("fixture should produce two reclaimable categories");

    // A heading shares its id with the first item under it; stepping up from
    // that item used to snap straight back down onto it.
    app.cursor = heading + 1;
    app.cursor -= 1;
    app.mark_dirty();
    app.rebuild_rows();
    assert_eq!(app.cursor, heading, "cursor bounced off the heading");

    app.cursor -= 1;
    app.mark_dirty();
    app.rebuild_rows();
    assert_eq!(app.cursor, heading - 1, "cursor stuck at the heading");
}

#[test]
fn reclaimable_categories_start_closed_and_open_on_demand() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |rel: &str, size: usize| {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; size]).unwrap();
    };
    mk("proj/Cargo.toml", 64);
    mk("proj/target/debug/big.rlib", 4 * 1024 * 1024);
    mk("other/Cargo.toml", 64);
    mk("other/target/debug/small.rlib", 1024 * 1024);
    mk("npm/_cacache/chunk.bin", 2 * 1024 * 1024);
    let mut app = app_for(dir.path());

    app.reclaim_view = true;
    app.mark_dirty();
    app.rebuild_rows();

    // Closed: the first screen is the category totals, nothing else.
    assert!(app.rows.iter().all(|r| r.header.is_some()), "items showing under a closed heading");
    let headings = app.rows.len();
    assert_eq!(headings, 2, "expected a build-artifact and a package-cache category");

    // A closed heading still knows what is under it, so `A` can stage it all.
    let build = fad::presets::Category::BuildArtifact;
    assert_eq!(app.reclaim_items(build).len(), 2);

    app.reclaim_open.insert(build);
    app.mark_dirty();
    app.rebuild_rows();
    assert_eq!(app.rows.len(), headings + 2, "opening the category did not reveal its items");

    app.reclaim_open.remove(&build);
    app.mark_dirty();
    app.rebuild_rows();
    assert_eq!(app.rows.len(), headings, "closing the category did not hide its items");
}

/// Growth is the comparison the numbers on screen cannot make on their own, so
/// the tree that comes off screen has to be kept, not dropped.
#[test]
fn the_detail_pane_reports_what_grew_since_the_last_scan() {
    let _guard = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let root = dir.path().join("scan");
    std::fs::create_dir_all(root.join("cache")).unwrap();
    std::fs::write(root.join("cache/a"), vec![0u8; 2 * 1024 * 1024]).unwrap();

    // A saved scan, then the same directory rather larger.
    let app = app_for(&root);
    fad::cache::save(&app.tree).unwrap();
    std::fs::write(root.join("cache/b"), vec![0u8; 6 * 1024 * 1024]).unwrap();

    let mut app2 = app_for(&root);
    let (snapshot, at) = fad::cache::load(&root).expect("snapshot did not load");
    // The walk has already finished here, so the snapshot is no use as a
    // display — it is kept purely for the comparison.
    app2.install_snapshot_for_test(snapshot, at);

    let cache = app2.tree.find_path(&app2.tree.root_path().join("cache")).unwrap();
    app2.cursor = app2.rows.iter().position(|r| r.id == cache).unwrap();
    app2.ensure_breakdown();

    let growth = app2.breakdown.as_ref().unwrap().growth.expect("no comparison was made");
    assert_eq!(growth, Some(6 * 1024 * 1024), "the 6M that arrived was not reported");
}
