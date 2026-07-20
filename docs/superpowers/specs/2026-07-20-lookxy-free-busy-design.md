# lookxy free/busy lookup — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-20.
**Builds on:** the event form (`ui/eventform.rs`, `App::event_form`,
`parse_attendee_pairs`), the Graph client (`send`/`parse_body`/`value_array`),
the direct-call sync pattern (`fetch_body`/`respond_meeting` — a Graph call
gated by `with_auth`/`react`, no store write), the local-datetime parser
(`datetime::parse_start`), the account email (`App::account`), and the centered
popup overlay pattern (`ui::centered_rect`, `ui::calendar::draw_rsvp_prompt`).

## Goal

From the event-create/edit form, look up the attendees' (and the organizer's
own) availability for the event's day — business hours 08:00–18:00, 30-minute
slots — via Graph `getSchedule`, and show a **read-only** busy/free grid (one
row per person plus a combined "everyone free" row) so the user can pick a
working slot and type it into the form.

## Background

`POST /me/calendar/getSchedule`:

```json
{ "schedules": ["me@x", "alice@x"],
  "startTime": {"dateTime": "2026-07-21T08:00:00", "timeZone": "UTC"},
  "endTime":   {"dateTime": "2026-07-21T18:00:00", "timeZone": "UTC"},
  "availabilityViewInterval": 30 }
```

returns per schedule an `availabilityView` string — one digit per interval slot:
`0` free, `1` tentative, `2` busy, `3` out-of-office, `4` working-elsewhere. For
08:00–18:00 at 30-min slots that's 20 characters. lookxy renders each digit as a
glyph.

## Product decisions (locked)

- **Read-only** — the grid informs; the user types the Start they pick. No
  slot-pick auto-fill.
- **Fixed window** — the form's Start **date**, 08:00–18:00 local, 30-min slots.
- **Trigger** — `Ctrl-B` in the event form.
- **Rows** — one per resolved email (organizer first) + a combined `free?` row.
- **Start-day only** — no multi-day / next-week; no `findMeetingTimes`
  suggestions.

## Architecture

### 1. Model + client (`mailcore`)

- `ScheduleEntry { email: String, availability: String }` in
  `graph/model.rs`; `availability` is the raw `availabilityView` digit string.
  A `from_json` reads `scheduleId` → `email` and `availabilityView` →
  `availability` (absent → `""`).
- `GraphClient::get_schedule(&self, schedules: &[String], start_utc: &str,
  end_utc: &str, interval_minutes: i64) -> Result<Vec<ScheduleEntry>, GraphError>`:
  `POST /me/calendar/getSchedule` with the body above (start/end each
  `{dateTime: <utc-without-Z>, timeZone: "UTC"}` — same shape as
  `event_body_json`'s `dt` helper, trimming the trailing `Z`); parse the `value`
  array via `ScheduleEntry::from_json`.

### 2. Sync (`mailcore/src/sync/engine.rs`)

- `SyncCommand::FetchSchedule { schedules: Vec<String>, start_utc: String,
  end_utc: String, interval_minutes: i64 }`.
- Handler `fetch_schedule` (direct call, signed-in guard like `fetch_body`):
  `with_auth(|c| c.get_schedule(...))`; on Ok emit
  `SyncEvent::ScheduleFetched { entries: Vec<ScheduleEntry> }`, else `react(e)`.
- `SyncEvent::ScheduleFetched { entries: Vec<ScheduleEntry> }`.

### 3. App (`lookxy/src/app.rs`)

- `FreeBusyView` (new; defined in `ui/freebusy.rs`):
  ```rust
  pub struct FreeBusyView {
      pub day_label: String,      // "Mon Jul 21" (display)
      pub interval_minutes: i64,  // 30
      pub slot_count: usize,      // (18-8)*60/30 = 20
      pub entries: Vec<mailcore::graph::model::ScheduleEntry>,
      pub loading: bool,
  }
  ```
  held as `App::free_busy: Option<FreeBusyView>`.
- `App::open_free_busy()` — bound to `Ctrl-B` in `eventform::handle_key`:
  - Collect emails: `self.account` (the organizer, if set) first, then the
    addresses from `parse_attendee_pairs(&form.attendees)` (deduped, non-empty).
  - Compute the window from the form's Start **date**: take the first 10 chars
    of `form.start` when it looks like `YYYY-MM-DD…`, else today's local date
    (`local_now`); build `"<date> 08:00"` / `"<date> 18:00"` and parse each via
    `datetime::parse_start(_, now, off)` to canonical UTC. `day_label` from the
    date via `ui::calendar` civil helpers (weekday + month abbrev + day).
  - Send `SyncCommand::FetchSchedule { schedules, start_utc, end_utc,
    interval_minutes: 30 }`; open `free_busy` with `loading = true`,
    `slot_count = 20`.
  - If the emails list is empty (no account, no attendees), still open with the
    organizer omitted — the grid just shows the `free?` row over an empty set
    (or a "no attendees" note); a fetch is still sent with whatever emails exist.
- `on_sync_event` `ScheduleFetched { entries }` → if `free_busy` is open, set
  `entries`, `loading = false`.
- `close_free_busy()` (Esc) → `free_busy = None`.
- Glyph helpers (free functions, `pub(crate)`):
  - `slot_glyph(c: char) -> char`: `'0'`→`'·'`, `'1'`→`'▓'`, `'2'|'3'|'4'`→`'█'`,
    else `' '`.
  - `combined_glyph(entries: &[ScheduleEntry], slot: usize) -> char`: `'✓'` when
    every entry's char at `slot` is `'0'` (all free), `'█'` when any is
    `'2'|'3'|'4'` (someone busy), else `'░'`. A shorter `availability` string
    counts as free (`'0'`) past its end.

### 4. UI (`lookxy/src/ui/freebusy.rs` + `ui/mod.rs`)

- `freebusy::draw(f, app)` — no-op unless `app.free_busy` is open. A centered
  overlay (`Clear` + bordered block, `centered_rect(80, 60, …)`). Title:
  `Availability — {day_label} (08:00–18:00)  [Esc: back]`. While `loading`,
  render "loading…". Otherwise: an hour-tick header row (`08 09 … 17`), one row
  per `entry` (`email` truncated to a fixed prefix width, then
  `slot_glyph` over the slots), and a final `free?` row of `combined_glyph`.
- `freebusy::handle_key(app, key)`: `Esc` → `app.close_free_busy()`; other keys
  ignored (read-only).
- `ui::handle_key`: route `if app.free_busy.is_some() { freebusy::handle_key;
  return; }` ahead of the event-form handler (the grid overlays the open form).
- `ui::draw` (calendar branch): call `freebusy::draw` after `eventform::draw`
  so it sits on top of the form.
- `eventform::handle_key`: `Ctrl-B` → `app.open_free_busy()` (checked with the
  other `Ctrl` combos, before the plain-char field editing).

## Data flow

```
event form (attendees + Start filled) → Ctrl-B
  → open_free_busy: emails = [account, ...attendee addrs]; window = Start-date 08:00–18:00 UTC
  → FetchSchedule{schedules, start_utc, end_utc, 30} ; free_busy = Some(loading)
  → engine: get_schedule  [POST /me/calendar/getSchedule]
          → ScheduleFetched{entries}
  → free_busy.entries filled, loading = false
  → grid renders per-attendee strips + free? row
  → Esc → close_free_busy → back to the form (user types the chosen Start)
```

## Error handling & edge cases

- **getSchedule fails** (auth/throttle/offline/4xx) → `react(e)` (standard
  handling; a 4xx surfaces `SyncEvent::Error`). The open view clears `loading`
  on the `Error` arm so it shows an empty grid rather than a stuck "loading…".
- **No attendees / no account** → the fetch carries whatever emails exist
  (possibly just the organizer, or empty); the grid shows the available rows +
  the `free?` row. An empty schedules list yields an empty grid with a
  "(no attendees)" note.
- **`availabilityView` length ≠ slot count** → render up to `slot_count`;
  missing trailing chars count as free. No panic (`chars().nth(slot)`).
- **Invalid/empty Start date** → fall back to today's local date for the window.
- **Not signed in** → the engine's signed-in guard emits `SyncEvent::Error`;
  the view clears loading.

## Testing

**mailcore (unit):**
- `ScheduleEntry::from_json` reads `scheduleId`/`availabilityView`.
- `get_schedule` (FakeServer POST): the request body has `schedules`,
  `availabilityViewInterval`, and `startTime.dateTime`; the parsed entries carry
  the `availabilityView` string.
- Engine `FetchSchedule` emits `ScheduleFetched` with the parsed entries
  (mirror the OOF-fetch engine test harness).

**lookxy (unit):**
- `open_free_busy` from a form with attendees + a Start date sends
  `FetchSchedule` whose `schedules` include the account + attendee addresses and
  whose `start_utc`/`end_utc` are the 08:00/18:00 UTC bounds; opens a loading
  view.
- `ScheduleFetched` fills `entries` and clears `loading`.
- `slot_glyph`: `'0'`→`'·'`, `'2'`→`'█'`, `'1'`→`'▓'`. `combined_glyph`: all-`'0'`
  → `'✓'`; a `'2'` present → `'█'`; a short string treats the tail as free.
- `freebusy::draw` renders an attendee email prefix and the `free?` row
  (`TestBackend`); `Esc` (`freebusy::handle_key`) closes the view.

## Scope boundaries (YAGNI)

- **Read-only** — no slot-pick auto-fill into the form.
- **Fixed 08:00–18:00 / 30-min** window — not configurable.
- **Start-day only** — no multi-day, next-week, or paging.
- **No `findMeetingTimes`** suggestions — just the raw availability grid.
- **No caching** — each `Ctrl-B` re-fetches.
