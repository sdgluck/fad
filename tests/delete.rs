//! Deletion is the one part of this tool that cannot be undone by rerunning it,
//! so it gets tested against the real Trash rather than a mock.

use std::path::{Path, PathBuf};

use fad::delete::{self, Disposal, Job};

mod common;

/// The journal lands just after the last outcome does. Wait for the write
/// rather than assume the two are the same moment.
fn wait_for_journal(batches: usize) {
    for _ in 0..2000 {
        if delete::read_journal().len() >= batches {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("journal never recorded {batches} batch(es)");
}

fn wait(job: &mut Job) {
    for _ in 0..2000 {
        job.poll();
        if job.is_finished() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("delete job never finished");
}

#[test]
fn permanent_delete_removes_files_and_trees() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file.bin");
    let tree = dir.path().join("tree");
    std::fs::write(&file, vec![0u8; 4096]).unwrap();
    std::fs::create_dir_all(tree.join("deep")).unwrap();
    std::fs::write(tree.join("deep/x.bin"), vec![0u8; 4096]).unwrap();

    let mut job = Job::start(
        vec![(file.clone(), 4096), (tree.clone(), 4096)],
        Disposal::Permanent,
    );
    wait(&mut job);

    assert_eq!(job.failures().len(), 0, "failures: {:?}", job.failures());
    assert!(!file.exists());
    assert!(!tree.exists());
    assert_eq!(job.freed(), 8192);
}

#[test]
fn trash_then_undo_puts_it_back() {
    let dir = tempfile::tempdir().unwrap();
    // Held for the whole test: the trash location is derived from HOME on
    // Linux, and the undo journal from FAD_STATE_DIR everywhere.
    let _env = common::env_lock();
    common::isolate(dir.path());
    let file = dir.path().join("fad-test-trashable.bin");
    std::fs::write(&file, vec![9u8; 8192]).unwrap();

    let mut job = Job::start(vec![(file.clone(), 8192)], Disposal::Trash);
    wait(&mut job);
    assert_eq!(job.failures().len(), 0, "trash failed: {:?}", job.failures());
    assert!(!file.exists(), "still in place after trashing");

    // The Trash URL macOS handed back is what makes this possible.
    let trashed: PathBuf = match &job.done[0].result {
        Ok(Some(p)) => p.clone(),
        other => panic!("no trash path reported: {other:?}"),
    };
    assert!(trashed.exists(), "not where macOS said it was: {}", trashed.display());

    let report = delete::undo_last().expect("undo failed");
    assert_eq!(report.restored, 1, "skipped: {:?}", report.skipped);
    assert!(file.exists(), "not restored");
    assert_eq!(std::fs::read(&file).unwrap().len(), 8192);
}

#[test]
fn guard_refuses_the_dangerous_paths() {
    // System directories differ per platform; the structural rules — the scan
    // root, its ancestors, and anything one level from `/` — do not.
    #[cfg(target_os = "macos")]
    let (root, systems) = (Path::new("/Users/someone/dev"), ["/", "/System", "/Users"]);
    #[cfg(not(target_os = "macos"))]
    let (root, systems) = (Path::new("/home/someone/dev"), ["/", "/usr", "/home"]);

    for bad in systems {
        assert!(delete::guard(Path::new(bad), root).is_err(), "guard let {bad} through");
    }
    for bad in [root, root.parent().unwrap()] {
        assert!(
            delete::guard(bad, root).is_err(),
            "guard let {} through",
            bad.display()
        );
    }
    assert!(delete::guard(&root.join("target"), root).is_ok());
}

/// The home directory is off limits however the environment describes it.
#[test]
fn guard_refuses_the_home_directory() {
    let dir = tempfile::tempdir().unwrap();
    let _env = common::env_lock();
    let home = dir.path().join("someone");
    std::fs::create_dir_all(home.join("project")).unwrap();
    common::isolate(&home);

    assert!(delete::guard(&home, &home.join("project")).is_err());
}

/// `u` reaches the top of the stack; the journal keeps twenty. Reaching past
/// the top has to restore the batch you picked and leave the others alone.
#[test]
fn any_remembered_batch_can_be_put_back() {
    let _guard = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let make = |name: &str| {
        let p = dir.path().join(name);
        std::fs::write(&p, vec![0u8; 1024]).unwrap();
        p
    };

    // Three separate commits, so the journal has three batches. The journal is
    // written after the last outcome is sent, so finishing the job is not the
    // same moment as the batch being recorded.
    for (i, name) in ["first", "second", "third"].iter().enumerate() {
        let p = make(name);
        let mut job = Job::start(vec![(p, 1024)], Disposal::Trash);
        wait(&mut job);
        wait_for_journal(i + 1);
    }

    let before = delete::read_journal();
    assert_eq!(before.len(), 3, "expected one batch per commit");
    let (_, trashed) = delete::still_in_trash();
    assert_eq!(trashed, 3 * 1024, "the trash total has to cover every batch");

    // The oldest, not the newest.
    let report = delete::undo_batch(before[0].id).expect("undo failed");
    assert_eq!(report.restored, 1);
    assert!(dir.path().join("first").exists(), "the chosen batch did not come back");
    assert!(!dir.path().join("third").exists(), "an untouched batch was restored too");

    let after = delete::read_journal();
    assert_eq!(after.len(), 2, "the restored batch should leave the journal");
    assert!(
        after.iter().all(|b| b.entries.iter().all(|e| !e.from.ends_with("first"))),
        "the restored batch is still on offer"
    );
}

// ------------------------------------------------------------ journal plumbing
//
// These drive the journal through `Recorder` with "trashed" items that live in
// a `.Trash` directory inside the scratch tree, so nothing here goes near the
// real Trash.

/// A file at `dir/name` "trashed" into `dir/.Trash/name`, the way a trash
/// implementation would leave it. Returns (original, trashed).
fn fake_trashed(dir: &Path, name: &str, len: usize) -> (PathBuf, PathBuf) {
    let trash = dir.join(".Trash");
    std::fs::create_dir_all(&trash).unwrap();
    let from = dir.join(name);
    let to = trash.join(name);
    std::fs::write(&to, vec![3u8; len]).unwrap();
    (from, to)
}

/// Each entry is in the journal as soon as it is recorded, not when the batch
/// ends — a fad killed mid-batch still has undo for what it already moved.
#[test]
fn entries_are_journalled_as_they_land() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let mut rec = delete::Recorder::new();
    let (a_from, a_to) = fake_trashed(dir.path(), "a", 10);
    rec.record(&a_from, &a_to, 10).unwrap();
    let j = delete::read_journal();
    assert_eq!(j.len(), 1);
    assert_eq!(j[0].entries.len(), 1, "the first entry was not written straight away");

    let (b_from, b_to) = fake_trashed(dir.path(), "b", 20);
    rec.record(&b_from, &b_to, 20).unwrap();
    let j = delete::read_journal();
    assert_eq!(j.len(), 1, "a second entry started a second batch");
    assert_eq!(j[0].entries.len(), 2);
    assert_eq!(j[0].bytes(), 30);
}

/// Two instances writing at once must not lose each other's batches: every
/// read-modify-write is under a lock.
#[test]
fn concurrent_writers_do_not_lose_batches() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let handles: Vec<_> = (0..8)
        .map(|t| {
            let base = dir.path().to_path_buf();
            std::thread::spawn(move || {
                let mut rec = delete::Recorder::new();
                for i in 0..5 {
                    let from = base.join(format!("t{t}-{i}"));
                    let to = base.join(".Trash").join(format!("t{t}-{i}"));
                    rec.record(&from, &to, 1).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let j = delete::read_journal();
    assert_eq!(j.len(), 8, "batches were lost");
    assert!(j.iter().all(|b| b.entries.len() == 5), "entries were lost");
    let mut ids: Vec<u64> = j.iter().map(|b| b.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 8, "two batches share an id");
}

/// Trimming keeps the newest twenty, and the rewrite leaves nothing behind.
#[test]
fn the_journal_is_trimmed_and_rewritten_cleanly() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    for i in 0..25 {
        let p = dir.path().join(format!("f{i}"));
        delete::Recorder::new().record(&p, &dir.path().join(".Trash/x"), i).unwrap();
    }
    let j = delete::read_journal();
    assert_eq!(j.len(), 20);
    assert_eq!(j.last().unwrap().entries[0].bytes, 24, "the newest batch was trimmed");
    let stray: Vec<_> = std::fs::read_dir(dir.path().join("state"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(stray.is_empty(), "temporary journal files left behind: {stray:?}");
}

/// The history screen can be stale by the time `U` lands. Restoring by id
/// still restores the batch that was under the cursor.
#[test]
fn a_batch_is_restored_by_id_from_a_stale_list() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    for name in ["one", "two", "three"] {
        let (from, to) = fake_trashed(dir.path(), name, 8);
        delete::Recorder::new().record(&from, &to, 8).unwrap();
    }
    let stale = delete::read_journal();

    // Something else restores the oldest batch; positions shift under `stale`.
    delete::undo_batch(stale[0].id).unwrap();
    // `three` was at position 2 — now out of range by position, still there by id.
    let r = delete::undo_batch(stale[2].id).expect("the stale id did not resolve");
    assert_eq!(r.restored, 1);
    assert!(dir.path().join("three").exists());
    assert!(!dir.path().join("two").exists(), "the wrong batch came back");
}

/// A journal written before batches carried ids still reads, and its batches
/// can still be put back.
#[test]
fn a_journal_from_before_ids_still_works() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let (from, to) = fake_trashed(dir.path(), "old", 4);
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    let line = serde_json::json!({
        "at": 1,
        "entries": [{ "from": from, "to": to, "bytes": 4 }],
    });
    std::fs::write(dir.path().join("state/undo.jsonl"), format!("{line}\n")).unwrap();

    let j = delete::read_journal();
    assert_eq!(j.len(), 1);
    assert_ne!(j[0].id, 0);
    assert_eq!(delete::read_journal()[0].id, j[0].id, "the derived id is not stable");
    let r = delete::undo_batch(j[0].id).unwrap();
    assert_eq!(r.restored, 1);
    assert!(from.exists());
}

/// A dangling symlink is something. `Path::exists` says otherwise, so undo
/// used to treat the slot as free and `rename` replaced the link.
#[test]
fn undo_does_not_replace_a_dangling_symlink() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let (from, to) = fake_trashed(dir.path(), "config", 8);
    delete::Recorder::new().record(&from, &to, 8).unwrap();
    std::os::unix::fs::symlink(dir.path().join("nowhere"), &from).unwrap();

    let report = delete::undo_last().unwrap();
    assert_eq!(report.restored, 0);
    assert_eq!(report.skipped.len(), 1);
    assert!(from.symlink_metadata().unwrap().file_type().is_symlink(), "the link was replaced");
    assert!(to.exists(), "the trashed item moved anyway");
}

/// The restore itself refuses to land on anything, so something arriving
/// between the check and the move is not replaced either.
#[test]
fn restore_never_replaces_what_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let (_, trashed) = fake_trashed(dir.path(), "a.txt", 4);
    let dest = dir.path().join("a.txt");
    std::fs::write(&dest, b"arrived meanwhile").unwrap();

    let err = fad::trash::restore(&trashed, &dest).expect_err("restored over a file");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&dest).unwrap(), b"arrived meanwhile");
    assert!(trashed.exists());

    // And over a dangling symlink.
    std::fs::remove_file(&dest).unwrap();
    std::os::unix::fs::symlink(dir.path().join("nowhere"), &dest).unwrap();
    assert!(fad::trash::restore(&trashed, &dest).is_err(), "restored over a dangling link");
    assert!(dest.symlink_metadata().unwrap().file_type().is_symlink());
}

/// An item that could not come back this time stays in the batch so it can
/// be tried again; the batch goes only once it is empty.
#[test]
fn an_item_that_could_not_be_restored_can_be_retried() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    let mut rec = delete::Recorder::new();
    let (a_from, a_to) = fake_trashed(dir.path(), "a", 4);
    let (b_from, b_to) = fake_trashed(dir.path(), "b", 4);
    rec.record(&a_from, &a_to, 4).unwrap();
    rec.record(&b_from, &b_to, 4).unwrap();
    std::fs::write(&b_from, b"in the way").unwrap();

    let report = delete::undo_last().unwrap();
    assert_eq!(report.restored, 1);
    assert_eq!(report.skipped.len(), 1);
    let j = delete::read_journal();
    assert_eq!(j.len(), 1, "the batch was dropped with an item still to restore");
    assert_eq!(j[0].entries.len(), 1);
    assert_eq!(j[0].entries[0].from, b_from, "the wrong entry was kept");

    std::fs::remove_file(&b_from).unwrap();
    let report = delete::undo_last().unwrap();
    assert_eq!(report.restored, 1, "{:?}", report.skipped);
    assert!(b_from.exists());
    assert!(delete::read_journal().is_empty(), "an empty batch stayed in the journal");
}

/// A cloud provider's folder inside a directory refuses the whole directory —
/// before anything in it is deleted, not when the walk reaches it.
#[cfg(target_os = "macos")]
#[test]
fn a_directory_holding_a_cloud_folder_is_refused_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("cache");
    std::fs::create_dir_all(tree.join("a/synced")).unwrap();
    std::fs::write(tree.join("a/first.bin"), b"x").unwrap();
    std::fs::write(tree.join("a/synced/doc.txt"), b"in the cloud").unwrap();
    let c = std::ffi::CString::new(tree.join("a/synced").as_os_str().as_encoded_bytes()).unwrap();
    let name = c"com.apple.file-provider-domain-id";
    // SAFETY: NUL-terminated strings and a buffer of the length passed.
    let rc = unsafe { libc::setxattr(c.as_ptr(), name.as_ptr(), b"x".as_ptr().cast(), 1, 0, 0) };
    assert_eq!(rc, 0);

    for disposal in [Disposal::Permanent, Disposal::Trash] {
        let mut job = Job::start(vec![(tree.clone(), 1)], disposal);
        wait(&mut job);
        let failures = job.failures();
        assert_eq!(failures.len(), 1, "{disposal:?} went ahead");
        assert!(failures[0].result.as_ref().unwrap_err().contains("cloud"));
        assert!(tree.join("a/first.bin").exists(), "{disposal:?} touched the tree before refusing");
        assert!(tree.join("a/synced/doc.txt").exists());
    }
}

/// What is under a directory that cannot be read cannot be vouched for.
#[test]
fn a_directory_with_an_unreadable_corner_is_refused_untouched() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: a plain query.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root reads everything");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("build");
    std::fs::create_dir_all(tree.join("locked")).unwrap();
    std::fs::write(tree.join("keep.bin"), b"x").unwrap();
    std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();

    let refused = delete::contained(&tree);
    std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(refused.is_err(), "an unreadable directory was vouched for");
}

/// A symlink to another filesystem is not a mount inside the tree: it is not
/// followed, and it does not refuse the delete.
#[test]
fn a_symlink_out_of_the_tree_is_not_a_mount() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("node_modules");
    std::fs::create_dir_all(&tree).unwrap();
    std::os::unix::fs::symlink("/dev", tree.join("devices")).unwrap();
    assert_eq!(delete::contained(&tree), Ok(()));
}

/// A trashed dangling symlink is still in the trash, and still recoverable.
#[test]
fn a_trashed_dangling_symlink_is_still_recoverable() {
    let _env = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());

    std::fs::create_dir_all(dir.path().join(".Trash")).unwrap();
    let to = dir.path().join(".Trash/link");
    std::os::unix::fs::symlink(dir.path().join("nowhere"), &to).unwrap();
    delete::Recorder::new().record(&dir.path().join("link"), &to, 0).unwrap();

    assert_eq!(delete::still_in_trash().0, 1, "a dangling link read as already emptied");
    let report = delete::undo_last().unwrap();
    assert_eq!(report.restored, 1, "{:?}", report.skipped);
}
