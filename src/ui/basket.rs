//! The staging basket: the whole batch on one screen, and editable there.
//!
//! Staging happens over minutes, across a tree and two other views, and the
//! last thing between it and a delete used to be a modal listing the biggest
//! eight. This is the screen where the batch can be read and corrected — the
//! answer to "wait, what did I put in here?" without going back to find the row.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::Theme;
use crate::app::{App, BasketRow};
use crate::format::human;

/// Two counts, never one sum: the trashable half and the permanent half are
/// different operations and adding their totals would imply one number that is
/// true of neither.
fn title(app: &App) -> String {
    let files = format!(" staged \u{b7} {} item(s) \u{b7} {} ", app.staged.len(), human(app.staged_bytes()));
    if app.staged_tools.is_empty() {
        return files;
    }
    format!(
        "{files}+ {} from tools \u{b7} {} ",
        app.staged_tools.len(),
        app.staged_tool_freed().short()
    )
}

pub fn draw(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let rows = app.basket_rows();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.staged)
        .title(title(app).bold());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let mut lines = Vec::new();

    // The number people are actually here for, when the platform will tell us.
    // Tool bytes reach it only where they really come back to this disk; see
    // `App::after_commit`.
    if let Some((after, before)) = app.after_commit() {
        lines.push(Line::from(vec![
            Span::styled("free after this batch  ", theme.dim),
            Span::styled(human(after), theme.emphasis),
            Span::styled(format!("   (now {})", human(before)), theme.dim),
        ]));
        lines.push(Line::from(""));
    }

    if rows.is_empty() {
        lines.push(Line::from(Span::styled("nothing staged", theme.dim)));
    }

    // Keep the cursor on screen: a long batch is exactly when this matters.
    let body = (inner.height as usize).saturating_sub(lines.len() + 2);
    let offset = app.basket_cursor.saturating_sub(body.saturating_sub(1)).min(
        rows.len().saturating_sub(body),
    );
    let width = inner.width as usize;

    for (i, row) in rows.iter().enumerate().skip(offset).take(body) {
        let selected = i == app.basket_cursor;
        let line = match row {
            BasketRow::Group { cat, count, bytes } => {
                let label = cat.map(|c| c.label()).unwrap_or("everything else");
                let head = format!(" {label} \u{b7} {count} \u{b7} {} ", human(*bytes));
                let rule = width.saturating_sub(head.chars().count() + 1);
                Line::from(vec![
                    Span::styled(head, theme.emphasis),
                    Span::styled("\u{2500}".repeat(rule), theme.dim),
                ])
            }
            BasketRow::ToolGroup { count, freed } => {
                // A floor, not a figure, whenever these share layers: what the
                // extra comes to depends on which of them share what.
                let head = format!(" tool storage \u{b7} {count} \u{b7} {} ", freed.label());
                let note = " permanent \u{b7} no trash, no undo ";
                let rule = width
                    .saturating_sub(head.chars().count() + note.chars().count() + 1);
                Line::from(vec![
                    Span::styled(head, theme.staged),
                    Span::styled(note, theme.warn),
                    Span::styled("\u{2500}".repeat(rule), theme.dim),
                ])
            }
            BasketRow::ToolItem(key) => {
                let name = app.tool_name(key);
                let cmd = crate::tools::remove_display(key);
                let room = width.saturating_sub(4);
                Line::from(vec![
                    Span::styled("  \u{25cf} ", theme.staged),
                    Span::styled(
                        super::compress(&format!("{name}   {cmd}"), room),
                        theme.normal,
                    ),
                ])
            }
            BasketRow::Item(id) => {
                let size = human(app.tree.size(*id, app.apparent));
                let path = app
                    .tree
                    .path(*id)
                    .strip_prefix(app.tree.root_path())
                    .unwrap_or(&app.tree.path(*id))
                    .display()
                    .to_string();
                let room = width.saturating_sub(size.len() + 5);
                Line::from(vec![
                    Span::styled("  \u{25cf} ", theme.staged),
                    Span::styled(format!("{:<room$}", super::compress(&path, room), room = room), theme.normal),
                    Span::styled(size, theme.emphasis),
                ])
            }
        };
        lines.push(if selected { line.style(theme.selection) } else { line });
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" enter ", theme.mode_badge),
        Span::styled(" review and commit   ", theme.dim),
        Span::styled(" space ", theme.emphasis),
        Span::styled("unstage   ", theme.dim),
        Span::styled(" C ", theme.emphasis),
        Span::styled("clear   ", theme.dim),
        Span::styled(" esc ", theme.emphasis),
        Span::styled("back", theme.dim),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}
