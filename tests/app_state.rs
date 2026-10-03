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

// ------------------------------------------- what the walk stopped at, staged

/// A tree with one of everything whose size on screen is not what deleting it
/// would remove: a volume mounted two levels down, a cloud folder, and a
/// directory that could not be read — beside two ordinary entries.
fn hazards(root: &Path) -> Tree {
    use fad::scan::meta::{Kind, Meta};
    use fad::scan::walk::{Batch, Entry, ROOT_ID, Skip};

    let meta = |kind: Kind, blocks: u64| Meta {
        blocks,
        len: blocks,
        mtime: 0,
        dev: 1,
        ino: 0,
        nlink: 1,
        kind,
    };
    let dir = |name: &str, descend: Option<u32>, skip: Option<Skip>| Entry {
        name: name.into(),
        meta: meta(Kind::Dir, 0),
        descend,
        skip,
    };
    let file = |name: &str| Entry {
        name: name.into(),
        meta: meta(Kind::File, 4096),
        descend: None,
        skip: None,
    };

    let mut tree = Tree::new(root.to_path_buf(), &meta(Kind::Dir, 0));
    tree.apply(Batch {
        parent: ROOT_ID,
        entries: vec![
            dir("mnt", Some(1), None),
            dir("Dropbox", None, Some(Skip::CloudStorage)),
            dir("locked", Some(2), None),
            dir("plain", Some(3), None),
            file("also.bin"),
        ],
        unreadable: None,
    });
    tree.apply(Batch { parent: 1, entries: vec![dir("deeper", Some(4), None)], unreadable: None });
    tree.apply(Batch {
        parent: 4,
        entries: vec![dir("backup", None, Some(Skip::OtherDevice))],
        unreadable: None,
    });
    tree.apply(Batch {
        parent: 2,
        entries: Vec::new(),
        unreadable: Some(std::io::ErrorKind::PermissionDenied),
    });
    tree.apply(Batch { parent: 3, entries: vec![file("a.bin")], unreadable: None });
    tree
}

fn hazard_app(root: &Path) -> App {
    let opts = ScanOpts::default();
    let tree = hazards(root);
    App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts)
}

/// Each of these shows as next to nothing, and deleting it would take a whole
/// volume, someone's synced folder, or contents nobody has seen. None of them
/// may reach the batch, and the status line has to say why.
#[test]
fn staging_refuses_mounts_cloud_folders_and_unreadable_directories() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = hazard_app(dir.path());

    for (rel, said) in [
        ("mnt/deeper/backup", "a mounted volume"),
        ("Dropbox", "a cloud folder"),
        ("locked", "could not read"),
    ] {
        let id = find(&app.tree, rel);
        app.status = None;
        assert!(!app.stage(id), "{rel} was staged");
        assert!(!app.staged.contains(&id));
        let status = app.status.clone().unwrap_or_default();
        assert!(status.contains(said), "{rel}: {status}");
    }

    // An ordinary directory and file still stage.
    assert!(app.stage(find(&app.tree, "plain")));
    assert!(app.stage(find(&app.tree, "also.bin")));
}

/// Staging a directory stages everything under it, so a mount three levels
/// down is as dangerous as the mount itself.
#[test]
fn staging_refuses_a_directory_with_a_mount_somewhere_beneath_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = hazard_app(dir.path());

    for rel in ["mnt", "mnt/deeper"] {
        let id = find(&app.tree, rel);
        assert!(!app.stage(id), "{rel} was staged with a volume mounted under it");
        let status = app.status.clone().unwrap_or_default();
        assert!(status.contains("contains a mounted volume"), "{status}");
        assert!(status.contains("won't delete across it"), "{status}");
    }
    assert!(app.staged.is_empty());

    // And the confirm step checks again, for a batch that got there some other
    // way — carried across a rescan by path, say.
    let mnt = find(&app.tree, "mnt");
    app.staged.insert(mnt);
    app.review_batch();
    assert!(app.staged.is_empty(), "the confirm step let a mount through");
    assert_eq!(app.refused.len(), 1);
}

/// `A` stages what it can, skips the rest, and says how many it skipped. A
/// second `A` still undoes the first even though some of the group never went
/// in.
#[test]
fn stage_all_skips_the_hazards_and_counts_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = hazard_app(dir.path());
    let group: Vec<_> =
        ["mnt", "Dropbox", "plain", "also.bin"].iter().map(|r| find(&app.tree, r)).collect();

    app.stage_all(group.clone());
    let plain = find(&app.tree, "plain");
    let also = find(&app.tree, "also.bin");
    assert_eq!(app.staged.len(), 2);
    assert!(app.staged.contains(&plain) && app.staged.contains(&also));
    let status = app.status.clone().unwrap_or_default();
    assert!(status.contains("skipped 2"), "{status}");

    app.stage_all(group);
    assert!(app.staged.is_empty(), "a second A did not undo the first");
}

// ------------------------------------------------ the batch across a tree swap

/// `R` used to clear the batch. It now carries it across as paths, re-stages
/// each one as the new walk finds it, and drops — out loud — whatever the walk
/// never finds.
#[test]
fn a_rescan_keeps_the_staged_batch_and_reports_what_vanished() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = settled(dir.path());

    let keep = ["Movies", "dev/fad/target"];
    for rel in keep {
        app.stage(find(&app.tree, rel));
    }
    let doomed = find(&app.tree, "Library/Caches/big.cache");
    app.stage(doomed);
    let want: Vec<_> = keep.iter().map(|r| app.tree.root_path().join(r)).collect();

    // Gone from disk behind fad's back, which is what R is for.
    std::fs::remove_file(dir.path().join("Library/Caches/big.cache")).unwrap();

    app.restart_scan().unwrap();
    while app.scanning() {
        app.poll_scan();
    }

    let mut got: Vec<_> = app.batch_items().into_iter().map(|(p, _)| p).collect();
    got.sort();
    let mut want = want;
    want.sort();
    assert_eq!(got, want, "the batch did not survive the rescan");
    let status = app.status.clone().unwrap_or_default();
    assert!(status.contains("1 staged item is gone"), "{status}");
}

/// A tree of the same root with padding entries ahead of the real ones, so no
/// path keeps the arena index it has in a plain scan.
fn shifted_snapshot(root: &Path) -> Tree {
    use fad::scan::meta::{Kind, Meta};
    use fad::scan::walk::{Batch, Entry};

    let dir_meta = Meta { blocks: 0, len: 0, mtime: 0, dev: 1, ino: 0, nlink: 1, kind: Kind::Dir };
    let real = scanned(root);
    let mut tree = Tree::new(real.root_path().to_path_buf(), &dir_meta);
    let mut entries: Vec<Entry> = (0..4)
        .map(|i| Entry { name: format!("pad-{i}").into(), meta: dir_meta, descend: None, skip: None })
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

/// Staging against a snapshot is fine; committing from one is deleting by
/// sizes that may be days old. The confirm step waits for the live walk, and
/// the batch is re-resolved against the live tree when it lands.
#[test]
fn a_batch_staged_on_a_snapshot_waits_for_the_live_scan() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut app = app_for(dir.path());
    assert!(app.install_snapshot(shifted_snapshot(dir.path())).is_ok());
    assert!(app.from_cache);

    let movies = find(&app.tree, "Movies");
    assert!(app.stage(movies));
    let path = app.tree.path(movies);
    app.mode = Mode::Basket;

    app.open_confirm();
    assert_eq!(app.mode, Mode::Basket, "the confirm step opened on a snapshot");
    let status = app.status.clone().unwrap_or_default();
    assert!(status.contains("wait for the scan to finish"), "{status}");
    // And commit refuses on its own account, however it was reached.
    app.commit();
    assert!(app.job.is_none(), "a batch was committed from a snapshot");

    while app.scanning() {
        app.poll_scan();
    }
    assert!(!app.from_cache);
    let live = app.tree.find_path(&path).unwrap();
    assert!(app.staged.contains(&live), "the batch was not re-resolved against the live tree");
    assert_eq!(app.staged.len(), 1);

    app.open_confirm();
    assert_eq!(app.mode, Mode::Confirm);
}
