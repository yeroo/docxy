# lookxy Event Reminders / Alerts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a dismissible in-TUI banner (and, opt-in, an agwinterm overlay over all sessions) when an event is starting soon, within its own reminder window.

**Architecture:** Read-only `reminder_minutes`/`is_reminder_on` fields ride the existing calendar sync into the agenda; a per-tick `App::check_due_reminders(now_epoch)` fires each event once (de-dup set) into a banner queue and, when a config flag is on, into `agwintermctl notify`. Read-only — no Graph writes.

**Tech Stack:** Rust (edition 2024), hand-rolled `mailcore::json`, `rusqlite` store, `ratatui`/`crossterm` TUI, `std::process::Command` for the agwinterm CLI.

## Global Constraints

- **Build wrapper:** never call bare `cargo` (fails os error 448). Use `bash "$LCARGO" <args>` where `LCARGO=C:/Users/BORIS_~1/AppData/Local/Temp/claude/C--Users-boris-kudriashov-Source-docxy/1da9a016-b606-4432-8951-6d73bb91c967/scratchpad/lcargo.sh`. EVERY Bash tool call that runs it MUST set `dangerouslyDisableSandbox: true`.
- **MSRV / edition:** edition 2024, MSRV 1.88. No new external crates.
- **No serde:** JSON only via `mailcore::json` (`Value`, `.get`/`.as_i64`/`.as_bool`).
- **Read-only:** no Graph writes, no editing reminder settings, no snooze, no bell.
- **Reminder window:** due when `start − reminderMinutes·60 ≤ now < start`, only when `isReminderOn`; each event id fires once per session.
- **agwinterm overlay is opt-in:** config `reminders_notify` (default **false**); the `agwintermctl notify` call also requires `AGWINTERM_ENABLED=1` at runtime.
- **Dismiss:** `Esc`, after the overlay handlers (overlays keep Esc priority).
- **Process spawn safety:** `agwintermctl` invoked argv-style via `std::process::Command` (no shell), best-effort, result ignored — same posture as `open_with_os_handler`.

---

### Task 1: Reminder fields through model + store

**Files:**
- Modify: `mailcore/src/graph/model.rs` (`Event` fields + `from_json`; test)
- Modify: `mailcore/src/store/schema.rs` (two `events` columns)
- Modify: `mailcore/src/store/mod.rs` (`EventRow`/`NewEvent` fields, `From<&Event>`, migration, `upsert_event`, `events_in_window`; tests + literal ripple)

**Interfaces:**
- Produces: `Event.reminder_minutes: i64`, `Event.is_reminder_on: bool`; `EventRow.reminder_minutes: i64`, `EventRow.is_reminder_on: bool`; `NewEvent.reminder_minutes: i64`, `NewEvent.is_reminder_on: bool`.

- [ ] **Step 1: Write the failing model test**

Add to the `tests` module in `mailcore/src/graph/model.rs`:

```rust
    #[test]
    fn event_parses_reminder_fields() {
        let v = parse(
            r#"{"id":"E1","subject":"Sync",
                "start":{"dateTime":"2026-07-20T09:00:00.0000000","timeZone":"UTC"},
                "end":{"dateTime":"2026-07-20T10:00:00.0000000","timeZone":"UTC"},
                "isReminderOn":true,"reminderMinutesBeforeStart":15}"#,
        )
        .unwrap();
        let e = Event::from_json(&v).unwrap();
        assert_eq!(e.reminder_minutes, 15);
        assert!(e.is_reminder_on);

        // Absent → defaults 0 / false.
        let v2 = parse(
            r#"{"id":"E2","subject":"x",
                "start":{"dateTime":"2026-07-20T09:00:00.0000000","timeZone":"UTC"},
                "end":{"dateTime":"2026-07-20T10:00:00.0000000","timeZone":"UTC"}}"#,
        )
        .unwrap();
        let e2 = Event::from_json(&v2).unwrap();
        assert_eq!(e2.reminder_minutes, 0);
        assert!(!e2.is_reminder_on);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p mailcore event_parses_reminder_fields` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — `Event` has no `reminder_minutes`.

- [ ] **Step 3: Add the `Event` fields + parse**

In `mailcore/src/graph/model.rs`, add to the `Event` struct (after `attendees`):

```rust
    pub attendees: Vec<Attendee>,
    /// Graph `reminderMinutesBeforeStart`: minutes before `start` the reminder
    /// fires. `is_reminder_on` gates whether a reminder exists at all.
    pub reminder_minutes: i64,
    pub is_reminder_on: bool,
}
```

In `Event::from_json`, add (after the `attendees:` field):

```rust
            reminder_minutes: v
                .get("reminderMinutesBeforeStart")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            is_reminder_on: v.get("isReminderOn").and_then(Value::as_bool).unwrap_or(false),
```

- [ ] **Step 4: Add the store columns + struct fields + migration**

In `mailcore/src/store/schema.rs`, extend the `events` table tail:

```sql
    body_html        TEXT NOT NULL DEFAULT '',
    recurrence       TEXT NOT NULL DEFAULT '',
    reminder_minutes INTEGER NOT NULL DEFAULT 0,
    is_reminder_on   INTEGER NOT NULL DEFAULT 0
);
```

In `mailcore/src/store/mod.rs`, add to `EventRow` (after `series_master_id`) and to `NewEvent` (after `body_html`):

```rust
    // EventRow: after series_master_id
    pub series_master_id: Option<String>,
    pub reminder_minutes: i64,
    pub is_reminder_on: bool,
}
```

```rust
    // NewEvent: after body_html
    pub body_html: String,
    pub reminder_minutes: i64,
    pub is_reminder_on: bool,
}
```

Extend `From<&Event> for NewEvent` (after `body_html: e.body_html.clone(),`):

```rust
            body_html: e.body_html.clone(),
            reminder_minutes: e.reminder_minutes,
            is_reminder_on: e.is_reminder_on,
        }
```

Add the idempotent migrations in `Store::init`, after the `events.recurrence` ALTER:

```rust
        let _ = conn.execute(
            "ALTER TABLE events ADD COLUMN reminder_minutes INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE events ADD COLUMN is_reminder_on INTEGER NOT NULL DEFAULT 0",
            [],
        );
```

- [ ] **Step 5: Persist + read the columns**

In `upsert_event`, extend the INSERT column list, placeholders, conflict `SET`, and params. The current 14-column INSERT (`… body_html) VALUES (?1 … ?14)`) becomes:

```rust
            "INSERT INTO events (
                 id, subject, start_utc, end_utc, is_all_day, location,
                 organizer_name, organizer_addr, response_status,
                 series_master_id, body_preview, web_link, last_modified,
                 body_html, reminder_minutes, is_reminder_on
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(id) DO UPDATE SET
                 subject = excluded.subject,
                 start_utc = excluded.start_utc,
                 end_utc = excluded.end_utc,
                 is_all_day = excluded.is_all_day,
                 location = excluded.location,
                 organizer_name = excluded.organizer_name,
                 organizer_addr = excluded.organizer_addr,
                 response_status = excluded.response_status,
                 series_master_id = excluded.series_master_id,
                 body_preview = excluded.body_preview,
                 web_link = excluded.web_link,
                 last_modified = excluded.last_modified,
                 body_html = excluded.body_html,
                 reminder_minutes = excluded.reminder_minutes,
                 is_reminder_on = excluded.is_reminder_on",
```

and append the two params after `e.body_html,`:

```rust
                e.body_html,
                e.reminder_minutes,
                e.is_reminder_on,
            ],
```

In `events_in_window`, add the two columns to the SELECT (after `series_master_id`) and to the `EventRow` map (indices 10, 11):

```rust
            "SELECT id, subject, start_utc, end_utc, is_all_day, location,
                    organizer_name, organizer_addr, response_status, series_master_id,
                    reminder_minutes, is_reminder_on
             FROM events
             WHERE start_utc < ?2 AND end_utc > ?1
             ORDER BY start_utc ASC",
```

```rust
                    series_master_id: row.get(9)?,
                    reminder_minutes: row.get(10)?,
                    is_reminder_on: row.get(11)?,
                })
```

- [ ] **Step 6: Write the failing store test**

Add to the `store` (calendar) tests in `mailcore/src/store/mod.rs`:

```rust
    #[test]
    fn events_round_trip_reminder_fields() {
        use crate::graph::model::{Event, RecurrenceKind}; // RecurrenceKind unused import guard: remove if the compiler warns
        let _ = RecurrenceKind::Daily; // keep the import used; or delete both lines
        let s = Store::open_in_memory().unwrap();
        let mut e = sample_new_event("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z", "Sync");
        e.reminder_minutes = 15;
        e.is_reminder_on = true;
        s.upsert_event(&e).unwrap();
        let rows = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap();
        let row = rows.iter().find(|r| r.id == "E1").unwrap();
        assert_eq!(row.reminder_minutes, 15);
        assert!(row.is_reminder_on);
    }
```

NOTE: use whatever `NewEvent` fixture helper the calendar tests already have (grep `fn sample_new_event` / how `upsert_event` tests build a `NewEvent`); if none exists, build a `NewEvent { … }` literal inline. Drop the `RecurrenceKind` lines — they're only shown to illustrate imports; the real test needs no recurrence.

- [ ] **Step 7: Build all targets, fix the literal ripple, run tests**

Run: `bash "$LCARGO" build -p mailcore --all-targets 2>&1 | grep -E "missing field|-->"` — add `reminder_minutes: 0,` and `is_reminder_on: false,` to every `Event { … }`, `NewEvent { … }`, and `EventRow { … }` literal the compiler flags (model tests, store tests, and any `calendar.rs`/`app.rs` fixtures — those are in lookxy, caught in later tasks' builds; for THIS task only mailcore literals must compile).
Run: `bash "$LCARGO" test -p mailcore` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS incl. the two new tests, whole `mailcore` green.

- [ ] **Step 8: Commit**

```bash
git add mailcore/src/graph/model.rs mailcore/src/store/schema.rs mailcore/src/store/mod.rs
git commit -m "mailcore: carry event reminder_minutes/is_reminder_on through model + store"
```

---

### Task 2: Config `reminders_notify` flag

**Files:**
- Modify: `lookxy/src/config.rs` (`Config` field + default + overlays; test)
- Modify: `lookxy/src/main.rs` (wire `app.reminders_notify` from config)

**Interfaces:**
- Produces: `Config.reminders_notify: bool` (default false), read from the `reminders_notify` JSON key and `LOOKXY_REMINDERS_NOTIFY` env.

- [ ] **Step 1: Write the failing config test**

Add to the `tests` module in `lookxy/src/config.rs`:

```rust
    #[test]
    fn reminders_notify_defaults_false_and_overlays_from_json() {
        assert!(!Config::default().reminders_notify);
        let dir = std::env::temp_dir().join(format!(
            "lookxy-cfg-rem-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"reminders_notify": true}"#).unwrap();
        let cfg = Config::load_from(Some(&path));
        assert!(cfg.reminders_notify);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p lookxy reminders_notify_defaults_false` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — no `reminders_notify` field.

- [ ] **Step 3: Add the field + default + overlays**

In `lookxy/src/config.rs`, add to `Config` (after `signature`):

```rust
    pub signature: String,
    /// When true, a firing reminder also raises an agwinterm notification
    /// (`agwintermctl notify`) over all sessions, when running inside agwinterm.
    /// Default false — the in-TUI banner always shows regardless.
    pub reminders_notify: bool,
}
```

In `Config::default()`, add `reminders_notify: false,`.

In `overlay_json`, after the `signature` block:

```rust
        if let Some(b) = value.get("reminders_notify").and_then(|v| v.as_bool()) {
            self.reminders_notify = b;
        }
```

In `overlay_env`, after the `LOOKXY_THREADED` block:

```rust
        if let Ok(v) = std::env::var("LOOKXY_REMINDERS_NOTIFY") {
            let v = v.trim();
            if v.eq_ignore_ascii_case("true") || v == "1" {
                self.reminders_notify = true;
            } else if v.eq_ignore_ascii_case("false") || v == "0" {
                self.reminders_notify = false;
            }
        }
```

- [ ] **Step 4: Wire it into the app**

In `lookxy/src/main.rs`, after `app.signature = config.signature.clone();`, add:

```rust
        app.reminders_notify = config.reminders_notify;
```

(The `App.reminders_notify` field is added in Task 3; this line will not compile until then. If executing strictly task-by-task, add a temporary `pub reminders_notify: bool` default in Task 3 before this compiles — Task 3 owns the App field. For a clean per-task build, do Step 4 as the FIRST step of Task 3 instead. Recorded here so the config→app wiring isn't forgotten.)

- [ ] **Step 5: Run the config test**

Run: `bash "$LCARGO" test -p lookxy reminders_notify_defaults_false` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS. (Defer the `main.rs` line to Task 3 Step 1 so the crate compiles; commit only `config.rs` here.)

- [ ] **Step 6: Commit**

```bash
git add lookxy/src/config.rs
git commit -m "lookxy: config reminders_notify flag (default off)"
```

---

### Task 3: Reminder check + agwinterm notify (`App`)

**Files:**
- Modify: `lookxy/src/app.rs` (`App` fields, `check_due_reminders`, `dismiss_reminder`, `notify_agwinterm`, `utc_to_epoch`, `starts_in_phrase`; tests)
- Modify: `lookxy/src/main.rs` (the deferred `app.reminders_notify` wiring + the per-tick `check_due_reminders` call + `now_unix_secs`)

**Interfaces:**
- Consumes: `EventRow.{reminder_minutes,is_reminder_on,start_utc,subject,id}` (Task 1); `Config.reminders_notify` (Task 2); `ui::calendar::{date_of_utc, days_from_civil, to_local}`.
- Produces: `App.reminders_notify: bool`, `App.reminder_queue: VecDeque<String>`, `App::check_due_reminders(&mut self, now_epoch: i64)`, `App::dismiss_reminder(&mut self)`.

- [ ] **Step 1: Add the App fields + the deferred main wiring**

In `lookxy/src/app.rs`, add to `App` (near `master_categories`):

```rust
    /// When true, a firing reminder also raises an agwinterm overlay (see
    /// `notify_agwinterm`). Set from `Config::reminders_notify`; default false.
    pub reminders_notify: bool,
    /// Event ids already alerted this session (fire-once de-dup).
    pub alerted_reminders: std::collections::HashSet<String>,
    /// Pending reminder banner lines (front = currently shown).
    pub reminder_queue: std::collections::VecDeque<String>,
    #[cfg(test)]
    pub agwinterm_notify_invocations: std::cell::Cell<u32>,
```

Initialize in `App::new`: `reminders_notify: false,`, `alerted_reminders: std::collections::HashSet::new(),`, `reminder_queue: std::collections::VecDeque::new(),`, and (guarded) `#[cfg(test)] agwinterm_notify_invocations: std::cell::Cell::new(0),`.

In `lookxy/src/main.rs`, add after `app.signature = config.signature.clone();`:

```rust
    app.reminders_notify = config.reminders_notify;
```

- [ ] **Step 2: Write the failing tests**

Add to the `tests` module in `lookxy/src/app.rs`:

```rust
    // Build an agenda EventRow directly (no store / no wall-clock-anchored
    // agenda window — `check_due_reminders` scans `app.agenda`, so setting it
    // directly keeps these tests deterministic regardless of the real date).
    fn reminder_row(id: &str, start_utc: &str, minutes: i64, on: bool) -> mailcore::store::EventRow {
        mailcore::store::EventRow {
            id: id.into(),
            subject: "Standup".into(),
            start_utc: start_utc.into(),
            end_utc: "2026-07-20T10:00:00Z".into(),
            is_all_day: false,
            location: String::new(),
            organizer_name: String::new(),
            organizer_addr: String::new(),
            response_status: "organizer".into(),
            series_master_id: None,
            reminder_minutes: minutes,
            is_reminder_on: on,
        }
    }

    #[test]
    fn utc_to_epoch_known_values() {
        assert_eq!(crate::app::utc_to_epoch("1970-01-01T00:00:00Z"), 0);
        assert_eq!(crate::app::utc_to_epoch("1970-01-01T00:01:00Z"), 60);
        assert_eq!(crate::app::utc_to_epoch("1970-01-02T00:00:00Z"), 86400);
    }

    #[test]
    fn check_due_reminders_fires_once_in_window() {
        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E1", "2026-07-20T09:00:00Z", 15, true)];
        let start = crate::app::utc_to_epoch("2026-07-20T09:00:00Z");
        let now = start - 5 * 60; // 5 min before, inside the 15-min window
        app.check_due_reminders(now);
        assert_eq!(app.reminder_queue.len(), 1);
        assert!(app.reminder_queue.front().unwrap().contains("Standup"));
        // De-dup: a second call at the same now fires nothing more.
        app.check_due_reminders(now);
        assert_eq!(app.reminder_queue.len(), 1);
    }

    #[test]
    fn check_due_reminders_respects_window_and_flag() {
        let start = crate::app::utc_to_epoch("2026-07-20T09:00:00Z");

        // reminder off → nothing.
        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E1", "2026-07-20T09:00:00Z", 15, false)];
        app.check_due_reminders(start - 5 * 60);
        assert!(app.reminder_queue.is_empty());

        // Before the window / at-or-after start → nothing.
        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E1", "2026-07-20T09:00:00Z", 15, true)];
        app.check_due_reminders(start - 60 * 60); // 60 min before, window is 15
        assert!(app.reminder_queue.is_empty());
        app.check_due_reminders(start + 60); // after start
        assert!(app.reminder_queue.is_empty());
    }

    #[test]
    fn agwinterm_notify_fires_only_when_flag_on() {
        let now = crate::app::utc_to_epoch("2026-07-20T09:00:00Z") - 5 * 60;

        // Flag off (default): banner but no agwinterm notify.
        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E1", "2026-07-20T09:00:00Z", 15, true)];
        app.check_due_reminders(now);
        assert_eq!(app.agwinterm_notify_invocations.get(), 0);
        assert_eq!(app.reminder_queue.len(), 1);

        // Flag on: fires the notify seam.
        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E2", "2026-07-20T09:00:00Z", 15, true)];
        app.reminders_notify = true;
        app.check_due_reminders(now);
        assert_eq!(app.agwinterm_notify_invocations.get(), 1);
    }

    #[test]
    fn dismiss_reminder_pops_the_front() {
        let mut app = App::for_test_with_seeded_store();
        app.reminder_queue.push_back("a".into());
        app.reminder_queue.push_back("b".into());
        app.dismiss_reminder();
        assert_eq!(app.reminder_queue.front().map(String::as_str), Some("b"));
        app.dismiss_reminder();
        assert!(app.reminder_queue.is_empty());
    }
```

- [ ] **Step 3: Run to verify they fail**

Run: `bash "$LCARGO" test -p lookxy -- utc_to_epoch check_due_reminders agwinterm_notify dismiss_reminder` (single filter) `bash "$LCARGO" test -p lookxy reminder` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — methods/fns don't exist.

- [ ] **Step 4: Implement the methods + free fns**

In `lookxy/src/app.rs`, add inside `impl App`:

```rust
    /// Scans the loaded agenda for events whose reminder window contains
    /// `now_epoch` (`start − reminderMinutes·60 ≤ now < start`, and
    /// `is_reminder_on`) and that haven't been alerted yet this session; each
    /// fires exactly once — pushing a banner line and, when `reminders_notify`
    /// is set, an agwinterm overlay. Called every main-loop tick.
    pub fn check_due_reminders(&mut self, now_epoch: i64) {
        // Collect first (immutable borrow of self.agenda) to avoid borrowing
        // self mutably while iterating it.
        let due: Vec<(String, String)> = self
            .agenda
            .iter()
            .filter(|e| e.is_reminder_on && !self.alerted_reminders.contains(&e.id))
            .filter_map(|e| {
                let start = utc_to_epoch(&e.start_utc);
                let remind_at = start - e.reminder_minutes.max(0) * 60;
                if remind_at <= now_epoch && now_epoch < start {
                    let phrase = starts_in_phrase(now_epoch, start, &e.start_utc);
                    Some((e.id.clone(), format!("⏰ {} {}", e.subject, phrase)))
                } else {
                    None
                }
            })
            .collect();
        for (id, msg) in due {
            self.alerted_reminders.insert(id);
            self.reminder_queue.push_back(msg.clone());
            if self.reminders_notify {
                self.notify_agwinterm(&msg);
            }
        }
    }

    /// Dismisses the front reminder banner.
    pub fn dismiss_reminder(&mut self) {
        self.reminder_queue.pop_front();
    }

    /// Best-effort agwinterm overlay for a fired reminder. Production: only
    /// inside agwinterm (`AGWINTERM_ENABLED=1`), spawns `agwintermctl notify
    /// {msg} --title lookxy` argv-style (no shell), result ignored. Tests: a
    /// `Cell` counts calls instead of spawning.
    #[cfg(not(test))]
    fn notify_agwinterm(&self, msg: &str) {
        if std::env::var("AGWINTERM_ENABLED").as_deref() == Ok("1") {
            let _ = std::process::Command::new("agwintermctl")
                .arg("notify")
                .arg(msg)
                .arg("--title")
                .arg("lookxy")
                .spawn();
        }
    }

    #[cfg(test)]
    fn notify_agwinterm(&self, _msg: &str) {
        self.agwinterm_notify_invocations
            .set(self.agwinterm_notify_invocations.get() + 1);
    }
```

And add these free fns near `local_now`/`weekday_name_of`:

```rust
/// Epoch seconds for a canonical-UTC `YYYY-MM-DDTHH:MM:SSZ` timestamp, via the
/// calendar module's civil-day math. Used to compare event starts to `now`.
pub(crate) fn utc_to_epoch(iso: &str) -> i64 {
    let (y, m, d) = crate::ui::calendar::date_of_utc(iso);
    let time = iso.split('T').nth(1).unwrap_or("").trim_end_matches('Z');
    let mut parts = time.splitn(3, ':');
    let h: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mi: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let s: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    crate::ui::calendar::days_from_civil(y, m, d) * 86400 + h * 3600 + mi * 60 + s
}

/// "starts now" (when `now >= start`) or "starts in N min (HH:MM)" — the
/// local start time via `ui::calendar::to_local`.
fn starts_in_phrase(now_epoch: i64, start_epoch: i64, start_utc: &str) -> String {
    if now_epoch >= start_epoch {
        return "starts now".to_string();
    }
    let mins = ((start_epoch - now_epoch) / 60).max(1);
    let l = crate::ui::calendar::to_local(start_utc);
    format!("starts in {mins} min ({:02}:{:02})", l.hour, l.minute)
}
```

REQUIRED visibility change: `ui::calendar::to_local` is currently a **private** `fn to_local(iso_utc: &str) -> LocalDateTime` (fields `.hour`/`.minute`). Change it to `pub(crate) fn to_local` so `app.rs` can call it. `date_of_utc` returns `(i64, u32, u32)` and `days_from_civil(i64, u32, u32) -> i64` are already `pub(crate)`.

In `lookxy/src/main.rs`, add a per-tick call in `run`, right after `drain_events(app);`:

```rust
        app.check_due_reminders(now_unix_secs());
```

and the helper (free fn in `main.rs`):

```rust
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 5: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy reminder` and `bash "$LCARGO" test -p lookxy utc_to_epoch` and `bash "$LCARGO" test -p lookxy agwinterm_notify` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add lookxy/src/app.rs lookxy/src/main.rs
git commit -m "lookxy: per-tick reminder check + optional agwinterm notify"
```

---

### Task 4: Reminder banner + Esc dismiss (UI)

**Files:**
- Modify: `lookxy/src/ui/mod.rs` (top banner in `draw`; Esc routing in `handle_key`; test)

**Interfaces:**
- Consumes: `App.reminder_queue` (Task 3), `App::dismiss_reminder` (Task 3).

- [ ] **Step 1: Write the failing render test**

Add to the `tests` module in `lookxy/src/ui/mod.rs`:

```rust
    #[test]
    fn reminder_banner_renders_front_and_more_count() {
        let mut app = App::for_test_with_seeded_store();
        app.reminder_queue.push_back("⏰ Standup starts in 5 min (09:00)".into());
        app.reminder_queue.push_back("⏰ Review starts in 8 min (09:03)".into());
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Standup starts in 5 min"));
        assert!(text.contains("+1 more"));
        assert!(text.contains("Esc"));
    }

    #[test]
    fn esc_dismisses_reminder_banner() {
        let mut app = App::for_test_with_seeded_store();
        app.reminder_queue.push_back("⏰ Standup".into());
        handle_key(&mut app, KeyEvent::from(KeyCode::Esc));
        assert!(app.reminder_queue.is_empty());
    }
```

(Import `App`, `Terminal`/`TestBackend`, `KeyEvent`/`KeyCode` in the test module as the other `ui::mod` tests already do — reuse their `use` lines.)

- [ ] **Step 2: Run to verify they fail**

Run: `bash "$LCARGO" test -p lookxy -- reminder_banner_renders esc_dismisses_reminder` (single filter) `bash "$LCARGO" test -p lookxy reminder_banner` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — banner not rendered / Esc not routed.

- [ ] **Step 3: Draw the banner + carve the working area**

In `lookxy/src/ui/mod.rs` `draw`, at the very top (after the `oof_form` full-frame early return is fine to leave before this — but the banner should show in the normal views), compute the working area once and render the banner. Replace the start of `draw`:

```rust
pub fn draw(f: &mut Frame, app: &mut App) {
    // Reminder banner: a 1-row strip at the top when reminders are queued; the
    // rest of the UI lays out against the remaining area.
    let full = f.area();
    let area = if app.reminder_queue.is_empty() {
        full
    } else {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(full);
        draw_reminder_banner(f, app, split[0]);
        split[1]
    };

    if app.oof_form.is_some() {
        oofform::draw(f, &*app);
        return;
    }
    // …existing body continues, but every `f.area()` used for the panes/
    // calendar/compose layout below must become `area`…
```

Then change the branches that currently split `f.area()` (the Calendar branch's `calendar::draw_calendar` uses `f.area()` internally — leave calendar/compose/oof as full-frame for v1; the Mail three-pane `Layout::…split(f.area())` becomes `.split(area)`). Concretely: in the Mail-mode body, change `.split(f.area())` (the `rows` vertical split) to `.split(area)`. Leave the full-frame overlays (compose/oof) and the Calendar branch using `f.area()` — the banner still draws above them because it's rendered before the early returns; on those screens the top row is the banner and the overlay draws over the rest (acceptable: overlays `Clear` their own frame, so the banner row is overwritten by a full-frame `Clear` — that's fine, reminders while composing just wait).

Add the banner renderer:

```rust
fn draw_reminder_banner(f: &mut Frame, app: &App, area: Rect) {
    let front = app.reminder_queue.front().cloned().unwrap_or_default();
    let more = app.reminder_queue.len().saturating_sub(1);
    let extra = if more > 0 { format!("  (+{more} more)") } else { String::new() };
    let text = format!("{front}{extra}   [Esc to dismiss]");
    let para = ratatui::widgets::Paragraph::new(text)
        .style(Style::default().fg(Color::Black).bg(Color::Yellow));
    f.render_widget(para, area);
}
```

(Add any missing imports — `Rect`, `Constraint`, `Direction`, `Layout`, `Style`, `Color` — that `ui::mod` doesn't already have.)

- [ ] **Step 4: Route Esc to dismiss**

In `ui::handle_key`, after the full-screen overlay handlers (signin / oof / category picker / file picker / compose / event form / confirm) and before the category-filter Esc / mode branches, add:

```rust
    if key.code == KeyCode::Esc && !app.reminder_queue.is_empty() {
        app.dismiss_reminder();
        return;
    }
```

- [ ] **Step 5: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy reminder_banner` and `bash "$LCARGO" test -p lookxy esc_dismisses` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 6: Full workspace gate**

Run: `bash "$LCARGO" test --workspace`, then `bash "$LCARGO" clippy --workspace --all-targets -- -D warnings`, then `bash "$LCARGO" fmt --all` + `bash "$LCARGO" fmt --all -- --check` (Bash, `dangerouslyDisableSandbox: true`)
Expected: all green, clippy clean, fmt clean. Fix any `Event`/`NewEvent`/`EventRow` literal in lookxy tests the build flags (add `reminder_minutes: 0, is_reminder_on: false,`).

- [ ] **Step 7: Commit**

```bash
git add lookxy/src/ui/mod.rs
git commit -m "lookxy: reminder banner + Esc dismiss"
```

---

## Self-Review

**Spec coverage:**
- `Event`/`EventRow`/`NewEvent` reminder fields + store column/migration/round-trip → Task 1. ✅
- `reminders_notify` config (default false, JSON + env) → Task 2. ✅
- `check_due_reminders` (window math, de-dup, banner push, flag-gated agwinterm), `utc_to_epoch`, `notify_agwinterm` seam, per-tick call → Task 3. ✅
- Banner draw (front + `(+N more)` + Esc hint) + Esc dismiss routing → Task 4. ✅
- Error handling: reminder-off / out-of-window / at-or-after-start / de-dup all covered by Task 3 tests; agwinterm env-guard + best-effort spawn in `notify_agwinterm`. ✅

**Placeholder scan:** No TBD/TODO. The NOTEs flag concrete real-code checks (`to_local` fields, `date_of_utc`/`days_from_civil` types, the `NewEvent` fixture helper, the agenda window covering the test date, the exact `f.area()`→`area` sites) with how to resolve each. The cross-task wiring subtlety (config→app line lands in Task 3) is called out explicitly in both tasks.

**Type consistency:** `reminder_minutes: i64` / `is_reminder_on: bool` identical across `Event` (T1), `EventRow`/`NewEvent` (T1), and the app scan (T3). `reminders_notify: bool` consistent Config (T2) ↔ App (T3) ↔ main wiring. `check_due_reminders(&mut self, now_epoch: i64)`, `dismiss_reminder(&mut self)`, `utc_to_epoch(&str) -> i64` used identically across T3 and T4 tests. `reminder_queue: VecDeque<String>` consistent T3 ↔ T4.
