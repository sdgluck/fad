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

/// The overview block that says where inside the selection the size is, so
/// finding the one child that matters does not mean expanding the tree by hand.
#[test]
fn the_detail_pane_says_where_the_size_is_concentrated() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    // The root has three children, ranked Movies 9M, dev 8M, Library 3M.
    app.cursor = 0;
    app.rebuild_rows();

    let out = render(&mut app, 100, 30);
    println!("{out}");

    assert!(out.contains("where it goes"), "no concentration block:\n{out}");
    let block = out.split("where it goes").nth(1).unwrap();
    let rows: Vec<&str> = block.lines().skip(1).take(3).collect();
    assert!(rows[0].contains("Movies"), "biggest child is not first: {rows:?}\n{out}");
    assert!(rows[1].contains("dev"), "children out of order: {rows:?}\n{out}");
    assert!(rows[2].contains("Library"), "children out of order: {rows:?}\n{out}");
    // 9M of 20M, and the share is what makes it worth opening.
    assert!(rows[0].contains("44%"), "no share of the selection: {rows:?}\n{out}");
}

/// The root is the whole scan by definition, and it has no parent to be a share
/// of. Both facts have to be said correctly rather than divided by zero.
#[test]
fn share_of_the_scan_is_everything_at_the_root_and_a_fraction_below_it() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    app.cursor = 0;
    app.rebuild_rows();
    let out = render(&mut app, 100, 30);
    assert!(out.contains("100% of scan"), "root is not the whole scan:\n{out}");
    assert!(!out.contains("of parent"), "the root was given a parent:\n{out}");

    // Directly under the root, the parent share would be the same number twice.
    app.cursor = 1;
    app.rebuild_rows();
    let out = render(&mut app, 100, 30);
    println!("{out}");
    assert!(!out.contains("100% of scan"), "a child is the whole scan:\n{out}");
    assert!(!out.contains("of parent"), "the root was quoted twice:\n{out}");

    // A grandchild has two genuinely different shares. dev/fad is 8M of dev's
    // 8M but only 40% of the scan.
    let root = app.tree.root();
    let dev = *app.tree.node(root).children.iter()
        .find(|c| app.tree.node(**c).name.as_ref() == "dev").unwrap();
    app.expanded.insert(dev);
    app.mark_dirty();
    app.rebuild_rows();
    app.cursor = (0..app.rows.len())
        .find(|i| app.tree.node(app.rows[*i].id).name.as_ref() == "fad")
        .expect("dev/fad is not on screen");
    let out = render(&mut app, 100, 30);
    println!("{out}");
    assert!(out.contains("of parent"), "no share of the parent:\n{out}");
}

/// The pane fills itself: as many breakdowns as the rows allow, in a fixed
/// order, and `S` moves that order round so the ones that did not fit can be
/// reached.
#[test]
fn the_detail_pane_shows_every_breakdown_that_fits() {
    use fad::app::Panel;

    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    app.cursor = 0;
    app.rebuild_rows();

    // Tall enough for all four, so all four are there and none is a top-N.
    let out = render(&mut app, 100, 40);
    println!("{out}");
    for section in ["by extension", "by age", "biggest files", "file sizes"] {
        assert!(out.contains(section), "{section} did not fit:\n{out}");
    }
    assert!(out.contains("huge.rlib"), "the biggest files are not listed:\n{out}");
    // Nothing is held back, so there is nothing for the key to reveal.
    assert!(!out.contains("\u{b7} S"), "a full pane offered more:\n{out}");

    // Short enough that only some fit. The leader is the extensions, and the
    // pane says so rather than quietly dropping the rest.
    let short = render(&mut app, 100, 26);
    println!("{short}");
    assert!(short.contains("\u{b7} S"), "a cut pane offered nothing:\n{short}");
    let head = short.split("── ").nth(2).unwrap();
    assert!(head.starts_with("by extension"), "wrong leader: {head}");
    assert!(!short.contains("file sizes"), "everything fit after all:\n{short}");

    // S moves the order round, so what did not fit comes to the front.
    app.cycle_panel();
    assert_eq!(app.panel, Panel::Ages);
    let next = render(&mut app, 100, 26);
    println!("{next}");
    let head = next.split("── ").nth(2).unwrap();
    assert!(head.starts_with("by age"), "S did not move the order on: {head}");

    // Every breakdown is reachable: whatever did not fit at first is on screen
    // within a lap of the key, which is the whole promise the hint makes.
    let mut seen = short.contains("file sizes") || next.contains("file sizes");
    for _ in 0..2 {
        app.cycle_panel();
        seen |= render(&mut app, 100, 26).contains("file sizes");
    }
    assert!(seen, "a breakdown could not be reached by pressing S");

    // Four presses come back to where it started.
    app.cycle_panel();
    assert_eq!(app.panel, Panel::Extensions);
}

/// A file has no children, so there is nothing to say about where its size is.
#[test]
fn a_file_gets_no_concentration_block() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alone.bin"), vec![0u8; 4096]).unwrap();
    let mut app = app_for(dir.path());
    app.cursor = 1;
    app.rebuild_rows();
    assert_eq!(app.tree.node(app.selected().unwrap()).name.as_ref(), "alone.bin");

    let out = render(&mut app, 100, 24);
    println!("{out}");
    assert!(!out.contains("where it goes"), "a file was broken down by child:\n{out}");
    assert!(!out.contains("of parent") || out.contains("100% of parent"));
}

/// The biggest-files panel is the one that tells a single huge file apart from
/// a directory of small ones, so its order has to be exactly right, and the
/// size classes have to account for every file in the subtree.
#[test]
fn the_breakdown_ranks_the_biggest_files_and_classes_all_of_them() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    app.cursor = 0;
    app.rebuild_rows();
    app.ensure_breakdown();

    let b = app.breakdown.as_ref().unwrap();
    let names: Vec<&str> = b.biggest.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(&names[..2], &["holiday.mov", "huge.rlib"], "not largest first: {names:?}");
    assert!(b.biggest.windows(2).all(|w| w[0].bytes >= w[1].bytes), "{names:?}");

    let counted: u64 = b.sizes.iter().map(|(_, c)| c).sum();
    assert_eq!(counted, app.tree.node(app.tree.root()).file_count, "files went unclassed");
}

/// With more children than the block can list, the three rows are only half the
/// answer: whether they are the whole problem is the other half.
#[test]
fn a_directory_of_many_children_reports_what_the_top_three_hold() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.bin"), vec![0u8; 8 * 1024 * 1024]).unwrap();
    for i in 0..6 {
        std::fs::write(dir.path().join(format!("small{i}.bin")), vec![0u8; 4096]).unwrap();
    }
    let mut app = app_for(dir.path());
    app.cursor = 0;
    app.rebuild_rows();

    let out = render(&mut app, 100, 30);
    println!("{out}");
    assert!(out.contains("top 3 of 7 hold"), "no verdict line:\n{out}");
    // One file is essentially all of it, so the verdict has to say so.
    assert!(out.contains("top 3 of 7 hold 99%"), "wrong share:\n{out}");
}

/// The pane does not scroll. On a short terminal the breakdowns are what give
/// way, never the path, the size, or where it goes — and a cut list says how
/// much of itself is showing rather than passing a top-N off as the whole
/// thing.
#[test]
fn a_short_pane_cuts_the_breakdowns_and_says_it_cut_them() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    app.cursor = 0;
    app.rebuild_rows();

    let out = render(&mut app, 100, 22);
    println!("{out}");
    assert!(out.contains("on disk"), "the size was clipped:\n{out}");
    assert!(out.contains("where it goes"), "the overview was clipped:\n{out}");
    assert!(out.contains("top 5 of 6"), "a cut list claimed to be whole:\n{out}");
    // Nothing may spill past the pane into the staged box below it.
    assert!(out.contains("staged"), "the staged box was pushed off:\n{out}");

    // With the room for it, the same list is whole and says nothing about tops.
    let tall = render(&mut app, 100, 40);
    assert!(tall.contains("by extension"), "no breakdown:\n{tall}");
    assert!(!tall.contains("top 5 of 6"), "a whole list claimed to be cut:\n{tall}");
}

/// The screen between a keystroke and the last recoverable copy of something.
/// It has to state all three consequences: the space arrives, the undo stops
/// working, and nobody else's trash is touched.
#[test]
fn the_empty_trash_screen_states_what_it_costs() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    fixture(dir.path());
    let mut app = app_for(dir.path());

    app.trash_pending = (3, 40 << 30);
    app.mode = fad::app::Mode::EmptyTrash;

    let out = render(&mut app, 100, 24);
    println!("{out}");
    assert!(out.contains("empty the trash"), "no title:\n{out}");
    assert!(out.contains("40G"), "the amount is missing:\n{out}");
    assert!(out.contains("cannot be undone"), "no warning that this is final:\n{out}");
    assert!(out.contains("left where it is"), "does not say other trash is spared:\n{out}");
}

/// A scan total on its own does not say whether it matters. The header has to
/// carry the denominator the user is actually trying to move.
#[test]
fn the_header_says_what_is_left_on_the_volume() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    app.poll_volume();

    let out = render(&mut app, 140, 20);
    println!("{out}");
    assert!(out.contains("free of"), "no free-space figure in the header:\n{out}");
    assert!(out.contains("dirs,"), "the entry count was dropped on a wide pane:\n{out}");

    // Narrow enough that the two cannot both fit. The count is context; the
    // free figure is the number the user is trying to move, so it is the one
    // that stays.
    let narrow = render(&mut app, 76, 20);
    println!("{narrow}");
    assert!(narrow.contains("free of"), "the free figure was dropped first:\n{narrow}");
}

/// With --cross-device the tree spans filesystems and one free-space figure
/// cannot describe all of them. Say so rather than let it read as the total.
#[test]
fn a_cross_device_scan_says_the_free_figure_is_one_volumes() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    app.opts.cross_device = true;
    app.poll_volume();

    let out = render(&mut app, 100, 20);
    println!("{out}");
    assert!(out.contains("crosses filesystems"), "no caveat for a multi-volume scan:\n{out}");
}

/// The find overlay shows paths, not names: two hundred things called
/// `node_modules` are told apart by where they are and nothing else.
#[test]
fn the_find_overlay_shows_where_each_hit_is() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());

    app.mode = fad::app::Mode::Search;
    app.search = "rlib".into();
    app.run_search();

    let out = render(&mut app, 100, 24);
    println!("{out}");
    assert!(out.contains("find"), "no title:\n{out}");
    assert!(out.contains("dev/fad/target/debug/huge.rlib"), "no path for the hit:\n{out}");
    assert!(out.contains("go there"), "no footer:\n{out}");
}

/// The screen that answers "why is this smaller than the Finder says". Each
/// heading has to carry the flag or the permission that would fix it, and the
/// two kinds — missing from the totals, and merely hidden — must not blur.
#[test]
fn the_omissions_screen_says_what_would_fix_each_kind() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let locked = dir.path().join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("x.bin"), vec![0u8; 4096]).unwrap();

    // Root reads anything; there is no unreadable directory to render.
    if unsafe { libc::getuid() } == 0 {
        eprintln!("skipped: running as root, where no directory is unreadable");
        return;
    }

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut app = app_for(dir.path());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    app.collect_omissions();
    app.mode = fad::app::Mode::Omissions;

    let out = render(&mut app, 100, 24);
    println!("{out}");
    assert!(out.contains("not in these numbers"), "no title:\n{out}");
    assert!(out.contains("could not be read"), "no heading for the unreadable group:\n{out}");
    assert!(out.contains("locked"), "the path is missing:\n{out}");
    assert!(out.contains("is short by whatever it holds"), "does not say the totals are wrong:\n{out}");
}
