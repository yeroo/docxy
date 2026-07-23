# lookxy Recurring Event Creation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user create recurring events (daily/weekly/monthly, interval, weekday selection, end date) from the event-create form, serialized to Graph's `event.recurrence`.

**Architecture:** A `Recurrence` model threads from the event form through `LocalEventFields` → store (`events.recurrence` column) → `event_for_send` → `EventInput` → `event_body_json`, reusing the existing `CreateEvent` optimistic-outbox path. Create-only.

**Tech Stack:** Rust (edition 2024), hand-rolled `mailcore::json`, `ureq` Graph client, `rusqlite` store, `ratatui`/`crossterm` TUI.

## Global Constraints

- **Build wrapper:** never call bare `cargo` (fails os error 448). Use `bash "$LCARGO" <args>` where `LCARGO=C:/Users/BORIS_~1/AppData/Local/Temp/claude/C--Users-boris-kudriashov-Source-docxy/1da9a016-b606-4432-8951-6d73bb91c967/scratchpad/lcargo.sh`. EVERY Bash tool call that runs it MUST set `dangerouslyDisableSandbox: true`.
- **MSRV / edition:** edition 2024, MSRV 1.88. No new external crates.
- **No serde:** JSON only via `mailcore::json` (`Value`, `parse`, `.get`/`.as_str`/`.as_i64`/`.as_array`/`Object`/`Str`/`Array`/`Number`, `Value::to_string`).
- **Recurrence types:** `daily` / `weekly` / `absoluteMonthly` only. Weekly sends `daysOfWeek` + `firstDayOfWeek:"sunday"`; monthly sends `dayOfMonth`.
- **End:** `until` date → `range.type = endDate`; blank → `noEnd`. Always `range.startDate`. No occurrence count.
- **Create-only:** recurrence is built only when `editing_id.is_none()`; edits never attach it.
- **Weekday keys:** `1`–`7` toggle Mon–Sun while the Days field is focused.
- **Secrets:** never log tokens/bodies.

---

### Task 1: `Recurrence` model + JSON

**Files:**
- Modify: `mailcore/src/graph/model.rs` (`RecurrenceKind`, `Recurrence`, `to_json`, `from_json`; tests)

**Interfaces:**
- Produces: `pub enum RecurrenceKind { Daily, Weekly, Monthly }`; `pub struct Recurrence { kind: RecurrenceKind, interval: u32, days_of_week: Vec<String>, day_of_month: u32, start_date: String, until: Option<String> }` with `pub fn to_json(&self) -> Value` and `pub fn from_json(&Value) -> Option<Self>`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `mailcore/src/graph/model.rs`:

```rust
    #[test]
    fn recurrence_weekly_to_json_round_trips() {
        let r = Recurrence {
            kind: RecurrenceKind::Weekly,
            interval: 2,
            days_of_week: vec!["monday".into(), "wednesday".into()],
            day_of_month: 0,
            start_date: "2026-07-20".into(),
            until: Some("2026-12-31".into()),
        };
        let v = r.to_json();
        let pat = v.get("pattern").unwrap();
        assert_eq!(pat.get("type").and_then(Value::as_str), Some("weekly"));
        assert_eq!(pat.get("interval").and_then(Value::as_i64), Some(2));
        assert_eq!(
            pat.get("daysOfWeek").and_then(Value::as_array).unwrap().len(),
            2
        );
        assert_eq!(
            pat.get("firstDayOfWeek").and_then(Value::as_str),
            Some("sunday")
        );
        let range = v.get("range").unwrap();
        assert_eq!(range.get("type").and_then(Value::as_str), Some("endDate"));
        assert_eq!(range.get("startDate").and_then(Value::as_str), Some("2026-07-20"));
        assert_eq!(range.get("endDate").and_then(Value::as_str), Some("2026-12-31"));
        assert_eq!(Recurrence::from_json(&v).unwrap(), r); // round-trip
    }

    #[test]
    fn recurrence_daily_and_monthly_shapes() {
        let daily = Recurrence {
            kind: RecurrenceKind::Daily,
            interval: 1,
            days_of_week: vec![],
            day_of_month: 0,
            start_date: "2026-07-20".into(),
            until: None,
        };
        let v = daily.to_json();
        assert_eq!(v.get("pattern").unwrap().get("type").and_then(Value::as_str), Some("daily"));
        assert!(v.get("pattern").unwrap().get("daysOfWeek").is_none());
        assert_eq!(
            v.get("range").unwrap().get("type").and_then(Value::as_str),
            Some("noEnd")
        );
        assert!(v.get("range").unwrap().get("endDate").is_none());
        assert_eq!(Recurrence::from_json(&v).unwrap(), daily);

        let monthly = Recurrence {
            kind: RecurrenceKind::Monthly,
            interval: 1,
            days_of_week: vec![],
            day_of_month: 15,
            start_date: "2026-07-15".into(),
            until: None,
        };
        let v = monthly.to_json();
        assert_eq!(
            v.get("pattern").unwrap().get("type").and_then(Value::as_str),
            Some("absoluteMonthly")
        );
        assert_eq!(
            v.get("pattern").unwrap().get("dayOfMonth").and_then(Value::as_i64),
            Some(15)
        );
        assert_eq!(Recurrence::from_json(&v).unwrap(), monthly);
    }

    #[test]
    fn recurrence_from_json_rejects_unknown_type() {
        let v = crate::json::parse(
            r#"{"pattern":{"type":"yearly","interval":1},"range":{"type":"noEnd","startDate":"2026-01-01"}}"#,
        )
        .unwrap();
        assert!(Recurrence::from_json(&v).is_none());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `bash "$LCARGO" test -p mailcore recurrence` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — types don't exist.

- [ ] **Step 3: Add the enum + struct + JSON**

In `mailcore/src/graph/model.rs` (near the `Event` type):

```rust
/// The recurrence pattern kind lookxy can create — a subset of Graph's
/// `recurrencePattern.type` (`absoluteMonthly` for `Monthly`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceKind {
    Daily,
    Weekly,
    Monthly,
}

impl RecurrenceKind {
    fn as_wire(&self) -> &'static str {
        match self {
            RecurrenceKind::Daily => "daily",
            RecurrenceKind::Weekly => "weekly",
            RecurrenceKind::Monthly => "absoluteMonthly",
        }
    }
    fn from_wire(s: &str) -> Option<RecurrenceKind> {
        match s {
            "daily" => Some(RecurrenceKind::Daily),
            "weekly" => Some(RecurrenceKind::Weekly),
            "absoluteMonthly" => Some(RecurrenceKind::Monthly),
            _ => None,
        }
    }
}

/// A recurring event's pattern + range, as lookxy creates it. Serializes to
/// Graph's `event.recurrence` (`to_json`); `from_json` round-trips it for the
/// store (see `Store::event_for_send`).
#[derive(Debug, Clone, PartialEq)]
pub struct Recurrence {
    pub kind: RecurrenceKind,
    pub interval: u32,
    pub days_of_week: Vec<String>, // "monday".."sunday" (weekly)
    pub day_of_month: u32,         // absoluteMonthly
    pub start_date: String,        // "YYYY-MM-DD" (range.startDate)
    pub until: Option<String>,     // "YYYY-MM-DD" (range endDate), None = noEnd
}

impl Recurrence {
    pub fn to_json(&self) -> Value {
        let mut pattern = vec![
            (
                "type".to_string(),
                Value::Str(self.kind.as_wire().to_string()),
            ),
            (
                "interval".to_string(),
                Value::Num(self.interval as f64),
            ),
        ];
        if self.kind == RecurrenceKind::Weekly {
            pattern.push((
                "daysOfWeek".to_string(),
                Value::Array(
                    self.days_of_week
                        .iter()
                        .map(|d| Value::Str(d.clone()))
                        .collect(),
                ),
            ));
            pattern.push((
                "firstDayOfWeek".to_string(),
                Value::Str("sunday".to_string()),
            ));
        }
        if self.kind == RecurrenceKind::Monthly {
            pattern.push((
                "dayOfMonth".to_string(),
                Value::Num(self.day_of_month as f64),
            ));
        }
        let mut range = vec![
            (
                "type".to_string(),
                Value::Str(
                    if self.until.is_some() { "endDate" } else { "noEnd" }.to_string(),
                ),
            ),
            (
                "startDate".to_string(),
                Value::Str(self.start_date.clone()),
            ),
        ];
        if let Some(end) = &self.until {
            range.push(("endDate".to_string(), Value::Str(end.clone())));
        }
        Value::Object(vec![
            ("pattern".to_string(), Value::Object(pattern)),
            ("range".to_string(), Value::Object(range)),
        ])
    }

    pub fn from_json(v: &Value) -> Option<Self> {
        let pattern = v.get("pattern")?;
        let range = v.get("range")?;
        let kind = RecurrenceKind::from_wire(pattern.get("type")?.as_str()?)?;
        let interval = pattern.get("interval").and_then(Value::as_i64).unwrap_or(1) as u32;
        let days_of_week = pattern
            .get("daysOfWeek")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let day_of_month = pattern.get("dayOfMonth").and_then(Value::as_i64).unwrap_or(0) as u32;
        let start_date = str_field(range, "startDate");
        let until = range
            .get("endDate")
            .and_then(Value::as_str)
            .map(str::to_string);
        Some(Recurrence {
            kind,
            interval,
            days_of_week,
            day_of_month,
            start_date,
            until,
        })
    }
}
```

CONFIRMED: `mailcore::json::Value::Num(f64)` is the numeric variant; `Value::as_i64` exists (used across `model.rs`).

- [ ] **Step 4: Run to verify they pass**

Run: `bash "$LCARGO" test -p mailcore recurrence` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add mailcore/src/graph/model.rs
git commit -m "mailcore: Recurrence model + Graph recurrence JSON"
```

---

### Task 2: Graph serialization — `EventInput.recurrence` + `event_body_json`

**Files:**
- Modify: `mailcore/src/graph/client.rs` (`EventInput` field, `event_body_json`, import; tests + EventInput literal ripple)

**Interfaces:**
- Consumes: `Recurrence` (Task 1).
- Produces: `EventInput.recurrence: Option<Recurrence>`; `event_body_json` inserts a `"recurrence"` key when `Some`.

- [ ] **Step 1: Add the field + import**

In `mailcore/src/graph/client.rs`, add `Recurrence` to the `use crate::graph::model::{…}` list. Add to `EventInput` (after `body_html`):

```rust
    pub body_html: String,
    /// The recurrence pattern for a repeating event, or `None` for a
    /// single event. Serialized into the create/update body by `event_body_json`.
    pub recurrence: Option<Recurrence>,
```

- [ ] **Step 2: Write the failing test**

Add to the client `tests` module:

```rust
    #[test]
    fn event_body_includes_recurrence_when_present() {
        use crate::graph::model::{Recurrence, RecurrenceKind};
        let input = EventInput {
            subject: "Standup".into(),
            start_utc: "2026-07-20T09:00:00Z".into(),
            end_utc: "2026-07-20T09:15:00Z".into(),
            is_all_day: false,
            location: "".into(),
            attendees: vec![],
            body_html: "".into(),
            recurrence: Some(Recurrence {
                kind: RecurrenceKind::Daily,
                interval: 1,
                days_of_week: vec![],
                day_of_month: 0,
                start_date: "2026-07-20".into(),
                until: None,
            }),
        };
        let body = json::parse(&event_body_json(&input)).unwrap();
        assert!(body.get("recurrence").is_some());
        assert_eq!(
            body.get("recurrence").unwrap().get("pattern").unwrap().get("type").and_then(Value::as_str),
            Some("daily")
        );

        let mut plain = input.clone();
        plain.recurrence = None;
        let body = json::parse(&event_body_json(&plain)).unwrap();
        assert!(body.get("recurrence").is_none());
    }
```

(If `EventInput` doesn't already derive `Clone`, use two separate literals instead of `input.clone()`.)

- [ ] **Step 3: Run to verify it fails**

Run: `bash "$LCARGO" test -p mailcore event_body_includes_recurrence` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — `EventInput` has no `recurrence` field until every literal is updated / the body doesn't emit it.

- [ ] **Step 4: Emit recurrence + fix EventInput literals**

In `event_body_json`, before the final `Value::Object(vec![...])` is returned, the object is built as a `vec![...]` of pairs. Change it to a mutable vec and push recurrence conditionally. Locate the `Value::Object(vec![ ("subject"...) ... ])` return and refactor to:

```rust
    let mut obj = vec![
        ("subject".to_string(), Value::Str(input.subject.clone())),
        ("start".to_string(), dt(&input.start_utc)),
        ("end".to_string(), dt(&input.end_utc)),
        ("isAllDay".to_string(), Value::Bool(input.is_all_day)),
        (
            "location".to_string(),
            Value::Object(vec![(
                "displayName".to_string(),
                Value::Str(input.location.clone()),
            )]),
        ),
        // …keep the remaining existing pairs (attendees, body, etc.) exactly as they are…
    ];
    if let Some(rec) = &input.recurrence {
        obj.push(("recurrence".to_string(), rec.to_json()));
    }
    Value::Object(obj).to_string()
```

Preserve every existing pair (attendees, body) in the vec — only wrap it in `let mut obj = vec![…]` and append recurrence. Then build the workspace and add `recurrence: None` to every `EventInput { … }` literal the compiler flags (the client's own create/update tests):

Run: `bash "$LCARGO" build -p mailcore --all-targets 2>&1 | grep -E "missing field .recurrence|-->"` and add `recurrence: None,` to each flagged `EventInput` literal.

- [ ] **Step 5: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore event_body_includes_recurrence` then `bash "$LCARGO" test -p mailcore create_event` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS (new test + the existing create/update event tests still green).

- [ ] **Step 6: Commit**

```bash
git add mailcore/src/graph/client.rs
git commit -m "mailcore: serialize EventInput.recurrence into the event body"
```

---

### Task 3: Store + outbox threading

**Files:**
- Modify: `mailcore/src/store/schema.rs` (`events.recurrence` column)
- Modify: `mailcore/src/store/mod.rs` (`LocalEventFields.recurrence`, `EventSendData.recurrence`, migration, `create_local_event`, `event_for_send`; tests + literal ripple)
- Modify: `mailcore/src/sync/outbox.rs` (`event_input_for` carries recurrence; sample-fields literal)

**Interfaces:**
- Consumes: `Recurrence`/`Recurrence::to_json`/`from_json` (Task 1), `EventInput.recurrence` (Task 2).
- Produces: `LocalEventFields.recurrence: Option<Recurrence>`; `EventSendData.recurrence: Option<Recurrence>`.

- [ ] **Step 1: Add the column + struct fields**

In `mailcore/src/store/schema.rs`, add `recurrence` to the `events` table (after `body_html`):

```sql
    last_modified    TEXT NOT NULL DEFAULT '',
    body_html        TEXT NOT NULL DEFAULT '',
    recurrence       TEXT NOT NULL DEFAULT ''
);
```

In `mailcore/src/store/mod.rs`, extend the model import to include `Recurrence`; add to `LocalEventFields` and `EventSendData` (after `attendees` in each — order doesn't matter, they're named):

```rust
    pub attendees: Vec<(String, String)>,
    /// The recurrence pattern for a repeating event (create-only), or `None`.
    pub recurrence: Option<Recurrence>,
}
```

(Add the identical field + doc to BOTH `LocalEventFields` and `EventSendData`.)

Add the idempotent migration in `Store::init`, next to the other event ALTERs (after the `body_html` one):

```rust
        let _ = conn.execute(
            "ALTER TABLE events ADD COLUMN recurrence TEXT NOT NULL DEFAULT ''",
            [],
        );
```

- [ ] **Step 2: Write the failing store test**

In `mailcore/src/store/mod.rs` tests:

```rust
    #[test]
    fn create_local_event_round_trips_recurrence() {
        use crate::graph::model::{Recurrence, RecurrenceKind};
        let s = Store::open_in_memory().unwrap();
        let rec = Recurrence {
            kind: RecurrenceKind::Weekly,
            interval: 1,
            days_of_week: vec!["monday".into()],
            day_of_month: 0,
            start_date: "2026-07-20".into(),
            until: Some("2026-08-31".into()),
        };
        let mut f = sample_fields();
        f.recurrence = Some(rec.clone());
        let id = s.create_local_event(&f, "Me", "me@x").unwrap();
        let sent = s.event_for_send(&id).unwrap().unwrap();
        assert_eq!(sent.recurrence, Some(rec));

        // A non-recurring event round-trips to None.
        let id2 = s.create_local_event(&sample_fields(), "Me", "me@x").unwrap();
        assert_eq!(s.event_for_send(&id2).unwrap().unwrap().recurrence, None);
    }
```

- [ ] **Step 3: Persist + read recurrence; fix `sample_fields`**

Add `recurrence: None,` to the `sample_fields()` test helper (store tests) and to `sample_event_fields()` in `mailcore/src/sync/outbox.rs` tests.

In `create_local_event`, after `self.put_event_attendees(&id, …)?;` and before `Ok(id)`, persist the recurrence:

```rust
        self.put_event_attendees(&id, &to_new_attendees(&f.attendees))?;
        if let Some(rec) = &f.recurrence {
            self.conn.execute(
                "UPDATE events SET recurrence = ?1 WHERE id = ?2",
                params![rec.to_json().to_string(), id],
            )?;
        }
        Ok(id)
```

In `event_for_send`, add `recurrence` to the SELECT and parse it:

```rust
        let row = self.conn.query_row(
            "SELECT subject, start_utc, end_utc, is_all_day, location, body_html, recurrence FROM events WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        );
        let (subject, start_utc, end_utc, is_all_day, location, body_html, recurrence_raw) =
            match row {
                Ok(t) => t,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
        let recurrence = if recurrence_raw.is_empty() {
            None
        } else {
            crate::json::parse(&recurrence_raw)
                .ok()
                .and_then(|v| crate::graph::model::Recurrence::from_json(&v))
        };
```

Then add `recurrence,` to the `Ok(Some(EventSendData { … }))` constructor at the end of `event_for_send`.

- [ ] **Step 4: Carry recurrence into `EventInput`**

In `mailcore/src/sync/outbox.rs` `event_input_for`, the returned `EventInput { … }` builds from the `EventSendData d`. Add `recurrence: d.recurrence,` to that literal (and it consumes `d`, so move is fine — place it after the existing fields).

- [ ] **Step 5: Build all targets, fix remaining literals, run tests**

Run: `bash "$LCARGO" build -p mailcore --all-targets 2>&1 | grep -E "missing field|-->"` — add `recurrence: None,` to any remaining `LocalEventFields`/`EventSendData` literal flagged.
Run: `bash "$LCARGO" test -p mailcore -- create_local_event_round_trips_recurrence event_input_for` then `bash "$LCARGO" test -p mailcore` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS, whole `mailcore` green.

- [ ] **Step 6: Commit**

```bash
git add mailcore/src/store/schema.rs mailcore/src/store/mod.rs mailcore/src/sync/outbox.rs
git commit -m "mailcore: persist + thread event recurrence through the store/outbox"
```

---

### Task 4: Event form fields + save + detail marker

**Files:**
- Modify: `lookxy/src/ui/eventform.rs` (`EventForm` fields, `EventField` variants, `cycle_focus`/back, `draw`, `handle_key`; blank_form test helper)
- Modify: `lookxy/src/app.rs` (`open_new_event` init, `save_event_form` builds recurrence; `LocalEventFields` literal; tests)
- Modify: `lookxy/src/ui/calendar.rs` (`↻ repeats` in `detail_header_lines`; test)

**Interfaces:**
- Consumes: `RecurrenceKind`/`Recurrence` (Task 1), `LocalEventFields.recurrence` (Task 3), `ui::calendar::{date_of_utc, days_from_civil}` (`pub(crate)`).
- Produces: `EventForm.repeat: Option<RecurrenceKind>`, `.interval: String`, `.days: [bool;7]`, `.until: String`; `EventField::{Repeat, Interval, Days, Until}`.

- [ ] **Step 1: Add the form fields + focus cycle**

In `lookxy/src/ui/eventform.rs`, import `RecurrenceKind` (`use mailcore::graph::model::RecurrenceKind;`). Add variants to `EventField` (after `AllDay`):

```rust
pub enum EventField {
    Title,
    Start,
    End,
    AllDay,
    Repeat,
    Interval,
    Days,
    Until,
    Location,
    Attendees,
    Body,
}
```

Add fields to `EventForm` (after `all_day`):

```rust
    pub all_day: bool,
    /// `None` = a single event; `Some(kind)` = repeat daily/weekly/monthly.
    pub repeat: Option<RecurrenceKind>,
    pub interval: String, // numeric text, default "1"
    pub days: [bool; 7],  // Mon..Sun, for weekly
    pub until: String,    // "YYYY-MM-DD" or "" (no end)
```

Update `cycle_focus` to the new order and add a matching `cycle_focus_back` if the file has one (grep `EventField::Body => EventField::` and `prev`). New forward order:

```rust
fn cycle_focus(form: &mut EventForm) {
    form.focus = match form.focus {
        EventField::Title => EventField::Start,
        EventField::Start => EventField::End,
        EventField::End => EventField::AllDay,
        EventField::AllDay => EventField::Repeat,
        EventField::Repeat => EventField::Interval,
        EventField::Interval => EventField::Days,
        EventField::Days => EventField::Until,
        EventField::Until => EventField::Location,
        EventField::Location => EventField::Attendees,
        EventField::Attendees => EventField::Body,
        EventField::Body => EventField::Title,
    };
}
```

(If a back-cycle exists — Shift-Tab — extend it symmetrically.)

- [ ] **Step 2: Add repeat-cycle + days-toggle key handling**

In `eventform::handle_key`, add handling. Repeat cycles on Space; Days toggles on `1`–`7`. Find the per-field key `match` (the same place `Space` on `AllDay` is handled) and add:

```rust
        KeyCode::Char(' ') if form.focus == EventField::Repeat => {
            form.repeat = match form.repeat {
                None => Some(RecurrenceKind::Daily),
                Some(RecurrenceKind::Daily) => Some(RecurrenceKind::Weekly),
                Some(RecurrenceKind::Weekly) => Some(RecurrenceKind::Monthly),
                Some(RecurrenceKind::Monthly) => None,
            };
        }
        KeyCode::Char(c @ '1'..='7') if form.focus == EventField::Days => {
            let i = c as usize - '1' as usize; // '1'->0 (Mon) .. '7'->6 (Sun)
            form.days[i] = !form.days[i];
        }
```

And route char input for the `Interval`/`Until` text fields the same way the existing `Location`/`Attendees` single-line fields consume `KeyCode::Char(c)` / `KeyCode::Backspace` (add `EventField::Interval` and `EventField::Until` arms that push/pop on `form.interval`/`form.until`). Place the two `Char`-guarded arms above the generic text `KeyCode::Char(c)` arm so they take precedence.

- [ ] **Step 3: Render the new rows**

In `eventform::draw`, extend the vertical layout constraints with four more `Constraint::Length(3)` rows (Repeat, Interval, Days, Until) inserted after All-day and before Location, and draw them. Use the existing `draw_field`/`draw_all_day` helpers as the model:

```rust
    // Repeat radio.
    let repeat_label = match form.repeat {
        None => "( )None  ( )Daily  ( )Weekly  ( )Monthly",
        Some(RecurrenceKind::Daily) => "( )None  (x)Daily  ( )Weekly  ( )Monthly",
        Some(RecurrenceKind::Weekly) => "( )None  ( )Daily  (x)Weekly  ( )Monthly",
        Some(RecurrenceKind::Monthly) => "( )None  ( )Daily  ( )Weekly  (x)Monthly",
    };
    draw_field(f, rows[N0], "Repeat", repeat_label, form.focus == EventField::Repeat);
    // Interval / Days / Until — dimmed when not applicable is optional; render plainly.
    draw_field(f, rows[N1], "Every", &form.interval, form.focus == EventField::Interval);
    let days_label = render_days(&form.days); // "[x]Mon [ ]Tue ..."
    draw_field(f, rows[N2], "Days", &days_label, form.focus == EventField::Days);
    draw_field(f, rows[N3], "Until", &form.until, form.focus == EventField::Until);
```

with a helper:

```rust
fn render_days(days: &[bool; 7]) -> String {
    const ABBR: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    days.iter()
        .zip(ABBR)
        .map(|(on, a)| format!("[{}]{a}", if *on { "x" } else { " " }))
        .collect::<Vec<_>>()
        .join(" ")
}
```

Adjust the `rows[...]` indices (`N0..N3` and the subsequent Location/Attendees/Body indices) to the new layout. Match `draw_field`'s actual signature (grep its definition — it may take `(f, area, label, value, focused)`).

- [ ] **Step 4: Initialize the fields + build recurrence on save**

In `lookxy/src/app.rs` `open_new_event`, add the new fields to the `EventForm { … }` it constructs: `repeat: None, interval: "1".into(), days: [false; 7], until: String::new(),`. Also add the same to any other `EventForm { … }` literal (`open_edit_event`, and the `blank_form()` test helper in eventform.rs).

In `save_event_form`, after `(start_utc, end_utc)` is resolved and before building `LocalEventFields`, build the recurrence (create-only):

```rust
        let recurrence = if form.repeat.is_some() && form.editing_id.is_none() {
            let kind = form.repeat.unwrap();
            let interval: u32 = match form.interval.trim().parse() {
                Ok(n) if n >= 1 => n,
                _ => {
                    self.set_form_error("Invalid interval");
                    return;
                }
            };
            // Start date + day-of-month from the parsed UTC start (YYYY-MM-DD…).
            let start_date = start_utc.get(..10).unwrap_or("").to_string();
            let day_of_month: u32 = start_utc.get(8..10).and_then(|d| d.parse().ok()).unwrap_or(1);
            // Weekly days: toggled Mon..Sun, defaulting to the start's weekday.
            const NAMES: [&str; 7] = [
                "monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday",
            ];
            let mut days_of_week: Vec<String> = (0..7)
                .filter(|&i| form.days[i])
                .map(|i| NAMES[i].to_string())
                .collect();
            if kind == crate::ui::eventform::RecurrenceKindAlias::Weekly && days_of_week.is_empty()
            {
                days_of_week.push(weekday_name_of(&start_utc));
            }
            // `until`: blank = no end; else validate the date and require >= start.
            let until = if form.until.trim().is_empty() {
                None
            } else {
                let u = form.until.trim().to_string();
                let now = local_now();
                let off = crate::ui::calendar::local_offset_minutes();
                if crate::datetime::parse_start(&u, now, off).is_none() {
                    self.set_form_error("Invalid until date");
                    return;
                }
                if u < start_date {
                    self.set_form_error("Until is before start");
                    return;
                }
                Some(u)
            };
            Some(mailcore::graph::model::Recurrence {
                kind,
                interval,
                days_of_week,
                day_of_month,
                start_date,
                until,
            })
        } else {
            None
        };
```

NOTE: `kind` is `RecurrenceKind`; the weekly check is `kind == mailcore::graph::model::RecurrenceKind::Weekly` (do NOT introduce an alias — that placeholder name is illustrative; use the real path). Add `recurrence,` to the `LocalEventFields { … }` literal.

Add the weekday helper as a free fn in `app.rs`:

```rust
/// The Graph weekday name (`"monday".."sunday"`) of a canonical-UTC start
/// timestamp's date, via the calendar module's civil-day math.
fn weekday_name_of(start_utc: &str) -> String {
    const NAMES: [&str; 7] = [
        "sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday",
    ];
    let (y, m, d) = crate::ui::calendar::date_of_utc(start_utc);
    let z = crate::ui::calendar::days_from_civil(y, m, d);
    NAMES[(z + 4).rem_euclid(7) as usize].to_string()
}
```

(This matches `ui::calendar::weekday_abbrev`'s `(z + 4).rem_euclid(7)` Sunday-indexed convention. Confirm `date_of_utc` returns `(i64/u32, u32, u32)` and `days_from_civil(y, m, d) -> i64` — grep their signatures; adjust the tuple types if needed.)

- [ ] **Step 5: `↻ repeats` in the detail view**

In `lookxy/src/ui/calendar.rs` `detail_header_lines(e: &EventRow)`, after the existing header lines vec is built (and before returning), push a repeats line when the event is a series occurrence:

```rust
    if e.series_master_id.is_some() {
        lines.push(Line::from("↻ repeats"));
    }
```

(If `detail_header_lines` returns a `vec![…]` directly, refactor to `let mut lines = vec![…]; …; lines`. Confirm `EventRow` has `series_master_id: Option<String>` — grep it.)

- [ ] **Step 6: Write the failing lookxy tests**

In `lookxy/src/app.rs` tests:

```rust
    #[test]
    fn save_event_form_weekly_builds_recurrence() {
        use crate::ui::eventform::EventForm;
        use mailcore::graph::model::RecurrenceKind;
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        let mut days = [false; 7];
        days[0] = true; // Mon
        days[2] = true; // Wed
        app.event_form = Some(EventForm {
            editing_id: None,
            title: "Standup".into(),
            start: "2026-07-20 09:00".into(),
            end: "2026-07-20 09:15".into(),
            all_day: false,
            repeat: Some(RecurrenceKind::Weekly),
            interval: "2".into(),
            days,
            until: "2026-12-31".into(),
            location: String::new(),
            attendees: String::new(),
            body: String::new(),
            focus: crate::ui::eventform::EventField::Title,
            autocomplete: None,
            error: None,
        });
        app.save_event_form();
        // The event form closed (no error) and a CreateEvent went out.
        assert!(app.event_form.is_none(), "form should close on success");
        // The stored local event carries the recurrence.
        let ev = app
            .store
            .events_in_window("2026-07-01T00:00:00Z", "2026-08-01T00:00:00Z")
            .unwrap();
        let id = ev.iter().find(|e| e.subject == "Standup").unwrap().id.clone();
        let sent = app.store.event_for_send(&id).unwrap().unwrap();
        let rec = sent.recurrence.unwrap();
        assert_eq!(rec.kind, RecurrenceKind::Weekly);
        assert_eq!(rec.interval, 2);
        assert_eq!(rec.days_of_week, vec!["monday".to_string(), "wednesday".to_string()]);
        assert_eq!(rec.until.as_deref(), Some("2026-12-31"));
    }

    #[test]
    fn save_event_form_invalid_interval_errors_and_sends_nothing() {
        use crate::ui::eventform::EventForm;
        use mailcore::graph::model::RecurrenceKind;
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.event_form = Some(EventForm {
            editing_id: None,
            title: "X".into(),
            start: "2026-07-20 09:00".into(),
            end: "2026-07-20 09:15".into(),
            all_day: false,
            repeat: Some(RecurrenceKind::Daily),
            interval: "zero".into(),
            days: [false; 7],
            until: String::new(),
            location: String::new(),
            attendees: String::new(),
            body: String::new(),
            focus: crate::ui::eventform::EventField::Title,
            autocomplete: None,
            error: None,
        });
        app.save_event_form();
        assert!(app.event_form.is_some()); // stayed open
        assert_eq!(
            app.event_form.as_ref().unwrap().error.as_deref(),
            Some("Invalid interval")
        );
    }
```

NOTE: confirm `Store::events_in_window(from, to)` is the accessor the app uses to read the agenda (grep `events_in_window` / how `reload_agenda` reads events); if the name/signature differs, use the same call `reload_agenda` uses. The assertion only needs to find the created local event's id.

- [ ] **Step 7: Run to verify + fmt**

Run: `bash "$LCARGO" test -p lookxy -- save_event_form_weekly save_event_form_invalid_interval` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS. Fix any signature/tuple mismatches the compiler surfaces (the NOTEs above).

- [ ] **Step 8: Full workspace gate**

Run: `bash "$LCARGO" test --workspace`, then `bash "$LCARGO" clippy --workspace --all-targets -- -D warnings`, then `bash "$LCARGO" fmt --all` + `bash "$LCARGO" fmt --all -- --check` (Bash, `dangerouslyDisableSandbox: true`)
Expected: all green, clippy clean, fmt clean.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "lookxy: recurrence fields in the event form + repeats marker"
```

---

## Self-Review

**Spec coverage:**
- `Recurrence` model + `to_json`/`from_json` (daily/weekly/monthly, noEnd/endDate) → Task 1. ✅
- `EventInput.recurrence` + `event_body_json` include/omit → Task 2. ✅
- `LocalEventFields`/`EventSendData` recurrence + `events.recurrence` column + migration + `create_local_event` persist + `event_for_send` read + `event_input_for` carry → Task 3. ✅
- Form fields (Repeat/Every/Days/Until, 1–7 weekday toggles, Space cycle) + `save_event_form` build (interval/until validation, weekday default, create-only) + `↻ repeats` marker → Task 4. ✅
- Error handling: invalid interval / invalid-or-past until → inline error (Task 4); weekly no-days default (Task 4); create-only guard (`editing_id.is_none()`, Task 4). ✅

**Placeholder scan:** No TBD/TODO. The NOTEs flag concrete real-code checks (the `Value` numeric variant name, `date_of_utc`/`days_from_civil` tuple types, `draw_field` signature, `events_in_window` accessor, layout row indices) with the exact grep to resolve each — not deferred work. One illustrative bad name (`RecurrenceKindAlias`) is explicitly called out in its NOTE as "do not use — use the real path `mailcore::graph::model::RecurrenceKind::Weekly`."

**Type consistency:** `Recurrence { kind, interval, days_of_week, day_of_month, start_date, until }` identical across model (T1), client `EventInput` (T2), store `LocalEventFields`/`EventSendData` (T3), and app `save_event_form` (T4). `RecurrenceKind::{Daily,Weekly,Monthly}` consistent. `to_json`/`from_json` used consistently (client serializes, store round-trips). `EventForm` new fields (`repeat: Option<RecurrenceKind>`, `interval: String`, `days: [bool;7]`, `until: String`) match between the struct (T4 S1), the tests (T4 S6), and `save_event_form` (T4 S4).
