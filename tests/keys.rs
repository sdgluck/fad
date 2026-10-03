//! What a key does depends on where you are, and getting that wrong costs a
//! session or a batch. These drive the real key handler, the same function the
//! event loop calls.

use std::path::Path;

use fad::app::{AgeFilter, App, Mode, View};
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

mod common;

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

fn app_for(root: &Path) -> App {
    let opts = ScanOpts::default();
    let (mut tree, scan) = Scan::start(root, opts.clone()).unwrap();
    scan.finish(&mut tree);
    let mut app = App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts);
    // The second scan is only there to satisfy the constructor; letting it
    // land would swap the tree out from under the test.
    app.scan = None;
    app
}

fn press(app: &mut App, code: KeyCode) {
    fad::run::on_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    app.rebuild_rows();
}

fn ctrl(app: &mut App, c: char) {
    fad::run::on_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    app.rebuild_rows();
}

fn typed(app: &mut App, s: &str) {
    for c in s.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn find(app: &App, rel: &str) -> fad::tree::NodeId {
    let tree = &app.tree;
    tree.find_path(&tree.root_path().join(rel)).unwrap_or_else(|| panic!("no node for {rel}"))
}

#[test]
fn esc_unwinds_one_layer_at_a_time_and_quits_only_at_the_top() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    // Every layer at once: an age filter, a view, and a kept fuzzy filter.
    app.age_filter = AgeFilter::D90;
    app.show_view(Some(View::Reclaim));
    press(&mut app, KeyCode::Char('/'));
    typed(&mut app, "cache");
    press(&mut app, KeyCode::Enter);
    assert!(app.mode == Mode::Normal);
    assert_eq!(app.filter, "cache");

    press(&mut app, KeyCode::Esc);
    assert!(app.filter.is_empty(), "the filter should go first");
    assert_eq!(app.view(), Some(View::Reclaim));
    assert!(!app.should_quit);

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.view(), None, "then the view");
    assert!(app.age_filter == AgeFilter::D90);
    assert!(!app.should_quit);

    press(&mut app, KeyCode::Esc);
    assert!(app.age_filter == AgeFilter::All, "then the age filter");
    assert!(!app.should_quit);

    press(&mut app, KeyCode::Esc);
    assert!(app.should_quit, "and only then quit");
}

#[test]
fn quitting_with_a_batch_staged_asks_first() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    let movies = find(&app, "Movies");
    app.stage(movies);

    press(&mut app, KeyCode::Char('q'));
    assert!(!app.should_quit, "one q threw a staged batch away");
    let msg = app.status.clone().unwrap_or_default();
    assert!(msg.contains("1 staged item will be forgotten"), "{msg}");

    // Any other key is "no", and does nothing else: `space` here must not
    // unstage the row the cursor happens to be on.
    app.cursor = app.rows.iter().position(|r| r.id == movies).unwrap();
    press(&mut app, KeyCode::Char(' '));
    assert!(!app.should_quit);
    assert!(app.staged.contains(&movies), "the cancelling key also acted");

    // Esc at the top level asks the same question, and q confirms it.
    press(&mut app, KeyCode::Esc);
    assert!(!app.should_quit);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_quit);
}

#[test]
fn ctrl_c_asks_too_but_quits_straight_away_with_nothing_staged() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    ctrl(&mut app, 'c');
    assert!(app.should_quit);

    let mut app = app_for(dir.path());
    let movies = find(&app, "Movies");
    app.stage(movies);
    ctrl(&mut app, 'c');
    assert!(!app.should_quit);
    ctrl(&mut app, 'c');
    assert!(app.should_quit);
}

#[test]
fn slash_reopens_a_kept_filter_to_refine_it() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    press(&mut app, KeyCode::Char('/'));
    typed(&mut app, "hol");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('/'));
    assert!(app.mode == Mode::Filter);
    assert_eq!(app.filter, "hol", "the kept query was thrown away");
    typed(&mut app, "iday");
    assert_eq!(app.filter, "holiday");
}

#[test]
fn ctrl_c_in_a_prompt_cancels_it_instead_of_typing_c() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    press(&mut app, KeyCode::Char('/'));
    typed(&mut app, "mov");
    ctrl(&mut app, 'c');
    assert!(app.mode == Mode::Normal);
    assert!(!app.filter.contains('c'), "ctrl-c typed a c: {:?}", app.filter);
    assert!(!app.should_quit, "ctrl-c in the prompt quit the session");

    press(&mut app, KeyCode::Char('f'));
    typed(&mut app, "hol");
    ctrl(&mut app, 'c');
    assert!(app.mode == Mode::Normal);
    assert!(app.search.is_empty());
    assert!(!app.should_quit);
}

/// `e` hands the terminal to the editor, which the key handler cannot do on
/// its own: it asks the event loop to, naming the file under the cursor.
#[test]
fn e_asks_for_the_editor_on_the_selected_path() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    let movies = find(&app, "Movies");
    app.cursor = app.rows.iter().position(|r| r.id == movies).unwrap();

    let effect = fad::run::on_key(&mut app, KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
    assert_eq!(effect, Some(fad::run::Effect::Edit(app.tree.path(movies))));
}

/// `y` commits on the confirmation screen. It must not also be the way into
/// it, or `y y` from the basket is a delete with nothing read in between.
#[test]
fn y_in_the_basket_does_not_head_for_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    let movies = find(&app, "Movies");
    app.stage(movies);

    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.mode == Mode::Basket, "y left the basket for the confirmation");
    press(&mut app, KeyCode::Enter);
    assert!(app.mode == Mode::Confirm, "enter is the way on");
}

/// The wheel over an open overlay scrolls its list; the tree underneath stays
/// where it was.
#[test]
fn the_wheel_scrolls_the_list_in_an_overlay() {
    use ratatui::crossterm::event::{MouseEvent, MouseEventKind};

    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    let root = app.tree.root();
    for c in app.tree.node(root).children.clone() {
        app.stage(c);
    }
    press(&mut app, KeyCode::Char('x'));
    let tree_cursor = app.cursor;
    let wheel = |kind| MouseEvent { kind, column: 10, row: 5, modifiers: KeyModifiers::NONE };

    fad::run::on_mouse(&mut app, wheel(MouseEventKind::ScrollDown));
    assert!(app.basket_cursor > 0, "the wheel did nothing in the basket");
    assert_eq!(app.cursor, tree_cursor, "the wheel moved the tree under the basket");
    fad::run::on_mouse(&mut app, wheel(MouseEventKind::ScrollUp));
    assert_eq!(app.basket_cursor, 0);
}

/// A heading carries the id of the first item under it, which may be in a
/// closed group the user cannot see. No single-item key may act on it — least
/// of all `i`, which would write that hidden item to the ignore file.
#[test]
fn single_item_keys_refuse_a_group_heading() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let _env = common::env_lock();
    common::isolate(dir.path());
    let mut app = app_for(dir.path());

    app.show_view(Some(View::Reclaim));
    app.rebuild_rows();
    assert!(app.rows.first().is_some_and(|r| r.header.is_some()), "no reclaimable heading to stand on");
    app.cursor = 0;

    for key in ['i', 'o', 'e', 'y'] {
        let effect =
            fad::run::on_key(&mut app, KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
        assert_eq!(effect, None, "{key} acted on a heading");
        let msg = app.status.clone().unwrap_or_default();
        assert!(msg.contains("group heading"), "{key} said {msg:?}");
    }
    let ignore = fad::ignore::path().unwrap();
    assert!(!ignore.exists(), "i wrote a hidden item to the ignore file");
}

#[test]
fn esc_closes_an_overlay_without_quitting() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    let movies = find(&app, "Movies");
    app.stage(movies);

    press(&mut app, KeyCode::Char('x'));
    assert!(app.mode == Mode::Basket);
    press(&mut app, KeyCode::Esc);
    assert!(app.mode == Mode::Normal);
    assert!(!app.should_quit);

    press(&mut app, KeyCode::Char('x'));
    ctrl(&mut app, 'c');
    assert!(app.mode == Mode::Normal, "ctrl-c should back out of the basket");
    assert!(!app.should_quit);
}
