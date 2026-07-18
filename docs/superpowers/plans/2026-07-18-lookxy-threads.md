# lookxy Conversation/Thread View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Group the lookxy folder message-list into collapsible cross-folder conversations (by `conversation_id`), toggled on/off with a persisted setting.

**Architecture:** A new pure `mailcore::thread` module groups `MessageRow`s into `Thread`s; a new `Store::conversations_in_folder` gathers, for the conversations present in a folder, all their messages across every folder. `lookxy::App` holds the threaded view-model (a flattened `visible_rows` projection of headers + child messages) alongside the existing flat list; the UI navigation/activation/rendering/triage paths branch on a `threaded_active()` guard. Whole-conversation triage reuses the existing optimistic-store + per-message `SyncCommand` outbox path.

**Tech Stack:** Rust (edition 2024, MSRV 1.88), rusqlite (bundled SQLite + FTS5), ratatui 0.29 (re-exports crossterm), hand-rolled `mailcore::json` (no serde). No new dependencies.

## Global Constraints

- **Build/test ONLY through the wrapper** (the repo's `.cargo/bin` shims are broken; bare `cargo` fails with os error 448). Every `cargo` command in this plan is written as `bash "$LCARGO" …` where
  `LCARGO = C:\Users\BORIS_~1\AppData\Local\Temp\claude\C--Users-boris-kudriashov-Source-docxy\1da9a016-b606-4432-8951-6d73bb91c967\scratchpad\lcargo.sh`
  Run it via the Bash tool with `dangerouslyDisableSandbox: true`.
- **No new dependencies.** Use `mailcore::json` for any JSON, hand-rolled to match the crate.
- **MSRV 1.88, edition 2024.** Extern blocks are `unsafe extern`; let-chains (`if let … && let …`) are available and used in this codebase.
- **CI runs `cargo clippy --all-targets -- -D warnings` on ubuntu/macos/windows.** No warnings, and no `#[cfg(windows)]`-only bindings left unused on Unix. Run `bash "$LCARGO" fmt` before every commit.
- **Preserve existing behavior when threaded is OFF.** The flat list (`messages`/`msg_index`) and search view must be byte-for-byte unchanged in flat mode; the shared `message_list::draw_list` stays as-is.
- **Reuse the optimistic-store + outbox pattern** for every mutation: local `Store` write → `reload_messages()` → `self.sync.cmd_tx.send(SyncCommand::…)`. Never call Graph directly from `App`.
- **`MessageRow` column order** (used by `map_message_row`, do not reorder): `id, folder_id, conversation_id, subject, from_name, from_addr, to_recipients, cc_recipients, received_at, sent_at, is_read, is_flagged, has_attachments, importance, preview, is_draft`.
- **`received_at` is ISO-8601 UTC** (`2026-07-16T10:00:00Z`), lexicographically sortable — plain string comparison is a valid time order.

---

### Task 1: `mailcore::thread` — pure grouping model

**Files:**
- Create: `mailcore/src/thread.rs`
- Modify: `mailcore/src/lib.rs` (add `pub mod thread;`)

**Interfaces:**
- Consumes: `mailcore::store::MessageRow`.
- Produces:
  - `pub fn conv_key(m: &MessageRow) -> String` — `conversation_id` if non-empty, else `format!("msg:{}", m.id)`.
  - `pub struct Thread { pub key: String, pub messages: Vec<MessageRow>, pub latest_received: String, pub unread_count: usize, pub any_flagged: bool, pub any_attachments: bool, pub subject: String, pub participants: Vec<String> }`
  - `pub fn build_threads(rows: &[MessageRow]) -> Vec<Thread>` — groups by `conv_key`; within each thread sorts messages by `received_at` ascending; orders threads by `latest_received` descending, tie-broken by `key` ascending (fully order-independent of the input).

- [ ] **Step 1: Write the failing tests**

Add to `mailcore/src/thread.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MessageRow;

    fn row(id: &str, conv: &str, recv: &str, from: &str, subj: &str, read: bool) -> MessageRow {
        MessageRow {
            id: id.into(),
            folder_id: "inbox".into(),
            conversation_id: conv.into(),
            subject: subj.into(),
            from_name: from.into(),
            from_addr: format!("{from}@x"),
            to_recipients: String::new(),
            cc_recipients: String::new(),
            received_at: recv.into(),
            sent_at: String::new(),
            is_read: read,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: String::new(),
            is_draft: false,
        }
    }

    #[test]
    fn groups_by_conversation_and_derives_aggregates() {
        // Two conversations; c1 has three messages (two unread), c2 one.
        let rows = vec![
            row("a", "c1", "2026-07-10T09:00:00Z", "Ann", "Q3 plan", true),
            row("b", "c2", "2026-07-11T09:00:00Z", "Zed", "Lunch", false),
            row("c", "c1", "2026-07-12T09:00:00Z", "Bob", "Re: Q3 plan", false),
            row("d", "c1", "2026-07-11T12:00:00Z", "Ann", "", false),
        ];
        let threads = build_threads(&rows);
        // c1 is most-recent (latest 07-12) so it sorts first.
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].key, "c1");
        assert_eq!(threads[0].messages.len(), 3);
        // messages sorted oldest->newest by received_at
        let ids: Vec<&str> = threads[0].messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "d", "c"]);
        assert_eq!(threads[0].latest_received, "2026-07-12T09:00:00Z");
        assert_eq!(threads[0].unread_count, 2);
        // participants: unique from_name in oldest->newest order
        assert_eq!(threads[0].participants, vec!["Ann".to_string(), "Bob".to_string()]);
        // subject: latest non-empty (message "c", since "d" is empty)
        assert_eq!(threads[0].subject, "Re: Q3 plan");
        assert_eq!(threads[1].key, "c2");
    }

    #[test]
    fn blank_conversation_id_is_a_singleton_keyed_by_id() {
        let rows = vec![
            row("x", "", "2026-07-10T09:00:00Z", "Ann", "one", false),
            row("y", "", "2026-07-11T09:00:00Z", "Bob", "two", false),
        ];
        let threads = build_threads(&rows);
        assert_eq!(threads.len(), 2);
        assert!(threads.iter().all(|t| t.messages.len() == 1));
        assert!(threads.iter().any(|t| t.key == "msg:x"));
        assert!(threads.iter().any(|t| t.key == "msg:y"));
    }

    #[test]
    fn flag_and_attachment_aggregate_across_the_thread() {
        let mut a = row("a", "c1", "2026-07-10T09:00:00Z", "Ann", "s", true);
        let mut b = row("b", "c1", "2026-07-11T09:00:00Z", "Bob", "s", true);
        a.is_flagged = true;
        b.has_attachments = true;
        let threads = build_threads(&[a, b]);
        assert_eq!(threads.len(), 1);
        assert!(threads[0].any_flagged);
        assert!(threads[0].any_attachments);
        assert_eq!(threads[0].unread_count, 0);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `bash "$LCARGO" test -p mailcore thread`
Expected: FAIL — `cannot find function build_threads` (module not written yet).

- [ ] **Step 3: Write the implementation**

Put this at the top of `mailcore/src/thread.rs` (above the test module):

```rust
//! Pure grouping of `MessageRow`s into conversation `Thread`s for the
//! threaded message-list view. No DB, no I/O — takes rows (as the store
//! returns them) and returns grouped, self-consistently-ordered threads, so
//! it can be unit-tested without a store and never depends on the caller's
//! row ordering.

use crate::store::MessageRow;

/// The grouping key for a message: its `conversation_id` when non-empty,
/// else `msg:<id>` so a message Graph gave no conversation for becomes its
/// own singleton thread rather than being merged with every other blank one.
pub fn conv_key(m: &MessageRow) -> String {
    if m.conversation_id.is_empty() {
        format!("msg:{}", m.id)
    } else {
        m.conversation_id.clone()
    }
}

/// One conversation: its messages (oldest→newest) plus display aggregates.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub key: String,
    pub messages: Vec<MessageRow>,
    pub latest_received: String,
    pub unread_count: usize,
    pub any_flagged: bool,
    pub any_attachments: bool,
    pub subject: String,
    pub participants: Vec<String>,
}

/// Groups `rows` by `conv_key`. Within a thread, messages are sorted by
/// `received_at` ascending; threads are ordered by `latest_received`
/// descending, tie-broken by `key` ascending — so the result is independent
/// of the input ordering.
pub fn build_threads(rows: &[MessageRow]) -> Vec<Thread> {
    let mut groups: Vec<(String, Vec<MessageRow>)> = Vec::new();
    for m in rows {
        let key = conv_key(m);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, msgs)) => msgs.push(m.clone()),
            None => groups.push((key, vec![m.clone()])),
        }
    }

    let mut threads: Vec<Thread> = groups
        .into_iter()
        .map(|(key, mut messages)| {
            messages.sort_by(|a, b| a.received_at.cmp(&b.received_at));
            let latest_received = messages
                .last()
                .map(|m| m.received_at.clone())
                .unwrap_or_default();
            let unread_count = messages.iter().filter(|m| !m.is_read).count();
            let any_flagged = messages.iter().any(|m| m.is_flagged);
            let any_attachments = messages.iter().any(|m| m.has_attachments);
            // Latest non-empty subject (walk newest→oldest).
            let subject = messages
                .iter()
                .rev()
                .map(|m| m.subject.as_str())
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_string();
            // Unique participant names, oldest→newest first-seen order.
            let mut participants: Vec<String> = Vec::new();
            for m in &messages {
                if !m.from_name.is_empty() && !participants.contains(&m.from_name) {
                    participants.push(m.from_name.clone());
                }
            }
            Thread {
                key,
                messages,
                latest_received,
                unread_count,
                any_flagged,
                any_attachments,
                subject,
                participants,
            }
        })
        .collect();

    threads.sort_by(|a, b| {
        b.latest_received
            .cmp(&a.latest_received)
            .then_with(|| a.key.cmp(&b.key))
    });
    threads
}
```

Add to `mailcore/src/lib.rs`, alphabetically among the existing `pub mod` lines:

```rust
pub mod thread;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `bash "$LCARGO" test -p mailcore thread`
Expected: PASS (3 tests).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/thread.rs mailcore/src/lib.rs
git commit -m "mailcore: pure thread grouping model (build_threads)"
```

---

### Task 2: `Store::conversations_in_folder` — cross-folder gather

**Files:**
- Modify: `mailcore/src/store/mod.rs` (add method near `messages_in_folder`, ~line 497; add a test in the store `mod tests` at ~line 1368)

**Interfaces:**
- Consumes: the `messages` table, `map_message_row`.
- Produces: `pub fn conversations_in_folder(&self, folder_id: &str, limit: i64, offset: i64) -> Result<Vec<MessageRow>, StoreError>` — returns every message (across all folders) belonging to the `limit` most-recent conversations that have ≥1 message in `folder_id`, ordered `(conversation latest received DESC, message received ASC)`.

- [ ] **Step 1: Write the failing test**

Add to `mailcore/src/store/mod.rs` inside the existing `#[cfg(test)] mod tests` (the one at ~line 1368 with `use super::*;` and the `msg` helper). Reuse that module's `Message`/`Recipient` seeding style (see `upserts_and_lists_messages_newest_first`). This test seeds two folders and a conversation that spans both:

```rust
    #[test]
    fn conversations_in_folder_gathers_the_thread_across_folders() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_folder(&MailFolder {
            id: "inbox".into(), display_name: "Inbox".into(), parent_id: None,
            total_count: 0, unread_count: 0, well_known_name: Some("inbox".into()),
        }).unwrap();
        store.upsert_folder(&MailFolder {
            id: "sent".into(), display_name: "Sent".into(), parent_id: None,
            total_count: 0, unread_count: 0, well_known_name: Some("sentitems".into()),
        }).unwrap();

        // Conversation c1: an inbox message + a reply the user sent (in Sent).
        let mut inbound = msg("10", false);   // helper sets received "2026-07-10T..."
        inbound.conversation_id = "c1".into();
        inbound.from = Recipient { name: "Ann".into(), address: "ann@x".into() };
        let mut reply = msg("12", true);
        reply.conversation_id = "c1".into();
        // A conversation c2 that lives only in Sent (must NOT appear in inbox view).
        let mut sent_only = msg("11", true);
        sent_only.conversation_id = "c2".into();

        store.upsert_message("inbox", &inbound).unwrap();
        store.upsert_message("sent", &reply).unwrap();
        store.upsert_message("sent", &sent_only).unwrap();

        let rows = store.conversations_in_folder("inbox", 50, 0).unwrap();
        // c1 only: both its messages, including the one filed under Sent.
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"10"));
        assert!(ids.contains(&"12"));   // cross-folder: the Sent reply is included
        assert!(!ids.contains(&"11"));  // c2 has no inbox message → excluded
    }

    #[test]
    fn conversations_in_folder_singletons_blank_conversation_ids() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_folder(&MailFolder {
            id: "inbox".into(), display_name: "Inbox".into(), parent_id: None,
            total_count: 0, unread_count: 0, well_known_name: Some("inbox".into()),
        }).unwrap();
        let mut a = msg("20", false);
        a.conversation_id = "".into();
        let mut b = msg("21", false);
        b.conversation_id = "".into();
        store.upsert_message("inbox", &a).unwrap();
        store.upsert_message("inbox", &b).unwrap();

        let rows = store.conversations_in_folder("inbox", 50, 0).unwrap();
        assert_eq!(rows.len(), 2); // two independent singletons, not one merged group
    }
```

Note: confirm the seeding helpers actually used by the surrounding tests (`Store::open_in_memory`, `upsert_folder`, `upsert_message`, the `msg` helper, `MailFolder`, `Recipient`) match these names; adapt the calls to the exact signatures already imported in that test module if they differ (e.g. if `msg` returns a `Message` whose `conversation_id` you set via a field). Do not invent new seed helpers.

- [ ] **Step 2: Run the test to verify it fails**

Run: `bash "$LCARGO" test -p mailcore conversations_in_folder`
Expected: FAIL — `no method named conversations_in_folder`.

- [ ] **Step 3: Write the implementation**

Add this method to the `impl Store` block, right after `messages_in_folder` (~line 516):

```rust
    /// Every message belonging to the `limit` most-recent conversations that
    /// have at least one message in `folder_id` — including their messages in
    /// *other* folders (Sent/Archive), so the folder view can show whole
    /// cross-folder conversations. Grouping key is `conversation_id`, or
    /// `msg:<id>` when it's blank (a blank-conversation message is its own
    /// singleton). Ordered `(conversation latest received DESC, message
    /// received ASC)`.
    pub fn conversations_in_folder(
        &self,
        folder_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<MessageRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "WITH keyed AS (
                 SELECT id, folder_id, conversation_id, subject, from_name, from_addr,
                        to_recipients, cc_recipients, received_at, sent_at,
                        is_read, is_flagged, has_attachments, importance, preview, is_draft,
                        CASE WHEN conversation_id <> '' THEN conversation_id
                             ELSE 'msg:' || id END AS conv_key
                 FROM messages
             ),
             ranked AS (
                 SELECT conv_key, MAX(received_at) AS latest
                 FROM keyed
                 WHERE conv_key IN (SELECT DISTINCT conv_key FROM keyed WHERE folder_id = ?1)
                 GROUP BY conv_key
                 ORDER BY latest DESC
                 LIMIT ?2 OFFSET ?3
             )
             SELECT k.id, k.folder_id, k.conversation_id, k.subject, k.from_name, k.from_addr,
                    k.to_recipients, k.cc_recipients, k.received_at, k.sent_at,
                    k.is_read, k.is_flagged, k.has_attachments, k.importance, k.preview, k.is_draft
             FROM keyed k
             JOIN ranked r ON k.conv_key = r.conv_key
             ORDER BY r.latest DESC, k.received_at ASC",
        )?;
        let rows = stmt
            .query_map(params![folder_id, limit, offset], map_message_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `bash "$LCARGO" test -p mailcore conversations_in_folder`
Expected: PASS (2 tests).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore --all-targets -- -D warnings
git add mailcore/src/store/mod.rs
git commit -m "mailcore: Store::conversations_in_folder cross-folder gather"
```

---

### Task 3: Config `threaded` field + persistence

**Files:**
- Modify: `lookxy/src/config.rs`

**Interfaces:**
- Consumes: `mailcore::json` (`parse`, `Value`).
- Produces:
  - `Config` gains `pub threaded: bool` (default `true`).
  - `pub fn config_file_path() -> Option<PathBuf>` (make the existing private fn `pub`).
  - `pub fn persist_threaded_to(path: &Path, value: bool) -> std::io::Result<()>` — read-modify-write the JSON file, replacing only the `threaded` key and preserving all others.
  - `pub fn persist_threaded(value: bool)` — calls `persist_threaded_to` with `config_file_path()`, ignoring the result (best-effort).

- [ ] **Step 1: Write the failing tests**

Add to `lookxy/src/config.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn threaded_defaults_true_and_file_overlay_can_disable() {
        assert!(Config::default().threaded);
        let _guard = lock_env();
        clear_env();
        let dir = std::env::temp_dir().join(format!(
            "lookxy-config-test-{}-threaded-overlay", std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"threaded":false}"#).unwrap();
        let c = Config::load_from(Some(&path));
        assert!(!c.threaded);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_threaded_roundtrips_and_preserves_other_keys() {
        let _guard = lock_env();
        clear_env();
        let dir = std::env::temp_dir().join(format!(
            "lookxy-config-test-{}-persist-threaded", std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"client_id":"keep-me","backfill_days":7}"#).unwrap();

        persist_threaded_to(&path, false).unwrap();

        let c = Config::load_from(Some(&path));
        assert!(!c.threaded);            // the toggle was written
        assert_eq!(c.client_id, "keep-me"); // other keys preserved
        assert_eq!(c.backfill_days, 7);

        persist_threaded_to(&path, true).unwrap();
        assert!(Config::load_from(Some(&path)).threaded);

        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `bash "$LCARGO" test -p lookxy config`
Expected: FAIL — `no field threaded` / `cannot find function persist_threaded_to`.

- [ ] **Step 3: Write the implementation**

In `lookxy/src/config.rs`:

Add the field to the struct:

```rust
pub struct Config {
    pub client_id: String,
    pub backfill_days: i64,
    pub refresh_secs: u64,
    /// Whether the folder message-list is grouped into conversations. Toggled
    /// at runtime with `t` (persisted via `persist_threaded`).
    pub threaded: bool,
}
```

Set the default (in `impl Default for Config`):

```rust
        Config {
            client_id: "14d82eec-204b-4c2f-b7e8-296a70dab67e".to_string(),
            backfill_days: 180,
            refresh_secs: 60,
            threaded: true,
        }
```

In `overlay_json`, after the `refresh_secs` block, add:

```rust
        if let Some(b) = value.get("threaded").and_then(|v| v.as_bool()) {
            self.threaded = b;
        }
```

In `overlay_env`, add an env override for parity with the other fields:

```rust
        if let Ok(v) = std::env::var("LOOKXY_THREADED") {
            let v = v.trim();
            if v.eq_ignore_ascii_case("true") || v == "1" {
                self.threaded = true;
            } else if v.eq_ignore_ascii_case("false") || v == "0" {
                self.threaded = false;
            }
        }
```

Make `config_file_path` public (change `fn config_file_path`  to `pub fn config_file_path`).

Add the persistence functions at module scope (below `config_file_path`):

```rust
/// Best-effort persistence of the `threaded` toggle to the real config file.
/// Silently does nothing if the config path can't be determined or the write
/// fails — a UI toggle must never crash or block on a settings-file problem.
pub fn persist_threaded(value: bool) {
    if let Some(path) = config_file_path() {
        let _ = persist_threaded_to(&path, value);
    }
}

/// Read-modify-write `path`, replacing only the `threaded` key and preserving
/// every other key already in the file (client_id, backfill_days, unknown
/// keys, …). Creates the file (and parent dir) if absent.
pub fn persist_threaded_to(path: &Path, value: bool) -> std::io::Result<()> {
    use mailcore::json::Value;

    // Start from the file's existing object (or an empty one).
    let mut entries: Vec<(String, Value)> = match std::fs::read_to_string(path) {
        Ok(text) => match mailcore::json::parse(&text) {
            Ok(Value::Object(e)) => e,
            _ => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    entries.retain(|(k, _)| k != "threaded");
    entries.push(("threaded".to_string(), Value::Bool(value)));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, Value::Object(entries).to_string())
}
```

Ensure `use std::path::{Path, PathBuf};` is present (it already is).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `bash "$LCARGO" test -p lookxy config`
Expected: PASS (existing config tests + 2 new).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/config.rs
git commit -m "lookxy: Config.threaded field + persist_threaded"
```

---

### Task 4: App threaded state, reload, and visible-rows projection

**Files:**
- Modify: `lookxy/src/app.rs`

**Interfaces:**
- Consumes: `Store::conversations_in_folder` (Task 2), `mailcore::thread::{Thread, build_threads}` (Task 1).
- Produces (new on `App`):
  - fields `pub threaded: bool`, `pub config_path: Option<PathBuf>`, `pub threads: Vec<ThreadView>`, `pub visible_rows: Vec<Row>`, `pub row_index: usize`
  - `pub struct ThreadView { pub thread: mailcore::thread::Thread, pub expanded: bool }`
  - `pub enum Row { Header(usize), Message(usize, usize) }` (thread index; (thread index, message index))
  - `pub fn threaded_active(&self) -> bool` — `self.threaded && self.search.is_none() && self.mode == Mode::Mail`
  - `fn rebuild_visible_rows(&mut self)`
  - `reload_messages` builds threads when `self.threaded`.

- [ ] **Step 1: Write the failing test**

First add the two shared seed helpers (`seed_second_in_c1`, `seed_singleton_c2`) from the "Shared test seed helpers" section above to `lookxy/src/app.rs`'s `#[cfg(test)] mod tests` (the one at ~line 1346). Then add these tests:

```rust
    #[test]
    fn threaded_reload_groups_into_visible_rows() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 now has m1 + m2 → a 2-message thread
        seed_singleton_c2(&app); // c2 is a 1-message thread
        app.reload_messages();

        // The multi-message thread (c1) is a Header; the singleton (c2) a bare Message.
        assert!(app.visible_rows.iter().any(|r| matches!(r, Row::Header(_))));
        assert!(app.visible_rows.iter().any(|r| matches!(r, Row::Message(_, _))));
    }

    #[test]
    fn threaded_reload_expands_to_show_children() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app);
        app.reload_messages();
        let pos = app.visible_rows.iter().position(|r| matches!(r, Row::Header(_))).unwrap();
        let before = app.visible_rows.len();
        if let Row::Header(t) = app.visible_rows[pos] {
            app.threads[t].expanded = true;
            app.rebuild_visible_rows();
        }
        assert!(app.visible_rows.len() > before); // child rows appeared
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `bash "$LCARGO" test -p lookxy threaded_reload`
Expected: FAIL — `no field threaded` / `cannot find type Row`.

- [ ] **Step 3: Write the implementation**

Add the types (near the other small state structs in `app.rs`, e.g. above `impl App`):

```rust
/// A conversation in the threaded folder view, plus whether it's expanded.
pub struct ThreadView {
    pub thread: mailcore::thread::Thread,
    pub expanded: bool,
}

/// One visible line in the threaded list: a collapsible conversation header,
/// or (only under an expanded header) one of its messages. A single-message
/// conversation is represented directly as a `Message` row with no header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Header(usize),         // index into `threads`
    Message(usize, usize), // (thread index, message index within the thread)
}
```

Add the fields to `pub struct App` (next to `messages`/`msg_index`):

```rust
    /// Whether the folder view groups messages into conversations. Seeded
    /// from `Config::threaded`; toggled by `t`.
    pub threaded: bool,
    /// Path used to persist the `threaded` toggle. `None` in tests (no disk
    /// write); `Some` in production (set by `main`).
    pub config_path: Option<PathBuf>,
    /// The threaded view-model, built by `reload_messages` when `threaded`.
    pub threads: Vec<ThreadView>,
    /// Flattened header+message rows for render + navigation in threaded mode.
    pub visible_rows: Vec<Row>,
    /// Cursor into `visible_rows` (threaded mode's equivalent of `msg_index`).
    pub row_index: usize,
```

Initialize them in `App::new` (in the struct literal). Default `threaded: false` so `App::new`'s internal reload builds the flat list (keeping every existing flat-mode test green with no change to the shared test helpers); production turns it on from config in `main` (Task 6), and threaded tests opt in with `app.threaded = true`:

```rust
            threaded: false,
            config_path: None,
            threads: Vec::new(),
            visible_rows: Vec::new(),
            row_index: 0,
```

Add the guard + rebuild helpers to `impl App`:

```rust
    /// True when the threaded folder view is what's on screen: threading is on,
    /// no search is active (search results stay flat), and we're in Mail mode.
    pub fn threaded_active(&self) -> bool {
        self.threaded && self.search.is_none() && self.mode == Mode::Mail
    }

    /// Rebuilds `visible_rows` from `threads` + their expanded flags, and
    /// clamps `row_index` into range. A single-message thread contributes one
    /// bare `Message` row (no header); a multi-message thread contributes a
    /// `Header` and, when expanded, its child `Message` rows.
    pub fn rebuild_visible_rows(&mut self) {
        let mut rows = Vec::new();
        for (t, tv) in self.threads.iter().enumerate() {
            if tv.thread.messages.len() == 1 {
                rows.push(Row::Message(t, 0));
            } else {
                rows.push(Row::Header(t));
                if tv.expanded {
                    for m in 0..tv.thread.messages.len() {
                        rows.push(Row::Message(t, m));
                    }
                }
            }
        }
        if self.row_index >= rows.len() {
            self.row_index = rows.len().saturating_sub(1);
        }
        self.visible_rows = rows;
    }
```

Modify `reload_messages` (currently ~line 419) to build threads when threaded. Replace its body with:

```rust
    /// Re-reads the selected folder's messages from the store. In flat mode
    /// this fills `messages` (newest first) and clamps `msg_index`. In
    /// threaded mode it instead builds `threads` (cross-folder conversations)
    /// and rebuilds `visible_rows`, preserving each thread's expanded state by
    /// conversation key across the rebuild.
    pub fn reload_messages(&mut self) {
        let Some(folder) = self.selected_folder.clone() else {
            self.messages.clear();
            self.threads.clear();
            self.visible_rows.clear();
            return;
        };
        if self.threaded {
            let expanded: std::collections::HashSet<String> = self
                .threads
                .iter()
                .filter(|tv| tv.expanded)
                .map(|tv| tv.thread.key.clone())
                .collect();
            let rows = self
                .store
                .conversations_in_folder(&folder, MESSAGE_PAGE_SIZE, 0)
                .unwrap_or_default();
            self.threads = mailcore::thread::build_threads(&rows)
                .into_iter()
                .map(|thread| {
                    let expanded = expanded.contains(&thread.key);
                    ThreadView { thread, expanded }
                })
                .collect();
            self.rebuild_visible_rows();
        } else {
            self.messages = self
                .store
                .messages_in_folder(&folder, MESSAGE_PAGE_SIZE, 0)
                .unwrap_or_default();
            if self.msg_index >= self.messages.len() {
                self.msg_index = self.messages.len().saturating_sub(1);
            }
        }
    }
```

Ensure `use std::path::PathBuf;` is in scope (it is — `token_path: PathBuf` already uses it). If `Mode` isn't already imported in `app.rs`, it is (the `mode: Mode` field uses it).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `bash "$LCARGO" test -p lookxy threaded_reload`
Expected: PASS.

Also run the full lookxy suite to confirm flat mode is unbroken:
Run: `bash "$LCARGO" test -p lookxy`
Expected: all existing tests still PASS. Because `App::new` defaults `threaded: false`, `App::new`'s internal reload builds the flat `messages` list exactly as before, so no existing test and no shared test helper needs to change.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/app.rs
git commit -m "lookxy: App threaded state, reload builds threads, visible_rows"
```

---

### Task 5: Threaded navigation + activate (expand/collapse/open)

**Files:**
- Modify: `lookxy/src/app.rs` (navigation/activation methods)
- Modify: `lookxy/src/ui/mod.rs` (`move_selection`, `activate` — branch on `threaded_active`)

**Interfaces:**
- Consumes: `threaded_active`, `visible_rows`, `threads`, `open_message`, `open_draft` (existing).
- Produces on `App`:
  - `pub fn move_thread_selection(&mut self, delta: isize)` — clamped move of `row_index` over `visible_rows`.
  - `pub fn activate_thread_row(&mut self)` — Header: toggle expand, and on expand open the thread's latest message; Message row: open it (draft → composer).
  - `fn selected_row(&self) -> Option<Row>`.

- [ ] **Step 1: Write the failing test**

Add to `app.rs` tests:

```rust
    #[test]
    fn thread_navigation_is_clamped_over_visible_rows() {
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 header
        seed_singleton_c2(&app); // c2 message → visible_rows.len() >= 2
        app.reload_messages();
        app.row_index = 0;
        app.move_thread_selection(-1);
        assert_eq!(app.row_index, 0); // clamped at the top
        let last = app.visible_rows.len().saturating_sub(1);
        app.row_index = last;
        app.move_thread_selection(1);
        assert_eq!(app.row_index, last); // clamped at the bottom
    }

    #[test]
    fn activating_a_collapsed_header_expands_and_opens_latest() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = [m1 10:00, m2 11:00]; latest = m2
        app.reload_messages();
        let pos = app.visible_rows.iter().position(|r| matches!(r, Row::Header(_))).unwrap();
        app.row_index = pos;
        app.activate_thread_row();
        if let Row::Header(t) = app.visible_rows[pos] {
            assert!(app.threads[t].expanded);
            assert_eq!(app.selected_msg.as_deref(), Some("m2")); // newest opened
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p lookxy thread_navigation activating_a_collapsed`
Expected: FAIL — `no method named move_thread_selection`.

- [ ] **Step 3: Write the implementation**

Add to `impl App`:

```rust
    /// The `visible_rows` entry the cursor is on, if any.
    fn selected_row(&self) -> Option<Row> {
        self.visible_rows.get(self.row_index).copied()
    }

    /// Moves the threaded-list cursor by `delta`, clamped to `[0, len)` (no
    /// wrap — a header and its children read as one block, so wrapping the
    /// cursor off either end would be disorienting).
    pub fn move_thread_selection(&mut self, delta: isize) {
        let len = self.visible_rows.len();
        if len == 0 {
            return;
        }
        let max = (len - 1) as isize;
        let next = (self.row_index as isize + delta).clamp(0, max);
        self.row_index = next as usize;
    }

    /// Enter on the highlighted threaded row. On a header: toggle expansion,
    /// and when it becomes expanded, open the thread's latest message in the
    /// reading pane. On a message row: open that message (a draft opens in the
    /// composer, matching the flat list's activate behavior).
    pub fn activate_thread_row(&mut self) {
        match self.selected_row() {
            Some(Row::Header(t)) => {
                let expanding = !self.threads[t].expanded;
                self.threads[t].expanded = expanding;
                if expanding {
                    if let Some(latest) = self.threads[t].thread.messages.last() {
                        let (id, is_draft) = (latest.id.clone(), latest.is_draft);
                        if is_draft {
                            self.open_draft(&id);
                        } else {
                            self.open_message(&id);
                            self.focus = Pane::Reading;
                        }
                    }
                }
                self.rebuild_visible_rows();
            }
            Some(Row::Message(t, m)) => {
                if let Some(msg) = self.threads[t].thread.messages.get(m) {
                    let (id, is_draft) = (msg.id.clone(), msg.is_draft);
                    if is_draft {
                        self.open_draft(&id);
                    } else {
                        self.open_message(&id);
                        self.focus = Pane::Reading;
                    }
                }
            }
            None => {}
        }
    }
```

In `lookxy/src/ui/mod.rs`, in `move_selection`, change the `Pane::List` arm to branch on threaded mode:

```rust
        Pane::List => {
            if app.threaded_active() {
                app.move_thread_selection(delta);
            } else if let Some(len) = nonzero(app.messages.len()) {
                app.msg_index = wrapped(app.msg_index, delta, len);
            }
        }
```

In `activate`, change the `Pane::List` arm:

```rust
        Pane::List => {
            if app.threaded_active() {
                app.activate_thread_row();
            } else if let Some(msg) = app.messages.get(app.msg_index) {
                let id = msg.id.clone();
                if msg.is_draft {
                    app.open_draft(&id);
                } else {
                    app.open_message(&id);
                    app.focus = Pane::Reading;
                }
            }
        }
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy`
Expected: new tests PASS; all existing PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/app.rs lookxy/src/ui/mod.rs
git commit -m "lookxy: threaded navigation + activate (expand/collapse/open)"
```

---

### Task 6: `t` toggle + main wiring

**Files:**
- Modify: `lookxy/src/app.rs` (`on_key_char` + `toggle_threaded`)
- Modify: `lookxy/src/main.rs` (seed `threaded`/`config_path` after `App::new`)

**Interfaces:**
- Consumes: `config::persist_threaded_to` (Task 3), `config::config_file_path` (Task 3), `reload_messages`.
- Produces: `pub fn toggle_threaded(&mut self)`; `t` bound in `on_key_char`.

- [ ] **Step 1: Write the failing test**

Add to `app.rs` tests:

```rust
    #[test]
    fn t_key_toggles_threaded_and_rebuilds() {
        let mut app = App::for_test_with_seeded_store();
        // starts flat by construction default; config_path is None → no disk write
        seed_second_in_c1(&app);
        app.reload_messages();
        assert!(!app.messages.is_empty()); // flat list populated

        app.on_key_char('t');
        assert!(app.threaded);
        assert!(!app.visible_rows.is_empty()); // threaded view built

        app.on_key_char('t');
        assert!(!app.threaded);
        assert!(!app.messages.is_empty()); // back to flat
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p lookxy t_key_toggles`
Expected: FAIL — `t` is currently ignored by `on_key_char` (no assertion change).

- [ ] **Step 3: Write the implementation**

Add to `impl App`:

```rust
    /// `t`: flips threaded/flat, rebuilds the folder view for the new mode,
    /// resets both cursors to the top, and persists the choice (best-effort;
    /// a `None` `config_path`, as in tests, just skips the write).
    pub fn toggle_threaded(&mut self) {
        self.threaded = !self.threaded;
        self.row_index = 0;
        self.msg_index = 0;
        self.reload_messages();
        if let Some(path) = &self.config_path {
            let _ = crate::config::persist_threaded_to(path, self.threaded);
        }
    }
```

Add the `'t'` arm to `on_key_char` (alongside the other triage keys):

```rust
            't' => self.toggle_threaded(),
```

In `lookxy/src/main.rs`, right after `let mut app = App::new(store, handle, token_path);` (~line 130), seed the threaded config and rebuild the initial view for the configured mode (since `App::new` built the flat list under the `threaded: false` default):

```rust
    app.threaded = config.threaded;
    app.config_path = crate::config::config_file_path();
    app.reload_messages();
```

(`config` is the `Config::load_from(None)` value already in scope at ~line 99. `reload_messages` is a no-op-safe rebuild — with no folder selected yet it just clears the lists; once a folder is selected it builds the right view.)

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy t_key_toggles`
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/app.rs lookxy/src/main.rs
git commit -m "lookxy: 't' toggles threaded view (persisted) + main wiring"
```

---

### Task 7: Threaded rendering

**Files:**
- Modify: `lookxy/src/ui/message_list.rs`

**Interfaces:**
- Consumes: `App::threaded_active`, `visible_rows`, `threads`, `row_index`; existing `truncate_width`, `border_style`, `short_time`, `Pane`.
- Produces: threaded rendering inside `message_list::draw` (dispatches to a new private `draw_threaded`).

- [ ] **Step 1: Write the failing test**

Add to `message_list.rs` tests:

```rust
    #[test]
    fn threaded_view_renders_headers_and_expanded_children() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = [Alice "Hello", Bob "Re: Hello"]
        app.reload_messages();
        // expand the first header
        if let Some(Row::Header(t)) =
            app.visible_rows.iter().copied().find(|r| matches!(r, Row::Header(_)))
        {
            app.threads[t].expanded = true;
            app.rebuild_visible_rows();
        }

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("(2)"));         // thread count
        assert!(text.contains("Re: Hello"));   // latest subject
        assert!(text.contains("Alice"));       // a participant
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p lookxy threaded_view_renders`
Expected: FAIL — `draw` still renders the flat list, `(2)` not present.

- [ ] **Step 3: Write the implementation**

In `message_list.rs`, change `draw` to dispatch, and add `draw_threaded`:

```rust
pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Pane::List;
    if app.threaded_active() {
        draw_threaded(f, area, focused, app);
    } else {
        draw_list(f, area, "Messages", focused, &app.messages, app.msg_index);
    }
}

/// Renders the threaded folder view: one row per `visible_rows` entry — a
/// conversation header (`▾`/`▸`, count, participants, subject, latest time,
/// aggregate unread-bold / `!` / `@`), or an indented child message row under
/// an expanded header. The cursor (`row_index`) is highlighted.
fn draw_threaded(f: &mut Frame, area: Rect, focused: bool, app: &App) {
    use crate::app::Row;

    let block = Block::default()
        .title("Conversations")
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let inner_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = app
        .visible_rows
        .iter()
        .map(|row| match *row {
            Row::Header(t) => header_line(&app.threads[t].thread, app.threads[t].expanded, inner_width),
            Row::Message(t, m) => {
                let tv = &app.threads[t];
                // A singleton (no header) renders flush-left like the flat list;
                // a child under an expanded header is indented.
                let indent = tv.thread.messages.len() > 1;
                child_line(&tv.thread.messages[m], indent, inner_width)
            }
        })
        .map(ListItem::new)
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(Color::Blue).fg(Color::White));

    let mut state = ListState::default();
    if !app.visible_rows.is_empty() {
        state.select(Some(app.row_index.min(app.visible_rows.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
}

/// A conversation header row.
fn header_line(t: &mailcore::thread::Thread, expanded: bool, width: usize) -> Line<'static> {
    let chevron = if expanded { "▾" } else { "▸" };
    let flagged = if t.any_flagged { "!" } else { " " };
    let attached = if t.any_attachments { "@" } else { " " };
    let time = short_time(&t.latest_received); // same-module private fn
    let who = t.participants.join(", ");
    let text = format!(
        "{chevron}{flagged}{attached} {time}  ({}) {} — {}",
        t.messages.len(),
        who,
        t.subject
    );
    let truncated = truncate_width(&text, width);
    let mut style = Style::default();
    if t.unread_count > 0 {
        style = style.add_modifier(Modifier::BOLD);
    }
    Line::from(Span::styled(truncated, style))
}

/// A single message row: indented when it's a child under an expanded header,
/// flush-left when it's a singleton conversation.
fn child_line(m: &MessageRow, indent: bool, width: usize) -> Line<'static> {
    let flagged = if m.is_flagged { "!" } else { " " };
    let attached = if m.has_attachments { "@" } else { " " };
    let time = short_time(&m.received_at);
    let pad = if indent { "    " } else { "" };
    let subject_or_preview = if m.subject.is_empty() { &m.preview } else { &m.subject };
    let text = format!("{pad}{flagged}{attached} {time}  {} — {}", m.from_name, subject_or_preview);
    let truncated = truncate_width(&text, width);
    let mut style = Style::default();
    if !m.is_read {
        style = style.add_modifier(Modifier::BOLD);
    }
    Line::from(Span::styled(truncated, style))
}
```

`short_time`, `truncate_width`, `border_style`, `Modifier`, `Span`, `Line`, `Style`, `Color`, `Block`, `Borders`, `List`, `ListItem`, `ListState`, and `MessageRow` are all already in scope in `message_list.rs` (from its existing `use` lines). `mailcore::thread::Thread` is referenced fully-qualified in the `header_line` signature, so no new `use` is required.

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy threaded_view_renders`
Expected: PASS. Then `bash "$LCARGO" test -p lookxy` — all PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/ui/message_list.rs
git commit -m "lookxy: threaded folder-view rendering (headers + children)"
```

---

### Task 8: Whole-conversation mark/flag (immediate)

**Files:**
- Modify: `lookxy/src/app.rs` (`mark_read`, `toggle_flag`)

**Interfaces:**
- Consumes: `selected_row`, `threads`, `store.set_read`/`set_flag`, `SyncCommand::MarkRead`/`SetFlag`.
- Produces: `mark_read`/`toggle_flag` act on the whole conversation when the cursor is on a collapsed header in threaded mode; on a message row (or in flat mode) they act on that one message as before.

- [ ] **Step 1: Write the failing test**

Add to `app.rs` tests (the test harness captures sent commands via `test_cmd_rx`; drain them like the existing `last_sent_command_is_mark_read` test does):

```rust
    #[test]
    fn mark_read_on_a_collapsed_header_marks_the_whole_thread() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = m1 + m2, both unread
        app.reload_messages();
        let pos = app.visible_rows.iter().position(|r| matches!(r, Row::Header(_))).unwrap();
        app.row_index = pos;

        app.mark_read(true);

        // Both messages are now read in the store-backed (reloaded) thread...
        let hpos = app.visible_rows.iter().position(|r| matches!(r, Row::Header(_))).unwrap();
        if let Row::Header(t) = app.visible_rows[hpos] {
            assert!(app.threads[t].thread.messages.iter().all(|m| m.is_read));
        }
        // ...and one MarkRead command was enqueued per message.
        let mut count = 0;
        while let Ok(SyncCommand::MarkRead { read: true, .. }) =
            app.test_cmd_rx.as_ref().unwrap().try_recv()
        {
            count += 1;
        }
        assert_eq!(count, 2);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p lookxy mark_read_on_a_collapsed_header`
Expected: FAIL — current `mark_read` acts only on `messages[msg_index]` (0 commands in threaded mode).

- [ ] **Step 3: Write the implementation**

Add a helper to `impl App` that yields the message ids the current threaded row targets (whole thread for a collapsed header; the single message for a message row):

```rust
    /// The message ids a triage key targets in threaded mode: every message
    /// of the conversation when the cursor is on a (collapsed or expanded)
    /// header, or the single message when it's on a message row. `None` in
    /// flat mode (callers fall back to the flat single-message path).
    fn threaded_target_ids(&self) -> Option<Vec<String>> {
        if !self.threaded_active() {
            return None;
        }
        match self.selected_row()? {
            Row::Header(t) => Some(
                self.threads[t]
                    .thread
                    .messages
                    .iter()
                    .map(|m| m.id.clone())
                    .collect(),
            ),
            Row::Message(t, m) => {
                self.threads[t].thread.messages.get(m).map(|msg| vec![msg.id.clone()])
            }
        }
    }
```

Rewrite `mark_read` and `toggle_flag` to branch on that:

```rust
    pub fn mark_read(&mut self, read: bool) {
        if let Some(ids) = self.threaded_target_ids() {
            for id in &ids {
                self.store.set_read(id, read);
                let _ = self.sync.cmd_tx.send(SyncCommand::MarkRead { id: id.clone(), read });
            }
            self.reload_messages();
            return;
        }
        let Some(id) = self.highlighted_message_id() else {
            return;
        };
        self.store.set_read(&id, read);
        self.reload_messages();
        let _ = self.sync.cmd_tx.send(SyncCommand::MarkRead { id, read });
    }

    pub fn toggle_flag(&mut self) {
        if let Some(ids) = self.threaded_target_ids() {
            // Flag the whole thread ON if any is currently unflagged, else clear
            // it — so one keypress makes the thread's flag state uniform.
            let want = match self.selected_row() {
                Some(Row::Header(t)) => !self.threads[t].thread.any_flagged,
                Some(Row::Message(t, m)) => {
                    !self.threads[t].thread.messages[m].is_flagged
                }
                None => return,
            };
            for id in &ids {
                self.store.set_flag(id, want);
                let _ = self.sync.cmd_tx.send(SyncCommand::SetFlag { id: id.clone(), flagged: want });
            }
            self.reload_messages();
            return;
        }
        let Some(row) = self.messages.get(self.msg_index) else {
            return;
        };
        let id = row.id.clone();
        let flagged = !row.is_flagged;
        self.store.set_flag(&id, flagged);
        self.reload_messages();
        let _ = self.sync.cmd_tx.send(SyncCommand::SetFlag { id, flagged });
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy`
Expected: new test PASS; existing flat-mode mark/flag tests PASS (flat path unchanged).

- [ ] **Step 5: fmt, clippy, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p lookxy --all-targets -- -D warnings
git add lookxy/src/app.rs
git commit -m "lookxy: whole-conversation mark/flag in threaded view"
```

---

### Task 9: Confirm modal + whole-conversation delete & move

**Files:**
- Modify: `lookxy/src/app.rs` (ConfirmModal state, `delete_selected`, thread-aware move, confirm execution)
- Modify: `lookxy/src/ui/mod.rs` (`handle_key` confirm branch)
- Modify: `lookxy/src/ui/message_list.rs` (draw the confirm modal)

**Interfaces:**
- Consumes: `threaded_target_ids`, `store.delete_message`/`move_message`, `SyncCommand::Delete`/`Move`, `centered_rect`, `Clear`.
- Produces:
  - `pub struct ConfirmModal { pub prompt: String, pub action: ConfirmAction }`
  - `pub enum ConfirmAction { DeleteThread(Vec<String>), MoveThread(Vec<String>, String) }`
  - `pub confirm: Option<ConfirmModal>` field on `App`
  - `pub fn confirm_yes(&mut self)`, `pub fn cancel_confirm(&mut self)`
  - `delete_selected` opens the modal for a multi-message thread; the existing move flow (`confirm_move`) routes a multi-message thread through the modal.

- [ ] **Step 1: Write the failing test**

Add to `app.rs` tests:

```rust
    #[test]
    fn deleting_a_thread_confirms_then_deletes_every_message() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = m1 + m2
        app.reload_messages();
        let pos = app.visible_rows.iter().position(|r| matches!(r, Row::Header(_))).unwrap();
        app.row_index = pos;

        // First `d` only opens the confirm modal — nothing deleted yet.
        app.delete_selected();
        assert!(app.confirm.is_some());
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());

        // Confirming deletes all messages and enqueues one Delete per message.
        app.confirm_yes();
        assert!(app.confirm.is_none());
        let mut count = 0;
        while let Ok(SyncCommand::Delete { .. }) = app.test_cmd_rx.as_ref().unwrap().try_recv() {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn canceling_the_confirm_deletes_nothing() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app);
        app.reload_messages();
        let pos = app.visible_rows.iter().position(|r| matches!(r, Row::Header(_))).unwrap();
        app.row_index = pos;
        app.delete_selected();
        app.cancel_confirm();
        assert!(app.confirm.is_none());
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash "$LCARGO" test -p lookxy deleting_a_thread canceling_the_confirm`
Expected: FAIL — `no field confirm` / `no method confirm_yes`.

- [ ] **Step 3: Write the implementation**

Add the types (near `ThreadView`/`Row` in `app.rs`):

```rust
/// A pending destructive confirmation (whole-conversation delete/move).
pub struct ConfirmModal {
    pub prompt: String,
    pub action: ConfirmAction,
}

pub enum ConfirmAction {
    DeleteThread(Vec<String>),
    MoveThread(Vec<String>, String), // (message ids, destination folder id)
}
```

Add the field to `App` and init `confirm: None` in `App::new`:

```rust
    /// A pending destructive-action confirmation, if any (whole-thread delete
    /// or move). `Some` blocks other keys until answered — see `ui::handle_key`.
    pub confirm: Option<ConfirmModal>,
```

Add a helper to build the "(incl. N in Sent)" wording. Find the Sent folder id via the store's folders (well-known `sentitems`):

```rust
    /// A human count like `5 messages (incl. 2 in Sent)` for a confirm prompt.
    fn describe_thread_scope(&self, ids: &[String]) -> String {
        let sent_id = self
            .store
            .folders()
            .unwrap_or_default()
            .into_iter()
            .find(|f| f.well_known_name.as_deref() == Some("sentitems"))
            .map(|f| f.id);
        let in_sent = match (&sent_id, self.selected_row()) {
            (Some(sid), Some(Row::Header(t))) => self.threads[t]
                .thread
                .messages
                .iter()
                .filter(|m| &m.folder_id == sid)
                .count(),
            _ => 0,
        };
        let n = ids.len();
        if in_sent > 0 {
            format!("{n} messages (incl. {in_sent} in Sent)")
        } else {
            format!("{n} messages")
        }
    }
```

Rewrite `delete_selected` to open the modal for a multi-message thread, else delete one message as today:

```rust
    pub fn delete_selected(&mut self) {
        if let Some(ids) = self.threaded_target_ids() {
            if ids.len() > 1 {
                let prompt = format!("Delete {}?", self.describe_thread_scope(&ids));
                self.confirm = Some(ConfirmModal {
                    prompt,
                    action: ConfirmAction::DeleteThread(ids),
                });
                return;
            }
            // A singleton / single message row: delete directly.
            if let Some(id) = ids.into_iter().next() {
                let _ = self.store.delete_message(&id);
                self.reload_messages();
                let _ = self.sync.cmd_tx.send(SyncCommand::Delete { id });
            }
            return;
        }
        let Some(id) = self.highlighted_message_id() else {
            return;
        };
        let _ = self.store.delete_message(&id);
        self.reload_messages();
        let _ = self.sync.cmd_tx.send(SyncCommand::Delete { id });
    }

    /// Esc on the confirm modal: dismiss it, doing nothing.
    pub fn cancel_confirm(&mut self) {
        self.confirm = None;
    }

    /// Enter on the confirm modal: carry out the pending action (per-message
    /// optimistic store write + `SyncCommand`), then close it and reload.
    pub fn confirm_yes(&mut self) {
        let Some(modal) = self.confirm.take() else {
            return;
        };
        match modal.action {
            ConfirmAction::DeleteThread(ids) => {
                for id in ids {
                    let _ = self.store.delete_message(&id);
                    let _ = self.sync.cmd_tx.send(SyncCommand::Delete { id });
                }
            }
            ConfirmAction::MoveThread(ids, dest) => {
                for id in ids {
                    if self.store.move_message(&id, &dest).is_ok() {
                        let _ = self.sync.cmd_tx.send(SyncCommand::Move { id, dest: dest.clone() });
                    }
                }
            }
        }
        self.reload_messages();
    }
```

Route a threaded move through the modal. In `confirm_move` (the move-picker's Enter handler, ~line 731), after computing `dest`, branch on a threaded header target:

```rust
    pub fn confirm_move(&mut self) {
        let Some(picker) = self.move_picker.take() else {
            return;
        };
        let Some(dest) = picker.folders.get(picker.index).map(|f| f.id.clone()) else {
            return;
        };
        // In threaded mode with a multi-message conversation selected, confirm
        // the whole-thread move; otherwise move the single captured message.
        if let Some(ids) = self.threaded_target_ids() {
            if ids.len() > 1 {
                let scope = self.describe_thread_scope(&ids);
                self.confirm = Some(ConfirmModal {
                    prompt: format!("Move {scope} to this folder?"),
                    action: ConfirmAction::MoveThread(ids, dest),
                });
                return;
            }
        }
        if self.store.move_message(&picker.message_id, &dest).is_ok() {
            self.reload_messages();
            let _ = self.sync.cmd_tx.send(SyncCommand::Move { id: picker.message_id, dest });
        }
    }
```

> Note: `open_move_picker` currently captures `highlighted_message_id()` (flat). In threaded mode `message_id` may be empty/wrong, but the threaded branch above uses `threaded_target_ids()` and ignores `picker.message_id`, so a multi-message thread is handled entirely by the modal. For a threaded singleton/message row, `threaded_target_ids()` returns one id; set `open_move_picker` to capture that id in threaded mode: replace its `highlighted_message_id()` call with `self.threaded_target_ids().and_then(|v| v.into_iter().next()).or_else(|| self.highlighted_message_id())`.

Wire the modal into key handling. In `lookxy/src/ui/mod.rs` `handle_key`, add a branch BEFORE the `match key.code` (after the search branch, before line 123):

```rust
    if app.confirm.is_some() {
        match key.code {
            KeyCode::Enter => app.confirm_yes(),
            KeyCode::Esc => app.cancel_confirm(),
            _ => {}
        }
        return;
    }
```

Draw the modal. In `lookxy/src/ui/message_list.rs`, add a `draw_confirm` and call it from `ui::mod::draw` (add `message_list::draw_confirm(f, app);` right after `message_list::draw_move_picker(f, app);` at ~line 72):

```rust
/// Renders the destructive-action confirmation as a centered overlay when
/// `app.confirm` is set; a no-op otherwise. Enter confirms, Esc cancels
/// (see `ui::handle_key`).
pub fn draw_confirm(f: &mut Frame, app: &App) {
    let Some(modal) = &app.confirm else {
        return;
    };
    let area = centered_rect(50, 20, f.area());
    f.render_widget(Clear, area);
    let text = format!("{}\n\n[Enter] confirm    [Esc] cancel", modal.prompt);
    let para = ratatui::widgets::Paragraph::new(text)
        .block(
            Block::default()
                .title("Confirm")
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::Yellow)),
        )
        .wrap(ratatui::widgets::Wrap { trim: true });
    f.render_widget(para, area);
}
```

- [ ] **Step 4: Run to verify pass**

Run: `bash "$LCARGO" test -p lookxy`
Expected: new tests PASS; all existing PASS.

- [ ] **Step 5: fmt, clippy, full workspace check, commit**

```bash
bash "$LCARGO" fmt
bash "$LCARGO" clippy -p mailcore -p lookxy --all-targets -- -D warnings
bash "$LCARGO" test -p mailcore -p lookxy
git add lookxy/src/app.rs lookxy/src/ui/mod.rs lookxy/src/ui/message_list.rs
git commit -m "lookxy: confirm modal + whole-conversation delete/move"
```

---

## Shared test seed helpers

`App::for_test_with_seeded_store()` already seeds folder `inbox` (selected) with one message: `id "m1"`, `conversation_id "c1"`, from `Alice`, subject `"Hello"`, received `2026-07-16T10:00:00Z`, **unread**. The threaded tests below build on that. Add these two helpers to the `app.rs` `#[cfg(test)] mod tests` module once (Task 4), and the later tasks reuse them:

```rust
    /// Adds a second message to conversation `c1` (m1's conversation) so `c1`
    /// becomes a 2-message thread: from Bob, newer than m1, unread.
    fn seed_second_in_c1(app: &App) {
        use mailcore::graph::model::{Message, Recipient};
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m2".into(),
                    conversation_id: "c1".into(),
                    subject: "Re: Hello".into(),
                    from: Recipient { name: "Bob".into(), address: "bob@example.com".into() },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-16T11:00:00Z".into(), // newer than m1 (10:00)
                    sent: "".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "re hi".into(),
                    is_draft: false,
                },
            )
            .expect("seed m2");
    }

    /// Adds a standalone message in its own conversation `c2` (a singleton).
    fn seed_singleton_c2(app: &App) {
        use mailcore::graph::model::{Message, Recipient};
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m3".into(),
                    conversation_id: "c2".into(),
                    subject: "Standalone".into(),
                    from: Recipient { name: "Carol".into(), address: "carol@example.com".into() },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-15T10:00:00Z".into(),
                    sent: "".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "alone".into(),
                    is_draft: false,
                },
            )
            .expect("seed m3");
    }
```

After `seed_second_in_c1`, conversation `c1` = [m1 (Alice, 10:00, unread), m2 (Bob, 11:00, unread)]: 2 messages, participants `[Alice, Bob]`, subject `"Re: Hello"` (latest non-empty), `unread_count 2`, latest `2026-07-16T11:00:00Z`. `seed_singleton_c2` adds a 1-message thread `c2`.

## Notes for the implementer

- **Do not weaken an assertion to make a test pass.** If a threaded assertion fails, fix the branch, not the test.
- **`Mode` import:** `threaded_active` references `Mode::Mail`; `app.rs` already uses `Mode` for its `mode` field, so it's in scope.
- **Flat mode is the safety net:** every task keeps the flat path intact and reachable (`threaded == false`, or search active). If any existing flat-mode test breaks, the change leaked into the flat path — fix the branch, don't edit the test.
