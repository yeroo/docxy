# lookxy v2 — Calendar (read + RSVP) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show the Exchange calendar in lookxy — an offline-first agenda (events grouped by day, with time/subject/location/response) plus accept/decline/tentative RSVP — mirroring the mail store/sync/TUI patterns.

**Architecture:** A new `events` store table synced from Graph `/me/calendarView` (which expands recurrences into instances) over a rolling window; a new calendar TUI mode (agenda list + detail pane); RSVP through the existing optimistic-store + outbox model. No new architectural pieces — new Graph methods, new `SyncCommand`/`SyncEvent`/`OutboxOp`, a new TUI surface.

**Tech Stack:** Rust (edition 2024), ratatui/crossterm, ureq+rustls, rusqlite, the existing `mailcore`. Hand-rolled JSON/HTML (no new deps).

## Global Constraints

- Edition 2024; MSRV 1.88; `[lints] workspace = true`. `clippy --all-targets -D warnings` + `fmt --all --check` green workspace-wide.
- Build/test only via `bash <scratchpad>/lcargo.sh <args>` with Bash `dangerouslyDisableSandbox: true` (broken `.cargo/bin` shims). Inline `#[cfg(test)]` tests.
- No new deps. Times stored as **UTC** (`start_utc`/`end_utc` as ISO-8601 strings); Graph `dateTime`+`timeZone` normalized to UTC on ingest; the TUI renders local zone.
- All Graph calls through the engine's existing `with_auth` refresh/throttle wrapper. RSVP rides the outbox — the UI never blocks; no direct `enqueue_op` from the TUI.
- Secrets never logged. Graph base `https://graph.microsoft.com/v1.0`.
- RSVP targets the specific **instance** id from `calendarView` (series-master responses out of scope).

---

### Task 1: Store — events + attendees schema and methods

**Files:**
- Modify: `mailcore/src/store/schema.rs`, `mailcore/src/store/mod.rs`

**Interfaces:**
- Produces:
  - Tables: `events(id TEXT PK, subject, start_utc, end_utc, is_all_day INTEGER, location, organizer_name, organizer_addr, response_status, series_master_id, body_preview, web_link, last_modified)`; `event_attendees(event_id TEXT, name, addr, type, response)`; `meta` holds `calendar_delta_link` + window bounds. Event bodies reuse `bodies` keyed by `event:<id>`.
  - `struct EventRow { pub id, subject, start_utc, end_utc, is_all_day, location, organizer_name, organizer_addr, response_status: String }`, `struct AttendeeRow { pub name, addr, r#type, response: String }`.
  - `fn upsert_event(&self, e: &Event) -> Result<(), StoreError>` (takes the Task-2 `Event` model), `fn put_event_attendees(&self, id: &str, a: &[Attendee])`, `fn events_in_window(&self, from_utc: &str, to_utc: &str) -> Result<Vec<EventRow>, StoreError>` (ordered `start_utc ASC`), `fn event_attendees(&self, id: &str) -> Result<Vec<AttendeeRow>, StoreError>`, `fn set_event_response(&self, id: &str, status: &str)`, `fn calendar_delta_link()`/`set_calendar_delta_link()`.

- [ ] **Step 1: Write failing tests** (in-memory DB): upsert two events → `events_in_window` returns them ordered by start; `set_event_response` flips the status; attendees round-trip; a delta-link round-trip.
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p mailcore store::`. Expected: FAIL.
- [ ] **Step 3: Implement.** Add the tables to the schema SQL (`CREATE TABLE IF NOT EXISTS`); prepared statements; `events_in_window` filters `start_utc < to AND end_utc > from` (so multi-day/ongoing events show) ordered by `start_utc`.
- [ ] **Step 4: Run, verify pass** + clippy + fmt.
- [ ] **Step 5: Commit** `mailcore: store events + attendees schema and queries`.

---

### Task 2: Graph client — calendarView + respond + Event model

**Files:**
- Modify: `mailcore/src/graph/client.rs`, `mailcore/src/graph/model.rs`

**Interfaces:**
- Produces (model): `struct Event { id, subject, start_utc, end_utc, is_all_day, location, organizer_name, organizer_addr, response_status, series_master_id, body_html, web_link, attendees: Vec<Attendee> }`, `struct Attendee { name, addr, r#type, response }`, with `Event::from_json(&Value) -> Option<Event>` that **normalizes** `start.dateTime`+`start.timeZone` → UTC ISO-8601 (`fn to_utc(dt: &str, tz: &str) -> String` — handle `"UTC"` passthrough and common IANA/Windows zones via a small offset table; unknown zone → treat as UTC and note). `response_status` from `responseStatus.response`.
- Produces (client): `fn calendar_view(&self, from_utc: &str, to_utc: &str) -> Result<Vec<Event>, GraphError>` — GET `/me/calendarView?startDateTime=&endDateTime=&$top=…` with `Prefer: outlook.timezone="UTC"` (so Graph returns UTC directly — simplifies normalization), paged via `@odata.nextLink`; `fn respond_event(&self, id: &str, kind: RsvpKind, comment: Option<&str>, send_response: bool) -> Result<(), GraphError>` — POST `/me/events/{id}/accept|decline|tentativelyAccept` with `{"comment":…,"sendResponse":…}`. `enum RsvpKind { Accept, Decline, Tentative }`.

- [ ] **Step 1: Write failing tests** against the fake server: `calendar_view` parses two events incl. UTC times (with `Prefer: outlook.timezone="UTC"` the dateTime is already UTC — assert `start_utc` ends in `Z` or is the raw UTC string), response_status, attendees; `respond_event(Accept, Some("ok"))` POSTs to `.../accept` with the comment in the body; a 401 → `Unauthorized`.
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p mailcore graph::`. Expected: FAIL.
- [ ] **Step 3: Implement.** Prefer-header makes Graph emit UTC, so `to_utc` mostly passes through; still guard a non-UTC `timeZone` defensively. Bodies parsed like message bodies. `encode_path_segment` on the id.
- [ ] **Step 4: Run, verify pass** + clippy + fmt.
- [ ] **Step 5: Commit** `mailcore: Graph calendarView + event RSVP + Event model`.

---

### Task 3: Sync — RefreshCalendar + RespondEvent outbox + engine

**Files:**
- Modify: `mailcore/src/store/mod.rs` (OutboxOp), `mailcore/src/sync/outbox.rs`, `mailcore/src/sync/engine.rs`

**Interfaces:**
- Produces: `OutboxOp::RespondEvent{ id, kind: String, comment: Option<String> }`; `SyncCommand::{ RefreshCalendar, RespondEvent{ id, kind, comment } }`; `SyncEvent::CalendarUpdated`. Engine: on `RefreshCalendar` (and on the periodic tick, interleaved with mail), fetch `calendar_view(window)`, upsert events + attendees, store the window bounds, emit `CalendarUpdated`. On `RespondEvent`: optimistic `store.set_event_response(id, kind)` + `CalendarUpdated`, enqueue the outbox op; drain via `apply_op` (→ `client.respond_event`) with the same quarantine/retry policy; failure → `SyncEvent::Error`. Window default −7d…+30d (from config `backfill`/a new `calendar_window_days`, default fixed for v2).

- [ ] **Step 1: Write failing tests:** outbox round-trip for `RespondEvent`; engine integration (fake server + temp DB): `RefreshCalendar` populates events + emits `CalendarUpdated`; `RespondEvent(Accept)` flips the local status immediately AND drains to a `.../accept` POST (assert via recorded requests), with quarantine on repeated 4xx. `recv_timeout` guards.
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p mailcore sync::`. Expected: FAIL.
- [ ] **Step 3: Implement** mirroring the mail RefreshCalendar/RespondEvent flow on the v1 patterns; compute the window from a fixed base time passed in (tests inject; prod uses `SystemTime::now()`). Never double-enqueue.
- [ ] **Step 4: Run, verify pass** (full `bash <wrapper> test -p mailcore`) + clippy + fmt.
- [ ] **Step 5: Commit** `mailcore: RefreshCalendar + RespondEvent outbox and engine`.

---

### Task 4: lookxy calendar mode — agenda list + detail

**Files:**
- Create: `lookxy/src/ui/calendar.rs`
- Modify: `lookxy/src/app.rs` (calendar mode + selection state), `lookxy/src/ui/mod.rs` (mode toggle + routing)

**Interfaces:**
- Produces:
  - App gains `enum Mode { Mail, Calendar }` (default Mail), a key (`g`) to toggle, `selected_event: Option<String>`, and on entering Calendar sends `SyncCommand::RefreshCalendar`.
  - `fn draw_calendar(f: &mut Frame, app: &App)` — two-pane (agenda list + detail): the list reads `store.events_in_window(...)`, groups by local day with a header (Today / Tomorrow / `Wed 23 Jul`), each row = `HH:MM–HH:MM` (or `all day`), subject, location, response glyph (`✓ ✗ ? •`). Detail pane (Enter): local time range, organizer, location, attendees + responses (`store.event_attendees`), and the body via `htmlrender`.
  - Navigation: ↑/↓/j/k move within the agenda (skipping day headers), Enter opens detail, `g`/Esc back to mail.

- [ ] **Step 1: Write failing test** (`TestBackend`): an `App` in Calendar mode over a store seeded with two events on different days renders both day headers + subjects; an empty calendar renders without panic; ↑/↓ move selection and don't panic on empty.
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p lookxy ui::calendar`. Expected: FAIL.
- [ ] **Step 3: Implement** the grouping (bucket events by local date; render headers + rows), the detail pane, and bounds-safe navigation (reuse v1's `.get()`/`saturating_sub` patterns; never index an empty list). Local-zone rendering from the stored UTC (a small UTC→local helper using the system offset).
- [ ] **Step 4: Run, verify pass** + clippy + fmt.
- [ ] **Step 5: Commit** `lookxy: calendar agenda + detail view`.

---

### Task 5: RSVP keys + wiring + status bar + docs

**Files:**
- Modify: `lookxy/src/ui/mod.rs`, `lookxy/src/app.rs`, `lookxy/src/ui/status_bar.rs`, `LOOKXY.md`

**Interfaces:**
- Produces: in Calendar mode, `a`/`d`/`t` on the selected event = accept/decline/tentative — prompt an optional one-line comment (Esc = none), then optimistically `store.set_event_response` + send `SyncCommand::RespondEvent{id,kind,comment}` (the engine enqueues the outbox op). The response glyph updates immediately. Status bar shows calendar sync state + event count when in Calendar mode (reuse the v1 status surface + `error_notice`). `SyncEvent::CalendarUpdated` reloads the visible agenda.

- [ ] **Step 1: Write failing test:** with an event selected, `on_key_char('a')` (through the comment prompt) sets the local response to accepted AND sends `SyncCommand::RespondEvent` with kind Accept (inspect via the test command channel); the glyph reflects it. Empty-calendar RSVP keys are no-ops (no panic).
- [ ] **Step 2: Run, verify fail.** `bash <wrapper> test -p lookxy`. Expected: FAIL.
- [ ] **Step 3: Implement** the RSVP keys + comment prompt + optimistic/outbox wiring; reload agenda on `CalendarUpdated`.
- [ ] **Step 4: Docs.** Add a "Calendar" section to `LOOKXY.md` (the `g` toggle, agenda, RSVP keys, that responses ride the outbox; read+RSVP scope).
- [ ] **Step 5: Full workspace green.** `bash <wrapper> test --workspace`, `clippy --workspace --all-targets -- -D warnings`, `fmt --all --check`.
- [ ] **Step 6: Commit** `lookxy: calendar RSVP keys, wiring, status bar, docs`.

---

## Self-Review Notes

- **Spec coverage:** §2 store → Task 1; §3 sync+Graph → Tasks 2–3; §4 TUI → Tasks 4–5; §5 error handling → Tasks 3 (quarantine/Error), 4/5 (empty/offline, bounds); §6 testing → every task TDD; §7 build order matches.
- **Type consistency:** `Event`/`Attendee`/`EventRow`/`AttendeeRow`/`RsvpKind` defined in Tasks 1–2 and reused; `OutboxOp::RespondEvent` / `SyncCommand::{RefreshCalendar,RespondEvent}` / `SyncEvent::CalendarUpdated` consistent across Tasks 3–5.
- **UTC normalization** is centralized in `Event::from_json`/`to_utc` (Task 2), aided by `Prefer: outlook.timezone="UTC"` so Graph returns UTC — the TUI does the single UTC→local conversion at render (Task 4).
- **Deferred (spec non-goals):** event create/edit/delete, multiple calendars, free/busy, propose-new-time — not in any task, by design.
