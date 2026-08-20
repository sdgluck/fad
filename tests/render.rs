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
    let loaded = fad::cache::load(dir.path()).expect("snapshot did not load");
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
