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
mod datetime;
// The control-surface verb dispatcher (mail read + triage against the live
// App), wired into the run loop below via `ctlcore::serve` and its request
// channel.
mod control;
// The MCP stdio bridge (thin client of a running lookxy's control surface),
// reached via the `--mcp` CLI early-return in `main`.
mod mcp;
// The bundled agent SKILL.md (self-onboarding for the MCP/control surface),
// reached via the `install skill` CLI early-return in `main`.
mod skill;
mod ui;

use std::io;
use std::sync::mpsc::Receiver;
use std::time::Duration;

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
use ratatui_image::picker::Picker;

/// Which of lookxy's CLI entry points a given argument vector selects. Kept
/// as a pure decision separate from what each mode actually does — `--mcp`
/// runs a stdio server and `install skill` writes files, neither of which is
/// practical to exercise in a unit test, so the decision itself is what gets
/// tested (see the `cli_mode` tests below).
#[derive(Debug, PartialEq, Eq)]
enum CliMode {
    /// Run the MCP stdio bridge instead of the TUI (`lookxy --mcp`).
    Mcp,
    /// Install the bundled agent SKILL.md and exit (`lookxy install skill`).
    InstallSkill,
    /// The ordinary TUI mail client.
    Tui,
}

fn cli_mode(args: &[String]) -> CliMode {
    if args.iter().any(|a| a == "--mcp") {
        return CliMode::Mcp;
    }
    if args.first().map(String::as_str) == Some("install")
        && args.get(1).map(String::as_str) == Some("skill")
    {
        return CliMode::InstallSkill;
    }
    CliMode::Tui
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--mcp` and `install skill` are headless entry points unrelated to the
    // TUI mail client — handle them before any terminal setup or sync-engine
    // spawn.
    match cli_mode(&args) {
        CliMode::Mcp => {
            if let Err(e) = mcp::run() {
                eprintln!("mcp: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
        CliMode::InstallSkill => match skill::install() {
            Ok(msg) => {
                println!("{msg}");
                return Ok(());
            }
            Err(e) => {
                eprintln!("install skill: {e}");
                std::process::exit(1);
            }
        },
        CliMode::Tui => {}
    }

    let config = Config::load_from(None);

    let local_appdata = app::lookxy_dir();
    let token_path = local_appdata.join("token.bin");

    // v1 is single-account: one fixed store DB alongside the single token
    // cache, rather than a per-account subdirectory. Resolving the account
    // before sign-in used to guess "default", opening a throwaway DB whose
    // first-run backfill was discarded once the real account was known — a
    // fixed path avoids that. (Per-account DBs are a future multi-account
    // concern; see `store_path_for`.)
    let store_path = app::store_path_for();
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
        Duration::from_secs(config.refresh_secs),
    );
    // `App` keeps its own copy of `token_path` too, so it can re-read the
    // account name for the status bar once a sign-in completes (see
    // `App::reload_account`) — the engine owns writing it, not the UI.
    let mut app = App::new(store, handle, token_path);
    app.threaded = config.threaded;
    app.signature = config.signature.clone();
    app.reminders_notify = config.reminders_notify;
    app.config_path = crate::config::config_file_path();
    app.reload_messages();

    // Bring up the agent control surface. Best-effort: if the config
    // directory can't be resolved or the loopback bind fails, lookxy runs
    // exactly as before, just without a control channel — no panic, no
    // user-visible error. `ctl_server` is held for the whole session so its
    // `Drop` (which removes the discovery file) runs when `main` returns.
    let ctl_instance = control::instance_name();
    let (ctl_server, ctl_rx) = match control::control_dir() {
        Some(dir) => match ctlcore::serve(&dir, &ctl_instance) {
            Ok((srv, rx)) => (Some(srv), Some(rx)),
            Err(_) => (None, None),
        },
        None => (None, None),
    };

    let res = run_tui(&mut app, ctl_rx);
    drop(ctl_server); // remove the discovery file
    res
}

/// Sets up the alternate screen + raw mode, runs the event loop, and tears
/// the terminal back down — even on panic, so a crash never leaves the
/// user's shell in raw mode / the alternate screen.
fn run_tui(app: &mut App, ctl_rx: Option<Receiver<ctlcore::Request>>) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Detect the terminal's graphics capability (kitty/iTerm2/Sixel); fall
    // back to a half-block renderer if the query fails (e.g. a plain
    // console) — same detection docxy uses (`main.rs`'s `run_tui`).
    app.picker =
        Some(Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16))));

    // Restore the terminal even if `run` panics, so the user's shell isn't
    // left in raw mode / the alternate screen.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));

    let res = run(&mut terminal, app, ctl_rx);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

/// The event loop: render the three-pane layout, poll for input without
/// blocking forever (so `SyncEvent`s get drained every tick), route
/// non-global keys to `ui::handle_key`, and quit on `q`/Ctrl-C. The sync
/// engine's own periodic tick (set from `Config::refresh_secs` at spawn
/// time, in `main`) is what keeps folders/messages current on its own — this
/// loop doesn't need to nudge it itself. It also drains any pending agent
/// control requests (`ctl_rx`, `None` when the control server failed to
/// bind) each tick, same cadence as the sync events.
fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    ctl_rx: Option<Receiver<ctlcore::Request>>,
) -> io::Result<()> {
    loop {
        drain_events(app);
        app.check_due_reminders(now_unix_secs());
        drain_ctl(app, ctl_rx.as_ref());

        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    // Any key press acknowledges (and clears) a transient error
                    // notice — same lifecycle as the sync states that clear it.
                    app.error_notice = None;
                    if is_global_quit(app, &k) {
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

/// Whether `k` should quit the whole app. Ctrl-C always quits. `q` quits
/// only when no text-input context is capturing keystrokes (`App::
/// is_capturing_text`) — otherwise `q` is a character the user is typing into
/// the search prompt (searching for "quarterly"/"query" must not quit), so it
/// falls through to `ui::handle_key` instead.
fn is_global_quit(app: &App, k: &event::KeyEvent) -> bool {
    let ctrl_c = k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
    let q_quit = k.code == KeyCode::Char('q') && !app.is_capturing_text();
    ctrl_c || q_quit
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

/// Current wall-clock as UTC epoch seconds — the `now` fed to
/// `App::check_due_reminders` each tick.
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Drains every pending agent control [`ctlcore::Request`] without blocking
/// (a no-op when `ctl_rx` is `None`, i.e. the control server never bound),
/// routing each through `control::dispatch` against the live `App` — the
/// same optimistic `Store` write + `SyncCommand` a UI triage key would make —
/// and replying with its JSON result or error message. The loop's
/// unconditional per-tick redraw is what shows the change; there's no
/// separate "mark dirty" step needed here.
fn drain_ctl(app: &mut App, ctl_rx: Option<&Receiver<ctlcore::Request>>) {
    let Some(rx) = ctl_rx else { return };
    while let Ok(req) = rx.try_recv() {
        match control::dispatch(app, &req.verb, &req.args) {
            Ok(result) => req.reply_ok(result),
            Err(e) => req.reply_err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    #[test]
    fn q_in_the_search_prompt_types_into_the_query_rather_than_quitting() {
        let mut app = App::for_test_with_seeded_store();
        app.start_search();
        let k = KeyEvent::from(KeyCode::Char('q'));
        // The event loop must not treat this `q` as a global quit...
        assert!(!is_global_quit(&app, &k));
        // ...it falls through to the search input handler, appending 'q'.
        ui::handle_key(&mut app, k);
        assert_eq!(app.search.as_ref().unwrap().query, "q");
        assert!(!app.quit);
    }

    #[test]
    fn q_still_quits_when_no_text_input_is_active() {
        let app = App::for_test_with_seeded_store();
        let k = KeyEvent::from(KeyCode::Char('q'));
        assert!(is_global_quit(&app, &k));
    }

    #[test]
    fn ctrl_c_quits_even_while_the_search_prompt_is_capturing_text() {
        let mut app = App::for_test_with_seeded_store();
        app.start_search();
        let k = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(is_global_quit(&app, &k));
    }

    fn strs(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cli_mode_mcp_flag_selects_mcp() {
        assert_eq!(cli_mode(&strs(&["--mcp"])), CliMode::Mcp);
    }

    #[test]
    fn cli_mode_install_skill_selects_install_skill() {
        assert_eq!(
            cli_mode(&strs(&["install", "skill"])),
            CliMode::InstallSkill
        );
    }

    #[test]
    fn cli_mode_defaults_to_tui() {
        assert_eq!(cli_mode(&[]), CliMode::Tui);
        assert_eq!(cli_mode(&strs(&["install"])), CliMode::Tui);
        assert_eq!(cli_mode(&strs(&["skill"])), CliMode::Tui);
        assert_eq!(cli_mode(&strs(&["some-other-flag"])), CliMode::Tui);
    }

    #[test]
    fn cli_mode_mcp_flag_wins_regardless_of_position() {
        // `--mcp` is detected anywhere in argv, not just as args[0].
        assert_eq!(cli_mode(&strs(&["install", "--mcp"])), CliMode::Mcp);
    }
}
