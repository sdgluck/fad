//! Rendering. Two panes: the tree on the left, everything about the selection
//! on the right.

mod detail_pane;
mod modal;
mod theme;
mod tree_pane;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Mode};
use crate::format::human;

pub use theme::Theme;

pub fn draw(f: &mut Frame, app: &mut App) {
    let theme = Theme::default();
    let area = f.area();
    app.ensure_breakdown();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    // The detail pane needs a fixed, readable width; the tree takes the rest.
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(38)])
        .split(chunks[0]);

    tree_pane::draw(f, app, &theme, panes[0]);
    detail_pane::draw(f, app, &theme, panes[1]);
    draw_status(f, app, &theme, chunks[1]);

    match app.mode {
        Mode::Help => draw_help(f, &theme, area),
        Mode::Confirm => modal::draw_confirm(f, app, &theme, area),
        Mode::Deleting => modal::draw_progress(f, app, &theme, area),
        _ => {}
    }
}

fn draw_status(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let line = match app.mode {
        Mode::Filter => Line::from(vec![
            Span::styled(" filter ", theme.mode_badge),
            Span::raw(" "),
            Span::styled(app.filter.clone(), theme.emphasis),
            Span::styled("\u{2588}", theme.emphasis),
            Span::styled("   enter accept \u{b7} esc clear", theme.dim),
        ]),
        _ => {
            let mut spans = vec![];
            if !app.staged.is_empty() {
                spans.push(Span::styled(
                    format!(" {} staged \u{b7} {} ", app.staged.len(), human(app.staged_bytes())),
                    theme.staged_badge,
                ));
                spans.push(Span::raw(" "));
            }
            if let Some(msg) = &app.status {
                spans.push(Span::styled(msg.clone(), theme.emphasis));
            } else {
                spans.push(Span::styled(
                    "space stage \u{b7} x commit \u{b7} r reclaimable \u{b7} / filter \u{b7} s sort \u{b7} ? help \u{b7} q quit",
                    theme.dim,
                ));
            }
            Line::from(spans)
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_help(f: &mut Frame, theme: &Theme, area: Rect) {
    use ratatui::widgets::{Block, Borders, Clear};

    const KEYS: &[(&str, &str)] = &[
        ("j / k, \u{2193} \u{2191}", "move"),
        ("g / G", "first / last"),
        ("ctrl-d / ctrl-u", "half page"),
        ("l / \u{2192} / enter", "expand, or open a category"),
        ("h / \u{2190}", "collapse, or jump to parent"),
        ("space", "stage / unstage"),
        ("A", "stage every child of this directory"),
        ("x", "commit the staged batch"),
        ("u", "undo the last committed batch"),
        ("/", "fuzzy filter"),
        ("r", "reclaimable view: build artifacts, caches, VM images"),
        ("a", "cycle age filter: any, 90 days, 1 year, 2 years untouched"),
        ("d", "duplicate view: files whose contents are byte-for-byte equal"),
        ("s", "cycle sort: size, count, modified, name"),
        ("o / e / y", "Finder / $EDITOR / copy path"),
        ("R", "rescan the selected subtree"),
        ("q", "quit"),
    ];

    let w = 58u16.min(area.width.saturating_sub(4));
    let h = (KEYS.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };

    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<16}"), theme.emphasis),
                Span::styled((*d).to_string(), theme.normal),
            ])
        })
        .collect();

    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border_focus)
                .title(" keys ".bold()),
        ),
        popup,
    );
}
