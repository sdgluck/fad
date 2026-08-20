//! Terminal setup, the event loop, and what each key does.

use std::io;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::app::{App, Mode};
use crate::delete::{self, Disposal};
use crate::tree::flags;
use crate::ui;

/// While the walk is live the tree changes constantly, so we redraw on a timer.
/// Once it settles we block on input instead and use no CPU at all.
const SCAN_TICK: Duration = Duration::from_millis(33);
const IDLE_TICK: Duration = Duration::from_millis(250);

/// Runs the UI and hands back the tree, but only if it is worth persisting.
pub fn run(mut app: App) -> io::Result<Option<crate::tree::Tree>> {
    let mut terminal = enter()?;
    let result = event_loop(&mut terminal, &mut app);
    // Restore the terminal first: whatever went wrong, the user should not be
    // left staring at a broken shell.
    leave(&mut terminal)?;
    result?;
    Ok(app.tree_is_complete().then_some(app.tree))
}

fn enter() -> io::Result<ratatui::DefaultTerminal> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(out);
    let terminal = ratatui::Terminal::new(backend)?;
    Ok(terminal)
}

fn leave(terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut last_draw = Instant::now() - SCAN_TICK;
    loop {
        app.poll_scan();
        app.poll_job();

        let tick = if app.scanning() { SCAN_TICK } else { IDLE_TICK };
        if last_draw.elapsed() >= tick {
            app.rebuild_rows();
            terminal.draw(|f| ui::draw(f, app))?;
            last_draw = Instant::now();
        }

        if event::poll(tick)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => on_key(app, k),
                Event::Resize(_, _) => app.mark_dirty(),
                _ => {}
            }
            // Respond to input immediately rather than at the next tick.
            app.rebuild_rows();
            terminal.draw(|f| ui::draw(f, app))?;
            last_draw = Instant::now();
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn on_key(app: &mut App, k: KeyEvent) {
    match app.mode {
        Mode::Filter => filter_key(app, k),
        Mode::Help => {
            app.mode = Mode::Normal;
            app.mark_dirty();
        }
        Mode::Confirm => confirm_key(app, k),
        Mode::Deleting => deleting_key(app, k),
        Mode::Normal => normal_key(app, k),
    }
}

fn confirm_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.disposal = Disposal::Trash;
            app.refused.clear();
            app.mode = Mode::Normal;
        }
        // Uppercase on purpose: a permanent delete should never be one
        // relaxed keystroke away from a recoverable one.
        KeyCode::Char('D') => {
            app.disposal = match app.disposal {
                Disposal::Trash => Disposal::Permanent,
                Disposal::Permanent => Disposal::Trash,
            };
        }
        KeyCode::Enter | KeyCode::Char('y') => app.commit(),
        _ => {}
    }
    app.mark_dirty();
}

fn deleting_key(app: &mut App, k: KeyEvent) {
    let done = app.job.as_ref().is_some_and(|j| j.is_finished());
    if done && matches!(k.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
        app.finish_job();
    }
}

fn filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc => {
            app.filter.clear();
            app.mode = Mode::Normal;
        }
        KeyCode::Enter => app.mode = Mode::Normal,
        KeyCode::Backspace => {
            app.filter.pop();
        }
        KeyCode::Char(c) => app.filter.push(c),
        _ => return,
    }
    app.mark_dirty();
}

fn normal_key(app: &mut App, k: KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    app.status = None;
    match k.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('c') if ctrl => app.should_quit = true,

        KeyCode::Char('j') | KeyCode::Down => move_cursor(app, 1),
        KeyCode::Char('k') | KeyCode::Up => move_cursor(app, -1),
        KeyCode::Char('d') if ctrl => move_cursor(app, 10),
        KeyCode::Char('u') if ctrl => move_cursor(app, -10),
        KeyCode::Char('g') => app.cursor = 0,
        KeyCode::Char('G') => app.cursor = app.rows.len().saturating_sub(1),

        KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => expand(app),
        KeyCode::Char('h') | KeyCode::Left => collapse(app),

        KeyCode::Char(' ') => toggle_stage(app),
        KeyCode::Char('A') => stage_children(app),

        KeyCode::Char('/') => {
            app.mode = Mode::Filter;
            app.filter.clear();
        }
        KeyCode::Char('s') => {
            app.sort = app.sort.next();
            app.status = Some(format!("sorting by {}", app.sort.label()));
        }
        KeyCode::Char('x') => {
            if app.staged.is_empty() {
                app.status = Some("nothing staged".into());
            } else {
                app.review_batch();
                app.mode = Mode::Confirm;
            }
        }
        KeyCode::Char('u') => undo(app),
        KeyCode::Char('r') => toggle_reclaim(app),
        KeyCode::Char('o') => reveal(app),
        KeyCode::Char('e') => open_editor(app),
        KeyCode::Char('y') => copy_path(app),
        KeyCode::Char('R') => rescan(app),
        KeyCode::Char('?') => app.mode = Mode::Help,
        _ => {}
    }
    app.mark_dirty();
}

fn reveal(app: &mut App) {
    let Some(id) = app.selected() else { return };
    let path = app.tree.path(id);
    match crate::platform::reveal(&path) {
        Ok(msg) => app.status = Some(msg.into()),
        Err(e) => app.status = Some(format!("could not open a file manager: {e}")),
    }
}

fn open_editor(app: &mut App) {
    let Some(id) = app.selected() else { return };
    let path = app.tree.path(id);
    let Some(editor) = std::env::var_os("EDITOR") else {
        app.status = Some("$EDITOR is not set".into());
        return;
    };
    // The TUI owns the terminal; handing it to a full-screen editor and taking
    // it back cleanly is a bigger job than it looks, so open detached instead.
    match std::process::Command::new(&editor).arg(&path).spawn() {
        Ok(_) => app.status = Some(format!("opened in {}", editor.to_string_lossy())),
        Err(e) => app.status = Some(format!("could not run $EDITOR: {e}")),
    }
}

fn copy_path(app: &mut App) {
    let Some(id) = app.selected() else { return };
    let path = app.tree.path(id);
    match crate::platform::copy_to_clipboard(&path.to_string_lossy()) {
        Ok(()) => app.status = Some("path copied".into()),
        Err(_) => {
            app.status = Some(format!("no clipboard \u{2014} {}", crate::platform::clipboard_hint()))
        }
    }
}

fn rescan(app: &mut App) {
    match app.restart_scan() {
        Ok(()) => app.status = Some("rescanning".into()),
        Err(e) => app.status = Some(format!("rescan failed: {e}")),
    }
}

fn undo(app: &mut App) {
    match delete::undo_last() {
        Ok(r) if r.restored == 0 && r.skipped.is_empty() => {
            app.status = Some("nothing to undo".into())
        }
        Ok(r) => {
            let mut msg = format!("restored {} item(s)", r.restored);
            if !r.skipped.is_empty() {
                msg.push_str(&format!(", {} could not be put back", r.skipped.len()));
            }
            msg.push_str(" — press R to rescan");
            app.status = Some(msg);
        }
        Err(e) => app.status = Some(e),
    }
}

fn move_cursor(app: &mut App, delta: i64) {
    let last = app.rows.len().saturating_sub(1);
    let next = (app.cursor as i64 + delta).clamp(0, last as i64);
    app.cursor = next as usize;
}

fn expand(app: &mut App) {
    let Some(id) = app.selected() else { return };
    let n = app.tree.node(id);
    if n.flags & flags::IS_DIR == 0 {
        return;
    }
    if n.flags & flags::CLOUD != 0 {
        app.status = Some("cloud folder — rerun with --cloud to scan it".into());
        return;
    }
    if app.expanded.insert(id) {
        return;
    }
    // Already open: step into the first child instead, so repeated presses
    // walk down the tree the way they do in a file manager.
    if app.cursor + 1 < app.rows.len() && app.rows[app.cursor + 1].depth > app.rows[app.cursor].depth {
        app.cursor += 1;
    }
}

fn collapse(app: &mut App) {
    let Some(id) = app.selected() else { return };
    if app.expanded.remove(&id) {
        return;
    }
    // Already closed: go to the parent, which is the row above at one less depth.
    let depth = app.rows[app.cursor].depth;
    if let Some(i) = (0..app.cursor).rev().find(|i| app.rows[*i].depth < depth) {
        app.cursor = i;
    }
}

fn toggle_stage(app: &mut App) {
    let Some(id) = app.selected() else { return };
    if id == app.tree.root() {
        app.status = Some("the scan root cannot be deleted".into());
        return;
    }
    if !app.staged.remove(&id) {
        app.staged.insert(id);
    }
}

fn toggle_reclaim(app: &mut App) {
    app.reclaim_view = !app.reclaim_view;
    app.cursor = 0;
    app.offset = 0;
    if app.reclaim_view && app.tree.reclaimable.is_empty() {
        app.status = Some(if app.scanning() {
            "nothing reclaimable found yet — still scanning".into()
        } else {
            "nothing matched the built-in reclaimable rules".into()
        });
    }
}

fn stage_children(app: &mut App) {
    // On a category heading, `A` means the whole category.
    if let Some(cat) = app.rows.get(app.cursor).and_then(|r| r.header) {
        let items = app.reclaim_items(cat);
        let all = items.iter().all(|id| app.staged.contains(id));
        for id in items {
            if all { app.staged.remove(&id); } else { app.staged.insert(id); }
        }
        return;
    }
    let Some(id) = app.selected() else { return };
    let kids = app.tree.node(id).children.clone();
    if kids.is_empty() {
        return;
    }
    // All-or-nothing, so a second press undoes the first.
    let all_staged = kids.iter().all(|c| app.staged.contains(c));
    for c in kids {
        if all_staged {
            app.staged.remove(&c);
        } else {
            app.staged.insert(c);
        }
    }
}
