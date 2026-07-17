//! The three-pane Outlook-like layout: folder tree | message list | reading
//! pane, plus a bottom status bar. `draw` composes the four widgets each
//! frame; `handle_key` is where Tab/arrow/j/k/Enter navigation turns into
//! `App` state changes. All panes render from `App`'s cached
//! `folders`/`messages` (see `app.rs`) — never by querying the store mid-draw.

mod folders;
mod message_list;
mod reading;
mod status_bar;

use crate::app::{App, Pane};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};

/// Renders the whole screen: folders (~22%) | message list (~38%) | reading
/// pane (~40%) on top, a 1-row status bar pinned to the bottom.
pub fn draw(f: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(22),
            Constraint::Percentage(38),
            Constraint::Percentage(40),
        ])
        .split(rows[0]);

    folders::draw(f, app, cols[0]);
    message_list::draw(f, app, cols[1]);
    reading::draw(f, app, cols[2]);
    status_bar::draw(f, app, rows[1]);

    // Drawn last (and over the full frame, not just the list column) so the
    // move-folder popup (`v`) sits on top of everything else.
    message_list::draw_move_picker(f, app);
}

/// Moves focus (Tab), moves the selection in the focused pane (↑/↓, j/k),
/// opens/activates the current selection (Enter), or — while the move-folder
/// popup (`v`) is open — routes to its own key handling instead. The sole
/// place keyboard input turns into `App` state changes; `main`'s event loop
/// routes non-global keys (everything but `q`/Ctrl-C) here. Triage keys
/// (`m`/`u`/`f`/`d`/`v`) fall through to `App::on_key_char`/`Del` to
/// `App::delete_selected` — see `app.rs` for what each one does.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    if app.move_picker.is_some() {
        handle_move_picker_key(app, key);
        return;
    }
    match key.code {
        KeyCode::Tab => cycle_focus(app),
        KeyCode::Up | KeyCode::Char('k') => move_selection(app, -1),
        KeyCode::Down | KeyCode::Char('j') => move_selection(app, 1),
        KeyCode::Enter => activate(app),
        KeyCode::Delete => app.delete_selected(),
        KeyCode::Char(c) => app.on_key_char(c),
        _ => {}
    }
}

/// Keys while the move-folder popup is open: ↑/↓/j/k pick a folder, Enter
/// confirms the move, Esc cancels. Nothing else (pane navigation, other
/// triage keys) reaches the app while the popup has focus.
fn handle_move_picker_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_move_picker(),
        KeyCode::Up | KeyCode::Char('k') => app.move_picker_select(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_picker_select(1),
        KeyCode::Enter => app.confirm_move(),
        _ => {}
    }
}

/// `Folders` → `List` → `Reading` → `Folders`.
fn cycle_focus(app: &mut App) {
    app.focus = match app.focus {
        Pane::Folders => Pane::List,
        Pane::List => Pane::Reading,
        Pane::Reading => Pane::Folders,
    };
}

/// Moves the focused pane's selection by `delta` (wrapping). Selecting a
/// different folder reloads its messages (and resets the message selection);
/// the reading pane has nothing to move a selection over.
fn move_selection(app: &mut App, delta: isize) {
    match app.focus {
        Pane::Folders => {
            if let Some(len) = nonzero(app.folders.len()) {
                app.folder_index = wrapped(app.folder_index, delta, len);
                app.selected_folder = Some(app.folders[app.folder_index].id.clone());
                app.msg_index = 0;
                app.reload_messages();
            }
        }
        Pane::List => {
            if let Some(len) = nonzero(app.messages.len()) {
                app.msg_index = wrapped(app.msg_index, delta, len);
            }
        }
        Pane::Reading => {}
    }
}

/// Enter: on `Folders`, move into the message list (matching how the
/// selection already loaded that folder's messages); on `List`, open the
/// highlighted message (sets `selected_msg`) and move focus to the reading
/// pane; `Reading` has nothing further to activate.
fn activate(app: &mut App) {
    match app.focus {
        Pane::Folders => app.focus = Pane::List,
        Pane::List => {
            if let Some(id) = app.messages.get(app.msg_index).map(|m| m.id.clone()) {
                app.open_message(&id);
            }
            app.focus = Pane::Reading;
        }
        Pane::Reading => {}
    }
}

fn nonzero(len: usize) -> Option<usize> {
    (len > 0).then_some(len)
}

/// `idx + delta`, wrapped into `[0, len)`.
fn wrapped(idx: usize, delta: isize, len: usize) -> usize {
    let len = len as isize;
    (((idx as isize + delta) % len + len) % len) as usize
}

/// The border color for a pane, bright when it holds focus.
pub(crate) fn border_style(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

/// Truncates `s` to at most `w` display columns (wide CJK/emoji glyphs count
/// as 2), so subject/sender columns never overrun their pane and align even
/// with mixed-width text.
pub(crate) fn truncate_width(s: &str, w: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::graph::model::{MailFolder, Message, Recipient};
    use mailcore::sync::engine::SyncCommand;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn draws_three_panes_with_folder_names() {
        // Build an App over an in-memory store seeded with folder "Inbox" and one message.
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let app = App::for_test_with_seeded_store();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Inbox"));
    }

    #[test]
    fn tab_cycles_focus_through_all_three_panes() {
        let mut app = App::for_test_with_seeded_store();
        assert_eq!(app.focus, Pane::Folders);
        handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Pane::List);
        handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Reading);
        handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Pane::Folders);
    }

    #[test]
    fn enter_on_list_selects_the_highlighted_message() {
        let mut app = App::for_test_with_seeded_store();
        app.focus = Pane::List;
        assert_eq!(app.selected_msg, None);
        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.selected_msg.as_deref(), Some("m1"));
        assert_eq!(app.focus, Pane::Reading);
    }

    /// Adds a second message to the "inbox" folder seeded by
    /// `for_test_with_seeded_store`, older than "m1" so it sorts second
    /// (newest first) — giving a 2-message list wrap-around tests can
    /// actually exercise (a 1-message list wraps to itself no matter what,
    /// so it can't distinguish a real wrap from a broken no-op).
    fn seed_second_message(app: &mut App) {
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m2".into(),
                    conversation_id: "c1".into(),
                    subject: "Second".into(),
                    from: Recipient {
                        name: "Bob".into(),
                        address: "bob@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-15T09:00:00Z".into(),
                    sent: "2026-07-15T08:00:00Z".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "second preview".into(),
                },
            )
            .expect("seed second message");
        app.reload_messages();
    }

    #[test]
    fn list_selection_wraps_at_both_ends() {
        let mut app = App::for_test_with_seeded_store();
        seed_second_message(&mut app);
        app.focus = Pane::List;
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.msg_index, 0);

        // Up from the first row wraps to the last row.
        handle_key(&mut app, KeyEvent::from(KeyCode::Up));
        assert_eq!(app.msg_index, app.messages.len() - 1);

        // Down from the last row wraps back to the first.
        handle_key(&mut app, KeyEvent::from(KeyCode::Down));
        assert_eq!(app.msg_index, 0);
    }

    #[test]
    fn moving_folder_selection_reloads_visible_messages() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .upsert_folder(&MailFolder {
                id: "sent".into(),
                display_name: "Sent Items".into(),
                parent_id: None,
                total_count: 1,
                unread_count: 0,
                well_known_name: Some("sentitems".into()),
            })
            .expect("seed second folder");
        app.store
            .upsert_message(
                "sent",
                &Message {
                    id: "m2".into(),
                    conversation_id: "c2".into(),
                    subject: "From Sent".into(),
                    from: Recipient {
                        name: "Bob".into(),
                        address: "bob@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-15T09:00:00Z".into(),
                    sent: "2026-07-15T08:00:00Z".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "sent preview".into(),
                },
            )
            .expect("seed message in second folder");
        app.reload_folders();

        // Well-known ordering (Store::folders) puts inbox before sentitems;
        // the seeded selection stays on "inbox" across the reload.
        assert_eq!(app.folders.len(), 2);
        assert_eq!(app.selected_folder.as_deref(), Some("inbox"));
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].id, "m1");

        app.focus = Pane::Folders;
        handle_key(&mut app, KeyEvent::from(KeyCode::Down));

        assert_eq!(app.selected_folder.as_deref(), Some("sent"));
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].id, "m2");
    }

    #[test]
    fn flag_key_toggles_the_highlighted_row_and_sends_set_flag() {
        let mut app = App::for_test_with_seeded_store();
        app.focus = Pane::List;
        assert!(!app.messages[0].is_flagged);

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('f')));

        assert!(app.messages[0].is_flagged);
        let rows = app.store.messages_in_folder("inbox", 50, 0).unwrap();
        assert!(rows[0].is_flagged);
        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(
            last,
            Ok(SyncCommand::SetFlag { flagged: true, .. })
        ));
    }

    #[test]
    fn delete_key_removes_the_row_and_clamps_selection() {
        let mut app = App::for_test_with_seeded_store();
        seed_second_message(&mut app);
        app.focus = Pane::List;
        assert_eq!(app.messages.len(), 2);
        // "m1" (the newest) sorts first; select the last row so the clamp is
        // actually exercised (deleting the only remaining row would trivially
        // land msg_index at 0 either way).
        app.msg_index = 1;
        let doomed_id = app.messages[1].id.clone();

        handle_key(&mut app, KeyEvent::from(KeyCode::Delete));

        assert_eq!(app.messages.len(), 1);
        assert!(app.messages.iter().all(|m| m.id != doomed_id));
        assert_eq!(app.msg_index, 0); // clamped, not left pointing past the end
        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(last, Ok(SyncCommand::Delete { .. })));

        // 'd' does the same thing as the Delete key.
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('d')));
        assert!(app.messages.is_empty());
        assert_eq!(app.msg_index, 0);
    }

    #[test]
    fn move_picker_opens_selects_and_confirms_a_move() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .upsert_folder(&MailFolder {
                id: "archive".into(),
                display_name: "Archive".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("archive".into()),
            })
            .expect("seed archive folder");
        app.focus = Pane::List;

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('v')));
        assert!(app.move_picker.is_some());
        // Well-known order puts "inbox" before "archive"; move down onto it.
        handle_key(&mut app, KeyEvent::from(KeyCode::Down));
        assert_eq!(
            app.move_picker.as_ref().unwrap().folders
                [app.move_picker.as_ref().unwrap().index]
                .id,
            "archive"
        );

        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));

        assert!(app.move_picker.is_none());
        assert!(app.store.messages_in_folder("inbox", 50, 0).unwrap().is_empty());
        assert_eq!(
            app.store.messages_in_folder("archive", 50, 0).unwrap()[0].id,
            "m1"
        );
        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(last, Ok(SyncCommand::Move { .. })));
    }

    #[test]
    fn esc_cancels_the_move_picker_without_moving_anything() {
        let mut app = App::for_test_with_seeded_store();
        app.focus = Pane::List;

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('v')));
        assert!(app.move_picker.is_some());
        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));

        assert!(app.move_picker.is_none());
        assert_eq!(app.store.messages_in_folder("inbox", 50, 0).unwrap().len(), 1);
    }

    #[test]
    fn move_picker_does_not_open_with_nothing_highlighted() {
        // The empty store has no messages, so there's nothing `v` could move
        // — it must be a no-op rather than opening a popup with no target.
        let mut app = App::for_test_with_empty_store();
        app.focus = Pane::List;

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('v')));

        assert!(app.move_picker.is_none());
    }

    #[test]
    fn empty_store_renders_and_handles_keys_without_panicking() {
        let mut app = App::for_test_with_empty_store();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();

        // Drive every navigation and triage key, through every pane (Tab
        // cycles focus in between), with nothing to select. None of this
        // should panic, and nothing should get selected out of thin air —
        // including `v`, which must not open a popup with no message and no
        // folders to populate it from.
        let keys = [
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Tab,
            KeyCode::Down,
            KeyCode::Up,
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Char('m'),
            KeyCode::Char('u'),
            KeyCode::Char('f'),
            KeyCode::Char('d'),
            KeyCode::Delete,
            KeyCode::Char('v'),
            KeyCode::Esc,
        ];
        for code in keys {
            handle_key(&mut app, KeyEvent::from(code));
            term.draw(|f| draw(f, &app)).unwrap();
        }

        assert!(app.selected_folder.is_none());
        assert!(app.selected_msg.is_none());
        assert_eq!(app.folder_index, 0);
        assert_eq!(app.msg_index, 0);
    }
}
