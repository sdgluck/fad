//! Duplicate detection decides which of two files a user is invited to delete.
//! Nearly-identical must never read as identical.

use fad::dupes::{self, Candidate};

fn candidate(path: &std::path::Path, id: u32, mtime: i64) -> Candidate {
    Candidate {
        id,
        path: path.to_path_buf(),
        bytes: std::fs::metadata(path).unwrap().len(),
        mtime,
    }
}

fn write(dir: &std::path::Path, name: &str, data: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

/// Big enough that the fingerprint pass cannot see the whole file, so the
/// verifying read is the thing under test.
fn body(fill: u8) -> Vec<u8> {
    let mut v = vec![fill; 300 * 1024];
    v[0] = b'h';
    *v.last_mut().unwrap() = b't';
    v
}

#[test]
fn identical_files_group_and_different_ones_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", &body(7));
    let b = write(dir.path(), "b.bin", &body(7));
    let c = write(dir.path(), "c.bin", &body(9));

    let report = dupes::find(vec![
        candidate(&a, 1, 100),
        candidate(&b, 2, 200),
        candidate(&c, 3, 300),
    ]);

    assert_eq!(report.groups.len(), 1, "expected exactly one group");
    let g = &report.groups[0];
    assert_eq!(g.ids.len(), 2);
    assert!(!g.ids.contains(&3), "a file with different contents was grouped");
    // Newest first, so "keep one" keeps the copy still in use.
    assert_eq!(g.ids[0], 2);
    assert_eq!(g.wasted(), g.bytes_each);
}

#[test]
fn same_size_and_same_ends_is_not_enough() {
    let dir = tempfile::tempdir().unwrap();
    // Identical first and last 64K, one byte apart in the middle: exactly the
    // pair the cheap fingerprint pass cannot tell apart.
    let mut one = body(7);
    let mut two = body(7);
    one[150 * 1024] = 1;
    two[150 * 1024] = 2;
    let a = write(dir.path(), "a.bin", &one);
    let b = write(dir.path(), "b.bin", &two);

    let report = dupes::find(vec![candidate(&a, 1, 100), candidate(&b, 2, 200)]);
    assert!(report.groups.is_empty(), "files differing mid-body were called duplicates");
}

#[test]
fn a_lone_file_of_its_size_is_never_read() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", &body(7));
    let b = write(dir.path(), "b.bin", &vec![1u8; 400 * 1024]);

    let report = dupes::find(vec![candidate(&a, 1, 100), candidate(&b, 2, 200)]);
    assert!(report.groups.is_empty());
    assert_eq!(report.bytes_read, 0, "sizes alone should have settled it");
}

/// Two files that already share their extents cost what one of them costs, so
/// deleting either frees nothing. Listing them as reclaimable would be inviting
/// the user to delete a file for no gain, which is the same reason hard links
/// never reach the candidate set.
#[test]
fn copies_that_already_share_their_storage_are_not_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.bin", &body(7));
    let b = write(dir.path(), "b.bin", &body(7));

    // Prove the pair is reported before they share anything, or the assertion
    // below would pass on a filesystem that simply found nothing.
    let before = dupes::find(vec![candidate(&a, 1, 100), candidate(&b, 2, 200)]);
    assert_eq!(before.groups.len(), 1, "the plain pair was not found in the first place");

    match fad::clone::share(&a, &b, std::fs::metadata(&a).unwrap().len()) {
        Ok(()) => {}
        Err(fad::clone::Refusal::Unsupported) => {
            eprintln!("skipped: this filesystem cannot share storage between files");
            return;
        }
        Err(e) => panic!("clone failed: {e}"),
    }

    let after = dupes::find(vec![candidate(&a, 1, 100), candidate(&b, 2, 200)]);
    assert!(
        after.groups.is_empty(),
        "a pair sharing one copy of its storage is still on offer to delete"
    );
}
