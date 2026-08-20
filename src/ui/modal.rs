//! The confirm and progress overlays.
//!
//! This is the last screen between a keypress and losing data, so it states the
//! whole truth plainly: what will go, how much comes back, where it goes, and
//! what was refused.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::Theme;
use crate::app::App;
use crate::delete::Disposal;
use crate::format::human;

pub fn draw_confirm(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let items = app.batch_items();
    let total: u64 = items.iter().map(|(_, b)| b).sum();
    let permanent = app.disposal == Disposal::Permanent;

    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{} ", items.len()), theme.emphasis),
        Span::styled("item(s), ", theme.normal),
        Span::styled(human(total), theme.emphasis),
        Span::styled(" reclaimed", theme.normal),
    ])];

    lines.push(Line::from(if permanent {
        Span::styled("permanently deleted — this cannot be undone", theme.staged)
    } else {
        Span::styled("moved to the Trash — u puts them back", theme.normal)
    }));
    lines.push(Line::from(""));

    // Show the biggest handful; the tail is what the count is for.
    const SHOWN: usize = 8;
    for (path, bytes) in items.iter().take(SHOWN) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:>8}  ", human(*bytes)), theme.emphasis),
            Span::styled(compress(&path.display().to_string(), 52), theme.dim),
        ]));
    }
    if items.len() > SHOWN {
        lines.push(Line::from(Span::styled(
            format!("        … and {} more", items.len() - SHOWN),
            theme.dim,
        )));
    }

    if !app.refused.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("refused:", theme.warn)));
        for (id, why) in app.refused.iter().take(3) {
            lines.push(Line::from(Span::styled(
                format!("  {} — {why}", app.tree.node(*id).name),
                theme.warn,
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" enter ", theme.mode_badge),
        Span::styled(
            format!(" {}   ", app.disposal.label()),
            if permanent { theme.staged } else { theme.normal },
        ),
        Span::styled(" D ", theme.emphasis),
        Span::styled(
            if permanent { "back to Trash   " } else { "delete permanently   " },
            theme.dim,
        ),
        Span::styled(" esc ", theme.emphasis),
        Span::styled("cancel", theme.dim),
    ]));

    let title = if permanent { " permanently delete " } else { " move to Trash " };
    popup(f, theme, area, title, lines, permanent);
}

pub fn draw_progress(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let Some(job) = app.job.as_ref() else { return };
    let done = job.done.len();
    let failures = job.failures();

    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{done}/{} ", job.total), theme.emphasis),
        Span::styled("done · ", theme.dim),
        Span::styled(human(job.freed()), theme.emphasis),
        Span::styled(" reclaimed", theme.dim),
    ])];
    lines.push(Line::from(progress_bar(done, job.total, 44)));

    if !failures.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("{} failed:", failures.len()),
            theme.warn,
        )));
        for o in failures.iter().take(4) {
            let why = o.result.as_ref().err().cloned().unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("  {} — {why}", compress(&o.path.display().to_string(), 40)),
                theme.warn,
            )));
        }
    }

    lines.push(Line::from(""));
    if job.is_finished() {
        let hint = if job.disposal == Disposal::Trash && failures.len() < job.total {
            "enter to close · u to undo this batch"
        } else {
            "enter to close"
        };
        lines.push(Line::from(Span::styled(hint, theme.dim)));
    } else {
        lines.push(Line::from(Span::styled("working…", theme.dim)));
    }

    popup(f, theme, area, " deleting ", lines, false);
}

fn progress_bar(done: usize, total: usize, width: usize) -> Line<'static> {
    let filled = if total == 0 { width } else { done * width / total };
    Line::from(vec![
        Span::from("\u{2588}".repeat(filled)).cyan(),
        Span::from("\u{2591}".repeat(width - filled)).dark_gray(),
    ])
}

fn popup(f: &mut Frame, theme: &Theme, area: Rect, title: &str, lines: Vec<Line>, danger: bool) {
    let w = 64u16.min(area.width.saturating_sub(4));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(if danger { theme.staged } else { theme.border_focus })
                .title(title.to_string().bold()),
        ),
        rect,
    );
}

/// Keep the start and the end of a path, drop the middle: both halves carry
/// information, the middle rarely does.
fn compress(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    let tail = width * 2 / 3;
    let head = width - tail - 1;
    format!(
        "{}…{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    )
}
