//! Deletion is the one part of this tool that cannot be undone by rerunning it,
//! so it gets tested against the real Trash rather than a mock.

use std::path::{Path, PathBuf};

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
