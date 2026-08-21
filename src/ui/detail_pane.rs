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
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(9)])
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

    // Compact on purpose: this pane is 38 columns and the extension breakdown
    // below it is worth more rows than a spacious header would be.
    let (headline_bytes, other) = if app.apparent {
        (n.total_len, n.total_bytes)
    } else {
        (n.total_bytes, n.total_len)
    };
    let mut headline = vec![
        Span::styled(human(headline_bytes), theme.emphasis),
        Span::styled(if app.apparent { " apparent" } else { " on disk" }, theme.dim),
    ];
    if n.flags & flags::IS_DIR != 0 {
        headline.push(Span::styled(
            format!(" \u{b7} {} files \u{b7} {} dirs", n.file_count, n.dir_count),
            theme.dim,
        ));
    }

    let mut lines = vec![
        Line::from(Span::from(path.display().to_string()).bold()),
        Line::from(headline),
    ];

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

    if let Some(b) = app.breakdown.as_ref().filter(|c| c.id == id) {
        let sampled = if b.partial { " (sampled)" } else { "" };
        if b.ages.iter().any(|v| *v > 0) {
            lines.push(Line::from(Span::styled(
                format!("\u{2500}\u{2500} by age{sampled} "),
                theme.dim,
            )));
            let widest = b.ages.iter().copied().max().unwrap_or(0);
            for (i, (label, _)) in crate::app::AGE_BUCKETS.iter().enumerate() {
                let bytes = b.ages[i];
                lines.push(Line::from(vec![
                    Span::styled(format!("  {label:<10}"), theme.normal),
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
    }

    if let Some(ext) = app.breakdown.as_ref().filter(|c| c.id == id && !c.exts.is_empty()) {
        let title = if ext.partial { "\u{2500}\u{2500} by extension (sampled) " } else { "\u{2500}\u{2500} by extension " };
        lines.push(Line::from(Span::styled(title, theme.dim)));
        let widest = ext.exts.iter().map(|r| r.bytes).max().unwrap_or(0);
        for r in &ext.exts {
            let label = if r.ext.is_empty() { "(no ext)".to_string() } else { format!(".{}", r.ext) };
            lines.push(Line::from(vec![
                Span::styled(format!("  {label:<10}"), theme.normal),
                Span::styled(format!("{:>7} ", human(r.bytes)), theme.emphasis),
                Span::styled(minibar(r.bytes, widest), theme.bar),
            ]));
        }
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// A short bar for the extension rows, scaled against the largest of them.
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
