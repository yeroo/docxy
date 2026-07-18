# lookxy v2 — Calendar (read + RSVP) — design

Let lookxy show your Outlook/Exchange **calendar** and respond to meeting
invites, so the terminal can cover the other main reason people keep Outlook
open. Read-only agenda + RSVP; mirrors lookxy v1's mail store/sync/TUI patterns
and Graph plumbing.

## 1. Goals / non-goals

**Goals**
- Sync the calendar to the local SQLite store and show an **agenda** —
  events grouped by day (Today, Tomorrow, …) with time, subject, location, and
  your response status; offline-first.
- **RSVP** to meeting invites: accept / decline / tentatively-accept, with an
  optional comment, applied optimistically and pushed to Graph via the outbox.
- Event **detail**: attendees (+ their response), organizer, full time range,
  location, and the event body (rendered via the existing HTML→text pass).

**Non-goals (this feature)**
- Creating or editing events, moving/rescheduling, deleting — read + RSVP only.
- Recurrence *editing* (we read expanded instances; we don't author rules).
- Multiple calendars / shared / group calendars — the user's default calendar.
- Free/busy lookup, scheduling assistant, reminders/notifications.
- Proposing a new time when declining (Graph supports it; deferred).

## 2. Store

New tables (same conventions as the mail store — WAL, FKs, delta links):
- `events(id TEXT PK, subject, start_utc, end_utc, is_all_day, location,
  organizer_name, organizer_addr, response_status, series_master_id,
  body_preview, web_link, last_modified)`
- `event_attendees(event_id, name, addr, type, response)` — for the detail view.
- `bodies` is reused for event bodies (keyed by event id) OR a parallel
  `event_bodies` table; reuse `bodies` with an `event:` id prefix to avoid a new
  table. (Decision: reuse `bodies`.)
- `meta` holds the calendar delta link + the synced window bounds.

Times are stored as UTC (`start_utc`/`end_utc`); the TUI renders in the local
zone. Graph returns `dateTime` + `timeZone`; the store normalizes to UTC on
ingest.

## 3. Sync + Graph

- **Windowed sync**: `GET /me/calendarView?startDateTime=&endDateTime=` over a
  rolling window (default −7 days … +30 days, configurable), which **expands
  recurring events into concrete instances** — so the store holds instances, not
  rules (matching the read-only scope). Delta-tracked via
  `/me/calendarView/delta` where available; otherwise a full windowed refetch on
  each calendar refresh (the window is small).
- Graph client gains: `calendar_view(start, end) -> Vec<Event>` (paged),
  `event_body(id)`, `event_attendees(id)` (or expand inline), and
  `respond_event(id, kind: Accept|Decline|Tentative, comment: Option<&str>,
  send_response: bool)` → `POST /me/events/{id}/accept|decline|tentativelyAccept`.
  All through the existing `with_auth` refresh/throttle wrapper.
- **Engine**: `SyncCommand::RefreshCalendar` fetches the window → store →
  `SyncEvent::CalendarUpdated`; runs on the same background thread on the tick
  loop (calendar refresh interleaved with mail delta). RSVP:
  `SyncCommand::RespondEvent{id, kind, comment}` → optimistic
  `response_status` update in the store + `SyncEvent::CalendarUpdated`, enqueue an
  outbox op (`RespondEvent`), drain with the same quarantine/retry policy as mail
  triage. A failed RSVP surfaces via `SyncEvent::Error`.
- New `OutboxOp::RespondEvent{id, kind, comment}`.

## 4. TUI

- **Calendar mode** — a distinct view toggled from the mail UI (a key, e.g. `g`,
  and back). Two-pane: an **agenda list** (left/main) and an **event detail**
  pane (right), mirroring the mail list/reading split so the code and muscle
  memory carry over.
- **Agenda list**: events from `store` within the window, grouped by day with a
  day header (Today / Tomorrow / weekday+date); each row shows start–end time (or
  "all day"), subject, location, and a response glyph (✓ accepted, ✗ declined,
  ? tentative, • no-response/needs-action). Newest-relevant first (today at top).
- **Detail pane** (Enter / selection): time range in local zone, organizer,
  location, attendee list with responses, and the event body via `htmlrender`.
- **RSVP keys** on the selected event: `a` accept, `d` decline, `t` tentative;
  an optional one-line comment prompt (Esc = no comment). Optimistic glyph
  update; outbox pushes.
- Status bar shows calendar sync state + count, reusing v1's status surface.

## 5. Error handling

- Offline: agenda reads from the store; RSVP queues in the outbox and syncs when
  back (offline-first, same as triage).
- RSVP failure: quarantine + `SyncEvent::Error`; the local glyph reverts on
  reconverge (the event re-syncs to server truth).
- Recurring-instance vs series: RSVP targets the specific **instance** id Graph
  returned in `calendarView`; responding to a series master is out of scope.
- All-day / multi-day events and cross-midnight events render without panic
  (grouping handles spans by start day; multi-day flagged in the row).

## 6. Testing

- `store`: event upsert/list-by-window, attendee rows, response-status update,
  delta-link round-trip, on temp DBs.
- `graph::client`: `calendar_view` parsing (incl. recurring expansion shape,
  timeZone→UTC normalization) and `respond_event` against the fake server with
  recorded Graph fixtures.
- `sync`: RefreshCalendar populates the store + emits CalendarUpdated; RSVP
  optimistic + outbox drain/quarantine end-to-end (fake server + temp DB).
- `lookxy` TUI: agenda grouping/render (day headers, response glyphs, all-day),
  detail pane, and RSVP key handling via `TestBackend`; empty-calendar and
  window-boundary cases don't panic.
- CI needs no network or account.

## 7. Build order (for the plan)

1. `store` events/attendees schema + methods (+ tests).
2. `graph::client` `calendar_view` + `respond_event` (+ fake-server tests) and
   the `Event`/attendee model + UTC normalization.
3. `sync` RefreshCalendar + RespondEvent outbox op + engine wiring.
4. `lookxy` calendar mode: agenda list + detail pane (render + navigation).
5. RSVP keys + optimistic/outbox wiring; status bar; docs.
