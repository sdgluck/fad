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

/// Run fad on an empty scratch directory with no terminal and its cache in
/// scratch too. Every combination below must be refused before anything runs.
fn refused(args: &[&str]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let out = fad()
        .arg(dir.path())
        .args(args)
        .env("FAD_CACHE_DIR", dir.path().join("cache"))
        .env("FAD_STATE_DIR", dir.path().join("state"))
        .env("FAD_CONFIG_DIR", dir.path().join("config"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(2), "{args:?} was not refused: {err}");
    err
}

/// A modifier without the thing it modifies was silently ignored: `--dry-run`
/// alone opened the UI, which reads as a promise not kept.
#[test]
fn flags_that_only_mean_something_together_are_refused_apart() {
    for alone in [&["--dry-run"][..], &["--permanent"], &["--max", "1G"]] {
        let err = refused(alone);
        assert!(err.contains("--yes"), "{alone:?}: {err}");
    }
    let err = refused(&["--yes"]);
    assert!(err.contains("--reclaim") || err.contains("--tools"), "{err}");
    refused(&["--print-path", "--json"]);
    for with in ["--reclaim", "--tools", "--yes"] {
        refused(&["--since", with]);
    }
}

/// Tool removals are permanent whatever the flags say; `--permanent` with them
/// is refused with the reason rather than a bare "cannot be used with".
#[test]
fn permanent_with_tools_says_why() {
    let err = refused(&["--tools", "--yes", "--permanent"]);
    assert!(err.contains("always permanent"), "{err}");
}

#[test]
fn the_help_says_where_the_report_flags_apply() {
    let out = fad().arg("--help").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let min = text.split("--min-size").nth(1).unwrap();
    assert!(min.contains("--since") && min.contains("--tools --yes"), "{min}");
    let max = text.split("--max").nth(1).unwrap();
    assert!(max.contains("--tools --yes"), "{max}");
}

/// No terminal: say what to use instead, not "Device not configured".
#[test]
fn without_a_terminal_the_ui_says_what_to_use_instead() {
    let dir = tempfile::tempdir().unwrap();
    let out = fad()
        .arg(dir.path())
        .env("FAD_CACHE_DIR", dir.path().join("cache"))
        .env("FAD_STATE_DIR", dir.path().join("state"))
        .env("FAD_CONFIG_DIR", dir.path().join("config"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("needs a terminal") && err.contains("--json"), "{err}");
}

/// fad with every directory it writes to, and every tool it asks, in scratch.
fn scratch(dir: &std::path::Path) -> Command {
    let mut c = fad();
    c.env("FAD_CACHE_DIR", dir.join("cache"))
        .env("FAD_STATE_DIR", dir.join("state"))
        .env("FAD_CONFIG_DIR", dir.join("config"))
        .env("FAD_PODMAN_BIN", dir.join("no-podman"))
        .env("FAD_TMUTIL_BIN", dir.join("no-tmutil"))
        .stdin(std::process::Stdio::null());
    c
}

/// "Nothing reclaimable" was the answer both when there was nothing and when
/// everything was bigger than `--max`; the second is a different problem.
#[test]
fn everything_over_the_cap_is_not_reported_as_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("scan");
    std::fs::create_dir_all(root.join("proj/node_modules/x")).unwrap();
    std::fs::write(root.join("proj/package.json"), b"{}").unwrap();
    std::fs::write(root.join("proj/node_modules/x/big"), vec![0u8; 256 * 1024]).unwrap();

    let out = scratch(dir.path())
        .arg(&root)
        .args(["--reclaim", "--yes", "--dry-run", "--max", "1K"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("1 item, larger than --max 1.0K"), "{text}");
    assert!(root.join("proj/node_modules/x/big").exists());
}

/// The daemon being down is not "nothing to clean". A nightly script was
/// told exactly that, with exit 0, every night the daemon was not up.
#[test]
fn tools_yes_with_the_daemon_down_says_so_and_fails() {
    let dir = tempfile::tempdir().unwrap();
    let docker = dir.path().join("docker");
    std::fs::write(
        &docker,
        "#!/bin/sh\necho 'Cannot connect to the Docker daemon at unix:///var/run/docker.sock. \
         Is the docker daemon running?' >&2\nexit 1\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = scratch(dir.path())
        .arg(dir.path())
        .args(["--tools", "--yes", "--dry-run"])
        .env("FAD_DOCKER_BIN", &docker)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "stdout: {text} stderr: {err}");
    assert!(err.contains("not running"), "{err}");
    assert!(!text.contains("nothing the tools report as unused"), "{text}");
}

/// The parser can describe the flags and nothing else. The keys, the files
/// fad writes and the environment it reads are what a man page is opened for.
#[test]
fn the_man_page_covers_keys_files_and_environment() {
    let out = fad().arg("--man").output().expect("could not run fad");
    let text = String::from_utf8(out.stdout).expect("not utf-8");
    for section in [".SH KEYS", ".SH FILES", ".SH ENVIRONMENT"] {
        assert!(text.contains(section), "no {section}");
    }
    let env = text.split(".SH ENVIRONMENT").nth(1).unwrap();
    // Every FAD_ variable the source reads, found the way a reviewer would.
    let mut vars = std::collections::BTreeSet::new();
    let mut stack = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let src = std::fs::read_to_string(&p).unwrap();
                for (i, _) in src.match_indices("\"FAD_") {
                    let name: String = src[i + 1..]
                        .chars()
                        .take_while(|c| c.is_ascii_uppercase() || *c == '_')
                        .collect();
                    vars.insert(name);
                }
            }
        }
    }
    assert!(vars.len() >= 6, "found {vars:?}");
    for v in &vars {
        assert!(env.contains(v.as_str()), "the man page does not mention {v}");
    }
    assert!(text.contains("ctrl\\-c"), "the quit-without-choosing key is not documented");
    assert!(text.contains("undo.jsonl") && text.contains("ignore"), "files missing");
}

/// Run a script in a shell with a clean environment, returning (status, out, err).
fn in_shell(shell: &str, flags: &[&str], script: &str, home: &std::path::Path) -> Option<(i32, String, String)> {
    let out = Command::new(shell)
        .args(flags)
        .arg("-c")
        .arg(script)
        .env("HOME", home)
        .env("ZDOTDIR", home)
        .output()
        .ok()?;
    Some((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Evaluated before `compinit`, the zsh script used to fail on every new shell
/// with "command not found: compdef".
#[test]
fn zsh_init_before_compinit_is_quiet_and_after_it_completes_fad_cd() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("init.zsh");
    std::fs::write(&script, init("zsh")).unwrap();
    let src = script.display();

    let Some((code, _, err)) = in_shell("zsh", &["-f"], &format!("source {src}"), dir.path()) else {
        eprintln!("skipped: no zsh on this machine");
        return;
    };
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("compdef"), "{err}");

    let dump = dir.path().join("zcompdump");
    let after = format!(
        "autoload -Uz compinit && compinit -u -d {}; source {src}; print -r -- \"${{_comps[fad-cd]}}\"",
        dump.display()
    );
    let (code, out, err) = in_shell("zsh", &["-f"], &after, dir.path()).unwrap();
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "_fad", "fad-cd has no completions: {err}");
}

#[test]
fn bash_completes_fad_cd_like_fad() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("init.bash");
    std::fs::write(&script, init("bash")).unwrap();
    let Some((code, out, err)) = in_shell(
        "bash",
        &["--norc", "--noprofile"],
        &format!("source {}; complete -p fad-cd", script.display()),
        dir.path(),
    ) else {
        eprintln!("skipped: no bash on this machine");
        return;
    };
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("-F _fad"), "{out}");
}

#[test]
fn fish_wraps_fad_for_completions() {
    assert!(init("fish").contains("function fad-cd --wraps fad"));
}

/// When fad prints nothing and fails — ctrl-c under `--print-path` — the
/// wrapper must leave the shell where it was. Exercised against a stand-in
/// `fad` on `PATH`, since the real one needs a terminal.
#[test]
fn the_wrapper_stays_put_when_nothing_was_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let stub = bin.join("fad");
    std::fs::write(&stub, "#!/bin/sh\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let script = dir.path().join("init.bash");
    std::fs::write(&script, init("bash")).unwrap();
    let start = std::fs::canonicalize(dir.path()).unwrap();

    let Some((_, out, err)) = in_shell(
        "bash",
        &["--norc", "--noprofile"],
        &format!(
            "source {}; cd {}; PATH={}:$PATH; fad-cd; pwd -P",
            script.display(),
            start.display(),
            bin.display()
        ),
        dir.path(),
    ) else {
        return;
    };
    assert_eq!(out.trim(), start.display().to_string(), "moved: {err}");
}
