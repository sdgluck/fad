//! Running another program and refusing to wait forever for it.
//!
//! Nothing else in `fad` needs this: `platform.rs` shells out to `open` and
//! `pbcopy`, which either answer at once or fail. A container daemon is
//! different. `docker system df` against a daemon that is starting, wedged, or
//! talking to a VM that has gone to sleep can sit there indefinitely, and a
//! disk tool that hangs because something else hung is a disk tool nobody
//! trusts. Every call here carries a deadline and kills the child at it.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum ExecErr {
    /// Not on `PATH`. For a tool like Docker this is the common case and not an
    /// error: most machines simply do not have it.
    NotInstalled,
    /// Still running at the deadline, and killed. Usually means the daemon is
    /// not answering rather than that the work is slow.
    TimedOut,
    Failed { code: Option<i32>, stderr: String },
}

/// How often we look to see whether the child has finished. Short enough that a
/// fast command is not noticeably delayed, long enough not to spin.
const TICK: Duration = Duration::from_millis(10);

/// Run `program`, capture stdout, and give up after `timeout`.
///
/// Both pipes are drained on their own threads. A pipe buffer is about 64K and
/// `docker system df -v` on a well-used machine is several hundred, so a
/// version of this that waited first and read afterwards would deadlock against
/// a child blocked writing to a full pipe — and would look exactly like the
/// daemon hang it is meant to detect.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Result<String, ExecErr> {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ExecErr::NotInstalled),
        Err(e) => return Err(ExecErr::Failed { code: None, stderr: e.to_string() }),
    };

    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => return Err(ExecErr::Failed { code: None, stderr: e.to_string() }),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            // Reap it, so a hung daemon does not leave a zombie behind every
            // time the view is opened.
            let _ = child.wait();
            return Err(ExecErr::TimedOut);
        }
        std::thread::sleep(TICK);
    };

    // Killing the child closes both pipes, so these always finish.
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();

    if status.success() {
        Ok(stdout)
    } else {
        Err(ExecErr::Failed { code: status.code(), stderr })
    }
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut p) = pipe {
            // Output that is not UTF-8 is output we cannot parse anyway; taking
            // it lossily beats failing the whole probe over one stray byte.
            let mut buf = Vec::new();
            let _ = p.read_to_end(&mut buf);
            s = String::from_utf8_lossy(&buf).into_owned();
        }
        s
    })
}
