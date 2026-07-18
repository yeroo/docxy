//! The local SQLite mail store — the single source of truth for the TUI.
//!
//! The sync engine writes Graph data in here via
//! `upsert_folder`/`upsert_message`/delta-link bookkeeping; the TUI reads
//! only from here (offline-first). This module owns the schema (see
//! `schema.rs`) and the folders/messages/meta surface, plus bodies,
//! attachments, full-text search, and the outbox queue (triage mutations
//! queued here for `sync::outbox::apply_op` to push to Graph later).

use std::path::Path;

use rusqlite::{Connection, Row, params};

use crate::graph::model::{AttachmentMeta, Body, MailFolder, Message, Recipient};
use crate::json::{self, Value};

mod schema;

/// A local SQLite store error. Wraps `rusqlite::Error` behind a small,
/// crate-local type so callers don't need to depend on `rusqlite` directly.
#[derive(Debug)]
pub enum StoreError {
    Sql(String),
    /// A stored `outbox.payload` value wasn't valid JSON, or wasn't a
    /// recognized `OutboxOp` shape. This should only happen if the row was
    /// written by something other than `enqueue_op`.
    Decode(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sql(msg) => write!(f, "sqlite error: {msg}"),
            StoreError::Decode(msg) => write!(f, "outbox decode error: {msg}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sql(e.to_string())
    }
}

/// A `folders` row, as read back from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderRow {
    pub id: String,
    pub parent_id: Option<String>,
    pub display_name: String,
    pub total_count: i64,
    pub unread_count: i64,
    pub delta_link: Option<String>,
    pub well_known_name: Option<String>,
    pub sort_order: Option<i64>,
}

/// A `messages` row, as read back from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRow {
    pub id: String,
    pub folder_id: String,
    pub conversation_id: String,
    pub subject: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_recipients: String,
    pub cc_recipients: String,
    pub received_at: String,
    pub sent_at: String,
    pub is_read: bool,
    pub is_flagged: bool,
    pub has_attachments: bool,
    pub importance: String,
    pub preview: String,
    pub is_draft: bool,
}

/// A queued local mutation, awaiting a push to Microsoft Graph by
/// `sync::outbox::apply_op`. Serializes to/from JSON via `to_json`/
/// `from_json` (`crate::json`, no `serde`) as `{"kind":"...","id":"...",...}`
/// — `kind` is the tag (`markRead`/`setFlag`/`move`/`delete`), the rest are
/// this variant's fields.
#[derive(Debug, Clone, PartialEq)]
pub enum OutboxOp {
    MarkRead { id: String, read: bool },
    SetFlag { id: String, flagged: bool },
    Move { id: String, dest: String },
    Delete { id: String },
}

impl OutboxOp {
    /// The `kind` tag stored alongside the JSON payload (used for the
    /// `outbox.op` column, which is `NOT NULL`; the payload itself is the
    /// source of truth when reading a row back).
    fn kind(&self) -> &'static str {
        match self {
            OutboxOp::MarkRead { .. } => "markRead",
            OutboxOp::SetFlag { .. } => "setFlag",
            OutboxOp::Move { .. } => "move",
            OutboxOp::Delete { .. } => "delete",
        }
    }

    /// Serializes this op to a JSON `Value` (`.to_string()` for the wire/
    /// storage form — see the module-level docs on `crate::json::Value`).
    pub fn to_json(&self) -> Value {
        match self {
            OutboxOp::MarkRead { id, read } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                ("read".to_string(), Value::Bool(*read)),
            ]),
            OutboxOp::SetFlag { id, flagged } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                ("flagged".to_string(), Value::Bool(*flagged)),
            ]),
            OutboxOp::Move { id, dest } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                ("dest".to_string(), Value::Str(dest.clone())),
            ]),
            OutboxOp::Delete { id } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
            ]),
        }
    }

    /// Parses a JSON `Value` (as produced by `to_json`) back into an
    /// `OutboxOp`. Returns `None` on an unrecognized `kind` or missing/
    /// mistyped fields, rather than panicking on a corrupt row.
    pub fn from_json(v: &Value) -> Option<OutboxOp> {
        let kind = v.get("kind")?.as_str()?;
        let id = || v.get("id").and_then(Value::as_str).map(str::to_string);
        match kind {
            "markRead" => Some(OutboxOp::MarkRead {
                id: id()?,
                read: v.get("read")?.as_bool()?,
            }),
            "setFlag" => Some(OutboxOp::SetFlag {
                id: id()?,
                flagged: v.get("flagged")?.as_bool()?,
            }),
            "move" => Some(OutboxOp::Move {
                id: id()?,
                dest: v.get("dest")?.as_str()?.to_string(),
            }),
            "delete" => Some(OutboxOp::Delete { id: id()? }),
            _ => None,
        }
    }
}

/// An `outbox` row, as read back from the store: the op to apply, its
/// queue position (`seq`, for ordering), and how many times applying it has
/// already failed (`attempts`, bumped by `bump_op_attempts`).
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxRow {
    pub seq: i64,
    pub op: OutboxOp,
    pub attempts: i64,
}

/// The local mail database. Single-threaded access is assumed (the sync
/// engine and TUI serialize their access through it); no internal locking.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) the database at `path` and brings it up
    /// to the current schema.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Opens an in-memory database — for tests only.
    pub fn open_in_memory() -> Result<Store, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Store, StoreError> {
        // WAL is a no-op on an in-memory database (SQLite silently keeps
        // it at MEMORY); harmless to request unconditionally.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(schema::SCHEMA_SQL)?;
        // `SCHEMA_SQL`'s `CREATE TABLE IF NOT EXISTS messages` already
        // includes `is_draft` for freshly-created databases; this
        // `ALTER TABLE` brings an *existing* database (created before this
        // column existed) up to date. It errors ("duplicate column name")
        // on every database that already has the column — i.e. every fresh
        // one, and every one already migrated — so the failure is expected
        // and swallowed rather than propagated.
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN is_draft INTEGER NOT NULL DEFAULT 0",
            [],
        );
        Ok(Store { conn })
    }

    /// Inserts or updates a folder row by `id`.
    pub fn upsert_folder(&self, f: &MailFolder) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO folders (id, parent_id, display_name, total_count, unread_count, well_known_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 parent_id = excluded.parent_id,
                 display_name = excluded.display_name,
                 total_count = excluded.total_count,
                 unread_count = excluded.unread_count,
                 well_known_name = excluded.well_known_name",
            params![
                f.id,
                f.parent_id,
                f.display_name,
                f.total_count,
                f.unread_count,
                f.well_known_name,
            ],
        )?;
        Ok(())
    }

    /// All folders, well-known ones first (Inbox, Drafts, Sent, Deleted,
    /// Junk, Archive, in that order), then everything else alphabetically.
    pub fn folders(&self) -> Result<Vec<FolderRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, display_name, total_count, unread_count, delta_link, well_known_name, sort_order
             FROM folders
             ORDER BY CASE well_known_name
                 WHEN 'inbox' THEN 0
                 WHEN 'drafts' THEN 1
                 WHEN 'sentitems' THEN 2
                 WHEN 'deleteditems' THEN 3
                 WHEN 'junkemail' THEN 4
                 WHEN 'archive' THEN 5
                 ELSE 99
             END, display_name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(FolderRow {
                    id: row.get(0)?,
                    parent_id: row.get(1)?,
                    display_name: row.get(2)?,
                    total_count: row.get(3)?,
                    unread_count: row.get(4)?,
                    delta_link: row.get(5)?,
                    well_known_name: row.get(6)?,
                    sort_order: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Inserts or updates a message row, filing it under `folder_id`.
    pub fn upsert_message(&self, folder_id: &str, m: &Message) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO messages (
                 id, folder_id, conversation_id, subject, from_name, from_addr,
                 to_recipients, cc_recipients, received_at, sent_at,
                 is_read, is_flagged, has_attachments, importance, preview, is_draft
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(id) DO UPDATE SET
                 folder_id = excluded.folder_id,
                 conversation_id = excluded.conversation_id,
                 subject = excluded.subject,
                 from_name = excluded.from_name,
                 from_addr = excluded.from_addr,
                 to_recipients = excluded.to_recipients,
                 cc_recipients = excluded.cc_recipients,
                 received_at = excluded.received_at,
                 sent_at = excluded.sent_at,
                 is_read = excluded.is_read,
                 is_flagged = excluded.is_flagged,
                 has_attachments = excluded.has_attachments,
                 importance = excluded.importance,
                 preview = excluded.preview,
                 is_draft = excluded.is_draft",
            params![
                m.id,
                folder_id,
                m.conversation_id,
                m.subject,
                m.from.name,
                m.from.address,
                encode_recipients(&m.to),
                encode_recipients(&m.cc),
                m.received,
                m.sent,
                m.is_read,
                m.is_flagged,
                m.has_attachments,
                m.importance,
                m.preview,
                m.is_draft,
            ],
        )?;
        Ok(())
    }

    /// Deletes a message by id. Affects zero rows (not an error) if the id
    /// doesn't match anything — including an empty id, which a delta
    /// `@removed` entry lacking `id` can produce (see `graph::model`).
    pub fn delete_message(&self, id: &str) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM messages WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Messages in `folder_id`, newest received first.
    pub fn messages_in_folder(
        &self,
        folder_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<MessageRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, folder_id, conversation_id, subject, from_name, from_addr,
                    to_recipients, cc_recipients, received_at, sent_at,
                    is_read, is_flagged, has_attachments, importance, preview, is_draft
             FROM messages
             WHERE folder_id = ?1
             ORDER BY received_at DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt
            .query_map(params![folder_id, limit, offset], map_message_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Locally marks a message read/unread. This is a local-only mutation
    /// of the row (the sync engine's outbox is what tells Graph); it's
    /// deliberately infallible-looking (no `Result`) since a mismatched or
    /// already-gone `id` just means zero rows change, not an error worth
    /// propagating to callers.
    pub fn set_read(&self, id: &str, read: bool) {
        let _ = self.conn.execute(
            "UPDATE messages SET is_read = ?1 WHERE id = ?2",
            params![read, id],
        );
    }

    /// Locally sets/clears the flagged state of a message. See `set_read`
    /// for why this doesn't return a `Result`.
    pub fn set_flag(&self, id: &str, flagged: bool) {
        let _ = self.conn.execute(
            "UPDATE messages SET is_flagged = ?1 WHERE id = ?2",
            params![flagged, id],
        );
    }

    /// Locally re-files a message under `dest_folder_id` — the optimistic
    /// half of a triage move (the sync engine's outbox is what tells Graph).
    /// Unlike `set_read`/`set_flag` this returns a `Result`: the sync engine
    /// applies it in the same spot it enqueues the outbox op, where a store
    /// failure is worth surfacing. A mismatched/already-gone `id` just changes
    /// zero rows (not an error). Graph mints a *new* id on move, but that's
    /// reconciled by the next delta (old id `@removed`, new id added), so the
    /// local id is left as-is here.
    pub fn move_message(&self, id: &str, dest_folder_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE messages SET folder_id = ?2 WHERE id = ?1",
            params![id, dest_folder_id],
        )?;
        Ok(())
    }

    /// The stored delta link for a folder, if any (`None` before the first
    /// sync, or if the folder doesn't exist).
    pub fn get_delta_link(&self, folder_id: &str) -> Result<Option<String>, StoreError> {
        let link = self
            .conn
            .query_row(
                "SELECT delta_link FROM folders WHERE id = ?1",
                params![folder_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        Ok(link)
    }

    /// Stores the delta link for a folder (used to resume delta sync).
    pub fn set_delta_link(&self, folder_id: &str, link: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET delta_link = ?1 WHERE id = ?2",
            params![link, folder_id],
        )?;
        Ok(())
    }

    /// Clears every folder's stored delta link, so the next sync re-fetches
    /// each folder from scratch (a fresh `DeltaCursor::Folder`) rather than
    /// resuming from a token. Used by the sync engine to reconverge the local
    /// store with server truth after it has quarantined a bad outbox op (whose
    /// optimistic local write would otherwise linger).
    pub fn clear_delta_links(&self) -> Result<(), StoreError> {
        self.conn
            .execute("UPDATE folders SET delta_link = NULL", [])?;
        Ok(())
    }

    /// Inserts or replaces the body of a message. The schema's `bodies_fts_*`
    /// triggers (see `schema.rs`) keep `messages_fts` in step with this
    /// automatically, so `search` sees the new content right away.
    pub fn put_body(&self, message_id: &str, b: &Body) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO bodies (message_id, content_type, content) VALUES (?1, ?2, ?3)
             ON CONFLICT(message_id) DO UPDATE SET
                 content_type = excluded.content_type,
                 content = excluded.content",
            params![message_id, b.content_type, b.content],
        )?;
        Ok(())
    }

    /// The stored body of a message, if any (`None` before `put_body` is
    /// ever called for it).
    pub fn get_body(&self, message_id: &str) -> Result<Option<Body>, StoreError> {
        let body = self
            .conn
            .query_row(
                "SELECT content_type, content FROM bodies WHERE message_id = ?1",
                params![message_id],
                |row| {
                    Ok(Body {
                        content_type: row.get(0)?,
                        content: row.get(1)?,
                    })
                },
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        Ok(body)
    }

    /// Creates a new draft entirely locally — no Graph round-trip — so
    /// compose can start editing immediately, even offline. Mints a
    /// `local:<hex>` id (see `local_draft_id`), files the message under the
    /// Drafts folder (resolved by `well_known_name = 'drafts'`; see
    /// `drafts_folder_id` for what happens if that folder hasn't synced
    /// yet), marks it `is_draft = 1`, and stores `body_html` as its body.
    /// Returns the minted id so the caller (compose) can address this draft
    /// with `update_draft_fields`/`draft` until `reconcile_id` swaps it for
    /// the real Graph id.
    pub fn create_local_draft(
        &self,
        subject: &str,
        to: &str,
        cc: &str,
        body_html: &str,
    ) -> Result<String, StoreError> {
        let id = local_draft_id();
        let folder_id = self.drafts_folder_id()?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO messages (
                 id, folder_id, conversation_id, subject, from_name, from_addr,
                 to_recipients, cc_recipients, received_at, sent_at,
                 is_read, is_flagged, has_attachments, importance, preview, is_draft
             ) VALUES (?1, ?2, '', ?3, '', '', ?4, ?5, '', '', 1, 0, 0, 'normal', '', 1)",
            params![id, folder_id, subject, to, cc],
        )?;
        tx.execute(
            "INSERT INTO bodies (message_id, content_type, content) VALUES (?1, 'html', ?2)",
            params![id, body_html],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Updates a draft's editable fields in place (subject, recipients, and
    /// body) — the store side of "compose autosaves as you type". `id` is
    /// whatever `create_local_draft` returned (a `local:` id before the
    /// draft has synced, or the reconciled Graph id after). A mismatched/
    /// already-gone `id` changes zero rows rather than erroring, matching
    /// `set_read`/`set_flag`'s convention for local mutations of a row that
    /// might have raced with something else — except here the body write
    /// is real work worth reporting failure on, so this does return a
    /// `Result`.
    pub fn update_draft_fields(
        &self,
        id: &str,
        subject: &str,
        to: &str,
        cc: &str,
        body_html: &str,
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE messages SET subject = ?2, to_recipients = ?3, cc_recipients = ?4
             WHERE id = ?1",
            params![id, subject, to, cc],
        )?;
        tx.execute(
            "INSERT INTO bodies (message_id, content_type, content) VALUES (?1, 'html', ?2)
             ON CONFLICT(message_id) DO UPDATE SET content = excluded.content",
            params![id, body_html],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Rewrites every row keyed by `local_id` (the message, its body, and —
    /// via the `messages`/`bodies` triggers described in `schema.rs` — the
    /// `messages_fts` index) to `graph_id`, once `create_draft` has pushed
    /// the local draft to Graph and gotten back its real id. Runs as one
    /// transaction with `defer_foreign_keys` turned on for its duration: the
    /// `messages` row's id (the parent key `bodies.message_id` and
    /// `attachments.message_id` reference) changes first, which would
    /// otherwise trip the `FOREIGN KEY` check immediately (there's no
    /// `ON UPDATE CASCADE` — only `ON DELETE CASCADE` — on those columns),
    /// since the child rows still point at the old id until the very next
    /// statement fixes them up. Deferring means the check runs once, at
    /// `commit()`, by which point every row agrees.
    pub fn reconcile_id(&self, local_id: &str, graph_id: &str) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        tx.execute(
            "UPDATE messages SET id = ?2 WHERE id = ?1",
            params![local_id, graph_id],
        )?;
        tx.execute(
            "UPDATE bodies SET message_id = ?2 WHERE message_id = ?1",
            params![local_id, graph_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Loads a draft (its message row and body) for editing, by whatever id
    /// currently addresses it (`local:` before sync, the Graph id after
    /// `reconcile_id`). `None` if there's no message with that id, or no
    /// body stored for it — either of which means there's nothing for
    /// compose to load.
    pub fn draft(&self, id: &str) -> Result<Option<(MessageRow, Body)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, folder_id, conversation_id, subject, from_name, from_addr,
                    to_recipients, cc_recipients, received_at, sent_at,
                    is_read, is_flagged, has_attachments, importance, preview, is_draft
             FROM messages
             WHERE id = ?1",
        )?;
        let row = stmt
            .query_map(params![id], map_message_row)?
            .next()
            .transpose()?;
        let Some(row) = row else {
            return Ok(None);
        };
        let Some(body) = self.get_body(id)? else {
            return Ok(None);
        };
        Ok(Some((row, body)))
    }

    /// Resolves the Drafts folder's id, for filing a new local draft.
    ///
    /// The normal case: the Drafts folder has already synced down from
    /// Graph, so it's a row with `well_known_name = 'drafts'` — return its
    /// real id.
    ///
    /// The cold-start case: compose runs before the first folder sync has
    /// happened, so no such row exists yet. Rather than fail (or block
    /// drafting on a network round-trip), file the draft under a stable
    /// local sentinel folder id, creating a placeholder `folders` row for
    /// it if needed (a real row has to exist for the `messages.folder_id`
    /// foreign key to accept the insert). This sentinel is *not* marked
    /// `well_known_name = 'drafts'`, so once the real sync happens the two
    /// don't collide — but nothing here re-files the sentinel's messages
    /// into the real Drafts folder afterward; that reconciliation is left
    /// to a later task (see the Task 6 report).
    fn drafts_folder_id(&self) -> Result<String, StoreError> {
        let existing = self.conn.query_row(
            "SELECT id FROM folders WHERE well_known_name = 'drafts' LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        );
        match existing {
            Ok(id) => Ok(id),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                self.conn.execute(
                    "INSERT OR IGNORE INTO folders
                         (id, parent_id, display_name, total_count, unread_count, well_known_name)
                     VALUES (?1, NULL, 'Drafts', 0, 0, NULL)",
                    params![LOCAL_DRAFTS_SENTINEL_FOLDER_ID],
                )?;
                Ok(LOCAL_DRAFTS_SENTINEL_FOLDER_ID.to_string())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Replaces the full set of attachment metadata stored for a message
    /// (no bytes — those are fetched separately, later, on demand).
    ///
    /// The delete-then-insert runs inside a single transaction (per spec §5
    /// on multi-row writes): `Store` only holds a shared `&Connection`
    /// (`put_attachments` takes `&self`, not `&mut self`, to match the
    /// rest of this type's methods), so this uses
    /// `Connection::unchecked_transaction` — rusqlite's transaction handle
    /// for exactly that shared-borrow case — rather than
    /// `Connection::transaction`, which needs `&mut Connection`. If any
    /// statement fails, the `?` drops the transaction before `commit()`
    /// runs, which rolls it back and leaves the prior attachments
    /// untouched instead of leaving the row set half-replaced.
    pub fn put_attachments(
        &self,
        message_id: &str,
        atts: &[AttachmentMeta],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM attachments WHERE message_id = ?1",
            params![message_id],
        )?;
        for a in atts {
            tx.execute(
                "INSERT INTO attachments (id, message_id, name, content_type, size, is_inline)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    a.id,
                    message_id,
                    a.name,
                    a.content_type,
                    a.size,
                    a.is_inline
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The attachment metadata stored for a message, ordered by attachment
    /// id (not insertion order).
    pub fn attachments(&self, message_id: &str) -> Result<Vec<AttachmentMeta>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, content_type, size, is_inline
             FROM attachments
             WHERE message_id = ?1
             ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![message_id], |row| {
                Ok(AttachmentMeta {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    content_type: row.get(2)?,
                    size: row.get(3)?,
                    is_inline: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Full-text search over subject, sender, and body (via `messages_fts`),
    /// newest matching message first.
    ///
    /// `query` is free-form user input: it's tokenized on whitespace and
    /// each token is individually quoted as an FTS5 string literal (with
    /// embedded `"` doubled) before being handed to `MATCH`. FTS5 would
    /// otherwise parse bare characters like `"`, `*`, `:`, `(`, `)`, `-`,
    /// `^`, or the bareword operators `AND`/`OR`/`NOT` as query syntax,
    /// so a search like `foo(bar` or `a & b` (an unbalanced paren, a lone
    /// `&`) could throw a syntax error instead of matching literally.
    /// Quoting every token turns it into a plain string match, and the
    /// tokens are still joined with an implicit `AND`, so multi-word
    /// queries keep working as "all of these words appear".
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<MessageRow>, StoreError> {
        let sanitized = sanitize_fts_query(query);
        if sanitized.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, folder_id, conversation_id, subject, from_name, from_addr,
                    to_recipients, cc_recipients, received_at, sent_at,
                    is_read, is_flagged, has_attachments, importance, preview, is_draft
             FROM messages
             WHERE id IN (SELECT message_id FROM messages_fts WHERE messages_fts MATCH ?1)
             ORDER BY received_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![sanitized, limit], map_message_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Queues a local mutation for the sync engine to push to Graph,
    /// returning its `seq` (the `outbox.seq` autoincrement rowid) so a
    /// caller could reference this exact queued op later if needed.
    pub fn enqueue_op(&self, op: &OutboxOp) -> Result<i64, StoreError> {
        let payload = op.to_json().to_string();
        self.conn.execute(
            "INSERT INTO outbox (op, payload) VALUES (?1, ?2)",
            params![op.kind(), payload],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// All queued ops, oldest (lowest `seq`) first — the order the sync
    /// engine should drain them in.
    pub fn pending_ops(&self) -> Result<Vec<OutboxRow>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, payload, attempts FROM outbox ORDER BY seq ASC")?;
        let rows = stmt
            .query_map([], |row| {
                let seq: i64 = row.get(0)?;
                let payload: String = row.get(1)?;
                let attempts: i64 = row.get(2)?;
                Ok((seq, payload, attempts))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(seq, payload, attempts)| {
                let v = json::parse(&payload).map_err(|e| StoreError::Decode(e.to_string()))?;
                let op = OutboxOp::from_json(&v).ok_or_else(|| {
                    StoreError::Decode(format!("unrecognized outbox payload: {payload}"))
                })?;
                Ok(OutboxRow { seq, op, attempts })
            })
            .collect()
    }

    /// Removes a queued op by `seq` — called once it's been applied
    /// successfully. See `set_read` for why this doesn't return a `Result`
    /// (a `seq` that's already gone just means zero rows change).
    pub fn drop_op(&self, seq: i64) {
        let _ = self
            .conn
            .execute("DELETE FROM outbox WHERE seq = ?1", params![seq]);
    }

    /// Records a failed attempt to apply a queued op: increments
    /// `attempts` and stores `err` as `last_error`, for retry backoff and
    /// diagnostics. See `set_read` for why this doesn't return a `Result`.
    pub fn bump_op_attempts(&self, seq: i64, err: &str) {
        let _ = self.conn.execute(
            "UPDATE outbox SET attempts = attempts + 1, last_error = ?1 WHERE seq = ?2",
            params![err, seq],
        );
    }
}

/// The local `folders.id` a new draft is filed under when the real Drafts
/// folder (`well_known_name = 'drafts'`) hasn't synced down from Graph yet.
/// See `Store::drafts_folder_id`.
const LOCAL_DRAFTS_SENTINEL_FOLDER_ID: &str = "local:drafts-pending";

/// Mints a fresh local draft id: `local:` followed by 16 bytes of
/// OS randomness as lowercase hex (reusing `pkce::random_bytes`, the same
/// OS-entropy source the PKCE verifier uses, rather than pulling in a
/// `uuid` crate for this one call site). The `local:` prefix is what marks
/// an id as not-yet-synced-to-Graph throughout the store and sync engine.
fn local_draft_id() -> String {
    format!("local:{}", to_hex(&crate::pkce::random_bytes(16)))
}

/// Lowercase-hex-encodes a byte slice (`format!("{b:02x}")` per byte,
/// concatenated) — used only for `local_draft_id`'s random suffix.
fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Encodes a list of recipients into the flat text form stored in
/// `to_recipients`/`cc_recipients` (`Name <addr>; Name <addr>; ...`).
/// There's no structured decode yet — nothing reads these back as
/// `Recipient`s in this task — but the format is delimiter-safe enough
/// (`;`/`<`/`>`) for the message-list and search use the later tasks need.
fn encode_recipients(list: &[Recipient]) -> String {
    list.iter()
        .map(|r| format!("{} <{}>", r.name, r.address))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Maps one row of a `SELECT id, folder_id, ..., preview, is_draft FROM
/// messages ...` query (that exact column order) to a `MessageRow`. Shared
/// by `messages_in_folder`, `search`, and `draft`, which all select those
/// columns in that order from `messages`, so there's only one place mapping
/// can drift out of sync with the column list.
fn map_message_row(row: &Row) -> rusqlite::Result<MessageRow> {
    Ok(MessageRow {
        id: row.get(0)?,
        folder_id: row.get(1)?,
        conversation_id: row.get(2)?,
        subject: row.get(3)?,
        from_name: row.get(4)?,
        from_addr: row.get(5)?,
        to_recipients: row.get(6)?,
        cc_recipients: row.get(7)?,
        received_at: row.get(8)?,
        sent_at: row.get(9)?,
        is_read: row.get(10)?,
        is_flagged: row.get(11)?,
        has_attachments: row.get(12)?,
        importance: row.get(13)?,
        preview: row.get(14)?,
        is_draft: row.get(15)?,
    })
}

/// Sanitizes free-form user input into an FTS5 `MATCH` query string that
/// can't throw a syntax error: splits on whitespace and wraps each token in
/// double quotes (doubling any embedded `"`), turning operator characters
/// (`*`, `:`, `(`, `)`, `-`, `^`) and bareword operators (`AND`/`OR`/`NOT`)
/// into inert literal text. Tokens stay implicitly `AND`-ed together, so
/// multi-word queries still mean "all of these words". Returns an empty
/// string for input with no tokens (e.g. all whitespace) — callers should
/// treat that as "no results" rather than passing it to `MATCH`.
fn sanitize_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::model::{MailFolder, Message, Recipient};

    fn msg(id: &str, read: bool) -> Message {
        Message {
            id: id.into(),
            conversation_id: "C".into(),
            subject: format!("s{id}"),
            from: Recipient {
                name: "A".into(),
                address: "a@x".into(),
            },
            to: vec![],
            cc: vec![],
            received: format!("2026-07-{id}T00:00:00Z"),
            sent: "".into(),
            is_read: read,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: "p".into(),
            is_draft: false,
        }
    }

    #[test]
    fn upserts_and_lists_messages_newest_first() {
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
        s.upsert_message("F", &msg("10", false)).unwrap();
        s.upsert_message("F", &msg("11", true)).unwrap();
        let rows = s.messages_in_folder("F", 50, 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "11"); // received 07-11 > 07-10
    }

    #[test]
    fn upsert_is_idempotent_and_updates_flags() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder {
            id: "F".into(),
            display_name: "I".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: None,
        })
        .unwrap();
        s.upsert_message("F", &msg("10", false)).unwrap();
        s.set_read("10", true);
        let rows = s.messages_in_folder("F", 50, 0).unwrap();
        assert!(rows[0].is_read);
    }

    #[test]
    fn move_message_refiles_locally() {
        let s = Store::open_in_memory().unwrap();
        for id in ["F", "DEST"] {
            s.upsert_folder(&MailFolder {
                id: id.into(),
                display_name: id.into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: None,
            })
            .unwrap();
        }
        s.upsert_message("F", &msg("10", false)).unwrap();
        s.move_message("10", "DEST").unwrap();
        assert!(s.messages_in_folder("F", 50, 0).unwrap().is_empty());
        assert_eq!(s.messages_in_folder("DEST", 50, 0).unwrap()[0].id, "10");
    }

    #[test]
    fn clear_delta_links_nulls_all_links() {
        let s = Store::open_in_memory().unwrap();
        for id in ["F1", "F2"] {
            s.upsert_folder(&MailFolder {
                id: id.into(),
                display_name: id.into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: None,
            })
            .unwrap();
            s.set_delta_link(id, "LINK").unwrap();
        }
        s.clear_delta_links().unwrap();
        assert!(s.get_delta_link("F1").unwrap().is_none());
        assert!(s.get_delta_link("F2").unwrap().is_none());
    }

    #[test]
    fn delta_link_roundtrips() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder {
            id: "F".into(),
            display_name: "I".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: None,
        })
        .unwrap();
        assert!(s.get_delta_link("F").unwrap().is_none());
        s.set_delta_link("F", "LINK").unwrap();
        assert_eq!(s.get_delta_link("F").unwrap().as_deref(), Some("LINK"));
    }
}

#[cfg(test)]
mod outbox_tests {
    use super::*;
    #[test]
    fn enqueue_and_read_back_in_order() {
        let s = Store::open_in_memory().unwrap();
        s.enqueue_op(&OutboxOp::MarkRead {
            id: "1".into(),
            read: true,
        })
        .unwrap();
        s.enqueue_op(&OutboxOp::Delete { id: "2".into() }).unwrap();
        let ops = s.pending_ops().unwrap();
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0].op, OutboxOp::MarkRead { .. }));
        assert!(matches!(ops[1].op, OutboxOp::Delete { .. }));
    }
    #[test]
    fn drop_removes_op() {
        let s = Store::open_in_memory().unwrap();
        let seq = s
            .enqueue_op(&OutboxOp::SetFlag {
                id: "1".into(),
                flagged: true,
            })
            .unwrap();
        s.drop_op(seq);
        assert!(s.pending_ops().unwrap().is_empty());
    }

    #[test]
    fn bump_op_attempts_increments_and_records_error() {
        let s = Store::open_in_memory().unwrap();
        let seq = s.enqueue_op(&OutboxOp::Delete { id: "1".into() }).unwrap();
        s.bump_op_attempts(seq, "throttled");
        s.bump_op_attempts(seq, "throttled again");
        let ops = s.pending_ops().unwrap();
        assert_eq!(ops[0].attempts, 2);
    }

    #[test]
    fn op_json_matches_expected_wire_shape() {
        assert_eq!(
            OutboxOp::MarkRead {
                id: "1".into(),
                read: true
            }
            .to_json()
            .to_string(),
            r#"{"kind":"markRead","id":"1","read":true}"#
        );
        assert_eq!(
            OutboxOp::SetFlag {
                id: "1".into(),
                flagged: false
            }
            .to_json()
            .to_string(),
            r#"{"kind":"setFlag","id":"1","flagged":false}"#
        );
        assert_eq!(
            OutboxOp::Move {
                id: "1".into(),
                dest: "F2".into()
            }
            .to_json()
            .to_string(),
            r#"{"kind":"move","id":"1","dest":"F2"}"#
        );
        assert_eq!(
            OutboxOp::Delete { id: "1".into() }.to_json().to_string(),
            r#"{"kind":"delete","id":"1"}"#
        );
    }

    #[test]
    fn op_json_round_trips_exactly() {
        let ops = vec![
            OutboxOp::MarkRead {
                id: "M1".into(),
                read: false,
            },
            OutboxOp::SetFlag {
                id: "M2".into(),
                flagged: true,
            },
            OutboxOp::Move {
                id: "M3".into(),
                dest: "Archive".into(),
            },
            OutboxOp::Delete { id: "M4".into() },
        ];
        for op in ops {
            let encoded = op.to_json().to_string();
            let decoded = OutboxOp::from_json(&json::parse(&encoded).unwrap()).unwrap();
            assert_eq!(decoded, op);
        }
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use crate::graph::model::{Body, MailFolder, Message, Recipient};
    // reuse msg() helper pattern from Task 8 tests (duplicate locally)

    #[test]
    fn search_matches_subject_and_body() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder {
            id: "F".into(),
            display_name: "I".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: None,
        })
        .unwrap();
        let mut m = Message {
            id: "1".into(),
            conversation_id: "C".into(),
            subject: "Quarterly budget".into(),
            from: Recipient {
                name: "A".into(),
                address: "a@x".into(),
            },
            to: vec![],
            cc: vec![],
            received: "2026-07-10T00:00:00Z".into(),
            sent: "".into(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: "".into(),
            is_draft: false,
        };
        s.upsert_message("F", &m).unwrap();
        s.put_body(
            "1",
            &Body {
                content_type: "text".into(),
                content: "the pizza party is friday".into(),
            },
        )
        .unwrap();
        m.id = "2".into();
        m.subject = "Unrelated".into();
        s.upsert_message("F", &m).unwrap();
        assert_eq!(s.search("budget", 50).unwrap().len(), 1);
        assert_eq!(s.search("pizza", 50).unwrap()[0].id, "1");
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;

    #[test]
    fn create_local_draft_appears_in_drafts_folder_with_local_id() {
        let s = Store::open_in_memory().unwrap();
        let id = s
            .create_local_draft("Hi", "bob@x", "carol@x", "<p>hello</p>")
            .unwrap();
        assert!(id.starts_with("local:"));

        let folders = s.folders().unwrap();
        let drafts_folder = folders
            .iter()
            .find(|f| f.display_name == "Drafts")
            .expect("a Drafts folder should exist");

        let rows = s.messages_in_folder(&drafts_folder.id, 50, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert!(rows[0].is_draft);
        assert_eq!(rows[0].subject, "Hi");
        assert_eq!(rows[0].to_recipients, "bob@x");
        assert_eq!(rows[0].cc_recipients, "carol@x");

        let body = s.get_body(&id).unwrap().unwrap();
        assert_eq!(body.content, "<p>hello</p>");
    }

    #[test]
    fn create_local_draft_reuses_the_synced_drafts_folder() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder {
            id: "REAL-DRAFTS".into(),
            display_name: "Drafts".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: Some("drafts".into()),
        })
        .unwrap();
        let id = s.create_local_draft("Hi", "", "", "").unwrap();
        let rows = s.messages_in_folder("REAL-DRAFTS", 50, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
    }

    #[test]
    fn update_draft_fields_changes_subject_and_body() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Old", "a@x", "", "old body").unwrap();
        s.update_draft_fields(&id, "New", "b@x", "c@x", "new body")
            .unwrap();

        let (row, body) = s.draft(&id).unwrap().expect("draft should still exist");
        assert_eq!(row.subject, "New");
        assert_eq!(row.to_recipients, "b@x");
        assert_eq!(row.cc_recipients, "c@x");
        assert_eq!(body.content, "new body");
    }

    #[test]
    fn reconcile_id_moves_message_and_body_to_the_graph_id() {
        let s = Store::open_in_memory().unwrap();
        let local_id = s
            .create_local_draft("Subj", "a@x", "", "body text")
            .unwrap();
        s.reconcile_id(&local_id, "GRAPH-ID-1").unwrap();

        assert!(s.draft(&local_id).unwrap().is_none());
        let (row, body) = s
            .draft("GRAPH-ID-1")
            .unwrap()
            .expect("reconciled draft should be found under the graph id");
        assert_eq!(row.id, "GRAPH-ID-1");
        assert_eq!(row.subject, "Subj");
        assert_eq!(body.content, "body text");

        // search (via messages_fts) should follow the row to the new id too.
        assert_eq!(s.search("Subj", 50).unwrap()[0].id, "GRAPH-ID-1");
    }

    #[test]
    fn draft_returns_none_for_unknown_id() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.draft("nope").unwrap().is_none());
    }
}
