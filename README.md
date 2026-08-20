# fad

Find what is eating your disk, and delete it, without leaving the terminal.

A keyboard-driven replacement for OmniDiskSweeper: two panes, a live tree ranked
by real disk usage, and a staged batch you review before anything is deleted.

Runs on macOS and Linux.

```
┌ /Users/you  285G  294014 dirs, 2411907 files ────────┬ selection ──────────────┐
│ ▾ you                              285G ████████████ │ /Users/you/Library      │
│   ▾ Library                        168G ████████████ │ 168G · 712041 files     │
│     ▸ Containers                    38G ██▊          │ modified 2 hours ago    │
│     ▸ Caches                       6.2G ▌            │ ── by extension         │
│   ▸ dev                             40G ██▊     ●    │   .js     438M ████████ │
│   ▸ Pictures                        23G █▋           │   .rlib   422M ███████  │
│                                                      │ ── staged · 1 · 40G ──  │
│ ⚠ 2 cloud folder(s) not counted — rerun with --cloud │ ● dev              40G  │
└──────────────────────────────────────────────────────┴─────────────────────────┘
 1 staged · 40G   space stage · x commit · r reclaimable · / filter · ? help
```

## Install

```sh
cargo install --path .
```

## Use

```sh
fad              # your home directory
fad /some/path
fad --json       # non-interactive: the ranked tree as JSON
```

| Key | |
|---|---|
| `j` `k` `↑` `↓` | move · `g` `G` first/last · `ctrl-d` `ctrl-u` half page |
| `l` `→` `enter` | expand · `h` `←` collapse, or jump to the parent |
| `space` | stage / unstage · `A` stage everything in this directory or category |
| `x` | review and commit the staged batch |
| `u` | undo the last committed batch |
| `r` | reclaimable view — build artifacts, caches, VM images |
| `/` | fuzzy filter · `s` cycle sort · `R` rescan |
| `o` `e` `y` | reveal in Finder · open in `$EDITOR` · copy path |
| `?` | help · `q` quit |

### Flags

```
--cross-device    follow mount points into other filesystems
--cloud           descend into iCloud/Dropbox/OneDrive folders (see below)
--apparent        report st_size instead of allocated blocks
--min-size 100M   hide entries below a threshold (--json)
--depth N         how deep to print (--json)
--no-cache        ignore any snapshot and always walk from scratch
--clear-cache     delete every saved snapshot and exit
```

## How it measures

Sizes are **allocated blocks** (`st_blocks × 512`), the same thing `du` reports —
not file length. Sparse files and APFS-compressed files are counted at what they
actually cost. The detail pane shows the apparent size alongside when the two
differ.

A file with several hard links is counted **once**, always against the
lexicographically first of its paths. First-one-wins would make subtotals jump
between runs, because the walk is parallel and arrival order is not stable.

`fad --json` totals match `du -sk` exactly; there is a test that asserts it.

## What it will not walk into

- **Other filesystems.** Mount points are shown but not entered, so a network
  share or an external drive cannot stall the scan. `--cross-device` opts in.
  This is what stops an NFS or SMB share, or a FUSE mount like rclone, from
  hanging a scan on Linux.
- **Cloud folders (macOS).** iCloud Drive, Dropbox, OneDrive and friends are
  backed by FileProvider extensions. They sit on the boot volume and report the
  boot volume's device number, so the mount check cannot see them — but
  `readdir` inside one can block on the network for *minutes*. They are detected
  by the `com.apple.file-provider-domain-id` xattr, skipped, and reported in a
  banner. `--cloud` opts in. Linux needs no equivalent: the clients that can
  stall a walk are real mounts, and Dropbox and friends sync into plain local
  directories.
- **What it cannot read.** Directories that refuse to open are counted and
  reported, with a hint about granting Full Disk Access to your terminal.

Nothing is skipped silently. If a number is incomplete, the UI says so.

## Deleting

`space` stages; `x` opens a confirmation showing the item count, the total
reclaimed, the largest items by name, and anything the guard refused. `enter`
commits.

The default is the system trash, and `u` restores the whole batch in place. `D`
in the confirmation toggles to a permanent delete, and says plainly that it
cannot be undone.

| | |
|---|---|
| macOS | `NSFileManager.trashItemAtURL:`, so Finder's "Put Back" works |
| Linux | the FreeDesktop.org trash spec: `~/.local/share/Trash`, or the per-filesystem `$topdir/.Trash-$uid` when the item lives on another volume |

Both implementations report *where the item landed*, which is what makes fad's
own undo a plain rename back. On Linux, restoring also removes the `.trashinfo`
file, so your desktop's trash does not keep showing an entry whose file is gone.

Trashing is always a rename, never a copy — which is why the Linux side picks a
trash directory by device number rather than by path.

Refused always: `/`, your home directory, the platform's system directories
(`/System` and `/Users` on macOS, `/usr`, `/etc`, `/home` and friends on Linux),
the scan root, and anything containing the scan root.

## Where it keeps things

| | macOS | Linux |
|---|---|---|
| scan snapshots | `~/Library/Caches/fad` | `$XDG_CACHE_HOME/fad` |
| undo journal | `~/Library/Application Support/fad` | `$XDG_DATA_HOME/fad` |

The journal is deliberately not in a cache directory: a cleaner is entitled to
wipe a cache, and losing your undo history to one would be a nasty surprise.
`FAD_CACHE_DIR` and `FAD_STATE_DIR` override both.

## Speed

On a 285 GB home directory with 2.4M files (M-series Mac, APFS):

| | |
|---|---|
| first frame | 14 ms |
| first sizes on screen | ~100 ms |
| complete totals, from snapshot | ~1.3 s |
| complete totals, cold | ~21 s |
| `du -sh` on the same tree | ~3.4× slower |

The UI opens on an empty tree and fills in as a parallel walk streams results —
it never waits for a scan. The previous scan is kept as a snapshot in
`~/Library/Caches/fad` and read on a background thread, so complete (if slightly
stale) totals arrive in about a second while the fresh walk continues behind
them. Snapshots are only written for scans that ran to completion.

## Development

```sh
cargo test                                    # du-parity, real trash round-trips, tree arithmetic, rendering
cargo run --release --example statbench -- ~  # per-entry stat cost
cargo run --release --example cachebench -- ~ # snapshot build/encode/load timings
```

To run the suite on Linux from a Mac:

```sh
docker run --rm -v "$PWD":/src:ro -w /work rust:latest bash -c \
  'cp -r /src/src /src/tests /src/examples /src/Cargo.toml /src/Cargo.lock /work/ && cargo test'
```

Tests that redirect `HOME`, `XDG_DATA_HOME`, `FAD_STATE_DIR` or `FAD_CACHE_DIR`
must hold `common::env_lock()` for their whole body — environment variables are
process-global and `cargo test` runs a binary's tests on several threads at
once. They use `common::isolate()` so a test run can never read or consume the
real user's trash, cache, or undo history.
