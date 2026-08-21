//! Everything about the selection, and the batch waiting to be deleted.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::Theme;
use crate::app::App;
use crate::format::human;
use crate::tree::{NodeId, flags};

pub fn draw(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    // The staged box takes what its contents need and no more. It used to be a
    // fixed nine rows, six of which were blank on the empty basket you are
    // looking at for most of a session — and every one of those rows is a row
    // the breakdowns above could have been using.
    let staged = 2 + app.staged.len().clamp(1, 7) as u16;
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(staged)])
        .split(area);

    draw_detail(f, app, theme, split[0]);
    draw_staged(f, app, theme, split[1]);
}

/// A tool heading: what the tool says about this kind as a whole, and the
/// caveats that apply to all of it.
///
/// `None` for any other heading, which keeps the tree's own detail pane
/// untouched.
fn tool_heading_detail(
    app: &App,
    theme: &Theme,
    heading: crate::app::Heading,
) -> Option<Vec<Line<'static>>> {
    use crate::app::Heading;
    let report = app.tools.as_ref()?;
    let mut lines = Vec::new();

    let source = match heading {
        Heading::Tool(source, kind) => {
            let sr = report.source(source)?;
            let items = sr.items_of(kind);
            lines.push(Line::from(Span::styled(kind.label().to_string(), theme.emphasis)));
            lines.push(Line::from(Span::styled(source.label().to_string(), theme.dim)));

            if let Some((size, reclaimable)) = sr.total(kind) {
                lines.push(Line::from(vec![
                    Span::styled(human(size), theme.emphasis),
                    Span::styled(" held in total", theme.dim),
                ]));
                lines.push(Line::from(vec![
                    Span::styled(human(reclaimable), theme.emphasis),
                    Span::styled(" of that is going spare", theme.dim),
                ]));
                // The one thing a reader might reasonably try to check for
                // themselves, and would get a different answer to.
                lines.push(Line::from(Span::styled(
                    format!("{}'s own figure, not a sum of the rows", source.program()),
                    theme.dim,
                )));
                if items.iter().any(|i| sr.items[*i].shared() > 0) {
                    lines.push(Line::from(Span::styled(
                        "these share layers, so adding the rows up would count                          the shared ones more than once",
                        theme.dim,
                    )));
                }
            }

            let blocked = items.iter().filter(|i| !sr.items[**i].removable()).count();
            if blocked > 0 {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("{blocked} in use and left alone"),
                    theme.warn,
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(kind.note().to_string(), theme.dim)));
            lines.push(Line::from(Span::styled(
                "A stages everything here that can go",
                theme.dim,
            )));
            source
        }
        Heading::ToolStatus(source) => {
            let sr = report.source(source)?;
            lines.push(Line::from(Span::styled(source.label().to_string(), theme.emphasis)));
            if let Some(line) = sr.status.line(source) {
                lines.push(Line::from(Span::styled(line, theme.warn)));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "nothing is being claimed about its disk, because we do not know",
                theme.dim,
            )));
            source
        }
        _ => return None,
    };

    if let Some(sr) = report.source(source) {
        let notes = sr.backing.notes();
        if !notes.is_empty() {
            lines.push(Line::from(""));
            for n in notes {
                lines.push(Line::from(Span::styled(n, theme.warn)));
            }
        }
    }
    Some(lines)
}

/// The selection, when the selection is something a tool is holding.
///
/// The one number a tree row never has to explain is what its size means. Here
/// it does: what removing this alone frees, what it shares with its siblings,
/// what the tool itself calls it, and — because it is the whole point — the
/// exact command that would do it.
fn tool_detail(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let Some(r) = app.tool_at_cursor() else { return Vec::new() };
    let mut lines = Vec::new();

    lines.push(Line::from(Span::styled(r.name.clone(), theme.emphasis)));
    lines.push(Line::from(Span::styled(
        format!("{} \u{b7} {}", r.source.label(), r.kind.label()),
        theme.dim,
    )));

    if r.sized() {
        lines.push(Line::from(vec![
            Span::styled(human(r.bytes), theme.emphasis),
            Span::styled(" freed by removing this", theme.dim),
        ]));
        if r.shared() > 0 {
            lines.push(Line::from(Span::styled(
                format!("{} more is shared with other {} and stays", human(r.shared()), r.kind.label()),
                theme.dim,
            )));
        }
        // Docker counts in powers of ten and fad counts in powers of two, so
        // the two figures will not match digit for digit. Showing the tool's
        // own string is the difference between a unit and a discrepancy.
        if !r.reported.is_empty() && r.reported != human(r.bytes) {
            lines.push(Line::from(Span::styled(
                format!("{} says {}", r.source.program(), r.reported),
                theme.dim,
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            format!("{} does not report a size", r.source.program()),
            theme.warn,
        )));
    }

    if let Some(last) = &r.last_used {
        lines.push(Line::from(Span::styled(last.clone(), theme.dim)));
    }

    lines.push(Line::from(""));
    if let Some(why) = &r.blocked {
        lines.push(Line::from(Span::styled(format!("in use \u{2014} {why}"), theme.warn)));
        lines.push(Line::from(Span::styled("fad will not remove it", theme.warn)));
    } else {
        lines.push(Line::from(Span::styled("removed with", theme.dim)));
        lines.push(Line::from(Span::styled(crate::tools::remove_display(&r.key()), theme.emphasis)));
        lines.push(Line::from(Span::styled("permanent \u{2014} no trash, no undo", theme.staged)));
    }

    // The same promise `rebuild_command` makes for a build directory: not
    // "your next build handles it" but the command itself.
    if let Some(restore) = &r.restore {
        lines.push(Line::from(Span::styled("put back with", theme.dim)));
        lines.push(Line::from(Span::styled(restore.clone(), theme.emphasis)));
    }

    if !r.detail.is_empty() {
        lines.push(Line::from(""));
        for d in &r.detail {
            lines.push(Line::from(Span::styled(d.clone(), theme.dim)));
        }
    }

    if let Some(p) = &r.path {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            super::compress(&p.display().to_string(), 36),
            theme.dim,
        )));
        if app.tool_in_tree(Some(p)) {
            lines.push(Line::from(Span::styled("the tree above already counts this", theme.warn)));
        }
    }
    lines
}

fn draw_detail(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(" selection ".to_string());
    let inner = block.inner(area);
    f.render_widget(block, area);

    // A tool row has no node behind it, so it gets its own pane rather than a
    // tree row's shape with the fields blanked out. A tool *heading* has none
    // either — it carries the root's id as a placeholder, and without this the
    // pane would describe the scan root as if that were the selection.
    if let Some(row) = app.rows.get(app.cursor).copied() {
        if row.tool.is_some() {
            f.render_widget(
                Paragraph::new(tool_detail(app, theme)).wrap(Wrap { trim: true }),
                inner,
            );
            return;
        }
        if let Some(h) = row.header {
            if let Some(lines) = tool_heading_detail(app, theme, h) {
                f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
                return;
            }
        }
    }

    let Some(id) = app.selected() else { return };
    let n = app.tree.node(id);
    let path = app.tree.path(id);

    // Compact on purpose: this pane is 38 columns and the breakdown below it is
    // worth more rows than a spacious header would be.
    let (headline_bytes, other) = if app.apparent {
        (n.total_len, n.total_bytes)
    } else {
        (n.total_bytes, n.total_len)
    };

    let mut lines = vec![
        Line::from(Span::from(path.display().to_string()).bold()),
        Line::from(vec![
            Span::styled(human(headline_bytes), theme.emphasis),
            Span::styled(if app.apparent { " apparent" } else { " on disk" }, theme.dim),
        ]),
    ];

    // Absolute size says what this is; its share says whether it is the reason
    // the disk is full. 40G is a lot of anything and nothing at all out of 2T.
    let root_bytes = app.tree.size(app.tree.root(), app.apparent);
    let mut share = vec![
        Span::styled(percent(headline_bytes, root_bytes), theme.emphasis),
        Span::styled(" of scan", theme.dim),
    ];
    // Only when the parent is not the scan root: directly under it the two
    // shares are the same number twice.
    if let Some(parent) = n.parent.filter(|p| *p != app.tree.root()) {
        share.push(Span::styled(" \u{b7} ", theme.dim));
        share.push(Span::styled(
            percent(headline_bytes, app.tree.size(parent, app.apparent)),
            theme.emphasis,
        ));
        share.push(Span::styled(" of parent", theme.dim));
    }
    lines.push(Line::from(share));

    if n.flags & flags::IS_DIR != 0 {
        lines.push(Line::from(Span::styled(
            format!("{} files \u{b7} {} dirs", n.file_count, n.dir_count),
            theme.dim,
        )));
    }

    // Only worth saying when the two genuinely differ: sparse files, APFS
    // compression, or a directory full of either.
    // Compare the rendered strings, not the raw numbers: a 0.4% difference is
    // noise, and "20M apparent size" under "20M on disk" says nothing.
    if human(other) != human(headline_bytes) && other > 0 {
        lines.push(Line::from(vec![
            Span::styled(human(other), theme.normal),
            Span::styled(if app.apparent { " on disk" } else { " apparent size" }, theme.dim),
        ]));
    }
    lines.push(Line::from(Span::styled(age(n.mtime), theme.dim)));

    // Growth is more actionable than size. A cache that put on 12G this week is
    // a better target than a stable 20G one.
    if let Some(growth) = app.breakdown.as_ref().filter(|b| b.id == id).and_then(|b| b.growth) {
        let when = app.previous_at.map(since).unwrap_or_else(|| "the last scan".into());
        match growth {
            Some(delta) if human(delta.unsigned_abs()) != "0B" => {
                let sign = if delta > 0 { "+" } else { "\u{2212}" };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{sign}{}", human(delta.unsigned_abs())),
                        if delta > 0 { theme.bar_hot } else { theme.bar },
                    ),
                    Span::styled(format!(" since {when}"), theme.dim),
                ]));
            }
            Some(_) => {}
            None => lines.push(Line::from(Span::styled(
                format!("new since {when}"),
                theme.bar_hot,
            ))),
        }
    }

    // What it costs to get this back. The generic per-category note is a
    // reassurance; the command is an answer.
    if let Some(cat) = n.preset {
        lines.push(Line::from(""));
        match app.tree.rebuild_command(id) {
            Some(cmd) => {
                lines.push(Line::from(Span::styled("restore with", theme.dim)));
                lines.push(Line::from(Span::styled(cmd.to_string(), theme.emphasis)));
            }
            None => lines.push(Line::from(Span::styled(cat.note(), theme.dim))),
        }
    }

    if n.flags & flags::CLOUD != 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "cloud folder \u{2014} not counted.",
            theme.warn,
        )));
        lines.push(Line::from(Span::styled(
            "Rerun with --cloud to include it.",
            theme.dim,
        )));
    }

    let b = app.breakdown.as_ref().filter(|c| c.id == id);

    // Where the size actually is. Standing on a 168G directory this is the
    // question the tree pane can only answer by being expanded, one level at a
    // time, by hand.
    if let Some(b) = b.filter(|b| b.child_count > 1) {
        lines.push(rule("where it goes", theme));
        let widest = b.children.first().map(|c| c.bytes).unwrap_or(0);
        for c in &b.children {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<12}", shorten(&c.name, 12)), theme.normal),
                Span::styled(format!("{:>6} ", human(c.bytes)), theme.emphasis),
                Span::styled(format!("{:>4} ", percent(c.bytes, headline_bytes)), theme.dim),
                Span::styled(minibar(c.bytes, widest), theme.bar),
            ]));
        }
        // The line that turns three rows into a verdict: either a couple of
        // children are the whole problem, or the size is spread thin and no
        // single deletion is going to help.
        if b.child_count > b.children.len() {
            let top: u64 = b.children.iter().map(|c| c.bytes).sum();
            lines.push(Line::from(Span::styled(
                format!(
                    "  top {} of {} hold {}",
                    b.children.len(),
                    b.child_count,
                    percent(top, headline_bytes)
                ),
                theme.dim,
            )));
        }
    }

    // Fill whatever rows are left with breakdowns, in order, until they run
    // out. On a tall terminal that is all four, and `S` only reorders them; on
    // a short one it is however many fit, and `S` is the way to the rest.
    //
    // The pane does not scroll, so the overview is built first and these last:
    // the breakdowns are the half that gives way, never the path or the size.
    let mut room = (inner.height as usize).saturating_sub(rows_used(&lines, inner.width));
    let mut sections: Vec<(String, Option<(usize, usize)>, Vec<Line<'static>>)> = Vec::new();
    let mut whole = true;
    if let Some(b) = b {
        for panel in app.panel.rotation() {
            let Some(section) = section_rows(panel, b, theme) else { continue };
            // Two rows is the floor: a heading and one row under it. Below that
            // there is nothing left to show that would not be a heading on its
            // own.
            if room < 2 {
                whole = false;
                break;
            }
            let (label, mut rows) = section;
            let mut cut = None;
            if rows.len() + 1 > room {
                // A heading over one row of six reads as "there is one", which
                // is the one thing this pane must never do. Room is what it is
                // on a short terminal, so a cut list says in its heading how
                // much of itself you are looking at.
                whole = false;
                cut = Some((room - 1, rows.len()));
                rows.truncate(room - 1);
            }
            room -= rows.len() + 1;
            sections.push((label, cut, rows));
        }
    }

    // The key is only worth mentioning when pressing it would show you
    // something you cannot already see. Once all four are on screen it only
    // reorders them, and a hint on every heading would be four times the noise
    // for none of the information.
    let last = sections.len().saturating_sub(1);
    for (i, (label, cut, rows)) in sections.into_iter().enumerate() {
        lines.push(rule(&heading(&label, cut, !whole && i == last, inner.width), theme));
        lines.extend(rows);
    }

    truncate_to(&mut lines, inner);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// How many rows these lines will occupy once `Wrap` has had them. Only the
/// path is ever wide enough to need more than one, but it always is.
fn rows_used(lines: &[Line<'static>], width: u16) -> usize {
    let w = width as usize;
    if w == 0 {
        return usize::MAX;
    }
    lines.iter().map(|l| l.width().div_ceil(w).max(1)).sum()
}

/// A section heading.
fn rule(label: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(format!("\u{2500}\u{2500} {label} "), theme.dim))
}

/// A breakdown's heading, guaranteed to fit on one row.
///
/// It has to: the row budget above was decided before this ran, and a heading
/// that wrapped would take a row from the list it introduces and make its own
/// `top N of M` a lie. So the parts go on in order of what they are worth —
/// what this is, how much of it you can see, and only then which key shows you
/// the rest — and whatever does not fit is left off.
fn heading(label: &str, cut: Option<(usize, usize)>, more: bool, width: u16) -> String {
    // `rule` frames it as "── {head} ", so three columns are already spent.
    let room = (width as usize).saturating_sub(4);
    // The pane's width is fixed, so no label reaches this — but the guarantee
    // is the point of the function, not a comment about one.
    let mut head: String = label.chars().take(room).collect();
    if let Some((shown, total)) = cut {
        let with = format!("{head} \u{b7} top {shown} of {total}");
        if with.chars().count() <= room {
            head = with;
        }
    }
    if more {
        let with = format!("{head} \u{b7} S");
        if with.chars().count() <= room {
            head = with;
        }
    }
    head
}

/// One breakdown, as its heading text and its rows. All four come out of the
/// same `Breakdown`, so laying out four costs no more than laying out one and
/// reordering them is a redraw rather than a recount. The caller owns the
/// heading, because how many rows fit is only known once everything above has
/// been laid out.
fn section_rows(
    panel: crate::app::Panel,
    b: &crate::app::Breakdown,
    theme: &Theme,
) -> Option<(String, Vec<Line<'static>>)> {
    use crate::app::Panel;
    let mut lines = Vec::new();
    let label = if b.partial {
        format!("{} (sampled)", panel.label())
    } else {
        panel.label().to_string()
    };

    match panel {
        Panel::Extensions => {
            if b.exts.is_empty() {
                return None;
            }
            let widest = b.exts.iter().map(|r| r.bytes).max().unwrap_or(0);
            for r in &b.exts {
                let name =
                    if r.ext.is_empty() { "(no ext)".to_string() } else { format!(".{}", r.ext) };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<10}", shorten(&name, 10)), theme.normal),
                    Span::styled(format!("{:>6} ", human(r.bytes)), theme.emphasis),
                    Span::styled(format!("{:>5} ", tally(r.count)), theme.dim),
                    Span::styled(minibar(r.bytes, widest), theme.bar),
                ]));
            }
        }
        Panel::Ages => {
            if !b.ages.iter().any(|v| *v > 0) {
                return None;
            }
            let widest = b.ages.iter().copied().max().unwrap_or(0);
            for (i, (name, _)) in crate::app::AGE_BUCKETS.iter().enumerate() {
                let bytes = b.ages[i];
                lines.push(Line::from(vec![
                    Span::styled(format!("  {name:<10}"), theme.normal),
                    Span::styled(format!("{:>7} ", human(bytes)), theme.emphasis),
                    // The oldest bucket is the answer to "what can go", so it
                    // gets the colour that the tree pane reserves for the row
                    // worth looking at.
                    Span::styled(
                        minibar(bytes, widest),
                        if i == 3 { theme.bar_hot } else { theme.bar },
                    ),
                ]));
            }
        }
        Panel::Biggest => {
            if b.biggest.is_empty() {
                return None;
            }
            let widest = b.biggest.first().map(|r| r.bytes).unwrap_or(0);
            for r in &b.biggest {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<14}", shorten(&r.name, 14)), theme.normal),
                    Span::styled(format!("{:>6} ", human(r.bytes)), theme.emphasis),
                    Span::styled(minibar(r.bytes, widest), theme.bar),
                ]));
            }
        }
        Panel::Sizes => {
            if !b.sizes.iter().any(|(_, c)| *c > 0) {
                return None;
            }
            let widest = b.sizes.iter().map(|(bytes, _)| *bytes).max().unwrap_or(0);
            for (i, (name, _)) in crate::app::SIZE_CLASSES.iter().enumerate() {
                let (bytes, count) = b.sizes[i];
                lines.push(Line::from(vec![
                    Span::styled(format!("  {name:<8}"), theme.normal),
                    Span::styled(format!("{:>6} ", human(bytes)), theme.emphasis),
                    Span::styled(format!("{:>5} ", tally(count)), theme.dim),
                    // The biggest files are the ones worth deleting one at a
                    // time, so that class gets the colour worth looking at.
                    Span::styled(
                        minibar(bytes, widest),
                        if i == 0 { theme.bar_hot } else { theme.bar },
                    ),
                ]));
            }
        }
    }
    Some((label, lines))
}

/// Drop whatever will not fit. `Wrap` is on, so a line wider than the pane
/// costs more than one row and has to be counted as such.
fn truncate_to(lines: &mut Vec<Line<'static>>, inner: Rect) {
    let (w, h) = (inner.width as usize, inner.height as usize);
    if w == 0 {
        lines.clear();
        return;
    }
    let mut used = 0usize;
    for (i, line) in lines.iter().enumerate() {
        used += line.width().div_ceil(w).max(1);
        if used > h {
            lines.truncate(i);
            return;
        }
    }
}

/// `bytes` as a share of `of`. Nothing is a share of nothing, and a
/// still-scanning root has a total of zero.
fn percent(bytes: u64, of: u64) -> String {
    if of == 0 {
        return "\u{2014}".into();
    }
    format!("{}%", (bytes as u128 * 100 / of as u128).min(100))
}

/// A file count in five columns. The exact number stops meaning anything
/// somewhere around ten thousand, and the pane has not got the columns for it.
fn tally(n: u64) -> String {
    match n {
        n if n < 10_000 => n.to_string(),
        n if n < 1_000_000 => format!("{:.0}k", n as f64 / 1000.0),
        n => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// A short bar for a breakdown row, scaled against the largest of them.
fn minibar(bytes: u64, max: u64) -> String {
    const W: usize = 8;
    if max == 0 {
        return String::new();
    }
    let cells = (bytes as u128 * W as u128 / max as u128) as usize;
    "\u{2588}".repeat(cells.min(W))
}

fn draw_staged(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let title = if app.staged.is_empty() {
        " staged ".to_string()
    } else {
        format!(" staged \u{b7} {} \u{b7} {} ", app.staged.len(), human(app.staged_bytes()))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if app.staged.is_empty() { theme.border } else { theme.staged })
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.staged.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "space stages the selection",
                theme.dim,
            ))),
            inner,
        );
        return;
    }

    let mut items: Vec<NodeId> = app.staged.iter().copied().collect();
    items.sort_unstable_by_key(|id| std::cmp::Reverse(app.tree.size(*id, app.apparent)));

    let width = inner.width as usize;
    let lines: Vec<Line> = items
        .iter()
        .take(inner.height as usize)
        .map(|id| {
            let n = app.tree.node(*id);
            let size = human(app.tree.size(*id, app.apparent));
            let name_w = width.saturating_sub(size.len() + 3);
            Line::from(vec![
                Span::styled("\u{25cf} ", theme.staged),
                Span::styled(format!("{:<name_w$}", shorten(&n.name, name_w)), theme.normal),
                Span::styled(size, theme.emphasis),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn shorten(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count <= width {
        return s.to_string();
    }
    format!("…{}", s.chars().skip(count - width + 1).collect::<String>())
}

/// When the last scan was, phrased the way someone would say it out loud.
fn since(at: std::time::SystemTime) -> String {
    let Ok(ago) = at.elapsed() else { return "the last scan".into() };
    let secs = ago.as_secs();
    match secs {
        s if s < 3600 => "an hour ago".into(),
        s if s < 36 * 3600 => format!("{} hours ago", s / 3600),
        s if s < 60 * 86400 => format!("{} days ago", s / 86400),
        s => format!("{} months ago", s / (30 * 86400)),
    }
}

/// Rough, human relative time. Precision past "days" is not useful here.
fn age(mtime: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(mtime);
    let secs = (now - mtime).max(0);
    let (n, unit) = match secs {
        s if s < 90 => (s, "second"),
        s if s < 90 * 60 => (s / 60, "minute"),
        s if s < 36 * 3600 => (s / 3600, "hour"),
        s if s < 60 * 86400 => (s / 86400, "day"),
        s if s < 2 * 365 * 86400 => (s / (30 * 86400), "month"),
        s => (s / (365 * 86400), "year"),
    };
    format!("modified {n} {unit}{} ago", if n == 1 { "" } else { "s" })
}
