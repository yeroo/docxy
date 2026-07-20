# lookxy event reminders / alerts — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-20.
**Builds on:** the calendar sync/agenda path (`Event`/`EventRow`,
`calendar_view`, `App::reload_agenda` on `SyncEvent::CalendarUpdated`), the
event store (`events` table, `NewEvent`/`upsert_event`/`events_in_window`), the
TUI main loop (`main::run`, a 200 ms tick), the transient-notice + status-bar
surfaces, the config file (`lookxy/src/config.rs`), and the OS-invocation seam
pattern (`open_with_os_handler` + its `open_invocations` test `Cell`).

## Goal

Surface a reminder inside lookxy when an event is starting soon — within the
event's own reminder window. Always show a **dismissible in-TUI banner**;
optionally (a config flag, default **off**) also raise an **agwinterm
notification** that overlays all terminal sessions. Read-only: no Graph writes,
no editing of reminder settings.

## Background

Graph's event representation returns `reminderMinutesBeforeStart` (int) and
`isReminderOn` (bool) **by default** (they are default-returned properties), so
`calendar_view` already receives them with no `$select` change. lookxy runs
inside agwinterm (a terminal manager); `agwintermctl notify "<msg>" --title
"<t>"` raises an in-app banner (shown over whatever session is active) plus an
OS tray balloon — exactly the "over all sessions" surface wanted, gated behind a
default-off flag.

## Product decisions (locked)

- **Two surfaces:** an in-TUI dismissible banner (always) + an agwinterm notify
  overlay (opt-in, config `reminders_notify`, default false).
- **Per-event window:** a reminder is due when `start − reminderMinutes ≤ now <
  start`, honoring the event's own `reminderMinutesBeforeStart`; only when
  `isReminderOn`.
- **Fire once:** each event id fires exactly once per session (a de-dup set).
- **Dismiss:** `Esc` pops the front banner (after the overlay handlers, so
  overlays still get `Esc` first).
- **agwinterm toggle is config-file-only** for v1 (no in-app key).
- **Read-only, no snooze, no audible bell, no missed-reminder persistence.**

## Architecture

### 1. Model + store (`mailcore`)

- `Event` gains `reminder_minutes: i64` (from `reminderMinutesBeforeStart`,
  default 0) and `is_reminder_on: bool` (from `isReminderOn`, default false),
  read in `Event::from_json`.
- `EventRow` gains the same two fields (so the agenda rows carry them).
- Store: `events.reminder_minutes INTEGER NOT NULL DEFAULT 0` and
  `events.is_reminder_on INTEGER NOT NULL DEFAULT 0` columns (idempotent
  `ALTER TABLE … ADD COLUMN` migrations, same pattern as `recurrence`).
  `NewEvent` gains the two fields; `upsert_event` writes them; `events_in_window`
  reads them into `EventRow` (and `NewEvent::from(&Event)` copies them from the
  parsed event).

### 2. Reminder check (`lookxy/src/app.rs`)

- `App::alerted_reminders: std::collections::HashSet<String>` — event ids
  already alerted this session.
- `App::reminder_queue: std::collections::VecDeque<String>` — pending banner
  lines (front = currently shown).
- `pub fn check_due_reminders(&mut self, now_epoch: i64)`:
  - For each `EventRow` in `self.agenda` with `is_reminder_on`:
    let `start = utc_to_epoch(&e.start_utc)`, `remind_at = start -
    e.reminder_minutes.max(0) * 60`.
    If `remind_at <= now_epoch && now_epoch < start && !alerted.contains(&e.id)`:
    insert the id into `alerted`, push
    `format!("⏰ {} {}", e.subject, starts_in_phrase(now_epoch, start, &e.start_utc))`
    onto `reminder_queue`, and — when `self.reminders_notify` is true — call
    `self.notify_agwinterm(&msg)`.
  - `starts_in_phrase`: `"starts now"` when `now_epoch >= start`; else
    `"starts in {mins} min ({HH:MM})"` where `mins = ((start - now)/60).max(1)`
    and `HH:MM` is the event's local start time (reuse `ui::calendar::to_local`).
- `utc_to_epoch(iso: &str) -> i64`: a free fn using
  `ui::calendar::{date_of_utc, days_from_civil}` for the date and the `T`-split
  `HH:MM:SS` for the time: `days_from_civil(y,m,d) * 86400 + h*3600 + m*60 + s`.
- Called once per main-loop tick from `main::run`:
  `app.check_due_reminders(now_unix_secs())` where `now_unix_secs()` reads
  `SystemTime::now()`. Cheap (in-memory scan over the loaded agenda).
- `App::dismiss_reminder()`: `reminder_queue.pop_front()`.

### 3. In-TUI banner (`lookxy/src/ui/mod.rs`)

- At the top of `ui::draw`, when `!app.reminder_queue.is_empty()`, split a
  1-row banner off the top of `f.area()` and render it (the front entry, plus
  ` (+{n} more)` when `reminder_queue.len() > 1`, and a `  [Esc to dismiss]`
  hint); the remaining area is what every existing branch (Mail panes, Calendar,
  compose, OOF) lays out against — computed once as a local `area` and passed
  down instead of `f.area()`.
- `ui::handle_key`: after the full-screen overlay handlers (signin / oof /
  category picker / file picker / compose / event form / confirm) and before
  the mode/pane handling, add: `if key == Esc && !app.reminder_queue.is_empty()
  { app.dismiss_reminder(); return; }`. (Overlays keep `Esc` priority; the
  banner's `Esc` only acts when no overlay is capturing it.)

### 4. agwinterm overlay (`lookxy/src/app.rs` + `config.rs`)

- `Config` gains `reminders_notify: bool` (default false), read from the
  config file (`reminders_notify = true|false`, same key/value parsing as the
  existing `threaded` flag) and exposed on `App` (e.g. `self.reminders_notify`
  loaded at construction).
- `App::notify_agwinterm(&self, msg: &str)`: best-effort. In production, only
  when `std::env::var("AGWINTERM_ENABLED").as_deref() == Ok("1")`, spawn
  `agwintermctl notify {msg} --title lookxy` via `std::process::Command`
  (argv, no shell — same safety posture as `open_with_os_handler`), ignoring
  the result. A `#[cfg(test)]` `Cell<u32>` (`agwinterm_notify_invocations`)
  counts calls so tests assert the decision without spawning.
- `check_due_reminders` calls `notify_agwinterm` only when `reminders_notify`
  is true — so the flag gates the overlay; the banner is unconditional.

## Data flow

```
calendar sync → CalendarUpdated → reload_agenda (agenda rows carry reminder fields)
each main-loop tick (200ms):
  check_due_reminders(now_epoch)
    → for each agenda event with reminder on, window contains now, not yet alerted:
        alerted.insert(id); reminder_queue.push_back("⏰ … starts in N min (HH:MM)")
        if reminders_notify: notify_agwinterm(msg)   [agwintermctl notify … over all sessions]
draw: top banner shows reminder_queue.front() (+N more)
Esc: dismiss_reminder → pop_front
```

## Error handling & edge cases

- **`isReminderOn` false** → never fires.
- **Window already elapsed while lookxy was closed** (`now >= start`) → not
  fired ("starting soon" only pre-start; the `now < start` guard).
- **`reminder_minutes <= 0`** → `remind_at == start`, fires right at start
  (phrase "starts now" / "starts in 1 min").
- **All-day events** → `start_utc` is the day's midnight; the same window math
  applies (Graph's `reminderMinutesBeforeStart` for all-day is minutes before
  that midnight).
- **agwintermctl missing / not inside agwinterm** → the spawn (or the
  `AGWINTERM_ENABLED` guard) no-ops; the banner still shows.
- **Duplicate fire** prevented by `alerted` (keyed by event id; survives
  agenda reloads within the session).
- **Local-created event before sync** → its `reminder_minutes`/`is_reminder_on`
  are defaults (0/false) until Graph sync returns the real values, so it won't
  spuriously fire pre-sync.

## Testing

**mailcore (unit):**
- `Event::from_json` reads `reminderMinutesBeforeStart`/`isReminderOn` (present
  and absent → 0/false).
- Store: `upsert_event`/`events_in_window` round-trip the two fields; migrations
  idempotent.

**lookxy (unit):**
- `utc_to_epoch` matches a known timestamp (e.g. `1970-01-01T00:00:00Z` → 0;
  a fixed date → its epoch).
- `check_due_reminders`: an agenda event with `is_reminder_on`, `reminder=15`,
  `start` 5 min ahead → pushes one banner and marks alerted; a second call at
  the same `now` pushes nothing (de-dup).
- Does not fire for: `is_reminder_on = false`; `now` before the window; `now`
  at/after `start`.
- `notify_agwinterm` seam: with `reminders_notify = true`, a fired reminder
  increments the invocation `Cell`; with it false, the counter stays 0 (banner
  still pushed).
- `dismiss_reminder` pops the front; the banner then shows the next queued
  reminder (or none).
- Banner render (`TestBackend`): the front reminder text and `(+1 more)` show
  when two are queued.

## Scope boundaries (YAGNI)

- **Read-only** — no editing an event's reminder minutes / on-off, no snooze,
  no "remind me again".
- **No audible bell**, no missed-reminder persistence across restarts.
- **agwinterm toggle is config-file-only** (no in-app key) in v1.
- **No per-reminder sound/blink** via agwinterm (plain `notify` only).
