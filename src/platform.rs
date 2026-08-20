//! Talking to the desktop: file manager, clipboard, editor.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

/// Show the item in the system file manager, selected where the platform
/// supports it.
pub fn reveal(path: &Path) -> io::Result<&'static str> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).status()?;
        Ok("revealed in Finder")
    }
    #[cfg(not(target_os = "macos"))]
    {
        // No portable "select this file" on Linux, so open the containing
        // directory: xdg-open on a file would launch its associated app, which
        // is emphatically not what `o` should do to a 40GB disk image.
        let target = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        Command::new("xdg-open")
            .arg(target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok("opened in your file manager")
    }
}

/// Free bytes on the filesystem holding `path`, as the user sees them: blocks
/// available to an unprivileged process, not the reserved total.
pub fn free_space(path: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    (st.f_bavail as u64).checked_mul(st.f_frsize as u64)
}

/// Copy text to the system clipboard.
pub fn copy_to_clipboard(text: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    const CANDIDATES: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(not(target_os = "macos"))]
    const CANDIDATES: &[(&str, &[&str])] = &[
        // Wayland first, then X11. Whichever is installed and running wins.
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];

    let mut last: Option<io::Error> = None;
    for (program, args) in CANDIDATES {
        match write_to(program, args, text) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no clipboard tool available")))
}

fn write_to(program: &str, args: &[&str], text: &str) -> io::Result<()> {
    use std::io::Write;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    // Dropping stdin closes the pipe, which is what tells the tool to finish.
    drop(child.stdin.take());
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{program} exited with {status}")))
    }
}

/// The clipboard tools worth suggesting when none is installed.
pub fn clipboard_hint() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "pbcopy is missing"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "install wl-clipboard, xclip, or xsel"
    }
}
