//! What the app does with a batch between the keypress and the delete: what it
//! promises, what it will stage at all, and what it will let through to the
//! confirm step.

use std::path::Path;

use fad::app::{App, Mode};
use fad::delete::Disposal;
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;
use fad::tree::Tree;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

mod common;

fn fixture(root: &Path) {
    let mk = |rel: &str, size: usize| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![1u8; size]).unwrap();
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

/// An app over a finished tree, with a second walk still running behind it —
/// the shape every test file here builds, because `App::new` wants a scan.
fn app_for(root: &Path) -> App {
    let opts = ScanOpts::default();
    let tree = scanned(root);
    App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts)
}

/// An app whose scan has run to completion, so nothing races the test.
fn settled(root: &Path) -> App {
    let mut app = app_for(root);
    while app.scanning() {
        app.poll_scan();
    }
    app
}

fn find(tree: &Tree, rel: &str) -> fad::tree::NodeId {
    tree.find_path(&tree.root_path().join(rel)).unwrap_or_else(|| panic!("no node for {rel}"))
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

// ------------------------------------------------------- free after the batch

/// The Trash is a folder on the same volume. A batch moved there frees nothing
/// until the trash goes out, so the "free after this batch" figure must not
/// move for it — and must move by exactly the batch once it is permanent.
#[test]
fn a_batch_bound_for_the_trash_frees_nothing_yet() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = settled(dir.path());
    let movies = find(&app.tree, "Movies");
    app.stage(movies);
    let bytes = app.staged_disk_bytes();
    assert!(bytes > 0);

    assert_eq!(app.disposal, Disposal::Trash, "the default disposal changed");
    let (after, before) = app.after_commit().expect("no free-space figure");
    assert_eq!(after, before, "promised space back for a batch that is only being trashed");
    assert_eq!(app.staged_to_trash(), bytes);

    app.disposal = Disposal::Permanent;
    let (after, before) = app.after_commit().expect("no free-space figure");
    assert_eq!(after - before, bytes);
    assert_eq!(app.staged_to_trash(), 0);
}

/// The confirm screen used to say "37M reclaimed" directly above "moved to the
/// Trash". It has to say which one is true.
#[test]
fn the_confirm_screen_does_not_call_a_trashed_batch_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = settled(dir.path());
    app.stage(find(&app.tree, "Movies"));
    app.review_batch();
    app.mode = Mode::Confirm;

    let out = render(&mut app, 100, 30);
    println!("{out}");
    for line in out.lines().filter(|l| l.contains("reclaimed")) {
        assert!(line.contains("only when the trash is emptied"), "called it reclaimed: {line}");
    }
    assert!(out.contains("moves to the Trash"), "{out}");
    assert!(out.contains("emptied (E)"), "{out}");

    app.disposal = Disposal::Permanent;
    let out = render(&mut app, 100, 30);
    println!("{out}");
    assert!(out.contains("reclaimed"), "{out}");
    assert!(out.contains("cannot be undone"), "{out}");
}
