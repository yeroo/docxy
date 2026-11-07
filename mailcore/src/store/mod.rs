//! The local SQLite mail store — the single source of truth for the TUI.
//!
//! The sync engine writes Graph data in here via
//! `upsert_folder`/`upsert_message`/delta-link bookkeeping; the TUI reads
//! only from here (offline-first). This module owns the schema (see
//! `schema.rs`) and the folders/messages/meta surface, plus bodies,
//! attachments, full-text search, and the outbox queue (triage mutations
//! queued here for `sync::outbox::apply_op` to push to Graph later).

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::graph::client::{AttendeeInput, EventFieldSet};
use crate::graph::model::{
    AttachmentKind, AttachmentMeta, Attendee, Body, BodyFormat, Event, EventRecurrence, MailFolder,
    MasterCategory, Message, MessageFields, MessageHeader, Recipient, Recurrence, is_stored_header,
    meeting_link_in,
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

/// What a folder's history sync has reached (#1871): the persisted state of
/// its whole-mailbox backfill, read by [`Store::folder_history`] and
/// [`Store::folder_histories`], so a UI can show progress before any event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderHistory {
    pub folder_id: String,
    /// What `delta_link` covers: `None` (never synced, or a link stored
    /// before #1871), [`SCOPE_HEAD`], [`SCOPE_WINDOW`] or [`SCOPE_ALL`].
    pub scope: Option<String>,
    /// Items the folder's full delta has walked past so far: its upserts
    /// within the window, merged or kept as a local edit (not `@removed`). It may
    /// exceed `total_count`: a UI clamps.
    pub synced: i64,
    /// The full delta's next page, while a backfill is under way.
    pub backfill_link: Option<String>,
    /// What the walk under way covers: [`SCOPE_WINDOW`] once a page ran with
    /// the window's cutoff, else [`SCOPE_ALL`]; `None` before its first
    /// page. A whole-mailbox engine restarts a `window` walk.
    pub backfill_scope: Option<String>,
    /// Whether the folder has a delta link (its phase 1 is done).
    pub has_delta_link: bool,
    /// The folder's `total_count` from its last listing.
    pub total_count: i64,
}

/// `history_scope` of a folder whose `delta_link` is a filtered head delta:
/// its recent mail only, the backfill still to come (#1871).
pub const SCOPE_HEAD: &str = "head";
/// `history_scope` of a folder whose full delta ran with the
/// `backfill_days` cutoff: complete in window mode.
pub const SCOPE_WINDOW: &str = "window";
/// `history_scope` of a folder whose full delta ran with no cutoff: complete
/// in every mode.
pub const SCOPE_ALL: &str = "all";

/// `Store::folders`' order: well-known folders first (Inbox, Drafts, Sent,
/// Deleted, Junk, Archive), then by name.
const FOLDER_ORDER: &str = "ORDER BY CASE
                 WHEN well_known_name = 'inbox' OR LOWER(display_name) = 'inbox' THEN 0
                 WHEN well_known_name = 'drafts' THEN 1
                 WHEN well_known_name = 'sentitems' THEN 2
                 WHEN well_known_name = 'deleteditems' THEN 3
                 WHEN well_known_name = 'junkemail' THEN 4
                 WHEN well_known_name = 'archive' THEN 5
                 ELSE 99
             END, display_name";

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
/// name + size), not bytes; the bytes are read from disk at send time. Or,
/// for a pasted picture (#1949), bytes kept in the row (`from_bytes`), whose
/// `path` is then only its key, `cid:<content_id>`; load them with
/// `Store::outbound_attachment_bytes`.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboundAttachment {
    /// The file's path, or a bytes row's key (`cid:<content_id>`): what
    /// `remove_outbound_attachment` and `mark_outbound_attachment_uploaded`
    /// take.
    pub path: String,
    pub name: String,
    pub size: i64,
    /// Already on the Graph draft (#1874): a send uploaded it, Graph
    /// confirmed it, and the send then failed, so a resend skips it: a
    /// confirmed upload is never repeated. An upload whose answer was lost,
    /// or whose marker could not be written, is not marked and is uploaded
    /// again. Removing it locally would not remove it from the Graph draft.
    pub uploaded: bool,
    /// Set for an inline attachment (#1949): uploaded with `isInline` and
    /// this `contentId`, so an html body's `<img src="cid:…">` finds it.
    pub content_id: Option<String>,
    /// The MIME type given when it was attached; `None` derives it from the
    /// name at send time (every plain file row).
    pub content_type: Option<String>,
    /// The bytes are in the row, not in a file (#1949).
    pub from_bytes: bool,
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
    /// `fields` are the groups the edit changed (`Store::update_event_fields`,
    /// #1994): the PATCH sends only those. A row queued before #1994 has no
    /// `fields` and reads as `EventFieldSet::LEGACY_UPDATE`.
    UpdateEvent {
        id: String,
        fields: EventFieldSet,
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
            OutboxOp::UpdateEvent { id, fields } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                (
                    "fields".to_string(),
                    Value::Array(
                        fields
                            .to_names()
                            .into_iter()
                            .map(|n| Value::Str(n.to_string()))
                            .collect(),
                    ),
                ),
            ]),
            OutboxOp::CreateEvent { id } | OutboxOp::DeleteEvent { id } => Value::Object(vec![
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
            "updateEvent" => Some(OutboxOp::UpdateEvent {
                id: id()?,
                fields: match v.get("fields").and_then(Value::as_array) {
                    Some(names) => EventFieldSet::from_names(
                        &names.iter().filter_map(Value::as_str).collect::<Vec<_>>(),
                    ),
                    None => EventFieldSet::LEGACY_UPDATE,
                },
            }),
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
    /// Graph `showAs` (#1983); see `graph::model::Event` for each field below.
    pub show_as: String,
    pub is_online_meeting: bool,
    pub online_meeting_provider: String,
    pub join_url: String,
    pub categories: Vec<String>,
    pub sensitivity: String,
    pub importance: String,
    pub event_type: String,
    /// A series' Graph recurrence, kept in `events.graph_recurrence` (not
    /// the create-side `events.recurrence` `event_for_send` re-sends).
    pub recurrence: Option<EventRecurrence>,
    pub is_cancelled: bool,
    pub is_organizer: bool,
    pub response_requested: bool,
    pub allow_new_time_proposals: bool,
    pub has_attachments: bool,
    pub original_start_time_zone: String,
    pub original_end_time_zone: String,
    pub calendar_id: String,
}

/// Every field empty, zero or `false`, except `response_requested` and
/// `allow_new_time_proposals`: `true`, as in Graph and `graph::model::Event`.
impl Default for EventRow {
    fn default() -> Self {
        EventRow {
            id: String::new(),
            subject: String::new(),
            start_utc: String::new(),
            end_utc: String::new(),
            is_all_day: false,
            location: String::new(),
            organizer_name: String::new(),
            organizer_addr: String::new(),
            response_status: String::new(),
            series_master_id: None,
            reminder_minutes: 0,
            is_reminder_on: false,
            show_as: String::new(),
            is_online_meeting: false,
            online_meeting_provider: String::new(),
            join_url: String::new(),
            categories: Vec::new(),
            sensitivity: String::new(),
            importance: String::new(),
            event_type: String::new(),
            recurrence: None,
            is_cancelled: false,
            is_organizer: false,
            response_requested: true,
            allow_new_time_proposals: true,
            has_attachments: false,
            original_start_time_zone: String::new(),
            original_end_time_zone: String::new(),
            calendar_id: String::new(),
        }
    }
}

/// An `event_attendees` row, as read back from the store.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AttendeeRow {
    pub name: String,
    pub addr: String,
    pub r#type: String,
    pub response: String,
    /// Graph `status.time`; `""` when never (#1983).
    pub response_time: String,
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
    /// Graph `showAs` (#1983); see `graph::model::Event` for each field below.
    pub show_as: String,
    pub is_online_meeting: bool,
    pub online_meeting_provider: String,
    pub join_url: String,
    pub categories: Vec<String>,
    pub sensitivity: String,
    pub importance: String,
    pub event_type: String,
    /// A series' Graph recurrence, kept in `events.graph_recurrence` (not
    /// the create-side `events.recurrence` `event_for_send` re-sends).
    pub recurrence: Option<EventRecurrence>,
    pub is_cancelled: bool,
    pub is_organizer: bool,
    pub response_requested: bool,
    pub allow_new_time_proposals: bool,
    pub has_attachments: bool,
    pub original_start_time_zone: String,
    pub original_end_time_zone: String,
    pub calendar_id: String,
}

/// Every field empty, zero or `false`, except `response_requested` and
/// `allow_new_time_proposals`: `true`, as in Graph and `graph::model::Event`.
impl Default for NewEvent {
    fn default() -> Self {
        NewEvent {
            id: String::new(),
            subject: String::new(),
            start_utc: String::new(),
            end_utc: String::new(),
            is_all_day: false,
            location: String::new(),
            organizer_name: String::new(),
            organizer_addr: String::new(),
            response_status: String::new(),
            series_master_id: None,
            body_preview: String::new(),
            web_link: String::new(),
            last_modified: String::new(),
            body_html: String::new(),
            reminder_minutes: 0,
            is_reminder_on: false,
            show_as: String::new(),
            is_online_meeting: false,
            online_meeting_provider: String::new(),
            join_url: String::new(),
            categories: Vec::new(),
            sensitivity: String::new(),
            importance: String::new(),
            event_type: String::new(),
            recurrence: None,
            is_cancelled: false,
            is_organizer: false,
            response_requested: true,
            allow_new_time_proposals: true,
            has_attachments: false,
            original_start_time_zone: String::new(),
            original_end_time_zone: String::new(),
            calendar_id: String::new(),
        }
    }
}

/// The field set `put_event_attendees` writes for one `event_attendees` row —
/// the same field set as `graph::model::Attendee`; see `From<&Attendee> for
/// NewAttendee` below.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewAttendee {
    pub name: String,
    pub addr: String,
    pub r#type: String,
    pub response: String,
    pub response_time: String,
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
            show_as: e.show_as.clone(),
            is_online_meeting: e.is_online_meeting,
            online_meeting_provider: e.online_meeting_provider.clone(),
            join_url: e.join_url.clone(),
            categories: e.categories.clone(),
            sensitivity: e.sensitivity.clone(),
            importance: e.importance.clone(),
            event_type: e.event_type.clone(),
            recurrence: e.recurrence.clone(),
            is_cancelled: e.is_cancelled,
            is_organizer: e.is_organizer,
            response_requested: e.response_requested,
            allow_new_time_proposals: e.allow_new_time_proposals,
            has_attachments: e.has_attachments,
            original_start_time_zone: e.original_start_time_zone.clone(),
            original_end_time_zone: e.original_end_time_zone.clone(),
            calendar_id: e.calendar_id.clone(),
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
            response_time: a.response_time.clone(),
        }
    }
}

/// The editable fields of an event a host collects — the input to
/// `create_local_event`/`update_event_fields`. Each `Option` is `None` for
/// "leave as stored" on an update and "Graph's default" on a create (#1994),
/// so a host whose form shows only some fields passes `..Default::default()`
/// without clobbering what another client set. `Some("")` for `show_as`,
/// `sensitivity`, `importance` or `online_meeting_provider` is `None`: Graph
/// refuses an empty enum.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LocalEventFields {
    pub subject: String,
    pub start_utc: String,
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub body_html: String,
    /// An attendee whose `r#type` is `""` keeps the type of the stored
    /// attendee with the same address on an update, and is `required`
    /// otherwise.
    pub attendees: Vec<AttendeeInput>,
    /// The recurrence pattern for a repeating event (create-only), or `None`.
    pub recurrence: Option<Recurrence>,
    pub show_as: Option<String>,
    pub sensitivity: Option<String>,
    pub importance: Option<String>,
    pub categories: Option<Vec<String>>,
    /// `None` on a create: Graph applies the mailbox's default reminder.
    pub reminder: Option<EventReminder>,
    pub is_online_meeting: Option<bool>,
    pub online_meeting_provider: Option<String>,
    pub response_requested: Option<bool>,
    pub allow_new_time_proposals: Option<bool>,
}

/// An event's reminder: Graph `isReminderOn` and `reminderMinutesBeforeStart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventReminder {
    pub on: bool,
    pub minutes_before_start: i64,
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
    pub attendees: Vec<AttendeeInput>,
    /// The recurrence pattern for a repeating event (create-only), or `None`.
    pub recurrence: Option<Recurrence>,
    pub show_as: String,
    pub sensitivity: String,
    pub importance: String,
    pub categories: Vec<String>,
    pub is_reminder_on: bool,
    pub reminder_minutes: i64,
    pub is_online_meeting: bool,
    pub online_meeting_provider: String,
    pub response_requested: bool,
    pub allow_new_time_proposals: bool,
    /// A `local:` event created without a reminder (`events.reminder_unset`):
    /// its `CreateEvent` sends none, so Graph applies the mailbox default.
    pub reminder_unset: bool,
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
        // Same idempotent-migration pattern, for what a calendar client
        // shows (#1983). `SCHEMA_SQL` runs before these, so it must not
        // index any of them: an old database would fail that `CREATE INDEX`
        // before the column exists.
        for column in [
            "show_as TEXT NOT NULL DEFAULT ''",
            "is_online_meeting INTEGER NOT NULL DEFAULT 0",
            "online_meeting_provider TEXT NOT NULL DEFAULT ''",
            "join_url TEXT NOT NULL DEFAULT ''",
            "categories TEXT NOT NULL DEFAULT ''",
            "sensitivity TEXT NOT NULL DEFAULT ''",
            "importance TEXT NOT NULL DEFAULT ''",
            "event_type TEXT NOT NULL DEFAULT ''",
            "graph_recurrence TEXT NOT NULL DEFAULT ''",
            "is_cancelled INTEGER NOT NULL DEFAULT 0",
            "is_organizer INTEGER NOT NULL DEFAULT 0",
            "response_requested INTEGER NOT NULL DEFAULT 1",
            "allow_new_time_proposals INTEGER NOT NULL DEFAULT 1",
            "has_attachments INTEGER NOT NULL DEFAULT 0",
            "original_start_tz TEXT NOT NULL DEFAULT ''",
            "original_end_tz TEXT NOT NULL DEFAULT ''",
            "calendar_id TEXT NOT NULL DEFAULT ''",
            "reminder_unset INTEGER NOT NULL DEFAULT 0",
        ] {
            let _ = conn.execute(&format!("ALTER TABLE events ADD COLUMN {column}"), []);
        }
        let _ = conn.execute(
            "ALTER TABLE event_attendees ADD COLUMN response_time TEXT NOT NULL DEFAULT ''",
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
        // Same idempotent-migration pattern, for the whole-mailbox backfill
        // columns (#1871). A folder synced before them reads as a legacy
        // link (`history_scope` NULL): complete in window mode, backfilled
        // in whole-mailbox mode.
        let _ = conn.execute("ALTER TABLE folders ADD COLUMN backfill_link TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE folders ADD COLUMN backfill_synced INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("ALTER TABLE folders ADD COLUMN history_scope TEXT", []);
        let _ = conn.execute("ALTER TABLE folders ADD COLUMN backfill_scope TEXT", []);
        // Same idempotent-migration pattern, for `messages.local_id` (#1884:
        // the `local:` id a reconciled draft had). Its unique index has to
        // come after the ALTER, not in `SCHEMA_SQL`: on an old database
        // `SCHEMA_SQL` runs before the column exists. NULLs never collide.
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN local_id TEXT", []);
        // Same idempotent-migration pattern, for `messages.local_edit` (#1871:
        // a draft's unsaved-edit generation). A row from before reads as
        // clean.
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN local_edit INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // Same idempotent-migration pattern, for `bodies.fetched_as` (#1901:
        // the format `FetchBody` asked for). Rows from before it stay NULL,
        // which `get_body_for` reads as "asked for text" for a text body,
        // as the engine then always did.
        let _ = conn.execute("ALTER TABLE bodies ADD COLUMN fetched_as TEXT", []);
        // Same idempotent-migration pattern, for `outbound_attachments.uploaded`
        // (#1874: the file is already on the Graph draft).
        let _ = conn.execute(
            "ALTER TABLE outbound_attachments ADD COLUMN uploaded INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // Same idempotent-migration pattern, for the inline outbound
        // attachment columns (#1949). Older rows read as plain file rows.
        let _ = conn.execute(
            "ALTER TABLE outbound_attachments ADD COLUMN content_id TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE outbound_attachments ADD COLUMN content_type TEXT",
            [],
        );
        let _ = conn.execute("ALTER TABLE outbound_attachments ADD COLUMN data BLOB", []);
        // Same idempotent-migration pattern, for `outbox.send_posted` (#1874:
        // a `SendDraft` op's `/send` POST went out). On the open that adds
        // the column, every `SendDraft` an older engine left queued reads
        // as posted: it may be there after a 5xx or a lost answer on
        // `/send`, so its next attempt checks delivery first (one GET; a
        // `local:` draft, never on Graph, skips it). A reopen, with the
        // column already there, leaves the flags alone.
        if conn
            .execute(
                "ALTER TABLE outbox ADD COLUMN send_posted INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .is_ok()
        {
            conn.execute(
                "UPDATE outbox SET send_posted = 1 WHERE op = 'sendDraft'",
                [],
            )?;
        }
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_local_id ON messages(local_id)",
            [],
        )?;
        Ok(Store { conn })
    }

    /// Inserts or updates a folder row by `id`: its parent, name, counts and
    /// well-known name. Its local state (`sort_order`, `is_expanded`) and
    /// `delta_link` are left as they are, so a folder renamed or moved on
    /// the server keeps them (#1886). True when the row was inserted or one
    /// of those columns changed; false when it already had these values.
    pub fn upsert_folder(&self, f: &MailFolder) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "INSERT INTO folders (id, parent_id, display_name, total_count, unread_count, well_known_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 parent_id = excluded.parent_id,
                 display_name = excluded.display_name,
                 total_count = excluded.total_count,
                 unread_count = excluded.unread_count,
                 well_known_name = excluded.well_known_name
             WHERE parent_id IS NOT excluded.parent_id
                OR display_name IS NOT excluded.display_name
                OR total_count IS NOT excluded.total_count
                OR unread_count IS NOT excluded.unread_count
                OR well_known_name IS NOT excluded.well_known_name",
            params![
                f.id,
                f.parent_id,
                f.display_name,
                f.total_count,
                f.unread_count,
                f.well_known_name,
            ],
        )?;
        Ok(changed > 0)
    }

    /// Removes every folder whose id is not in `keep`: the folders a
    /// *complete* enumeration no longer lists, deleted on the server
    /// (#1886). Their messages go with them (`ON DELETE CASCADE`, and so
    /// their bodies, attachments and search rows), and so do their delta
    /// links, which live on the folder row. The local drafts sentinel
    /// (`drafts_folder_id`) is never listed by Graph: it is removed only
    /// once it holds no message (its drafts adopted into the real Drafts
    /// folder), in the same statement that checks it, so a draft
    /// `create_local_draft` files there is never lost. One IMMEDIATE
    /// transaction. Returns how many folder rows were removed.
    pub fn retain_folders(&self, keep: &[String]) -> Result<usize, StoreError> {
        let keep_json =
            Value::Array(keep.iter().map(|id| Value::Str(id.clone())).collect()).to_string();
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut removed = tx.execute(
            "DELETE FROM folders
             WHERE id <> ?2 AND id NOT IN (SELECT value FROM json_each(?1))",
            params![keep_json, LOCAL_DRAFTS_SENTINEL_FOLDER_ID],
        )?;
        removed += tx.execute(
            "DELETE FROM folders
             WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM messages WHERE folder_id = ?1)",
            params![LOCAL_DRAFTS_SENTINEL_FOLDER_ID],
        )?;
        tx.commit()?;
        Ok(removed)
    }

    /// Sets one folder's `total_count`/`unread_count`, as one folder's
    /// `GET /me/mailFolders/{id}` read them (#1885), and leaves its name,
    /// parent and well-known name as they are (`upsert_folder` would
    /// overwrite them). True when a stored count changed; false when they
    /// were already these, or no folder has `id`.
    pub fn set_folder_counts(&self, id: &str, total: i64, unread: i64) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "UPDATE folders SET total_count = ?2, unread_count = ?3
             WHERE id = ?1 AND (total_count IS NOT ?2 OR unread_count IS NOT ?3)",
            params![id, total, unread],
        )?;
        Ok(changed > 0)
    }

    /// All folders, well-known ones first (Inbox, Drafts, Sent, Deleted,
    /// Junk, Archive, in that order), then everything else alphabetically.
    pub fn folders(&self) -> Result<Vec<FolderRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, parent_id, display_name, total_count, unread_count, delta_link, well_known_name, sort_order, is_expanded
             FROM folders
             {FOLDER_ORDER}"
        ))?;
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
        self.merge_message_guarded(folder_id, m, fields, false)
            .map(|_| ())
    }

    /// [`Store::merge_message`] for a delta that starts fresh (#1871: a
    /// head, a small folder's full delta, a backfill page), which carries
    /// every message, not only what changed: an existing row holding an
    /// unsaved edit (`local_edit > 0`, a draft compose saved since its last
    /// push) is left as it is. The check is in the same statement as the
    /// write (`DO UPDATE … WHERE`), so a compose save on another connection
    /// can never land between them. A missing row is inserted. Returns
    /// whether a row was written.
    pub fn merge_message_unless_edited(
        &self,
        folder_id: &str,
        m: &Message,
        fields: &MessageFields,
    ) -> Result<bool, StoreError> {
        self.merge_message_guarded(folder_id, m, fields, true)
    }

    fn merge_message_guarded(
        &self,
        folder_id: &str,
        m: &Message,
        fields: &MessageFields,
        guard: bool,
    ) -> Result<bool, StoreError> {
        let n = self.conn.execute(
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
                 categories = CASE WHEN ?33 THEN excluded.categories ELSE messages.categories END
             WHERE NOT ?34 OR messages.local_edit = 0",
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
                guard,
            ],
        )?;
        Ok(n > 0)
    }

    /// The ids of the rows holding a local edit Graph has not seen
    /// (#1871): drafts compose changed (`update_draft_fields`) since their
    /// last successful push (`local_edit > 0`), sorted. Every delta that
    /// starts fresh leaves them alone
    /// ([`Store::merge_message_unless_edited`]). Tests read it; the engine
    /// checks the marker in the statement that writes.
    #[cfg(test)]
    pub fn locally_edited_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM messages WHERE local_edit > 0 ORDER BY id")?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }

    /// The unsaved-edit generation of the draft `id` addresses (#1871; a
    /// reconciled `local:` id still reaches it, #1884), 0 for a clean or
    /// unknown row. Read before a push reads the draft, so an edit after it
    /// bumps the generation past it.
    pub fn local_edit_generation(&self, id: &str) -> Result<i64, StoreError> {
        let generation = self
            .conn
            .query_row(
                concat!("SELECT local_edit FROM messages WHERE ", draft_row_sql!()),
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(0)
                } else {
                    Err(e)
                }
            })?;
        Ok(generation)
    }

    /// Marks the draft `id` addresses clean after a push that sent
    /// generation `generation` of its fields (#1871), only if no edit came
    /// since (the generation is still that one), in one statement. Returns
    /// whether it did.
    pub fn clear_local_edit(&self, id: &str, generation: i64) -> Result<bool, StoreError> {
        let n = self.conn.execute(
            concat!(
                "UPDATE messages SET local_edit = 0 WHERE local_edit = ?2 AND ",
                draft_row_sql!()
            ),
            params![id, generation],
        )?;
        Ok(n > 0)
    }

    /// The message ids queued outbox ops name (#1871): every delta that
    /// starts fresh (a head, a small folder's full delta, a backfill page)
    /// leaves them alone, as their optimistic local write (a read flag, a
    /// move, a delete, a draft's fields) has not reached Graph yet. Calendar
    /// event ops name events, not messages, and are left out.
    pub fn pending_message_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut ids: Vec<String> = self
            .pending_ops()?
            .into_iter()
            .filter_map(|row| match row.op {
                OutboxOp::MarkRead { id, .. }
                | OutboxOp::SetFlag { id, .. }
                | OutboxOp::SetCategories { id, .. }
                | OutboxOp::Move { id, .. }
                | OutboxOp::Delete { id }
                | OutboxOp::SaveDraft { id }
                | OutboxOp::SendDraft { id } => Some(id),
                OutboxOp::RespondEvent { .. }
                | OutboxOp::CreateEvent { .. }
                | OutboxOp::UpdateEvent { .. }
                | OutboxOp::DeleteEvent { .. } => None,
            })
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
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

    /// Undoes `mark_sent` and `SendDraft`'s optimistic re-file into Sent
    /// Items after a send failed for good (#1874): the row is a draft again
    /// (`is_draft = 1`), filed under `drafts_folder_id`. One statement, so a
    /// `local:` id the failed send's own create reconciled still reaches the
    /// row (#1884). Subject, recipients, body and attachments are untouched.
    /// A missing row changes nothing (not an error).
    pub fn return_to_drafts(&self, id: &str, drafts_folder_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            concat!(
                "UPDATE messages SET is_draft = 1, folder_id = ?2 WHERE ",
                draft_row_sql!()
            ),
            params![id, drafts_folder_id],
        )?;
        Ok(())
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
    /// each folder from scratch (a fresh head delta) rather than
    /// resuming from a token. Used by the sync engine to reconverge the local
    /// store with server truth after it has quarantined a bad outbox op (whose
    /// optimistic local write would otherwise linger).
    ///
    /// It also clears every folder's backfill (#1871): its link, count and
    /// scope, so each folder runs its head and then its full delta again.
    /// And it marks a reconverge pending (`meta.reconverge_pending`, read by
    /// [`Store::reconverge_pending`]), in the same transaction, so an engine
    /// restarted before the reconverge ends still runs it without the
    /// window's cutoff.
    pub fn clear_delta_links(&self) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE folders SET delta_link = NULL, backfill_link = NULL,
                                backfill_synced = 0, history_scope = NULL,
                                backfill_scope = NULL",
            [],
        )?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('reconverge_pending', '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Whether a quarantine reconverge `clear_delta_links` started is still
    /// to finish (#1871): the engine loads it at startup.
    pub fn reconverge_pending(&self) -> Result<bool, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM meta WHERE key = 'reconverge_pending'",
            [],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// The reconverge is over (#1871): every folder's delta ran and no
    /// backfill remains.
    pub fn clear_reconverge_pending(&self) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM meta WHERE key = 'reconverge_pending'", [])?;
        Ok(())
    }

    /// Stores a folder's head delta link (#1871: its recent mail, run in
    /// the sync pass), marking what it covers as [`SCOPE_HEAD`].
    pub fn set_head_link(&self, folder_id: &str, link: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET delta_link = ?2, history_scope = ?3 WHERE id = ?1",
            params![folder_id, link, SCOPE_HEAD],
        )?;
        Ok(())
    }

    /// Records a backfill page (#1871): the full delta's next page, the
    /// running count of items it has walked past, and what the page
    /// covered (`page_scope`: [`SCOPE_WINDOW`] when it ran with the
    /// window's cutoff): the walk's scope becomes [`SCOPE_WINDOW`] once any
    /// page is, else stays what it was ([`SCOPE_ALL`] for a first page).
    pub fn set_backfill_progress(
        &self,
        folder_id: &str,
        next_link: &str,
        synced: i64,
        page_scope: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET backfill_link = ?2, backfill_synced = ?3,
                    backfill_scope = CASE WHEN ?4 = 'window' THEN 'window'
                                          ELSE COALESCE(backfill_scope, ?4) END
             WHERE id = ?1",
            params![folder_id, next_link, synced, page_scope],
        )?;
        Ok(())
    }

    /// Ends a folder's backfill (#1871), in one statement: the full
    /// delta's `deltaLink` becomes the folder's `delta_link` (replacing the
    /// head's), `scope` records what it covers, the backfill link is
    /// cleared and `synced` is set to the final count.
    pub fn complete_backfill(
        &self,
        folder_id: &str,
        delta_link: &str,
        scope: &str,
        synced: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET delta_link = ?2, history_scope = ?3,
                                backfill_link = NULL, backfill_synced = ?4,
                                backfill_scope = NULL
             WHERE id = ?1",
            params![folder_id, delta_link, scope, synced],
        )?;
        Ok(())
    }

    /// Restarts a folder's backfill from a fresh full delta (#1871: its
    /// page link expired, or a window walk resumed in whole-mailbox mode):
    /// clears the link, the count and the walk's scope, and keeps the
    /// head's `delta_link`.
    pub fn reset_backfill(&self, folder_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET backfill_link = NULL, backfill_synced = 0,
                                backfill_scope = NULL
             WHERE id = ?1",
            params![folder_id],
        )?;
        Ok(())
    }

    /// One folder's history state (#1871), `None` for an unknown id.
    pub fn folder_history(&self, folder_id: &str) -> Result<Option<FolderHistory>, StoreError> {
        Ok(self
            .folder_histories()?
            .into_iter()
            .find(|h| h.folder_id == folder_id))
    }

    /// Every folder's history state (#1871), in [`Store::folders`]' order.
    pub fn folder_histories(&self) -> Result<Vec<FolderHistory>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, history_scope, backfill_synced, backfill_link,
                    delta_link IS NOT NULL, total_count, backfill_scope
             FROM folders
             {FOLDER_ORDER}"
        ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(FolderHistory {
                    folder_id: row.get(0)?,
                    scope: row.get(1)?,
                    synced: row.get(2)?,
                    backfill_link: row.get(3)?,
                    has_delta_link: row.get::<_, i64>(4)? != 0,
                    total_count: row.get(5)?,
                    backfill_scope: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Inserts or replaces the body of a message, as a body that was not
    /// fetched by `FetchBody` (a draft's, written locally or from
    /// `createReply`): no fetch format is recorded (`fetched_as` NULL; see
    /// [`Store::get_body_for`]). The schema's `bodies_fts_*` triggers (see
    /// `schema.rs`) keep `messages_fts` in step with this automatically, so
    /// `search` sees the new content right away.
    pub fn put_body(&self, message_id: &str, b: &Body) -> Result<(), StoreError> {
        write_body_on(&self.conn, message_id, b, None)
    }

    /// Inserts or replaces the body of a message as `FetchBody` fetched it,
    /// recording the format it asked Graph for (`fetched_as`, #1901), which
    /// for `Html` can differ from the content type that came back (a
    /// plain-text message's native body is text). Leaves the message's
    /// stored headers as they are, an earlier fetch's included: the engine
    /// stores a fetched body with [`Store::put_fetched_body_with_headers`].
    /// Tests use it.
    #[cfg(test)]
    pub fn put_fetched_body(
        &self,
        message_id: &str,
        b: &Body,
        asked: BodyFormat,
    ) -> Result<(), StoreError> {
        write_body_on(&self.conn, message_id, b, Some(asked))
    }

    /// Inserts or replaces the body of a message as `FetchBody` fetched it,
    /// recording the format it asked Graph for (`fetched_as`, #1901), which
    /// for `Html` can differ from the content type that came back (a
    /// plain-text message's native body is text), and the internet headers
    /// that came with it (#1933), in one transaction: the message's stored headers
    /// are replaced by those of `headers` named in `STORED_HEADERS` (a
    /// header Graph no longer sends reads `None` after a refetch), names
    /// lowercased, values trimmed, the first of a name kept. Read them with
    /// [`Store::message_header`].
    pub fn put_fetched_body_with_headers(
        &self,
        message_id: &str,
        b: &Body,
        asked: BodyFormat,
        headers: &[MessageHeader],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        write_body_on(&tx, message_id, b, Some(asked))?;
        tx.execute(
            "DELETE FROM message_headers WHERE message_id = ?1",
            params![message_id],
        )?;
        for h in headers.iter().filter(|h| is_stored_header(&h.name)) {
            tx.execute(
                "INSERT OR IGNORE INTO message_headers (message_id, name, value) VALUES (?1, ?2, ?3)",
                params![message_id, h.name.to_ascii_lowercase(), h.value.trim()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The value of the internet header `name` stored with the message's
    /// fetched body (#1933), matched ignoring ASCII case: `None` when the
    /// body came without that header, the header is not one the store keeps
    /// (`STORED_HEADERS`), or the message was stored before headers were
    /// (old mail is never refetched for them). Deciding on it is the
    /// caller's.
    pub fn message_header(
        &self,
        message_id: &str,
        name: &str,
    ) -> Result<Option<String>, StoreError> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM message_headers WHERE message_id = ?1 AND name = ?2",
                params![message_id, name.to_ascii_lowercase()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(value)
    }

    /// The stored body of a message when it satisfies `format`, else `None`
    /// (#1901): what an embedder checks before sending `FetchBody`, so a
    /// store filled under the other format upgrades one message at a time
    /// and a body is fetched once per format, not on every open. Use
    /// [`Store::get_body`] to show whatever is stored meanwhile.
    ///
    /// A body `FetchBody` recorded (`fetched_as`) satisfies the format it
    /// was asked in, so a plain-text message fetched under `Html` (its
    /// native body is text) satisfies `Html` and is not fetched again. A
    /// body with no recorded format (`NULL`) satisfies both when it is html,
    /// or a draft's (below), and otherwise counts as asked for text: an
    /// engine from before the column always asked for text.
    ///
    /// A draft's body with no recorded format is a local write (compose,
    /// `createReply`), never a refetch candidate: a `FetchBody` over it
    /// would replace unsynced edits, which a queued `SaveDraft` then pushes
    /// back as Graph's old body. That covers a `createReply` draft of a
    /// plain-text message (stored as text) and a draft an older build's
    /// `update_draft_fields` left as html content under `content_type =
    /// 'text'`. A draft is a message marked `is_draft` or filed in the
    /// Drafts folder (the synced one, or the local sentinel): a complete
    /// delta item resets `is_draft` to 0, as `isDraft` is not in
    /// `MESSAGE_SELECT`, so the flag alone would not last past a sync.
    pub fn get_body_for(
        &self,
        message_id: &str,
        format: BodyFormat,
    ) -> Result<Option<Body>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT b.content_type, b.content, b.fetched_as,
                        COALESCE(m.is_draft = 1 OR f.well_known_name = 'drafts' OR m.folder_id = ?2, 0)
                 FROM bodies b
                 LEFT JOIN messages m ON m.id = b.message_id
                 LEFT JOIN folders f ON f.id = m.folder_id
                 WHERE b.message_id = ?1",
                params![message_id, LOCAL_DRAFTS_SENTINEL_FOLDER_ID],
                |row| {
                    Ok((
                        Body {
                            content_type: row.get(0)?,
                            content: row.get(1)?,
                        },
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, bool>(3)?,
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
        let Some((body, fetched_as, draft)) = row else {
            return Ok(None);
        };
        let satisfies = body_satisfies(fetched_as.as_deref(), &body.content_type, draft, format);
        Ok(satisfies.then_some(body))
    }

    /// Whether a message exists in the store (by its current id).
    pub fn message_exists(&self, message_id: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM messages WHERE id = ?1",
                params![message_id],
                |_| Ok(()),
            )
            .map(|()| true)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(false)
                } else {
                    Err(e)
                }
            })?)
    }

    /// Up to `limit` ids of the messages the engine fetches a body for on
    /// its own (#1978), newest first: received at `since` or later (so a
    /// header-less row is out), not a draft (`get_body_for`'s rule: marked
    /// `is_draft`, or filed in the Drafts folder, the synced one or the
    /// local sentinel), and whose stored body does not satisfy `format`
    /// (`body_satisfies`, the rule `get_body_for` uses). Ids in `skip`
    /// are left out.
    pub fn body_candidates(
        &self,
        format: BodyFormat,
        since: &str,
        limit: usize,
        skip: &HashSet<String>,
    ) -> Result<Vec<String>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT m.id, b.message_id IS NOT NULL, b.fetched_as, COALESCE(b.content_type, '')
             FROM messages m
             LEFT JOIN folders f ON f.id = m.folder_id
             LEFT JOIN bodies b ON b.message_id = m.id
             WHERE m.received_at >= ?1 AND m.received_at != ''
               AND m.is_draft = 0
               AND COALESCE(f.well_known_name, '') != 'drafts'
               AND m.folder_id != ?2
             ORDER BY m.received_at DESC, m.id",
        )?;
        let mut rows = stmt.query(params![since, LOCAL_DRAFTS_SENTINEL_FOLDER_ID])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let has_body: bool = row.get(1)?;
            let fetched_as: Option<String> = row.get(2)?;
            let content_type: String = row.get(3)?;
            if has_body && body_satisfies(fetched_as.as_deref(), &content_type, false, format) {
                continue;
            }
            if skip.contains(&id) {
                continue;
            }
            out.push(id);
            if out.len() == limit {
                break;
            }
        }
        Ok(out)
    }

    /// Whether the engine still fetches a body for `message_id` on its own
    /// (#1978): [`Store::body_candidates`]' rule for one row. A missing row
    /// needs none.
    pub fn needs_body(
        &self,
        message_id: &str,
        format: BodyFormat,
        since: &str,
    ) -> Result<bool, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT m.received_at,
                        m.is_draft = 1 OR COALESCE(f.well_known_name, '') = 'drafts'
                            OR m.folder_id = ?2,
                        b.message_id IS NOT NULL, b.fetched_as, COALESCE(b.content_type, '')
                 FROM messages m
                 LEFT JOIN folders f ON f.id = m.folder_id
                 LEFT JOIN bodies b ON b.message_id = m.id
                 WHERE m.id = ?1",
                params![message_id, LOCAL_DRAFTS_SENTINEL_FOLDER_ID],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
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
        let Some((received, draft, has_body, fetched_as, content_type)) = row else {
            return Ok(false);
        };
        if received.is_empty() || received.as_str() < since || draft {
            return Ok(false);
        }
        Ok(!(has_body && body_satisfies(fetched_as.as_deref(), &content_type, false, format)))
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
        // The folder is resolved inside the write (IMMEDIATE), so the sync
        // thread's `retain_folders` runs wholly before it (the sentinel is
        // not there yet, or Drafts is) or after it (the sentinel holds this
        // draft and is kept), never between the sentinel's insert and the
        // draft's (#1886).
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let folder_id = drafts_folder_id_on(&tx)?;
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
    /// The row's unsaved-edit generation goes up (`local_edit`, #1871): a
    /// fresh delta does not overwrite it until a push clears it.
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
                "UPDATE messages SET subject = ?2, to_recipients = ?3, cc_recipients = ?4, bcc_recipients = ?5,
                                     local_edit = local_edit + 1
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

    /// Records a file attached to draft `draft_id`. Idempotent per (draft, path):
    /// an existing row keeps its `uploaded` flag.
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

    /// Records the file `path` attached inline to draft `draft_id` with
    /// `content_id` (#1949), so an html body's `cid:` reference finds it.
    /// `content_type` `None` is derived from the name at send time. Keyed by
    /// its path. The same path already attached as a pending plain file is
    /// made inline in place (its attach position holds, a `None` type keeps
    /// the row's); an uploaded plain row stays plain (Graph has it
    /// non-inline). An inline row is left as it is: the file is read at send
    /// time, so a re-add needs no update, and the same file under a second
    /// content id is ignored.
    pub fn add_inline_outbound_attachment_file(
        &self,
        draft_id: &str,
        path: &str,
        name: &str,
        size: i64,
        content_type: Option<&str>,
        content_id: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            concat!(
                "INSERT INTO outbound_attachments
                     (draft_id, path, name, size, content_type, content_id) VALUES (",
                current_id_sql!(),
                ", ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(draft_id, path) DO UPDATE SET
                     content_id = excluded.content_id,
                     content_type = COALESCE(excluded.content_type,
                                             outbound_attachments.content_type)
                 WHERE outbound_attachments.uploaded = 0
                   AND outbound_attachments.content_id IS NULL"
            ),
            params![draft_id, path, name, size, content_type, content_id],
        )?;
        Ok(())
    }

    /// Records a picture with no file (a pasted one, #1949), attached inline
    /// to draft `draft_id` with `content_id`: its bytes are kept in the row,
    /// keyed `cid:<content_id>` (returned; the `cid:` key namespace is
    /// reserved, never a file path), `size` = `bytes.len()`. Re-adding a
    /// content id replaces a pending row's name, type and bytes in place
    /// (its attach position holds); an uploaded one is left alone, as the
    /// Graph draft already has its bytes.
    pub fn add_outbound_attachment_bytes(
        &self,
        draft_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
        content_id: &str,
    ) -> Result<String, StoreError> {
        let key = format!("cid:{content_id}");
        self.conn.execute(
            concat!(
                "INSERT INTO outbound_attachments
                     (draft_id, path, name, size, content_type, content_id, data) VALUES (",
                current_id_sql!(),
                ", ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(draft_id, path) DO UPDATE SET
                     name = excluded.name, size = excluded.size,
                     content_type = excluded.content_type, data = excluded.data
                 WHERE outbound_attachments.uploaded = 0"
            ),
            params![
                draft_id,
                key,
                name,
                bytes.len() as i64,
                content_type,
                content_id,
                bytes
            ],
        )?;
        Ok(key)
    }

    /// The bytes of the attachment `path` (its key) of `draft_id` when they
    /// are kept in the row (#1949); `None` for a file row or a missing one.
    pub fn outbound_attachment_bytes(
        &self,
        draft_id: &str,
        path: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let found = self
            .conn
            .query_row(
                concat!(
                    "SELECT data FROM outbound_attachments WHERE draft_id = ",
                    current_id_sql!(),
                    " AND path = ?2"
                ),
                params![draft_id, path],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?;
        Ok(found.flatten())
    }

    /// The files attached to `draft_id`, in attach (insertion) order — SQLite's
    /// implicit `rowid` tracks insertion order, and callers (compose's
    /// attachment list, Ctrl+R's LIFO removal) rely on the last element being
    /// the most-recently-added attachment. A bytes row's bytes are not
    /// loaded (compose lists these on every redraw): see
    /// `outbound_attachment_bytes`.
    pub fn outbound_attachments(
        &self,
        draft_id: &str,
    ) -> Result<Vec<OutboundAttachment>, StoreError> {
        let mut stmt = self.conn.prepare(concat!(
            "SELECT path, name, size, uploaded, content_id, content_type, data IS NOT NULL
             FROM outbound_attachments WHERE draft_id = ",
            current_id_sql!(),
            " ORDER BY rowid"
        ))?;
        let rows = stmt
            .query_map(params![draft_id], |r| {
                Ok(OutboundAttachment {
                    path: r.get(0)?,
                    name: r.get(1)?,
                    size: r.get(2)?,
                    uploaded: r.get(3)?,
                    content_id: r.get(4)?,
                    content_type: r.get(5)?,
                    from_bytes: r.get(6)?,
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

    /// Marks the file `path` of `draft_id` as already on the Graph draft
    /// (#1874), right after `SendDraft` uploaded it, so a resend after a
    /// failed send does not upload it again. No-op if absent.
    pub fn mark_outbound_attachment_uploaded(
        &self,
        draft_id: &str,
        path: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            concat!(
                "UPDATE outbound_attachments SET uploaded = 1 WHERE draft_id = ",
                current_id_sql!(),
                " AND path = ?2"
            ),
            params![draft_id, path],
        )?;
        Ok(())
    }

    /// Removes the files attached to `draft_id` that are not on the Graph
    /// draft yet (`uploaded = 0`): compose's Discard (#1874). An uploaded
    /// one stays listed, as Send would still deliver it.
    pub fn discard_pending_outbound_attachments(&self, draft_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            concat!(
                "DELETE FROM outbound_attachments WHERE uploaded = 0 AND draft_id = ",
                current_id_sql!()
            ),
            params![draft_id],
        )?;
        Ok(())
    }

    /// Removes every attached file from `draft_id` (after a send).
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

    /// Rewrites every row keyed by `local_id` (the message, its body and
    /// stored headers, and —
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
            "UPDATE message_headers SET message_id = ?2 WHERE message_id = ?1",
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
        drafts_folder_id_on(&self.conn)
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

    /// Records whether the `/send` POST of the `SendDraft` op `seq` went out
    /// (#1874). Set right before the POST, so a lost answer or a crash
    /// counts; cleared when Graph refuses the POST (a 4xx, a 404, a 401) or
    /// later says the message is still a draft. A missing `seq` changes
    /// nothing.
    pub fn set_send_posted(&self, seq: i64, posted: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE outbox SET send_posted = ?2 WHERE seq = ?1",
            params![seq, posted],
        )?;
        Ok(())
    }

    /// Whether the `/send` POST of the op `seq` may have gone out
    /// (`set_send_posted`). A missing `seq` is `false`.
    pub fn send_posted(&self, seq: i64) -> Result<bool, StoreError> {
        let posted = self
            .conn
            .query_row(
                "SELECT send_posted FROM outbox WHERE seq = ?1",
                params![seq],
                |row| row.get::<_, bool>(0),
            )
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(false)
                } else {
                    Err(e)
                }
            })?;
        Ok(posted)
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
        upsert_event_on(&self.conn, e)
    }

    /// Replaces the full set of attendees stored for an event. See
    /// `put_attachments` for why the delete-then-insert runs in one
    /// transaction via `unchecked_transaction` (same shared-`&self`
    /// reasoning applies here).
    pub fn put_event_attendees(&self, id: &str, a: &[NewAttendee]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        put_event_attendees_on(&tx, id, a)?;
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
    /// untouched. Each event and its attendees go through `upsert_event_on`
    /// and `put_event_attendees_on`, the statements `upsert_event` and
    /// `put_event_attendees` run, here on the transaction (calling
    /// `put_event_attendees` itself would nest a second `BEGIN` on the same
    /// connection while this one is still open, which errors). The
    /// per-event attendee `DELETE` before each insert is technically
    /// redundant with `events`' `ON DELETE CASCADE` for an event this
    /// call's own window-delete just removed, but not for one that instead
    /// takes the `ON CONFLICT` update path (a boundary mismatch between this
    /// window and the row's stored dates) — cheap insurance against a
    /// duplicate attendee row in that edge case, and the same
    /// delete-then-insert `put_event_attendees` itself always does.
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
            upsert_event_on(&tx, e)?;
        }
        for (id, atts) in attendees {
            put_event_attendees_on(&tx, id, atts)?;
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
                    reminder_minutes, is_reminder_on, show_as, is_online_meeting,
                    online_meeting_provider, join_url, categories, sensitivity, importance,
                    event_type, graph_recurrence, is_cancelled, is_organizer,
                    response_requested, allow_new_time_proposals, has_attachments,
                    original_start_tz, original_end_tz, calendar_id
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
                    show_as: row.get(12)?,
                    is_online_meeting: row.get(13)?,
                    online_meeting_provider: row.get(14)?,
                    join_url: row.get(15)?,
                    categories: decode_categories(&row.get::<_, String>(16)?),
                    sensitivity: row.get(17)?,
                    importance: row.get(18)?,
                    event_type: row.get(19)?,
                    recurrence: decode_graph_recurrence(&row.get::<_, String>(20)?),
                    is_cancelled: row.get(21)?,
                    is_organizer: row.get(22)?,
                    response_requested: row.get(23)?,
                    allow_new_time_proposals: row.get(24)?,
                    has_attachments: row.get(25)?,
                    original_start_time_zone: row.get(26)?,
                    original_end_time_zone: row.get(27)?,
                    calendar_id: row.get(28)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The attendees stored for an event, in insertion order (the order
    /// `put_event_attendees` last wrote them in).
    pub fn event_attendees(&self, id: &str) -> Result<Vec<AttendeeRow>, StoreError> {
        event_attendees_on(&self.conn, id)
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
    /// Graph). `response_status` is `"organizer"`. A `None` field stores what
    /// Graph sets on create (#1983, #1994); a `None` reminder, which Graph
    /// takes from the mailbox's default, is marked `reminder_unset` (its
    /// `CreateEvent` sends none, and `reconcile_created_event` writes back
    /// Graph's).
    pub fn create_local_event(
        &self,
        f: &LocalEventFields,
        organizer_name: &str,
        organizer_addr: &str,
    ) -> Result<String, StoreError> {
        let id = local_draft_id(); // a unique "local:<hex>" id
        let reminder = f.reminder.unwrap_or_default();
        let enum_or =
            |v: &Option<String>, default: &str| non_empty(v).unwrap_or_else(|| default.to_string());
        let tx = self.conn.unchecked_transaction()?;
        upsert_event_on(
            &tx,
            &NewEvent {
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
                reminder_minutes: reminder.minutes_before_start,
                is_reminder_on: reminder.on,
                // What the host set, else what Graph sets on create (#1983),
                // and the join link `Event::from_json` would find in the
                // location or body. A series reads as `calendarView`'s
                // occurrences on the next refresh.
                show_as: enum_or(&f.show_as, "busy"),
                sensitivity: enum_or(&f.sensitivity, "normal"),
                importance: enum_or(&f.importance, "normal"),
                categories: f.categories.clone().unwrap_or_default(),
                is_online_meeting: f.is_online_meeting.unwrap_or(false),
                online_meeting_provider: non_empty(&f.online_meeting_provider).unwrap_or_default(),
                response_requested: f.response_requested.unwrap_or(true),
                allow_new_time_proposals: f.allow_new_time_proposals.unwrap_or(true),
                event_type: if f.recurrence.is_some() {
                    "seriesMaster"
                } else {
                    "singleInstance"
                }
                .to_string(),
                // The create-side recurrence is already in Graph's shape.
                recurrence: f
                    .recurrence
                    .as_ref()
                    .and_then(|r| EventRecurrence::from_json(&r.to_json())),
                is_organizer: true,
                join_url: meeting_link_in(&f.location, &f.body_html).unwrap_or_default(),
                ..NewEvent::default()
            },
        )?;
        put_event_attendees_on(&tx, &id, &typed_attendees(&f.attendees, &[]))?;
        tx.execute(
            "UPDATE events SET recurrence = ?2, reminder_unset = ?3 WHERE id = ?1",
            params![
                id,
                f.recurrence
                    .as_ref()
                    .map(|r| r.to_json().to_string())
                    .unwrap_or_default(),
                f.reminder.is_none()
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Writes a host's edit of a stored event and returns the groups whose
    /// value changed (#1994): the `UpdateEvent` op sends only those. A `None`
    /// field keeps the stored value and is not in the set; so is a `Some`
    /// equal to it, except a reminder on a row still marked `reminder_unset`
    /// (its create would otherwise send none). Attendees are rewritten only when they changed (as the
    /// ordered list of name, address and type), and each one whose address
    /// matches a stored one (case-insensitive) keeps its response and
    /// response time, and its type when the input's is `""`. Recurrence is
    /// create-only: never written here, never in the set.
    ///
    /// `join_url` follows the new location and body (`meeting_link_in`, as
    /// `create_local_event` derives it), except on an event stored as an
    /// online meeting, whose join link is Graph's `onlineMeeting.joinUrl`,
    /// which no location or body edit changes (#1983), and which Graph keeps
    /// even when an edit sends `isOnlineMeeting: false`.
    pub fn update_event_fields(
        &self,
        id: &str,
        f: &LocalEventFields,
    ) -> Result<EventFieldSet, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let old = event_for_send_on(&tx, id)?
            .ok_or_else(|| StoreError::Sql(format!("no event stored for {id}")))?;
        let stored_attendees = event_attendees_on(&tx, id)?;
        let mut changed = EventFieldSet::default();
        let mut mark = |group: EventFieldSet, differs: bool| {
            if differs {
                changed.insert(group);
            }
        };
        mark(EventFieldSet::SUBJECT, f.subject != old.subject);
        mark(
            EventFieldSet::TIMES,
            f.start_utc != old.start_utc
                || f.end_utc != old.end_utc
                || f.is_all_day != old.is_all_day,
        );
        mark(EventFieldSet::LOCATION, f.location != old.location);
        mark(EventFieldSet::BODY, f.body_html != old.body_html);
        let show_as = non_empty(&f.show_as).unwrap_or(old.show_as.clone());
        mark(EventFieldSet::SHOW_AS, show_as != old.show_as);
        let sensitivity = non_empty(&f.sensitivity).unwrap_or(old.sensitivity.clone());
        mark(EventFieldSet::SENSITIVITY, sensitivity != old.sensitivity);
        let importance = non_empty(&f.importance).unwrap_or(old.importance.clone());
        mark(EventFieldSet::IMPORTANCE, importance != old.importance);
        let categories = f.categories.clone().unwrap_or(old.categories.clone());
        mark(EventFieldSet::CATEGORIES, categories != old.categories);
        let old_reminder = EventReminder {
            on: old.is_reminder_on,
            minutes_before_start: old.reminder_minutes,
        };
        let reminder = f.reminder.unwrap_or(old_reminder);
        mark(
            EventFieldSet::REMINDER,
            f.reminder.is_some() && (reminder != old_reminder || old.reminder_unset),
        );
        let online = f.is_online_meeting.unwrap_or(old.is_online_meeting);
        let provider =
            non_empty(&f.online_meeting_provider).unwrap_or(old.online_meeting_provider.clone());
        mark(
            EventFieldSet::ONLINE_MEETING,
            online != old.is_online_meeting || provider != old.online_meeting_provider,
        );
        let response_requested = f.response_requested.unwrap_or(old.response_requested);
        mark(
            EventFieldSet::RESPONSE_REQUESTED,
            response_requested != old.response_requested,
        );
        let allow_new = f
            .allow_new_time_proposals
            .unwrap_or(old.allow_new_time_proposals);
        mark(
            EventFieldSet::ALLOW_NEW_TIME_PROPOSALS,
            allow_new != old.allow_new_time_proposals,
        );
        let attendees = typed_attendees(&f.attendees, &stored_attendees);
        let attendees_differ = attendees.len() != stored_attendees.len()
            || attendees
                .iter()
                .zip(&stored_attendees)
                .any(|(a, s)| a.name != s.name || a.addr != s.addr || a.r#type != s.r#type);
        mark(EventFieldSet::ATTENDEES, attendees_differ);

        tx.execute(
            "UPDATE events SET subject = ?2, start_utc = ?3, end_utc = ?4, is_all_day = ?5,
                    location = ?6, body_html = ?7,
                    join_url = CASE WHEN is_online_meeting THEN join_url ELSE ?8 END,
                    show_as = ?9, sensitivity = ?10, importance = ?11, categories = ?12,
                    is_reminder_on = ?13, reminder_minutes = ?14,
                    reminder_unset = reminder_unset AND ?15,
                    is_online_meeting = ?16, online_meeting_provider = ?17,
                    response_requested = ?18, allow_new_time_proposals = ?19
             WHERE id = ?1",
            params![
                id,
                f.subject,
                f.start_utc,
                f.end_utc,
                f.is_all_day,
                f.location,
                f.body_html,
                meeting_link_in(&f.location, &f.body_html).unwrap_or_default(),
                show_as,
                sensitivity,
                importance,
                encode_categories(&categories),
                reminder.on,
                reminder.minutes_before_start,
                f.reminder.is_none(),
                online,
                provider,
                response_requested,
                allow_new
            ],
        )?;
        if attendees_differ {
            put_event_attendees_on(&tx, id, &attendees)?;
        }
        tx.commit()?;
        Ok(changed)
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
        event_for_send_on(&self.conn, id)
    }

    /// Re-points a `local:` event id to its Graph id after `create_event`
    /// (mirrors `reconcile_id` for drafts) and writes back what only Graph
    /// decides on a create (#1994), in one transaction: the reminder (the
    /// host's, or the mailbox default when it set none), and an online
    /// meeting's join link and provider when Graph returned them. Clears
    /// `reminder_unset`, so a Graph-id row never keeps it.
    pub fn reconcile_created_event(
        &self,
        local_id: &str,
        created: &Event,
    ) -> Result<(), StoreError> {
        let graph_id = created.id.as_str();
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
        tx.execute(
            "UPDATE events SET is_reminder_on = ?2, reminder_minutes = ?3, reminder_unset = 0
             WHERE id = ?1",
            params![graph_id, created.is_reminder_on, created.reminder_minutes],
        )?;
        set_event_online_meeting_on(
            &tx,
            graph_id,
            &created.join_url,
            &created.online_meeting_provider,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Writes an online meeting's join link and provider as Graph returned
    /// them (an update that turned the meeting on, #1994); an empty one is
    /// left as stored.
    pub fn set_event_online_meeting(
        &self,
        id: &str,
        join_url: &str,
        provider: &str,
    ) -> Result<(), StoreError> {
        set_event_online_meeting_on(&self.conn, id, join_url, provider)
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

/// `Store::drafts_folder_id` on `conn`, which `create_local_draft` passes
/// its transaction as.
fn drafts_folder_id_on(conn: &Connection) -> Result<String, StoreError> {
    let existing = conn.query_row(
        "SELECT id FROM folders WHERE well_known_name = 'drafts' LIMIT 1",
        [],
        |row| row.get::<_, String>(0),
    );
    match existing {
        Ok(id) => Ok(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            conn.execute(
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

/// Whether a stored body satisfies `format` (#1901), the one rule
/// [`Store::get_body_for`] and the engine's own fetches (#1978,
/// [`Store::body_candidates`], [`Store::needs_body`]) share. A body
/// `FetchBody` recorded (`fetched_as`) satisfies the format it was asked in
/// (a value this build does not know satisfies none: refetch). One with no
/// recorded format satisfies both when it is html or a draft's, and
/// otherwise counts as asked for text.
fn body_satisfies(
    fetched_as: Option<&str>,
    content_type: &str,
    draft: bool,
    format: BodyFormat,
) -> bool {
    match fetched_as {
        Some(asked) => BodyFormat::parse(asked) == Some(format),
        None => draft || content_type.eq_ignore_ascii_case("html") || format == BodyFormat::Text,
    }
}

/// The local `folders.id` a new draft is filed under when the real Drafts
/// folder (`well_known_name = 'drafts'`) hasn't synced down from Graph yet.
/// See `Store::drafts_folder_id`.
pub(crate) const LOCAL_DRAFTS_SENTINEL_FOLDER_ID: &str = "local:drafts-pending";

/// Mints a fresh local draft id: `local:` followed by 16 bytes of
/// OS randomness as lowercase hex (reusing `pkce::random_bytes`, the same
/// OS-entropy source the PKCE verifier uses, rather than pulling in a
/// `uuid` crate for this one call site). The `local:` prefix is what marks
/// an id as not-yet-synced-to-Graph throughout the store and sync engine.
fn local_draft_id() -> String {
    format!("local:{}", to_hex(&crate::pkce::random_bytes(16)))
}

/// Inserts or replaces a message's body on `conn` (the store's connection or
/// a transaction on it), with the format it was fetched in (`None`: a local
/// write).
fn write_body_on(
    conn: &Connection,
    message_id: &str,
    b: &Body,
    asked: Option<BodyFormat>,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO bodies (message_id, content_type, content, fetched_as) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(message_id) DO UPDATE SET
             content_type = excluded.content_type,
             content = excluded.content,
             fetched_as = excluded.fetched_as",
        params![
            message_id,
            b.content_type,
            b.content,
            asked.map(BodyFormat::as_str)
        ],
    )?;
    Ok(())
}

/// `Store::event_for_send` on `conn` (a transaction in
/// `Store::update_event_fields`).
fn event_for_send_on(conn: &Connection, id: &str) -> Result<Option<EventSendData>, StoreError> {
    let row = conn
        .query_row(
            "SELECT subject, start_utc, end_utc, is_all_day, location, body_html, recurrence,
                    show_as, sensitivity, importance, categories, is_reminder_on,
                    reminder_minutes, is_online_meeting, online_meeting_provider,
                    response_requested, allow_new_time_proposals, reminder_unset
             FROM events WHERE id = ?1",
            params![id],
            |r| {
                let recurrence_raw: String = r.get(6)?;
                let recurrence = if recurrence_raw.is_empty() {
                    None
                } else {
                    crate::json::parse(&recurrence_raw)
                        .ok()
                        .and_then(|v| Recurrence::from_json(&v))
                };
                Ok(EventSendData {
                    subject: r.get(0)?,
                    start_utc: r.get(1)?,
                    end_utc: r.get(2)?,
                    is_all_day: r.get(3)?,
                    location: r.get(4)?,
                    body_html: r.get(5)?,
                    attendees: Vec::new(),
                    recurrence,
                    show_as: r.get(7)?,
                    sensitivity: r.get(8)?,
                    importance: r.get(9)?,
                    categories: decode_categories(&r.get::<_, String>(10)?),
                    is_reminder_on: r.get(11)?,
                    reminder_minutes: r.get(12)?,
                    is_online_meeting: r.get(13)?,
                    online_meeting_provider: r.get(14)?,
                    response_requested: r.get(15)?,
                    allow_new_time_proposals: r.get(16)?,
                    reminder_unset: r.get(17)?,
                })
            },
        )
        .optional()?;
    let Some(mut data) = row else {
        return Ok(None);
    };
    data.attendees = event_attendees_on(conn, id)?
        .into_iter()
        .map(|a| AttendeeInput {
            name: a.name,
            addr: a.addr,
            r#type: a.r#type,
        })
        .collect();
    Ok(Some(data))
}

/// `Store::event_attendees` on `conn`.
fn event_attendees_on(conn: &Connection, id: &str) -> Result<Vec<AttendeeRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT name, addr, type, response, response_time
         FROM event_attendees WHERE event_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![id], |row| {
            Ok(AttendeeRow {
                name: row.get(0)?,
                addr: row.get(1)?,
                r#type: row.get(2)?,
                response: row.get(3)?,
                response_time: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `Store::set_event_online_meeting` on `conn`.
fn set_event_online_meeting_on(
    conn: &Connection,
    id: &str,
    join_url: &str,
    provider: &str,
) -> Result<(), StoreError> {
    if !join_url.is_empty() {
        conn.execute(
            "UPDATE events SET join_url = ?2 WHERE id = ?1",
            params![id, join_url],
        )?;
    }
    if !provider.is_empty() {
        conn.execute(
            "UPDATE events SET online_meeting_provider = ?2 WHERE id = ?1",
            params![id, provider],
        )?;
    }
    Ok(())
}

/// A host's enum string, `Some("")` read as `None` (Graph refuses `""`).
fn non_empty(v: &Option<String>) -> Option<String> {
    v.clone().filter(|s| !s.is_empty())
}

/// The attendees a host's `LocalEventFields::attendees` stores, given the
/// event's stored ones (`&[]` on a create): one whose address matches a
/// stored attendee (case-insensitive) keeps that row's response and response
/// time, and its type when the input's is `""` (#1994); a new one is
/// `required` (or its given type) with no response.
fn typed_attendees(input: &[AttendeeInput], stored: &[AttendeeRow]) -> Vec<NewAttendee> {
    input
        .iter()
        .map(|a| {
            let old = stored.iter().find(|s| s.addr.eq_ignore_ascii_case(&a.addr));
            let r#type = if !a.r#type.is_empty() {
                a.r#type.clone()
            } else {
                old.map(|s| s.r#type.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "required".to_string())
            };
            NewAttendee {
                name: a.name.clone(),
                addr: a.addr.clone(),
                r#type,
                response: old
                    .map(|s| s.response.clone())
                    .unwrap_or_else(|| "none".to_string()),
                response_time: old.map(|s| s.response_time.clone()).unwrap_or_default(),
            }
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

/// Reads `events.graph_recurrence` (`EventRecurrence::to_json` text, `''`
/// for none). Text that does not parse reads as `None`.
fn decode_graph_recurrence(s: &str) -> Option<EventRecurrence> {
    if s.is_empty() {
        return None;
    }
    json::parse(s)
        .ok()
        .and_then(|v| EventRecurrence::from_json(&v))
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

/// The one `INSERT ... ON CONFLICT(id) DO UPDATE` that writes an `events`
/// row from a `NewEvent`, shared by `Store::upsert_event` and
/// `Store::replace_events_in_window` (on its transaction, which derefs to a
/// `Connection`) so every column `NewEvent` carries is written by both:
/// the calendar sync once wrote its own copy without `reminder_minutes`/
/// `is_reminder_on` and lost every synced event's reminder (#1937).
fn upsert_event_on(conn: &Connection, e: &NewEvent) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO events (
             id, subject, start_utc, end_utc, is_all_day, location,
             organizer_name, organizer_addr, response_status,
             series_master_id, body_preview, web_link, last_modified,
             body_html, reminder_minutes, is_reminder_on,
             show_as, is_online_meeting, online_meeting_provider, join_url,
             categories, sensitivity, importance, event_type, graph_recurrence,
             is_cancelled, is_organizer, response_requested,
             allow_new_time_proposals, has_attachments, original_start_tz,
             original_end_tz, calendar_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                   ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                   ?31, ?32, ?33)
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
             is_reminder_on = excluded.is_reminder_on,
             show_as = excluded.show_as,
             is_online_meeting = excluded.is_online_meeting,
             online_meeting_provider = excluded.online_meeting_provider,
             join_url = excluded.join_url,
             categories = excluded.categories,
             sensitivity = excluded.sensitivity,
             importance = excluded.importance,
             event_type = excluded.event_type,
             graph_recurrence = excluded.graph_recurrence,
             is_cancelled = excluded.is_cancelled,
             is_organizer = excluded.is_organizer,
             response_requested = excluded.response_requested,
             allow_new_time_proposals = excluded.allow_new_time_proposals,
             has_attachments = excluded.has_attachments,
             original_start_tz = excluded.original_start_tz,
             original_end_tz = excluded.original_end_tz,
             calendar_id = excluded.calendar_id",
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
            e.show_as,
            e.is_online_meeting,
            e.online_meeting_provider,
            e.join_url,
            encode_categories(&e.categories),
            e.sensitivity,
            e.importance,
            e.event_type,
            e.recurrence
                .as_ref()
                .map(|r| r.to_json().to_string())
                .unwrap_or_default(),
            e.is_cancelled,
            e.is_organizer,
            e.response_requested,
            e.allow_new_time_proposals,
            e.has_attachments,
            e.original_start_time_zone,
            e.original_end_time_zone,
            e.calendar_id,
        ],
    )?;
    Ok(())
}

/// Replaces an event's `event_attendees` rows with `a` (delete, then
/// insert), on `conn`: `Store::put_event_attendees` runs it in its own
/// transaction, `Store::replace_events_in_window` on its window's, so the
/// attendee SQL exists once, as `upsert_event_on`'s does (#1937).
fn put_event_attendees_on(
    conn: &Connection,
    id: &str,
    a: &[NewAttendee],
) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM event_attendees WHERE event_id = ?1",
        params![id],
    )?;
    for att in a {
        conn.execute(
            "INSERT INTO event_attendees (event_id, name, addr, type, response, response_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                att.name,
                att.addr,
                att.r#type,
                att.response,
                att.response_time
            ],
        )?;
    }
    Ok(())
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
            ("local_edit", V::Integer(0)),
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
                ("local_edit", V::Integer(0)),
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
    fn set_folder_counts_changes_only_the_counts() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder {
            id: "F1".into(),
            display_name: "Inbox".into(),
            parent_id: Some("ROOT".into()),
            total_count: 2,
            unread_count: 1,
            well_known_name: Some("inbox".into()),
        })
        .unwrap();
        s.set_delta_link("F1", "LINK").unwrap();

        assert!(s.set_folder_counts("F1", 5, 3).unwrap(), "changed");
        let f = s
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.id == "F1")
            .unwrap();
        assert_eq!((f.total_count, f.unread_count), (5, 3));
        assert_eq!(f.display_name, "Inbox");
        assert_eq!(f.parent_id.as_deref(), Some("ROOT"));
        assert_eq!(f.well_known_name.as_deref(), Some("inbox"));
        assert_eq!(f.delta_link.as_deref(), Some("LINK"));

        assert!(!s.set_folder_counts("F1", 5, 3).unwrap(), "unchanged");
        assert!(
            s.set_folder_counts("F1", 5, 2).unwrap(),
            "unread alone changed"
        );
        assert!(!s.set_folder_counts("NOPE", 1, 1).unwrap(), "unknown id");
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
            ..Default::default()
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
                    ..Default::default()
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

    /// #1937: the calendar sync's writer keeps an event's reminder settings,
    /// on insert and when a later refresh changes them.
    #[test]
    fn replace_events_in_window_writes_reminder_fields() {
        let s = Store::open_in_memory().unwrap();
        let (from, to) = ("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z");
        let mut e = ev("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z");
        e.reminder_minutes = 15;
        e.is_reminder_on = true;
        s.replace_events_in_window(from, to, std::slice::from_ref(&e), &[])
            .unwrap();
        let rows = s.events_in_window(from, to).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reminder_minutes, 15);
        assert!(rows[0].is_reminder_on);

        e.reminder_minutes = 30;
        e.is_reminder_on = false;
        s.replace_events_in_window(from, to, std::slice::from_ref(&e), &[])
            .unwrap();
        let rows = s.events_in_window(from, to).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reminder_minutes, 30);
        assert!(!rows[0].is_reminder_on);
    }

    /// #1937: an event whose stored dates lie outside the replaced window
    /// survives the window delete and takes the `ON CONFLICT` update path;
    /// that path writes the reminder settings too.
    #[test]
    fn replace_events_in_window_conflict_path_updates_reminder_fields() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("E1", "2026-08-01T09:00:00Z", "2026-08-01T10:00:00Z"))
            .unwrap();
        let mut moved = ev("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z");
        moved.reminder_minutes = 30;
        moved.is_reminder_on = true;
        s.replace_events_in_window(
            "2026-07-20T00:00:00Z",
            "2026-07-21T00:00:00Z",
            &[moved],
            &[],
        )
        .unwrap();
        let rows = s
            .events_in_window("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].start_utc, "2026-07-20T09:00:00Z");
        assert_eq!(rows[0].reminder_minutes, 30);
        assert!(rows[0].is_reminder_on);
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
                    ..Default::default()
                },
                NewAttendee {
                    name: "Carol".into(),
                    addr: "carol@x".into(),
                    r#type: "optional".into(),
                    response: "none".into(),
                    ..Default::default()
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
                ..Default::default()
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
            attendees: vec![AttendeeInput {
                name: "Bob".into(),
                addr: "bob@x.com".into(),
                r#type: String::new(),
            }],
            recurrence: None,
            ..Default::default()
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
            vec![AttendeeInput {
                name: "Bob".into(),
                addr: "bob@x.com".into(),
                r#type: "required".into(),
            }]
        );
        assert_eq!(send.body_html, "<p>agenda</p>");
    }

    #[test]
    fn reconcile_created_event_repoints_event_and_attendees() {
        let s = Store::open_in_memory().unwrap();
        let id = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        let created = Event {
            id: "EV1".into(),
            ..Event::default()
        };
        s.reconcile_created_event(&id, &created).unwrap();
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

    /// A Graph series recurrence for the #1983 tests.
    fn graph_recurrence(kind: &str, occurrences: u32) -> EventRecurrence {
        let json = format!(
            r#"{{"pattern":{{"type":"{kind}","interval":2,"month":0,"dayOfMonth":0,
                "daysOfWeek":["tuesday"],"firstDayOfWeek":"sunday","index":"second"}},
                "range":{{"type":"numbered","startDate":"2026-07-14","endDate":"0001-01-01",
                "recurrenceTimeZone":"UTC","numberOfOccurrences":{occurrences}}}}}"#
        );
        EventRecurrence::from_json(&crate::json::parse(&json).unwrap()).unwrap()
    }

    /// `ev` with every #1983 field set away from its default.
    fn client_event(id: &str, start: &str, end: &str) -> NewEvent {
        NewEvent {
            show_as: "oof".into(),
            is_online_meeting: true,
            online_meeting_provider: "teamsForBusiness".into(),
            join_url: "https://teams.microsoft.com/l/meetup-join/1".into(),
            categories: vec!["Red category".into(), "Plan, phase; 2".into()],
            sensitivity: "private".into(),
            importance: "high".into(),
            event_type: "seriesMaster".into(),
            recurrence: Some(graph_recurrence("relativeMonthly", 4)),
            is_cancelled: true,
            is_organizer: true,
            response_requested: false,
            allow_new_time_proposals: false,
            has_attachments: true,
            original_start_time_zone: "Pacific Standard Time".into(),
            original_end_time_zone: "Eastern Standard Time".into(),
            calendar_id: "CAL1".into(),
            ..ev(id, start, end)
        }
    }

    /// The #1983 fields of an `EventRow` match the `NewEvent` written.
    fn assert_client_fields(row: &EventRow, e: &NewEvent) {
        assert_eq!(row.show_as, e.show_as);
        assert_eq!(row.is_online_meeting, e.is_online_meeting);
        assert_eq!(row.online_meeting_provider, e.online_meeting_provider);
        assert_eq!(row.join_url, e.join_url);
        assert_eq!(row.categories, e.categories);
        assert_eq!(row.sensitivity, e.sensitivity);
        assert_eq!(row.importance, e.importance);
        assert_eq!(row.event_type, e.event_type);
        assert_eq!(row.recurrence, e.recurrence);
        assert_eq!(row.is_cancelled, e.is_cancelled);
        assert_eq!(row.is_organizer, e.is_organizer);
        assert_eq!(row.response_requested, e.response_requested);
        assert_eq!(row.allow_new_time_proposals, e.allow_new_time_proposals);
        assert_eq!(row.has_attachments, e.has_attachments);
        assert_eq!(row.original_start_time_zone, e.original_start_time_zone);
        assert_eq!(row.original_end_time_zone, e.original_end_time_zone);
        assert_eq!(row.calendar_id, e.calendar_id);
    }

    #[test]
    fn store_defaults_agree_with_graph_on_request_flags() {
        for (requested, proposals) in [
            (
                NewEvent::default().response_requested,
                NewEvent::default().allow_new_time_proposals,
            ),
            (
                EventRow::default().response_requested,
                EventRow::default().allow_new_time_proposals,
            ),
        ] {
            assert!(requested && proposals);
        }
        // And the schema's column default: a row written without them.
        let s = Store::open_in_memory().unwrap();
        s.conn
            .execute(
                "INSERT INTO events (id, start_utc, end_utc) VALUES ('E', ?1, ?2)",
                params!["2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"],
            )
            .unwrap();
        let rows = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap();
        assert!(rows[0].response_requested);
        assert!(rows[0].allow_new_time_proposals);
    }

    #[test]
    fn replace_events_in_window_writes_calendar_client_fields() {
        let s = Store::open_in_memory().unwrap();
        let (from, to) = ("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z");
        let mut e = client_event("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z");
        let attendee = NewAttendee {
            name: "Room".into(),
            addr: "room@x".into(),
            r#type: "resource".into(),
            response: "accepted".into(),
            response_time: "2026-07-18T08:01:02Z".into(),
        };
        s.replace_events_in_window(
            from,
            to,
            std::slice::from_ref(&e),
            &[("E1".into(), vec![attendee.clone()])],
        )
        .unwrap();
        let rows = s.events_in_window(from, to).unwrap();
        assert_eq!(rows.len(), 1);
        assert_client_fields(&rows[0], &e);
        let atts = s.event_attendees("E1").unwrap();
        assert_eq!(atts[0].r#type, "resource");
        assert_eq!(atts[0].response, "accepted");
        assert_eq!(atts[0].response_time, "2026-07-18T08:01:02Z");

        // A later refresh that changes every field overwrites them.
        e = NewEvent {
            show_as: "tentative".into(),
            is_online_meeting: false,
            online_meeting_provider: "unknown".into(),
            join_url: "".into(),
            categories: vec![],
            sensitivity: "normal".into(),
            importance: "low".into(),
            event_type: "singleInstance".into(),
            recurrence: None,
            is_cancelled: false,
            is_organizer: false,
            response_requested: true,
            allow_new_time_proposals: true,
            has_attachments: false,
            original_start_time_zone: "UTC".into(),
            original_end_time_zone: "UTC".into(),
            calendar_id: "".into(),
            ..e
        };
        s.replace_events_in_window(
            from,
            to,
            std::slice::from_ref(&e),
            &[(
                "E1".into(),
                vec![NewAttendee {
                    response_time: "".into(),
                    ..attendee
                }],
            )],
        )
        .unwrap();
        let rows = s.events_in_window(from, to).unwrap();
        assert_eq!(rows.len(), 1);
        assert_client_fields(&rows[0], &e);
        assert_eq!(s.event_attendees("E1").unwrap()[0].response_time, "");
    }

    /// The `ON CONFLICT` path (a stored row outside the replaced window, and
    /// a second `upsert_event`) writes every #1983 column too.
    #[test]
    fn upsert_event_conflict_path_updates_calendar_client_fields() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_event(&ev("E1", "2026-08-01T09:00:00Z", "2026-08-01T10:00:00Z"))
            .unwrap();
        let moved = client_event("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z");
        s.replace_events_in_window(
            "2026-07-20T00:00:00Z",
            "2026-07-21T00:00:00Z",
            std::slice::from_ref(&moved),
            &[],
        )
        .unwrap();
        let all = ("2026-01-01T00:00:00Z", "2027-01-01T00:00:00Z");
        let rows = s.events_in_window(all.0, all.1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_client_fields(&rows[0], &moved);

        let again = NewEvent {
            show_as: "free".into(),
            categories: vec!["Blue category".into()],
            recurrence: Some(graph_recurrence("weekly", 9)),
            response_requested: true,
            calendar_id: "CAL2".into(),
            ..moved
        };
        s.upsert_event(&again).unwrap();
        let rows = s.events_in_window(all.0, all.1).unwrap();
        assert_client_fields(&rows[0], &again);
    }

    #[test]
    fn a_corrupt_graph_recurrence_reads_as_none() {
        let s = Store::open_in_memory().unwrap();
        let e = client_event("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z");
        s.upsert_event(&e).unwrap();
        s.conn
            .execute(
                "UPDATE events SET graph_recurrence = '{\"pattern\":' WHERE id = 'E1'",
                [],
            )
            .unwrap();
        let rows = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap();
        assert_eq!(rows[0].recurrence, None);
        assert_eq!(rows[0].show_as, "oof", "the rest of the row still reads");
    }

    #[test]
    fn create_local_event_reads_as_graph_will_create_it() {
        use crate::graph::model::{Recurrence, RecurrenceKind};
        let s = Store::open_in_memory().unwrap();
        let window = ("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z");
        let single = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        let rec = Recurrence {
            kind: RecurrenceKind::Weekly,
            interval: 1,
            days_of_week: vec!["monday".into()],
            day_of_month: 0,
            start_date: "2026-07-20".into(),
            until: Some("2026-08-31".into()),
        };
        let series = s
            .create_local_event(
                &LocalEventFields {
                    recurrence: Some(rec),
                    body_html: "<a href=\"https://zoom.us/j/7\">Join</a>".into(),
                    ..sample_fields()
                },
                "Me",
                "me@x",
            )
            .unwrap();
        let rows = s.events_in_window(window.0, window.1).unwrap();
        let row = |id: &str| rows.iter().find(|r| r.id == id).unwrap().clone();
        for r in [row(&single), row(&series)] {
            assert_eq!(r.show_as, "busy");
            assert_eq!(r.sensitivity, "normal");
            assert_eq!(r.importance, "normal");
            assert!(r.is_organizer);
            assert!(r.response_requested);
            assert!(r.allow_new_time_proposals);
            assert!(!r.is_cancelled && !r.is_online_meeting && !r.has_attachments);
        }
        assert_eq!(row(&single).event_type, "singleInstance");
        assert_eq!(row(&single).recurrence, None);
        assert_eq!(row(&single).join_url, "", "Room 1, no link in the body");
        let series = row(&series);
        assert_eq!(series.event_type, "seriesMaster");
        assert_eq!(series.join_url, "https://zoom.us/j/7");
        let r = series.recurrence.unwrap();
        assert_eq!(r.pattern.r#type, "weekly");
        assert_eq!(r.pattern.days_of_week, vec!["monday"]);
        assert_eq!(r.range.r#type, "endDate");
        assert_eq!(r.range.end_date, "2026-08-31");
        assert_eq!(s.event_attendees(&series.id).unwrap()[0].response_time, "");
    }

    #[test]
    fn update_event_fields_follows_the_join_link_unless_online_meeting() {
        let s = Store::open_in_memory().unwrap();
        let window = ("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z");
        let join = |s: &Store, id: &str| {
            s.events_in_window(window.0, window.1)
                .unwrap()
                .into_iter()
                .find(|r| r.id == id)
                .unwrap()
                .join_url
        };
        let fields = |location: &str| LocalEventFields {
            location: location.into(),
            ..sample_fields()
        };
        let id = s
            .create_local_event(&fields("https://zoom.us/j/1"), "Me", "me@x")
            .unwrap();
        assert_eq!(join(&s, &id), "https://zoom.us/j/1");
        s.update_event_fields(&id, &fields("https://zoom.us/j/2"))
            .unwrap();
        assert_eq!(join(&s, &id), "https://zoom.us/j/2");
        s.update_event_fields(&id, &fields("Room 2")).unwrap();
        assert_eq!(join(&s, &id), "", "no link left in location or body");
        s.update_event_fields(
            &id,
            &LocalEventFields {
                body_html: "<a href=\"https://meet.google.com/abc-defg-hij\">Join</a>".into(),
                ..fields("Room 2")
            },
        )
        .unwrap();
        assert_eq!(join(&s, &id), "https://meet.google.com/abc-defg-hij");

        // A synced online meeting keeps Graph's join link through an edit
        // that drops every link.
        let teams = "https://teams.microsoft.com/l/meetup-join/T";
        s.upsert_event(&NewEvent {
            is_online_meeting: true,
            join_url: teams.into(),
            ..ev("G1", "2026-07-20T13:00:00Z", "2026-07-20T14:00:00Z")
        })
        .unwrap();
        s.update_event_fields("G1", &fields("Room 3")).unwrap();
        assert_eq!(join(&s, "G1"), teams);
    }

    /// A synced event as a refresh stores it, with what Outlook set: an HTML
    /// body, `tentative` / `private`, categories, a 15-minute reminder, and
    /// an accepted optional attendee (#1994).
    fn synced_event(s: &Store) -> LocalEventFields {
        s.upsert_event(&NewEvent {
            subject: "Sync".into(),
            location: "Room 1".into(),
            body_html: "<p>agenda</p>".into(),
            show_as: "tentative".into(),
            sensitivity: "private".into(),
            importance: "high".into(),
            categories: vec!["Red".into()],
            reminder_minutes: 15,
            is_reminder_on: true,
            ..ev("G1", "2026-07-20T11:00:00Z", "2026-07-20T12:00:00Z")
        })
        .unwrap();
        s.put_event_attendees(
            "G1",
            &[NewAttendee {
                name: "Bob".into(),
                addr: "Bob@X.com".into(),
                r#type: "optional".into(),
                response: "accepted".into(),
                response_time: "2026-07-19T08:00:00Z".into(),
            }],
        )
        .unwrap();
        // What a host whose form shows only the core fields sends back
        // untouched: no type, no Outlook fields.
        LocalEventFields {
            subject: "Sync".into(),
            start_utc: "2026-07-20T11:00:00Z".into(),
            end_utc: "2026-07-20T12:00:00Z".into(),
            location: "Room 1".into(),
            body_html: "<p>agenda</p>".into(),
            attendees: vec![AttendeeInput {
                name: "Bob".into(),
                addr: "Bob@X.com".into(),
                r#type: String::new(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn update_event_fields_returns_only_changed_fields() {
        let s = Store::open_in_memory().unwrap();
        let untouched = synced_event(&s);
        assert_eq!(
            s.update_event_fields("G1", &untouched).unwrap(),
            EventFieldSet::default(),
            "an untouched save changes nothing"
        );
        let changed = s
            .update_event_fields(
                "G1",
                &LocalEventFields {
                    subject: "Sync v2".into(),
                    ..untouched.clone()
                },
            )
            .unwrap();
        assert_eq!(changed, EventFieldSet::SUBJECT);
        let sent = s.event_for_send("G1").unwrap().unwrap();
        assert_eq!(sent.subject, "Sync v2");
        assert_eq!(sent.body_html, "<p>agenda</p>");
        assert_eq!(sent.start_utc, "2026-07-20T11:00:00Z");
        // Each group on its own, each on a fresh copy of the synced event.
        let one = |f: LocalEventFields| {
            let s = Store::open_in_memory().unwrap();
            synced_event(&s);
            let changed = s.update_event_fields("G1", &f).unwrap();
            (changed, s.event_for_send("G1").unwrap().unwrap())
        };
        let base = || untouched.clone();
        assert_eq!(
            one(LocalEventFields {
                is_all_day: true,
                ..base()
            })
            .0,
            EventFieldSet::TIMES
        );
        assert_eq!(
            one(LocalEventFields {
                end_utc: "2026-07-20T13:00:00Z".into(),
                ..base()
            })
            .0,
            EventFieldSet::TIMES
        );
        assert_eq!(
            one(LocalEventFields {
                location: "Room 2".into(),
                ..base()
            })
            .0,
            EventFieldSet::LOCATION
        );
        assert_eq!(
            one(LocalEventFields {
                body_html: "x".into(),
                ..base()
            })
            .0,
            EventFieldSet::BODY
        );
        let (changed, sent) = one(LocalEventFields {
            show_as: Some("free".into()),
            sensitivity: Some("normal".into()),
            importance: Some("low".into()),
            categories: Some(vec![]),
            ..base()
        });
        assert_eq!(
            changed,
            EventFieldSet::SHOW_AS
                .union(EventFieldSet::SENSITIVITY)
                .union(EventFieldSet::IMPORTANCE)
                .union(EventFieldSet::CATEGORIES)
        );
        assert_eq!(sent.show_as, "free");
        assert_eq!(sent.sensitivity, "normal");
        assert_eq!(sent.importance, "low");
        assert!(sent.categories.is_empty());
        let (changed, sent) = one(LocalEventFields {
            reminder: Some(EventReminder {
                on: false,
                minutes_before_start: 15,
            }),
            ..base()
        });
        assert_eq!(changed, EventFieldSet::REMINDER);
        assert!(!sent.is_reminder_on);
        let (changed, sent) = one(LocalEventFields {
            is_online_meeting: Some(true),
            ..base()
        });
        assert_eq!(changed, EventFieldSet::ONLINE_MEETING);
        assert!(sent.is_online_meeting);
        let (changed, sent) = one(LocalEventFields {
            online_meeting_provider: Some("teamsForBusiness".into()),
            ..base()
        });
        assert_eq!(changed, EventFieldSet::ONLINE_MEETING);
        assert_eq!(sent.online_meeting_provider, "teamsForBusiness");
        let (changed, sent) = one(LocalEventFields {
            response_requested: Some(false),
            allow_new_time_proposals: Some(false),
            ..base()
        });
        assert_eq!(
            changed,
            EventFieldSet::RESPONSE_REQUESTED.union(EventFieldSet::ALLOW_NEW_TIME_PROPOSALS)
        );
        assert!(!sent.response_requested && !sent.allow_new_time_proposals);
        // A `Some` equal to the stored value is no change.
        let (changed, _) = one(LocalEventFields {
            show_as: Some("tentative".into()),
            categories: Some(vec!["Red".into()]),
            reminder: Some(EventReminder {
                on: true,
                minutes_before_start: 15,
            }),
            response_requested: Some(true),
            ..base()
        });
        assert!(changed.is_empty(), "{changed:?}");
    }

    #[test]
    fn update_event_fields_none_keeps_stored_values() {
        let s = Store::open_in_memory().unwrap();
        let untouched = synced_event(&s);
        s.update_event_fields(
            "G1",
            &LocalEventFields {
                subject: "Sync v2".into(),
                ..untouched
            },
        )
        .unwrap();
        let sent = s.event_for_send("G1").unwrap().unwrap();
        assert_eq!(sent.show_as, "tentative");
        assert_eq!(sent.sensitivity, "private");
        assert_eq!(sent.importance, "high");
        assert_eq!(sent.categories, vec!["Red".to_string()]);
        assert!(sent.is_reminder_on);
        assert_eq!(sent.reminder_minutes, 15);
        assert!(sent.response_requested && sent.allow_new_time_proposals);
    }

    #[test]
    fn update_event_fields_empty_enum_string_is_none() {
        let s = Store::open_in_memory().unwrap();
        let untouched = synced_event(&s);
        let changed = s
            .update_event_fields(
                "G1",
                &LocalEventFields {
                    show_as: Some(String::new()),
                    sensitivity: Some(String::new()),
                    importance: Some(String::new()),
                    online_meeting_provider: Some(String::new()),
                    ..untouched
                },
            )
            .unwrap();
        assert!(changed.is_empty(), "{changed:?}");
        let sent = s.event_for_send("G1").unwrap().unwrap();
        assert_eq!(sent.show_as, "tentative");
        assert_eq!(sent.sensitivity, "private");
        assert_eq!(sent.importance, "high");
    }

    #[test]
    fn update_event_fields_attendee_type_kept_when_unset() {
        let s = Store::open_in_memory().unwrap();
        let untouched = synced_event(&s);
        let with_ann = |ann_type: &str| LocalEventFields {
            attendees: vec![
                // Graph's address case differs from the form's: still the
                // same attendee.
                AttendeeInput {
                    name: "Bob".into(),
                    addr: "Bob@X.com".into(),
                    r#type: String::new(),
                },
                AttendeeInput {
                    name: "Ann".into(),
                    addr: "ann@x.com".into(),
                    r#type: ann_type.into(),
                },
            ],
            ..untouched.clone()
        };
        assert_eq!(
            s.update_event_fields("G1", &with_ann("")).unwrap(),
            EventFieldSet::ATTENDEES
        );
        let atts = s.event_attendees("G1").unwrap();
        assert_eq!(atts[0].r#type, "optional", "Bob keeps his stored type");
        assert_eq!(atts[1].r#type, "required", "a new attendee is required");
        // A type the host sets is a change, and is stored.
        assert_eq!(
            s.update_event_fields("G1", &with_ann("resource")).unwrap(),
            EventFieldSet::ATTENDEES
        );
        assert_eq!(s.event_attendees("G1").unwrap()[1].r#type, "resource");
        // And the same list again (Ann's type unset) changes nothing.
        assert!(
            s.update_event_fields("G1", &with_ann(""))
                .unwrap()
                .is_empty()
        );
        assert_eq!(s.event_attendees("G1").unwrap()[1].r#type, "resource");
    }

    #[test]
    fn update_event_fields_keeps_attendee_responses() {
        let s = Store::open_in_memory().unwrap();
        let untouched = synced_event(&s);
        let accepted = |s: &Store| {
            let a = s.event_attendees("G1").unwrap();
            (a[0].response.clone(), a[0].response_time.clone())
        };
        let want = ("accepted".to_string(), "2026-07-19T08:00:00Z".to_string());
        s.update_event_fields(
            "G1",
            &LocalEventFields {
                subject: "Sync v2".into(),
                ..untouched.clone()
            },
        )
        .unwrap();
        assert_eq!(accepted(&s), want, "a subject-only edit");
        let mut two = untouched.clone();
        two.attendees.insert(
            0,
            AttendeeInput {
                name: "Ann".into(),
                addr: "ann@x.com".into(),
                r#type: String::new(),
            },
        );
        s.update_event_fields("G1", &two).unwrap();
        let atts = s.event_attendees("G1").unwrap();
        assert_eq!(atts.len(), 2);
        assert_eq!(
            (atts[0].addr.as_str(), atts[0].response.as_str()),
            ("ann@x.com", "none")
        );
        assert_eq!(atts[1].response, "accepted", "Bob is still accepted");
        assert_eq!(atts[1].response_time, "2026-07-19T08:00:00Z");
    }

    #[test]
    fn update_event_fields_of_a_missing_event_is_an_error() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.update_event_fields("nope", &sample_fields()).is_err());
    }

    #[test]
    fn create_local_event_stores_given_fields() {
        let s = Store::open_in_memory().unwrap();
        let id = s
            .create_local_event(
                &LocalEventFields {
                    show_as: Some("oof".into()),
                    sensitivity: Some("confidential".into()),
                    importance: Some("low".into()),
                    categories: Some(vec!["Blue".into(), "Travel".into()]),
                    reminder: Some(EventReminder {
                        on: true,
                        minutes_before_start: 60,
                    }),
                    is_online_meeting: Some(true),
                    online_meeting_provider: Some("teamsForBusiness".into()),
                    response_requested: Some(false),
                    allow_new_time_proposals: Some(false),
                    attendees: vec![AttendeeInput {
                        name: "Room".into(),
                        addr: "room@x".into(),
                        r#type: "resource".into(),
                    }],
                    ..sample_fields()
                },
                "Me",
                "me@x",
            )
            .unwrap();
        let sent = s.event_for_send(&id).unwrap().unwrap();
        assert_eq!(sent.show_as, "oof");
        assert_eq!(sent.sensitivity, "confidential");
        assert_eq!(sent.importance, "low");
        assert_eq!(sent.categories, vec!["Blue".to_string(), "Travel".into()]);
        assert!(sent.is_reminder_on);
        assert_eq!(sent.reminder_minutes, 60);
        assert!(!sent.reminder_unset);
        assert!(sent.is_online_meeting);
        assert_eq!(sent.online_meeting_provider, "teamsForBusiness");
        assert!(!sent.response_requested && !sent.allow_new_time_proposals);
        assert_eq!(sent.attendees[0].r#type, "resource");
        assert_eq!(s.event_attendees(&id).unwrap()[0].response, "none");
    }

    #[test]
    fn create_local_event_without_reminder_marks_it_unset() {
        let s = Store::open_in_memory().unwrap();
        let id = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        let sent = s.event_for_send(&id).unwrap().unwrap();
        assert!(sent.reminder_unset);
        assert!(!sent.is_reminder_on);
        // An edit that leaves the reminder alone keeps it unset; one that
        // sets it clears the mark.
        s.update_event_fields(&id, &sample_fields()).unwrap();
        assert!(s.event_for_send(&id).unwrap().unwrap().reminder_unset);
        let changed = s
            .update_event_fields(
                &id,
                &LocalEventFields {
                    // The stored 0/false, set explicitly: still a change,
                    // since the create would otherwise send no reminder.
                    reminder: Some(EventReminder::default()),
                    ..sample_fields()
                },
            )
            .unwrap();
        assert_eq!(changed, EventFieldSet::REMINDER);
        assert!(!s.event_for_send(&id).unwrap().unwrap().reminder_unset);
    }

    #[test]
    fn reconcile_created_event_writes_back_reminder_and_join_url() {
        let s = Store::open_in_memory().unwrap();
        let id = s
            .create_local_event(
                &LocalEventFields {
                    is_online_meeting: Some(true),
                    ..sample_fields()
                },
                "Me",
                "me@x",
            )
            .unwrap();
        let teams = "https://teams.microsoft.com/l/meetup-join/X";
        s.reconcile_created_event(
            &id,
            &Event {
                id: "EV1".into(),
                is_reminder_on: true,
                reminder_minutes: 15,
                is_online_meeting: true,
                online_meeting_provider: "teamsForBusiness".into(),
                join_url: teams.into(),
                ..Event::default()
            },
        )
        .unwrap();
        let sent = s.event_for_send("EV1").unwrap().unwrap();
        assert!(sent.is_reminder_on);
        assert_eq!(sent.reminder_minutes, 15);
        assert!(!sent.reminder_unset, "a Graph-id row never keeps the mark");
        assert_eq!(sent.online_meeting_provider, "teamsForBusiness");
        let row = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap()
            .into_iter()
            .find(|r| r.id == "EV1")
            .unwrap();
        assert_eq!(row.join_url, teams);

        // An answer without a join link or provider leaves the stored ones.
        s.set_event_online_meeting("EV1", "", "").unwrap();
        let row = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap()
            .into_iter()
            .find(|r| r.id == "EV1")
            .unwrap();
        assert_eq!(row.join_url, teams);
        assert_eq!(row.online_meeting_provider, "teamsForBusiness");
    }

    /// A database whose `events` / `event_attendees` tables are at their
    /// last pre-#1983 shape gains the new columns in place: its rows stay,
    /// read the new fields as their defaults, and a second open is fine.
    #[test]
    fn events_without_1983_columns_migrate() {
        let dir = std::env::temp_dir().join(format!(
            "mailcore-events-1983-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE events (
                     id TEXT PRIMARY KEY, subject TEXT NOT NULL DEFAULT '',
                     start_utc TEXT NOT NULL DEFAULT '', end_utc TEXT NOT NULL DEFAULT '',
                     is_all_day INTEGER NOT NULL DEFAULT 0, location TEXT NOT NULL DEFAULT '',
                     organizer_name TEXT NOT NULL DEFAULT '',
                     organizer_addr TEXT NOT NULL DEFAULT '',
                     response_status TEXT NOT NULL DEFAULT '', series_master_id TEXT,
                     body_preview TEXT NOT NULL DEFAULT '', web_link TEXT NOT NULL DEFAULT '',
                     last_modified TEXT NOT NULL DEFAULT '', body_html TEXT NOT NULL DEFAULT '',
                     recurrence TEXT NOT NULL DEFAULT '',
                     reminder_minutes INTEGER NOT NULL DEFAULT 0,
                     is_reminder_on INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE event_attendees (
                     event_id TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
                     name TEXT NOT NULL DEFAULT '', addr TEXT NOT NULL DEFAULT '',
                     type TEXT NOT NULL DEFAULT '', response TEXT NOT NULL DEFAULT '');
                 INSERT INTO events (id, subject, start_utc, end_utc, reminder_minutes,
                                     is_reminder_on)
                 VALUES ('E1', 'Old', '2026-07-20T09:00:00Z', '2026-07-20T10:00:00Z', 15, 1);
                 INSERT INTO event_attendees VALUES ('E1', 'Bob', 'bob@x', 'optional',
                                                     'accepted');",
            )
            .unwrap();
        }
        for _ in 0..2 {
            let s = Store::open(&path).unwrap();
            let rows = s
                .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
                .unwrap();
            assert_eq!(rows.len(), 1);
            let r = &rows[0];
            assert_eq!(r.subject, "Old");
            assert_eq!(r.reminder_minutes, 15);
            assert!(r.is_reminder_on);
            let expected = EventRow {
                id: r.id.clone(),
                subject: r.subject.clone(),
                start_utc: r.start_utc.clone(),
                end_utc: r.end_utc.clone(),
                reminder_minutes: 15,
                is_reminder_on: true,
                ..EventRow::default()
            };
            assert_eq!(r, &expected, "the new fields read as their defaults");
            let atts = s.event_attendees("E1").unwrap();
            assert_eq!(atts.len(), 1);
            assert_eq!(atts[0].name, "Bob");
            assert_eq!(atts[0].r#type, "optional");
            assert_eq!(atts[0].response_time, "");
            // `reminder_unset` (#1994) reads 0: the old row's reminder
            // came from Graph.
            assert!(!s.event_for_send("E1").unwrap().unwrap().reminder_unset);
        }
        // The migrated table takes every new column, `reminder_unset` too.
        let s = Store::open(&path).unwrap();
        let local = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        assert!(s.event_for_send(&local).unwrap().unwrap().reminder_unset);
        s.delete_event(&local).unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        let e = client_event("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z");
        s.upsert_event(&e).unwrap();
        let rows = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap();
        assert_client_fields(&rows[0], &e);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
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
            OutboxOp::UpdateEvent {
                id: "EV1".into(),
                fields: EventFieldSet::SUBJECT.union(EventFieldSet::REMINDER),
            },
            OutboxOp::DeleteEvent { id: "EV2".into() },
        ] {
            assert_eq!(OutboxOp::from_json(&op.to_json()), Some(op));
        }
    }

    #[test]
    fn update_event_op_round_trips_fields() {
        let op = OutboxOp::UpdateEvent {
            id: "EV1".into(),
            fields: EventFieldSet::SUBJECT.union(EventFieldSet::ONLINE_MEETING),
        };
        let v = op.to_json();
        assert_eq!(
            v.to_string(),
            r#"{"kind":"updateEvent","id":"EV1","fields":["subject","onlineMeeting"]}"#
        );
        assert_eq!(OutboxOp::from_json(&v), Some(op));
        // An empty set round-trips as empty, not as the legacy full set.
        let empty = OutboxOp::UpdateEvent {
            id: "EV1".into(),
            fields: EventFieldSet::default(),
        };
        assert_eq!(OutboxOp::from_json(&empty.to_json()), Some(empty));
        // An unknown name is dropped.
        let v = json::parse(r#"{"kind":"updateEvent","id":"EV1","fields":["subject","later"]}"#)
            .unwrap();
        assert_eq!(
            OutboxOp::from_json(&v),
            Some(OutboxOp::UpdateEvent {
                id: "EV1".into(),
                fields: EventFieldSet::SUBJECT,
            })
        );
    }

    /// An `UpdateEvent` queued before #1994 has no `fields`: it still sends
    /// what an update sent then.
    #[test]
    fn legacy_update_event_op_reads_as_full_set() {
        let v = json::parse(r#"{"kind":"updateEvent","id":"X"}"#).unwrap();
        let Some(OutboxOp::UpdateEvent { id, fields }) = OutboxOp::from_json(&v) else {
            panic!("not an UpdateEvent");
        };
        assert_eq!(id, "X");
        assert_eq!(fields, EventFieldSet::LEGACY_UPDATE);
        assert_eq!(
            fields.to_names(),
            [
                "subject",
                "times",
                "location",
                "attendees",
                "body",
                "recurrence"
            ]
        );
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
    fn the_uploaded_flag_survives_reconcile_and_a_second_add() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Hi", "b@x", "", "body").unwrap();
        s.add_outbound_attachment(&id, "/tmp/a.pdf", "a.pdf", 10)
            .unwrap();
        s.add_outbound_attachment(&id, "/tmp/b.pdf", "b.pdf", 10)
            .unwrap();
        assert!(!s.outbound_attachments(&id).unwrap()[0].uploaded);
        s.reconcile_id(&id, "GRAPH-1").unwrap();
        // Marked by the reconciled local id, as `SendDraft` may hold it.
        s.mark_outbound_attachment_uploaded(&id, "/tmp/a.pdf")
            .unwrap();
        s.add_outbound_attachment(&id, "/tmp/a.pdf", "a.pdf", 10)
            .unwrap();
        let got = s.outbound_attachments("GRAPH-1").unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].uploaded, "the same path again keeps the flag");
        assert!(!got[1].uploaded, "only the one uploaded is marked");
    }

    #[test]
    fn send_posted_is_set_and_cleared_per_op() {
        let s = Store::open_in_memory().unwrap();
        let a = s
            .enqueue_op(&OutboxOp::SendDraft { id: "G1".into() })
            .unwrap();
        let b = s
            .enqueue_op(&OutboxOp::SendDraft { id: "G2".into() })
            .unwrap();
        assert!(!s.send_posted(a).unwrap());
        s.set_send_posted(a, true).unwrap();
        assert!(s.send_posted(a).unwrap());
        assert!(!s.send_posted(b).unwrap(), "only that op");
        s.set_send_posted(a, false).unwrap();
        assert!(!s.send_posted(a).unwrap());
        assert!(!s.send_posted(9999).unwrap(), "a missing op is not posted");
    }

    #[test]
    fn discard_keeps_only_the_uploaded_attachments() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Hi", "b@x", "", "body").unwrap();
        s.add_outbound_attachment(&id, "/tmp/up.pdf", "up.pdf", 1)
            .unwrap();
        s.add_outbound_attachment(&id, "/tmp/new.pdf", "new.pdf", 1)
            .unwrap();
        s.reconcile_id(&id, "G").unwrap();
        s.mark_outbound_attachment_uploaded(&id, "/tmp/up.pdf")
            .unwrap();
        s.discard_pending_outbound_attachments(&id).unwrap();
        let left = s.outbound_attachments("G").unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "up.pdf");
        s.clear_outbound_attachments(&id).unwrap();
        assert!(s.outbound_attachments("G").unwrap().is_empty());
    }

    #[test]
    fn init_adds_the_uploaded_and_send_posted_columns_to_old_tables() {
        let dir = std::env::temp_dir().join(format!(
            "mailcore-uploaded-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE outbound_attachments (
                     draft_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                     size INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (draft_id, path));
                 INSERT INTO outbound_attachments VALUES ('local:d', '/tmp/a.pdf', 'a.pdf', 1);
                 CREATE TABLE outbox (
                     seq INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT NOT NULL, message_id TEXT,
                     payload TEXT NOT NULL DEFAULT '', attempts INTEGER NOT NULL DEFAULT 0,
                     last_error TEXT);",
            )
            .unwrap();
            // Rows as an older engine's `enqueue_op` wrote them.
            for (seq, op) in [
                (7, OutboxOp::SendDraft { id: "G".into() }),
                (
                    8,
                    OutboxOp::MarkRead {
                        id: "M1".into(),
                        read: true,
                    },
                ),
            ] {
                conn.execute(
                    "INSERT INTO outbox (seq, op, payload) VALUES (?1, ?2, ?3)",
                    params![seq, op.kind(), op.to_json().to_string()],
                )
                .unwrap();
            }
        }
        let s = Store::open(&path).unwrap();
        let got = s.outbound_attachments("local:d").unwrap();
        assert_eq!(got.len(), 1);
        assert!(!got[0].uploaded, "an old row is not on any Graph draft");
        assert!(
            s.send_posted(7).unwrap(),
            "an inherited send may follow a lost answer: it reads as posted"
        );
        assert!(!s.send_posted(8).unwrap(), "another op kind does not");
        assert_eq!(s.pending_ops().unwrap().len(), 2);
        // A reopen, with the column already there, leaves the flags alone.
        s.set_send_posted(7, false).unwrap();
        let fresh = s
            .enqueue_op(&OutboxOp::SendDraft { id: "G2".into() })
            .unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert!(!s.send_posted(7).unwrap());
        assert!(!s.send_posted(fresh).unwrap());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn return_to_drafts_undoes_a_send_by_either_id() {
        let s = Store::open_in_memory().unwrap();
        for (id, wk) in [("DRAFTS1", "drafts"), ("SENT1", "sentitems")] {
            s.upsert_folder(&MailFolder {
                id: id.into(),
                display_name: id.into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some(wk.into()),
            })
            .unwrap();
        }
        let local = s.create_local_draft("Hi", "b@x", "", "body").unwrap();
        s.reconcile_id(&local, "G").unwrap();
        for id in [local.as_str(), "G"] {
            s.mark_sent(id);
            s.move_message(id, "SENT1").unwrap();
            s.return_to_drafts(id, "DRAFTS1").unwrap();
            let (row, body) = s.draft("G").unwrap().unwrap();
            assert_eq!(row.folder_id, "DRAFTS1", "by {id}");
            assert!(row.is_draft, "by {id}");
            assert_eq!(row.subject, "Hi");
            assert_eq!(row.to_recipients, "b@x");
            assert_eq!(body.content, "body");
        }
        // Sent Items never synced: the row never left Drafts, only
        // `is_draft` flips back.
        let other = s.create_local_draft("Two", "c@x", "", "b2").unwrap();
        s.mark_sent(&other);
        s.return_to_drafts(&other, "DRAFTS1").unwrap();
        let (row, _) = s.draft(&other).unwrap().unwrap();
        assert_eq!(row.folder_id, "DRAFTS1");
        assert!(row.is_draft);
        // A missing row is no error.
        s.return_to_drafts("local:gone", "DRAFTS1").unwrap();
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
    fn inline_and_bytes_outbound_attachments_round_trip() {
        // #1949: a plain file row, an inline file row and a bytes row.
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Hi", "b@x", "", "<p>x</p>").unwrap();
        s.add_outbound_attachment(&id, "/tmp/a.pdf", "a.pdf", 3)
            .unwrap();
        s.add_inline_outbound_attachment_file(&id, "/tmp/logo.png", "logo.png", 7, None, "logo")
            .unwrap();
        let key = s
            .add_outbound_attachment_bytes(&id, "pic.png", "image/png", b"\x89PNG", "pic1")
            .unwrap();
        assert_eq!(key, "cid:pic1");
        let got = s.outbound_attachments(&id).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(
            (&got[0].content_id, &got[0].content_type, got[0].from_bytes),
            (&None, &None, false),
            "a plain file row"
        );
        assert_eq!(got[1].path, "/tmp/logo.png");
        assert_eq!(got[1].content_id.as_deref(), Some("logo"));
        assert_eq!(got[1].content_type, None);
        assert!(!got[1].from_bytes);
        assert_eq!(got[2].path, "cid:pic1");
        assert_eq!(got[2].name, "pic.png");
        assert_eq!(got[2].size, 4, "a bytes row's size is its length");
        assert_eq!(got[2].content_id.as_deref(), Some("pic1"));
        assert_eq!(got[2].content_type.as_deref(), Some("image/png"));
        assert!(got[2].from_bytes);
        assert!(!got[2].uploaded);
        assert_eq!(
            s.outbound_attachment_bytes(&id, "cid:pic1").unwrap(),
            Some(b"\x89PNG".to_vec())
        );
        assert_eq!(
            s.outbound_attachment_bytes(&id, "/tmp/logo.png").unwrap(),
            None
        );
        assert_eq!(s.outbound_attachment_bytes(&id, "cid:nope").unwrap(), None);
        // An inline file row with a given type keeps it.
        s.add_inline_outbound_attachment_file(&id, "/tmp/x", "x", 1, Some("image/gif"), "x1")
            .unwrap();
        assert_eq!(
            s.outbound_attachments(&id).unwrap()[3]
                .content_type
                .as_deref(),
            Some("image/gif")
        );
        // The same file inline under a second content id is ignored.
        s.add_inline_outbound_attachment_file(&id, "/tmp/logo.png", "logo.png", 7, None, "other")
            .unwrap();
        let got = s.outbound_attachments(&id).unwrap();
        assert_eq!(got.len(), 4);
        assert_eq!(got[1].content_id.as_deref(), Some("logo"));
        // A bytes row is removed by its key.
        s.remove_outbound_attachment(&id, &key).unwrap();
        assert!(s.outbound_attachment_bytes(&id, &key).unwrap().is_none());
        assert_eq!(s.outbound_attachments(&id).unwrap().len(), 3);
    }

    #[test]
    fn an_inline_add_makes_a_pending_plain_file_inline_in_place() {
        // #1949 r1: the same path attached plain, then inline: one row, same
        // position, now inline (else `cid:logo` would find nothing).
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Hi", "b@x", "", "<p>x</p>").unwrap();
        s.add_outbound_attachment(&id, "/tmp/logo.png", "logo.png", 7)
            .unwrap();
        s.add_outbound_attachment(&id, "/tmp/z.pdf", "z.pdf", 1)
            .unwrap();
        s.add_inline_outbound_attachment_file(
            &id,
            "/tmp/logo.png",
            "logo.png",
            7,
            Some("image/x-logo"),
            "logo",
        )
        .unwrap();
        let got = s.outbound_attachments(&id).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path, "/tmp/logo.png", "same attach position");
        assert_eq!(got[0].content_id.as_deref(), Some("logo"));
        assert_eq!(got[0].content_type.as_deref(), Some("image/x-logo"));
        // Now inline: a second content id is ignored, and so is a plain add.
        s.add_inline_outbound_attachment_file(&id, "/tmp/logo.png", "logo.png", 7, None, "other")
            .unwrap();
        s.add_outbound_attachment(&id, "/tmp/logo.png", "logo.png", 7)
            .unwrap();
        let got = s.outbound_attachments(&id).unwrap();
        assert_eq!(got[0].content_id.as_deref(), Some("logo"));
        assert_eq!(got[0].content_type.as_deref(), Some("image/x-logo"));
        // An uploaded plain row stays plain: Graph has it non-inline.
        s.mark_outbound_attachment_uploaded(&id, "/tmp/z.pdf")
            .unwrap();
        s.add_inline_outbound_attachment_file(&id, "/tmp/z.pdf", "z.pdf", 1, None, "z")
            .unwrap();
        let got = s.outbound_attachments(&id).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].content_id, None);
        assert!(got[1].uploaded);
    }

    #[test]
    fn re_adding_a_content_id_replaces_pending_bytes_in_place() {
        // #1949: a re-pasted picture under the same content id sends the new
        // bytes, keeps its attach position, and never touches an uploaded row.
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Hi", "b@x", "", "<p>x</p>").unwrap();
        s.add_outbound_attachment_bytes(&id, "a.png", "image/png", b"AAAA", "pic1")
            .unwrap();
        s.add_outbound_attachment(&id, "/tmp/z.pdf", "z.pdf", 1)
            .unwrap();
        s.add_outbound_attachment_bytes(&id, "b.jpg", "image/jpeg", b"BB", "pic1")
            .unwrap();
        let got = s.outbound_attachments(&id).unwrap();
        assert_eq!(got.len(), 2, "one row per content id");
        assert_eq!(got[0].path, "cid:pic1", "same attach position");
        assert_eq!(got[0].name, "b.jpg");
        assert_eq!(got[0].size, 2);
        assert_eq!(got[0].content_type.as_deref(), Some("image/jpeg"));
        assert_eq!(
            s.outbound_attachment_bytes(&id, "cid:pic1").unwrap(),
            Some(b"BB".to_vec())
        );
        s.mark_outbound_attachment_uploaded(&id, "cid:pic1")
            .unwrap();
        s.add_outbound_attachment_bytes(&id, "c.gif", "image/gif", b"CCCCCC", "pic1")
            .unwrap();
        let got = s.outbound_attachments(&id).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].uploaded);
        assert_eq!(got[0].name, "b.jpg", "an uploaded row is left alone");
        assert_eq!(
            s.outbound_attachment_bytes(&id, "cid:pic1").unwrap(),
            Some(b"BB".to_vec())
        );
    }

    #[test]
    fn reconcile_id_carries_inline_bytes_rows() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Sub", "", "", "body").unwrap();
        s.add_outbound_attachment_bytes(&id, "p.png", "image/png", b"PNG", "pic1")
            .unwrap();
        s.add_inline_outbound_attachment_file(&id, "/tmp/l.png", "l.png", 1, None, "logo")
            .unwrap();
        s.reconcile_id(&id, "GRAPH-1").unwrap();
        for draft in ["GRAPH-1", id.as_str()] {
            let got = s.outbound_attachments(draft).unwrap();
            assert_eq!(got.len(), 2, "{draft}");
            assert_eq!(got[0].content_id.as_deref(), Some("pic1"));
            assert!(got[0].from_bytes);
            assert_eq!(got[1].content_id.as_deref(), Some("logo"));
            assert_eq!(
                s.outbound_attachment_bytes(draft, "cid:pic1").unwrap(),
                Some(b"PNG".to_vec()),
                "{draft}"
            );
        }
        // Added by the reconciled local id, it lands on the Graph id.
        s.add_outbound_attachment_bytes(&id, "q.png", "image/png", b"Q", "pic2")
            .unwrap();
        assert_eq!(s.outbound_attachments("GRAPH-1").unwrap().len(), 3);
    }

    #[test]
    fn discard_pending_removes_pending_inline_rows() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Hi", "b@x", "", "body").unwrap();
        s.add_outbound_attachment_bytes(&id, "up.png", "image/png", b"U", "up")
            .unwrap();
        s.add_outbound_attachment_bytes(&id, "new.png", "image/png", b"N", "new")
            .unwrap();
        s.add_inline_outbound_attachment_file(&id, "/tmp/f.png", "f.png", 1, None, "f")
            .unwrap();
        s.reconcile_id(&id, "G").unwrap();
        s.mark_outbound_attachment_uploaded(&id, "cid:up").unwrap();
        s.discard_pending_outbound_attachments(&id).unwrap();
        let left = s.outbound_attachments("G").unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].path, "cid:up");
        assert_eq!(
            s.outbound_attachment_bytes("G", "cid:up").unwrap(),
            Some(b"U".to_vec())
        );
        s.clear_outbound_attachments(&id).unwrap();
        assert!(s.outbound_attachments("G").unwrap().is_empty());
    }

    #[test]
    fn outbound_attachments_without_the_1949_columns_migrate() {
        let dir = std::env::temp_dir().join(format!(
            "mailcore-1949-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE outbound_attachments (
                     draft_id TEXT NOT NULL, path TEXT NOT NULL, name TEXT NOT NULL,
                     size INTEGER NOT NULL DEFAULT 0, uploaded INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (draft_id, path));
                 INSERT INTO outbound_attachments VALUES ('G', '/tmp/a.pdf', 'a.pdf', 1, 1);",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        let got = s.outbound_attachments("G").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content_id, None);
        assert_eq!(got[0].content_type, None);
        assert!(!got[0].from_bytes);
        assert!(got[0].uploaded, "the old flag is kept");
        // The new columns are usable on the migrated table.
        s.add_outbound_attachment_bytes("G", "p.png", "image/png", b"P", "pic1")
            .unwrap();
        assert_eq!(
            s.outbound_attachment_bytes("G", "cid:pic1").unwrap(),
            Some(b"P".to_vec())
        );
        drop(s);
        // A reopen (columns already there) changes nothing.
        let s = Store::open(&path).unwrap();
        assert_eq!(s.outbound_attachments("G").unwrap().len(), 2);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
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

#[cfg(test)]
mod body_format_tests {
    use super::*;

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

    /// A store holding one Inbox message `M`, not a draft, for bodies to
    /// hang off.
    fn store_with_message() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&MailFolder {
            id: "INBOX".into(),
            display_name: "Inbox".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: Some("inbox".into()),
        })
        .unwrap();
        s.upsert_message("INBOX", &message("M", false)).unwrap();
        s
    }

    fn message(id: &str, is_draft: bool) -> Message {
        Message {
            id: id.into(),
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
            is_draft,
            is_meeting_request: false,
            categories: Vec::new(),
        }
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
        // Recorded as a text fetch, the strict rule holds even for a draft.
        s.conn
            .execute("UPDATE messages SET is_draft = 1 WHERE id = 'M'", [])
            .unwrap();
        assert_eq!(satisfies(&s, "M"), (true, false));
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
            s.upsert_folder(&MailFolder {
                id: "INBOX".into(),
                display_name: "Inbox".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("inbox".into()),
            })
            .unwrap();
            s.upsert_message("INBOX", &message("M", false)).unwrap();
            s.put_body("M", &text_body("old")).unwrap();
            // Take the database back to before the column existed.
            s.conn
                .execute_batch("ALTER TABLE bodies DROP COLUMN fetched_as;")
                .unwrap();
            "M".to_string()
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

    /// The Drafts folder as Graph syncs it.
    fn synced_drafts(s: &Store) {
        s.upsert_folder(&MailFolder {
            id: "DRAFTS".into(),
            display_name: "Drafts".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: Some("drafts".into()),
        })
        .unwrap();
    }

    #[test]
    fn a_legacy_edited_draft_is_never_a_refetch_candidate() {
        // An older build's `update_draft_fields` kept `content_type = 'text'`
        // under the html it wrote: a FetchBody over it would lose the edit.
        let s = store_with_message();
        synced_drafts(&s);
        s.upsert_message("DRAFTS", &message("D", true)).unwrap();
        s.conn
            .execute(
                "INSERT INTO bodies (message_id, content_type, content)
                 VALUES ('D', 'text', '<p>unsynced edit</p>')",
                [],
            )
            .unwrap();
        assert_eq!(satisfies(&s, "D"), (true, true));
        // A complete delta item writes `is_draft = 0` (`isDraft` is not
        // selected): the Drafts folder still says it is a draft.
        s.upsert_message("DRAFTS", &message("D", false)).unwrap();
        assert_eq!(satisfies(&s, "D"), (true, true));
        // A message elsewhere with the same body still upgrades.
        s.put_body("M", &text_body("old")).unwrap();
        assert_eq!(satisfies(&s, "M"), (true, false));
    }

    #[test]
    fn a_draft_under_the_local_sentinel_is_a_draft() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("S", "", "", "").unwrap();
        s.conn
            .execute(
                "UPDATE messages SET is_draft = 0 WHERE id = ?1",
                params![id],
            )
            .unwrap();
        s.put_body(&id, &text_body("t")).unwrap();
        assert_eq!(satisfies(&s, &id), (true, true));
    }

    /// A store with an Inbox and a Drafts folder, and `rows` (id, folder,
    /// received, is_draft) in them.
    fn store_with_rows(rows: &[(&str, &str, &str, bool)]) -> Store {
        let s = Store::open_in_memory().unwrap();
        for (id, name, wk) in [("INBOX", "Inbox", "inbox"), ("DRAFTS", "Drafts", "drafts")] {
            s.upsert_folder(&MailFolder {
                id: id.into(),
                display_name: name.into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some(wk.into()),
            })
            .unwrap();
        }
        for (id, folder, received, draft) in rows {
            let m = Message {
                received: (*received).into(),
                ..message(id, *draft)
            };
            s.upsert_message(folder, &m).unwrap();
        }
        s
    }

    const SINCE: &str = "2026-07-01T00:00:00Z";

    #[test]
    fn body_candidates_are_the_window_newest_first_without_a_satisfying_body() {
        let s = store_with_rows(&[
            ("OLD", "INBOX", "2026-06-30T23:59:59Z", false),
            ("A", "INBOX", "2026-07-02T00:00:00Z", false),
            ("B", "INBOX", "2026-07-03T00:00:00Z", false),
            ("C", "INBOX", "2026-07-01T00:00:00Z", false),
            ("HEADERLESS", "INBOX", "", false),
        ]);
        let all = s
            .body_candidates(BodyFormat::Text, SINCE, 10, &HashSet::new())
            .unwrap();
        assert_eq!(all, vec!["B", "A", "C"]);
        // The limit takes the newest.
        let two = s
            .body_candidates(BodyFormat::Text, SINCE, 2, &HashSet::new())
            .unwrap();
        assert_eq!(two, vec!["B", "A"]);
        assert!(
            s.body_candidates(BodyFormat::Text, SINCE, 0, &HashSet::new())
                .unwrap()
                .is_empty()
        );
        // Skipped ids are left out, and the limit counts what is kept.
        let skip = HashSet::from(["B".to_string()]);
        let kept = s
            .body_candidates(BodyFormat::Text, SINCE, 2, &skip)
            .unwrap();
        assert_eq!(kept, vec!["A", "C"]);
        for id in ["B", "A", "C"] {
            assert!(s.needs_body(id, BodyFormat::Text, SINCE).unwrap(), "{id}");
        }
        for id in ["OLD", "HEADERLESS", "missing"] {
            assert!(!s.needs_body(id, BodyFormat::Text, SINCE).unwrap(), "{id}");
        }
    }

    #[test]
    fn body_candidates_leave_drafts_out() {
        let s = store_with_rows(&[
            ("FLAGGED", "INBOX", "2026-07-02T00:00:00Z", true),
            ("FILED", "DRAFTS", "2026-07-03T00:00:00Z", false),
            ("MAIL", "INBOX", "2026-07-04T00:00:00Z", false),
        ]);
        let local = s.create_local_draft("S", "", "", "<p>hi</p>").unwrap();
        s.conn
            .execute(
                "UPDATE messages SET received_at = '2026-07-05T00:00:00Z' WHERE id = ?1",
                params![local],
            )
            .unwrap();
        // The local draft has a body; drop it so only the folder rule keeps
        // it out.
        s.conn
            .execute("DELETE FROM bodies WHERE message_id = ?1", params![local])
            .unwrap();
        let all = s
            .body_candidates(BodyFormat::Html, SINCE, 10, &HashSet::new())
            .unwrap();
        assert_eq!(all, vec!["MAIL"]);
        for id in ["FLAGGED", "FILED", local.as_str()] {
            assert!(!s.needs_body(id, BodyFormat::Html, SINCE).unwrap(), "{id}");
        }
    }

    #[test]
    fn body_candidates_follow_get_body_for() {
        // Every body case of the `get_body_for` tests: a candidate exactly
        // when `get_body_for` has nothing for the format.
        let s = store_with_rows(&[
            ("NONE", "INBOX", "2026-07-02T00:00:00Z", false),
            ("TEXT", "INBOX", "2026-07-03T00:00:00Z", false),
            ("HTML", "INBOX", "2026-07-04T00:00:00Z", false),
            ("NATIVE", "INBOX", "2026-07-05T00:00:00Z", false),
            ("OLDTEXT", "INBOX", "2026-07-06T00:00:00Z", false),
            ("LOCALHTML", "INBOX", "2026-07-07T00:00:00Z", false),
            ("RTF", "INBOX", "2026-07-08T00:00:00Z", false),
        ]);
        s.put_fetched_body("TEXT", &text_body("t"), BodyFormat::Text)
            .unwrap();
        s.put_fetched_body("HTML", &html_body("<p>h</p>"), BodyFormat::Html)
            .unwrap();
        s.put_fetched_body("NATIVE", &text_body("plain"), BodyFormat::Html)
            .unwrap();
        s.put_body("OLDTEXT", &text_body("old")).unwrap();
        s.put_body("LOCALHTML", &html_body("<p>l</p>")).unwrap();
        s.put_fetched_body("RTF", &html_body("<p>r</p>"), BodyFormat::Html)
            .unwrap();
        s.conn
            .execute(
                "UPDATE bodies SET fetched_as = 'rtf' WHERE message_id = 'RTF'",
                [],
            )
            .unwrap();
        let ids = [
            "NONE",
            "TEXT",
            "HTML",
            "NATIVE",
            "OLDTEXT",
            "LOCALHTML",
            "RTF",
        ];
        for format in [BodyFormat::Text, BodyFormat::Html] {
            let candidates = s
                .body_candidates(format, SINCE, 100, &HashSet::new())
                .unwrap();
            for id in ids {
                let lacking = s.get_body_for(id, format).unwrap().is_none();
                assert_eq!(
                    candidates.contains(&id.to_string()),
                    lacking,
                    "{id} {format:?}"
                );
                assert_eq!(
                    s.needs_body(id, format, SINCE).unwrap(),
                    lacking,
                    "{id} {format:?}"
                );
            }
        }
        // The cases themselves, so the rule cannot drift unnoticed.
        assert_eq!(
            s.body_candidates(BodyFormat::Text, SINCE, 100, &HashSet::new())
                .unwrap(),
            vec!["RTF", "NATIVE", "HTML", "NONE"]
        );
        assert_eq!(
            s.body_candidates(BodyFormat::Html, SINCE, 100, &HashSet::new())
                .unwrap(),
            vec!["RTF", "OLDTEXT", "TEXT", "NONE"]
        );
    }

    // --- Exchange headers with a fetched body (#1933) --------------------

    const AUTH_AS: &str = "X-MS-Exchange-Organization-AuthAs";
    const DIRECTIONALITY: &str = "X-MS-Exchange-Organization-MessageDirectionality";

    fn header(name: &str, value: &str) -> MessageHeader {
        MessageHeader {
            name: name.into(),
            value: value.into(),
        }
    }

    fn headers_rows(s: &Store) -> i64 {
        s.conn
            .query_row("SELECT COUNT(*) FROM message_headers", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn message_header_stored_with_fetched_body() {
        let s = store_with_message();
        s.put_fetched_body_with_headers(
            "M",
            &text_body("hi"),
            BodyFormat::Text,
            &[
                header(AUTH_AS, " Internal "),
                header(DIRECTIONALITY, "Originating"),
                // Not one the store keeps, whatever a caller passes.
                header("Received", "from x"),
            ],
        )
        .unwrap();
        assert_eq!(s.get_body("M").unwrap().unwrap().content, "hi");
        assert!(s.get_body_for("M", BodyFormat::Text).unwrap().is_some());
        assert_eq!(
            s.message_header("M", AUTH_AS).unwrap().as_deref(),
            Some("Internal")
        );
        assert_eq!(
            s.message_header("M", DIRECTIONALITY).unwrap().as_deref(),
            Some("Originating")
        );
        assert_eq!(s.message_header("M", "Received").unwrap(), None);
        assert_eq!(headers_rows(&s), 2);
    }

    #[test]
    fn message_header_absent_is_none() {
        let s = store_with_message();
        // No body stored at all, and no such message.
        assert_eq!(s.message_header("M", AUTH_AS).unwrap(), None);
        assert_eq!(s.message_header("nope", AUTH_AS).unwrap(), None);
        // A body fetched without the header.
        s.put_fetched_body_with_headers(
            "M",
            &text_body("hi"),
            BodyFormat::Text,
            &[header(DIRECTIONALITY, "Incoming")],
        )
        .unwrap();
        assert_eq!(s.message_header("M", AUTH_AS).unwrap(), None);
        // An empty value is a value (as Graph sent it, trimmed).
        s.put_fetched_body_with_headers(
            "M",
            &text_body("hi"),
            BodyFormat::Text,
            &[header(AUTH_AS, "  ")],
        )
        .unwrap();
        assert_eq!(s.message_header("M", AUTH_AS).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn message_header_name_ignores_case() {
        let s = store_with_message();
        s.put_fetched_body_with_headers(
            "M",
            &text_body("hi"),
            BodyFormat::Text,
            &[
                header("x-ms-exchange-organization-AUTHAS", "Anonymous"),
                // A second of the same name (another case): the first wins.
                header(AUTH_AS, "Internal"),
            ],
        )
        .unwrap();
        for name in [
            AUTH_AS,
            "x-ms-exchange-organization-authas",
            "X-MS-EXCHANGE-ORGANIZATION-AUTHAS",
        ] {
            assert_eq!(
                s.message_header("M", name).unwrap().as_deref(),
                Some("Anonymous"),
                "{name}"
            );
        }
    }

    #[test]
    fn refetch_replaces_message_headers() {
        let s = store_with_message();
        s.put_fetched_body_with_headers(
            "M",
            &text_body("v1"),
            BodyFormat::Text,
            &[
                header(AUTH_AS, "Internal"),
                header(DIRECTIONALITY, "Originating"),
            ],
        )
        .unwrap();
        s.put_fetched_body_with_headers(
            "M",
            &html_body("<p>v2</p>"),
            BodyFormat::Html,
            &[header(AUTH_AS, "Anonymous")],
        )
        .unwrap();
        assert_eq!(s.get_body("M").unwrap().unwrap().content, "<p>v2</p>");
        assert_eq!(
            s.message_header("M", AUTH_AS).unwrap().as_deref(),
            Some("Anonymous")
        );
        assert_eq!(s.message_header("M", DIRECTIONALITY).unwrap(), None);
    }

    #[test]
    fn a_failed_header_write_keeps_the_earlier_body_and_headers() {
        // The body and its headers are one transaction: a header write that
        // fails after the body write succeeded leaves the earlier body and
        // headers, not the new body with the old headers.
        let s = store_with_message();
        s.put_fetched_body_with_headers(
            "M",
            &text_body("v1"),
            BodyFormat::Text,
            &[header(AUTH_AS, "Internal")],
        )
        .unwrap();
        s.conn
            .execute_batch(
                "CREATE TEMP TRIGGER fail_headers BEFORE INSERT ON message_headers
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        assert!(
            s.put_fetched_body_with_headers(
                "M",
                &text_body("v2"),
                BodyFormat::Text,
                &[header(AUTH_AS, "Anonymous")],
            )
            .is_err()
        );
        assert_eq!(s.get_body("M").unwrap().unwrap().content, "v1");
        assert_eq!(
            s.message_header("M", AUTH_AS).unwrap().as_deref(),
            Some("Internal")
        );
    }

    #[test]
    fn a_missing_message_stores_no_headers() {
        // No message row: the body's foreign key refuses it, and no header
        // row is written for it.
        let s = store_with_message();
        assert!(
            s.put_fetched_body_with_headers(
                "nope",
                &text_body("hi"),
                BodyFormat::Text,
                &[header(AUTH_AS, "Internal")],
            )
            .is_err()
        );
        assert_eq!(headers_rows(&s), 0);
    }

    #[test]
    fn message_headers_go_with_a_deleted_message() {
        let s = store_with_message();
        s.put_fetched_body_with_headers(
            "M",
            &text_body("hi"),
            BodyFormat::Text,
            &[header(AUTH_AS, "Internal")],
        )
        .unwrap();
        s.delete_message("M").unwrap();
        assert_eq!(s.message_header("M", AUTH_AS).unwrap(), None);
        assert_eq!(headers_rows(&s), 0);
    }

    #[test]
    fn reconcile_id_moves_message_headers() {
        let s = Store::open_in_memory().unwrap();
        let local = s.create_local_draft("Subj", "a@x", "", "body").unwrap();
        s.put_fetched_body_with_headers(
            &local,
            &text_body("body"),
            BodyFormat::Text,
            &[header(AUTH_AS, "Internal")],
        )
        .unwrap();
        s.reconcile_id(&local, "G").unwrap();
        assert_eq!(
            s.message_header("G", AUTH_AS).unwrap().as_deref(),
            Some("Internal")
        );
        assert_eq!(s.message_header(&local, AUTH_AS).unwrap(), None);
    }

    #[test]
    fn message_header_of_a_store_from_before_the_table_is_none() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-headers-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        {
            let s = Store::open(&path).unwrap();
            // Take the database back to before the table existed, then
            // store mail the way an older build did.
            s.conn.execute_batch("DROP TABLE message_headers;").unwrap();
            s.upsert_folder(&MailFolder {
                id: "INBOX".into(),
                display_name: "Inbox".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("inbox".into()),
            })
            .unwrap();
            s.upsert_message("INBOX", &message("M", false)).unwrap();
            s.put_fetched_body("M", &text_body("old"), BodyFormat::Text)
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.message_header("M", AUTH_AS).unwrap(), None);
        assert_eq!(s.get_body("M").unwrap().unwrap().content, "old");
        // The table is there now: a later fetch stores its headers.
        s.put_fetched_body_with_headers(
            "M",
            &text_body("new"),
            BodyFormat::Text,
            &[header(AUTH_AS, "Internal")],
        )
        .unwrap();
        assert_eq!(
            s.message_header("M", AUTH_AS).unwrap().as_deref(),
            Some("Internal")
        );
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Folders the server renamed, moved or deleted (#1886).
#[cfg(test)]
mod folder_retain_tests {
    use super::*;
    use crate::graph::model::{Body, MailFolder, Message, Recipient};

    fn msg(id: &str) -> Message {
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
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: "p".into(),
            is_draft: false,
            is_meeting_request: false,
            categories: Vec::new(),
        }
    }

    fn folder(id: &str, parent: Option<&str>, name: &str, total: i64, unread: i64) -> MailFolder {
        MailFolder {
            id: id.into(),
            display_name: name.into(),
            parent_id: parent.map(str::to_string),
            total_count: total,
            unread_count: unread,
            well_known_name: None,
        }
    }

    fn folder_ids(s: &Store) -> Vec<String> {
        let mut ids: Vec<String> = s.folders().unwrap().into_iter().map(|f| f.id).collect();
        ids.sort();
        ids
    }

    #[test]
    fn upsert_folder_reports_whether_the_row_changed() {
        let s = Store::open_in_memory().unwrap();
        assert!(
            s.upsert_folder(&folder("F", None, "A", 1, 0)).unwrap(),
            "inserted"
        );
        assert!(
            !s.upsert_folder(&folder("F", None, "A", 1, 0)).unwrap(),
            "same"
        );
        assert!(
            s.upsert_folder(&folder("F", None, "B", 1, 0)).unwrap(),
            "renamed"
        );
        assert!(
            s.upsert_folder(&folder("F", None, "B", 2, 0)).unwrap(),
            "total"
        );
        assert!(
            s.upsert_folder(&folder("F", None, "B", 2, 1)).unwrap(),
            "unread"
        );
        assert!(
            s.upsert_folder(&folder("F", Some("P"), "B", 2, 1)).unwrap(),
            "moved"
        );
        let mut wk = folder("F", Some("P"), "B", 2, 1);
        wk.well_known_name = Some("inbox".into());
        assert!(s.upsert_folder(&wk).unwrap(), "well-known name");
        assert!(!s.upsert_folder(&wk).unwrap(), "same again");
    }

    #[test]
    fn upsert_folder_keeps_local_state_across_a_rename_and_a_move() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&folder("F", None, "Old", 0, 0)).unwrap();
        s.set_folder_expanded("F", true).unwrap();
        s.conn
            .execute("UPDATE folders SET sort_order = 7 WHERE id = 'F'", [])
            .unwrap();
        s.set_delta_link("F", "LINK").unwrap();

        s.upsert_folder(&folder("F", Some("P"), "New", 3, 1))
            .unwrap();
        let f = s
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.id == "F")
            .unwrap();
        assert_eq!(f.display_name, "New");
        assert_eq!(f.parent_id.as_deref(), Some("P"));
        assert_eq!((f.total_count, f.unread_count), (3, 1));
        assert!(f.is_expanded);
        assert_eq!(f.sort_order, Some(7));
        assert_eq!(f.delta_link.as_deref(), Some("LINK"));
    }

    #[test]
    fn retain_folders_drops_unlisted_folders_with_their_messages() {
        let s = Store::open_in_memory().unwrap();
        for id in ["KEEP", "GONE", "GONE_CHILD"] {
            s.upsert_folder(&folder(id, None, id, 0, 0)).unwrap();
            s.set_delta_link(id, "LINK").unwrap();
        }
        s.upsert_message("KEEP", &msg("11")).unwrap();
        s.upsert_message("GONE", &msg("12")).unwrap();
        s.upsert_message("GONE_CHILD", &msg("13")).unwrap();
        s.put_body(
            "12",
            &Body {
                content_type: "text".into(),
                content: "zanzibar".into(),
            },
        )
        .unwrap();
        s.conn
            .execute(
                "INSERT INTO attachments (id, message_id, name) VALUES ('A1', '12', 'a.txt')",
                [],
            )
            .unwrap();
        assert_eq!(s.search("zanzibar", 10).unwrap().len(), 1);

        let removed = s.retain_folders(&["KEEP".to_string()]).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(folder_ids(&s), vec!["KEEP"]);
        assert_eq!(s.messages_in_folder("KEEP", 50, 0).unwrap()[0].id, "11");
        for table in ["messages", "bodies", "attachments"] {
            let col = if table == "messages" {
                "id"
            } else {
                "message_id"
            };
            let n: i64 = s
                .conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {col} IN ('12', '13')"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0, "{table}");
        }
        assert!(s.search("zanzibar", 10).unwrap().is_empty());
        assert!(s.search("s12", 10).unwrap().is_empty());
        assert_eq!(s.search("s11", 10).unwrap().len(), 1);

        assert_eq!(
            s.retain_folders(&["KEEP".to_string()]).unwrap(),
            0,
            "idempotent"
        );
    }

    #[test]
    fn retain_folders_keeps_the_drafts_sentinel_while_it_holds_a_draft() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_folder(&folder("INBOX", None, "Inbox", 0, 0))
            .unwrap();
        // No Drafts row: the draft lands under the sentinel.
        let id = s.create_local_draft("Hi", "", "", "<p>x</p>").unwrap();
        assert_eq!(
            s.messages_in_folder(LOCAL_DRAFTS_SENTINEL_FOLDER_ID, 50, 0)
                .unwrap()[0]
                .id,
            id
        );

        assert_eq!(s.retain_folders(&["INBOX".to_string()]).unwrap(), 0);
        assert_eq!(
            folder_ids(&s),
            vec!["INBOX", LOCAL_DRAFTS_SENTINEL_FOLDER_ID]
        );

        // Adopted into the real Drafts folder, the empty sentinel goes.
        let mut drafts = folder("DRAFTS", None, "Drafts", 0, 0);
        drafts.well_known_name = Some("drafts".into());
        s.upsert_folder(&drafts).unwrap();
        s.adopt_sentinel_drafts("DRAFTS").unwrap();
        let keep = ["INBOX".to_string(), "DRAFTS".to_string()];
        assert_eq!(s.retain_folders(&keep).unwrap(), 1);
        assert_eq!(folder_ids(&s), vec!["DRAFTS", "INBOX"]);
        assert_eq!(s.messages_in_folder("DRAFTS", 50, 0).unwrap()[0].id, id);
    }
}

#[cfg(test)]
mod backfill_tests {
    //! The whole-mailbox backfill's folder state (#1871).
    use super::*;

    fn folder(s: &Store, id: &str, total: i64, well_known: Option<&str>) {
        s.upsert_folder(&MailFolder {
            id: id.into(),
            display_name: id.into(),
            parent_id: None,
            total_count: total,
            unread_count: 0,
            well_known_name: well_known.map(str::to_string),
        })
        .unwrap();
    }

    fn history(s: &Store, id: &str) -> FolderHistory {
        s.folder_history(id).unwrap().unwrap()
    }

    #[test]
    fn init_adds_the_backfill_columns_to_an_old_folders_table() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-backfill-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        {
            let s = Store::open(&path).unwrap();
            folder(&s, "F1", 7, None);
            s.set_delta_link("F1", "OLD").unwrap();
            // Take the database back to before the columns existed.
            s.conn
                .execute_batch(
                    "ALTER TABLE folders DROP COLUMN backfill_link;
                     ALTER TABLE folders DROP COLUMN backfill_synced;
                     ALTER TABLE folders DROP COLUMN history_scope;
                     ALTER TABLE folders DROP COLUMN backfill_scope;",
                )
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        // A folder synced before #1871: a legacy link, no scope, no backfill.
        assert_eq!(
            history(&s, "F1"),
            FolderHistory {
                folder_id: "F1".into(),
                scope: None,
                synced: 0,
                backfill_link: None,
                backfill_scope: None,
                has_delta_link: true,
                total_count: 7,
            }
        );
        s.set_backfill_progress("F1", "NEXT", 3, SCOPE_ALL).unwrap();
        assert_eq!(history(&s, "F1").backfill_link.as_deref(), Some("NEXT"));
        drop(s);
        // Opening again (columns already there) keeps the state.
        let s = Store::open(&path).unwrap();
        assert_eq!(history(&s, "F1").synced, 3);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn head_progress_and_complete_backfill_record_the_folder_history() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1", 120, None);
        let h = history(&s, "F1");
        assert_eq!((h.scope, h.has_delta_link), (None, false));

        s.set_head_link("F1", "HEAD").unwrap();
        assert_eq!(s.get_delta_link("F1").unwrap().as_deref(), Some("HEAD"));
        assert_eq!(history(&s, "F1").scope.as_deref(), Some(SCOPE_HEAD));

        s.set_backfill_progress("F1", "PAGE2", 50, SCOPE_ALL)
            .unwrap();
        let h = history(&s, "F1");
        assert_eq!((h.backfill_link.as_deref(), h.synced), (Some("PAGE2"), 50));
        // The head link stays the folder's link while the backfill runs.
        assert_eq!(s.get_delta_link("F1").unwrap().as_deref(), Some("HEAD"));
    }

    #[test]
    fn complete_backfill_sets_link_and_scope_and_clears_the_cursor() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1", 120, None);
        s.set_head_link("F1", "HEAD").unwrap();
        s.set_backfill_progress("F1", "PAGE3", 100, SCOPE_ALL)
            .unwrap();
        s.complete_backfill("F1", "FULL", SCOPE_ALL, 120).unwrap();
        assert_eq!(s.get_delta_link("F1").unwrap().as_deref(), Some("FULL"));
        assert_eq!(
            history(&s, "F1"),
            FolderHistory {
                folder_id: "F1".into(),
                scope: Some(SCOPE_ALL.into()),
                synced: 120,
                backfill_link: None,
                backfill_scope: None,
                has_delta_link: true,
                total_count: 120,
            }
        );
    }

    #[test]
    fn reset_backfill_keeps_the_head_and_restarts_the_count() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1", 120, None);
        s.set_head_link("F1", "HEAD").unwrap();
        s.set_backfill_progress("F1", "PAGE3", 100, SCOPE_ALL)
            .unwrap();
        s.reset_backfill("F1").unwrap();
        let h = history(&s, "F1");
        assert_eq!((h.backfill_link, h.synced), (None, 0));
        assert_eq!(h.scope.as_deref(), Some(SCOPE_HEAD));
        assert_eq!(s.get_delta_link("F1").unwrap().as_deref(), Some("HEAD"));
    }

    #[test]
    fn clear_delta_links_clears_backfill_state() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1", 120, None);
        folder(&s, "F2", 10, None);
        s.set_head_link("F1", "HEAD").unwrap();
        s.set_backfill_progress("F1", "PAGE3", 100, SCOPE_ALL)
            .unwrap();
        s.complete_backfill("F2", "FULL", SCOPE_WINDOW, 10).unwrap();
        s.clear_delta_links().unwrap();
        for id in ["F1", "F2"] {
            let h = history(&s, id);
            assert_eq!(
                (h.scope, h.synced, h.backfill_link, h.has_delta_link),
                (None, 0, None, false),
                "{id}"
            );
        }
    }

    #[test]
    fn upsert_folder_leaves_the_backfill_state_alone() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1", 120, None);
        s.set_head_link("F1", "HEAD").unwrap();
        s.set_backfill_progress("F1", "PAGE2", 50, SCOPE_ALL)
            .unwrap();
        // The next listing renames it and changes its count.
        s.upsert_folder(&MailFolder {
            id: "F1".into(),
            display_name: "Renamed".into(),
            parent_id: None,
            total_count: 130,
            unread_count: 2,
            well_known_name: None,
        })
        .unwrap();
        let h = history(&s, "F1");
        assert_eq!(
            (
                h.scope.as_deref(),
                h.backfill_link.as_deref(),
                h.synced,
                h.total_count
            ),
            (Some(SCOPE_HEAD), Some("PAGE2"), 50, 130)
        );
    }

    #[test]
    fn folder_histories_follow_the_folder_order() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "A-other", 1, None);
        folder(&s, "B-inbox", 1, Some("inbox"));
        folder(&s, "C-sent", 1, Some("sentitems"));
        let ids: Vec<String> = s
            .folder_histories()
            .unwrap()
            .into_iter()
            .map(|h| h.folder_id)
            .collect();
        let order: Vec<String> = s.folders().unwrap().into_iter().map(|f| f.id).collect();
        assert_eq!(ids, order);
        assert_eq!(ids, ["B-inbox", "C-sent", "A-other"]);
        assert!(s.folder_history("NOPE").unwrap().is_none());
    }

    #[test]
    fn update_draft_fields_bumps_the_edit_generation_and_a_push_clears_it() {
        let s = Store::open_in_memory().unwrap();
        let local = s.create_local_draft("Sub", "", "", "body").unwrap();
        assert_eq!(s.local_edit_generation(&local).unwrap(), 0);
        s.update_draft_fields(&local, "a", "", "", "", "b").unwrap();
        s.update_draft_fields(&local, "b", "", "", "", "b").unwrap();
        let read = s.local_edit_generation(&local).unwrap();
        assert_eq!(read, 2);
        assert_eq!(
            s.locally_edited_ids().unwrap(),
            std::slice::from_ref(&local)
        );
        // The push reconciles the id; the local: id still reaches the row.
        s.reconcile_id(&local, "G").unwrap();
        assert_eq!(s.local_edit_generation(&local).unwrap(), 2);
        assert_eq!(s.locally_edited_ids().unwrap(), ["G"]);
        // An edit after the push read the draft: a stale clear keeps it dirty.
        s.update_draft_fields(&local, "c", "", "", "", "b").unwrap();
        assert!(!s.clear_local_edit(&local, read).unwrap());
        assert_eq!(s.local_edit_generation("G").unwrap(), 3);
        // A clear of the current generation makes it clean.
        assert!(s.clear_local_edit(&local, 3).unwrap());
        assert_eq!(s.local_edit_generation("G").unwrap(), 0);
        assert!(s.locally_edited_ids().unwrap().is_empty());
        assert_eq!(s.local_edit_generation("NOPE").unwrap(), 0);
    }

    #[test]
    fn merge_message_unless_edited_leaves_an_edited_row_alone() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1", 10, None);
        folder(&s, "F2", 10, None);
        let edited = s.create_local_draft("Sub", "", "", "body").unwrap();
        s.reconcile_id(&edited, "E").unwrap();
        s.update_draft_fields("E", "mine", "me@x", "cc@x", "", "b")
            .unwrap();
        let clean = s.create_local_draft("Old", "", "", "body").unwrap();
        s.reconcile_id(&clean, "C").unwrap();
        let server = |id: &str| Message {
            id: id.into(),
            conversation_id: "C".into(),
            subject: "server".into(),
            from: Recipient::default(),
            to: vec![Recipient {
                name: "S".into(),
                address: "s@x".into(),
            }],
            cc: vec![],
            received: "2026-01-01T00:00:00Z".into(),
            sent: String::new(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: String::new(),
            is_draft: false,
            is_meeting_request: false,
            categories: vec![],
        };
        let all = MessageFields::all();
        assert!(
            !s.merge_message_unless_edited("F2", &server("E"), &all)
                .unwrap()
        );
        let (row, _) = s.draft("E").unwrap().unwrap();
        assert_eq!(
            (
                row.subject.as_str(),
                row.to_recipients.as_str(),
                row.cc_recipients.as_str()
            ),
            ("mine", "me@x", "cc@x")
        );
        assert_ne!(row.folder_id, "F2");
        assert!(
            s.merge_message_unless_edited("F2", &server("C"), &all)
                .unwrap()
        );
        let (row, _) = s.draft("C").unwrap().unwrap();
        assert_eq!(
            (row.subject.as_str(), row.folder_id.as_str()),
            ("server", "F2")
        );
        // A missing row is inserted.
        assert!(
            s.merge_message_unless_edited("F2", &server("NEW"), &all)
                .unwrap()
        );
        // The plain merge still writes over the edit.
        s.merge_message("F2", &server("E"), &all).unwrap();
        assert_eq!(s.draft("E").unwrap().unwrap().0.subject, "server");
    }

    #[test]
    fn a_database_from_before_local_edit_reads_clean() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-local-edit-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        let local = {
            let s = Store::open(&path).unwrap();
            let local = s.create_local_draft("Old", "", "", "body").unwrap();
            s.conn
                .execute_batch("ALTER TABLE messages DROP COLUMN local_edit;")
                .unwrap();
            local
        };
        let s = Store::open(&path).unwrap();
        assert_eq!(s.local_edit_generation(&local).unwrap(), 0);
        s.update_draft_fields(&local, "new", "", "", "", "b")
            .unwrap();
        assert_eq!(s.local_edit_generation(&local).unwrap(), 1);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_message_ids_names_the_message_ops_only() {
        let s = Store::open_in_memory().unwrap();
        for op in [
            OutboxOp::MarkRead {
                id: "A".into(),
                read: true,
            },
            OutboxOp::SetFlag {
                id: "B".into(),
                flagged: true,
            },
            OutboxOp::Move {
                id: "C".into(),
                dest: "F".into(),
            },
            OutboxOp::Delete { id: "D".into() },
            OutboxOp::SaveDraft { id: "E".into() },
            OutboxOp::MarkRead {
                id: "A".into(),
                read: false,
            },
            OutboxOp::DeleteEvent { id: "EVT".into() },
        ] {
            s.enqueue_op(&op).unwrap();
        }
        assert_eq!(s.pending_message_ids().unwrap(), ["A", "B", "C", "D", "E"]);
    }

    #[test]
    fn clear_delta_links_marks_a_reconverge_until_it_is_cleared() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1", 10, None);
        s.set_delta_link("F1", "LINK").unwrap();
        assert!(!s.reconverge_pending().unwrap());
        s.clear_delta_links().unwrap();
        assert!(s.reconverge_pending().unwrap());
        assert!(s.get_delta_link("F1").unwrap().is_none());
        s.clear_delta_links().unwrap();
        s.clear_reconverge_pending().unwrap();
        assert!(!s.reconverge_pending().unwrap());
    }

    #[test]
    fn a_walk_is_window_once_any_page_ran_with_the_cutoff() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1", 120, None);
        s.set_head_link("F1", "HEAD").unwrap();
        s.set_backfill_progress("F1", "P2", 1, SCOPE_ALL).unwrap();
        assert_eq!(history(&s, "F1").backfill_scope.as_deref(), Some(SCOPE_ALL));
        s.set_backfill_progress("F1", "P3", 2, SCOPE_WINDOW)
            .unwrap();
        s.set_backfill_progress("F1", "P4", 3, SCOPE_ALL).unwrap();
        assert_eq!(
            history(&s, "F1").backfill_scope.as_deref(),
            Some(SCOPE_WINDOW)
        );
        s.reset_backfill("F1").unwrap();
        assert_eq!(history(&s, "F1").backfill_scope, None);
        s.set_backfill_progress("F1", "P2", 1, SCOPE_WINDOW)
            .unwrap();
        s.complete_backfill("F1", "FULL", SCOPE_WINDOW, 1).unwrap();
        assert_eq!(history(&s, "F1").backfill_scope, None);
        s.set_backfill_progress("F1", "P2", 1, SCOPE_WINDOW)
            .unwrap();
        s.clear_delta_links().unwrap();
        assert_eq!(history(&s, "F1").backfill_scope, None);
    }
}
