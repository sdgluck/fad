//! Sharing storage between identical files.
//!
//! Every assertion here is about the promise the feature makes: the second path
//! keeps working, nothing observable about it changes, and the space really
//! comes back. A filesystem with no clone operation makes most of this
//! untestable, so those tests say so and stop rather than pretend to pass.

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use fad::clone::{self, Refusal};

/// Skip the body when the filesystem underneath the temporary directory has no
/// clone operation — ext4, HFS+, a container's overlayfs. There is nothing to
/// test there and a failure would say the wrong thing.
macro_rules! needs_clones {
    ($result:expr) => {
        match $result {
            Err(Refusal::Unsupported) => {
                eprintln!("skipped: this filesystem cannot share storage between files");
                return;
            }
            other => other,
        }
    };
}

fn write(path: &std::path::Path, byte: u8, len: usize) {
    std::fs::write(path, vec![byte; len]).unwrap();
}

/// Two files with the same contents and no shared storage.
///
/// Deliberately not `std::fs::copy`: on macOS the standard library reaches for
/// `fclonefileat` first, so a copy made that way is already a clone and there
/// would be nothing left for these tests to prove.
fn two_copies(dir: &std::path::Path, byte: u8, len: usize) -> (std::path::PathBuf, std::path::PathBuf) {
    let (a, b) = (dir.join("keep.bin"), dir.join("other.bin"));
    write(&a, byte, len);
    write(&b, byte, len);
    (a, b)
}

#[test]
fn a_clone_keeps_the_file_and_gives_the_space_back() {
    let dir = tempfile::tempdir().unwrap();
    // Big enough that the allocation is visible against filesystem noise.
    let (keep, other) = two_copies(dir.path(), 0xAB, 8 << 20);

    // The destination's own identity, which the clone must not disturb.
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o640)).unwrap();
    let before = std::fs::metadata(&other).unwrap();
    let (mode, mtime) = (before.mode() & 0o7777, before.mtime());
    let mtime_nsec = before.mtime_nsec();

    needs_clones!(clone::share(&keep, &other, 8 << 20)).expect("clone failed");

    // The whole point: the path still works and holds what it held.
    let content = std::fs::read(&other).unwrap();
    assert_eq!(content.len(), 8 << 20);
    assert!(content.iter().all(|b| *b == 0xAB), "contents changed");

    let after = std::fs::metadata(&other).unwrap();
    assert_eq!(after.mode() & 0o7777, mode, "permissions changed");
    assert_eq!(after.mtime(), mtime, "modification time changed");
    // To the nanosecond. Both APFS and ext4 keep them, and anything watching
    // mtimes to decide what to rebuild reads them.
    assert_eq!(after.mtime_nsec(), mtime_nsec, "modification time lost its sub-second part");

    // And they are now one copy of the bytes, not two.
    assert!(clone::already_shared(&keep, &other), "not sharing after the clone");
}

/// A plain copy of the same bytes is not sharing anything, or the duplicate
/// hunt would have nothing left to find.
#[test]
fn a_plain_copy_is_not_reported_as_shared() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = two_copies(dir.path(), 0x11, 4 << 20);

    if clone::physical_start(&a).is_none() {
        eprintln!("skipped: this filesystem will not report extents");
        return;
    }
    assert!(!clone::already_shared(&a, &b), "two separate copies reported as shared");
}

/// The evidence that the contents are identical was gathered some time ago. If
/// either file has moved on since, the clone must not happen: it would replace
/// one file's contents with another's.
#[test]
fn a_file_that_changed_since_it_was_hashed_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let keep = dir.path().join("keep.bin");
    let other = dir.path().join("other.bin");
    write(&keep, 0x22, 1 << 20);
    write(&other, 0x33, 2 << 20);

    let err = clone::share(&keep, &other, 1 << 20).expect_err("cloned a mismatched pair");
    assert!(
        matches!(err, Refusal::Refused(ref why) if why.contains("changed")),
        "wrong refusal: {err}"
    );
    // And it left the destination exactly as it found it.
    assert_eq!(std::fs::metadata(&other).unwrap().len(), 2 << 20);
}

/// Cloning something onto itself would be a long way to delete a file.
#[test]
fn a_file_is_not_cloned_onto_itself() {
    let dir = tempfile::tempdir().unwrap();
    let only = dir.path().join("only.bin");
    write(&only, 0x44, 1 << 20);

    let err = clone::share(&only, &only, 1 << 20).expect_err("cloned a file onto itself");
    assert!(matches!(err, Refusal::Refused(_)), "wrong refusal: {err}");
    assert_eq!(std::fs::metadata(&only).unwrap().len(), 1 << 20);
}

/// Doing it twice is not an error worth reporting as a failure, but it is not
/// work either — and it must not claim to have freed anything a second time.
#[test]
fn an_already_shared_pair_is_refused_rather_than_redone() {
    let dir = tempfile::tempdir().unwrap();
    let (keep, other) = two_copies(dir.path(), 0x55, 2 << 20);

    needs_clones!(clone::share(&keep, &other, 2 << 20)).expect("clone failed");
    let err = clone::share(&keep, &other, 2 << 20).expect_err("cloned an already shared pair");
    assert!(
        matches!(err, Refusal::Refused(ref why) if why.contains("sharing")),
        "wrong refusal: {err}"
    );
}

/// The clone is built beside the destination and renamed over it, so a failure
/// must not leave the working directory littered with half-written files.
#[test]
fn nothing_is_left_behind() {
    let dir = tempfile::tempdir().unwrap();
    let (keep, other) = two_copies(dir.path(), 0x66, 1 << 20);

    needs_clones!(clone::share(&keep, &other, 1 << 20)).expect("clone failed");

    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".fad-clone-"))
        .collect();
    assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
}
