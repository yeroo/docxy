# lookxy Automatic Replies (Out-of-Office) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** View and set the mailbox's automatic-replies (out-of-office) configuration from lookxy — status, scheduled window, external audience, and separate internal/external reply messages — via Graph `/me/mailboxSettings`.

**Architecture:** A new `AutomaticReplies` model (parsed from / serialized to `automaticRepliesSetting`), two Graph client calls (`get_`/`set_automatic_replies`), two direct-call sync commands (fetch/set — no outbox, like `respond_meeting`), and an `OofForm` modal overlay (opened by `O`, modeled on `ui/eventform.rs`) that fetches on open, prefills, validates the schedule, and writes through.

**Tech Stack:** Rust (edition 2024), hand-rolled `mailcore::json`, `ureq` Graph client, `ratatui`/`crossterm` TUI, `std::sync::mpsc` engine channels.

## Global Constraints

- **Build wrapper:** never call bare `cargo` (fails os error 448). Use `bash "$LCARGO" <args>` where `LCARGO=C:/Users/BORIS_~1/AppData/Local/Temp/claude/C--Users-boris-kudriashov-Source-docxy/1da9a016-b606-4432-8951-6d73bb91c967/scratchpad/lcargo.sh`. EVERY Bash tool call that runs it MUST set `dangerouslyDisableSandbox: true`.
- **MSRV / edition:** edition 2024, MSRV 1.88. No new external crates.
- **No serde:** JSON only via `mailcore::json` (`Value`, `parse`, `.get`/`.as_str`/`.as_bool`/`Object`/`Str`/`Bool`, `Value::to_string`).
- **Entry key:** global uppercase **`O`** (free in `on_key_char`, both Mail and Calendar modes).
- **Direct Graph calls, no outbox** — fetch/set mirror `respond_meeting`/`fetch_body` (signed-in guard, `with_auth`, `react` on error).
- **Wire vocab (exact):** status `"disabled"/"alwaysEnabled"/"scheduled"`; externalAudience `"none"/"contactsOnly"/"all"`. Scheduled datetimes are `{"dateTime": <UTC>, "timeZone": "UTC"}` and are **included only when status = Scheduled**.
- **Plain-text messages:** edited as plain text; `html_to_plain` on read, `plain_to_html` (escape `& < > "`, `\n`→`<br>`) on write.
- **Secrets:** never log tokens/bodies; error strings carry no secret.

---

### Task 1: `AutomaticReplies` model + HTML text conversion (`mailcore`)

**Files:**
- Modify: `mailcore/src/graph/model.rs` (new enums, struct, `from_json`, `html_to_plain`, `plain_to_html`; tests)

**Interfaces:**
- Consumes: `crate::json::Value`, the existing module-private `datetime_field_to_utc(&Value) -> String`, `str_field`.
- Produces: `pub enum OofStatus { Disabled, AlwaysEnabled, Scheduled }` (+ `from_wire(&str)->Self`, `as_wire(&self)->&'static str`); `pub enum ExternalAudience { None, ContactsOnly, All }` (+ same); `pub struct AutomaticReplies { status: OofStatus, external_audience: ExternalAudience, internal_message: String, external_message: String, scheduled_start_utc: String, scheduled_end_utc: String }` (+ `from_json(&Value)->Option<Self>`); `pub fn html_to_plain(&str)->String`; `pub fn plain_to_html(&str)->String`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `mailcore/src/graph/model.rs`:

```rust
    #[test]
    fn oof_status_and_audience_wire_round_trip() {
        for s in [OofStatus::Disabled, OofStatus::AlwaysEnabled, OofStatus::Scheduled] {
            assert_eq!(OofStatus::from_wire(s.as_wire()), s);
        }
        assert_eq!(OofStatus::from_wire("bogus"), OofStatus::Disabled);
        for a in [ExternalAudience::None, ExternalAudience::ContactsOnly, ExternalAudience::All] {
            assert_eq!(ExternalAudience::from_wire(a.as_wire()), a);
        }
        assert_eq!(ExternalAudience::from_wire("bogus"), ExternalAudience::All);
    }

    #[test]
    fn automatic_replies_parses_scheduled_setting() {
        let v = parse(
            r#"{"automaticRepliesSetting":{
                "status":"scheduled","externalAudience":"contactsOnly",
                "internalReplyMessage":"<p>Away &amp; back <b>Monday</b></p>",
                "externalReplyMessage":"Out<br>of office",
                "scheduledStartDateTime":{"dateTime":"2026-07-20T09:00:00.0000000","timeZone":"UTC"},
                "scheduledEndDateTime":{"dateTime":"2026-07-27T17:00:00.0000000","timeZone":"UTC"}
            }}"#,
        )
        .unwrap();
        let r = AutomaticReplies::from_json(&v).unwrap();
        assert_eq!(r.status, OofStatus::Scheduled);
        assert_eq!(r.external_audience, ExternalAudience::ContactsOnly);
        assert_eq!(r.internal_message, "Away & back Monday");
        assert_eq!(r.external_message, "Out\nof office");
        assert_eq!(r.scheduled_start_utc, "2026-07-20T09:00:00Z");
        assert_eq!(r.scheduled_end_utc, "2026-07-27T17:00:00Z");
    }

    #[test]
    fn automatic_replies_disabled_drops_schedule_even_when_wire_has_defaults() {
        let v = parse(
            r#"{"automaticRepliesSetting":{
                "status":"disabled","externalAudience":"all",
                "internalReplyMessage":"","externalReplyMessage":"",
                "scheduledStartDateTime":{"dateTime":"0001-01-01T00:00:00.0000000","timeZone":"UTC"},
                "scheduledEndDateTime":{"dateTime":"0001-01-01T00:00:00.0000000","timeZone":"UTC"}
            }}"#,
        )
        .unwrap();
        let r = AutomaticReplies::from_json(&v).unwrap();
        assert_eq!(r.status, OofStatus::Disabled);
        assert_eq!(r.scheduled_start_utc, ""); // dropped: only kept when Scheduled
        assert_eq!(r.scheduled_end_utc, "");
    }

    #[test]
    fn html_to_plain_strips_tags_decodes_entities_and_breaks() {
        assert_eq!(
            html_to_plain("<div>Hi &amp; bye</div><p>line1</p>line2<br>line3"),
            "Hi & bye\nline1\nline2\nline3"
        );
        assert_eq!(html_to_plain("a<br/><br/><br/><br/>b"), "a\n\nb"); // 3+ newlines collapse to 2
        assert_eq!(html_to_plain("&lt;tag&gt; &quot;q&quot; &#39;s&#39; &nbsp;x"), "<tag> \"q\" 's' \u{a0}x".replace('\u{a0}', " "));
    }

    #[test]
    fn plain_to_html_escapes_and_encodes_newlines() {
        assert_eq!(
            plain_to_html("a & b < c > d \"e\"\nf"),
            "a &amp; b &lt; c &gt; d &quot;e&quot;<br>f"
        );
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `bash "$LCARGO" test -p mailcore automatic_replies oof_status html_to_plain plain_to_html 2>&1 | tail` — NOTE cargo takes ONE filter; run instead: `bash "$LCARGO" test -p mailcore automatic_ 2>&1 | tail` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — types/functions don't exist.

- [ ] **Step 3: Add the enums**

In `mailcore/src/graph/model.rs`, add (place near the other model types, e.g. after `AttachmentMeta`):

```rust
/// Graph `automaticRepliesSetting.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OofStatus {
    Disabled,
    AlwaysEnabled,
    Scheduled,
}

impl OofStatus {
    pub fn as_wire(&self) -> &'static str {
        match self {
            OofStatus::Disabled => "disabled",
            OofStatus::AlwaysEnabled => "alwaysEnabled",
            OofStatus::Scheduled => "scheduled",
        }
    }
    /// Inverse of `as_wire`; an unrecognized value reads back as `Disabled`
    /// (the safe "auto-replies are off" default).
    pub fn from_wire(s: &str) -> OofStatus {
        match s {
            "alwaysEnabled" => OofStatus::AlwaysEnabled,
            "scheduled" => OofStatus::Scheduled,
            _ => OofStatus::Disabled,
        }
    }
}

/// Graph `automaticRepliesSetting.externalAudience`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalAudience {
    None,
    ContactsOnly,
    All,
}

impl ExternalAudience {
    pub fn as_wire(&self) -> &'static str {
        match self {
            ExternalAudience::None => "none",
            ExternalAudience::ContactsOnly => "contactsOnly",
            ExternalAudience::All => "all",
        }
    }
    /// Inverse of `as_wire`; unrecognized reads back as `All` (Graph's own
    /// default external audience).
    pub fn from_wire(s: &str) -> ExternalAudience {
        match s {
            "none" => ExternalAudience::None,
            "contactsOnly" => ExternalAudience::ContactsOnly,
            _ => ExternalAudience::All,
        }
    }
}
```

- [ ] **Step 4: Add the struct + `from_json`**

```rust
/// The mailbox's automatic-replies (out-of-office) configuration, parsed from
/// Graph's `mailboxSettings.automaticRepliesSetting`. Reply messages are held
/// as plain text (`html_to_plain` on read; `plain_to_html` on write — see
/// `graph::client::set_automatic_replies`). `scheduled_*_utc` are canonical
/// UTC only when `status == Scheduled`, else `""`.
#[derive(Debug, Clone, PartialEq)]
pub struct AutomaticReplies {
    pub status: OofStatus,
    pub external_audience: ExternalAudience,
    pub internal_message: String,
    pub external_message: String,
    pub scheduled_start_utc: String,
    pub scheduled_end_utc: String,
}

impl AutomaticReplies {
    pub fn from_json(v: &Value) -> Option<Self> {
        let s = v.get("automaticRepliesSetting")?;
        let status = OofStatus::from_wire(&str_field(s, "status"));
        // Graph always echoes the scheduled datetimes (with `0001-01-01`
        // defaults when off); only keep them when actually Scheduled so the
        // form doesn't prefill a garbage window for a disabled mailbox.
        let (start, end) = if status == OofStatus::Scheduled {
            (
                s.get("scheduledStartDateTime")
                    .map(datetime_field_to_utc)
                    .unwrap_or_default(),
                s.get("scheduledEndDateTime")
                    .map(datetime_field_to_utc)
                    .unwrap_or_default(),
            )
        } else {
            (String::new(), String::new())
        };
        Some(AutomaticReplies {
            status,
            external_audience: ExternalAudience::from_wire(&str_field(s, "externalAudience")),
            internal_message: html_to_plain(&str_field(s, "internalReplyMessage")),
            external_message: html_to_plain(&str_field(s, "externalReplyMessage")),
            scheduled_start_utc: start,
            scheduled_end_utc: end,
        })
    }
}
```

- [ ] **Step 5: Add `html_to_plain` and `plain_to_html`**

```rust
/// Best-effort conversion of an OOF HTML reply message to plain text: `<br>`,
/// `<p>`/`</p>`, and `<div>`/`</div>` become newlines; every other tag is
/// dropped; the common entities are decoded; runs of 3+ newlines collapse to
/// 2; both ends are trimmed. Rich formatting (tables, styling) is flattened to
/// its text content — see the design's fidelity note.
pub fn html_to_plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            // Unclosed '<': keep the rest verbatim and stop tag-scanning.
            out.push_str(&rest[lt..]);
            rest = "";
            break;
        };
        let name = after[..gt]
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(name.as_str(), "br" | "p" | "div") {
            out.push('\n');
        }
        rest = &after[gt + 1..];
    }
    out.push_str(rest);

    // Decode entities — `&amp;` LAST so `&amp;lt;` doesn't double-decode to `<`.
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");

    // Collapse 3+ consecutive newlines to 2.
    let mut collapsed = String::with_capacity(decoded.len());
    let mut nl = 0;
    for ch in decoded.chars() {
        if ch == '\n' {
            nl += 1;
            if nl <= 2 {
                collapsed.push('\n');
            }
        } else {
            nl = 0;
            collapsed.push(ch);
        }
    }
    collapsed.trim().to_string()
}

/// Inverse of `html_to_plain` for writing an OOF message: HTML-escape
/// `& < > "` and turn `\n` into `<br>` (dropping any `\r`). A message authored
/// in lookxy round-trips faithfully through this pair.
pub fn plain_to_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}
```

- [ ] **Step 6: Run to verify they pass**

Run: `bash "$LCARGO" test -p mailcore automatic_ 2>&1 | tail` then `bash "$LCARGO" test -p mailcore html_to_plain 2>&1 | tail` and `bash "$LCARGO" test -p mailcore plain_to_html 2>&1 | tail` and `bash "$LCARGO" test -p mailcore oof_status 2>&1 | tail` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS (5 new tests).

- [ ] **Step 7: Commit**

```bash
git add mailcore/src/graph/model.rs
git commit -m "mailcore: AutomaticReplies model + html/plain message conversion"
```

---

### Task 2: Graph client `get_`/`set_automatic_replies` (`mailcore`)

**Files:**
- Modify: `mailcore/src/graph/client.rs` (import, two methods, `automatic_replies_body`/`datetime_obj` helpers; tests)

**Interfaces:**
- Consumes: `AutomaticReplies`, `OofStatus`, `plain_to_html` (Task 1); `send`, `parse_body`, `Method`, `Value`.
- Produces: `pub fn get_automatic_replies(&self) -> Result<AutomaticReplies, GraphError>`; `pub fn set_automatic_replies(&self, r: &AutomaticReplies) -> Result<(), GraphError>`.

- [ ] **Step 1: Extend the model import**

In `mailcore/src/graph/client.rs`, change:

```rust
use crate::graph::model::{
    AttachmentMeta, Body, DeltaPage, Event, MailFolder, Message, Person, Recipient,
};
```

to add the new types:

```rust
use crate::graph::model::{
    AttachmentMeta, AutomaticReplies, Body, DeltaPage, Event, MailFolder, Message, OofStatus,
    Person, Recipient, plain_to_html,
};
```

- [ ] **Step 2: Write the failing tests**

Add to the `tests` module in `mailcore/src/graph/client.rs`:

```rust
    #[test]
    fn get_automatic_replies_parses_setting() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/mailboxSettings".into(),
            status: 200,
            headers: vec![],
            body: r#"{"automaticRepliesSetting":{"status":"alwaysEnabled","externalAudience":"all","internalReplyMessage":"<p>hi</p>","externalReplyMessage":"bye","scheduledStartDateTime":{"dateTime":"0001-01-01T00:00:00.0000000","timeZone":"UTC"},"scheduledEndDateTime":{"dateTime":"0001-01-01T00:00:00.0000000","timeZone":"UTC"}}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let r = c.get_automatic_replies().unwrap();
        assert_eq!(r.status, OofStatus::AlwaysEnabled);
        assert_eq!(r.internal_message, "hi");
        assert_eq!(r.external_message, "bye");
    }

    #[test]
    fn set_automatic_replies_scheduled_patches_full_body() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/mailboxSettings".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.set_automatic_replies(&AutomaticReplies {
            status: OofStatus::Scheduled,
            external_audience: ExternalAudience::ContactsOnly,
            internal_message: "Away <b>&</b>\nback".into(),
            external_message: "Out".into(),
            scheduled_start_utc: "2026-07-20T09:00:00Z".into(),
            scheduled_end_utc: "2026-07-27T17:00:00Z".into(),
        })
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        let sent = json::parse(&reqs[0].body).unwrap();
        let setting = sent.get("automaticRepliesSetting").unwrap();
        assert_eq!(setting.get("status").and_then(Value::as_str), Some("scheduled"));
        assert_eq!(setting.get("externalAudience").and_then(Value::as_str), Some("contactsOnly"));
        // plain_to_html escaped the `&`/`<`/`>` and encoded the newline.
        assert_eq!(
            setting.get("internalReplyMessage").and_then(Value::as_str),
            Some("Away &lt;b&gt;&amp;&lt;/b&gt;<br>back")
        );
        // Scheduled → both datetime objects present, timeZone UTC.
        assert_eq!(
            setting.get("scheduledStartDateTime").and_then(|d| d.get("dateTime")).and_then(Value::as_str),
            Some("2026-07-20T09:00:00Z")
        );
        assert_eq!(
            setting.get("scheduledEndDateTime").and_then(|d| d.get("timeZone")).and_then(Value::as_str),
            Some("UTC")
        );
    }

    #[test]
    fn set_automatic_replies_disabled_omits_datetimes() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/mailboxSettings".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.set_automatic_replies(&AutomaticReplies {
            status: OofStatus::Disabled,
            external_audience: ExternalAudience::All,
            internal_message: "x".into(),
            external_message: "y".into(),
            scheduled_start_utc: "".into(),
            scheduled_end_utc: "".into(),
        })
        .unwrap();
        let reqs = srv.requests();
        let sent = json::parse(&reqs[0].body).unwrap();
        let setting = sent.get("automaticRepliesSetting").unwrap();
        assert!(setting.get("scheduledStartDateTime").is_none());
        assert!(setting.get("scheduledEndDateTime").is_none());
    }
```

Add `use crate::graph::model::ExternalAudience;` to the `tests` module if `use super::*;` doesn't already bring it (it does via the top-level import extension in Step 1 — `ExternalAudience` is used only in tests, so also add it to the Step 1 `use` list: append `ExternalAudience`).

- [ ] **Step 3: Run to verify they fail**

Run: `bash "$LCARGO" test -p mailcore automatic_replies 2>&1 | tail` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — methods don't exist.

- [ ] **Step 4: Implement the methods + helpers**

In `mailcore/src/graph/client.rs`, add inside `impl GraphClient` (next to `respond_event`):

```rust
    /// GET `/me/mailboxSettings` and parse its `automaticRepliesSetting` into
    /// an [`AutomaticReplies`].
    pub fn get_automatic_replies(&self) -> Result<AutomaticReplies, GraphError> {
        let resp = self.send(Method::Get, "/me/mailboxSettings", None, &[])?;
        let v = parse_body(resp)?;
        AutomaticReplies::from_json(&v)
            .ok_or_else(|| GraphError::Parse("no automaticRepliesSetting".to_string()))
    }

    /// PATCH `/me/mailboxSettings` with the automatic-replies configuration.
    /// The reply messages are `plain_to_html`-encoded; the scheduled datetimes
    /// are sent ONLY when `status == Scheduled` (a non-scheduled write that
    /// carried them would make Graph reject the PATCH or flip to scheduled).
    pub fn set_automatic_replies(&self, r: &AutomaticReplies) -> Result<(), GraphError> {
        self.send(
            Method::Patch,
            "/me/mailboxSettings",
            Some(automatic_replies_body(r)),
            &[],
        )?;
        Ok(())
    }
```

And add these free functions near the other body-builders (e.g. next to `draft_body_json`):

```rust
/// Builds the PATCH body for `set_automatic_replies`:
/// `{"automaticRepliesSetting": {...}}`. Scheduled start/end are included only
/// for `OofStatus::Scheduled`.
fn automatic_replies_body(r: &AutomaticReplies) -> String {
    let mut setting = vec![
        ("status".to_string(), Value::Str(r.status.as_wire().to_string())),
        (
            "externalAudience".to_string(),
            Value::Str(r.external_audience.as_wire().to_string()),
        ),
        (
            "internalReplyMessage".to_string(),
            Value::Str(plain_to_html(&r.internal_message)),
        ),
        (
            "externalReplyMessage".to_string(),
            Value::Str(plain_to_html(&r.external_message)),
        ),
    ];
    if r.status == OofStatus::Scheduled {
        setting.push((
            "scheduledStartDateTime".to_string(),
            utc_datetime_obj(&r.scheduled_start_utc),
        ));
        setting.push((
            "scheduledEndDateTime".to_string(),
            utc_datetime_obj(&r.scheduled_end_utc),
        ));
    }
    Value::Object(vec![(
        "automaticRepliesSetting".to_string(),
        Value::Object(setting),
    )])
    .to_string()
}

/// A Graph `{"dateTime": <utc>, "timeZone": "UTC"}` object for a canonical UTC
/// timestamp.
fn utc_datetime_obj(utc: &str) -> Value {
    Value::Object(vec![
        ("dateTime".to_string(), Value::Str(utc.to_string())),
        ("timeZone".to_string(), Value::Str("UTC".to_string())),
    ])
}
```

- [ ] **Step 5: Run to verify they pass**

Run: `bash "$LCARGO" test -p mailcore automatic_replies 2>&1 | tail` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add mailcore/src/graph/client.rs
git commit -m "mailcore: GraphClient get_/set_automatic_replies (mailboxSettings)"
```

---

### Task 3: Sync `FetchAutomaticReplies`/`SetAutomaticReplies` (`mailcore`)

**Files:**
- Modify: `mailcore/src/sync/engine.rs` (import, 2 command variants, 2 event variants, 2 dispatch arms, 2 handlers; tests)

**Interfaces:**
- Consumes: `GraphClient::get_automatic_replies`/`set_automatic_replies`, `AutomaticReplies` (Tasks 1–2); `with_auth`, `react`, `emit`.
- Produces: `SyncCommand::FetchAutomaticReplies`; `SyncCommand::SetAutomaticReplies { replies: AutomaticReplies }`; `SyncEvent::AutomaticRepliesFetched { replies: AutomaticReplies }`; `SyncEvent::AutomaticRepliesUpdated`.

- [ ] **Step 1: Import `AutomaticReplies`**

In `mailcore/src/sync/engine.rs`, change:

```rust
use crate::graph::model::{DeltaItem, Message};
```

to:

```rust
use crate::graph::model::{AutomaticReplies, DeltaItem, Message};
```

- [ ] **Step 2: Add the command variants**

In the `SyncCommand` enum, after `RespondMeeting { .. }`, add:

```rust
    /// Fetch the mailbox's automatic-replies config
    /// (`GraphClient::get_automatic_replies`) and emit
    /// [`SyncEvent::AutomaticRepliesFetched`]. Direct call, no outbox.
    FetchAutomaticReplies,
    /// Write the mailbox's automatic-replies config
    /// (`GraphClient::set_automatic_replies`) and emit
    /// [`SyncEvent::AutomaticRepliesUpdated`]. Direct call, no outbox.
    SetAutomaticReplies { replies: AutomaticReplies },
```

- [ ] **Step 3: Add the event variants**

In the `SyncEvent` enum, after `MeetingResponded { .. }`, add:

```rust
    /// The mailbox's automatic-replies config was fetched (from
    /// [`SyncCommand::FetchAutomaticReplies`]); the UI prefills its OOF form.
    AutomaticRepliesFetched { replies: AutomaticReplies },
    /// The automatic-replies config was written (from
    /// [`SyncCommand::SetAutomaticReplies`]); the UI closes its OOF form.
    AutomaticRepliesUpdated,
```

- [ ] **Step 4: Add the dispatch arms**

In `handle_command`, after the `SyncCommand::RespondMeeting { .. } => ...` arm, add:

```rust
            SyncCommand::FetchAutomaticReplies => self.fetch_automatic_replies(),
            SyncCommand::SetAutomaticReplies { replies } => self.set_automatic_replies(replies),
```

- [ ] **Step 5: Write the failing engine tests**

Add to the engine `tests` module (mirrors `respond_meeting`'s harness):

```rust
    #[test]
    fn fetch_automatic_replies_emits_fetched() {
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailboxSettings".into(),
                status: 200,
                headers: vec![],
                body: r#"{"automaticRepliesSetting":{"status":"alwaysEnabled","externalAudience":"all","internalReplyMessage":"hi","externalReplyMessage":"bye"}}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir("fetch-oof");
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
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::MessagesUpdated { .. }));
        handle.cmd_tx.send(SyncCommand::FetchAutomaticReplies).unwrap();
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::AutomaticRepliesFetched { replies }
                if replies.status == crate::graph::model::OofStatus::AlwaysEnabled
                    && replies.internal_message == "hi")
        });
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_automatic_replies_patches_and_emits_updated() {
        use crate::graph::model::{AutomaticReplies, ExternalAudience, OofStatus};
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/mailboxSettings".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir("set-oof");
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
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::MessagesUpdated { .. }));
        handle
            .cmd_tx
            .send(SyncCommand::SetAutomaticReplies {
                replies: AutomaticReplies {
                    status: OofStatus::Disabled,
                    external_audience: ExternalAudience::All,
                    internal_message: "x".into(),
                    external_message: "y".into(),
                    scheduled_start_utc: "".into(),
                    scheduled_end_utc: "".into(),
                },
            })
            .unwrap();
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::AutomaticRepliesUpdated));
        assert!(srv.requests().iter().any(|r| r.method == "PATCH" && r.path.contains("/me/mailboxSettings")));
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 6: Run to verify they fail**

Run: `bash "$LCARGO" test -p mailcore automatic_replies 2>&1 | tail` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — handlers don't exist.

- [ ] **Step 7: Implement the handlers**

In `mailcore/src/sync/engine.rs`, add inside `impl Engine` (next to `respond_meeting`):

```rust
    /// Fetch the mailbox's automatic-replies config and emit
    /// `AutomaticRepliesFetched`; a Graph failure goes through `react`. Same
    /// signed-in guard as `fetch_body`.
    fn fetch_automatic_replies(&mut self) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_automatic_replies()) {
            Ok(replies) => self.emit(SyncEvent::AutomaticRepliesFetched { replies }),
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Write the mailbox's automatic-replies config and emit
    /// `AutomaticRepliesUpdated`; a Graph failure goes through `react`. Same
    /// signed-in guard as `fetch_body`.
    fn set_automatic_replies(&mut self, replies: AutomaticReplies) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.set_automatic_replies(&replies)) {
            Ok(()) => self.emit(SyncEvent::AutomaticRepliesUpdated),
            Err(e) => {
                self.react(e);
            }
        }
    }
```

- [ ] **Step 8: Run to verify they pass**

Run: `bash "$LCARGO" test -p mailcore automatic_replies 2>&1 | tail` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS (2 engine tests).

- [ ] **Step 9: Commit**

```bash
git add mailcore/src/sync/engine.rs
git commit -m "mailcore: Fetch/SetAutomaticReplies sync commands + events"
```

---

### Task 4: `OofForm` state + overlay rendering (`lookxy`)

**Files:**
- Create: `lookxy/src/ui/oofform.rs` (state, cycling, prefill, `draw`; render test)
- Modify: `lookxy/src/ui/mod.rs` (declare `pub mod oofform;`)
- Modify: `lookxy/src/app.rs` (`app.oof_form` field + `App::new` initializer)

**Interfaces:**
- Consumes: `OofStatus`, `ExternalAudience`, `AutomaticReplies` (Task 1); `App`.
- Produces: `pub enum OofField { Status, Start, End, Audience, Internal, External }`; `pub struct OofForm { loading, status, start, end, audience, internal, external, focus, error }` with `pub fn loading_default() -> OofForm`, `pub fn prefill(&mut self, r: &AutomaticReplies, off: i64)`, `pub fn cycle_status(&mut self)`, `pub fn cycle_audience(&mut self)`, `pub fn next_field(&mut self)`, `pub fn prev_field(&mut self)`; `pub fn draw(f: &mut Frame, app: &App)`. `App` gains `pub oof_form: Option<OofForm>`.

- [ ] **Step 1: Add the `app.oof_form` field**

In `lookxy/src/app.rs`, add to the `App` struct (near `event_form`):

```rust
    /// The automatic-replies (out-of-office) editor overlay, when open
    /// (opened by `O`; see `App::open_oof_form`).
    pub oof_form: Option<crate::ui::oofform::OofForm>,
```

And in `App::new`'s initializer (near `event_form: None,`), add `oof_form: None,`.

- [ ] **Step 2: Declare the module**

In `lookxy/src/ui/mod.rs`, add alongside the other `pub mod` declarations (e.g. after `pub mod eventform;`):

```rust
pub mod oofform;
```

- [ ] **Step 3: Write the failing render test**

Create `lookxy/src/ui/oofform.rs` starting with the test (it won't compile until the impl exists — write the impl in Step 4). For TDD, add this test at the bottom of the file once the module skeleton is in:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use mailcore::graph::model::{AutomaticReplies, ExternalAudience, OofStatus};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn draw_renders_radios_and_message_labels() {
        let mut app = App::for_test_with_seeded_store();
        let mut form = OofForm::loading_default();
        form.loading = false;
        form.prefill(
            &AutomaticReplies {
                status: OofStatus::Scheduled,
                external_audience: ExternalAudience::All,
                internal_message: "Away".into(),
                external_message: "Out".into(),
                scheduled_start_utc: "2026-07-20T09:00:00Z".into(),
                scheduled_end_utc: "2026-07-27T17:00:00Z".into(),
            },
            0, // UTC offset for the test
        );
        app.oof_form = Some(form);

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Automatic Replies"));
        assert!(text.contains("Scheduled"));
        assert!(text.contains("Internal"));
        assert!(text.contains("External"));
        assert!(text.contains("Away"));
    }

    #[test]
    fn cycling_status_and_audience_wraps() {
        let mut form = OofForm::loading_default();
        assert_eq!(form.status, OofStatus::Disabled);
        form.cycle_status();
        assert_eq!(form.status, OofStatus::AlwaysEnabled);
        form.cycle_status();
        assert_eq!(form.status, OofStatus::Scheduled);
        form.cycle_status();
        assert_eq!(form.status, OofStatus::Disabled);

        form.audience = ExternalAudience::None;
        form.cycle_audience();
        assert_eq!(form.audience, ExternalAudience::ContactsOnly);
        form.cycle_audience();
        assert_eq!(form.audience, ExternalAudience::All);
        form.cycle_audience();
        assert_eq!(form.audience, ExternalAudience::None);
    }
}
```

- [ ] **Step 4: Write the module (state + helpers + draw)**

Write `lookxy/src/ui/oofform.rs` (above the test module):

```rust
//! The automatic-replies (out-of-office) editor overlay. Modeled on
//! `ui::eventform`: a full-frame modal (opened by `O`) with Tab-navigated
//! fields, `Space`-cycled status/audience radios, two multi-line message
//! editors, and an inline error footer. Fetched on open and written through on
//! save — see `App::open_oof_form`/`save_oof_form` and the `Fetch/Set
//! AutomaticReplies` sync commands. This module renders and holds state; the
//! app owns the fetch/save wiring.

use crate::app::App;
use mailcore::graph::model::{AutomaticReplies, ExternalAudience, OofStatus};

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OofField {
    Status,
    Start,
    End,
    Audience,
    Internal,
    External,
}

/// The open automatic-replies editor. `start`/`end` hold local-time display
/// text (parsed on save, only when `status == Scheduled`); `internal`/
/// `external` are plain-text reply messages. `loading` is true from open until
/// `AutomaticRepliesFetched` prefills the form. `error` is the inline footer
/// validation message.
pub struct OofForm {
    pub loading: bool,
    pub status: OofStatus,
    pub start: String,
    pub end: String,
    pub audience: ExternalAudience,
    pub internal: String,
    pub external: String,
    pub focus: OofField,
    pub error: Option<String>,
}

impl OofForm {
    /// The freshly-opened, still-loading form (fields are placeholders until
    /// `prefill`). Status defaults to `Disabled`, audience to `All`.
    pub fn loading_default() -> OofForm {
        OofForm {
            loading: true,
            status: OofStatus::Disabled,
            start: String::new(),
            end: String::new(),
            audience: ExternalAudience::All,
            internal: String::new(),
            external: String::new(),
            focus: OofField::Status,
            error: None,
        }
    }

    /// Fill the fields from a fetched `AutomaticReplies`. `off` is the local
    /// UTC offset in minutes (`ui::calendar::local_offset_minutes()`); the
    /// scheduled UTC bounds are rendered to the form's local display text
    /// (empty when the bound is `""`).
    pub fn prefill(&mut self, r: &AutomaticReplies, off: i64) {
        self.status = r.status;
        self.audience = r.external_audience;
        self.internal = r.internal_message.clone();
        self.external = r.external_message.clone();
        self.start = utc_to_display(&r.scheduled_start_utc, off);
        self.end = utc_to_display(&r.scheduled_end_utc, off);
        self.error = None;
    }

    pub fn cycle_status(&mut self) {
        self.status = match self.status {
            OofStatus::Disabled => OofStatus::AlwaysEnabled,
            OofStatus::AlwaysEnabled => OofStatus::Scheduled,
            OofStatus::Scheduled => OofStatus::Disabled,
        };
    }

    pub fn cycle_audience(&mut self) {
        self.audience = match self.audience {
            ExternalAudience::None => ExternalAudience::ContactsOnly,
            ExternalAudience::ContactsOnly => ExternalAudience::All,
            ExternalAudience::All => ExternalAudience::None,
        };
    }

    pub fn next_field(&mut self) {
        self.focus = match self.focus {
            OofField::Status => OofField::Start,
            OofField::Start => OofField::End,
            OofField::End => OofField::Audience,
            OofField::Audience => OofField::Internal,
            OofField::Internal => OofField::External,
            OofField::External => OofField::Status,
        };
    }

    pub fn prev_field(&mut self) {
        self.focus = match self.focus {
            OofField::Status => OofField::External,
            OofField::Start => OofField::Status,
            OofField::End => OofField::Start,
            OofField::Audience => OofField::End,
            OofField::Internal => OofField::Audience,
            OofField::External => OofField::Internal,
        };
    }
}

/// Renders one canonical-UTC bound to the form's `YYYY-MM-DD HH:MM` local text,
/// or `""` when the bound is empty/unparseable. `utc_iso_to_local` (the same
/// `pub(crate)` inverse the timed event-edit path uses) returns
/// `Option<LocalDateTime>`, and an empty `utc` parses to `None`, so the whole
/// thing collapses to `""` via `unwrap_or_default`.
fn utc_to_display(utc: &str, off: i64) -> String {
    crate::datetime::utc_iso_to_local(utc, off)
        .map(crate::datetime::format_local)
        .unwrap_or_default()
}

/// Renders the OOF editor overlay when `app.oof_form` is open; a no-op
/// otherwise (mirrors `eventform::draw`).
pub fn draw(f: &mut Frame, app: &App) {
    let Some(form) = &app.oof_form else {
        return;
    };
    let area = f.area();
    f.render_widget(Clear, area);

    if form.loading {
        f.render_widget(
            Paragraph::new("Automatic Replies — loading…").block(
                Block::default().borders(Borders::ALL).title("Automatic Replies"),
            ),
            area,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Status
            Constraint::Length(3), // Start
            Constraint::Length(3), // End
            Constraint::Length(3), // Audience
            Constraint::Min(3),    // Internal
            Constraint::Min(3),    // External
            Constraint::Length(1), // Footer
        ])
        .split(area);

    let scheduled = form.status == OofStatus::Scheduled;
    draw_radio(
        f,
        rows[0],
        "Status",
        &[
            ("Off", form.status == OofStatus::Disabled),
            ("On", form.status == OofStatus::AlwaysEnabled),
            ("Scheduled", scheduled),
        ],
        form.focus == OofField::Status,
        true,
    );
    draw_field(f, rows[1], "Start", &form.start, form.focus == OofField::Start, scheduled);
    draw_field(f, rows[2], "End", &form.end, form.focus == OofField::End, scheduled);
    draw_radio(
        f,
        rows[3],
        "External audience",
        &[
            ("None", form.audience == ExternalAudience::None),
            ("Contacts", form.audience == ExternalAudience::ContactsOnly),
            ("All", form.audience == ExternalAudience::All),
        ],
        form.focus == OofField::Audience,
        true,
    );
    draw_field(f, rows[4], "Internal reply", &form.internal, form.focus == OofField::Internal, true);
    draw_field(f, rows[5], "External reply", &form.external, form.focus == OofField::External, true);

    let footer = form
        .error
        .clone()
        .unwrap_or_else(|| "Tab: next  Space: toggle  Ctrl-S: save  Esc: cancel".to_string());
    f.render_widget(Paragraph::new(footer), rows[6]);
}

/// A titled radio row: `Label: (x) A  ( ) B  ( ) C`. `enabled=false` dims it.
fn draw_radio(f: &mut Frame, area: Rect, label: &str, opts: &[(&str, bool)], focused: bool, enabled: bool) {
    let mut spans = format!("{label}: ");
    for (name, on) in opts {
        spans.push_str(if *on { "(x) " } else { "( ) " });
        spans.push_str(name);
        spans.push_str("  ");
    }
    let style = field_style(focused, enabled);
    f.render_widget(
        Paragraph::new(Line::from(spans)).block(border(focused)).style(style),
        area,
    );
}

/// A titled single-line text field. `enabled=false` (e.g. Start/End when not
/// Scheduled) dims it.
fn draw_field(f: &mut Frame, area: Rect, label: &str, value: &str, focused: bool, enabled: bool) {
    f.render_widget(
        Paragraph::new(value.to_string())
            .block(border(focused).title(label.to_string()))
            .style(field_style(focused, enabled)),
        area,
    );
}

fn border(focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(crate::ui::border_style(focused))
}

fn field_style(focused: bool, enabled: bool) -> Style {
    if !enabled {
        Style::default().fg(Color::DarkGray)
    } else if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}
```

CONFIRMED seams: `crate::datetime::utc_iso_to_local(utc: &str, offset_min: i64) -> Option<LocalDateTime>` (`pub(crate)`), `crate::datetime::format_local(LocalDateTime) -> String`, and `crate::ui::border_style(bool) -> Style` (`pub(crate)`) all exist and are reachable from this module.

- [ ] **Step 5: Run the tests**

Run: `bash "$LCARGO" test -p lookxy oofform 2>&1 | tail` and `bash "$LCARGO" test -p lookxy cycling_status 2>&1 | tail` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS (2 tests). Fix any `utc_iso_to_local` name mismatch surfaced here.

- [ ] **Step 6: Commit**

```bash
git add lookxy/src/ui/oofform.rs lookxy/src/ui/mod.rs lookxy/src/app.rs
git commit -m "lookxy: OofForm overlay state + rendering"
```

---

### Task 5: App wiring — open, save, key routing, confirmation (`lookxy`)

**Files:**
- Modify: `lookxy/src/app.rs` (`RsvpKind`-style import if needed; `open_oof_form`, `save_oof_form`, `on_key_char` `O`, `on_sync_event` arms, `is_capturing_text`; tests)
- Modify: `lookxy/src/ui/oofform.rs` (`handle_key`)
- Modify: `lookxy/src/ui/mod.rs` (draw + key routing for the overlay)

**Interfaces:**
- Consumes: `OofForm`/`OofField` (Task 4), `SyncCommand::{FetchAutomaticReplies, SetAutomaticReplies}`, `SyncEvent::{AutomaticRepliesFetched, AutomaticRepliesUpdated}` (Task 3), `AutomaticReplies`, `datetime::parse_start`/`parse_end`, `local_now`, `ui::calendar::local_offset_minutes`.
- Produces: `App::open_oof_form`, `App::save_oof_form`; `oofform::handle_key(&mut App, KeyEvent)`.

- [ ] **Step 1: Import model types into app.rs**

Ensure `lookxy/src/app.rs` imports the OOF model types. Extend the existing model use:

```rust
use mailcore::graph::model::{AttachmentKind, AttachmentMeta, AutomaticReplies, Body, ExternalAudience, OofStatus};
```

(`RsvpKind` is already imported from `mailcore::graph::client`.)

- [ ] **Step 2: Write the failing app tests**

Add to the `tests` module in `lookxy/src/app.rs`:

```rust
    #[test]
    fn o_opens_the_oof_form_and_fetches() {
        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('O');
        assert!(app.oof_form.as_ref().unwrap().loading);
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::FetchAutomaticReplies) => {}
            other => panic!("expected FetchAutomaticReplies, got {other:?}"),
        }
    }

    #[test]
    fn automatic_replies_fetched_prefills_the_form() {
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        app.on_sync_event(SyncEvent::AutomaticRepliesFetched {
            replies: AutomaticReplies {
                status: OofStatus::AlwaysEnabled,
                external_audience: ExternalAudience::ContactsOnly,
                internal_message: "Away".into(),
                external_message: "Out".into(),
                scheduled_start_utc: "".into(),
                scheduled_end_utc: "".into(),
            },
        });
        let form = app.oof_form.as_ref().unwrap();
        assert!(!form.loading);
        assert_eq!(form.status, OofStatus::AlwaysEnabled);
        assert_eq!(form.internal, "Away");
    }

    #[test]
    fn save_oof_form_scheduled_sends_set_with_parsed_utc() {
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain the fetch
        let form = app.oof_form.as_mut().unwrap();
        form.loading = false;
        form.status = OofStatus::Scheduled;
        form.start = "2026-07-20 09:00".into();
        form.end = "2026-07-27 17:00".into();
        form.internal = "Away".into();
        app.save_oof_form();
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::SetAutomaticReplies { replies }) => {
                assert_eq!(replies.status, OofStatus::Scheduled);
                assert!(replies.scheduled_start_utc.ends_with("Z"));
                assert!(!replies.scheduled_start_utc.is_empty());
                assert!(!replies.scheduled_end_utc.is_empty());
            }
            other => panic!("expected SetAutomaticReplies, got {other:?}"),
        }
    }

    #[test]
    fn save_oof_form_invalid_schedule_errors_and_sends_nothing() {
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain the fetch
        let form = app.oof_form.as_mut().unwrap();
        form.loading = false;
        form.status = OofStatus::Scheduled;
        form.start = "not a time".into();
        app.save_oof_form();
        assert!(app.oof_form.as_ref().unwrap().error.is_some());
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err()); // nothing sent
    }

    #[test]
    fn save_oof_form_disabled_sends_empty_schedule() {
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain the fetch
        let form = app.oof_form.as_mut().unwrap();
        form.loading = false;
        form.status = OofStatus::Disabled;
        form.start = "garbage".into(); // ignored when not Scheduled
        app.save_oof_form();
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::SetAutomaticReplies { replies }) => {
                assert_eq!(replies.status, OofStatus::Disabled);
                assert_eq!(replies.scheduled_start_utc, "");
                assert_eq!(replies.scheduled_end_utc, "");
            }
            other => panic!("expected SetAutomaticReplies, got {other:?}"),
        }
    }

    #[test]
    fn automatic_replies_updated_closes_form_and_notifies() {
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        app.on_sync_event(SyncEvent::AutomaticRepliesUpdated);
        assert!(app.oof_form.is_none());
        assert_eq!(app.attachment_notice.as_deref(), Some("Automatic replies updated"));
    }

    #[test]
    fn oof_form_captures_text() {
        let mut app = App::for_test_with_seeded_store();
        assert!(!app.is_capturing_text());
        app.open_oof_form();
        assert!(app.is_capturing_text());
    }
```

- [ ] **Step 3: Run to verify they fail**

Run: `bash "$LCARGO" test -p lookxy oof 2>&1 | tail` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — `open_oof_form`/`save_oof_form`/event arms don't exist.

- [ ] **Step 4: Add `open_oof_form` and `save_oof_form`**

In `lookxy/src/app.rs`, add inside `impl App` (near `respond_meeting`):

```rust
    /// `O`: open the automatic-replies editor and fetch the current config
    /// (the form shows "loading…" until `AutomaticRepliesFetched` prefills it).
    pub fn open_oof_form(&mut self) {
        self.oof_form = Some(crate::ui::oofform::OofForm::loading_default());
        let _ = self.sync.cmd_tx.send(SyncCommand::FetchAutomaticReplies);
    }

    /// Validate and write the automatic-replies form. When `status ==
    /// Scheduled`, the Start/End text is parsed to UTC (inline error on a bad
    /// value or an end at/before the start); other statuses ignore and clear
    /// the window. Sends `SetAutomaticReplies` and leaves the form open — it
    /// closes on `AutomaticRepliesUpdated`.
    pub fn save_oof_form(&mut self) {
        let Some(form) = self.oof_form.as_ref() else {
            return;
        };
        let (start_utc, end_utc) = if form.status == OofStatus::Scheduled {
            let now = local_now();
            let off = crate::ui::calendar::local_offset_minutes();
            let Some(start) = crate::datetime::parse_start(&form.start, now, off) else {
                self.set_oof_error("Invalid start time");
                return;
            };
            let Some(end) = crate::datetime::parse_end(&form.end, &start, now, off) else {
                self.set_oof_error("Invalid end time");
                return;
            };
            if end <= start {
                self.set_oof_error("End must be after start");
                return;
            }
            (start, end)
        } else {
            (String::new(), String::new())
        };
        let form = self.oof_form.as_ref().unwrap();
        let replies = AutomaticReplies {
            status: form.status,
            external_audience: form.audience,
            internal_message: form.internal.clone(),
            external_message: form.external.clone(),
            scheduled_start_utc: start_utc,
            scheduled_end_utc: end_utc,
        };
        self.attachment_notice = Some("Saving…".to_string());
        let _ = self
            .sync
            .cmd_tx
            .send(SyncCommand::SetAutomaticReplies { replies });
    }

    /// Sets the OOF form's inline footer error (no-op if the form isn't open).
    fn set_oof_error(&mut self, msg: &str) {
        if let Some(form) = self.oof_form.as_mut() {
            form.error = Some(msg.to_string());
        }
    }
```

- [ ] **Step 5: Route `O` and handle the events**

In `on_key_char`, add an arm before the `_ => {}` (alongside the RSVP keys):

```rust
            'O' => self.open_oof_form(),
```

In `on_sync_event`, add arms (place near the `MeetingResponded` arm):

```rust
            SyncEvent::AutomaticRepliesFetched { replies } => {
                if let Some(form) = self.oof_form.as_mut() {
                    let off = crate::ui::calendar::local_offset_minutes();
                    form.prefill(&replies, off);
                    form.loading = false;
                }
            }
            SyncEvent::AutomaticRepliesUpdated => {
                self.oof_form = None;
                self.attachment_notice = Some("Automatic replies updated".to_string());
            }
```

Amend the existing `SyncEvent::Error(msg)` arm so a fetch failure clears the loading state (leaving the form open + editable), instead of leaving a stuck "loading…":

```rust
            SyncEvent::Error(msg) => {
                if let Some(form) = self.oof_form.as_mut() {
                    form.loading = false;
                }
                self.error_notice = Some(msg);
            }
```

In `is_capturing_text`, add `oof_form`:

```rust
    pub fn is_capturing_text(&self) -> bool {
        self.search.is_some()
            || self.compose.is_some()
            || self.rsvp_prompt.is_some()
            || self.oof_form.is_some()
    }
```

- [ ] **Step 6: Add `oofform::handle_key`**

In `lookxy/src/ui/oofform.rs`, add (above the test module):

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Key handling while the OOF form is open (routed from `ui::mod::handle_key`
/// ahead of the pane/mode handlers, same precedence `eventform` gets). Tab/↓
/// and Shift-Tab/↑ move focus; Space toggles the focused radio; Ctrl-S (and
/// Enter outside the message editors) saves; Esc cancels; other keys edit the
/// focused text field (the message editors accept Enter as a newline).
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.oof_form = None;
            return;
        }
        KeyCode::Char('s') if ctrl => {
            app.save_oof_form();
            return;
        }
        _ => {}
    }
    let Some(form) = app.oof_form.as_mut() else {
        return;
    };
    if form.loading {
        return; // ignore edits until the fetch lands
    }
    match key.code {
        KeyCode::Tab | KeyCode::Down => form.next_field(),
        KeyCode::BackTab | KeyCode::Up => form.prev_field(),
        KeyCode::Char(' ') if form.focus == OofField::Status => form.cycle_status(),
        KeyCode::Char(' ') if form.focus == OofField::Audience => form.cycle_audience(),
        KeyCode::Enter => match form.focus {
            OofField::Internal => form.internal.push('\n'),
            OofField::External => form.external.push('\n'),
            _ => app.save_oof_form(),
        },
        KeyCode::Backspace => {
            if let Some(field) = editable_field(form) {
                field.pop();
            }
        }
        KeyCode::Char(c) => {
            if let Some(field) = editable_field(form) {
                field.push(c);
            }
        }
        _ => {}
    }
}

/// The mutable text buffer for the focused field, or `None` for the radio
/// fields (Status/Audience, which are edited via Space, not typing).
fn editable_field(form: &mut OofForm) -> Option<&mut String> {
    match form.focus {
        OofField::Start => Some(&mut form.start),
        OofField::End => Some(&mut form.end),
        OofField::Internal => Some(&mut form.internal),
        OofField::External => Some(&mut form.external),
        OofField::Status | OofField::Audience => None,
    }
}
```

NOTE: the `Enter`-outside-message-editors path calls `save_oof_form` after the earlier `let Some(form) = app.oof_form.as_mut()` borrow ends — Rust's NLL allows this because the `form` borrow isn't used after the match arm begins. If the borrow checker objects, restructure so the `Enter`/save cases are handled before taking the `&mut form` (as `Esc`/`Ctrl-S` already are).

- [ ] **Step 7: Wire draw + key routing in `ui/mod.rs`**

In `lookxy/src/ui/mod.rs` `draw`, add at the very top (before the compose check — the OOF form is a full-frame overlay openable from either mode):

```rust
pub fn draw(f: &mut Frame, app: &mut App) {
    if app.oof_form.is_some() {
        oofform::draw(f, &*app);
        return;
    }
    if app.compose.is_some() {
```

In `handle_key`, add right after the `signin_modal` block (so OOF wins over the panes/compose but sign-in still wins):

```rust
    if app.signin_modal.is_some() {
        handle_signin_key(app, key);
        return;
    }
    if app.oof_form.is_some() {
        oofform::handle_key(app, key);
        return;
    }
```

- [ ] **Step 8: Run the tests**

Run: `bash "$LCARGO" test -p lookxy oof 2>&1 | tail` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS (7 app tests).

- [ ] **Step 9: Full workspace test + clippy + fmt**

Run: `bash "$LCARGO" test --workspace 2>&1 | grep -E "test result|error|FAILED"`, then `bash "$LCARGO" clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3`, then `bash "$LCARGO" fmt --all` and `bash "$LCARGO" fmt --all -- --check` (Bash, `dangerouslyDisableSandbox: true`)
Expected: all tests pass; clippy clean; fmt clean (run `fmt --all` to apply, then `--check` passes).

- [ ] **Step 10: Commit**

```bash
git add lookxy/src/app.rs lookxy/src/ui/oofform.rs lookxy/src/ui/mod.rs
git commit -m "lookxy: wire OOF form open/save/keys + fetched/updated events"
```

---

## Self-Review

**Spec coverage:**
- `AutomaticReplies` + `OofStatus`/`ExternalAudience` + wire mapping → Task 1. ✅
- `html_to_plain`/`plain_to_html` → Task 1. ✅
- `get_`/`set_automatic_replies` + PATCH body (datetimes only when Scheduled) → Task 2. ✅
- `FetchAutomaticReplies`/`SetAutomaticReplies` + `AutomaticRepliesFetched`/`AutomaticRepliesUpdated` → Task 3. ✅
- `OofForm` overlay (radios, dimmed schedule, two editors, footer) → Task 4. ✅
- `O` open + fetch; prefill; validated save; Updated closes + notice; `is_capturing_text`; key routing; Error clears loading → Task 5. ✅
- Error handling (invalid/empty schedule inline; end ≤ start; not-signed-in; fetch/patch failure via `react`) → Tasks 3 & 5. ✅

**Placeholder scan:** No TBD/TODO. Two explicit NOTEs flag real-world checks (the `utc_iso_to_local` name; the NLL borrow in `handle_key`) with concrete fallbacks — not deferred work.

**Type consistency:** `OofStatus`/`ExternalAudience`/`AutomaticReplies` field names identical across model (T1), client body-builder (T2), engine command/event (T3), form (T4), and app (T5). `OofForm` field names (`status`/`start`/`end`/`audience`/`internal`/`external`/`focus`/`loading`/`error`) match between the struct (T4), `draw`/`handle_key` (T4/T5), and the app tests (T5). `SyncCommand::SetAutomaticReplies { replies }` / `SyncEvent::AutomaticRepliesFetched { replies }` field name `replies` consistent T3↔T5. `save_oof_form`'s Scheduled-only datetime handling matches the client's Scheduled-only PATCH datetimes (T2), so a Disabled save sends `""`/omits and round-trips.
