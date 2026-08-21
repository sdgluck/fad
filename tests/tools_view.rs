//! The tools view, staged and rendered, with no daemon anywhere.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use fad::app::App;
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;
use fad::tools::docker::{parse_totals, parse_verbose};
use fad::tools::{Backing, Freed, Kind, Report, Source, SourceReport, Status};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

mod common;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/").to_string() + name,
    )
    .expect("fixture")
}

/// The real captured Docker output, with a backing we choose so the caveat can
/// be tested both ways.
fn report(backing: Backing) -> Report {
    let totals = parse_totals(&fixture("docker_df.json"));
    let items = parse_verbose(&fixture("docker_df_verbose.json"), Source::Docker, &totals);
    Report {
        sources: vec![SourceReport { source: Source::Docker, status: Status::Ok, backing, items, totals }],
    }
}

fn vm_disk(shrinks: bool) -> Backing {
    Backing::VmDisk {
        disk: Some(PathBuf::from("/Users/nobody/Library/Containers/x/Docker.raw")),
        host_bytes: Some(73_000_000_000),
        shrinks,
    }
}

fn key(report: &Report, name: &str) -> fad::tools::ToolKey {
    report.items().find(|r| r.name == name).expect(name).key()
}

// ------------------------------------------------------------------ estimates

#[test]
fn one_image_frees_exactly_what_it_owns() {
    // `UniqueSize` is by definition the bytes only this image has, so removing
    // it alone gives back precisely that. Quoting a range here would overstate
    // it by the whole shared base.
    let r = report(Backing::Host);
    let mut set = BTreeSet::new();
    set.insert(key(&r, "<dangling>"));
    assert_eq!(fad::tools::freed(&r, &set), Freed::Exact(759_000_000));
}

#[test]
fn a_sharing_subset_is_a_floor_and_says_so() {
    // Both images share a 2.476GB base. Take both — which is every image there
    // is — and the base goes too, so the daemon's own image total is exact.
    let r = report(Backing::Host);
    let both: BTreeSet<_> =
        [key(&r, "<dangling>"), key(&r, "tinkerbell-capture:1.60.0")].into_iter().collect();
    assert_eq!(fad::tools::freed(&r, &both), Freed::Exact(3_995_000_000));

    // And that is more than their own bytes summed, which is the shared base
    // being released rather than double-counted.
    assert!(3_995_000_000u64 > 759_000_000u64 + 759_200_000u64);
}

#[test]
fn build_cache_does_not_borrow_the_kinds_total() {
    // The build-cache item is sized at what pruning gives back. Its kind total
    // also counts in-use cache, which pruning leaves alone — taking the total
    // would promise storage that is not going anywhere.
    let r = report(Backing::Host);
    let mut set = BTreeSet::new();
    set.insert(key(&r, "cold build cache"));
    assert_eq!(fad::tools::freed(&r, &set), Freed::Exact(573_600_000));
    let (size, reclaimable) =
        r.source(Source::Docker).unwrap().total(Kind::BuildCache).unwrap();
    assert_eq!((size, reclaimable), (573_600_000, 573_600_000));
}

#[test]
fn nothing_staged_frees_nothing() {
    let r = report(Backing::Host);
    assert_eq!(fad::tools::freed(&r, &BTreeSet::new()), Freed::Exact(0));
}

#[test]
fn only_idle_unheld_things_are_offered_to_a_script() {
    let r = report(Backing::Host);
    let names: Vec<String> = r
        .candidates(0)
        .iter()
        .filter_map(|k| r.get(k))
        .map(|x| x.name.clone())
        .collect();
    // Largest first, by what each frees on its own.
    assert_eq!(names, vec!["old-db", "<dangling>", "cold build cache", "orphan"]);
    // The running container, the image it uses, and the attached volume are
    // all absent, and they are the three things that would break something.
    for held in ["web", "tinkerbell-capture:1.60.0", "pgdata"] {
        assert!(!names.contains(&held.to_string()), "{held} was offered");
    }
}

// -------------------------------------------------------------------- the view

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

fn open_all(app: &mut App) {
    for kind in Kind::all() {
        app.tools_open.insert((Source::Docker, kind));
    }
    app.mark_dirty();
}

#[test]
fn the_view_shows_the_tools_totals_and_never_a_sum_of_rows() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(Backing::Host));
    open_all(&mut app);

    let out = render(&mut app, 100, 30);

    // The daemon's deduplicated figure, not the 6.5G the rows add up to.
    assert!(out.contains("3.7G"), "{out}");
    assert!(!out.contains("6.5G") && !out.contains("6.0G"), "a sum of rows leaked in:\n{out}");
    // Each row carries its own bytes and reports the shared ones separately.
    assert!(out.contains("724M"), "{out}");
    assert!(out.contains("shared"), "{out}");
    // Things in use are shown, and shown as untouchable.
    assert!(out.contains("running"), "{out}");
    assert!(out.contains("used by 1 container"), "{out}");
}

#[test]
fn a_vm_disk_that_does_not_shrink_is_called_out_and_one_that_does_is_not() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(vm_disk(false)));
    let out = render(&mut app, 100, 30);
    // Both halves survive the pane's width; the consequence is on the first
    // line precisely so that truncation cannot eat it.
    // Both halves survive the pane's width. The consequence leads precisely so
    // that truncation cannot eat it.
    assert!(out.contains("will not free space on your disk"), "the caveat is missing:\n{out}");
    assert!(out.contains("never shrinks"), "the caveat was truncated:\n{out}");

    // OrbStack trims its own disk, so the space really does come back and the
    // warning would be false.
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(vm_disk(true)));
    let out = render(&mut app, 100, 30);
    assert!(!out.contains("will not free space"), "{out}");
    assert!(out.contains("shrinks itself"), "{out}");

    // Native: nothing to caveat at all.
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(Backing::Host));
    let out = render(&mut app, 100, 30);
    assert!(!out.contains("shrink") && !out.contains("will not free space"), "{out}");
}

#[test]
fn a_tool_that_is_not_running_says_so_where_its_numbers_would_be() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let mut app = app_for(dir.path());
    app.install_tools_for_test(Report {
        sources: vec![SourceReport::empty(
            Source::Docker,
            Status::NotRunning("cannot connect".into()),
        )],
    });
    let out = render(&mut app, 100, 20);
    assert!(out.contains("installed but not running"), "{out}");
}

#[test]
fn something_in_use_cannot_be_staged() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(Backing::Host));
    open_all(&mut app);
    app.rebuild_rows();

    // Stage it anyway — as if the container had started after it was staged —
    // and confirm the review drops it rather than handing it to the daemon.
    let held = key(app.tools.as_ref().unwrap(), "web");
    app.staged_tools.insert(held.clone());
    app.review_tool_batch();

    assert!(!app.staged_tools.contains(&held));
    assert_eq!(app.tools_refused.len(), 1);
    assert_eq!(app.tools_refused[0].1, "running");
}

#[test]
fn a_staged_tool_item_is_a_batch_even_with_no_files_in_it() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(Backing::Host));
    app.staged_tools.insert(key(app.tools.as_ref().unwrap(), "<dangling>"));

    // `x` used to ask `staged.is_empty()`, which would have called this nothing.
    assert!(!app.nothing_staged());
    assert_eq!(app.tool_batch_items().len(), 1);

    let out = render(&mut app, 100, 24);
    assert!(out.contains("from tools"), "the staged badge is missing:\n{out}");
}

#[test]
fn vm_backed_bytes_never_reach_the_free_space_line() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    // Inside a disk image that does not shrink: freeing it moves nothing here.
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(vm_disk(false)));
    app.staged_tools.insert(key(app.tools.as_ref().unwrap(), "<dangling>"));
    assert_eq!(app.staged_tool_host_bytes(), 0);

    // Native: the same bytes really do come back.
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(Backing::Host));
    app.staged_tools.insert(key(app.tools.as_ref().unwrap(), "<dangling>"));
    assert_eq!(app.staged_tool_host_bytes(), 759_000_000);
}

#[test]
fn re_probing_drops_what_is_no_longer_there() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(Backing::Host));

    let gone = key(app.tools.as_ref().unwrap(), "<dangling>");
    app.staged_tools.insert(gone.clone());

    // An answer that no longer mentions it. Staging by id rather than by
    // position is what makes this drop the item instead of quietly restaging
    // whatever now sits where it used to.
    app.install_tools_for_test(Report {
        sources: vec![SourceReport::empty(Source::Docker, Status::Ok)],
    });
    app.review_tool_batch();
    assert!(app.staged_tools.is_empty());
    assert_eq!(app.tools_refused[0].1, "no longer there");
}

#[test]
fn the_basket_keeps_the_permanent_half_apart() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(vm_disk(false)));
    app.staged_tools.insert(key(app.tools.as_ref().unwrap(), "<dangling>"));
    app.staged_tools.insert(key(app.tools.as_ref().unwrap(), "orphan"));
    app.mode = fad::app::Mode::Basket;

    let out = render(&mut app, 100, 26);
    assert!(out.contains("tool storage"), "{out}");
    // The sentence that has to be on this screen and nowhere else.
    assert!(out.contains("no trash, no undo"), "{out}");
    // Two unshared-or-single items: an exact figure, not a floor.
    assert!(!out.contains("at least"), "{out}");
}

#[test]
fn the_confirm_screen_never_lets_the_two_halves_blur() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    std::fs::write(dir.path().join("junk.bin"), vec![0u8; 4096]).unwrap();

    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(vm_disk(false)));
    // One of each, which is the case the wording has to survive.
    let file = app.tree.find_path(&app.tree.root_path().join("junk.bin")).unwrap();
    app.stage(file);
    app.staged_tools.insert(key(app.tools.as_ref().unwrap(), "<dangling>"));
    app.review_batch();
    app.mode = fad::app::Mode::Confirm;

    let out = render(&mut app, 100, 30);
    // The trashable half offers undo...
    assert!(out.contains("Trash"), "{out}");
    assert!(out.contains("u puts them back"), "{out}");
    // ...and the tool half explicitly says D does not reach it.
    assert!(out.contains("no trash, no undo, whatever D says"), "{out}");
    // Plus the reason the free-space figure will not move.
    assert!(out.contains("free space will not move yet"), "{out}");
}

#[test]
fn a_batch_of_only_files_says_nothing_about_tools() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    std::fs::write(dir.path().join("junk.bin"), vec![0u8; 4096]).unwrap();

    let mut app = app_for(dir.path());
    let file = app.tree.find_path(&app.tree.root_path().join("junk.bin")).unwrap();
    app.stage(file);
    app.review_batch();
    app.mode = fad::app::Mode::Confirm;

    let out = render(&mut app, 100, 24);
    assert!(!out.contains("from tools"), "{out}");
    assert!(!out.contains("no undo"), "{out}");
}

/// Every row in this view carries the root's id as a placeholder, so the
/// cursor anchor has to key on more than that. It did not, and the effect was
/// that `j` and `k` did nothing at all: the keypress moved the cursor and the
/// rebuild that follows every keypress put it straight back.
#[test]
fn the_cursor_moves_and_stays_moved() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(Backing::Host));
    open_all(&mut app);
    app.rebuild_rows();

    // Headings and items both, so the walk crosses every row shape there is.
    assert!(app.rows.len() > 4, "{} rows", app.rows.len());
    assert!(app.rows.iter().all(|r| r.id == app.tree.root()), "ids are not placeholders");

    // What the event loop does: move, mark dirty, rebuild, draw.
    let step = |app: &mut App, to: usize| {
        app.cursor = to;
        app.mark_dirty();
        app.rebuild_rows();
        app.cursor
    };

    for i in 0..app.rows.len() {
        assert_eq!(step(&mut app, i), i, "the rebuild dragged the cursor off row {i}");
    }
    // And back up again.
    for i in (0..app.rows.len()).rev() {
        assert_eq!(step(&mut app, i), i, "the rebuild dragged the cursor off row {i}");
    }
}

/// The flip side: the anchor still has to do its job. A re-probe replaces the
/// report wholesale, and the cursor should come back to the same thing rather
/// than to the same row number.
#[test]
fn the_cursor_follows_its_row_across_a_re_probe() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let mut app = app_for(dir.path());
    app.install_tools_for_test(report(Backing::Host));
    open_all(&mut app);
    app.rebuild_rows();

    // Sit on a heading, and remember which one.
    let on = app.rows.iter().position(|r| r.header.is_some() && r.tool.is_none()).unwrap();
    app.cursor = on;
    let want = app.rows[on].header;

    app.install_tools_for_test(report(Backing::Host));
    open_all(&mut app);
    app.rebuild_rows();

    assert_eq!(app.rows[app.cursor].header, want);
}
