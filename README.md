# fad

**f**ind **a**nd **d**estroy — a terminal tool for finding what is using your
disk and deleting it.

`fad` walks a directory tree, ranks everything by real disk usage, and shows it
in a two-pane TUI: a live tree on the left, details of the selected item on the
right. You mark items for deletion, review the batch, and commit it. Deleted
items go to the system trash by default and can be restored with one keystroke.

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
fad              # scans your home directory
fad /some/path   # scans a directory
fad --json       # prints the ranked tree as JSON and exits
```

The UI opens immediately and fills in as the scan streams results. If a previous
scan of the same root was saved, its totals appear while the fresh walk catches
up — and its entry count becomes the denominator for a progress bar and an ETA,
which is why the first scan of a directory has neither.

While the walk runs, the bottom of the tree pane shows which directory it is in
and how many entries a second it is getting through. A three-minute scan with no
sign of movement reads as a hang.

### Keys

| Key | |
|---|---|
| `j` `k` `↑` `↓` | move · `g` `G` first/last · `ctrl-d` `ctrl-u` jump 10 lines |
| `l` `→` `enter` | expand · `h` `←` collapse, or jump to the parent |
| `space` | stage / unstage · `A` stage everything in this directory or category |
| `x` | open the staging basket — review, unstage, commit |
| `u` | undo the last committed batch |
| `r` | reclaimable view — build artifacts, package caches, app caches, VM images |
| `d` | duplicate view — files whose contents are byte-for-byte equal |
| `a` | age filter — cycle: any age → untouched 90 days → 1 year → 2 years |
| `/` | fuzzy filter (`enter` to keep it, `esc` to clear) |
| `s` | cycle sort: size → count → modified → name · `R` rescan |
| `o` `e` `y` | reveal in your file manager · open in `$EDITOR` · copy path |
| `i` | never rank this again — adds it to your ignore list |
| `?` | help · `q` or `esc` quit |

The mouse works too: click a row to select it, click its arrow to open or close
it, click the left edge to stage it, and use the wheel to move the selection.
Capturing the mouse takes over your terminal's own text selection, so
`--no-mouse` turns it off.

In the basket: `space` unstages the selected item, or the whole group when the
cursor is on a heading; `C` clears the batch; `enter` goes on to the
confirmation. It also reports how much free space you will have when the batch
lands, which is the number you came for.

In the confirmation screen: `D` toggles between trash and permanent delete,
`enter` or `y` commits, `esc` or `q` goes back to the basket.

### Flags

```
--cross-device    follow mount points into other filesystems
--cloud           descend into iCloud/Dropbox/OneDrive folders
--apparent        report st_size instead of allocated blocks
--reclaim         open in the reclaimable view; with --json, print that set
--json            dump the ranked tree as JSON instead of opening the UI
--min-size 100M   hide entries below a threshold (--json)
--depth N         how deep to print (--json, default 2)
--no-mouse        do not capture the mouse, so text selection keeps working
--no-cache        ignore any snapshot and always walk from scratch
--clear-cache     delete every saved snapshot and exit
```

## Age

Size says what a directory is; age says whether you still want it. The detail
pane breaks the selection down into four buckets — under 90 days, 90 days to a
year, one to two years, over two — so 40G of `dev` reads as 34G of `dev` you
abandoned.

`a` turns that into a filter, hiding any subtree with a recent write in it. The
test is the newest *file* below an entry, not the directory's own `mtime`:
a directory's timestamp moves every time a child is renamed, so a project
nobody has opened in two years would otherwise look freshly touched the moment
it was reorganised.

## Reclaimable

`r` shows only what the built-in rules recognise: build artifacts, package
caches, app caches and VM images, grouped by category.

A build directory is matched against its siblings, never its name alone — a
`target` is only a Rust build directory when there is a `Cargo.toml` next to it.
Having proved which tool made it, `fad` can also say what puts it back, so the
detail pane shows `cargo build` or `npm install` rather than a generic promise
that your next build will handle it.

`fad --reclaim --json` prints the same set for a script, each entry carrying its
path, its size, and that restore command:

```sh
fad --reclaim --json ~/dev | jq -r '.categories[].items[] | "\(.size)\t\(.path)"'
```

## Duplicates

`d` looks for files that exist more than once. Files are grouped by exact size
first, which is free — the scan already knows every size. Groups of two or more
are fingerprinted from their first and last 64K, and only what survives that is
read end to end and hashed with SHA-256.

A group is shown only once every member has been hashed in full. Same size and
same ends is a *likely* duplicate, and inviting you to delete one on that basis
is how a tool like this destroys your work. If the read budget runs out, the
unverified groups are counted in a banner and never listed.

Only files over 1M are considered, and hardlinked copies are skipped: they
already share their storage, so deleting one frees nothing.

`A` on a group stages every copy but the newest.

## What grew

`fad` already keeps a snapshot of the last scan of each root. The detail pane
diffs the selection against it and reports the change: `+12G since 3 days ago`,
or `new since last week`.

Growth is usually more actionable than size. A cache that put on 12G this week
is a better target than a stable 20G one.

## Sizes

Sizes are allocated blocks (`st_blocks × 512`), matching `du`. Sparse and
APFS-compressed files are counted at what they actually cost on disk; use
`--apparent` for file length instead. Where the two differ, the detail pane
shows both.

A file with several hard links is counted once, against the lexicographically
first of its paths.

## What is not scanned

- **Other filesystems.** Mount points are shown but not entered. `--cross-device`
  opts in.
- **Cloud folders (macOS).** iCloud Drive, Dropbox, OneDrive and similar
  FileProvider-backed folders are skipped, because reading inside one can block
  on the network for minutes. `--cloud` opts in. Linux has no equivalent skip.
- **Directories that cannot be read.** Counted and reported; on macOS, granting
  your terminal Full Disk Access usually fixes this.

Anything skipped is reported in a banner, so an incomplete number is never shown
as a complete one.

## Deleting

`space` stages an item; the staged batch and its total are shown in the right
pane and the status bar. `x` opens a confirmation showing the item count, the
total to be reclaimed, the largest items by name, and anything the guard
refused.

Deletion goes to the system trash by default, and `u` restores the whole batch
to its original location. `D` in the confirmation switches to a permanent
delete, which cannot be undone.

| | |
|---|---|
| macOS | the Finder trash — "Put Back" works |
| Linux | the FreeDesktop.org trash: `~/.local/share/Trash`, or `$topdir/.Trash-$uid` for items on another filesystem |

These paths are always refused: `/`, your home directory, system directories
(`/System` and `/Users` on macOS; `/usr`, `/etc`, `/home` and friends on Linux),
the scan root, and any parent of the scan root.

## Ignoring things

Everyone has directories they will not delete and do not want to scroll past
every session. `i` adds the selection to `~/.config/fad/ignore`
(`$XDG_CONFIG_HOME/fad/ignore`, or `FAD_CONFIG_DIR`), which is a plain text file
you can also edit by hand:

```text
# a comment
/Users/you/VMs                     an absolute path, and everything under it
~/Pictures/Photos.photoslibrary
*.sparsebundle                     a glob, matched against each entry's name
Steam/                             a glob, directories only
```

Ignoring is not a way to make a number smaller. An ignored entry still counts
towards every total above it — a size that quietly omits things is the one
failure this tool cannot afford — so it is hidden from the views, reported in a
banner, and refused for deletion.

## Files it writes

| | macOS | Linux |
|---|---|---|
| ignore list | `~/.config/fad` | `$XDG_CONFIG_HOME/fad` |
| scan snapshots | `~/Library/Caches/fad` | `$XDG_CACHE_HOME/fad` |
| undo journal | `~/Library/Application Support/fad` | `$XDG_DATA_HOME/fad` |

`FAD_CONFIG_DIR`, `FAD_CACHE_DIR` and `FAD_STATE_DIR` override these. Snapshots are only written
for scans that ran to completion, and `--clear-cache` removes them all.

## Development

```sh
cargo test                                    # du-parity, trash round-trips, tree arithmetic, rendering
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
