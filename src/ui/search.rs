//! Finding an entry anywhere in the tree.
//!
//! The list is the whole answer, so it gets the middle of the screen and shows
//! paths rather than names: two hundred things called `node_modules` are told
//! apart by where they are, and nothing else.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::Theme;
use crate::app::App;
use crate::format::human;
use crate::tree::flags;

pub fn draw(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let w = 78u16.min(area.width.saturating_sub(4));
    // As tall as it has to be and no taller: a query with three hits should not
    // black out most of the screen to show three hits.
    let rows = app.search_hits.len() + usize::from(app.search_more > 0);
    let h = (rows as u16 + 6).clamp(7, 22).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    let width = inner.width as usize;

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" \u{203a} ", theme.dim),
            Span::styled(app.search.clone(), theme.emphasis),
            Span::styled("\u{2588}", theme.emphasis),
        ]),
        Line::from(""),
    ];

    // Room for the query, the blank line, and the footer.
    let body = (inner.height as usize).saturating_sub(4);

    if app.search.is_empty() {
        lines.push(Line::from(Span::styled(
            "type to find any entry in the tree, wherever it is",
            theme.dim,
        )));
    } else if app.search_hits.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("nothing in the tree matches \"{}\"", app.search),
            theme.dim,
        )));
    }

    // Keep the selected hit on screen; a hundred results is exactly when it
    // stops being automatic.
    let offset = app
        .search_cursor
        .saturating_sub(body.saturating_sub(1))
        .min(app.search_hits.len().saturating_sub(body));

    for (i, (id, bytes)) in app.search_hits.iter().enumerate().skip(offset).take(body) {
        let n = app.tree.node(*id);
        // The path, not the name: what tells one `node_modules` from another is
        // where it is, and nothing else on the row does that.
        let path = app.tree.path(*id);
        let shown = path.strip_prefix(app.tree.root_path()).unwrap_or(&path).display().to_string();
        let size = human(*bytes);
        let room = width.saturating_sub(size.chars().count() + 4);
        let style = if n.flags & flags::IS_DIR != 0 { theme.dir } else { theme.normal };
        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<room$}", super::compress(&shown, room), room = room),
                style,
            ),
            Span::raw(" "),
            Span::styled(size, theme.emphasis),
        ]);
        lines.push(if i == app.search_cursor { line.style(theme.selection) } else { line });
    }

    // Never a top-N passed off as the whole set.
    if app.search_more > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "  showing the {} biggest \u{b7} {} more match",
                app.search_hits.len(),
                app.search_more
            ),
            theme.dim,
        )));
    }

    while lines.len() + 1 < inner.height as usize {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(vec![
        Span::styled(" enter ", theme.mode_badge),
        Span::styled(" go there   ", theme.dim),
        Span::styled(" \u{2191} \u{2193} ", theme.emphasis),
        Span::styled("choose   ", theme.dim),
        Span::styled(" esc ", theme.emphasis),
        Span::styled("back", theme.dim),
    ]));

    f.render_widget(Clear, rect);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_focus)
            .title(" find ".bold()),
        rect,
    );
    f.render_widget(Paragraph::new(lines), inner);
}
