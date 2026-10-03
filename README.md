# fad

**f**ind **a**nd **d**estroy — a terminal tool for finding what is using your
disk and deleting it.

`fad` walks a directory tree, ranks everything by real disk usage, and shows it
in a two-pane TUI: a live tree on the left, details of the selected item on the
right. You mark items for deletion, review the batch, and commit it. Deleted
items go to the system trash by default and can be restored with one keystroke.

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
| `U` | undo history — put any of the last 20 batches back |
| `E` | empty the trash — only what `fad` put there, and only then is it reclaimed |
| `r` | reclaimable view — build artifacts, package caches, app caches, VM images |
| `t` | tool storage — what Docker and friends hold that a walk cannot see |
| `d` | duplicate view — files whose contents are byte-for-byte equal |
| `L` | in the duplicate view — make the copies share one copy of the storage |
| `a` | age filter — cycle: any age → untouched 90 days → 1 year → 2 years |
| `/` | fuzzy filter — narrow what is on screen (`enter` to keep it, `esc` to clear) |
| `f` | find — every entry in the tree by name, biggest first, `enter` to go there |
| `s` | cycle sort: size → count → modified → name · `R` rescan |
| `S` | bring the next detail breakdown to the top, when they do not all fit |
| `o` `e` `y` | reveal in your file manager · open in `$EDITOR` · copy path |
| `i` | never rank this again — adds it to your ignore list |
| `!` | what is not in these numbers — skipped, unreadable, ignored |
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
--tools           open in the tool storage view; with --json, print that report
--yes             with --reclaim or --tools, act instead of opening the UI
--max 10G         with --yes (either kind), skip anything that would go over this
--dry-run         with --yes (either kind), print what would go and remove nothing
--permanent       with --reclaim --yes, delete outright instead of trashing
--since           print what changed since the last saved scan of this root
--print-path      print the selected path on exit, for `cd "$(fad --print-path)"`
--json            dump the ranked tree as JSON instead of opening the UI
--min-size 100M   leave out smaller entries (--json, --since, --reclaim/--tools --yes)
--depth N         how deep to report (--json and --since, default 2)
--init zsh        print shell integration: completions and a `fad-cd` function
--man             print this tool's man page, in roff
--no-mouse        do not capture the mouse, so text selection keeps working
--no-cache        ignore any snapshot and always walk from scratch
--clear-cache     delete every saved snapshot and exit
```

Combinations that would quietly do something other than what they say are
refused: `--dry-run`, `--permanent` and `--max` need `--yes`; `--yes` needs
`--reclaim` or `--tools`; `--permanent` does not go with `--tools`, whose
removals are always permanent; `--since` stands alone; and `--print-path` and
`--json` both want standard output. Without a terminal, the interactive view
says so and points at the flags that need none.

## Where the size is

A size tells you a directory is worth opening. It does not tell you what to open
next, and on 168G with forty children that is the only question left.

The detail pane answers it without you expanding anything. Under the headline it
gives the selection's share of the whole scan — 40G is a lot of anything and
nothing at all out of 2T — and then the three biggest children with what each
holds, closed off by the line that actually decides your afternoon:

```
── where it goes
  Containers  38G 23% ███
  Caches     6.2G  4% ▊
  Developer  2.1G  1% ▎
  top 3 of 41 hold 82%
```

`top 3 of 41 hold 82%` and `top 3 of 41 hold 19%` are two different problems.
The first is three deletions; the second is not worth starting.

Below that come four breakdowns of the whole subtree — not one level down,
which for a directory of directories says nothing:

| | |
|---|---|
| by extension | what kind of thing it is, with how many of them |
| by age | the four buckets, and how much nobody has touched in two years |
| biggest files | the largest individual files anywhere below here |
| file sizes | how the bytes split across `<1M`, `1-10M`, `10-100M`, `>100M` |

The last two are the pair that tell 168G in one disk image apart from 168G in
seven hundred thousand small files. Same headline, same age, completely
different work — one is a keystroke and the other is a lost afternoon.

As many of the four as the terminal has rows for are shown at once, so on a tall
window there is nothing to press and nothing hidden. When they do not all fit,
`S` moves the order round and brings the next one to the top.

None of this is ever quietly incomplete. The subtree walk behind it is capped,
so on a very large selection the answers are a sample and every heading says
`(sampled)`. A list the pane was too short to finish says how much of itself you
are looking at — `by extension · top 7 of 10 · S` — rather than passing a top
seven off as the whole set. And the `· S` only appears when pressing it would
show you something you cannot already see.

## Finding one thing

`/` narrows what is on screen. `f` answers the other question: where in all of
this is the thing called that. It matches every entry in the tree, at any depth,
open or not, and ranks the hits by size — because "where is the big one" is what
is being asked, and a tidier match that costs nothing is not the answer.

```
┌ find ────────────────────────────────────────────────────┐
│ › simulator█                                             │
│                                                          │
│  Library/Developer/CoreSimulator/Devices              14G │
│  Library/Developer/CoreSimulator/Caches              2.1G │
│  dev/app/ios/build/Simulator.runtime                 8.0M │
│                                                          │
│ enter  go there    ↑ ↓ choose    esc back                │
└──────────────────────────────────────────────────────────┘
```

Hits are shown by path, not by name: two hundred things called `node_modules`
are told apart by where they are and nothing else. `enter` puts the cursor on
one and opens every directory above it on the way down.

If the hit is behind something you turned on — a filter, an age filter — that is
undone to get there, and the status line says which, because dropping it
silently would be as confusing as refusing to move.

The list is the hundred biggest matches. When there are more it says how many,
so "not found" and "not shown" never look the same.

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

Only files over 1M are considered, and copies that already share their storage
are skipped — hardlinks, and anything cloned — because deleting one of those
frees nothing. That matters more on a Mac than it sounds: APFS clones on `cp`,
and so does the standard library's own file copy, so a great many byte-identical
pairs have never cost anything twice. Listing them would be inviting you to
delete a file for no gain.

`A` on a group stages every copy but the newest.

### Or keep every copy

`L` is the other answer, and usually the better one. Instead of deleting all but
the newest copy, it makes the others *share* the newest one's storage: the same
bytes come back, and every path goes on working. On APFS and on btrfs or XFS
these are copy-on-write clones, so writing to either path afterwards splits them
apart again and neither can surprise the other.

```
2 copies now share storage with the newest — 128M back on the volume,
though du still counts both
```

That last clause is not a hedge. `du` reports a clone at its full size, `fad`'s
sizes are `du`'s, and so the tree above will go on showing both copies at what
they used to cost. The number that moves is the free-space figure in the header,
which is the one this was ever about. The group leaves the duplicate list,
because there is no longer anything to reclaim by deleting either half of it.

Nothing is staged, nothing goes to the trash, and this does not pass through the
basket — nothing is being removed. Each replacement is built beside its target
and renamed into place, so an interruption leaves either the old file or the new
one and never half of either, and the destination keeps its own permissions and
its own modification time: only where its bytes live is different.

Where the filesystem has no clone operation — ext4, HFS+, a network mount — `L`
says so and does nothing. There is deliberately no fallback to a hard link: a
hard link would make writing to one path change the other, which is a different
thing from what you asked for and a considerably worse one.

## Tool storage

Press `t` and `fad` asks Docker, Podman and Time Machine what they are holding.

A walk cannot answer this. On macOS everything Docker owns lives inside one VM
disk image, so a scan can only report it as a single opaque forty-gigabyte file;
on Linux it is under `/var/lib/docker`, outside a home-directory scan and
unreadable without root. Time Machine's local snapshots are worse — they are
below the filesystem rather than in it, and `du` cannot see them at all. On the
machine this was written on, `t` found 17G of cold Docker build cache that no
amount of scanning would ever have surfaced.

```
┌ /Users/you  285G  294014 dirs, 2411907 files   tool storage ─┬ selection ──────────────┐
│ ▾ docker · build cache · 1 · 19G  17G reclaimable ────────── │ images                  │
│     cold build cache                       17G ████████████  │ docker                  │
│ ▾ docker · images · 2 · 3.7G  1.4G reclaimable ───────────── │ 3.7G held in total      │
│     nginx:latest                 724M ▍       +2.3G shared   │ 1.4G of that is spare   │
│     <dangling>                   724M ▍       +2.3G shared   │ docker's own figure,    │
│                                                              │ not a sum of the rows   │
│ ⚠ docker: this will not free space on your disk              │                         │
│   inside Docker.raw, 68G here, never shrinks                 │                         │
└──────────────────────────────────────────────────────────────┴─────────────────────────┘
```

Nothing is asked of any daemon until `t` is pressed. `docker system df` is not a
lookup — the daemon walks its own store to answer it, and sixteen seconds is a
normal reply on a busy machine — so the probe runs in the background, the header
counts the seconds while it waits, and every call is killed at a deadline rather
than allowed to hang the UI behind a wedged daemon.

A tool nobody has installed is not mentioned. One that is installed but not
answering gets a row saying so, where its numbers would have been, because
"docker is holding 20G" is the wrong thing to imply when we do not know.

### The sizes here are on a different axis

None of this is under the scan root, and none of it is added to the headline
total. Two things follow that `fad` will not fudge.

**Image sizes overlap.** Two images built on the same base each report the whole
base. On this machine that is two images reporting 3.24GB apiece — 6.48GB
summed — for 3.995GB of actual storage. So a heading shows the *tool's own*
deduplicated figure and never a sum of the rows beneath it, and each row shows
what it owns outright with the shared part reported separately and never counted:

```
nginx:latest      724M ▍   +2.3G shared
```

Removing that image gives back 724M. Removing every image that shares the base
gives back all of it, and staging them all says so exactly. Anything in between
is reported as a floor — `at least 1.4G` — because which layers a subset
releases is not something `docker system df` will tell us. Whatever the estimate,
the figure reported *afterwards* is measured by asking the tool again.

**Removing something may free nothing.** Inside a VM disk image that does not
shrink — Docker Desktop, Colima — deleting an image frees space inside that file
and nothing on your disk until it is compacted. `fad` asks the daemon which
backend it is, says so in a banner, and leaves those bytes out of the "free
after this batch" line. OrbStack trims its own disk, so there the space really
does come back and there is no warning to make. Native Linux has neither
problem.

When that disk image is itself under the scan root, the banner says so too: the
tree above already counts it, and these are not two separate piles.

### Removing things

`space` stages a resource, and it joins the same basket as everything else — but
under its own heading, and it never mixes in among the files. There is no trash
for `docker image rm`. The confirmation says `no trash, no undo, whatever D
says`, and `D` does not reach that half of the batch.

`fad` will not touch a running container, an image a container is using, or a
volume something is attached to. Those are shown, greyed, with the reason. The
check runs again at the confirmation, so a container that started while you were
staging cannot have its image pulled out from under it. Build cache is one row
rather than six hundred, because `builder prune` is the only handle any Docker
CLI offers and a list of individually unremovable records is a menu of things
that do not work.

`y` copies the exact command instead, for anyone who would rather run it
themselves.

### From a script

```sh
fad --tools --json                    # the whole report
fad --tools --yes --dry-run           # what a cleanup would take
fad --tools --yes --max 10G           # take up to 10G of it
```

`--tools --yes` only ever offers what the tool itself reports as unused, prints
every command before running it, and is the one scripted path in `fad` that
cannot be undone. `--max` skips rather than stops, the same rule `--reclaim`
follows.

The JSON lists every source, including ones that said nothing usable — a script
has to be able to tell "no Docker here" from "Docker with nothing to clean".
Each source carries its own totals and its backing caveat, and there is
deliberately no grand total: adding a host-backed source to one living inside a
VM disk produces exactly the number this view exists to refuse to print.

## What grew

`fad` already keeps a snapshot of the last scan of each root. The detail pane
diffs the selection against it and reports the change: `+12G since 3 days ago`,
or `new since last week`.

Growth is usually more actionable than size. A cache that put on 12G this week
is a better target than a stable 20G one.

## Without the UI

```sh
fad --reclaim --yes --dry-run ~/dev     # what a cleanup would take
fad --reclaim --yes --max 10G ~/dev     # take up to 10G of it, to the trash
fad --since ~/dev                       # what changed since the last scan
fad --tools --yes --dry-run             # what docker would give back
cd "$(fad --print-path)"                # quit on a directory, land in it
eval "$(fad --init zsh)"                # completions, and a fad-cd that does that
```

`--reclaim --yes` is deliberately narrow. It only ever considers entries the
built-in rules recognise, it honours your ignore list and the same guard the UI
uses, and it prints every path before touching it. It trashes by default;
`--permanent` is the flag that makes it irreversible.

`--max` takes candidates largest first and *skips* anything that would push the
batch over the cap rather than stopping there — otherwise a 2G cap could reclaim
nothing at all when the biggest candidate happens to be 3G.

`--since` compares against the saved snapshot and prints the biggest changes
first — growth, entries that are new, and entries that are gone, each reported
once at the top of what appeared or went — then leaves the fresh walk behind as
the new baseline, so it can be run on a timer. It answers "what did that install
just add?", which no single scan can. A snapshot only counts as a baseline for a
scan of the same root with the same `--cross-device` and `--cloud`; the first
run of a new combination saves one and exits 1.

### In your shell

```sh
eval "$(fad --init zsh)"                     # in ~/.zshrc — bash and fish too
fad --man > /usr/local/share/man/man1/fad.1  # and man fad works
```

That gives you completions for every flag, and `fad-cd`: run `fad`, quit on a
directory, and land in it. Nothing can change your shell's directory from a
child process, so this is the one thing a shell function is needed for. Quitting
on a file lands you in the directory holding it — `cd` into a 40G disk image is
not what anyone meant.

Both are generated from the argument parser itself, so they describe the flags
this binary has rather than a copy that drifts away from it.

`--print-path` draws the interface on your terminal device rather than on
standard output, which is what makes `cd "$(fad --print-path)"` work at all: a
command substitution swallows standard output, so a UI drawn there would be a
blank screen for the length of the session and a shell buffer full of escape
codes at the end of it.

## Free space

The header carries what is left on the volume: `71G free of 1.0T`. A scan total
on its own says how big something is, not whether it matters — 285G is most of a
512G disk and a quarter of a 2T one, and those are different afternoons. It is
also the number every decision in this tool is ultimately about, so it is on
screen the whole time rather than only in the basket.

When the pane is too narrow for all of it, the entry count goes and the free
figure stays. A count is context; this is the answer.

`--cross-device` makes the tree span filesystems, and one free-space figure
cannot describe several disks. `fad` says so in a banner rather than let the
header read as a total.

## Sizes

Sizes are allocated blocks (`st_blocks × 512`), matching `du`. Sparse and
APFS-compressed files are counted at what they actually cost on disk; use
`--apparent` for file length instead. Where the two differ, the detail pane
shows both.

A file with several hard links is counted once, against the lexicographically
first of its paths.

Every unit is a power of 1024, as in `du -h`: `1G` is 2³⁰ bytes, on screen and
on the command line alike. `--min-size` and `--max` take `K`, `M`, `G`, `T` and
`P` in any case, with or without `B` or `iB` (`100M`, `100mb` and `100MiB` are
the same size), and refuse negatives, `nan`, `inf` and unknown units rather
than guessing.

## What is not scanned

- **Other filesystems.** Mount points are shown but not entered. `--cross-device`
  opts in.
- **Anything a daemon is holding.** Docker's image store, Podman's, Time
  Machine's local snapshots. A walk cannot attribute a VM disk image and cannot
  see an APFS snapshot at all, so `t` asks the tools instead.
- **Cloud folders (macOS).** iCloud Drive, Dropbox, OneDrive and similar
  FileProvider-backed folders are skipped, because reading inside one can block
  on the network for minutes. `--cloud` opts in. Linux has no equivalent skip.
- **Directories that cannot be read.** Counted and reported, with the reason;
  when the reason is a permission refusal on macOS, granting your terminal Full
  Disk Access usually fixes it. A directory that lists its names but will not
  let them be examined (read without search permission) is reported the same
  way rather than shown as an empty 0 B, and trees deeper than `PATH_MAX` are
  walked to the bottom.

Anything skipped is reported in a banner, so an incomplete number is never shown
as a complete one. A banner says how many, which is enough to know a total is
short and not enough to do anything about it — `!` says which, path by path,
with the flag or the permission that would fix each kind on its heading.

```
┌ what is not in these numbers ─────────────────────────────────────┐
│ 12 entries were not counted — every total above them is short by  │
│    whatever they hold                                             │
│ 2 hidden by your ignore list · 41G — counted, just not shown      │
│                                                                   │
│ could not be read  not counted — grant your terminal Full Disk    │
│   Library/Application Support/MobileSync                        — │
│ cloud folders  not counted — rerun with --cloud                   │
│   Library/Mobile Documents                                      — │
│ on your ignore list  counted in every total above it, not shown   │
│   VMs                                                         38G │
└───────────────────────────────────────────────────────────────────┘
```

The two halves are never added together. Something unreadable is missing from
every total above it and makes the headline wrong; something ignored is in the
headline and only missing from the view. A single figure spanning both would be
true of neither.

Nothing uncounted is given a size, because there isn't one to give: not knowing
what an unread directory holds is the whole content of the row, and a `0` there
would be the exact failure this screen exists to expose.

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

The journal keeps the last twenty committed batches. `u` puts the most recent
one back; `U` opens the history, where any of them can be restored, and says
which ones have since been emptied out of the trash and cannot be.

Trashing reclaims nothing until the trash goes out, so a banner reports what
`fad` has put there and not yet seen emptied — and `E` takes it out.

`E` empties **only what `fad` trashed**. It knows exactly where each item
landed because it wrote it down, and everything else in your trash was put there
by someone else for reasons `fad` does not know. Every path is checked to be
inside a real trash directory before anything is removed, because the alternative
— a hand-edited or corrupted journal pointing at a live file — is the one
mistake this tool cannot take back.

The screen before it says all three things that change:

```
┌ empty the trash ───────────────────────────────────┐
│ 12 item(s) fad trashed, 40G                        │
│ removed from the trash for good — cannot be undone │
│ 3 batch(es) in the undo history can no longer be   │
│   put back                                         │
│ free space  71G → 111G                             │
│                                                    │
│     28G  /Users/you/dev/fad/target                 │
│    6.2G  /Users/you/Library/Caches/big.cache       │
│                                                    │
│ anything else in your trash is left where it is    │
└────────────────────────────────────────────────────┘
```

The journal is not rewritten. Those batches stay on the `U` screen and start
reporting themselves as emptied and unrestorable, which is precisely what has
happened to them — quietly dropping the record would lose the only evidence that
the deletion ever took place.

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

`FAD_CONFIG_DIR`, `FAD_CACHE_DIR` and `FAD_STATE_DIR` override these, and name
the directory itself — no `fad/` is appended. Snapshots are only written for
scans that ran to completion, one per root and set of scan options.

The snapshot cache looks after its own size. Each save drops snapshots that
have not been saved or read in 60 days, then, if what is left is over 1 GiB,
the least recently used until it fits; the one just written is always kept.
`--clear-cache` removes every snapshot — only the `*.snap` files fad wrote,
never anything else in the directory — and the directory too if that leaves
it empty.

## Development

```sh
cargo test                                    # du-parity, trash round-trips, tree arithmetic, rendering
cargo run --release --example statbench -- ~  # per-entry stat cost
cargo run --release --example cachebench -- ~ # snapshot build/encode/load timings
cargo run --example toolprobe                 # what the tools view will show
```

To run the suite on Linux from a Mac:

```sh
docker run --rm -v "$PWD":/src:ro -w /work rust:latest bash -c \
  'cp -r /src/src /src/tests /src/examples /src/Cargo.toml /src/Cargo.lock /work/ && cargo test'
```

The tool tests never run `docker`: the parsers are pure functions over captured
output in `tests/fixtures`, and `FAD_DOCKER_BIN` / `FAD_PODMAN_BIN` /
`FAD_TMUTIL_BIN` point the probe at a stub when one is wanted. The suite passes
on a machine with no container runtime installed.

Tests that redirect `HOME`, `XDG_DATA_HOME`, `FAD_STATE_DIR` or `FAD_CACHE_DIR`
must hold `common::env_lock()` for their whole body — environment variables are
process-global and `cargo test` runs a binary's tests on several threads at
once. They use `common::isolate()` so a test run can never read or consume the
real user's trash, cache, or undo history.
