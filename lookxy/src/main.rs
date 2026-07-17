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
mod ui;

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

/// The event loop: render the three-pane layout, poll for input without
/// blocking forever (so `SyncEvent`s get drained every tick), route
/// non-global keys to `ui::handle_key`, and quit on `q`/Ctrl-C.
fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        drain_events(app);

        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    let ctrl_c = k.modifiers.contains(KeyModifiers::CONTROL)
                        && k.code == KeyCode::Char('c');
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
    }
}

/// Drains every pending `SyncEvent` without blocking: status transitions
/// update `app.status`; `FoldersUpdated` reloads the folder list;
/// `MessagesUpdated` reloads the message list only when it names the
/// currently visible folder (an update to some other folder doesn't need to
/// disturb what's on screen); `BodyReady` re-reads the body from the store
/// only when it names the currently open message (`App::open_message`
/// already sent the `FetchBody` that led here); `AttachmentsUpdated` fills
/// in the attachments popup once its metadata fetch lands (see
/// `App::open_attachments_popup`/`reload_attachments`); `AttachmentSaved`
/// reports the saved path (and opens it with the OS handler, if `o` rather
/// than Enter triggered that particular save — see
/// `App::finish_attachment_save`).
fn drain_events(app: &mut App) {
    while let Ok(evt) = app.sync.evt_rx.try_recv() {
        match evt {
            SyncEvent::State(s) => app.status = s,
            SyncEvent::FoldersUpdated => app.reload_folders(),
            SyncEvent::MessagesUpdated { folder_id }
                if app.selected_folder.as_deref() == Some(folder_id.as_str()) =>
            {
                app.reload_messages();
            }
            SyncEvent::BodyReady { id } if app.selected_msg.as_deref() == Some(id.as_str()) => {
                app.reload_body();
            }
            SyncEvent::AttachmentsUpdated { message_id } => app.reload_attachments(&message_id),
            SyncEvent::AttachmentSaved { path } => app.finish_attachment_save(path),
            _ => {}
        }
    }
}
