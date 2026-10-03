use std::path::PathBuf;

use clap::{CommandFactory, Parser, ValueEnum};

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

    /// What container runtimes and snapshot stores are holding: docker images,
    /// volumes, build cache, Time Machine local snapshots. Opens the UI in that
    /// view, or with --json prints the report and exits. This storage is not
    /// under the scan root and is never added to its totals.
    #[arg(long)]
    tools: bool,

    /// Ignore any saved snapshot and always walk from scratch.
    #[arg(long)]
    no_cache: bool,

    /// Delete every saved snapshot and exit.
    #[arg(long)]
    clear_cache: bool,

    /// Print the selected path to stdout on exit, so `cd "$(fad --print-path)"`
    /// works.
    #[arg(long)]
    print_path: bool,

    /// With --reclaim or --tools, act instead of opening the UI. With --tools
    /// it only ever touches resources the tool itself reports as unused, and
    /// removal there is permanent: there is no trash for `docker image rm`.
    #[arg(long)]
    yes: bool,

    /// With --reclaim --yes, stop once this much has been staged, largest
    /// first. e.g. 10G.
    #[arg(long, value_parser = parse_size)]
    max: Option<u64>,

    /// With --reclaim --yes, print what would go and delete nothing.
    #[arg(long)]
    dry_run: bool,

    /// With --reclaim --yes, delete permanently instead of trashing.
    #[arg(long)]
    permanent: bool,

    /// Print what changed since the last saved scan of this root, largest
    /// change first.
    #[arg(long)]
    since: bool,

    /// Print shell integration for your shell and exit: completions, and a
    /// `fad-cd` function that leaves you in the directory you quit on.
    /// Put `eval "$(fad --init zsh)"` in your shell's startup file.
    #[arg(long, value_name = "SHELL")]
    init: Option<Shell>,

    /// Print this tool's man page, in roff, and exit.
    #[arg(long)]
    man: bool,

    /// Do not capture the mouse. Clicking and the wheel stop working; your
    /// terminal's own text selection starts working again.
    #[arg(long)]
    no_mouse: bool,
}

/// The shells `--init` knows how to write for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Shell {
    Bash,
    Zsh,
    Fish,
}

/// A size on the command line: a number, optionally fractional, and an
/// optional unit — `500`, `1.5G`, `100mb`, `2GiB`.
///
/// Every unit is a power of 1024, whichever way it is spelled. That is what
/// every size fad prints means (`human` is `du -h`'s base-1024 `K`/`M`/`G`), so
/// `--min-size 1G` hides exactly what shows as under `1.0G`; reading `GB` as
/// 10^9 would make the same letters mean two sizes 7% apart in one tool.
///
/// Refused outright rather than read as something: an empty string, a
/// negative, `nan` and `inf` (all of which `f64` parses happily, and which
/// cast to 0 or `u64::MAX` — `--max -5G` was "no limit at all"), anything too
/// big for a byte count, and an unknown unit.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = (&s[..split], s[split..].trim());
    if num.is_empty() {
        return Err(format!("not a size: {s:?} (expected a number and an optional unit, e.g. 500M)"));
    }
    let n: f64 = num.parse().map_err(|_| format!("not a size: {s:?}"))?;
    let power = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 0,
        "k" | "kb" | "kib" => 1,
        "m" | "mb" | "mib" => 2,
        "g" | "gb" | "gib" => 3,
        "t" | "tb" | "tib" => 4,
        "p" | "pb" | "pib" => 5,
        _ => return Err(format!("not a size: {s:?} (units are K, M, G, T, P \u{2014} powers of 1024)")),
    };
    let bytes = n * 1024f64.powi(power);
    if !bytes.is_finite() || bytes >= u64::MAX as f64 {
        return Err(format!("too large: {s:?}"));
    }
    Ok(bytes as u64)
}

fn main() {
    let args = Args::parse();

    if let Some(shell) = args.init {
        print_init(shell);
        return;
    }

    if args.man {
        let mut out = Vec::new();
        // Straight from the parser, so the flags in the man page are the flags
        // this binary has rather than a copy that drifts.
        if let Err(e) = clap_mangen::Man::new(Args::command()).render(&mut out) {
            eprintln!("fad: could not render the man page: {e}");
            std::process::exit(1);
        }
        use std::io::Write;
        let _ = std::io::stdout().write_all(&out);
        return;
    }

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

    // Asking the tools has nothing to do with the walk, so it does not wait for
    // one. `--tools --json` on a big home directory should not cost a scan it
    // will not print.
    if args.tools && (args.json || args.yes) {
        let report = fad::tools::Report::probe();
        std::process::exit(if args.json {
            print_tools_json(&report);
            0
        } else {
            tools_now(&report, &args)
        });
    }

    // Ahead of --json, because --since has a JSON shape of its own: a list of
    // changes, which is the whole question being asked. Below it, that shape
    // was unreachable and `--since --json` quietly printed a plain tree dump.
    if args.since {
        scan.finish(&mut tree);
        // Read before writing, since the write replaces it.
        let previous = fad::cache::load(tree.root_path(), tree.scan_opts());
        // Leave this walk behind as the new baseline, or a script that runs
        // --since on a timer would keep measuring against the same old scan.
        // Said, not swallowed, when it fails: a baseline that silently did
        // not save makes the next --since compare against something older
        // than the person running it believes.
        let saved = !args.no_cache
            && match fad::cache::save(&tree) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("fad: could not save this scan as the next baseline: {e}");
                    false
                }
            };
        let Some((previous, at)) = previous else {
            let root = display_path(tree.root_path());
            if saved {
                eprintln!(
                    "fad: no earlier scan of {root} to compare against; saved one now \u{2014} \
                     run --since again later"
                );
            } else {
                eprintln!("fad: no earlier scan of {root} to compare against, and none was saved");
            }
            std::process::exit(1);
        };
        std::process::exit(print_since(&tree, &previous, at, &args));
    }

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

    if args.reclaim && args.yes {
        scan.finish(&mut tree);
        std::process::exit(reclaim_now(&tree, &args));
    }

    let mut app = App::new(tree, scan, opts);
    app.apparent = args.apparent;
    app.mouse = !args.no_mouse;
    // One view or the other, never both: `rebuild_rows` would show the tools
    // and leave the reclaimable view up invisibly underneath it.
    if args.tools {
        app.show_view(Some(fad::app::View::Tools));
        app.start_tool_probe();
    } else if args.reclaim {
        app.show_view(Some(fad::app::View::Reclaim));
    }
    if !args.no_cache {
        app.load_snapshot_async();
    }
    let save = !args.no_cache;
    // With --print-path the shell is reading stdout, so the interface has to go
    // somewhere else. See `run::Screen`.
    match run::run(app, args.print_path) {
        Ok(outcome) => {
            // Quit before the walk finished leaves an incomplete tree, and the
            // previous snapshot — which at least was whole — stays put.
            if let (true, Some(final_tree)) = (save, outcome.tree.as_ref()) {
                // Best effort: failing to write a cache is never worth an error
                // on the way out of a session that otherwise went fine.
                // In a debug build, say so anyway: a cache that never saves
                // looks exactly like one that works until the next launch.
                if let Err(e) = fad::cache::save(final_tree)
                    && cfg!(debug_assertions)
                {
                    eprintln!("fad: could not save the scan for next time: {e}");
                }
            }
            // Last, and on stdout alone, so it is the only thing a shell
            // substitution picks up.
            if let (true, Some(p)) = (args.print_path, outcome.selected) {
                println!("{}", p.display());
            }
        }
        Err(e) => {
            eprintln!("fad: {e}");
            std::process::exit(1);
        }
    }
}

/// Everything the shell needs: completions from the parser itself, and the
/// wrapper that makes `--print-path` worth having.
///
/// One command rather than two, because a user who has to be told about
/// completions and separately about `fad-cd` will set up neither.
fn print_init(shell: Shell) {
    let mut cmd = Args::command();
    let target = match shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
    };
    clap_complete::generate(target, &mut cmd, "fad", &mut std::io::stdout());

    // `fad` on its own cannot change your shell's directory — nothing can, from
    // a child process — so the one thing a shell function is needed for is the
    // thing it does. Landing on a file means landing in the directory holding
    // it: `cd` into a 40G disk image is not what anyone meant.
    let sh = r#"
fad-cd() {
  local target
  target="$(command fad --print-path "$@")" || return
  [ -n "$target" ] || return
  [ -d "$target" ] || target="$(dirname -- "$target")"
  cd -- "$target"
}
"#;
    let fish = r#"
function fad-cd --description 'run fad and cd to where you left the cursor'
  set -l target (command fad --print-path $argv)
  or return
  test -n "$target"; or return
  test -d "$target"; or set target (dirname -- "$target")
  cd -- "$target"
end
"#;
    println!("{}", if shell == Shell::Fish { fish } else { sh });
}

fn print_json(tree: &Tree, args: &Args) {
    let v = node_json(tree, tree.root(), args, 0);
    print_json_value(&v);
    warn_omissions(tree);
}

/// Everything the totals just printed do not include, on stderr, path by
/// path — the same account the `!` screen gives, for a reader with no screen.
///
/// Every kind of omission is listed, not only the ones with a flag to fix
/// them: a mount point that was not entered is as missing from the numbers as
/// a cloud folder, and a script comparing totals against `df` needs to know.
fn warn_omissions(tree: &Tree) {
    /// Past this, a list is noise; the count still says how many.
    const SHOW: usize = 20;

    fn list(paths: &[(PathBuf, String)]) {
        for (p, why) in paths.iter().take(SHOW) {
            if why.is_empty() {
                eprintln!("       {}", display_path(p));
            } else {
                eprintln!("       {}  ({why})", display_path(p));
            }
        }
        if paths.len() > SHOW {
            eprintln!("       ... and {} more", paths.len() - SHOW);
        }
    }

    let unreadable: Vec<(PathBuf, String)> = tree
        .unreadable_why
        .iter()
        .filter(|(id, _)| tree.node(*id).flags & fad::tree::flags::DELETED == 0)
        .map(|(id, kind)| (tree.path(*id), fad::tree::unreadable_reason(*kind)))
        .collect();
    if !unreadable.is_empty() {
        eprintln!(
            "fad: {} director{} could not be read, in whole or in part, and {} not fully counted:",
            unreadable.len(),
            if unreadable.len() == 1 { "y" } else { "ies" },
            if unreadable.len() == 1 { "is" } else { "are" },
        );
        list(&unreadable);
    }

    for (reason, heading) in [
        (Skip::OtherDevice, "mount point(s) not entered, not counted \u{2014} rescan with --cross-device to include"),
        (Skip::CloudStorage, "cloud folder(s) skipped, not counted \u{2014} rescan with --cloud to include"),
        (Skip::UnrepresentableName, "entr(ies) whose name is not valid UTF-8, not counted \u{2014} fad cannot name them to act on them"),
    ] {
        let paths: Vec<(PathBuf, String)> = tree
            .skipped
            .iter()
            .filter(|(id, r)| *r == reason && tree.node(*id).flags & fad::tree::flags::DELETED == 0)
            .map(|(id, _)| (tree.path(*id), String::new()))
            .collect();
        if !paths.is_empty() {
            eprintln!("fad: {} {heading}:", paths.len());
            list(&paths);
        }
    }
}

/// Scripted cleanup. Deliberately narrow: it only ever touches entries the
/// built-in rules recognise, it honours the ignore list and the same guard the
/// UI uses, and it prints every path before it goes. A tool that deletes
/// without a person watching has to be boring about what it will consider.
fn reclaim_now(tree: &Tree, args: &Args) -> i32 {
    let ignore = fad::ignore::Rules::load();
    let root = tree.root_path();

    let chosen = fad::reclaim::under_cap(
        fad::reclaim::candidates(tree, args.apparent, args.min_size, &ignore),
        args.max,
    );
    let total: u64 = chosen.iter().map(|(_, b)| b).sum();
    let batch: Vec<(PathBuf, u64)> =
        chosen.iter().map(|(id, bytes)| (tree.path(*id), *bytes)).collect();

    if batch.is_empty() {
        println!("fad: nothing reclaimable under {}", root.display());
        return 0;
    }

    for (path, bytes) in &batch {
        println!("{:>8}  {}", human(*bytes), path.display());
    }
    let verb = if args.dry_run {
        "would reclaim"
    } else if args.permanent {
        "permanently deleting"
    } else {
        "moving to the trash"
    };
    println!("{verb}: {} item(s), {}", batch.len(), human(total));

    if args.dry_run {
        return 0;
    }

    let disposal =
        if args.permanent { fad::delete::Disposal::Permanent } else { fad::delete::Disposal::Trash };
    let mut job = fad::delete::Job::start(batch, disposal);
    while !job.is_finished() {
        job.poll();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    job.poll();

    let failures = job.failures();
    for o in &failures {
        eprintln!("fad: {}: {}", o.path.display(), o.result.as_ref().err().cloned().unwrap_or_default());
    }
    println!("reclaimed {}", human(job.freed()));
    if !args.permanent {
        println!("still in the trash until you empty it");
    }
    i32::from(!failures.is_empty())
}

/// The tools report as JSON.
///
/// Every source appears, including the ones that said nothing usable: a script
/// has to be able to tell "no Docker here" from "Docker with nothing to clean",
/// and an absent key does not say which.
///
/// There is deliberately no top-level total. Adding a host-backed source to one
/// living inside a VM disk produces exactly the number this view exists to
/// refuse to print.
fn print_tools_json(report: &fad::tools::Report) {
    use serde_json::{Map, Value, json};

    let sources: Vec<Value> = report
        .sources
        .iter()
        .map(|s| {
            let mut o = Map::new();
            o.insert("source".into(), json!(s.source.label()));
            o.insert("status".into(), json!(status_word(&s.status)));
            if let Some(line) = s.status.line(s.source) {
                o.insert("status_detail".into(), json!(line));
            }
            o.insert(
                "backing".into(),
                json!(match &s.backing {
                    fad::tools::Backing::Host => "host",
                    fad::tools::Backing::VmDisk { shrinks: true, .. } => "vm_disk_auto_shrink",
                    fad::tools::Backing::VmDisk { .. } => "vm_disk",
                }),
            );
            if let fad::tools::Backing::VmDisk { disk, host_bytes, .. } = &s.backing {
                o.insert(
                    "vm_disk".into(),
                    json!({
                        "path": disk.as_ref().map(|p| p.display().to_string()),
                        "host_bytes": host_bytes,
                    }),
                );
            }
            // The caveat travels with the data. A script that prints "freed
            // 4G" off this without it would be as wrong as the UI would be.
            if !s.backing.notes().is_empty() {
                o.insert("note".into(), json!(s.backing.notes().join("; ")));
            }
            o.insert(
                "totals".into(),
                Value::Array(
                    s.totals
                        .iter()
                        .map(|(k, size, recl)| {
                            json!({
                                "kind": k.label(),
                                "bytes": size,
                                "size": human(*size),
                                "reclaimable_bytes": recl,
                                "reclaimable": human(*recl),
                            })
                        })
                        .collect(),
                ),
            );
            o.insert(
                "items".into(),
                Value::Array(s.items.iter().map(tool_item_json).collect()),
            );
            Value::Object(o)
        })
        .collect();

    print_json_value(&json!({
            "sources": sources,
            "totals_are_not_sums_of_items":
                "each source's totals come from the tool itself; item sizes are \
                 per-item unique storage and shared bytes are reported separately",
        }));
}

fn tool_item_json(r: &fad::tools::Resource) -> serde_json::Value {
    use serde_json::json;
    json!({
        "kind": r.kind.label(),
        "id": r.id,
        "name": r.name,
        // What removing this alone frees. Summable across items; `shared_bytes`
        // is not, and is why the two are separate fields.
        "bytes": r.sized().then_some(r.bytes),
        "size": r.sized().then(|| human(r.bytes)),
        "shared_bytes": r.shared(),
        "reported_by_tool": r.reported,
        "idle": r.idle,
        "blocked": r.blocked,
        "last_used": r.last_used,
        "restore_with": r.restore,
        "remove_with": fad::tools::remove_line(&r.key()),
    })
}

fn status_word(s: &fad::tools::Status) -> &'static str {
    use fad::tools::Status;
    match s {
        Status::Missing => "missing",
        Status::NotRunning(_) => "not_running",
        Status::Ok => "ok",
        Status::Failed(_) => "failed",
        Status::TimedOut => "timed_out",
        Status::Unsupported { .. } => "unsupported",
    }
}

/// `--tools --yes`: remove what the tools themselves call unused.
///
/// As deliberately narrow as `--reclaim --yes`, and narrower in one way: it
/// only ever offers what the tool reports as idle and unheld, and it prints
/// every command before running it. It is also the one scripted path in fad
/// that cannot be undone, so it says so before it starts.
fn tools_now(report: &fad::tools::Report, args: &Args) -> i32 {
    let chosen = report.candidates(args.min_size);
    let items: Vec<&fad::tools::Resource> =
        chosen.iter().filter_map(|k| report.get(k)).collect();

    // Same rule as --reclaim --max: skip anything that would take the batch
    // over the cap rather than stopping at the first overshoot.
    let mut running = 0u64;
    let items: Vec<&fad::tools::Resource> = items
        .into_iter()
        .filter(|r| match args.max {
            Some(max) if running + r.bytes > max => false,
            _ => {
                running += r.bytes;
                true
            }
        })
        .collect();

    if items.is_empty() {
        println!("fad: nothing the tools report as unused");
        return 0;
    }

    for r in &items {
        println!("{:>8}  {}", human(r.bytes), fad::tools::remove_line(&r.key()));
    }
    // Report-aware, so that taking *every* image of a kind reports the tool's
    // own exact total rather than a floor: with nothing left behind to hold a
    // shared layer, the shared bytes go too.
    let set: std::collections::BTreeSet<_> = items.iter().map(|r| r.key()).collect();
    let amount = fad::tools::freed(report, &set).label();
    println!(
        "{}: {} item(s), {amount}",
        if args.dry_run { "would remove" } else { "removing permanently" },
        items.len()
    );
    for s in &report.sources {
        for note in s.backing.notes() {
            println!("note: {} \u{2014} {note}", s.source.label());
        }
    }
    if args.dry_run {
        return 0;
    }

    let batch: Vec<_> = items.iter().map(|r| (r.key(), r.name.clone(), r.bytes)).collect();
    let mut job = fad::tools::Job::start(batch);
    while !job.is_finished() {
        job.poll();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    job.poll();

    let failures = job.failures();
    for o in &failures {
        eprintln!("fad: {}: {}", o.label, o.result.as_ref().err().cloned().unwrap_or_default());
    }
    // Measured by asking the tools again, not by adding up what we hoped for.
    match job.measured {
        Some(bytes) => println!("freed {} (measured)", human(bytes)),
        None => println!("removed {} item(s)", job.done.len() - failures.len()),
    }
    i32::from(!failures.is_empty())
}

/// How a path moved between two scans.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Grew,
    New,
    Gone,
}

/// What changed since the last saved scan of this root. Answers "what did that
/// install just add?", which no single scan can — and "what did that cleanup
/// take?", which needs the paths that are no longer there.
///
/// Both trees are walked together, matching children by name: one pass over
/// each, where resolving every path from the root again was quadratic in the
/// depth and in the width of every directory on the way down.
fn print_since(tree: &Tree, previous: &Tree, at: std::time::SystemTime, args: &Args) -> i32 {
    let threshold = args.min_size.max(1);
    let mut changes: Vec<(PathBuf, i64, Change)> = Vec::new();
    let mut stack = vec![(tree.root(), previous.root(), 0usize)];
    while let Some((now_id, then_id, depth)) = stack.pop() {
        let delta = tree.size(now_id, args.apparent) as i64
            - previous.size(then_id, args.apparent) as i64;
        if delta.unsigned_abs() >= threshold {
            changes.push((tree.path(now_id), delta, Change::Grew));
        }
        if depth >= args.depth {
            continue;
        }
        let mut before: std::collections::HashMap<&str, NodeId> = previous
            .node(then_id)
            .children
            .iter()
            .map(|c| (previous.node(*c).name.as_ref(), *c))
            .collect();
        for c in &tree.node(now_id).children {
            match before.remove(tree.node(*c).name.as_ref()) {
                Some(then) => stack.push((*c, then, depth + 1)),
                // Entirely new: reported once, not once per file inside it.
                None => {
                    let size = tree.size(*c, args.apparent);
                    if size >= threshold {
                        changes.push((tree.path(*c), size as i64, Change::New));
                    }
                }
            }
        }
        // What is left was there last time and is not now. Reported once,
        // at the top of whatever went, with everything it held as the loss.
        for (_, c) in before {
            let size = previous.size(c, args.apparent);
            if size >= threshold {
                changes.push((previous.path(c), -(size as i64), Change::Gone));
            }
        }
    }
    changes.sort_by(|(pa, a, _), (pb, b, _)| b.abs().cmp(&a.abs()).then_with(|| pa.cmp(pb)));

    if args.json {
        let items: Vec<_> = changes
            .iter()
            .map(|(path, delta, change)| {
                serde_json::json!({
                    "path": json_path(path),
                    "delta": delta,
                    "change": format!("{}{}", if *delta < 0 { "-" } else { "+" }, human(delta.unsigned_abs())),
                    "new": *change == Change::New,
                    "gone": *change == Change::Gone,
                })
            })
            .collect();
        print_json_value(&serde_json::json!({
            "root": json_path(tree.root_path()),
            "since": at.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "changes": items,
        }));
        return 0;
    }

    if changes.is_empty() {
        println!("fad: nothing changed by more than {}", human(args.min_size.max(1)));
        return 0;
    }
    for (path, delta, change) in &changes {
        println!(
            "{}{:>7}  {}{}",
            if *delta < 0 { '-' } else { '+' },
            human(delta.unsigned_abs()),
            display_path(path),
            match change {
                Change::New => "  (new)",
                Change::Gone => "  (gone)",
                Change::Grew => "",
            }
        );
    }
    0
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
            .filter(|id| !fad::reclaim::has_reclaimable_ancestor(tree, *id))
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
                    "path": json_path(&tree.path(*id)),
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
    print_json_value(&serde_json::json!({
            "root": json_path(tree.root_path()),
            "bytes": total,
            "size": human(total),
            "categories": cats,
        }));
}

fn node_json(tree: &Tree, id: NodeId, args: &Args, depth: usize) -> serde_json::Value {
    let n = tree.node(id);
    let bytes = if args.apparent { n.total_len } else { n.total_bytes };
    let mut v = serde_json::json!({
        "path": json_path(&tree.path(id)),
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

/// A path as it is shown to a person reading the terminal.
fn display_path(p: &std::path::Path) -> String {
    p.display().to_string()
}

/// A path for JSON output. Filenames are bytes, and on Linux need not be
/// UTF-8; serde refuses such a path outright, and `json!` turned that refusal
/// into a panic, so one oddly named directory under the root took the whole
/// report down. Lossy is the honest rendering for a reader that only takes
/// text — and the entries fad cannot name exactly are listed on stderr.
fn json_path(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Print a JSON report, or say why it could not be printed. Never a panic.
fn print_json_value(v: &serde_json::Value) {
    match serde_json::to_string_pretty(v) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("fad: could not write the JSON report: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn sizes_take_any_spelling_of_a_binary_unit() {
        assert_eq!(parse_size("0"), Ok(0));
        assert_eq!(parse_size("512"), Ok(512));
        assert_eq!(parse_size("512b"), Ok(512));
        for k in ["1k", "1K", "1kb", "1KB", "1KiB", "1kib", " 1 K "] {
            assert_eq!(parse_size(k), Ok(1024), "{k}");
        }
        for g in ["2G", "2g", "2GB", "2GiB", "2gib"] {
            assert_eq!(parse_size(g), Ok(2 << 30), "{g}");
        }
        assert_eq!(parse_size("1.5M"), Ok(3 << 19));
        assert_eq!(parse_size("1T"), Ok(1 << 40));
        assert_eq!(parse_size("1PiB"), Ok(1 << 50));
    }

    #[test]
    fn nonsense_is_refused_rather_than_read_as_something() {
        for bad in ["", "  ", "-5G", "-0", "nan", "NaN", "inf", "-inf", "infinity", "G", "1.2.3", "10X", "1e3", "99999999999P"] {
            assert!(parse_size(bad).is_err(), "accepted {bad:?} as {:?}", parse_size(bad));
        }
    }
}
