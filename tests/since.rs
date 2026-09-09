//! `--since` answers a different question from `--json`, and has its own JSON
//! shape for it. The two flags used to be tested in the wrong order, so
//! `--since --json` printed a plain tree dump and the list of changes was
//! unreachable from a script.

use std::process::Command;

fn fad(dir: &std::path::Path, cache: &std::path::Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_fad"))
        .arg(dir)
        .args(args)
        .env("FAD_CACHE_DIR", cache)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn since_with_json_prints_the_changes_not_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("x")).unwrap();
    std::fs::write(dir.path().join("x/a"), vec![0u8; 200 * 1024]).unwrap();

    // The first run has nothing to compare against; it leaves the baseline.
    let (_, err, code) = fad(dir.path(), cache.path(), &["--since"]);
    assert_eq!(code, 1, "expected the no-baseline exit code, stderr: {err}");

    std::fs::write(dir.path().join("x/b"), vec![0u8; 500 * 1024]).unwrap();

    let (out, err, code) = fad(dir.path(), cache.path(), &["--since", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v.get("changes").is_some(), "not the --since shape: {out}");
    assert!(v.get("since").is_some(), "not the --since shape: {out}");
    // The plain tree dump's marker, which is what used to come out here.
    assert!(v.get("children").is_none(), "printed the tree instead: {out}");

    let changes = v["changes"].as_array().unwrap();
    let added = changes
        .iter()
        .find(|c| c["path"].as_str().unwrap().ends_with("/x/b"))
        .expect("the new file is not in the changes");
    assert_eq!(added["new"], serde_json::json!(true));
    assert!(added["delta"].as_i64().unwrap() >= 500 * 1024);
}
