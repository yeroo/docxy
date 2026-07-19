# lookxy Categories (Color Labels) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Assign/clear Outlook categories on a message, show them as colored dots in the list and colored chips in the reader, and filter a folder to one category.

**Architecture:** `Message.categories` (name list) + a `MasterCategory` list (name→color) flow through model → store → client → sync (an outbox-backed `SetCategories`, plus a best-effort master-list fetch). The lookxy UI renders colored dots/chips via a `preset→Color` map and a one-popup-two-modes category picker (`l` assign, `L` filter).

**Tech Stack:** Rust (edition 2024), hand-rolled `mailcore::json`, `ureq` Graph client, `rusqlite` store, `ratatui`/`crossterm` TUI, `std::sync::mpsc` engine channels.

## Global Constraints

- **Build wrapper:** never call bare `cargo` (fails os error 448). Use `bash "$LCARGO" <args>` where `LCARGO=C:/Users/BORIS_~1/AppData/Local/Temp/claude/C--Users-boris-kudriashov-Source-docxy/1da9a016-b606-4432-8951-6d73bb91c967/scratchpad/lcargo.sh`. EVERY Bash tool call that runs it MUST set `dangerouslyDisableSandbox: true`.
- **MSRV / edition:** edition 2024, MSRV 1.88. No new external crates.
- **No serde:** JSON only via `mailcore::json` (`Value`, `parse`, `.get`/`.as_str`/`.as_bool`/`.as_array`/`Object`/`Str`/`Array`, `Value::to_string`).
- **Keys:** **`l`** opens the picker in Assign mode; **`L`** in Filter mode. Both Mail-mode only (reached via `on_key_char`).
- **List display:** one colored `●` per category before the subject. **Reader:** a `Categories: [name] …` header line. **Colors:** `preset0..preset24` → best-effort ratatui named colors; `"none"`/unknown → `Color::Gray`.
- **Outbox-backed writes:** `SetCategories` is optimistic-local + queued Graph op, same as `MarkRead`/`SetFlag`.
- **Category name encoding:** the store column joins names with the ASCII Unit Separator `\u{1f}` (stripped from names on encode); no serde, lossless for real names.
- **Secrets:** never log tokens/bodies.

---

### Task 1: Per-message categories through model + store

**Files:**
- Modify: `mailcore/src/graph/model.rs` (`Message.categories` + `from_json`; test)
- Modify: `mailcore/src/graph/client.rs` (`MESSAGE_SELECT`)
- Modify: `mailcore/src/store/schema.rs` (`messages.categories` column)
- Modify: `mailcore/src/store/mod.rs` (`MessageRow.categories`; migration; `encode_categories`/`decode_categories`; `upsert_message`; 4 SELECTs; `map_message_row`; `set_categories`; `msg` helper; tests)
- Modify (literal ripple): `mailcore/src/sync/outbox.rs`, `mailcore/src/thread.rs`, `lookxy/src/app.rs`, `lookxy/src/ui/mod.rs`, `lookxy/src/ui/search.rs`, `lookxy/src/ui/attachments.rs`, `lookxy/src/control.rs`

**Interfaces:**
- Produces: `Message.categories: Vec<String>`; `MessageRow.categories: Vec<String>`; `Store::set_categories(&self, id: &str, categories: &[String])`; module-private `encode_categories(&[String])->String` / `decode_categories(&str)->Vec<String>`.

- [ ] **Step 1: Write the failing model test**

Add to the `tests` module in `mailcore/src/graph/model.rs`:

```rust
    #[test]
    fn message_parses_categories() {
        let with = parse(
            r#"{"id":"M1","conversationId":"C","subject":"s","from":{"emailAddress":{"name":"","address":""}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":"","categories":["Work","Urgent"]}"#,
        )
        .unwrap();
        assert_eq!(
            Message::from_json(&with).unwrap().categories,
            vec!["Work".to_string(), "Urgent".to_string()]
        );
        let without = parse(
            r#"{"id":"M2","conversationId":"C","subject":"s","from":{"emailAddress":{"name":"","address":""}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":""}"#,
        )
        .unwrap();
        assert!(Message::from_json(&without).unwrap().categories.is_empty());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p mailcore message_parses_categories` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — `Message` has no `categories`.

- [ ] **Step 3: Add the model field + parse + select**

In `mailcore/src/graph/model.rs`, add to `Message` (after `is_meeting_request`):

```rust
    pub is_meeting_request: bool,
    /// Graph `categories`: the message's assigned category names (color labels).
    /// Colors live separately in the master category list (`MasterCategory`).
    pub categories: Vec<String>,
```

In `Message::from_json`, after the `is_meeting_request` line:

```rust
            categories: v
                .get("categories")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
```

In `mailcore/src/graph/client.rs`, append `categories` to `MESSAGE_SELECT`:

```rust
const MESSAGE_SELECT: &str = "id,conversationId,subject,from,toRecipients,ccRecipients,\
receivedDateTime,sentDateTime,isRead,flag,hasAttachments,importance,bodyPreview,categories";
```

- [ ] **Step 4: Add the store column, MessageRow field, migration, encoders**

In `mailcore/src/store/schema.rs`, extend the `messages` table's tail (currently ends `is_meeting_request INTEGER NOT NULL DEFAULT 0`):

```sql
    is_draft        INTEGER NOT NULL DEFAULT 0,
    is_meeting_request INTEGER NOT NULL DEFAULT 0,
    categories      TEXT NOT NULL DEFAULT ''
);
```

In `mailcore/src/store/mod.rs`, add to `MessageRow` (after `is_meeting_request`):

```rust
    pub is_meeting_request: bool,
    /// Assigned category names (color labels). Empty when none.
    pub categories: Vec<String>,
}
```

Add the idempotent migration in `Store::init`, after the `is_meeting_request` ALTER:

```rust
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN categories TEXT NOT NULL DEFAULT ''",
            [],
        );
```

Add the encoders near `encode_recipients` (bottom of the file):

```rust
/// Encodes a message's category names into the flat `messages.categories`
/// column: names joined by the ASCII Unit Separator (`\u{1f}`), which is
/// stripped from any name first (a real Outlook category name never contains a
/// control char, so this is lossless in practice). An empty list encodes to
/// `""`.
fn encode_categories(cats: &[String]) -> String {
    cats.iter()
        .map(|c| c.replace('\u{1f}', " "))
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

/// Inverse of `encode_categories`: splits on `\u{1f}`. An empty string decodes
/// to an empty list (not `[""]`).
fn decode_categories(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split('\u{1f}').map(str::to_string).collect()
}
```

- [ ] **Step 5: Persist + read the column, add `set_categories`**

In `upsert_message`, extend the column list, placeholders, conflict, and params. Change:

```rust
                 is_read, is_flagged, has_attachments, importance, preview, is_draft,
                 is_meeting_request
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
```

to:

```rust
                 is_read, is_flagged, has_attachments, importance, preview, is_draft,
                 is_meeting_request, categories
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
```

Change the conflict tail from:

```rust
                 is_meeting_request = excluded.is_meeting_request",
```

to:

```rust
                 is_meeting_request = excluded.is_meeting_request,
                 categories = excluded.categories",
```

Add the param after `m.is_meeting_request,`:

```rust
                m.is_meeting_request,
                encode_categories(&m.categories),
            ],
```

In `map_message_row`, after `is_meeting_request: row.get(17)?,`:

```rust
        is_meeting_request: row.get(17)?,
        categories: decode_categories(&row.get::<_, String>(18)?),
    })
```

In EACH of the four SELECTs feeding `map_message_row` (`messages_in_folder`, `conversations_in_folder` inner CTE + outer `k.` list, `draft`, `search`), append `categories` (and `k.categories`) after `is_meeting_request` so it lands at index 18. Change the three flat selects' tail:

```sql
                    bcc_recipients, is_meeting_request
```

to:

```sql
                    bcc_recipients, is_meeting_request, categories
```

For `conversations_in_folder`'s inner `keyed` CTE change `bcc_recipients, is_meeting_request,` → `bcc_recipients, is_meeting_request, categories,`; and its outer select `k.bcc_recipients, k.is_meeting_request` → `k.bcc_recipients, k.is_meeting_request, k.categories`.

Add `set_categories` next to `set_flag`:

```rust
    /// Locally sets a message's category names (the optimistic half of
    /// `SyncCommand::SetCategories`). See `set_read` for why this doesn't
    /// return a `Result`.
    pub fn set_categories(&self, id: &str, categories: &[String]) {
        let _ = self.conn.execute(
            "UPDATE messages SET categories = ?1 WHERE id = ?2",
            params![encode_categories(categories), id],
        );
    }
```

- [ ] **Step 6: Write the failing store tests + fix the `msg` helper**

In `mailcore/src/store/mod.rs`, add `categories: Vec::new(),` to the `msg` test helper (after `is_meeting_request: false,`). Then add:

```rust
    #[test]
    fn message_round_trips_categories_including_delimiter_names() {
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
        m.categories = vec!["Work".into(), "A\u{1f}B".into()]; // embedded separator
        s.upsert_message("F", &m).unwrap();
        let rows = s.messages_in_folder("F", 10, 0).unwrap();
        // The embedded separator was neutralized to a space, so the list stays 2.
        assert_eq!(rows[0].categories, vec!["Work".to_string(), "A B".to_string()]);
    }

    #[test]
    fn set_categories_updates_only_that_column() {
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
        let m = msg("01", true);
        s.upsert_message("F", &m).unwrap();
        s.set_categories("01", &["Urgent".to_string()]);
        let rows = s.messages_in_folder("F", 10, 0).unwrap();
        assert_eq!(rows[0].categories, vec!["Urgent".to_string()]);
        assert!(rows[0].is_read); // untouched
    }
```

- [ ] **Step 7: Build all targets, fix the literal ripple**

Run: `bash "$LCARGO" build --workspace --all-targets 2>&1 | grep -E "^error|missing field|-->"` (Bash, `dangerouslyDisableSandbox: true`)

Add `categories: Vec::new(),` to every `Message { … }` and `MessageRow { … }` literal the compiler flags (all fixtures have no categories). The literals close with `is_meeting_request: <bool>,` immediately before `}`, so this one-liner inserts it (run from the worktree root):

```bash
perl -0777 -i -pe 's/^(\s*)is_meeting_request: (true|false),\n(\s*\},?)$/$1is_meeting_request: $2,\n$1categories: Vec::new(),\n$3/mg' \
  mailcore/src/sync/outbox.rs mailcore/src/thread.rs lookxy/src/app.rs \
  lookxy/src/ui/mod.rs lookxy/src/ui/search.rs lookxy/src/ui/attachments.rs lookxy/src/control.rs
```

Then re-run the build; hand-fix any literal the perl didn't reach (e.g. a differently-formatted one). Note: the model/store test helpers were already handled in Steps 3/6.

Run: `bash "$LCARGO" build --workspace --all-targets` — expect a clean build.

- [ ] **Step 8: Run the tests**

Run: `bash "$LCARGO" test -p mailcore` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS incl. the 3 new tests (parse, round-trip, set).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "mailcore: carry per-message categories through model + store"
```

---

### Task 2: Master category list — model + store

**Files:**
- Modify: `mailcore/src/graph/model.rs` (`MasterCategory` + `from_json`; test)
- Modify: `mailcore/src/store/schema.rs` (`master_categories` table)
- Modify: `mailcore/src/store/mod.rs` (`replace_master_categories`, `master_categories`; tests)

**Interfaces:**
- Produces: `pub struct MasterCategory { display_name: String, color: String }` (+ `from_json`); `Store::replace_master_categories(&self, cats: &[MasterCategory]) -> Result<(), StoreError>`; `Store::master_categories(&self) -> Result<Vec<MasterCategory>, StoreError>`.

- [ ] **Step 1: Write the failing model test**

Add to the `tests` module in `mailcore/src/graph/model.rs`:

```rust
    #[test]
    fn master_category_parses_name_and_color() {
        let v = parse(r#"{"id":"c1","displayName":"Work","color":"preset0"}"#).unwrap();
        let c = MasterCategory::from_json(&v).unwrap();
        assert_eq!(c.display_name, "Work");
        assert_eq!(c.color, "preset0");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p mailcore master_category_parses` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — no `MasterCategory`.

- [ ] **Step 3: Add the model**

In `mailcore/src/graph/model.rs` (near the `Message` types):

```rust
/// One entry of the mailbox's master category list (Graph `outlookCategory`):
/// a category's display name and its `color` (`"preset0"`…`"preset24"` or
/// `"none"`). The UI maps `color` to a terminal color; the name is what a
/// message's `categories` list references.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterCategory {
    pub display_name: String,
    pub color: String,
}

impl MasterCategory {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(MasterCategory {
            display_name: str_field(v, "displayName"),
            color: str_field(v, "color"),
        })
    }
}
```

- [ ] **Step 4: Add the table + store methods**

In `mailcore/src/store/schema.rs`, add (a new `CREATE TABLE IF NOT EXISTS` — runs on every `open`, so existing DBs get it with no ALTER):

```sql
CREATE TABLE IF NOT EXISTS master_categories (
    display_name TEXT PRIMARY KEY,
    color        TEXT NOT NULL DEFAULT ''
);
```

In `mailcore/src/store/mod.rs`, add (import `MasterCategory` — extend the existing `use crate::graph::model::{…}` at the top of the file to include `MasterCategory`):

```rust
    /// Replaces the whole master category list in one transaction (Graph's
    /// `/me/outlook/masterCategories` is not a delta endpoint — each fetch is
    /// the full set, so replacing prunes deleted categories).
    pub fn replace_master_categories(&self, cats: &[MasterCategory]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM master_categories", [])?;
        for c in cats {
            tx.execute(
                "INSERT OR REPLACE INTO master_categories (display_name, color) VALUES (?1, ?2)",
                params![c.display_name, c.color],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The master category list, ordered by display name.
    pub fn master_categories(&self) -> Result<Vec<MasterCategory>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT display_name, color FROM master_categories ORDER BY display_name")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(MasterCategory {
                    display_name: row.get(0)?,
                    color: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
```

(If `Store` uses `self.conn.transaction()` elsewhere rather than `unchecked_transaction`, match whichever the file already uses — grep `transaction(` in `store/mod.rs`.)

- [ ] **Step 5: Write the failing store test**

```rust
    #[test]
    fn master_categories_replace_and_read_round_trip() {
        use crate::graph::model::MasterCategory;
        let s = Store::open_in_memory().unwrap();
        s.replace_master_categories(&[
            MasterCategory { display_name: "Work".into(), color: "preset0".into() },
            MasterCategory { display_name: "Urgent".into(), color: "preset1".into() },
        ])
        .unwrap();
        let got = s.master_categories().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].display_name, "Urgent"); // ordered by name
        assert_eq!(got[1].color, "preset0");
        // Replace prunes the old set.
        s.replace_master_categories(&[MasterCategory {
            display_name: "Only".into(),
            color: "none".into(),
        }])
        .unwrap();
        let got = s.master_categories().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].display_name, "Only");
    }
```

- [ ] **Step 6: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore master_categ` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS (model + store tests).

- [ ] **Step 7: Commit**

```bash
git add mailcore/src/graph/model.rs mailcore/src/store/schema.rs mailcore/src/store/mod.rs
git commit -m "mailcore: MasterCategory model + master_categories store table"
```

---

### Task 3: Graph client — master list + set categories

**Files:**
- Modify: `mailcore/src/graph/client.rs` (import, two methods; tests)

**Interfaces:**
- Consumes: `MasterCategory` (Task 2), `send`, `parse_body`, `value_array`, `encode_path_segment`, `Value`.
- Produces: `pub fn get_master_categories(&self) -> Result<Vec<MasterCategory>, GraphError>`; `pub fn set_message_categories(&self, id: &str, categories: &[String]) -> Result<(), GraphError>`.

- [ ] **Step 1: Extend the model import**

In `mailcore/src/graph/client.rs`, add `MasterCategory` to the `use crate::graph::model::{…}` list.

- [ ] **Step 2: Write the failing tests**

Add to the client `tests` module:

```rust
    #[test]
    fn get_master_categories_parses_value_array() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/outlook/masterCategories".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[{"id":"c1","displayName":"Work","color":"preset0"},{"id":"c2","displayName":"Urgent","color":"preset1"}]}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let cats = c.get_master_categories().unwrap();
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].display_name, "Work");
        assert_eq!(cats[1].color, "preset1");
    }

    #[test]
    fn set_message_categories_patches_array() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.set_message_categories("M1", &["Work".to_string(), "Urgent".to_string()])
            .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        let sent = json::parse(&reqs[0].body).unwrap();
        let arr = sent.get("categories").and_then(Value::as_array).unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str(), Some("Work"));
    }
```

- [ ] **Step 3: Run to verify they fail**

Run: `bash "$LCARGO" test -p mailcore master_categories set_message_categories` — (single filter) run `bash "$LCARGO" test -p mailcore _categories` (Bash, `dangerouslyDisableSandbox: true`)
Expected: FAIL — methods don't exist.

- [ ] **Step 4: Implement the methods**

In `mailcore/src/graph/client.rs`, inside `impl GraphClient` (near `set_flag`):

```rust
    /// GET `/me/outlook/masterCategories` — the mailbox's master category list.
    pub fn get_master_categories(&self) -> Result<Vec<MasterCategory>, GraphError> {
        let resp = self.send(Method::Get, "/me/outlook/masterCategories", None, &[])?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        Ok(items.iter().filter_map(MasterCategory::from_json).collect())
    }

    /// PATCH `/me/messages/{id}` with `{"categories": [ …names… ]}` — sets the
    /// message's assigned category names (an empty slice clears them).
    pub fn set_message_categories(
        &self,
        id: &str,
        categories: &[String],
    ) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/messages/{id}");
        let arr = Value::Array(categories.iter().map(|c| Value::Str(c.clone())).collect());
        let body = Value::Object(vec![("categories".to_string(), arr)]).to_string();
        self.send(Method::Patch, &path, Some(body), &[])?;
        Ok(())
    }
```

- [ ] **Step 5: Run to verify pass**

Run: `bash "$LCARGO" test -p mailcore _categories` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mailcore/src/graph/client.rs
git commit -m "mailcore: client get_master_categories + set_message_categories"
```

---

### Task 4: Sync — SetCategories outbox op + master-list fetch

**Files:**
- Modify: `mailcore/src/store/mod.rs` (`OutboxOp::SetCategories`: variant, `kind`, `to_json`, `from_json`)
- Modify: `mailcore/src/sync/outbox.rs` (`apply_op` arm; test)
- Modify: `mailcore/src/sync/engine.rs` (`SyncCommand::SetCategories`/`RefreshCategories`, `SyncEvent::CategoriesUpdated`, dispatch, `refresh_master_categories`, full-pass call; tests)

**Interfaces:**
- Consumes: `Store::set_categories` (Task 1), `Store::replace_master_categories` (Task 2), `GraphClient::set_message_categories`/`get_master_categories` (Task 3).
- Produces: `OutboxOp::SetCategories { id: String, categories: Vec<String> }`; `SyncCommand::SetCategories { id: String, categories: Vec<String> }`; `SyncCommand::RefreshCategories`; `SyncEvent::CategoriesUpdated`.

- [ ] **Step 1: Add the outbox op**

In `mailcore/src/store/mod.rs` `OutboxOp` (after `SetFlag`):

```rust
    SetFlag {
        id: String,
        flagged: bool,
    },
    /// Push a message's assigned category names to Graph
    /// (`client.set_message_categories`).
    SetCategories {
        id: String,
        categories: Vec<String>,
    },
```

In `kind`, after the `SetFlag` arm: `OutboxOp::SetCategories { .. } => "setCategories",`.

In `to_json`, add an arm:

```rust
            OutboxOp::SetCategories { id, categories } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                (
                    "categories".to_string(),
                    Value::Array(categories.iter().map(|c| Value::Str(c.clone())).collect()),
                ),
            ]),
```

In `from_json`, after the `"setFlag"` arm:

```rust
            "setCategories" => Some(OutboxOp::SetCategories {
                id: id()?,
                categories: v
                    .get("categories")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
```

- [ ] **Step 2: Write the failing apply_op test**

In `mailcore/src/sync/outbox.rs` tests (mirror `apply_op_dispatches_set_flag`):

```rust
    #[test]
    fn apply_op_dispatches_set_categories() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let client = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &client,
            &store,
            &OutboxOp::SetCategories {
                id: "M1".into(),
                categories: vec!["Work".into()],
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        assert!(reqs[0].path.contains("/me/messages/M1"));
    }
```

- [ ] **Step 3: Add the apply_op arm**

In `mailcore/src/sync/outbox.rs` `apply_op`, after the `SetFlag` arm:

```rust
        OutboxOp::SetCategories { id, categories } => {
            client.set_message_categories(id, categories)
        }
```

- [ ] **Step 4: Add the sync command/event + handlers**

In `mailcore/src/sync/engine.rs`:

`SyncCommand` (after `SetFlag`):

```rust
    SetFlag { id: String, flagged: bool },
    /// Set a message's category names (optimistic local + queued Graph op).
    SetCategories { id: String, categories: Vec<String> },
    /// Fetch the master category list (`GraphClient::get_master_categories`),
    /// store it, and emit [`SyncEvent::CategoriesUpdated`]. Direct call.
    RefreshCategories,
```

`SyncEvent` (after `AutomaticRepliesUpdated`, or anywhere in the enum):

```rust
    /// The master category list changed; the UI re-reads `Store::master_categories`.
    CategoriesUpdated,
```

`handle_command` (after the `SetFlag` arm):

```rust
            SyncCommand::SetCategories { id, categories } => {
                self.store.set_categories(&id, &categories);
                self.enqueue_and_drain(OutboxOp::SetCategories { id, categories });
            }
            SyncCommand::RefreshCategories => self.refresh_master_categories(),
```

Add the handler inside `impl Engine` (near `sync_people`):

```rust
    /// Best-effort master-category fetch → store + `CategoriesUpdated`. Any
    /// failure (scope/offline/parse) is swallowed like `sync_people`: category
    /// colors are a display bonus, never a hard dependency, so this never fails
    /// a sync pass. Also called on demand via `SyncCommand::RefreshCategories`.
    fn refresh_master_categories(&mut self) {
        if self.token.is_none() {
            return;
        }
        let cats = match self.with_auth(|c| c.get_master_categories()) {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = self.store.replace_master_categories(&cats);
        self.emit(SyncEvent::CategoriesUpdated);
    }
```

Call it on the full pass — in `sync_pass`, in the `if include_folders { self.sync_people(); }` block, add `self.refresh_master_categories();` right after `self.sync_people();`.

- [ ] **Step 5: Write the failing engine test**

In the engine `tests` module (mirror the OOF fetch test):

```rust
    #[test]
    fn refresh_categories_stores_and_emits() {
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/outlook/masterCategories".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"c1","displayName":"Work","color":"preset0"}]}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir("refresh-cats");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);
        let handle = spawn_with_bases(
            store_path.clone(),
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
        handle.cmd_tx.send(SyncCommand::RefreshCategories).unwrap();
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::CategoriesUpdated));
        let store = Store::open(&store_path).unwrap();
        assert_eq!(store.master_categories().unwrap()[0].display_name, "Work");
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_categories_optimistically_writes_and_drains() {
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/messages/M1".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir("set-cats");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);
        let handle = spawn_with_bases(
            store_path.clone(),
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
            .send(SyncCommand::SetCategories {
                id: "M1".into(),
                categories: vec!["Work".into()],
            })
            .unwrap();
        // Wait for the drain to PATCH.
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::State(_)));
        // Give the store a moment via a Refresh round-trip, then assert local write.
        let store = Store::open(&store_path).unwrap();
        let rows = store.messages_in_folder("F1", 50, 0).unwrap();
        assert_eq!(
            rows.iter().find(|m| m.id == "M1").unwrap().categories,
            vec!["Work".to_string()]
        );
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 6: Run to verify (fail → pass)**

Run: `bash "$LCARGO" test -p mailcore -- refresh_categories set_categories apply_op_dispatches_set_categories` — (single filter) run these three via `bash "$LCARGO" test -p mailcore categories` then `bash "$LCARGO" test -p mailcore set_categories` (Bash, `dangerouslyDisableSandbox: true`)
Expected: after Steps 1–4, PASS. Then run the full `bash "$LCARGO" test -p mailcore` — all green (new `OutboxOp` variant is now exhaustively matched everywhere).

- [ ] **Step 7: Commit**

```bash
git add mailcore/src/store/mod.rs mailcore/src/sync/outbox.rs mailcore/src/sync/engine.rs
git commit -m "mailcore: SetCategories outbox op + RefreshCategories master fetch"
```

---

### Task 5: Display — dots, chips, color map

**Files:**
- Create: `lookxy/src/ui/categories.rs` (`preset_color`, `color_for`)
- Modify: `lookxy/src/ui/mod.rs` (`pub mod categories;`)
- Modify: `lookxy/src/app.rs` (`master_categories` field + `reload_master_categories` + `CategoriesUpdated` handling + startup load; test)
- Modify: `lookxy/src/ui/message_list.rs` (category dots in rows; test)
- Modify: `lookxy/src/ui/reading.rs` (Categories chip line; test)

**Interfaces:**
- Consumes: `MasterCategory` (Task 2), `SyncEvent::CategoriesUpdated` (Task 4), `MessageRow.categories` (Task 1).
- Produces: `ui::categories::preset_color(&str) -> Color`; `ui::categories::color_for(cats: &[MasterCategory], name: &str) -> Color`; `App::master_categories: Vec<MasterCategory>`; `App::reload_master_categories`.

- [ ] **Step 1: Create the color helper + test**

Create `lookxy/src/ui/categories.rs`:

```rust
//! Category color mapping: Graph `outlookCategory.color` presets → terminal
//! colors, and a name→color lookup over the master category list. Presentation
//! only — the master list itself lives in the store (`mailcore`).

use mailcore::graph::model::MasterCategory;
use ratatui::style::Color;

/// Maps a Graph category color (`"preset0"`…`"preset24"`, or `"none"`) to a
/// best-effort terminal color. Unknown / `"none"` → `Color::Gray`.
pub fn preset_color(preset: &str) -> Color {
    match preset {
        "preset0" => Color::Red,
        "preset1" => Color::LightRed,
        "preset2" => Color::Yellow,
        "preset3" => Color::LightYellow,
        "preset4" => Color::Green,
        "preset5" => Color::Cyan,
        "preset6" => Color::LightGreen,
        "preset7" => Color::Blue,
        "preset8" => Color::Magenta,
        "preset9" => Color::LightMagenta,
        "preset10" => Color::LightBlue,
        "preset11" => Color::LightCyan,
        "preset12" => Color::Gray,
        "preset13" => Color::DarkGray,
        "preset14" => Color::White,
        "preset15" => Color::Red,
        "preset16" => Color::Yellow,
        "preset17" => Color::LightRed,
        "preset18" => Color::LightYellow,
        "preset19" => Color::Green,
        "preset20" => Color::Cyan,
        "preset21" => Color::LightGreen,
        "preset22" => Color::Blue,
        "preset23" => Color::Magenta,
        "preset24" => Color::LightMagenta,
        _ => Color::Gray,
    }
}

/// The color for a category `name`, looked up in the master list; a name not in
/// the list (deleted, or shared-mailbox) falls back to `Color::Gray`.
pub fn color_for(master: &[MasterCategory], name: &str) -> Color {
    master
        .iter()
        .find(|c| c.display_name == name)
        .map(|c| preset_color(&c.color))
        .unwrap_or(Color::Gray)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_and_lookup() {
        assert_eq!(preset_color("preset0"), Color::Red);
        assert_eq!(preset_color("none"), Color::Gray);
        assert_eq!(preset_color("bogus"), Color::Gray);
        let master = vec![MasterCategory {
            display_name: "Work".into(),
            color: "preset4".into(),
        }];
        assert_eq!(color_for(&master, "Work"), Color::Green);
        assert_eq!(color_for(&master, "Missing"), Color::Gray); // fallback
    }
}
```

In `lookxy/src/ui/mod.rs`, add `pub mod categories;` alongside the other module declarations.

- [ ] **Step 2: Run the color test**

Run: `bash "$LCARGO" test -p lookxy preset_and_lookup` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 3: Add `app.master_categories` + reload + event handling**

In `lookxy/src/app.rs`, import `MasterCategory` (extend the `use mailcore::graph::model::{…}`), add the field to `App` (near `folders`):

```rust
    /// The mailbox's master category list (name→color), for rendering category
    /// dots/chips and the picker's choices. Loaded from the store on
    /// `CategoriesUpdated` and at startup.
    pub master_categories: Vec<MasterCategory>,
```

Initialize in `App::new` (`master_categories: Vec::new(),`), and after `app.reload_account();` in `new`, add `app.reload_master_categories();`. Add the method (near `reload_folders`):

```rust
    /// Re-reads the master category list from the store (`Store::master_categories`).
    pub fn reload_master_categories(&mut self) {
        self.master_categories = self.store.master_categories().unwrap_or_default();
    }
```

In `on_sync_event`, add an arm:

```rust
            SyncEvent::CategoriesUpdated => self.reload_master_categories(),
```

- [ ] **Step 4: Write the failing list + reader tests**

In `lookxy/src/ui/message_list.rs` tests (create a `#[cfg(test)] mod tests` if none; import `App`, `TestBackend`):

```rust
#[cfg(test)]
mod tests {
    use crate::app::App;
    use mailcore::graph::model::{MasterCategory, Message, Recipient};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn message_row_shows_a_category_dot() {
        let mut app = App::for_test_with_seeded_store();
        app.master_categories = vec![MasterCategory {
            display_name: "Work".into(),
            color: "preset0".into(),
        }];
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "mc".into(),
                    conversation_id: "c1".into(),
                    subject: "Budget".into(),
                    from: Recipient { name: "Al".into(), address: "a@x".into() },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-19T10:00:00Z".into(),
                    sent: "".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "p".into(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: vec!["Work".into()],
                },
            )
            .unwrap();
        app.reload_messages();
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| crate::ui::message_list::draw(f, &app, f.area())).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains('●'));
    }
}
```

In `lookxy/src/ui/reading.rs` tests, add:

```rust
    #[test]
    fn reader_shows_category_chips() {
        use mailcore::graph::model::{MasterCategory, Message, Recipient};
        let mut app = App::for_test_with_seeded_store();
        app.master_categories = vec![MasterCategory { display_name: "Work".into(), color: "preset0".into() }];
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "mc".into(),
                    conversation_id: "c1".into(),
                    subject: "Budget".into(),
                    from: Recipient { name: "Al".into(), address: "a@x".into() },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-19T10:00:00Z".into(),
                    sent: "".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "p".into(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: vec!["Work".into()],
                },
            )
            .unwrap();
        app.reload_messages();
        app.open_message("mc");
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &mut app, f.area())).unwrap();
        let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Categories:"));
        assert!(text.contains("Work"));
    }
```

- [ ] **Step 5: Render dots in the list**

In `lookxy/src/ui/message_list.rs`, the row builders return `Line`. Add colored dots before the subject. The message-row builders (`child_line` and the shared flat `line`) build a single `Span`; change them to build a `Vec<Span>` where category dots (one per `m.categories`, colored via the app's master list) precede the text. Because `child_line`/`line` don't currently receive the master list, thread the app's `master_categories` slice through: change `fn line(m: &MessageRow, width: usize)` and `fn child_line(m: &MessageRow, indent: bool, width: usize)` to also take `master: &[MasterCategory]`, and pass `&app.master_categories` (or `&[]` from `draw_list`, which is shared with search — search rows can pass `&[]` for no dots, or thread the app list; pass `master` through `draw_list` too). Concretely, prepend to the row's spans:

```rust
    // Category dots: one ● per assigned category, colored via the master list.
    let mut spans: Vec<Span<'static>> = Vec::new();
    for name in &m.categories {
        spans.push(Span::styled(
            "●",
            Style::default().fg(crate::ui::categories::color_for(master, name)),
        ));
    }
    if !m.categories.is_empty() {
        spans.push(Span::raw(" "));
    }
    // …then the existing truncated text Span(s)…
    spans.push(Span::styled(truncated, style));
    Line::from(spans)
```

Ensure the width budget subtracts the dots (`m.categories.len() * 2` columns for `● ` groups) so text still truncates within the pane; keep it simple by truncating the text to `width.saturating_sub(dot_cols)`.

- [ ] **Step 6: Render chips in the reader**

In `lookxy/src/ui/reading.rs`, `header_lines(m)` returns `Vec<Line>`. It currently takes `&MessageRow`; thread the master list in (change its signature to `header_lines(m: &MessageRow, master: &[MasterCategory])` and pass `&app.master_categories` at the call site in `draw`). After the existing header lines, when `!m.categories.is_empty()`, push a `Categories:` line built from colored chips:

```rust
    if !m.categories.is_empty() {
        let mut spans: Vec<Span<'static>> = vec![Span::raw("Categories: ")];
        for name in &m.categories {
            spans.push(Span::styled(
                format!("[{name}] "),
                Style::default().fg(crate::ui::categories::color_for(master, name)),
            ));
        }
        lines.push(Line::from(spans));
    }
```

(Import `mailcore::graph::model::MasterCategory` and ratatui `Span`/`Style`/`Color` as needed; `header_lines` builds a `Vec<Line>` — make it `let mut lines = vec![…]; …; lines`.)

- [ ] **Step 7: Run the tests**

Run: `bash "$LCARGO" test -p lookxy -- message_row_shows_a_category_dot reader_shows_category_chips` — (single filter) `bash "$LCARGO" test -p lookxy category` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS. Fix any signature threading the compiler flags.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "lookxy: category dots in the list, chips in the reader, color map"
```

---

### Task 6: Category picker (assign + filter)

**Files:**
- Create: `lookxy/src/ui/categorypicker.rs` (`CategoryPicker`, `PickerMode`, `draw`, `handle_key`)
- Modify: `lookxy/src/ui/mod.rs` (`pub mod categorypicker;`, draw + key routing)
- Modify: `lookxy/src/app.rs` (`category_picker`/`category_filter` fields + `open_category_picker`, `apply_category_picker`, picker nav/toggle; `on_key_char` `l`/`L`; `reload_messages` filter; test)
- Modify: `lookxy/src/ui/status_bar.rs` (active-filter hint; optional)

**Interfaces:**
- Consumes: `App::master_categories` (Task 5), `SyncCommand::{SetCategories, RefreshCategories}` (Task 4), `MessageRow.categories` (Task 1), `ui::categories::color_for`.
- Produces: `CategoryPicker { mode, items, index }`, `PickerMode { Assign, Filter }`, `CategoryItem { name, color, selected }`; `App::category_picker: Option<CategoryPicker>`, `App::category_filter: Option<String>`.

- [ ] **Step 1: Create the picker module**

Create `lookxy/src/ui/categorypicker.rs`:

```rust
//! The category picker overlay — one popup, two modes. Assign mode toggles
//! categories on the highlighted message (`Space`) and applies on `Enter`;
//! Filter mode picks one category to filter the folder view by (`Enter`).
//! Opened by `l` (Assign) / `L` (Filter); see `App::open_category_picker`.

use crate::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerMode {
    Assign,
    Filter,
}

pub struct CategoryItem {
    pub name: String,
    pub color: Color,
    pub selected: bool,
}

pub struct CategoryPicker {
    pub mode: PickerMode,
    pub message_id: Option<String>, // the message being edited, in Assign mode
    pub items: Vec<CategoryItem>,
    pub index: usize,
}

/// Renders the picker overlay when open; a no-op otherwise.
pub fn draw(f: &mut Frame, app: &App) {
    let Some(p) = &app.category_picker else {
        return;
    };
    let area = centered(f.area(), 50, 60);
    f.render_widget(Clear, area);
    let title = match p.mode {
        PickerMode::Assign => "Categories (Space: toggle, Enter: apply, Esc: cancel)",
        PickerMode::Filter => "Filter by category (Enter: apply, Esc: cancel)",
    };
    let items: Vec<ListItem> = if p.items.is_empty() {
        vec![ListItem::new("(no categories — define them in Outlook)")]
    } else {
        p.items
            .iter()
            .map(|it| {
                let mark = match p.mode {
                    PickerMode::Assign => {
                        if it.selected {
                            "[x] "
                        } else {
                            "[ ] "
                        }
                    }
                    PickerMode::Filter => "",
                };
                ListItem::new(Line::from(vec![
                    Span::raw(mark),
                    Span::styled("● ", Style::default().fg(it.color)),
                    Span::raw(it.name.clone()),
                ]))
            })
            .collect()
    };
    let list = List::new(items)
        .block(Block::default().title(title).borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default();
    if !p.items.is_empty() {
        state.select(Some(p.index.min(p.items.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// A centered rect `pct_w`×`pct_h` percent of `area`.
fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_h) / 2),
            Constraint::Percentage(pct_h),
            Constraint::Percentage((100 - pct_h) / 2),
        ])
        .split(area)[1];
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_w) / 2),
            Constraint::Percentage(pct_w),
            Constraint::Percentage((100 - pct_w) / 2),
        ])
        .split(v)[1]
}

/// Key handling while the picker is open (routed ahead of the panes).
pub fn handle_key(app: &mut App, key: ratatui::crossterm::event::KeyEvent) {
    use ratatui::crossterm::event::KeyCode;
    match key.code {
        KeyCode::Esc => app.category_picker = None,
        KeyCode::Up | KeyCode::Char('k') => app.category_picker_select(-1),
        KeyCode::Down | KeyCode::Char('j') => app.category_picker_select(1),
        KeyCode::Char(' ') => app.category_picker_toggle(),
        KeyCode::Enter => app.apply_category_picker(),
        _ => {}
    }
}
```

- [ ] **Step 2: Add the app fields + methods**

In `lookxy/src/app.rs`, add fields to `App`:

```rust
    /// The category picker overlay (assign or filter), when open.
    pub category_picker: Option<crate::ui::categorypicker::CategoryPicker>,
    /// The active category filter (`L`), or `None`. When set, `reload_messages`
    /// shows only messages carrying this category (flat view).
    pub category_filter: Option<String>,
```

Initialize both `None` in `App::new`. Add methods (near `open_attachments_popup`):

```rust
    /// `l`: open the picker in Assign mode for the highlighted message, each
    /// category preselected iff the message already has it; also refresh the
    /// master list so the choices are current.
    pub fn open_category_picker(&mut self, mode: crate::ui::categorypicker::PickerMode) {
        use crate::ui::categorypicker::{CategoryItem, CategoryPicker};
        use crate::ui::categories::color_for;
        let _ = self.sync.cmd_tx.send(SyncCommand::RefreshCategories);
        let (message_id, current): (Option<String>, Vec<String>) = match mode {
            crate::ui::categorypicker::PickerMode::Assign => match self.highlighted_message_fields() {
                Some((id, _)) => {
                    let cats = self
                        .messages
                        .iter()
                        .find(|m| m.id == id)
                        .map(|m| m.categories.clone())
                        .or_else(|| {
                            self.threads
                                .iter()
                                .flat_map(|t| t.thread.messages.iter())
                                .find(|m| m.id == id)
                                .map(|m| m.categories.clone())
                        })
                        .unwrap_or_default();
                    (Some(id), cats)
                }
                None => return, // nothing highlighted
            },
            crate::ui::categorypicker::PickerMode::Filter => (None, Vec::new()),
        };
        // Master categories, plus any category on the message that isn't in the
        // master list (so it can still be toggled off / shown).
        let mut names: Vec<String> = self
            .master_categories
            .iter()
            .map(|c| c.display_name.clone())
            .collect();
        for c in &current {
            if !names.contains(c) {
                names.push(c.clone());
            }
        }
        let items = names
            .into_iter()
            .map(|name| CategoryItem {
                color: color_for(&self.master_categories, &name),
                selected: current.contains(&name),
                name,
            })
            .collect();
        self.category_picker = Some(CategoryPicker {
            mode,
            message_id,
            items,
            index: 0,
        });
    }

    /// Moves the picker's highlight by `delta`, clamped.
    pub fn category_picker_select(&mut self, delta: isize) {
        if let Some(p) = &mut self.category_picker {
            let len = p.items.len();
            if len == 0 {
                return;
            }
            let max = (len - 1) as isize;
            p.index = (p.index as isize + delta).clamp(0, max) as usize;
        }
    }

    /// Space in Assign mode: toggles the highlighted item's `selected`.
    pub fn category_picker_toggle(&mut self) {
        if let Some(p) = &mut self.category_picker {
            if p.mode == crate::ui::categorypicker::PickerMode::Assign {
                if let Some(it) = p.items.get_mut(p.index) {
                    it.selected = !it.selected;
                }
            }
        }
    }

    /// Enter: Assign → send `SetCategories` with the selected names + close;
    /// Filter → set `category_filter` to the highlighted category + reload.
    pub fn apply_category_picker(&mut self) {
        let Some(p) = self.category_picker.as_ref() else {
            return;
        };
        match p.mode {
            crate::ui::categorypicker::PickerMode::Assign => {
                let Some(id) = p.message_id.clone() else {
                    self.category_picker = None;
                    return;
                };
                let names: Vec<String> = p
                    .items
                    .iter()
                    .filter(|it| it.selected)
                    .map(|it| it.name.clone())
                    .collect();
                self.store.set_categories(&id, &names);
                self.reload_messages();
                let _ = self
                    .sync
                    .cmd_tx
                    .send(SyncCommand::SetCategories { id, categories: names });
                self.category_picker = None;
            }
            crate::ui::categorypicker::PickerMode::Filter => {
                if let Some(it) = p.items.get(p.index) {
                    self.category_filter = Some(it.name.clone());
                }
                self.category_picker = None;
                self.reload_messages();
            }
        }
    }

    /// Clears an active category filter (Esc in the folder view). No-op if none.
    pub fn clear_category_filter(&mut self) {
        if self.category_filter.take().is_some() {
            self.reload_messages();
        }
    }
```

- [ ] **Step 3: Filter in `reload_messages`**

In `lookxy/src/app.rs` `reload_messages`, force the flat path and filter when `category_filter` is set. At the top of the method (after resolving `folder`), change the `if self.threaded {` condition to `if self.threaded && self.category_filter.is_none() {`, and in the flat `else` branch, after `self.messages = self.store.messages_in_folder(...)...;`, add:

```rust
            if let Some(cat) = &self.category_filter {
                self.messages.retain(|m| m.categories.iter().any(|c| c == cat));
            }
```

- [ ] **Step 4: Route the keys**

In `on_key_char`, add (before `_ => {}`):

```rust
            'l' => self.open_category_picker(crate::ui::categorypicker::PickerMode::Assign),
            'L' => self.open_category_picker(crate::ui::categorypicker::PickerMode::Filter),
```

In `lookxy/src/ui/mod.rs`:
- add `pub mod categorypicker;`.
- In `draw`, after the pane popups (near `attachments::draw(f, &*app);`), add `categorypicker::draw(f, &*app);`.
- In `handle_key`, add a routing block ahead of the pane handlers (after the `oof_form` block, before `file_picker`):

```rust
    if app.category_picker.is_some() {
        categorypicker::handle_key(app, key);
        return;
    }
```

- [ ] **Step 5: Write the failing app tests**

In `lookxy/src/app.rs` tests:

```rust
    #[test]
    fn l_opens_assign_picker_seeded_from_message_and_applies() {
        use crate::ui::categorypicker::PickerMode;
        let mut app = App::for_test_with_seeded_store();
        app.master_categories = vec![
            mailcore::graph::model::MasterCategory { display_name: "Work".into(), color: "preset0".into() },
            mailcore::graph::model::MasterCategory { display_name: "Urgent".into(), color: "preset1".into() },
        ];
        // Highlight the seeded m1, open Assign.
        app.open_category_picker(PickerMode::Assign);
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain RefreshCategories
        // Toggle the first item on, apply.
        app.category_picker_toggle();
        app.apply_category_picker();
        assert!(app.category_picker.is_none());
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::SetCategories { categories, .. }) => {
                assert_eq!(categories.len(), 1);
            }
            other => panic!("expected SetCategories, got {other:?}"),
        }
    }

    #[test]
    fn filter_shows_only_matching_and_clears() {
        use crate::ui::categorypicker::PickerMode;
        use mailcore::graph::model::{Message, Recipient};
        let mut app = App::for_test_with_seeded_store();
        app.master_categories = vec![mailcore::graph::model::MasterCategory {
            display_name: "Work".into(),
            color: "preset0".into(),
        }];
        // Add a Work-tagged message alongside the plain seeded m1.
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "w".into(),
                    conversation_id: "c2".into(),
                    subject: "Work item".into(),
                    from: Recipient { name: "B".into(), address: "b@x".into() },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-19T11:00:00Z".into(),
                    sent: "".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "p".into(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: vec!["Work".into()],
                },
            )
            .unwrap();
        app.reload_messages();
        app.open_category_picker(PickerMode::Filter);
        // Highlight "Work" (only master item) and apply.
        app.apply_category_picker();
        assert_eq!(app.category_filter.as_deref(), Some("Work"));
        assert!(app.messages.iter().all(|m| m.categories.contains(&"Work".to_string())));
        assert_eq!(app.messages.len(), 1);
        app.clear_category_filter();
        assert!(app.category_filter.is_none());
        assert!(app.messages.len() >= 2); // m1 back
    }
```

- [ ] **Step 6: Run the tests**

Run: `bash "$LCARGO" test -p lookxy -- l_opens_assign filter_shows_only` — (single filter) `bash "$LCARGO" test -p lookxy category` and `bash "$LCARGO" test -p lookxy filter_shows` (Bash, `dangerouslyDisableSandbox: true`)
Expected: PASS.

- [ ] **Step 7: Full workspace gate + commit**

Run: `bash "$LCARGO" test --workspace`, then `bash "$LCARGO" clippy --workspace --all-targets -- -D warnings`, then `bash "$LCARGO" fmt --all` + `bash "$LCARGO" fmt --all -- --check` (Bash, `dangerouslyDisableSandbox: true`)
Expected: all green, clippy clean, fmt clean.

```bash
git add -A
git commit -m "lookxy: category picker (assign + filter) with l/L keys"
```

---

## Self-Review

**Spec coverage:**
- `Message.categories` + `MESSAGE_SELECT` + store column/migration/encoding + `set_categories` → Task 1. ✅
- `MasterCategory` + `master_categories` table + replace/read → Task 2. ✅
- `get_master_categories` + `set_message_categories` client → Task 3. ✅
- `SetCategories` outbox op + `RefreshCategories` + full-pass fetch + `CategoriesUpdated` → Task 4. ✅
- Dots (list) + chips (reader) + `preset→Color` map + app master list + `CategoriesUpdated` handling → Task 5. ✅
- Assign+filter picker (`l`/`L`), `category_filter`, in-memory flat filter, key routing → Task 6. ✅
- Error handling: outbox retry/quarantine reconverge (Task 4 uses the existing path); master-fetch silent degradation (Task 4 `refresh_master_categories` swallows); unknown-color → gray (Task 5 `color_for`); category-on-message-not-in-master shown + toggleable (Task 6 `open_category_picker` synthesizes it) → covered. ✅

**Placeholder scan:** No TBD/TODO. The list/reader threading (Task 5 Steps 5–6) describes the exact spans to prepend and the signature change; the perl ripple (Task 1 Step 7) is mechanical with a compiler backstop.

**Type consistency:** `categories: Vec<String>` on `Message`/`MessageRow`/`OutboxOp::SetCategories`/`SyncCommand::SetCategories` throughout. `MasterCategory { display_name, color }` consistent model↔store↔client↔UI. `CategoryPicker`/`PickerMode`/`CategoryItem` field names match between the module (Task 6 Step 1) and the app methods (Step 2). `color_for(master, name)` / `preset_color(preset)` signatures consistent Task 5↔6. `set_categories(id, &[String])` (store) vs `set_message_categories(id, &[String])` (client) — distinct names, used correctly (store = local, client = Graph).
