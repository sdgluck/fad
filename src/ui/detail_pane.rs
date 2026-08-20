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

fn draw_detail(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(" selection ".to_string());
    let inner = block.inner(area);
    f.render_widget(block, area);

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

    if let Some(ext) = app.ext_cache.as_ref().filter(|c| c.id == id && !c.items.is_empty()) {
        let title = if ext.partial { "\u{2500}\u{2500} by extension (sampled) " } else { "\u{2500}\u{2500} by extension " };
        lines.push(Line::from(Span::styled(title, theme.dim)));
        let widest = ext.items.iter().map(|r| r.bytes).max().unwrap_or(0);
        for r in &ext.items {
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
