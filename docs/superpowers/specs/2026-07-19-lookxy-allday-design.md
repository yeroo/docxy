# lookxy all-day events — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-19.
**Builds on:** lookxy v6 (calendar create/edit/delete — event form, `datetime` parser, `EventInput`, optimistic+outbox) and v2 (calendar read/agenda/detail).

## Goal

Make all-day events work end-to-end: a checked "all day" event **creates
successfully on Graph** (currently rejected, so it silently quarantines), and
all-day events **display correctly by date** (currently a single-day all-day
event is mislabelled "(multi-day)" and its date can shift under timezone
conversion — bugs that affect read-back all-day events too, not just newly
created ones).

## Background — why it's broken today

- **Create fails.** The event form's save sends `isAllDay:true` but with the
  Start/End parsed as *timed* instants (local→UTC), which are not the
  midnight, whole-day boundaries Graph requires for an all-day event → Graph
  400 → the `CreateEvent` op quarantines. (After the v6 final-review fix it
  fails *visibly*, but still doesn't create.)
- **Display is wrong.** `calendar.rs::is_multi_day` compares `to_local(start)`
  vs `to_local(end)`; Graph's all-day End is the **exclusive next-day**
  midnight, so a one-day all-day event (end = start + 1 day) is flagged
  "(multi-day)". And the agenda buckets events by `to_local(start_utc)`'s day,
  which shifts an all-day event's date for some UTC offsets.

## Product decision (locked)

**The End field is the last inclusive day** (Outlook-style). Start `2026-07-20`
+ End `2026-07-20` = a one-day event; End `2026-07-22` = a three-day event
(July 20–22). Toggling all-day ignores any time typed in the Start/End fields —
only the date is used.

## Architecture

Represent an all-day event as a **floating date range** stored at nominal
midnight-UTC of the picked local date(s), with End stored as the **exclusive
next-day** midnight (Graph's convention):

- `start_utc = "{start_date}T00:00:00Z"`
- `end_utc   = "{end_last_day + 1 day}T00:00:00Z"`

The dates are floating — NOT offset-converted — because an all-day date is
absolute (July 20 is July 20 regardless of timezone). This single
representation is both what Graph accepts on create and exactly what
`calendar_view` returns for read-back (all-day events come back as midnight
`dateTime` under `Prefer: outlook.timezone="UTC"`), so created and read all-day
events are identical in the store and render through the same code.

## Components

### 1. Datetime — all-day bounds (`lookxy/src/datetime.rs`)

```rust
pub fn all_day_bounds(start_input: &str, end_input: &str, now: LocalDateTime) -> Option<(String, String)>
```
`now` is passed in (not read from the clock) to keep the function pure/testable
and to resolve `today`/`tomorrow`, like the other parsers.
- Parses the **date** from each field (a `YYYY-MM-DD` prefix, or the date part
  of a `YYYY-MM-DD HH:MM` string; `today`/`tomorrow` resolve via `now`).
  Ignores any time component.
- `start_date` = the Start field's date. `end_last_day` = the End field's date
  if it parses and is ≥ `start_date`, else `start_date` (single day).
- Returns `("{start_date}T00:00:00Z", "{end_last_day plus 1 day}T00:00:00Z")`.
- Returns `None` if the Start date can't be parsed.
- Date arithmetic (`+1 day`, month/year rollover) reuses the existing
  `civil_from_days`/`days_from_civil`.

### 2. Save (`lookxy/src/app.rs` `save_event_form`)

When `form.all_day` is set, compute the bounds via
`datetime::all_day_bounds(&form.start, &form.end, local_now())` instead of
`parse_start`/`parse_end`; on `None`, set the inline form error and keep the
form open (same as the timed path). Everything downstream is unchanged: the
stored `LocalEventFields` carries `is_all_day = true` and these UTC boundaries,
and `graph::client::event_body_json` already emits `isAllDay:true`,
`timeZone:"UTC"`, and the midnight `dateTime` values — which Graph now accepts.

### 3. Display (`lookxy/src/ui/calendar.rs`)

For `is_all_day` events, use the **date part of `start_utc`/`end_utc`
directly** (no `to_local`), and treat End as exclusive:

- **Day grouping / date bucketing:** an all-day event's day is the date in its
  stored `start_utc` (e.g. `2026-07-20`), not `to_local(start_utc)`'s day — so
  the date never shifts under offset conversion. (Timed events keep using
  `to_local`.)
- **`is_multi_day`:** for an all-day event, compare `start_date` vs
  `end_date − 1 day` (the last inclusive day). A one-day all-day
  (`end = start + 1`) → same day → NOT multi-day. A 3-day all-day
  (`end = start + 3`) → different → "(multi-day)" as before. Timed events keep
  their existing `to_local` comparison.
- The `"all day"` time label (calendar.rs:218) is unchanged.

Introduce a small helper to extract the `YYYY-MM-DD` date from a stored UTC
timestamp (the substring before `T`) so grouping and `is_multi_day` share it.

## Data flow

```
Create all-day:  form (all_day=true, start "2026-07-20", end "2026-07-20")
  → all_day_bounds → ("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
  → LocalEventFields{is_all_day:true, ..} → create_local_event → CreateEvent
  → event_body_json: isAllDay:true, start/end midnight, timeZone:"UTC" → Graph accepts
Display:  is_all_day → bucket under 2026-07-20 (date part), is_multi_day compares
          start=2026-07-20 vs end−1day=2026-07-20 → same → not multi-day
```

## Error handling & edge cases

- **Unparseable Start date** → inline form error; nothing saved (same UX as timed).
- **End before Start** (or blank End) → single-day event (`end = start + 1`).
- **Toggling all-day OFF** → save reverts to the timed `parse_start`/`parse_end`
  path, unchanged.
- **Read-back all-day events** (from Graph, or created here) → the shared
  `is_multi_day` + grouping fixes render them correctly by date; this corrects
  the pre-existing mislabel/shift for read all-day events too.
- **Offset independence** → because all-day storage and display both use the
  floating date (never `to_local`), the displayed date is correct for both
  positive and negative UTC offsets.

## Testing

**Datetime (unit):**
- `all_day_bounds`: single day (`start==end`) → `[date, date+1)`; multi-day
  → `[start, end_last_day+1)`; End < Start → single day; `tomorrow` resolves;
  unparseable Start → `None`; month/year rollover (Dec 31 → Jan 1).

**App (unit):**
- Saving an all-day form stores an event with midnight boundaries and
  `end = start + 1 day` for a single day, `is_all_day = true`, and enqueues
  `CreateEvent`; an all-day form with an unparseable Start keeps the form open
  with an error.

**Calendar display (unit):**
- A single-day all-day event is **not** `is_multi_day`; a 3-day all-day event
  **is**.
- An all-day event buckets under its stored start date — asserted with BOTH a
  positive and a negative `local_offset_minutes` (so the no-shift is real,
  not a coincidence of the tester's timezone). If the day-bucketing helper
  can't be unit-tested without the real offset, test the shared date-extract +
  `is_multi_day` helpers directly and cover bucketing via a render/agenda test.

## Scope boundaries (YAGNI)

- **Only single-timezone all-day** — no per-event timezone; floating dates.
- **No recurrence** (recurring all-day events remain RSVP-only, per v6).
- **No change to timed events** — their parse/display paths are untouched.
- **No all-day-specific UI affordance** beyond the existing "all day" label and
  the AllDay checkbox already in the form.
