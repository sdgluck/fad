//! macOS Time Machine local snapshots.
//!
//! The most invisible disk on a Mac. APFS snapshots are not files: `du` cannot
//! see them, no directory contains them, and a machine can quietly be holding
//! tens of gigabytes of them while every tool that walks the filesystem reports
//! the disk as half empty. This is the one source here that a scan cannot even
//! approximate.
//!
//! `tmutil` will list them and delete them but will not size them, and it is
//! not getting an invented figure — every snapshot is `Measure::Unknown`. A row
//! that says "this is here, this is why your free space does not add up, and
//! this is how it goes" is a better answer than a number nobody can check.

use super::exec::{self, ExecErr};
use super::{Backing, Kind, Measure, Resource, Source, SourceReport, Status};

pub fn probe() -> SourceReport {
    let bin = Source::Snapshots.bin();
    let out = match exec::run(&bin, &["listlocalsnapshots", "/"], super::INFO_TIMEOUT) {
        Ok(s) => s,
        Err(ExecErr::NotInstalled) => return SourceReport::empty(Source::Snapshots, Status::Missing),
        Err(ExecErr::TimedOut) => return SourceReport::empty(Source::Snapshots, Status::TimedOut),
        Err(ExecErr::Failed { stderr, .. }) => {
            return SourceReport::empty(Source::Snapshots, Status::Failed(super::tail(&stderr)));
        }
    };

    let items = parse(&out);
    if items.is_empty() {
        // Nothing to say, and a heading reading "0" would only invite the
        // question of whether we looked.
        return SourceReport::empty(Source::Snapshots, Status::Missing);
    }
    SourceReport {
        source: Source::Snapshots,
        status: Status::Ok,
        backing: Backing::Host,
        items,
        totals: Vec::new(),
    }
}

/// `tmutil listlocalsnapshots /` prints a heading and then one identifier per
/// line, of the form `com.apple.TimeMachine.2026-08-21-094500.local`.
pub fn parse(out: &str) -> Vec<Resource> {
    out.lines()
        .map(str::trim)
        .filter(|l| l.starts_with("com.apple.TimeMachine."))
        .map(|id| {
            let stamp = id
                .trim_start_matches("com.apple.TimeMachine.")
                .trim_end_matches(".local")
                .to_string();
            Resource {
                source: Source::Snapshots,
                kind: Kind::Snapshot,
                // `deletelocalsnapshots` takes the date, not the full name.
                id: stamp.clone(),
                name: stamp.replace('-', " ").to_string(),
                bytes: 0,
                measure: Measure::Unknown,
                reported: String::new(),
                idle: true,
                blocked: None,
                last_used: None,
                restore: None,
                path: None,
                detail: vec![
                    "tmutil does not report a size, so fad will not invent one".into(),
                    "these hold space no directory contains, which is why du cannot \
                     account for it"
                        .into(),
                ],
            }
        })
        .collect()
}
