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
use super::compress;

pub fn draw_confirm(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let items = app.batch_items();
    let tools = app.tool_batch_items();
    let total: u64 = items.iter().map(|(_, b)| b).sum();
    let permanent = app.disposal == Disposal::Permanent;

    // Fewer of each when both halves are present, so neither is pushed off.
    let shown = if tools.is_empty() { 8 } else { 4 };

    let mut lines = Vec::new();

    if !items.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", items.len()), theme.emphasis),
            Span::styled("item(s), ", theme.normal),
            Span::styled(human(total), theme.emphasis),
            Span::styled(" reclaimed", theme.normal),
        ]));
        lines.push(Line::from(if permanent {
            Span::styled("permanently deleted — this cannot be undone", theme.staged)
        } else {
            Span::styled("moved to the Trash — u puts them back", theme.normal)
        }));
        lines.push(Line::from(""));

        // Show the biggest handful; the tail is what the count is for.
        for (path, bytes) in items.iter().take(shown) {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>8}  ", human(*bytes)), theme.emphasis),
                Span::styled(compress(&path.display().to_string(), 52), theme.dim),
            ]));
        }
        if items.len() > shown {
            lines.push(Line::from(Span::styled(
                format!("        … and {} more", items.len() - shown),
                theme.dim,
            )));
        }
    }

    // The permanent half, always in its own block. `D` does not reach it: there
    // is no trash for `docker image rm`, so there is no choice to offer, and
    // sitting it among things that *can* come back is how someone reads one
    // line and assumes it applies to both.
    if !tools.is_empty() {
        if !items.is_empty() {
            lines.push(Line::from(""));
        }
        let freed = app.staged_tool_freed();
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", tools.len()), theme.staged),
            Span::styled("from tools, ", theme.normal),
            Span::styled(freed.label(), theme.staged),
            Span::styled(" freed", theme.normal),
        ]));
        lines.push(Line::from(Span::styled(
            "handed back to the tool — no trash, no undo, whatever D says",
            theme.staged,
        )));
        if !freed.is_exact() {
            // "at least" is not hedging, and saying why stops it reading as
            // hedging.
            lines.push(Line::from(Span::styled(
                "a floor because these share layers; the figure after is measured",
                theme.dim,
            )));
        }
        if let Some(note) = host_note(app) {
            lines.push(Line::from(Span::styled(note, theme.warn)));
        }
        for (key, name, bytes) in tools.iter().take(shown) {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>8}  ", human(*bytes)), theme.staged),
                Span::styled(
                    compress(&format!("{name}   {}", crate::tools::remove_display(key)), 52),
                    theme.dim,
                ),
            ]));
        }
        if tools.len() > shown {
            lines.push(Line::from(Span::styled(
                format!("        … and {} more", tools.len() - shown),
                theme.dim,
            )));
        }
        if !app.tools_refused.is_empty() {
            lines.push(Line::from(Span::styled("in use, left alone:", theme.warn)));
            for (name, why) in app.tools_refused.iter().take(2) {
                lines.push(Line::from(Span::styled(
                    format!("  {name} — {why}"),
                    theme.warn,
                )));
            }
        }
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
            if items.is_empty() {
                "\u{2014}   "
            } else if permanent {
                "back to Trash   "
            } else {
                "delete permanently   "
            },
            theme.dim,
        ),
        Span::styled(" esc ", theme.emphasis),
        Span::styled("cancel", theme.dim),
    ]));

    let title = if !tools.is_empty() && !items.is_empty() {
        " delete and remove "
    } else if !tools.is_empty() {
        " remove permanently "
    } else if permanent {
        " permanently delete "
    } else {
        " move to Trash "
    };
    // Anything unrecoverable in the batch makes the whole frame the warning
    // colour, whichever half it came from.
    popup(f, theme, area, title, lines, permanent || !tools.is_empty());
}

/// Taking the trash out.
///
/// Its own screen rather than a line in the confirmation, because it is the
/// opposite operation: nothing here is about to be deleted — it already was —
/// and the only thing that changes is that the space finally arrives and the
/// undo stops working. Both of those are worth saying before the keystroke.
pub fn draw_empty_trash(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let (count, bytes) = app.trash_pending;
    let entries = crate::delete::trashed_entries();
    let batches = crate::delete::batches_still_restorable();

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{count} "), theme.emphasis),
            Span::styled("item(s) fad trashed, ", theme.normal),
            Span::styled(human(bytes), theme.emphasis),
        ]),
        Line::from(Span::styled(
            "removed from the trash for good — this cannot be undone",
            theme.staged,
        )),
    ];
    if batches > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "{batches} batch(es) in the undo history can no longer be put back",
                ),
            theme.warn,
        )));
    }
    // The whole reason the key exists: until now this was the one number fad
    // could not move.
    if let Some((after, before)) = app.after_empty() {
        lines.push(Line::from(vec![
            Span::styled("free space  ", theme.dim),
            Span::styled(human(before), theme.normal),
            Span::styled(" \u{2192} ", theme.dim),
            Span::styled(human(after), theme.emphasis),
        ]));
    }
    lines.push(Line::from(""));

    // Only fad's own entries, and the screen says so rather than leaving the
    // user to wonder what happened to the rest of their trash.
    let mut biggest = entries;
    biggest.sort_by_key(|e| std::cmp::Reverse(e.2));
    for (_, from, bytes) in biggest.iter().take(6) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:>8}  ", human(*bytes)), theme.emphasis),
            Span::styled(compress(&from.display().to_string(), 52), theme.dim),
        ]));
    }
    if biggest.len() > 6 {
        lines.push(Line::from(Span::styled(
            format!("        \u{2026} and {} more", biggest.len() - 6),
            theme.dim,
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "anything else in your trash is left where it is",
        theme.dim,
    )));
    lines.push(Line::from(vec![
        Span::styled(" enter ", theme.mode_badge),
        Span::styled(" empty the trash   ", theme.normal),
        Span::styled(" esc ", theme.emphasis),
        Span::styled("cancel", theme.dim),
    ]));

    popup(f, theme, area, " empty the trash ", lines, true);
}

/// Whether the staged tool bytes actually return to this disk, when they do not
/// all do so.
fn host_note(app: &App) -> Option<String> {
    let report = app.tools.as_ref()?;
    let stuck: Vec<&str> = report
        .sources
        .iter()
        .filter(|s| !s.backing.frees_host_space())
        .filter(|s| app.staged_tools.iter().any(|k| k.source == s.source))
        .map(|s| s.source.label())
        .collect();
    if stuck.is_empty() {
        return None;
    }
    Some(format!(
        // Short enough to survive the popup's width: the clause that matters
        // is the one about free space, and a longer sentence loses it.
        "{} keeps this in a VM disk \u{2014} free space will not move yet",
        stuck.join(" and ")
    ))
}

pub fn draw_progress(f: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let mut lines = Vec::new();
    let mut total = 0usize;
    let mut done = 0usize;

    if let Some(job) = app.job.as_ref() {
        total += job.total;
        done += job.done.len();
        lines.push(Line::from(vec![
            Span::styled(format!("{}/{} ", job.done.len(), job.total), theme.emphasis),
            // The bytes were deleted when the batch was committed; what is
            // happening now is only that they stop being held.
            Span::styled(if app.emptying { "emptied · " } else { "deleted · " }, theme.dim),
            Span::styled(human(job.freed()), theme.emphasis),
            Span::styled(" reclaimed", theme.dim),
        ]));
        for o in job.failures().iter().take(3) {
            let why = o.result.as_ref().err().cloned().unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("  {} — {why}", compress(&o.path.display().to_string(), 40)),
                theme.warn,
            )));
        }
    }

    if let Some(job) = app.tool_job.as_ref() {
        total += job.total;
        done += job.done.len();
        // Once the measurement lands it replaces the estimate outright. The
        // predicted figure was bounds; this one is the tools' own totals before
        // and after, which is the only number here that was not a guess.
        let (amount, label) = match job.measured {
            Some(b) => (human(b), " freed, measured"),
            None if job.is_finished() => (human(job.expected()), " freed, measuring…"),
            None => (human(job.expected()), " freed so far"),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{}/{} ", job.done.len(), job.total), theme.staged),
            Span::styled("removed · ", theme.dim),
            Span::styled(amount, theme.staged),
            Span::styled(label, theme.dim),
        ]));
        for o in job.failures().iter().take(3) {
            let why = o.result.as_ref().err().cloned().unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("  {} — {why}", compress(&o.label, 40)),
                theme.warn,
            )));
        }
        if job.measured.is_some() && !app.stuck_in_vm() {
            // Nothing to caveat: the space is really back.
        } else if job.measured.is_some() {
            lines.push(Line::from(Span::styled(
                "that came back inside the VM disk, not on your own",
                theme.warn,
            )));
        }
    }

    lines.push(progress_bar(done, total, 44));
    lines.push(Line::from(""));

    let finished = app.batch_finished();
    if finished {
        let undoable = app
            .job
            .as_ref()
            .is_some_and(|j| j.disposal == Disposal::Trash && j.failures().len() < j.total);
        lines.push(Line::from(Span::styled(
            if undoable {
                "enter to close · u to undo the deleted files"
            } else {
                "enter to close"
            },
            theme.dim,
        )));
    } else {
        lines.push(Line::from(Span::styled("working…", theme.dim)));
    }

    let title = if app.emptying {
        " emptying the trash "
    } else if app.tool_job.is_some() && app.job.is_some() {
        " deleting and removing "
    } else if app.tool_job.is_some() {
        " removing "
    } else {
        " deleting "
    };
    popup(f, theme, area, title, lines, app.tool_job.is_some());
}

fn progress_bar(done: usize, total: usize, width: usize) -> Line<'static> {
    let filled = (done * width).checked_div(total).unwrap_or(width);
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
