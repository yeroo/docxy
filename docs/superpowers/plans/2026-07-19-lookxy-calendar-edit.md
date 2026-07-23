# lookxy Calendar Create / Edit / Delete Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create, edit, and delete single calendar events from lookxy's calendar view, via an event form and the existing optimistic-local + outbox pattern.

**Architecture:** An event form collects title/start/end/all-day/location/attendees/body; saving writes an optimistic local event and enqueues a `CreateEvent`/`UpdateEvent`/`DeleteEvent` outbox op (mirroring `RespondEvent`), which the sync engine drains to Graph (`POST`/`PATCH`/`DELETE /me/events`), reconciling a `local:` id to the Graph id on create. Local-time input is parsed to UTC by a bounded, deterministic parser; attendee entry reuses the contacts autocomplete.

**Tech Stack:** Rust (edition 2024, MSRV 1.88), rusqlite, ureq+rustls Graph client, ratatui 0.29, hand-rolled `mailcore::json` (no serde). No new dependencies.

## Global Constraints

- **Build/test ONLY through the wrapper** (bare `cargo` fails with os error 448). Every command is `bash "$LCARGO" …` where
  `LCARGO = C:\Users\BORIS_~1\AppData\Local\Temp\claude\C--Users-boris-kudriashov-Source-docxy\1da9a016-b606-4432-8951-6d73bb91c967\scratchpad\lcargo.sh`
  Run it via the Bash tool with `dangerouslyDisableSandbox: true`.
- **No new dependencies.** Hand-rolled JSON/date math.
- **MSRV 1.88, edition 2024.** Let-chains available.
- **CI runs `cargo clippy --all-targets -- -D warnings` on ubuntu/macos/windows.** No warnings; no `#[cfg(windows)]`-only bindings left unused on Unix. Run `bash "$LCARGO" fmt` before every commit.
- **Preserve existing behavior.** Existing calendar read + RSVP, mail, compose, and every other feature must stay green; extend only where a task says so.
- **UTC canonical format:** timestamps are `YYYY-MM-DDTHH:MM:SSZ` (zero-padded, `Z` suffix) so lexical order == chronological — matches `graph::model::to_utc` and the store's `start_utc`/`end_utc`.
- **Optimistic-local + outbox:** save/delete write the local store first, then enqueue an op the engine drains; never call Graph directly from the UI.
- **Single events only.** New events are non-recurring; an event with `series_master_id.is_some()` is RSVP-only — `e`/`x` on it are refused with a notice.

---

### Task 1: Local-time datetime parser

**Files:**
- Create: `lookxy/src/datetime.rs`
- Modify: `lookxy/src/main.rs` (or `lookxy/src/lib`-equivalent module root — add `mod datetime;`), `lookxy/src/ui/calendar.rs` (make `civil_from_days`/`days_from_civil` `pub(crate)`)

**Interfaces:**
- Produces:
  - `pub struct LocalDateTime { pub year: i64, pub month: u32, pub day: u32, pub hour: u32, pub min: u32 }`
  - `pub fn parse_start(input: &str, now: LocalDateTime, offset_min: i64) -> Option<String>` — UTC ISO
  - `pub fn parse_end(input: &str, start_utc: &str, now: LocalDateTime, offset_min: i64) -> Option<String>` — UTC ISO; additionally accepts `+Nh`/`+Nm`/`+Nd` relative to `start_utc`

- [ ] **Step 1: Write the failing tests**

Create `lookxy/src/datetime.rs` with a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // 2026-07-19 10:00 local, UTC+3 (offset_min = 180) — like EPAM/MSK.
    fn now() -> LocalDateTime { LocalDateTime { year: 2026, month: 7, day: 19, hour: 10, min: 0 } }
    const OFF: i64 = 180;

    #[test]
    fn parses_fixed_datetime_to_utc() {
        // 14:00 local at +3 → 11:00 UTC
        assert_eq!(parse_start("2026-07-20 14:00", now(), OFF), Some("2026-07-20T11:00:00Z".into()));
    }
    #[test]
    fn parses_bare_date_as_midnight() {
        // 2026-07-20 00:00 local at +3 → 2026-07-19 21:00 UTC
        assert_eq!(parse_start("2026-07-20", now(), OFF), Some("2026-07-19T21:00:00Z".into()));
    }
    #[test]
    fn parses_today_and_tomorrow_with_time() {
        assert_eq!(parse_start("today 09:30", now(), OFF), Some("2026-07-19T06:30:00Z".into()));
        assert_eq!(parse_start("tomorrow 09:30", now(), OFF), Some("2026-07-20T06:30:00Z".into()));
    }
    #[test]
    fn parses_bare_time_and_12h() {
        assert_eq!(parse_start("14:00", now(), OFF), Some("2026-07-19T11:00:00Z".into()));
        assert_eq!(parse_start("2pm", now(), OFF), Some("2026-07-19T11:00:00Z".into()));
        assert_eq!(parse_start("2:30pm", now(), OFF), Some("2026-07-19T11:30:00Z".into()));
        // 12am → 00:00 today (2026-07-19) local; at +3 that's 21:00 the PREVIOUS UTC day
        assert_eq!(parse_start("12am", now(), OFF), Some("2026-07-18T21:00:00Z".into()));
    }
    #[test]
    fn end_accepts_relative_to_start() {
        let start = "2026-07-20T11:00:00Z"; // 14:00 local
        assert_eq!(parse_end("+1h", start, now(), OFF), Some("2026-07-20T12:00:00Z".into()));
        assert_eq!(parse_end("+90m", start, now(), OFF), Some("2026-07-20T12:30:00Z".into()));
        assert_eq!(parse_end("+1d", start, now(), OFF), Some("2026-07-21T11:00:00Z".into()));
        // non-relative end still works
        assert_eq!(parse_end("2026-07-20 15:00", start, now(), OFF), Some("2026-07-20T12:00:00Z".into()));
    }
    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_start("", now(), OFF), None);
        assert_eq!(parse_start("not a time", now(), OFF), None);
        assert_eq!(parse_start("25:99", now(), OFF), None);
        assert_eq!(parse_start("2026-13-40", now(), OFF), None);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy datetime`
Expected: FAIL — module/functions missing.

- [ ] **Step 3: Implement**

In `lookxy/src/ui/calendar.rs`, change `fn civil_from_days` and `fn days_from_civil` to `pub(crate) fn` (visibility only).

Write `lookxy/src/datetime.rs`:

```rust
//! Parses local-time event-form input into UTC ISO timestamps. A bounded,
//! deterministic grammar (fixed format, `today`/`tomorrow`, bare/12-hour time,
//! `+Nh/m/d` relative) — not open-ended natural language. `now`/`offset_min`
//! are passed in (never read from the clock) so every shape is unit-testable.

use crate::ui::calendar::{civil_from_days, days_from_civil};

/// A local wall-clock instant, no timezone.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LocalDateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
}

/// Minutes since the Unix epoch for a local instant (used for UTC conversion
/// and relative arithmetic — handles all rollover via day-count math).
fn to_epoch_min(t: LocalDateTime) -> i64 {
    days_from_civil(t.year, t.month, t.day) * 1440 + (t.hour as i64) * 60 + t.min as i64
}

fn from_epoch_min(total: i64) -> LocalDateTime {
    let days = total.div_euclid(1440);
    let rem = total.rem_euclid(1440);
    let (y, m, d) = civil_from_days(days);
    LocalDateTime { year: y, month: m, day: d, hour: (rem / 60) as u32, min: (rem % 60) as u32 }
}

/// Formats a local instant as a UTC ISO timestamp, subtracting the local
/// offset (`offset_min` = minutes east of UTC).
fn to_utc_iso(t: LocalDateTime, offset_min: i64) -> String {
    let u = from_epoch_min(to_epoch_min(t) - offset_min);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:00Z", u.year, u.month, u.day, u.hour, u.min)
}

/// UTC ISO → local instant (for the End field's `+rel` base).
fn utc_iso_to_local(utc: &str, offset_min: i64) -> Option<LocalDateTime> {
    // YYYY-MM-DDTHH:MM:SSZ
    let (date, rest) = utc.split_once('T')?;
    let (y, m, d) = parse_ymd(date)?;
    let hh: u32 = rest.get(0..2)?.parse().ok()?;
    let mm: u32 = rest.get(3..5)?.parse().ok()?;
    let base = LocalDateTime { year: y, month: m, day: d, hour: hh, min: mm };
    Some(from_epoch_min(to_epoch_min(base) + offset_min))
}

fn parse_ymd(s: &str) -> Option<(i64, u32, u32)> {
    let mut it = s.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// `HH:MM` (24-hour) or `H[:MM]am|pm` (12-hour). Returns (hour, min) in 24h.
fn parse_time(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(rest) = s.strip_suffix("am").or_else(|| s.strip_suffix("pm")) {
        let pm = s.ends_with("pm");
        let (h, m) = match rest.split_once(':') {
            Some((h, m)) => (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?),
            None => (rest.trim().parse::<u32>().ok()?, 0),
        };
        if !(1..=12).contains(&h) || m > 59 {
            return None;
        }
        let h24 = match (h, pm) {
            (12, false) => 0,   // 12am → 00
            (12, true) => 12,   // 12pm → 12
            (h, false) => h,
            (h, true) => h + 12,
        };
        return Some((h24, m));
    }
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some((h, m))
}

/// Parses one non-relative local input into a `LocalDateTime`.
fn parse_local(input: &str, now: LocalDateTime) -> Option<LocalDateTime> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // "today"/"tomorrow" [time]
    for (word, add_days) in [("today", 0i64), ("tomorrow", 1)] {
        if let Some(rest) = s.strip_prefix(word) {
            let base = from_epoch_min(to_epoch_min(LocalDateTime { hour: 0, min: 0, ..now }) + add_days * 1440);
            let (h, m) = if rest.trim().is_empty() { (0, 0) } else { parse_time(rest)? };
            return Some(LocalDateTime { hour: h, min: m, ..base });
        }
    }
    // "YYYY-MM-DD [HH:MM]"
    if s.len() >= 10 && s.as_bytes()[4] == b'-' {
        let (date_part, time_part) = match s.split_once(' ') {
            Some((d, t)) => (d, Some(t)),
            None => (s, None),
        };
        let (y, mo, d) = parse_ymd(date_part)?;
        let (h, m) = match time_part {
            Some(t) => parse_time(t)?,
            None => (0, 0),
        };
        return Some(LocalDateTime { year: y, month: mo, day: d, hour: h, min: m });
    }
    // bare time → today
    let (h, m) = parse_time(s)?;
    Some(LocalDateTime { hour: h, min: m, ..now })
}

/// `+Nh` / `+Nm` / `+Nd` (the leading `+` already stripped). Returns minutes.
fn parse_relative_minutes(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = num.trim().parse().ok()?;
    match unit {
        "h" | "H" => Some(n * 60),
        "m" | "M" => Some(n),
        "d" | "D" => Some(n * 1440),
        _ => None,
    }
}

pub fn parse_start(input: &str, now: LocalDateTime, offset_min: i64) -> Option<String> {
    Some(to_utc_iso(parse_local(input, now)?, offset_min))
}

pub fn parse_end(input: &str, start_utc: &str, now: LocalDateTime, offset_min: i64) -> Option<String> {
    let s = input.trim();
    if let Some(rel) = s.strip_prefix('+') {
        let base = utc_iso_to_local(start_utc, offset_min)?;
        let delta = parse_relative_minutes(rel)?;
        return Some(to_utc_iso(from_epoch_min(to_epoch_min(base) + delta), offset_min));
    }
    Some(to_utc_iso(parse_local(s, now)?, offset_min))
}
```

Add `mod datetime;` to the crate's module root (wherever `mod ui;` is declared — `lookxy/src/main.rs`).

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy datetime`
Expected: PASS (6 tests).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/datetime.rs lookxy/src/main.rs lookxy/src/ui/calendar.rs
git commit -m "lookxy: local-time event datetime parser (fixed + today/tomorrow + rel + 12h)"
```

---

### Task 2: Graph create / update / delete event

**Files:**
- Modify: `mailcore/src/graph/client.rs` (`EventInput`, `create_event`, `update_event`, `delete_event`, `event_body_json`; tests)

**Interfaces:**
- Consumes: `send`, `parse_body`, `encode_path_segment`, `Value`, `Event`/`Event::from_json`.
- Produces:
  - `pub struct EventInput { pub subject: String, pub start_utc: String, pub end_utc: String, pub is_all_day: bool, pub location: String, pub attendees: Vec<(String, String)>, pub body_html: String }`
  - `pub fn create_event(&self, input: &EventInput) -> Result<Event, GraphError>`
  - `pub fn update_event(&self, id: &str, input: &EventInput) -> Result<(), GraphError>`
  - `pub fn delete_event(&self, id: &str) -> Result<(), GraphError>`

- [ ] **Step 1: Write the failing tests**

Add to `client.rs` tests (mirror the `respond_event`/`create_draft` FakeServer + captured-body style):

```rust
    fn sample_input() -> EventInput {
        EventInput {
            subject: "Sync".into(),
            start_utc: "2026-07-20T11:00:00Z".into(),
            end_utc: "2026-07-20T12:00:00Z".into(),
            is_all_day: false,
            location: "Room 1".into(),
            attendees: vec![("Bob".into(), "bob@x.com".into())],
            body_html: "<p>agenda</p>".into(),
        }
    }

    #[test]
    fn create_event_posts_expected_body() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(), path_prefix: "/me/events".into(), status: 201, headers: vec![],
            body: r#"{"id":"EV1","subject":"Sync","start":{"dateTime":"2026-07-20T11:00:00.0000000","timeZone":"UTC"},"end":{"dateTime":"2026-07-20T12:00:00.0000000","timeZone":"UTC"},"isAllDay":false,"location":{"displayName":"Room 1"},"organizer":{"emailAddress":{"name":"Me","address":"me@x"}},"responseStatus":{"response":"organizer"},"attendees":[],"bodyPreview":"agenda","webLink":"","lastModifiedDateTime":""}"#.into(),
        }]);
        let c = GraphClient::new(srv.base_url(), "T".into());
        let ev = c.create_event(&sample_input()).unwrap();
        assert_eq!(ev.id, "EV1");
        let body = srv.requests()[0].body.clone();
        let sent = mailcore::json::parse(&body).unwrap();
        use mailcore::json::Value;
        assert_eq!(sent.get("subject").and_then(Value::as_str), Some("Sync"));
        // start/end are {dateTime (no Z), timeZone: "UTC"}
        let start = sent.get("start").unwrap();
        assert_eq!(start.get("dateTime").and_then(Value::as_str), Some("2026-07-20T11:00:00"));
        assert_eq!(start.get("timeZone").and_then(Value::as_str), Some("UTC"));
        assert_eq!(sent.get("isAllDay").and_then(Value::as_bool), Some(false));
        assert_eq!(sent.get("location").and_then(|l| l.get("displayName")).and_then(Value::as_str), Some("Room 1"));
        let att = sent.get("attendees").and_then(Value::as_array).unwrap();
        assert_eq!(att[0].get("emailAddress").and_then(|e| e.get("address")).and_then(Value::as_str), Some("bob@x.com"));
        assert_eq!(att[0].get("type").and_then(Value::as_str), Some("required"));
    }

    #[test]
    fn update_event_patches_and_delete_hits_delete() {
        let srv = FakeServer::start(vec![
            Route { method: "PATCH".into(), path_prefix: "/me/events/EV1".into(), status: 200, headers: vec![], body: "{}".into() },
            Route { method: "DELETE".into(), path_prefix: "/me/events/EV2".into(), status: 204, headers: vec![], body: "".into() },
        ]);
        let c = GraphClient::new(srv.base_url(), "T".into());
        c.update_event("EV1", &sample_input()).unwrap();
        c.delete_event("EV2").unwrap();
        assert!(srv.requests().iter().any(|r| r.method == "PATCH" && r.path.contains("/me/events/EV1")));
        assert!(srv.requests().iter().any(|r| r.method == "DELETE" && r.path.contains("/me/events/EV2")));
    }
```

(Adapt `GraphClient::new`, `Route`, `srv.requests()` to the exact shapes the neighbouring tests use. `Method::Patch`/`Delete` already exist in the client's `Method` enum — `mark_read` uses Patch, `delete_message` uses Delete.)

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore create_event update_event`
Expected: FAIL — types/methods missing.

- [ ] **Step 3: Implement**

Add the struct (near `Event` / other model types):

```rust
/// The editable fields of an event, as the create/update calls serialize them.
#[derive(Debug, Clone, PartialEq)]
pub struct EventInput {
    pub subject: String,
    pub start_utc: String, // YYYY-MM-DDTHH:MM:SSZ
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub attendees: Vec<(String, String)>, // (name, address)
    pub body_html: String,
}
```

Add to `impl GraphClient`:

```rust
    /// POST `/me/events` with the event body; returns the created `Event`
    /// (with its Graph-minted id). Graph sends invites to any attendees.
    pub fn create_event(&self, input: &EventInput) -> Result<Event, GraphError> {
        let resp = self.send(Method::Post, "/me/events", Some(event_body_json(input)), &[])?;
        let v = parse_body(resp)?;
        Event::from_json(&v).ok_or_else(|| GraphError::Parse("malformed created event".to_string()))
    }

    /// PATCH `/me/events/{id}` with the same body shape as `create_event`.
    pub fn update_event(&self, id: &str, input: &EventInput) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}");
        self.send(Method::Patch, &path, Some(event_body_json(input)), &[])?;
        Ok(())
    }

    /// DELETE `/me/events/{id}` (cancels an organized event / removes an
    /// invited one from the user's calendar).
    pub fn delete_event(&self, id: &str) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}");
        self.send(Method::Delete, &path, None, &[])?;
        Ok(())
    }
```

Add the body builder (module scope in `client.rs`):

```rust
/// Serializes an `EventInput` to the JSON body shared by `create_event` and
/// `update_event`. `start`/`end` carry the UTC wall clock (the `Z` suffix
/// stripped) with `timeZone:"UTC"`, matching how `calendar_view` reads events
/// back under the `Prefer: outlook.timezone="UTC"` header.
fn event_body_json(input: &EventInput) -> String {
    let dt = |utc: &str| -> Value {
        Value::Object(vec![
            ("dateTime".to_string(), Value::Str(utc.trim_end_matches('Z').to_string())),
            ("timeZone".to_string(), Value::Str("UTC".to_string())),
        ])
    };
    let attendees = Value::Array(
        input
            .attendees
            .iter()
            .map(|(name, addr)| {
                Value::Object(vec![
                    (
                        "emailAddress".to_string(),
                        Value::Object(vec![
                            ("address".to_string(), Value::Str(addr.clone())),
                            ("name".to_string(), Value::Str(name.clone())),
                        ]),
                    ),
                    ("type".to_string(), Value::Str("required".to_string())),
                ])
            })
            .collect(),
    );
    Value::Object(vec![
        ("subject".to_string(), Value::Str(input.subject.clone())),
        ("start".to_string(), dt(&input.start_utc)),
        ("end".to_string(), dt(&input.end_utc)),
        ("isAllDay".to_string(), Value::Bool(input.is_all_day)),
        (
            "location".to_string(),
            Value::Object(vec![("displayName".to_string(), Value::Str(input.location.clone()))]),
        ),
        ("attendees".to_string(), attendees),
        (
            "body".to_string(),
            Value::Object(vec![
                ("contentType".to_string(), Value::Str("HTML".to_string())),
                ("content".to_string(), Value::Str(input.body_html.clone())),
            ]),
        ),
    ])
    .to_string()
}
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore create_event update_event` then `bash "$LCARGO" test -p mailcore`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/graph/client.rs
git commit -m "mailcore: GraphClient create/update/delete_event + EventInput"
```

---

### Task 3: OutboxOp CreateEvent / UpdateEvent / DeleteEvent

**Files:**
- Modify: `mailcore/src/store/mod.rs` (`OutboxOp` enum + `kind`/`to_json`/`from_json`; a round-trip test)

**Interfaces:**
- Produces: `OutboxOp::CreateEvent { id }`, `OutboxOp::UpdateEvent { id }`, `OutboxOp::DeleteEvent { id }` (all id-only, like `SaveDraft`).

- [ ] **Step 1: Write the failing test**

Add to `mailcore/src/store/mod.rs` tests:

```rust
    #[test]
    fn event_mutation_ops_round_trip_through_json() {
        for op in [
            OutboxOp::CreateEvent { id: "local:e1".into() },
            OutboxOp::UpdateEvent { id: "EV1".into() },
            OutboxOp::DeleteEvent { id: "EV2".into() },
        ] {
            assert_eq!(OutboxOp::from_json(&op.to_json()), Some(op));
        }
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore event_mutation_ops_round_trip`
Expected: FAIL — variants don't exist.

- [ ] **Step 3: Implement**

Add the variants to the `OutboxOp` enum (after `RespondEvent`):

```rust
    /// Push a locally-created/edited/deleted calendar event to Graph. `id` is
    /// the store's event id — a `local:` id (before its first `create_event`
    /// reconciles it) or the Graph id. See `sync::outbox::apply_op`.
    CreateEvent { id: String },
    UpdateEvent { id: String },
    DeleteEvent { id: String },
```

Add to `kind`:
```rust
            OutboxOp::CreateEvent { .. } => "createEvent",
            OutboxOp::UpdateEvent { .. } => "updateEvent",
            OutboxOp::DeleteEvent { .. } => "deleteEvent",
```

Add to `to_json` (each id-only, like `SaveDraft`):
```rust
            OutboxOp::CreateEvent { id } | OutboxOp::UpdateEvent { id } | OutboxOp::DeleteEvent { id } => {
                Value::Object(vec![
                    ("kind".to_string(), Value::Str(self.kind().to_string())),
                    ("id".to_string(), Value::Str(id.clone())),
                ])
            }
```

Add to `from_json`:
```rust
            "createEvent" => Some(OutboxOp::CreateEvent { id: id()? }),
            "updateEvent" => Some(OutboxOp::UpdateEvent { id: id()? }),
            "deleteEvent" => Some(OutboxOp::DeleteEvent { id: id()? }),
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore event_mutation_ops_round_trip` then `bash "$LCARGO" test -p mailcore`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/store/mod.rs
git commit -m "mailcore: OutboxOp Create/Update/DeleteEvent + json round-trip"
```

---

### Task 4: Store — local event create / update / delete / read-for-send / reconcile

**Files:**
- Modify: `mailcore/src/store/mod.rs` (methods + `LocalEventFields`, `EventSendData`; `reconcile_event_id`; tests)

**Interfaces:**
- Consumes: `upsert_event`, `put_event_attendees`, `NewEvent`, `NewAttendee`, `EventRow`, `event_attendees`, `event_body` (existing).
- Produces:
  - `pub struct LocalEventFields { pub subject, start_utc, end_utc, is_all_day, location, body_html: String/bool; pub attendees: Vec<(String, String)> }` (name,address pairs)
  - `pub fn create_local_event(&self, f: &LocalEventFields, organizer_name: &str, organizer_addr: &str) -> Result<String, StoreError>`
  - `pub fn update_event_fields(&self, id: &str, f: &LocalEventFields) -> Result<(), StoreError>`
  - `pub fn delete_event(&self, id: &str) -> Result<(), StoreError>`
  - `pub struct EventSendData { pub subject, start_utc, end_utc, is_all_day, location, body_html; pub attendees: Vec<(String, String)> }`
  - `pub fn event_for_send(&self, id: &str) -> Result<Option<EventSendData>, StoreError>`
  - `pub fn reconcile_event_id(&self, local_id: &str, graph_id: &str) -> Result<(), StoreError>`

- [ ] **Step 1: Write the failing tests**

Add to `mailcore/src/store/mod.rs` tests:

```rust
    fn sample_fields() -> LocalEventFields {
        LocalEventFields {
            subject: "Sync".into(),
            start_utc: "2026-07-20T11:00:00Z".into(),
            end_utc: "2026-07-20T12:00:00Z".into(),
            is_all_day: false,
            location: "Room 1".into(),
            body_html: "<p>agenda</p>".into(),
            attendees: vec![("Bob".into(), "bob@x.com".into())],
        }
    }

    #[test]
    fn create_local_event_is_visible_and_readable_for_send() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_event(&sample_fields(), "Me", "me@x").unwrap();
        assert!(id.starts_with("local:"));
        // shows in the window
        let rows = s.events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z").unwrap();
        assert!(rows.iter().any(|e| e.id == id && e.subject == "Sync"));
        // read back for the outbox
        let send = s.event_for_send(&id).unwrap().unwrap();
        assert_eq!(send.subject, "Sync");
        assert_eq!(send.attendees, vec![("Bob".to_string(), "bob@x.com".to_string())]);
        assert_eq!(send.body_html, "<p>agenda</p>");
    }

    #[test]
    fn reconcile_event_id_repoints_event_and_attendees() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_event(&sample_fields(), "Me", "me@x").unwrap();
        s.reconcile_event_id(&id, "EV1").unwrap();
        assert!(s.event_for_send(&id).unwrap().is_none());          // old id gone
        let send = s.event_for_send("EV1").unwrap().unwrap();        // under graph id
        assert_eq!(send.attendees.len(), 1);                        // attendees moved too
    }

    #[test]
    fn delete_event_removes_it() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_event(&sample_fields(), "Me", "me@x").unwrap();
        s.delete_event(&id).unwrap();
        assert!(s.event_for_send(&id).unwrap().is_none());
        assert!(s.events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z").unwrap().is_empty());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore create_local_event reconcile_event_id delete_event_removes`
Expected: FAIL — types/methods missing.

- [ ] **Step 3: Implement**

Add the structs (near `NewEvent`):

```rust
/// The editable fields of an event the compose form collects — the input to
/// `create_local_event`/`update_event_fields`.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalEventFields {
    pub subject: String,
    pub start_utc: String,
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub body_html: String,
    pub attendees: Vec<(String, String)>, // (name, address)
}

/// Everything `sync::outbox` needs to build a `graph::client::EventInput` for a
/// stored event (`event_for_send`).
#[derive(Debug, Clone, PartialEq)]
pub struct EventSendData {
    pub subject: String,
    pub start_utc: String,
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub body_html: String,
    pub attendees: Vec<(String, String)>,
}
```

Add to `impl Store` (there is a `local_draft_id()` helper that mints `local:<hex>` ids — add a sibling `local_event_id()` or reuse `local_draft_id()` since it's just a unique `local:` id; the plan uses `local_draft_id()`):

```rust
    /// Inserts a locally-created event with a fresh `local:` id and the given
    /// organizer, so it shows in the agenda immediately (before it syncs to
    /// Graph). `response_status` is `"organizer"`.
    pub fn create_local_event(&self, f: &LocalEventFields, organizer_name: &str, organizer_addr: &str) -> Result<String, StoreError> {
        let id = local_draft_id(); // a unique "local:<hex>" id
        self.upsert_event(&NewEvent {
            id: id.clone(),
            subject: f.subject.clone(),
            start_utc: f.start_utc.clone(),
            end_utc: f.end_utc.clone(),
            is_all_day: f.is_all_day,
            location: f.location.clone(),
            organizer_name: organizer_name.to_string(),
            organizer_addr: organizer_addr.to_string(),
            response_status: "organizer".to_string(),
            series_master_id: None,
            body_preview: String::new(),
            web_link: String::new(),
            last_modified: String::new(),
            body_html: f.body_html.clone(),
        })?;
        self.put_event_attendees(&id, &to_new_attendees(&f.attendees))?;
        Ok(id)
    }

    /// Overwrites a stored event's editable fields + attendees + body in place.
    pub fn update_event_fields(&self, id: &str, f: &LocalEventFields) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE events SET subject = ?2, start_utc = ?3, end_utc = ?4, is_all_day = ?5,
                    location = ?6, body_html = ?7 WHERE id = ?1",
            params![id, f.subject, f.start_utc, f.end_utc, f.is_all_day, f.location, f.body_html],
        )?;
        self.put_event_attendees(id, &to_new_attendees(&f.attendees))?;
        Ok(())
    }

    /// Removes an event (and its attendees) locally.
    pub fn delete_event(&self, id: &str) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM event_attendees WHERE event_id = ?1", params![id])?;
        tx.execute("DELETE FROM events WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// Reads a stored event's send-relevant fields (`None` if no such event).
    pub fn event_for_send(&self, id: &str) -> Result<Option<EventSendData>, StoreError> {
        let row = self.conn.query_row(
            "SELECT subject, start_utc, end_utc, is_all_day, location, body_html FROM events WHERE id = ?1",
            params![id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?)),
        );
        let (subject, start_utc, end_utc, is_all_day, location, body_html) = match row {
            Ok(t) => t,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let attendees = self
            .event_attendees(id)?
            .into_iter()
            .map(|a| (a.name, a.addr))
            .collect();
        Ok(Some(EventSendData { subject, start_utc, end_utc, is_all_day, location, body_html, attendees }))
    }

    /// Re-points a `local:` event id to its Graph id after `create_event`
    /// (mirrors `reconcile_id` for drafts): updates `events.id` and
    /// `event_attendees.event_id` in one transaction.
    pub fn reconcile_event_id(&self, local_id: &str, graph_id: &str) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        tx.execute("UPDATE events SET id = ?2 WHERE id = ?1", params![local_id, graph_id])?;
        tx.execute("UPDATE event_attendees SET event_id = ?2 WHERE event_id = ?1", params![local_id, graph_id])?;
        tx.commit()?;
        Ok(())
    }
```

Add the helper (module scope):
```rust
fn to_new_attendees(pairs: &[(String, String)]) -> Vec<NewAttendee> {
    pairs
        .iter()
        .map(|(name, addr)| NewAttendee {
            name: name.clone(),
            addr: addr.clone(),
            r#type: "required".to_string(),
            response: "none".to_string(),
        })
        .collect()
}
```

Note: confirm `local_draft_id()` produces a `local:`-prefixed unique id (it does — used by `create_local_draft`), `event_attendees(id)` returns rows with `name`/`addr` fields, and `NewAttendee` field names (`name`, `addr`, `r#type`, `response`) match. Adjust `event_for_send`'s attendee mapping to the actual `event_attendees` return type if it differs.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore create_local_event reconcile_event_id delete_event_removes` then `bash "$LCARGO" test -p mailcore`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/store/mod.rs
git commit -m "mailcore: local event create/update/delete + event_for_send + reconcile_event_id"
```

---

### Task 5: Sync — commands, engine handlers, apply_op arms

**Files:**
- Modify: `mailcore/src/sync/engine.rs` (`SyncCommand` variants + `handle_command` arms)
- Modify: `mailcore/src/sync/outbox.rs` (`apply_op` arms)

**Interfaces:**
- Consumes: `event_for_send`/`reconcile_event_id` (Task 4), `create_event`/`update_event`/`delete_event` + `EventInput` (Task 2), the `OutboxOp`s (Task 3).
- Produces: `SyncCommand::{CreateEvent, UpdateEvent, DeleteEvent} { id }`.

- [ ] **Step 1: Write the failing test**

Add to `mailcore/src/sync/outbox.rs` tests (mirror `apply_op_save_draft_creates_and_reconciles_a_local_draft`):

```rust
    #[test]
    fn apply_op_create_event_posts_and_reconciles() {
        // ... seed store: create_local_event(sample fields, "Me","me@x") → local id ...
        // ... FakeServer route: POST /me/events → returns {"id":"EV1", ...full event json...} ...
        // apply_op(&client, &store, &OutboxOp::CreateEvent { id: local_id.clone() }).unwrap();
        // reconciled: the event now lives under "EV1"
        assert!(store.event_for_send("EV1").unwrap().is_some());
        assert!(store.event_for_send(&local_id).unwrap().is_none());
    }

    #[test]
    fn apply_op_delete_event_of_a_local_id_makes_no_graph_call() {
        // A local:-only event id that never synced: DeleteEvent must NOT hit Graph.
        // ... FakeServer with NO routes (any request → panic/unmatched) ...
        // apply_op(&client, &store, &OutboxOp::DeleteEvent { id: "local:never".into() }).unwrap();
        // (no request made; op succeeds)
    }
```

Adapt the seeding/route wiring to the existing send-draft apply_op test; keep the assertions.

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p mailcore apply_op_create_event apply_op_delete_event_of_a_local`
Expected: FAIL — apply_op has no such arms.

- [ ] **Step 3: Implement**

In `mailcore/src/sync/outbox.rs`, add arms to `apply_op`'s match:

```rust
        OutboxOp::CreateEvent { id } => {
            let input = event_input_for(store, id)?;
            let created = client.create_event(&input)?;
            store
                .reconcile_event_id(id, &created.id)
                .map_err(|e| GraphError::Parse(e.to_string()))?;
            Ok(())
        }
        OutboxOp::UpdateEvent { id } => {
            let input = event_input_for(store, id)?;
            client.update_event(id, &input)
        }
        OutboxOp::DeleteEvent { id } => {
            // A local:-only event never reached Graph; nothing to delete there.
            if id.starts_with("local:") {
                Ok(())
            } else {
                client.delete_event(id)
            }
        }
```

Add the helper (module scope in `outbox.rs`), converting the store's send data into a graph `EventInput`:

```rust
/// Reads the stored event `id` and builds the `EventInput` the create/update
/// calls take. Errors if the event isn't in the store (a corrupt/raced op).
fn event_input_for(store: &Store, id: &str) -> Result<crate::graph::client::EventInput, GraphError> {
    let d = store
        .event_for_send(id)
        .map_err(|e| GraphError::Parse(e.to_string()))?
        .ok_or_else(|| GraphError::Parse(format!("no local event stored for {id}")))?;
    Ok(crate::graph::client::EventInput {
        subject: d.subject,
        start_utc: d.start_utc,
        end_utc: d.end_utc,
        is_all_day: d.is_all_day,
        location: d.location,
        attendees: d.attendees,
        body_html: d.body_html,
    })
}
```

In `mailcore/src/sync/engine.rs`, add the `SyncCommand` variants (near `RespondEvent`):
```rust
    CreateEvent { id: String },
    UpdateEvent { id: String },
    DeleteEvent { id: String },
```
and the `handle_command` arms (the optimistic local write already happened in the UI; the engine enqueues + drains, then repaints):
```rust
            SyncCommand::CreateEvent { id } => {
                self.enqueue_and_drain(OutboxOp::CreateEvent { id });
                self.emit(SyncEvent::CalendarUpdated);
            }
            SyncCommand::UpdateEvent { id } => {
                self.enqueue_and_drain(OutboxOp::UpdateEvent { id });
                self.emit(SyncEvent::CalendarUpdated);
            }
            SyncCommand::DeleteEvent { id } => {
                self.enqueue_and_drain(OutboxOp::DeleteEvent { id });
                self.emit(SyncEvent::CalendarUpdated);
            }
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore apply_op_create_event apply_op_delete_event_of_a_local` then `bash "$LCARGO" test -p mailcore`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/sync/engine.rs mailcore/src/sync/outbox.rs
git commit -m "mailcore: sync commands + apply_op for create/update/delete event"
```

---

### Task 6: Event form — state, open new/edit, draw

**Files:**
- Create: `lookxy/src/ui/eventform.rs` (`EventForm`, `EventField`, `draw`)
- Modify: `lookxy/src/ui/mod.rs` (`pub mod eventform;`, draw call), `lookxy/src/app.rs` (`App.event_form` field; `open_new_event`/`open_edit_event`)

**Interfaces:**
- Consumes: `datetime::LocalDateTime` (Task 1); `App.signature`? no — events use no signature; `store.event_for_send` / `event_attendees` (Task 4); the calendar `local_offset_minutes` + `civil_from_days`.
- Produces: `App.event_form: Option<EventForm>`; `EventForm { editing_id: Option<String>, title, start, end, location, attendees, body: String, all_day: bool, focus: EventField, autocomplete: Option<crate::ui::compose::Autocomplete> }`; `EventField { Title, Start, End, AllDay, Location, Attendees, Body }`.

- [ ] **Step 1: Write the failing tests**

Add to `lookxy/src/app.rs` tests:

```rust
    #[test]
    fn open_new_event_starts_a_blank_form_with_prefilled_times() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        let f = app.event_form.as_ref().unwrap();
        assert!(f.editing_id.is_none());
        assert!(f.title.is_empty());
        assert!(!f.start.is_empty() && !f.end.is_empty()); // prefilled (next hour / +1h)
    }

    #[test]
    fn open_edit_event_prefills_from_the_selected_event() {
        let mut app = App::for_test_with_seeded_store();
        // ... seed a NON-recurring event "e1" (subject "Standup", start/end) and select it ...
        app.open_edit_event();
        let f = app.event_form.as_ref().unwrap();
        assert_eq!(f.editing_id.as_deref(), Some("e1"));
        assert_eq!(f.title, "Standup");
    }

    #[test]
    fn open_edit_event_refuses_a_recurring_event() {
        let mut app = App::for_test_with_seeded_store();
        // ... seed a RECURRING event (series_master_id = Some(...)) and select it ...
        app.open_edit_event();
        assert!(app.event_form.is_none()); // refused
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy open_new_event open_edit_event`
Expected: FAIL — types/methods missing.

- [ ] **Step 3: Implement**

Write `lookxy/src/ui/eventform.rs` with these exact types (the field set later tasks reference):

```rust
/// Which field the event form's keyboard focus is on. Tab cycles
/// Title → Start → End → AllDay → Location → Attendees → Body → Title.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EventField { Title, Start, End, AllDay, Location, Attendees, Body }

/// The open create/edit event form. `editing_id` is `Some(event id)` when
/// editing an existing event, `None` for a new one. `start`/`end` hold the
/// raw local-time text the datetime parser consumes; `attendees` is the flat
/// `Name <addr>; …` text (with contacts autocomplete). `error` is the inline
/// validation message shown in the footer.
pub struct EventForm {
    pub editing_id: Option<String>,
    pub title: String,
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub location: String,
    pub attendees: String,
    pub body: String,
    pub focus: EventField,
    pub autocomplete: Option<crate::ui::compose::Autocomplete>,
    pub error: Option<String>,
}
```

and a `draw` that renders Title / Start / End / [ ] all-day / Location / Attendees / Body rows (plus `error` in a footer) (mirror `ui::compose::draw` for the field-row rendering + focus highlight; reuse `draw_field` if it's reachable, else a local copy), plus the attendee autocomplete dropdown when open (Task 8 fills the dropdown; render an empty overlay-safe placeholder now). Add `pub mod eventform;` to `ui/mod.rs` and `eventform::draw(f, app)` in `ui::draw` (Calendar mode, after the calendar draw).

In `app.rs`:
- Add `pub event_form: Option<crate::ui::eventform::EventForm>` to `App` (init `None`).
- `open_new_event`: compute `now` local (from the system clock via a helper + `local_offset_minutes`), prefill `start` = next full hour formatted `YYYY-MM-DD HH:00`, `end` = +1h, and open a blank `EventForm { editing_id: None, .. }`.
- `open_edit_event`: from `self.selected_event` (or the highlighted agenda row), read the event; if `series_master_id.is_some()` set a status notice (`error_notice`/`attachment_notice`) and return without opening; else read `event_for_send` + `event_attendees`, convert `start_utc`/`end_utc` to local display strings (`YYYY-MM-DD HH:MM`), join attendees as `Name <addr>; …`, and open `EventForm { editing_id: Some(id), title, start, end, all_day, location, attendees, body, .. }`.

The local "now"/offset + UTC→local formatting reuse `crate::ui::calendar::{local_offset_minutes, civil_from_days}`; add a small `fn local_now() -> datetime::LocalDateTime` (system clock → local) in `app.rs` or `datetime.rs`. (System time via `std::time::SystemTime::now()` is available in the app — only the workflow sandbox forbids it.)

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy open_new_event open_edit_event` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/eventform.rs lookxy/src/ui/mod.rs lookxy/src/app.rs
git commit -m "lookxy: event form state + open new/edit (prefill, recurring refused) + draw"
```

---

### Task 7: Event form — key handling + save

**Files:**
- Modify: `lookxy/src/ui/eventform.rs` (`handle_key`), `lookxy/src/ui/mod.rs` (route to it; bind `c`/`e` in Calendar mode), `lookxy/src/app.rs` (`save_event_form`)

**Interfaces:**
- Consumes: `datetime::parse_start`/`parse_end` (Task 1); `store.create_local_event`/`update_event_fields` (Task 4); `SyncCommand::CreateEvent`/`UpdateEvent` (Task 5); the recipient-token helpers reused in Task 8.

- [ ] **Step 1: Write the failing test**

Add to `lookxy/src/app.rs` tests (the test harness captures `SyncCommand`s via `test_cmd_rx`):

```rust
    #[test]
    fn saving_a_new_event_form_creates_and_enqueues_create_event() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() {
            f.title = "Planning".into();
            f.start = "2026-07-20 14:00".into();
            f.end = "2026-07-20 15:00".into();
        }
        app.save_event_form();
        assert!(app.event_form.is_none()); // form closed on success
        // a CreateEvent was enqueued and a local event exists
        let cmd = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(cmd, Ok(SyncCommand::CreateEvent { .. })));
    }

    #[test]
    fn saving_with_an_invalid_time_keeps_the_form_open() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() { f.title = "X".into(); f.start = "nonsense".into(); }
        app.save_event_form();
        assert!(app.event_form.is_some()); // still open — invalid start
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy saving_a_new_event_form saving_with_an_invalid_time`
Expected: FAIL — `no method named save_event_form`.

- [ ] **Step 3: Implement**

In `app.rs`, add `save_event_form`:
```rust
    /// Ctrl-Enter in the event form: parse + validate the times, build the
    /// fields, and either create a new local event (+ CreateEvent) or update
    /// the edited one (+ UpdateEvent). A parse/validation failure sets an inline
    /// error on the form and leaves it open.
    pub fn save_event_form(&mut self) {
        let Some(form) = self.event_form.as_ref() else { return; };
        let now = local_now();
        let off = crate::ui::calendar::local_offset_minutes();
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
        let fields = mailcore::store::LocalEventFields {
            subject: form.title.clone(),
            start_utc,
            end_utc,
            is_all_day: form.all_day,
            location: form.location.clone(),
            body_html: mailcore::compose_html::escape_html(&form.body), // plain text as HTML text
            attendees: parse_attendee_pairs(&form.attendees),
        };
        let editing = form.editing_id.clone();
        match editing {
            Some(id) => {
                let _ = self.store.update_event_fields(&id, &fields);
                let _ = self.sync.cmd_tx.send(SyncCommand::UpdateEvent { id });
            }
            None => {
                let account = self.account.clone().unwrap_or_default();
                if let Ok(id) = self.store.create_local_event(&fields, &account, &account) {
                    let _ = self.sync.cmd_tx.send(SyncCommand::CreateEvent { id });
                }
            }
        }
        self.event_form = None;
        self.reload_agenda();
    }
```
Add a `set_form_error(&mut self, msg: &str)` that stores the message on the form (a `pub error: Option<String>` field on `EventForm`, drawn in the form footer), a `parse_attendee_pairs(field: &str) -> Vec<(String, String)>` that reuses the recipient parsing (`Name <addr>; …` → pairs — mirror `mailcore::sync::outbox::parse_recipients` shape, or reuse `compose`'s current parsing), and `local_now()`.

In `eventform.rs` `handle_key(app, key)`: Tab cycles focus (Title→Start→End→AllDay→Location→Attendees→Body→Title); Char/Backspace edit the focused text field (AllDay toggles on Space); `Ctrl-Enter` → `app.save_event_form()`; `Esc` → `app.event_form = None`. (Attendees autocomplete keys are added in Task 8.) In `ui/mod.rs handle_key`: route to `eventform::handle_key` when `app.event_form.is_some()` (before the calendar handler); and in the Calendar-mode key branch bind `c` → `app.open_new_event()`, `e` → `app.open_edit_event()`. Confirm `c`/`e` are free in Calendar mode (current calendar keys: `a`/`d`/`t` RSVP, Enter, `g`/Esc, `j`/`k`).

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy saving_a_new_event_form saving_with_an_invalid_time` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/eventform.rs lookxy/src/ui/mod.rs lookxy/src/app.rs
git commit -m "lookxy: event form key handling + save (create/update, validate)"
```

---

### Task 8: Event form — attendee autocomplete

**Files:**
- Modify: `lookxy/src/ui/eventform.rs` (attendee dropdown keys + refresh), `lookxy/src/ui/compose.rs` (make the reused helpers reachable)

**Interfaces:**
- Consumes: `Store::search_contacts` (v4), `compose::{current_token, apply_completion, Autocomplete}` (make `pub(crate)` if not already).

- [ ] **Step 1: Write the failing test**

Add to `lookxy/src/app.rs` tests (or `eventform.rs`):

```rust
    #[test]
    fn attendee_field_autocompletes_from_contacts() {
        let mut app = App::for_test_with_seeded_store();
        app.store.upsert_contact(&mailcore::store::Contact {
            name: "Alice".into(), address: "alice@x.com".into(), source: "local".into(),
            last_seen: "".into(), frequency: 3, relevance: None,
        }).unwrap();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        // focus Attendees, type "al" → dropdown opens with Alice; accept → field gets "Alice <alice@x.com>; "
        // drive through eventform::handle_key the way the compose autocomplete integration test does.
        // assert the accepted field text and that the dropdown closed.
    }
```

Model this on the compose autocomplete integration test (`autocomplete_opens_accepts_and_esc_routes_through_handle_key`); keep the assertions concrete (dropdown opens, Enter accepts + rewrites the Attendees field, Esc closes).

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy attendee_field_autocompletes`
Expected: FAIL — no autocomplete on the attendees field.

- [ ] **Step 3: Implement**

In `compose.rs`, ensure `current_token`, `apply_completion`, and `Autocomplete` (with `move_selection`) are `pub(crate)` (they were `pub(crate)` for the compose module's own tests; confirm and widen if needed).

In `eventform.rs` `handle_key`, mirror the compose autocomplete flow but scoped to the Attendees field: when `focus == Attendees`, a Char/Backspace recomputes `current_token(&form.attendees)` and, via `app.store.search_contacts(token, 8)` (borrow-split: search into an owned Vec first, then set `form.autocomplete`), opens/updates the dropdown; when the dropdown is open, Down/Up move, Enter/Tab accept (`form.attendees = apply_completion(&form.attendees, &contact)` + close), Esc closes (handle Esc at the top of `handle_key` like compose does, so it closes the dropdown before it would cancel the form). Render the dropdown below the Attendees row in `draw`.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy attendee_field_autocompletes` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/eventform.rs lookxy/src/ui/compose.rs
git commit -m "lookxy: attendee autocomplete in the event form (reuses contacts)"
```

---

### Task 9: Delete event + confirm

**Files:**
- Modify: `lookxy/src/ui/calendar.rs` (bind `x`), `lookxy/src/app.rs` (`delete_selected_event`; confirm-modal action)

**Interfaces:**
- Consumes: the existing confirm-modal (`ConfirmModal`/`ConfirmAction` from the thread-view feature), `store.delete_event` (Task 4), `SyncCommand::DeleteEvent` (Task 5).

- [ ] **Step 1: Write the failing test**

Add to `lookxy/src/app.rs` tests:

```rust
    #[test]
    fn deleting_an_event_confirms_then_removes_and_enqueues() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        // ... seed + select a NON-recurring event "e1" ...
        app.delete_selected_event();
        assert!(app.confirm.is_some());              // confirm modal opened, nothing deleted yet
        app.confirm_yes();                           // execute
        assert!(app.confirm.is_none());
        assert!(app.store.event_for_send("e1").unwrap().is_none()); // removed locally
        let cmd = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(cmd, Ok(SyncCommand::DeleteEvent { .. })));
    }

    #[test]
    fn deleting_a_recurring_event_is_refused() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        // ... seed + select a RECURRING event ...
        app.delete_selected_event();
        assert!(app.confirm.is_none()); // refused, no modal
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `bash "$LCARGO" test -p lookxy deleting_an_event deleting_a_recurring`
Expected: FAIL — `no method named delete_selected_event`.

- [ ] **Step 3: Implement**

In `app.rs`:
- Add a `ConfirmAction::DeleteEvent(String)` variant to the existing confirm enum (from the thread-view feature).
- `delete_selected_event`: resolve the selected event; if recurring (`series_master_id.is_some()`) set a notice and return; else open `self.confirm = Some(ConfirmModal { prompt: format!("Delete event '{title}'?"), action: ConfirmAction::DeleteEvent(id) })`.
- In `confirm_yes`'s match, add the `DeleteEvent(id)` arm: `let _ = self.store.delete_event(&id); let _ = self.sync.cmd_tx.send(SyncCommand::DeleteEvent { id }); self.reload_agenda();`.

In `calendar.rs handle_key` (Calendar mode), bind `KeyCode::Char('x') => app.delete_selected_event()`. Confirm `x` is free (calendar keys: a/d/t/Enter/g/j/k/c/e).

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy deleting_an_event deleting_a_recurring` then `bash "$LCARGO" test -p lookxy`
Expected: PASS; full suite green.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/calendar.rs lookxy/src/app.rs
git commit -m "lookxy: delete event (x) with confirm modal"
```

---

## Notes for the implementer

- **Match existing signatures/helpers.** Many tasks say "adapt to the existing X" (FakeServer/Route/captured-body, `GraphClient::new`, the send-draft apply_op test, `event_attendees`/`NewAttendee` fields, the compose autocomplete integration test, the `ConfirmModal`/`ConfirmAction` enum). Read the neighbouring code and match it exactly; keep the assertions.
- **Existing calendar read + RSVP and everything else stays green.** The new form/keys are additive; `c`/`e`/`x` must not shadow the existing calendar keys (`a`/`d`/`t`/Enter/`g`/`j`/`k`), and the event form is only reachable in Calendar mode.
- **Optimistic-local + outbox is load-bearing.** Save writes the local store then enqueues; the engine reconciles a `local:` create to its Graph id; a `local:`-only delete makes no Graph call. On a Graph failure the op quarantines and the optimistic state reconverges on the next `RefreshCalendar`, exactly as `RespondEvent` does.
- **Borrows in `app.rs`:** `self.event_form`, `self.store`, `self.sync` are disjoint fields — read/clone what you need (e.g. the form fields, `draft_id`-style ids) before a store/sync call, mirroring the compose autocomplete's borrow structure.
- **Deferred (noted for the final review):** all-day End normalization (Task 2/7 send whatever the form produced; a dedicated all-day End=Start+1day default is a refinement); the form does not yet surface Graph-side attendee response status.
