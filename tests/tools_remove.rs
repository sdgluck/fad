//! The removal path, against a stub daemon.
//!
//! `FAD_DOCKER_BIN` points the whole module at a shell script, so this exercises
//! the real `Job`, the real command construction and the real failure handling
//! without a container runtime anywhere near it.

use std::path::{Path, PathBuf};

use fad::tools::{Job, Kind, Source, ToolKey};

mod common;

/// A `docker` that records what it was asked to do.
fn stub(dir: &Path, body: &str) -> PathBuf {
    let bin = dir.join("docker");
    std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

fn key(kind: Kind, id: &str) -> ToolKey {
    ToolKey { source: Source::Docker, kind, id: id.into() }
}

fn drain(mut job: Job) -> Job {
    while !job.is_finished() {
        job.poll();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    job.poll();
    job
}

#[test]
fn a_batch_runs_the_right_command_for_every_kind() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let bin = stub(dir.path(), &format!(r#"echo "$@" >> {}"#, log.display()));
    // SAFETY: the whole test holds `env_lock`, which is what this variable is
    // for; the tools module reads it on every call.
    unsafe { std::env::set_var("FAD_DOCKER_BIN", &bin) };

    let job = drain(Job::start(vec![
        (key(Kind::Volume, "small"), "small".into(), 10),
        (key(Kind::Image, "sha256:abc"), "big".into(), 900),
        (key(Kind::BuildCache, "*"), "cache".into(), 100),
    ]));

    assert_eq!(job.done.len(), 3);
    assert!(job.failures().is_empty(), "{:?}", job.failures());

    let ran = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = ran.lines().collect();

    // The full id, not the twelve characters the UI shows: there is no
    // ambiguity to risk when the whole digest is right there.
    assert!(lines.contains(&"image rm sha256:abc"), "{lines:?}");
    assert!(lines.contains(&"volume rm small"), "{lines:?}");
    assert!(lines.contains(&"builder prune --force"), "{lines:?}");

    // The batch opens with the daemon's own totals, which is what makes a
    // figure reported at the end measured rather than predicted. With a stub
    // that reports nothing there is no measurement at all — so no point asking
    // a second time either. It used to come out as zero, which the modal then
    // printed as "0 freed, measured"; unknown is not empty.
    assert_eq!(lines.first(), Some(&"system df --format {{json .}}"));
    assert_eq!(lines.iter().filter(|l| l.starts_with("system df")).count(), 1, "{lines:?}");
    assert_eq!(job.measured, None);
    assert_eq!(job.expected(), 1010, "the estimate is all there is, and it has to be there");

    unsafe { std::env::remove_var("FAD_DOCKER_BIN") };
}

#[test]
fn a_removal_never_reaches_the_undo_journal() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    common::isolate(dir.path());
    let bin = stub(dir.path(), "exit 0");
    unsafe { std::env::set_var("FAD_DOCKER_BIN", &bin) };

    let before = fad::delete::read_journal().len();
    let job = drain(Job::start(vec![(key(Kind::Image, "sha256:abc"), "gone".into(), 900)]));
    assert_eq!(job.done.len(), 1);

    // The whole reason this is a separate job type. A journal entry is a
    // promise that `u` can put it back, and nothing here can come back.
    assert_eq!(fad::delete::read_journal().len(), before);

    unsafe { std::env::remove_var("FAD_DOCKER_BIN") };
}

#[test]
fn what_the_daemon_refuses_is_reported_not_swallowed() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let bin = stub(
        dir.path(),
        "echo 'Error response from daemon: conflict: unable to delete' >&2\nexit 1",
    );
    unsafe { std::env::set_var("FAD_DOCKER_BIN", &bin) };

    let job = drain(Job::start(vec![(key(Kind::Image, "sha256:abc"), "held".into(), 900)]));

    assert_eq!(job.failures().len(), 1);
    let why = job.done[0].result.as_ref().err().unwrap();
    assert!(why.contains("unable to delete"), "{why}");
    // Failed removals are not counted as space returned.
    assert_eq!(job.expected(), 0);

    unsafe { std::env::remove_var("FAD_DOCKER_BIN") };
}

/// Ordering is the caller's job, exactly as it is for `delete::Job`.
#[test]
fn the_batch_a_staged_set_produces_is_biggest_first() {
    use fad::tools::{Backing, Measure, Report, Resource, SourceReport, Status};

    let mk = |id: &str, bytes: u64| Resource {
        source: Source::Docker,
        kind: Kind::Volume,
        id: id.into(),
        name: id.into(),
        bytes,
        measure: Measure::Exact,
        reported: String::new(),
        idle: true,
        blocked: None,
        last_used: None,
        restore: None,
        path: None,
        detail: Vec::new(),
    };
    let report = Report {
        sources: vec![SourceReport {
            source: Source::Docker,
            status: Status::Ok,
            backing: Backing::Host,
            items: vec![mk("small", 10), mk("big", 900), mk("mid", 100)],
            totals: Vec::new(),
        }],
    };

    let dir = tempfile::tempdir().unwrap();
    let opts = fad::scan::walk::ScanOpts::default();
    let (mut tree, scan) = fad::scan::Scan::start(dir.path(), opts.clone()).unwrap();
    scan.finish(&mut tree);
    let mut app = fad::app::App::new(
        tree,
        fad::scan::Scan::start(dir.path(), opts.clone()).unwrap().1,
        opts,
    );
    app.install_tools_for_test(report);
    app.staged_tools = ["small", "big", "mid"].iter().map(|i| key(Kind::Volume, i)).collect();

    let names: Vec<String> = app.tool_batch_items().into_iter().map(|(_, n, _)| n).collect();
    assert_eq!(names, vec!["big", "mid", "small"]);
}

/// When the daemon answers both ends, the figure is the difference between its
/// own totals — not the estimate, however different the estimate was.
#[test]
fn a_store_measured_at_both_ends_reports_the_difference() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("seen");
    let body = format!(
        r#"if [ "$1" = system ]; then
  if [ -f {seen} ]; then
    echo '{{"Reclaimable":"0B","Size":"3.236GB","Type":"Images"}}'
  else
    touch {seen}
    echo '{{"Reclaimable":"759MB","Size":"3.995GB","Type":"Images"}}'
  fi
fi"#,
        seen = seen.display()
    );
    let bin = stub(dir.path(), &body);
    unsafe { std::env::set_var("FAD_DOCKER_BIN", &bin) };

    let job = drain(Job::start(vec![(key(Kind::Image, "sha256:abc"), "old".into(), 5)]));
    assert_eq!(job.measured, Some(759_000_000));

    unsafe { std::env::remove_var("FAD_DOCKER_BIN") };
}

/// A daemon that answers before the batch and not after has not freed its
/// whole store. That used to be the reading: the missing answer was zero.
#[test]
fn a_store_that_stops_answering_is_estimated_not_emptied() {
    let _lock = common::env_lock();
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("seen");
    let body = format!(
        r#"if [ "$1" = system ]; then
  if [ -f {seen} ]; then exit 1; fi
  touch {seen}
  echo '{{"Reclaimable":"759MB","Size":"3.995GB","Type":"Images"}}'
fi"#,
        seen = seen.display()
    );
    let bin = stub(dir.path(), &body);
    unsafe { std::env::set_var("FAD_DOCKER_BIN", &bin) };

    let job = drain(Job::start(vec![(key(Kind::Image, "sha256:abc"), "old".into(), 5)]));
    assert_eq!(job.measured, None, "an unanswered df was read as an empty store");
    assert_eq!(job.expected(), 5);

    unsafe { std::env::remove_var("FAD_DOCKER_BIN") };
}
