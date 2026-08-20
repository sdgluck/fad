use std::path::PathBuf;

use clap::Parser;

use fad::app::App;
use fad::format::human;
use fad::run;
use fad::scan::Scan;
use fad::scan::walk::{ScanOpts, Skip};
use fad::presets::Category;
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

    /// Only what the built-in rules consider reclaimable: build artifacts,
    /// package caches, app caches, VM images. Opens the UI in that view, or
    /// with --json prints the set, grouped by category.
    #[arg(long)]
    reclaim: bool,

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
        if args.reclaim {
            print_reclaim_json(&tree, &args);
        } else {
            print_json(&tree, &args);
        }
        return;
    }

    let mut app = App::new(tree, scan, opts);
    app.apparent = args.apparent;
    app.mouse = !args.no_mouse;
    app.reclaim_view = args.reclaim;
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

/// The reclaimable set, grouped by category and ranked. Shaped for a script:
/// every entry carries the path, the size, and — where we can name one — the
/// command that puts it back, so a cleanup can be reviewed before it is run.
fn print_reclaim_json(tree: &Tree, args: &Args) {
    let mut cats = Vec::new();
    for cat in Category::all() {
        let mut items: Vec<NodeId> = tree
            .reclaimable
            .iter()
            .copied()
            .filter(|id| tree.node(*id).preset == Some(cat))
            // A `node_modules` inside a `node_modules` is already covered by
            // its ancestor; listing both would double the headline total.
            .filter(|id| !has_reclaimable_ancestor(tree, *id))
            .filter(|id| tree.size(*id, args.apparent) >= args.min_size)
            .collect();
        if items.is_empty() {
            continue;
        }
        items.sort_unstable_by_key(|id| std::cmp::Reverse(tree.size(*id, args.apparent)));

        let total: u64 = items.iter().map(|id| tree.size(*id, args.apparent)).sum();
        let entries: Vec<_> = items
            .iter()
            .map(|id| {
                let bytes = tree.size(*id, args.apparent);
                let mut v = serde_json::json!({
                    "path": tree.path(*id),
                    "bytes": bytes,
                    "size": human(bytes),
                });
                if let Some(cmd) = tree.rebuild_command(*id) {
                    v["restore_with"] = serde_json::Value::String(cmd.to_string());
                }
                v
            })
            .collect();
        cats.push(serde_json::json!({
            "category": cat.label(),
            "note": cat.note(),
            "bytes": total,
            "size": human(total),
            "items": entries,
        }));
    }

    let total: u64 = cats.iter().map(|c| c["bytes"].as_u64().unwrap_or(0)).sum();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "root": tree.root_path(),
            "bytes": total,
            "size": human(total),
            "categories": cats,
        }))
        .unwrap()
    );
}

fn has_reclaimable_ancestor(tree: &Tree, id: NodeId) -> bool {
    let mut cur = tree.node(id).parent;
    while let Some(p) = cur {
        if tree.node(p).preset.is_some() {
            return true;
        }
        cur = tree.node(p).parent;
    }
    false
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
