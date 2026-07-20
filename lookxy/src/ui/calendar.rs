//! Calendar mode: an agenda list (events grouped by local day) plus an event
//! detail pane — entered from the mail view via `g` (`App::toggle_mode`),
//! mirroring v1's list/reading two-pane split (`ui::message_list`/
//! `ui::reading`) but over `mailcore::store::EventRow`s instead of
//! `MessageRow`s. `App::agenda`/`App::agenda_index`/`App::selected_event`
//! hold the state this module renders and navigates; this file owns the
//! rendering, the local-day grouping, and the UTC→local time math.
//!
//! Events are stored as UTC ISO-8601 strings (`Store::events_in_window`
//! already normalizes Graph's `dateTime`+`timeZone` pairs to
//! `YYYY-MM-DDTHH:MM:SSZ` — see `mailcore::graph::model::Event`). This module
//! converts them to local wall-clock time for grouping/display using the
//! system's local UTC offset (`local_offset_minutes`) and a small hand-rolled
//! civil-calendar (Howard Hinnant's `days_from_civil`/`civil_from_days`
//! algorithm) — the same no-new-tz-dependency approach
//! `mailcore::sync::engine` already uses for its own UTC math, just run in
//! the other direction (UTC → local wall-clock, not "seconds since epoch →
//! UTC calendar date").

use crate::app::App;
use crate::ui::border_style;

use mailcore::htmlrender::{self, StyledLine, StyledSpan};
use mailcore::store::EventRow;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use std::time::{SystemTime, UNIX_EPOCH};

/// How many days into the past/future `agenda_window` covers — matches
/// `mailcore::sync::engine`'s `RefreshCalendar` fetch window (see its
/// `CALENDAR_WINDOW_PAST_DAYS`/`CALENDAR_WINDOW_FUTURE_DAYS`, private to that
/// module), so the agenda never asks the store for a wider range than what
/// actually gets synced into it.
const AGENDA_WINDOW_PAST_DAYS: i64 = 7;
const AGENDA_WINDOW_FUTURE_DAYS: i64 = 30;

/// The `[from, to)` UTC ISO-8601 bounds `App::reload_agenda` queries
/// `Store::events_in_window` with: `now - 7d` .. `now + 30d`, anchored at
/// `SystemTime::now()`.
pub(crate) fn agenda_window() -> (String, String) {
    let now = unix_now();
    let from = now - AGENDA_WINDOW_PAST_DAYS * 86_400;
    let to = now + AGENDA_WINDOW_FUTURE_DAYS * 86_400;
    (unix_to_iso8601(from), unix_to_iso8601(to))
}

/// Keys while `App::mode` is `Calendar`: ↑/↓/j/k move the agenda selection
/// (bounds-safe, clamped rather than wrapping — see
/// `App::move_agenda_selection`), Enter opens the detail pane on whatever's
/// highlighted, `g`/Esc return to the mail view, `a`/`d`/`t` start an
/// RSVP (accept/decline/tentative) on the highlighted row — routed here
/// (rather than through `App::on_key_char`, the Mail-mode dispatch) so they
/// can never clobber `a` (attachments) / `d` (delete)'s Mail-mode meanings;
/// see `RsvpPrompt` — `c`/`e` open the create/edit event form
/// (`App::open_new_event`/`open_edit_event`; `ui::eventform::handle_key`
/// takes over key handling from there, checked ahead of this function's own
/// call in `ui::handle_key` once the form is open) — and `x` opens the
/// confirm modal to delete the highlighted event (`App::delete_selected_event`;
/// `ui::handle_key` takes over Enter/Esc from there once the modal is open,
/// same precedence as the form). While the RSVP comment
/// prompt is open, every key instead goes to `handle_rsvp_prompt_key` —
/// checked first, ahead of the normal calendar keys, the same "prompt/popup
/// takes over key handling" precedence `ui::handle_key` already gives the
/// move-folder/attachments/search prompts over plain pane navigation.
pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    // The RSVP prompt is routed at the top of `ui::handle_key` now (it serves
    // both Mail and Calendar), so it's already handled before we get here.
    match key.code {
        KeyCode::Esc | KeyCode::Char('g') => app.toggle_mode(),
        KeyCode::Up | KeyCode::Char('k') => app.move_agenda_selection(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_agenda_selection(1),
        KeyCode::Enter => app.open_selected_event(),
        KeyCode::Char('a') => app.start_rsvp("accepted"),
        KeyCode::Char('d') => app.start_rsvp("declined"),
        KeyCode::Char('t') => app.start_rsvp("tentativelyAccepted"),
        KeyCode::Char('c') => app.open_new_event(),
        KeyCode::Char('e') => app.open_edit_event(),
        KeyCode::Char('x') => app.delete_selected_event(),
        // `O` opens the automatic-replies editor here too — it's an
        // account-level setting, so it's reachable from Calendar mode as well
        // as Mail mode (Calendar's key handling doesn't fall through to
        // `on_key_char`, so it must be bound explicitly).
        KeyCode::Char('O') => app.open_oof_form(),
        _ => {}
    }
}

/// Keys while the RSVP comment prompt is open: every printable character
/// types into the comment (so a comment containing `g`/`j`/`k`/`a`/`d`/`t`
/// still works, the same "the prompt owns every letter" reasoning
/// `ui::handle_search_key` already documents for the search query), Enter
/// submits with whatever was typed, Backspace edits it, and Esc submits with
/// no comment (see `App::cancel_rsvp_comment`'s doc comment for why that's
/// not "cancel the whole RSVP").
pub(crate) fn handle_rsvp_prompt_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => app.submit_rsvp(),
        KeyCode::Esc => app.cancel_rsvp_comment(),
        KeyCode::Tab => app.cycle_rsvp_focus(),
        KeyCode::Backspace => app.backspace_rsvp_comment(),
        KeyCode::Char(c) => app.type_rsvp_comment(&c.to_string()),
        _ => {}
    }
}

/// Renders Calendar mode: the agenda list (left) and the event detail pane
/// (right). Bounds-safe on an empty agenda — `draw_agenda`/`draw_detail`
/// both degrade to an empty list / a placeholder line rather than indexing
/// anything directly.
pub fn draw_calendar(f: &mut Frame, app: &App, area: Rect) {
    // Same vertical split as the mail three-pane layout (`ui::draw`): the
    // main area on top, a 1-row status bar pinned to the bottom — so
    // Calendar mode gets the same status surface (sync state + `error_notice`)
    // instead of losing it entirely while this view is showing. `area` is the
    // frame minus any reminder banner row (see `ui::draw`).
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(rows[0]);

    draw_agenda(f, app, cols[0]);
    draw_detail(f, app, cols[1]);
    crate::ui::status_bar::draw(f, app, rows[1]);
    draw_rsvp_prompt(f, app);
}

/// Renders the RSVP comment prompt as a centered overlay (same shape as the
/// attachments/move-folder popups — see `ui::attachments::draw`) when
/// `app.rsvp_prompt` is open; a no-op otherwise. Drawn last, on top of the
/// agenda/detail panes.
pub(crate) fn draw_rsvp_prompt(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;

    let Some(prompt) = &app.rsvp_prompt else {
        return;
    };
    let verb = match prompt.kind.as_str() {
        "accepted" => "Accept",
        "declined" => "Decline",
        "tentativelyAccepted" => "Tentative",
        other => other,
    };
    let proposes = prompt.proposes();
    let height = if proposes { 40 } else { 20 };
    let area = crate::ui::centered_rect(70, height, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(
            "{verb} \u{2014} Tab: field  Enter: send  Esc: send without"
        ))
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let field_line = |label: &str, value: &str, focused: bool| -> Line<'static> {
        let caret = if focused { "_" } else { "" };
        Line::from(format!("{label}: {value}{caret}"))
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    if proposes {
        lines.push(Line::from("Propose new time (blank = no proposal):"));
        lines.push(field_line(
            "  Proposed start",
            &prompt.proposed_start,
            prompt.focus == crate::app::RsvpField::ProposedStart,
        ));
        lines.push(field_line(
            "  Proposed end",
            &prompt.proposed_end,
            prompt.focus == crate::app::RsvpField::ProposedEnd,
        ));
        lines.push(Line::from(""));
    }
    lines.push(field_line(
        "Comment",
        &prompt.comment,
        prompt.focus == crate::app::RsvpField::Comment,
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

/// One line the agenda list renders: either a day header (a pure display
/// artifact) or an event, referenced by its index into the `events` slice
/// `agenda_lines` was built from — so `App::agenda_index` can keep indexing
/// straight into `App::agenda` (skipping headers is then just "headers
/// aren't in that vec at all", not something navigation needs to know
/// about).
enum AgendaLine {
    Header(String),
    Event(usize),
}

/// Groups `events` (already `start_utc`-ordered, per `Store::events_in_window`)
/// into day-headed runs by local start day: a header line is emitted before
/// the first event of each new local day, comparing against the previous
/// event's local day rather than re-deriving "today" for every row.
fn agenda_lines(events: &[EventRow]) -> Vec<AgendaLine> {
    let today = to_local(&unix_to_iso8601(unix_now()));
    let today_ymd = (today.year, today.month, today.day);
    let mut lines = Vec::with_capacity(events.len());
    let mut last_ymd: Option<(i64, u32, u32)> = None;
    for (i, e) in events.iter().enumerate() {
        let ymd = if e.is_all_day {
            date_of_utc(&e.start_utc)
        } else {
            let start = to_local(&e.start_utc);
            (start.year, start.month, start.day)
        };
        if last_ymd != Some(ymd) {
            lines.push(AgendaLine::Header(day_header(ymd, today_ymd)));
            last_ymd = Some(ymd);
        }
        lines.push(AgendaLine::Event(i));
    }
    lines
}

/// "Today"/"Tomorrow" for the two common cases, else `Wed 23 Jul` — computed
/// by comparing absolute day counts (`days_from_civil`), not by string/tuple
/// comparison, so it's correct across month/year boundaries.
fn day_header(ymd: (i64, u32, u32), today_ymd: (i64, u32, u32)) -> String {
    let day_num = days_from_civil(ymd.0, ymd.1, ymd.2);
    let today_num = days_from_civil(today_ymd.0, today_ymd.1, today_ymd.2);
    match day_num - today_num {
        0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        _ => format!(
            "{} {:02} {}",
            weekday_abbrev(day_num),
            ymd.2,
            month_abbrev(ymd.1)
        ),
    }
}

/// One agenda row: `HH:MM–HH:MM` (or `all day`), the subject, the location
/// (if any), the response glyph, and a `(multi-day)` marker when the local
/// start/end days differ.
fn event_row(e: &EventRow) -> Line<'static> {
    let glyph = response_glyph(&e.response_status);
    let time = if e.is_all_day {
        "all day".to_string()
    } else {
        let start = to_local(&e.start_utc);
        let end = to_local(&e.end_utc);
        format!(
            "{:02}:{:02}\u{2013}{:02}:{:02}",
            start.hour, start.minute, end.hour, end.minute
        )
    };
    let multi_day = if is_multi_day(e) { " (multi-day)" } else { "" };
    let text = if e.location.is_empty() {
        format!("{glyph} {time}  {}{multi_day}", e.subject)
    } else {
        format!(
            "{glyph} {time}  {} \u{2014} {}{multi_day}",
            e.subject, e.location
        )
    };
    Line::from(text)
}

/// Whether `e` spans more than one calendar day. All-day events use their
/// stored dates with the End treated as the exclusive next-day midnight (so a
/// one-day all-day event, `end = start + 1 day`, is NOT multi-day); timed
/// events compare local start/end days as before.
fn is_multi_day(e: &EventRow) -> bool {
    if e.is_all_day {
        let start_days = {
            let (y, m, d) = date_of_utc(&e.start_utc);
            days_from_civil(y, m, d)
        };
        let last_inclusive_day = {
            let (y, m, d) = date_of_utc(&e.end_utc);
            days_from_civil(y, m, d) - 1
        };
        return start_days != last_inclusive_day;
    }
    let start = to_local(&e.start_utc);
    let end = to_local(&e.end_utc);
    (start.year, start.month, start.day) != (end.year, end.month, end.day)
}

/// The response-status glyph shown per row/attendee: `✓` accepted, `✗`
/// declined, `?` tentative, `•` anything else (none/notResponded/organizer/
/// unrecognized — the Graph vocabulary is an unvalidated `String`, so this
/// must have a catch-all rather than assume only the four known values ever
/// appear).
fn response_glyph(status: &str) -> char {
    match status {
        "accepted" => '✓',
        "declined" => '✗',
        "tentativelyAccepted" => '?',
        _ => '•',
    }
}

fn draw_agenda(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title("Agenda")
        .borders(Borders::ALL)
        .border_style(border_style(true));

    let lines = agenda_lines(&app.agenda);
    let items: Vec<ListItem> = lines
        .iter()
        .map(|l| match l {
            AgendaLine::Header(h) => ListItem::new(Line::from(Span::styled(
                h.clone(),
                Style::new().add_modifier(Modifier::BOLD).fg(Color::Yellow),
            ))),
            AgendaLine::Event(i) => ListItem::new(event_row(&app.agenda[*i])),
        })
        .collect();

    // The visual row the highlight belongs on: `App::agenda_index` indexes
    // `app.agenda` directly (headers aren't part of that vec), so find the
    // `AgendaLine::Event` matching it rather than using the index as-is —
    // every header before it shifts the on-screen row down by one.
    let visual_selected = lines
        .iter()
        .position(|l| matches!(l, AgendaLine::Event(i) if *i == app.agenda_index));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));
    let mut state = ListState::default();
    if let Some(pos) = visual_selected {
        state.select(Some(pos));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title("Event")
        .borders(Borders::ALL)
        .border_style(border_style(false));
    let inner_width = area.width.saturating_sub(2) as usize;

    let lines = match selected_event(app) {
        Some(e) => {
            let mut lines = detail_header_lines(e);
            lines.push(Line::from(""));
            lines.extend(attendee_lines(app, &e.id));
            lines.push(Line::from(""));
            lines.extend(body_lines(app, &e.id, inner_width));
            lines
        }
        None => vec![Line::from("(no event selected — press Enter on an event)")],
    };

    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The event named by `App::selected_event`, if it's still in the currently
/// loaded agenda window — same "look it up by id in the cached list" shape
/// as `ui::reading::selected_message`.
fn selected_event(app: &App) -> Option<&EventRow> {
    let id = app.selected_event.as_deref()?;
    app.agenda.iter().find(|e| e.id == id)
}

fn detail_header_lines(e: &EventRow) -> Vec<Line<'static>> {
    let start = to_local(&e.start_utc);
    let end = to_local(&e.end_utc);
    let when = if e.is_all_day {
        // All-day dates are floating — read the same absolute
        // `date_of_utc` source the agenda buckets by, not `to_local`'s
        // offset conversion (see BUG 2 in the whole-branch review: on
        // negative UTC offsets `to_local` shifted this a day earlier than
        // the agenda's own bucketing of the same event).
        let (y, m, d) = date_of_utc(&e.start_utc);
        format!("{y:04}-{m:02}-{d:02} (all day)")
    } else {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}\u{2013}{:02}:{:02}",
            start.year, start.month, start.day, start.hour, start.minute, end.hour, end.minute
        )
    };
    let mut lines = vec![
        Line::from(format!("Subject: {}", e.subject)),
        Line::from(format!("When: {when}")),
        Line::from(format!(
            "Organizer: {} <{}>",
            e.organizer_name, e.organizer_addr
        )),
        Line::from(format!("Location: {}", e.location)),
    ];
    // A series occurrence (synced from Graph) carries a `series_master_id`.
    if e.series_master_id.is_some() {
        lines.push(Line::from("\u{21bb} repeats"));
    }
    lines
}

/// Attendee lines for the detail pane: `store.event_attendees(id)`, one row
/// per attendee with its response glyph — `(none)` if the event has no
/// attendees stored (e.g. a solo appointment).
fn attendee_lines(app: &App, id: &str) -> Vec<Line<'static>> {
    let attendees = app.store.event_attendees(id).unwrap_or_default();
    if attendees.is_empty() {
        return vec![Line::from("Attendees: (none)")];
    }
    let mut lines = vec![Line::from("Attendees:")];
    lines.extend(attendees.iter().map(|a| {
        Line::from(format!(
            "  {} {} <{}> \u{2014} {}",
            response_glyph(&a.response),
            a.name,
            a.addr,
            a.response
        ))
    }));
    lines
}

/// The event's body (`store.event_body`), rendered via `htmlrender` — always
/// HTML (`NewEvent::body_html`/`Event::body_html` are unconditionally HTML,
/// unlike a mail body which can be `text/plain`), so there's no
/// `render_text` branch to mirror from `ui::reading::body_lines`.
fn body_lines(app: &App, id: &str, width: usize) -> Vec<Line<'static>> {
    match app.store.event_body(id).unwrap_or(None) {
        Some(html) if !html.is_empty() => htmlrender::render_html(&html, width)
            .iter()
            .map(to_ratatui_line)
            .collect(),
        _ => vec![Line::from("(no body)")],
    }
}

/// Same mapping `ui::reading::to_ratatui_line` does — duplicated rather than
/// shared, since that function is private to its own module (the same
/// module-privacy reason `App::move_picker_select` duplicates
/// `ui::wrapped`'s shape instead of reusing it).
fn to_ratatui_line(line: &StyledLine) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    let indent = line.indent as usize * htmlrender::INDENT_SPACES;
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent)));
    }
    spans.extend(line.spans.iter().map(to_ratatui_span));
    Line::from(spans)
}

fn to_ratatui_span(span: &StyledSpan) -> Span<'static> {
    let mut style = Style::default();
    if span.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if span.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if span.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if span.link.is_some() {
        style = style.fg(Color::Cyan);
    }
    Span::styled(span.text.clone(), style)
}

// --- UTC → local time math (no chrono/time dependency) ---------------------

/// A UTC instant translated to local wall-clock time (see
/// `local_offset_minutes`) — just the pieces grouping/rendering need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalDateTime {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
}

/// Current wall-clock time as a Unix timestamp (seconds) — same seam
/// `mailcore::sync::engine::now_unix` uses, kept as its own copy here since
/// that one is private to its module.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Parses one of the store's fixed-width `YYYY-MM-DDTHH:MM:SSZ` UTC strings
/// into a Unix timestamp. Bounds/format-safe: any malformed or short input
/// (never expected in practice — every `start_utc`/`end_utc` the store hands
/// back went through `graph::model`'s `to_utc` normalization — but this must
/// not panic on it regardless) falls back to the Unix epoch rather than
/// indexing/parsing something that isn't there.
fn parse_iso_utc(s: &str) -> i64 {
    let field = |range: std::ops::Range<usize>| s.get(range).and_then(|p| p.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) = (
        field(0..4),
        field(5..7),
        field(8..10),
        field(11..13),
        field(14..16),
        field(17..19),
    ) else {
        return 0;
    };
    days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + se
}

/// Formats a Unix timestamp as an ISO-8601 UTC string
/// (`YYYY-MM-DDTHH:MM:SSZ`) — the inverse of `parse_iso_utc`, used only to
/// turn `unix_now()`/the agenda window bounds back into the string form
/// `Store::events_in_window` takes.
fn unix_to_iso8601(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, mi, se) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{se:02}Z")
}

/// Converts a UTC ISO-8601 string to local wall-clock time: parse to a Unix
/// timestamp, add the system's local offset (`local_offset_minutes`), and
/// re-derive the calendar fields from that shifted instant.
/// The local wall-clock `HH:MM` of a canonical-UTC timestamp — used by the
/// reminder banner ("starts in N min (HH:MM)"). Formats `to_local` (whose
/// fields are private to this module) so callers don't need them.
pub(crate) fn local_hhmm(iso_utc: &str) -> String {
    let l = to_local(iso_utc);
    format!("{:02}:{:02}", l.hour, l.minute)
}

fn to_local(iso_utc: &str) -> LocalDateTime {
    let utc_secs = parse_iso_utc(iso_utc);
    let local_secs = utc_secs + local_offset_minutes() * 60;
    let days = local_secs.div_euclid(86_400);
    let rem = local_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    LocalDateTime {
        year: y,
        month: m,
        day: d,
        hour: (rem / 3600) as u32,
        minute: ((rem % 3600) / 60) as u32,
    }
}

/// The `(year, month, day)` in the leading `YYYY-MM-DD` of a stored UTC
/// timestamp — used for all-day events, whose date is absolute (floating) and
/// must NOT be shifted by `to_local`'s offset conversion.
pub(crate) fn date_of_utc(iso: &str) -> (i64, u32, u32) {
    // `iso` is an ASCII ISO-8601 `YYYY-MM-DDT…`; `get(..)` is bounds/UTF-8 safe.
    let year: i64 = iso.get(0..4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let month: u32 = iso.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1);
    let day: u32 = iso.get(8..10).and_then(|s| s.parse().ok()).unwrap_or(1);
    (year, month, day)
}

/// The system's local UTC offset, in minutes (local = UTC + this many
/// minutes) — via the Windows API (`GetTimeZoneInformation`), so no
/// `chrono`/`time` dependency is needed just to render times in the user's
/// own zone. lookxy is Windows-only in practice (see `App::lookxy_dir`'s
/// `%LOCALAPPDATA%` path); off Windows this is a fixed `0` (UTC) fallback so
/// the crate still builds/tests elsewhere.
///
/// `pub(crate)` (was private) so `App::open_new_event`/`open_edit_event` (the
/// event form's "now"/prefill and UTC→local display conversion) can reuse the
/// exact same offset the agenda itself renders with, rather than duplicating
/// the Win32 call.
#[cfg(windows)]
pub(crate) fn local_offset_minutes() -> i64 {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct WinSystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[repr(C)]
    struct WinTimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: WinSystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: WinSystemTime,
        daylight_bias: i32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetTimeZoneInformation"]
        fn get_time_zone_information(info: *mut WinTimeZoneInformation) -> u32;
    }

    // SAFETY: `info` is a zero-initialized, correctly-sized/aligned
    // `#[repr(C)]` mirror of the real `TIME_ZONE_INFORMATION` (field-for-field
    // same types/order as the Win32 struct), and `GetTimeZoneInformation`
    // only ever writes through the pointer it's given — it takes no other
    // input and can't retain the pointer past the call returning.
    let mut info: WinTimeZoneInformation = unsafe { std::mem::zeroed() };
    let result = unsafe { get_time_zone_information(&mut info) };
    // TIME_ZONE_ID_DAYLIGHT = 2, TIME_ZONE_ID_STANDARD = 1,
    // TIME_ZONE_ID_UNKNOWN = 0 (no DST rule; `Bias` alone applies).
    // `0xFFFFFFFF` is the documented failure sentinel — fall back to UTC
    // rather than trust an unset `info`.
    let bias = match result {
        2 => info.bias + info.daylight_bias,
        1 => info.bias + info.standard_bias,
        0 => info.bias,
        _ => 0,
    };
    // Win32's `Bias` is "UTC = local + Bias" (minutes); this wants the
    // opposite direction ("local = UTC + offset").
    -(bias as i64)
}

#[cfg(not(windows))]
pub(crate) fn local_offset_minutes() -> i64 {
    0
}

/// Converts a count of days since the Unix epoch into a `(year, month, day)`
/// civil date — Howard Hinnant's `civil_from_days` algorithm, the same one
/// `mailcore::sync::engine` already uses (private to that module, so kept as
/// its own copy here — same duplication reasoning as `to_ratatui_line`).
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The inverse of `civil_from_days` — Howard Hinnant's `days_from_civil`
/// algorithm: a `(year, month, day)` civil date to a day count since the
/// Unix epoch.
pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let mp = (m as i64 + 9).rem_euclid(12) as u64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as u64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe as i64 - 719_468
}

/// `Sun`/`Mon`/… for the day-count `z` (days since the Unix epoch, which was
/// a Thursday) — `(z + 4).rem_euclid(7)` lands `z == 0` on index 4 (`Thu`),
/// matching that anchor.
/// A short calendar-date label like `Mon Jul 21` for a `(year, month, day)` —
/// used by the free/busy overlay's title.
pub(crate) fn day_label(y: i64, m: u32, d: u32) -> String {
    let z = days_from_civil(y, m, d);
    format!("{} {} {:02}", weekday_abbrev(z), month_abbrev(m), d)
}

fn weekday_abbrev(z: i64) -> &'static str {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    WEEKDAYS[(z + 4).rem_euclid(7) as usize]
}

fn month_abbrev(m: u32) -> &'static str {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .get(m.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("???")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Mode};
    use mailcore::sync::engine::SyncCommand;
    use ratatui::crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};

    fn sample_event(id: &str, start: &str, end: &str, subject: &str) -> mailcore::store::NewEvent {
        mailcore::store::NewEvent {
            id: id.into(),
            subject: subject.into(),
            start_utc: start.into(),
            end_utc: end.into(),
            is_all_day: false,
            location: "Room 1".into(),
            organizer_name: "Boss".into(),
            organizer_addr: "boss@example.com".into(),
            response_status: "accepted".into(),
            series_master_id: None,
            body_preview: "".into(),
            web_link: "".into(),
            last_modified: "2020-01-01T00:00:00Z".into(),
            body_html: "<p>agenda</p>".into(),
            reminder_minutes: 0,
            is_reminder_on: false,
        }
    }

    /// An ISO-8601 UTC timestamp `days` days from the real "now", at noon —
    /// for App-level (`reload_agenda`) tests, which filter through the real
    /// `agenda_window()` (anchored at the actual `SystemTime::now()`) rather
    /// than a fixed instant. `agenda_lines`-level unit tests don't need this
    /// (they operate on `EventRow`s directly, no store/window involved) and
    /// use fixed 2020 dates instead, deliberately far from any real "today".
    fn days_from_now(days: i64) -> String {
        unix_to_iso8601(unix_now() + days * 86_400 + 12 * 3600)
    }

    /// `iso_utc` plus one hour — for building a same-day `end_utc` from a
    /// `days_from_now`-produced `start_utc`.
    fn an_hour_after(iso_utc: &str) -> String {
        unix_to_iso8601(parse_iso_utc(iso_utc) + 3600)
    }

    fn row(id: &str, start: &str, end: &str, is_all_day: bool) -> EventRow {
        EventRow {
            id: id.into(),
            subject: format!("Subject {id}"),
            start_utc: start.into(),
            end_utc: end.into(),
            is_all_day,
            location: "".into(),
            organizer_name: "".into(),
            organizer_addr: "".into(),
            response_status: "none".into(),
            series_master_id: None,
            reminder_minutes: 0,
            is_reminder_on: false,
        }
    }

    // --- pure date math ----------------------------------------------------

    #[test]
    fn rsvp_prompt_shows_proposed_rows_for_decline_only() {
        use crate::app::{RsvpField, RsvpPrompt, RsvpTarget};
        let mut app = App::for_test_with_seeded_store();
        app.rsvp_prompt = Some(RsvpPrompt {
            target: RsvpTarget::Event("E1".into()),
            kind: "declined".into(),
            comment: String::new(),
            proposed_start: String::new(),
            proposed_end: String::new(),
            focus: RsvpField::ProposedStart,
        });
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw_rsvp_prompt(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Proposed start"));
        assert!(text.contains("Proposed end"));

        app.rsvp_prompt.as_mut().unwrap().kind = "accepted".into();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw_rsvp_prompt(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains("Proposed start"));
    }

    #[test]
    fn days_from_civil_and_civil_from_days_round_trip() {
        for (y, m, d) in [(1970, 1, 1), (2026, 7, 17), (2000, 2, 29), (1969, 12, 31)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "{y}-{m}-{d}");
        }
    }

    #[test]
    fn parse_iso_utc_round_trips_through_unix_to_iso8601() {
        let secs = parse_iso_utc("2026-07-17T09:30:15Z");
        assert_eq!(unix_to_iso8601(secs), "2026-07-17T09:30:15Z");
    }

    #[test]
    fn parse_iso_utc_falls_back_to_epoch_on_malformed_input() {
        assert_eq!(parse_iso_utc(""), 0);
        assert_eq!(parse_iso_utc("garbage"), 0);
    }

    #[test]
    fn local_offset_minutes_is_within_a_plausible_range() {
        // No fixed value to assert (depends on the machine running the
        // test), but any real timezone offset is within UTC-12..UTC+14.
        let mins = local_offset_minutes();
        assert!((-12 * 60..=14 * 60).contains(&mins));
    }

    #[test]
    fn response_glyph_maps_known_statuses() {
        assert_eq!(response_glyph("accepted"), '✓');
        assert_eq!(response_glyph("declined"), '✗');
        assert_eq!(response_glyph("tentativelyAccepted"), '?');
        assert_eq!(response_glyph("none"), '•');
        assert_eq!(response_glyph("notResponded"), '•');
        assert_eq!(response_glyph(""), '•');
    }

    #[test]
    fn agenda_lines_groups_events_on_different_days_into_separate_headers() {
        // Two events far apart (and far from "today"), each solidly at
        // midday UTC so no plausible local offset can shift them onto the
        // same calendar day as each other or as "today" — keeps this
        // deterministic regardless of the machine's timezone.
        let events = vec![
            row("e1", "2020-01-05T12:00:00Z", "2020-01-05T13:00:00Z", false),
            row("e2", "2020-06-15T12:00:00Z", "2020-06-15T13:00:00Z", false),
        ];
        let lines = agenda_lines(&events);
        let headers: Vec<&String> = lines
            .iter()
            .filter_map(|l| match l {
                AgendaLine::Header(h) => Some(h),
                AgendaLine::Event(_) => None,
            })
            .collect();
        assert_eq!(headers.len(), 2, "expected two distinct day groups");
        let event_indices: Vec<usize> = lines
            .iter()
            .filter_map(|l| match l {
                AgendaLine::Event(i) => Some(*i),
                AgendaLine::Header(_) => None,
            })
            .collect();
        assert_eq!(event_indices, vec![0, 1]);
    }

    #[test]
    fn agenda_lines_on_an_empty_list_is_empty() {
        assert!(agenda_lines(&[]).is_empty());
    }

    #[test]
    fn is_multi_day_detects_events_crossing_midnight_local() {
        let e = row("e1", "2026-07-17T23:00:00Z", "2026-07-18T01:00:00Z", false);
        // Whether this actually crosses local midnight depends on the local
        // offset, but the same local conversion `event_row`/`agenda_lines`
        // use must agree with `is_multi_day` — check that consistency rather
        // than a fixed answer.
        let start = to_local(&e.start_utc);
        let end = to_local(&e.end_utc);
        let expected = (start.year, start.month, start.day) != (end.year, end.month, end.day);
        assert_eq!(is_multi_day(&e), expected);
    }

    #[test]
    fn date_of_utc_extracts_the_calendar_date() {
        assert_eq!(date_of_utc("2026-07-20T00:00:00Z"), (2026, 7, 20));
        assert_eq!(date_of_utc("2027-01-01T00:00:00Z"), (2027, 1, 1));
    }

    #[test]
    fn single_day_all_day_event_is_not_multi_day() {
        // end is the exclusive next-day midnight (Graph's convention)
        let e = row("e1", "2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z", true);
        assert!(!is_multi_day(&e));
    }

    #[test]
    fn three_day_all_day_event_is_multi_day() {
        let e = row("e2", "2026-07-20T00:00:00Z", "2026-07-23T00:00:00Z", true);
        assert!(is_multi_day(&e));
    }

    #[test]
    fn all_day_event_buckets_under_its_stored_start_date_regardless_of_offset() {
        // The all-day day comes from the DATE PART of start_utc, not to_local,
        // so it never shifts with the local offset. Assert via date_of_utc,
        // which agenda_lines uses for all-day events.
        assert_eq!(date_of_utc("2026-07-20T00:00:00Z"), (2026, 7, 20));
        // (a timed event still buckets by to_local — unchanged, covered by the
        // existing agenda_lines tests)
    }

    // --- App wiring / rendering ---------------------------------------------

    #[test]
    fn g_toggles_mode_and_refreshes_calendar_then_back_to_mail() {
        use std::sync::mpsc;

        let mut app = App::for_test_with_seeded_store();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        app.sync.cmd_tx = cmd_tx;

        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(app.mode, Mode::Calendar);
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(SyncCommand::RefreshCalendar)
        ));

        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(app.mode, Mode::Mail);
    }

    #[test]
    fn esc_also_returns_to_mail_from_calendar() {
        let mut app = App::for_test_with_seeded_store();
        app.toggle_mode();
        assert_eq!(app.mode, Mode::Calendar);
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Mail);
    }

    #[test]
    fn calendar_mode_renders_day_headers_and_subjects_for_events_on_different_days() {
        let mut app = App::for_test_with_seeded_store();
        let day1 = days_from_now(1);
        app.store
            .upsert_event(&sample_event("e1", &day1, &an_hour_after(&day1), "Standup"))
            .unwrap();
        let day10 = days_from_now(10);
        app.store
            .upsert_event(&sample_event(
                "e2",
                &day10,
                &an_hour_after(&day10),
                "Planning",
            ))
            .unwrap();
        app.toggle_mode();
        assert_eq!(app.agenda.len(), 2);

        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw_calendar(f, &app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Standup"));
        assert!(text.contains("Planning"));
    }

    #[test]
    fn empty_calendar_renders_without_panicking() {
        let mut app = App::for_test_with_empty_store();
        app.toggle_mode();
        assert!(app.agenda.is_empty());

        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw_calendar(f, &app, f.area())).unwrap();
    }

    #[test]
    fn navigation_on_an_empty_agenda_does_not_panic_and_stays_put() {
        let mut app = App::for_test_with_empty_store();
        app.toggle_mode();
        app.move_agenda_selection(-1);
        app.move_agenda_selection(1);
        assert_eq!(app.agenda_index, 0);
        app.open_selected_event();
        assert!(app.selected_event.is_none());
    }

    #[test]
    fn navigation_clamps_at_both_ends_without_wrapping() {
        let mut app = App::for_test_with_seeded_store();
        let day1 = days_from_now(1);
        app.store
            .upsert_event(&sample_event("e1", &day1, &an_hour_after(&day1), "First"))
            .unwrap();
        let day2 = days_from_now(2);
        app.store
            .upsert_event(&sample_event("e2", &day2, &an_hour_after(&day2), "Second"))
            .unwrap();
        app.toggle_mode();
        assert_eq!(app.agenda.len(), 2);
        assert_eq!(app.agenda_index, 0);

        app.move_agenda_selection(-1);
        assert_eq!(app.agenda_index, 0, "must clamp, not wrap, at the top");

        app.move_agenda_selection(1);
        assert_eq!(app.agenda_index, 1);
        app.move_agenda_selection(1);
        assert_eq!(app.agenda_index, 1, "must clamp, not wrap, at the bottom");
    }

    #[test]
    fn enter_opens_the_detail_pane_for_the_highlighted_event() {
        let mut app = App::for_test_with_seeded_store();
        let day1 = days_from_now(1);
        app.store
            .upsert_event(&sample_event("e1", &day1, &an_hour_after(&day1), "Standup"))
            .unwrap();
        app.store
            .put_event_attendees(
                "e1",
                &[mailcore::store::NewAttendee {
                    name: "Alice".into(),
                    addr: "alice@example.com".into(),
                    r#type: "required".into(),
                    response: "accepted".into(),
                }],
            )
            .unwrap();
        app.toggle_mode();

        app.open_selected_event();
        assert_eq!(app.selected_event.as_deref(), Some("e1"));

        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw_calendar(f, &app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Standup"));
        assert!(text.contains("Alice"));
        assert!(text.contains("agenda")); // the event body, via htmlrender
    }

    #[test]
    fn calendar_updated_event_reloads_the_agenda() {
        let mut app = App::for_test_with_seeded_store();
        app.toggle_mode();
        assert!(app.agenda.is_empty());

        let day1 = days_from_now(1);
        app.store
            .upsert_event(&sample_event("e1", &day1, &an_hour_after(&day1), "Standup"))
            .unwrap();
        app.on_sync_event(mailcore::sync::engine::SyncEvent::CalendarUpdated);

        assert_eq!(app.agenda.len(), 1);
    }

    // --- RSVP keys + comment prompt + optimistic/outbox wiring --------------

    /// Seeds one event ("e1", starting tomorrow) with `response_status`
    /// "none" (unlike `sample_event`'s default "accepted" — starting from
    /// "none" makes an accept/decline transition actually observable) and
    /// enters Calendar mode with it highlighted. Returns the event's id.
    fn seed_one_event_in_calendar_mode(app: &mut App) -> String {
        let day1 = days_from_now(1);
        let mut e = sample_event("e1", &day1, &an_hour_after(&day1), "Standup");
        e.response_status = "none".into();
        app.store.upsert_event(&e).unwrap();
        app.toggle_mode();
        assert_eq!(app.agenda.len(), 1);
        assert_eq!(app.agenda_index, 0);
        "e1".to_string()
    }

    #[test]
    fn accept_key_opens_a_comment_prompt_and_enter_submits_it_with_the_comment() {
        use std::sync::mpsc;

        let mut app = App::for_test_with_seeded_store();
        let id = seed_one_event_in_calendar_mode(&mut app);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        app.sync.cmd_tx = cmd_tx;

        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char('a')));
        assert!(
            app.rsvp_prompt.is_some(),
            "'a' must open the comment prompt"
        );

        for c in "sounds good".chars() {
            crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char(c)));
        }
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Enter));

        assert!(app.rsvp_prompt.is_none());
        // Optimistic local write landed immediately.
        let row = app
            .store
            .events_in_window("2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z")
            .unwrap()
            .into_iter()
            .find(|e| e.id == id)
            .unwrap();
        assert_eq!(row.response_status, "accepted");
        // The agenda (and therefore the rendered glyph) reflects it too.
        assert_eq!(app.agenda[0].response_status, "accepted");

        match cmd_rx.try_recv() {
            Ok(SyncCommand::RespondEvent {
                id: sent_id,
                kind,
                comment,
                ..
            }) => {
                assert_eq!(sent_id, id);
                assert_eq!(kind, "accepted");
                assert_eq!(comment.as_deref(), Some("sounds good"));
            }
            other => panic!("expected a RespondEvent command, got {other:?}"),
        }

        let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
        term.draw(|f| draw_calendar(f, &app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains('✓'), "the glyph must reflect the new status");
    }

    #[test]
    fn esc_on_the_comment_prompt_submits_the_rsvp_with_no_comment() {
        use std::sync::mpsc;

        let mut app = App::for_test_with_seeded_store();
        let id = seed_one_event_in_calendar_mode(&mut app);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        app.sync.cmd_tx = cmd_tx;

        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char('d')));
        assert!(app.rsvp_prompt.is_some());
        for c in "never mind".chars() {
            crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char(c)));
        }
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Esc));

        assert!(app.rsvp_prompt.is_none());
        assert_eq!(app.agenda[0].response_status, "declined");
        match cmd_rx.try_recv() {
            Ok(SyncCommand::RespondEvent {
                id: sent_id,
                kind,
                comment,
                ..
            }) => {
                assert_eq!(sent_id, id);
                assert_eq!(kind, "declined");
                assert_eq!(comment, None, "Esc must send with no comment");
            }
            other => panic!("expected a RespondEvent command, got {other:?}"),
        }
    }

    #[test]
    fn tentative_key_sets_the_tentative_status() {
        let mut app = App::for_test_with_seeded_store();
        seed_one_event_in_calendar_mode(&mut app);

        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char('t')));
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Enter));

        assert_eq!(app.agenda[0].response_status, "tentativelyAccepted");
    }

    #[test]
    fn rsvp_keys_on_an_empty_agenda_are_no_ops() {
        use std::sync::mpsc;

        let mut app = App::for_test_with_empty_store();
        app.toggle_mode();
        assert!(app.agenda.is_empty());
        let (cmd_tx, cmd_rx) = mpsc::channel();
        app.sync.cmd_tx = cmd_tx;

        for key in ['a', 'd', 't'] {
            crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char(key)));
            assert!(
                app.rsvp_prompt.is_none(),
                "no event to respond to — must not open a prompt"
            );
        }
        assert!(
            cmd_rx.try_recv().is_err(),
            "no command should have been sent"
        );
    }
}
