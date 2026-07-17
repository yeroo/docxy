//! `lookxy` — a terminal (TUI) Outlook/Exchange mail client.
//!
//! The mail-client sibling of `docxy`/`xlsxy`/`yppxy`: where those sit on
//! `docxcore`/`gridcore`/`projcore`, this is the TUI shell over the headless
//! `mailcore` engine — a folder tree, a message list, and a reading pane,
//! kept live by a background sync thread talking to Microsoft Graph.
//!
//! This is the crate skeleton: terminal setup/teardown, the `App` state
//! (see `app.rs`), and a minimal run loop that spawns the sync engine,
//! drains its events each tick, and quits on `q`/Ctrl-C. The three-pane
//! layout and its navigation land in a later task.

mod app;

use std::io;
use std::time::Duration;

use app::App;

use mailcore::auth::AuthConfig;
use mailcore::store::Store;
use mailcore::sync::engine::{self as sync_engine, SyncCommand, SyncEvent};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::widgets::Paragraph;

/// How many days of mail history the sync engine backfills on first run.
const BACKFILL_DAYS: i64 = 30;

fn main() -> io::Result<()> {
    let local_appdata = app::lookxy_dir();
    let token_path = local_appdata.join("token.bin");

    // The account isn't known until sign-in completes; reuse whatever a
    // previously cached token names, or fall back to a placeholder store
    // until then.
    let account = mailcore::tokencache::load(&token_path)
        .ok()
        .flatten()
        .map(|t| t.account)
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "default".to_string());
    let store_path = app::store_path_for(&account);
    if let Some(dir) = store_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    let store = Store::open(&store_path).map_err(io::Error::other)?;
    let handle = sync_engine::spawn(store_path, token_path, AuthConfig::default(), BACKFILL_DAYS);
    let mut app = App::new(store, handle);

    run_tui(&mut app)
}

/// Sets up the alternate screen + raw mode, runs the event loop, and tears
/// the terminal back down — even on panic, so a crash never leaves the
/// user's shell in raw mode / the alternate screen.
fn run_tui(app: &mut App) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Restore the terminal even if `run` panics, so the user's shell isn't
    // left in raw mode / the alternate screen.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));

    let res = run(&mut terminal, app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

/// The minimal event loop: draw a placeholder, poll for input without
/// blocking forever (so `SyncEvent`s get drained every tick), and quit on
/// `q`/Ctrl-C.
fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        drain_events(app);

        terminal.draw(|f| {
            f.render_widget(Paragraph::new(placeholder_line(app)), f.area());
        })?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    let ctrl_c = k.modifiers.contains(KeyModifiers::CONTROL)
                        && k.code == KeyCode::Char('c');
                    if ctrl_c || k.code == KeyCode::Char('q') {
                        app.quit = true;
                    }
                }
                _ => {}
            }
        }

        if app.quit {
            let _ = app.sync.cmd_tx.send(SyncCommand::Shutdown);
            return Ok(());
        }
    }
}

/// The single line drawn while the real three-pane layout (Task 13) isn't
/// wired up yet: how many folders are cached locally, the current focus, and
/// the selection — proof that `App`'s fields are already live, not just
/// declared.
fn placeholder_line(app: &App) -> String {
    let folders = app.store.folders().map(|v| v.len()).unwrap_or(0);
    format!(
        "lookxy — {folders} folder(s) cached — focus: {:?} — folder: {} — msg: {} — press q to quit",
        app.focus,
        app.selected_folder.as_deref().unwrap_or("(none)"),
        app.selected_msg.as_deref().unwrap_or("(none)"),
    )
}

/// Drains every pending `SyncEvent` without blocking, updating `app.status`
/// on status transitions. Panes (folders/list/reading) start reacting to the
/// rest of the events in a later task.
fn drain_events(app: &mut App) {
    while let Ok(evt) = app.sync.evt_rx.try_recv() {
        if let SyncEvent::State(s) = evt {
            app.status = s;
        }
    }
}
