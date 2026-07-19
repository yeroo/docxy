//! The create/edit event form: a full-screen-over-the-calendar overlay with
//! Title/Start/End/All-day/Location/Attendees/Body fields, opened by
//! `App::open_new_event`/`open_edit_event` (`c`/`e` in Calendar mode) and
//! drawn by `draw` whenever `App::event_form` is `Some` (see `ui::draw`'s
//! Calendar branch).
//!
//! Covers the form's state (`EventForm`/`EventField`), the two entry points
//! that populate it, rendering (mirroring `ui::compose::draw_compose`'s
//! field-row layout and focus highlighting), and key handling (`handle_key`):
//! Tab cycles focus, Char/Backspace edit the focused text field (Space
//! toggles All-day), Ctrl-Enter saves (`App::save_event_form`), Esc closes
//! without saving. The attendee-autocomplete dropdown's actual content
//! (search-as-you-type over the store) is a later task's concern; the
//! dropdown overlay is wired here in an overlay-safe way (a no-op while
//! `autocomplete` is `None`, which it always is until that task sets it).

use crate::app::App;
use crate::ui::border_style;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

/// Which field the event form's keyboard focus is on. Tab cycles
/// Title → Start → End → AllDay → Location → Attendees → Body → Title.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EventField {
    Title,
    Start,
    End,
    AllDay,
    Location,
    Attendees,
    Body,
}

/// The open create/edit event form. `editing_id` is `Some(event id)` when
/// editing an existing event, `None` for a new one. `start`/`end` hold the
/// raw local-time text the datetime parser consumes; `attendees` is the flat
/// `Name <addr>; …` text (with contacts autocomplete). `error` is the inline
/// validation message shown in the footer.
pub struct EventForm {
    /// Opaque to this module: `draw` doesn't render it. `App::save_event_form`
    /// reads it to decide create (`None`) vs. update (`Some(id)`) — and, for
    /// an update, whether `id` is a still-not-yet-synced `local:` id (see
    /// that function's doc comment for why that changes what gets enqueued).
    pub editing_id: Option<String>,
    pub title: String,
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub location: String,
    pub attendees: String,
    pub body: String,
    pub focus: EventField,
    pub autocomplete: Option<crate::ui::compose::Autocomplete>,
    pub error: Option<String>,
}

/// Renders the event form overlay when `app.event_form` is open; a no-op
/// otherwise (mirrors `ui::compose::draw_compose`/`ui::filepicker::draw`).
/// Layout, top to bottom: Title / Start / End / All-day / Location /
/// Attendees (3 rows each), the Body editor (everything else), and a 1-row
/// footer (the inline `error`, or a key-hint reminder when there is none).
pub fn draw(f: &mut Frame, app: &App) {
    let Some(form) = &app.event_form else {
        return;
    };

    let frame_area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Length(3), // Start
            Constraint::Length(3), // End
            Constraint::Length(3), // All-day
            Constraint::Length(3), // Location
            Constraint::Length(3), // Attendees
            Constraint::Min(3),    // Body
            Constraint::Length(1), // Footer
        ])
        .split(frame_area);

    // `Clear` first so the overlay doesn't show the calendar bleeding through
    // behind its fields — the same "clear before drawing" pattern the other
    // popups (`attachments::draw`, `filepicker::draw`) use over the panes
    // they sit on.
    f.render_widget(Clear, frame_area);

    draw_field(
        f,
        rows[0],
        "Title",
        &form.title,
        form.focus == EventField::Title,
    );
    draw_field(
        f,
        rows[1],
        "Start",
        &form.start,
        form.focus == EventField::Start,
    );
    draw_field(f, rows[2], "End", &form.end, form.focus == EventField::End);
    draw_all_day(f, rows[3], form.all_day, form.focus == EventField::AllDay);
    draw_field(
        f,
        rows[4],
        "Location",
        &form.location,
        form.focus == EventField::Location,
    );
    draw_field(
        f,
        rows[5],
        "Attendees",
        &form.attendees,
        form.focus == EventField::Attendees,
    );
    draw_field(
        f,
        rows[6],
        "Body",
        &form.body,
        form.focus == EventField::Body,
    );
    draw_footer(f, rows[7], form.error.as_deref());

    if let Some(ac) = &form.autocomplete {
        if ac.field == crate::ui::compose::ComposeField::To {
            // Placeholder wiring only — Task 8 gives the Attendees field its
            // own dropdown field-matching (compose's `ComposeField` isn't
            // what `EventField` autocomplete keys off of); nothing sets
            // `autocomplete` to `Some` yet, so this never actually runs.
            draw_autocomplete(f, rows[5], frame_area, ac);
        }
    }
}

/// One single-line field: a bordered box, bright when focused
/// (`border_style`, shared with every other pane/popup), with a trailing `_`
/// caret when it holds focus — same convention as
/// `ui::compose::draw_field` (private to that module, so kept as its own
/// copy here per the brief's "reuse if reachable, else a local copy" note).
fn draw_field(f: &mut Frame, area: Rect, title: &str, value: &str, focused: bool) {
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let text = if focused {
        format!("{value}_")
    } else {
        value.to_string()
    };
    f.render_widget(Paragraph::new(text).block(block), area);
}

/// The All-day row: a bordered box titled "All-day", showing `[x]`/`[ ]` —
/// toggled with Space (Task 7's key handling), not typed into.
fn draw_all_day(f: &mut Frame, area: Rect, all_day: bool, focused: bool) {
    let block = Block::default()
        .title("All-day (Space to toggle)")
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let text = if all_day { "[x]" } else { "[ ]" };
    f.render_widget(Paragraph::new(text).block(block), area);
}

/// The footer: the inline validation `error` (if any), styled to stand out,
/// otherwise a reminder of the keys that aren't otherwise visible on screen —
/// same shape as `ui::compose::draw_action_bar`.
fn draw_footer(f: &mut Frame, area: Rect, error: Option<&str>) {
    match error {
        Some(msg) => {
            f.render_widget(
                Paragraph::new(msg.to_string()).style(Style::new().fg(Color::White).bg(Color::Red)),
                area,
            );
        }
        None => {
            let text = "Save: Ctrl-Enter   Cancel: Esc   Tab: next field   Space: toggle all-day";
            f.render_widget(
                Paragraph::new(text).style(Style::new().fg(Color::White).bg(Color::DarkGray)),
                area,
            );
        }
    }
}

/// The attendee-autocomplete dropdown: a bordered overlay directly below
/// `field_area`, listing `ac.matches` as `Name <addr>`, highlighting
/// `ac.index`. Mirrors `ui::compose::draw_autocomplete` (private to that
/// module, so kept as its own copy here); unreachable in practice until a
/// later task ever sets `EventForm::autocomplete` to `Some`.
fn draw_autocomplete(
    f: &mut Frame,
    field_area: Rect,
    frame_area: Rect,
    ac: &crate::ui::compose::Autocomplete,
) {
    let wanted_height = (ac.matches.len() as u16 + 2).min(8);
    let area = Rect {
        x: field_area.x,
        y: field_area.y + field_area.height,
        width: field_area.width,
        height: wanted_height,
    }
    .intersection(frame_area);
    if area.height == 0 || area.width == 0 {
        return;
    }

    f.render_widget(Clear, area);

    let items: Vec<ListItem> = ac
        .matches
        .iter()
        .map(|c| {
            let name = c.name.trim();
            let label = if name.is_empty() {
                c.address.clone()
            } else {
                format!("{} <{}>", name, c.address)
            };
            ListItem::new(label)
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));

    let mut state = ListState::default();
    state.select(Some(ac.index));
    f.render_stateful_widget(list, area, &mut state);
}

/// Keys while the event form is open (checked ahead of the calendar's own
/// key handling — see `ui::handle_key`): Tab cycles focus, Ctrl-Enter saves
/// (`App::save_event_form`, which sets an inline `error` and leaves the form
/// open on a validation failure — nothing here needs to react to that), Esc
/// closes the form without saving. Otherwise, Char/Backspace edit whichever
/// field currently has focus — Space on the All-day field toggles it instead
/// of "typing" (there's no text there to edit); any other character while on
/// All-day is ignored, same as compose's non-text fields ignore keys that
/// don't apply to them. A no-op if the form isn't open (defensive; the only
/// caller already checks `app.event_form.is_some()` first). Attendee
/// autocomplete keys are Task 8's concern — none are handled here yet.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    if app.event_form.is_none() {
        return;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Enter {
        app.save_event_form();
        return;
    }
    if key.code == KeyCode::Esc {
        app.event_form = None;
        return;
    }
    let Some(form) = app.event_form.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Tab => cycle_focus(form),
        KeyCode::Char(' ') if form.focus == EventField::AllDay => {
            form.all_day = !form.all_day;
        }
        KeyCode::Char(c) => match form.focus {
            EventField::Title => form.title.push(c),
            EventField::Start => form.start.push(c),
            EventField::End => form.end.push(c),
            EventField::AllDay => {}
            EventField::Location => form.location.push(c),
            EventField::Attendees => form.attendees.push(c),
            EventField::Body => form.body.push(c),
        },
        KeyCode::Backspace => match form.focus {
            EventField::Title => {
                form.title.pop();
            }
            EventField::Start => {
                form.start.pop();
            }
            EventField::End => {
                form.end.pop();
            }
            EventField::AllDay => {}
            EventField::Location => {
                form.location.pop();
            }
            EventField::Attendees => {
                form.attendees.pop();
            }
            EventField::Body => {
                form.body.pop();
            }
        },
        _ => {}
    }
}

/// `Title` → `Start` → `End` → `AllDay` → `Location` → `Attendees` → `Body`
/// → `Title`.
fn cycle_focus(form: &mut EventForm) {
    form.focus = match form.focus {
        EventField::Title => EventField::Start,
        EventField::Start => EventField::End,
        EventField::End => EventField::AllDay,
        EventField::AllDay => EventField::Location,
        EventField::Location => EventField::Attendees,
        EventField::Attendees => EventField::Body,
        EventField::Body => EventField::Title,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn blank_form() -> EventForm {
        EventForm {
            editing_id: None,
            title: String::new(),
            start: "2026-07-20 14:00".into(),
            end: "2026-07-20 15:00".into(),
            all_day: false,
            location: String::new(),
            attendees: String::new(),
            body: String::new(),
            focus: EventField::Title,
            autocomplete: None,
            error: None,
        }
    }

    #[test]
    fn draw_is_a_no_op_when_no_form_is_open() {
        let app = App::for_test_with_seeded_store();
        assert!(app.event_form.is_none());
        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("All-day"));
    }

    #[test]
    fn draw_renders_fields_and_error_footer_without_panicking() {
        let mut app = App::for_test_with_seeded_store();
        let mut form = blank_form();
        form.title = "Standup".into();
        form.error = Some("Invalid start time".into());
        app.event_form = Some(form);

        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Standup"));
        assert!(text.contains("2026-07-20 14:00"));
        assert!(text.contains("Invalid start time"));
    }

    #[test]
    fn tab_cycles_focus_through_every_field_and_back_to_title() {
        let mut app = App::for_test_with_seeded_store();
        app.event_form = Some(blank_form());
        let order = [
            EventField::Start,
            EventField::End,
            EventField::AllDay,
            EventField::Location,
            EventField::Attendees,
            EventField::Body,
            EventField::Title,
        ];
        for expected in order {
            handle_key(&mut app, KeyEvent::from(KeyCode::Tab));
            assert!(app.event_form.as_ref().unwrap().focus == expected);
        }
    }

    #[test]
    fn char_and_backspace_edit_the_focused_text_field() {
        let mut app = App::for_test_with_seeded_store();
        app.event_form = Some(blank_form()); // focus starts on Title

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('X')));
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('Y')));
        assert_eq!(app.event_form.as_ref().unwrap().title, "XY");

        handle_key(&mut app, KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.event_form.as_ref().unwrap().title, "X");
    }

    #[test]
    fn space_toggles_all_day_and_other_chars_on_it_do_nothing() {
        let mut app = App::for_test_with_seeded_store();
        let mut form = blank_form();
        form.focus = EventField::AllDay;
        app.event_form = Some(form);
        assert!(!app.event_form.as_ref().unwrap().all_day);

        handle_key(&mut app, KeyEvent::from(KeyCode::Char('z')));
        assert!(!app.event_form.as_ref().unwrap().all_day); // ignored, not text

        handle_key(&mut app, KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.event_form.as_ref().unwrap().all_day);

        handle_key(&mut app, KeyEvent::from(KeyCode::Char(' ')));
        assert!(!app.event_form.as_ref().unwrap().all_day); // toggles back off
    }

    #[test]
    fn esc_closes_the_form_without_saving() {
        let mut app = App::for_test_with_seeded_store();
        let mut form = blank_form();
        form.title = "Unsaved".into();
        app.event_form = Some(form);

        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));

        assert!(app.event_form.is_none());
        assert!(
            app.store
                .events_in_window("2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn ctrl_enter_calls_save_and_closes_the_form_on_valid_input() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        let mut form = blank_form();
        form.title = "Planning".into();
        app.event_form = Some(form);

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
        );

        assert!(app.event_form.is_none()); // save_event_form ran and closed it
        assert_eq!(app.agenda.len(), 1);
        assert_eq!(app.agenda[0].subject, "Planning");
    }

    #[test]
    fn handle_key_is_a_no_op_when_the_form_is_closed() {
        let mut app = App::for_test_with_seeded_store();
        assert!(app.event_form.is_none());
        handle_key(&mut app, KeyEvent::from(KeyCode::Char('x'))); // must not panic
        assert!(app.event_form.is_none());
    }
}
