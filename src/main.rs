use std::path::PathBuf;

use clap::Parser;

use fad::app::App;
use fad::format::human;
use fad::run;
use fad::scan::Scan;
use fad::scan::walk::{ScanOpts, Skip};
use fad::tree::{NodeId, Tree};

/// Find and delete what is eating your disk.
#[derive(Parser, Debug)]
#[command(name = "fad", version, about)]
struct Args {
    /// Directory to scan.
    #[arg(default_value = None)]
    path: Option<PathBuf>,

    /// Follow mount points into other filesystems.
    #[arg(long)]
    cross_device: bool,

    /// Descend into cloud-provider folders (iCloud, Dropbox, OneDrive, ...).
    /// Off by default: enumerating one can stall for minutes on the network.
    #[arg(long)]
    cloud: bool,

    /// Report `st_size` instead of allocated blocks, in the UI and in --json.
    #[arg(long)]
    apparent: bool,

    /// Hide entries below this size, e.g. 100M, 2G.
    #[arg(long, value_parser = parse_size, default_value = "0")]
    min_size: u64,

    /// How deep to print. Only meaningful with --json.
    #[arg(long, default_value_t = 2)]
    depth: usize,

    /// Dump the ranked tree as JSON and exit instead of opening the UI.
    #[arg(long)]
    json: bool,

    /// Ignore any saved snapshot and always walk from scratch.
    #[arg(long)]
    no_cache: bool,

    /// Delete every saved snapshot and exit.
    #[arg(long)]
    clear_cache: bool,

    /// Do not capture the mouse. Clicking and the wheel stop working; your
    /// terminal's own text selection starts working again.
    #[arg(long)]
    no_mouse: bool,
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('K') | Some('k') => (&s[..s.len() - 1], 1024u64),
        Some('M') | Some('m') => (&s[..s.len() - 1], 1024 * 1024),
        Some('G') | Some('g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        Some('T') | Some('t') => (&s[..s.len() - 1], 1024u64.pow(4)),
        _ => (s, 1),
    };
    num.trim()
        .parse::<f64>()
        .map(|n| (n * mult as f64) as u64)
        .map_err(|_| format!("not a size: {s}"))
}

fn main() {
    let args = Args::parse();

    if args.clear_cache {
        match fad::cache::clear() {
            Ok(()) => println!("fad: cache cleared"),
            Err(e) => {
                eprintln!("fad: could not clear cache: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let root = args
        .path
        .clone()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));

    let opts = ScanOpts { cross_device: args.cross_device, cloud: args.cloud };
    let (mut tree, scan) = match Scan::start(&root, opts.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fad: {}: {e}", root.display());
            std::process::exit(1);
        }
    };

    if args.json {
        scan.finish(&mut tree);
        tree.sort_all_by_size(args.apparent);
        print_json(&tree, &args);
        return;
    }

    let mut app = App::new(tree, scan, opts);
    app.apparent = args.apparent;
    app.mouse = !args.no_mouse;
    if !args.no_cache {
        app.load_snapshot_async();
    }
    let save = !args.no_cache;
    match run::run(app) {
        Ok(Some(final_tree)) => {
            if save {
                // Best effort: failing to write a cache is never worth an error
                // on the way out of a session that otherwise went fine.
                let _ = fad::cache::save(&final_tree);
            }
        }
        // Quit before the walk finished, so the tree is incomplete and the
        // previous snapshot — which at least was whole — stays put.
        Ok(None) => {}
        Err(e) => {
            eprintln!("fad: {e}");
            std::process::exit(1);
        }
    }
}

fn print_json(tree: &Tree, args: &Args) {
    let v = node_json(tree, tree.root(), args, 0);
    println!("{}", serde_json::to_string_pretty(&v).unwrap());
    if tree.unreadable_count > 0 {
        eprintln!(
            "fad: {} directories were unreadable and are not counted \
             (grant Full Disk Access to your terminal to include them)",
            tree.unreadable_count
        );
    }
    let cloud: Vec<_> = tree
        .skipped
        .iter()
        .filter(|(_, r)| *r == Skip::CloudStorage)
        .map(|(id, _)| tree.path(*id))
        .collect();
    if !cloud.is_empty() {
        eprintln!(
            "fad: skipped {} cloud folder(s), not counted — rescan with --cloud to include:",
            cloud.len()
        );
        for p in &cloud {
            eprintln!("       {}", p.display());
        }
    }
}

fn node_json(tree: &Tree, id: NodeId, args: &Args, depth: usize) -> serde_json::Value {
    let n = tree.node(id);
    let bytes = if args.apparent { n.total_len } else { n.total_bytes };
    let mut v = serde_json::json!({
        "path": tree.path(id),
        "bytes": bytes,
        "size": human(bytes),
        "files": n.file_count,
        "dirs": n.dir_count,
    });
    if depth < args.depth && !n.children.is_empty() {
        let kids: Vec<_> = n
            .children
            .iter()
            .filter(|c| {
                let c = tree.node(**c);
                (if args.apparent { c.total_len } else { c.total_bytes }) >= args.min_size
            })
            .map(|c| node_json(tree, *c, args, depth + 1))
            .collect();
        if !kids.is_empty() {
            v["children"] = serde_json::Value::Array(kids);
        }
    }
    v
}
