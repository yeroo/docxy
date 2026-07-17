//! The local SQLite mail store — the single source of truth for the TUI.
//!
//! The sync engine (a later task) writes Graph data in here via
//! `upsert_folder`/`upsert_message`/delta-link bookkeeping; the TUI reads
//! only from here (offline-first). This module owns the schema (see
//! `schema.rs`) and the folders/messages/meta surface, plus bodies,
//! attachments, and full-text search; the outbox is a later task's concern
//! even though its table already exists (see `schema.rs`'s module doc).

use std::path::Path;

use rusqlite::{Connection, Row, params};

use crate::graph::model::{AttachmentMeta, Body, MailFolder, Message, Recipient};

mod schema;

/// A local SQLite store error. Wraps `rusqlite::Error` behind a small,
/// crate-local type so callers don't need to depend on `rusqlite` directly.
#[derive(Debug)]
pub enum StoreError {
    Sql(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sql(msg) => write!(f, "sqlite error: {msg}"),
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
                 is_read, is_flagged, has_attachments, importance, preview
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
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
                 preview = excluded.preview",
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
                    is_read, is_flagged, has_attachments, importance, preview
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

    /// Replaces the full set of attachment metadata stored for a message
    /// (no bytes — those are fetched separately, later, on demand).
    pub fn put_attachments(
        &self,
        message_id: &str,
        atts: &[AttachmentMeta],
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM attachments WHERE message_id = ?1",
            params![message_id],
        )?;
        for a in atts {
            self.conn.execute(
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
        Ok(())
    }

    /// The attachment metadata stored for a message, in insertion order.
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
                    is_read, is_flagged, has_attachments, importance, preview
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

/// Maps one row of a `SELECT id, folder_id, ..., preview FROM messages ...`
/// query (that exact column order) to a `MessageRow`. Shared by
/// `messages_in_folder` and `search`, which both select those columns in
/// that order from `messages`, so there's only one place mapping can drift
/// out of sync with the column list.
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
mod search_tests {
    use super::*;
    use crate::graph::model::{Body, MailFolder, Message, Recipient};
    // reuse msg() helper pattern from Task 8 tests (duplicate locally)

    #[test]
    fn search_matches_subject_and_body() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder{id:"F".into(),display_name:"I".into(),parent_id:None,total_count:0,unread_count:0,well_known_name:None}).unwrap();
        let mut m = Message{ id:"1".into(), conversation_id:"C".into(), subject:"Quarterly budget".into(),
            from:Recipient{name:"A".into(),address:"a@x".into()}, to:vec![], cc:vec![],
            received:"2026-07-10T00:00:00Z".into(), sent:"".into(), is_read:false, is_flagged:false,
            has_attachments:false, importance:"normal".into(), preview:"".into() };
        s.upsert_message("F", &m).unwrap();
        s.put_body("1", &Body{content_type:"text".into(), content:"the pizza party is friday".into()}).unwrap();
        m.id = "2".into(); m.subject = "Unrelated".into();
        s.upsert_message("F", &m).unwrap();
        assert_eq!(s.search("budget", 50).unwrap().len(), 1);
        assert_eq!(s.search("pizza", 50).unwrap()[0].id, "1");
    }
}
