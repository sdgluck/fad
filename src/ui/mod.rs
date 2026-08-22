//! Rendering. Two panes: the tree on the left, everything about the selection
//! on the right.

mod basket;
mod detail_pane;
mod history;
mod modal;
mod search;
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

/// Keep the start and the end of a path, drop the middle: both halves carry
/// information, the middle rarely does.
pub(crate) fn compress(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    let tail = width * 2 / 3;
    let head = width.saturating_sub(tail + 1);
    format!(
        "{}\u{2026}{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    )
}

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
        Mode::Search => search::draw(f, app, &theme, area),
        Mode::Basket => basket::draw(f, app, &theme, area),
        Mode::History => history::draw(f, app, &theme, area),
        Mode::Confirm => modal::draw_confirm(f, app, &theme, area),
        Mode::EmptyTrash => modal::draw_empty_trash(f, app, &theme, area),
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
            // Its own badge, never folded into the one above. One is bytes on
            // this disk that will go to the trash; the other is a range of
            // bytes inside a tool that will not come back. A single figure
            // spanning both would be true of neither.
            if !app.staged_tools.is_empty() {
                spans.push(Span::styled(
                    format!(
                        " {} from tools \u{b7} {} ",
                        app.staged_tools.len(),
                        app.staged_tool_freed().short()
                    ),
                    theme.staged_badge,
                ));
                spans.push(Span::raw(" "));
            }
            if let Some(msg) = &app.status {
                spans.push(Span::styled(msg.clone(), theme.emphasis));
            } else if app.dupe_view {
                // The one view with a second, better verb: the hint is the only
                // place anyone will find it.
                spans.push(Span::styled(
                    "A delete all but the newest \u{b7} L share one copy instead, keeping every path \u{b7} d back",
                    theme.dim,
                ));
            } else {
                spans.push(Span::styled(
                    "space stage \u{b7} x basket \u{b7} r reclaimable \u{b7} t tools \u{b7} / filter \u{b7} s sort \u{b7} ? help \u{b7} q quit",
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
        ("x", "open the staging basket: review, edit, commit"),
        ("u", "undo the last committed batch"),
        ("E", "empty what fad put in the trash \u{2014} the space is not back until you do"),
        ("U", "the undo history: put any remembered batch back"),
        ("/", "fuzzy filter: narrow what is on screen"),
        ("f", "find: every entry in the tree by name, biggest first, and jump to one"),
        ("r", "reclaimable view: build artifacts, caches, VM images"),
        ("t", "tool storage: what Docker and friends hold that a walk cannot see"),
        ("a", "cycle age filter: any, 90 days, 1 year, 2 years untouched"),
        ("d", "duplicate view: files whose contents are byte-for-byte equal"),
        ("L", "in the duplicate view: share one copy of the storage, keeping every path"),
        ("s", "cycle sort: size, count, modified, name"),
        ("S", "bring the next detail breakdown to the top, when they do not all fit"),
        ("o / e / y", "Finder / $EDITOR / copy path"),
        ("i", "never rank this again \u{2014} adds it to your ignore list"),
        ("R", "rescan the selected subtree"),
        ("click", "select \u{b7} on the arrow: open \u{b7} on the left edge: stage"),
        ("wheel", "move the selection"),
        ("q", "quit"),
    ];

    let w = 66u16.min(area.width.saturating_sub(4));
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
