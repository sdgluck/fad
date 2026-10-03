//! Asking Docker (or Podman) what it is holding.
//!
//! Everything that talks to a subprocess is in [`probe`]; everything that turns
//! bytes into [`Resource`]s is a pure function below it. That split is not
//! tidiness — it is what lets the whole feature be tested on a machine with no
//! daemon, against captured output in `tests/fixtures`.
//!
//! Two calls, both cheap: `system df` for the tool's own deduplicated totals,
//! and `system df -v` for the items. A third, `info`, decides [`Backing`] and
//! is best-effort: not knowing where the bytes live is worth a vaguer caveat,
//! not a failed probe.

use std::path::PathBuf;

use serde_json::Value;

use super::exec::{self, ExecErr};
use super::{Backing, Kind, Measure, Resource, Source, SourceReport, Status};

/// The oldest CLI whose `system df --format "{{json .}}"` has the shape below.
/// Older ones print a human table, and a number read off a column-aligned table
/// is not a number to delete by.
const NEED: &str = "docker 23.0";

pub fn probe(source: Source) -> SourceReport {
    let bin = source.bin();

    // These three questions are independent, and `system df` is not fast: on a
    // machine with a few hundred build-cache records it measured just under
    // sixteen seconds, because the daemon walks its own store to answer. Asking
    // them one after another would triple a wait that is already the slowest
    // thing in this module, so they go out together and the probe costs the
    // slowest one rather than the sum.
    let (b1, b2, b3) = (bin.clone(), bin.clone(), bin.clone());
    let totals_t = std::thread::spawn(move || {
        exec::run(&b1, &["system", "df", "--format", "{{json .}}"], super::PROBE_TIMEOUT)
    });
    let verbose_t = std::thread::spawn(move || {
        exec::run(&b2, &["system", "df", "-v", "--format", "{{json .}}"], super::PROBE_TIMEOUT)
    });
    let backing_t = std::thread::spawn(move || backing(&b3));

    let totals_raw = match totals_t.join().unwrap_or(Err(ExecErr::TimedOut)) {
        Ok(s) => s,
        Err(e) => {
            // Let the other two finish rather than leaving them running behind
            // a report that has already been returned.
            let _ = verbose_t.join();
            let _ = backing_t.join();
            return SourceReport::empty(source, status_for(e));
        }
    };

    let totals = parse_totals(&totals_raw);
    if totals.is_empty() {
        let _ = verbose_t.join();
        let _ = backing_t.join();
        // It ran and said something we do not recognise. That is a version
        // problem, not a disk problem, and guessing is how a delete tool ends
        // up deleting the wrong thing.
        return SourceReport::empty(
            source,
            Status::Unsupported { need: NEED, found: version(&bin).unwrap_or_else(|| "?".into()) },
        );
    }

    // The totals are already worth showing; losing the item list costs detail,
    // not honesty.
    let items = match verbose_t.join().unwrap_or(Err(ExecErr::TimedOut)) {
        Ok(s) => parse_verbose(&s, source, &totals),
        Err(_) => Vec::new(),
    };
    let backing = backing_t.join().unwrap_or(Backing::Host);

    SourceReport { source, status: Status::Ok, backing, items, totals }
}

fn status_for(e: ExecErr) -> Status {
    match e {
        ExecErr::NotInstalled => Status::Missing,
        ExecErr::TimedOut | ExecErr::Stopped => Status::TimedOut,
        ExecErr::Failed { stderr, .. } => {
            if looks_like_no_daemon(&stderr) {
                Status::NotRunning(super::tail(&stderr))
            } else {
                Status::Failed(super::tail(&stderr))
            }
        }
    }
}

/// Docker and Podman both say some version of this, and the wording has changed
/// more than once. Match on the durable parts.
fn looks_like_no_daemon(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("cannot connect")
        || s.contains("connection refused")
        || s.contains("is the docker daemon running")
        || s.contains("daemon is not running")
        || s.contains("no such file or directory")
}

fn version(bin: &str) -> Option<String> {
    exec::run(bin, &["info", "--format", "{{.ServerVersion}}"], super::INFO_TIMEOUT)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Where this daemon's storage physically lives.
///
/// Asked of the daemon rather than inferred from `cfg!(target_os)`, because
/// both directions of that inference are wrong: Docker Desktop runs on Linux
/// too, and OrbStack is a VM on macOS that reclaims its own disk.
fn backing(bin: &str) -> Backing {
    let os = exec::run(bin, &["info", "--format", "{{.OperatingSystem}}"], super::INFO_TIMEOUT)
        .unwrap_or_default();
    backing_for(os.trim())
}

/// Split out from [`backing`] so it can be tested without a daemon.
///
/// Asked of the daemon rather than inferred from `cfg!(target_os)`, because
/// both directions of that inference are wrong: Docker Desktop runs on Linux
/// too, and OrbStack is a VM on macOS that reclaims its own disk.
pub fn backing_for(operating_system: &str) -> Backing {
    let os = operating_system.to_ascii_lowercase();
    let home = crate::paths::home();

    // OrbStack trims its own disk image, so its bytes really do come back.
    if os.contains("orbstack") {
        let disk = home.as_ref().and_then(|h| orbstack_disk(h));
        let host_bytes = disk.as_ref().and_then(|p| on_disk_bytes(p));
        return Backing::VmDisk { disk, host_bytes, shrinks: true };
    }

    let candidates: &[&str] = if os.contains("docker desktop") {
        &[
            "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
            "Library/Containers/com.docker.docker/Data/vms/0/Docker.raw",
        ]
    } else if os.contains("colima") || os.contains("lima") {
        &[".colima/_lima/colima/diffdisk", ".lima/default/diffdisk"]
    } else if cfg!(target_os = "macos") {
        // There is no native Docker on macOS: whatever this is, it is a VM.
        // Worth saying even when we cannot find the file.
        &[
            "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
            ".colima/_lima/colima/diffdisk",
            ".lima/default/diffdisk",
        ]
    } else {
        return Backing::Host;
    };

    let disk = home
        .as_ref()
        .and_then(|h| candidates.iter().map(|c| h.join(c)).find(|p| p.exists()));
    let host_bytes = disk.as_ref().and_then(|p| on_disk_bytes(p));
    Backing::VmDisk { disk, host_bytes, shrinks: false }
}

/// OrbStack keeps its disk in a group container named after a team identifier
/// that is not ours to hardcode, so look for the suffix instead.
fn orbstack_disk(home: &std::path::Path) -> Option<PathBuf> {
    let groups = home.join("Library/Group Containers");
    let entry = std::fs::read_dir(groups)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().ends_with(".orbstack"))?;
    let disk = entry.path().join("data/data.img.raw");
    disk.exists().then_some(disk)
}

/// Allocated blocks, not `st_size`: a disk image is usually sparse, and its
/// apparent length is not what it is costing anyone.
fn on_disk_bytes(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| m.blocks() * 512)
}

// ---------------------------------------------------------------------------
// Parsing. Pure, and the only part with tests.
// ---------------------------------------------------------------------------

/// A size as Docker writes it, in bytes.
///
/// Docker counts in SI — `3.24GB` is 3.24 × 10⁹, not 3.24 × 2³⁰ — while `fad`
/// renders in binary like `du`. Converting on the SI reading is correct and
/// means `fad` shows Docker's `3.24GB` as `3.0G`; the detail pane carries
/// Docker's own string beside it so that is a difference in units and not an
/// apparent discrepancy.
///
/// Handles the shapes Docker actually emits: `0B`, `759.2MB`, `N/A`,
/// `1.518GB (38%)`, and `0B (virtual 3.24GB)` — where the leading figure is the
/// one that belongs to this item.
pub fn parse_si(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("n/a") {
        return None;
    }
    // Anything after a space or a bracket is a gloss on the number, not part of
    // it.
    let head = s.split([' ', '(']).next()?.trim();

    let split = head.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(head.len());
    let (num, unit) = head.split_at(split);
    let n: f64 = num.trim().parse().ok()?;

    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "pb" => 1e15,
        // Not a shape Docker emits, but Podman has been known to, and reading
        // a binary unit as SI would be a 7% lie per step.
        "kib" => 1024.0,
        "mib" => 1024f64.powi(2),
        "gib" => 1024f64.powi(3),
        "tib" => 1024f64.powi(4),
        _ => return None,
    };
    Some((n * mult) as u64)
}

fn kind_of(type_field: &str) -> Option<Kind> {
    match type_field {
        "Images" => Some(Kind::Image),
        "Containers" => Some(Kind::Container),
        "Local Volumes" => Some(Kind::Volume),
        "Build Cache" => Some(Kind::BuildCache),
        _ => None,
    }
}

/// The tool's own deduplicated figures, one JSON object per line, as
/// `(kind, size, reclaimable)`.
///
/// These are the only numbers a heading may show. Adding up the per-item sizes
/// from [`parse_verbose`] would double-count every shared layer.
pub fn parse_totals(out: &str) -> Vec<(Kind, u64, u64)> {
    let mut v = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(o) = serde_json::from_str::<Value>(line) else { continue };
        let Some(kind) = o.get("Type").and_then(Value::as_str).and_then(kind_of) else { continue };
        let size = o.get("Size").and_then(Value::as_str).and_then(parse_si).unwrap_or(0);
        let recl = o.get("Reclaimable").and_then(Value::as_str).and_then(parse_si).unwrap_or(0);
        v.push((kind, size, recl));
    }
    v
}

fn str_of(o: &Value, key: &str) -> String {
    o.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn count_of(o: &Value, key: &str) -> u64 {
    o.get(key)
        .and_then(|v| v.as_str().and_then(|s| s.trim().parse().ok()).or_else(|| v.as_u64()))
        .unwrap_or(0)
}

const NONE: &str = "<none>";

/// One item per thing the tool is holding, except build cache — see
/// [`build_cache`].
pub fn parse_verbose(out: &str, source: Source, totals: &[(Kind, u64, u64)]) -> Vec<Resource> {
    let Ok(root) = serde_json::from_str::<Value>(out.trim()) else { return Vec::new() };
    let arr = |k: &str| -> Vec<Value> {
        root.get(k).and_then(Value::as_array).cloned().unwrap_or_default()
    };

    let mut items = Vec::new();
    items.extend(arr("Images").iter().map(|o| image(o, source)));
    items.extend(arr("Containers").iter().map(|o| container(o, source)));
    items.extend(arr("Volumes").iter().map(|o| volume(o, source)));
    if let Some(bc) = build_cache(&arr("BuildCache"), source, totals) {
        items.push(bc);
    }
    items
}

fn image(o: &Value, source: Source) -> Resource {
    let repo = str_of(o, "Repository");
    let tag = str_of(o, "Tag");
    let dangling = repo == NONE || repo.is_empty();
    let name = if dangling { "<dangling>".to_string() } else { format!("{repo}:{tag}") };

    let used_by = count_of(o, "Containers");
    let reported = str_of(o, "Size");
    // `UniqueSize` is this image's own layers. It is the only per-image figure
    // that can be added to another one without lying.
    let unique = parse_si(&str_of(o, "UniqueSize")).unwrap_or(0);
    let shared = parse_si(&str_of(o, "SharedSize")).unwrap_or(0);

    let mut detail = vec![format!(
        "docker reports {reported} for this image, of which {} is layers it shares",
        crate::format::human(shared)
    )];
    if let Some(since) = nonempty(str_of(o, "CreatedSince")) {
        detail.push(format!("created {since}"));
    }

    Resource {
        source,
        kind: Kind::Image,
        id: str_of(o, "ID"),
        name,
        bytes: unique,
        measure: Measure::Unique { shared },
        reported,
        idle: used_by == 0,
        blocked: (used_by > 0).then(|| {
            format!("used by {used_by} container{}", if used_by == 1 { "" } else { "s" })
        }),
        last_used: nonempty(str_of(o, "CreatedSince")),
        restore: (!dangling && tag != NONE).then(|| format!("docker pull {repo}:{tag}")),
        path: None,
        detail,
    }
}

fn container(o: &Value, source: Source) -> Resource {
    let state = str_of(o, "State");
    let running = state == "running";
    let reported = str_of(o, "Size");
    // The writable layer only. `SizeRootFs` would include the image, which the
    // image rows already account for, and counting it here would double it.
    let bytes = parse_si(&reported).unwrap_or(0);

    let mut detail = vec![format!("{state} \u{b7} {}", str_of(o, "Image"))];
    detail.push("the size is its writable layer; the image is counted separately".into());
    if let Some(status) = nonempty(str_of(o, "Status")) {
        detail.push(status);
    }

    Resource {
        source,
        kind: Kind::Container,
        id: str_of(o, "ID"),
        name: nonempty(str_of(o, "Names")).unwrap_or_else(|| str_of(o, "ID")),
        bytes,
        measure: Measure::Exact,
        reported,
        idle: !running,
        blocked: running.then(|| "running".to_string()),
        last_used: nonempty(str_of(o, "RunningFor")),
        restore: None,
        path: None,
        detail,
    }
}

fn volume(o: &Value, source: Source) -> Resource {
    let links = count_of(o, "Links");
    let reported = str_of(o, "Size");
    let bytes = parse_si(&reported).unwrap_or(0);
    let known = parse_si(&reported).is_some();

    Resource {
        source,
        kind: Kind::Volume,
        id: str_of(o, "Name"),
        name: str_of(o, "Name"),
        bytes,
        // A volume's bytes are its own; nothing shares them.
        measure: if known { Measure::Exact } else { Measure::Unknown },
        reported,
        idle: links == 0,
        blocked: (links > 0).then(|| {
            format!("attached to {links} container{}", if links == 1 { "" } else { "s" })
        }),
        last_used: None,
        restore: None,
        path: nonempty(str_of(o, "Mountpoint")).map(PathBuf::from),
        detail: vec!["whatever is in here is the one thing on this screen that is not \
                      reproducible"
            .into()],
    }
}

/// The cold build cache, as one item.
///
/// No Docker CLI can remove an individual cache record: the only handle is
/// `builder prune`, which takes the lot. Listing six hundred rows that cannot
/// be acted on individually would be a menu of things that do not work, so the
/// records are collapsed into one item sized by the daemon's own reclaimable
/// figure, with the record count in the detail.
fn build_cache(records: &[Value], source: Source, totals: &[(Kind, u64, u64)]) -> Option<Resource> {
    let (_, size, reclaimable) = *totals.iter().find(|(k, _, _)| *k == Kind::BuildCache)?;
    if reclaimable == 0 {
        return None;
    }

    let cold = records.iter().filter(|o| str_of(o, "InUse") != "true").count();
    let oldest = records
        .iter()
        .filter(|o| str_of(o, "InUse") != "true")
        .filter_map(|o| nonempty(str_of(o, "LastUsedSince")))
        .next_back();

    let mut detail = vec![format!(
        "{} of build cache in total, {} of it cold",
        crate::format::human(size),
        crate::format::human(reclaimable)
    )];
    if cold > 0 {
        detail.push(format!("{cold} cache record(s) listed"));
    }
    detail.push(
        "there is no way to remove one record; this prunes every cold one at once".into(),
    );

    Some(Resource {
        source,
        kind: Kind::BuildCache,
        id: "*".into(),
        name: "cold build cache".into(),
        bytes: reclaimable,
        measure: Measure::Exact,
        reported: crate::format::human(reclaimable),
        idle: true,
        blocked: None,
        last_used: oldest.map(|s| format!("oldest last used {s}")),
        restore: None,
        path: None,
        detail,
    })
}

fn nonempty(s: String) -> Option<String> {
    let s = s.trim().to_string();
    (!s.is_empty() && s != NONE).then_some(s)
}

/// Ask the tool again and return its totals, for measuring what a batch
/// actually freed rather than predicting it.
pub fn measure(source: Source) -> Option<Vec<(Kind, u64, u64)>> {
    measure_until(source, &std::sync::atomic::AtomicBool::new(false))
}

/// [`measure`], abandoned as soon as `stop` is set.
pub fn measure_until(
    source: Source,
    stop: &std::sync::atomic::AtomicBool,
) -> Option<Vec<(Kind, u64, u64)>> {
    let out = exec::run_until(
        &source.bin(),
        &["system", "df", "--format", "{{json .}}"],
        super::MEASURE_TIMEOUT,
        stop,
    )
    .ok()?;
    let t = parse_totals(&out);
    (!t.is_empty()).then_some(t)
}
