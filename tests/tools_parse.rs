//! Turning what Docker says into what fad shows, with no daemon anywhere.
//!
//! The fixtures are real `docker system df` output from a working machine, with
//! containers and volumes added by hand because the machine had none. Every
//! size in them is a string Docker actually emits.

use fad::tools::docker::{backing_for, parse_si, parse_totals, parse_verbose};
use fad::tools::{Backing, Kind, Measure, Source};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/").to_string() + name)
        .expect("fixture")
}

#[test]
fn sizes_are_read_as_si() {
    // Docker counts in powers of ten. Reading `3.24GB` as 3.24 * 2^30 would
    // overstate it by 7%, and every figure in the view would be wrong.
    assert_eq!(parse_si("3.24GB"), Some(3_240_000_000));
    assert_eq!(parse_si("759.2MB"), Some(759_200_000));
    assert_eq!(parse_si("0B"), Some(0));
    assert_eq!(parse_si("17.6MB"), Some(17_600_000));

    // The shapes with a gloss after the number: the leading figure is the one
    // that belongs to the item.
    assert_eq!(parse_si("1.518GB (38%)"), Some(1_518_000_000));
    assert_eq!(parse_si("0B (virtual 3.24GB)"), Some(0));

    // A binary unit read as SI would be the same 7% lie in reverse.
    assert_eq!(parse_si("1GiB"), Some(1_073_741_824));

    // No size is not zero, and must not become it.
    assert_eq!(parse_si("N/A"), None);
    assert_eq!(parse_si(""), None);
    assert_eq!(parse_si("lots"), None);
}

#[test]
fn totals_come_from_the_daemons_own_figures() {
    let t = parse_totals(&fixture("docker_df.json"));

    assert_eq!(t.iter().find(|(k, _, _)| *k == Kind::Image).map(|(_, s, r)| (*s, *r)),
               Some((3_995_000_000, 759_200_000)));
    assert_eq!(t.iter().find(|(k, _, _)| *k == Kind::BuildCache).map(|(_, s, r)| (*s, *r)),
               Some((573_600_000, 573_600_000)));
    assert_eq!(t.len(), 4);
}

/// The regression this whole module exists to prevent.
///
/// The two images in the fixture each report 3.24GB and share a 2.476GB base.
/// Adding their reported sizes gives 6.48GB for 3.995GB of actual storage. A
/// heading that showed the sum would be overstating the disk by 62%.
#[test]
fn a_heading_total_is_never_the_sum_of_its_items() {
    let totals = parse_totals(&fixture("docker_df.json"));
    let items = parse_verbose(&fixture("docker_df_verbose.json"), Source::Docker, &totals);

    let images: Vec<_> = items.iter().filter(|r| r.kind == Kind::Image).collect();
    assert_eq!(images.len(), 2);

    let naive: u64 = images.iter().map(|r| parse_si(&r.reported).unwrap()).sum();
    assert_eq!(naive, 6_480_000_000, "the wrong number, kept here so it stays recognisable");

    let (daemon_total, _) = totals.iter().find(|(k, _, _)| *k == Kind::Image)
        .map(|(_, s, r)| (*s, *r)).unwrap();
    assert_eq!(daemon_total, 3_995_000_000);
    assert!(daemon_total < naive);

    // What fad ranks and sums is each image's own layers, which never overlap.
    let summable: u64 = images.iter().map(|r| r.bytes).sum();
    assert_eq!(summable, 759_200_000 + 759_000_000);
    assert!(summable <= daemon_total);

    // And the shared bytes are carried, not counted.
    for img in &images {
        assert_eq!(img.measure, Measure::Unique { shared: 2_476_000_000 });
    }
}

#[test]
fn things_in_use_are_blocked_and_things_idle_are_not() {
    let totals = parse_totals(&fixture("docker_df.json"));
    let items = parse_verbose(&fixture("docker_df_verbose.json"), Source::Docker, &totals);
    let find = |name: &str| items.iter().find(|r| r.name == name).expect(name);

    // An image a container is using.
    let held = find("tinkerbell-capture:1.60.0");
    assert!(!held.idle);
    assert_eq!(held.blocked.as_deref(), Some("used by 1 container"));

    // A dangling image: nothing refers to it.
    let dangling = find("<dangling>");
    assert!(dangling.idle && dangling.removable());

    // A running container, and a stopped one.
    assert_eq!(find("web").blocked.as_deref(), Some("running"));
    assert!(find("old-db").removable());
    // The writable layer only. Counting the whole root filesystem here would
    // double what the image rows already report.
    assert_eq!(find("old-db").bytes, 842_000_000);

    // A volume something is attached to, and an orphan.
    assert_eq!(find("pgdata").blocked.as_deref(), Some("attached to 1 container"));
    assert!(find("orphan").removable());
    assert_eq!(find("orphan").bytes, 430_000_000);
}

#[test]
fn build_cache_is_one_item_because_one_record_cannot_be_removed() {
    let totals = parse_totals(&fixture("docker_df.json"));
    let items = parse_verbose(&fixture("docker_df_verbose.json"), Source::Docker, &totals);

    let bc: Vec<_> = items.iter().filter(|r| r.kind == Kind::BuildCache).collect();
    assert_eq!(bc.len(), 1, "four records in the fixture, one actionable item");
    // Sized by the daemon's reclaimable figure, not by the listed records.
    assert_eq!(bc[0].bytes, 573_600_000);
    assert_eq!(fad::tools::remove_line(&bc[0].key()), "docker builder prune --force");
}

#[test]
fn removal_commands_take_ids_and_never_paths() {
    let totals = parse_totals(&fixture("docker_df.json"));
    let items = parse_verbose(&fixture("docker_df_verbose.json"), Source::Docker, &totals);
    let find = |name: &str| items.iter().find(|r| r.name == name).expect(name);

    assert_eq!(fad::tools::remove_line(&find("orphan").key()), "docker volume rm orphan");
    assert_eq!(fad::tools::remove_line(&find("old-db").key()), "docker container rm 11ab22cd33ef");
    assert!(fad::tools::remove_line(&find("<dangling>").key()).starts_with("docker image rm sha256:"));
    // Deliberately no -f: the daemon's own in-use check is a second gate.
    assert!(!fad::tools::remove_line(&find("<dangling>").key()).contains(" -f"));
}

#[test]
fn an_image_that_can_be_re_pulled_says_so() {
    let totals = parse_totals(&fixture("docker_df.json"));
    let items = parse_verbose(&fixture("docker_df_verbose.json"), Source::Docker, &totals);
    let find = |name: &str| items.iter().find(|r| r.name == name).expect(name);

    assert_eq!(find("tinkerbell-capture:1.60.0").restore.as_deref(),
               Some("docker pull tinkerbell-capture:1.60.0"));
    // A dangling image has no name to pull, and we do not pretend otherwise.
    assert_eq!(find("<dangling>").restore, None);
}

#[test]
fn the_backend_decides_whether_removal_frees_anything() {
    // OrbStack trims its own disk, so the bytes really do come back.
    assert!(matches!(backing_for("OrbStack"), Backing::VmDisk { shrinks: true, .. }));
    assert!(backing_for("OrbStack").frees_host_space());
    assert!(backing_for("OrbStack").notes()[0].contains("shrinks itself"));

    // Docker Desktop does not, whichever host it is on.
    assert!(!backing_for("Docker Desktop").frees_host_space());
    let notes = backing_for("Docker Desktop").notes();
    // The consequence first, because in a sixty-column pane the tail is what
    // gets truncated away.
    assert!(notes[0].contains("will not free space on your disk"), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("never shrinks")), "{notes:?}");
    // Short enough to survive the pane it is drawn into. The tree pane is the
    // terminal's width less the 38-column detail pane and two borders, so a
    // 100-column terminal leaves 58 — and the widest prefix, " ⚠ docker: ",
    // takes 11 of them.
    //
    // Built here rather than taken from `backing_for`, which goes looking for a
    // real disk image: the longest plausible name and size, not whatever this
    // machine happens to have.
    const ROOM: usize = 58 - 11;
    let worst = Backing::VmDisk {
        disk: Some(std::path::PathBuf::from("/x/Data/vms/0/data/Docker.raw")),
        host_bytes: Some(999 << 30),
        shrinks: false,
    };
    for n in worst.notes() {
        assert!(n.chars().count() <= ROOM, "{} chars: {n:?}", n.chars().count());
    }

    // Native Linux: bytes freed are bytes freed, and there is no caveat to make.
    if cfg!(not(target_os = "macos")) {
        assert_eq!(backing_for("Ubuntu 24.04.1 LTS"), Backing::Host);
        assert!(backing_for("Ubuntu 24.04.1 LTS").notes().is_empty());
    }
}

#[test]
fn garbage_in_is_not_a_panic_and_is_not_a_zero() {
    assert!(parse_totals("").is_empty());
    assert!(parse_totals("not json\n{}\n").is_empty());
    assert!(parse_verbose("", Source::Docker, &[]).is_empty());
    assert!(parse_verbose("{\"Images\":\"nope\"}", Source::Docker, &[]).is_empty());
}
