# fad

**f**ind **a**nd **d**estroy — a terminal tool that shows what is using your
disk and deletes it.

`fad` scans a directory, ranks everything by disk usage, and shows it in two
panes: the tree on the left, details of the selection on the right. Stage items,
review them, and commit. Deleted items go to the trash by default and can be
restored.

Runs on macOS and Linux.

```
┌ /Users/you  285G  294014 dirs, 2411907 files  71G free of 1.0T ┬ selection ──────┐
│ ▾ you                              285G ████████████ │ /Users/you/Library        │
│   ▾ Library                        168G ████████████ │ 168G on disk              │
│     ▸ Containers                    38G ██▊          │ 59% of scan               │
│     ▸ Caches                       6.2G ▌            │ 712041 files · 41027 dirs │
│   ▸ dev                             40G ██▊     ●    │ modified 2 hours ago      │
│   ▸ Pictures                        23G █▋           │ ── where it goes          │
│                                                      │   Containers  38G 23% ███ │
│                                                      │   Caches     6.2G  4% ▊   │
│                                                      │   Developer  2.1G  1% ▎   │
│                                                      │   top 3 of 41 hold 82%    │
│                                                      │ ── by extension           │
│                                                      │   .js    438M  12k ████   │
│                                                      │   .rlib  422M  358 ███    │
│                                                      │ ── biggest files          │
│                                                      │   Docker.raw   68G ████   │
│                                                      │   vm.qcow2     12G ▉      │
│                                                      │ ── staged · 1 · 40G ──    │
│ ⚠ 2 cloud folder(s) not counted — rerun with --cloud │ ● dev              40G    │
└──────────────────────────────────────────────────────┴───────────────────────────┘
 1 staged · 40G  space stage · x basket · r reclaimable · t tools · / filter · ? help · q quit
```

## Install

Requires Rust 1.88 or newer.

```sh
cargo install --git https://github.com/sdgluck/fad
```

From a checkout: `cargo install --path .`

## Use

```sh
fad              # scan your home directory
fad /some/path   # scan a directory
fad --json       # print the ranked tree as JSON
```

The UI opens immediately and fills in as the scan runs. If an earlier scan of
the same directory was saved, its numbers show until the new scan finishes, and
a progress bar and ETA are shown.

### Keys

| Key | |
|---|---|
| `j` `k` `↑` `↓` | move · `g` `G` first/last · `ctrl-d` `ctrl-u` jump 10 lines |
| `l` `→` `enter` | expand · `h` `←` collapse or go to parent |
| `space` | stage / unstage · `A` stage everything in this directory or group |
| `x` | open the basket: review, unstage, commit |
| `u` | undo the last batch |
| `U` | undo history: restore any of the last 20 batches |
| `E` | empty from the trash what `fad` put there |
| `r` | reclaimable view: build output, package caches, app caches, VM images |
| `t` | tool storage: what Docker, Podman and Time Machine hold |
| `d` | duplicate files |
| `L` | in the duplicate view: make copies share storage |
| `a` | age filter: any → untouched 90 days → 1 year → 2 years |
| `/` | filter what is on screen (`enter` keeps it, `/` edits it, `esc` clears it) |
| `f` | find anything in the tree by name, biggest first |
| `s` | sort: size → count → modified → name |
| `R` | rescan |
| `S` | show the next detail breakdown, when they don't all fit |
| `o` | reveal in Finder / file manager |
| `e` | open in `$EDITOR` |
| `y` | copy path |
| `i` | add to your ignore list |
| `!` | list what was skipped, unreadable or ignored |
| `esc` | back out one level (filter, view, age filter); quits at the top |
| `?` | help |
| `q` | quit |
| `ctrl-c` | quit without choosing a path (for `--print-path`) |

Quitting asks first if anything is staged.

Mouse: click to select, click the arrow to expand, click the left edge to stage,
scroll to move. `--no-mouse` turns it off so your terminal's text selection
works.

**Basket:** `space` unstages the item (or the group, on a heading), `C` clears
everything, `enter` goes to the confirmation. Shows your free space after the
batch.

**Confirmation:** `D` switches between trash and permanent delete, `enter` or
`y` commits, `esc` or `q` goes back.

### Flags

```
--cross-device    scan into other filesystems
--cloud           scan into iCloud/Dropbox/OneDrive folders
--apparent        show file length instead of disk usage
--reclaim         open in the reclaimable view; with --json, print it
--tools           open in the tool storage view; with --json, print it
--yes             with --reclaim or --tools: delete without opening the UI
--max 10G         with --yes: skip anything that would go over this total
--dry-run         with --yes: print what would be deleted, delete nothing
--permanent       with --reclaim --yes: delete instead of trashing
--since           print what changed since the last saved scan
--print-path      print the selected path on exit
--json            print the tree as JSON
--min-size 100M   hide smaller entries (--json, --since, --yes)
--depth N         how deep to print (--json, --since; default 2)
--init zsh        print shell completions and a `fad-cd` function
--man             print the man page
--no-mouse        don't capture the mouse
--no-cache        ignore saved scans
--clear-cache     delete saved scans and exit
```

`--dry-run`, `--permanent` and `--max` need `--yes`. `--yes` needs `--reclaim`
or `--tools`. `--permanent` can't be used with `--tools` (tool removals are
always permanent). `--since` can't be combined with other modes.

Sizes use powers of 1024: `K`, `M`, `G`, `T`, `P`, any case, with or without
`B`/`iB`.

## Detail pane

For the selection:

- share of the whole scan
- **where it goes**: the three biggest children, and how much of the total they hold
- **by extension**: size and count per file type
- **by age**: how much is under 90 days, 90 days–1 year, 1–2 years, over 2 years old
- **biggest files**: the largest files anywhere below
- **file sizes**: bytes in files `<1M`, `1-10M`, `10-100M`, `>100M`
- **growth**: change since the last saved scan, e.g. `+12G since 3 days ago`

Breakdowns cover the whole subtree. On very large selections they are sampled
and marked `(sampled)`. A cut-off list says so, e.g. `top 7 of 10`.

## Find

`f` searches every entry in the tree by name and lists the 100 biggest matches
by path. `enter` jumps there, opening parent directories and clearing any filter
that hides it.

## Age

`a` hides anything with a file written more recently than the chosen age. Age
comes from the newest file below an entry, not the directory's timestamp.

## Reclaimable

`r` lists things that can be regenerated, grouped by category, with the command
that rebuilds each (`cargo build`, `npm install`, …).

- Build directories only count next to their project file, e.g. `target` next to
  `Cargo.toml`.
- Gradle: `~/.gradle/caches`, `~/.gradle/wrapper/dists`, `~/.gradle/daemon`, and
  a project's `.gradle` next to its build script.
- `vendor`: next to `composer.json`, next to `go.mod` with `vendor/modules.txt`,
  or Ruby's `vendor/bundle`.
- On Linux, `~/.cache`.
- VM and disk images are listed but never picked by `--yes`.

```sh
fad --reclaim --json ~/dev
fad --reclaim --yes --dry-run ~/dev     # show what would go
fad --reclaim --yes --max 10G ~/dev     # trash up to 10G, largest first
```

`--reclaim --yes` only uses these rules, respects your ignore list, prints every
path, and trashes unless `--permanent` is given. It skips anything containing
another filesystem, a cloud folder or an unreadable directory.

## Duplicates

`d` finds files over 1M with identical contents (SHA-256, fully read). Files
that already share storage (hard links, clones) are left out.

- `A` on a group stages every copy except the newest.
- `L` replaces the other copies with clones of the newest, so they share
  storage. Every path keeps working, with its own permissions, timestamps and
  extended attributes. Works on APFS, btrfs and XFS; elsewhere it does nothing.
  Copies that changed since they were checked, or that have other hard links,
  are skipped.

After `L`, `du` and the tree still show full sizes; the free-space figure goes
up.

## Tool storage

`t` asks Docker, Podman and Time Machine what they hold: images, containers,
volumes, build cache, local snapshots. Tools that aren't installed are not
shown; tools that don't respond are shown as such.

- Group totals are the tool's own figures. Image rows show what each image
  owns, with shared layers shown separately.
- Running containers, images in use and attached volumes can't be staged.
- Tool items are staged into the same basket but can't be trashed or undone.
- With Docker Desktop or Colima, removing images frees space inside the VM disk,
  not on your disk. `fad` shows a warning and doesn't count it.
- `y` copies the command instead of running it.

```sh
fad --tools --json
fad --tools --yes --dry-run
fad --tools --yes --max 10G
```

## What changed

```sh
fad --since ~/dev
```

Prints what grew, what is new and what is gone since the last saved scan of that
directory, biggest first, then saves the new scan. If there is no earlier scan
with the same `--cross-device`/`--cloud` options, it saves one and exits 1.

## Shell integration

```sh
eval "$(fad --init zsh)"                     # also bash, fish
fad --man > /usr/local/share/man/man1/fad.1
```

Adds completions and `fad-cd`: run `fad`, quit with `q` on a directory, and
your shell moves there (to the parent, for a file). `ctrl-c` quits without
moving. In zsh, put the `eval` after `compinit`.

`cd "$(fad --print-path)"` does the same without the function.

## What is not scanned

- **Other filesystems.** Shown, not entered. Use `--cross-device`.
- **Cloud folders (macOS).** iCloud, Dropbox, OneDrive, etc. Use `--cloud`.
- **Unreadable directories.** Reported with the reason. On macOS, permission
  errors are usually fixed by giving your terminal Full Disk Access.
- **Tool storage** (Docker etc.). Use `t`.

Skipped items are shown in a banner. `!` lists them by path. Ignored items still
count towards totals; unreadable ones don't.

## Sizes

Sizes are disk usage (`st_blocks × 512`), the same as `du`. `--apparent` shows
file length instead. Hard-linked files are counted once.

The header shows free space on the volume.

## Deleting

1. `space` to stage.
2. `x` to review in the basket.
3. `enter` to confirm. `D` switches to permanent delete.

Items go to the trash by default:

| | |
|---|---|
| macOS | the Finder trash ("Put Back" works) |
| Linux | `~/.local/share/Trash`, or `$topdir/.Trash-$uid` on other filesystems |

- `u` restores the last batch; `U` restores any of the last 20.
- Undo never overwrites a file that is now at the original path.
- Trashed items still use space until the trash is emptied. `E` empties only
  what `fad` trashed, and skips anything that has been replaced since.

Always refused:

- `/`, your home directory, system directories
- `/Volumes`, `/Volumes/<disk>`, `/mnt`, `/media`, `/opt`, `/tmp`, `/var`
- mount points
- the scan root, its parents, and anything outside it
- directories containing another filesystem, a cloud folder, or something
  unreadable

## Ignoring things

`i` adds the selection to your ignore file. You can also edit it:

```text
# a comment
/Users/you/VMs                     a path and everything under it
~/Pictures/Photos.photoslibrary
*.sparsebundle                     a name pattern
Steam/                             a name pattern, directories only
```

Ignored items are hidden and can't be deleted, but still count towards totals.

## Files

| | macOS | Linux |
|---|---|---|
| ignore list | `~/.config/fad/ignore` | `$XDG_CONFIG_HOME/fad/ignore` |
| saved scans | `~/Library/Caches/fad` | `$XDG_CACHE_HOME/fad` |
| undo history | `~/Library/Application Support/fad` | `$XDG_DATA_HOME/fad` |

`FAD_CONFIG_DIR`, `FAD_CACHE_DIR` and `FAD_STATE_DIR` override these directories.

Saved scans unused for 60 days are deleted, and the cache is kept under 1 GiB.
`--clear-cache` deletes only `fad`'s scan files.

## Development

```sh
cargo test
cargo run --release --example statbench -- ~   # stat cost per entry
cargo run --release --example cachebench -- ~  # snapshot timings
cargo run --example toolprobe                  # tool storage report
```

Run the tests on Linux from a Mac:

```sh
docker run --rm -v "$PWD":/src:ro -w /work rust:latest bash -c \
  'cp -r /src/src /src/tests /src/examples /src/Cargo.toml /src/Cargo.lock /work/ && cargo test'
```

Tests never run real `docker`; `FAD_DOCKER_BIN`, `FAD_PODMAN_BIN` and
`FAD_TMUTIL_BIN` point at stubs. Tests that change environment variables must
hold `common::env_lock()` and call `common::isolate()`.

## License

MIT — see [LICENSE](LICENSE).
