//! The tree: one row per visible node, with a size bar scaled against siblings.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::Theme;
use crate::app::{App, Heading};
use crate::format::human;
use crate::tree::flags;

/// Eighth-block glyphs give the bar eight times the resolution of whole cells,
/// which matters when a row is one twentieth of its parent.
const EIGHTHS: [&str; 9] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];

const BAR_WIDTH: usize = 12;

pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let title = header(app);
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

fn header(app: &App) -> Line<'static> {
    let root = app.tree.node(app.tree.root());
    let path = app.tree.root_path().display().to_string();
    let mut spans = vec![
        Span::from(" ").into(),
        Span::from(path).bold(),
        Span::from("  "),
        Span::from(human(app.tree.size(app.tree.root(), app.apparent))).bold(),
    ];
    if app.apparent {
        spans.push(Span::from(" apparent"));
    }
    if app.scanning() {
        spans.push(Span::from(format!(
            "  scanning {} dirs, {} files ",
            root.dir_count, root.file_count
        )));
    } else {
        spans.push(Span::from(format!(
            "  {} dirs, {} files ",
            root.dir_count, root.file_count
        )));
    }
    if app.reclaim_view {
        spans.push(Span::from(" reclaimable ").bold().reversed());
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
    Line::from(spans)
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
            let cells = width.saturating_sub(left.chars().count() + 4);
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
                    " \u{26a0} {} in the trash from {n} item(s) \u{2014} not reclaimed until you empty it",
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

fn truncate_end(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    s.chars().take(width.saturating_sub(1)).collect::<String>() + "\u{2026}"
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
        };
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

    let indent = "  ".repeat(row.depth as usize);
    let bytes = app.tree.size(row.id, app.apparent);
    let size = human(bytes);
    let bar = bar(bytes, row.sibling_max);

    // Name column gets whatever the fixed columns leave behind.
    let fixed = 1 + 1 + indent.len() + 2 + 8 + 1 + BAR_WIDTH + 1;
    let name_w = width.saturating_sub(fixed).max(6);
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
        Span::styled(format!("{name:<name_w$}"), name_style),
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
    let rule = width.saturating_sub(head.chars().count() + note.chars().count() + 1);
    let line = Line::from(vec![
        Span::styled(head, theme.emphasis),
        Span::styled(note, theme.dim),
        Span::styled("\u{2500}".repeat(rule), theme.dim),
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
    let rule = width.saturating_sub(head.chars().count() + note.chars().count() + 1);
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

/// Truncate from the left: the tail of a filename is what distinguishes it.
fn truncate(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count <= width {
        return s.to_string();
    }
    let skip = count - width + 1;
    format!("…{}", s.chars().skip(skip).collect::<String>())
}
