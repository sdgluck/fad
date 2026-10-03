//! `--since` answers a different question from `--json`, and has its own JSON
//! shape for it. The two flags used to be tested in the wrong order, so
//! `--since --json` printed a plain tree dump and the list of changes was
//! unreachable from a script.

use std::process::Command;

fn fad(dir: &std::path::Path, cache: &std::path::Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_fad"))
        .arg(dir)
        .args(args)
        .env("FAD_CACHE_DIR", cache)
        .env("FAD_STATE_DIR", cache.join("state"))
        .env("FAD_CONFIG_DIR", cache.join("config"))
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn since_with_json_prints_the_changes_not_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("x")).unwrap();
    std::fs::write(dir.path().join("x/a"), vec![0u8; 200 * 1024]).unwrap();

    // The first run has nothing to compare against; it leaves the baseline.
    let (_, err, code) = fad(dir.path(), cache.path(), &["--since"]);
    assert_eq!(code, 1, "expected the no-baseline exit code, stderr: {err}");

    std::fs::write(dir.path().join("x/b"), vec![0u8; 500 * 1024]).unwrap();

    let (out, err, code) = fad(dir.path(), cache.path(), &["--since", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("changes").is_some(), "not the --since shape: {out}");
    assert!(v.get("since").is_some(), "not the --since shape: {out}");
    // The plain tree dump's marker, which is what used to come out here.
    assert!(v.get("children").is_none(), "printed the tree instead: {out}");

    let changes = v["changes"].as_array().unwrap();
    let added = changes
        .iter()
        .find(|c| c["path"].as_str().unwrap().ends_with("/x/b"))
        .expect("the new file is not in the changes");
    assert_eq!(added["new"], serde_json::json!(true));
    assert!(added["delta"].as_i64().unwrap() >= 500 * 1024);
}

mod common;

fn scan(root: &std::path::Path, opts: fad::scan::walk::ScanOpts) -> fad::tree::Tree {
    let (mut tree, scan) = fad::scan::Scan::start(root, opts).unwrap();
    scan.finish(&mut tree);
    tree
}

fn the_snapshot(cache: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(cache)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "snap"))
        .expect("no snapshot was written")
}

/// The numbers depend on the options. A baseline taken without
/// `--cross-device` is not a baseline for a scan with it, and comparing the two
/// would report every mounted disk as growth.
#[test]
fn a_snapshot_taken_with_other_options_is_not_a_baseline() {
    use fad::scan::walk::ScanOpts;
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let root = dir.path().join("scan");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("f"), vec![0u8; 4096]).unwrap();

    let tree = scan(&root, ScanOpts::default());
    fad::cache::save(&tree).unwrap();
    assert!(fad::cache::load(&root, &ScanOpts::default()).is_some());
    let other = ScanOpts { cross_device: true, ..ScanOpts::default() };
    assert!(fad::cache::load(&root, &other).is_none(), "--cross-device read a plain baseline");
    let other = ScanOpts { cloud: true, ..ScanOpts::default() };
    assert!(fad::cache::load(&root, &other).is_none(), "--cloud read a plain baseline");
}

/// Same through the binary: `--since --cross-device` after a plain `--since`
/// has nothing to compare against.
#[test]
fn since_with_different_options_finds_no_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f"), vec![0u8; 4096]).unwrap();
    let (_, _, code) = fad(dir.path(), cache.path(), &["--since"]);
    assert_eq!(code, 1);
    let (_, _, code) = fad(dir.path(), cache.path(), &["--since"]);
    assert_eq!(code, 0, "the baseline was not found with the same options");
    let (_, err, code) = fad(dir.path(), cache.path(), &["--since", "--cross-device"]);
    assert_eq!(code, 1, "compared against a scan with other options: {err}");
}

/// "Since" means since the walk finished, not since the file was written. The
/// TUI saves on the way out, so a long session used to make its baseline look
/// as new as the moment it quit.
#[test]
fn the_baseline_is_dated_by_when_the_walk_finished() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let root = dir.path().join("scan");
    std::fs::create_dir_all(&root).unwrap();

    let mut tree = scan(&root, Default::default());
    let finished = std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_789);
    tree.mark_complete(finished);
    fad::cache::save(&tree).unwrap();
    let (_, at) = fad::cache::load(&root, &Default::default()).unwrap();
    assert_eq!(at, finished);
}

/// A snapshot from an older fad is read as no snapshot, not misread.
#[test]
fn an_old_format_snapshot_is_treated_as_absent() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let root = dir.path().join("scan");
    std::fs::create_dir_all(&root).unwrap();
    fad::cache::save(&scan(&root, Default::default())).unwrap();

    let snap = the_snapshot(&dir.path().join("cache/fad"));
    let mut bytes = std::fs::read(&snap).unwrap();
    bytes[4..8].copy_from_slice(&5u32.to_le_bytes());
    std::fs::write(&snap, &bytes).unwrap();
    assert!(fad::cache::load(&root, &Default::default()).is_none());

    // And one cut short is no better.
    std::fs::write(&snap, &bytes[..bytes.len() / 2]).unwrap();
    assert!(fad::cache::load(&root, &Default::default()).is_none());
}

/// `FAD_CACHE_DIR` names the directory itself. Pointed at a shared one, as
/// `FAD_CACHE_DIR=~/.cache` would be, `--clear-cache` used to delete all of it.
#[test]
fn clearing_the_cache_touches_only_what_fad_wrote() {
    let cache = tempfile::tempdir().unwrap();
    let shared = cache.path().join("shared");
    std::fs::create_dir_all(shared.join("someone-else")).unwrap();
    std::fs::write(shared.join("someone-else/data"), b"keep").unwrap();
    std::fs::write(shared.join("notes.txt"), b"keep").unwrap();
    std::fs::write(shared.join("home-0123.snap"), b"x").unwrap();
    std::fs::write(shared.join("home-0123.snap.tmp"), b"x").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_fad"))
        .arg("--clear-cache")
        .env("FAD_CACHE_DIR", &shared)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(shared.join("someone-else/data").exists(), "deleted another program's cache");
    assert!(shared.join("notes.txt").exists(), "deleted a file fad did not write");
    assert!(!shared.join("home-0123.snap").exists());
    assert!(!shared.join("home-0123.snap.tmp").exists());

    // Its own directory, holding only snapshots, goes entirely.
    let own = cache.path().join("own");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("a.snap"), b"x").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fad"))
        .arg("--clear-cache")
        .env("FAD_CACHE_DIR", &own)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!own.exists());
}

/// A scan root whose name is not UTF-8 — possible on Linux, refused by APFS.
/// `--json` panicked serialising its path, and the snapshot could not be saved
/// at all, so `--since` never had a baseline for it.
#[cfg(target_os = "linux")]
#[test]
fn a_root_that_is_not_utf8_reports_and_keeps_a_baseline() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let root = dir.path().join(std::ffi::OsStr::from_bytes(b"bad\xffroot"));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("f"), vec![0u8; 4096]).unwrap();

    let (out, err, code) = fad(&root, cache.path(), &["--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["path"].as_str().unwrap().contains("bad\u{fffd}root"));

    let (_, _, code) = fad(&root, cache.path(), &["--since"]);
    assert_eq!(code, 1);
    let (out, err, code) = fad(&root, cache.path(), &["--since", "--json"]);
    assert_eq!(code, 0, "no baseline was saved: {err}");
    assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok());
}

/// A cleanup is a change too. Paths only in the old scan were never visited,
/// because the walk went over the new tree alone.
#[test]
fn what_was_removed_is_reported_as_gone() {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("old/inner")).unwrap();
    std::fs::write(dir.path().join("old/inner/big"), vec![0u8; 300 * 1024]).unwrap();
    std::fs::write(dir.path().join("stays"), vec![0u8; 4096]).unwrap();

    let (_, err, code) = fad(dir.path(), cache.path(), &["--since"]);
    assert_eq!(code, 1);
    assert!(err.contains("saved one now"), "told to run fad first after saving: {err}");

    std::fs::remove_dir_all(dir.path().join("old")).unwrap();

    let (out, err, code) = fad(dir.path(), cache.path(), &["--since"]);
    assert_eq!(code, 0, "stderr: {err}");
    let line = out.lines().find(|l| l.ends_with("/old  (gone)")).unwrap_or_else(|| panic!("{out}"));
    assert!(line.starts_with('-'), "a removal reported as growth: {line}");
    // Once, at the top of what went, not for every file it held.
    assert!(!out.contains("inner"), "{out}");

    // The baseline moved on, so ask the same question against the old one
    // again through JSON.
    std::fs::create_dir_all(dir.path().join("old2")).unwrap();
    std::fs::write(dir.path().join("old2/f"), vec![0u8; 300 * 1024]).unwrap();
    fad(dir.path(), cache.path(), &["--since"]);
    std::fs::remove_dir_all(dir.path().join("old2")).unwrap();
    let (out, _, code) = fad(dir.path(), cache.path(), &["--since", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let gone = v["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["path"].as_str().unwrap().ends_with("/old2"))
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(gone["gone"], serde_json::json!(true));
    assert_eq!(gone["new"], serde_json::json!(false));
    assert!(gone["delta"].as_i64().unwrap() <= -300 * 1024);
}

/// Walked in step, by name: a wide directory where most entries are unchanged
/// still lines every one up with its old self.
#[test]
fn a_wide_directory_is_matched_entry_for_entry() {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("w")).unwrap();
    for i in 0..2000 {
        std::fs::write(dir.path().join(format!("w/{i}")), b"x").unwrap();
    }
    fad(dir.path(), cache.path(), &["--since"]);
    std::fs::write(dir.path().join("w/1999"), vec![0u8; 200 * 1024]).unwrap();
    let (out, err, code) = fad(dir.path(), cache.path(), &["--since", "--min-size", "100K"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.lines().any(|l| l.ends_with("/w/1999")), "{out}");
    assert!(!out.contains("(new)") && !out.contains("(gone)"), "lost track of a name: {out}");
}
