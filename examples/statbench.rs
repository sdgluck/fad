//! A/B for the per-entry stat cost: full-path `symlink_metadata` versus
//! `fstatat` against the directory's own descriptor. Serial on purpose —
//! parallelism would hide the difference behind scheduler noise.
//!
//! cargo run --release --example statbench -- <dir>

use std::path::{Path, PathBuf};
use std::time::Instant;

use fad::scan::dir::read_dir_stat;
use fad::scan::meta::Meta;

fn walk_std(root: &Path) -> (u64, u64) {
    let (mut entries, mut bytes) = (0u64, 0u64);
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = std::fs::symlink_metadata(e.path()) else { continue };
            let m = Meta::from_metadata(&m);
            entries += 1;
            bytes += m.blocks;
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    (entries, bytes)
}

fn walk_fstatat(root: &Path) -> (u64, u64) {
    let (mut entries, mut bytes) = (0u64, 0u64);
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(items) = read_dir_stat(&d) else { continue };
        for it in items {
            entries += 1;
            bytes += it.meta.blocks;
            if it.meta.is_dir() {
                stack.push(d.join(&*it.name));
            }
        }
    }
    (entries, bytes)
}

fn main() {
    let root = std::env::args().nth(1).map(PathBuf::from).expect("usage: statbench <dir>");
    // Warm the metadata cache so we compare CPU cost, not disk reads.
    walk_std(&root);

    for round in 1..=3 {
        let t = Instant::now();
        let (n, b) = walk_std(&root);
        let std_ms = t.elapsed().as_millis();

        let t = Instant::now();
        let (n2, b2) = walk_fstatat(&root);
        let fst_ms = t.elapsed().as_millis();

        if (n, b) != (n2, b2) {
            // A live tree changes underneath us; only a repeatable gap is a bug.
            println!("  note: {n} entries/{b} bytes vs {n2}/{b2} (delta {})", b as i64 - b2 as i64);
        }
        println!(
            "round {round}: {n} entries  read_dir+symlink_metadata {std_ms}ms  \
             readdir+fstatat {fst_ms}ms  ({:.2}x)",
            std_ms as f64 / fst_ms.max(1) as f64
        );
    }
}
