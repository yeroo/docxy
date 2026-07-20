//! The three-pane Outlook-like layout: folder tree | message list | reading
//! pane, plus a bottom status bar. `draw` composes the four widgets each
//! frame; `handle_key` is where Tab/arrow/j/k/Enter navigation turns into
//! `App` state changes. All panes render from `App`'s cached
//! `folders`/`messages` (see `app.rs`) — never by querying the store mid-draw.

mod attachments;
pub(crate) mod calendar;
pub mod categories;
pub mod categorypicker;
pub(crate) mod compose;
pub mod eventform;
pub mod filepicker;
mod folders;
pub mod freebusy;
mod message_list;
pub mod oofform;
mod reading;
mod search;
mod signin;
mod status_bar;

use crate::app::{App, Mode, Pane};

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};

/// Renders the whole screen: folders (~22%) | message list (~38%) | reading
/// pane (~40%) on top, a 1-row status bar pinned to the bottom. While the
/// search prompt (`/`) is open, `search::draw` takes over the message-list
/// column instead of `message_list::draw` — same column, a virtual list of
/// results instead of the selected folder's messages.
///
/// While the compose view (`App::compose`) is open, it takes over the
/// entire frame instead — a full-screen mode, not an overlay over the
/// three panes (unlike the move-folder/confirm/attachments/sign-in popups,
/// which are drawn on top of them, in that order).
pub fn draw(f: &mut Frame, app: &mut App) {
    // Reminder banner: a 1-row strip at the top when reminders are queued; the
    // Mail panes and the calendar lay out against the remaining `area`. The
    // full-frame modal editors (compose/OOF) ignore it — a reminder that fires
    // while one is open simply shows once it's closed.
    let full = f.area();
    let area = if app.reminder_queue.is_empty() {
        full
    } else {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(full);
        draw_reminder_banner(f, app, split[0]);
        split[1]
    };

    // The automatic-replies editor is a full-frame overlay openable from either
    // mode; drawn first (like compose) so it covers the panes/calendar behind it.
    if app.oof_form.is_some() {
        oofform::draw(f, &*app);
        return;
    }
    if app.compose.is_some() {
        compose::draw_compose(f, &*app);
        return;
    }
    if app.mode == Mode::Calendar {
        calendar::draw_calendar(f, &*app, area);
        // The create/edit event form (`c`/`e` — wired in a later task) is an
        // overlay on top of the calendar, not a full-screen mode like
        // compose — same "no-op unless open" shape as `eventform::draw`'s
        // own doc comment describes.
        eventform::draw(f, &*app);
        freebusy::draw(f, &*app);
        // The delete-confirm modal (`x`) is also an overlay on top of the
        // calendar — without this, `app.confirm` could be set (by
        // `App::delete_selected_event`) with nothing on screen to show it,
        // even though `handle_key` now routes Enter/Esc to it correctly.
        message_list::draw_confirm(f, &*app);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(22),
            Constraint::Percentage(38),
            Constraint::Percentage(40),
        ])
        .split(rows[0]);

    folders::draw(f, &*app, cols[0]);
    if app.search.is_some() {
        search::draw(f, &*app, cols[1]);
    } else {
        message_list::draw(f, &*app, cols[1]);
    }
    // The only pane that needs `&mut App`: it records the live viewport
    // height and rendered content-row count each frame (`reading_viewport`/
    // `reading_content_rows`) so scroll can clamp — see `App::reading_scroll_by`.
    reading::draw(f, app, cols[2]);
    status_bar::draw(f, &*app, rows[1]);

    // Drawn last (and over the full frame, not just the list column) so
    // popups sit on top of everything else. The sign-in modal is drawn
    // last of all — it's the one popup that can be showing with no
    // mailbox behind it at all (first run, no token), and it should win
    // over any other popup that somehow got left open.
    message_list::draw_move_picker(f, &*app);
    message_list::draw_confirm(f, &*app);
    attachments::draw(f, &*app);
    categorypicker::draw(f, &*app);
    filepicker::draw(f, &*app);
    // The RSVP prompt can be opened from the mail reader (`D`/`T` on an
    // invite); the Calendar branch already draws it via `draw_calendar`.
    calendar::draw_rsvp_prompt(f, &*app);
    signin::draw(f, &*app);
}

/// Renders the reminder banner (the front queued reminder, plus a `(+N more)`
/// count and an `[Esc to dismiss]` hint) into `area` — a 1-row yellow strip.
fn draw_reminder_banner(f: &mut Frame, app: &App, area: Rect) {
    let front = app.reminder_queue.front().cloned().unwrap_or_default();
    let more = app.reminder_queue.len().saturating_sub(1);
    let extra = if more > 0 {
        format!("  (+{more} more)")
    } else {
        String::new()
    };
    let text = format!("{front}{extra}   [Esc to dismiss]");
    let para = ratatui::widgets::Paragraph::new(text)
        .style(Style::default().fg(Color::Black).bg(Color::Yellow));
    f.render_widget(para, area);
}

/// Moves focus (Tab), moves the selection in the focused pane (↑/↓, j/k),
/// opens/activates the current selection (Enter), or — while the move-folder
/// popup (`v`), the attachments popup (`a`), or the search prompt (`/`) is
/// open — routes to that one's own key handling instead (checked in that
/// order; only one can be open at a time in practice). The sole place
/// keyboard input turns into `App` state changes; `main`'s event loop routes
/// non-global keys (everything but `q`/Ctrl-C) here. Triage keys
/// (`m`/`u`/`f`/`d`/`v`/`a`/`/`) fall through to `App::on_key_char`/`Del` to
/// `App::delete_selected` — see `app.rs` for what each one does.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    // Checked before every other popup: while sign-in is required (or
    // already under way), there's nothing else the rest of the UI can
    // usefully do without a token.
    if app.signin_modal.is_some() {
        handle_signin_key(app, key);
        return;
    }
    // The automatic-replies editor (opened by `O`) gets first crack at every
    // key while open — ahead of the panes/compose, though sign-in still wins.
    if app.oof_form.is_some() {
        oofform::handle_key(app, key);
        return;
    }
    // The RSVP prompt (calendar a/d/t or mail-reader D/T) captures every key
    // while open, in both modes.
    if app.rsvp_prompt.is_some() {
        calendar::handle_rsvp_prompt_key(app, key);
        return;
    }
    // The free/busy overlay (Ctrl-B in the event form) captures keys while open.
    if app.free_busy.is_some() {
        freebusy::handle_key(app, key);
        return;
    }
    // The category picker (opened by `l`/`L`) gets keys ahead of the panes.
    if app.category_picker.is_some() {
        categorypicker::handle_key(app, key);
        return;
    }
    // The file picker (opened over the composer to choose an attachment) —
    // checked ahead of the compose view itself, so the picker (drawn on top
    // of it) gets keys first while both are notionally open.
    if app.file_picker.is_some() {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(fp) = app.file_picker.as_mut() {
                    fp.move_selection(-1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(fp) = app.file_picker.as_mut() {
                    fp.move_selection(1);
                }
            }
            KeyCode::Enter => app.file_picker_enter(),
            KeyCode::Esc => app.file_picker = None,
            _ => {}
        }
        return;
    }
    // The compose view is a full-screen mode, not a popup over the normal
    // panes — checked next (still ahead of the popups below), since none
    // of them can meaningfully be open at the same time as it in practice.
    if app.compose.is_some() {
        compose::handle_key(app, key);
        // `compose::handle_key` only *records* a Send/Save/Discard request
        // on `app.compose_action` (Ctrl-Enter/Esc/Ctrl-D) — this is what
        // actually carries it out (serialize + store + `SyncCommand`) and
        // closes the composer. Run every keystroke, not just those three:
        // it's a no-op whenever nothing was requested.
        app.apply_compose_action();
        return;
    }
    // The create/edit event form is an overlay over the calendar, not a
    // full-screen mode like compose — but it still needs first crack at
    // every key while it's open, same as `file_picker` gets ahead of the
    // compose view it sits on top of. Checked ahead of `calendar::handle_key`
    // so `c`/`e`/navigation underneath it can't leak through while the form
    // has focus.
    if app.event_form.is_some() {
        eventform::handle_key(app, key);
        return;
    }
    // Checked ahead of the Calendar-mode short-circuit below: the confirm
    // modal (opened by `x` in Calendar mode, or a whole-thread delete/move in
    // Mail mode) must win over both modes' own key handling, or its Enter/Esc
    // leak through to whatever's underneath instead of confirming/cancelling
    // — see `draw`'s matching `draw_confirm` call in the Calendar branch.
    if app.confirm.is_some() {
        match key.code {
            KeyCode::Enter => app.confirm_yes(),
            KeyCode::Esc => app.cancel_confirm(),
            _ => {}
        }
        return;
    }
    // Esc dismisses the front reminder banner (after the overlay handlers, so
    // an open overlay keeps Esc priority; ahead of the mode/pane handling so
    // it isn't swallowed). Works in both Mail and Calendar mode — but NOT while
    // a text field is capturing keystrokes (search prompt, RSVP comment), where
    // Esc means "cancel that field"; the reminder stays queued for a later Esc.
    if key.code == KeyCode::Esc && !app.reminder_queue.is_empty() && !app.is_capturing_text() {
        app.dismiss_reminder();
        return;
    }
    // Esc in the Mail folder view clears an active category filter (`L`) —
    // checked ahead of the pane handlers so it isn't swallowed. Only when a
    // filter is set and no text prompt is capturing Esc.
    if key.code == KeyCode::Esc
        && app.mode == Mode::Mail
        && app.category_filter.is_some()
        && app.search.is_none()
    {
        app.clear_category_filter();
        return;
    }
    if app.mode == Mode::Calendar {
        calendar::handle_key(app, key);
        return;
    }
    if app.move_picker.is_some() {
        handle_move_picker_key(app, key);
        return;
    }
    if app.attachments.is_some() {
        handle_attachments_key(app, key);
        return;
    }
    if app.search.is_some() {
        handle_search_key(app, key);
        return;
    }
    match key.code {
        KeyCode::Tab => cycle_focus(app),
        // Reading-focused vertical keys scroll the reader instead of moving
        // a selection (`move_selection`'s `Pane::Reading` arm is a no-op —
        // there's nothing there to select over). These guarded arms must
        // precede the unguarded ones below for the same keys.
        KeyCode::Char('k') | KeyCode::Up if app.focus == Pane::Reading => app.reading_scroll_by(-1),
        KeyCode::Char('j') | KeyCode::Down if app.focus == Pane::Reading => {
            app.reading_scroll_by(1)
        }
        KeyCode::PageUp if app.focus == Pane::Reading => app.reading_scroll_page(-1),
        KeyCode::PageDown if app.focus == Pane::Reading => app.reading_scroll_page(1),
        KeyCode::Home if app.focus == Pane::Reading => app.reading_scroll_home(),
        KeyCode::End if app.focus == Pane::Reading => app.reading_scroll_end(),
        KeyCode::Up | KeyCode::Char('k') => move_selection(app, -1),
        KeyCode::Down | KeyCode::Char('j') => move_selection(app, 1),
        KeyCode::Enter => activate(app),
        KeyCode::Delete => app.delete_selected(),
        // Checked ahead of the general `on_key_char` catch-all: `g` isn't
        // claimed by any existing triage key (unlike the brief's own bare
        // `f`, already taken by flag-toggle — see `App::compose_forward`'s
        // doc comment for that precedent), so it's free for the
        // mail↔calendar toggle.
        KeyCode::Char('g') => app.toggle_mode(),
        KeyCode::Char(c) => app.on_key_char(c),
        _ => {}
    }
}

/// Keys while the sign-in modal is open: Enter drives `App::on_key_enter`
/// (sends `SyncCommand::SignIn` from the `Required` state; a no-op once
/// `Started`, since the browser is already open). Nothing else is handled —
/// there's no folder tree or message list to navigate without a token yet,
/// and no way to cancel out of a mandatory sign-in.
fn handle_signin_key(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Enter {
        app.on_key_enter();
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

/// Keys while the attachments popup is open: ↑/↓/j/k pick an attachment,
/// Enter saves it to Downloads, `o` saves then opens it, Esc closes the
/// popup without saving anything.
fn handle_attachments_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_attachments_popup(),
        KeyCode::Up | KeyCode::Char('k') => app.attachments_select(-1),
        KeyCode::Down | KeyCode::Char('j') => app.attachments_select(1),
        KeyCode::Enter => app.save_attachment(),
        KeyCode::Char('o') => app.save_and_open_attachment(),
        _ => {}
    }
}

/// Keys while the search prompt is open. Every printable character types
/// into the query (so a query containing `j`/`k` still works — unlike the
/// move/attachments popups, this one can't reserve those letters for
/// navigation); ↑/↓ move the selection within the results instead. Enter
/// (re-)runs the search, Backspace edits the query, Esc closes the prompt
/// and restores the normal folder view.
fn handle_search_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_search(),
        KeyCode::Enter => app.submit_search(),
        KeyCode::Backspace => app.backspace_query(),
        KeyCode::Up => app.move_search_selection(-1),
        KeyCode::Down => app.move_search_selection(1),
        KeyCode::Char(c) => app.type_query(&c.to_string()),
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
            if app.threaded_active() {
                app.move_thread_selection(delta);
            } else if let Some(len) = nonzero(app.messages.len()) {
                app.msg_index = wrapped(app.msg_index, delta, len);
            }
        }
        Pane::Reading => {}
    }
}

/// Enter: on `Folders`, move into the message list (matching how the
/// selection already loaded that folder's messages); on `List`, activate the
/// highlighted message — a draft (`MessageRow::is_draft`, whether it's
/// sitting in the real Drafts folder or a not-yet-synced sentinel one, see
/// `Store::drafts_folder_id`) opens straight into the composer
/// (`App::open_draft`) instead of the reading pane, since there's nothing
/// useful to read-only-render for something still being written; anything
/// else opens normally (sets `selected_msg`) and moves focus to the reading
/// pane. `Reading` has nothing further to activate.
fn activate(app: &mut App) {
    match app.focus {
        Pane::Folders => app.focus = Pane::List,
        Pane::List => {
            if app.threaded_active() {
                app.activate_thread_row();
            } else if let Some(msg) = app.messages.get(app.msg_index) {
                let id = msg.id.clone();
                if msg.is_draft {
                    app.open_draft(&id);
                } else {
                    app.open_message(&id);
                    app.focus = Pane::Reading;
                }
            }
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

/// A `percent_x` × `percent_y` rectangle centered within `r` — the overlay
/// placement shared by every popup (`message_list::draw_move_picker`,
/// `attachments::draw`).
pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
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
    use crate::ui::compose::{Compose, ComposeField};
    use editcore::ops::Editor;
    use mailcore::compose_html;
    use mailcore::graph::model::{MailFolder, Message, Recipient};
    use mailcore::sync::engine::SyncCommand;
    use ratatui::crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn reminder_banner_renders_front_and_more_count() {
        let mut app = App::for_test_with_seeded_store();
        app.reminder_queue
            .push_back("⏰ Standup starts in 5 min (09:00)".into());
        app.reminder_queue
            .push_back("⏰ Review starts in 8 min (09:03)".into());
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Standup starts in 5 min"));
        assert!(text.contains("+1 more"));
        assert!(text.contains("Esc"));
    }

    #[test]
    fn esc_dismisses_reminder_banner() {
        let mut app = App::for_test_with_seeded_store();
        app.reminder_queue.push_back("⏰ Standup".into());
        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
        assert!(app.reminder_queue.is_empty());
    }

    #[test]
    fn draws_three_panes_with_folder_names() {
        // Build an App over an in-memory store seeded with folder "Inbox" and one message.
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut app = App::for_test_with_seeded_store();
        term.draw(|f| draw(f, &mut app)).unwrap();
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
                    is_draft: false,
                    is_meeting_request: false,
                    categories: Vec::new(),
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
                    is_draft: false,
                    is_meeting_request: false,
                    categories: Vec::new(),
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
            app.move_picker.as_ref().unwrap().folders[app.move_picker.as_ref().unwrap().index].id,
            "archive"
        );

        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));

        assert!(app.move_picker.is_none());
        assert!(
            app.store
                .messages_in_folder("inbox", 50, 0)
                .unwrap()
                .is_empty()
        );
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
        assert_eq!(
            app.store.messages_in_folder("inbox", 50, 0).unwrap().len(),
            1
        );
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
    fn slash_opens_search_and_letters_type_into_the_query_not_navigation() {
        // 'j'/'k' navigate the list pane normally, but inside the search
        // prompt they must be ordinary query characters — otherwise no one
        // could ever search for a word containing either letter.
        let mut app = App::for_test_with_seeded_store();
        app.focus = Pane::List;

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('/')));
        assert!(app.search.is_some());

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('j')));
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(app.search.as_ref().unwrap().query, "jk");

        handle_key(&mut app, KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.search.as_ref().unwrap().query, "j");

        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
        assert!(app.search.is_none());
    }

    #[test]
    fn search_enter_runs_the_query_and_esc_restores_the_folder_view() {
        let mut app = App::for_test_with_seeded_store();
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('/')));
        for c in "Hello".chars() {
            handle_key(&mut app, KeyEvent::from(KeyCode::Char(c)));
        }
        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));

        assert_eq!(app.visible_message_count(), 1);

        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));

        assert!(app.search.is_none());
        assert_eq!(app.visible_message_count(), app.messages.len());
    }

    #[test]
    fn attachments_popup_enter_sends_save_attachment_and_esc_closes_it() {
        use mailcore::graph::model::{AttachmentKind, AttachmentMeta};

        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "f.txt".into(),
                    content_type: "text/plain".into(),
                    size: 5,
                    is_inline: false,
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
                }],
            )
            .expect("seed attachment");
        app.focus = Pane::List;

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('a')));
        assert!(app.attachments.is_some());

        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));
        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(last, Ok(SyncCommand::SaveAttachment { .. })));

        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
        assert!(app.attachments.is_none());
    }

    #[test]
    fn a_key_fetches_attachment_metadata_when_graph_has_some_but_none_are_stored_yet() {
        use mailcore::graph::model::{Message, Recipient};

        let mut app = App::for_test_with_seeded_store();
        // "m1" has `has_attachments = true` per Graph but no local rows —
        // `Store::put_attachments` is otherwise only ever written by the
        // sync engine's `FetchAttachments` handler, never by the initial
        // message backfill, so this is the realistic "just synced, never
        // opened the popup before" state.
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m1".into(),
                    conversation_id: "c1".into(),
                    subject: "Hello".into(),
                    from: Recipient {
                        name: "Alice".into(),
                        address: "alice@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-16T10:00:00Z".into(),
                    sent: "2026-07-16T09:00:00Z".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: true,
                    importance: "normal".into(),
                    preview: "hi there".into(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: Vec::new(),
                },
            )
            .expect("update message to has_attachments=true");
        app.reload_messages();
        app.focus = Pane::List;

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('a')));

        let popup = app
            .attachments
            .as_ref()
            .expect("popup opens in a loading state");
        assert!(popup.loading);
        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(
            last,
            Ok(SyncCommand::FetchAttachments { message_id }) if message_id == "m1"
        ));
    }

    #[test]
    fn empty_store_renders_and_handles_keys_without_panicking() {
        let mut app = App::for_test_with_empty_store();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();

        // Drive every navigation and triage key, through every pane (Tab
        // cycles focus in between), with nothing to select. None of this
        // should panic, and nothing should get selected out of thin air —
        // including `v` and `a`, which must not open a popup with no
        // message (and, for `v`, no folders) to populate it from. `/` DOES
        // open on an empty store (there's nothing wrong with searching an
        // empty mailbox), so its Enter/Esc are driven too, over an empty
        // results list.
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
            KeyCode::Char('a'),
            KeyCode::Esc,
            KeyCode::Char('/'),
            KeyCode::Char('x'),
            KeyCode::Backspace,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Esc,
        ];
        for code in keys {
            handle_key(&mut app, KeyEvent::from(code));
            term.draw(|f| draw(f, &mut app)).unwrap();
        }

        assert!(app.selected_folder.is_none());
        assert!(app.selected_msg.is_none());
        assert!(app.attachments.is_none());
        assert!(app.search.is_none());
        assert_eq!(app.folder_index, 0);
        assert_eq!(app.msg_index, 0);
    }

    // --- Drafts resume + compose send/save/discard wiring ------------------

    #[test]
    fn enter_on_a_draft_message_opens_the_composer_loaded_from_the_store() {
        let mut app = App::for_test_with_seeded_store();
        let id = app
            .store
            .create_local_draft("Draft Subj", "bob@x", "carol@x", "<p>Body text</p>")
            .unwrap();
        let (row, _) = app.store.draft(&id).unwrap().unwrap();
        app.selected_folder = Some(row.folder_id.clone());
        app.reload_messages();
        app.focus = Pane::List;
        app.msg_index = 0;
        assert_eq!(app.messages.len(), 1);
        assert!(app.messages[0].is_draft);

        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));

        let compose = app.compose.as_ref().expect("compose should be open");
        assert_eq!(compose.subject, "Draft Subj");
        assert_eq!(compose.to, "bob@x");
        assert_eq!(compose.cc, "carol@x");
        assert_eq!(compose.editor.text.plain(), "Body text");
        assert_eq!(compose.draft_id, id);
        // Must not also fall through to the normal reading-pane open path.
        assert!(app.selected_msg.is_none());
    }

    /// A compose session over the seeded draft "d1", body "old" — the shared
    /// setup `ctrl_enter_...`/`esc_...`/`ctrl_d_...` below each start from.
    fn seeded_compose_session(app: &mut App) -> String {
        let id = app
            .store
            .create_local_draft("Subj", "a@x", "", "<p>old</p>")
            .unwrap();
        app.compose = Some(Compose {
            to: "a@x".into(),
            cc: "".into(),
            bcc: "".into(),
            subject: "Subj".into(),
            editor: Editor::from(compose_html::from_html("<p>new body</p>")),
            focus: ComposeField::Body,
            draft_id: id.clone(),
            autocomplete: None,
            attachments: Vec::new(),
        });
        id
    }

    #[test]
    fn ctrl_enter_in_compose_saves_the_body_sends_send_draft_and_closes_the_composer() {
        let mut app = App::for_test_with_seeded_store();
        let id = seeded_compose_session(&mut app);

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
        );

        assert!(app.compose.is_none());
        let (_, body) = app.store.draft(&id).unwrap().unwrap();
        assert!(body.content.contains("new body"));
        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(last, Ok(SyncCommand::SendDraft { id: sent }) if sent == id));
    }

    #[test]
    fn esc_in_compose_saves_the_body_sends_save_draft_and_closes_the_composer() {
        let mut app = App::for_test_with_seeded_store();
        let id = seeded_compose_session(&mut app);

        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));

        assert!(app.compose.is_none());
        let (_, body) = app.store.draft(&id).unwrap().unwrap();
        assert!(body.content.contains("new body"));
        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(last, Ok(SyncCommand::SaveDraft { id: saved }) if saved == id));
    }

    #[test]
    fn ctrl_d_in_compose_discards_and_sends_no_command() {
        let mut app = App::for_test_with_seeded_store();
        let id = seeded_compose_session(&mut app);

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        );

        assert!(app.compose.is_none());
        // The store's copy of the draft is untouched — discard never wrote
        // the in-progress body back.
        let (_, body) = app.store.draft(&id).unwrap().unwrap();
        assert!(body.content.contains("old"));
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());
    }

    // --- Event form: c/e bindings + routing --------------------------------

    #[test]
    fn c_opens_the_event_form_in_calendar_mode_and_routes_keys_to_it_ahead_of_calendar() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = Mode::Calendar;
        assert!(app.event_form.is_none());

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('c')));
        assert!(app.event_form.is_some());

        // While the form is open, a letter that's otherwise a calendar key
        // ('t' = RSVP tentative) must type into the focused Title field
        // instead of leaking through to `calendar::handle_key` — proof the
        // form is routed to ahead of the calendar's own key handling.
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('t')));
        assert_eq!(app.event_form.as_ref().unwrap().title, "t");
        assert!(app.rsvp_prompt.is_none());

        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
        assert!(app.event_form.is_none());
    }

    /// A UTC ISO-8601 timestamp `offset_secs` from the real "now" — same
    /// "anchor at the actual clock" shape as `app::tests::seeded_event`
    /// (private to that module, so kept as its own tiny copy here) — so a
    /// seeded event's start/end always falls inside `agenda_window()`
    /// regardless of what day the test suite happens to run on.
    fn iso_from_now(offset_secs: i64) -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            + offset_secs;
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (y, m, d) = crate::ui::calendar::civil_from_days(days);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    #[test]
    fn e_opens_the_event_form_prefilled_for_the_highlighted_event() {
        use mailcore::store::NewEvent;

        let mut app = App::for_test_with_seeded_store();
        app.mode = Mode::Calendar;
        app.store
            .upsert_event(&NewEvent {
                id: "e1".into(),
                subject: "Standup".into(),
                start_utc: iso_from_now(86_400),
                end_utc: iso_from_now(86_400 + 1_800),
                is_all_day: false,
                location: "".into(),
                organizer_name: "Boss".into(),
                organizer_addr: "boss@example.com".into(),
                response_status: "accepted".into(),
                series_master_id: None,
                body_preview: "".into(),
                web_link: "".into(),
                last_modified: "2020-01-01T00:00:00Z".into(),
                body_html: "".into(),
                reminder_minutes: 0,
                is_reminder_on: false,
            })
            .unwrap();
        app.reload_agenda();
        app.selected_event = Some("e1".into());

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('e')));

        let f = app.event_form.as_ref().expect("form opens");
        assert_eq!(f.editing_id.as_deref(), Some("e1"));
        assert_eq!(f.title, "Standup");
    }

    // --- Confirm modal routing in Calendar mode (final-review C1 fix) ------

    /// Regression test for the merge-blocker where `handle_key` returned from
    /// the `Mode::Calendar` branch before ever reaching the `app.confirm`
    /// check: `x` set `app.confirm`, but the following Enter was routed to
    /// `calendar::handle_key` instead, which treats Enter as
    /// `open_selected_event` — so the delete could never actually be
    /// confirmed. This drives the exact same two keystrokes a user would
    /// press and must FAIL before the routing fix (the event would still
    /// exist and no `DeleteEvent` would have been enqueued).
    #[test]
    fn deleting_an_event_through_handle_key_in_calendar_mode() {
        use mailcore::store::NewEvent;

        let mut app = App::for_test_with_seeded_store();
        app.mode = Mode::Calendar;
        app.store
            .upsert_event(&NewEvent {
                id: "e1".into(),
                subject: "Standup".into(),
                start_utc: iso_from_now(86_400),
                end_utc: iso_from_now(86_400 + 1_800),
                is_all_day: false,
                location: "".into(),
                organizer_name: "Boss".into(),
                organizer_addr: "boss@example.com".into(),
                response_status: "accepted".into(),
                series_master_id: None,
                body_preview: "".into(),
                web_link: "".into(),
                last_modified: "2020-01-01T00:00:00Z".into(),
                body_html: "".into(),
                reminder_minutes: 0,
                is_reminder_on: false,
            })
            .unwrap();
        app.reload_agenda();
        app.selected_event = Some("e1".into());

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('x')));
        assert!(
            app.confirm.is_some(),
            "'x' should open the delete-confirm modal"
        );

        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));

        assert!(
            app.confirm.is_none(),
            "Enter should have confirmed and closed the modal, not opened the event"
        );
        assert!(
            app.store.event_for_send("e1").unwrap().is_none(),
            "the event should have been deleted from the store"
        );
        let cmd = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(cmd, Ok(SyncCommand::DeleteEvent { .. })));
    }

    /// Regression test for the other half of the same merge-blocker: `draw`
    /// took the Calendar-mode branch and returned before it would call
    /// `message_list::draw_confirm`, so the modal — even once `handle_key`
    /// routed to it correctly — never actually rendered over the calendar.
    #[test]
    fn confirm_modal_renders_over_the_calendar() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = Mode::Calendar;
        app.confirm = Some(crate::app::ConfirmModal {
            prompt: "Delete event 'Standup'?".into(),
            action: crate::app::ConfirmAction::DeleteEvent("e1".into()),
        });

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Delete event"));
    }
}
