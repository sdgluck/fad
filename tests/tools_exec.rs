//! Running a tool, and refusing to wait forever for one.
//!
//! None of this needs Docker: it needs a program that is missing, one that
//! fails, one that hangs, and one that says more than a pipe buffer holds.

use std::time::{Duration, Instant};

use fad::tools::exec::{self, ExecErr};

#[test]
fn a_missing_tool_is_not_an_error() {
    // The common case by far. It has to be cheap and it has to be
    // distinguishable from every other kind of failure, because the view says
    // nothing at all about a tool nobody has installed.
    let e = exec::run("fad-no-such-program-9f3a", &[], Duration::from_secs(1));
    assert!(matches!(e, Err(ExecErr::NotInstalled)), "{e:?}");
}

#[test]
fn a_hang_is_killed_at_the_deadline() {
    let start = Instant::now();
    let e = exec::run("sleep", &["30"], Duration::from_millis(150));
    let took = start.elapsed();

    assert!(matches!(e, Err(ExecErr::TimedOut)), "{e:?}");
    // The point of the deadline: opening the view against a wedged daemon is a
    // blip, not a hang.
    assert!(took < Duration::from_secs(2), "took {took:?}");
}

#[test]
fn a_failure_keeps_what_the_tool_said() {
    let e = exec::run("sh", &["-c", "echo 'cannot connect to the Docker daemon' >&2; exit 1"],
                      Duration::from_secs(5));
    match e {
        Err(ExecErr::Failed { code, stderr }) => {
            assert_eq!(code, Some(1));
            assert!(stderr.contains("cannot connect"), "{stderr:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn output_larger_than_a_pipe_buffer_still_comes_back() {
    // `docker system df -v` on a well-used machine is several hundred kilobytes
    // and a pipe holds about 64K. Anything that waited first and read afterwards
    // would deadlock here — and would look exactly like the daemon hang the
    // deadline exists to catch.
    let out = exec::run("sh", &["-c", "yes abcdefghij | head -c 400000"],
                        Duration::from_secs(20))
        .expect("should not deadlock");
    assert_eq!(out.len(), 400_000);
}

#[test]
fn stdin_is_closed_so_a_prompt_cannot_wait_for_us() {
    // A tool that decides to ask a question should die, not sit there holding
    // the view open.
    let out = exec::run("sh", &["-c", "cat; echo done"], Duration::from_secs(5));
    assert_eq!(out.expect("should not hang").trim(), "done");
}
