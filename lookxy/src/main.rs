//! `lookxy` — a terminal (TUI) Outlook/Exchange mail client.
//!
//! The mail-client sibling of `docxy`/`xlsxy`/`yppxy`: where those sit on
//! `docxcore`/`gridcore`/`projcore`, this is the TUI shell over the headless
//! `mailcore` engine — a folder tree, a message list, and a reading pane,
//! kept live by a background sync thread talking to Microsoft Graph.
//!
//! This is the crate skeleton: terminal setup/teardown, the `App` state
//! (see `app.rs`), and a run loop that spawns the sync engine, drains its
//! events each tick, renders the three-pane layout (`ui::draw`), routes
//! keyboard input to it (`ui::handle_key`), and quits on `q`/Ctrl-C.

mod app;
mod config;
mod ui;

use std::io;
use std::time::{Duration, Instant};

use app::App;
use config::Config;

use mailcore::auth::AuthConfig;
use mailcore::store::Store;
use mailcore::sync::engine::{self as sync_engine, SyncCommand};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

fn main() -> io::Result<()> {
    let config = Config::load_from(None);

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

    let auth_config = AuthConfig {
        client_id: config.client_id.clone(),
        ..AuthConfig::default()
    };
    let store = Store::open(&store_path).map_err(io::Error::other)?;
    let handle = sync_engine::spawn(
        store_path,
        token_path.clone(),
        auth_config,
        config.backfill_days,
    );
    // `App` keeps its own copy of `token_path` too, so it can re-read the
    // account name for the status bar once a sign-in completes (see
    // `App::reload_account`) — the engine owns writing it, not the UI.
    let mut app = App::new(store, handle, token_path);

    run_tui(&mut app, Duration::from_secs(config.refresh_secs))
}

/// Sets up the alternate screen + raw mode, runs the event loop, and tears
/// the terminal back down — even on panic, so a crash never leaves the
/// user's shell in raw mode / the alternate screen.
fn run_tui(app: &mut App, refresh_interval: Duration) -> io::Result<()> {
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

    let res = run(&mut terminal, app, refresh_interval);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

/// The event loop: render the three-pane layout, poll for input without
/// blocking forever (so `SyncEvent`s get drained every tick), route
/// non-global keys to `ui::handle_key`, quit on `q`/Ctrl-C, and — every
/// `refresh_interval` — nudge the sync engine with an explicit
/// `SyncCommand::Refresh` (the `Config::refresh_secs` knob; the engine also
/// ticks on its own fixed internal timer regardless, so this only shortens
/// the effective interval when configured below that default).
fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    refresh_interval: Duration,
) -> io::Result<()> {
    let mut last_refresh = Instant::now();
    loop {
        drain_events(app);

        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    let ctrl_c =
                        k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
                    if ctrl_c || k.code == KeyCode::Char('q') {
                        app.quit = true;
                    } else {
                        ui::handle_key(app, k);
                    }
                }
                _ => {}
            }
        }

        if app.quit {
            let _ = app.sync.cmd_tx.send(SyncCommand::Shutdown);
            return Ok(());
        }

        if last_refresh.elapsed() >= refresh_interval {
            let _ = app.sync.cmd_tx.send(SyncCommand::Refresh);
            last_refresh = Instant::now();
        }
    }
}

/// Drains every pending `SyncEvent` without blocking, handing each to
/// `App::on_sync_event` — which reloads whatever cached state it
/// invalidated (folders/messages/body/attachments), tracks the sync status,
/// and drives the sign-in modal (`SignInRequired`/`SignInStarted`, cleared
/// again on the next successful sync).
fn drain_events(app: &mut App) {
    while let Ok(evt) = app.sync.evt_rx.try_recv() {
        app.on_sync_event(evt);
    }
}
