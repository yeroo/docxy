# lookxy Propose New Time Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When declining or tentatively-accepting a meeting (from the Calendar RSVP or the mail reader), optionally counter-propose a new time via Graph's `proposedNewTime`.

**Architecture:** A proposed `(start_utc, end_utc)` window threads from a generalized RSVP prompt through both RSVP command paths (`RespondEvent` outbox op, `RespondMeeting` direct call) down to `GraphClient::respond_event`, which adds `proposedNewTime` to the decline/tentative POST body.

**Tech Stack:** Rust (edition 2024), hand-rolled `mailcore::json`, `ureq` Graph client, `ratatui`/`crossterm` TUI.

## Global Constraints

- **Build wrapper:** never call bare `cargo` (fails os error 448). Use `bash "$LCARGO" <args>` where `LCARGO=C:/Users/BORIS_~1/AppData/Local/Temp/claude/C--Users-boris-kudriashov-Source-docxy/1da9a016-b606-4432-8951-6d73bb91c967/scratchpad/lcargo.sh`. EVERY Bash tool call that runs it MUST set `dangerouslyDisableSandbox: true`.
- **MSRV / edition:** edition 2024, MSRV 1.88. No new external crates.
- **No serde:** JSON only via `mailcore::json` (`Value`, `.get`/`.as_str`/`.as_array`/`Object`/`Str`/`Bool`).
- **Propose = decline/tentative only** (Graph rejects `proposedNewTime` on accept); **mail `A` stays instant**; **blank = no proposal**; **both-or-neither** (if either proposed field is filled, both must parse and `end > start`).
- **Proposed datetimes:** local text parsed via `datetime::parse_start`/`parse_end` to canonical UTC; sent as `{dateTime: <utc>, timeZone: "UTC"}`.
- **Esc** sends the plain RSVP (no comment, no proposal), unchanged.

---

### Task 1: Client — `respond_event` proposed window

**Files:**
- Modify: `mailcore/src/graph/client.rs` (`respond_event` signature + body; tests)
- Modify: `mailcore/src/sync/outbox.rs` (call site stopgap)
- Modify: `mailcore/src/sync/engine.rs` (call site stopgap)

**Interfaces:**
- Produces: `respond_event(&self, id, kind: RsvpKind, comment: Option<&str>, send_response: bool, proposed: Option<(String, String)>) -> Result<(), GraphError>` — adds `proposedNewTime` when `proposed.is_some()`.

- [ ] **Step 1: Write the failing test**

Add to the client `tests` module in `mailcore/src/graph/client.rs`:

```rust
    #[test]
    fn respond_event_decline_with_proposed_time_sends_proposed_new_time() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/decline".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event(
            "E1",
            RsvpKind::Decline,
            Some("can we push?"),
            true,
            Some(("2026-07-21T14:00:00Z".into(), "2026-07-21T15:00:00Z".into())),
        )
        .unwrap();
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        let pnt = sent.get("proposedNewTime").unwrap();
        assert_eq!(
            pnt.get("start").unwrap().get("dateTime").and_then(Value::as_str),
            Some("2026-07-21T14:00:00Z")
        );
        assert_eq!(
            pnt.get("end").unwrap().get("timeZone").and_then(Value::as_str),
            Some("UTC")
        );
    }

    #[test]
    fn respond_event_without_proposed_time_omits_the_key() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/accept".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event("E1", RsvpKind::Accept, None, true, None).unwrap();
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        assert!(sent.get("proposedNewTime").is_none());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p mailcore respond_event_decline_with_proposed` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — `respond_event` takes 4 args, not 5.

- [ ] **Step 3: Add the param + body**

In `mailcore/src/graph/client.rs`, change `respond_event`'s signature and body:

```rust
    pub fn respond_event(
        &self,
        id: &str,
        kind: RsvpKind,
        comment: Option<&str>,
        send_response: bool,
        proposed: Option<(String, String)>,
    ) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let action = match kind {
            RsvpKind::Accept => "accept",
            RsvpKind::Decline => "decline",
            RsvpKind::Tentative => "tentativelyAccept",
        };
        let path = format!("/me/events/{id}/{action}");
        let mut obj = vec![
            (
                "comment".to_string(),
                Value::Str(comment.unwrap_or("").to_string()),
            ),
            ("sendResponse".to_string(), Value::Bool(send_response)),
        ];
        if let Some((start, end)) = &proposed {
            let dt = |utc: &str| {
                Value::Object(vec![
                    ("dateTime".to_string(), Value::Str(utc.to_string())),
                    ("timeZone".to_string(), Value::Str("UTC".to_string())),
                ])
            };
            obj.push((
                "proposedNewTime".to_string(),
                Value::Object(vec![
                    ("start".to_string(), dt(start)),
                    ("end".to_string(), dt(end)),
                ]),
            ));
        }
        let body = Value::Object(obj).to_string();
        self.send(Method::Post, &path, Some(body), &[])?;
        Ok(())
    }
```

- [ ] **Step 4: Update the non-test call sites (stopgap `None`) + client test literals**

Two production callers break. Add a trailing `None` for now (Task 2 wires the real value):
- `mailcore/src/sync/outbox.rs` `apply_op` `RespondEvent` arm: `client.respond_event(id, rsvp, comment.as_deref(), true, None)`.
- `mailcore/src/sync/engine.rs` `respond_meeting`: `self.with_auth(|c| c.respond_event(&event_id, kind, None, true, None))`.

Also update the existing client `respond_event` tests (the calendar RSVP tests) to pass a trailing `None`. Build to find them:

Run: `bash "$LCARGO" build -p mailcore --all-targets 2>&1 | grep -E "this function takes|arguments|-->"` and add `, None` to each existing `respond_event(...)` call the compiler flags.

- [ ] **Step 5: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore respond_event` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS (new + existing respond_event tests).

- [ ] **Step 6: Commit**

```bash
git add mailcore/src/graph/client.rs mailcore/src/sync/outbox.rs mailcore/src/sync/engine.rs
git commit -m "mailcore: respond_event proposedNewTime param"
```

---

### Task 2: Sync — thread proposed window through both RSVP commands

**Files:**
- Modify: `mailcore/src/store/mod.rs` (`OutboxOp::RespondEvent` fields, `to_json`, `from_json`)
- Modify: `mailcore/src/sync/outbox.rs` (`apply_op` passes the proposed window)
- Modify: `mailcore/src/sync/engine.rs` (`SyncCommand::RespondEvent`/`RespondMeeting` fields, dispatch, `respond_meeting` handler; tests)

**Interfaces:**
- Consumes: `respond_event(..., proposed)` (Task 1).
- Produces: `OutboxOp::RespondEvent { id, kind, comment, proposed_start_utc: Option<String>, proposed_end_utc: Option<String> }`; `SyncCommand::RespondEvent { id, kind, comment, proposed_start_utc, proposed_end_utc }`; `SyncCommand::RespondMeeting { message_id, kind, comment: Option<String>, proposed_start_utc, proposed_end_utc }`.

- [ ] **Step 1: Extend `OutboxOp::RespondEvent`**

In `mailcore/src/store/mod.rs`, add the two fields to the `RespondEvent` variant (after `comment`):

```rust
    RespondEvent {
        id: String,
        kind: String,
        comment: Option<String>,
        proposed_start_utc: Option<String>,
        proposed_end_utc: Option<String>,
    },
```

In `to_json`, update the `RespondEvent` arm to serialize them (after the `comment` pair):

```rust
            OutboxOp::RespondEvent {
                id,
                kind,
                comment,
                proposed_start_utc,
                proposed_end_utc,
            } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                ("rsvp".to_string(), Value::Str(kind.clone())),
                (
                    "comment".to_string(),
                    match comment {
                        Some(c) => Value::Str(c.clone()),
                        None => Value::Null,
                    },
                ),
                (
                    "proposedStart".to_string(),
                    match proposed_start_utc {
                        Some(s) => Value::Str(s.clone()),
                        None => Value::Null,
                    },
                ),
                (
                    "proposedEnd".to_string(),
                    match proposed_end_utc {
                        Some(s) => Value::Str(s.clone()),
                        None => Value::Null,
                    },
                ),
            ]),
```

In `from_json`, update the `"respondEvent"` arm:

```rust
            "respondEvent" => Some(OutboxOp::RespondEvent {
                id: id()?,
                kind: v.get("rsvp")?.as_str()?.to_string(),
                comment: v.get("comment").and_then(Value::as_str).map(str::to_string),
                proposed_start_utc: v
                    .get("proposedStart")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                proposed_end_utc: v
                    .get("proposedEnd")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
```

- [ ] **Step 2: `apply_op` passes the proposed window**

In `mailcore/src/sync/outbox.rs`, update the `RespondEvent` arm to destructure the new fields and pass `proposed`:

```rust
        OutboxOp::RespondEvent {
            id,
            kind,
            comment,
            proposed_start_utc,
            proposed_end_utc,
        } => {
            let rsvp = rsvp_kind(kind)
                .ok_or_else(|| GraphError::Parse(format!("unrecognized RSVP kind: {kind}")))?;
            let proposed = proposed_start_utc
                .clone()
                .zip(proposed_end_utc.clone());
            client.respond_event(id, rsvp, comment.as_deref(), true, proposed)
        }
```

- [ ] **Step 3: Extend the two sync commands + dispatch + handler**

In `mailcore/src/sync/engine.rs`:

`SyncCommand::RespondEvent` — add the two fields:

```rust
    RespondEvent {
        id: String,
        kind: String,
        comment: Option<String>,
        proposed_start_utc: Option<String>,
        proposed_end_utc: Option<String>,
    },
```

`SyncCommand::RespondMeeting` — add comment + the two fields:

```rust
    RespondMeeting {
        message_id: String,
        kind: RsvpKind,
        comment: Option<String>,
        proposed_start_utc: Option<String>,
        proposed_end_utc: Option<String>,
    },
```

Dispatch `RespondEvent` (thread the fields into the outbox op):

```rust
            SyncCommand::RespondEvent {
                id,
                kind,
                comment,
                proposed_start_utc,
                proposed_end_utc,
            } => {
                self.store.set_event_response(&id, &kind);
                self.emit(SyncEvent::CalendarUpdated);
                self.enqueue_and_drain(OutboxOp::RespondEvent {
                    id,
                    kind,
                    comment,
                    proposed_start_utc,
                    proposed_end_utc,
                });
            }
```

Dispatch `RespondMeeting`:

```rust
            SyncCommand::RespondMeeting {
                message_id,
                kind,
                comment,
                proposed_start_utc,
                proposed_end_utc,
            } => self.respond_meeting(
                &message_id,
                kind,
                comment,
                proposed_start_utc.zip(proposed_end_utc),
            ),
```

Update `respond_meeting`'s signature + the `respond_event` call:

```rust
    fn respond_meeting(
        &mut self,
        message_id: &str,
        kind: RsvpKind,
        comment: Option<String>,
        proposed: Option<(String, String)>,
    ) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        let event_id = match self.with_auth(|c| c.meeting_event_id(message_id)) {
            Ok(Some(id)) => id,
            Ok(None) => {
                self.emit(SyncEvent::Error("not a meeting invite".to_string()));
                return;
            }
            Err(e) => {
                self.react(e);
                return;
            }
        };
        match self.with_auth(|c| {
            c.respond_event(&event_id, kind, comment.as_deref(), true, proposed.clone())
        }) {
            Ok(()) => self.emit(SyncEvent::MeetingResponded {
                message_id: message_id.to_string(),
                kind,
            }),
            Err(e) => {
                self.react(e);
            }
        }
    }
```

- [ ] **Step 4: Write the failing engine test**

Add to the engine `tests` module (mirror `respond_meeting_resolves_event_and_posts_accept`, but decline + proposed):

```rust
    #[test]
    fn respond_meeting_with_proposed_time_posts_decline_with_proposed() {
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "POST".into(),
                path_prefix: "/me/events/E1/decline".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
        );
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/M1".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"M1","event":{"id":"E1"}}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir("respond-meeting-proposed");
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
            .send(SyncCommand::RespondMeeting {
                message_id: "M1".into(),
                kind: RsvpKind::Decline,
                comment: Some("push please".into()),
                proposed_start_utc: Some("2026-07-21T14:00:00Z".into()),
                proposed_end_utc: Some("2026-07-21T15:00:00Z".into()),
            })
            .unwrap();
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::MeetingResponded { .. }));
        let posted = srv.requests().into_iter().find(|r| r.path.ends_with("/decline")).unwrap();
        let sent = crate::json::parse(&posted.body).unwrap();
        assert!(sent.get("proposedNewTime").is_some());
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 5: Build all targets, fix engine/store literal ripple, run tests**

Run: `bash "$LCARGO" build -p mailcore --all-targets 2>&1 | grep -E "missing field|this function takes|-->"` — the existing engine `RespondEvent` send tests and any `OutboxOp::RespondEvent { … }` / `SyncCommand::RespondEvent { … }` / `SyncCommand::RespondMeeting { … }` literals need the new fields (`proposed_start_utc: None, proposed_end_utc: None,` and, for `RespondMeeting`, `comment: None,`). Add them to each flagged literal.
Run: `bash "$LCARGO" test -p mailcore` — expect green incl. the new test.

- [ ] **Step 6: Commit**

```bash
git add mailcore/src/store/mod.rs mailcore/src/sync/outbox.rs mailcore/src/sync/engine.rs
git commit -m "mailcore: thread proposed window through RespondEvent/RespondMeeting"
```

---

### Task 3: App — generalized RSVP prompt + both entry points

**Files:**
- Modify: `lookxy/src/app.rs` (`RsvpTarget`/`RsvpField`, `RsvpPrompt`, `start_rsvp`, mail `D`/`T` opener, `submit_rsvp`/`cancel_rsvp_comment`/`apply_rsvp`, field-edit helpers, `on_key_char`; tests)

**Interfaces:**
- Consumes: `SyncCommand::{RespondEvent, RespondMeeting}` (Task 2), `RsvpKind`, `datetime::parse_start`/`parse_end`.
- Produces: `pub enum RsvpTarget { Event(String), Message(String) }`; `pub enum RsvpField { ProposedStart, ProposedEnd, Comment }`; `RsvpPrompt { target, kind, comment, proposed_start, proposed_end, focus }`.

- [ ] **Step 1: Generalize `RsvpPrompt`**

In `lookxy/src/app.rs`, replace the `RsvpPrompt` struct (currently `{ event_id, kind, comment }`) with:

```rust
/// Which RSVP surface a prompt is for: a calendar `Event` (→ `RespondEvent`)
/// or a mail-reader meeting invite `Message` (→ `RespondMeeting`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RsvpTarget {
    Event(String),
    Message(String),
}

/// The focused field in the RSVP prompt. Proposed-time fields only apply to
/// decline/tentative (accept shows only `Comment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsvpField {
    ProposedStart,
    ProposedEnd,
    Comment,
}

pub struct RsvpPrompt {
    pub target: RsvpTarget,
    /// The response-status vocabulary (`"accepted"`/`"declined"`/
    /// `"tentativelyAccepted"`).
    pub kind: String,
    pub comment: String,
    /// Local-time text for a proposed new window; `""` = no proposal. Only
    /// meaningful/shown for decline/tentative.
    pub proposed_start: String,
    pub proposed_end: String,
    pub focus: RsvpField,
}

impl RsvpPrompt {
    /// True when this RSVP kind can carry a proposed new time (decline or
    /// tentative — never accept).
    pub fn proposes(&self) -> bool {
        self.kind == "declined" || self.kind == "tentativelyAccepted"
    }
}
```

- [ ] **Step 2: Open the prompt from both surfaces**

Update `start_rsvp` (Calendar `a`/`d`/`t`):

```rust
    pub fn start_rsvp(&mut self, kind: &str) {
        let Some(event_id) = self.highlighted_event_id() else {
            return;
        };
        let focus = if kind == "declined" || kind == "tentativelyAccepted" {
            RsvpField::ProposedStart
        } else {
            RsvpField::Comment
        };
        self.rsvp_prompt = Some(RsvpPrompt {
            target: RsvpTarget::Event(event_id),
            kind: kind.to_string(),
            comment: String::new(),
            proposed_start: String::new(),
            proposed_end: String::new(),
            focus,
        });
    }
```

Add a mail opener and rework `on_key_char`'s mail RSVP keys. `respond_meeting` stays for instant accept; add:

```rust
    /// Mail reader `D`/`T` on an opened meeting invite: open the RSVP prompt
    /// (with proposed-time fields) targeting the message. A no-op unless the
    /// opened message is a meeting request (same guard as `respond_meeting`).
    pub fn start_meeting_rsvp(&mut self, kind: &str) {
        let Some(message_id) = self
            .selected_message_row()
            .filter(|m| m.is_meeting_request)
            .map(|m| m.id.clone())
        else {
            return;
        };
        self.rsvp_prompt = Some(RsvpPrompt {
            target: RsvpTarget::Message(message_id),
            kind: kind.to_string(),
            comment: String::new(),
            proposed_start: String::new(),
            proposed_end: String::new(),
            focus: RsvpField::ProposedStart,
        });
    }
```

In `on_key_char`, change the mail RSVP arms:

```rust
            'A' => self.respond_meeting(RsvpKind::Accept),
            'D' => self.start_meeting_rsvp("declined"),
            'T' => self.start_meeting_rsvp("tentativelyAccepted"),
```

(`respond_meeting(Accept)` is unchanged — instant; it sends `RespondMeeting { …, comment: None, proposed_start_utc: None, proposed_end_utc: None }` — update its `SyncCommand::RespondMeeting` literal to include the three new fields as `None`.)

- [ ] **Step 3: Field editing + submit/cancel**

Add field-edit helpers (replacing/augmenting `type_rsvp_comment`/`backspace_rsvp_comment` to act on the focused field), plus focus cycling:

```rust
    /// Types into the focused RSVP field (comment or a proposed-time field).
    pub fn type_rsvp_comment(&mut self, s: &str) {
        if let Some(p) = &mut self.rsvp_prompt {
            match p.focus {
                RsvpField::ProposedStart => p.proposed_start.push_str(s),
                RsvpField::ProposedEnd => p.proposed_end.push_str(s),
                RsvpField::Comment => p.comment.push_str(s),
            }
        }
    }

    /// Backspaces the focused RSVP field.
    pub fn backspace_rsvp_comment(&mut self) {
        if let Some(p) = &mut self.rsvp_prompt {
            match p.focus {
                RsvpField::ProposedStart => {
                    p.proposed_start.pop();
                }
                RsvpField::ProposedEnd => {
                    p.proposed_end.pop();
                }
                RsvpField::Comment => {
                    p.comment.pop();
                }
            }
        }
    }

    /// Tab in the RSVP prompt: cycle focus. Accept skips the proposed-time
    /// fields (Comment only).
    pub fn cycle_rsvp_focus(&mut self) {
        if let Some(p) = &mut self.rsvp_prompt {
            p.focus = if p.proposes() {
                match p.focus {
                    RsvpField::ProposedStart => RsvpField::ProposedEnd,
                    RsvpField::ProposedEnd => RsvpField::Comment,
                    RsvpField::Comment => RsvpField::ProposedStart,
                }
            } else {
                RsvpField::Comment
            };
        }
    }
```

Rework `submit_rsvp` to parse the proposed window and dispatch by target:

```rust
    pub fn submit_rsvp(&mut self) {
        let Some(prompt) = self.rsvp_prompt.as_ref() else {
            return;
        };
        // Parse the proposed window (both-or-neither) when this kind proposes.
        let proposed: Option<(String, String)> = if prompt.proposes()
            && (!prompt.proposed_start.trim().is_empty() || !prompt.proposed_end.trim().is_empty())
        {
            let now = local_now();
            let off = crate::ui::calendar::local_offset_minutes();
            let Some(start) = crate::datetime::parse_start(prompt.proposed_start.trim(), now, off)
            else {
                self.set_rsvp_error();
                return;
            };
            let Some(end) =
                crate::datetime::parse_end(prompt.proposed_end.trim(), &start, now, off)
            else {
                self.set_rsvp_error();
                return;
            };
            if end <= start {
                self.set_rsvp_error();
                return;
            }
            Some((start, end))
        } else {
            None
        };
        let prompt = self.rsvp_prompt.take().unwrap();
        let comment = (!prompt.comment.is_empty()).then_some(prompt.comment.clone());
        self.dispatch_rsvp(prompt.target, prompt.kind, comment, proposed);
    }

    /// Esc: send the plain RSVP (no comment, no proposal).
    pub fn cancel_rsvp_comment(&mut self) {
        let Some(prompt) = self.rsvp_prompt.take() else {
            return;
        };
        self.dispatch_rsvp(prompt.target, prompt.kind, None, None);
    }

    /// Surfaces a proposed-time validation error on the RSVP prompt without
    /// closing it — reuses the transient error notice (the prompt has no
    /// inline error field of its own).
    fn set_rsvp_error(&mut self) {
        self.error_notice = Some("Invalid proposed time".to_string());
    }

    /// The shared dispatch: `Event` → optimistic `set_event_response` +
    /// `RespondEvent`; `Message` → `RespondMeeting` (kind mapped to `RsvpKind`).
    fn dispatch_rsvp(
        &mut self,
        target: RsvpTarget,
        kind: String,
        comment: Option<String>,
        proposed: Option<(String, String)>,
    ) {
        let (proposed_start_utc, proposed_end_utc) = match proposed {
            Some((s, e)) => (Some(s), Some(e)),
            None => (None, None),
        };
        match target {
            RsvpTarget::Event(id) => {
                self.store.set_event_response(&id, &kind);
                self.reload_agenda();
                let _ = self.sync.cmd_tx.send(SyncCommand::RespondEvent {
                    id,
                    kind,
                    comment,
                    proposed_start_utc,
                    proposed_end_utc,
                });
            }
            RsvpTarget::Message(message_id) => {
                let rsvp = match kind.as_str() {
                    "declined" => RsvpKind::Decline,
                    "tentativelyAccepted" => RsvpKind::Tentative,
                    _ => RsvpKind::Accept,
                };
                let _ = self.sync.cmd_tx.send(SyncCommand::RespondMeeting {
                    message_id,
                    kind: rsvp,
                    comment,
                    proposed_start_utc,
                    proposed_end_utc,
                });
            }
        }
    }
```

Delete the old `apply_rsvp` (replaced by `dispatch_rsvp`) and update any caller.

- [ ] **Step 4: Write the failing app tests**

Add to the `tests` module in `lookxy/src/app.rs`:

```rust
    #[test]
    fn calendar_decline_with_proposed_time_sends_respond_event() {
        use crate::app::{RsvpField, RsvpTarget};
        let mut app = App::for_test_with_seeded_store();
        app.rsvp_prompt = Some(crate::app::RsvpPrompt {
            target: RsvpTarget::Event("E1".into()),
            kind: "declined".into(),
            comment: String::new(),
            proposed_start: "2026-07-21 14:00".into(),
            proposed_end: "2026-07-21 15:00".into(),
            focus: RsvpField::ProposedStart,
        });
        app.submit_rsvp();
        // Drain any CalendarUpdated-adjacent commands; find the RespondEvent.
        let mut found = None;
        while let Ok(cmd) = app.test_cmd_rx.as_ref().unwrap().try_recv() {
            if let SyncCommand::RespondEvent { proposed_start_utc, .. } = &cmd {
                found = proposed_start_utc.clone();
                break;
            }
        }
        assert!(found.is_some_and(|s| s.ends_with('Z')));
        assert!(app.rsvp_prompt.is_none());
    }

    #[test]
    fn mail_d_opens_prompt_and_a_is_instant() {
        use crate::app::{RsvpField, RsvpTarget};
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app); // existing helper: seeds + opens an invite
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain open
        // 'A' fires instantly (RespondMeeting), no prompt.
        app.on_key_char('A');
        assert!(app.rsvp_prompt.is_none());
        assert!(matches!(
            app.test_cmd_rx.as_ref().unwrap().try_recv(),
            Ok(SyncCommand::RespondMeeting { .. })
        ));
        // 'D' opens the prompt (Message target), no command yet.
        app.on_key_char('D');
        let p = app.rsvp_prompt.as_ref().unwrap();
        assert!(matches!(p.target, RsvpTarget::Message(_)));
        assert_eq!(p.focus, RsvpField::ProposedStart);
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());
    }

    #[test]
    fn mail_decline_with_proposed_time_sends_respond_meeting() {
        use crate::app::RsvpTarget;
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app);
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {}
        app.start_meeting_rsvp("declined");
        let p = app.rsvp_prompt.as_mut().unwrap();
        p.proposed_start = "2026-07-21 14:00".into();
        p.proposed_end = "2026-07-21 15:00".into();
        let _ = matches!(&p.target, RsvpTarget::Message(_));
        app.submit_rsvp();
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::RespondMeeting { kind, proposed_start_utc, .. }) => {
                assert_eq!(kind, mailcore::graph::client::RsvpKind::Decline);
                assert!(proposed_start_utc.is_some());
            }
            other => panic!("expected RespondMeeting, got {other:?}"),
        }
    }

    #[test]
    fn half_filled_proposed_time_errors_and_sends_nothing() {
        use crate::app::{RsvpField, RsvpTarget};
        let mut app = App::for_test_with_seeded_store();
        app.rsvp_prompt = Some(crate::app::RsvpPrompt {
            target: RsvpTarget::Event("E1".into()),
            kind: "declined".into(),
            comment: String::new(),
            proposed_start: "2026-07-21 14:00".into(),
            proposed_end: String::new(), // half-filled
            focus: RsvpField::ProposedStart,
        });
        app.submit_rsvp();
        assert!(app.rsvp_prompt.is_some()); // stayed open
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());
        assert_eq!(app.error_notice.as_deref(), Some("Invalid proposed time"));
    }
```

- [ ] **Step 5: Run to verify (fail → pass)**

Run: `bash "$LCARGO" test -p lookxy -- calendar_decline_with_proposed mail_d_opens_prompt mail_decline_with_proposed half_filled_proposed` (single filter) `bash "$LCARGO" test -p lookxy proposed` (Bash, `dangerouslyDisableSandbox: true`)
Expected: after Steps 1–3, PASS. Fix any `RsvpPrompt { … }` literal (e.g. the old start_rsvp test) the compiler flags.

- [ ] **Step 6: Commit**

```bash
git add lookxy/src/app.rs
git commit -m "lookxy: generalized RSVP prompt with proposed new time, both surfaces"
```

---

### Task 4: UI — multi-field RSVP prompt + shared routing

**Files:**
- Modify: `lookxy/src/ui/calendar.rs` (`draw_rsvp_prompt`, `handle_rsvp_prompt_key`; test)
- Modify: `lookxy/src/ui/mod.rs` (route `rsvp_prompt` keys at the top; draw the prompt in mail mode)

**Interfaces:**
- Consumes: `RsvpPrompt`/`RsvpField` (Task 3), `App::{cycle_rsvp_focus, submit_rsvp, cancel_rsvp_comment, type_rsvp_comment, backspace_rsvp_comment}`.

- [ ] **Step 1: Write the failing render test**

Add to the `tests` module in `lookxy/src/ui/calendar.rs`:

```rust
    #[test]
    fn rsvp_prompt_shows_proposed_rows_for_decline_only() {
        use crate::app::{RsvpField, RsvpPrompt, RsvpTarget};
        let mut app = App::for_test_with_seeded_store();
        app.rsvp_prompt = Some(RsvpPrompt {
            target: RsvpTarget::Event("E1".into()),
            kind: "declined".into(),
            comment: String::new(),
            proposed_start: String::new(),
            proposed_end: String::new(),
            focus: RsvpField::ProposedStart,
        });
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw_rsvp_prompt(f, &app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Proposed start"));
        assert!(text.contains("Proposed end"));

        // Accept: no proposed rows.
        app.rsvp_prompt.as_mut().unwrap().kind = "accepted".into();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw_rsvp_prompt(f, &app)).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("Proposed start"));
    }
```

(Reuse the calendar test module's existing `Terminal`/`TestBackend`/`App` imports.)

- [ ] **Step 2: Render the multi-field prompt**

In `lookxy/src/ui/calendar.rs`, replace `draw_rsvp_prompt`'s body so it renders proposed rows (for decline/tentative) + a comment row, with the focused field marked. Use a taller centered rect and a vertical layout:

```rust
fn draw_rsvp_prompt(f: &mut Frame, app: &App) {
    use ratatui::widgets::Clear;

    let Some(prompt) = &app.rsvp_prompt else {
        return;
    };
    let verb = match prompt.kind.as_str() {
        "accepted" => "Accept",
        "declined" => "Decline",
        "tentativelyAccepted" => "Tentative",
        other => other,
    };
    let proposes = prompt.proposes();
    let height = if proposes { 40 } else { 20 };
    let area = crate::ui::centered_rect(70, height, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(
            "{verb} \u{2014} Tab: field  Enter: send  Esc: send without"
        ))
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let field_line = |label: &str, value: &str, focused: bool| -> Line<'static> {
        let caret = if focused { "_" } else { "" };
        Line::from(format!("{label}: {value}{caret}"))
    };
    if proposes {
        lines.push(Line::from("Propose new time (blank = no proposal):"));
        lines.push(field_line(
            "  Proposed start",
            &prompt.proposed_start,
            prompt.focus == crate::app::RsvpField::ProposedStart,
        ));
        lines.push(field_line(
            "  Proposed end",
            &prompt.proposed_end,
            prompt.focus == crate::app::RsvpField::ProposedEnd,
        ));
        lines.push(Line::from(""));
    }
    lines.push(field_line(
        "Comment",
        &prompt.comment,
        prompt.focus == crate::app::RsvpField::Comment,
    ));
    f.render_widget(Paragraph::new(lines), inner);
}
```

(Add any missing imports — `Line` is already used in this file.)

- [ ] **Step 3: Field navigation in `handle_rsvp_prompt_key`**

Change `handle_rsvp_prompt_key` (make it `pub(crate)` — it's routed from `ui::mod` now) to cycle focus on Tab and edit the focused field:

```rust
pub(crate) fn handle_rsvp_prompt_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => app.submit_rsvp(),
        KeyCode::Esc => app.cancel_rsvp_comment(),
        KeyCode::Tab => app.cycle_rsvp_focus(),
        KeyCode::Backspace => app.backspace_rsvp_comment(),
        KeyCode::Char(c) => app.type_rsvp_comment(&c.to_string()),
        _ => {}
    }
}
```

- [ ] **Step 4: Route + draw in both modes (`ui/mod.rs`)**

In `lookxy/src/ui/calendar.rs` `handle_key`, remove the `if app.rsvp_prompt.is_some() { handle_rsvp_prompt_key(app, key); return; }` block at its top (routing moves up).

In `lookxy/src/ui/mod.rs` `handle_key`, add near the top — right after the sign-in and OOF handlers, before the other overlays/mode branches:

```rust
    // The RSVP prompt (calendar a/d/t or mail-reader D/T) captures every key
    // while open, in both modes.
    if app.rsvp_prompt.is_some() {
        calendar::handle_rsvp_prompt_key(app, key);
        return;
    }
```

In `ui::mod::draw`'s mail-mode body (after the panes/status bar, alongside the other popup draws), add:

```rust
    calendar::draw_rsvp_prompt_public(f, &*app);
```

exposing a thin `pub(crate) fn draw_rsvp_prompt_public(f, app)` in `calendar.rs` that calls the private `draw_rsvp_prompt` (or make `draw_rsvp_prompt` itself `pub(crate)` and call it directly). The Calendar branch already draws it via `draw_calendar` → `draw_rsvp_prompt`, so only the mail branch needs the extra call.

- [ ] **Step 5: Run the tests**

Run: `bash "$LCARGO" test -p lookxy rsvp_prompt_shows_proposed` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 6: Full workspace gate**

Run: `bash "$LCARGO" test --workspace`, then `bash "$LCARGO" clippy --workspace --all-targets -- -D warnings`, then `bash "$LCARGO" fmt --all` + `bash "$LCARGO" fmt --all -- --check` (Bash, `dangerouslyDisableSandbox: true`)
Expected: all green, clippy clean, fmt clean. Fix any `RsvpPrompt`/`SyncCommand::RespondEvent`/`RespondMeeting`/`OutboxOp::RespondEvent` literal the build flags.

- [ ] **Step 7: Commit**

```bash
git add lookxy/src/ui/calendar.rs lookxy/src/ui/mod.rs
git commit -m "lookxy: multi-field RSVP prompt (proposed time) routed in both modes"
```

---

## Self-Review

**Spec coverage:**
- `respond_event` proposed param + `proposedNewTime` body (include/omit) → Task 1. ✅
- `OutboxOp::RespondEvent` proposed fields + `apply_op`; `SyncCommand::RespondEvent`/`RespondMeeting` proposed (+ RespondMeeting comment) + engine `respond_meeting` → Task 2. ✅
- Generalized `RsvpPrompt` (`RsvpTarget`/`RsvpField`), calendar `a/d/t` + mail `A` instant / `D`/`T` prompt, both-or-neither parse + validation, dispatch by target, Esc plain → Task 3. ✅
- Multi-field prompt render (proposed rows for decline only), Tab/edit key handling, routing moved to `ui::mod` top + mail-mode draw → Task 4. ✅
- Error handling: half-filled/invalid/`end ≤ start` → error, nothing sent (Task 3); accept never proposes (Task 3 `proposes()` + Task 4 render guard); mail non-invite no-op (Task 3 guard). ✅

**Placeholder scan:** No TBD/TODO. The literal-ripple steps (Task 2 Step 5, Task 3 Step 5, Task 4 Step 6) are mechanical with a compiler backstop. `open_meeting_invite` (Task 3 tests) is the existing helper from the meeting-RSVP tests — reused, not redefined.

**Type consistency:** `proposed_start_utc`/`proposed_end_utc: Option<String>` identical across `OutboxOp::RespondEvent`, `SyncCommand::RespondEvent`, `SyncCommand::RespondMeeting` (T2) and the app dispatch (T3). `respond_event(..., proposed: Option<(String,String)>)` consistent T1↔T2. `RsvpTarget`/`RsvpField`/`RsvpPrompt` fields (`target`/`kind`/`comment`/`proposed_start`/`proposed_end`/`focus`) match between the struct (T3), the app methods (T3), and the UI (T4). `RespondMeeting.kind: RsvpKind` (T2) matches the `dispatch_rsvp` mapping (T3). `proposes()` used consistently T3↔T4.
