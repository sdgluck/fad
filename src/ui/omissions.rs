//! What the scan did not count, and what to do about each kind.
//!
//! The banners at the bottom of the tree say how many of each there are, which
//! is enough to know a total is short and not enough to act on. This is the
//! same information with the paths attached and, on every heading, the flag or
//! the permission that would fix it.
//!
//! The two halves are kept apart on purpose. Something unreadable is missing
//! from every total above it and makes the headline wrong; something ignored is
//! counted and merely hidden. One figure spanning both would be true of
//! neither.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::Theme;
use crate::app::{App, Why};
use crate::format::human;

pub fn draw(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_focus)
        .title(" what is not in these numbers ".bold());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let width = inner.width as usize;

    let (uncounted, hidden, hidden_bytes) = app.omission_summary();
    let mut lines = Vec::new();

    if uncounted == 0 && hidden == 0 {
        lines.push(Line::from(Span::styled(
            "nothing was skipped \u{2014} every directory under the root was read",
            theme.normal,
        )));
    } else {
        if uncounted > 0 {
            lines.push(Line::from(vec![
                Span::styled(format!("{uncounted} "), theme.warn),
                Span::styled(
                    if uncounted == 1 {
                        "entry was not counted \u{2014} every total above it is short by whatever it holds"
                    } else {
                        "entries were not counted \u{2014} every total above them is short by whatever they hold"
                    },
                    theme.normal,
                ),
            ]));
        }
        if hidden > 0 {
            lines.push(Line::from(vec![
                Span::styled(format!("{hidden} "), theme.emphasis),
                Span::styled("hidden by your ignore list \u{b7} ", theme.normal),
                Span::styled(human(hidden_bytes), theme.emphasis),
                // The distinction the whole screen turns on.
                Span::styled(" \u{2014} counted, just not shown", theme.dim),
            ]));
        }
    }
    lines.push(Line::from(""));

    // Room for the summary above and the footer below. Headings take rows
    // too, which this used to leave out: a list with three kinds in it ran
    // three rows long and pushed the footer off the bottom.
    let body = (inner.height as usize).saturating_sub(lines.len() + 1);
    let mut offset = app
        .omission_cursor
        .saturating_sub(body.saturating_sub(1))
        .min(app.omissions.len().saturating_sub(body));
    // Walk the window down until the cursor's row, and every heading drawn
    // above it, fits.
    while offset < app.omission_cursor && rows_to(app, offset, app.omission_cursor) > body {
        offset += 1;
    }

    // Starts unset, so the first visible row always carries its heading. When
    // scrolling has pushed the real one off the top that heading is redrawn,
    // and a row is never on screen without the reason it is there.
    let mut last: Option<Why> = None;
    let used = lines.len();

    for (i, o) in app.omissions.iter().enumerate().skip(offset) {
        if lines.len() >= used + body {
            break;
        }
        if last != Some(o.why) {
            last = Some(o.why);
            let head = format!(" {} ", o.why.heading());
            // The banner already picks its remedy from the errors the walk
            // actually hit; this heading has to say the same thing, or a path
            // too long to open gets told to go and grant a permission.
            let note = match o.why {
                Why::Unreadable => {
                    let fix = app.tree.unreadable_fix();
                    // Its other answer points here, which is no answer on this screen.
                    let fix = if fix == crate::tree::PERMISSION_FIX { fix } else { "not a permissions problem" };
                    format!(" not counted \u{2014} {fix} ")
                }
                why => format!(" {} ", why.note()),
            };
            let rule = width.saturating_sub(super::cols(&head) + super::cols(&note) + 1);
            lines.push(Line::from(vec![
                Span::styled(head, theme.emphasis),
                Span::styled(note, if o.why.uncounted() { theme.warn } else { theme.dim }),
                Span::styled("\u{2500}".repeat(rule), theme.dim),
            ]));
        }

        // A size we do not have is left blank, never shown as zero. Not knowing
        // what an unread directory holds is the entire point of the row.
        let size = o.bytes.map(human).unwrap_or_else(|| "\u{2014}".into());
        let room = width.saturating_sub(super::cols(&size) + 4);
        let path = o.path.strip_prefix(app.tree.root_path()).unwrap_or(&o.path);
        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(
                super::pad(&super::compress(&path.display().to_string(), room), room),
                theme.normal,
            ),
            Span::raw(" "),
            Span::styled(size, theme.emphasis),
        ]);
        lines.push(if i == app.omission_cursor { line.style(theme.selection) } else { line });
    }

    let footer = Line::from(vec![
        Span::styled(" y ", theme.mode_badge),
        Span::styled(" copy the path   ", theme.dim),
        Span::styled(" j k ", theme.emphasis),
        Span::styled("move   ", theme.dim),
        Span::styled(" esc ", theme.emphasis),
        Span::styled("back", theme.dim),
    ]);
    let lines = super::pin_footer(lines, footer, inner.height as usize);

    f.render_widget(Paragraph::new(lines), inner);
}

/// Screen rows taken by entries `from..=to`, counting the heading drawn above
/// the first of them and above every change of kind after it.
fn rows_to(app: &App, from: usize, to: usize) -> usize {
    let mut last = None;
    let mut rows = 0;
    for o in &app.omissions[from..=to.min(app.omissions.len().saturating_sub(1))] {
        if last != Some(o.why) {
            last = Some(o.why);
            rows += 1;
        }
        rows += 1;
    }
    rows
}
