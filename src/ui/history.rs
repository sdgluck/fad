//! The undo journal, on screen.
//!
//! `u` reaches the top of the stack. The other nineteen batches were already
//! being recorded; they were just unreachable, which made "undo the last batch"
//! feel like the only chance you got.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::Theme;
use crate::app::App;
use crate::format::human;

pub fn draw(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_focus)
        .title(" undo history ".bold());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let mut lines = Vec::new();
    if app.history.is_empty() {
        lines.push(Line::from(Span::styled(
            "nothing has been trashed from here yet",
            theme.dim,
        )));
    }

    // Room for the empty-state line, if any, and the footer.
    let body = (inner.height as usize).saturating_sub(lines.len() + 1);
    let offset = app
        .history_cursor
        .saturating_sub(body.saturating_sub(1))
        .min(app.history.len().saturating_sub(body));

    for (i, batch) in app.history.iter().enumerate().skip(offset).take(body) {
        let (n, bytes) = batch.recoverable();
        let total = batch.entries.len();
        // A batch whose items have since been emptied out of the trash cannot
        // come back, and saying so here is cheaper than finding out on enter.
        let state = if n == 0 {
            Span::styled("  emptied \u{2014} cannot be restored", theme.warn)
        } else if n < total {
            Span::styled(format!("  {n} of {total} still in the trash"), theme.warn)
        } else {
            Span::styled(format!("  {total} item(s)"), theme.dim)
        };
        let line = Line::from(vec![
            Span::styled(format!(" {:<22}", when(batch.at)), theme.normal),
            Span::styled(format!("{:>8}", human(bytes)), theme.emphasis),
            state,
        ]);
        lines.push(if i == app.history_cursor { line.style(theme.selection) } else { line });
    }

    let footer = Line::from(vec![
        Span::styled(" enter ", theme.mode_badge),
        Span::styled(" put this batch back   ", theme.dim),
        Span::styled(" esc ", theme.emphasis),
        Span::styled("close", theme.dim),
    ]);

    let lines = super::pin_footer(lines, footer, inner.height as usize);
    f.render_widget(Paragraph::new(lines), inner);
}

/// How long ago, in the terms someone would use about their own afternoon.
fn when(at: u64) -> String {
    let now = crate::app::now_secs().max(0) as u64;
    let ago = now.saturating_sub(at);
    match ago {
        s if s < 90 => "just now".into(),
        s if s < 90 * 60 => format!("{} minutes ago", s / 60),
        s if s < 36 * 3600 => format!("{} hours ago", s / 3600),
        s if s < 60 * 86400 => format!("{} days ago", s / 86400),
        s => format!("{} months ago", s / (30 * 86400)),
    }
}
