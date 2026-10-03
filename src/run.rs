//! Terminal setup, the event loop, and what each key does.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode,
};

use crate::app::{AgeFilter, App, Heading, Mode, View};
use crate::delete::{self, Disposal};
use crate::tree::flags;
use crate::ui;

/// While the walk is live the tree changes constantly, so we redraw on a timer.
/// Once it settles we block on input instead and use no CPU at all.
const SCAN_TICK: Duration = Duration::from_millis(33);
const IDLE_TICK: Duration = Duration::from_millis(250);

/// How long the cursor has to rest before the detail pane's breakdown catches
/// up with it. Longer than the default macOS key repeat (90ms), so a
/// held key reads as one burst; short enough that letting go and seeing the
/// numbers feel like the same moment.
const SETTLE: Duration = Duration::from_millis(120);

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

/// Whether the terminal is currently ours: raw mode on, alternate screen up.
/// Whoever swaps it back to false does the restoring, so the guard, the panic
/// hook and the signal path can all race for it and the escape codes still go
/// out exactly once — and a panic before startup or after a clean exit writes
/// nothing to a terminal that is already fine.
static TAKEN: AtomicBool = AtomicBool::new(false);
static MOUSE: AtomicBool = AtomicBool::new(false);
static KEEP_STDOUT_CLEAN: AtomicBool = AtomicBool::new(false);

/// The last terminating signal to arrive, or zero. A handler may do almost
/// nothing safely — no allocation, no locks, no writing escape codes through a
/// buffered terminal — so it records the number and the event loop, which
/// wakes at least four times a second, does the restoring on its way out.
static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    SIGNAL.store(sig, Ordering::SeqCst);
}

/// SIGTERM from `kill`, SIGHUP from a closed terminal window, SIGINT from
/// anything that is not the keyboard (raw mode turns ctrl-c into a key). The
/// default action for each kills the process with the terminal still raw and on
/// the alternate screen, which leaves the shell unusable until the user types
/// `reset` blind.
///
/// A caught signal goes back to its default disposition across `exec`, so an
/// editor started from here still gets ctrl-c the normal way.
fn install_signal_handlers() {
    for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        // SAFETY: a zeroed `sigaction` is a valid empty one, and the handler
        // only stores to an atomic, which is async-signal-safe.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            // Restarted, so a signal landing mid-`read` does not surface as an
            // error that ends the session before the loop has seen the flag.
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

/// A terminating signal that has arrived since the last call, if any.
fn take_signal() -> Option<i32> {
    match SIGNAL.swap(0, Ordering::SeqCst) {
        0 => None,
        s => Some(s),
    }
}

/// The release profile aborts on panic, so no destructor runs and the guard
/// below never gets its chance: the terminal would be left raw, reporting mouse
/// movements as garbage, with the panic message drawn onto the alternate screen
/// and then thrown away with it. The hook puts the terminal back first and only
/// then lets the default hook print, so the message lands in the scrollback
/// where it can be read and reported.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_now();
            previous(info);
        }));
    });
}

/// Put the terminal back from wherever we are. Opens its own handle rather than
/// borrowing the session's, which a panicking thread may have been halfway
/// through writing to.
fn restore_now() {
    if !TAKEN.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = Screen::open(KEEP_STDOUT_CLEAN.load(Ordering::SeqCst));
    restore(&mut out, MOUSE.load(Ordering::SeqCst));
}

/// Every step, each attempted whatever the one before it did, and every error
/// ignored: this runs on the way out of a panic or a signal, where there is
/// nobody left to report a failure to, and a terminal with raw mode off but the
/// alternate screen still up is barely better than one with neither undone.
fn restore(out: &mut impl io::Write, mouse: bool) {
    let _ = disable_raw_mode();
    if mouse {
        let _ = execute!(out, DisableMouseCapture);
    }
    let _ = execute!(out, LeaveAlternateScreen, Show);
}

/// The terminal, taken over, and given back on every way out.
///
/// Dropping it is the normal path — an error return, an early `?`, the end of
/// the session. The panic hook covers a panic, which under `panic = "abort"`
/// never unwinds as far as a destructor, and the signal flag covers being
/// killed. All three go through `restore`, and `TAKEN` stops two of them both
/// doing it.
struct Session {
    terminal: Term,
    mouse: bool,
}

impl Session {
    fn enter(mouse: bool, keep_stdout_clean: bool) -> io::Result<Session> {
        install_panic_hook();
        install_signal_handlers();
        MOUSE.store(mouse, Ordering::SeqCst);
        KEEP_STDOUT_CLEAN.store(keep_stdout_clean, Ordering::SeqCst);

        enable_raw_mode()?;
        // From here on a failure has something to undo. Marked before the
        // alternate screen goes up, so a terminal that refuses it is still
        // taken out of raw mode: raw and nothing else is still a broken shell.
        TAKEN.store(true, Ordering::SeqCst);
        let mut out = Screen::open(keep_stdout_clean);
        let entered = execute!(out, EnterAlternateScreen).and_then(|()| {
            if mouse { execute!(out, EnableMouseCapture) } else { Ok(()) }
        });
        if let Err(e) = entered {
            restore_now();
            return Err(e);
        }
        match ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(out)) {
            Ok(terminal) => Ok(Session { terminal, mouse }),
            Err(e) => {
                restore_now();
                Err(e)
            }
        }
    }
}

impl Session {
    /// Hand the terminal to another full-screen program, and take it back.
    ///
    /// Leaves the alternate screen rather than drawing over it, so the child
    /// starts on the user's own screen and whatever it leaves there is not
    /// mixed into ours. `TAKEN` is down for the duration: a panic while the
    /// child runs must not try to restore a terminal that is the child's.
    fn suspend<T>(&mut self, f: impl FnOnce() -> T) -> io::Result<T> {
        TAKEN.store(false, Ordering::SeqCst);
        restore(self.terminal.backend_mut(), self.mouse);
        let out = f();
        enable_raw_mode()?;
        TAKEN.store(true, Ordering::SeqCst);
        execute!(self.terminal.backend_mut(), EnterAlternateScreen, Clear(ClearType::All))?;
        if self.mouse {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
        }
        // Ratatui only ever sends what changed since its last frame, and its
        // last frame is no longer what is on the screen: without this the next
        // draw paints a few changed cells onto a blank page. An empty frame
        // makes its idea of the screen blank too, so the next one is drawn in
        // full. Not `Terminal::clear`, which asks the terminal where its cursor
        // is and ends the session if the answer is slow to come back.
        self.terminal.draw(|_| {})?;
        Ok(out)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if TAKEN.swap(false, Ordering::SeqCst) {
            restore(self.terminal.backend_mut(), self.mouse);
        }
    }
}

pub fn run(mut app: App, keep_stdout_clean: bool) -> io::Result<Outcome> {
    let mut session = Session::enter(app.mouse, keep_stdout_clean)?;
    let result = event_loop(&mut session, &mut app);
    // Restore the terminal first: whatever went wrong, the user should not be
    // left staring at a broken shell.
    drop(session);
    if let Some(sig) = take_signal() {
        // The conventional status for "killed by this signal", which is what a
        // shell or a supervisor checks for. Nothing is saved and nothing is
        // printed: a session that was told to stop has no answer to give.
        std::process::exit(128 + sig);
    }
    result?;
    let selected =
        if app.cancelled { None } else { app.selected().map(|id| app.tree.path(id)) };
    Ok(Outcome { tree: app.tree_is_complete().then_some(app.tree), selected })
}

fn event_loop(session: &mut Session, app: &mut App) -> io::Result<()> {
    let mut last_draw = Instant::now() - SCAN_TICK;
    let mut last_input = Instant::now() - SETTLE;
    loop {
        // Left set for `run` to find: the flag is all a handler could do, and
        // the restoring happens on the way out.
        if SIGNAL.load(Ordering::SeqCst) != 0 {
            return Ok(());
        }

        app.poll_scan();
        app.poll_job();
        app.poll_dupes();
        app.poll_tools();
        app.poll_tool_job();
        app.poll_volume();

        // The burst is over: draw now, with the breakdown it was holding back,
        // rather than leaving the pane short until the next tick.
        let settled = app.settling && last_input.elapsed() >= SETTLE;
        if settled {
            app.settling = false;
        }

        let tick = if app.scanning() { SCAN_TICK } else { IDLE_TICK };
        if settled || last_draw.elapsed() >= tick {
            app.rebuild_rows();
            session.terminal.draw(|f| ui::draw(f, app))?;
            last_draw = Instant::now();
        }

        // Wake for the end of a burst, not just the next tick: idle, the tick
        // is a quarter of a second, and the breakdown would arrive that late.
        let wait = if app.settling { SETTLE.saturating_sub(last_input.elapsed()) } else { tick };
        let ready = match event::poll(wait) {
            Ok(ready) => ready,
            // A signal arriving mid-poll; the check at the top of the loop is
            // what deals with it.
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if ready {
            // Everything already queued, then one frame. Held, a key repeats
            // faster than a frame can be drawn on a big tree, and drawing after
            // each one left the cursor running on after the key came up.
            loop {
                let ev = match event::read() {
                    Ok(ev) => ev,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                    Err(e) => return Err(e),
                };
                let effect = match ev {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        // A key on the heels of the last one is a burst; one on
                        // its own is answered in full, breakdown and all.
                        app.settling = last_input.elapsed() < SETTLE;
                        last_input = Instant::now();
                        on_key(app, k)
                    }
                    Event::Mouse(m) => {
                        on_mouse(app, m);
                        None
                    }
                    Event::Resize(_, _) => {
                        app.mark_dirty();
                        None
                    }
                    _ => None,
                };
                if let Some(Effect::Edit(path)) = effect {
                    edit(session, app, &path)?;
                    break;
                }
                if app.should_quit || !matches!(event::poll(Duration::ZERO), Ok(true)) {
                    break;
                }
            }
            // Respond to input immediately rather than at the next tick.
            app.rebuild_rows();
            session.terminal.draw(|f| ui::draw(f, app))?;
            last_draw = Instant::now();
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

/// What a key asks of the terminal itself, which the key handler cannot do: it
/// sees the app and nothing else, and that is what keeps it testable.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Hand the terminal to the user's editor on this path, and take it back.
    Edit(PathBuf),
}

/// One key, in whatever mode the app is in. Public so the tests can drive the
/// real key handling rather than a copy of it.
pub fn on_key(app: &mut App, k: KeyEvent) -> Option<Effect> {
    // Ctrl-c means "stop what I am doing" in every mode. At the top level that
    // is quitting; inside a prompt or an overlay it is backing out of it —
    // never typing a `c` into the filter or the search — and while a batch is
    // going it is the same as esc there: stop after this item.
    if k.code == KeyCode::Char('c')
        && k.modifiers.contains(KeyModifiers::CONTROL)
        && app.mode != Mode::Normal
    {
        return on_key(app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }
    match app.mode {
        Mode::Filter => filter_key(app, k),
        Mode::Search => search_key(app, k),
        Mode::Help => help_key(app, k),
        Mode::Basket => basket_key(app, k),
        Mode::History => history_key(app, k),
        Mode::Omissions => omissions_key(app, k),
        Mode::Confirm => confirm_key(app, k),
        Mode::EmptyTrash => empty_key(app, k),
        Mode::Deleting => deleting_key(app, k),
        Mode::Normal => return normal_key(app, k),
    }
    None
}

/// Clicks and the wheel. A modal owns the screen while it is up, so the mouse
/// never moves a selection underneath it — but the wheel does scroll a list
/// inside it, which is what anyone turning it over a list expects.
pub fn on_mouse(app: &mut App, m: MouseEvent) {
    if app.mode != Mode::Normal {
        let code = match m.kind {
            MouseEventKind::ScrollDown => KeyCode::Down,
            MouseEventKind::ScrollUp => KeyCode::Up,
            _ => return,
        };
        if matches!(
            app.mode,
            Mode::Basket | Mode::History | Mode::Omissions | Mode::Search | Mode::Help
        ) {
            for _ in 0..3 {
                on_key(app, KeyEvent::new(code, KeyModifiers::NONE));
            }
        }
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
    } else if x as usize == 1 + ui::row_indent(r.depth, list.width as usize) {
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

/// The help overlay scrolls — it is longer than a short terminal — and any key
/// that is not scrolling closes it, as it always did. The draw clamps the
/// offset, since only it knows how many lines the text wrapped to.
fn help_key(app: &mut App, k: KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let scroll = &mut app.ui.help_scroll;
    match k.code {
        KeyCode::Char('j') | KeyCode::Down => *scroll += 1,
        KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
        KeyCode::PageDown => *scroll += 10,
        KeyCode::Char('d') if ctrl => *scroll += 10,
        KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
        KeyCode::Char('u') if ctrl => *scroll = scroll.saturating_sub(10),
        KeyCode::Char('g') | KeyCode::Home => *scroll = 0,
        KeyCode::Char('G') | KeyCode::End => *scroll = usize::MAX,
        _ => app.mode = Mode::Normal,
    }
    app.mark_dirty();
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
        Ok(r) => undo_message(app, &r),
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
        // Enter only. `y` copies a path everywhere else, and it is also what
        // commits on the next screen — so with it here, `y y` from the basket
        // deleted the batch with no screen read in between.
        KeyCode::Enter => app.open_confirm(),
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
    if app.batch_finished() {
        if matches!(k.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) {
            app.finish_job();
        }
    } else if k.code == KeyCode::Esc {
        // Stops after the item in hand rather than closing the modal: what has
        // already gone is gone, and the screen that says so has to stay up.
        app.cancel_deleting();
    }
    app.mark_dirty();
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
        // Any other chord is a command the prompt does not have, not a letter.
        KeyCode::Char(_) if k.modifiers.contains(KeyModifiers::CONTROL) => return,
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
        KeyCode::Char(_) if ctrl => return,
        KeyCode::Char(c) => {
            app.search.push(c);
            app.run_search();
        }
        _ => return,
    }
    app.mark_dirty();
}

fn normal_key(app: &mut App, k: KeyEvent) -> Option<Effect> {
    let mut effect = None;
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    app.status = None;
    // The key after "N staged items will be forgotten" answers that and does
    // nothing else: a stray `space` meant as "no" should not also unstage
    // something.
    if std::mem::take(&mut app.ui.quit_armed) {
        if k.code == KeyCode::Char('q') {
            app.should_quit = true;
        } else if ctrl && k.code == KeyCode::Char('c') {
            app.should_quit = true;
            app.cancelled = true;
        }
        app.mark_dirty();
        return None;
    }
    match k.code {
        KeyCode::Char('q') => request_quit(app),
        // ctrl-c is the way out with nothing chosen: under `--print-path` it
        // prints nothing, so `fad-cd` stays where it was.
        KeyCode::Char('c') if ctrl => {
            request_quit(app);
            app.cancelled = app.should_quit;
        }
        KeyCode::Esc => back_out(app),

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

        // A kept filter comes back into the prompt to be refined, not thrown
        // away: the usual reason to press `/` again is that the first query
        // was nearly right.
        KeyCode::Char('/') => app.mode = Mode::Filter,
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
        KeyCode::Char('e') => effect = editor_target(app).map(Effect::Edit),
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
        KeyCode::Char('?') => {
            app.ui.help_scroll = 0;
            app.mode = Mode::Help;
        }
        _ => {}
    }
    app.mark_dirty();
    effect
}

/// Quit, unless that would throw a batch away without a word. Staging can be
/// ten minutes' work across three views, and `q` and `esc` are both one
/// keystroke from it.
fn request_quit(app: &mut App) {
    let n = app.staged.len() + app.staged_tools.len();
    if n == 0 {
        app.should_quit = true;
        return;
    }
    app.ui.quit_armed = true;
    app.status = Some(format!(
        "{n} staged item{} will be forgotten \u{2014} q again to quit",
        if n == 1 { "" } else { "s" }
    ));
}

/// Esc undoes one layer of what is narrowing the screen, innermost first, and
/// only quits once there is nothing left to undo. It used to quit outright from
/// anywhere, so the key everyone presses to back out of a filter or a view
/// took the whole session with it.
fn back_out(app: &mut App) {
    if !app.filter.is_empty() {
        app.filter.clear();
        app.status = Some("filter cleared".into());
    } else if app.view().is_some() {
        app.show_view(None);
    } else if app.age_filter != AgeFilter::All {
        app.age_filter = AgeFilter::All;
        app.status = Some(format!("showing {}", app.age_filter.label()));
    } else {
        request_quit(app);
    }
}

fn reveal(app: &mut App) {
    if app.tool_at_cursor().is_some() {
        app.status = Some("this is not a file \u{2014} y copies the command that removes it".into());
        return;
    }
    let Some(id) = item_at_cursor(app) else { return };
    let path = app.tree.path(id);
    match crate::platform::reveal(&path) {
        Ok(msg) => app.status = Some(msg.into()),
        Err(e) => app.status = Some(format!("could not open a file manager: {e}")),
    }
}

/// The path `e` would open, if the cursor is on one.
fn editor_target(app: &mut App) -> Option<PathBuf> {
    if app.tool_at_cursor().is_some() {
        app.status = Some("this is not a file \u{2014} y copies the command that removes it".into());
        return None;
    }
    let id = item_at_cursor(app)?;
    Some(app.tree.path(id))
}

/// The tree node under the cursor, for a key that acts on exactly one item.
///
/// A group heading carries the id of the first item under it — which may be in
/// a closed group, or hidden by a filter — so acting on "the selection" there
/// silently acted on something the user could not see. `i` would write it to
/// the ignore file. Every such key comes through here instead, and on a
/// heading it says why it did nothing.
fn item_at_cursor(app: &mut App) -> Option<crate::tree::NodeId> {
    if app.rows.get(app.cursor).is_some_and(|r| r.header.is_some()) {
        app.status = Some("that is a group heading \u{2014} pick an item under it".into());
        return None;
    }
    app.selected()
}

/// The editor the user asked for, the way every other terminal program picks
/// one: `$EDITOR`, then `$VISUAL`, then `vi`, which POSIX promises is there.
fn editor() -> String {
    ["EDITOR", "VISUAL"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".into())
}

/// Run the editor in the foreground, with the terminal handed over to it.
///
/// It used to be spawned detached on top of the running interface, which a
/// terminal editor cannot survive — two programs drawing on one screen and both
/// reading the keyboard. Now the session steps aside for the length of the
/// edit, and comes back with a full redraw.
///
/// Through `sh` rather than exec'd directly, because `$EDITOR` is a command
/// line and not a program name: `code -w` and `emacsclient -t` are both normal
/// values, and only word splitting turns them into something runnable. The path
/// goes in as `$1`, never spliced into the script, so nothing in a filename is
/// ever read as shell.
fn edit(session: &mut Session, app: &mut App, path: &std::path::Path) -> io::Result<()> {
    let editor = editor();
    let ran = session.suspend(|| {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("$EDITOR \"$1\"").arg("sh").arg(path).env("EDITOR", &editor);
        // Under `--print-path` our stdout is a pipe into the shell's command
        // substitution. The editor gets the terminal instead, or its screen
        // would end up in the path the shell is about to `cd` to.
        if KEEP_STDOUT_CLEAN.load(Ordering::SeqCst) {
            let tty = || std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty");
            if let (Ok(i), Ok(o)) = (tty(), tty()) {
                cmd.stdin(i).stdout(o);
            }
        }
        cmd.status()
    })?;
    // A ctrl-c typed at an editor that leaves the terminal cooked reaches the
    // whole foreground group, us included. It was the editor's, not ours.
    let _ = SIGNAL.compare_exchange(libc::SIGINT, 0, Ordering::SeqCst, Ordering::SeqCst);
    app.status = Some(match ran {
        Ok(s) if s.success() => format!("back from {editor}"),
        Ok(s) => match s.code() {
            Some(code) => format!("{editor} exited with status {code}"),
            None => format!("{editor} was killed by a signal"),
        },
        Err(e) => format!("could not run {editor}: {e}"),
    });
    app.mark_dirty();
    Ok(())
}

/// Add the selection to the persistent ignore list. Deliberately the whole
/// path rather than the name: ignoring `Caches` because of one of them would
/// hide every other.
fn ignore_selected(app: &mut App) {
    if app.tool_at_cursor().is_some() {
        app.status = Some("the ignore list is for files and folders \u{2014} this lives inside a tool".into());
        return;
    }
    let Some(id) = item_at_cursor(app) else { return };
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
    let Some(id) = item_at_cursor(app) else { return };
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
        Ok(r) => undo_message(app, &r),
        Err(e) => e,
    });
}

fn undo_message(app: &App, r: &delete::UndoReport) -> String {
    if r.restored == 0 && r.skipped.is_empty() {
        return "nothing to undo".into();
    }
    let mut msg = format!("restored {} item(s)", r.restored);
    if !r.skipped.is_empty() {
        msg.push_str(&format!(", {} could not be put back", r.skipped.len()));
    }
    // In the tools view R asks Docker again and leaves the tree alone, which
    // is exactly the wrong thing after putting files back.
    msg.push_str(if app.tools_view {
        " \u{2014} esc, then R to rescan"
    } else {
        " \u{2014} press R to rescan"
    });
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
    if let Some(row) = app.rows.get(app.cursor).copied()
        && row.tool.is_some()
    {
        let Some(r) = app.tool_of(&row) else { return };
        if let Some(why) = r.blocked.clone() {
            app.status = Some(format!("{} \u{2014} {why}", r.name));
            return;
        }
        let key = r.key();
        app.toggle_tool_stage(key);
        return;
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
        app.status = Some("still scanning \u{2014} the hunt starts when the scan finishes".into());
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
    // Which tools it is asking is the probe's to say; it knows the list.
    app.start_tool_probe();
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
        app.stage_all(items);
        return;
    }
    let Some(id) = app.selected() else { return };
    let kids = app.tree.node(id).children.clone();
    if kids.is_empty() {
        return;
    }
    // All-or-nothing, so a second press undoes the first.
    app.stage_all(kids);
}
