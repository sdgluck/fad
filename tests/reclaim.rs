//! `--reclaim --yes` deletes with nobody watching, so what it will consider has
//! to be exactly what the UI offers, and nothing else.

use std::path::Path;

use fad::ignore::Rules;
use fad::reclaim;
use fad::scan::Scan;
use fad::scan::walk::ScanOpts;
use fad::tree::Tree;

fn fixture(root: &Path) {
    let mk = |rel: &str, size: usize| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![0u8; size]).unwrap();
    };
    mk("proj/Cargo.toml", 64);
    mk("proj/target/debug/lib.rlib", 3 * 1024 * 1024);
    mk("web/package.json", 64);
    mk("web/node_modules/dep/index.js", 6 * 1024 * 1024);
    // A `target` with no Cargo.toml beside it is somebody's directory, not a
    // build artifact.
    mk("notes/target/important.txt", 9 * 1024 * 1024);
}

fn scanned(root: &Path) -> Tree {
    let (mut tree, scan) = Scan::start(root, ScanOpts::default()).unwrap();
    scan.finish(&mut tree);
    tree
}

fn names(tree: &Tree, items: &[(u32, u64)]) -> Vec<String> {
    items
        .iter()
        .map(|(id, _)| {
            tree.path(*id).strip_prefix(tree.root_path()).unwrap().display().to_string()
        })
        .collect()
}

#[test]
fn only_what_the_rules_recognise_is_a_candidate() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let tree = scanned(dir.path());

    let items = reclaim::candidates(&tree, false, 0, &Rules::default());
    let found = names(&tree, &items);
    assert_eq!(found, vec!["web/node_modules", "proj/target"], "largest first, build dirs only");
    assert!(
        !found.iter().any(|n| n.starts_with("notes/")),
        "a target with no Cargo.toml beside it is not a build directory"
    );
}

#[test]
fn the_ignore_list_is_honoured_without_a_person_watching() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let tree = scanned(dir.path());

    let rules = Rules::parse(&format!("{}\n", tree.root_path().join("web").display()));
    let items = reclaim::candidates(&tree, false, 0, &rules);
    assert_eq!(names(&tree, &items), vec!["proj/target"]);
}

#[test]
fn the_cap_skips_what_would_overshoot_rather_than_stopping() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let tree = scanned(dir.path());
    let items = reclaim::candidates(&tree, false, 0, &Rules::default());

    // The largest is 6M. A 4M cap has to fall through to the 3M one rather than
    // give up at the first thing that does not fit.
    let capped = reclaim::under_cap(items.clone(), Some(4 * 1024 * 1024));
    assert_eq!(names(&tree, &capped), vec!["proj/target"]);

    let total: u64 = capped.iter().map(|(_, b)| b).sum();
    assert!(total <= 4 * 1024 * 1024, "the cap was exceeded");

    assert_eq!(reclaim::under_cap(items, None).len(), 2, "no cap means take everything");
}

/// A build directory the scan could not see all of is not handed to an
/// unattended delete — whether the unreadable part is the directory itself or
/// somewhere inside it.
#[test]
fn an_unreadable_candidate_or_corner_is_not_offered() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: a plain query.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root reads everything");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let locked = [dir.path().join("web/node_modules/dep"), dir.path().join("proj/target")];
    for p in &locked {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let tree = scanned(dir.path());
    for p in &locked {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let items = reclaim::candidates(&tree, false, 0, &Rules::default());
    assert!(items.is_empty(), "offered: {:?}", names(&tree, &items));
}

/// Something under a directory deleted this session is gone, even though only
/// the detached directory itself carries the mark.
#[test]
fn nothing_under_a_deleted_directory_is_offered() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let mut tree = scanned(dir.path());

    let web = tree
        .reclaimable
        .iter()
        .find(|id| tree.path(**id).ends_with("web/node_modules"))
        .and_then(|id| tree.node(*id).parent)
        .unwrap();
    tree.remove(web).unwrap();

    let items = reclaim::candidates(&tree, false, 0, &Rules::default());
    assert_eq!(names(&tree, &items), vec!["proj/target"]);
}
