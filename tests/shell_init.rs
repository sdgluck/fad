//! `--init` and `--man`.
//!
//! Both are generated from the parser rather than written out beside it, so the
//! only thing worth testing is that they come out complete and that the shell
//! half is syntactically valid in the shell it claims to be for — a broken
//! `eval` in someone's startup file is a worse failure than no completions.

use std::process::Command;

fn fad() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fad"))
}

fn init(shell: &str) -> String {
    let out = fad().args(["--init", shell]).output().expect("could not run fad");
    assert!(out.status.success(), "fad --init {shell} failed");
    String::from_utf8(out.stdout).expect("not utf-8")
}

#[test]
fn every_shell_gets_completions_and_the_cd_wrapper() {
    for (shell, marker) in [("bash", "complete "), ("zsh", "#compdef fad"), ("fish", "complete -c fad")] {
        let text = init(shell);
        assert!(text.contains(marker), "{shell}: no completions");
        assert!(text.contains("--print-path"), "{shell}: completions do not mention --print-path");
        assert!(text.contains("fad-cd"), "{shell}: no cd wrapper");
    }
}

/// The wrapper is meant to be `eval`ed into a startup file. A syntax error
/// there breaks the user's shell, not just fad.
#[test]
fn what_it_prints_is_valid_in_the_shell_it_is_for() {
    for (shell, check) in [("bash", vec!["-n"]), ("zsh", vec!["-n"])] {
        let Ok(mut child) = Command::new(shell)
            .args(&check)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
        else {
            eprintln!("skipped: no {shell} on this machine");
            continue;
        };
        use std::io::Write;
        child.stdin.as_mut().unwrap().write_all(init(shell).as_bytes()).unwrap();
        drop(child.stdin.take());
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{shell} rejected its own init script: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Landing on a file has to mean landing in the directory holding it. `cd` into
/// a 40G disk image is not what anyone meant by "quit here".
#[test]
fn the_wrapper_lands_in_a_directory_even_when_the_cursor_was_on_a_file() {
    let text = init("bash");
    let body = text.split("fad-cd()").nth(1).expect("no wrapper body");
    assert!(body.contains("dirname"), "a file would be cd'd into:\n{body}");
}

#[test]
fn the_man_page_covers_the_flags() {
    let out = fad().arg("--man").output().expect("could not run fad");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("not utf-8");
    assert!(text.starts_with(".ie"), "not roff:\n{}", &text[..60.min(text.len())]);
    for flag in ["\\-\\-reclaim", "\\-\\-tools", "\\-\\-print\\-path", "\\-\\-cross\\-device"] {
        assert!(text.contains(flag), "the man page does not document {flag}");
    }
}
