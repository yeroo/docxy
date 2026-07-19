# lookxy recurring event creation — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-20.
**Builds on:** the event-create pipeline — the event form
(`ui/eventform.rs` + `App::open_new_event`/`save_event_form`), the local-event
store path (`LocalEventFields` → `Store::create_local_event` →
`Store::event_for_send`), the `CreateEvent` outbox op
(`sync::outbox::event_input_for` → `GraphClient::create_event` →
`event_body_json`), and the local-datetime parser (`datetime::parse_start`,
`ui::calendar` date helpers).

## Goal

Let the user create **recurring** events from lookxy: a recurrence pattern
(daily / weekly / monthly), an interval, weekday selection (weekly), and an end
date, serialized to Graph's `event.recurrence` on create. **Create-only** —
editing a series, per-occurrence edits, and deleting a series stay out of scope
(deletion already refuses recurring events).

## Background

Graph's `event.recurrence` is:

```json
"recurrence": {
  "pattern": {
    "type": "daily" | "weekly" | "absoluteMonthly",
    "interval": 1,
    "daysOfWeek": ["monday", "wednesday"],   // weekly
    "firstDayOfWeek": "sunday",              // weekly
    "dayOfMonth": 15                         // absoluteMonthly
  },
  "range": {
    "type": "noEnd" | "endDate",
    "startDate": "2026-07-20",
    "endDate": "2026-12-31"                  // endDate only
  }
}
```

lookxy already reads recurring events back (`calendarView` returns each
occurrence, with `seriesMasterId` set); it just can't create them. The create
pipeline threads a set of fields from the form all the way to `event_body_json`,
so recurrence follows the same path.

## Product decisions (locked)

- **Types:** daily / weekly / **absolute**monthly. No yearly, no relative
  ("2nd Tuesday") monthly.
- **Weekly = specific weekdays:** a Mon–Sun multi-select (toggled with keys
  `1`–`7` while the Days field is focused). If none are picked, defaults to the
  start date's weekday (always valid).
- **Interval:** a positive integer (default 1).
- **End:** an `until` date → `endDate`; blank → `noEnd`. No "after N
  occurrences".
- **Create-only:** the recurrence fields take effect only when creating; on an
  edit they're ignored.
- **Detail marker:** a `↻ repeats` line in the event detail view when an event
  is part of a series (`series_master_id` present).

## Architecture

### 1. Recurrence model (`mailcore/src/graph/model.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceKind { Daily, Weekly, Monthly }

#[derive(Debug, Clone, PartialEq)]
pub struct Recurrence {
    pub kind: RecurrenceKind,
    pub interval: u32,                // >= 1
    pub days_of_week: Vec<String>,    // "monday".."sunday" (weekly)
    pub day_of_month: u32,            // absoluteMonthly, from the start date
    pub start_date: String,           // "YYYY-MM-DD" (range.startDate)
    pub until: Option<String>,        // "YYYY-MM-DD" (range endDate), None = noEnd
}
```

- `Recurrence::to_json(&self) -> Value` builds the Graph shape above.
  - `pattern.type`: `daily`/`weekly`/`absoluteMonthly` per `kind`.
  - Weekly adds `daysOfWeek` (the list) and `firstDayOfWeek: "sunday"`.
  - Monthly adds `dayOfMonth`.
  - `range.type`: `endDate` when `until.is_some()`, else `noEnd`; always
    `startDate`; `endDate` only when set.
- `Recurrence::from_json(&Value) -> Option<Self>` — the inverse, for the store
  round-trip (maps `type` back to `kind`, reads interval/daysOfWeek/dayOfMonth/
  startDate/endDate). Used by `event_for_send`.

### 2. Pipeline threading

- `GraphClient::EventInput.recurrence: Option<Recurrence>`;
  `event_body_json` inserts a `"recurrence"` key (from `rec.to_json()`) only
  when `Some`. `create_event`/`update_event` are unchanged otherwise.
- `Store::LocalEventFields.recurrence: Option<Recurrence>`.
- Store: an `events.recurrence TEXT` column (idempotent `ALTER TABLE … ADD
  COLUMN … DEFAULT ''` migration, same pattern as `body_html`), holding
  `rec.to_json().to_string()` (or `""` when none). `create_local_event` writes
  it; `event_for_send` reads it back (`Recurrence::from_json(&json::parse(s))`,
  `None` when `""`/unparseable) into the returned fields.
- `sync::outbox::event_input_for` copies the stored recurrence into `EventInput`.
- `update_event`'s path is unchanged: on an edit, `save_event_form` never sets
  recurrence, so a stored non-recurring event's `recurrence` stays `""`/`None`.

### 3. Event form (`ui/eventform.rs` + `App::save_event_form`/`open_new_event`)

New `EventForm` fields and `EventField` variants:

```
Repeat:  ( )None (x)Weekly ...     ← Space cycles None → Daily → Weekly → Monthly
Every:   1                         ← interval, numeric text (default "1")
Days:    [x]Mon [ ]Tue [x]Wed ...  ← keys 1–7 toggle Mon–Sun; shown only for Weekly
Until:   2026-12-31                ← date text, blank = no end
```

- `EventForm` gains: `repeat: RepeatChoice` (`None`/`Daily`/`Weekly`/`Monthly`),
  `interval: String` (default `"1"`), `days: [bool; 7]` (Mon…Sun), `until:
  String`. `open_new_event` initializes them (`None`, `"1"`, all-false, `""`).
- Layout (top→bottom): Title / Start / End / All-day / **Repeat / Every / Days /
  Until** / Location / Attendees / Body / footer. `EventField` gets `Repeat`,
  `Interval`, `Days`, `Until`; `next_field`/`prev_field` include them.
- Key handling in the form: `Space` on Repeat cycles the choice; `1`–`7` on Days
  toggle `days[0..7]`; other text fields (Interval, Until) edit as usual.
  Interval/Days/Until render dimmed when Repeat=None; Days additionally dimmed
  unless Repeat=Weekly.
- `save_event_form`: after the existing time parse, when `repeat != None` **and**
  `editing_id.is_none()`, build a `Recurrence`:
  - `kind` from `repeat`.
  - `interval` = parse `interval` as `u32` ≥ 1 (else inline error
    "Invalid interval").
  - `start_date` = the start's date (`YYYY-MM-DD`, from the parsed `start_utc`
    via `ui::calendar::date_of_utc` formatted, matching how the event's own
    start date is derived).
  - `day_of_month` = the start date's day (for Monthly).
  - `days_of_week` (Weekly) = the toggled `days` mapped to
    `"monday".."sunday"`; if empty, `[start weekday]`
    (`ui::calendar` weekday-of-date helper).
  - `until` = if `until` non-empty, parse it as a date (`YYYY-MM-DD`; inline
    error "Invalid until date" on failure) and require it ≥ `start_date`
    (else "Until is before start"); blank → `None`.
  - Attach `Some(recurrence)` to `LocalEventFields`. On an edit, or Repeat=None,
    `recurrence` is `None`.

### 4. Display (`ui/calendar.rs`)

In the event detail view, when the selected event's `series_master_id` is
`Some`, add a `↻ repeats` line (the model already carries `series_master_id`
for synced occurrences — no new fetch). The agenda list rows are unchanged.

## Data flow

```
open new event (c) → fill Repeat/Every/Days/Until → Ctrl-Enter
  → save_event_form: parse times; build Recurrence (create only)
  → LocalEventFields{…, recurrence: Some(rec)}
  → Store::create_local_event  [events.recurrence = rec.to_json().to_string()]
  → CreateEvent outbox op
  → event_input_for: event_for_send reads recurrence back → EventInput.recurrence
  → create_event → event_body_json inserts "recurrence"
  → Graph creates the series; next calendarView sync returns the occurrences
    (each with seriesMasterId) → agenda shows them; detail shows ↻ repeats
```

## Error handling & edge cases

- **Invalid interval** (non-numeric, 0) → inline "Invalid interval", nothing sent.
- **Invalid / past `until`** → inline error, nothing sent.
- **Weekly, no days toggled** → defaults to the start's weekday (valid).
- **Monthly on day 29–31** → Graph natively skips months without that day (no
  client handling).
- **Edit of an existing event** → recurrence fields ignored; the stored
  `recurrence` column is untouched.
- **All-day recurring** → valid; recurrence and `isAllDay` coexist.
- **Optimistic local view** → the local store holds a single event until the
  next calendar sync pulls the real occurrences; consistent with how
  create-event already behaves.

## Testing

**mailcore (unit):**
- `Recurrence::to_json`: daily (interval only), weekly (`daysOfWeek` +
  `firstDayOfWeek`), monthly (`dayOfMonth`); `range` noEnd vs endDate.
- `Recurrence::from_json` round-trips each; unknown `type` → `None`.
- `event_body_json` includes `recurrence` when `Some`, omits it when `None`.
- Store: `create_local_event` with a recurrence, then `event_for_send` returns
  it (`from_json` round-trip); migration idempotent; a non-recurring event round-
  trips to `None`.
- `event_input_for` (outbox) carries the stored recurrence into `EventInput`.

**lookxy (unit):**
- `save_event_form` with Repeat=Weekly + days {Mon,Wed} + interval 2 + until
  builds a `Recurrence { kind: Weekly, interval: 2, days_of_week:
  ["monday","wednesday"], until: Some(..) }` on the `CreateEvent` path.
- Repeat=Monthly derives `day_of_month` from the start; Repeat=Daily has empty
  days.
- Invalid interval / invalid until → inline error, no `CreateEvent` sent.
- Weekly with no days → defaults to the start's weekday.
- An **edit** with Repeat set does not attach recurrence.
- The event form renders the Repeat/Every/Days/Until rows; the detail view
  renders `↻ repeats` for a series occurrence (`TestBackend`).

## Scope boundaries (YAGNI)

- **Create-only** — no editing/deleting a series, no per-occurrence edits.
- **Types:** daily / weekly / absoluteMonthly only.
- **End:** until-date or no-end (no occurrence count).
- **No relative monthly / yearly**, no `firstDayOfWeek` configurability (fixed
  `"sunday"`).
