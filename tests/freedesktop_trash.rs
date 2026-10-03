//! The FreeDesktop trash layout, checked against the spec rather than against
//! our own implementation's habits. Linux only; macOS has its own Trash.
#![cfg(all(unix, not(target_os = "macos")))]

use std::path::{Path, PathBuf};

use fad::trash;

mod common;

/// Redirect the trash at a scratch directory and return where it will be.
/// The returned guard must be held for the rest of the test.
fn isolate(dir: &Path) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
    let guard = common::env_lock();
    common::isolate(dir);
    (dir.join("share/Trash"), guard)
}

fn info_for(trashed: &Path) -> PathBuf {
    let name = trashed.file_name().unwrap().to_string_lossy();
    trashed
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("info")
        .join(format!("{name}.trashinfo"))
}

#[test]
fn trashing_writes_files_and_info_in_the_right_places() {
    let dir = tempfile::tempdir().unwrap();
    let (trash_root, _env) = isolate(dir.path());

    let file = dir.path().join("report.pdf");
    std::fs::write(&file, b"hello").unwrap();

    let landed = trash::trash(&file).expect("trash failed");
    assert!(!file.exists(), "original still in place");
    assert_eq!(landed, trash_root.join("files/report.pdf"));
    assert_eq!(std::fs::read(&landed).unwrap(), b"hello");

    let info = std::fs::read_to_string(info_for(&landed)).expect("no trashinfo written");
    assert!(info.starts_with("[Trash Info]\n"), "bad header: {info}");
    assert!(info.contains(&format!("Path={}", file.display())), "wrong Path: {info}");
    // YYYY-MM-DDThh:mm:ss
    let date = info
        .lines()
        .find_map(|l| l.strip_prefix("DeletionDate="))
        .expect("no DeletionDate");
    assert_eq!(date.len(), 19, "malformed DeletionDate: {date}");
    assert_eq!(&date[4..5], "-");
    assert_eq!(&date[10..11], "T");
}

#[test]
fn a_second_file_of_the_same_name_gets_a_distinct_slot() {
    let dir = tempfile::tempdir().unwrap();
    let (trash_root, _env) = isolate(dir.path());

    let mut landed = Vec::new();
    for i in 0..3 {
        let file = dir.path().join("notes.md");
        std::fs::write(&file, format!("copy {i}")).unwrap();
        landed.push(trash::trash(&file).expect("trash failed"));
    }

    // Distinct names, each with its own info file, and no lost content.
    assert_eq!(landed[0], trash_root.join("files/notes.md"));
    for (i, p) in landed.iter().enumerate() {
        assert!(info_for(p).exists(), "no trashinfo for {}", p.display());
        assert_eq!(std::fs::read_to_string(p).unwrap(), format!("copy {i}"));
    }
    let unique: std::collections::HashSet<_> = landed.iter().collect();
    assert_eq!(unique.len(), 3, "names collided: {landed:?}");
    // The suffix goes before the extension, so a restore keeps the file type.
    assert!(
        landed[1].to_string_lossy().ends_with(".md"),
        "extension lost: {}",
        landed[1].display()
    );
}

#[test]
fn restoring_puts_it_back_and_removes_the_info_file() {
    let dir = tempfile::tempdir().unwrap();
    let (_trash_root, _env) = isolate(dir.path());

    let file = dir.path().join("project/data.bin");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, vec![7u8; 4096]).unwrap();

    let landed = trash::trash(&file).unwrap();
    let info = info_for(&landed);
    assert!(info.exists());

    trash::restore(&landed, &file).expect("restore failed");
    assert!(file.exists(), "not restored");
    assert_eq!(std::fs::read(&file).unwrap().len(), 4096);
    assert!(!landed.exists(), "still in the trash");
    assert!(!info.exists(), "orphaned trashinfo left behind");
}

#[test]
fn directories_go_to_the_trash_whole() {
    let dir = tempfile::tempdir().unwrap();
    let (_trash_root, _env) = isolate(dir.path());

    let tree = dir.path().join("node_modules");
    std::fs::create_dir_all(tree.join("dep/nested")).unwrap();
    std::fs::write(tree.join("dep/nested/index.js"), b"x").unwrap();

    let landed = trash::trash(&tree).expect("trash failed");
    assert!(!tree.exists());
    assert!(landed.join("dep/nested/index.js").exists(), "subtree did not come along");
}

// ------------------------------------------------- per-filesystem trash dirs
//
// `$topdir` is usually writable by other users, so these are about what fad
// will and will not adopt there. Driven through `trash_in_topdir` against a
// scratch directory standing in for a mount's top.

use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn uid() -> u32 {
    // SAFETY: a plain query.
    unsafe { libc::getuid() }
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().mode() & 0o7777
}

#[test]
fn a_fresh_topdir_gets_a_private_trash_of_our_own() {
    let top = tempfile::tempdir().unwrap();
    let got = trash::trash_in_topdir(top.path(), uid()).unwrap();
    assert_eq!(got, top.path().join(format!(".Trash-{}", uid())));
    assert_eq!(mode(&got), 0o700, "anyone could read what was trashed");
}

#[test]
fn a_planted_symlink_is_never_adopted() {
    let top = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), top.path().join(format!(".Trash-{}", uid())))
        .unwrap();
    assert!(trash::trash_in_topdir(top.path(), uid()).is_err(), "followed a planted symlink");
}

#[test]
fn a_sticky_shared_trash_is_used_with_a_private_subdirectory() {
    let top = tempfile::tempdir().unwrap();
    let shared = top.path().join(".Trash");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();

    let got = trash::trash_in_topdir(top.path(), uid()).unwrap();
    assert_eq!(got, shared.join(uid().to_string()));
    assert_eq!(mode(&got), 0o700);
}

#[test]
fn a_shared_trash_that_fails_the_spec_falls_back_to_our_own() {
    let own = |top: &Path| top.join(format!(".Trash-{}", uid()));

    // Not sticky.
    let top = tempfile::tempdir().unwrap();
    std::fs::create_dir(top.path().join(".Trash")).unwrap();
    std::fs::set_permissions(top.path().join(".Trash"), std::fs::Permissions::from_mode(0o777))
        .unwrap();
    assert_eq!(trash::trash_in_topdir(top.path(), uid()).unwrap(), own(top.path()));

    // A symlink to a sticky directory.
    let top = tempfile::tempdir().unwrap();
    let real = tempfile::tempdir().unwrap();
    std::fs::set_permissions(real.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();
    std::os::unix::fs::symlink(real.path(), top.path().join(".Trash")).unwrap();
    assert_eq!(trash::trash_in_topdir(top.path(), uid()).unwrap(), own(top.path()));

    // Sticky, but our slot in it is a symlink someone planted.
    let top = tempfile::tempdir().unwrap();
    let shared = top.path().join(".Trash");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
    std::os::unix::fs::symlink(real.path(), shared.join(uid().to_string())).unwrap();
    assert_eq!(trash::trash_in_topdir(top.path(), uid()).unwrap(), own(top.path()));
}
