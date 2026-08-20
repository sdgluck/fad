//! Where the snapshot time actually goes: building it, encoding it, writing it,
//! and reading it back. Run against a real directory, not a fixture.
//!
//! cargo run --release --example cachebench -- ~

use std::path::PathBuf;
use std::time::Instant;

use fad::scan::Scan;
use fad::scan::walk::ScanOpts;

fn main() {
    let root: PathBuf = std::env::args().nth(1).map(PathBuf::from).expect("usage: cachebench <dir>");
    unsafe { std::env::set_var("FAD_CACHE_DIR", std::env::temp_dir().join("fad-cachebench")) };

    let t = Instant::now();
    let (mut tree, scan) = Scan::start(&root, ScanOpts::default()).unwrap();
    scan.finish(&mut tree);
    println!("scan            {:>7} ms  ({} nodes)", t.elapsed().as_millis(), tree.len());

    let t = Instant::now();
    let snap = tree.to_snapshot();
    println!("to_snapshot     {:>7} ms", t.elapsed().as_millis());

    let t = Instant::now();
    let bytes = postcard::to_allocvec(&snap).unwrap();
    println!("encode          {:>7} ms  ({:.0} MB)", t.elapsed().as_millis(), bytes.len() as f64 / 1e6);

    let t = Instant::now();
    fad::cache::save(&tree).unwrap();
    println!("save (total)    {:>7} ms", t.elapsed().as_millis());

    let t = Instant::now();
    let loaded = fad::cache::load(&std::fs::canonicalize(&root).unwrap()).expect("load failed");
    println!("load (total)    {:>7} ms", t.elapsed().as_millis());

    assert_eq!(loaded.len(), tree.len(), "node count changed across the round trip");
    assert_eq!(
        loaded.node(loaded.root()).total_bytes,
        tree.node(tree.root()).total_bytes,
        "root total changed across the round trip"
    );
    println!("round trip verified");
}
