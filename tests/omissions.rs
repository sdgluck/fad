//! What the scan did not count.
//!
//! Every total fad prints is only as good as its account of what it left out.
//! The banners say how many; this screen says which, and the distinction it
//! must never blur is between an entry missing from the totals and one that is
//! counted and merely hidden.

use std::path::Path;

use fad::app::{App, Why};
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;

mod common;

fn app_for(root: &Path) -> App {
    let opts = ScanOpts::default();
    let (mut tree, scan) = Scan::start(root, opts.clone()).unwrap();
    scan.finish(&mut tree);
    App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts)
}

fn mk(root: &Path, rel: &str, size: usize) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, vec![0u8; size]).unwrap();
}

/// A directory the walk could not read is missing from every total above it,
/// and its size is unknown — which is what "not counted" means, and why it must
/// never be reported as zero.
#[test]
fn an_unreadable_directory_is_listed_with_no_size() {
    let dir = tempfile::tempdir().unwrap();
    mk(dir.path(), "readable/file.bin", 4096);
    let locked = dir.path().join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("hidden.bin"), vec![0u8; 8192]).unwrap();

    // Root reads anything, so there is no such thing as an unreadable directory
    // to test against — as happens in the container the Linux suite runs in.
    if unsafe { libc::getuid() } == 0 {
        eprintln!("skipped: running as root, where no directory is unreadable");
        return;
    }

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut app = app_for(dir.path());
    // Put it back before any assertion can fail and leave it unremovable.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    app.collect_omissions();
    let found: Vec<_> = app.omissions.iter().filter(|o| o.why == Why::Unreadable).collect();
    assert_eq!(found.len(), 1, "the unreadable directory was not reported");
    assert!(found[0].path.ends_with("locked"));
    assert!(found[0].bytes.is_none(), "invented a size for something it could not read");

    let (uncounted, hidden, _) = app.omission_summary();
    assert_eq!((uncounted, hidden), (1, 0));
}

/// An ignored entry is the opposite case: it is in every total and only missing
/// from the view. The summary has to keep the two apart.
#[test]
fn an_ignored_entry_is_reported_as_hidden_rather_than_missing() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let root = dir.path().join("scan");
    mk(&root, "keep/file.bin", 4096);
    mk(&root, "vms/disk.img", 64 * 1024);

    let mut app = app_for(&root);
    // Through the tree's own path: the scan resolves the root, and on macOS a
    // temporary directory reaches it through a symlinked /var.
    let vms = app.tree.root_path().join("vms");
    app.ignore.add(&vms).unwrap();
    app.collect_omissions();

    let hidden: Vec<_> = app.omissions.iter().filter(|o| o.why == Why::Ignored).collect();
    assert_eq!(hidden.len(), 1, "the ignored entry was not reported: {}", app.omissions.len());
    assert!(hidden[0].path.ends_with("vms"));
    assert!(hidden[0].bytes.is_some(), "an ignored entry's size is known and should be shown");

    let (uncounted, count, bytes) = app.omission_summary();
    assert_eq!(uncounted, 0, "an ignored entry was counted as missing from the totals");
    assert_eq!(count, 1);
    assert!(bytes >= 64 * 1024, "hidden bytes: {bytes}");
    assert!(!Why::Ignored.uncounted(), "ignored must not read as uncounted");
}

/// An absolute rule covers everything beneath it. Listing all of that would
/// bury the handful of rules the user actually wrote.
#[test]
fn only_the_top_of_an_ignored_branch_is_listed() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let root = dir.path().join("scan");
    for i in 0..5 {
        mk(&root, &format!("vms/deep/nested{i}.img"), 1024);
    }

    let mut app = app_for(&root);
    // Through the tree's own path: the scan resolves the root, and on macOS a
    // temporary directory reaches it through a symlinked /var.
    let vms = app.tree.root_path().join("vms");
    app.ignore.add(&vms).unwrap();
    app.collect_omissions();

    let hidden: Vec<_> = app.omissions.iter().filter(|o| o.why == Why::Ignored).collect();
    assert_eq!(hidden.len(), 1, "listed every entry under an ignored branch: {}", hidden.len());
    assert!(hidden[0].path.ends_with("vms"));
}

/// A clean scan has to say so. An empty screen would read as a bug in the
/// screen rather than an absence of problems.
#[test]
fn a_scan_that_left_nothing_out_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    mk(dir.path(), "all/there.bin", 4096);

    let mut app = app_for(dir.path());
    app.collect_omissions();

    assert!(app.omissions.is_empty());
    assert_eq!(app.omission_summary(), (0, 0, 0));
}

/// A name that is not valid UTF-8 — possible on Linux, rejected by APFS — is a
/// name fad cannot carry. It holds names as text, so `Tree::path` would rebuild
/// a path that reaches nothing, and a delete aimed at it would miss or, if two
/// names flatten to the same text, hit the wrong entry. So the walk stops at
/// one, the screen says so, and nothing can stage it.
mod unnamed {
    use super::*;
    use fad::scan::meta::{Kind, Meta};
    use fad::scan::walk::{Batch, Entry, ROOT_ID, Skip};
    use fad::tree::{Tree, flags};

    fn meta(kind: Kind, len: u64) -> Meta {
        Meta { blocks: len, len, mtime: 0, dev: 1, ino: 7, nlink: 1, kind }
    }

    /// A tree of `root` holding one ordinary directory and one entry the walk
    /// refused to name. Built by hand because the filesystem under this test
    /// may not accept such a name at all.
    fn tree_with_a_bad_name(root: &Path) -> Tree {
        let mut tree = Tree::new(root.to_path_buf(), &meta(Kind::Dir, 0));
        tree.apply(Batch {
            parent: ROOT_ID,
            entries: vec![
                Entry {
                    name: "readable".into(),
                    meta: meta(Kind::Dir, 4096),
                    descend: None,
                    skip: None,
                },
                Entry {
                    // What `from_utf8_lossy` leaves behind.
                    name: "bad\u{fffd}name".into(),
                    meta: meta(Kind::Dir, 8192),
                    descend: None,
                    skip: Some(Skip::UnrepresentableName),
                },
            ],
            unreadable: None,
        });
        tree
    }

    fn app_with(tree: Tree, root: &Path) -> App {
        let opts = ScanOpts::default();
        let mut app = App::new(tree, Scan::start(root, opts.clone()).unwrap().1, opts);
        app.mark_dirty();
        app.rebuild_rows();
        app
    }

    #[test]
    fn it_is_flagged_and_never_descended_into() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree_with_a_bad_name(dir.path());

        let bad = tree
            .node(tree.root())
            .children
            .iter()
            .copied()
            .find(|c| tree.node(*c).flags & flags::UNNAMED != 0)
            .expect("the entry was not flagged");
        // Marked scanned so nothing goes looking for children it cannot reach.
        assert!(tree.node(bad).flags & flags::SCANNED != 0);
        assert_eq!(tree.skipped.len(), 1, "not recorded as something left out");
    }

    #[test]
    fn it_is_reported_as_missing_from_the_totals() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(tree_with_a_bad_name(dir.path()), dir.path());
        app.collect_omissions();

        let found: Vec<_> = app.omissions.iter().filter(|o| o.why == Why::Unnamed).collect();
        assert_eq!(found.len(), 1, "the unnamed entry was not reported");
        assert!(found[0].bytes.is_none(), "claimed to know what it holds");
        assert!(Why::Unnamed.uncounted(), "must not read as merely hidden");

        let (uncounted, hidden, _) = app.omission_summary();
        assert_eq!((uncounted, hidden), (1, 0));
    }

    #[test]
    fn it_cannot_reach_a_delete_batch() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_with(tree_with_a_bad_name(dir.path()), dir.path());
        let bad = app
            .tree
            .node(app.tree.root())
            .children
            .iter()
            .copied()
            .find(|c| app.tree.node(*c).flags & flags::UNNAMED != 0)
            .unwrap();

        // Staged directly, as a stale batch carried across a rescan could be.
        app.stage(bad);
        assert_eq!(app.staged.len(), 1);

        app.review_batch();
        assert!(app.staged.is_empty(), "an unnameable entry reached the batch");
        assert!(app.batch_items().is_empty());
        assert_eq!(app.refused.len(), 1);
        assert!(app.refused[0].1.contains("not valid text"), "{:?}", app.refused[0].1);
    }

    /// The real thing, where the filesystem allows it. APFS and HFS+ reject
    /// these names outright, so this can only run on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_one_is_stopped_at_rather_than_walked_into() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        mk(dir.path(), "fine/a.bin", 4096);
        let bad = dir.path().join(std::ffi::OsStr::from_bytes(b"bad\xffdir"));
        std::fs::create_dir(&bad).unwrap();
        std::fs::write(bad.join("inside.bin"), vec![0u8; 8192]).unwrap();

        let mut app = app_for(dir.path());
        app.collect_omissions();

        let found: Vec<_> = app.omissions.iter().filter(|o| o.why == Why::Unnamed).collect();
        assert_eq!(found.len(), 1, "a real unnameable directory was not reported");

        // It was stopped at, not walked into and not reported as unreadable.
        assert_eq!(app.tree.unreadable_count, 0, "reported as a permissions problem");
        assert!(
            app.omissions.iter().all(|o| o.why != Why::Unreadable),
            "misreported as unreadable"
        );
    }
}

/// Read permission without search permission: `readdir` lists the names and
/// `fstatat` refuses every one of them. That used to come out as an empty,
/// complete directory of 0 B — the one answer this screen exists to rule out.
#[test]
fn a_directory_that_lists_but_will_not_stat_is_reported() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let root = dir.path().join("scan");
    mk(&root, "shut/inside.bin", 64 * 1024);
    let shut = root.join("shut");

    if unsafe { libc::getuid() } == 0 {
        eprintln!("skipped: running as root, where every entry stats");
        return;
    }

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut app = app_for(&root);
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).unwrap();

    let id = app.tree.find_path(&app.tree.root_path().join("shut")).unwrap();
    assert_ne!(
        app.tree.node(id).flags & fad::tree::flags::UNREADABLE,
        0,
        "reported as complete when nothing in it could be measured"
    );
    assert_eq!(app.tree.unreadable_count, 1);
    assert_eq!(app.tree.unreadable_why[0].1, std::io::ErrorKind::PermissionDenied);

    app.collect_omissions();
    let found: Vec<_> = app.omissions.iter().filter(|o| o.why == Why::Unreadable).collect();
    assert_eq!(found.len(), 1, "not on the omissions screen");
    assert!(found[0].path.ends_with("shut"));
}

/// The same thing from a script: `--json` names it on stderr, with a reason.
#[test]
fn json_names_a_partly_unreadable_directory() {
    let dir = tempfile::tempdir().unwrap();
    mk(dir.path(), "shut/inside.bin", 4096);
    let shut = dir.path().join("shut");
    if unsafe { libc::getuid() } == 0 {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_fad"))
        .arg(dir.path())
        .arg("--json")
        .env("FAD_CACHE_DIR", dir.path().join("cache"))
        .env("FAD_STATE_DIR", dir.path().join("state"))
        .env("FAD_CONFIG_DIR", dir.path().join("config"))
        .output()
        .unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {err}");
    assert!(err.contains("could not be read"), "no warning: {err}");
    assert!(err.contains("/shut"), "the directory is not named: {err}");
    assert!(err.contains("permission denied"), "no reason given: {err}");
}

/// Full Disk Access is the remedy for a permission refusal and for nothing
/// else; a path too long to open gets told what it is.
#[test]
fn only_a_permission_refusal_is_told_to_grant_access() {
    use fad::tree::{PERMISSION_FIX, unreadable_reason};
    assert!(unreadable_reason(std::io::ErrorKind::PermissionDenied).contains(PERMISSION_FIX));
    let long = unreadable_reason(std::io::ErrorKind::InvalidFilename);
    assert!(!long.contains(PERMISSION_FIX), "{long}");
    assert!(long.contains("too long"), "{long}");
    assert!(!unreadable_reason(std::io::ErrorKind::NotFound).contains(PERMISSION_FIX));
}

/// Deeper than `PATH_MAX`: the kernel refuses the whole path, so the walk has
/// to reach it a component at a time. Built with `mkdirat` for the same reason.
#[test]
fn a_tree_deeper_than_path_max_is_walked_to_the_bottom() {
    use std::ffi::CString;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let cname = CString::new("d".repeat(200)).unwrap();
    // 30 levels of 201 bytes is past 4096, which is past every PATH_MAX.
    let levels = 30;
    let croot = CString::new(root.as_os_str().as_encoded_bytes()).unwrap();
    let mut fd = unsafe { libc::open(croot.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    assert!(fd >= 0);
    for _ in 0..levels {
        assert_eq!(unsafe { libc::mkdirat(fd, cname.as_ptr(), 0o755) }, 0);
        let next =
            unsafe { libc::openat(fd, cname.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
        unsafe { libc::close(fd) };
        assert!(next >= 0);
        fd = next;
    }
    let file = CString::new("bottom.bin").unwrap();
    let f = unsafe { libc::openat(fd, file.as_ptr(), libc::O_CREAT | libc::O_WRONLY, 0o644) };
    assert!(f >= 0);
    let buf = vec![7u8; 256 * 1024];
    assert_eq!(unsafe { libc::write(f, buf.as_ptr().cast(), buf.len()) }, buf.len() as isize);
    unsafe {
        libc::close(f);
        libc::close(fd);
    }

    let (mut tree, scan) = Scan::start(&root, ScanOpts::default()).unwrap();
    scan.finish(&mut tree);
    assert_eq!(tree.unreadable_count, 0, "{:?}", tree.unreadable_why);
    let r = tree.node(tree.root());
    assert_eq!(r.dir_count, levels as u64);
    assert_eq!(r.file_count, 1, "never reached the bottom");
    assert!(r.total_bytes >= 256 * 1024);
}

fn fad_json(root: &Path, extra: &[&str]) -> std::process::Output {
    let cache = tempfile::tempdir().unwrap();
    std::process::Command::new(env!("CARGO_BIN_EXE_fad"))
        .arg(root)
        .args(extra)
        .env("FAD_CACHE_DIR", cache.path())
        .env("FAD_STATE_DIR", cache.path().join("state"))
        .env("FAD_CONFIG_DIR", cache.path().join("config"))
        .output()
        .unwrap()
}

/// A root that cannot be read has no size to report. It used to print
/// `"bytes": 0` and exit 0 — an empty directory, as far as a script could tell.
#[test]
fn an_unreadable_root_is_an_error_not_zero_bytes() {
    if unsafe { libc::getuid() } == 0 {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o000, 0o644] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        mk(&root, "inside.bin", 4096);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(mode)).unwrap();
        let json = fad_json(&root, &["--json"]);
        let since = fad_json(&root, &["--since"]);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

        for out in [json, since] {
            let err = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(1), "mode {mode:o}: {err}");
            assert!(out.stdout.is_empty(), "mode {mode:o} printed a report");
            assert!(err.to_lowercase().contains("permission denied"), "mode {mode:o}: {err}");
        }
    }
}

/// Every kind of omission reaches `--json`'s stderr, not only the ones with a
/// flag to fix them.
#[cfg(target_os = "linux")]
#[test]
fn json_lists_entries_it_cannot_name() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join(std::ffi::OsStr::from_bytes(b"bad\xffdir"));
    std::fs::create_dir(&bad).unwrap();
    let out = fad_json(dir.path(), &["--json"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(err.contains("not valid UTF-8") && err.contains("bad"), "{err}");
}
