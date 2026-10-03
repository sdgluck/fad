//! Rendering. Two panes: the tree on the left, everything about the selection
//! on the right.

mod basket;
mod detail_pane;
mod history;
mod modal;
mod omissions;
mod search;
mod theme;
mod tree_pane;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Mode};
use crate::format::human;

pub use theme::Theme;
pub(crate) use tree_pane::indent as row_indent;

/// What the screen remembers from one key to the next that nothing outside
/// the interface has any reason to read.
#[derive(Default)]
pub struct UiState {
    /// First line of the help overlay on screen. Clamped by the draw, which is
    /// the only place that knows how long the wrapped text came out.
    pub help_scroll: usize,
    /// A quit was asked for with a batch staged, and the next key decides:
    /// `q` again goes, anything else stays.
    pub quit_armed: bool,
}

// Every width below is in terminal columns, never in chars. A CJK name or an
// emoji is one char and two columns, so counting chars let those rows run two
// columns long per character — off the edge of the pane, taking the size and
// the bar with them — and `{:<w$}` padding, which also counts chars, left the
// columns after them ragged.

/// How many terminal columns `s` takes.
pub(crate) fn cols(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn char_cols(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// The longest start of `s` that fits in `width` columns. A wide character
/// that would straddle the edge is left off whole, never split.
fn head_cols(s: &str, width: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += char_cols(c);
        if used > width {
            return &s[..i];
        }
    }
    s
}

/// The longest end of `s` that fits in `width` columns.
fn tail_cols(s: &str, width: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices().rev() {
        used += char_cols(c);
        if used > width {
            return &s[i + c.len_utf8()..];
        }
    }
    s
}

/// `s` padded with spaces to `width` columns: `{:<width$}`, counting columns.
pub(crate) fn pad(s: &str, width: usize) -> String {
    let w = cols(s);
    if w >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - w))
}

/// A list overlay's lines with its footer on the last row, whatever the list
/// did. The footer is the only place an overlay says how to leave it or act on
/// it, so on a short terminal it is the list that gives way: lines past the
/// room are dropped, a short list is padded down to it.
pub(crate) fn pin_footer(mut lines: Vec<Line<'static>>, footer: Line<'static>, height: usize) -> Vec<Line<'static>> {
    if height == 0 {
        return Vec::new();
    }
    lines.truncate(height - 1);
    lines.resize(height - 1, Line::from(""));
    lines.push(footer);
    lines
}

/// Keep the start and the end of a path, drop the middle: both halves carry
/// information, the middle rarely does.
pub(crate) fn compress(s: &str, width: usize) -> String {
    if cols(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let tail = width * 2 / 3;
    let head = width.saturating_sub(tail + 1);
    format!("{}\u{2026}{}", head_cols(s, head), tail_cols(s, tail))
}

/// Cut from the right, marking the cut.
pub(crate) fn truncate_end(s: &str, width: usize) -> String {
    if cols(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    format!("{}\u{2026}", head_cols(s, width - 1))
}

/// Cut from the left, marking the cut: the tail of a filename is what tells it
/// apart from its neighbours.
pub(crate) fn truncate_start(s: &str, width: usize) -> String {
    if cols(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    format!("\u{2026}{}", tail_cols(s, width - 1))
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
        Mode::Omissions => omissions::draw(f, app, &theme, area),
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
            Span::styled("   enter keep \u{b7} esc clear \u{b7} / again to refine", theme.dim),
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
        ("!", "what is not in these numbers: skipped, unreadable, ignored"),
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
