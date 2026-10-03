//! Terminal setup, the event loop, and what each key does.

use std::io;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::app::{App, Heading, Mode, View};
use crate::delete::{self, Disposal};
use crate::tree::flags;
use crate::ui;

/// While the walk is live the tree changes constantly, so we redraw on a timer.
/// Once it settles we block on input instead and use no CPU at all.
const SCAN_TICK: Duration = Duration::from_millis(33);
const IDLE_TICK: Duration = Duration::from_millis(250);

/// What the session left behind.
pub struct Outcome {
    /// The tree, but only if it is worth persisting.
    pub tree: Option<crate::tree::Tree>,
    /// Where the cursor was when the user quit, for `--print-path`.
    pub selected: Option<std::path::PathBuf>,
}

/// Where the UI is drawn.
///
/// Normally standard output. But `--print-path` puts a path on standard output
/// for a shell to read, and the whole point of it is `cd "$(fad --print-path)"`
/// — where the substitution swallows standard output, so drawing the UI there
/// means drawing it into the shell's buffer and showing the user a blank
/// terminal for the length of the session. The interface goes to the terminal
/// device instead and leaves stdout for the one line that is meant to be read
/// by a program.
pub enum Screen {
    Stdout(io::Stdout),
    Tty(std::fs::File),
}

impl io::Write for Screen {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Screen::Stdout(o) => o.write(buf),
            Screen::Tty(f) => f.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Screen::Stdout(o) => o.flush(),
            Screen::Tty(f) => f.flush(),
        }
    }
}

impl Screen {
    /// The terminal device when stdout is wanted for something else, falling
    /// back to stdout when there is no controlling terminal — a session with
    /// neither is not going to run a UI anyway, and failing here would be a
    /// worse error message than whatever comes next.
    fn open(keep_stdout_clean: bool) -> Screen {
        if !keep_stdout_clean {
            return Screen::Stdout(io::stdout());
        }
        match std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
            Ok(f) => Screen::Tty(f),
            Err(_) => Screen::Stdout(io::stdout()),
        }
    }
}

type Term = ratatui::Terminal<ratatui::backend::CrosstermBackend<Screen>>;

pub fn run(mut app: App, keep_stdout_clean: bool) -> io::Result<Outcome> {
    let mouse = app.mouse;
    let mut terminal = enter(mouse, keep_stdout_clean)?;
    let result = event_loop(&mut terminal, &mut app);
    // Restore the terminal first: whatever went wrong, the user should not be
    // left staring at a broken shell.
    leave(&mut terminal, mouse)?;
    result?;
    let selected = app.selected().map(|id| app.tree.path(id));
    Ok(Outcome { tree: app.tree_is_complete().then_some(app.tree), selected })
}

fn enter(mouse: bool, keep_stdout_clean: bool) -> io::Result<Term> {
    enable_raw_mode()?;
    let mut out = Screen::open(keep_stdout_clean);
    execute!(out, EnterAlternateScreen)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    let backend = ratatui::backend::CrosstermBackend::new(out);
    let terminal = ratatui::Terminal::new(backend)?;
    Ok(terminal)
}

fn leave(terminal: &mut Term, mouse: bool) -> io::Result<()> {
    disable_raw_mode()?;
    if mouse {
        execute!(terminal.backend_mut(), DisableMouseCapture)?;
    }
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn event_loop(terminal: &mut Term, app: &mut App) -> io::Result<()> {
    let mut last_draw = Instant::now() - SCAN_TICK;
    loop {
        app.poll_scan();
        app.poll_job();
        app.poll_dupes();
        app.poll_tools();
        app.poll_tool_job();
        app.poll_volume();

        let tick = if app.scanning() { SCAN_TICK } else { IDLE_TICK };
        if last_draw.elapsed() >= tick {
            app.rebuild_rows();
            terminal.draw(|f| ui::draw(f, app))?;
            last_draw = Instant::now();
        }

        if event::poll(tick)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => on_key(app, k),
                Event::Mouse(m) => on_mouse(app, m),
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
        Mode::Search => search_key(app, k),
        Mode::Help => {
            app.mode = Mode::Normal;
            app.mark_dirty();
        }
        Mode::Basket => basket_key(app, k),
        Mode::History => history_key(app, k),
        Mode::Omissions => omissions_key(app, k),
        Mode::Confirm => confirm_key(app, k),
        Mode::EmptyTrash => empty_key(app, k),
        Mode::Deleting => deleting_key(app, k),
        Mode::Normal => normal_key(app, k),
    }
}

/// Clicks and the wheel. A modal owns the screen while it is up, so the mouse
/// does nothing there rather than quietly moving a selection underneath it.
fn on_mouse(app: &mut App, m: MouseEvent) {
    if app.mode != Mode::Normal {
        return;
    }
    match m.kind {
        // Move the cursor rather than the viewport. The detail pane follows the
        // selection, so scrolling the two apart would leave the right-hand pane
        // describing something off screen.
        MouseEventKind::ScrollDown => move_cursor(app, 3),
        MouseEventKind::ScrollUp => move_cursor(app, -3),
        MouseEventKind::Down(MouseButton::Left) => click(app, m.column, m.row),
        _ => return,
    }
    app.mark_dirty();
}

fn click(app: &mut App, column: u16, row: u16) {
    let list = app.tree_list;
    if column < list.x
        || column >= list.x + list.width
        || row < list.y
        || row >= list.y + list.height
    {
        return;
    }
    let Some(i) = (app.offset).checked_add((row - list.y) as usize) else { return };
    if i >= app.rows.len() {
        return;
    }
    app.cursor = i;
    app.status = None;

    // The three columns a row starts with are the three things a click on a row
    // can mean: the stage marker, the indent, and the twisty.
    let r = app.rows[i];
    let x = column - list.x;
    if x == 0 {
        toggle_stage(app);
    } else if x == 1 + 2 * r.depth {
        // Clicking the arrow toggles, rather than stepping in the way `l` does
        // on an already-open row: a second click in the same place undoing the
        // first is the only behaviour a pointer can have.
        let open = match r.header {
            Some(Heading::Category(c)) => app.reclaim_is_open(c),
            Some(Heading::Dupes(g)) => app.dupes_is_open(g),
            Some(Heading::Tool(src, kind)) => app.tools_is_open(src, kind),
            Some(Heading::ToolStatus(_)) => false,
            None => app.expanded.contains(&r.id),
        };
        if open { collapse(app) } else { expand(app) }
    }
}

/// The journal. `u` reaches the top of the stack; this reaches the rest of it.
fn history_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('U') => app.mode = Mode::Normal,
        KeyCode::Char('j') | KeyCode::Down => {
            app.history_cursor =
                (app.history_cursor + 1).min(app.history.len().saturating_sub(1))
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.history_cursor = app.history_cursor.saturating_sub(1)
        }
        KeyCode::Enter | KeyCode::Char('u') => restore_selected(app),
        _ => {}
    }
    app.mark_dirty();
}

fn restore_selected(app: &mut App) {
    if app.history.is_empty() {
        return;
    }
    // By id, not position: the list on screen can be stale by the time the key
    // lands, and an index into it would name a different batch.
    let Some(id) = app.history.get(app.history_cursor).map(|b| b.id) else { return };
    let outcome = delete::undo_batch(id);
    app.refresh_history();
    app.mode = Mode::Normal;
    app.status = Some(match outcome {
        Ok(r) => undo_message(&r),
        Err(e) => e,
    });
}

/// What the scan did not count. Read-only: every line here is a thing to go and
/// do something about outside fad, or a flag to rerun with, so the only action
/// worth offering is taking the path away with you.
fn omissions_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('!') => app.mode = Mode::Normal,
        KeyCode::Char('j') | KeyCode::Down => {
            app.omission_cursor =
                (app.omission_cursor + 1).min(app.omissions.len().saturating_sub(1))
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.omission_cursor = app.omission_cursor.saturating_sub(1)
        }
        KeyCode::Char('g') => app.omission_cursor = 0,
        KeyCode::Char('G') => app.omission_cursor = app.omissions.len().saturating_sub(1),
        KeyCode::Char('y') => {
            let Some(path) = app.omission_at_cursor().map(|o| o.path.clone()) else { return };
            match crate::platform::copy_to_clipboard(&path.to_string_lossy()) {
                Ok(()) => app.status = Some("path copied".into()),
                Err(_) => {
                    app.status =
                        Some(format!("no clipboard \u{2014} {}", crate::platform::clipboard_hint()))
                }
            }
        }
        _ => {}
    }
    app.mark_dirty();
}

fn basket_key(app: &mut App, k: KeyEvent) {
    use crate::app::BasketRow;

    let rows = app.basket_rows();
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('x') => app.mode = Mode::Normal,
        KeyCode::Char('j') | KeyCode::Down => {
            app.basket_cursor = (app.basket_cursor + 1).min(rows.len().saturating_sub(1))
        }
        KeyCode::Char('k') | KeyCode::Up => app.basket_cursor = app.basket_cursor.saturating_sub(1),
        KeyCode::Char('g') => app.basket_cursor = 0,
        KeyCode::Char('G') => app.basket_cursor = rows.len().saturating_sub(1),
        // Unstaging a whole group from its heading is the mirror of `A`, which
        // is how most of a batch this size got staged in the first place.
        KeyCode::Char(' ') | KeyCode::Backspace | KeyCode::Char('d') => {
            match rows.get(app.basket_cursor) {
                Some(BasketRow::Item(id)) => {
                    app.staged.remove(id);
                }
                Some(BasketRow::Group { cat, .. }) => {
                    let cat = *cat;
                    app.staged.retain(|id| app.tree.node(*id).preset != cat);
                }
                Some(BasketRow::ToolItem(key)) => {
                    let key = key.clone();
                    app.staged_tools.remove(&key);
                }
                Some(BasketRow::ToolGroup { .. }) => app.staged_tools.clear(),
                None => {}
            }
            let len = app.basket_rows().len();
            app.basket_cursor = app.basket_cursor.min(len.saturating_sub(1));
            if app.nothing_staged() {
                app.mode = Mode::Normal;
            }
        }
        KeyCode::Char('C') => {
            app.staged.clear();
            app.staged_tools.clear();
            app.mode = Mode::Normal;
            app.status = Some("batch cleared".into());
        }
        KeyCode::Enter | KeyCode::Char('y') => {
            app.review_batch();
            app.mode = Mode::Confirm;
        }
        _ => {}
    }
    app.mark_dirty();
}

fn confirm_key(app: &mut App, k: KeyEvent) {
    match k.code {
        // Back to the basket, not out of the flow entirely: cancelling a
        // confirmation almost always means "let me fix one entry".
        KeyCode::Esc | KeyCode::Char('q') => {
            app.disposal = Disposal::Trash;
            app.refused.clear();
            app.tools_refused.clear();
            app.mode = if app.nothing_staged() { Mode::Normal } else { Mode::Basket };
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

/// Taking the trash out. Uppercase `y` is not required here the way `D` is in
/// the confirmation — the items are already deleted, and this only closes the
/// gap between fad's arithmetic and the volume's.
fn empty_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => app.mode = Mode::Normal,
        KeyCode::Enter | KeyCode::Char('y') => app.empty_trash(),
        _ => {}
    }
    app.mark_dirty();
}

fn deleting_key(app: &mut App, k: KeyEvent) {
    // Both halves, or the modal closes while a removal is still running.
    if app.batch_finished() && matches!(k.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
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

/// Searching the whole tree. Everything here is live: the list is rebuilt on
/// every keystroke, so what is on screen is always what the query says.
fn search_key(app: &mut App, k: KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Esc => {
            app.search.clear();
            app.search_hits.clear();
            app.mode = Mode::Normal;
        }
        KeyCode::Enter => {
            app.mode = Mode::Normal;
            app.status = app.jump_to_hit();
        }
        KeyCode::Down | KeyCode::Char('n') if ctrl => {
            app.search_cursor =
                (app.search_cursor + 1).min(app.search_hits.len().saturating_sub(1))
        }
        KeyCode::Up | KeyCode::Char('p') if ctrl => {
            app.search_cursor = app.search_cursor.saturating_sub(1)
        }
        KeyCode::Down => {
            app.search_cursor =
                (app.search_cursor + 1).min(app.search_hits.len().saturating_sub(1))
        }
        KeyCode::Up => app.search_cursor = app.search_cursor.saturating_sub(1),
        KeyCode::Backspace => {
            app.search.pop();
            app.run_search();
        }
        KeyCode::Char(c) => {
            app.search.push(c);
            app.run_search();
        }
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
        KeyCode::Char('L') => share_storage(app),

        KeyCode::Char('/') => {
            app.mode = Mode::Filter;
            app.filter.clear();
        }
        // The other question: not "narrow what I am looking at" but "where in
        // all of this is the thing called that".
        KeyCode::Char('f') => {
            app.mode = Mode::Search;
            app.search.clear();
            app.run_search();
        }
        KeyCode::Char('s') => {
            app.sort = app.sort.next();
            app.status = Some(format!("sorting by {}", app.sort.label()));
        }
        KeyCode::Char('S') => {
            app.cycle_panel();
            app.status = Some(format!("{} first", app.panel.label()));
        }
        KeyCode::Char('x') => {
            if app.nothing_staged() {
                app.status = Some("nothing staged".into());
            } else {
                app.basket_cursor = 0;
                app.mode = Mode::Basket;
            }
        }
        KeyCode::Char('u') => undo(app),
        KeyCode::Char('U') => {
            app.refresh_history();
            app.history_cursor = 0;
            app.mode = Mode::History;
        }
        KeyCode::Char('E') => empty_trash(app),
        KeyCode::Char('r') => toggle_reclaim(app),
        KeyCode::Char('d') => toggle_dupes(app),
        KeyCode::Char('t') => toggle_tools(app),
        KeyCode::Char('a') => {
            app.age_filter = app.age_filter.next();
            app.status = Some(format!("showing {}", app.age_filter.label()));
        }
        KeyCode::Char('o') => reveal(app),
        KeyCode::Char('e') => open_editor(app),
        KeyCode::Char('y') => copy_path(app),
        KeyCode::Char('i') => ignore_selected(app),
        // The banners say how many; this says which, and what would fix each.
        KeyCode::Char('!') => {
            app.collect_omissions();
            app.mode = Mode::Omissions;
        }
        KeyCode::Char('R') => {
            // In the tools view there is no tree to rescan: R means ask the
            // tools again, which is the only thing here that goes stale.
            if app.tools_view {
                app.tools = None;
                app.start_tool_probe();
                app.status = Some("asking again\u{2026}".into());
            } else {
                rescan(app)
            }
        }
        KeyCode::Char('?') => app.mode = Mode::Help,
        _ => {}
    }
    app.mark_dirty();
}

fn reveal(app: &mut App) {
    if app.tool_at_cursor().is_some() {
        app.status = Some("this is not a file \u{2014} y copies the command that removes it".into());
        return;
    }
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

/// Add the selection to the persistent ignore list. Deliberately the whole
/// path rather than the name: ignoring `Caches` because of one of them would
/// hide every other.
fn ignore_selected(app: &mut App) {
    let Some(id) = app.selected() else { return };
    if id == app.tree.root() {
        app.status = Some("the scan root cannot be ignored".into());
        return;
    }
    let path = app.tree.path(id);
    match app.ignore.add(&path) {
        Ok(file) => {
            // Ignoring something already staged would leave a batch that the
            // review will refuse; drop it now, while the reason is on screen.
            app.staged.remove(&id);
            app.status = Some(format!("ignoring {} \u{2014} edit {}", app.tree.node(id).name, file.display()));
        }
        Err(e) => app.status = Some(format!("could not write the ignore list: {e}")),
    }
}

fn copy_path(app: &mut App) {
    // On a tool row there is no path to copy, and the useful thing to put on
    // the clipboard is the command that would remove it — which is also the
    // whole of what fad offers anyone who would rather not let it do the
    // removing.
    if let Some(r) = app.tool_at_cursor() {
        let cmd = crate::tools::remove_line(&r.key());
        return match crate::platform::copy_to_clipboard(&cmd) {
            Ok(()) => app.status = Some(format!("copied: {cmd}")),
            Err(_) => {
                app.status =
                    Some(format!("no clipboard \u{2014} {}", crate::platform::clipboard_hint()))
            }
        };
    }
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
    let outcome = delete::undo_last();
    app.refresh_history();
    app.status = Some(match outcome {
        Ok(r) => undo_message(&r),
        Err(e) => e,
    });
}

fn undo_message(r: &delete::UndoReport) -> String {
    if r.restored == 0 && r.skipped.is_empty() {
        return "nothing to undo".into();
    }
    let mut msg = format!("restored {} item(s)", r.restored);
    if !r.skipped.is_empty() {
        msg.push_str(&format!(", {} could not be put back", r.skipped.len()));
    }
    msg.push_str(" \u{2014} press R to rescan");
    msg
}

fn move_cursor(app: &mut App, delta: i64) {
    let last = app.rows.len().saturating_sub(1);
    let next = (app.cursor as i64 + delta).clamp(0, last as i64);
    app.cursor = next as usize;
}

fn expand(app: &mut App) {
    // Group headings open and close like directories, and start closed.
    if let Some(h) = app.rows.get(app.cursor).and_then(|r| r.header) {
        let opened = match h {
            Heading::Category(cat) => app.reclaim_open.insert(cat),
            Heading::Dupes(i) => app.dupes_open.insert(i),
            Heading::Tool(src, kind) => app.tools_open.insert((src, kind)),
            // Nothing under it to open: it is the tool saying why it has
            // nothing to say.
            Heading::ToolStatus(_) => false,
        };
        if opened {
            return;
        }
        // Already open: step onto the first item, matching what expanding an
        // open directory does.
        if app.cursor + 1 < app.rows.len() && app.rows[app.cursor + 1].header.is_none() {
            app.cursor += 1;
        }
        return;
    }
    let Some(id) = app.selected() else { return };
    let n = app.tree.node(id);
    if n.flags & flags::IS_DIR == 0 {
        return;
    }
    if n.flags & flags::CLOUD != 0 {
        app.status = Some("cloud folder — rerun with --cloud to scan it".into());
        return;
    }
    if n.flags & flags::UNNAMED != 0 {
        app.status =
            Some("its name is not valid text, so fad cannot open it — ! for the list".into());
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
    if let Some(h) = app.rows.get(app.cursor).and_then(|r| r.header) {
        match h {
            Heading::Category(cat) => app.reclaim_open.remove(&cat),
            Heading::Dupes(i) => app.dupes_open.remove(&i),
            Heading::Tool(src, kind) => app.tools_open.remove(&(src, kind)),
            Heading::ToolStatus(_) => false,
        };
        return;
    }
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
    // A heading carries the id of the first item under it, so `space` here
    // would stage a row the cursor is not on. `A` is the key for a group.
    if let Some(h) = app.rows.get(app.cursor).and_then(|r| r.header) {
        app.status = Some(match h {
            Heading::Category(_) => "A stages the whole category".into(),
            Heading::Dupes(_) => "A stages every copy but the newest".to_string(),
            Heading::Tool(..) => "A stages everything under this heading".into(),
            Heading::ToolStatus(src) => {
                format!("nothing to stage \u{2014} {} said nothing usable", src.program())
            }
        });
        return;
    }
    // A tool row is not a tree node, and the daemon's own answer decides
    // whether it can go at all.
    if let Some(row) = app.rows.get(app.cursor).copied() {
        if row.tool.is_some() {
            let Some(r) = app.tool_of(&row) else { return };
            if let Some(why) = r.blocked.clone() {
                app.status = Some(format!("{} \u{2014} {why}", r.name));
                return;
            }
            let key = r.key();
            app.toggle_tool_stage(key);
            return;
        }
    }
    let Some(id) = app.selected() else { return };
    if id == app.tree.root() {
        app.status = Some("the scan root cannot be deleted".into());
        return;
    }
    // Said here rather than at the confirm screen, which is a long way from
    // the keypress that would have staged it.
    if app.tree.node(id).flags & flags::UNNAMED != 0 {
        app.status = Some("its name is not valid text — fad cannot name it to delete it".into());
        return;
    }
    if !app.staged.remove(&id) {
        app.stage(id);
    }
}

/// Trashing reclaims nothing until the trash goes out, and until now the only
/// thing fad could do about that was print a banner and send the user
/// elsewhere. This closes that loop, and only over what fad itself put there.
fn empty_trash(app: &mut App) {
    app.refresh_history();
    if app.trash_pending.0 == 0 {
        app.status = Some("nothing of fad's is in the trash".into());
        return;
    }
    app.mode = Mode::EmptyTrash;
}

/// The other thing to do about a duplicate: keep every copy and stop paying
/// for all but one of them.
fn share_storage(app: &mut App) {
    if !app.dupe_view {
        app.status = Some("L shares storage between duplicate copies \u{2014} press d first".into());
        return;
    }
    // From a copy as well as from the heading: having walked into a group to
    // look at the paths is exactly when the user decides what to do about it.
    let group = match app.rows.get(app.cursor).and_then(|r| r.header) {
        Some(Heading::Dupes(g)) => Some(g),
        Some(_) => None,
        None => (0..=app.cursor)
            .rev()
            .find_map(|i| match app.rows.get(i).and_then(|r| r.header) {
                Some(Heading::Dupes(g)) => Some(g),
                _ => None,
            }),
    };
    let Some(group) = group else {
        app.status = Some("no duplicate group here".into());
        return;
    };
    app.status = Some(app.clone_group(group));
}

fn toggle_reclaim(app: &mut App) {
    if !app.toggle_view(View::Reclaim) {
        return;
    }
    if app.tree.reclaimable.is_empty() {
        app.status = Some(if app.scanning() {
            "nothing reclaimable found yet — still scanning".into()
        } else {
            "nothing matched the built-in reclaimable rules".into()
        });
    }
}

/// The duplicate view. Hashing is only meaningful once the walk has finished,
/// and only starts when the user asks for it.
fn toggle_dupes(app: &mut App) {
    if !app.toggle_view(View::Dupes) {
        return;
    }
    if app.dupes.is_some() || app.dupe_hunt_running() {
        return;
    }
    if app.scanning() {
        app.status = Some("still scanning \u{2014} duplicates need the whole tree".into());
        return;
    }
    app.start_dupe_hunt();
    app.status = Some("hashing candidates\u{2026}".into());
}

/// The tools view. Nothing is asked of any daemon until this is pressed.
fn toggle_tools(app: &mut App) {
    if !app.toggle_view(View::Tools) {
        return;
    }
    if app.tools.is_some() || app.tool_probe_running() {
        return;
    }
    app.start_tool_probe();
    app.status = Some("asking docker\u{2026}".into());
}

fn stage_children(app: &mut App) {
    // On a category heading, `A` means the whole category. On a duplicate
    // group it means every copy *but one* — staging all of them would delete
    // the file, which is never what "these are duplicates" is asking for.
    if let Some(h) = app.rows.get(app.cursor).and_then(|r| r.header) {
        // On a tool heading it means everything the tool will let go of.
        if let Heading::Tool(src, kind) = h {
            let keys: Vec<crate::tools::ToolKey> = app
                .tool_items(src, kind)
                .into_iter()
                .filter(|k| {
                    app.tools.as_ref().and_then(|r| r.get(k)).is_some_and(|r| r.removable())
                })
                .collect();
            if keys.is_empty() {
                app.status = Some("nothing here can be removed while it is in use".into());
                return;
            }
            let all = keys.iter().all(|k| app.staged_tools.contains(k));
            for k in keys {
                if all { app.staged_tools.remove(&k); } else { app.staged_tools.insert(k); }
            }
            return;
        }
        if matches!(h, Heading::ToolStatus(_)) {
            return;
        }
        let items = match h {
            Heading::Category(cat) => app.reclaim_items(cat),
            Heading::Dupes(i) => app.dupe_items(i).into_iter().skip(1).collect(),
            Heading::Tool(..) | Heading::ToolStatus(_) => unreachable!("handled above"),
        };
        if items.is_empty() {
            return;
        }
        let all = items.iter().all(|id| app.staged.contains(id));
        for id in items {
            if all { app.staged.remove(&id); } else { app.stage(id); }
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
            app.stage(c);
        }
    }
}
