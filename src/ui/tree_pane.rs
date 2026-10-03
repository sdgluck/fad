//! The tree: one row per visible node, with a size bar scaled against siblings.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::{Theme, cols, pad, truncate_end, truncate_start as truncate};
use crate::app::{App, Heading};
use crate::format::human;
use crate::tree::flags;

/// Eighth-block glyphs give the bar eight times the resolution of whole cells,
/// which matters when a row is one twentieth of its parent.
const EIGHTHS: [&str; 9] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];

const BAR_WIDTH: usize = 12;

/// Everything on a tree row but the indent and the name: the stage marker, the
/// twisty and its space, a space, the size, a space, the bar, and one column
/// of slack on the right.
const ROW_FIXED: usize = 1 + 2 + 1 + 8 + 1 + BAR_WIDTH + 1;

/// The narrowest a name is squeezed to before the indent gives way instead.
const MIN_NAME: usize = 8;

/// Columns of indent for a row at `depth` in a pane `width` wide.
///
/// Two a level, until the name would drop below `MIN_NAME`, and then no more:
/// a deep row keeps its name readable and stays inside the pane, at the cost of
/// lining up with its parent. Before, the name had a floor and the indent did
/// not, so a deep enough row ran off the right edge and took its size and bar
/// with it. Public because a click on the twisty is hit-tested against it.
pub(crate) fn indent(depth: u16, width: usize) -> usize {
    let room = width.saturating_sub(ROW_FIXED + MIN_NAME);
    (2 * depth as usize).min(room / 2 * 2)
}

pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let title = header(app, area.width.saturating_sub(2) as usize);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Banners sit at the bottom of the pane, where they cannot push the tree
    // around as they appear and disappear during a scan.
    let banners = banner_lines(app, theme, inner.width as usize);
    let list_h = inner.height.saturating_sub(banners.len() as u16);
    let height = list_h as usize;
    scroll_into_view(app, height);

    let mut lines = Vec::with_capacity(height);
    if app.rows.is_empty() {
        // A blank pane reads as a bug. Say which of the three reasons it is.
        lines.push(Line::from(Span::styled(empty_reason(app), theme.dim)));
    }
    for i in app.offset..(app.offset + height).min(app.rows.len()) {
        lines.push(row_line(app, theme, i, inner.width as usize));
    }
    // The tree always shows its root, so a filter that matches nothing leaves
    // one row rather than none — and a lone root with no children under it
    // read as an empty directory, never as "your filter hid everything".
    if app.view().is_none()
        && app.rows.len() == 1
        && (!app.filter.is_empty() || app.age_filter != crate::app::AgeFilter::All)
    {
        lines.push(Line::from(Span::styled(empty_reason(app), theme.dim)));
    }
    let list = Rect { height: list_h, ..inner };
    // Remembered for hit-testing: a click is a terminal coordinate and means
    // nothing without the rectangle the rows were last drawn into.
    app.tree_list = list;
    f.render_widget(Paragraph::new(lines), list);

    if !banners.is_empty() {
        let at = Rect {
            y: inner.y + list_h,
            height: banners.len() as u16,
            ..inner
        };
        f.render_widget(Paragraph::new(banners), at);
    }
}

/// The one line that is always on screen, so what it drops when it runs out of
/// room matters.
///
/// Everything after the path is built first and the path is given what is
/// left, because the path is the part the user already knows — they typed it —
/// and the sizes are the part they came for. A block title is truncated from
/// the right, so laying it out the other way round loses the free-space figure
/// first, which is exactly backwards.
fn header(app: &App, width: usize) -> Line<'static> {
    let root = app.tree.node(app.tree.root());
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::from("  "));
    spans.push(Span::from(human(app.tree.size(app.tree.root(), app.apparent))).bold());
    if app.apparent {
        spans.push(Span::from(" apparent"));
    }
    // The first thing to go when the pane is narrow: an entry count is context,
    // and everything else on this line is the answer.
    let counts = spans.len();
    if app.scanning() {
        spans.push(Span::from(format!(
            "  scanning {} dirs, {} files",
            root.dir_count, root.file_count
        )));
    } else {
        spans.push(Span::from(format!(
            "  {} dirs, {} files",
            root.dir_count, root.file_count
        )));
    }
    // The denominator. A scan total on its own says how big something is; this
    // is what says whether it matters, and it is the number the user is
    // actually trying to move.
    if let Some(v) = app.volume.as_ref() {
        spans.push(Span::from(format!("  {} free of {} ", human(v.free), human(v.total))).bold());
    }
    if app.reclaim_view {
        spans.push(Span::from(" reclaimable ").bold().reversed());
        spans.push(Span::from(" "));
    }
    if app.tools_view {
        // Deliberately not a size. The one number that would fit here would
        // have to span a host disk and a VM's insides, and adding those
        // together is the thing this view exists to refuse to do.
        let label = if app.tool_probe_running() {
            match app.tools_started.map(|t| t.elapsed().as_secs()) {
                // `docker system df` walks its own store; sixteen seconds with
                // nothing wrong is normal. Show the clock so it reads as work.
                Some(secs) if secs >= 2 => format!(" tools \u{b7} asking\u{2026} {secs}s "),
                _ => " tools \u{b7} asking\u{2026} ".to_string(),
            }
        } else {
            " tool storage ".to_string()
        };
        spans.push(Span::from(label).bold().reversed());
        spans.push(Span::from(" "));
    }
    if app.dupe_view {
        let label = match app.dupes.as_ref() {
            Some(r) => format!(" duplicates \u{b7} {} reclaimable ", human(r.wasted())),
            None => " duplicates \u{b7} hashing\u{2026} ".to_string(),
        };
        spans.push(Span::from(label).bold().reversed());
        spans.push(Span::from(" "));
    }
    // Rows vanishing with no explanation reads as a bug, so an active age
    // filter has to be as visible as the reclaimable view is.
    if app.age_filter != crate::app::AgeFilter::All {
        spans.push(Span::from(format!(" {} ", app.age_filter.label())).bold().reversed());
        spans.push(Span::from(" "));
    }
    // Same reason, and a kept filter is easier still to forget: it was typed
    // once, `enter` put the prompt away, and nothing else on screen says the
    // tree is a subset. Capped, because the query is a reminder here and the
    // free-space figure is the answer.
    if !app.filter.is_empty() {
        spans.push(Span::from(format!(" /{} ", truncate_end(&app.filter, 20))).bold().reversed());
        spans.push(Span::from(" "));
    }

    /// A path compressed below this is no longer a path, and a title that
    /// overflows loses the free-space figure off the right-hand end — so when
    /// it comes to it, the entry count goes instead.
    const MIN_PATH: usize = 16;

    let width_of =
        |v: &[Span<'static>]| v.iter().map(|s| cols(&s.content)).sum::<usize>();
    if width.saturating_sub(width_of(&spans) + 1) < MIN_PATH {
        spans.remove(counts);
    }

    let path = app.tree.root_path().display().to_string();
    let room = width.saturating_sub(width_of(&spans) + 1);
    let head = vec![Span::from(" "), Span::from(super::compress(&path, room)).bold()];
    Line::from([head, spans].concat())
}

fn empty_reason(app: &App) -> String {
    if !app.filter.is_empty() {
        format!(" nothing matches \"{}\" \u{2014} esc to clear", app.filter)
    } else if app.age_filter != crate::app::AgeFilter::All {
        format!(
            " nothing here is {} \u{2014} a cycles the age filter",
            app.age_filter.label()
        )
    } else if app.dupe_view {
        if app.dupe_hunt_running() {
            " hashing candidates\u{2026}".to_string()
        } else if app.dupes.is_none() {
            " duplicates need a finished scan \u{2014} R to rescan".to_string()
        } else {
            " no duplicates over 1M \u{2014} d for the full tree".to_string()
        }
    } else if app.tools_view {
        if app.tool_probe_running() {
            " asking each tool what it is holding\u{2026}".to_string()
        } else if app.tools.is_none() {
            " nothing asked yet \u{2014} t again to ask".to_string()
        } else {
            " no container runtime or snapshot store found on this machine".to_string()
        }
    } else if app.reclaim_view {
        " nothing reclaimable here \u{2014} r for the full tree".to_string()
    } else if app.scanning() {
        " scanning\u{2026}".to_string()
    } else {
        " empty".to_string()
    }
}

/// What the tree is not telling you: stale numbers, unread directories, and
/// folders we refused to enter. Saying nothing here would make the totals look
/// more authoritative than they are.
fn banner_lines(app: &App, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();

    // A long walk with no sign of movement reads as a hang. Say where it is,
    // how fast it is going, and — once a previous snapshot gives us a
    // denominator — how much longer it has.
    if let Some((p, fraction)) = app.scan_progress() {
        let here = p
            .current
            .strip_prefix(app.tree.root_path())
            .unwrap_or(&p.current)
            .display()
            .to_string();
        let mut head = format!(" \u{2937} {}", super::compress(&here, width.saturating_sub(28)));
        if p.rate > 0.0 {
            head.push_str(&format!("  {}/s", si(p.rate)));
        }
        out.push(Line::from(Span::styled(truncate_end(&head, width), theme.dim)));

        if let Some(f) = fraction {
            let left = remaining(&p, f);
            let cells = width.saturating_sub(cols(&left) + 4);
            let filled = (f * cells as f64) as usize;
            out.push(Line::from(vec![
                Span::raw(" "),
                Span::styled("\u{2588}".repeat(filled), theme.bar),
                Span::styled("\u{2591}".repeat(cells.saturating_sub(filled)), theme.dim),
                Span::styled(format!("  {left}"), theme.dim),
            ]));
        }
    }

    if app.from_cache {
        out.push(Line::from(Span::styled(
            " \u{25cc} last known sizes \u{2014} rescanning in the background".to_string(),
            theme.dim,
        )));
    }

    // An ignored entry still counts towards every total above it — a size that
    // quietly omits things is the one failure this tool cannot afford — so what
    // is hidden has to be said out loud.
    let (n, bytes) = app.ignored;
    if n > 0 {
        out.push(Line::from(Span::styled(
            truncate_end(
                &format!(
                    " \u{25cc} {n} ignored entr{} hidden \u{b7} {} \u{2014} still counted in the totals",
                    if n == 1 { "y" } else { "ies" },
                    human(bytes)
                ),
                width,
            ),
            theme.dim,
        )));
    }

    // Trashing reclaims nothing until the trash goes out. A session that has
    // just "freed" 40G and changed nothing on the volume has to say so.
    let (n, bytes) = app.trash_pending;
    if n > 0 {
        out.push(Line::from(Span::styled(
            truncate_end(
                &format!(
                    " \u{26a0} {} in the trash from {n} item(s) \u{2014} E empties it, and only then is it reclaimed",
                    human(bytes)
                ),
                width,
            ),
            theme.warn,
        )));
    }

    let unreadable = app.tree.unreadable_count;
    if unreadable > 0 {
        out.push(Line::from(Span::styled(
            truncate_end(
                &format!(
                    " \u{26a0} {unreadable} unreadable director{} not counted \u{2014} grant Full Disk Access to your terminal",
                    if unreadable == 1 { "y" } else { "ies" }
                ),
                width,
            ),
            theme.warn,
        )));
    }

    if let Some(n) = app.dupes.as_ref().map(|r| r.unverified).filter(|n| *n > 0) {
        out.push(Line::from(Span::styled(
            truncate_end(
                &format!(
                    " \u{26a0} {n} group(s) looked identical but were not read in full \u{2014} not shown"
                ),
                width,
            ),
            theme.warn,
        )));
    }

    // The one thing a size in this view cannot say for itself: whether removing
    // it gives the user's disk anything back, and whether the tree has counted
    // it already.
    if app.tools_view {
        if let Some(report) = app.tools.as_ref() {
            for sr in &report.sources {
                for (i, note) in sr.backing.notes().iter().enumerate() {
                    // The tool's name once, on the first line only; the rest
                    // are continuations of the same warning.
                    // A tight prefix on purpose: every character here is one
                    // the warning itself does not get.
                    let lead =
                        if i == 0 { format!(" \u{26a0} {}: ", sr.source.label()) } else { "   ".into() };
                    out.push(Line::from(Span::styled(
                        truncate_end(&format!("{lead}{note}"), width),
                        theme.warn,
                    )));
                }
                if sr.backing.notes().is_empty() {
                    continue;
                }
                if app.tool_in_tree(sr.backing.disk().map(|p| p.as_path())) {
                    out.push(Line::from(Span::styled(
                        truncate_end(
                            " \u{26a0} that file is under the scan root, so the tree above \
                              already counts it \u{2014} these are not two separate piles",
                            width,
                        ),
                        theme.warn,
                    )));
                }
            }
        }
    }

    // With --cross-device the tree spans filesystems and the header's free
    // figure is only the root volume's. One number over several disks is
    // exactly the kind of total the rest of this tool refuses to print.
    if app.spans_volumes() && app.volume.is_some() {
        out.push(Line::from(Span::styled(
            truncate_end(
                " \u{25cc} this scan crosses filesystems \u{2014} the free figure is the root volume's alone",
                width,
            ),
            theme.dim,
        )));
    }

    let cloud = app
        .tree
        .skipped
        .iter()
        .filter(|(_, r)| *r == crate::scan::walk::Skip::CloudStorage)
        .count();
    if cloud > 0 {
        out.push(Line::from(Span::styled(
            truncate_end(
                &format!(" \u{26a0} {cloud} cloud folder(s) not counted \u{2014} rerun with --cloud to include them"),
                width,
            ),
            theme.warn,
        )));
    }
    out
}

/// The time left, from the rate so far and how much of last run's entry count
/// is still to come. Stated as an estimate, because that is what it is.
fn remaining(p: &crate::scan::ScanProgress, fraction: f64) -> String {
    if p.rate <= 0.0 || fraction >= 1.0 {
        return "almost there".to_string();
    }
    let total = p.entries as f64 / fraction;
    let secs = ((total - p.entries as f64) / p.rate).max(0.0);
    if secs < 1.0 {
        "almost there".to_string()
    } else if secs < 90.0 {
        format!("~{}s left", secs as u64)
    } else {
        format!("~{}m left", (secs / 60.0).round() as u64)
    }
}

/// Thousands and millions, for a rate. Base ten, unlike a size: nobody thinks
/// of files per second in units of 1024.
fn si(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.0}k", v / 1_000.0)
    } else {
        format!("{v:.0}")
    }
}


/// Keep the cursor on screen with a little breathing room above and below.
fn scroll_into_view(app: &mut App, height: usize) {
    const MARGIN: usize = 3;
    if height == 0 {
        return;
    }
    let margin = MARGIN.min(height / 3);
    if app.cursor < app.offset + margin {
        app.offset = app.cursor.saturating_sub(margin);
    }
    let bottom = app.offset + height.saturating_sub(margin + 1);
    if app.cursor > bottom {
        app.offset = app.cursor + margin + 1 - height;
    }
    let max_offset = app.rows.len().saturating_sub(height);
    app.offset = app.offset.min(max_offset);
}

fn row_line(app: &App, theme: &Theme, i: usize, width: usize) -> Line<'static> {
    let row = app.rows[i];
    if let Some(h) = row.header {
        return match h {
            Heading::Category(cat) => category_line(app, theme, cat, i == app.cursor, width),
            Heading::Dupes(g) => dupe_line(app, theme, g, i == app.cursor, width),
            Heading::Tool(src, kind) => tool_line(app, theme, src, kind, i == app.cursor, width),
            Heading::ToolStatus(src) => tool_status_line(app, theme, src, i == app.cursor, width),
        };
    }
    if row.tool.is_some() {
        return tool_row_line(app, theme, &row, i == app.cursor, width);
    }
    let n = app.tree.node(row.id);
    let selected = i == app.cursor;
    let staged = app.staged.contains(&row.id);

    let marker = if staged { "●" } else { " " };
    let twisty = if n.flags & flags::IS_DIR == 0 {
        " "
    } else if app.expanded.contains(&row.id) {
        "▾"
    } else {
        "▸"
    };

    // In the duplicate view a bare filename is useless: copies of the same
    // file usually share one. The path is the only thing telling them apart.
    let dupe_label;
    let label: &str = if app.dupe_view && row.depth > 0 {
        dupe_label = app
            .tree
            .path(row.id)
            .strip_prefix(app.tree.root_path())
            .unwrap_or(&app.tree.path(row.id))
            .display()
            .to_string();
        &dupe_label
    } else if row.id == app.tree.root() {
        app.tree
            .root_path()
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("/")
    } else {
        &n.name
    };

    let indent = " ".repeat(indent(row.depth, width));
    let bytes = app.tree.size(row.id, app.apparent);
    let size = human(bytes);
    let bar = bar(bytes, row.sibling_max);

    // Name column gets whatever the fixed columns leave behind.
    let name_w = width.saturating_sub(ROW_FIXED + indent.len());
    let name = truncate(label, name_w);

    let name_style = if staged {
        theme.staged
    } else if n.flags & (flags::CLOUD | flags::OTHER_DEVICE) != 0 {
        theme.skipped
    } else if n.flags & flags::IS_DIR != 0 {
        theme.dir
    } else {
        theme.normal
    };

    let mut spans = vec![
        Span::styled(marker.to_string(), theme.staged),
        Span::raw(indent),
        Span::styled(format!("{twisty} "), theme.dim),
        Span::styled(pad(&name, name_w), name_style),
        Span::raw(" "),
        Span::styled(format!("{size:>8}"), theme.emphasis),
        Span::raw(" "),
        Span::styled(bar, if is_hot(bytes, row.sibling_max) { theme.bar_hot } else { theme.bar }),
    ];

    if n.flags & flags::CLOUD != 0 {
        spans.push(Span::styled(" cloud, not counted".to_string(), theme.warn));
    } else if n.flags & flags::OTHER_DEVICE != 0 {
        spans.push(Span::styled(" other volume".to_string(), theme.warn));
    } else if n.flags & flags::UNREADABLE != 0 {
        spans.push(Span::styled(" unreadable".to_string(), theme.warn));
    }

    let line = Line::from(spans);
    if selected { line.style(theme.selection) } else { line }
}

/// A category heading in the reclaimable view, carrying the category total and
/// the one caveat the user needs before staging all of it.
fn category_line(app: &App, theme: &Theme, cat: crate::presets::Category, selected: bool, width: usize) -> Line<'static> {
    let items = app.reclaim_items(cat);
    let total: u64 = items.iter().map(|id| app.tree.size(*id, app.apparent)).sum();
    let arrow = if app.reclaim_is_open(cat) { '\u{25be}' } else { '\u{25b8}' };
    let head =
        format!(" {arrow} {} \u{b7} {} \u{b7} {} ", cat.label(), items.len(), human(total));
    let note = format!(" {} ", cat.note());
    let rule = width.saturating_sub(cols(&head) + cols(&note) + 1);
    let line = Line::from(vec![
        Span::styled(head, theme.emphasis),
        Span::styled(note, theme.dim),
        Span::styled("\u{2500}".repeat(rule), theme.dim),
    ]);
    if selected { line.style(theme.selection) } else { line }
}

/// A tool-and-kind heading: `\u{25b8} images \u{b7} 2 \u{b7} 3.7G   1.4G reclaimable`.
///
/// The total is the tool's own deduplicated figure, never a sum of the rows
/// underneath. Docker's image layers are shared, so adding up what each image
/// reports overstates the disk — on the machine this was written on, by 62%.
fn tool_line(
    app: &App,
    theme: &Theme,
    source: crate::tools::Source,
    kind: crate::tools::Kind,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let report = app.tools.as_ref();
    let sr = report.and_then(|r| r.source(source));
    let count = sr.map(|s| s.items_of(kind).len()).unwrap_or(0);
    let arrow = if app.tools_is_open(source, kind) { '\u{25be}' } else { '\u{25b8}' };

    let total = sr.and_then(|s| s.total(kind));
    let head = match total {
        Some((size, _)) => format!(
            " {arrow} {} \u{b7} {} \u{b7} {} \u{b7} {} ",
            source.label(),
            kind.label(),
            count,
            human(size)
        ),
        None => format!(" {arrow} {} \u{b7} {} \u{b7} {} ", source.label(), kind.label(), count),
    };

    // What the tool itself says is going spare. The heading's job is to make
    // that the first thing read.
    let note = match total {
        Some((_, recl)) if recl > 0 => format!(" {} reclaimable ", human(recl)),
        _ => format!(" {} ", kind.note()),
    };
    let rule = width.saturating_sub(cols(&head) + cols(&note) + 1);
    let line = Line::from(vec![
        Span::styled(head, theme.emphasis),
        Span::styled(note, theme.dim),
        Span::styled("\u{2500}".repeat(rule), theme.dim),
    ]);
    if selected { line.style(theme.selection) } else { line }
}

/// A tool that is there but had nothing usable to say.
///
/// A row rather than a banner, because "docker is not running" belongs exactly
/// where docker's numbers would have been. A tool nobody has installed never
/// reaches here — it is not mentioned at all.
fn tool_status_line(
    app: &App,
    theme: &Theme,
    source: crate::tools::Source,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let msg = app
        .tools
        .as_ref()
        .and_then(|r| r.source(source))
        .and_then(|s| s.status.line(source))
        .unwrap_or_else(|| format!("{} said nothing", source.program()));
    let line = Line::from(Span::styled(truncate_end(&format!(" \u{26a0} {msg}"), width), theme.warn));
    if selected { line.style(theme.selection) } else { line }
}

/// One resource. Same shape as a tree row so the eye does not have to relearn
/// the column layout, with two additions the tree never needs: the storage this
/// shares with its siblings, and the reason it cannot be removed.
fn tool_row_line(
    app: &App,
    theme: &Theme,
    row: &crate::app::Row,
    selected: bool,
    width: usize,
) -> Line<'static> {
    let Some(r) = app.tool_of(row) else { return Line::from("") };
    let staged = app.staged_tools.contains(&r.key());

    let marker = if staged { "\u{25cf}" } else { " " };
    // A size we do not have is left blank, not shown as zero. `tmutil` will not
    // size a snapshot and it is not getting an invented figure.
    let size = if r.sized() { human(r.bytes) } else { "\u{2014}".to_string() };
    let bar = if r.sized() { bar(r.bytes, row.sibling_max) } else { " ".repeat(BAR_WIDTH) };

    let suffix = if let Some(why) = &r.blocked {
        format!(" {why}")
    } else if r.shared() > 0 {
        // Carried, never counted: removing this alone frees `bytes`, not
        // `bytes + shared`.
        format!(" +{} shared", human(r.shared()))
    } else {
        String::new()
    };

    let indent = " ".repeat(indent(row.depth, width));
    // The suffix gives way before the name does: which image this is matters
    // more than the note about it.
    let suffix = truncate_end(&suffix, width.saturating_sub(ROW_FIXED + indent.len() + MIN_NAME));
    let name_w = width.saturating_sub(ROW_FIXED + indent.len() + cols(&suffix));
    let name = truncate(&r.name, name_w);

    let name_style = if staged {
        theme.staged
    } else if r.blocked.is_some() {
        theme.skipped
    } else {
        theme.normal
    };

    let line = Line::from(vec![
        Span::styled(marker.to_string(), theme.staged),
        Span::raw(indent),
        Span::styled("  ".to_string(), theme.dim),
        Span::styled(pad(&name, name_w), name_style),
        Span::raw(" "),
        Span::styled(format!("{size:>8}"), theme.emphasis),
        Span::raw(" "),
        Span::styled(bar, theme.bar),
        Span::styled(suffix, if r.blocked.is_some() { theme.warn } else { theme.dim }),
    ]);
    if selected { line.style(theme.selection) } else { line }
}

/// A duplicate group heading. The number that matters is not the file's size
/// but what deleting the extra copies would give back.
fn dupe_line(app: &App, theme: &Theme, group: usize, selected: bool, width: usize) -> Line<'static> {
    let items = app.dupe_items(group);
    let each = app
        .dupes
        .as_ref()
        .and_then(|r| r.groups.get(group))
        .map(|g| g.bytes_each)
        .unwrap_or(0);
    let wasted = each * (items.len().saturating_sub(1)) as u64;
    let name = items
        .first()
        .map(|id| app.tree.node(*id).name.to_string())
        .unwrap_or_default();
    let arrow = if app.dupes_is_open(group) { '\u{25be}' } else { '\u{25b8}' };
    let head = format!(
        " {arrow} {} copies of {} \u{b7} {} each ",
        items.len(),
        truncate(&name, 24),
        human(each)
    );
    let note = format!(" {} reclaimable ", human(wasted));
    let rule = width.saturating_sub(cols(&head) + cols(&note) + 1);
    let line = Line::from(vec![
        Span::styled(head, theme.emphasis),
        Span::styled(note, theme.bar_hot),
        Span::styled("\u{2500}".repeat(rule), theme.dim),
    ]);
    if selected { line.style(theme.selection) } else { line }
}

/// A row that is most of its parent gets the hot colour: that is the one worth
/// looking at.
fn is_hot(bytes: u64, max: u64) -> bool {
    max > 0 && bytes * 2 >= max
}

fn bar(bytes: u64, max: u64) -> String {
    if max == 0 {
        return " ".repeat(BAR_WIDTH);
    }
    let eighths = (bytes as u128 * (BAR_WIDTH as u128 * 8) / max as u128) as usize;
    let full = eighths / 8;
    let rem = eighths % 8;
    let mut s = "█".repeat(full.min(BAR_WIDTH));
    if full < BAR_WIDTH && rem > 0 {
        s.push_str(EIGHTHS[rem]);
    }
    let used = full.min(BAR_WIDTH) + usize::from(full < BAR_WIDTH && rem > 0);
    s.push_str(&" ".repeat(BAR_WIDTH.saturating_sub(used)));
    s
}

