# lookxy Meeting RSVP from Mail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user Accept / Decline / Tentatively-accept a meeting-invite email from the reading pane, sending the response to the organizer and recording the RSVP on the underlying calendar event.

**Architecture:** A meeting invite arrives as a message whose Graph `@odata.type` is `#microsoft.graph.eventMessageRequest`. We mirror that as a boolean on the message (model + store), show a flag-driven banner in the reader, and — on `A`/`D`/`T` — resolve the message's underlying `event` id via `$expand` and reuse the existing `respond_event` (accept/decline/tentativelyAccept) through a new synchronous `RespondMeeting` sync command (a direct Graph call like `SaveAttachment`, not an optimistic-local outbox op — there is no local event row to update).

**Tech Stack:** Rust (edition 2024), hand-rolled `mailcore::json`, `ureq` Graph client, `rusqlite` store, `ratatui`/`crossterm` TUI, `std::sync::mpsc` engine channels.

## Global Constraints

- **Build wrapper:** never call bare `cargo`. Use `bash "$LCARGO" <args>` where `LCARGO = C:\Users\BORIS_~1\AppData\Local\Temp\claude\C--Users-boris-kudriashov-Source-docxy\1da9a016-b606-4432-8951-6d73bb91c967\scratchpad\lcargo.sh`. Every Bash tool call that runs it MUST set `dangerouslyDisableSandbox: true`. Bare cargo fails with os error 448.
- **MSRV / edition:** edition 2024, MSRV 1.88. No new external crates.
- **No serde:** JSON is parsed/emitted only via `mailcore::json` (`Value`, `parse`, `Value::get`/`as_str`/`as_bool`/`Object`/`Str`/`Bool`).
- **Keys (locked):** uppercase `A`=Accept, `D`=Decline, `T`=Tentative, active ONLY when the opened message is a meeting request. Lowercase `a`/`d`/`t` keep their existing meanings (attachments / delete / toggle-threaded) unchanged.
- **Always notify the organizer** (`send_response = true`); **no comment prompt**. Invites only (`eventMessageRequest`) — response/cancellation messages get no affordance.
- **Secrets:** never log tokens or bodies. Error strings surfaced via `SyncEvent::Error` must carry no secret.
- **`@odata.type` needs no `$select` change:** it is an OData control annotation auto-emitted for derived resource types, so invite messages already carry it in the existing delta response. Do not modify `MESSAGE_SELECT`.

---

### Task 1: Message carries `is_meeting_request` end to end (model + store)

Add the boolean to both `Message` (parsed from Graph) and `MessageRow` (persisted/read), wire the store schema, migration, upsert, and every read query, and update all struct literals so the workspace compiles.

**Files:**
- Modify: `mailcore/src/graph/model.rs` (`Message` struct + `from_json`; test)
- Modify: `mailcore/src/store/schema.rs` (`messages` CREATE TABLE)
- Modify: `mailcore/src/store/mod.rs` (`MessageRow` struct; `Store::init` migration; `upsert_message`; `messages_in_folder`/`conversations_in_folder`/`draft`/`search` SELECTs; `map_message_row`; `msg` test helper; tests)
- Modify (mechanical literal updates only): `mailcore/src/sync/outbox.rs`, `mailcore/src/thread.rs`, `lookxy/src/app.rs`, `lookxy/src/ui/mod.rs`, `lookxy/src/ui/search.rs`, `lookxy/src/ui/attachments.rs`, `lookxy/src/control.rs`

**Interfaces:**
- Produces: `mailcore::graph::model::Message.is_meeting_request: bool`; `mailcore::store::MessageRow.is_meeting_request: bool`. `Message::from_json` sets it from `@odata.type`. Both structs otherwise unchanged.

- [ ] **Step 1: Write the failing model test**

Add to the `tests` module in `mailcore/src/graph/model.rs`:

```rust
#[test]
fn message_flags_event_message_request_as_meeting() {
    let invite = crate::json::parse(
        r##"{"@odata.type":"#microsoft.graph.eventMessageRequest","id":"M1","conversationId":"C","subject":"Invite","from":{"emailAddress":{"name":"A","address":"a@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":""}"##,
    )
    .unwrap();
    assert!(Message::from_json(&invite).unwrap().is_meeting_request);

    let ordinary = crate::json::parse(
        r#"{"id":"M2","conversationId":"C","subject":"Hi","from":{"emailAddress":{"name":"A","address":"a@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":""}"#,
    )
    .unwrap();
    assert!(!Message::from_json(&ordinary).unwrap().is_meeting_request);

    let response = crate::json::parse(
        r##"{"@odata.type":"#microsoft.graph.eventMessageResponse","id":"M3","conversationId":"C","subject":"RE","from":{"emailAddress":{"name":"A","address":"a@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":""}"##,
    )
    .unwrap();
    assert!(!Message::from_json(&response).unwrap().is_meeting_request);
}
```

- [ ] **Step 2: Run it to verify it fails to compile**

Run: `bash "$LCARGO" test -p mailcore message_flags_event_message_request_as_meeting` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: FAIL — `Message` has no field `is_meeting_request`.

- [ ] **Step 3: Add the field and parse it**

In `mailcore/src/graph/model.rs`, add to the `Message` struct (after `is_draft`):

```rust
    pub is_draft: bool,
    /// Graph's `@odata.type == "#microsoft.graph.eventMessageRequest"`: this
    /// message is a meeting invite the user can RSVP to (see the reader's
    /// meeting banner and `SyncCommand::RespondMeeting`). `@odata.type` is an
    /// OData control annotation auto-emitted for derived resource types, so it
    /// arrives with the normal delta response — no `$select` change needed.
    pub is_meeting_request: bool,
```

And in `Message::from_json`, after the `is_draft` line:

```rust
            is_draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
            is_meeting_request: v.get("@odata.type").and_then(Value::as_str)
                == Some("#microsoft.graph.eventMessageRequest"),
```

- [ ] **Step 4: Add the store column and `MessageRow` field**

In `mailcore/src/store/schema.rs`, change the `messages` table's last column line from:

```sql
    is_draft        INTEGER NOT NULL DEFAULT 0
);
```

to:

```sql
    is_draft        INTEGER NOT NULL DEFAULT 0,
    is_meeting_request INTEGER NOT NULL DEFAULT 0
);
```

In `mailcore/src/store/mod.rs`, add to the `MessageRow` struct (after `bcc_recipients`):

```rust
    pub is_draft: bool,
    pub bcc_recipients: String,
    /// Mirror of `Message::is_meeting_request` — true for a meeting-invite
    /// (`eventMessageRequest`) message. Drives the reader's RSVP banner.
    pub is_meeting_request: bool,
}
```

- [ ] **Step 5: Add the idempotent migration**

In `Store::init` (`mailcore/src/store/mod.rs`), after the `source_url` ALTER (the last `ALTER TABLE attachments ...` line), add:

```rust
        let _ = conn.execute("ALTER TABLE attachments ADD COLUMN source_url TEXT", []);
        // Same idempotent-migration pattern as `is_draft`/`bcc_recipients`
        // above, for `messages.is_meeting_request` (meeting-invite RSVP): the
        // ALTER errors ("duplicate column name") on any DB already carrying it
        // (every fresh DB gets it from schema.rs), so swallow that.
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN is_meeting_request INTEGER NOT NULL DEFAULT 0",
            [],
        );
```

- [ ] **Step 6: Persist and read the column**

In `upsert_message`, extend the INSERT column list, the `VALUES` placeholders, the conflict `SET` list, and the params. Change:

```rust
                 is_read, is_flagged, has_attachments, importance, preview, is_draft
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
```

to:

```rust
                 is_read, is_flagged, has_attachments, importance, preview, is_draft,
                 is_meeting_request
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
```

Change the conflict tail from:

```rust
                 preview = excluded.preview,
                 is_draft = excluded.is_draft",
```

to:

```rust
                 preview = excluded.preview,
                 is_draft = excluded.is_draft,
                 is_meeting_request = excluded.is_meeting_request",
```

And add the param after `m.is_draft,`:

```rust
                m.is_draft,
                m.is_meeting_request,
            ],
```

In `map_message_row`, add the field after `bcc_recipients: row.get(16)?,`:

```rust
        bcc_recipients: row.get(16)?,
        is_meeting_request: row.get(17)?,
    })
```

In EACH of the four SELECTs that feed `map_message_row` — `messages_in_folder`, `conversations_in_folder` (both the inner `keyed` CTE select AND the outer `k.` select), `draft`, and `search` — append the new column so it lands at index 17. For the three flat selects change:

```sql
                    is_read, is_flagged, has_attachments, importance, preview, is_draft,
                    bcc_recipients
```

to:

```sql
                    is_read, is_flagged, has_attachments, importance, preview, is_draft,
                    bcc_recipients, is_meeting_request
```

For `conversations_in_folder`'s inner `keyed` CTE, change `is_draft, bcc_recipients,` (it is followed by the `CASE ... AS conv_key`) to `is_draft, bcc_recipients, is_meeting_request,`; and change its outer select `k.is_draft, k.bcc_recipients` to `k.is_draft, k.bcc_recipients, k.is_meeting_request`.

- [ ] **Step 7: Write the failing store round-trip + migration tests**

In `mailcore/src/store/mod.rs`, update the `msg` test helper to set the new field:

```rust
            preview: "p".into(),
            is_draft: false,
            is_meeting_request: false,
        }
```

Then add these tests to the store `tests` module:

```rust
    #[test]
    fn message_round_trips_is_meeting_request() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder {
            id: "F".into(),
            display_name: "Inbox".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: Some("inbox".into()),
        })
        .unwrap();
        let mut m = msg("01", true);
        m.is_meeting_request = true;
        s.upsert_message("F", &m).unwrap();
        let rows = s.messages_in_folder("F", 10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_meeting_request);
    }

    #[test]
    fn is_meeting_request_migration_is_idempotent() {
        // `Store::init` runs the ALTER even on a fresh DB (which already has
        // the column from schema.rs); opening twice must not error.
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-mtg-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        Store::open(&path).unwrap();
        Store::open(&path).unwrap(); // second open re-runs init; must not panic/err
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 8: Update every remaining `Message`/`MessageRow` struct literal**

Adding a non-`Default` field breaks every struct literal. Build the workspace and add `is_meeting_request: false,` to each `Message { .. }` and `MessageRow { .. }` literal the compiler flags (all are non-invite fixtures, so `false` everywhere). Known literal sites: `mailcore/src/sync/outbox.rs`, `mailcore/src/thread.rs`, `lookxy/src/app.rs` (incl. `for_test_with_seeded_store`), `lookxy/src/ui/mod.rs`, `lookxy/src/ui/search.rs`, `lookxy/src/ui/attachments.rs`, `lookxy/src/control.rs`, plus the model/store test helpers already handled above.

Run: `bash "$LCARGO" build --workspace` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: after adding the field to each flagged literal, a clean build (0 errors).

- [ ] **Step 9: Run the tests**

Run: `bash "$LCARGO" test -p mailcore` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: PASS, including the three new tests.

- [ ] **Step 10: Commit**

```bash
git add mailcore/src/graph/model.rs mailcore/src/store/schema.rs mailcore/src/store/mod.rs mailcore/src/sync/outbox.rs mailcore/src/thread.rs lookxy/src/app.rs lookxy/src/ui/mod.rs lookxy/src/ui/search.rs lookxy/src/ui/attachments.rs lookxy/src/control.rs
git commit -m "mailcore: carry is_meeting_request on message model + store"
```

---

### Task 2: `GraphClient::meeting_event_id`

Resolve a meeting-invite message's underlying calendar `event` id via `$expand`, so the RSVP can be sent to that event.

**Files:**
- Modify: `mailcore/src/graph/client.rs` (new method + test)

**Interfaces:**
- Consumes: `GraphClient::send`, `parse_body`, `encode_path_segment`, `Value::get`/`as_str` (existing).
- Produces: `pub fn meeting_event_id(&self, message_id: &str) -> Result<Option<String>, GraphError>` — `Some(event_id)` for an invite with an expanded `event`, `None` when the message has no `event` (not an invite / odd server state).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `mailcore/src/graph/client.rs`:

```rust
    #[test]
    fn meeting_event_id_reads_expanded_event() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"M1","event":{"id":"E1"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.meeting_event_id("M1").unwrap().as_deref(), Some("E1"));
        // The request carries the $expand for the invite's event id.
        let reqs = srv.requests();
        assert!(reqs[0].path.contains("expand"));
    }

    #[test]
    fn meeting_event_id_is_none_without_event() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M2".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"M2"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.meeting_event_id("M2").unwrap(), None);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p mailcore meeting_event_id` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: FAIL — no method `meeting_event_id`.

- [ ] **Step 3: Implement the method**

In `mailcore/src/graph/client.rs`, add inside `impl GraphClient` (next to `respond_event`):

```rust
    /// GET `/me/messages/{id}?$expand=microsoft.graph.eventMessageRequest/event($select=id)`
    /// — an `eventMessageRequest` message exposes the calendar event it invites
    /// to via its `event` navigation property; expanding it (selecting only
    /// `id`) is enough to then `respond_event` on that event. Returns the
    /// expanded `event.id`, or `None` when the message carries no `event`
    /// (not an invite, or an odd server state) so the caller can surface a
    /// "not a meeting invite" notice rather than a hard error.
    pub fn meeting_event_id(&self, message_id: &str) -> Result<Option<String>, GraphError> {
        let id = encode_path_segment(message_id);
        let path = format!(
            "/me/messages/{id}?$expand=microsoft.graph.eventMessageRequest/event($select=id)"
        );
        let resp = self.send(Method::Get, &path, None, &[])?;
        let v = parse_body(resp)?;
        Ok(v.get("event")
            .and_then(|e| e.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string))
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `bash "$LCARGO" test -p mailcore meeting_event_id` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add mailcore/src/graph/client.rs
git commit -m "mailcore: GraphClient::meeting_event_id resolves an invite's event id"
```

---

### Task 3: Engine `RespondMeeting` command + `MeetingResponded` event

Add a synchronous meeting-RSVP command handled like `SaveAttachment` (direct Graph call via `with_auth`/`react`, no outbox op): resolve the event id, then `respond_event(event_id, kind, None, true)`, then emit `MeetingResponded`.

**Files:**
- Modify: `mailcore/src/sync/engine.rs` (`SyncCommand`, `SyncEvent`, dispatch arm, handler; test)

**Interfaces:**
- Consumes: `GraphClient::meeting_event_id` (Task 2), `GraphClient::respond_event`, `RsvpKind` (existing, re-exported via `crate::graph::client`), `with_auth`, `react`, `emit`.
- Produces: `SyncCommand::RespondMeeting { message_id: String, kind: RsvpKind }`; `SyncEvent::MeetingResponded { message_id: String, kind: RsvpKind }`.

- [ ] **Step 1: Import `RsvpKind` into the engine**

In `mailcore/src/sync/engine.rs`, change the client import:

```rust
use crate::graph::client::{DeltaCursor, GraphClient, GraphError};
```

to:

```rust
use crate::graph::client::{DeltaCursor, GraphClient, GraphError, RsvpKind};
```

- [ ] **Step 2: Add the command variant**

In the `SyncCommand` enum, after the `SaveItemAttachment { .. }` variant, add:

```rust
    /// RSVP to a meeting-invite email: resolve the invite's underlying event
    /// (`GraphClient::meeting_event_id`) and `respond_event` on it
    /// (`send_response = true`, no comment). A direct Graph call like
    /// `SaveAttachment` — not an optimistic-local outbox op — since there's no
    /// local event row to update from the mail side. Emits
    /// [`SyncEvent::MeetingResponded`] on success, or [`SyncEvent::Error`] when
    /// the message resolves to no event.
    RespondMeeting {
        message_id: String,
        kind: RsvpKind,
    },
```

- [ ] **Step 3: Add the event variant**

In the `SyncEvent` enum, after `AttachmentSaved { path: PathBuf }`, add:

```rust
    /// A meeting invite was RSVP'd (from [`SyncCommand::RespondMeeting`]); the
    /// UI shows a confirmation and marks the message read.
    MeetingResponded {
        message_id: String,
        kind: RsvpKind,
    },
```

- [ ] **Step 4: Add the dispatch arm**

In `handle_command`, after the `SyncCommand::SaveItemAttachment { .. } => ...` arm, add:

```rust
            SyncCommand::RespondMeeting { message_id, kind } => {
                self.respond_meeting(&message_id, kind)
            }
```

- [ ] **Step 5: Write the failing engine test**

Add to the engine `tests` module in `mailcore/src/sync/engine.rs` (mirrors `save_item_attachment`'s harness — front-insert routes, wait for backfill, send the command):

```rust
    #[test]
    fn respond_meeting_resolves_event_and_posts_accept() {
        let mut routes = backfill_routes();
        // POST the accept (front so it wins over any generic prefix).
        routes.insert(
            0,
            Route {
                method: "POST".into(),
                path_prefix: "/me/events/E1/accept".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
        );
        // GET the invite message with its expanded event id.
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

        let dir = unique_dir("respond-meeting");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);

        let handle = spawn_with_bases(
            store_path,
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );

        handle
            .cmd_tx
            .send(SyncCommand::RespondMeeting {
                message_id: "M1".into(),
                kind: RsvpKind::Accept,
            })
            .unwrap();
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MeetingResponded { message_id, kind }
                if message_id == "M1" && *kind == RsvpKind::Accept),
        );
        // The accept POST actually hit Graph.
        assert!(srv.requests().iter().any(|r| r.path.ends_with("/accept")));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn respond_meeting_without_event_emits_error() {
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/M1".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"M1"}"#.into(), // no expanded event
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("respond-meeting-noevent");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);

        let handle = spawn_with_bases(
            store_path,
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );

        handle
            .cmd_tx
            .send(SyncCommand::RespondMeeting {
                message_id: "M1".into(),
                kind: RsvpKind::Decline,
            })
            .unwrap();
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::Error(_)));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 6: Run to verify it fails**

Run: `bash "$LCARGO" test -p mailcore respond_meeting` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: FAIL — no method `respond_meeting`.

- [ ] **Step 7: Implement the handler**

In `mailcore/src/sync/engine.rs`, add the method inside `impl Engine` (place it right after `save_item_attachment`, in the attachments/fetch section):

```rust
    /// Resolve a meeting-invite message's underlying event
    /// (`GraphClient::meeting_event_id`) and RSVP to it via `respond_event`
    /// (`send_response = true`, no comment) — a direct Graph call like
    /// `save_attachment`, not an outbox op. On success emit
    /// `SyncEvent::MeetingResponded`; a message that resolves to no event
    /// emits `SyncEvent::Error` ("not a meeting invite"); a Graph failure goes
    /// through `react` for the standard auth/throttle/transport handling. Same
    /// signed-in guard as `fetch_body`.
    fn respond_meeting(&mut self, message_id: &str, kind: RsvpKind) {
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
        match self.with_auth(|c| c.respond_event(&event_id, kind, None, true)) {
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

- [ ] **Step 8: Run to verify it passes**

Run: `bash "$LCARGO" test -p mailcore respond_meeting` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: PASS (both tests).

- [ ] **Step 9: Commit**

```bash
git add mailcore/src/sync/engine.rs
git commit -m "mailcore: RespondMeeting sync command resolves + RSVPs a meeting invite"
```

---

### Task 4: App `respond_meeting`, key routing, and confirmation

Add the app-level accessor for the opened message row, guard the uppercase `A`/`D`/`T` keys to meeting invites, send `RespondMeeting`, and on `MeetingResponded` show a notice and mark the message read.

**Files:**
- Modify: `lookxy/src/app.rs` (`selected_message_row`, `respond_meeting`, `on_key_char`, `on_sync_event`; tests)

**Interfaces:**
- Consumes: `SyncCommand::RespondMeeting`, `SyncEvent::MeetingResponded`, `mailcore::graph::client::RsvpKind`, `MessageRow.is_meeting_request` (Task 1), `App::mark_read`.
- Produces: `pub(crate) fn selected_message_row(&self) -> Option<&MessageRow>`; `pub fn respond_meeting(&mut self, kind: RsvpKind)`. Sets `App::attachment_notice` for the confirmation (reuses the existing transient-notice field the reader already renders).

- [ ] **Step 1: Confirm the `RsvpKind` import in app.rs**

Check the top of `lookxy/src/app.rs` for how `RsvpKind` is referenced (calendar RSVP already uses it). If it is not already imported, add it to the existing `mailcore::graph::client` use (e.g. `use mailcore::graph::client::RsvpKind;` alongside the existing client imports). Do not duplicate an import that is already present.

- [ ] **Step 2: Write the failing app tests**

Add to the `tests` module in `lookxy/src/app.rs`. Helper seeds a meeting-invite message into the seeded fixture's inbox and opens it:

```rust
    fn open_meeting_invite(app: &mut App) {
        use mailcore::graph::model::{Message, Recipient};
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "invite1".into(),
                    conversation_id: "c9".into(),
                    subject: "Sprint review".into(),
                    from: Recipient { name: "Boss".into(), address: "boss@x".into() },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-18T10:00:00Z".into(),
                    sent: "2026-07-18T09:00:00Z".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "invite".into(),
                    is_draft: false,
                    is_meeting_request: true,
                },
            )
            .expect("seed invite");
        app.reload_messages();
        app.open_message("invite1");
    }

    #[test]
    fn respond_meeting_on_an_invite_sends_command() {
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app);
        app.respond_meeting(RsvpKind::Accept);
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::RespondMeeting { message_id, kind }) => {
                assert_eq!(message_id, "invite1");
                assert_eq!(kind, RsvpKind::Accept);
            }
            other => panic!("expected RespondMeeting, got {other:?}"),
        }
    }

    #[test]
    fn respond_meeting_on_ordinary_mail_is_a_noop() {
        let mut app = App::for_test_with_seeded_store();
        app.open_message("m1"); // m1 is an ordinary message
        app.respond_meeting(RsvpKind::Accept);
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err()); // nothing sent
    }

    #[test]
    fn uppercase_a_d_t_route_to_respond_meeting_only_for_invites() {
        // Ordinary mail: A/D/T send nothing.
        let mut app = App::for_test_with_seeded_store();
        app.open_message("m1");
        app.on_key_char('A');
        app.on_key_char('D');
        app.on_key_char('T');
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());

        // Invite: each maps to the matching RsvpKind.
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app);
        app.on_key_char('D');
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::RespondMeeting { kind, .. }) => assert_eq!(kind, RsvpKind::Decline),
            other => panic!("expected RespondMeeting Decline, got {other:?}"),
        }
    }

    #[test]
    fn meeting_responded_notice_and_marks_read() {
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app);
        app.on_sync_event(SyncEvent::MeetingResponded {
            message_id: "invite1".into(),
            kind: RsvpKind::Tentative,
        });
        assert_eq!(
            app.attachment_notice.as_deref(),
            Some("Tentatively accepted the invite")
        );
        // Marked read locally.
        let rows = app.store.messages_in_folder("inbox", 50, 0).unwrap();
        assert!(rows.iter().find(|m| m.id == "invite1").unwrap().is_read);
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `bash "$LCARGO" test -p lookxy respond_meeting` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: FAIL — no method `respond_meeting` / no `SyncEvent::MeetingResponded` arm.

- [ ] **Step 4: Add the accessor and `respond_meeting`**

In `lookxy/src/app.rs`, add these methods inside `impl App` (place near `open_message`/`mark_read`):

```rust
    /// The currently-open (`selected_msg`) message's row, resolved from the
    /// loaded flat list or, in threaded mode, the built threads. `None` when
    /// nothing is open or that row isn't loaded. Shared by the reader's
    /// meeting banner and the RSVP-key guard so both agree on what's open.
    pub(crate) fn selected_message_row(&self) -> Option<&MessageRow> {
        let id = self.selected_msg.as_deref()?;
        if let Some(m) = self.messages.iter().find(|m| m.id == id) {
            return Some(m);
        }
        self.threads
            .iter()
            .flat_map(|tv| tv.thread.messages.iter())
            .find(|m| m.id == id)
    }

    /// RSVP to the opened meeting-invite email: no-op unless the opened
    /// message is a meeting request (so `A`/`D`/`T` never act on ordinary
    /// mail). Sends `SyncCommand::RespondMeeting`; the confirmation + mark-read
    /// happen when `SyncEvent::MeetingResponded` lands (see `on_sync_event`).
    pub fn respond_meeting(&mut self, kind: RsvpKind) {
        let Some(message_id) = self
            .selected_message_row()
            .filter(|m| m.is_meeting_request)
            .map(|m| m.id.clone())
        else {
            return;
        };
        self.attachment_notice = Some("Responding…".to_string());
        let _ = self
            .sync
            .cmd_tx
            .send(SyncCommand::RespondMeeting { message_id, kind });
    }
```

- [ ] **Step 5: Route the uppercase keys**

In `on_key_char`, add arms before the `_ => {}` catch-all (lowercase `a`/`d`/`t` arms are untouched):

```rust
            't' => self.toggle_threaded(),
            'A' => self.respond_meeting(RsvpKind::Accept),
            'D' => self.respond_meeting(RsvpKind::Decline),
            'T' => self.respond_meeting(RsvpKind::Tentative),
            _ => {}
```

- [ ] **Step 6: Handle `MeetingResponded`**

In `on_sync_event`, add an arm (place it next to `AttachmentSaved`):

```rust
            SyncEvent::MeetingResponded { message_id, kind } => {
                if self.selected_msg.as_deref() == Some(message_id.as_str()) {
                    self.attachment_notice = Some(
                        match kind {
                            RsvpKind::Accept => "Accepted the invite",
                            RsvpKind::Decline => "Declined the invite",
                            RsvpKind::Tentative => "Tentatively accepted the invite",
                        }
                        .to_string(),
                    );
                }
                // Mark the invite read locally (a small courtesy) and push the
                // change to Graph via the existing read path.
                self.store.set_read(&message_id, true);
                self.reload_messages();
                let _ = self
                    .sync
                    .cmd_tx
                    .send(SyncCommand::MarkRead { id: message_id, read: true });
            }
```

- [ ] **Step 7: Run to verify it passes**

Run: `bash "$LCARGO" test -p lookxy respond_meeting meeting` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: PASS. If the `MeetingResponded` test also drains a trailing `MarkRead` command, that is fine — the assertions only inspect the first `RespondMeeting`/`try_recv` where relevant.

- [ ] **Step 8: Commit**

```bash
git add lookxy/src/app.rs
git commit -m "lookxy: respond_meeting sends RSVP for the opened invite via A/D/T"
```

---

### Task 5: Reading-pane meeting banner

Show a flag-driven banner between the headers and the body when the opened message is a meeting invite.

**Files:**
- Modify: `lookxy/src/ui/reading.rs` (banner in the fixed-header area; delegate `selected_message` to the app accessor; test)

**Interfaces:**
- Consumes: `App::selected_message_row` (Task 4), `MessageRow.is_meeting_request` (Task 1).
- Produces: the banner line `📅 Meeting invite — [A]ccept  [D]ecline  [T]entative` rendered in the header block for an invite; nothing for ordinary mail.

- [ ] **Step 1: Write the failing render test**

Add to the `tests` module in `lookxy/src/ui/reading.rs` (mirrors the existing `TestBackend` reader tests):

```rust
    #[test]
    fn renders_the_meeting_banner_for_an_invite_only() {
        use mailcore::graph::model::{Message, Recipient};
        let mut app = App::for_test_with_seeded_store();
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "invite1".into(),
                    conversation_id: "c9".into(),
                    subject: "Sprint review".into(),
                    from: Recipient { name: "Boss".into(), address: "boss@x".into() },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-18T10:00:00Z".into(),
                    sent: "2026-07-18T09:00:00Z".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "invite".into(),
                    is_draft: false,
                    is_meeting_request: true,
                },
            )
            .expect("seed invite");
        app.reload_messages();

        // Invite → banner present.
        app.open_message("invite1");
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &mut app, f.area())).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Meeting invite"));
        assert!(text.contains("[A]ccept"));

        // Ordinary message → no banner.
        app.open_message("m1");
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &mut app, f.area())).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("Meeting invite"));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p lookxy renders_the_meeting_banner` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: FAIL — the banner text isn't rendered.

- [ ] **Step 3: Delegate `selected_message` to the app accessor**

In `lookxy/src/ui/reading.rs`, replace the free `selected_message` function body so both the header and banner resolve the opened row the same way the RSVP guard does:

```rust
/// The message named by `App::selected_msg`, resolved via the app's shared
/// accessor (flat list or, in threaded mode, the built threads).
fn selected_message(app: &App) -> Option<&MessageRow> {
    app.selected_message_row()
}
```

- [ ] **Step 4: Add the banner to the header block**

In `draw`, replace the header construction:

```rust
    // Fixed header (From/Subject/Received + blank), then the scrolling body.
    let header = header_lines(m);
```

with:

```rust
    // Fixed header (From/Subject/Received, an optional meeting-invite banner,
    // + blank), then the scrolling body.
    let mut header = header_lines(m);
    if m.is_meeting_request {
        header.push(Line::from("📅 Meeting invite — [A]ccept  [D]ecline  [T]entative"));
    }
```

(`header_h` already derives from `header.len()`, so the extra line is accounted for; the banner scrolls with the fixed header block, not the body.)

- [ ] **Step 5: Run to verify it passes**

Run: `bash "$LCARGO" test -p lookxy renders_the_meeting_banner` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 6: Full workspace test + clippy**

Run: `bash "$LCARGO" test --workspace` then `bash "$LCARGO" clippy --workspace --all-targets` (Bash tool, `dangerouslyDisableSandbox: true`)
Expected: all tests pass; clippy clean.

- [ ] **Step 7: Commit**

```bash
git add lookxy/src/ui/reading.rs
git commit -m "lookxy: reader shows a meeting-invite RSVP banner for invites"
```

---

## Self-Review

**Spec coverage:**
- Detect (`Message.is_meeting_request` from `@odata.type`, store column + migration) → Task 1. ✅
- Reader banner (flag-driven, in the fixed-header area) → Task 5. ✅
- Resolve event id (`meeting_event_id` via `$expand`) → Task 2. ✅
- Reuse `respond_event` → Task 3 (`respond_meeting` handler). ✅
- `SyncCommand::RespondMeeting` / `SyncEvent::MeetingResponded` → Task 3. ✅
- `App::respond_meeting` guarded on the opened message being an invite → Task 4. ✅
- Uppercase `A`/`D`/`T` routed + guarded; lowercase unchanged → Task 4. ✅
- Notice + mark-read on success → Task 4 (`on_sync_event`). ✅
- Error handling: not-an-invite → `Error("not a meeting invite")`; Graph failure → `react`; response/cancellation messages → flag stays false (Task 1's `from_json`, verified by the `eventMessageResponse` case). ✅
- Always `send_response = true`, no comment → Task 3 (`respond_event(&event_id, kind, None, true)`). ✅

**Placeholder scan:** No TBD/TODO. Every code step shows complete code; the one mechanical ripple (Task 1 Step 8) names each file and the exact field/value to add, with the compiler as the enumerator — acceptable for a pure struct-literal fan-out.

**Type consistency:** `is_meeting_request: bool` on both `Message` and `MessageRow`; `RespondMeeting { message_id: String, kind: RsvpKind }` and `MeetingResponded { message_id: String, kind: RsvpKind }` match between engine definition (Task 3) and app usage (Task 4); `meeting_event_id(&self, &str) -> Result<Option<String>, GraphError>` consistent across Tasks 2–3; `selected_message_row(&self) -> Option<&MessageRow>` consistent across Tasks 4–5. `RsvpKind` is `Copy` (existing derive), so passing `kind` by value into both `with_auth` closures and the notice `match` is fine.
