# lookxy Free/Busy Lookup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From the event form, look up attendees' + your own availability for the event's day (08:00–18:00, 30-min slots) via Graph `getSchedule` and show a read-only busy/free grid.

**Architecture:** A `ScheduleEntry` list flows from a new `get_schedule` client call through a direct-call `FetchSchedule` sync command to a `FreeBusyView` overlay the event form opens with `Ctrl-B`. Read-only.

**Tech Stack:** Rust (edition 2024), hand-rolled `mailcore::json`, `ureq` Graph client, `ratatui`/`crossterm` TUI.

## Global Constraints

- **Build wrapper:** never call bare `cargo` (fails os error 448). Use `bash "$LCARGO" <args>` where `LCARGO=C:/Users/BORIS_~1/AppData/Local/Temp/claude/C--Users-boris-kudriashov-Source-docxy/1da9a016-b606-4432-8951-6d73bb91c967/scratchpad/lcargo.sh`. EVERY Bash tool call that runs it MUST set `dangerouslyDisableSandbox: true`.
- **MSRV / edition:** edition 2024, MSRV 1.88. No new external crates.
- **No serde:** JSON only via `mailcore::json` (`Value`, `.get`/`.as_str`/`.as_array`/`Object`/`Str`/`Num`/`Array`).
- **Read-only** — the grid informs; no slot-pick auto-fill. **Trigger:** `Ctrl-B` in the event form. **Window:** the form's Start date, 08:00–18:00 local → UTC, 30-min slots (20 slots). **Start-day only**; no `findMeetingTimes`.
- **Glyphs:** `availabilityView` digit → glyph: `'0'`→`'·'`, `'1'`→`'▓'`, `'2'|'3'|'4'`→`'█'`, else `' '`. Combined `free?`: `'✓'` all-free, `'█'` any-busy, else `'░'`.
- **Datetimes:** local text parsed via `datetime::parse_start` to canonical UTC; sent as `{dateTime: <utc-without-Z>, timeZone: "UTC"}`.

---

### Task 1: Model + client — `get_schedule`

**Files:**
- Modify: `mailcore/src/graph/model.rs` (`ScheduleEntry` + `from_json`; test)
- Modify: `mailcore/src/graph/client.rs` (`get_schedule`; tests)

**Interfaces:**
- Produces: `ScheduleEntry { email: String, availability: String }` (+ `from_json`); `GraphClient::get_schedule(&self, schedules: &[String], start_utc: &str, end_utc: &str, interval_minutes: i64) -> Result<Vec<ScheduleEntry>, GraphError>`.

- [ ] **Step 1: Write the failing model test**

Add to the `tests` module in `mailcore/src/graph/model.rs`:

```rust
    #[test]
    fn schedule_entry_parses_id_and_view() {
        let v = parse(r#"{"scheduleId":"alice@x","availabilityView":"002200"}"#).unwrap();
        let e = ScheduleEntry::from_json(&v).unwrap();
        assert_eq!(e.email, "alice@x");
        assert_eq!(e.availability, "002200");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p mailcore schedule_entry_parses` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — no `ScheduleEntry`.

- [ ] **Step 3: Add the model**

In `mailcore/src/graph/model.rs` (near the `Event` types):

```rust
/// One schedule's availability from Graph `getSchedule`: the mailbox address
/// (`scheduleId`) and its `availabilityView` digit string (one char per slot:
/// `0` free, `1` tentative, `2` busy, `3` out-of-office, `4` working-elsewhere).
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleEntry {
    pub email: String,
    pub availability: String,
}

impl ScheduleEntry {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(ScheduleEntry {
            email: str_field(v, "scheduleId"),
            availability: str_field(v, "availabilityView"),
        })
    }
}
```

- [ ] **Step 4: Write the failing client test**

Add to the client `tests` module in `mailcore/src/graph/client.rs`:

```rust
    #[test]
    fn get_schedule_posts_body_and_parses_view() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/calendar/getSchedule".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[{"scheduleId":"me@x","availabilityView":"000222"},{"scheduleId":"alice@x","availabilityView":"220000"}]}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let entries = c
            .get_schedule(
                &["me@x".to_string(), "alice@x".to_string()],
                "2026-07-21T08:00:00Z",
                "2026-07-21T18:00:00Z",
                30,
            )
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].email, "alice@x");
        assert_eq!(entries[0].availability, "000222");
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        assert_eq!(
            sent.get("availabilityViewInterval").and_then(Value::as_i64),
            Some(30)
        );
        assert_eq!(
            sent.get("schedules").and_then(Value::as_array).unwrap().len(),
            2
        );
        assert_eq!(
            sent.get("startTime")
                .unwrap()
                .get("dateTime")
                .and_then(Value::as_str),
            Some("2026-07-21T08:00:00")
        );
    }
```

- [ ] **Step 5: Run to verify it fails**

Run: `bash "$LCARGO" test -p mailcore get_schedule_posts_body` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — no `get_schedule`.

- [ ] **Step 6: Implement the client method**

In `mailcore/src/graph/client.rs`, add `ScheduleEntry` to the `use crate::graph::model::{…}` import, then add inside `impl GraphClient` (near `calendar_view`):

```rust
    /// POST `/me/calendar/getSchedule` — the availability of each address in
    /// `schedules` over `[start_utc, end_utc)` at `interval_minutes` slots.
    /// Returns each schedule's `availabilityView` digit string.
    pub fn get_schedule(
        &self,
        schedules: &[String],
        start_utc: &str,
        end_utc: &str,
        interval_minutes: i64,
    ) -> Result<Vec<ScheduleEntry>, GraphError> {
        let dt = |utc: &str| {
            Value::Object(vec![
                (
                    "dateTime".to_string(),
                    Value::Str(utc.trim_end_matches('Z').to_string()),
                ),
                ("timeZone".to_string(), Value::Str("UTC".to_string())),
            ])
        };
        let body = Value::Object(vec![
            (
                "schedules".to_string(),
                Value::Array(schedules.iter().map(|s| Value::Str(s.clone())).collect()),
            ),
            ("startTime".to_string(), dt(start_utc)),
            ("endTime".to_string(), dt(end_utc)),
            (
                "availabilityViewInterval".to_string(),
                Value::Num(interval_minutes as f64),
            ),
        ])
        .to_string();
        let resp = self.send(Method::Post, "/me/calendar/getSchedule", Some(body), &[])?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        Ok(items.iter().filter_map(ScheduleEntry::from_json).collect())
    }
```

- [ ] **Step 7: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore -- schedule_entry_parses get_schedule_posts_body` — (single filter) `bash "$LCARGO" test -p mailcore schedule` and `bash "$LCARGO" test -p mailcore get_schedule` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add mailcore/src/graph/model.rs mailcore/src/graph/client.rs
git commit -m "mailcore: ScheduleEntry model + GraphClient::get_schedule"
```

---

### Task 2: Sync — `FetchSchedule` / `ScheduleFetched`

**Files:**
- Modify: `mailcore/src/sync/engine.rs` (import, command + event, dispatch, handler; test)

**Interfaces:**
- Consumes: `GraphClient::get_schedule`, `ScheduleEntry` (Task 1).
- Produces: `SyncCommand::FetchSchedule { schedules: Vec<String>, start_utc: String, end_utc: String, interval_minutes: i64 }`; `SyncEvent::ScheduleFetched { entries: Vec<ScheduleEntry> }`.

- [ ] **Step 1: Import `ScheduleEntry`**

In `mailcore/src/sync/engine.rs`, extend the model import to include `ScheduleEntry` (it currently imports `AutomaticReplies, DeltaItem, Message`):

```rust
use crate::graph::model::{AutomaticReplies, DeltaItem, Message, ScheduleEntry};
```

- [ ] **Step 2: Add the command + event**

In `SyncCommand` (after `RefreshCategories` or any variant), add:

```rust
    /// Fetch attendee availability (`GraphClient::get_schedule`) for a window
    /// and emit [`SyncEvent::ScheduleFetched`]. Direct call, no store.
    FetchSchedule {
        schedules: Vec<String>,
        start_utc: String,
        end_utc: String,
        interval_minutes: i64,
    },
```

In `SyncEvent` (after `CategoriesUpdated`), add:

```rust
    /// Attendee availability (from [`SyncCommand::FetchSchedule`]); the UI fills
    /// its free/busy grid.
    ScheduleFetched { entries: Vec<ScheduleEntry> },
```

- [ ] **Step 3: Dispatch + handler**

In `handle_command`, add an arm (near `FetchAutomaticReplies`):

```rust
            SyncCommand::FetchSchedule {
                schedules,
                start_utc,
                end_utc,
                interval_minutes,
            } => self.fetch_schedule(schedules, &start_utc, &end_utc, interval_minutes),
```

Add the handler inside `impl Engine` (near `fetch_automatic_replies`):

```rust
    /// Fetch attendee availability and emit `ScheduleFetched`; a Graph failure
    /// goes through `react`. Same signed-in guard as `fetch_body`.
    fn fetch_schedule(
        &mut self,
        schedules: Vec<String>,
        start_utc: &str,
        end_utc: &str,
        interval_minutes: i64,
    ) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_schedule(&schedules, start_utc, end_utc, interval_minutes)) {
            Ok(entries) => self.emit(SyncEvent::ScheduleFetched { entries }),
            Err(e) => {
                self.react(e);
            }
        }
    }
```

- [ ] **Step 4: Write the failing engine test**

Add to the engine `tests` module (mirror the OOF fetch test):

```rust
    #[test]
    fn fetch_schedule_emits_entries() {
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "POST".into(),
                path_prefix: "/me/calendar/getSchedule".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"scheduleId":"me@x","availabilityView":"000222"}]}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir("fetch-schedule");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);
        let handle = spawn_with_bases(
            dir.join("mail.db"),
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { .. })
        });
        handle
            .cmd_tx
            .send(SyncCommand::FetchSchedule {
                schedules: vec!["me@x".into()],
                start_utc: "2026-07-21T08:00:00Z".into(),
                end_utc: "2026-07-21T18:00:00Z".into(),
                interval_minutes: 30,
            })
            .unwrap();
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::ScheduleFetched { entries } if entries[0].email == "me@x")
        });
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 5: Run to verify (fail → pass)**

Run: `bash "$LCARGO" test -p mailcore fetch_schedule_emits` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL then, after Steps 2–3, PASS. Then `bash "$LCARGO" test -p mailcore` — whole crate green (new `SyncCommand`/`SyncEvent` variants are exhaustively matched within mailcore).

- [ ] **Step 6: Commit**

```bash
git add mailcore/src/sync/engine.rs
git commit -m "mailcore: FetchSchedule sync command + ScheduleFetched event"
```

---

### Task 3: App — free/busy state, trigger, glyphs

**Files:**
- Create: `lookxy/src/ui/freebusy.rs` (`FreeBusyView`, `slot_glyph`, `combined_glyph`; tests)
- Modify: `lookxy/src/ui/mod.rs` (`pub mod freebusy;`)
- Modify: `lookxy/src/ui/calendar.rs` (`pub(crate) fn day_label`; )
- Modify: `lookxy/src/app.rs` (`App::free_busy` field + init, `open_free_busy`, `close_free_busy`, `ScheduleFetched`/`Error` handling; tests)
- Modify: `lookxy/src/ui/eventform.rs` (`Ctrl-B` → `open_free_busy`)

**Interfaces:**
- Consumes: `SyncCommand::FetchSchedule`, `SyncEvent::ScheduleFetched` (Task 2); `ScheduleEntry`; `parse_attendee_pairs`, `datetime::parse_start`, `App::account`, `ui::calendar::{date_of_utc, days_from_civil}`.
- Produces: `App::free_busy: Option<FreeBusyView>`; `App::open_free_busy`, `App::close_free_busy`; `ui::freebusy::{FreeBusyView, slot_glyph, combined_glyph}`; `ui::calendar::day_label`.

- [ ] **Step 1: Create the freebusy module (state + glyphs)**

Create `lookxy/src/ui/freebusy.rs`:

```rust
//! The free/busy availability grid — an overlay opened by `Ctrl-B` in the
//! event form. Read-only: shows each attendee's `availabilityView` as a strip
//! of busy/free glyphs plus a combined "everyone free" row. State + glyph
//! mapping live here; the draw/key handling is below (Task 4).

use mailcore::graph::model::ScheduleEntry;

pub struct FreeBusyView {
    pub day_label: String,
    pub interval_minutes: i64,
    pub slot_count: usize,
    pub entries: Vec<ScheduleEntry>,
    pub loading: bool,
}

/// One availability digit → its grid glyph. `'0'` free, `'1'` tentative,
/// `'2'`/`'3'`/`'4'` busy/OOF/elsewhere, anything else blank.
pub fn slot_glyph(c: char) -> char {
    match c {
        '0' => '·',
        '1' => '▓',
        '2' | '3' | '4' => '█',
        _ => ' ',
    }
}

/// The combined `free?`-row glyph for one slot across all entries: `'✓'` when
/// everyone is free (`'0'`, or past the end of a short string), `'█'` when
/// anyone is busy (`'2'`/`'3'`/`'4'`), else `'░'` (only tentatives).
pub fn combined_glyph(entries: &[ScheduleEntry], slot: usize) -> char {
    let mut any_busy = false;
    let mut any_tentative = false;
    for e in entries {
        match e.availability.chars().nth(slot) {
            Some('2') | Some('3') | Some('4') => any_busy = true,
            Some('1') => any_tentative = true,
            _ => {} // '0' or missing = free
        }
    }
    if any_busy {
        '█'
    } else if any_tentative {
        '░'
    } else {
        '✓'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_mapping() {
        assert_eq!(slot_glyph('0'), '·');
        assert_eq!(slot_glyph('1'), '▓');
        assert_eq!(slot_glyph('2'), '█');
        let entries = vec![
            ScheduleEntry { email: "a".into(), availability: "02".into() },
            ScheduleEntry { email: "b".into(), availability: "00".into() },
        ];
        assert_eq!(combined_glyph(&entries, 0), '✓'); // both free
        assert_eq!(combined_glyph(&entries, 1), '█'); // a busy
        assert_eq!(combined_glyph(&entries, 5), '✓'); // past end = free
    }
}
```

In `lookxy/src/ui/mod.rs`, add `pub mod freebusy;` alongside the other module declarations.

- [ ] **Step 2: Add the `day_label` helper (`calendar.rs`)**

In `lookxy/src/ui/calendar.rs`, add a `pub(crate)` helper (it uses the private `weekday_abbrev`/`month_abbrev`, so it must live in this module):

```rust
/// A short calendar-date label like `Mon Jul 21` for a `(year, month, day)`.
pub(crate) fn day_label(y: i64, m: u32, d: u32) -> String {
    let z = days_from_civil(y, m, d);
    format!("{} {} {:02}", weekday_abbrev(z), month_abbrev(m), d)
}
```

- [ ] **Step 3: Add the `App::free_busy` field + methods**

In `lookxy/src/app.rs`, extend the model import to include `ScheduleEntry` isn't needed here (only the struct is used via `ui::freebusy`), but add the `App` field (near `reminder_queue`):

```rust
    /// The free/busy availability overlay (opened by `Ctrl-B` in the event
    /// form), when open.
    pub free_busy: Option<crate::ui::freebusy::FreeBusyView>,
```

Initialize `free_busy: None,` in `App::new`.

Add the methods inside `impl App` (near `open_new_event`/`save_event_form`):

```rust
    /// `Ctrl-B` in the event form: fetch and show attendees' + the organizer's
    /// availability for the form's Start date (08:00–18:00 local, 30-min
    /// slots). Read-only.
    pub fn open_free_busy(&mut self) {
        let Some(form) = self.event_form.as_ref() else {
            return;
        };
        // Emails: the organizer (own account) first, then attendee addresses.
        let mut schedules: Vec<String> = Vec::new();
        if let Some(me) = self.account.clone() {
            if !me.is_empty() {
                schedules.push(me);
            }
        }
        for (_, addr) in parse_attendee_pairs(&form.attendees) {
            if !addr.is_empty() && !schedules.contains(&addr) {
                schedules.push(addr);
            }
        }
        // Window: the Start field's date (first 10 chars if `YYYY-MM-DD…`, else
        // today) at 08:00–18:00 local → UTC.
        let now = local_now();
        let off = crate::ui::calendar::local_offset_minutes();
        let date = form
            .start
            .get(..10)
            .filter(|d| d.len() == 10 && d.as_bytes()[4] == b'-')
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!("{:04}-{:02}-{:02}", now.year, now.month, now.day)
            });
        let start_utc = crate::datetime::parse_start(&format!("{date} 08:00"), now, off)
            .unwrap_or_default();
        let end_utc =
            crate::datetime::parse_start(&format!("{date} 18:00"), now, off).unwrap_or_default();
        let (y, m, d) = crate::ui::calendar::date_of_utc(&start_utc);
        let _ = self.sync.cmd_tx.send(SyncCommand::FetchSchedule {
            schedules,
            start_utc,
            end_utc,
            interval_minutes: 30,
        });
        self.free_busy = Some(crate::ui::freebusy::FreeBusyView {
            day_label: crate::ui::calendar::day_label(y, m, d),
            interval_minutes: 30,
            slot_count: 20, // (18-8)*60/30
            entries: Vec::new(),
            loading: true,
        });
    }

    /// Esc in the free/busy overlay: close it (back to the event form).
    pub fn close_free_busy(&mut self) {
        self.free_busy = None;
    }
```

NOTE: confirm `local_now()` returns a value with `.year`/`.month`/`.day` (it returns `crate::datetime::LocalDateTime`, used by `open_new_event`). `date_of_utc(&str) -> (i64, u32, u32)`.

In `on_sync_event`, add arms (place near the `CategoriesUpdated` arm):

```rust
            SyncEvent::ScheduleFetched { entries } => {
                if let Some(v) = self.free_busy.as_mut() {
                    v.entries = entries;
                    v.loading = false;
                }
            }
```

Amend the existing `SyncEvent::Error` arm so a failed fetch clears the overlay's loading state (leaving an empty grid rather than a stuck "loading…") — add, alongside the existing `oof_form` loading clear:

```rust
            SyncEvent::Error(msg) => {
                if let Some(form) = self.oof_form.as_mut() {
                    form.loading = false;
                }
                if let Some(v) = self.free_busy.as_mut() {
                    v.loading = false;
                }
                self.error_notice = Some(msg);
            }
```

- [ ] **Step 4: Bind `Ctrl-B` in the event form**

In `lookxy/src/ui/eventform.rs` `handle_key`, after the `Ctrl-Enter` block:

```rust
    if ctrl && key.code == KeyCode::Enter {
        app.save_event_form();
        return;
    }
    if ctrl && key.code == KeyCode::Char('b') {
        app.open_free_busy();
        return;
    }
```

- [ ] **Step 5: Write the failing app tests**

Add to the `tests` module in `lookxy/src/app.rs`:

```rust
    #[test]
    fn open_free_busy_sends_fetch_with_organizer_and_attendees() {
        use crate::ui::eventform::{EventField, EventForm};
        let mut app = App::for_test_with_seeded_store();
        app.account = Some("me@x".into());
        app.event_form = Some(EventForm {
            editing_id: None,
            title: "Sync".into(),
            start: "2026-07-21 14:00".into(),
            end: "2026-07-21 15:00".into(),
            all_day: false,
            repeat: None,
            interval: "1".into(),
            days: [false; 7],
            until: String::new(),
            location: String::new(),
            attendees: "Alice <alice@x>; bob@x".into(),
            body: String::new(),
            focus: EventField::Title,
            autocomplete: None,
            error: None,
        });
        app.open_free_busy();
        assert!(app.free_busy.as_ref().unwrap().loading);
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::FetchSchedule {
                schedules,
                start_utc,
                end_utc,
                interval_minutes,
            }) => {
                assert_eq!(schedules, vec!["me@x", "alice@x", "bob@x"]);
                assert!(start_utc.contains("2026-07-21") && start_utc.ends_with('Z'));
                assert!(end_utc.ends_with('Z'));
                assert_eq!(interval_minutes, 30);
            }
            other => panic!("expected FetchSchedule, got {other:?}"),
        }
    }

    #[test]
    fn schedule_fetched_fills_the_view() {
        use mailcore::graph::model::ScheduleEntry;
        let mut app = App::for_test_with_seeded_store();
        app.free_busy = Some(crate::ui::freebusy::FreeBusyView {
            day_label: "Mon Jul 21".into(),
            interval_minutes: 30,
            slot_count: 20,
            entries: Vec::new(),
            loading: true,
        });
        app.on_sync_event(SyncEvent::ScheduleFetched {
            entries: vec![ScheduleEntry {
                email: "me@x".into(),
                availability: "000222".into(),
            }],
        });
        let v = app.free_busy.as_ref().unwrap();
        assert!(!v.loading);
        assert_eq!(v.entries.len(), 1);
    }
```

- [ ] **Step 6: Run to verify (fail → pass)**

Run: `bash "$LCARGO" test -p lookxy -- open_free_busy_sends_fetch schedule_fetched_fills glyph_mapping` — (single filter) `bash "$LCARGO" test -p lookxy free_busy` and `bash "$LCARGO" test -p lookxy glyph_mapping` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add lookxy/src/ui/freebusy.rs lookxy/src/ui/mod.rs lookxy/src/ui/calendar.rs lookxy/src/app.rs lookxy/src/ui/eventform.rs
git commit -m "lookxy: free/busy state, Ctrl-B trigger, ScheduleFetched handling"
```

---

### Task 4: UI — the availability grid overlay

**Files:**
- Modify: `lookxy/src/ui/freebusy.rs` (`draw`, `handle_key`; test)
- Modify: `lookxy/src/ui/mod.rs` (route keys + draw)

**Interfaces:**
- Consumes: `App::free_busy`, `App::close_free_busy` (Task 3), `slot_glyph`/`combined_glyph` (Task 3), `ui::centered_rect`.

- [ ] **Step 1: Write the failing render test**

Add to the `tests` module in `lookxy/src/ui/freebusy.rs`:

```rust
    #[test]
    fn draw_renders_rows_and_free_row() {
        use crate::app::App;
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = App::for_test_with_seeded_store();
        app.free_busy = Some(FreeBusyView {
            day_label: "Mon Jul 21".into(),
            interval_minutes: 30,
            slot_count: 20,
            entries: vec![ScheduleEntry {
                email: "alice@x".into(),
                availability: "00222200000000000000".into(),
            }],
            loading: false,
        });
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Availability"));
        assert!(text.contains("alice@x"));
        assert!(text.contains("free?"));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p lookxy draw_renders_rows_and_free_row` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — `draw` not defined.

- [ ] **Step 3: Add `draw` + `handle_key`**

In `lookxy/src/ui/freebusy.rs`, add the imports and functions (above the test module):

```rust
use crate::app::App;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// Renders the free/busy overlay when `app.free_busy` is open; a no-op
/// otherwise. A centered bordered panel with an hour header, one row per
/// entry, and a combined `free?` row.
pub fn draw(f: &mut Frame, app: &App) {
    let Some(v) = &app.free_busy else {
        return;
    };
    let area = crate::ui::centered_rect(80, 60, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(
            "Availability \u{2014} {} (08:00\u{2013}18:00)  [Esc: back]",
            v.day_label
        ))
        .borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if v.loading {
        f.render_widget(Paragraph::new("loading\u{2026}"), inner);
        return;
    }

    // How wide the leading email label column is.
    const LABEL_W: usize = 12;
    let pad = |s: &str| -> String {
        let mut t: String = s.chars().take(LABEL_W).collect();
        while t.chars().count() < LABEL_W {
            t.push(' ');
        }
        t
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Hour header: two slots per hour, so a label every 2 slots.
    let mut header = pad("");
    for slot in 0..v.slot_count {
        header.push(if slot % 2 == 0 {
            std::char::from_digit((8 + slot as u32 / 2) % 10, 10).unwrap_or(' ')
        } else {
            ' '
        });
    }
    lines.push(Line::from(header));
    // One row per entry.
    for e in &v.entries {
        let mut row = pad(&e.email);
        for slot in 0..v.slot_count {
            row.push(slot_glyph(e.availability.chars().nth(slot).unwrap_or('0')));
        }
        lines.push(Line::from(row));
    }
    // Combined free row.
    let mut free_row = pad("free?");
    for slot in 0..v.slot_count {
        free_row.push(combined_glyph(&v.entries, slot));
    }
    lines.push(Line::from(free_row));
    if v.entries.is_empty() {
        lines.push(Line::from("(no attendees)"));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Keys while the overlay is open: `Esc` closes it; other keys ignored.
pub fn handle_key(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Esc {
        app.close_free_busy();
    }
}
```

- [ ] **Step 4: Route + draw in `ui/mod.rs`**

In `lookxy/src/ui/mod.rs` `handle_key`, add ahead of the event-form handler (right after the RSVP-prompt block):

```rust
    // The free/busy overlay (opened by Ctrl-B in the event form) captures keys.
    if app.free_busy.is_some() {
        freebusy::handle_key(app, key);
        return;
    }
```

In `ui::draw`'s Calendar branch, add a draw call after `eventform::draw(f, &*app);`:

```rust
        eventform::draw(f, &*app);
        freebusy::draw(f, &*app);
```

- [ ] **Step 5: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy draw_renders_rows_and_free_row` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 6: Full workspace gate**

Run: `bash "$LCARGO" test --workspace`, then `bash "$LCARGO" clippy --workspace --all-targets -- -D warnings`, then `bash "$LCARGO" fmt --all` + `bash "$LCARGO" fmt --all -- --check` (Bash, `dangerouslyDisableSandbox: true`)
Expected: all green, clippy clean, fmt clean.

- [ ] **Step 7: Commit**

```bash
git add lookxy/src/ui/freebusy.rs lookxy/src/ui/mod.rs
git commit -m "lookxy: free/busy availability grid overlay + Esc"
```

---

## Self-Review

**Spec coverage:**
- `ScheduleEntry` + `get_schedule` client (body + parse) → Task 1. ✅
- `FetchSchedule`/`ScheduleFetched` + engine handler → Task 2. ✅
- `FreeBusyView` + `Ctrl-B` trigger (organizer + attendees, Start-day 08:00–18:00 window) + `ScheduleFetched`/`Error` handling + glyph helpers + `day_label` → Task 3. ✅
- Grid overlay (header + per-entry rows + `free?` row + loading + no-attendees note) + `Esc` + routing/draw → Task 4. ✅
- Error handling: fetch failure clears loading (Task 3 `Error` arm); short/absent `availabilityView` treated as free (Task 3 `combined_glyph`, Task 4 `nth().unwrap_or('0')`); invalid Start date → today (Task 3); not-signed-in guard (Task 2). ✅

**Placeholder scan:** No TBD/TODO. The one NOTE (Task 3) flags concrete real-code checks (`local_now` fields, `date_of_utc` return) — not deferred work.

**Type consistency:** `ScheduleEntry { email, availability }` identical across model (T1), client (T1), engine command/event (T2), and the UI (T3/T4). `FetchSchedule { schedules, start_utc, end_utc, interval_minutes }` / `ScheduleFetched { entries }` consistent T2↔T3. `FreeBusyView { day_label, interval_minutes, slot_count, entries, loading }` consistent T3 struct ↔ T3 app methods ↔ T4 draw. `slot_glyph(char)->char` / `combined_glyph(&[ScheduleEntry], usize)->char` / `day_label(i64,u32,u32)->String` used identically across tasks.
