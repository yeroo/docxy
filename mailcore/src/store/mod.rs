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

use crate::graph::model::{AttachmentMeta, Attendee, Body, Event, MailFolder, Message, Recipient};
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
    pub bcc_recipients: String,
}

/// A file the user has attached to an outbound draft — a reference (path +
/// name + size), not bytes; the bytes are read from disk at send time.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundAttachment {
    pub path: String,
    pub name: String,
    pub size: i64,
}

/// A `contacts` row — one per normalized (lowercased) email address, the
/// autocomplete query surface. `source` is `local`/`graph`/`both`;
/// `relevance` is the Graph `/me/people` rank (lower = more relevant),
/// `None` for a purely locally-mined contact.
#[derive(Debug, Clone, PartialEq)]
pub struct Contact {
    pub name: String,
    pub address: String,
    pub source: String,
    pub last_seen: String,
    pub frequency: i64,
    pub relevance: Option<i64>,
}

/// A queued local mutation, awaiting a push to Microsoft Graph by
/// `sync::outbox::apply_op`. Serializes to/from JSON via `to_json`/
/// `from_json` (`crate::json`, no `serde`) as `{"kind":"...","id":"...",...}`
/// — `kind` is the tag (`markRead`/`setFlag`/`move`/`delete`), the rest are
/// this variant's fields.
#[derive(Debug, Clone, PartialEq)]
pub enum OutboxOp {
    MarkRead {
        id: String,
        read: bool,
    },
    SetFlag {
        id: String,
        flagged: bool,
    },
    Move {
        id: String,
        dest: String,
    },
    Delete {
        id: String,
    },
    /// Push a draft's current locally-stored fields (subject/to/cc/body) to
    /// Graph: `sync::outbox::apply_op` creates it (and reconciles the local
    /// `local:` id to the Graph-minted one) if `id` hasn't synced yet,
    /// otherwise patches the existing Graph draft in place.
    SaveDraft {
        id: String,
    },
    /// Ensure the draft addressed by `id` exists on Graph (same as
    /// `SaveDraft`, if it hasn't already), then hand it to Graph for
    /// delivery via `.../send`.
    SendDraft {
        id: String,
    },
    /// Push an RSVP for a calendar event to Graph
    /// (`.../accept`|`.../decline`|`.../tentativelyAccept`, always with
    /// `sendResponse: true` — see `sync::outbox::apply_op`). `kind` is the
    /// same response_status vocabulary `Store::set_event_response` writes
    /// locally (`"accepted"`, `"declined"`, `"tentativelyAccepted"`);
    /// `apply_op` converts it to a `graph::client::RsvpKind`. `comment` is
    /// the optional note attendees see alongside the response.
    RespondEvent {
        id: String,
        kind: String,
        comment: Option<String>,
    },
    /// Push a locally-created/edited/deleted calendar event to Graph. `id` is
    /// the store's event id — a `local:` id (before its first `create_event`
    /// reconciles it) or the Graph id. See `sync::outbox::apply_op`.
    CreateEvent {
        id: String,
    },
    UpdateEvent {
        id: String,
    },
    DeleteEvent {
        id: String,
    },
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
            OutboxOp::SaveDraft { .. } => "saveDraft",
            OutboxOp::SendDraft { .. } => "sendDraft",
            OutboxOp::RespondEvent { .. } => "respondEvent",
            OutboxOp::CreateEvent { .. } => "createEvent",
            OutboxOp::UpdateEvent { .. } => "updateEvent",
            OutboxOp::DeleteEvent { .. } => "deleteEvent",
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
            OutboxOp::SaveDraft { id } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
            ]),
            OutboxOp::SendDraft { id } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
            ]),
            OutboxOp::RespondEvent { id, kind, comment } => Value::Object(vec![
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
            ]),
            OutboxOp::CreateEvent { id }
            | OutboxOp::UpdateEvent { id }
            | OutboxOp::DeleteEvent { id } => Value::Object(vec![
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
            "saveDraft" => Some(OutboxOp::SaveDraft { id: id()? }),
            "sendDraft" => Some(OutboxOp::SendDraft { id: id()? }),
            "respondEvent" => Some(OutboxOp::RespondEvent {
                id: id()?,
                kind: v.get("rsvp")?.as_str()?.to_string(),
                comment: v.get("comment").and_then(Value::as_str).map(str::to_string),
            }),
            "createEvent" => Some(OutboxOp::CreateEvent { id: id()? }),
            "updateEvent" => Some(OutboxOp::UpdateEvent { id: id()? }),
            "deleteEvent" => Some(OutboxOp::DeleteEvent { id: id()? }),
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

/// An `events` row, as read back from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub id: String,
    pub subject: String,
    pub start_utc: String,
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub organizer_name: String,
    pub organizer_addr: String,
    pub response_status: String,
}

/// An `event_attendees` row, as read back from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct AttendeeRow {
    pub name: String,
    pub addr: String,
    pub r#type: String,
    pub response: String,
}

/// The field set `upsert_event` writes to an `events` row.
///
/// Same field set as `EventRow` plus the extra columns (`series_master_id`,
/// `body_preview`, `web_link`, `last_modified`, `body_html`) that `EventRow`
/// doesn't surface. `graph::model::Event` (Task 2) turned out to be a
/// field-for-field superset of this (it additionally carries `attendees`),
/// so rather than folding `NewEvent` away, the sync engine bridges the two
/// via the `From<&Event> for NewEvent` conversion below — a straight
/// per-field copy the engine runs for every event `calendar_view` returns.
///
/// `body_html` supersedes the original spec idea of reusing the `bodies`
/// table (keyed `event:<id>`) for event bodies — `bodies.message_id` has an
/// FK to `messages(id)`, which an `event:<id>` key can't satisfy. Since an
/// event has exactly one body, a plain column on `events` avoids the FK
/// fight entirely. See `event_body` for reading it back.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub id: String,
    pub subject: String,
    pub start_utc: String,
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub organizer_name: String,
    pub organizer_addr: String,
    pub response_status: String,
    pub series_master_id: Option<String>,
    pub body_preview: String,
    pub web_link: String,
    pub last_modified: String,
    pub body_html: String,
}

/// The field set `put_event_attendees` writes for one `event_attendees` row —
/// the same field set as `graph::model::Attendee`; see `From<&Attendee> for
/// NewAttendee` below.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAttendee {
    pub name: String,
    pub addr: String,
    pub r#type: String,
    pub response: String,
}

/// Straight per-field copy: what the sync engine's `RefreshCalendar` runs for
/// every event `GraphClient::calendar_view` returns, before `upsert_event`.
impl From<&Event> for NewEvent {
    fn from(e: &Event) -> Self {
        NewEvent {
            id: e.id.clone(),
            subject: e.subject.clone(),
            start_utc: e.start_utc.clone(),
            end_utc: e.end_utc.clone(),
            is_all_day: e.is_all_day,
            location: e.location.clone(),
            organizer_name: e.organizer_name.clone(),
            organizer_addr: e.organizer_addr.clone(),
            response_status: e.response_status.clone(),
            series_master_id: e.series_master_id.clone(),
            body_preview: e.body_preview.clone(),
            web_link: e.web_link.clone(),
            last_modified: e.last_modified.clone(),
            body_html: e.body_html.clone(),
        }
    }
}

/// Straight per-field copy: what the sync engine's `RefreshCalendar` runs for
/// every attendee of every event, before `put_event_attendees`.
impl From<&Attendee> for NewAttendee {
    fn from(a: &Attendee) -> Self {
        NewAttendee {
            name: a.name.clone(),
            addr: a.addr.clone(),
            r#type: a.r#type.clone(),
            response: a.response.clone(),
        }
    }
}

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
        // Same idempotent-migration pattern as `is_draft` above: `events`
        // already includes `body_html` for freshly-created databases; this
        // brings an existing database up to date, erroring (and being
        // swallowed) on every database that already has the column.
        let _ = conn.execute(
            "ALTER TABLE events ADD COLUMN body_html TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Same idempotent-migration pattern as `is_draft`/`body_html` above:
        // `messages` already includes `bcc_recipients` for freshly-created
        // databases; this brings an existing database up to date, erroring
        // (and being swallowed) on every database that already has the
        // column.
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN bcc_recipients TEXT NOT NULL DEFAULT ''",
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
                    is_read, is_flagged, has_attachments, importance, preview, is_draft,
                    bcc_recipients
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
                        bcc_recipients,
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
                    k.is_read, k.is_flagged, k.has_attachments, k.importance, k.preview, k.is_draft,
                    k.bcc_recipients
             FROM keyed k
             JOIN ranked r ON k.conv_key = r.conv_key
             ORDER BY r.latest DESC, k.received_at ASC",
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

    /// Locally marks a draft as sent (`is_draft = 0`) — the optimistic half
    /// of `SyncCommand::SendDraft` (the sync engine's outbox, via
    /// `OutboxOp::SendDraft`, is what actually hands the draft to Graph for
    /// delivery). See `set_read` for why this doesn't return a `Result`: a
    /// mismatched/already-gone `id` just changes zero rows.
    pub fn mark_sent(&self, id: &str) {
        let _ = self.conn.execute(
            "UPDATE messages SET is_draft = 0 WHERE id = ?1",
            params![id],
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
        bcc: &str,
        body_html: &str,
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE messages SET subject = ?2, to_recipients = ?3, cc_recipients = ?4, bcc_recipients = ?5
             WHERE id = ?1",
            params![id, subject, to, cc, bcc],
        )?;
        tx.execute(
            "INSERT INTO bodies (message_id, content_type, content) VALUES (?1, 'html', ?2)
             ON CONFLICT(message_id) DO UPDATE SET content = excluded.content",
            params![id, body_html],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records a file attached to draft `draft_id`. Idempotent per (draft, path).
    pub fn add_outbound_attachment(
        &self,
        draft_id: &str,
        path: &str,
        name: &str,
        size: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO outbound_attachments (draft_id, path, name, size) VALUES (?1, ?2, ?3, ?4)",
            params![draft_id, path, name, size],
        )?;
        Ok(())
    }

    /// The files attached to `draft_id`, in attach (insertion) order — SQLite's
    /// implicit `rowid` tracks insertion order, and callers (compose's
    /// attachment list, Ctrl+R's LIFO removal) rely on the last element being
    /// the most-recently-added attachment.
    pub fn outbound_attachments(
        &self,
        draft_id: &str,
    ) -> Result<Vec<OutboundAttachment>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT path, name, size FROM outbound_attachments WHERE draft_id = ?1 ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map(params![draft_id], |r| {
                Ok(OutboundAttachment {
                    path: r.get(0)?,
                    name: r.get(1)?,
                    size: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Removes one attached file from `draft_id` (by path). No-op if absent.
    pub fn remove_outbound_attachment(&self, draft_id: &str, path: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM outbound_attachments WHERE draft_id = ?1 AND path = ?2",
            params![draft_id, path],
        )?;
        Ok(())
    }

    /// Removes every attached file from `draft_id` (after a send, or on discard).
    pub fn clear_outbound_attachments(&self, draft_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM outbound_attachments WHERE draft_id = ?1",
            params![draft_id],
        )?;
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
        tx.execute(
            "UPDATE outbound_attachments SET draft_id = ?2 WHERE draft_id = ?1",
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
                    is_read, is_flagged, has_attachments, importance, preview, is_draft,
                    bcc_recipients
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

    /// Resolves the Drafts folder's id, for filing a new local draft (or a
    /// reply/forward draft the sync engine just fetched from Graph).
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
    /// don't collide — see `adopt_sentinel_drafts`, which the sync engine
    /// calls once the real Drafts folder appears, to re-file whatever ended
    /// up under the sentinel.
    ///
    /// `pub(crate)` (not `pub`) rather than private: the sync engine
    /// (`sync::engine`) needs the same resolution when it stores a
    /// reply/forward draft fetched from Graph, so both call sites agree on
    /// where "the Drafts folder" is instead of duplicating this logic.
    pub(crate) fn drafts_folder_id(&self) -> Result<String, StoreError> {
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

    /// Re-files every message still sitting under the local drafts sentinel
    /// folder (`LOCAL_DRAFTS_SENTINEL_FOLDER_ID`) into `real_drafts_id`, the
    /// id of the Drafts folder (`well_known_name = 'drafts'`) once it has
    /// actually synced down from Graph. Called by the sync engine right
    /// after upserting a fresh folder list that contains the real Drafts
    /// folder, so drafts created (or reply/forward-fetched) before the
    /// first folder sync don't stay stranded under the sentinel — see
    /// `drafts_folder_id`'s doc comment and the Task 6 report for the gap
    /// this closes. Affects zero rows (not an error) if nothing was ever
    /// filed under the sentinel.
    pub fn adopt_sentinel_drafts(&self, real_drafts_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE messages SET folder_id = ?1 WHERE folder_id = ?2",
            params![real_drafts_id, LOCAL_DRAFTS_SENTINEL_FOLDER_ID],
        )?;
        Ok(())
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
                    is_read, is_flagged, has_attachments, importance, preview, is_draft,
                    bcc_recipients
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

    /// Inserts or updates an `events` row by `id`. See `NewEvent`'s doc
    /// comment for why this takes that stand-in type rather than a
    /// Task-2 `Event` (which doesn't exist yet).
    pub fn upsert_event(&self, e: &NewEvent) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO events (
                 id, subject, start_utc, end_utc, is_all_day, location,
                 organizer_name, organizer_addr, response_status,
                 series_master_id, body_preview, web_link, last_modified,
                 body_html
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
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
                 body_html = excluded.body_html",
            params![
                e.id,
                e.subject,
                e.start_utc,
                e.end_utc,
                e.is_all_day,
                e.location,
                e.organizer_name,
                e.organizer_addr,
                e.response_status,
                e.series_master_id,
                e.body_preview,
                e.web_link,
                e.last_modified,
                e.body_html,
            ],
        )?;
        Ok(())
    }

    /// Replaces the full set of attendees stored for an event. See
    /// `put_attachments` for why the delete-then-insert runs in one
    /// transaction via `unchecked_transaction` (same shared-`&self`
    /// reasoning applies here).
    pub fn put_event_attendees(&self, id: &str, a: &[NewAttendee]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM event_attendees WHERE event_id = ?1",
            params![id],
        )?;
        for att in a {
            tx.execute(
                "INSERT INTO event_attendees (event_id, name, addr, type, response)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, att.name, att.addr, att.r#type, att.response],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Replaces every event overlapping `[from_utc, to_utc)` (the same
    /// overlap condition `events_in_window` filters by: `start_utc < to_utc
    /// AND end_utc > from_utc`) with `events`/`attendees` — a full
    /// server-truth refetch for that window, which is all
    /// `GraphClient::calendar_view` ever returns (there's no delta endpoint
    /// to fetch only what changed). Any event previously stored in the
    /// window that ISN'T in `events` (cancelled server-side, or moved
    /// outside the window) is pruned rather than left stuck locally
    /// forever; every event in `events` is upserted with its attendees
    /// replaced from the matching entry in `attendees` (paired by id — an
    /// event with no entry there gets none, the same "replace with empty"
    /// semantics `put_event_attendees(id, &[])` has).
    ///
    /// Runs as ONE transaction (the window delete, then every event upsert
    /// and attendee replace) so a crash mid-refresh can't leave the window
    /// emptied without ever refilling it: either the whole replacement
    /// commits, or none of it does and the prior window contents are
    /// untouched. Deliberately does NOT call `upsert_event`/
    /// `put_event_attendees` (each opens its own transaction via
    /// `unchecked_transaction`; nesting a second `BEGIN` on the same
    /// connection while this one is still open would error) — the same SQL
    /// is inlined here instead. The per-event attendee `DELETE` before each
    /// insert is technically redundant with `events`' `ON DELETE CASCADE`
    /// for an event this call's own window-delete just removed, but not for
    /// one that instead takes the `ON CONFLICT` update path (a boundary
    /// mismatch between this window and the row's stored dates) — cheap
    /// insurance against a duplicate attendee row in that edge case, and
    /// the same delete-then-insert `put_event_attendees` itself always does.
    pub fn replace_events_in_window(
        &self,
        from_utc: &str,
        to_utc: &str,
        events: &[NewEvent],
        attendees: &[(String, Vec<NewAttendee>)],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM events WHERE start_utc < ?2 AND end_utc > ?1",
            params![from_utc, to_utc],
        )?;
        for e in events {
            tx.execute(
                "INSERT INTO events (
                     id, subject, start_utc, end_utc, is_all_day, location,
                     organizer_name, organizer_addr, response_status,
                     series_master_id, body_preview, web_link, last_modified,
                     body_html
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
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
                     body_html = excluded.body_html",
                params![
                    e.id,
                    e.subject,
                    e.start_utc,
                    e.end_utc,
                    e.is_all_day,
                    e.location,
                    e.organizer_name,
                    e.organizer_addr,
                    e.response_status,
                    e.series_master_id,
                    e.body_preview,
                    e.web_link,
                    e.last_modified,
                    e.body_html,
                ],
            )?;
        }
        for (id, atts) in attendees {
            tx.execute(
                "DELETE FROM event_attendees WHERE event_id = ?1",
                params![id],
            )?;
            for att in atts {
                tx.execute(
                    "INSERT INTO event_attendees (event_id, name, addr, type, response)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, att.name, att.addr, att.r#type, att.response],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Events overlapping `[from_utc, to_utc)`: `start_utc < to_utc AND
    /// end_utc > from_utc`, so multi-day/ongoing events that started before
    /// the window (or end after it) still show, not just ones fully
    /// contained in it. Ordered `start_utc ASC`.
    pub fn events_in_window(
        &self,
        from_utc: &str,
        to_utc: &str,
    ) -> Result<Vec<EventRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, subject, start_utc, end_utc, is_all_day, location,
                    organizer_name, organizer_addr, response_status
             FROM events
             WHERE start_utc < ?2 AND end_utc > ?1
             ORDER BY start_utc ASC",
        )?;
        let rows = stmt
            .query_map(params![from_utc, to_utc], |row| {
                Ok(EventRow {
                    id: row.get(0)?,
                    subject: row.get(1)?,
                    start_utc: row.get(2)?,
                    end_utc: row.get(3)?,
                    is_all_day: row.get(4)?,
                    location: row.get(5)?,
                    organizer_name: row.get(6)?,
                    organizer_addr: row.get(7)?,
                    response_status: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The attendees stored for an event, in insertion order (the order
    /// `put_event_attendees` last wrote them in).
    pub fn event_attendees(&self, id: &str) -> Result<Vec<AttendeeRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT name, addr, type, response FROM event_attendees WHERE event_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![id], |row| {
                Ok(AttendeeRow {
                    name: row.get(0)?,
                    addr: row.get(1)?,
                    r#type: row.get(2)?,
                    response: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The stored `body_html` for an event: `None` if there's no `events`
    /// row with that id at all, `Some("")` if the row exists but no body
    /// was ever written for it (or it was written empty) — mirroring
    /// `get_body`'s `None`-means-"nothing stored" convention, adapted to a
    /// column that's always present (`NOT NULL DEFAULT ''`) rather than a
    /// row that may or may not exist in a separate table.
    pub fn event_body(&self, id: &str) -> Result<Option<String>, StoreError> {
        let body = self
            .conn
            .query_row(
                "SELECT body_html FROM events WHERE id = ?1",
                params![id],
                |row| row.get::<_, String>(0),
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

    /// Inserts a locally-created event with a fresh `local:` id and the given
    /// organizer, so it shows in the agenda immediately (before it syncs to
    /// Graph). `response_status` is `"organizer"`.
    pub fn create_local_event(
        &self,
        f: &LocalEventFields,
        organizer_name: &str,
        organizer_addr: &str,
    ) -> Result<String, StoreError> {
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
            params![
                id,
                f.subject,
                f.start_utc,
                f.end_utc,
                f.is_all_day,
                f.location,
                f.body_html
            ],
        )?;
        self.put_event_attendees(id, &to_new_attendees(&f.attendees))?;
        Ok(())
    }

    /// Removes an event (and its attendees) locally.
    pub fn delete_event(&self, id: &str) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM event_attendees WHERE event_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM events WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// Reads a stored event's send-relevant fields (`None` if no such event).
    pub fn event_for_send(&self, id: &str) -> Result<Option<EventSendData>, StoreError> {
        let row = self.conn.query_row(
            "SELECT subject, start_utc, end_utc, is_all_day, location, body_html FROM events WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
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
        Ok(Some(EventSendData {
            subject,
            start_utc,
            end_utc,
            is_all_day,
            location,
            body_html,
            attendees,
        }))
    }

    /// Re-points a `local:` event id to its Graph id after `create_event`
    /// (mirrors `reconcile_id` for drafts): updates `events.id` and
    /// `event_attendees.event_id` in one transaction.
    pub fn reconcile_event_id(&self, local_id: &str, graph_id: &str) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        tx.execute(
            "UPDATE events SET id = ?2 WHERE id = ?1",
            params![local_id, graph_id],
        )?;
        tx.execute(
            "UPDATE event_attendees SET event_id = ?2 WHERE event_id = ?1",
            params![local_id, graph_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Locally sets an event's RSVP response status (`"accepted"`,
    /// `"declined"`, `"tentativelyAccepted"`, ...) — the optimistic half of
    /// an RSVP a later task's outbox will push to Graph. See `set_read` for
    /// why this doesn't return a `Result`.
    pub fn set_event_response(&self, id: &str, status: &str) {
        let _ = self.conn.execute(
            "UPDATE events SET response_status = ?1 WHERE id = ?2",
            params![status, id],
        );
    }

    /// The stored delta link for the calendar, if any (`None` before the
    /// first calendar sync). Unlike `get_delta_link` (one per folder), the
    /// calendar has exactly one, so it's kept in `meta` rather than a
    /// dedicated column.
    pub fn calendar_delta_link(&self) -> Result<Option<String>, StoreError> {
        let link = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'calendar_delta_link'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        Ok(link)
    }

    /// Stores the calendar's delta link (used to resume delta sync). See
    /// `calendar_delta_link` for why this lives in `meta`.
    pub fn set_calendar_delta_link(&self, link: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('calendar_delta_link', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![link],
        )?;
        Ok(())
    }

    /// Inserts or merges a contact keyed by `address`. Merge rules keep the
    /// strongest signal from either source: a non-empty `name` wins over an
    /// empty one; `source` becomes `both` once both a local and a graph upsert
    /// have touched the row; `last_seen`/`frequency` take the MAX (so re-running
    /// the local miner is idempotent, and a graph upsert with 0/"" never lowers
    /// them); `relevance` takes the incoming value when present, else keeps the
    /// stored one.
    pub fn upsert_contact(&self, c: &Contact) -> Result<(), StoreError> {
        let address = c.address.to_lowercase();
        self.conn.execute(
            "INSERT INTO contacts (address, name, source, last_seen, frequency, relevance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(address) DO UPDATE SET
                 name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE contacts.name END,
                 source = CASE WHEN contacts.source = excluded.source THEN contacts.source ELSE 'both' END,
                 last_seen = MAX(contacts.last_seen, excluded.last_seen),
                 frequency = MAX(contacts.frequency, excluded.frequency),
                 relevance = COALESCE(excluded.relevance, contacts.relevance)",
            params![address, c.name, c.source, c.last_seen, c.frequency, c.relevance],
        )?;
        Ok(())
    }

    /// Ranked autocomplete matches for `query` (matched case-insensitively
    /// against name and address). Prefix matches rank ahead of interior
    /// matches; then by Graph relevance (lower first, nulls last), then
    /// frequency (higher first), then recency, then name.
    ///
    /// `query` is escaped before being bound as the `LIKE` argument: `\`,
    /// `%`, and `_` are all LIKE metacharacters (`_` matches any single
    /// character, `%` matches any run of characters), and `_` in particular
    /// is common in email addresses, so leaving it unescaped would make a
    /// query like `"a_b"` match `"axb"`, `"a.b"`, etc. — far broader than the
    /// literal substring the user typed. Escaping `\` first (so a literal
    /// backslash in the query doesn't get mistaken for part of one of the
    /// two escapes added after it) and pairing every `LIKE` with `ESCAPE
    /// '\'` restores literal matching.
    pub fn search_contacts(&self, query: &str, limit: i64) -> Result<Vec<Contact>, StoreError> {
        let q = query
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let mut stmt = self.conn.prepare(
            "SELECT address, name, source, last_seen, frequency, relevance
             FROM contacts
             WHERE lower(name) LIKE '%' || ?1 || '%' ESCAPE '\\' OR lower(address) LIKE '%' || ?1 || '%' ESCAPE '\\'
             ORDER BY
                 (CASE WHEN lower(name) LIKE ?1 || '%' ESCAPE '\\' OR lower(address) LIKE ?1 || '%' ESCAPE '\\' THEN 0 ELSE 1 END) ASC,
                 (CASE WHEN relevance IS NULL THEN 1 ELSE 0 END) ASC,
                 relevance ASC,
                 frequency DESC,
                 last_seen DESC,
                 name ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![q, limit], |row| {
                Ok(Contact {
                    address: row.get(0)?,
                    name: row.get(1)?,
                    source: row.get(2)?,
                    last_seen: row.get(3)?,
                    frequency: row.get(4)?,
                    relevance: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Rebuilds the local contact signal from the `messages` table: every
    /// distinct address seen as a sender or a to/cc recipient becomes a
    /// contact, with `frequency` = how many messages it appeared in and
    /// `last_seen` = the most recent of those messages' dates. Idempotent —
    /// safe to run every sync pass (the miner computes exact counts and
    /// `upsert_contact` takes the MAX, so re-runs don't inflate anything).
    pub fn refresh_local_contacts(&self) -> Result<(), StoreError> {
        use std::collections::HashMap;
        // (name, last_seen, count) aggregated by lowercased address.
        let mut agg: HashMap<String, (String, String, i64)> = HashMap::new();
        let consider = |name: &str,
                        addr: &str,
                        date: &str,
                        agg: &mut HashMap<String, (String, String, i64)>| {
            let addr = addr.trim().to_lowercase();
            if addr.is_empty() || !addr.contains('@') {
                return;
            }
            let e = agg
                .entry(addr)
                .or_insert_with(|| (String::new(), String::new(), 0));
            if e.0.is_empty() && !name.trim().is_empty() {
                e.0 = name.trim().to_string();
            }
            if date > e.1.as_str() {
                e.1 = date.to_string();
            }
            e.2 += 1;
        };

        let mut stmt = self.conn.prepare(
            "SELECT from_name, from_addr, to_recipients, cc_recipients, received_at, sent_at FROM messages",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        for (from_name, from_addr, to, cc, received, sent) in rows {
            let date = if !received.is_empty() {
                received.as_str()
            } else {
                sent.as_str()
            };
            // Dedup this message's parties by lowercased address first, so an
            // address that appears in more than one role of the SAME message
            // (e.g. a reply-all sender also CC'd, or a to+cc duplicate)
            // contributes at most one increment to `frequency` for this
            // message rather than one per role.
            let mut seen: HashMap<String, String> = HashMap::new();
            let mut note = |name: &str, addr: &str| {
                let a = addr.trim().to_lowercase();
                if a.is_empty() || !a.contains('@') {
                    return;
                }
                let e = seen.entry(a).or_default();
                if e.is_empty() && !name.trim().is_empty() {
                    *e = name.trim().to_string();
                }
            };
            note(&from_name, &from_addr);
            for r in crate::sync::outbox::parse_recipients(&to) {
                note(&r.name, &r.address);
            }
            for r in crate::sync::outbox::parse_recipients(&cc) {
                note(&r.name, &r.address);
            }
            for (addr, name) in seen {
                consider(&name, &addr, date, &mut agg);
            }
        }

        let tx = self.conn.unchecked_transaction()?;
        for (address, (name, last_seen, frequency)) in agg {
            self.upsert_contact(&Contact {
                name,
                address,
                source: "local".to_string(),
                last_seen,
                frequency,
                relevance: None,
            })?;
        }
        tx.commit()?;
        Ok(())
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

/// Converts the (name, address) pairs `LocalEventFields::attendees` collects
/// into `NewAttendee`s, defaulting every attendee to required/no-response —
/// the compose form doesn't collect attendee type or track RSVP state itself.
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
///
/// The display `name` is attacker-controlled — it comes straight off a
/// sender's `From`/`To` on a synced message or a reply draft — so it is
/// sanitized before formatting. `;`, `<`, and `>` are the structural
/// delimiters this flat form and `outbox::parse_recipients` rely on; a name
/// that smuggled any of them (or a CR/LF) would inject an *extra* recipient
/// when the column is parsed back at send time, silently exfiltrating a reply
/// to an attacker-chosen address. Stripping them keeps every recipient exactly
/// one `Name <addr>` unit.
fn encode_recipients(list: &[Recipient]) -> String {
    list.iter()
        .map(|r| format!("{} <{}>", sanitize_recipient_name(&r.name), r.address))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Neutralizes the structural delimiters of the flat recipient encoding
/// (`;`, `<`, `>`) and CR/LF in an attacker-controlled display name, so it can
/// never be parsed back as more than the one recipient it belongs to. Each
/// stripped delimiter becomes a space (rather than vanishing) so neighbouring
/// name tokens don't fuse; the result is trimmed.
fn sanitize_recipient_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            ';' | '<' | '>' | '\r' | '\n' => ' ',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
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
        bcc_recipients: row.get(16)?,
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
    fn encode_recipients_neutralizes_display_name_injection() {
        // A malicious sender sets their display name to smuggle the flat
        // encoding's delimiters. When the victim replies, this name must not
        // round-trip into a second recipient (silent reply exfiltration). H1.
        let evil = Recipient {
            name: "Ann <attacker@evil.com>; x".into(),
            address: "mal@sender.com".into(),
        };
        let encoded = encode_recipients(&[evil]);
        // Only the real address contributes an angle-bracket pair, and no `;`
        // survives to act as a recipient separator — so a parse yields exactly
        // one recipient, the legitimate one.
        assert_eq!(encoded.matches('<').count(), 1);
        assert_eq!(encoded.matches('>').count(), 1);
        assert!(!encoded.contains(';'));
        assert!(encoded.ends_with("<mal@sender.com>"));
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
    fn conversations_in_folder_gathers_the_thread_across_folders() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_folder(&MailFolder {
                id: "inbox".into(),
                display_name: "Inbox".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("inbox".into()),
            })
            .unwrap();
        store
            .upsert_folder(&MailFolder {
                id: "sent".into(),
                display_name: "Sent".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("sentitems".into()),
            })
            .unwrap();

        // Conversation c1: an inbox message + a reply the user sent (in Sent).
        let mut inbound = msg("10", false); // helper sets received "2026-07-10T..."
        inbound.conversation_id = "c1".into();
        inbound.from = Recipient {
            name: "Ann".into(),
            address: "ann@x".into(),
        };
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
        assert!(ids.contains(&"12")); // cross-folder: the Sent reply is included
        assert!(!ids.contains(&"11")); // c2 has no inbox message → excluded
    }

    #[test]
    fn conversations_in_folder_singletons_blank_conversation_ids() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_folder(&MailFolder {
                id: "inbox".into(),
                display_name: "Inbox".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("inbox".into()),
            })
            .unwrap();
        let mut a = msg("20", false);
        a.conversation_id = "".into();
        let mut b = msg("21", false);
        b.conversation_id = "".into();
        store.upsert_message("inbox", &a).unwrap();
        store.upsert_message("inbox", &b).unwrap();

        let rows = store.conversations_in_folder("inbox", 50, 0).unwrap();
        assert_eq!(rows.len(), 2); // two independent singletons, not one merged group

        // limit=1 only returns a single row if the two blanks are genuinely
        // separate singleton conversations; if they were wrongly merged into
        // one group, both messages would still come back under this cap.
        let top = store.conversations_in_folder("inbox", 1, 0).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].id, "21"); // most recent: received 2026-07-21 > 2026-07-20
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

    #[test]
    fn upsert_contact_merges_local_then_graph() {
        let s = Store::open_in_memory().unwrap();
        // local mining: a name + frequency + recency, no relevance
        s.upsert_contact(&Contact {
            name: "Bob Jones".into(),
            address: "bob@x.com".into(),
            source: "local".into(),
            last_seen: "2026-07-10T00:00:00Z".into(),
            frequency: 3,
            relevance: None,
        })
        .unwrap();
        // graph sync: same address, a display name + relevance, no local signal
        s.upsert_contact(&Contact {
            name: "Robert Jones".into(),
            address: "bob@x.com".into(),
            source: "graph".into(),
            last_seen: "".into(),
            frequency: 0,
            relevance: Some(2),
        })
        .unwrap();

        let got = s.search_contacts("bob", 10).unwrap();
        assert_eq!(got.len(), 1);
        let c = &got[0];
        assert_eq!(c.source, "both"); // both sources contributed
        assert_eq!(c.relevance, Some(2)); // graph relevance kept
        assert_eq!(c.frequency, 3); // local frequency kept (MAX, not clobbered to 0)
        assert_eq!(c.last_seen, "2026-07-10T00:00:00Z"); // recency kept (MAX, not clobbered to "")
        assert_eq!(c.name, "Robert Jones"); // non-empty graph name applied
    }

    #[test]
    fn search_contacts_ranks_prefix_first_then_frequency() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_contact(&Contact {
            name: "Ann Lee".into(),
            address: "ann@x.com".into(),
            source: "local".into(),
            last_seen: "2026-07-01T00:00:00Z".into(),
            frequency: 1,
            relevance: None,
        })
        .unwrap();
        s.upsert_contact(&Contact {
            name: "Danny".into(),
            address: "dan@x.com".into(),
            source: "local".into(),
            last_seen: "2026-07-02T00:00:00Z".into(),
            frequency: 9,
            relevance: None,
        })
        .unwrap();
        // "an": "Ann"/"ann@" are PREFIX matches; "Danny"/"dan@" contain "an" only interior.
        let got = s.search_contacts("an", 10).unwrap();
        let addrs: Vec<&str> = got.iter().map(|c| c.address.as_str()).collect();
        assert_eq!(addrs, ["ann@x.com", "dan@x.com"]); // prefix match ranks ahead of higher-frequency interior match
        // limit is respected
        assert_eq!(s.search_contacts("an", 1).unwrap().len(), 1);
        // matching is case-insensitive on both name and address
        assert_eq!(
            s.search_contacts("ANN", 10).unwrap()[0].address,
            "ann@x.com"
        );
    }

    #[test]
    fn search_contacts_treats_underscore_literally() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_contact(&Contact {
            name: "Underscore Bob".into(),
            address: "a_b@x.com".into(),
            source: "local".into(),
            last_seen: "2026-07-01T00:00:00Z".into(),
            frequency: 1,
            relevance: None,
        })
        .unwrap();
        s.upsert_contact(&Contact {
            name: "Plain Axb".into(),
            address: "axb@x.com".into(),
            source: "local".into(),
            last_seen: "2026-07-01T00:00:00Z".into(),
            frequency: 1,
            relevance: None,
        })
        .unwrap();

        // A literal "_" in the query must match only the literal "_" in
        // "a_b@x.com" — NOT stand in for the "x" of "axb@x.com" as a LIKE
        // wildcard would. Unescaped, "a_b" LIKE '%a_b%' matches both rows
        // (the `_` matches any single character, including "x"), so this
        // is the assertion that would fail without `ESCAPE '\'`.
        let got = s.search_contacts("a_b", 10).unwrap();
        let addrs: Vec<&str> = got.iter().map(|c| c.address.as_str()).collect();
        assert_eq!(addrs, ["a_b@x.com"]); // "axb@x.com" must NOT show up here

        // The reverse query, "axb", must match only the row that literally
        // contains "axb" — not the underscore row (a literal "_" isn't "x").
        let got = s.search_contacts("axb", 10).unwrap();
        let addrs: Vec<&str> = got.iter().map(|c| c.address.as_str()).collect();
        assert_eq!(addrs, ["axb@x.com"]);
    }

    #[test]
    fn refresh_local_contacts_mines_from_and_to_and_cc() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder {
            id: "inbox".into(),
            display_name: "Inbox".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: Some("inbox".into()),
        })
        .unwrap();
        // A message FROM Alice, TO Bob + Carol.
        let mut m = msg("1", false); // helper sets received "2026-07-1T00:00:00Z"
        m.from = Recipient {
            name: "Alice".into(),
            address: "alice@x.com".into(),
        };
        m.to = vec![Recipient {
            name: "Bob".into(),
            address: "bob@x.com".into(),
        }];
        m.cc = vec![Recipient {
            name: "Carol".into(),
            address: "carol@x.com".into(),
        }];
        s.upsert_message("inbox", &m).unwrap();

        s.refresh_local_contacts().unwrap();
        // All three parties are mined as contacts.
        let names: Vec<String> = ["alice", "bob", "carol"]
            .iter()
            .map(|q| {
                s.search_contacts(q, 1)
                    .unwrap()
                    .into_iter()
                    .next()
                    .map(|c| c.address)
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(names, ["alice@x.com", "bob@x.com", "carol@x.com"]);
        // Idempotent: a second pass does not double-count frequency.
        let f1 = s.search_contacts("alice", 1).unwrap()[0].frequency;
        s.refresh_local_contacts().unwrap();
        let f2 = s.search_contacts("alice", 1).unwrap()[0].frequency;
        assert_eq!(f1, f2);
        assert_eq!(f1, 1); // appeared in exactly one message
    }

    #[test]
    fn refresh_local_contacts_counts_a_multi_role_address_once() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder {
            id: "inbox".into(),
            display_name: "Inbox".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: Some("inbox".into()),
        })
        .unwrap();
        // A single message FROM Alice, TO Bob, and ALSO CC Bob (reply-all /
        // self-CC style duplication of the same address across roles).
        let mut m = msg("1", false);
        m.from = Recipient {
            name: "Alice".into(),
            address: "alice@x.com".into(),
        };
        m.to = vec![Recipient {
            name: "Bob".into(),
            address: "bob@x.com".into(),
        }];
        m.cc = vec![Recipient {
            name: "Bob".into(),
            address: "Bob@X.com".into(), // same address, different case
        }];
        s.upsert_message("inbox", &m).unwrap();

        s.refresh_local_contacts().unwrap();
        let bob = s
            .search_contacts("bob", 1)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        // Bob appeared in ONE message, even though he's listed in both `to`
        // and `cc` of it — frequency counts messages, not role-appearances.
        assert_eq!(bob.frequency, 1);
    }
}

#[cfg(test)]
mod calendar_tests {
    use super::*;

    fn ev(id: &str, start: &str, end: &str) -> NewEvent {
        NewEvent {
            id: id.into(),
            subject: format!("s{id}"),
            start_utc: start.into(),
            end_utc: end.into(),
            is_all_day: false,
            location: "".into(),
            organizer_name: "Org".into(),
            organizer_addr: "org@x".into(),
            response_status: "none".into(),
            series_master_id: None,
            body_preview: "".into(),
            web_link: "".into(),
            last_modified: "".into(),
            body_html: "".into(),
        }
    }

    #[test]
    fn upserts_and_lists_events_in_window_ordered_by_start() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("2", "2026-07-18T10:00:00Z", "2026-07-18T11:00:00Z"))
            .unwrap();
        s.upsert_event(&ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z"))
            .unwrap();
        let rows = s
            .events_in_window("2026-07-17T00:00:00Z", "2026-07-19T00:00:00Z")
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "1");
        assert_eq!(rows[1].id, "2");
    }

    #[test]
    fn events_in_window_excludes_events_outside_range() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("1", "2026-07-10T00:00:00Z", "2026-07-10T01:00:00Z"))
            .unwrap();
        let rows = s
            .events_in_window("2026-07-17T00:00:00Z", "2026-07-19T00:00:00Z")
            .unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn events_in_window_includes_ongoing_multi_day_events() {
        // An event that started before `from` and ends after `to` should
        // still show (start_utc < to AND end_utc > from), even though
        // neither endpoint falls inside the window.
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("1", "2026-07-01T00:00:00Z", "2026-07-31T00:00:00Z"))
            .unwrap();
        let rows = s
            .events_in_window("2026-07-17T00:00:00Z", "2026-07-19T00:00:00Z")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "1");
    }

    #[test]
    fn upsert_event_is_idempotent() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z"))
            .unwrap();
        let mut updated = ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z");
        updated.subject = "changed".into();
        s.upsert_event(&updated).unwrap();
        let rows = s
            .events_in_window("2026-07-17T00:00:00Z", "2026-07-18T00:00:00Z")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subject, "changed");
    }

    #[test]
    fn set_event_response_flips_status() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z"))
            .unwrap();
        s.set_event_response("1", "accepted");
        let rows = s
            .events_in_window("2026-07-17T00:00:00Z", "2026-07-18T00:00:00Z")
            .unwrap();
        assert_eq!(rows[0].response_status, "accepted");
    }

    #[test]
    fn replace_events_in_window_prunes_events_not_in_the_new_set() {
        // A cancelled (or moved-out-of-window) event: `upsert_event` alone
        // would never remove it, since it's simply never returned by a
        // later `calendar_view` fetch — `replace_events_in_window` is what
        // prunes it.
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("OLD", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z"))
            .unwrap();
        s.replace_events_in_window(
            "2026-07-17T00:00:00Z",
            "2026-07-18T00:00:00Z",
            &[ev("NEW", "2026-07-17T12:00:00Z", "2026-07-17T13:00:00Z")],
            &[],
        )
        .unwrap();
        let rows = s
            .events_in_window("2026-07-17T00:00:00Z", "2026-07-18T00:00:00Z")
            .unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["NEW"], "OLD should have been pruned: {ids:?}");
    }

    #[test]
    fn replace_events_in_window_leaves_events_outside_the_window_untouched() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev(
            "OUTSIDE",
            "2026-08-01T10:00:00Z",
            "2026-08-01T11:00:00Z",
        ))
        .unwrap();
        s.replace_events_in_window("2026-07-17T00:00:00Z", "2026-07-18T00:00:00Z", &[], &[])
            .unwrap();
        let rows = s
            .events_in_window("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "OUTSIDE");
    }

    #[test]
    fn replace_events_in_window_sets_attendees_for_upserted_events() {
        let s = Store::open_in_memory().unwrap();
        s.replace_events_in_window(
            "2026-07-17T00:00:00Z",
            "2026-07-18T00:00:00Z",
            &[ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z")],
            &[(
                "1".to_string(),
                vec![NewAttendee {
                    name: "Bob".into(),
                    addr: "bob@x".into(),
                    r#type: "required".into(),
                    response: "accepted".into(),
                }],
            )],
        )
        .unwrap();
        let attendees = s.event_attendees("1").unwrap();
        assert_eq!(attendees.len(), 1);
        assert_eq!(attendees[0].name, "Bob");
    }

    #[test]
    fn attendees_round_trip() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z"))
            .unwrap();
        s.put_event_attendees(
            "1",
            &[
                NewAttendee {
                    name: "Bob".into(),
                    addr: "bob@x".into(),
                    r#type: "required".into(),
                    response: "accepted".into(),
                },
                NewAttendee {
                    name: "Carol".into(),
                    addr: "carol@x".into(),
                    r#type: "optional".into(),
                    response: "none".into(),
                },
            ],
        )
        .unwrap();
        let attendees = s.event_attendees("1").unwrap();
        assert_eq!(attendees.len(), 2);
        assert_eq!(attendees[0].name, "Bob");
        assert_eq!(attendees[0].r#type, "required");
        assert_eq!(attendees[1].name, "Carol");
    }

    #[test]
    fn put_event_attendees_replaces_the_prior_set() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z"))
            .unwrap();
        s.put_event_attendees(
            "1",
            &[NewAttendee {
                name: "Bob".into(),
                addr: "bob@x".into(),
                r#type: "required".into(),
                response: "accepted".into(),
            }],
        )
        .unwrap();
        s.put_event_attendees("1", &[]).unwrap();
        assert!(s.event_attendees("1").unwrap().is_empty());
    }

    #[test]
    fn event_body_round_trips() {
        let s = Store::open_in_memory().unwrap();
        let mut e = ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z");
        e.body_html = "<p>agenda</p>".into();
        s.upsert_event(&e).unwrap();
        assert_eq!(s.event_body("1").unwrap().as_deref(), Some("<p>agenda</p>"));
    }

    #[test]
    fn event_body_is_none_for_unknown_event() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.event_body("nope").unwrap().is_none());
    }

    #[test]
    fn event_body_is_some_empty_when_event_has_no_body() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z"))
            .unwrap();
        assert_eq!(s.event_body("1").unwrap().as_deref(), Some(""));
    }

    #[test]
    fn calendar_delta_link_round_trips() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.calendar_delta_link().unwrap().is_none());
        s.set_calendar_delta_link("LINK").unwrap();
        assert_eq!(s.calendar_delta_link().unwrap().as_deref(), Some("LINK"));
    }

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
        let id = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        assert!(id.starts_with("local:"));
        // shows in the window
        let rows = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap();
        assert!(rows.iter().any(|e| e.id == id && e.subject == "Sync"));
        // read back for the outbox
        let send = s.event_for_send(&id).unwrap().unwrap();
        assert_eq!(send.subject, "Sync");
        assert_eq!(
            send.attendees,
            vec![("Bob".to_string(), "bob@x.com".to_string())]
        );
        assert_eq!(send.body_html, "<p>agenda</p>");
    }

    #[test]
    fn reconcile_event_id_repoints_event_and_attendees() {
        let s = Store::open_in_memory().unwrap();
        let id = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        s.reconcile_event_id(&id, "EV1").unwrap();
        assert!(s.event_for_send(&id).unwrap().is_none()); // old id gone
        let send = s.event_for_send("EV1").unwrap().unwrap(); // under graph id
        assert_eq!(send.attendees.len(), 1); // attendees moved too
    }

    #[test]
    fn delete_event_removes_it() {
        let s = Store::open_in_memory().unwrap();
        let id = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        s.delete_event(&id).unwrap();
        assert!(s.event_for_send(&id).unwrap().is_none());
        assert!(
            s.events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
                .unwrap()
                .is_empty()
        );
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
        assert_eq!(
            OutboxOp::SaveDraft {
                id: "local:1".into()
            }
            .to_json()
            .to_string(),
            r#"{"kind":"saveDraft","id":"local:1"}"#
        );
        assert_eq!(
            OutboxOp::SendDraft {
                id: "local:1".into()
            }
            .to_json()
            .to_string(),
            r#"{"kind":"sendDraft","id":"local:1"}"#
        );
        assert_eq!(
            OutboxOp::RespondEvent {
                id: "E1".into(),
                kind: "accepted".into(),
                comment: Some("looking forward to it".into()),
            }
            .to_json()
            .to_string(),
            r#"{"kind":"respondEvent","id":"E1","rsvp":"accepted","comment":"looking forward to it"}"#
        );
        assert_eq!(
            OutboxOp::RespondEvent {
                id: "E1".into(),
                kind: "declined".into(),
                comment: None,
            }
            .to_json()
            .to_string(),
            r#"{"kind":"respondEvent","id":"E1","rsvp":"declined","comment":null}"#
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
            OutboxOp::SaveDraft {
                id: "local:M5".into(),
            },
            OutboxOp::SendDraft {
                id: "local:M6".into(),
            },
            OutboxOp::RespondEvent {
                id: "E1".into(),
                kind: "accepted".into(),
                comment: Some("ok".into()),
            },
            OutboxOp::RespondEvent {
                id: "E2".into(),
                kind: "tentativelyAccepted".into(),
                comment: None,
            },
        ];
        for op in ops {
            let encoded = op.to_json().to_string();
            let decoded = OutboxOp::from_json(&json::parse(&encoded).unwrap()).unwrap();
            assert_eq!(decoded, op);
        }
    }

    #[test]
    fn event_mutation_ops_round_trip_through_json() {
        for op in [
            OutboxOp::CreateEvent {
                id: "local:e1".into(),
            },
            OutboxOp::UpdateEvent { id: "EV1".into() },
            OutboxOp::DeleteEvent { id: "EV2".into() },
        ] {
            assert_eq!(OutboxOp::from_json(&op.to_json()), Some(op));
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
        s.update_draft_fields(&id, "New", "b@x", "c@x", "", "new body")
            .unwrap();

        let (row, body) = s.draft(&id).unwrap().expect("draft should still exist");
        assert_eq!(row.subject, "New");
        assert_eq!(row.to_recipients, "b@x");
        assert_eq!(row.cc_recipients, "c@x");
        assert_eq!(body.content, "new body");
    }

    #[test]
    fn update_draft_fields_persists_bcc() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("", "", "", "").unwrap();
        s.update_draft_fields(&id, "Sub", "to@x", "cc@x", "bcc@x", "body")
            .unwrap();
        let (row, _) = s.draft(&id).unwrap().expect("draft should still exist");
        assert_eq!(row.to_recipients, "to@x");
        assert_eq!(row.cc_recipients, "cc@x");
        assert_eq!(row.bcc_recipients, "bcc@x");
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
    fn outbound_attachment_crud_and_dedup() {
        let s = Store::open_in_memory().unwrap();
        // Insert in an order that is NOT alphabetical, to prove
        // `outbound_attachments` returns insertion order (rowid), not name
        // order — the last element must be the most-recently-added one for
        // Ctrl+R's LIFO removal to work.
        s.add_outbound_attachment("local:d1", "/tmp/z.pdf", "z.pdf", 10)
            .unwrap();
        s.add_outbound_attachment("local:d1", "/tmp/a.txt", "a.txt", 20)
            .unwrap();
        s.add_outbound_attachment("local:d1", "/tmp/z.pdf", "z.pdf", 10)
            .unwrap(); // dup path → no-op
        let got = s.outbound_attachments("local:d1").unwrap();
        assert_eq!(got.len(), 2);
        let names: Vec<&str> = got.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["z.pdf", "a.txt"]); // insertion order, not name order
        s.remove_outbound_attachment("local:d1", "/tmp/z.pdf")
            .unwrap();
        assert_eq!(s.outbound_attachments("local:d1").unwrap().len(), 1);
        s.clear_outbound_attachments("local:d1").unwrap();
        assert!(s.outbound_attachments("local:d1").unwrap().is_empty());
    }

    #[test]
    fn reconcile_id_repoints_outbound_attachments() {
        let s = Store::open_in_memory().unwrap();
        // a local draft (message + body) plus a pending attachment on it
        let id = s.create_local_draft("Sub", "", "", "body").unwrap();
        s.add_outbound_attachment(&id, "/tmp/a.pdf", "a.pdf", 10)
            .unwrap();
        s.reconcile_id(&id, "GRAPH-1").unwrap();
        assert!(s.outbound_attachments(&id).unwrap().is_empty()); // old id emptied
        assert_eq!(s.outbound_attachments("GRAPH-1").unwrap().len(), 1); // moved to graph id
    }

    #[test]
    fn draft_returns_none_for_unknown_id() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.draft("nope").unwrap().is_none());
    }

    #[test]
    fn mark_sent_clears_is_draft() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Hi", "a@x", "", "body").unwrap();
        assert!(s.draft(&id).unwrap().unwrap().0.is_draft);
        s.mark_sent(&id);
        assert!(!s.draft(&id).unwrap().unwrap().0.is_draft);
    }

    #[test]
    fn adopt_sentinel_drafts_refiles_into_the_newly_synced_drafts_folder() {
        // A draft created before the first folder sync lands under the local
        // sentinel folder (see `drafts_folder_id`). Once the real Drafts
        // folder syncs down from Graph, `adopt_sentinel_drafts` must re-file
        // it so it doesn't stay stranded under the sentinel forever.
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Hi", "a@x", "", "<p>hi</p>").unwrap();
        assert_eq!(
            s.messages_in_folder(LOCAL_DRAFTS_SENTINEL_FOLDER_ID, 50, 0)
                .unwrap()
                .len(),
            1
        );

        s.upsert_folder(&MailFolder {
            id: "REAL-DRAFTS".into(),
            display_name: "Drafts".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: Some("drafts".into()),
        })
        .unwrap();
        s.adopt_sentinel_drafts("REAL-DRAFTS").unwrap();

        assert!(
            s.messages_in_folder(LOCAL_DRAFTS_SENTINEL_FOLDER_ID, 50, 0)
                .unwrap()
                .is_empty()
        );
        let rows = s.messages_in_folder("REAL-DRAFTS", 50, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
    }

    #[test]
    fn adopt_sentinel_drafts_is_a_no_op_when_nothing_is_filed_under_the_sentinel() {
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
        s.adopt_sentinel_drafts("REAL-DRAFTS").unwrap();
        assert!(
            s.messages_in_folder("REAL-DRAFTS", 50, 0)
                .unwrap()
                .is_empty()
        );
    }
}
