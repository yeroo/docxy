# lookxy calendar create / edit / delete — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-19.
**Builds on:** lookxy v1 (mailcore store + sync + Graph client + outbox), v2 (calendar read + RSVP, editcore compose), v3 (thread view), v4 (contacts/autocomplete), v5 (attachments).

## Goal

Let the user create, edit, and delete calendar events from lookxy's calendar
view — a form collecting title / start / end / all-day / location / attendees /
body — using the same optimistic-local + outbox pattern the existing RSVP flow
uses. Attendee entry reuses the contacts autocomplete. Recurring events remain
RSVP-only.

## Product decisions (locked)

1. **Create + edit + delete of single events.** New events are non-recurring.
   Recurring events (those with a `series_master_id`) stay read + RSVP only —
   `e`/`x` are refused with a notice.
2. **Optimistic-local + outbox.** Saving writes the event to the local store
   immediately (so it appears in the agenda) and enqueues an outbox op that the
   engine drains to Graph, reconciling a `local:` id to the Graph id on create —
   exactly how drafts and RSVPs already work. Invites are sent by Graph on the
   `POST`/`PATCH`.
3. **Local-time input, bounded datetime grammar.** Times are typed in local time
   and converted to UTC. The parser accepts a fixed, deterministic set of shapes
   (fixed format + `today`/`tomorrow` + `+Nh/m/d` relative + 12-hour) — not
   open-ended natural language.

## Architecture

An **event form** (a full-screen mode like the mail composer, but for events)
collects the fields; on save it validates + parses the times to UTC, writes an
optimistic local event, and enqueues a `CreateEvent`/`UpdateEvent` op; delete
enqueues `DeleteEvent`. The sync engine drains each op to Graph and reconciles.
Responsibility split:

- **datetime parser** (pure): local strings → UTC ISO.
- **mailcore/graph**: `create_event`/`update_event`/`delete_event` + the
  `EventInput` payload type.
- **mailcore/store**: local-event create/update/delete, an `event_for_send`
  read, `reconcile_event_id`, and the three new `OutboxOp`s.
- **mailcore/sync**: `SyncCommand`s + `apply_op` arms + optimistic-write commands.
- **lookxy**: the event form UI, its attendee autocomplete, and the delete confirm.

## Components

### 1. Datetime parser — pure, deterministic

`parse_local_datetime(input: &str, now_local: LocalDateTime, offset_min: i64) -> Option<String>`
returns a UTC ISO timestamp (`YYYY-MM-DDTHH:MM:SSZ`). `now_local` and
`offset_min` are passed in (not read from the clock) so the function is fully
testable. Also `parse_local_end(input, start_utc, now_local, offset_min)` for the
End field, which additionally accepts `+Nh/m/d` relative to the parsed Start.

Accepted input shapes (tried in this order; first match wins):

1. `YYYY-MM-DD HH:MM` — explicit local date + 24-hour time.
2. `YYYY-MM-DD` — that date at 00:00 local (used with the all-day flag).
3. `today HH:MM` / `tomorrow HH:MM` (also `today`/`tomorrow` alone → 00:00).
4. `HH:MM` (24-hour) → today at that time.
5. 12-hour `H[:MM]am|pm` (e.g. `2pm`, `2:30pm`) → today at that time.
6. `+Nh` / `+Nm` / `+Nd` — relative to *now* for Start, relative to *Start* for
   End (End only).

Anything else → `None`. Conversion: build the local wall-clock instant, subtract
`offset_min` minutes to get UTC, format zero-padded with a `Z` suffix (matching
the store's existing `to_utc` canonical form so lexical order == chronological).
`LocalDateTime` is a small `{year, month, day, hour, min}` struct; the date
arithmetic (add N days with month/year rollover) uses the calendar code's
existing `civil_from_days` plus its inverse `days_from_civil` — adding the
inverse if only the days→date direction exists today.

### 2. Graph client — create / update / delete

```rust
pub struct EventInput {
    pub subject: String,
    pub start_utc: String,     // YYYY-MM-DDTHH:MM:SSZ
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub attendees: Vec<(String, String)>, // (name, address)
    pub body_html: String,
}
```

- `create_event(&EventInput) -> Result<Event, GraphError>` — `POST /me/events`,
  returns the created `Event` (with its Graph id), parsed by the existing
  `Event::from_json`.
- `update_event(id, &EventInput) -> Result<(), GraphError>` — `PATCH /me/events/{id}`.
- `delete_event(id) -> Result<(), GraphError>` — `DELETE /me/events/{id}`.

Body JSON (shared by create/update):
```json
{ "subject": "...",
  "start": {"dateTime": "<start_utc without Z>", "timeZone": "UTC"},
  "end":   {"dateTime": "<end_utc without Z>",   "timeZone": "UTC"},
  "isAllDay": <bool>,
  "location": {"displayName": "..."},
  "attendees": [{"emailAddress": {"address": "...", "name": "..."}, "type": "required"}],
  "body": {"contentType": "HTML", "content": "..."} }
```
(`id` percent-encoded via `encode_path_segment`, like every other id path.) The
`dateTime` field is the UTC wall clock with `timeZone:"UTC"`, matching how
`calendar_view` reads events back with the `Prefer: outlook.timezone="UTC"`
header.

### 3. Store — local event mutation + outbox ops

- `create_local_event(input: &LocalEventFields) -> Result<String, StoreError>` —
  mints a `local:` event id, `upsert_event`s a `NewEvent` (with the input's
  fields; organizer = the signed-in user; `response_status = "organizer"`),
  `put_event_attendees`, and stores the body. Returns the id.
- `update_event_fields(id, &LocalEventFields)` — overwrites the stored event's
  editable fields + attendees + body in place.
- `delete_event(id)` — removes the event row + its attendees + body locally.
- `event_for_send(id) -> Option<(EventSendData)>` — reads back everything the
  outbox needs to build an `EventInput`: subject, start/end, all-day, location,
  attendees, body.
- `reconcile_event_id(local_id, graph_id)` — in one transaction (deferred FKs),
  re-points `events.id`, `event_attendees.event_id`, and the event body row from
  `local_id` to `graph_id` (mirrors `reconcile_id` for drafts).
- `OutboxOp::CreateEvent { id }`, `UpdateEvent { id }`, `DeleteEvent { id }` —
  added to the enum with `op_kind`, `to_json`, and `from_json` entries next to
  `RespondEvent`.

### 4. Sync — commands + apply

- `SyncCommand::{CreateEvent, UpdateEvent, DeleteEvent} { id }` — the engine
  enqueues the matching `OutboxOp` and drains (like `RespondEvent`).
- `apply_op`:
  - `CreateEvent { id }` → `event_for_send(id)` → `client.create_event(&input)`
    → `reconcile_event_id(id, created.id)` → Ok.
  - `UpdateEvent { id }` → `event_for_send(id)` → `client.update_event(id, &input)`.
  - `DeleteEvent { id }` → `client.delete_event(id)` (the local row is already
    gone from the optimistic delete; a `local:` id that never synced is dropped
    without a Graph call).
- The engine's optimistic-write path emits `SyncEvent::CalendarUpdated` so the
  agenda repaints; quarantine after `MAX_OP_ATTEMPTS` and reconverge local state
  from server truth on the next `RefreshCalendar`, exactly as `RespondEvent` does.

### 5. Event form UI — `lookxy`

`app.event_form: Option<EventForm>` where
`EventForm { editing_id: Option<String>, title, start, end, all_day: bool, location, attendees, body, focus, autocomplete: Option<Autocomplete> }`.

- **`c`** (Calendar mode) opens a blank form: `start` prefilled to the next full
  hour (local), `end` to +1h, all-day off.
- **`e`** on the selected event opens the form prefilled from it (UTC→local for
  the time fields), `editing_id = Some(id)`. Refused with a notice when the event
  is recurring (`series_master_id.is_some()`).
- Focus cycles Title → Start → End → AllDay → Location → Attendees → Body → Title
  (Tab). All-day is toggled with Space when focused.
- **Attendees** field reuses the compose autocomplete: `current_token` /
  `apply_completion` (made reachable from this module) + `store.search_contacts`,
  with the same dropdown keys (↓/↑ move, Enter/Tab accept, Esc close).
- **Ctrl-Enter** saves: parse Start (and End, which also accepts `+Nh/m/d`);
  validate (both parse; End ≥ Start); build `LocalEventFields`; for a new form
  `create_local_event` + `SyncCommand::CreateEvent`, for an edit
  `update_event_fields` + `SyncCommand::UpdateEvent`; close the form; reload the
  agenda. A parse/validation failure shows an inline error and keeps the form
  open. **Esc** cancels (discards; a never-saved local event is never created).
- Drawn over the calendar (Calendar mode) by a new `ui::eventform::draw`, and
  `ui::handle_key` routes to it (checked before the calendar key handler) when
  `event_form.is_some()`.

### 6. Delete + confirm

- **`x`** on the selected event opens the existing confirm modal
  ("Delete event '<title>'?") — refused with a notice when recurring. On confirm:
  optimistic `store.delete_event(id)` + `SyncCommand::DeleteEvent { id }` + agenda
  reload. A `local:`-only event (never synced) is just dropped locally; the
  `DeleteEvent` op no-ops its Graph call for a `local:` id.

## Data flow

```
Create:  c → form → Ctrl-Enter
         → parse+validate → store.create_local_event(local:X) → agenda shows it
         → SyncCommand::CreateEvent{local:X}
         → engine: event_for_send → client.create_event → reconcile_event_id(local:X, GRAPH) → CalendarUpdated
Edit:    e → prefilled form → Ctrl-Enter → store.update_event_fields → SyncCommand::UpdateEvent → client.update_event(PATCH)
Delete:  x → confirm → store.delete_event → SyncCommand::DeleteEvent → client.delete_event(DELETE)
```

## Error handling & edge cases

- **Invalid datetime / End < Start** → inline form error; nothing saved.
- **Editing/deleting a recurring event** → refused with a status notice; no op.
- **PATCH/DELETE of an event you don't organize** → Graph rejects → outbox
  quarantine + `error_notice`; the optimistic change reconverges from server
  truth on the next `RefreshCalendar`.
- **All-day** → when the flag is set, Start and End are taken as dates at 00:00
  local with `isAllDay:true`. Graph requires an all-day event's End date to be
  strictly after its Start date, so a single-day all-day event stores
  `End = Start + 1 day`; toggling all-day on defaults the End to a later date
  when it isn't already one.
- **A `local:` event edited before it syncs** → `update_event_fields` writes the
  `local:` row; the still-pending `CreateEvent` op carries the latest fields when
  it drains (it reads `event_for_send` at drain time, not enqueue time).
- **Delete of a `local:` event before it syncs** → dropped locally; the
  `DeleteEvent` op recognizes the `local:` id and makes no Graph call.

## Testing

**mailcore (unit):**
- Datetime parser: each accepted shape (fixed, `YYYY-MM-DD`, `today/tomorrow`,
  bare `HH:MM`, 12-hour, `+Nh/m/d`) → correct UTC; invalids → `None`; End's
  relative-to-Start.
- Graph: `create_event`/`update_event` POST/PATCH body shape (captured), including
  UTC `dateTime`+`timeZone`, attendees, all-day; `delete_event` hits `DELETE`;
  ids percent-encoded.
- Store: `create_local_event` → agenda-visible; `event_for_send` round-trips;
  `reconcile_event_id` re-points event + attendees + body; the three `OutboxOp`s
  round-trip through `to_json`/`from_json`.
- Sync: `apply_op` CreateEvent → create + reconcile; UpdateEvent → PATCH;
  DeleteEvent → DELETE (and no-op for a `local:` id).

**lookxy (unit + render):**
- Form: focus cycle incl. AllDay/Attendees; prefill on edit (UTC→local); save
  builds `CreateEvent` vs `UpdateEvent` correctly; invalid time keeps the form
  open with an error; attendee autocomplete opens/accepts.
- Recurring event: `e` and `x` refused with a notice.
- Delete: `x` → confirm → `DeleteEvent` enqueued + optimistic local delete.

## Scope boundaries (YAGNI)

- **Single events only** — no recurrence creation; recurring events RSVP-only.
- **No attachments on events**; **body is plain text** (appended as HTML text).
- **No free/busy / scheduling assistant**; **all attendees are `required`** (no
  optional/resource types).
- **No in-form timezone selection** — local time in, UTC stored, per the app's
  single-timezone assumption.
