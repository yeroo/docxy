# lookxy All-Day Events Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make all-day calendar events create successfully on Graph and display correctly by date (fixing the create-rejection and the "(multi-day)" mislabel / date-shift that also affect read-back all-day events).

**Architecture:** Model an all-day event as a floating date range stored at nominal midnight-UTC of the picked local date(s), with End the exclusive next-day midnight (Graph's convention). A new `all_day_bounds` parser produces these; `save_event_form` uses it when all-day is checked; and the agenda display derives an all-day event's day from the stored date part (not `to_local`), treating End as exclusive.

**Tech Stack:** Rust (edition 2024, MSRV 1.88), ratatui 0.29. No new dependencies.

## Global Constraints

- **Build/test ONLY through the wrapper** (bare `cargo` fails with os error 448). Every command is `bash "$LCARGO" …` where
  `LCARGO = C:\Users\BORIS_~1\AppData\Local\Temp\claude\C--Users-boris-kudriashov-Source-docxy\1da9a016-b606-4432-8951-6d73bb91c967\scratchpad\lcargo.sh`
  Run it via the Bash tool with `dangerouslyDisableSandbox: true`.
- **No new dependencies.** Reuse the existing `datetime`/`civil_from_days`/`days_from_civil` helpers.
- **MSRV 1.88, edition 2024.** clippy `-D warnings` clean on ubuntu/macos/windows. Run `bash "$LCARGO" fmt` before every commit.
- **Preserve timed-event behavior.** Timed events' parse (`parse_start`/`parse_end`) and display (`to_local`) paths are UNCHANGED — only `is_all_day` events take the new branches.
- **All-day storage is a FLOATING date range**, never offset-converted: `start_utc = "{start_date}T00:00:00Z"`, `end_utc = "{last_inclusive_day + 1 day}T00:00:00Z"` (exclusive). Format is the canonical `YYYY-MM-DDTHH:MM:SSZ`.
- **End field = last inclusive day** (Outlook-style): Start `2026-07-20` + End `2026-07-20` → one day; End `2026-07-22` → three days.

---

### Task 1: `all_day_bounds` datetime helper

**Files:**
- Modify: `lookxy/src/datetime.rs` (add `all_day_bounds` + tests)

**Interfaces:**
- Consumes: the module's existing private `parse_local(input, now) -> Option<LocalDateTime>`, `civil_from_days`, `days_from_civil`, `LocalDateTime`.
- Produces: `pub fn all_day_bounds(start_input: &str, end_input: &str, now: LocalDateTime) -> Option<(String, String)>` — `(start_utc, end_utc)`, both `YYYY-MM-DDT00:00:00Z`, end exclusive.

- [ ] **Step 1: Write the failing tests**

Add to `lookxy/src/datetime.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn all_day_bounds_single_day_is_start_to_next_midnight() {
        let now = LocalDateTime { year: 2026, month: 7, day: 19, hour: 10, min: 0 };
        // same start/end date → a one-day event: [date, date+1)
        assert_eq!(
            all_day_bounds("2026-07-20", "2026-07-20", now),
            Some(("2026-07-20T00:00:00Z".into(), "2026-07-21T00:00:00Z".into()))
        );
        // a blank/unparseable End also means a single day
        assert_eq!(
            all_day_bounds("2026-07-20", "", now),
            Some(("2026-07-20T00:00:00Z".into(), "2026-07-21T00:00:00Z".into()))
        );
    }

    #[test]
    fn all_day_bounds_multi_day_end_is_last_day_plus_one() {
        let now = LocalDateTime { year: 2026, month: 7, day: 19, hour: 10, min: 0 };
        // 20th..=22nd inclusive → stored end = the 23rd (exclusive)
        assert_eq!(
            all_day_bounds("2026-07-20", "2026-07-22", now),
            Some(("2026-07-20T00:00:00Z".into(), "2026-07-23T00:00:00Z".into()))
        );
    }

    #[test]
    fn all_day_bounds_end_before_start_collapses_to_single_day() {
        let now = LocalDateTime { year: 2026, month: 7, day: 19, hour: 10, min: 0 };
        assert_eq!(
            all_day_bounds("2026-07-20", "2026-07-18", now),
            Some(("2026-07-20T00:00:00Z".into(), "2026-07-21T00:00:00Z".into()))
        );
    }

    #[test]
    fn all_day_bounds_ignores_time_and_handles_rollover_and_relative() {
        let now = LocalDateTime { year: 2026, month: 12, day: 31, hour: 10, min: 0 };
        // time part ignored; Dec 31 single day → end rolls to Jan 1 next year
        assert_eq!(
            all_day_bounds("2026-12-31 14:00", "2026-12-31 15:00", now),
            Some(("2026-12-31T00:00:00Z".into(), "2027-01-01T00:00:00Z".into()))
        );
        // "today" resolves via `now`
        assert_eq!(
            all_day_bounds("today", "today", now),
            Some(("2026-12-31T00:00:00Z".into(), "2027-01-01T00:00:00Z".into()))
        );
    }

    #[test]
    fn all_day_bounds_rejects_unparseable_start() {
        let now = LocalDateTime { year: 2026, month: 7, day: 19, hour: 10, min: 0 };
        assert_eq!(all_day_bounds("not a date", "", now), None);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy all_day_bounds`
Expected: FAIL — `cannot find function all_day_bounds`.

- [ ] **Step 3: Implement**

Add to `lookxy/src/datetime.rs`:

```rust
/// Formats a day-count (days since the Unix epoch) as a floating all-day
/// boundary: that date at nominal midnight-UTC.
fn day_at_midnight_utc(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T00:00:00Z")
}

/// Bounds for an all-day event from the form's Start/End fields. The *date* of
/// each field is used (any time is ignored); the dates are floating (NOT
/// offset-converted — an all-day date is absolute). Start is that date at
/// midnight; End is the exclusive next-day midnight after the last inclusive
/// day (the End field's date, or the Start date when End is missing/earlier).
/// `None` if the Start field's date can't be parsed.
pub fn all_day_bounds(start_input: &str, end_input: &str, now: LocalDateTime) -> Option<(String, String)> {
    let s = parse_local(start_input.trim(), now)?;
    let start_days = days_from_civil(s.year, s.month, s.day);
    let end_days = match parse_local(end_input.trim(), now) {
        Some(e) => {
            let ed = days_from_civil(e.year, e.month, e.day);
            if ed >= start_days { ed } else { start_days }
        }
        None => start_days,
    };
    Some((day_at_midnight_utc(start_days), day_at_midnight_utc(end_days + 1)))
}
```

Note: `parse_local` accepts `YYYY-MM-DD`, `YYYY-MM-DD HH:MM` (time ignored here), `today`/`tomorrow`, bare/12-hour time (→ today's date), so `all_day_bounds` transparently accepts the same shapes and keeps only the date. Confirm `parse_local`/`civil_from_days`/`days_from_civil`/`LocalDateTime` are all reachable in this module (they are — `datetime.rs` already imports the civil helpers and defines `parse_local`/`LocalDateTime`).

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy all_day_bounds`
Expected: PASS (5 tests).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/datetime.rs
git commit -m "lookxy: all_day_bounds — floating date-range boundaries for all-day events"
```

---

### Task 2: `save_event_form` all-day branch

**Files:**
- Modify: `lookxy/src/app.rs` (`save_event_form` — branch on `form.all_day`; add a test)

**Interfaces:**
- Consumes: `datetime::all_day_bounds` (Task 1), `datetime::parse_start`/`parse_end` (existing), `local_now`, `LocalEventFields`.

- [ ] **Step 1: Write the failing test**

Add to `lookxy/src/app.rs` tests:

```rust
    #[test]
    fn saving_an_all_day_event_stores_midnight_boundaries_and_enqueues_create() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() {
            f.title = "Holiday".into();
            f.all_day = true;
            f.start = "2026-07-20".into();
            f.end = "2026-07-20".into(); // one-day all-day
        }
        app.save_event_form();
        assert!(app.event_form.is_none()); // saved + closed
        // a CreateEvent was enqueued...
        let draft_id = match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::CreateEvent { id }) => id,
            other => panic!("expected CreateEvent, got {other:?}"),
        };
        // ...and the stored event has midnight boundaries, end = start + 1 day, all-day set
        let send = app.store.event_for_send(&draft_id).unwrap().unwrap();
        assert_eq!(send.start_utc, "2026-07-20T00:00:00Z");
        assert_eq!(send.end_utc, "2026-07-21T00:00:00Z");
        assert!(send.is_all_day);
    }

    #[test]
    fn saving_an_all_day_event_with_an_unparseable_date_keeps_the_form_open() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() { f.all_day = true; f.start = "nonsense".into(); }
        app.save_event_form();
        assert!(app.event_form.is_some()); // still open — invalid all-day date
    }
```

(`event_for_send`'s `EventSendData` has `is_all_day` — confirm the field name; adapt if it differs.)

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy saving_an_all_day_event`
Expected: FAIL — the all-day path currently parses the date as a timed instant, so `start_utc`/`end_utc` won't be the midnight boundaries the test asserts.

- [ ] **Step 3: Implement**

In `save_event_form` (app.rs:932), replace the start/end parsing block (currently `parse_start` → `parse_end` → `end_utc < start_utc`, lines 938-949) with an all-day branch:

```rust
        let now = local_now();
        let off = crate::ui::calendar::local_offset_minutes();
        let (start_utc, end_utc) = if form.all_day {
            match crate::datetime::all_day_bounds(&form.start, &form.end, now) {
                Some(bounds) => bounds,
                None => {
                    self.set_form_error("Invalid date");
                    return;
                }
            }
        } else {
            let Some(start_utc) = crate::datetime::parse_start(&form.start, now, off) else {
                self.set_form_error("Invalid start time");
                return;
            };
            let Some(end_utc) = crate::datetime::parse_end(&form.end, &start_utc, now, off) else {
                self.set_form_error("Invalid end time");
                return;
            };
            if end_utc < start_utc {
                self.set_form_error("End is before start");
                return;
            }
            (start_utc, end_utc)
        };
```

The rest of `save_event_form` (building `LocalEventFields` with these `start_utc`/`end_utc` + `is_all_day: form.all_day`, the create/update dispatch) is unchanged. `set_form_error` borrows `self` mutably, so make sure the `form` borrow from the top of the function (`let Some(form) = self.event_form.as_ref()`) has ended before the `set_form_error` calls — it already must, since the existing timed-path `set_form_error` calls compile; clone `form.start`/`form.end`/`form.all_day` up front if the borrow checker complains.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy saving_an_all_day_event` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green (existing timed-event save tests unaffected — they take the `else` branch, byte-identical to before).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/app.rs
git commit -m "lookxy: use all_day_bounds when saving an all-day event"
```

---

### Task 3: All-day display — date bucketing + `is_multi_day`

**Files:**
- Modify: `lookxy/src/ui/calendar.rs` (`agenda_lines`, `is_multi_day`, a shared `date_of_utc` helper; tests)

**Interfaces:**
- Consumes: `days_from_civil` (existing), `EventRow.is_all_day`/`start_utc`/`end_utc`.
- Produces: `fn date_of_utc(iso: &str) -> (i64, u32, u32)` (the `YYYY-MM-DD` of a stored UTC timestamp).

- [ ] **Step 1: Write the failing tests**

Add to `lookxy/src/ui/calendar.rs`'s `#[cfg(test)] mod tests` (the `row(id, start, end, is_all_day)` helper already exists):

```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy date_of_utc single_day_all_day three_day_all_day`
Expected: FAIL — `date_of_utc` missing; `single_day_all_day_event_is_not_multi_day` fails because the current `is_multi_day` compares `to_local(start)` vs `to_local(end)` and sees different days.

- [ ] **Step 3: Implement**

Add the helper (module scope in `calendar.rs`, near `to_local`):

```rust
/// The `(year, month, day)` in the leading `YYYY-MM-DD` of a stored UTC
/// timestamp — used for all-day events, whose date is absolute (floating) and
/// must NOT be shifted by `to_local`'s offset conversion.
fn date_of_utc(iso: &str) -> (i64, u32, u32) {
    // `iso` is an ASCII ISO-8601 `YYYY-MM-DDT…`; `get(..)` is bounds/UTF-8 safe.
    let year: i64 = iso.get(0..4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let month: u32 = iso.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1);
    let day: u32 = iso.get(8..10).and_then(|s| s.parse().ok()).unwrap_or(1);
    (year, month, day)
}
```

In `agenda_lines` (calendar.rs:184), change the per-event day computation so all-day events bucket by their stored date:

```rust
        let ymd = if e.is_all_day {
            date_of_utc(&e.start_utc)
        } else {
            let start = to_local(&e.start_utc);
            (start.year, start.month, start.day)
        };
```

Rewrite `is_multi_day` (calendar.rs:241) to treat an all-day event's End as exclusive and compare stored dates:

```rust
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
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy date_of_utc single_day_all_day three_day_all_day all_day_event_buckets` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green (existing `is_multi_day_detects_events_crossing_midnight_local` and `agenda_lines_groups_events…` tests still pass — timed events are unchanged).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/calendar.rs
git commit -m "lookxy: display all-day events by stored date (bucket + is_multi_day exclusive end)"
```

---

## Notes for the implementer

- **Timed events are untouched.** Every change is gated on `e.is_all_day` / `form.all_day`; the `else`/non-all-day branches must be byte-identical to the pre-change code so existing timed-event tests stay green.
- **All-day dates are floating** — never run them through `to_local`/`local_offset_minutes`. `all_day_bounds` stores the picked date at nominal midnight-UTC, and the display reads the date part back with `date_of_utc`; the two are inverses, so a created all-day event round-trips to the same date it was entered, on any offset.
- **This also fixes read-back all-day events** (from Graph): they arrive as midnight `dateTime` and now render through the same `is_multi_day`/bucketing branches, so the pre-existing "(multi-day)" mislabel on them is corrected too.
