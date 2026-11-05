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

use crate::graph::model::{
    AttachmentKind, AttachmentMeta, Attendee, Body, BodyFormat, Event, MailFolder, MasterCategory,
    Message, MessageFields, Recipient, Recurrence,
};
use crate::json::{self, Value};

mod schema;

/// SQL matching the `messages` row a draft id `?1` addresses now (#1884):
/// the row whose `id` is `?1`, or, for a `local:` id only, the row
/// `reconcile_id` renamed from it (`local_id`). A Graph id is never looked up
/// in `local_id`. Used inside the one statement that acts on the row, so the
/// lookup is atomic against a `reconcile_id` the sync thread's own
/// connection commits. Unqualified, so a `JOIN bodies` (which has no `id` or
/// `local_id` column) can use it as is.
macro_rules! draft_row_sql {
    () => {
        "(id = ?1 OR (substr(?1, 1, 6) = 'local:' AND local_id = ?1))"
    };
}

/// SQL for the id a draft addressed by `?1` is stored under now: the Graph
/// id a `local:` id was reconciled to, else `?1` itself (a Graph id, or a
/// `local:` id never reconciled or whose message is gone). For tables keyed
/// by the message id without being `messages` (`bodies`,
/// `outbound_attachments`); see `draft_row_sql!` for why it is inline SQL.
macro_rules! current_id_sql {
    () => {
        "(CASE WHEN substr(?1, 1, 6) = 'local:'
              THEN COALESCE((SELECT id FROM messages WHERE local_id = ?1), ?1)
              ELSE ?1 END)"
    };
}

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
    /// Whether this folder is expanded in the folder-tree pane. Persisted;
    /// preserved across syncs (`upsert_folder` never writes it).
    pub is_expanded: bool,
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
    /// Mirror of `Message::is_meeting_request` — true for a meeting-invite
    /// (`eventMessageRequest`) message. Drives the reader's RSVP banner.
    pub is_meeting_request: bool,
    /// Assigned category names (color labels). Empty when none.
    pub categories: Vec<String>,
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
    /// Push a message's assigned category names to Graph
    /// (`client.set_message_categories`).
    SetCategories {
        id: String,
        categories: Vec<String>,
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
        proposed_start_utc: Option<String>,
        proposed_end_utc: Option<String>,
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
            OutboxOp::SetCategories { .. } => "setCategories",
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
            OutboxOp::SetCategories { id, categories } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                (
                    "categories".to_string(),
                    Value::Array(categories.iter().map(|c| Value::Str(c.clone())).collect()),
                ),
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
                proposed_start_utc: v
                    .get("proposedStart")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                proposed_end_utc: v
                    .get("proposedEnd")
                    .and_then(Value::as_str)
                    .map(str::to_string),
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

/// An `events` row, as read back from the store. `series_master_id` is
/// `Some` when the event is an occurrence of a recurring series — lookxy's
/// `App::open_edit_event`/`delete_selected_event` refuse to edit/delete those
/// (recurring events stay read + RSVP only; see the calendar-edit design
/// spec), so the flag has to travel with every row the agenda holds, not just
/// `event_for_send`'s single-event read.
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
    pub series_master_id: Option<String>,
    pub reminder_minutes: i64,
    pub is_reminder_on: bool,
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
/// Same field set as `EventRow` plus the extra columns (`body_preview`,
/// `web_link`, `last_modified`, `body_html`) that `EventRow` doesn't surface.
/// `graph::model::Event` (Task 2) turned out to be a field-for-field
/// superset of this (it additionally carries `attendees`),
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
    pub reminder_minutes: i64,
    pub is_reminder_on: bool,
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
            reminder_minutes: e.reminder_minutes,
            is_reminder_on: e.is_reminder_on,
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
    /// The recurrence pattern for a repeating event (create-only), or `None`.
    pub recurrence: Option<Recurrence>,
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
    /// The recurrence pattern for a repeating event (create-only), or `None`.
    pub recurrence: Option<Recurrence>,
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
        // Same idempotent-migration pattern, for `events.recurrence` (recurring
        // event creation): the serialized Graph recurrence JSON, or `''`.
        let _ = conn.execute(
            "ALTER TABLE events ADD COLUMN recurrence TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Same idempotent-migration pattern, for `events.reminder_minutes`/
        // `is_reminder_on` (event reminders/alerts).
        let _ = conn.execute(
            "ALTER TABLE events ADD COLUMN reminder_minutes INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE events ADD COLUMN is_reminder_on INTEGER NOT NULL DEFAULT 0",
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
        // Additive migration for DBs created before `attachments.content_id`
        // existed. SQLite has no `ADD COLUMN IF NOT EXISTS`, so attempt the
        // ALTER and swallow the "duplicate column name" error it raises when
        // the column is already present (fresh DBs get it from schema.rs).
        let _ = conn.execute("ALTER TABLE attachments ADD COLUMN content_id TEXT", []);
        // Same idempotent-migration pattern, for `attachments.kind`/
        // `source_url` (added alongside `content_id` to support non-file
        // attachments: itemAttachments and referenceAttachments).
        let _ = conn.execute(
            "ALTER TABLE attachments ADD COLUMN kind TEXT NOT NULL DEFAULT 'file'",
            [],
        );
        let _ = conn.execute("ALTER TABLE attachments ADD COLUMN source_url TEXT", []);
        // Same idempotent-migration pattern as `is_draft`/`bcc_recipients`
        // above, for `messages.is_meeting_request` (meeting-invite RSVP): the
        // ALTER errors ("duplicate column name") on any DB already carrying it
        // (every fresh DB gets it from schema.rs), so swallow that.
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN is_meeting_request INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // Same idempotent-migration pattern, for `messages.categories` (color
        // labels): an empty list is stored as `""`.
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN categories TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Same idempotent-migration pattern, for `folders.is_expanded` (the
        // folder-tree collapse state). Persisted per folder and deliberately
        // left untouched by `upsert_folder`, so a re-sync never resets the
        // user's expand/collapse choices.
        let _ = conn.execute(
            "ALTER TABLE folders ADD COLUMN is_expanded INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // Same idempotent-migration pattern, for `messages.local_id` (#1884:
        // the `local:` id a reconciled draft had). Its unique index has to
        // come after the ALTER, not in `SCHEMA_SQL`: on an old database
        // `SCHEMA_SQL` runs before the column exists. NULLs never collide.
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN local_id TEXT", []);
        // Same idempotent-migration pattern, for `bodies.fetched_as` (#1901:
        // the format `FetchBody` asked for). Rows from before it stay NULL,
        // which `get_body_for` reads as "asked for text" for a text body,
        // as the engine then always did.
        let _ = conn.execute("ALTER TABLE bodies ADD COLUMN fetched_as TEXT", []);
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_local_id ON messages(local_id)",
            [],
        )?;
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
            "SELECT id, parent_id, display_name, total_count, unread_count, delta_link, well_known_name, sort_order, is_expanded
             FROM folders
             ORDER BY CASE
                 WHEN well_known_name = 'inbox' OR LOWER(display_name) = 'inbox' THEN 0
                 WHEN well_known_name = 'drafts' THEN 1
                 WHEN well_known_name = 'sentitems' THEN 2
                 WHEN well_known_name = 'deleteditems' THEN 3
                 WHEN well_known_name = 'junkemail' THEN 4
                 WHEN well_known_name = 'archive' THEN 5
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
                    is_expanded: row.get::<_, i64>(8)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Sets a single folder's tree-expand flag (the folder-pane `→`/`←`/Space
    /// toggle). A no-op if the id is unknown.
    pub fn set_folder_expanded(&self, id: &str, expanded: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET is_expanded = ?2 WHERE id = ?1",
            params![id, expanded as i64],
        )?;
        Ok(())
    }

    /// Inserts or updates a message row, filing it under `folder_id`: every
    /// column from `m` (`merge_message` with every property carried).
    pub fn upsert_message(&self, folder_id: &str, m: &Message) -> Result<(), StoreError> {
        self.merge_message(folder_id, m, &MessageFields::all())
    }

    /// Inserts or updates a message row from a Graph item that may be
    /// partial (#1899), filing it under `folder_id`: on an existing row only
    /// the columns whose property `fields` says the item carried are
    /// written, so an absent property never blanks a stored one; a missing
    /// row is inserted with what the item carried (empty otherwise). With
    /// every property carried it writes every column (`upsert_message`).
    /// `is_meeting_request` and `is_draft` follow
    /// `MessageFields::writes_meeting_request` / `writes_is_draft`.
    ///
    /// One static statement, so the FTS update trigger fires once.
    pub fn merge_message(
        &self,
        folder_id: &str,
        m: &Message,
        fields: &MessageFields,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO messages (
                 id, folder_id, conversation_id, subject, from_name, from_addr,
                 to_recipients, cc_recipients, received_at, sent_at,
                 is_read, is_flagged, has_attachments, importance, preview, is_draft,
                 is_meeting_request, categories
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(id) DO UPDATE SET
                 folder_id = excluded.folder_id,
                 conversation_id = CASE WHEN ?19 THEN excluded.conversation_id ELSE messages.conversation_id END,
                 subject = CASE WHEN ?20 THEN excluded.subject ELSE messages.subject END,
                 from_name = CASE WHEN ?21 THEN excluded.from_name ELSE messages.from_name END,
                 from_addr = CASE WHEN ?21 THEN excluded.from_addr ELSE messages.from_addr END,
                 to_recipients = CASE WHEN ?22 THEN excluded.to_recipients ELSE messages.to_recipients END,
                 cc_recipients = CASE WHEN ?23 THEN excluded.cc_recipients ELSE messages.cc_recipients END,
                 received_at = CASE WHEN ?24 THEN excluded.received_at ELSE messages.received_at END,
                 sent_at = CASE WHEN ?25 THEN excluded.sent_at ELSE messages.sent_at END,
                 is_read = CASE WHEN ?26 THEN excluded.is_read ELSE messages.is_read END,
                 is_flagged = CASE WHEN ?27 THEN excluded.is_flagged ELSE messages.is_flagged END,
                 has_attachments = CASE WHEN ?28 THEN excluded.has_attachments ELSE messages.has_attachments END,
                 importance = CASE WHEN ?29 THEN excluded.importance ELSE messages.importance END,
                 preview = CASE WHEN ?30 THEN excluded.preview ELSE messages.preview END,
                 is_draft = CASE WHEN ?31 THEN excluded.is_draft ELSE messages.is_draft END,
                 is_meeting_request = CASE WHEN ?32 THEN excluded.is_meeting_request ELSE messages.is_meeting_request END,
                 categories = CASE WHEN ?33 THEN excluded.categories ELSE messages.categories END",
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
                m.is_meeting_request,
                encode_categories(&m.categories),
                fields.conversation_id,
                fields.subject,
                fields.from,
                fields.to,
                fields.cc,
                fields.received,
                fields.sent,
                fields.is_read,
                fields.flag,
                fields.has_attachments,
                fields.importance,
                fields.preview,
                fields.writes_is_draft(),
                fields.writes_meeting_request(),
                fields.categories,
            ],
        )?;
        Ok(())
    }

    /// Up to `limit` ids of `folder_id`'s header-less rows (`received_at`
    /// empty: every message Graph has carries `receivedDateTime`), oldest
    /// row first, for the sync engine to complete from Graph (#1899). A row
    /// docxy made itself is left out: a `local:` id, a draft
    /// (`is_draft = 1`, still edited here), and a row with a `local_id` (a
    /// draft reconciled and then sent: `mark_sent` cleared `is_draft`, and a
    /// `SendDraft` retry may still need the row). Mail Graph delivered has
    /// none of these.
    pub fn headerless_message_ids(
        &self,
        folder_id: &str,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM messages
             WHERE folder_id = ?1 AND received_at = ''
               AND substr(id, 1, 6) <> 'local:' AND is_draft = 0 AND local_id IS NULL
             ORDER BY rowid LIMIT ?2",
        )?;
        let ids = stmt
            .query_map(params![folder_id, limit as i64], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    /// Deletes a message by id. Affects zero rows (not an error) if the id
    /// doesn't match anything — including an empty id, which a delta
    /// `@removed` entry lacking `id` can produce (see `graph::model`).
    ///
    /// A draft's `local:` id still reaches it after `reconcile_id` (#1884),
    /// and the deleted row takes that alias with it. So a caller that will
    /// also send `SyncCommand::Delete` uses `delete_message_resolved`
    /// instead, and sends the command with the id it returns: once the row
    /// is gone, a `local:` id no longer leads to the Graph id.
    pub fn delete_message(&self, id: &str) -> Result<(), StoreError> {
        self.delete_message_resolved(id)?;
        Ok(())
    }

    /// `delete_message`, returning the id the deleted row was stored under
    /// (the Graph id for a reconciled draft's `local:` id, #1884), or `None`
    /// when nothing matched. The match and the delete are one statement, so
    /// a `reconcile_id` the sync thread commits on its own connection cannot
    /// land between them. A caller that also sends `SyncCommand::Delete`
    /// sends it with the returned id, else with the id it had.
    pub fn delete_message_resolved(&self, id: &str) -> Result<Option<String>, StoreError> {
        let deleted = self
            .conn
            .query_row(
                concat!(
                    "DELETE FROM messages WHERE ",
                    draft_row_sql!(),
                    " RETURNING id"
                ),
                params![id],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        Ok(deleted)
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
                    bcc_recipients, is_meeting_request, categories
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
                        bcc_recipients, is_meeting_request, categories,
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
                    k.bcc_recipients, k.is_meeting_request, k.categories
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

    /// Locally sets a message's category names (the optimistic half of
    /// `SyncCommand::SetCategories`). See `set_read` for why this doesn't
    /// return a `Result`.
    pub fn set_categories(&self, id: &str, categories: &[String]) {
        let _ = self.conn.execute(
            "UPDATE messages SET categories = ?1 WHERE id = ?2",
            params![encode_categories(categories), id],
        );
    }

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

    /// Locally marks a draft as sent (`is_draft = 0`) — the optimistic half
    /// of `SyncCommand::SendDraft` (the sync engine's outbox, via
    /// `OutboxOp::SendDraft`, is what actually hands the draft to Graph for
    /// delivery). See `set_read` for why this doesn't return a `Result`: a
    /// mismatched/already-gone `id` just changes zero rows. A reconciled
    /// draft's `local:` id still reaches it (#1884).
    pub fn mark_sent(&self, id: &str) {
        let _ = self.conn.execute(
            concat!("UPDATE messages SET is_draft = 0 WHERE ", draft_row_sql!()),
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
    /// local id is left as-is here. A reconciled draft's `local:` id still
    /// reaches it (#1884; `SendDraft`'s optimistic re-file into Sent Items).
    pub fn move_message(&self, id: &str, dest_folder_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            concat!(
                "UPDATE messages SET folder_id = ?2 WHERE ",
                draft_row_sql!()
            ),
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

    /// Inserts or replaces the body of a message, as a body that was not
    /// fetched by `FetchBody` (a draft's, written locally or from
    /// `createReply`): no fetch format is recorded (`fetched_as` NULL; see
    /// [`Store::get_body_for`]). The schema's `bodies_fts_*` triggers (see
    /// `schema.rs`) keep `messages_fts` in step with this automatically, so
    /// `search` sees the new content right away.
    pub fn put_body(&self, message_id: &str, b: &Body) -> Result<(), StoreError> {
        self.write_body(message_id, b, None)
    }

    /// Inserts or replaces the body of a message as `FetchBody` fetched it,
    /// recording the format it asked Graph for (`fetched_as`, #1901), which
    /// for `Html` can differ from the content type that came back (a
    /// plain-text message's native body is text).
    pub fn put_fetched_body(
        &self,
        message_id: &str,
        b: &Body,
        asked: BodyFormat,
    ) -> Result<(), StoreError> {
        self.write_body(message_id, b, Some(asked))
    }

    fn write_body(
        &self,
        message_id: &str,
        b: &Body,
        asked: Option<BodyFormat>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO bodies (message_id, content_type, content, fetched_as) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(message_id) DO UPDATE SET
                 content_type = excluded.content_type,
                 content = excluded.content,
                 fetched_as = excluded.fetched_as",
            params![message_id, b.content_type, b.content, asked.map(BodyFormat::as_str)],
        )?;
        Ok(())
    }

    /// The stored body of a message when it satisfies `format`, else `None`
    /// (#1901): what an embedder checks before sending `FetchBody`, so a
    /// store filled under the other format upgrades one message at a time
    /// and a body is fetched once per format, not on every open. Use
    /// [`Store::get_body`] to show whatever is stored meanwhile.
    ///
    /// A body satisfies `format` unless it was asked for in the other one.
    /// What it was asked for is `fetched_as` when `FetchBody` recorded it;
    /// otherwise (`NULL`) a text body counts as asked for text, since an
    /// engine from before the column always asked for text, and an html
    /// body (a draft written locally, or by `createReply`) as asked for
    /// neither, so it satisfies both. So a plain-text message fetched under
    /// `Html` (its native body is text) satisfies `Html` and is not fetched
    /// again. One edge: a `createReply` draft of a plain-text message comes
    /// back as text and is stored with no format, so under `Html` it reads
    /// as asked for text and is fetched once more, natively (the same text);
    /// compose edits are stored as html (`update_draft_fields`), so they
    /// are never invited to be overwritten.
    pub fn get_body_for(
        &self,
        message_id: &str,
        format: BodyFormat,
    ) -> Result<Option<Body>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT content_type, content, fetched_as FROM bodies WHERE message_id = ?1",
                params![message_id],
                |row| {
                    Ok((
                        Body {
                            content_type: row.get(0)?,
                            content: row.get(1)?,
                        },
                        row.get::<_, Option<String>>(2)?,
                    ))
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
        let Some((body, fetched_as)) = row else {
            return Ok(None);
        };
        let satisfies = match fetched_as.as_deref() {
            // A value this build does not know is not `format`: refetch.
            Some(asked) => BodyFormat::parse(asked) == Some(format),
            None => body.content_type.eq_ignore_ascii_case("html") || format == BodyFormat::Text,
        };
        Ok(satisfies.then_some(body))
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
    /// with `update_draft_fields`/`draft`, and the `SaveDraft`/`SendDraft`
    /// commands; it keeps working after `reconcile_id` stores the draft under
    /// its Graph id (#1884).
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
    ///
    /// A reconciled draft's `local:` id still reaches it (#1884): the row is
    /// matched by `draft_row_sql!` and the body keyed by `current_id_sql!`,
    /// in one transaction whose first statement is the write, so both see
    /// the same state.
    ///
    /// The body is html (compose writes html), so a draft whose stored body
    /// was fetched as text becomes `content_type = 'html'` with no fetch
    /// format (#1901): [`Store::get_body_for`] then keeps the user's edits
    /// in either format rather than inviting a `FetchBody` over them.
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
            concat!(
                "UPDATE messages SET subject = ?2, to_recipients = ?3, cc_recipients = ?4, bcc_recipients = ?5
                 WHERE ",
                draft_row_sql!()
            ),
            params![id, subject, to, cc, bcc],
        )?;
        tx.execute(
            concat!(
                "INSERT INTO bodies (message_id, content_type, content) VALUES (",
                current_id_sql!(),
                ", 'html', ?2)
                 ON CONFLICT(message_id) DO UPDATE SET
                     content_type = 'html', content = excluded.content, fetched_as = NULL"
            ),
            params![id, body_html],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records a file attached to draft `draft_id`. Idempotent per (draft, path).
    /// A reconciled draft's `local:` id still reaches it (#1884), as in every
    /// `outbound_attachments` call: the row is keyed by `current_id_sql!`.
    pub fn add_outbound_attachment(
        &self,
        draft_id: &str,
        path: &str,
        name: &str,
        size: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            concat!(
                "INSERT OR IGNORE INTO outbound_attachments (draft_id, path, name, size) VALUES (",
                current_id_sql!(),
                ", ?2, ?3, ?4)"
            ),
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
        let mut stmt = self.conn.prepare(concat!(
            "SELECT path, name, size FROM outbound_attachments WHERE draft_id = ",
            current_id_sql!(),
            " ORDER BY rowid"
        ))?;
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
            concat!(
                "DELETE FROM outbound_attachments WHERE draft_id = ",
                current_id_sql!(),
                " AND path = ?2"
            ),
            params![draft_id, path],
        )?;
        Ok(())
    }

    /// Removes every attached file from `draft_id` (after a send, or on discard).
    pub fn clear_outbound_attachments(&self, draft_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            concat!(
                "DELETE FROM outbound_attachments WHERE draft_id = ",
                current_id_sql!()
            ),
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
    ///
    /// A `local:` id is kept on the row as `local_id` (#1884), so a caller
    /// still holding it (a compose window left open across `SaveDraft`, or
    /// a queued `SendDraft` retried after its send failed, #1881) keeps
    /// reaching the draft: see `current_id` and the calls that take a draft
    /// id. The alias goes with the row whenever the message is deleted.
    pub fn reconcile_id(&self, local_id: &str, graph_id: &str) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        tx.execute(
            "UPDATE messages SET id = ?2,
                 local_id = CASE WHEN substr(?1, 1, 6) = 'local:' THEN ?1 ELSE local_id END
             WHERE id = ?1",
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

    /// Loads a draft (its message row and body) for editing, by its `local:`
    /// id (before sync, and still after `reconcile_id`, #1884) or its Graph
    /// id. The returned row's `id` is the id it is stored under now. `None`
    /// if there's no message with that id, or no body stored for it — either
    /// of which means there's nothing for compose to load. One statement, so
    /// the row and body always belong together.
    pub fn draft(&self, id: &str) -> Result<Option<(MessageRow, Body)>, StoreError> {
        let mut stmt = self.conn.prepare(concat!(
            "SELECT id, folder_id, conversation_id, subject, from_name, from_addr,
                    to_recipients, cc_recipients, received_at, sent_at,
                    is_read, is_flagged, has_attachments, importance, preview, is_draft,
                    bcc_recipients, is_meeting_request, categories,
                    bodies.content_type, bodies.content
             FROM messages JOIN bodies ON bodies.message_id = messages.id
             WHERE ",
            draft_row_sql!()
        ))?;
        let found = stmt
            .query_map(params![id], |r| {
                Ok((
                    map_message_row(r)?,
                    Body {
                        content_type: r.get(19)?,
                        content: r.get(20)?,
                    },
                ))
            })?
            .next()
            .transpose()?;
        Ok(found)
    }

    /// The id the message addressed by `id` is stored under now: the Graph
    /// id a draft's `local:` id was reconciled to (`reconcile_id`, #1884),
    /// else `id` itself (a Graph id, which is never looked up, or a `local:`
    /// id never reconciled or whose message has since been deleted).
    pub fn current_id(&self, id: &str) -> Result<String, StoreError> {
        let current =
            self.conn
                .query_row(concat!("SELECT ", current_id_sql!()), params![id], |r| {
                    r.get(0)
                })?;
        Ok(current)
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
                "INSERT INTO attachments (id, message_id, name, content_type, size, is_inline, content_id, kind, source_url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    a.id,
                    message_id,
                    a.name,
                    a.content_type,
                    a.size,
                    a.is_inline,
                    a.content_id,
                    a.kind.as_db_str(),
                    a.source_url,
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
            "SELECT id, name, content_type, size, is_inline, content_id, kind, source_url
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
                    content_id: row.get(5)?,
                    kind: AttachmentKind::from_db_str(&row.get::<_, String>(6)?),
                    source_url: row.get(7)?,
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
                    bcc_recipients, is_meeting_request, categories
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
                 body_html, reminder_minutes, is_reminder_on
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
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
                 body_html = excluded.body_html,
                 reminder_minutes = excluded.reminder_minutes,
                 is_reminder_on = excluded.is_reminder_on",
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
                e.reminder_minutes,
                e.is_reminder_on,
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
    /// The window delete excludes `id LIKE 'local:%'` — a not-yet-synced
    /// optimistic create (see `Store::create_local_event`/`CreateEvent`) has
    /// no Graph id yet, so it can never appear in `events` (server truth for
    /// this window) no matter how current the refetch is. Pruning it
    /// anyway would silently delete the user's just-created event out from
    /// under a pending `CreateEvent` op, which then finds no such row at
    /// `event_for_send` time and quarantines instead of sending — the same
    /// "server truth never clobbers an unsynced local row" rule
    /// `drafts_folder_id`/local drafts already follow. A `local:` id is
    /// always `local:<hex>` (see `create_local_event`), which contains no
    /// `LIKE` metacharacters (`%`/`_`), so no `ESCAPE` clause is needed. Once
    /// an event reconciles to its real Graph id (or a genuinely
    /// server-cancelled event just keeps its Graph id), it's a normal row
    /// again and this exclusion no longer applies — it prunes on the next
    /// refresh like any other event no longer present server-side.
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
            "DELETE FROM events WHERE start_utc < ?2 AND end_utc > ?1 AND id NOT LIKE 'local:%'",
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
                    organizer_name, organizer_addr, response_status, series_master_id,
                    reminder_minutes, is_reminder_on
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
                    series_master_id: row.get(9)?,
                    reminder_minutes: row.get(10)?,
                    is_reminder_on: row.get(11)?,
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
            reminder_minutes: 0,
            is_reminder_on: false,
        })?;
        self.put_event_attendees(&id, &to_new_attendees(&f.attendees))?;
        if let Some(rec) = &f.recurrence {
            self.conn.execute(
                "UPDATE events SET recurrence = ?1 WHERE id = ?2",
                params![rec.to_json().to_string(), id],
            )?;
        }
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
                .and_then(|v| Recurrence::from_json(&v))
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
            recurrence,
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
        is_meeting_request: row.get(17)?,
        categories: decode_categories(&row.get::<_, String>(18)?),
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
            is_meeting_request: false,
            categories: Vec::new(),
        }
    }

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
        assert_eq!(
            rows[0].categories,
            vec!["Work".to_string(), "A B".to_string()]
        );
    }

    #[test]
    fn master_categories_replace_and_read_round_trip() {
        use crate::graph::model::MasterCategory;
        let s = Store::open_in_memory().unwrap();
        s.replace_master_categories(&[
            MasterCategory {
                display_name: "Work".into(),
                color: "preset0".into(),
            },
            MasterCategory {
                display_name: "Urgent".into(),
                color: "preset1".into(),
            },
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

    #[test]
    fn inbox_sorts_first_even_without_a_well_known_name() {
        let s = Store::open_in_memory().unwrap();
        // An "Inbox" whose wellKnownName didn't come through, plus a user folder
        // that would sort before "Inbox" alphabetically.
        for (id, name) in [("f2", "Archive Stuff"), ("f1", "Inbox")] {
            s.upsert_folder(&MailFolder {
                id: id.into(),
                display_name: name.into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: None,
            })
            .unwrap();
        }
        assert_eq!(s.folders().unwrap()[0].display_name, "Inbox");
    }

    #[test]
    fn folder_expanded_persists_and_survives_resync() {
        let s = Store::open_in_memory().unwrap();
        let inbox = MailFolder {
            id: "F1".into(),
            display_name: "Inbox".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: Some("inbox".into()),
        };
        s.upsert_folder(&inbox).unwrap();
        assert!(!s.folders().unwrap()[0].is_expanded); // default collapsed
        s.set_folder_expanded("F1", true).unwrap();
        assert!(s.folders().unwrap()[0].is_expanded);
        // A re-sync upsert must NOT reset the flag.
        s.upsert_folder(&inbox).unwrap();
        assert!(s.folders().unwrap()[0].is_expanded);
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

    fn folder(s: &Store, id: &str) {
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

    /// Every column of a message row, as SQLite holds it.
    fn raw_row(s: &Store, id: &str) -> Vec<rusqlite::types::Value> {
        let mut stmt = s
            .conn
            .prepare("SELECT * FROM messages WHERE id = ?1")
            .unwrap();
        let n = stmt.column_count();
        stmt.query_row(params![id], |r| {
            (0..n)
                .map(|i| r.get::<_, rusqlite::types::Value>(i))
                .collect()
        })
        .unwrap()
    }

    /// The fields a Graph item `{id, isRead}` carries.
    fn read_only_fields() -> MessageFields {
        MessageFields {
            is_read: true,
            ..MessageFields::default()
        }
    }

    #[test]
    fn merge_message_updates_only_present_columns() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        folder(&s, "G");
        let mut full = msg("11", false);
        full.is_flagged = true;
        full.categories = vec!["Work".into()];
        full.to = vec![Recipient {
            name: "B".into(),
            address: "b@x".into(),
        }];
        s.upsert_message("F", &full).unwrap();
        let before = raw_row(&s, "11");

        // `{id, isRead: true}` as `Message::from_json` parses it: every
        // other field empty.
        let partial = Message {
            id: "11".into(),
            conversation_id: String::new(),
            subject: String::new(),
            from: Recipient::default(),
            to: vec![],
            cc: vec![],
            received: String::new(),
            sent: String::new(),
            is_read: true,
            is_flagged: false,
            has_attachments: false,
            importance: String::new(),
            preview: String::new(),
            is_draft: false,
            is_meeting_request: false,
            categories: vec![],
        };
        s.merge_message("G", &partial, &read_only_fields()).unwrap();
        let after = raw_row(&s, "11");

        let stmt = s.conn.prepare("SELECT * FROM messages LIMIT 0").unwrap();
        let names: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
        for (i, name) in names.iter().enumerate() {
            match name.as_str() {
                "is_read" => assert_eq!(after[i], rusqlite::types::Value::Integer(1)),
                "folder_id" => assert_eq!(after[i], rusqlite::types::Value::Text("G".into())),
                _ => assert_eq!(after[i], before[i], "{name} kept"),
            }
        }
        let is_read = names.iter().position(|n| n == "is_read").unwrap();
        assert_ne!(before[is_read], after[is_read]);
        // The full-text index still finds the kept subject.
        let hits = s.search("s11", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn merge_message_inserts_missing_row() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        let mut m = msg("12", true);
        m.subject = String::new();
        m.received = String::new();
        s.merge_message("F", &m, &read_only_fields()).unwrap();
        let rows = s.messages_in_folder("F", 10, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "12");
        assert!(rows[0].is_read);
        assert_eq!(rows[0].subject, "");
        assert_eq!(rows[0].received_at, "");
        // And it is header-less, so the engine will complete it.
        assert_eq!(s.headerless_message_ids("F", 10).unwrap(), vec!["12"]);
    }

    /// `upsert_message` (a merge with every property carried) writes every
    /// column from the message, on insert and over an existing row, and
    /// leaves the columns it does not own alone. Spelled out column by
    /// column, as the statement was before it became a merge.
    #[test]
    fn upsert_writes_every_column() {
        use rusqlite::types::Value as V;
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        folder(&s, "G");
        let names: Vec<String> = s
            .conn
            .prepare("SELECT * FROM messages LIMIT 0")
            .unwrap()
            .column_names()
            .iter()
            .map(|c| c.to_string())
            .collect();
        let expect = |s: &Store, id: &str, want: &[(&str, V)]| {
            let row = raw_row(s, id);
            for (i, name) in names.iter().enumerate() {
                let w = want
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| panic!("column {name} not covered"));
                assert_eq!(row[i], w, "{name}");
            }
        };
        let t = |x: &str| V::Text(x.into());
        let mut m = msg("13", true);
        m.is_flagged = true;
        m.has_attachments = true;
        m.is_draft = true;
        m.is_meeting_request = true;
        m.categories = vec!["X".into(), "Y".into()];
        m.to = vec![Recipient {
            name: "B".into(),
            address: "b@x".into(),
        }];
        m.cc = vec![Recipient {
            name: "C".into(),
            address: "c@x".into(),
        }];
        m.sent = "2026-07-13T00:00:00Z".into();
        s.upsert_message("F", &m).unwrap();
        let first = [
            ("id", t("13")),
            ("folder_id", t("F")),
            ("conversation_id", t("C")),
            ("subject", t("s13")),
            ("from_name", t("A")),
            ("from_addr", t("a@x")),
            ("to_recipients", t(&encode_recipients(&m.to))),
            ("cc_recipients", t(&encode_recipients(&m.cc))),
            ("bcc_recipients", t("")),
            ("received_at", t("2026-07-13T00:00:00Z")),
            ("sent_at", t("2026-07-13T00:00:00Z")),
            ("is_read", V::Integer(1)),
            ("is_flagged", V::Integer(1)),
            ("has_attachments", V::Integer(1)),
            ("importance", t("normal")),
            ("preview", t("p")),
            ("is_draft", V::Integer(1)),
            ("is_meeting_request", V::Integer(1)),
            ("categories", t(&encode_categories(&m.categories))),
            ("local_id", V::Null),
        ];
        expect(&s, "13", &first);

        // Over the row, with every column changed (to empty / false where
        // a value can be), moved to G; bcc (not a Graph column here) kept.
        s.conn
            .execute(
                "UPDATE messages SET bcc_recipients = 'kept' WHERE id = '13'",
                [],
            )
            .unwrap();
        let m2 = Message {
            id: "13".into(),
            conversation_id: "C2".into(),
            subject: "changed".into(),
            from: Recipient::default(),
            to: vec![],
            cc: vec![],
            received: String::new(),
            sent: String::new(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "high".into(),
            preview: String::new(),
            is_draft: false,
            is_meeting_request: false,
            categories: vec![],
        };
        s.upsert_message("G", &m2).unwrap();
        expect(
            &s,
            "13",
            &[
                ("id", t("13")),
                ("folder_id", t("G")),
                ("conversation_id", t("C2")),
                ("subject", t("changed")),
                ("from_name", t("")),
                ("from_addr", t("")),
                ("to_recipients", t(&encode_recipients(&[]))),
                ("cc_recipients", t(&encode_recipients(&[]))),
                ("bcc_recipients", t("kept")),
                ("received_at", t("")),
                ("sent_at", t("")),
                ("is_read", V::Integer(0)),
                ("is_flagged", V::Integer(0)),
                ("has_attachments", V::Integer(0)),
                ("importance", t("high")),
                ("preview", t("")),
                ("is_draft", V::Integer(0)),
                ("is_meeting_request", V::Integer(0)),
                ("categories", t(&encode_categories(&[]))),
                ("local_id", V::Null),
            ],
        );
    }

    /// The `MessageFields` of a complete Graph item under `MESSAGE_SELECT`:
    /// every selected key, no `isDraft` (not selected) and no `@odata.type`
    /// (a plain message).
    fn complete_plain_fields() -> MessageFields {
        MessageFields {
            is_draft: false,
            odata_type: false,
            ..MessageFields::all()
        }
    }

    #[test]
    fn merge_plain_message_clears_meeting_flag_like_upsert() {
        let a = Store::open_in_memory().unwrap();
        let b = Store::open_in_memory().unwrap();
        let mut invite = msg("14", false);
        invite.is_meeting_request = true;
        for s in [&a, &b] {
            folder(s, "F");
            s.upsert_message("F", &invite).unwrap();
        }
        let plain = msg("14", false);
        let fields = complete_plain_fields();
        assert!(!fields.is_partial());
        a.upsert_message("F", &plain).unwrap();
        b.merge_message("F", &plain, &fields).unwrap();
        assert_eq!(raw_row(&a, "14"), raw_row(&b, "14"));
        let flag: bool = b
            .conn
            .query_row(
                "SELECT is_meeting_request FROM messages WHERE id = '14'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!flag);
    }

    #[test]
    fn merge_complete_item_clears_draft_like_upsert_and_partial_keeps_it() {
        let a = Store::open_in_memory().unwrap();
        let b = Store::open_in_memory().unwrap();
        let mut draft = msg("15", false);
        draft.is_draft = true;
        for s in [&a, &b] {
            folder(s, "F");
            s.upsert_message("F", &draft).unwrap();
        }
        let is_draft = |s: &Store| -> bool {
            s.conn
                .query_row("SELECT is_draft FROM messages WHERE id = '15'", [], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        // A partial `{id, isRead}` keeps the draft a draft.
        let mut read = msg("15", true);
        read.is_draft = false;
        b.merge_message("F", &read, &read_only_fields()).unwrap();
        assert!(is_draft(&b));
        // A complete item (no `isDraft` key) writes 0, as `upsert_message`.
        let plain = msg("15", false);
        a.upsert_message("F", &plain).unwrap();
        b.merge_message("F", &plain, &complete_plain_fields())
            .unwrap();
        assert_eq!(raw_row(&a, "15"), raw_row(&b, "15"));
        assert!(!is_draft(&b));
    }

    #[test]
    fn headerless_ids_skip_local_and_draft_rows() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        folder(&s, "Other");
        let blank = |id: &str| {
            let mut m = msg("01", false);
            m.id = id.into();
            m.received = String::new();
            m
        };
        s.upsert_message("F", &blank("G1")).unwrap();
        s.upsert_message("F", &msg("02", false)).unwrap(); // complete: not listed
        s.upsert_message("Other", &blank("G2")).unwrap(); // another folder
        s.upsert_message("F", &blank("local:abc")).unwrap();
        let mut draft = blank("G3");
        draft.is_draft = true;
        s.upsert_message("F", &draft).unwrap();
        s.upsert_message("F", &blank("G4")).unwrap();
        assert_eq!(s.headerless_message_ids("F", 10).unwrap(), vec!["G1", "G4"]);
        assert_eq!(s.headerless_message_ids("F", 1).unwrap(), vec!["G1"]);
    }

    #[test]
    fn headerless_ids_skip_reconciled_sent_draft() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "SENT");
        // A draft written here, reconciled to its Graph id, then sent:
        // `local_id` set, `is_draft = 0`, `received_at = ''`, in Sent Items.
        let local = s.create_local_draft("Hi", "bob@x", "", "<p>x</p>").unwrap();
        s.reconcile_id(&local, "GRAPH-DRAFT").unwrap();
        s.mark_sent("GRAPH-DRAFT");
        s.move_message("GRAPH-DRAFT", "SENT").unwrap();
        let (received, is_draft): (String, bool) = s
            .conn
            .query_row(
                "SELECT received_at, is_draft FROM messages WHERE id = 'GRAPH-DRAFT'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((received.as_str(), is_draft), ("", false));
        assert!(s.headerless_message_ids("SENT", 10).unwrap().is_empty());
    }

    #[test]
    fn attachments_round_trip_content_id() {
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
        s.put_attachments(
            "10",
            &[AttachmentMeta {
                id: "a1".into(),
                name: "logo.png".into(),
                content_type: "image/png".into(),
                size: 10,
                is_inline: true,
                content_id: Some("logo123".into()),
                kind: AttachmentKind::File,
                source_url: None,
            }],
        )
        .unwrap();
        let got = s.attachments("10").unwrap();
        assert_eq!(got[0].content_id.as_deref(), Some("logo123"));
    }

    #[test]
    fn attachments_round_trip_kind_and_source_url() {
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
        s.upsert_message("F", &msg("m1", false)).unwrap();
        s.put_attachments(
            "m1",
            &[AttachmentMeta {
                id: "a1".into(),
                name: "Doc".into(),
                content_type: "".into(),
                size: 0,
                is_inline: false,
                content_id: None,
                kind: AttachmentKind::Reference,
                source_url: Some("https://x/y".into()),
            }],
        )
        .unwrap();
        let got = s.attachments("m1").unwrap();
        assert_eq!(got[0].kind, AttachmentKind::Reference);
        assert_eq!(got[0].source_url.as_deref(), Some("https://x/y"));
    }

    #[test]
    fn content_id_migration_is_idempotent() {
        // `Store::init` always runs `ALTER TABLE attachments ADD COLUMN
        // content_id` right after `SCHEMA_SQL`, even on a fresh in-memory DB
        // where `SCHEMA_SQL` already created that column (see `schema.rs`) —
        // so every `Store::open_in_memory()` call already exercises the
        // "duplicate column name" error being swallowed by `let _ =` in
        // `init`. There's no test accessor for the raw connection (and one
        // isn't worth adding just for this), so this test instead confirms
        // the store is still fully usable for attachments afterward.
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
        s.upsert_message("F", &msg("11", false)).unwrap();
        s.put_attachments(
            "11",
            &[AttachmentMeta {
                id: "a1".into(),
                name: "x.txt".into(),
                content_type: "text/plain".into(),
                size: 1,
                is_inline: false,
                content_id: None,
                kind: AttachmentKind::File,
                source_url: None,
            }],
        )
        .unwrap();
        assert!(s.attachments("11").is_ok());
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
            reminder_minutes: 0,
            is_reminder_on: false,
        }
    }

    #[test]
    fn events_round_trip_reminder_fields() {
        let s = Store::open_in_memory().unwrap();
        let mut e = ev("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z");
        e.reminder_minutes = 15;
        e.is_reminder_on = true;
        s.upsert_event(&e).unwrap();
        let rows = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap();
        let row = rows.iter().find(|r| r.id == "E1").unwrap();
        assert_eq!(row.reminder_minutes, 15);
        assert!(row.is_reminder_on);
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

    /// Final-review C2 fix: `replace_events_in_window`'s prune must not
    /// sweep up a not-yet-synced `local:` event. Before the `id NOT LIKE
    /// 'local:%'` exclusion, a calendar refresh racing a just-created event
    /// (which can never appear in the server-truth `events` list — it has no
    /// Graph id yet) would silently delete it out from under its pending
    /// `CreateEvent` op. A genuinely stale event with a real Graph id must
    /// still be pruned exactly as before.
    #[test]
    fn replace_events_in_window_preserves_unsynced_local_events_but_still_prunes_stale_ones() {
        let s = Store::open_in_memory().unwrap();
        let local_id = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        s.upsert_event(&ev("STALE", "2026-07-20T09:00:00Z", "2026-07-20T09:30:00Z"))
            .unwrap();

        // Server truth for the window doesn't mention either id — the local
        // event because it was never sent, "STALE" because it was cancelled.
        s.replace_events_in_window(
            "2026-07-20T00:00:00Z",
            "2026-07-21T00:00:00Z",
            &[ev("SERVER", "2026-07-20T14:00:00Z", "2026-07-20T15:00:00Z")],
            &[],
        )
        .unwrap();

        let rows = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&local_id.as_str()),
            "unsynced local: event was wiped by the refresh prune: {ids:?}"
        );
        assert!(ids.contains(&"SERVER"));
        assert!(
            !ids.contains(&"STALE"),
            "a real Graph-id event no longer present server-side must still be pruned: {ids:?}"
        );
        // The local event is still fully readable for its pending
        // `CreateEvent` op to send.
        assert!(s.event_for_send(&local_id).unwrap().is_some());
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
            recurrence: None,
        }
    }

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
        let id2 = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        assert_eq!(s.event_for_send(&id2).unwrap().unwrap().recurrence, None);
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
                proposed_start_utc: None,
                proposed_end_utc: None,
            }
            .to_json()
            .to_string(),
            r#"{"kind":"respondEvent","id":"E1","rsvp":"accepted","comment":"looking forward to it","proposedStart":null,"proposedEnd":null}"#
        );
        assert_eq!(
            OutboxOp::RespondEvent {
                id: "E1".into(),
                kind: "declined".into(),
                comment: None,
                proposed_start_utc: None,
                proposed_end_utc: None,
            }
            .to_json()
            .to_string(),
            r#"{"kind":"respondEvent","id":"E1","rsvp":"declined","comment":null,"proposedStart":null,"proposedEnd":null}"#
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
                proposed_start_utc: None,
                proposed_end_utc: None,
            },
            OutboxOp::RespondEvent {
                id: "E2".into(),
                kind: "tentativelyAccepted".into(),
                comment: None,
                proposed_start_utc: None,
                proposed_end_utc: None,
            },
            // A declined RSVP that carries a proposed new time — the calendar
            // propose-new-time path serializes this into the outbox, so the
            // proposed window must survive to_json → from_json (a key-name
            // mismatch would silently drop the proposal on drain).
            OutboxOp::RespondEvent {
                id: "E3".into(),
                kind: "declined".into(),
                comment: Some("can we push?".into()),
                proposed_start_utc: Some("2026-07-21T14:00:00Z".into()),
                proposed_end_utc: Some("2026-07-21T15:00:00Z".into()),
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
            is_meeting_request: false,
            categories: Vec::new(),
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

        // The local id still reaches the draft (#1884), which is now stored
        // under the Graph id.
        let (row, _) = s
            .draft(&local_id)
            .unwrap()
            .expect("local id still resolves");
        assert_eq!(row.id, "GRAPH-ID-1");
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
        // The row itself moved to the graph id ...
        let keys: Vec<String> = s
            .conn
            .prepare("SELECT draft_id FROM outbound_attachments")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(keys, vec!["GRAPH-1".to_string()]);
        assert_eq!(s.outbound_attachments("GRAPH-1").unwrap().len(), 1);
        // ... and the local id still lists it (#1884).
        assert_eq!(s.outbound_attachments(&id).unwrap().len(), 1);
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

/// A draft stays addressable by its `local:` id after `reconcile_id` (#1884).
#[cfg(test)]
mod local_id_tests {
    use super::*;
    use crate::graph::model::{Message, Recipient};

    /// A store holding one reconciled draft: `(store, local id)`, with the
    /// draft filed under the sentinel Drafts folder and stored as `G`.
    fn reconciled() -> (Store, String) {
        let s = Store::open_in_memory().unwrap();
        let local = s
            .create_local_draft("Subj", "a@x", "", "<p>old</p>")
            .unwrap();
        s.reconcile_id(&local, "G").unwrap();
        (s, local)
    }

    #[test]
    fn current_id_follows_a_reconciled_local_id_only() {
        let (s, local) = reconciled();
        assert_eq!(s.current_id(&local).unwrap(), "G");
        assert_eq!(s.current_id("G").unwrap(), "G");
        assert_eq!(s.current_id("local:never").unwrap(), "local:never");
        assert_eq!(s.current_id("OTHER").unwrap(), "OTHER");
    }

    #[test]
    fn a_graph_id_is_never_looked_up_as_an_alias() {
        // Even a row whose `local_id` held a non-`local:` value (no code
        // writes one) is not reached by it.
        let (s, _) = reconciled();
        s.conn
            .execute("UPDATE messages SET local_id = 'X' WHERE id = 'G'", [])
            .unwrap();
        assert_eq!(s.current_id("X").unwrap(), "X");
        assert!(s.draft("X").unwrap().is_none());
    }

    #[test]
    fn every_draft_call_reaches_the_reconciled_draft_by_its_local_id() {
        let (s, local) = reconciled();

        let (row, body) = s.draft(&local).unwrap().expect("local id resolves");
        assert_eq!(row.id, "G");
        assert_eq!(row.subject, "Subj");
        assert_eq!(body.content, "<p>old</p>");

        s.update_draft_fields(&local, "New", "b@x", "c@x", "d@x", "<p>new</p>")
            .unwrap();
        let (row, body) = s.draft("G").unwrap().unwrap();
        assert_eq!(
            (
                row.subject.as_str(),
                row.to_recipients.as_str(),
                row.cc_recipients.as_str(),
                row.bcc_recipients.as_str()
            ),
            ("New", "b@x", "c@x", "d@x")
        );
        assert_eq!(body.content, "<p>new</p>");
        // No stray body was written under the local id.
        let bodies: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM bodies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bodies, 1);

        s.add_outbound_attachment(&local, "/tmp/a.pdf", "a.pdf", 1)
            .unwrap();
        s.add_outbound_attachment(&local, "/tmp/b.pdf", "b.pdf", 2)
            .unwrap();
        assert_eq!(s.outbound_attachments("G").unwrap().len(), 2);
        assert_eq!(s.outbound_attachments(&local).unwrap().len(), 2);
        s.remove_outbound_attachment(&local, "/tmp/a.pdf").unwrap();
        let left = s.outbound_attachments("G").unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "b.pdf");
        s.clear_outbound_attachments(&local).unwrap();
        assert!(s.outbound_attachments("G").unwrap().is_empty());

        s.mark_sent(&local);
        assert!(!s.draft("G").unwrap().unwrap().0.is_draft);

        s.conn
            .execute(
                "INSERT INTO folders (id, display_name) VALUES ('SENT', 'Sent Items')",
                [],
            )
            .unwrap();
        s.move_message(&local, "SENT").unwrap();
        assert_eq!(s.draft("G").unwrap().unwrap().0.folder_id, "SENT");
    }

    #[test]
    fn deleting_the_graph_id_drops_the_alias() {
        let (s, local) = reconciled();
        s.delete_message("G").unwrap();
        assert_eq!(s.current_id(&local).unwrap(), local);
        assert!(s.draft(&local).unwrap().is_none());
    }

    #[test]
    fn deleting_by_the_local_id_deletes_the_graph_row() {
        let (s, local) = reconciled();
        s.delete_message(&local).unwrap();
        assert!(s.draft("G").unwrap().is_none());
        assert_eq!(s.current_id(&local).unwrap(), local);
    }

    #[test]
    fn delete_message_resolved_returns_the_id_it_deleted() {
        let (s, local) = reconciled();
        assert_eq!(
            s.delete_message_resolved(&local).unwrap().as_deref(),
            Some("G")
        );
        assert!(s.draft("G").unwrap().is_none());
        assert_eq!(s.current_id(&local).unwrap(), local);
        // Nothing left to match, by either id.
        assert_eq!(s.delete_message_resolved(&local).unwrap(), None);
        assert_eq!(s.delete_message_resolved("G").unwrap(), None);
        assert_eq!(s.delete_message_resolved("unknown").unwrap(), None);
        assert_eq!(s.delete_message_resolved("").unwrap(), None);

        let (s, _) = reconciled();
        assert_eq!(
            s.delete_message_resolved("G").unwrap().as_deref(),
            Some("G")
        );
        assert!(s.draft("G").unwrap().is_none());
    }

    #[test]
    fn a_folder_cascade_drops_the_alias() {
        let (s, local) = reconciled();
        let folder = s.draft("G").unwrap().unwrap().0.folder_id;
        s.conn
            .execute("DELETE FROM folders WHERE id = ?1", params![folder])
            .unwrap();
        assert!(s.draft("G").unwrap().is_none());
        assert_eq!(s.current_id(&local).unwrap(), local);
    }

    #[test]
    fn a_delta_upsert_of_the_graph_id_keeps_the_alias() {
        let (s, local) = reconciled();
        let folder = s.draft("G").unwrap().unwrap().0.folder_id;
        let m = Message {
            id: "G".into(),
            conversation_id: "C".into(),
            subject: "From Graph".into(),
            from: Recipient {
                name: String::new(),
                address: String::new(),
            },
            to: vec![],
            cc: vec![],
            received: String::new(),
            sent: String::new(),
            is_read: true,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: String::new(),
            is_draft: true,
            is_meeting_request: false,
            categories: Vec::new(),
        };
        s.upsert_message(&folder, &m).unwrap();
        assert_eq!(s.current_id(&local).unwrap(), "G");
        assert_eq!(s.draft(&local).unwrap().unwrap().0.subject, "From Graph");
    }

    // --- Body format (#1901) ------------------------------------------------

    fn text_body(content: &str) -> Body {
        Body {
            content_type: "text".into(),
            content: content.into(),
        }
    }

    fn html_body(content: &str) -> Body {
        Body {
            content_type: "html".into(),
            content: content.into(),
        }
    }

    /// A store holding one message `M` (in Drafts, so `update_draft_fields`
    /// reaches it), for bodies to hang off.
    fn store_with_message() -> Store {
        let s = Store::open_in_memory().unwrap();
        let folder = s.drafts_folder_id().unwrap();
        let m = Message {
            id: "M".into(),
            conversation_id: String::new(),
            subject: "S".into(),
            from: Recipient::default(),
            to: Vec::new(),
            cc: Vec::new(),
            received: String::new(),
            sent: String::new(),
            is_read: true,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: String::new(),
            is_draft: false,
            is_meeting_request: false,
            categories: Vec::new(),
        };
        s.upsert_message(&folder, &m).unwrap();
        s
    }

    fn satisfies(s: &Store, id: &str) -> (bool, bool) {
        (
            s.get_body_for(id, BodyFormat::Text).unwrap().is_some(),
            s.get_body_for(id, BodyFormat::Html).unwrap().is_some(),
        )
    }

    #[test]
    fn get_body_for_has_nothing_without_a_body() {
        let s = store_with_message();
        assert_eq!(satisfies(&s, "M"), (false, false));
        assert_eq!(satisfies(&s, "nope"), (false, false));
    }

    #[test]
    fn a_fetched_body_satisfies_the_format_it_was_asked_in() {
        let s = store_with_message();
        s.put_fetched_body("M", &text_body("t"), BodyFormat::Text)
            .unwrap();
        assert_eq!(satisfies(&s, "M"), (true, false));
        s.put_fetched_body("M", &html_body("<p>h</p>"), BodyFormat::Html)
            .unwrap();
        assert_eq!(satisfies(&s, "M"), (false, true));
        let b = s.get_body_for("M", BodyFormat::Html).unwrap().unwrap();
        assert_eq!(b, html_body("<p>h</p>"));
    }

    #[test]
    fn a_native_text_body_asked_as_html_satisfies_html() {
        // A plain-text message's native body is text: asked for html, it is
        // the right answer and must not read as "fetch again".
        let s = store_with_message();
        s.put_fetched_body("M", &text_body("plain"), BodyFormat::Html)
            .unwrap();
        assert_eq!(satisfies(&s, "M"), (false, true));
    }

    #[test]
    fn a_body_with_no_format_counts_as_text_when_text_and_as_either_when_html() {
        // No `fetched_as`: an older engine's fetch (always text), or a body
        // written locally. `put_body` writes none, and overwrites a format.
        let s = store_with_message();
        s.put_body("M", &text_body("old")).unwrap();
        assert_eq!(satisfies(&s, "M"), (true, false));
        s.put_body("M", &html_body("<p>local</p>")).unwrap();
        assert_eq!(satisfies(&s, "M"), (true, true));
        s.put_fetched_body("M", &text_body("t"), BodyFormat::Text)
            .unwrap();
        s.put_body("M", &html_body("<p>local</p>")).unwrap();
        assert_eq!(satisfies(&s, "M"), (true, true));
        // Content types compare as lookxy's readers do, ignoring case.
        s.put_body(
            "M",
            &Body {
                content_type: "HTML".into(),
                content: "<p>x</p>".into(),
            },
        )
        .unwrap();
        assert_eq!(satisfies(&s, "M"), (true, true));
    }

    #[test]
    fn an_unknown_fetch_format_is_fetched_again() {
        let s = store_with_message();
        s.put_fetched_body("M", &html_body("<p>h</p>"), BodyFormat::Html)
            .unwrap();
        s.conn
            .execute("UPDATE bodies SET fetched_as = 'rtf'", [])
            .unwrap();
        assert_eq!(satisfies(&s, "M"), (false, false));
    }

    #[test]
    fn local_draft_bodies_satisfy_both_formats() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("S", "", "", "<p>hi</p>").unwrap();
        assert_eq!(satisfies(&s, &id), (true, true));
        s.update_draft_fields(&id, "S", "", "", "", "<p>edited</p>")
            .unwrap();
        assert_eq!(satisfies(&s, &id), (true, true));
    }

    #[test]
    fn editing_a_draft_fetched_as_text_keeps_the_edit_for_html() {
        // A synced draft whose body `FetchBody` stored as text: compose
        // writes html, which must not read as "fetch the html over it".
        let s = store_with_message();
        s.put_fetched_body("M", &text_body("server text"), BodyFormat::Text)
            .unwrap();
        s.update_draft_fields("M", "S", "", "", "", "<p>my edit</p>")
            .unwrap();
        let b = s.get_body_for("M", BodyFormat::Html).unwrap().unwrap();
        assert_eq!(b, html_body("<p>my edit</p>"));
        assert_eq!(satisfies(&s, "M"), (true, true));
    }

    #[test]
    fn reconcile_id_keeps_the_fetch_format() {
        let s = store_with_message();
        s.put_fetched_body("M", &html_body("<p>h</p>"), BodyFormat::Html)
            .unwrap();
        s.reconcile_id("M", "G").unwrap();
        assert_eq!(satisfies(&s, "G"), (false, true));
    }

    #[test]
    fn a_database_from_before_fetched_as_is_migrated() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-fetched-as-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        let id = {
            let s = Store::open(&path).unwrap();
            let id = s.create_local_draft("Old", "", "", "body").unwrap();
            s.conn
                .execute(
                    "UPDATE bodies SET content_type = 'text' WHERE message_id = ?1",
                    params![id],
                )
                .unwrap();
            // Take the database back to before the column existed.
            s.conn
                .execute_batch("ALTER TABLE bodies DROP COLUMN fetched_as;")
                .unwrap();
            id
        };
        let s = Store::open(&path).unwrap();
        // The old row reads as an older engine's text fetch.
        assert_eq!(satisfies(&s, &id), (true, false));
        s.put_fetched_body(&id, &html_body("<p>h</p>"), BodyFormat::Html)
            .unwrap();
        assert_eq!(satisfies(&s, &id), (false, true));
        drop(s);
        // Opening again (column already there) is fine.
        let s = Store::open(&path).unwrap();
        assert_eq!(satisfies(&s, &id), (false, true));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_database_from_before_local_id_is_migrated() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-local-id-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        let local = {
            let s = Store::open(&path).unwrap();
            let local = s.create_local_draft("Old", "", "", "body").unwrap();
            // Take the database back to before the column existed.
            s.conn
                .execute_batch(
                    "DROP INDEX idx_messages_local_id;
                     ALTER TABLE messages DROP COLUMN local_id;",
                )
                .unwrap();
            local
        };
        let s = Store::open(&path).unwrap();
        let has_index: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_messages_local_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_index, 1);
        s.reconcile_id(&local, "G").unwrap();
        assert_eq!(s.current_id(&local).unwrap(), "G");
        assert_eq!(s.draft(&local).unwrap().unwrap().0.id, "G");
        drop(s);
        // Opening again (column and index already there) is fine.
        let s = Store::open(&path).unwrap();
        assert_eq!(s.current_id(&local).unwrap(), "G");
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
