//! Taking the trash out.
//!
//! Trashing reclaims nothing until the trash goes out, so this is the step that
//! makes every "reclaimed" figure fad has printed actually true. It is also
//! irreversible and it operates on paths read back off disk, so the two things
//! under test are that it removes exactly fad's own entries and that it refuses
//! anything that is not in a trash directory at all.

use std::path::PathBuf;

use fad::delete::{self, Disposal, Job};

mod common;

fn wait(job: &mut Job) {
    for _ in 0..2000 {
        job.poll();
        if job.is_finished() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("job never finished");
}

fn wait_for_journal(batches: usize) {
    for _ in 0..2000 {
        if delete::read_journal().len() >= batches {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("journal never recorded {batches} batch(es)");
}

/// Trash something, empty it, and check both halves: the file is gone from the
/// trash, and the batch that put it there is still remembered — as one that can
/// no longer be put back.
#[test]
fn emptying_removes_what_fad_trashed_and_leaves_the_record() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let file = dir.path().join("fad-test-emptyable.bin");
    std::fs::write(&file, vec![7u8; 16384]).unwrap();

    let mut job = Job::start(vec![(file.clone(), 16384)], Disposal::Trash);
    wait(&mut job);
    assert_eq!(job.failures().len(), 0, "trash failed: {:?}", job.failures());
    wait_for_journal(1);

    let trashed: PathBuf = match &job.done[0].result {
        Ok(Some(p)) => p.clone(),
        other => panic!("no trash path reported: {other:?}"),
    };
    assert!(trashed.exists());

    let entries = delete::trashed_entries();
    assert_eq!(entries.len(), 1, "fad's own trashed items: {entries:?}");
    assert_eq!(entries[0].0, trashed);
    assert_eq!(delete::still_in_trash(), (1, 16384));
    assert_eq!(delete::batches_still_restorable(), 1);

    let mut job = Job::erase(entries.into_iter().map(|(to, _, b)| (to, b)).collect());
    wait(&mut job);
    assert_eq!(job.failures().len(), 0, "erase failed: {:?}", job.failures());

    assert!(!trashed.exists(), "still in the trash after emptying");
    assert_eq!(delete::still_in_trash(), (0, 0), "the trash total did not come down");
    // The journal is not rewritten: the batch stays on the history screen and
    // starts reporting itself as emptied, which is exactly what happened to it.
    assert_eq!(delete::read_journal().len(), 1, "the batch was forgotten");
    assert_eq!(delete::batches_still_restorable(), 0);
    assert!(delete::undo_last().unwrap().skipped.len() == 1, "undo claimed it could put it back");
}

/// The one thing standing between a hand-edited or corrupted journal and an
/// unrecoverable delete of a live file.
#[test]
fn erase_refuses_anything_that_is_not_in_a_trash() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("precious.txt");
    std::fs::write(&live, b"do not delete me").unwrap();

    assert!(fad::trash::erase(&live).is_err(), "erased a path outside any trash");
    assert!(live.exists(), "erase removed a file it should have refused");

    // And the shapes it must accept, on both platforms' trash layouts.
    for good in [
        "/Users/someone/.Trash/thing.bin",
        "/Volumes/disk/.Trashes/501/thing.bin",
        "/home/someone/.local/share/Trash/files/thing.bin",
        "/mnt/data/.Trash-1000/files/thing.bin",
    ] {
        assert!(fad::trash::is_trash_path(std::path::Path::new(good)), "rejected {good}");
    }
    for bad in ["/Users/someone/dev/fad", "/home/someone/Trashcan/x", "/etc/passwd"] {
        assert!(!fad::trash::is_trash_path(std::path::Path::new(bad)), "accepted {bad}");
    }
}

/// A journal entry whose item has already been emptied by the desktop is not an
/// error and not a candidate: there is nothing left to take out.
#[test]
fn already_emptied_entries_are_not_offered() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let file = dir.path().join("fad-test-gone.bin");
    std::fs::write(&file, vec![1u8; 2048]).unwrap();
    let mut job = Job::start(vec![(file, 2048)], Disposal::Trash);
    wait(&mut job);
    wait_for_journal(1);

    let trashed = match &job.done[0].result {
        Ok(Some(p)) => p.clone(),
        other => panic!("no trash path reported: {other:?}"),
    };
    // Somebody else emptied the trash.
    std::fs::remove_file(&trashed).unwrap();

    assert!(delete::trashed_entries().is_empty(), "offered to empty something already gone");
    assert_eq!(delete::still_in_trash(), (0, 0));
}

/// A file "trashed" into a `.Trash` inside the scratch tree and journalled the
/// way `Job` journals it, so these tests never touch the real Trash.
fn fake_trashed(dir: &std::path::Path, name: &str) -> (PathBuf, PathBuf) {
    let trash = dir.join(".Trash");
    std::fs::create_dir_all(&trash).unwrap();
    let (from, to) = (dir.join(name), trash.join(name));
    std::fs::write(&to, b"what fad trashed").unwrap();
    delete::Recorder::new().record(&from, &to, 16).unwrap();
    (from, to)
}

fn erase(paths: Vec<PathBuf>) -> Job {
    let mut job = Job::erase(paths.into_iter().map(|p| (p, 16)).collect());
    wait(&mut job);
    job
}

/// Something else now sitting at the recorded trash path — the user emptied
/// the trash and trashed another file of the same name — is not fad's to erase.
#[test]
fn a_replaced_item_is_not_erased() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let (_, to) = fake_trashed(dir.path(), "report.pdf");
    std::fs::remove_file(&to).unwrap();
    std::fs::write(&to, b"somebody else's report").unwrap();

    let job = erase(vec![to.clone()]);
    let failures = job.failures();
    assert_eq!(failures.len(), 1, "erased something fad did not trash");
    assert!(
        failures[0].result.as_ref().unwrap_err().contains("no longer the thing fad trashed"),
        "wrong reason: {:?}",
        failures[0].result
    );
    assert_eq!(std::fs::read(&to).unwrap(), b"somebody else's report");
}

/// The same thing still there is erased as before.
#[test]
fn the_item_fad_trashed_is_erased() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let (_, to) = fake_trashed(dir.path(), "old.log");
    let job = erase(vec![to.clone()]);
    assert_eq!(job.failures().len(), 0, "{:?}", job.failures());
    assert!(to.symlink_metadata().is_err());
}

/// An entry from a journal that predates identities cannot be checked, and an
/// erase that cannot be checked does not happen.
#[test]
fn an_unverifiable_entry_is_not_erased() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let to = dir.path().join(".Trash/legacy.bin");
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::write(&to, b"x").unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    let line = serde_json::json!({
        "at": 1,
        "entries": [{ "from": dir.path().join("legacy.bin"), "to": to, "bytes": 1 }],
    });
    std::fs::write(dir.path().join("state/undo.jsonl"), format!("{line}\n")).unwrap();

    let job = erase(vec![to.clone()]);
    assert_eq!(job.failures().len(), 1);
    assert!(job.failures()[0].result.as_ref().unwrap_err().contains("older fad"));
    assert!(to.exists(), "erased an entry it could not verify");
}

/// A path the journal has never heard of is refused outright, even inside a
/// trash directory.
#[test]
fn a_path_fad_never_trashed_is_not_erased() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let stranger = dir.path().join(".Trash/stranger.bin");
    std::fs::create_dir_all(stranger.parent().unwrap()).unwrap();
    std::fs::write(&stranger, b"x").unwrap();

    let job = erase(vec![stranger.clone()]);
    assert_eq!(job.failures().len(), 1);
    assert!(stranger.exists());
}

/// Undo will not put back something that took the trashed item's place.
#[test]
fn undo_skips_a_replaced_item() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let (from, to) = fake_trashed(dir.path(), "notes.md");
    std::fs::remove_file(&to).unwrap();
    std::fs::write(&to, b"different").unwrap();

    let report = delete::undo_last().unwrap();
    assert_eq!(report.restored, 0);
    assert!(report.skipped[0].1.contains("no longer the thing fad trashed"));
    assert!(!from.exists(), "restored something fad did not trash");
    assert!(to.exists());
}
