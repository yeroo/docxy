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

use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::graph::client::{AttendeeInput, ContactFieldSet, EventFieldSet, event_zone_error};
use crate::graph::model::{
    AttachmentKind, AttachmentMeta, Attendee, Body, BodyFormat, Calendar, ContactFields, Event,
    EventRecurrence, FolderContact, MailFolder, MasterCategory, MeetingMessageType, Message,
    MessageFields, MessageHeader, MessageProtection, PhysicalAddress, Recipient, Recurrence,
    Sensitivity, is_stored_header, meeting_link_in,
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
    /// An argument the store cannot use (#2054: a time that is not one).
    Invalid(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sql(msg) => write!(f, "sqlite error: {msg}"),
            StoreError::Decode(msg) => write!(f, "outbox decode error: {msg}"),
            StoreError::Invalid(msg) => f.write_str(msg),
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
    /// The flagged message's Outlook reminder is on (#2054,
    /// `Message::is_reminder_on`). Never true on an unflagged row: a merge
    /// or `set_flag` that leaves it unflagged turns it off.
    pub is_reminder_on: bool,
    /// The time the reminder was set for (`PidLidReminderTime`), UTC
    /// `YYYY-MM-DDTHH:MM:SSZ`, or `""`.
    pub reminder_at: String,
    /// When the reminder fires next (`PidLidReminderSignalTime`, moved by a
    /// snooze), UTC `YYYY-MM-DDTHH:MM:SSZ`, or `""`.
    pub reminder_signal_at: String,
    /// The flag's due time (`flag.dueDateTime`), UTC, or `""`.
    pub flag_due_at: String,
    /// The message's meeting message type (#2081,
    /// `Message::meeting_message_type`): a request, a cancellation or an
    /// attendee's response, `None` for any other message or a row stored
    /// before the type was (until a sync re-reads it or
    /// `FetchMeetingMessage` answers for it). A host's list shows its icon.
    pub meeting_message_type: Option<MeetingMessageType>,
    /// The message's protection (#2088, `Message::protection`): its
    /// Sensitivity, its enabled sensitivity label and whether it is
    /// rights-protected. The default (unprotected) for a row stored before
    /// it was, until a sync re-reads it. For a host's list to show a lock
    /// or a label without opening the message.
    pub protection: MessageProtection,
    /// A meeting message's own reminder is set (#2089,
    /// `Message::meeting_reminder_on`): for a host's list to show classic
    /// Outlook's bell beside the meeting icon. Independent of the flag and
    /// of `is_reminder_on`; it never fires a reminder (it is not in
    /// `due_message_reminders`' condition). False for any other message, and
    /// for a row stored before it was, until a sync re-reads it.
    pub meeting_reminder_on: bool,
    /// The message's own Internet message id (#2096,
    /// `Message::internet_message_id`), what a reply's `in_reply_to` names.
    /// `""` when Graph sent none, and for a row stored before #2096 until a
    /// sync re-reads it.
    pub internet_message_id: String,
    /// The Internet message id of the message this one answers (#2096,
    /// `Message::in_reply_to`): set on a reply, and on a reply draft from
    /// the moment it is composed here. `""` on anything else, and for a row
    /// stored before #2096 until a sync re-reads it. Never cleared by a
    /// merge. With `conversation_id`, what `Store::reply_draft_links` reads.
    pub in_reply_to: String,
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
    /// The Graph upload session a send opened for this attachment and has
    /// not finished (#1941): a pre-authenticated url, so it is kept like
    /// the message bodies, not like the OAuth tokens. A later attempt
    /// resumes it only while `upload_digest` matches what it would upload.
    pub upload_url: Option<String>,
    /// The fingerprint of what the `upload_url` session uploads (name,
    /// content type, content id and bytes, see `sync::outbox`), set with it.
    pub upload_digest: Option<String>,
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
    /// Move a message to the folder `dest` (`client.move_message`). The
    /// drain re-keys the row to the id Graph answers with (#1896,
    /// `Store::follow_move`).
    Move {
        id: String,
        dest: String,
    },
    /// Delete a message (`client.delete_message`): Graph soft-deletes it to
    /// the mailbox's Recoverable Items, not to Deleted Items, which is a
    /// `Move` to that folder.
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
    /// (`.../accept`|`.../decline`|`.../tentativelyAccept` — see
    /// `sync::outbox::apply_op`). `kind` is the same response_status
    /// vocabulary `Store::set_event_response` writes locally (`"accepted"`,
    /// `"declined"`, `"tentativelyAccepted"`); `apply_op` converts it with
    /// `graph::client::RsvpKind::from_status`. `comment` is the optional note
    /// attendees see alongside the response. `send_response` false is "Don't
    /// send a response" (#2023): the JSON key `sendResponse`, which a row
    /// queued before #2023 lacks and reads as `true`, the only behaviour then.
    RespondEvent {
        id: String,
        kind: String,
        comment: Option<String>,
        send_response: bool,
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
    /// Cancel a meeting the user organizes (#1985): Graph's `.../cancel`
    /// sends every attendee a cancellation with `comment` and deletes the
    /// organizer's copy. `SyncCommand::CancelEvent` has already deleted the
    /// row locally. A `local:` id never reached Graph and sends nothing.
    CancelEvent {
        id: String,
        comment: Option<String>,
    },
    /// Forward a meeting to `to` with `comment` (#1985, `.../forward`).
    /// Nothing local changes. `to` is stored as `[{"name":…,"address":…}]`.
    ForwardEvent {
        id: String,
        to: Vec<Recipient>,
        comment: Option<String>,
    },
    /// A series edit (#2022): PATCH the series master `id` with `body`, the
    /// JSON `event_body_json` built at command time. The body is stored
    /// whole because the master was not stored when #2022 added this, and
    /// still may not be (a calendar pass stores it only when a round
    /// returns or fetches it, #2064), so nothing is read at send time. No local write goes
    /// with it. A `local:` id sends nothing.
    UpdateEventSeries {
        id: String,
        body: String,
    },
    /// Dismiss the event's reminder on the server (#2025,
    /// `.../dismissReminder`), so the user's other devices stop showing it.
    /// Nothing local changes: what Exchange does to the reminder comes back
    /// with the next calendar round. A `local:` id sends nothing.
    DismissReminder {
        id: String,
    },
    /// Snooze the event's reminder until `new_reminder_utc` (#2025,
    /// `.../snoozeReminder`), the store's `YYYY-MM-DDTHH:MM:SSZ`. Nothing
    /// local changes. A `local:` id sends nothing.
    SnoozeReminder {
        id: String,
        new_reminder_utc: String,
    },
    /// Dismiss a flagged message's reminder on the server (#2054): a PATCH
    /// of `PidLidReminderSet` to false. A message op (it follows a move,
    /// #1896); the command wrote the store first
    /// (`Store::dismiss_message_reminder`). A `local:` id sends nothing.
    DismissMessageReminder {
        id: String,
    },
    /// Snooze a flagged message's reminder until `new_reminder_utc`, the
    /// store's `YYYY-MM-DDTHH:MM:SSZ` (#2054): a PATCH of
    /// `PidLidReminderSignalTime`. A message op, written to the store first
    /// (`Store::snooze_message_reminder`). A `local:` id sends nothing.
    SnoozeMessageReminder {
        id: String,
        new_reminder_utc: String,
    },
    /// Create a contact the host wrote locally (#2063,
    /// `Store::create_local_contact`): `POST /me/contacts` with the row as
    /// stored at send time, then re-key it to Graph's id
    /// (`Store::reconcile_created_contact`). A row deleted before the drain,
    /// or an id already re-keyed (a retry after the re-key), sends nothing.
    CreateContact {
        id: String,
    },
    /// PATCH the properties in `fields` of a stored contact (#2063,
    /// `Store::update_contact_fields`), each with its value at send time.
    /// The id is resolved through `contact_aliases`; one still `local:`, or a
    /// row gone, sends nothing.
    UpdateContact {
        id: String,
        fields: ContactFieldSet,
    },
    /// DELETE a contact the host removed locally (#2063,
    /// `Store::delete_contact`). A 404 is done. A `local:` id never re-keyed
    /// sends nothing.
    DeleteContact {
        id: String,
    },
    /// Create a mail folder the host made locally (#1910,
    /// `Store::create_local_folder`): `POST /me/mailFolders` (or the
    /// parent's `childFolders`) with `name`, then re-key the `local:` row
    /// `id` to Graph's id (`Store::reconcile_created_folder`). `parent` is
    /// the stored parent at command time (`None`: the top), resolved again
    /// through `folder_aliases` at send time. A row gone, or an id already
    /// re-keyed, sends nothing.
    CreateFolder {
        id: String,
        parent: Option<String>,
        name: String,
    },
    /// PATCH a folder's `displayName` to `name` (#1910,
    /// `Store::rename_folder`); `prev_name` is what the store held before,
    /// for the revert of a refusal. A `local:` id never re-keyed sends
    /// nothing.
    RenameFolder {
        id: String,
        name: String,
        prev_name: String,
    },
    /// Move a folder under `dest` (#1910, `Store::move_folder`; `None`: the
    /// top of the mailbox, `msgfolderroot`). `parent` is the `parent_id` the
    /// command stored and `prev_parent` the one before it, for the revert
    /// of a refusal. A `local:` id never re-keyed sends nothing.
    MoveFolder {
        id: String,
        dest: Option<String>,
        parent: Option<String>,
        prev_parent: Option<String>,
    },
    /// DELETE a folder (#1910, `Store::delete_folder`), which the command
    /// removed locally with its subtree; `removed` is a snapshot of the
    /// removed rows Graph has (not `local:` ones), put back if Graph
    /// refuses. A 404 is done.
    DeleteFolder {
        id: String,
        removed: Vec<FolderSnapshot>,
    },
}

/// A folder row as a refused `DeleteFolder` puts it back (#1910): what the
/// listing gives plus the local state (`sort_order`, `is_expanded`). No
/// delta link: the next pass's delta brings its messages back.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderSnapshot {
    pub id: String,
    pub parent_id: Option<String>,
    pub display_name: String,
    pub total_count: i64,
    pub unread_count: i64,
    pub well_known_name: Option<String>,
    pub sort_order: Option<i64>,
    pub is_expanded: bool,
}

impl FolderSnapshot {
    fn to_json(&self) -> Value {
        Value::Object(vec![
            ("id".to_string(), Value::Str(self.id.clone())),
            ("parentId".to_string(), optional_str(&self.parent_id)),
            (
                "displayName".to_string(),
                Value::Str(self.display_name.clone()),
            ),
            ("total".to_string(), Value::Num(self.total_count as f64)),
            ("unread".to_string(), Value::Num(self.unread_count as f64)),
            (
                "wellKnownName".to_string(),
                optional_str(&self.well_known_name),
            ),
            (
                "sortOrder".to_string(),
                self.sort_order
                    .map_or(Value::Null, |n| Value::Num(n as f64)),
            ),
            ("expanded".to_string(), Value::Bool(self.is_expanded)),
        ])
    }

    fn from_json(v: &Value) -> Option<FolderSnapshot> {
        let opt = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(FolderSnapshot {
            id: v.get("id")?.as_str()?.to_string(),
            parent_id: opt("parentId"),
            display_name: v.get("displayName")?.as_str()?.to_string(),
            total_count: v.get("total").and_then(Value::as_i64).unwrap_or(0),
            unread_count: v.get("unread").and_then(Value::as_i64).unwrap_or(0),
            well_known_name: opt("wellKnownName"),
            sort_order: v.get("sortOrder").and_then(Value::as_i64),
            is_expanded: v.get("expanded").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

impl OutboxOp {
    /// The message a message op names, as it was queued (a `local:` draft
    /// id, or an id from before a move: resolve it with `current_id`);
    /// `None` for an event, contact or folder op. Exhaustive, so a new op
    /// must say which it is.
    pub fn message_id(&self) -> Option<&str> {
        match self {
            OutboxOp::MarkRead { id, .. }
            | OutboxOp::SetFlag { id, .. }
            | OutboxOp::SetCategories { id, .. }
            | OutboxOp::Move { id, .. }
            | OutboxOp::Delete { id }
            | OutboxOp::SaveDraft { id }
            | OutboxOp::SendDraft { id }
            | OutboxOp::DismissMessageReminder { id }
            | OutboxOp::SnoozeMessageReminder { id, .. } => Some(id),
            OutboxOp::RespondEvent { .. }
            | OutboxOp::CreateEvent { .. }
            | OutboxOp::UpdateEvent { .. }
            | OutboxOp::DeleteEvent { .. }
            | OutboxOp::CancelEvent { .. }
            | OutboxOp::ForwardEvent { .. }
            | OutboxOp::UpdateEventSeries { .. }
            | OutboxOp::DismissReminder { .. }
            | OutboxOp::SnoozeReminder { .. }
            | OutboxOp::CreateContact { .. }
            | OutboxOp::UpdateContact { .. }
            | OutboxOp::DeleteContact { .. }
            | OutboxOp::CreateFolder { .. }
            | OutboxOp::RenameFolder { .. }
            | OutboxOp::MoveFolder { .. }
            | OutboxOp::DeleteFolder { .. } => None,
        }
    }

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
            OutboxOp::CancelEvent { .. } => "cancelEvent",
            OutboxOp::ForwardEvent { .. } => "forwardEvent",
            OutboxOp::UpdateEventSeries { .. } => "updateEventSeries",
            OutboxOp::DismissReminder { .. } => "dismissReminder",
            OutboxOp::SnoozeReminder { .. } => "snoozeReminder",
            OutboxOp::DismissMessageReminder { .. } => "dismissMessageReminder",
            OutboxOp::SnoozeMessageReminder { .. } => "snoozeMessageReminder",
            OutboxOp::CreateContact { .. } => "createContact",
            OutboxOp::UpdateContact { .. } => "updateContact",
            OutboxOp::DeleteContact { .. } => "deleteContact",
            OutboxOp::CreateFolder { .. } => "createFolder",
            OutboxOp::RenameFolder { .. } => "renameFolder",
            OutboxOp::MoveFolder { .. } => "moveFolder",
            OutboxOp::DeleteFolder { .. } => "deleteFolder",
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
                send_response,
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
                ("sendResponse".to_string(), Value::Bool(*send_response)),
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
            OutboxOp::UpdateContact { id, fields } => Value::Object(vec![
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
            OutboxOp::CreateEvent { id }
            | OutboxOp::DeleteEvent { id }
            | OutboxOp::DismissReminder { id }
            | OutboxOp::DismissMessageReminder { id }
            | OutboxOp::CreateContact { id }
            | OutboxOp::DeleteContact { id } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
            ]),
            OutboxOp::CreateFolder { id, parent, name } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                ("parent".to_string(), optional_str(parent)),
                ("name".to_string(), Value::Str(name.clone())),
            ]),
            OutboxOp::RenameFolder {
                id,
                name,
                prev_name,
            } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                ("name".to_string(), Value::Str(name.clone())),
                ("prevName".to_string(), Value::Str(prev_name.clone())),
            ]),
            OutboxOp::MoveFolder {
                id,
                dest,
                parent,
                prev_parent,
            } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                ("dest".to_string(), optional_str(dest)),
                ("parent".to_string(), optional_str(parent)),
                ("prevParent".to_string(), optional_str(prev_parent)),
            ]),
            OutboxOp::DeleteFolder { id, removed } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                (
                    "removed".to_string(),
                    Value::Array(removed.iter().map(FolderSnapshot::to_json).collect()),
                ),
            ]),
            OutboxOp::CancelEvent { id, comment } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                ("comment".to_string(), optional_str(comment)),
            ]),
            OutboxOp::UpdateEventSeries { id, body } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                ("body".to_string(), Value::Str(body.clone())),
            ]),
            OutboxOp::SnoozeReminder {
                id,
                new_reminder_utc,
            }
            | OutboxOp::SnoozeMessageReminder {
                id,
                new_reminder_utc,
            } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                (
                    "newReminderUtc".to_string(),
                    Value::Str(new_reminder_utc.clone()),
                ),
            ]),
            OutboxOp::ForwardEvent { id, to, comment } => Value::Object(vec![
                ("kind".to_string(), Value::Str(self.kind().to_string())),
                ("id".to_string(), Value::Str(id.clone())),
                (
                    "to".to_string(),
                    Value::Array(
                        to.iter()
                            .map(|r| {
                                Value::Object(vec![
                                    ("name".to_string(), Value::Str(r.name.clone())),
                                    ("address".to_string(), Value::Str(r.address.clone())),
                                ])
                            })
                            .collect(),
                    ),
                ),
                ("comment".to_string(), optional_str(comment)),
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
                send_response: v
                    .get("sendResponse")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
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
            "cancelEvent" => Some(OutboxOp::CancelEvent {
                id: id()?,
                comment: v.get("comment").and_then(Value::as_str).map(str::to_string),
            }),
            // An empty `to` is kept: the engine refuses one at command
            // time, so only a hand-edited row holds one, and Graph's
            // refusal then quarantines it. A recipient without an address
            // is a corrupt row.
            "forwardEvent" => Some(OutboxOp::ForwardEvent {
                id: id()?,
                to: v
                    .get("to")?
                    .as_array()?
                    .iter()
                    .map(|r| {
                        Some(Recipient {
                            name: r
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            address: r.get("address")?.as_str()?.to_string(),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?,
                comment: v.get("comment").and_then(Value::as_str).map(str::to_string),
            }),
            "updateEventSeries" => Some(OutboxOp::UpdateEventSeries {
                id: id()?,
                body: v.get("body")?.as_str()?.to_string(),
            }),
            "dismissReminder" => Some(OutboxOp::DismissReminder { id: id()? }),
            "snoozeReminder" => Some(OutboxOp::SnoozeReminder {
                id: id()?,
                new_reminder_utc: v.get("newReminderUtc")?.as_str()?.to_string(),
            }),
            "dismissMessageReminder" => Some(OutboxOp::DismissMessageReminder { id: id()? }),
            "snoozeMessageReminder" => Some(OutboxOp::SnoozeMessageReminder {
                id: id()?,
                new_reminder_utc: v.get("newReminderUtc")?.as_str()?.to_string(),
            }),
            "createContact" => Some(OutboxOp::CreateContact { id: id()? }),
            // A row without `fields` is corrupt: every `updateContact` ever
            // queued has them.
            "updateContact" => Some(OutboxOp::UpdateContact {
                id: id()?,
                fields: ContactFieldSet::from_names(
                    &v.get("fields")?
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>(),
                ),
            }),
            "deleteContact" => Some(OutboxOp::DeleteContact { id: id()? }),
            "createFolder" => Some(OutboxOp::CreateFolder {
                id: id()?,
                parent: v.get("parent").and_then(Value::as_str).map(str::to_string),
                name: v.get("name")?.as_str()?.to_string(),
            }),
            "renameFolder" => Some(OutboxOp::RenameFolder {
                id: id()?,
                name: v.get("name")?.as_str()?.to_string(),
                prev_name: v.get("prevName")?.as_str()?.to_string(),
            }),
            "moveFolder" => Some(OutboxOp::MoveFolder {
                id: id()?,
                dest: v.get("dest").and_then(Value::as_str).map(str::to_string),
                parent: v.get("parent").and_then(Value::as_str).map(str::to_string),
                prev_parent: v
                    .get("prevParent")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
            // A snapshot row that does not parse makes the whole row
            // corrupt, as a recipient without an address does.
            "deleteFolder" => Some(OutboxOp::DeleteFolder {
                id: id()?,
                removed: v
                    .get("removed")?
                    .as_array()?
                    .iter()
                    .map(FolderSnapshot::from_json)
                    .collect::<Option<Vec<_>>>()?,
            }),
            _ => None,
        }
    }
}

/// `Some` as a JSON string, `None` as `null` (an outbox op's optional text).
fn optional_str(s: &Option<String>) -> Value {
    match s {
        Some(s) => Value::Str(s.clone()),
        None => Value::Null,
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
    /// When the user answered (`responseStatus.time`) and when the event was
    /// created (`createdDateTime`), canonical UTC or `""` (#2068); see
    /// `graph::model::Event`.
    pub response_time: String,
    pub created_utc: String,
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
            response_time: String::new(),
            created_utc: String::new(),
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
    /// The new time the attendee proposed, canonical UTC; both `""` when
    /// none (Graph `attendees[].proposedNewTime`, the organizer's view,
    /// #2023).
    pub proposed_start_utc: String,
    pub proposed_end_utc: String,
}

/// A stored calendar (`Store::calendars`, #1984): Graph's fields, and
/// whether the engine syncs it (`Store::synced_calendar_ids`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CalendarRow {
    pub calendar: Calendar,
    pub syncs: bool,
}

/// One calendar's events as a fetch got them: a sync pass's round
/// (`SyncedCalendar`, `Store::apply_calendar_sync`) or a range
/// (`Store::apply_calendar_range`). Every event is stored under
/// `calendar_id`, whatever its own `calendar_id` says.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FetchedCalendar {
    pub calendar_id: String,
    pub events: Vec<NewEvent>,
    pub attendees: Vec<(String, Vec<NewAttendee>)>,
}

/// What one calendar's round of a sync pass fetched (#2013).
#[derive(Debug, Clone, PartialEq)]
pub enum CalendarRound {
    /// An initial round over the window: `fetched` is the whole window,
    /// which replaces the calendar's rows overlapping it.
    Window,
    /// A round from the stored deltaLink: `fetched`'s events (and their
    /// attendees) are added or changed, and each `removed` id goes, within
    /// the window, with its occurrences when the round upserts none of
    /// them (`apply_calendar_delta_on` has the rule). Each `gone_masters`
    /// id (#2064) is a stored series master a refetch found gone (a 404):
    /// its `seriesMaster` row of that calendar goes, whatever its dates,
    /// as a master before the window is outside the removal's bound.
    Delta {
        removed: Vec<String>,
        gone_masters: Vec<String>,
    },
}

/// One calendar of a sync pass (`Store::apply_calendar_sync`, #2013): what
/// its round fetched, and the deltaLink the next pass starts from.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncedCalendar {
    pub fetched: FetchedCalendar,
    pub round: CalendarRound,
    pub delta_link: String,
}

/// A calendar's stored delta state (`Store::calendar_delta`, #2013): the
/// window its last round covered and the deltaLink it ended with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarDeltaState {
    pub from_utc: String,
    pub to_utc: String,
    pub delta_link: String,
}

/// When a calendar range was fetched (#2014), for the `calendar_ranges`
/// rows `apply_calendar_sync` and `apply_calendar_range` write: `at` is
/// the fetch time (unix seconds), `fresh_since` the freshness cutoff
/// (`at` minus how long a range stays fresh). A row is fresh while its
/// `fetched_at > fresh_since` (`Store::calendar_range_gaps`), so recording
/// first drops the calendar's rows at or before the cutoff: they can never
/// count again, and the window pass would otherwise leave one sliver per
/// pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeStamp {
    pub at: i64,
    pub fresh_since: i64,
}

/// The field set `upsert_event` writes to an `events` row.
///
/// Same field set as `EventRow` plus the extra columns (`body_preview`,
/// `web_link`, `last_modified`, `body_html`) that `EventRow` doesn't surface.
/// `graph::model::Event` (Task 2) turned out to be a field-for-field
/// superset of this (it additionally carries `attendees`),
/// so rather than folding `NewEvent` away, the sync engine bridges the two
/// via the `From<&Event> for NewEvent` conversion below — a straight
/// per-field copy the engine runs for every event `calendar_view_in` returns.
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
    /// When the user answered (`responseStatus.time`) and when the event was
    /// created (`createdDateTime`), canonical UTC or `""` (#2068); see
    /// `graph::model::Event`.
    pub response_time: String,
    pub created_utc: String,
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
            response_time: String::new(),
            created_utc: String::new(),
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
    pub proposed_start_utc: String,
    pub proposed_end_utc: String,
}

/// Straight per-field copy: what the sync engine's `RefreshCalendar` runs for
/// every event `GraphClient::calendar_view_in` returns, before
/// `apply_calendar_sync`.
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
            response_time: e.response_time.clone(),
            created_utc: e.created_utc.clone(),
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
            proposed_start_utc: a.proposed_start_utc.clone(),
            proposed_end_utc: a.proposed_end_utc.clone(),
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
    /// The recurrence pattern for a repeating event (create-only), or `None`:
    /// three kinds and two range types (`Recurrence`). `series_recurrence`
    /// wins when both are set.
    pub recurrence: Option<Recurrence>,
    /// Graph's whole recurrence for a repeating event (create-only, #2034):
    /// every pattern type, `noEnd`/`endDate`/`numbered` ranges,
    /// `firstDayOfWeek`, `index`, `month` and `recurrenceTimeZone` as given,
    /// except that a start sent in its named zone (`start_time_zone`, #2039)
    /// sends that zone as `recurrenceTimeZone` instead. Its `CreateEvent`
    /// sends it with `EventRecurrence::to_send_json`, and it wins over
    /// `recurrence`. `update_event_fields` ignores it.
    pub series_recurrence: Option<EventRecurrence>,
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
    /// The time zone the start is sent to Graph in, and its wall clock there
    /// (#2039), as `EventInput::start_time_zone` / `start_local` take them:
    /// a Windows time zone id or an IANA name Graph lists, and
    /// `YYYY-MM-DDTHH:MM:SS`. `start_utc` stays the stored instant. `""`
    /// sends UTC. A named pair `graph::client::event_zone_error` refuses is
    /// a `StoreError`, with nothing written. On `update_event_fields` a
    /// named pair is always written but never marks `TIMES` by itself, and
    /// a `""` zone keeps the stored pair unless the times change (then it
    /// is cleared, and the update sends UTC).
    pub start_time_zone: String,
    pub start_local: String,
    /// The same for the end, which may be in another zone.
    pub end_time_zone: String,
    pub end_local: String,
    /// The calendar a create files the event in (#2048), create-only:
    /// `None` or `Some("")` is the default calendar (its id, or `''` before
    /// the calendar list is known), and `Some(id)` must be a stored calendar
    /// whose `can_edit` is set, or `create_local_event` refuses it. Its
    /// `CreateEvent` POSTs to that calendar's `/events`
    /// (`Store::event_create_calendar`). `update_event_fields` ignores it:
    /// an edit never moves an event between calendars.
    pub calendar_id: Option<String>,
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
    /// The create-side recurrence a repeating event was created with, or
    /// `None` (also when it was created with `series_recurrence`).
    pub recurrence: Option<Recurrence>,
    /// The lossless recurrence a repeating event was created with
    /// (`LocalEventFields::series_recurrence`, #2034), or `None`.
    pub series_recurrence: Option<EventRecurrence>,
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
    /// Each end's named zone and wall clock (#2039, `events.send_start_tz`,
    /// `send_start_local`, `send_end_tz`, `send_end_local`), `""` for UTC.
    pub start_time_zone: String,
    pub start_local: String,
    pub end_time_zone: String,
    pub end_local: String,
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

    /// Begins a write transaction: IMMEDIATE, so it takes the write lock at
    /// `BEGIN` and waits out another connection's write (rusqlite's 5 s busy
    /// timeout) before it reads. The engine and a host (lookxy, its tests)
    /// each hold a connection to one WAL file; a DEFERRED transaction that
    /// read first and then writes after the other connection committed gets
    /// `SQLITE_BUSY_SNAPSHOT` ("database is locked") at once, with no busy
    /// wait, as its snapshot is stale. Every store transaction writes.
    fn write_tx(&self) -> rusqlite::Result<rusqlite::Transaction<'_>> {
        rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)
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
            // Each end's named zone and wall clock (#2039).
            "send_start_tz TEXT NOT NULL DEFAULT ''",
            "send_start_local TEXT NOT NULL DEFAULT ''",
            "send_end_tz TEXT NOT NULL DEFAULT ''",
            "send_end_local TEXT NOT NULL DEFAULT ''",
            // The user's own response time and the creation time (#2068).
            "response_time TEXT NOT NULL DEFAULT ''",
            "created_utc TEXT NOT NULL DEFAULT ''",
        ] {
            let _ = conn.execute(&format!("ALTER TABLE events ADD COLUMN {column}"), []);
        }
        for column in [
            "response_time TEXT NOT NULL DEFAULT ''",
            "proposed_start_utc TEXT NOT NULL DEFAULT ''",
            "proposed_end_utc TEXT NOT NULL DEFAULT ''",
        ] {
            let _ = conn.execute(
                &format!("ALTER TABLE event_attendees ADD COLUMN {column}"),
                [],
            );
        }
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
        // Same idempotent-migration pattern, for the upload session a send
        // opened (#1941). Older rows have none, so their next send opens one.
        let _ = conn.execute(
            "ALTER TABLE outbound_attachments ADD COLUMN upload_url TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE outbound_attachments ADD COLUMN upload_digest TEXT",
            [],
        );
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
        migrate_message_reminders(&conn)?;
        migrate_meeting_message_type(&conn)?;
        migrate_message_protection(&conn)?;
        migrate_meeting_reminder(&conn)?;
        migrate_reply_links(&conn)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_reminder
                 ON messages(reminder_signal_at) WHERE is_reminder_on = 1",
            [],
        )?;
        // After `migrate_reply_links`, never in `schema.rs`: its batch runs
        // before the migrations, when an old table lacks `in_reply_to`.
        conn.execute(REPLY_DRAFTS_INDEX_SQL, [])?;
        register_contact_match(&conn)?;
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
        let tx = self.write_tx()?;
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
    /// column from `m` (`merge_message` with every property carried), save
    /// that a stored non-empty `internet_message_id` or `in_reply_to` is
    /// never replaced by `""` (#2096).
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
    /// `MessageFields::writes_meeting_request` / `writes_is_draft`, the
    /// reminder columns `writes_reminder`, `flag_due_at` the flag (#2054)
    /// `meeting_message_type` `writes_meeting_type` (#2081: only when
    /// the item carried it) and the protection columns `writes_protection`
    /// (#2088: only when the item carried its class), `meeting_reminder_on`
    /// `writes_meeting_reminder` (#2089: likewise, and with no flag term),
    /// `internet_message_id` and `in_reply_to`
    /// `writes_internet_message_id` / `writes_in_reply_to` (#2096: only
    /// when carried) **and** only with a non-empty value, so a stored id is
    /// never cleared, not even by a carried `""` or a `Message` built in
    /// code (`upsert_message`).
    /// The reminder is on only while the row is flagged (the
    /// stored flag when the item carries none), so an unflag or a completed
    /// flag Graph reports leaves it off even if Exchange kept
    /// `PidLidReminderSet`.
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
                 is_meeting_request, categories,
                 is_reminder_on, reminder_at, reminder_signal_at, flag_due_at,
                 meeting_message_type,
                 sensitivity, has_sensitivity_label, sensitivity_label_name, is_rights_protected,
                 meeting_reminder_on, internet_message_id, in_reply_to
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                       ?35 AND ?12, ?36, ?37, ?38, ?40, ?42, ?43, ?44, ?45, ?47, ?49, ?51)
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
                 categories = CASE WHEN ?33 THEN excluded.categories ELSE messages.categories END,
                 is_reminder_on =
                     (CASE WHEN ?39 THEN ?35 ELSE messages.is_reminder_on END)
                     AND (CASE WHEN ?27 THEN excluded.is_flagged ELSE messages.is_flagged END),
                 reminder_at = CASE WHEN ?39 THEN excluded.reminder_at ELSE messages.reminder_at END,
                 reminder_signal_at = CASE WHEN ?39 THEN excluded.reminder_signal_at
                                           ELSE messages.reminder_signal_at END,
                 flag_due_at = CASE WHEN ?27 THEN excluded.flag_due_at ELSE messages.flag_due_at END,
                 meeting_message_type = CASE WHEN ?41 THEN excluded.meeting_message_type
                                             ELSE messages.meeting_message_type END,
                 sensitivity = CASE WHEN ?46 THEN excluded.sensitivity ELSE messages.sensitivity END,
                 has_sensitivity_label = CASE WHEN ?46 THEN excluded.has_sensitivity_label
                                              ELSE messages.has_sensitivity_label END,
                 sensitivity_label_name = CASE WHEN ?46 THEN excluded.sensitivity_label_name
                                               ELSE messages.sensitivity_label_name END,
                 is_rights_protected = CASE WHEN ?46 THEN excluded.is_rights_protected
                                            ELSE messages.is_rights_protected END,
                 meeting_reminder_on = CASE WHEN ?48 THEN excluded.meeting_reminder_on
                                            ELSE messages.meeting_reminder_on END,
                 internet_message_id = CASE WHEN ?50 AND excluded.internet_message_id <> ''
                                            THEN excluded.internet_message_id
                                            ELSE messages.internet_message_id END,
                 in_reply_to = CASE WHEN ?52 AND excluded.in_reply_to <> ''
                                    THEN excluded.in_reply_to ELSE messages.in_reply_to END
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
                m.is_reminder_on,
                m.reminder_at,
                m.reminder_signal_at,
                m.flag_due_at,
                fields.writes_reminder(),
                m.meeting_message_type.map(MeetingMessageType::as_graph),
                fields.writes_meeting_type(),
                m.protection.sensitivity.as_mapi(),
                m.protection.has_label,
                m.protection.label_name,
                m.protection.is_rights_protected,
                fields.writes_protection(),
                m.meeting_reminder_on,
                fields.writes_meeting_reminder(),
                m.internet_message_id,
                fields.writes_internet_message_id(),
                m.in_reply_to,
                fields.writes_in_reply_to(),
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
    /// event ops name events, not messages, and are left out. Each id is
    /// the one its message is stored under now (`current_id`: an op queued
    /// before a move still protects the re-keyed row, #1896).
    pub fn pending_message_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut ids: Vec<String> = self
            .pending_ops()?
            .into_iter()
            .filter_map(|row| row.op.message_id().map(str::to_string))
            .map(|id| self.current_id(&id))
            .collect::<Result<_, _>>()?;
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_ROW_COLUMNS}
             FROM messages
             WHERE folder_id = ?1
             ORDER BY received_at DESC
             LIMIT ?2 OFFSET ?3"
        ))?;
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
                        is_reminder_on, reminder_at, reminder_signal_at, flag_due_at,
                        meeting_message_type,
                        sensitivity, has_sensitivity_label, sensitivity_label_name,
                        is_rights_protected, meeting_reminder_on,
                        internet_message_id, in_reply_to,
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
                    k.bcc_recipients, k.is_meeting_request, k.categories,
                    k.is_reminder_on, k.reminder_at, k.reminder_signal_at, k.flag_due_at,
                    k.meeting_message_type,
                    k.sensitivity, k.has_sensitivity_label, k.sensitivity_label_name,
                    k.is_rights_protected, k.meeting_reminder_on,
                    k.internet_message_id, k.in_reply_to
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
    ///
    /// Clearing the flag turns its reminder off too, as Outlook does
    /// (#2054); its times are kept. Setting it leaves the reminder as it
    /// is (off, until Graph reports one).
    pub fn set_flag(&self, id: &str, flagged: bool) {
        let _ = self.conn.execute(
            "UPDATE messages SET is_flagged = ?1, is_reminder_on = is_reminder_on AND ?1
             WHERE id = ?2",
            params![flagged, id],
        );
    }

    /// Locally turns a message's reminder off (the optimistic half of
    /// `SyncCommand::DismissMessageReminder`, #2054), under exactly `id`
    /// (the engine resolves a moved id first). See `set_read` for why this
    /// doesn't return a `Result`.
    ///
    /// `meeting_reminder_on` goes off too (#2089): the dismiss PATCHes
    /// `PidLidReminderSet` false, the bit it is read from, so this matches
    /// what the next sync reports; a refused PATCH's reread restores it.
    pub fn dismiss_message_reminder(&self, id: &str) {
        let _ = self.conn.execute(
            "UPDATE messages SET is_reminder_on = 0, meeting_reminder_on = 0 WHERE id = ?1",
            params![id],
        );
    }

    /// Locally moves a message reminder's next signal to `utc` (the
    /// optimistic half of `SyncCommand::SnoozeMessageReminder`, #2054), a
    /// store-shaped UTC time, under exactly `id`. The reminder stays as it
    /// is otherwise: snoozing one that is off does not turn it on. See
    /// `set_read` for why this doesn't return a `Result`.
    pub fn snooze_message_reminder(&self, id: &str, utc: &str) {
        let _ = self.conn.execute(
            "UPDATE messages SET reminder_signal_at = ?2 WHERE id = ?1",
            params![id, utc],
        );
    }

    /// The flagged messages, in any folder, whose reminder is on and due at
    /// or before `now_utc` (#2054): their next signal time
    /// (`reminder_signal_at`, else `reminder_at` when Graph sent no signal
    /// time) is not after it. Soonest first, then by id. `now_utc` is read
    /// as a reminder time is (`model::reminder_time`: UTC, `Z` or no
    /// suffix, a fraction allowed) and normalised like every stored time,
    /// as they compare as text; one it cannot read (an offset, not a time)
    /// is a `StoreError::Invalid`, so a caller's bug shows. A client polls
    /// it, as it reads events for their reminders.
    pub fn due_message_reminders(&self, now_utc: &str) -> Result<Vec<MessageRow>, StoreError> {
        let now = crate::graph::model::reminder_time(now_utc);
        if now.is_empty() {
            return Err(StoreError::Invalid(format!(
                "due message reminders: not a UTC time: {now_utc:?}"
            )));
        }
        let mut stmt = self.conn.prepare(&due_message_reminders_sql())?;
        let rows = stmt
            .query_map(params![now], map_message_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The `(conversation_id, in_reply_to)` of every unsent reply draft, in
    /// any folder but Deleted Items (#2096): rows with `is_draft = 1` and a
    /// non-empty `in_reply_to`, each pair once, sorted. A draft the user
    /// discarded moves to Deleted Items with Graph's `isDraft` still true,
    /// so a row filed in the well-known `deleteditems` folder is left out. A host marks a message that
    /// has an unsent reply draft, as classic Outlook's red `[Draft]` does,
    /// when its `conversation_id` and `internet_message_id` match a pair,
    /// without reading the Drafts folder's rows. A sent draft (`mark_sent`)
    /// and a forward (no In-Reply-To) are left out. Reads the partial index
    /// `idx_messages_reply_drafts`.
    pub fn reply_draft_links(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self.conn.prepare(REPLY_DRAFT_LINKS_SQL)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
        let tx = self.write_tx()?;
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
    /// zero rows (not an error). Graph mints a *new* id on move, which the
    /// sync engine's drain applies with `follow_move` (#1896) once the op
    /// succeeds, so the local id is left as-is here. A reconciled draft's
    /// `local:` id still reaches it (#1884; `SendDraft`'s optimistic re-file
    /// into Sent Items); an id from before a move does not (resolve it with
    /// `current_id` first).
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

    /// Records whether the first call of the walk whose link `folder_id`
    /// stores as `delta_link` found Graph refusing the reminder `$expand`
    /// (#2054). The engine writes it with the link itself:
    /// `set_head_link` and a small folder's `complete_backfill` take it,
    /// and an ending backfill's `complete_backfill` moves the walk's own
    /// record. While it is set, a round on that link (which carries no
    /// expand) merges its items as `MessageFields::reminder_unasked`,
    /// keeping the stored reminders. `clear_delta_links` clears it.
    pub fn set_reminder_expand_refused(
        &self,
        folder_id: &str,
        refused: bool,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET reminder_expand_refused = ?2 WHERE id = ?1",
            params![folder_id, refused],
        )?;
        Ok(())
    }

    /// Whether `folder_id`'s `delta_link` came from a walk whose reminder
    /// `$expand` was refused ([`Store::set_reminder_expand_refused`]);
    /// false for an unknown folder.
    pub fn reminder_expand_refused(&self, folder_id: &str) -> Result<bool, StoreError> {
        self.folder_flag(folder_id, "reminder_expand_refused")
    }

    /// Records that the backfill walk under way for `folder_id` had its
    /// first call's reminder `$expand` refused (#2054), so its later pages,
    /// on links that carry none, keep stored reminders too. Moved to the
    /// folder's mark by `complete_backfill`; cleared by `reset_backfill`
    /// and `clear_delta_links`.
    pub fn set_backfill_expand_refused(&self, folder_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET backfill_expand_refused = 1 WHERE id = ?1",
            params![folder_id],
        )?;
        Ok(())
    }

    /// Whether the backfill walk under way for `folder_id` was refused the
    /// expand ([`Store::set_backfill_expand_refused`]); false for an
    /// unknown folder.
    pub fn backfill_expand_refused(&self, folder_id: &str) -> Result<bool, StoreError> {
        self.folder_flag(folder_id, "backfill_expand_refused")
    }

    /// One boolean `folders` column of `folder_id`, false for an unknown
    /// folder. `column` is one of this file's literals, never input.
    fn folder_flag(&self, folder_id: &str, column: &str) -> Result<bool, StoreError> {
        let flag = self
            .conn
            .query_row(
                &format!("SELECT {column} FROM folders WHERE id = ?1"),
                params![folder_id],
                |r| r.get::<_, bool>(0),
            )
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(false)
                } else {
                    Err(e)
                }
            })?;
        Ok(flag)
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
    /// It clears every folder's refused-expand marks (#2054,
    /// [`Store::set_reminder_expand_refused`]) with them.
    ///
    /// And it marks a reconverge pending (`meta.reconverge_pending`, read by
    /// [`Store::reconverge_pending`]), in the same transaction, so an engine
    /// restarted before the reconverge ends still runs it without the
    /// window's cutoff.
    pub fn clear_delta_links(&self) -> Result<(), StoreError> {
        let tx = self.write_tx()?;
        tx.execute(
            "UPDATE folders SET delta_link = NULL, backfill_link = NULL,
                                backfill_synced = 0, history_scope = NULL,
                                backfill_scope = NULL, reminder_expand_refused = 0,
                                backfill_expand_refused = 0",
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
    /// the sync pass), marking what it covers as [`SCOPE_HEAD`], and
    /// whether its walk's first call was refused the reminder `$expand`
    /// (#2054, `refused`): in the same statement, so no reader (and no
    /// crash) ever sees the new link with the old link's mark.
    pub fn set_head_link(
        &self,
        folder_id: &str,
        link: &str,
        refused: bool,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET delta_link = ?2, history_scope = ?3,
                                reminder_expand_refused = ?4
             WHERE id = ?1",
            params![folder_id, link, SCOPE_HEAD, refused],
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
    /// cleared and `synced` is set to the final count. The folder's
    /// refused-reminder-expand mark (#2054) goes with the link, in the same
    /// statement: `refused` when the caller knows it (`Some`: a small
    /// folder walked whole in the sync pass), else the walk's own record
    /// (`None`: `backfill_expand_refused`, a backfill that ends).
    pub fn complete_backfill(
        &self,
        folder_id: &str,
        delta_link: &str,
        scope: &str,
        synced: i64,
        refused: Option<bool>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET delta_link = ?2, history_scope = ?3,
                                backfill_link = NULL, backfill_synced = ?4,
                                backfill_scope = NULL,
                                reminder_expand_refused = COALESCE(?5, backfill_expand_refused),
                                backfill_expand_refused = 0
             WHERE id = ?1",
            params![folder_id, delta_link, scope, synced, refused],
        )?;
        Ok(())
    }

    /// Restarts a folder's backfill from a fresh full delta (#1871: its
    /// page link expired, or a window walk resumed in whole-mailbox mode):
    /// clears the link, the count, the walk's scope and its refused
    /// reminder expand (#2054), and keeps the head's `delta_link`.
    pub fn reset_backfill(&self, folder_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE folders SET backfill_link = NULL, backfill_synced = 0,
                                backfill_scope = NULL, backfill_expand_refused = 0
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
    ///
    /// A local write always lands: the rule that keeps a draft's unpushed
    /// body against a fetch (#2082, [`Store::put_fetched_body_with_headers`])
    /// is not this method's, as this body *is* the local one.
    pub fn put_body(&self, message_id: &str, b: &Body) -> Result<(), StoreError> {
        write_body_on(&self.conn, message_id, b, None)
    }

    /// Inserts or replaces the body of a message as `FetchBody` fetched it,
    /// recording the format it asked Graph for (`fetched_as`, #1901), which
    /// for `Html` can differ from the content type that came back (a
    /// plain-text message's native body is text). Leaves the message's
    /// stored headers as they are, an earlier fetch's included: the engine
    /// stores a fetched body with [`Store::put_fetched_body_with_headers`],
    /// whose rule for a draft's unpushed body (#2082) this follows too
    /// (`Ok(false)`: kept, nothing written). Tests use it.
    #[cfg(test)]
    pub fn put_fetched_body(
        &self,
        message_id: &str,
        b: &Body,
        asked: BodyFormat,
    ) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        if fetched_body_is_kept_on(&tx, message_id)? {
            return Ok(false);
        }
        write_body_on(&tx, message_id, b, Some(asked))?;
        tx.commit()?;
        Ok(true)
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
    ///
    /// A draft's body Graph has not got yet is kept (#2082): when a body is
    /// stored for `message_id` and the row has an unsaved compose edit
    /// (`local_edit > 0`) or a queued `SaveDraft` / `SendDraft` names it (by
    /// the id it is stored under now, [`Store::current_id`]), nothing is
    /// written, neither the body nor the headers, and this returns
    /// `Ok(false)`. That op PATCHes the stored body at drain time, so Graph's
    /// older body written here would be sent back as the draft's. Other ops
    /// push no body and keep nothing; a row with no body yet takes the fetch,
    /// as it holds no user text. The check and the write share one
    /// transaction. `Ok(true)`: written.
    pub fn put_fetched_body_with_headers(
        &self,
        message_id: &str,
        b: &Body,
        asked: BodyFormat,
        headers: &[MessageHeader],
    ) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        if fetched_body_is_kept_on(&tx, message_id)? {
            return Ok(false);
        }
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
        Ok(true)
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
    /// Drafts folder (the synced one, or the local sentinel). Since #2096 a
    /// delta item carries Graph's `isDraft`, but a row stored before then,
    /// or one a code-built `Message` wrote without the flag, may still read
    /// 0 in Drafts, so the folder still counts.
    ///
    /// This decides whether an embedder asks; a fetch that comes anyway
    /// still keeps a draft's unpushed body
    /// ([`Store::put_fetched_body_with_headers`], #2082).
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

    /// The folder a stored message is filed in (by its current id), `None`
    /// when no row has that id. The engine reads it before storing a search
    /// hit (#2006), whose merge re-files a row Graph has elsewhere.
    pub fn message_folder(&self, message_id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT folder_id FROM messages WHERE id = ?1",
                params![message_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// A stored message's list row by its exact current id (#2053), as
    /// `messages_in_folder` maps it (`MESSAGE_ROW_COLUMNS`,
    /// `map_message_row`), whether or not a body is stored. `None` when no
    /// row has that id. Like `message_exists`, no `local:` or moved-id
    /// resolution: pass the id through `current_id` first for that.
    pub fn message_row(&self, message_id: &str) -> Result<Option<MessageRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {MESSAGE_ROW_COLUMNS} FROM messages WHERE id = ?1"),
                params![message_id],
                map_message_row,
            )
            .optional()?)
    }

    /// The list rows of the stored messages among `ids` (#2053), in no
    /// particular order: an id the store does not have is skipped, and an
    /// id given twice gives its row once. Each row is `message_row`'s (exact
    /// current ids). One statement whose one parameter is the ids as a JSON
    /// array read through `json_each` (as `retain_folders` passes a list),
    /// so it has no SQLite variable limit and a concurrent writer cannot
    /// shift what it sees part-way; `messages` is read through its primary
    /// key index (`message_rows_sql`). An empty `ids` runs no statement.
    pub fn message_rows(&self, ids: &[&str]) -> Result<Vec<MessageRow>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json =
            Value::Array(ids.iter().map(|id| Value::Str((*id).to_string())).collect()).to_string();
        let mut stmt = self.conn.prepare(&message_rows_sql())?;
        let rows = stmt
            .query_map(params![ids_json], map_message_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
        let tx = self.write_tx()?;
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
        let tx = self.write_tx()?;
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
            "SELECT path, name, size, uploaded, content_id, content_type, data IS NOT NULL,
                    upload_url, upload_digest
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
                    upload_url: r.get(7)?,
                    upload_digest: r.get(8)?,
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
    /// failed send does not upload it again. Its upload session (#1941) is
    /// forgotten: it is done. No-op if absent.
    pub fn mark_outbound_attachment_uploaded(
        &self,
        draft_id: &str,
        path: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            concat!(
                "UPDATE outbound_attachments
                 SET uploaded = 1, upload_url = NULL, upload_digest = NULL
                 WHERE draft_id = ",
                current_id_sql!(),
                " AND path = ?2"
            ),
            params![draft_id, path],
        )?;
        Ok(())
    }

    /// Records the upload session a send opened for the attachment `path`
    /// of `draft_id` (#1941): `Some((upload_url, upload_digest))`, or `None`
    /// to forget it (Graph refused it). No-op if absent.
    pub fn set_outbound_attachment_upload_session(
        &self,
        draft_id: &str,
        path: &str,
        session: Option<(&str, &str)>,
    ) -> Result<(), StoreError> {
        let (url, digest) = session.unzip();
        self.conn.execute(
            concat!(
                "UPDATE outbound_attachments SET upload_url = ?3, upload_digest = ?4
                 WHERE draft_id = ",
                current_id_sql!(),
                " AND path = ?2"
            ),
            params![draft_id, path, url, digest],
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
        let tx = self.write_tx()?;
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_ROW_COLUMNS},
                    bodies.content_type, bodies.content
             FROM messages JOIN bodies ON bodies.message_id = messages.id
             WHERE {}",
            draft_row_sql!()
        ))?;
        let found = stmt
            .query_map(params![id], |r| {
                Ok((
                    map_message_row(r)?,
                    Body {
                        content_type: r.get(BODY_AFTER_ROW_COLUMNS)?,
                        content: r.get(BODY_AFTER_ROW_COLUMNS + 1)?,
                    },
                ))
            })?
            .next()
            .transpose()?;
        Ok(found)
    }

    /// Whether the draft `id` addresses (its `local:` id or its Graph id,
    /// as [`Store::draft`] takes it) has a body written here (#2096): a body
    /// row with no recorded fetch format (`fetched_as IS NULL`), which only
    /// compose's `update_draft_fields`, `create_local_draft` and a composed
    /// reply/forward's `put_body` write. False for no row, no body, or a
    /// body `FetchBody` stored. A text body an engine from before #1901
    /// fetched (stored with no format) is marked fetched by
    /// `migrate_reply_links` in any folder, so it reads false too. Whether
    /// the row is a draft is the caller's to know (lookxy asks only for
    /// `is_draft` rows); a pre-#1901 non-draft html body still has no
    /// format, and reads true. A host opens only such a draft in its
    /// editor: a draft made elsewhere has no local body, and editing Graph's
    /// rendering of it would save that rendering back over it.
    pub fn draft_body_is_local(&self, id: &str) -> Result<bool, StoreError> {
        let local: i64 = self.conn.query_row(
            concat!(
                "SELECT COUNT(*) FROM bodies
                 WHERE fetched_as IS NULL
                   AND message_id IN (SELECT id FROM messages WHERE ",
                draft_row_sql!(),
                ")"
            ),
            params![id],
            |r| r.get(0),
        )?;
        Ok(local > 0)
    }

    /// Records that Graph moved the message `old_id` and gave it `new_id`
    /// (#1896), and re-keys its row when one is stored. `old_id` then
    /// resolves to `new_id` through `moved_ids` (`current_id`), even once
    /// the row is gone: a message deleted while its move was in flight
    /// (the engine's queued `Delete`, lookxy's own delete) still has its
    /// queued or later `Delete` by the old id reach the id Graph knows now.
    /// Every earlier id that pointed at `old_id` points at `new_id` after
    /// it, so a chain A -> B -> C resolves A and B to C in one lookup. An
    /// alias never names its own row: a move that answers with an earlier
    /// id of the message drops that alias.
    ///
    /// The row's body, stored headers, attachment metadata (whose
    /// attachment ids stay the pre-move ones until the next
    /// `FetchAttachments`) and outbound attachments follow, and the
    /// `messages_fts` index through the triggers, as in `reconcile_id`; a
    /// draft's `local_id` stays on the row. When a row `new_id` already
    /// exists (a delta stored the moved copy first), it is kept: the old
    /// row's children and `local_id` move onto it where it has none of its
    /// own (a body, a header, an attachment of the same id), and the old
    /// row is deleted. One transaction with `defer_foreign_keys` (see
    /// `reconcile_id`).
    ///
    /// Returns whether it recorded the move: nothing happens when `old_id`
    /// equals `new_id` or `new_id` is empty.
    pub fn follow_move(&self, old_id: &str, new_id: &str) -> Result<bool, StoreError> {
        if old_id == new_id || new_id.is_empty() {
            return Ok(false);
        }
        let tx = self.write_tx()?;
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;
        let exists = |id: &str| -> rusqlite::Result<bool> {
            tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM messages WHERE id = ?1)",
                params![id],
                |r| r.get(0),
            )
        };
        const CHILDREN: [&str; 4] = [
            "UPDATE OR IGNORE bodies SET message_id = ?2 WHERE message_id = ?1",
            "UPDATE OR IGNORE message_headers SET message_id = ?2 WHERE message_id = ?1",
            "UPDATE OR IGNORE attachments SET message_id = ?2 WHERE message_id = ?1",
            "UPDATE OR IGNORE outbound_attachments SET draft_id = ?2 WHERE draft_id = ?1",
        ];
        if exists(old_id)? {
            if exists(new_id)? {
                // `local_id` is UNIQUE: off the old row before onto the new.
                let local: Option<String> = tx.query_row(
                    "SELECT local_id FROM messages WHERE id = ?1",
                    params![old_id],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "UPDATE messages SET local_id = NULL WHERE id = ?1",
                    params![old_id],
                )?;
                tx.execute(
                    "UPDATE messages SET local_id = COALESCE(local_id, ?2) WHERE id = ?1",
                    params![new_id, local],
                )?;
                for sql in CHILDREN {
                    tx.execute(sql, params![old_id, new_id])?;
                }
                // What the new row already had stays; the old row's copy
                // goes (`outbound_attachments` has no foreign key).
                tx.execute(
                    "DELETE FROM outbound_attachments WHERE draft_id = ?1",
                    params![old_id],
                )?;
                tx.execute("DELETE FROM messages WHERE id = ?1", params![old_id])?;
            } else {
                tx.execute(
                    "UPDATE messages SET id = ?2 WHERE id = ?1",
                    params![old_id, new_id],
                )?;
                for sql in CHILDREN {
                    tx.execute(sql, params![old_id, new_id])?;
                }
            }
        }
        tx.execute(
            "UPDATE moved_ids SET message_id = ?2 WHERE message_id = ?1",
            params![old_id, new_id],
        )?;
        tx.execute("DELETE FROM moved_ids WHERE old_id = ?1", params![new_id])?;
        tx.execute(
            "INSERT OR REPLACE INTO moved_ids (old_id, message_id) VALUES (?1, ?2)",
            params![old_id, new_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Whether `id` is an id a message had before a move (#1896,
    /// `follow_move`): a delta upsert of it is the pre-move copy Graph has
    /// not removed yet, as Graph never reuses an id.
    pub fn is_moved_alias(&self, id: &str) -> Result<bool, StoreError> {
        let alias = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM moved_ids WHERE old_id = ?1 AND message_id <> ?1)",
            params![id],
            |r| r.get(0),
        )?;
        Ok(alias)
    }

    /// The id the message addressed by `id` is stored under now: the Graph
    /// id a draft's `local:` id was reconciled to (`reconcile_id`, #1884),
    /// then the latest id Graph gave a moved message (`follow_move`, #1896,
    /// so a reconciled draft moved since resolves too; still so once the
    /// message is deleted, for a `Delete` queued by the old id), else `id`
    /// itself (an id never moved, or a `local:` id never reconciled or
    /// whose message has since been deleted).
    ///
    /// Only this function follows a move: `draft_row_sql!`,
    /// `current_id_sql!` and the calls built on them (`delete_message`,
    /// which the delta `@removed` path uses, `set_read`, `move_message`, …)
    /// do not, so a caller holding an id from before a move resolves it
    /// here first.
    pub fn current_id(&self, id: &str) -> Result<String, StoreError> {
        current_id_on(&self.conn, id)
    }

    /// Writes a message's meeting message type (#2081), on the row `id`
    /// resolves to now ([`Store::current_id`], so an id from before a move
    /// still reaches it): what `FetchMeetingMessage`'s answer gives a row
    /// stored before the type was. Returns the row's folder id when the
    /// stored value changed, for the engine's `MessagesUpdated`; `None` when
    /// it already held the value or no row has the id.
    pub fn set_meeting_message_type(
        &self,
        id: &str,
        meeting_type: Option<MeetingMessageType>,
    ) -> Result<Option<String>, StoreError> {
        let tx = self.write_tx()?;
        let id = current_id_on(&tx, id)?;
        let folder = tx
            .query_row(
                "UPDATE messages SET meeting_message_type = ?2
                 WHERE id = ?1 AND meeting_message_type IS NOT ?2
                 RETURNING folder_id",
                params![id, meeting_type.map(MeetingMessageType::as_graph)],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        tx.commit()?;
        Ok(folder)
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
    /// rest of this type's methods), so this uses `write_tx`
    /// (`Transaction::new_unchecked`, IMMEDIATE) — rusqlite's transaction
    /// handle for exactly that shared-borrow case — rather than
    /// `Connection::transaction`, which needs `&mut Connection`. If any
    /// statement fails, the `?` drops the transaction before `commit()`
    /// runs, which rolls it back and leaves the prior attachments
    /// untouched instead of leaving the row set half-replaced.
    pub fn put_attachments(
        &self,
        message_id: &str,
        atts: &[AttachmentMeta],
    ) -> Result<(), StoreError> {
        let tx = self.write_tx()?;
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_ROW_COLUMNS}
             FROM messages
             WHERE id IN (SELECT message_id FROM messages_fts WHERE messages_fts MATCH ?1)
             ORDER BY received_at DESC
             LIMIT ?2"
        ))?;
        let rows = stmt
            .query_map(params![sanitized, limit], map_message_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Queues a local mutation for the sync engine to push to Graph,
    /// returning its `seq` (the `outbox.seq` autoincrement rowid) so a
    /// caller could reference this exact queued op later if needed.
    pub fn enqueue_op(&self, op: &OutboxOp) -> Result<i64, StoreError> {
        enqueue_op_on(&self.conn, op)
    }

    /// All queued ops, oldest (lowest `seq`) first — the order the sync
    /// engine should drain them in.
    pub fn pending_ops(&self) -> Result<Vec<OutboxRow>, StoreError> {
        pending_ops_on(&self.conn)
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

    /// Whether the op `seq` is still queued (#1910): a refusal in a drain
    /// may drop later ops the drain already read.
    pub(crate) fn has_op(&self, seq: i64) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM outbox WHERE seq = ?1)",
            params![seq],
            |r| r.get(0),
        )?)
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
    /// transaction via `write_tx` (same shared-`&self`
    /// reasoning applies here).
    pub fn put_event_attendees(&self, id: &str, a: &[NewAttendee]) -> Result<(), StoreError> {
        let tx = self.write_tx()?;
        put_event_attendees_on(&tx, id, a)?;
        tx.commit()?;
        Ok(())
    }

    /// Every calendar's window at once, the pre-#1984 writer: the engine now
    /// writes a pass per calendar with `apply_calendar_sync`, which keeps
    /// this one's rules (the `local:` exclusion, one transaction).
    ///
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
        let tx = self.write_tx()?;
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

    /// The stored event `id` (`None` if there is none): what
    /// `SyncCommand::CancelEvent` checks before it cancels (#1985).
    pub fn event(&self, id: &str) -> Result<Option<EventRow>, StoreError> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {EVENT_ROW_COLUMNS} FROM events WHERE id = ?1"),
                params![id],
                event_row,
            )
            .optional()?;
        Ok(row)
    }

    /// Events overlapping `[from_utc, to_utc)`: `start_utc < to_utc AND
    /// end_utc > from_utc`, so multi-day/ongoing events that started before
    /// the window (or end after it) still show, not just ones fully
    /// contained in it. Ordered `start_utc ASC`. Only what a grid draws
    /// (`GRID_ROWS`, #2064): no series master once a row of its series is
    /// stored, no cancelled occurrence or exception.
    pub fn events_in_window(
        &self,
        from_utc: &str,
        to_utc: &str,
    ) -> Result<Vec<EventRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {EVENT_ROW_COLUMNS} FROM events
             WHERE start_utc < ?2 AND end_utc > ?1 AND {GRID_ROWS}
             ORDER BY start_utc ASC"
        ))?;
        let rows = stmt
            .query_map(params![from_utc, to_utc], event_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// `events_in_window`'s overlap rule, order and `GRID_ROWS`, restricted
    /// to events of the calendars in `calendar_ids` (#1984). An empty slice returns no
    /// events (not every calendar's).
    pub fn events_in_range(
        &self,
        from_utc: &str,
        to_utc: &str,
        calendar_ids: &[String],
    ) -> Result<Vec<EventRow>, StoreError> {
        if calendar_ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; calendar_ids.len()].join(", ");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {EVENT_ROW_COLUMNS} FROM events
             WHERE start_utc < ?2 AND end_utc > ?1 AND calendar_id IN ({marks})
                   AND {GRID_ROWS}
             ORDER BY start_utc ASC"
        ))?;
        let args = [from_utc, to_utc]
            .into_iter()
            .chain(calendar_ids.iter().map(String::as_str));
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), event_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The days (1..=31, sorted, each once) of `year`-`month` on which an
    /// event of `calendar_ids` falls, in local time at `utc_offset_minutes`
    /// east of UTC (#1984): what a date navigator bolds.
    ///
    /// A timed event marks every local day its `[start, end)` overlaps, so
    /// one ending exactly at local midnight does not mark the next day, and
    /// a zero-length one marks its start day. An all-day event marks the
    /// days of its UTC dates `[start, end)`, without the offset: under
    /// `Prefer: outlook.timezone="UTC"` Graph gives an all-day event's
    /// dates at UTC midnight. The events are `events_in_range`'s, so a
    /// series master marks no day once a row of its series is stored, and
    /// a cancelled occurrence or exception marks none (#2064), while a
    /// cancelled single event counts like any other: it stays on the grid
    /// for Remove from Calendar (#2024).
    ///
    /// One fixed offset, not a time zone: in a month with a DST change, an
    /// event within the shifted hour of local midnight can mark the
    /// neighbouring day. Close enough for bold days. A `month` outside
    /// 1..=12 has no days.
    pub fn days_with_events(
        &self,
        year: i32,
        month: u32,
        utc_offset_minutes: i32,
        calendar_ids: &[String],
    ) -> Result<Vec<u32>, StoreError> {
        if !(1..=12).contains(&month) {
            return Ok(Vec::new());
        }
        let first = days_from_civil(i64::from(year), month, 1);
        let last = first + i64::from(days_in_month(i64::from(year), month));
        let off = i64::from(utc_offset_minutes) * 60;
        // Timed events: the local month's span in UTC; all-day ones: the
        // unshifted dates. One query over both, then classify each event.
        // The lower bound is a second early: the query keeps `end > from`,
        // which would drop a zero-length event at midnight on the 1st.
        let from = (first * 86400).min(first * 86400 - off) - 1;
        let to = (last * 86400).max(last * 86400 - off);
        let events = self.events_in_range(
            &unix_secs_to_iso8601(from),
            &unix_secs_to_iso8601(to),
            calendar_ids,
        )?;
        let mut marked = [false; 31];
        for e in &events {
            let (Some(start), Some(end)) = (
                iso8601_to_unix_secs(&e.start_utc),
                iso8601_to_unix_secs(&e.end_utc),
            ) else {
                continue;
            };
            let shift = if e.is_all_day { 0 } else { off };
            let (start, end) = (start + shift, end + shift);
            let first_day = start.div_euclid(86400);
            // The day holding the last instant before `end`; a zero-length
            // (or inverted) event keeps its start day.
            let last_day = if end > start {
                (end - 1).div_euclid(86400)
            } else {
                first_day
            };
            for day in first_day.max(first)..=last_day.min(last - 1) {
                marked[(day - first) as usize] = true;
            }
        }
        Ok((1..=31u32).filter(|d| marked[*d as usize - 1]).collect())
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
    ///
    /// An occurrence with no body of its own reads its stored series
    /// master's (#2064): the fill refers to the master's body rather than
    /// copying it into every occurrence.
    pub fn event_body(&self, id: &str) -> Result<Option<String>, StoreError> {
        event_body_on(&self.conn, id)
    }

    /// The series masters of `calendar_id`'s blank occurrences (#2064,
    /// `BLANK_OCCURRENCE`), distinct and sorted: what a sync pass fetches
    /// when the master is not stored (`Engine::refresh_calendar`).
    pub fn blank_occurrence_masters(&self, calendar_id: &str) -> Result<Vec<String>, StoreError> {
        let ids = self
            .conn
            .prepare(&format!(
                "SELECT DISTINCT series_master_id FROM events
                 WHERE calendar_id = ?1 AND event_type = 'occurrence'
                       AND series_master_id IS NOT NULL AND id NOT LIKE 'local:%'
                       AND {BLANK_OCCURRENCE}
                 ORDER BY series_master_id"
            ))?
            .query_map(params![calendar_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(ids)
    }

    /// Whether `id` is stored as a `seriesMaster` of `calendar_id` (#2064).
    pub fn has_series_master(&self, calendar_id: &str, id: &str) -> Result<bool, StoreError> {
        let found = self
            .conn
            .query_row(
                "SELECT 1 FROM events
                 WHERE id = ?2 AND calendar_id = ?1 AND event_type = 'seriesMaster'",
                params![calendar_id, id],
                |_| Ok(()),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Inserts a locally-created event with a fresh `local:` id and the given
    /// organizer, so it shows in the agenda immediately (before it syncs to
    /// Graph). `response_status` is `"organizer"`. A `None` field stores what
    /// Graph sets on create (#1983, #1994); a `None` reminder, which Graph
    /// takes from the mailbox's default, is marked `reminder_unset` (its
    /// `CreateEvent` sends none, and `reconcile_created_event` writes back
    /// Graph's). Each end's named zone and wall clock (#2039) are stored for
    /// its `CreateEvent`; a pair `graph::client::event_zone_error` refuses
    /// is an error, with nothing written. The row is filed in
    /// `f.calendar_id` (#2048), or the default calendar when it is `None` or
    /// `""` (`''` until the calendar list is known; the next sync pass
    /// adopts it then); a named calendar that is not stored or not
    /// `can_edit` is an error, with nothing written.
    pub fn create_local_event(
        &self,
        f: &LocalEventFields,
        organizer_name: &str,
        organizer_addr: &str,
    ) -> Result<String, StoreError> {
        check_event_zones(f)?;
        let id = local_draft_id(); // a unique "local:<hex>" id
        let reminder = f.reminder.unwrap_or_default();
        let enum_or =
            |v: &Option<String>, default: &str| non_empty(v).unwrap_or_else(|| default.to_string());
        let tx = self.write_tx()?;
        let calendar_id = create_calendar_on(&tx, &f.calendar_id)?;
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
                event_type: if f.series_recurrence.is_some() || f.recurrence.is_some() {
                    "seriesMaster"
                } else {
                    "singleInstance"
                }
                .to_string(),
                // The lossless recurrence as given (#2034), else the
                // create-side one, which is already in Graph's shape.
                recurrence: f.series_recurrence.clone().or_else(|| {
                    f.recurrence
                        .as_ref()
                        .and_then(|r| EventRecurrence::from_json(&r.to_json()))
                }),
                is_organizer: true,
                join_url: meeting_link_in(&f.location, &f.body_html).unwrap_or_default(),
                calendar_id,
                ..NewEvent::default()
            },
        )?;
        put_event_attendees_on(&tx, &id, &typed_attendees(&f.attendees, &[]))?;
        tx.execute(
            "UPDATE events SET recurrence = ?2, reminder_unset = ?3,
                    send_start_tz = ?4, send_start_local = ?5,
                    send_end_tz = ?6, send_end_local = ?7
             WHERE id = ?1",
            params![
                id,
                encode_send_recurrence(f.recurrence.as_ref(), f.series_recurrence.as_ref()),
                f.reminder.is_none(),
                f.start_time_zone,
                zone_local(&f.start_time_zone, &f.start_local),
                f.end_time_zone,
                zone_local(&f.end_time_zone, &f.end_local),
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
    /// response time, and its type when the input's is `""`. Recurrence,
    /// `recurrence` and `series_recurrence` alike, is create-only: never
    /// written here, never in the set.
    ///
    /// `join_url` follows the new location and body (`meeting_link_in`, as
    /// `create_local_event` derives it), except on an event stored as an
    /// online meeting, whose join link is Graph's `onlineMeeting.joinUrl`,
    /// which no location or body edit changes (#1983), and which Graph keeps
    /// even when an edit sends `isOnlineMeeting: false`.
    ///
    /// Each end's named zone (#2039): `TIMES` is marked only when
    /// `start_utc`, `end_utc` or `is_all_day` changed, never by a zone or
    /// wall clock alone (a form always has a zone, and a synced row stores
    /// `""`, so a title edit would otherwise re-send a meeting's times). A
    /// named pair is always written; a `""` zone keeps the stored pair when
    /// `TIMES` is not marked and clears it when it is. So whenever `TIMES`
    /// is sent, each end's pair was given with the edit that last moved the
    /// times, or later, or is `""` (UTC): a stale wall clock is never sent.
    /// Changing only the zone at the same instant is therefore not sent to
    /// Graph (a series can, through `SyncCommand::UpdateEventSeries`). A
    /// named pair `graph::client::event_zone_error` refuses is an error,
    /// with nothing written.
    pub fn update_event_fields(
        &self,
        id: &str,
        f: &LocalEventFields,
    ) -> Result<EventFieldSet, StoreError> {
        check_event_zones(f)?;
        let tx = self.write_tx()?;
        let mut old = event_for_send_on(&tx, id)?
            .ok_or_else(|| StoreError::Sql(format!("no event stored for {id}")))?;
        // An occurrence with no body of its own shows its master's (#2064,
        // `Store::event_body`): compare the edit against that, so a form
        // that leaves it alone does not mark `BODY`. Only here: the send
        // (`event_for_send`) reads the row's own body, so a cleared body
        // is sent cleared (r4). The row's own body is kept for the write.
        let own_body = old.body_html.clone();
        if old.body_html.is_empty() {
            old.body_html = event_body_on(&tx, id)?.unwrap_or_default();
        }
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
        // An edit that leaves the body alone keeps the row's own (r5): for
        // an occurrence shown its master's body, writing the shown one
        // would detach it from its master, and would put the master's body
        // into a row whose queued clear (`BODY`) then sends it.
        let body_html = if f.body_html != old.body_html {
            f.body_html.as_str()
        } else {
            own_body.as_str()
        };
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
                body_html,
                // The shown body, the user's: for a filled occurrence that
                // is its master's, whose link it already has.
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
        // Each end's named zone (#2039): a named pair is written as given; a
        // `""` one keeps the stored pair, unless the times changed, which
        // clears it so `TIMES` is sent in UTC and never with a stale wall
        // clock. A zone alone never marks `TIMES` (a form always has one,
        // and a synced row stores `""`).
        let times_changed = changed.contains(EventFieldSet::TIMES);
        let end_pair = |zone: &str, local: &str, old_zone: &str, old_local: &str| {
            if !zone.is_empty() {
                (zone.to_string(), local.to_string())
            } else if times_changed {
                (String::new(), String::new())
            } else {
                (old_zone.to_string(), old_local.to_string())
            }
        };
        let (start_tz, start_local) = end_pair(
            &f.start_time_zone,
            &f.start_local,
            &old.start_time_zone,
            &old.start_local,
        );
        let (end_tz, end_local) = end_pair(
            &f.end_time_zone,
            &f.end_local,
            &old.end_time_zone,
            &old.end_local,
        );
        tx.execute(
            "UPDATE events SET send_start_tz = ?2, send_start_local = ?3,
                    send_end_tz = ?4, send_end_local = ?5
             WHERE id = ?1",
            params![id, start_tz, start_local, end_tz, end_local],
        )?;
        tx.commit()?;
        Ok(changed)
    }

    /// Removes an event (and its attendees) locally.
    pub fn delete_event(&self, id: &str) -> Result<(), StoreError> {
        let tx = self.write_tx()?;
        tx.execute(
            "DELETE FROM event_attendees WHERE event_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM events WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// Removes a series locally (#2022): the row `master_id` (a local
    /// series is its own row; a synced master is stored when a sync round
    /// returned or fetched it, #2064) and every row
    /// whose `series_master_id` it is, in any calendar, with their
    /// attendees, in one transaction. Returns how many events went. The
    /// host calls it before `SyncCommand::DeleteEvent { id: master_id }`,
    /// as it calls `delete_event` before deleting one event.
    pub fn delete_event_series(&self, master_id: &str) -> Result<usize, StoreError> {
        let tx = self.write_tx()?;
        tx.execute(
            "DELETE FROM event_attendees WHERE event_id IN
                (SELECT id FROM events WHERE id = ?1 OR series_master_id = ?1)",
            params![master_id],
        )?;
        let n = tx.execute(
            "DELETE FROM events WHERE id = ?1 OR series_master_id = ?1",
            params![master_id],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// The non-empty `calendar_id` of a stored row of the series
    /// `master_id` (#2022): the master's own row, else an occurrence whose
    /// `series_master_id` it is. `None` when no such row is stored.
    pub fn series_calendar_id(&self, master_id: &str) -> Result<Option<String>, StoreError> {
        let id = self
            .conn
            .query_row(
                "SELECT calendar_id FROM events
                 WHERE (id = ?1 OR series_master_id = ?1) AND calendar_id <> ''
                 ORDER BY id = ?1 DESC, id LIMIT 1",
                params![master_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id)
    }

    /// Where a stored event's `CreateEvent` POSTs (#2048): `None` for
    /// `/me/events` (the default calendar) when its `calendar_id` is `''`
    /// or the stored default calendar's id, so a create in the default
    /// calendar sends what it sent before #2048, else `Some(id)` for
    /// `/me/calendars/{id}/events`. Read at send time, so a `''` row a sync
    /// pass adopted since still goes to `/me/events`. `None` for no such
    /// event.
    pub fn event_create_calendar(&self, id: &str) -> Result<Option<String>, StoreError> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT calendar_id FROM events WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(calendar) = stored.filter(|c| !c.is_empty()) else {
            return Ok(None);
        };
        if default_calendar_id_on(&self.conn)?.as_deref() == Some(calendar.as_str()) {
            return Ok(None);
        }
        Ok(Some(calendar))
    }

    /// Reads a stored event's send-relevant fields (`None` if no such event).
    pub fn event_for_send(&self, id: &str) -> Result<Option<EventSendData>, StoreError> {
        event_for_send_on(&self.conn, id)
    }

    /// Re-points a `local:` event id to its Graph id after `create_event`
    /// (mirrors `reconcile_id` for drafts) and writes back what only Graph
    /// decides on a create (#1994), in one transaction: the reminder (the
    /// host's, or the mailbox default when it set none), and an online
    /// meeting's join link and provider when Graph returned them, and its
    /// creation and response times (#2068). Clears `reminder_unset`, so a
    /// Graph-id row never keeps it.
    pub fn reconcile_created_event(
        &self,
        local_id: &str,
        created: &Event,
    ) -> Result<(), StoreError> {
        let graph_id = created.id.as_str();
        let tx = self.write_tx()?;
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
        // Graph's creation time (#2068): a just-sent meeting shows its
        // "Sent" date at once, not after a delta round names it. An
        // organizer's response time is Graph's "never", `''`.
        tx.execute(
            "UPDATE events SET created_utc = ?2, response_time = ?3 WHERE id = ?1",
            params![graph_id, created.created_utc, created.response_time],
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
    /// `"declined"`, `"tentativelyAccepted"`, ...) and when it was given
    /// (`at`, canonical UTC, #2068: Graph's `responseStatus.time` is the time
    /// of the response) — the optimistic half of an RSVP the outbox pushes to
    /// Graph. Status and time are written together, so the row never shows an
    /// answer with another answer's time. The caller passes the time, which
    /// keeps the store clock-free. See `set_read` for why this doesn't return
    /// a `Result`.
    pub fn set_event_response(&self, id: &str, status: &str, at: &str) {
        let _ = self.conn.execute(
            "UPDATE events SET response_status = ?1, response_time = ?3 WHERE id = ?2",
            params![status, id, at],
        );
    }

    /// Writes the calendar list `GET /me/calendars` returned (#1984), in its
    /// order (`position`), in one transaction: the stored list becomes
    /// exactly `calendars` (a calendar Graph no longer lists is deleted; a
    /// repeated id keeps its first place). Returns whether the stored list
    /// changed (any field, or the order), so the engine announces
    /// `CalendarsUpdated` only then. The selection (`set_calendar_selection`)
    /// is kept apart from this list, so a chosen calendar that vanishes and
    /// comes back is still chosen.
    pub fn replace_calendars(&self, calendars: &[Calendar]) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        let before = stored_calendars_on(&tx)?;
        let mut seen = HashSet::new();
        let after: Vec<Calendar> = calendars
            .iter()
            .filter(|c| seen.insert(c.id.clone()))
            .cloned()
            .collect();
        if before == after {
            return Ok(false);
        }
        tx.execute("DELETE FROM calendars", [])?;
        for (position, c) in after.iter().enumerate() {
            tx.execute(
                "INSERT INTO calendars (id, name, color, hex_color, is_default, can_edit,
                                        owner_name, owner_addr, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    c.id,
                    c.name,
                    c.color,
                    c.hex_color,
                    c.is_default,
                    c.can_edit,
                    c.owner_name,
                    c.owner_addr,
                    position as i64
                ],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// The stored calendars in Graph's order, each with whether it syncs
    /// (`synced_calendar_ids`).
    pub fn calendars(&self) -> Result<Vec<CalendarRow>, StoreError> {
        let synced: HashSet<String> = synced_calendar_ids_on(&self.conn)?.into_iter().collect();
        Ok(stored_calendars_on(&self.conn)?
            .into_iter()
            .map(|c| CalendarRow {
                syncs: synced.contains(&c.id),
                calendar: c,
            })
            .collect())
    }

    /// The ids of the calendars the engine syncs, in `position` order: the
    /// stored calendars a host chose (`set_calendar_selection`), or, with no
    /// choice, the user's own. A calendar is the user's own when its owner
    /// address equals the default calendar's owner address (ASCII case and
    /// surrounding whitespace ignored, as `is_contact` folds); the default
    /// calendar always is. With no default calendar, or one without an owner
    /// address, every calendar counts as own. A chosen id Graph does not
    /// list syncs nothing until it does.
    pub fn synced_calendar_ids(&self) -> Result<Vec<String>, StoreError> {
        synced_calendar_ids_on(&self.conn)
    }

    /// The host's explicit choice of calendars (`SyncCommand::SetCalendars`),
    /// `None` when it made none (the own-calendars default). Kept in `meta`
    /// (`calendar_selection`, a JSON array), so it survives a restart. A
    /// stored value that is not a JSON array of strings reads as `None`.
    pub fn calendar_selection(&self) -> Result<Option<Vec<String>>, StoreError> {
        calendar_selection_on(&self.conn)
    }

    /// Stores (`Some`) or clears (`None`, back to the own-calendars default)
    /// the host's choice of calendars, and, in the same transaction, deletes
    /// the synced (non-`local:`) events of every **stored** calendar that no
    /// longer syncs. Events of an id the list does not hold, and pre-#1984
    /// rows without a calendar id, are left to the next sync pass
    /// (`apply_calendar_sync`), which runs only after a successful list: so
    /// a host restoring its choice at startup, before the first list,
    /// deletes nothing. Returns whether the synced set changed.
    pub fn set_calendar_selection(&self, ids: Option<&[String]>) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        let before = synced_calendar_ids_on(&tx)?;
        match ids {
            Some(ids) => {
                let list = Value::Array(ids.iter().map(|i| Value::Str(i.clone())).collect());
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES ('calendar_selection', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![list.to_string()],
                )?;
            }
            None => {
                tx.execute("DELETE FROM meta WHERE key = 'calendar_selection'", [])?;
            }
        }
        let after = synced_calendar_ids_on(&tx)?;
        let synced: HashSet<&str> = after.iter().map(String::as_str).collect();
        for c in stored_calendars_on(&tx)? {
            if !synced.contains(c.id.as_str()) {
                delete_calendar_events_on(&tx, &c.id)?;
            }
        }
        tx.commit()?;
        Ok(before != after)
    }

    /// Writes one sync pass over `[from_utc, to_utc)` (#1984), in one
    /// transaction, so no event disappears between a calendar's delete and
    /// its refill:
    ///
    /// 1. Pre-#1984 rows (`calendar_id = ''`, `local:` ones too) move to the
    ///    stored default calendar's id, when there is one. Sync read only
    ///    the default calendar before #1984, and rows that slid out of the
    ///    window are history a date navigator shows, so they are adopted,
    ///    never deleted.
    /// 2. For each calendar in `fetched`: an initial round
    ///    (`CalendarRound::Window`) replaces its synced events overlapping
    ///    the window (`events_in_window`'s rule; `local:` rows excluded, as
    ///    in `replace_events_in_window`) with the fetched ones; a delta
    ///    round (`CalendarRound::Delta`, #2013) upserts the fetched ones and
    ///    deletes, of that calendar's non-`local:` rows overlapping the
    ///    window, each removed id's row and, unless the round upserts
    ///    occurrences of it, the rows whose `series_master_id` it is
    ///    (`apply_calendar_delta_on`). Either way each event is stored
    ///    under that calendar's id, with its attendees, the window is
    ///    recorded as fetched at `stamp` (#2014, `calendar_range_gaps`),
    ///    and the window and deltaLink become the calendar's delta state.
    ///    Except when a delta round's series cascade deleted occurrences:
    ///    then the calendar's delta state is deleted and the window not
    ///    recorded, so the next pass runs an initial round, which restores
    ///    occurrences Graph still has (a master listed as removed may only
    ///    have changed). A synced calendar missing from `fetched`
    ///    (its view failed) keeps its rows, its delta state, and records
    ///    nothing.
    /// 3. Every synced event, of any date, whose non-empty `calendar_id` is
    ///    not in `synced_calendar_ids` is deleted: a deselected calendar, or
    ///    one Graph no longer lists. `''` rows (left only when no default is
    ///    known) and `local:` rows are kept. The fetched ranges and the
    ///    delta state of every calendar not synced go too, events or not
    ///    (#2014, #2013).
    pub fn apply_calendar_sync(
        &self,
        from_utc: &str,
        to_utc: &str,
        fetched: &[SyncedCalendar],
        stamp: RangeStamp,
    ) -> Result<(), StoreError> {
        let tx = self.write_tx()?;
        adopt_blank_calendar_rows_on(&tx)?;
        for c in fetched {
            // Read before the round's writes replace the masters.
            let changed = changed_masters_on(&tx, &c.fetched)?;
            let cascaded = match &c.round {
                CalendarRound::Window => {
                    replace_calendar_range_on(&tx, from_utc, to_utc, &c.fetched, stamp, false)?;
                    false
                }
                CalendarRound::Delta {
                    removed,
                    gone_masters,
                } => apply_calendar_delta_on(
                    &tx,
                    from_utc,
                    to_utc,
                    &c.fetched,
                    removed,
                    gone_masters,
                    stamp,
                )?,
            };
            fill_occurrences_on(&tx, &c.fetched, &changed)?;
            if cascaded {
                tx.execute(
                    "DELETE FROM calendar_deltas WHERE calendar_id = ?1",
                    params![c.fetched.calendar_id],
                )?;
                continue;
            }
            tx.execute(
                "INSERT INTO calendar_deltas (calendar_id, from_utc, to_utc, delta_link)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(calendar_id) DO UPDATE SET from_utc = excluded.from_utc,
                     to_utc = excluded.to_utc, delta_link = excluded.delta_link",
                params![c.fetched.calendar_id, from_utc, to_utc, c.delta_link],
            )?;
        }
        let synced: HashSet<String> = synced_calendar_ids_on(&tx)?.into_iter().collect();
        let stored: Vec<String> = tx
            .prepare("SELECT DISTINCT calendar_id FROM events WHERE calendar_id <> ''")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        for id in stored.iter().filter(|id| !synced.contains(*id)) {
            delete_calendar_events_on(&tx, id)?;
        }
        for table in ["calendar_ranges", "calendar_deltas"] {
            let kept: Vec<String> = tx
                .prepare(&format!("SELECT DISTINCT calendar_id FROM {table}"))?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            for id in kept.iter().filter(|id| !synced.contains(*id)) {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE calendar_id = ?1"),
                    params![id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Writes one calendar's events fetched over `[from_utc, to_utc)` by a
    /// `FetchCalendarRange` (#2014), in one transaction: pre-#1984 rows
    /// are adopted into the default calendar (`apply_calendar_sync`'s step
    /// 1), then the calendar's synced (non-`local:`) events overlapping the
    /// range are replaced by `f`'s, and the range is recorded as fetched at
    /// `stamp`. Other calendars' rows, and rows of other calendars that no
    /// longer sync, are left to the sync pass.
    pub fn apply_calendar_range(
        &self,
        from_utc: &str,
        to_utc: &str,
        f: &FetchedCalendar,
        stamp: RangeStamp,
    ) -> Result<(), StoreError> {
        let tx = self.write_tx()?;
        adopt_blank_calendar_rows_on(&tx)?;
        replace_calendar_range_on(&tx, from_utc, to_utc, f, stamp, true)?;
        tx.commit()?;
        Ok(())
    }

    /// The parts of `[from_utc, to_utc)` that `calendar_id`'s fetched
    /// ranges do not cover fresh (#2014), sorted and disjoint; empty when
    /// the whole range is fresh. A range is fresh while its `fetched_at >
    /// fresh_since` (strictly: with `fresh_since` = now, nothing is fresh).
    /// Bounds are the store's fixed-width `YYYY-MM-DDTHH:MM:SSZ`, which
    /// compare as strings.
    pub fn calendar_range_gaps(
        &self,
        calendar_id: &str,
        from_utc: &str,
        to_utc: &str,
        fresh_since: i64,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let covered: Vec<(String, String)> = self
            .conn
            .prepare(
                "SELECT from_utc, to_utc FROM calendar_ranges
                 WHERE calendar_id = ?1 AND fetched_at > ?2
                       AND from_utc < ?4 AND to_utc > ?3
                 ORDER BY from_utc",
            )?
            .query_map(params![calendar_id, fresh_since, from_utc, to_utc], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut gaps = Vec::new();
        let mut cursor = from_utc.to_string();
        for (a, b) in covered {
            if cursor.as_str() >= to_utc {
                break;
            }
            if a > cursor {
                gaps.push((cursor.clone(), a.min(to_utc.to_string())));
            }
            if b > cursor {
                cursor = b;
            }
        }
        if cursor.as_str() < to_utc {
            gaps.push((cursor, to_utc.to_string()));
        }
        Ok(gaps)
    }

    /// `calendar_id`'s calendarView delta state (#2013), `None` before
    /// its first round, or once its events were forgotten.
    pub fn calendar_delta(
        &self,
        calendar_id: &str,
    ) -> Result<Option<CalendarDeltaState>, StoreError> {
        let state = self
            .conn
            .query_row(
                "SELECT from_utc, to_utc, delta_link FROM calendar_deltas
                 WHERE calendar_id = ?1",
                params![calendar_id],
                |row| {
                    Ok(CalendarDeltaState {
                        from_utc: row.get(0)?,
                        to_utc: row.get(1)?,
                        delta_link: row.get(2)?,
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
        Ok(state)
    }

    /// Forgets every calendar's delta state (#2013), so the next pass runs
    /// initial rounds that replace each window with Graph's truth. A
    /// quarantined event op needs that: its optimistic local write is on
    /// an event Graph never changed, which no delta round names.
    pub fn clear_calendar_deltas(&self) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM calendar_deltas", [])?;
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

    /// Ranked autocomplete matches for `query`, matched case-insensitively
    /// against name and address in any script (#1890).
    ///
    /// Both sides are folded in Rust (`fold_contact_text`: Turkish `İ` as
    /// `i`, Unicode `to_lowercase`, final `ς` as `σ`) by the `contact_match`
    /// SQL function, never by SQLite's `lower()`/`LIKE`, which fold ASCII
    /// only in this ICU-less bundled build. The match is a literal substring
    /// (`_`, `%` and `\` are plain characters).
    ///
    /// A word-prefix match ranks first: the query starts the name or the
    /// address, or starts right after a separator (a character that is not
    /// alphanumeric: a space, `-`, `'`, `’`, `,` …; a combining mark or a
    /// zero-width joiner is not one) in the name or in the address' local
    /// part (before its `@`; never in the domain). A name that is itself an
    /// address (an `@`, no whitespace) takes the address rule. Any other
    /// occurrence ranks after those. Then Graph relevance (lower first,
    /// nulls last), frequency (higher first), recency, name.
    ///
    /// An empty `query` matches every contact, in that tie order. A
    /// negative `limit` means no limit (SQLite's `LIMIT -1`). A caller that
    /// does its own matching reads `all_contacts` instead.
    pub fn search_contacts(&self, query: &str, limit: i64) -> Result<Vec<Contact>, StoreError> {
        let q = fold_contact_text(query);
        let mut stmt = self.conn.prepare(
            "SELECT address, name, source, last_seen, frequency, relevance FROM (
                 SELECT *, contact_match(name, address, ?1) AS m FROM contacts)
             WHERE m IS NOT NULL
             ORDER BY
                 m ASC,
                 (relevance IS NULL) ASC,
                 relevance ASC,
                 frequency DESC,
                 last_seen DESC,
                 name ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![q, limit], contact_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every contact, in `search_contacts`' tie order: Graph relevance
    /// (lower first, nulls last), frequency (higher first), recency, name.
    /// For a caller that does its own matching (#1890).
    pub fn all_contacts(&self) -> Result<Vec<Contact>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT address, name, source, last_seen, frequency, relevance
             FROM contacts
             ORDER BY
                 (relevance IS NULL) ASC,
                 relevance ASC,
                 frequency DESC,
                 last_seen DESC,
                 name ASC",
        )?;
        let rows = stmt
            .query_map([], contact_from_row)?
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

        let tx = self.write_tx()?;
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

    /// Replaces the stored Contacts folder (#1934, #2063) in one transaction
    /// with `contacts`, what `GraphClient::contacts` fetched: every stored
    /// contact Graph no longer lists is removed (and stops counting for
    /// `is_contact`), every listed one written, keyed by its Graph id, except
    /// where a local write has not reached Graph yet:
    ///
    /// - a `local:` row (a create not sent yet) is never removed;
    /// - a contact a queued `UpdateContact` names (its id resolved through
    ///   `contact_aliases`) keeps its stored row: the server's version would
    ///   undo the edit before the PATCH sends it;
    /// - a contact a queued `DeleteContact` names is not written back.
    ///
    /// The queue is read inside the same transaction. A repeated id keeps its
    /// first record. The legacy #1934 address table is emptied. Never touches
    /// `contacts` (people and mined correspondents).
    pub fn replace_contacts(&self, contacts: &[FolderContact]) -> Result<(), StoreError> {
        let tx = self.write_tx()?;
        let QueuedContacts {
            edited, deleted, ..
        } = queued_contacts_on(&tx)?;
        let mut seen = HashSet::new();
        let listed: Vec<&FolderContact> = contacts
            .iter()
            .filter(|c| !c.id.is_empty() && seen.insert(c.id.as_str()))
            .collect();
        let stored: Vec<String> = {
            let mut stmt = tx.prepare("SELECT id FROM contact_records")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        for id in &stored {
            if !seen.contains(id.as_str()) && !id.starts_with("local:") && !edited.contains(id) {
                tx.execute("DELETE FROM contact_records WHERE id = ?1", params![id])?;
            }
        }
        for c in listed {
            if deleted.contains(&c.id) {
                continue;
            }
            if edited.contains(&c.id) && read_contact_on(&tx, &c.id)?.is_some() {
                continue;
            }
            write_contact_on(&tx, &c.id, &c.change_key, &c.fields)?;
        }
        tx.execute("DELETE FROM folder_contacts", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Every stored contact of the Contacts folder (#2063), ordered for a
    /// list: by `file_as`, or `display_name` where `file_as` is empty,
    /// ignoring ASCII case, then by id. A contact created locally is listed
    /// under its `local:` id until its create reaches Graph.
    pub fn folder_contacts(&self) -> Result<Vec<FolderContact>, StoreError> {
        let ids: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT id FROM contact_records
                 ORDER BY CASE WHEN file_as <> '' THEN file_as ELSE display_name END
                          COLLATE NOCASE, id",
            )?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.extend(read_contact_on(&self.conn, &id)?);
        }
        Ok(out)
    }

    /// One stored contact by id (#2063): its Graph id, or the `local:` id it
    /// was created under (followed through `contact_aliases` once its create
    /// re-keyed it). `None` when no such contact is stored.
    pub fn folder_contact(&self, id: &str) -> Result<Option<FolderContact>, StoreError> {
        let id = resolve_contact_id_on(&self.conn, id)?;
        read_contact_on(&self.conn, &id)
    }

    /// Writes a new contact locally (#2063) under a fresh `local:` id, which
    /// it returns, and queues its `OutboxOp::CreateContact` in the same
    /// transaction, so no full pass or re-read can see the row without the
    /// op that protects it. `SyncCommand::CreateContact` then pushes it now;
    /// without the command the next drain (any tick) sends it. Blank phone
    /// and category entries are dropped.
    pub fn create_local_contact(&self, fields: &ContactFields) -> Result<String, StoreError> {
        let id = local_draft_id();
        let tx = self.write_tx()?;
        write_contact_on(&tx, &id, "", fields)?;
        enqueue_op_on(&tx, &OutboxOp::CreateContact { id: id.clone() })?;
        tx.commit()?;
        Ok(id)
    }

    /// Writes an edit of a stored contact (#2063) and returns the properties
    /// it changed: what the PATCH sends. When the set is not empty it queues
    /// `OutboxOp::UpdateContact` with it in the same transaction (under the
    /// id the contact is stored as), so the edit is protected from a full
    /// pass and a refused write's re-read from the moment it is written;
    /// `SyncCommand::UpdateContact` then pushes it now, and without the
    /// command the next drain sends it. Empty when nothing changed or no such
    /// contact is stored: then nothing is written or queued. `id` may be the
    /// `local:` id the contact was created under. The stored `change_key` is
    /// kept. Blank phone and category entries are dropped before the
    /// comparison, so what is compared is what is stored.
    pub fn update_contact_fields(
        &self,
        id: &str,
        fields: &ContactFields,
    ) -> Result<ContactFieldSet, StoreError> {
        let fields = tidy_contact_fields(fields);
        let tx = self.write_tx()?;
        let id = resolve_contact_id_on(&tx, id)?;
        let Some(old) = read_contact_on(&tx, &id)? else {
            return Ok(ContactFieldSet::default());
        };
        let changed = ContactFieldSet::changed(&old.fields, &fields);
        if !changed.is_empty() {
            write_contact_on(&tx, &id, &old.change_key, &fields)?;
            enqueue_op_on(
                &tx,
                &OutboxOp::UpdateContact {
                    id: id.clone(),
                    fields: changed,
                },
            )?;
            tx.commit()?;
        }
        Ok(changed)
    }

    /// Removes a stored contact (#2063; its email rows go with it) and, when
    /// a row was removed, queues `OutboxOp::DeleteContact` for it in the same
    /// transaction; `SyncCommand::DeleteContact` then pushes it now, and
    /// without the command the next drain sends it. `id` may be the `local:`
    /// id it was created under. Whether a row was removed.
    pub fn delete_contact(&self, id: &str) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        let id = resolve_contact_id_on(&tx, id)?;
        let n = tx.execute("DELETE FROM contact_records WHERE id = ?1", params![id])?;
        if n > 0 {
            enqueue_op_on(&tx, &OutboxOp::DeleteContact { id })?;
            tx.commit()?;
        }
        Ok(n > 0)
    }

    /// Deletes the row of a contact whose create Graph refused (#2063), and
    /// queues nothing: Graph has no such contact to delete.
    pub(crate) fn discard_contact_row(&self, id: &str) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM contact_records WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Brings a contact back to Graph's version after a refused write
    /// (#2063): `server` is the record `GET /me/contacts/{id}` returned
    /// (written over the row, or added back after a refused delete), `None`
    /// when Graph answered 404 (the row is deleted). Queues nothing. In one
    /// transaction it first checks the queue: while any contact op names the
    /// contact (an edit written since the re-read began), nothing is written
    /// and `false` is returned, the re-read still owed; otherwise the owed
    /// re-read (`owe_contact_reread`) is cleared and `true` returned.
    pub(crate) fn revert_folder_contact(
        &self,
        id: &str,
        server: Option<&FolderContact>,
    ) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        if queued_contacts_on(&tx)?.any.contains(id) {
            return Ok(false);
        }
        match server {
            Some(c) => write_contact_on(&tx, id, &c.change_key, &c.fields)?,
            None => {
                tx.execute("DELETE FROM contact_records WHERE id = ?1", params![id])?;
            }
        }
        tx.execute("DELETE FROM contact_reread_owed WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(true)
    }

    /// Records that the contact `id` (as stored now) must be read again from
    /// Graph after a refused write (#2063), once no queued op names it.
    /// Persisted, so a restart before then keeps it.
    pub(crate) fn owe_contact_reread(&self, id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO contact_reread_owed (id) VALUES (?1)",
            params![id],
        )?;
        Ok(())
    }

    /// The owed re-reads (`owe_contact_reread`), each with whether a queued
    /// contact op still names it. A plain read, no write lock: it runs on
    /// every drain, and `revert_folder_contact` checks the queue again
    /// inside its own write transaction.
    pub(crate) fn owed_contact_rereads(&self) -> Result<Vec<(String, bool)>, StoreError> {
        let ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM contact_reread_owed ORDER BY id")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?
        };
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let queued = queued_contacts_on(&self.conn)?.any;
        Ok(ids
            .into_iter()
            .map(|id| {
                let q = queued.contains(&id);
                (id, q)
            })
            .collect())
    }

    /// Gives up an owed re-read (#2063): Graph refused it for good (a 4xx
    /// other than 404, an answer that did not parse), so the row is left for
    /// the next full pass to replace.
    pub(crate) fn forget_contact_reread(&self, id: &str) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM contact_reread_owed WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Re-keys a contact created locally to the id Graph gave it (#2063), in
    /// one transaction: the row and its email rows (`ON UPDATE CASCADE`) move
    /// to `created.id`, `change_key` is Graph's, and `display_name` /
    /// `file_as` take Graph's computed values where the stored ones are
    /// empty. No other field is overwritten: an edit made while the POST was
    /// in flight is kept, and its queued `UpdateContact` sends it. The alias
    /// `local_id → created.id` is recorded even when the row is gone (deleted
    /// before Graph answered), so a `DeleteContact` queued by the `local:` id
    /// still reaches the Graph contact.
    pub fn reconcile_created_contact(
        &self,
        local_id: &str,
        created: &FolderContact,
    ) -> Result<(), StoreError> {
        let tx = self.write_tx()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM contact_records WHERE id = ?1)",
            params![local_id],
            |r| r.get(0),
        )?;
        if exists && local_id != created.id {
            // A stale row under the Graph id cannot be Graph's newer copy of
            // a contact it only just created; the local row wins.
            tx.execute(
                "DELETE FROM contact_records WHERE id = ?1",
                params![created.id],
            )?;
            tx.execute(
                "UPDATE contact_records SET
                     id = ?2,
                     change_key = ?3,
                     display_name = CASE WHEN display_name = '' THEN ?4 ELSE display_name END,
                     file_as = CASE WHEN file_as = '' THEN ?5 ELSE file_as END
                 WHERE id = ?1",
                params![
                    local_id,
                    created.id,
                    created.change_key,
                    created.fields.display_name,
                    created.fields.file_as
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO contact_aliases (local_id, id) VALUES (?1, ?2)
             ON CONFLICT(local_id) DO UPDATE SET id = excluded.id",
            params![local_id, created.id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The id the contact `id` names is stored under now (#2063): the Graph
    /// id a `local:` id was re-keyed to (`reconcile_created_contact`), else
    /// `id` itself.
    pub fn resolve_contact_id(&self, id: &str) -> Result<String, StoreError> {
        resolve_contact_id_on(&self.conn, id)
    }

    /// Whether `address` is in the user's own Contacts folder, as stored
    /// (`replace_contacts`, and the local writes since): an exact match
    /// ignoring ASCII case and surrounding whitespace, never a substring
    /// (`folder_contact_key`), against any address of any stored contact.
    /// `/me/people` and mined correspondents (`contacts`) never count. A
    /// blank address is `false`. A database from before #2063 also answers
    /// from #1934's address table until the first records fetch empties it.
    ///
    /// The folder is refreshed by the engine's full pass only (startup,
    /// `Refresh`, sign-in, a pass `needs_full_pass` asks for), never by a
    /// periodic tick: in a long-running session a contact removed from the
    /// folder elsewhere can keep counting for days, until the next full
    /// pass. A failed fetch (no `Contacts.Read`, offline) keeps the last
    /// stored set. A contact created or deleted here counts at once.
    pub fn is_contact(&self, address: &str) -> Result<bool, StoreError> {
        let address = folder_contact_key(address);
        if address.is_empty() {
            return Ok(false);
        }
        let found: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM contact_emails WHERE address_key = ?1)
                 OR EXISTS(SELECT 1 FROM folder_contacts WHERE address = ?1)",
            params![address],
            |row| row.get(0),
        )?;
        Ok(found)
    }
}

/// Why the store refused a folder command (#1910): nothing was written or
/// queued, and no request goes to Graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FolderRefusal {
    /// No stored folder has this id (after `folder_aliases`).
    Unknown(String),
    /// The local drafts sentinel (`local:drafts-pending`), which Graph
    /// never had: it is never renamed, moved, deleted or a parent.
    DraftsSentinel,
    /// A move into the folder itself or a folder inside it: the stored
    /// `parent_id` would hold a cycle.
    IntoItself,
    /// A delete whose subtree holds mail Graph does not have: a `local:`
    /// message, one a queued `SaveDraft`/`SendDraft` names (a send re-filed
    /// into Sent Items), or one with an unpushed edit (`local_edit`, #2082).
    /// The cascade would lose it for good.
    HoldsUnsentMail,
}

impl std::fmt::Display for FolderRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FolderRefusal::Unknown(id) => write!(f, "no folder {id}"),
            FolderRefusal::DraftsSentinel => {
                f.write_str("the local drafts folder is not on the server")
            }
            FolderRefusal::IntoItself => {
                f.write_str("a folder cannot move into itself or a folder inside it")
            }
            FolderRefusal::HoldsUnsentMail => f.write_str("folder holds unsent mail"),
        }
    }
}

/// What a folder command did to the store (#1910).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FolderWriteResult {
    /// The row was written and its op queued in the same transaction; the
    /// folder's id as stored now (a create's new `local:` id).
    Written(String),
    /// Nothing to do (the same name, the same parent): nothing queued.
    Unchanged,
    Refused(FolderRefusal),
}

/// What `Store::discard_local_folder` did (#1910).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct LocalFolderDiscard {
    /// A message Graph has went with the folders (moved there locally):
    /// only a mail reconverge brings it back.
    pub had_mail: bool,
    /// The folder unsent mail in them was filed back in (Drafts, or the
    /// drafts sentinel).
    pub refiled_into: Option<String>,
}

/// The folders queued folder ops name (#1910), each id resolved through
/// `folder_aliases`: what a listing must not undo (`sync_folders`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct QueuedFolderWrites {
    pub renamed: HashSet<String>,
    pub moved: HashSet<String>,
    pub deleted: HashSet<String>,
}

/// Folder writes (#1910). Each command's write is one IMMEDIATE transaction
/// that writes the row(s) and queues the op, so no listing sees the change
/// without the op that protects it.
impl Store {
    /// Whether a folder row has exactly this id (#1910).
    pub(crate) fn has_folder(&self, id: &str) -> Result<bool, StoreError> {
        folder_exists_on(&self.conn, id)
    }

    /// The id the folder `id` is stored under now: the Graph id a `local:`
    /// id was re-keyed to, or the id Graph answered a move with
    /// (`folder_aliases`), else `id` itself.
    pub fn current_folder_id(&self, id: &str) -> Result<String, StoreError> {
        current_folder_id_on(&self.conn, id)
    }

    /// Adds a folder named `name` under `parent` (`None`: the top of the
    /// mailbox, stored under the root id the listing gives top folders, see
    /// `root_folder_id_on`) with a fresh `local:` id, and queues its
    /// `CreateFolder`. `parent` is resolved through `folder_aliases`; an
    /// unknown parent or the drafts sentinel is refused.
    pub(crate) fn create_local_folder(
        &self,
        parent: Option<&str>,
        name: &str,
    ) -> Result<FolderWriteResult, StoreError> {
        let tx = self.write_tx()?;
        let parent = match parent {
            Some(p) => {
                let p = current_folder_id_on(&tx, p)?;
                if let Some(r) = check_folder_on(&tx, &p)? {
                    return Ok(FolderWriteResult::Refused(r));
                }
                Some(p)
            }
            None => None,
        };
        let stored_parent = match &parent {
            Some(p) => Some(p.clone()),
            None => root_folder_id_on(&tx)?,
        };
        let id = local_draft_id();
        tx.execute(
            "INSERT INTO folders (id, parent_id, display_name) VALUES (?1, ?2, ?3)",
            params![id, stored_parent, name],
        )?;
        enqueue_op_on(
            &tx,
            &OutboxOp::CreateFolder {
                id: id.clone(),
                parent,
                name: name.to_string(),
            },
        )?;
        tx.commit()?;
        Ok(FolderWriteResult::Written(id))
    }

    /// Renames a stored folder and queues its `RenameFolder`. Writes only
    /// `display_name`. The same name is `Unchanged`.
    pub(crate) fn rename_folder(
        &self,
        id: &str,
        name: &str,
    ) -> Result<FolderWriteResult, StoreError> {
        let tx = self.write_tx()?;
        let id = current_folder_id_on(&tx, id)?;
        if let Some(r) = check_folder_on(&tx, &id)? {
            return Ok(FolderWriteResult::Refused(r));
        }
        let prev: String = tx.query_row(
            "SELECT display_name FROM folders WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        if prev == name {
            return Ok(FolderWriteResult::Unchanged);
        }
        tx.execute(
            "UPDATE folders SET display_name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        enqueue_op_on(
            &tx,
            &OutboxOp::RenameFolder {
                id: id.clone(),
                name: name.to_string(),
                prev_name: prev,
            },
        )?;
        tx.commit()?;
        Ok(FolderWriteResult::Written(id))
    }

    /// Moves a stored folder under `dest` (`None`: the top of the mailbox)
    /// and queues its `MoveFolder`. Writes only `parent_id`. Refused: an
    /// unknown id or dest, the drafts sentinel as either, and a dest that
    /// is the folder or inside it. A folder already there is `Unchanged`.
    pub(crate) fn move_folder(
        &self,
        id: &str,
        dest: Option<&str>,
    ) -> Result<FolderWriteResult, StoreError> {
        let tx = self.write_tx()?;
        let id = current_folder_id_on(&tx, id)?;
        if let Some(r) = check_folder_on(&tx, &id)? {
            return Ok(FolderWriteResult::Refused(r));
        }
        let dest = match dest {
            Some(d) => {
                let d = current_folder_id_on(&tx, d)?;
                if let Some(r) = check_folder_on(&tx, &d)? {
                    return Ok(FolderWriteResult::Refused(r));
                }
                if subtree_ids_on(&tx, &id)?.contains(&d) {
                    return Ok(FolderWriteResult::Refused(FolderRefusal::IntoItself));
                }
                Some(d)
            }
            None => None,
        };
        let prev: Option<String> = tx.query_row(
            "SELECT parent_id FROM folders WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        let parent = match &dest {
            Some(d) => {
                if prev.as_deref() == Some(d.as_str()) {
                    return Ok(FolderWriteResult::Unchanged);
                }
                Some(d.clone())
            }
            None => {
                if is_top_level_on(&tx, prev.as_deref())? {
                    return Ok(FolderWriteResult::Unchanged);
                }
                root_folder_id_on(&tx)?
            }
        };
        tx.execute(
            "UPDATE folders SET parent_id = ?2 WHERE id = ?1",
            params![id, parent],
        )?;
        enqueue_op_on(
            &tx,
            &OutboxOp::MoveFolder {
                id: id.clone(),
                dest,
                parent,
                prev_parent: prev,
            },
        )?;
        tx.commit()?;
        Ok(FolderWriteResult::Written(id))
    }

    /// Removes a stored folder and every folder under it, and queues its
    /// `DeleteFolder` with a snapshot of the removed rows Graph has. Their
    /// messages go by `ON DELETE CASCADE` (bodies, attachments, search
    /// rows) and their delta links with the rows, as `retain_folders`
    /// removes a folder deleted on the server (#1886). Refused, before
    /// anything is written, when the subtree holds mail Graph does not have
    /// ([`FolderRefusal::HoldsUnsentMail`]).
    ///
    /// The `bool` says whether a `local:` folder it removed held a message:
    /// one moved there locally, which Graph still has where it was, so only
    /// a mail reconverge brings it back. The queued message ops naming such
    /// a message go in the same transaction (`drop_local_folder_mail_on`):
    /// they could send nothing that matters now, and while queued they
    /// would keep the reconverge's fresh deltas from storing it again.
    pub(crate) fn delete_folder(&self, id: &str) -> Result<(FolderWriteResult, bool), StoreError> {
        let tx = self.write_tx()?;
        let id = current_folder_id_on(&tx, id)?;
        if let Some(r) = check_folder_on(&tx, &id)? {
            return Ok((FolderWriteResult::Refused(r), false));
        }
        let subtree_ids = subtree_ids_on(&tx, &id)?;
        let subtree = ids_json(&subtree_ids);
        if !unsent_mail_in_on(&tx, &subtree_ids)?.is_empty() {
            return Ok((
                FolderWriteResult::Refused(FolderRefusal::HoldsUnsentMail),
                false,
            ));
        }
        let local = local_folders(subtree_ids.clone());
        let dropped_mail = drop_local_folder_mail_on(&tx, &local)?;
        let removed = {
            let mut stmt = tx.prepare(
                "SELECT id, parent_id, display_name, total_count, unread_count,
                        well_known_name, sort_order, is_expanded
                 FROM folders
                 WHERE id IN (SELECT value FROM json_each(?1))
                   AND substr(id, 1, 6) <> 'local:'
                 ORDER BY id",
            )?;
            stmt.query_map(params![subtree], |r| {
                Ok(FolderSnapshot {
                    id: r.get(0)?,
                    parent_id: r.get(1)?,
                    display_name: r.get(2)?,
                    total_count: r.get(3)?,
                    unread_count: r.get(4)?,
                    well_known_name: r.get(5)?,
                    sort_order: r.get(6)?,
                    is_expanded: r.get::<_, i64>(7)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        // A folder Graph has that was moved under a removed `local:`
        // folder has no Graph parent to go with: its move could never be
        // sent (the dest never reached Graph). It gets a delete of its own,
        // with its own part of the snapshot, and its queued moves go; its
        // snapshot row takes the parent Graph has it under, the first
        // dropped move's `prev_parent`, so a refused delete restores it
        // there. Any other queued move into a removed `local:` folder goes
        // too, wherever the moved folder is now (deleted itself, or moved
        // on): it could only be refused.
        let roots = delete_roots(&id, &removed, &local);
        let orphans: Vec<&String> = roots.iter().filter(|r| **r != id).collect();
        let mut graph_parents: Vec<(String, Option<String>)> = Vec::new();
        for row in pending_ops_on(&tx)? {
            let OutboxOp::MoveFolder {
                id: moved,
                dest,
                prev_parent,
                ..
            } = &row.op
            else {
                continue;
            };
            let moved = current_folder_id_on(&tx, moved)?;
            let into_local = match dest {
                Some(d) => local.contains(&current_folder_id_on(&tx, d)?),
                None => false,
            };
            let orphan = orphans.contains(&&moved);
            if orphan && !graph_parents.iter().any(|(m, _)| *m == moved) {
                let prev = prev_parent
                    .as_deref()
                    .map(|p| current_folder_id_on(&tx, p))
                    .transpose()?;
                graph_parents.push((moved.clone(), prev));
            }
            if orphan || into_local {
                tx.execute("DELETE FROM outbox WHERE seq = ?1", params![row.seq])?;
            }
        }
        let mut removed = removed;
        for row in &mut removed {
            if let Some((_, prev)) = graph_parents.iter().find(|(m, _)| *m == row.id) {
                row.parent_id = prev.clone();
            }
        }
        tx.execute(
            "DELETE FROM folders WHERE id IN (SELECT value FROM json_each(?1))",
            params![subtree],
        )?;
        let mut parts: Vec<(String, Vec<FolderSnapshot>)> = Vec::new();
        for (row, root) in removed.into_iter().zip(roots) {
            match parts.iter_mut().find(|(r, _)| *r == root) {
                Some((_, rows)) => rows.push(row),
                None => parts.push((root, vec![row])),
            }
        }
        for (root, rows) in parts.iter().filter(|(r, _)| *r != id) {
            enqueue_op_on(
                &tx,
                &OutboxOp::DeleteFolder {
                    id: root.clone(),
                    removed: rows.clone(),
                },
            )?;
        }
        let removed = parts
            .into_iter()
            .find(|(r, _)| *r == id)
            .map(|(_, rows)| rows)
            .unwrap_or_default();
        enqueue_op_on(
            &tx,
            &OutboxOp::DeleteFolder {
                id: id.clone(),
                removed,
            },
        )?;
        tx.commit()?;
        Ok((FolderWriteResult::Written(id), dropped_mail))
    }

    /// Re-keys a folder created locally to the id Graph gave it (#1910), in
    /// one transaction (`rekey_folder_on`): its messages and child folders
    /// follow, and every other column (name and parent as stored, which a
    /// later queued rename or move sends, `sort_order`, `is_expanded`) is
    /// kept. The alias `local_id → created.id` is recorded even when the row
    /// is gone. Whether a row was re-keyed.
    pub(crate) fn reconcile_created_folder(
        &self,
        local_id: &str,
        created: &MailFolder,
    ) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        let exists = folder_exists_on(&tx, local_id)?;
        if exists {
            rekey_folder_on(&tx, local_id, &created.id, false)?;
        }
        record_folder_alias_on(&tx, local_id, &created.id)?;
        tx.commit()?;
        Ok(exists)
    }

    /// Stores what Graph answered a folder move with (#1910): its parent,
    /// while the stored parent is still the one the op wrote (`parent`; a
    /// later queued move keeps its own), and its id, re-keying the folder
    /// when the answer's differs. A re-key clears the folder's delta and
    /// backfill links, which name the old id, so its history runs again
    /// against the new one. Whether the store changed.
    pub(crate) fn apply_moved_folder(
        &self,
        id: &str,
        parent: Option<&str>,
        answer: &MailFolder,
    ) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        let id = current_folder_id_on(&tx, id)?;
        let stored: Option<Option<String>> = tx
            .query_row(
                "SELECT parent_id FROM folders WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let mut changed = false;
        if let Some(stored) = stored {
            let written = parent.map(|p| current_folder_id_on(&tx, p)).transpose()?;
            if stored == written && answer.parent_id.is_some() && stored != answer.parent_id {
                tx.execute(
                    "UPDATE folders SET parent_id = ?2 WHERE id = ?1",
                    params![id, answer.parent_id],
                )?;
                changed = true;
            }
            if answer.id != id {
                rekey_folder_on(&tx, &id, &answer.id, true)?;
                changed = true;
            }
        }
        if answer.id != id {
            record_folder_alias_on(&tx, &id, &answer.id)?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Removes a folder whose create Graph refused (#1910) and the `local:`
    /// folders under it, queueing nothing. A folder Graph has that was
    /// moved under it locally is left: its own queued move is refused next
    /// and reverts it. Mail Graph does not have (`unsent_mail_in_on`: a
    /// draft moved there) is first filed back in Drafts (or the drafts
    /// sentinel before Drafts is stored), its own `SaveDraft`/`SendDraft`
    /// still queued. Any other message that went with them was moved there
    /// locally and only a mail reconverge brings it back
    /// ([`LocalFolderDiscard::had_mail`]); the queued message ops naming it
    /// go too, as in `delete_folder`. An id that is not `local:`, or the
    /// drafts sentinel, removes nothing.
    pub(crate) fn discard_local_folder(&self, id: &str) -> Result<LocalFolderDiscard, StoreError> {
        let mut out = LocalFolderDiscard::default();
        if !id.starts_with("local:") || id == LOCAL_DRAFTS_SENTINEL_FOLDER_ID {
            return Ok(out);
        }
        let tx = self.write_tx()?;
        let local = local_subtree_ids_on(&tx, id)?;
        let unsent = unsent_mail_in_on(&tx, &local)?;
        if !unsent.is_empty() {
            let drafts = drafts_folder_id_on(&tx)?;
            tx.execute(
                "UPDATE messages SET folder_id = ?2
                 WHERE id IN (SELECT value FROM json_each(?1))",
                params![ids_json(&unsent), drafts],
            )?;
            out.refiled_into = Some(drafts);
        }
        out.had_mail = drop_local_folder_mail_on(&tx, &local)?;
        let local = ids_json(&local);
        tx.execute(
            "DELETE FROM folders WHERE id IN (SELECT value FROM json_each(?1))",
            params![local],
        )?;
        tx.commit()?;
        Ok(out)
    }

    /// Puts a refused rename's previous name back (#1910), only while the
    /// stored name is still the one the op wrote (`attempted`): a later
    /// queued rename is kept, and the next listing corrects anything else.
    /// Whether it wrote.
    pub(crate) fn revert_folder_name(
        &self,
        id: &str,
        prev: &str,
        attempted: &str,
    ) -> Result<bool, StoreError> {
        let id = self.current_folder_id(id)?;
        let n = self.conn.execute(
            "UPDATE folders SET display_name = ?2 WHERE id = ?1 AND display_name = ?3",
            params![id, prev, attempted],
        )?;
        Ok(n > 0)
    }

    /// Puts a refused move's previous parent back (#1910), only while the
    /// stored parent is still the one the op wrote (`attempted`), each id
    /// resolved through `folder_aliases`. Whether it wrote.
    pub(crate) fn revert_folder_parent(
        &self,
        id: &str,
        prev: Option<&str>,
        attempted: Option<&str>,
    ) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        let id = current_folder_id_on(&tx, id)?;
        let prev = prev.map(|p| current_folder_id_on(&tx, p)).transpose()?;
        let attempted = attempted
            .map(|p| current_folder_id_on(&tx, p))
            .transpose()?;
        let n = tx.execute(
            "UPDATE folders SET parent_id = ?2 WHERE id = ?1 AND parent_id IS ?3",
            params![id, prev, attempted],
        )?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Puts back the folder rows a refused delete removed (#1910), with
    /// their `sort_order` and `is_expanded`; a row stored again since (a
    /// listing) is left. No delta link: the next pass's delta brings their
    /// messages back from Graph, which still has them. Whether any came
    /// back.
    pub(crate) fn restore_folders(&self, rows: &[FolderSnapshot]) -> Result<bool, StoreError> {
        let tx = self.write_tx()?;
        let mut restored = false;
        for f in rows {
            restored |= tx.execute(
                "INSERT OR IGNORE INTO folders
                     (id, parent_id, display_name, total_count, unread_count,
                      well_known_name, sort_order, is_expanded)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    f.id,
                    f.parent_id,
                    f.display_name,
                    f.total_count,
                    f.unread_count,
                    f.well_known_name,
                    f.sort_order,
                    f.is_expanded as i64
                ],
            )? > 0;
        }
        tx.commit()?;
        Ok(restored)
    }

    /// The folders a queued rename, move or delete names (#1910), resolved
    /// through `folder_aliases`. `deleted` also holds every folder a queued
    /// delete removed with it (its `removed` snapshot): one moved under the
    /// deleted folder locally is listed under its old parent until its
    /// move reaches Graph.
    pub(crate) fn queued_folder_writes(&self) -> Result<QueuedFolderWrites, StoreError> {
        let mut q = QueuedFolderWrites::default();
        for row in pending_ops_on(&self.conn)? {
            let (id, set) = match row.op {
                OutboxOp::RenameFolder { id, .. } => (id, &mut q.renamed),
                OutboxOp::MoveFolder { id, .. } => (id, &mut q.moved),
                OutboxOp::DeleteFolder { id, removed } => {
                    for f in removed {
                        q.deleted.insert(current_folder_id_on(&self.conn, &f.id)?);
                    }
                    (id, &mut q.deleted)
                }
                _ => continue,
            };
            set.insert(current_folder_id_on(&self.conn, &id)?);
        }
        Ok(q)
    }
}

/// The messages filed in `folders` that Graph does not have (#1910): a
/// `local:` id, an unpushed edit (`local_edit > 0`, #2082), or one a queued
/// `SaveDraft`/`SendDraft` names (resolved through `current_id`; a send
/// re-files its draft into Sent Items at command time). A delete refuses
/// to cascade them; a refused create files them back in Drafts.
fn unsent_mail_in_on(conn: &Connection, folders: &[String]) -> Result<Vec<String>, StoreError> {
    if folders.is_empty() {
        return Ok(Vec::new());
    }
    let mut drafts = Vec::new();
    for row in pending_ops_on(conn)? {
        if let OutboxOp::SaveDraft { id } | OutboxOp::SendDraft { id } = row.op {
            drafts.push(current_id_on(conn, &id)?);
        }
    }
    let mut stmt = conn.prepare(
        "SELECT id FROM messages
         WHERE folder_id IN (SELECT value FROM json_each(?1))
           AND (substr(id, 1, 6) = 'local:'
                OR local_edit > 0
                OR id IN (SELECT value FROM json_each(?2)))
         ORDER BY id",
    )?;
    let ids = stmt
        .query_map(params![ids_json(folders), ids_json(&drafts)], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

/// `id` and the `local:` folders under it reached through `local:`
/// parents only (#1910): a Graph folder moved under it, and anything under
/// that, stays (its move is refused next and reverts it, and a `local:`
/// folder created under it still goes to Graph under it). Never the drafts
/// sentinel.
fn local_subtree_ids_on(conn: &Connection, id: &str) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE sub(id) AS (
             SELECT ?1
             UNION
             SELECT f.id FROM folders f JOIN sub ON f.parent_id = sub.id
             WHERE substr(f.id, 1, 6) = 'local:' AND f.id <> ?2
         )
         SELECT id FROM sub",
    )?;
    let ids = stmt
        .query_map(params![id, LOCAL_DRAFTS_SENTINEL_FOLDER_ID], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

/// For each row of a delete's snapshot (`removed`, rows Graph has), the
/// folder whose `DeleteFolder` carries it (#1910): `target`, or the
/// highest folder above the row whose stored parent is a removed `local:`
/// folder (one moved under it here), which has no Graph parent to go with.
fn delete_roots(target: &str, removed: &[FolderSnapshot], local: &[String]) -> Vec<String> {
    removed
        .iter()
        .map(|row| {
            let mut at = row;
            // Bounded by the snapshot's size, so a cycle cannot hang it.
            for _ in 0..removed.len() {
                if at.id == target {
                    break;
                }
                let Some(parent) = at.parent_id.as_deref() else {
                    break;
                };
                if local.iter().any(|l| l == parent) {
                    break;
                }
                match removed.iter().find(|f| f.id == parent) {
                    Some(p) => at = p,
                    None => break,
                }
            }
            if at.id != target
                && at
                    .parent_id
                    .as_deref()
                    .is_some_and(|p| local.iter().any(|l| l == p))
            {
                at.id.clone()
            } else {
                target.to_string()
            }
        })
        .collect()
}

/// The `local:` folders (created here, not on Graph yet) among `ids`,
/// never the drafts sentinel.
fn local_folders(ids: Vec<String>) -> Vec<String> {
    ids.into_iter()
        .filter(|f| f.starts_with("local:") && f != LOCAL_DRAFTS_SENTINEL_FOLDER_ID)
        .collect()
}

/// Before the `local:` folders `local` are removed (#1910): whether they
/// hold a message. Every queued message move into one of them is dropped
/// (it could only send nothing), and when they hold mail, so is every
/// other queued message op naming it. Such a message was moved there
/// locally and Graph still has it where it was; a mail reconverge brings
/// it back, which a queued op naming it would block (a fresh delta skips
/// every id a queued op names). A draft's own save or send is kept.
fn drop_local_folder_mail_on(conn: &Connection, local: &[String]) -> Result<bool, StoreError> {
    if local.is_empty() {
        return Ok(false);
    }
    let local_json = ids_json(local);
    let had_mail: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages
             WHERE folder_id IN (SELECT value FROM json_each(?1)))",
        params![local_json],
        |r| r.get(0),
    )?;
    // A move into them goes whatever they hold now (a draft filed back in
    // Drafts first, or a message moved on since).
    for row in pending_ops_on(conn)? {
        // A draft's own save or send is kept: unsent mail never reaches
        // here (a delete refuses it, a refused create files it back in
        // Drafts first), and its op must still create it on Graph.
        if matches!(
            row.op,
            OutboxOp::SaveDraft { .. } | OutboxOp::SendDraft { .. }
        ) {
            continue;
        }
        let Some(message) = row.op.message_id() else {
            continue;
        };
        let dest = match &row.op {
            OutboxOp::Move { dest, .. } => Some(dest),
            _ => None,
        };
        let into_local = match dest {
            Some(d) => local.contains(&current_folder_id_on(conn, d)?),
            None => false,
        };
        let message = current_id_on(conn, message)?;
        let folder: Option<String> = conn
            .query_row(
                "SELECT folder_id FROM messages WHERE id = ?1",
                params![message],
                |r| r.get(0),
            )
            .optional()?;
        if into_local || (had_mail && folder.is_some_and(|f| local.contains(&f))) {
            conn.execute("DELETE FROM outbox WHERE seq = ?1", params![row.seq])?;
        }
    }
    Ok(had_mail)
}

/// `Store::current_folder_id` on `conn`.
fn current_folder_id_on(conn: &Connection, id: &str) -> Result<String, StoreError> {
    let aliased: Option<String> = conn
        .query_row(
            "SELECT id FROM folder_aliases WHERE old_id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(aliased.unwrap_or_else(|| id.to_string()))
}

fn folder_exists_on(conn: &Connection, id: &str) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM folders WHERE id = ?1)",
        params![id],
        |r| r.get(0),
    )?)
}

/// Why `id` cannot be a folder command's folder, parent or dest: the drafts
/// sentinel, or no such folder. `None` when it can.
fn check_folder_on(conn: &Connection, id: &str) -> Result<Option<FolderRefusal>, StoreError> {
    if id == LOCAL_DRAFTS_SENTINEL_FOLDER_ID {
        return Ok(Some(FolderRefusal::DraftsSentinel));
    }
    if !folder_exists_on(conn, id)? {
        return Ok(Some(FolderRefusal::Unknown(id.to_string())));
    }
    Ok(None)
}

/// `id` and every stored folder under it (`parent_id`), `id` first.
/// `UNION` stops at a cycle.
fn subtree_ids_on(conn: &Connection, id: &str) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE sub(id) AS (
             SELECT ?1
             UNION
             SELECT f.id FROM folders f JOIN sub ON f.parent_id = sub.id
         )
         SELECT id FROM sub",
    )?;
    let ids = stmt
        .query_map(params![id], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

/// The id the listing gives top folders as their parent (Graph's
/// `msgfolderroot`, which is never listed): the parent most stored folders
/// share that is not itself stored. `None` before any listing; lookxy's
/// folder tree shows a folder under either at the top.
fn root_folder_id_on(conn: &Connection) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT parent_id FROM folders
             WHERE parent_id IS NOT NULL
               AND parent_id NOT IN (SELECT id FROM folders)
             GROUP BY parent_id
             ORDER BY COUNT(*) DESC, parent_id
             LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

/// Whether a folder whose parent is `parent` is at the top: no parent, or
/// one that is not a stored folder (the root).
fn is_top_level_on(conn: &Connection, parent: Option<&str>) -> Result<bool, StoreError> {
    match parent {
        None => Ok(true),
        Some(p) => Ok(!folder_exists_on(conn, p)?),
    }
}

/// `ids` as a JSON array, for `json_each`.
fn ids_json(ids: &[String]) -> String {
    Value::Array(ids.iter().map(|id| Value::Str(id.clone())).collect()).to_string()
}

/// Re-keys the folder `old` to `new` on `conn` (a transaction): the row,
/// its messages (`folder_id`) and its child folders (`parent_id`). A stale
/// row already under `new` gives its messages and children to the kept row
/// first, so none is cascaded away, then goes. `messages.folder_id` has no
/// `ON UPDATE CASCADE`, so foreign keys are deferred to the commit
/// (`defer_foreign_keys` resets itself there). `clear_links` also clears
/// the delta and backfill state (a move's new id: the links name the old).
fn rekey_folder_on(
    conn: &Connection,
    old: &str,
    new: &str,
    clear_links: bool,
) -> Result<(), StoreError> {
    if old == new {
        return Ok(());
    }
    conn.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    conn.execute(
        "UPDATE messages SET folder_id = ?1 WHERE folder_id = ?2",
        params![old, new],
    )?;
    conn.execute(
        "UPDATE folders SET parent_id = ?1 WHERE parent_id = ?2",
        params![old, new],
    )?;
    conn.execute("DELETE FROM folders WHERE id = ?1", params![new])?;
    conn.execute(
        "UPDATE folders SET id = ?2 WHERE id = ?1",
        params![old, new],
    )?;
    conn.execute(
        "UPDATE messages SET folder_id = ?2 WHERE folder_id = ?1",
        params![old, new],
    )?;
    conn.execute(
        "UPDATE folders SET parent_id = ?2 WHERE parent_id = ?1",
        params![old, new],
    )?;
    if clear_links {
        conn.execute(
            "UPDATE folders SET delta_link = NULL, backfill_link = NULL,
                 backfill_synced = 0, history_scope = NULL, backfill_scope = NULL,
                 reminder_expand_refused = 0, backfill_expand_refused = 0
             WHERE id = ?1",
            params![new],
        )?;
    }
    Ok(())
}

/// Records `old → new` in `folder_aliases`, and points every alias that
/// named `old` at `new`, so one lookup resolves any old id.
fn record_folder_alias_on(conn: &Connection, old: &str, new: &str) -> Result<(), StoreError> {
    if old == new {
        return Ok(());
    }
    conn.execute(
        "UPDATE folder_aliases SET id = ?2 WHERE id = ?1",
        params![old, new],
    )?;
    conn.execute("DELETE FROM folder_aliases WHERE old_id = ?1", params![new])?;
    conn.execute(
        "INSERT INTO folder_aliases (old_id, id) VALUES (?1, ?2)
         ON CONFLICT(old_id) DO UPDATE SET id = excluded.id",
        params![old, new],
    )?;
    Ok(())
}

/// `Store::enqueue_op` on `conn` (the store's connection or a transaction).
fn enqueue_op_on(conn: &Connection, op: &OutboxOp) -> Result<i64, StoreError> {
    let payload = op.to_json().to_string();
    conn.execute(
        "INSERT INTO outbox (op, payload) VALUES (?1, ?2)",
        params![op.kind(), payload],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The contacts queued contact ops name (#2063), each id resolved through
/// `contact_aliases`.
#[derive(Default)]
struct QueuedContacts {
    /// Named by an `UpdateContact`.
    edited: HashSet<String>,
    /// Named by a `DeleteContact`.
    deleted: HashSet<String>,
    /// Named by any contact op.
    any: HashSet<String>,
}

/// `QueuedContacts` as the outbox on `conn` holds them now.
fn queued_contacts_on(conn: &Connection) -> Result<QueuedContacts, StoreError> {
    let mut q = QueuedContacts::default();
    for row in pending_ops_on(conn)? {
        let (id, set) = match row.op {
            OutboxOp::UpdateContact { id, .. } => (id, Some(&mut q.edited)),
            OutboxOp::DeleteContact { id } => (id, Some(&mut q.deleted)),
            OutboxOp::CreateContact { id } => (id, None),
            _ => continue,
        };
        let id = resolve_contact_id_on(conn, &id)?;
        if let Some(set) = set {
            set.insert(id.clone());
        }
        q.any.insert(id);
    }
    Ok(q)
}

/// `f` with blank (whitespace-only) phone and category entries dropped
/// (#2063): `encode_categories` would store `[""]` as `""`, which reads back
/// as `[]`, so an edit carrying one would compare as changed on every save.
fn tidy_contact_fields(f: &ContactFields) -> ContactFields {
    let keep = |l: &[String]| -> Vec<String> {
        l.iter().filter(|s| !s.trim().is_empty()).cloned().collect()
    };
    ContactFields {
        business_phones: keep(&f.business_phones),
        home_phones: keep(&f.home_phones),
        categories: keep(&f.categories),
        ..f.clone()
    }
}

/// `Store::current_id` on `conn` (a transaction in
/// `fetched_body_is_kept_on`'s callers).
fn current_id_on(conn: &Connection, id: &str) -> Result<String, StoreError> {
    let current = conn.query_row(
        concat!(
            "SELECT COALESCE((SELECT message_id FROM moved_ids WHERE old_id = c.id), c.id)
             FROM (SELECT ",
            current_id_sql!(),
            " AS id) AS c"
        ),
        params![id],
        |r| r.get(0),
    )?;
    Ok(current)
}

/// One `outbox.payload` decoded (`pending_ops_on`,
/// `fetched_body_is_kept_on`).
fn decode_outbox_payload(payload: &str) -> Result<OutboxOp, StoreError> {
    let v = json::parse(payload).map_err(|e| StoreError::Decode(e.to_string()))?;
    OutboxOp::from_json(&v)
        .ok_or_else(|| StoreError::Decode(format!("unrecognized outbox payload: {payload}")))
}

/// Whether a fetched body for `message_id` must leave the stored one alone
/// (#2082, `Store::put_fetched_body_with_headers`): a body is stored, and
/// the row has an unsaved compose edit (`local_edit > 0`) or a queued
/// `SaveDraft` / `SendDraft` names it by the id it is stored under now
/// (`current_id_on`: an op queued under a `local:` id since reconciled, or
/// before a move, #1896). Cheapest test first: most fetches are of a
/// message with no body yet, and those never read the outbox. An outbox row
/// that does not decode is skipped, as it is no draft op this build knows:
/// one bad row must not fail every fetch.
fn fetched_body_is_kept_on(conn: &Connection, message_id: &str) -> Result<bool, StoreError> {
    let has_body: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bodies WHERE message_id = ?1)",
        params![message_id],
        |r| r.get(0),
    )?;
    if !has_body {
        return Ok(false);
    }
    let edited: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE id = ?1 AND local_edit > 0)",
        params![message_id],
        |r| r.get(0),
    )?;
    if edited {
        return Ok(true);
    }
    let mut stmt = conn.prepare("SELECT payload FROM outbox ORDER BY seq ASC")?;
    let payloads = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for payload in payloads {
        let id = match decode_outbox_payload(&payload) {
            Ok(OutboxOp::SaveDraft { id } | OutboxOp::SendDraft { id }) => id,
            _ => continue,
        };
        if current_id_on(conn, &id)? == message_id {
            return Ok(true);
        }
    }
    Ok(false)
}

/// All queued ops on `conn`, oldest first (`Store::pending_ops`).
fn pending_ops_on(conn: &Connection) -> Result<Vec<OutboxRow>, StoreError> {
    let mut stmt = conn.prepare("SELECT seq, payload, attempts FROM outbox ORDER BY seq ASC")?;
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
            let op = decode_outbox_payload(&payload)?;
            Ok(OutboxRow { seq, op, attempts })
        })
        .collect()
}

/// The id the contact `id` names is stored under now, on `conn`
/// (`Store::resolve_contact_id`).
fn resolve_contact_id_on(conn: &Connection, id: &str) -> Result<String, StoreError> {
    let aliased: Option<String> = conn
        .query_row(
            "SELECT id FROM contact_aliases WHERE local_id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(aliased.unwrap_or_else(|| id.to_string()))
}

/// Inserts or replaces the contact `id` on `conn` with `fields`, and
/// rewrites its email rows. An upsert, not `INSERT OR REPLACE`, which would
/// delete the row first.
fn write_contact_on(
    conn: &Connection,
    id: &str,
    change_key: &str,
    f: &ContactFields,
) -> Result<(), StoreError> {
    let f = &tidy_contact_fields(f);
    let b = &f.business_address;
    let h = &f.home_address;
    conn.execute(
        "INSERT INTO contact_records (
             id, change_key, given_name, middle_name, surname, display_name, file_as,
             business_phones, home_phones, mobile_phone, company_name, job_title, department,
             business_street, business_city, business_state, business_postal_code,
             business_country, home_street, home_city, home_state, home_postal_code,
             home_country, birthday, personal_notes, categories)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)
         ON CONFLICT(id) DO UPDATE SET
             change_key = excluded.change_key, given_name = excluded.given_name,
             middle_name = excluded.middle_name, surname = excluded.surname,
             display_name = excluded.display_name, file_as = excluded.file_as,
             business_phones = excluded.business_phones, home_phones = excluded.home_phones,
             mobile_phone = excluded.mobile_phone, company_name = excluded.company_name,
             job_title = excluded.job_title, department = excluded.department,
             business_street = excluded.business_street,
             business_city = excluded.business_city,
             business_state = excluded.business_state,
             business_postal_code = excluded.business_postal_code,
             business_country = excluded.business_country,
             home_street = excluded.home_street, home_city = excluded.home_city,
             home_state = excluded.home_state, home_postal_code = excluded.home_postal_code,
             home_country = excluded.home_country, birthday = excluded.birthday,
             personal_notes = excluded.personal_notes, categories = excluded.categories",
        params![
            id,
            change_key,
            &f.given_name,
            &f.middle_name,
            &f.surname,
            &f.display_name,
            &f.file_as,
            encode_categories(&f.business_phones),
            encode_categories(&f.home_phones),
            &f.mobile_phone,
            &f.company_name,
            &f.job_title,
            &f.department,
            &b.street,
            &b.city,
            &b.state,
            &b.postal_code,
            &b.country_or_region,
            &h.street,
            &h.city,
            &h.state,
            &h.postal_code,
            &h.country_or_region,
            &f.birthday,
            &f.personal_notes,
            encode_categories(&f.categories),
        ],
    )?;
    conn.execute(
        "DELETE FROM contact_emails WHERE contact_id = ?1",
        params![id],
    )?;
    for (position, e) in f.email_addresses.iter().enumerate() {
        let key = folder_contact_key(&e.address);
        let key = if key.contains('@') {
            key
        } else {
            String::new()
        };
        conn.execute(
            "INSERT INTO contact_emails (contact_id, position, name, address, address_key)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, position as i64, e.name, e.address, key],
        )?;
    }
    Ok(())
}

/// The stored contact `id` on `conn`, with its email rows in order.
fn read_contact_on(conn: &Connection, id: &str) -> Result<Option<FolderContact>, StoreError> {
    let row = conn
        .query_row(
            "SELECT id, change_key, given_name, middle_name, surname, display_name, file_as,
                    business_phones, home_phones, mobile_phone, company_name, job_title,
                    department, business_street, business_city, business_state,
                    business_postal_code, business_country, home_street, home_city,
                    home_state, home_postal_code, home_country, birthday, personal_notes,
                    categories
             FROM contact_records WHERE id = ?1",
            params![id],
            |r| {
                let s = |i: usize| r.get::<_, String>(i);
                Ok(FolderContact {
                    id: s(0)?,
                    change_key: s(1)?,
                    fields: ContactFields {
                        given_name: s(2)?,
                        middle_name: s(3)?,
                        surname: s(4)?,
                        display_name: s(5)?,
                        file_as: s(6)?,
                        email_addresses: Vec::new(),
                        business_phones: decode_categories(&s(7)?),
                        home_phones: decode_categories(&s(8)?),
                        mobile_phone: s(9)?,
                        company_name: s(10)?,
                        job_title: s(11)?,
                        department: s(12)?,
                        business_address: PhysicalAddress {
                            street: s(13)?,
                            city: s(14)?,
                            state: s(15)?,
                            postal_code: s(16)?,
                            country_or_region: s(17)?,
                        },
                        home_address: PhysicalAddress {
                            street: s(18)?,
                            city: s(19)?,
                            state: s(20)?,
                            postal_code: s(21)?,
                            country_or_region: s(22)?,
                        },
                        birthday: s(23)?,
                        personal_notes: s(24)?,
                        categories: decode_categories(&s(25)?),
                    },
                })
            },
        )
        .optional()?;
    let Some(mut c) = row else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT name, address FROM contact_emails WHERE contact_id = ?1 ORDER BY position",
    )?;
    c.fields.email_addresses = stmt
        .query_map(params![id], |r| {
            Ok(Recipient {
                name: r.get(0)?,
                address: r.get(1)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(Some(c))
}

/// The key `folder_contacts` stores an address under, and `is_contact`
/// looks one up by: trimmed, ASCII letters lowercased, nothing else folded.
/// Not Unicode `to_lowercase` (which `contacts` keeps for autocomplete): it
/// maps characters such as U+212A KELVIN SIGN onto ASCII `k`, so a sender
/// spelled `\u{212A}ate@x.com` would match a stored `kate@x.com`, a false
/// positive on a trust decision. The cost errs on the safe side: a stored
/// `Ärger@x.com` does not match a sender spelled `ärger@x.com`.
fn folder_contact_key(address: &str) -> String {
    address.trim().to_ascii_lowercase()
}

/// A `contacts` row, in `search_contacts`' and `all_contacts`' column order:
/// address, name, source, last_seen, frequency, relevance.
fn contact_from_row(row: &Row<'_>) -> rusqlite::Result<Contact> {
    Ok(Contact {
        address: row.get(0)?,
        name: row.get(1)?,
        source: row.get(2)?,
        last_seen: row.get(3)?,
        frequency: row.get(4)?,
        relevance: row.get(5)?,
    })
}

/// Contact search's case fold (#1890), for the query and both columns
/// alike: Turkish dotted capital `İ` as plain `i`, then Unicode
/// `to_lowercase`, then a final sigma `ς` as `σ`. `to_lowercase` alone
/// writes `İ` as `i` + U+0307 (so `ilk` would never find `İlker`) and a
/// capital `Σ` that ends a word as `ς` (so `ΟΔΥΣ`, `οδυς`, would never be
/// found in `Οδυσσέας`, `οδυσσέας`). Spelling equivalences (`ß`/`ss`),
/// accents and Unicode normalisation are not folded.
fn fold_contact_text(s: &str) -> String {
    s.replace('İ', "i").to_lowercase().replace('ς', "σ")
}

/// Whether `c`, not alphanumeric itself, belongs to the character before
/// it rather than separating words: a combining mark (Latin, Cyrillic,
/// Hebrew points, Arabic harakat, Indic viramas and nuktas, Thai marks) or
/// a zero-width (non-)joiner or variation selector. std has no general
/// category lookup, so these are the common blocks' ranges; punctuation
/// inside them (Hebrew maqaf and sof pasuq, Arabic ayah signs, the Indic
/// dandas) is left out and still separates.
fn continues_a_word(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}'
        | '\u{0483}'..='\u{0489}'
        | '\u{0591}'..='\u{05BD}'
        | '\u{05BF}'
        | '\u{05C1}'..='\u{05C2}'
        | '\u{05C4}'..='\u{05C5}'
        | '\u{05C7}'
        | '\u{0610}'..='\u{061A}'
        | '\u{064B}'..='\u{065F}'
        | '\u{0670}'
        | '\u{06D6}'..='\u{06DC}'
        | '\u{06DF}'..='\u{06E8}'
        | '\u{06EA}'..='\u{06ED}'
        | '\u{0900}'..='\u{0963}'
        | '\u{0966}'..='\u{0DFF}'
        | '\u{0E31}'
        | '\u{0E34}'..='\u{0E3A}'
        | '\u{0E47}'..='\u{0E4E}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{200C}'..='\u{200D}'
        | '\u{20D0}'..='\u{20FF}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{FE20}'..='\u{FE2F}')
}

/// Whether `q` occurs in `hay` at a word start lying before byte `end`: at
/// 0, or right after a separator, a character that is neither alphanumeric
/// nor `continues_a_word` (after a combining mark or a joiner the word goes
/// on). Every char boundary is tried, so an earlier mid-word occurrence
/// never hides a later word-start one.
fn has_word_prefix(hay: &str, q: &str, end: usize) -> bool {
    let mut in_word = false;
    for (i, c) in hay.char_indices() {
        if i >= end {
            break;
        }
        if !in_word && hay[i..].starts_with(q) {
            return true;
        }
        if c.is_alphanumeric() {
            in_word = true;
        } else if !continues_a_word(c) {
            in_word = false;
        }
    }
    false
}

/// The `contact_match` rank of one contact for an already folded query
/// (#1890): `Some(0)` for a word-prefix match (in the name, or in the
/// address' local part, before the `@`; a name that is itself an address,
/// an `@` and no whitespace, takes the address rule), `Some(1)` for any
/// other occurrence in the folded name or address, `None` for no match.
/// An empty query matches every contact at `Some(0)`.
fn contact_match_rank(name: &str, address: &str, folded_query: &str) -> Option<i64> {
    if folded_query.is_empty() {
        return Some(0);
    }
    let name = fold_contact_text(name);
    let address = fold_contact_text(address);
    let local_part_end = |s: &str| s.find('@').unwrap_or(s.len());
    let name_end = if name.contains('@') && !name.contains(char::is_whitespace) {
        local_part_end(&name)
    } else {
        name.len()
    };
    if has_word_prefix(&name, folded_query, name_end)
        || has_word_prefix(&address, folded_query, local_part_end(&address))
    {
        Some(0)
    } else if name.contains(folded_query) || address.contains(folded_query) {
        Some(1)
    } else {
        None
    }
}

/// Registers `contact_match(name, address, folded_query)` on `conn`:
/// `contact_match_rank` as a deterministic SQL function, a NULL argument
/// read as `''`. `Store::init` calls it, so every connection has it.
fn register_contact_match(conn: &Connection) -> Result<(), StoreError> {
    conn.create_scalar_function(
        "contact_match",
        3,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let name: Option<String> = ctx.get(0)?;
            let address: Option<String> = ctx.get(1)?;
            let query: Option<String> = ctx.get(2)?;
            Ok(contact_match_rank(
                name.as_deref().unwrap_or(""),
                address.as_deref().unwrap_or(""),
                query.as_deref().unwrap_or(""),
            ))
        },
    )?;
    Ok(())
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

/// The columns `event_row` reads, in its order.
const EVENT_ROW_COLUMNS: &str = "id, subject, start_utc, end_utc, is_all_day, location,
    organizer_name, organizer_addr, response_status, series_master_id,
    reminder_minutes, is_reminder_on, show_as, is_online_meeting,
    online_meeting_provider, join_url, categories, sensitivity, importance,
    event_type, graph_recurrence, is_cancelled, is_organizer,
    response_requested, allow_new_time_proposals, has_attachments,
    original_start_tz, original_end_tz, calendar_id, response_time, created_utc";

/// The rows a calendar grid draws (#2064), for `events_in_window` and
/// `events_in_range` (whose `FROM events` it names): a series master is
/// not an appointment once a row of its series (an occurrence or an
/// exception, of any date) is stored, since those are, and a cancelled
/// occurrence or exception is gone from the series. A master with none
/// stays: a new series is one `seriesMaster` row until its occurrences
/// sync, `local:` (#2034) and after its `CreateEvent` reconciles to its
/// Graph id alike. A cancelled `singleInstance` stays too, for Remove from
/// Calendar (#2024). `idx_events_series` serves the `EXISTS`.
const GRID_ROWS: &str = "NOT (event_type = 'seriesMaster'
        AND EXISTS (SELECT 1 FROM events AS o WHERE o.series_master_id = events.id))
    AND NOT (is_cancelled = 1 AND event_type IN ('occurrence', 'exception'))";

/// An `EventRow` from a row selected with `EVENT_ROW_COLUMNS`.
fn event_row(row: &Row<'_>) -> rusqlite::Result<EventRow> {
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
        response_time: row.get(29)?,
        created_utc: row.get(30)?,
    })
}

/// The stored calendars in `position` order (#1984).
fn stored_calendars_on(conn: &Connection) -> Result<Vec<Calendar>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, hex_color, is_default, can_edit, owner_name, owner_addr
         FROM calendars ORDER BY position ASC, id ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Calendar {
                id: row.get(0)?,
                name: row.get(1)?,
                color: row.get(2)?,
                hex_color: row.get(3)?,
                is_default: row.get(4)?,
                can_edit: row.get(5)?,
                owner_name: row.get(6)?,
                owner_addr: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The first stored default calendar's id, if any.
fn default_calendar_id_on(conn: &Connection) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT id FROM calendars WHERE is_default = 1 ORDER BY position ASC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

/// See `Store::calendar_selection`.
fn calendar_selection_on(conn: &Connection) -> Result<Option<Vec<String>>, StoreError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'calendar_selection'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Ok(Value::Array(items)) = json::parse(&raw) else {
        return Ok(None);
    };
    Ok(items
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect())
}

/// See `Store::synced_calendar_ids`.
fn synced_calendar_ids_on(conn: &Connection) -> Result<Vec<String>, StoreError> {
    let calendars = stored_calendars_on(conn)?;
    let ids = match calendar_selection_on(conn)? {
        Some(chosen) => {
            let chosen: HashSet<String> = chosen.into_iter().collect();
            calendars
                .into_iter()
                .filter(|c| chosen.contains(&c.id))
                .map(|c| c.id)
                .collect()
        }
        None => {
            let owner = calendars
                .iter()
                .find(|c| c.is_default)
                .map(|c| owner_key(&c.owner_addr))
                .filter(|o| !o.is_empty());
            calendars
                .into_iter()
                .filter(|c| match &owner {
                    Some(o) => c.is_default || owner_key(&c.owner_addr) == *o,
                    None => true,
                })
                .map(|c| c.id)
                .collect()
        }
    };
    Ok(ids)
}

/// A calendar owner address as compared: ASCII case and surrounding
/// whitespace ignored (`folder_contact_key`'s fold, #1934).
fn owner_key(addr: &str) -> String {
    addr.trim().to_ascii_lowercase()
}

/// Deletes a calendar's synced (non-`local:`) events, of any date; their
/// attendees go by `ON DELETE CASCADE`.
fn delete_calendar_events_on(conn: &Connection, calendar_id: &str) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM events WHERE calendar_id = ?1 AND id NOT LIKE 'local:%'",
        params![calendar_id],
    )?;
    // Its fetched ranges go with them (#2014): selected again, the
    // calendar fetches them again. So does its delta state (#2013), or
    // the calendar selected again would replay its link onto no rows.
    conn.execute(
        "DELETE FROM calendar_ranges WHERE calendar_id = ?1",
        params![calendar_id],
    )?;
    conn.execute(
        "DELETE FROM calendar_deltas WHERE calendar_id = ?1",
        params![calendar_id],
    )?;
    Ok(())
}

/// `apply_calendar_sync`'s step 1: pre-#1984 rows (`calendar_id = ''`)
/// take the stored default calendar's id, when there is one.
fn adopt_blank_calendar_rows_on(conn: &Connection) -> Result<(), StoreError> {
    if let Some(default) = default_calendar_id_on(conn)? {
        conn.execute(
            "UPDATE events SET calendar_id = ?1 WHERE calendar_id = ''",
            params![default],
        )?;
    }
    Ok(())
}

/// Replaces `f.calendar_id`'s synced (non-`local:`) events overlapping
/// `[from_utc, to_utc)` with `f`'s, each stored under that id, with their
/// attendees, and records the range as fetched at `stamp`.
///
/// With `keep_masters` (#2064) its stored `seriesMaster` rows stay: a
/// `FetchCalendarRange`'s plain `calendarView` never returns a master, so
/// replacing the range holding a series' first occurrence (the master's
/// own times) would drop the master its occurrences are filled from. A
/// delta round's initial window returns them, so it replaces them too.
fn replace_calendar_range_on(
    conn: &Connection,
    from_utc: &str,
    to_utc: &str,
    f: &FetchedCalendar,
    stamp: RangeStamp,
    keep_masters: bool,
) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM events
         WHERE calendar_id = ?3 AND start_utc < ?2 AND end_utc > ?1
               AND id NOT LIKE 'local:%'
               AND NOT (?4 AND event_type = 'seriesMaster')",
        params![from_utc, to_utc, f.calendar_id, keep_masters],
    )?;
    upsert_fetched_events_on(conn, f)?;
    record_calendar_range_on(conn, &f.calendar_id, from_utc, to_utc, stamp)
}

/// Upserts `f`'s events, each under `f.calendar_id` (the URL decides,
/// #1984), and replaces their attendees.
fn upsert_fetched_events_on(conn: &Connection, f: &FetchedCalendar) -> Result<(), StoreError> {
    for e in &f.events {
        if e.calendar_id == f.calendar_id {
            upsert_event_on(conn, e)?;
        } else {
            let e = NewEvent {
                calendar_id: f.calendar_id.clone(),
                ..e.clone()
            };
            upsert_event_on(conn, &e)?;
        }
    }
    for (id, atts) in &f.attendees {
        put_event_attendees_on(conn, id, atts)?;
    }
    Ok(())
}

/// Applies one calendar's delta round (#2013, `apply_calendar_sync`'s
/// step 2): `f`'s events are upserted under `f.calendar_id` with their
/// attendees; each `removed` id deletes that calendar's non-`local:` row
/// with that id and those whose `series_master_id` it is (a removed series
/// takes its occurrences; an id with no row is a no-op). The window is
/// then recorded as fetched at `stamp`: the round brought it up to date.
///
/// A removal deletes only rows overlapping the window: Graph's
/// calendarView delta also lists as `@removed` events outside the range
/// that were added, changed or deleted, which Graph may still have (an
/// event moved to day 200, a `FetchCalendarRange` row past the window,
/// history). An event that moved out of the window still has its old
/// in-window times here, so it goes. A removed id the round upserts
/// occurrences of is not a removed series: a series master that starts
/// before the window is "outside the range" when it is edited, and its
/// occurrences stay; only the row with that id goes.
///
/// Each `gone_masters` id's `seriesMaster` row of the calendar goes too,
/// of any date (#2064, `CalendarRound::Delta`), before the upserts.
///
/// Returns whether a series cascade deleted any row other than the removed
/// id's own. Then the window is not recorded as fetched, and the caller
/// drops the calendar's delta state: a master Graph lists as removed may
/// only have changed (its series end moved past the window, say), so the
/// next pass's initial round restores the occurrences Graph still has,
/// or confirms they are gone.
fn apply_calendar_delta_on(
    conn: &Connection,
    from_utc: &str,
    to_utc: &str,
    f: &FetchedCalendar,
    removed: &[String],
    gone_masters: &[String],
    stamp: RangeStamp,
) -> Result<bool, StoreError> {
    let named_masters: HashSet<&str> = f
        .events
        .iter()
        .filter_map(|e| e.series_master_id.as_deref())
        .collect();
    let mut cascaded = false;
    for id in removed {
        conn.execute(
            "DELETE FROM events
             WHERE calendar_id = ?1 AND id = ?2
                   AND start_utc < ?4 AND end_utc > ?3 AND id NOT LIKE 'local:%'",
            params![f.calendar_id, id, from_utc, to_utc],
        )?;
        if !named_masters.contains(id.as_str()) {
            let occurrences = conn.execute(
                "DELETE FROM events
                 WHERE calendar_id = ?1 AND series_master_id = ?2
                       AND start_utc < ?4 AND end_utc > ?3 AND id NOT LIKE 'local:%'",
                params![f.calendar_id, id, from_utc, to_utc],
            )?;
            cascaded |= occurrences > 0;
        }
    }
    for id in gone_masters {
        conn.execute(
            "DELETE FROM events
             WHERE calendar_id = ?1 AND id = ?2 AND event_type = 'seriesMaster'
                   AND id NOT LIKE 'local:%'",
            params![f.calendar_id, id],
        )?;
    }
    upsert_fetched_events_on(conn, f)?;
    if !cascaded {
        record_calendar_range_on(conn, &f.calendar_id, from_utc, to_utc, stamp)?;
    }
    Ok(cascaded)
}

/// What makes a stored occurrence blank (#2064): none of the text a series
/// gives it. Graph's calendarView delta returns an occurrence with only its
/// id, type, times and `seriesMasterId`, and every real event has an
/// organizer, so an occurrence with none of these was never filled.
const BLANK_OCCURRENCE: &str = "subject = '' AND organizer_name = '' AND organizer_addr = ''
    AND location = '' AND response_status = ''";

/// The columns an occurrence takes from its series master (#2064,
/// `fill_occurrences_on`): everything a calendar client shows of it except
/// its own identity, times, links and body (`event_body_on` refers to the
/// master's instead of copying it).
const SERIES_FIELDS: &str = "subject, location, organizer_name, organizer_addr,
    response_status, response_time, created_utc, body_preview, is_all_day, reminder_minutes, is_reminder_on, show_as,
    is_online_meeting, online_meeting_provider, join_url, categories, sensitivity,
    importance, is_cancelled, is_organizer, response_requested, allow_new_time_proposals,
    has_attachments, original_start_tz, original_end_tz";

/// Fills `f.calendar_id`'s occurrences from their series masters (#2064),
/// after a sync round's removals and upserts (`apply_calendar_sync`).
///
/// An occurrence is filled when it is a non-`local:` `occurrence` of that
/// calendar whose master is stored there as a `seriesMaster`, and either
/// the round upserted it, or the round upserted its master and the master
/// changed (`changed`, from `changed_masters_on` before the round's
/// writes), or it is blank
/// (`BLANK_OCCURRENCE`: a store from before #2064, or a master that came
/// later). An occurrence neither the round nor its master touched keeps
/// its own values: an optimistic local write (a `RespondEvent`'s
/// `response_status`) waits for its op. So does one whose master the round
/// brought unchanged (r3): every initial round lists or refetches the
/// masters of its occurrences, and filling every occurrence of the series,
/// of any date, from an unchanged master would overwrite a pending write
/// on one outside the window. Exceptions are full items, and never filled.
///
/// The master's `SERIES_FIELDS` are copied and its attendees replace the
/// occurrence's. This runs after `upsert_fetched_events_on`, which writes
/// each minimal occurrence's own (empty) attendee list, so the copy wins.
fn fill_occurrences_on(
    conn: &Connection,
    f: &FetchedCalendar,
    changed: &HashSet<String>,
) -> Result<(), StoreError> {
    let touched_occurrences: HashSet<&str> = f
        .events
        .iter()
        .filter(|e| e.event_type == "occurrence")
        .map(|e| e.id.as_str())
        .collect();
    let candidates: Vec<(String, String, bool)> = conn
        .prepare(&format!(
            "SELECT id, series_master_id, ({BLANK_OCCURRENCE}) FROM events AS o
             WHERE calendar_id = ?1 AND event_type = 'occurrence' AND id NOT LIKE 'local:%'
                   AND EXISTS (SELECT 1 FROM events AS m
                               WHERE m.id = o.series_master_id
                                     AND m.event_type = 'seriesMaster'
                                     AND m.calendar_id = o.calendar_id)
             ORDER BY id"
        ))?
        .query_map(params![f.calendar_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    for (id, master, blank) in candidates {
        if !(blank || touched_occurrences.contains(id.as_str()) || changed.contains(&master)) {
            continue;
        }
        conn.prepare_cached(&format!(
            "UPDATE events SET ({SERIES_FIELDS}) = (SELECT {SERIES_FIELDS} FROM events WHERE id = ?2)
             WHERE id = ?1"
        ))?
        .execute(params![id, master])?;
        conn.prepare_cached("DELETE FROM event_attendees WHERE event_id = ?1")?
            .execute(params![id])?;
        conn.prepare_cached(
            "INSERT INTO event_attendees (event_id, name, addr, type, response, response_time,
                                          proposed_start_utc, proposed_end_utc)
             SELECT ?1, name, addr, type, response, response_time, proposed_start_utc,
                    proposed_end_utc
             FROM event_attendees WHERE event_id = ?2 ORDER BY rowid",
        )?
        .execute(params![id, master])?;
    }
    Ok(())
}

/// The `seriesMaster`s among `f`'s events that changed (#2064 r3): not
/// stored as a master of `f.calendar_id`, or stored with another
/// `last_modified` (Graph's `lastModifiedDateTime`), or with either one
/// empty. Read before the round's writes, for `fill_occurrences_on`.
fn changed_masters_on(
    conn: &Connection,
    f: &FetchedCalendar,
) -> Result<HashSet<String>, StoreError> {
    let mut changed = HashSet::new();
    let mut stmt = conn.prepare_cached(
        "SELECT last_modified FROM events
         WHERE id = ?1 AND calendar_id = ?2 AND event_type = 'seriesMaster'",
    )?;
    for e in f.events.iter().filter(|e| e.event_type == "seriesMaster") {
        let stored: Option<String> = stmt
            .query_row(params![e.id, f.calendar_id], |r| r.get(0))
            .optional()?;
        let same = matches!(
            stored.as_deref(),
            Some(m) if !m.is_empty() && m == e.last_modified
        );
        if !same {
            changed.insert(e.id.clone());
        }
    }
    Ok(changed)
}

/// `Store::event_body` on `conn`: the row's own `body_html`, or for an
/// `occurrence` with none its stored master's (same calendar,
/// `seriesMaster`), which an occurrence refers to rather than copies
/// (#2064). `None` for no such row.
fn event_body_on(conn: &Connection, id: &str) -> Result<Option<String>, StoreError> {
    let body = conn
        .query_row(
            "SELECT CASE WHEN o.body_html = '' AND o.event_type = 'occurrence'
                         THEN coalesce(m.body_html, '') ELSE o.body_html END
             FROM events AS o
             LEFT JOIN events AS m ON m.id = o.series_master_id
                  AND m.event_type = 'seriesMaster' AND m.calendar_id = o.calendar_id
             WHERE o.id = ?1",
            params![id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(body)
}

/// Records `[from_utc, to_utc)` of `calendar_id` as fetched at `stamp.at`,
/// keeping the calendar's intervals disjoint: first its rows that can no
/// longer be fresh (`fetched_at <= stamp.fresh_since`) go, then every
/// interval overlapping the range is cut back to its pieces outside it
/// (with their own `fetched_at`), then the range goes in.
fn record_calendar_range_on(
    conn: &Connection,
    calendar_id: &str,
    from_utc: &str,
    to_utc: &str,
    stamp: RangeStamp,
) -> Result<(), StoreError> {
    conn.execute(
        "DELETE FROM calendar_ranges WHERE calendar_id = ?1 AND fetched_at <= ?2",
        params![calendar_id, stamp.fresh_since],
    )?;
    let overlapping: Vec<(i64, String, String, i64)> = conn
        .prepare(
            "SELECT rowid, from_utc, to_utc, fetched_at FROM calendar_ranges
             WHERE calendar_id = ?1 AND from_utc < ?3 AND to_utc > ?2",
        )?
        .query_map(params![calendar_id, from_utc, to_utc], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    let insert = |a: &str, b: &str, at: i64| {
        conn.execute(
            "INSERT INTO calendar_ranges (calendar_id, from_utc, to_utc, fetched_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![calendar_id, a, b, at],
        )
    };
    for (rowid, a, b, at) in overlapping {
        conn.execute(
            "DELETE FROM calendar_ranges WHERE rowid = ?1",
            params![rowid],
        )?;
        if a.as_str() < from_utc {
            insert(&a, from_utc, at)?;
        }
        if b.as_str() > to_utc {
            insert(to_utc, &b, at)?;
        }
    }
    insert(from_utc, to_utc, stamp.at)?;
    Ok(())
}

/// Days since the Unix epoch of a civil date (Howard Hinnant's
/// `days_from_civil`, the inverse of `civil_from_days`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Converts a count of days since the Unix epoch into a `(year, month, day)`
/// civil date (Howard Hinnant's `civil_from_days`).
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The number of days in a month (1..=12) of the proleptic Gregorian
/// calendar.
fn days_in_month(y: i64, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (days_from_civil(ny, nm, 1) - days_from_civil(y, m, 1)) as u32
}

/// A Unix timestamp as the store's `YYYY-MM-DDTHH:MM:SSZ` (the engine's
/// `unix_to_iso8601` too).
pub(crate) fn unix_secs_to_iso8601(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86400));
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// The store's `YYYY-MM-DDTHH:MM:SSZ` as a Unix timestamp; `None` when it
/// is not that shape or not a real date (Graph's no-date
/// `0000-00-00T00:00:00Z` fallback).
pub(crate) fn iso8601_to_unix_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<u32>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&m)
        || d == 0
        || d > days_in_month(i64::from(y), m)
        || h > 23
        || mi > 59
        || sec > 60
    {
        return None;
    }
    Some(days_from_civil(i64::from(y), m, d) * 86400 + i64::from(h * 3600 + mi * 60 + sec))
}

/// Brings a store from before #2054 up to date: adds the message reminder
/// columns and resets every folder's delta and backfill state (what
/// [`Store::clear_delta_links`] clears, without a reconverge: nothing was
/// quarantined), as a link Graph minted before then never returns the
/// reminder's `$expand`. One IMMEDIATE transaction that probes for the
/// column first, so a crash cannot leave the columns without the reset,
/// and a second connection opening the same file at once waits and then
/// finds the columns there. A store that has them (every fresh one, from
/// `SCHEMA_SQL`) is left alone. Returns whether it migrated.
fn migrate_message_reminders(conn: &Connection) -> Result<bool, StoreError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let has: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'is_reminder_on'",
        [],
        |r| r.get(0),
    )?;
    if has > 0 {
        return Ok(false);
    }
    tx.execute_batch(
        "ALTER TABLE messages ADD COLUMN is_reminder_on INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE messages ADD COLUMN reminder_at TEXT NOT NULL DEFAULT '';
         ALTER TABLE messages ADD COLUMN reminder_signal_at TEXT NOT NULL DEFAULT '';
         ALTER TABLE messages ADD COLUMN flag_due_at TEXT NOT NULL DEFAULT '';
         ALTER TABLE folders ADD COLUMN reminder_expand_refused INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE folders ADD COLUMN backfill_expand_refused INTEGER NOT NULL DEFAULT 0;
         UPDATE folders SET delta_link = NULL, backfill_link = NULL,
                            backfill_synced = 0, history_scope = NULL,
                            backfill_scope = NULL;",
    )?;
    tx.commit()?;
    Ok(true)
}

/// Brings a store from before #2081 up to date: adds
/// `messages.meeting_message_type` (old rows read `None`) and resets every
/// folder's delta and backfill state, as `migrate_message_reminders` does
/// and for the same reason: a link Graph minted before then carries the old
/// `$expand`, without the message class, so new mail through it would
/// never get its type. The expand-refused marks reset with the links, so
/// the next walk asks again. One IMMEDIATE transaction that probes for the
/// column first; a store that has it (every fresh one) is left alone.
/// Returns whether it migrated.
fn migrate_meeting_message_type(conn: &Connection) -> Result<bool, StoreError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let has: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'meeting_message_type'",
        [],
        |r| r.get(0),
    )?;
    if has > 0 {
        return Ok(false);
    }
    tx.execute_batch(
        "ALTER TABLE messages ADD COLUMN meeting_message_type TEXT;
         UPDATE folders SET delta_link = NULL, backfill_link = NULL,
                            backfill_synced = 0, history_scope = NULL,
                            backfill_scope = NULL, reminder_expand_refused = 0,
                            backfill_expand_refused = 0;",
    )?;
    tx.commit()?;
    Ok(true)
}

/// Brings a store from before #2088 up to date: adds the protection
/// columns (`sensitivity`, `has_sensitivity_label`,
/// `sensitivity_label_name`, `is_rights_protected`; old rows read
/// unprotected) and resets every folder's delta and backfill state and its
/// expand-refused marks, as `migrate_meeting_message_type` does and for the
/// same reason: a link Graph minted before then carries the old `$expand`,
/// with the class but without the protection properties, so new mail
/// through it would be stored unprotected. Runs after
/// `migrate_message_reminders`, which adds the marks it resets. One
/// IMMEDIATE transaction that probes for `sensitivity` first; a store that
/// has it (every fresh one) is left alone. Returns whether it migrated.
fn migrate_message_protection(conn: &Connection) -> Result<bool, StoreError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let has: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'sensitivity'",
        [],
        |r| r.get(0),
    )?;
    if has > 0 {
        return Ok(false);
    }
    tx.execute_batch(
        "ALTER TABLE messages ADD COLUMN sensitivity INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE messages ADD COLUMN has_sensitivity_label INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE messages ADD COLUMN sensitivity_label_name TEXT NOT NULL DEFAULT '';
         ALTER TABLE messages ADD COLUMN is_rights_protected INTEGER NOT NULL DEFAULT 0;
         UPDATE folders SET delta_link = NULL, backfill_link = NULL,
                            backfill_synced = 0, history_scope = NULL,
                            backfill_scope = NULL, reminder_expand_refused = 0,
                            backfill_expand_refused = 0;",
    )?;
    tx.commit()?;
    Ok(true)
}

/// Brings a store from before #2089 up to date: adds
/// `messages.meeting_reminder_on` (old rows read false). Unlike the
/// migrations before it, the delta and backfill state is **not** reset:
/// the `$expand` did not change (`PidLidReminderSet` came in #2054 and the
/// class in #2081), so every stored link already brings what new and
/// changed mail needs, and a store older than #2088 has just been reset by
/// `migrate_message_protection`. Rows stored before read false until a
/// sync re-reports them; a third whole-mailbox re-enumeration for a bell on
/// old mail is not worth it. One IMMEDIATE transaction that probes for the
/// column first, so a second connection opening the same file at once
/// waits and then finds it; a store that has it (every fresh one) is left
/// alone. Returns whether it migrated.
fn migrate_meeting_reminder(conn: &Connection) -> Result<bool, StoreError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let has: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'meeting_reminder_on'",
        [],
        |r| r.get(0),
    )?;
    if has > 0 {
        return Ok(false);
    }
    tx.execute_batch(
        "ALTER TABLE messages ADD COLUMN meeting_reminder_on INTEGER NOT NULL DEFAULT 0;",
    )?;
    tx.commit()?;
    Ok(true)
}

/// Brings a store from before #2096 up to date: adds
/// `messages.internet_message_id` and `messages.in_reply_to` (old rows read
/// `""`) and resets every folder's delta and backfill state and both
/// expand-refused marks, as `migrate_message_protection` does: a link Graph
/// minted before then carries the old `$select` (no `internetMessageId`, no
/// `isDraft`) and the old `$expand` (no `PidTagInReplyToId`), so new mail
/// through it would never get either id, and a draft made elsewhere would
/// stay `is_draft = 0`. The cost is one more re-enumeration. Last in
/// `Store::init`, after every migration whose columns or marks it resets.
/// One IMMEDIATE transaction that probes for `internet_message_id` first; a
/// store that has it (every fresh one) is left alone. Returns whether it
/// migrated.
///
/// In the same transaction, a one-off body fix: a draft body that an
/// engine from before #1901 wrote as compose html under `content_type =
/// 'text'` is relabelled `html`, so a host that opens a local text body as
/// text (lookxy's `draft_rich_text`, #2096) does not show, and save, its
/// tags. Only a local write (`fetched_as IS NULL`) of a draft row
/// (`is_draft`, or filed in Drafts or the local sentinel, as
/// `get_body_for` decides) whose content starts as `compose_html::to_html`
/// output does (`<p>`, `<ul><li>`, `<ol><li>`); a genuine text body, such
/// as a `createReply` of a plain-text message, and any fetched or
/// non-draft body are left alone. Every pre-#1901 store is pre-#2096, so
/// this runs on each of them once.
///
/// Then every body still `text` with no recorded format is marked fetched
/// as text (`fetched_as = 'text'`), in any folder: before #1901 every body
/// `FetchBody` stored was written with none, and a draft made elsewhere
/// reads `is_draft = 0` until the re-enumeration, wherever it is filed
/// (Drafts, a Drafts subfolder, a user folder, Deleted Items). Without it a
/// reader's text rendering of such a draft would read as a local write
/// (`Store::draft_body_is_local`) once the row is `is_draft`, and a host
/// would open it in its editor and save the flattened text over the
/// server's html draft. Left alone, as compose's own: a row holding an
/// unpushed edit (`local_edit > 0`), a `local:` draft, a reconciled one
/// (`local_id`) and a row in the local Drafts sentinel. For a non-draft
/// nothing changes: `None` and `Some("text")` both satisfy only a text ask
/// (`body_satisfies`). The cost, accepted: a plain-text `createReply` body
/// stored before #2096 then opens read-only; such a draft body satisfies a
/// text ask as before and no longer an html one, so an html embedder
/// fetches it once, and a fetch that comes still keeps an edited or queued
/// draft's body (#2082). The html relabel above keeps its draft-row
/// condition: it must never touch a genuine non-draft text body.
fn migrate_reply_links(conn: &Connection) -> Result<bool, StoreError> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let has: i64 = tx.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'internet_message_id'",
        [],
        |r| r.get(0),
    )?;
    if has > 0 {
        return Ok(false);
    }
    tx.execute_batch(
        "ALTER TABLE messages ADD COLUMN internet_message_id TEXT NOT NULL DEFAULT '';
         ALTER TABLE messages ADD COLUMN in_reply_to TEXT NOT NULL DEFAULT '';
         UPDATE folders SET delta_link = NULL, backfill_link = NULL,
                            backfill_synced = 0, history_scope = NULL,
                            backfill_scope = NULL, reminder_expand_refused = 0,
                            backfill_expand_refused = 0;",
    )?;
    tx.execute(
        "UPDATE bodies SET content_type = 'html'
         WHERE fetched_as IS NULL AND content_type = 'text'
           AND (ltrim(content) LIKE '<p>%' OR ltrim(content) LIKE '<ul><li>%'
                OR ltrim(content) LIKE '<ol><li>%')
           AND message_id IN (
               SELECT m.id FROM messages m LEFT JOIN folders f ON f.id = m.folder_id
               WHERE m.is_draft = 1 OR f.well_known_name = 'drafts' OR m.folder_id = ?1)",
        params![LOCAL_DRAFTS_SENTINEL_FOLDER_ID],
    )?;
    tx.execute(
        "UPDATE bodies SET fetched_as = 'text'
         WHERE fetched_as IS NULL AND content_type = 'text'
           AND message_id IN (
               SELECT id FROM messages
               WHERE local_edit = 0 AND local_id IS NULL
                 AND substr(id, 1, 6) <> 'local:' AND folder_id <> ?1)",
        params![LOCAL_DRAFTS_SENTINEL_FOLDER_ID],
    )?;
    tx.commit()?;
    Ok(true)
}

/// The partial index `Store::reply_draft_links` reads (#2096): only unsent
/// reply drafts, so it stays as small as the Drafts folder's replies. Its
/// `WHERE` is spelled exactly as the query's, or SQLite would not use it;
/// `folder_id` makes it covering for the query's Deleted Items join.
const REPLY_DRAFTS_INDEX_SQL: &str = "CREATE INDEX IF NOT EXISTS idx_messages_reply_drafts
     ON messages(conversation_id, in_reply_to, folder_id)
     WHERE is_draft = 1 AND in_reply_to <> ''";

/// `Store::reply_draft_links`' statement (#2096), over
/// `idx_messages_reply_drafts`; a test checks the plan. The `messages`
/// side of the WHERE is the index's, spelled the same; the folder join
/// leaves out a draft thrown away into Deleted Items (Graph keeps its
/// `isDraft`).
const REPLY_DRAFT_LINKS_SQL: &str = "SELECT DISTINCT m.conversation_id, m.in_reply_to
     FROM messages m LEFT JOIN folders f ON f.id = m.folder_id
     WHERE m.is_draft = 1 AND m.in_reply_to <> ''
       AND COALESCE(f.well_known_name, '') <> 'deleteditems'
     ORDER BY m.conversation_id, m.in_reply_to";

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
                    response_requested, allow_new_time_proposals, reminder_unset,
                    send_start_tz, send_start_local, send_end_tz, send_end_local
             FROM events WHERE id = ?1",
            params![id],
            |r| {
                let (recurrence, series_recurrence) =
                    decode_send_recurrence(&r.get::<_, String>(6)?);
                Ok(EventSendData {
                    subject: r.get(0)?,
                    start_utc: r.get(1)?,
                    end_utc: r.get(2)?,
                    is_all_day: r.get(3)?,
                    location: r.get(4)?,
                    body_html: r.get(5)?,
                    attendees: Vec::new(),
                    recurrence,
                    series_recurrence,
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
                    start_time_zone: r.get(18)?,
                    start_local: r.get(19)?,
                    end_time_zone: r.get(20)?,
                    end_local: r.get(21)?,
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
        "SELECT name, addr, type, response, response_time, proposed_start_utc,
                proposed_end_utc
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
                proposed_start_utc: row.get(5)?,
                proposed_end_utc: row.get(6)?,
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

/// The calendar `create_local_event` files a new event in (#2048): a named
/// one only when it is stored and `can_edit`, else the default calendar's id
/// (`''` before the calendar list is known).
fn create_calendar_on(conn: &Connection, chosen: &Option<String>) -> Result<String, StoreError> {
    let Some(id) = non_empty(chosen) else {
        return Ok(default_calendar_id_on(conn)?.unwrap_or_default());
    };
    let can_edit: Option<bool> = conn
        .query_row(
            "SELECT can_edit FROM calendars WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    match can_edit {
        None => Err(StoreError::Sql(format!("no calendar {id} is stored"))),
        Some(false) => Err(StoreError::Sql(format!("calendar {id} is read-only"))),
        Some(true) => Ok(id),
    }
}

/// `graph::client::event_zone_error` for both ends of a host's fields
/// (#2039), as a `StoreError` naming the end.
fn check_event_zones(f: &LocalEventFields) -> Result<(), StoreError> {
    for (end, zone, local, utc) in [
        ("start", &f.start_time_zone, &f.start_local, &f.start_utc),
        ("end", &f.end_time_zone, &f.end_local, &f.end_utc),
    ] {
        if let Some(why) = event_zone_error(zone, local, utc) {
            return Err(StoreError::Sql(format!("event {end} in {zone}: {why}")));
        }
    }
    Ok(())
}

/// The wall clock stored with a zone: none without one (`""` sends UTC
/// and ignores it, so a stray one is not kept).
fn zone_local(zone: &str, local: &str) -> String {
    if zone.is_empty() {
        String::new()
    } else {
        local.to_string()
    }
}

/// A host's enum string, `Some("")` read as `None` (Graph refuses `""`).
fn non_empty(v: &Option<String>) -> Option<String> {
    v.clone().filter(|s| !s.is_empty())
}

/// The attendees a host's `LocalEventFields::attendees` stores, given the
/// event's stored ones (`&[]` on a create): one whose address matches a
/// stored attendee (case-insensitive) keeps that row's response, response
/// time and proposed new time (#2023), and its type when the input's is `""`
/// (#1994); a new one is `required` (or its given type) with no response and
/// no proposal.
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
                proposed_start_utc: old
                    .map(|s| s.proposed_start_utc.clone())
                    .unwrap_or_default(),
                proposed_end_utc: old.map(|s| s.proposed_end_utc.clone()).unwrap_or_default(),
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

/// The key `events.recurrence` wraps a lossless recurrence in (#2034):
/// `{"graphRecurrence": <EventRecurrence::to_json>}`. A bare object is the
/// create-side `Recurrence::to_json`, as rows before #2034 hold it. The
/// wrapper keeps the two apart: `Recurrence::from_json` would read a bare
/// lossless daily, weekly or absoluteMonthly text, dropping its `index`,
/// `month` and `numbered` range and reading `"endDate":""` as an end date.
const SEND_RECURRENCE_KEY: &str = "graphRecurrence";

/// The `events.recurrence` text `create_local_event` stores: the lossless
/// recurrence wrapped (`SEND_RECURRENCE_KEY`) when set, else the create-side
/// one bare, else `''`.
fn encode_send_recurrence(
    recurrence: Option<&Recurrence>,
    series_recurrence: Option<&EventRecurrence>,
) -> String {
    if let Some(r) = series_recurrence {
        Value::Object(vec![(SEND_RECURRENCE_KEY.to_string(), r.to_json())]).to_string()
    } else {
        recurrence
            .map(|r| r.to_json().to_string())
            .unwrap_or_default()
    }
}

/// Reads `events.recurrence` (`encode_send_recurrence`) as
/// `(recurrence, series_recurrence)`, at most one of them `Some`. Lenient:
/// `''`, text that does not parse, a wrapper whose value has no `pattern`
/// object, and a bare object `Recurrence::from_json` does not read are all
/// `(None, None)`.
fn decode_send_recurrence(s: &str) -> (Option<Recurrence>, Option<EventRecurrence>) {
    if s.is_empty() {
        return (None, None);
    }
    let Ok(v) = json::parse(s) else {
        return (None, None);
    };
    match v.get(SEND_RECURRENCE_KEY) {
        Some(inner) => (None, EventRecurrence::from_json(inner)),
        None => (Recurrence::from_json(&v), None),
    }
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

/// The `messages` columns `map_message_row` reads, in its order.
/// `messages_in_folder`, `message_row`, `message_rows`, `search`, `draft`
/// and `due_message_reminders` select this list, so they cannot drift from
/// the mapping; only `conversations_in_folder` still spells the columns
/// out (twice: its `keyed` CTE and its outer select).
const MESSAGE_ROW_COLUMNS: &str = "id, folder_id, conversation_id, subject, from_name, from_addr,
    to_recipients, cc_recipients, received_at, sent_at,
    is_read, is_flagged, has_attachments, importance, preview, is_draft,
    bcc_recipients, is_meeting_request, categories,
    is_reminder_on, reminder_at, reminder_signal_at, flag_due_at,
    meeting_message_type,
    sensitivity, has_sensitivity_label, sensitivity_label_name, is_rights_protected,
    meeting_reminder_on, internet_message_id, in_reply_to";

/// How many columns `MESSAGE_ROW_COLUMNS` names: the index of the first
/// column a query selects after them (`Store::draft`'s body).
const BODY_AFTER_ROW_COLUMNS: usize = 31;

/// `Store::due_message_reminders`'s statement (#2054): flagged rows whose
/// reminder is on and whose next signal (`reminder_signal_at`, else
/// `reminder_at`), bound once as `due`, is set and not after `?1`. The
/// `is_reminder_on = 1` term lets SQLite use the partial
/// `idx_messages_reminder`; a test checks the plan.
fn due_message_reminders_sql() -> String {
    format!(
        "SELECT {MESSAGE_ROW_COLUMNS} FROM (
             SELECT *, COALESCE(NULLIF(reminder_signal_at, ''), reminder_at) AS due
             FROM messages
             WHERE is_flagged = 1 AND is_reminder_on = 1
         )
         WHERE due <> '' AND due <= ?1
         ORDER BY due, id"
    )
}

/// `Store::message_rows`'s statement (#2053): the rows whose id is in the
/// JSON array `?1`. SQLite plans the `IN` subquery as one primary-key
/// lookup per listed id, not a scan of `messages`; a test checks the plan.
fn message_rows_sql() -> String {
    format!(
        "SELECT {MESSAGE_ROW_COLUMNS} FROM messages
         WHERE id IN (SELECT value FROM json_each(?1))"
    )
}

/// Maps one row of a `SELECT {MESSAGE_ROW_COLUMNS} FROM messages ...` query
/// (that exact column order) to a `MessageRow`. Shared by
/// `messages_in_folder`, `message_row`, `message_rows`, `search`, `draft`
/// and `conversations_in_folder`, which all select those columns in that
/// order from `messages` (`search`, `draft` and `conversations_in_folder`
/// spell the list out), so there's only one place mapping can drift out of
/// sync with the column list.
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
        is_reminder_on: row.get(19)?,
        reminder_at: row.get(20)?,
        reminder_signal_at: row.get(21)?,
        flag_due_at: row.get(22)?,
        meeting_message_type: row
            .get::<_, Option<String>>(23)?
            .as_deref()
            .and_then(MeetingMessageType::from_graph),
        protection: MessageProtection {
            sensitivity: Sensitivity::from_mapi(row.get(24)?),
            has_label: row.get(25)?,
            label_name: row.get(26)?,
            is_rights_protected: row.get(27)?,
        },
        meeting_reminder_on: row.get(28)?,
        internet_message_id: row.get(29)?,
        in_reply_to: row.get(30)?,
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
/// row from a `NewEvent`, shared by `Store::upsert_event`,
/// `Store::replace_events_in_window` and `Store::apply_calendar_sync` (on
/// their transactions, which deref to a `Connection`) so every column
/// `NewEvent` carries is written by all three:
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
             original_end_tz, calendar_id, response_time, created_utc
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                   ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                   ?31, ?32, ?33, ?34, ?35)
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
             calendar_id = excluded.calendar_id,
             response_time = excluded.response_time,
             created_utc = excluded.created_utc",
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
            e.response_time,
            e.created_utc,
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
            "INSERT INTO event_attendees (event_id, name, addr, type, response, response_time,
                                          proposed_start_utc, proposed_end_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                att.name,
                att.addr,
                att.r#type,
                att.response,
                att.response_time,
                att.proposed_start_utc,
                att.proposed_end_utc
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
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: Vec::new(),
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
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

    #[test]
    fn message_folder_reads_a_rows_folder() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1");
        folder(&s, "F2");
        s.upsert_message("F1", &msg("01", false)).unwrap();
        assert_eq!(s.message_folder("01").unwrap().as_deref(), Some("F1"));
        s.move_message("01", "F2").unwrap();
        assert_eq!(s.message_folder("01").unwrap().as_deref(), Some("F2"));
        assert_eq!(s.message_folder("missing").unwrap(), None);
    }

    /// A message whose text columns all differ from each other and from the
    /// defaults, so a mapping that reads a wrong column shows (#2053). The
    /// bools are `flags`' bits: is_read, is_flagged, has_attachments,
    /// is_draft, is_meeting_request (bit 0 up). `Message` has no bcc, so a
    /// test sets `bcc_recipients` itself (`set_bcc`).
    fn rich_msg(id: &str, flags: u8) -> Message {
        let bit = |i: u8| flags & (1 << i) != 0;
        let mut m = msg(id, bit(0));
        m.conversation_id = format!("conv{id}");
        m.is_flagged = bit(1);
        m.has_attachments = bit(2);
        m.is_draft = bit(3);
        m.is_meeting_request = bit(4);
        m.importance = "high".into();
        m.categories = vec!["Work".into(), "Red".into()];
        m.to = vec![Recipient {
            name: "T".into(),
            address: "t@x".into(),
        }];
        m.cc = vec![Recipient {
            name: "C".into(),
            address: "c@x".into(),
        }];
        m.sent = format!("2026-06-{id}T00:00:00Z");
        m
    }

    /// Bit patterns for three `rich_msg`s under which every pair of the five
    /// bool columns differs in at least one message, so swapping any two
    /// shows: is_read 100, is_flagged 010, has_attachments 001, is_draft
    /// 110, is_meeting_request 011 (messages 1, 2, 3).
    const RICH_FLAGS: [u8; 3] = [0b01001, 0b11010, 0b10100];

    fn set_bcc(s: &Store, id: &str, bcc: &str) {
        s.conn
            .execute(
                "UPDATE messages SET bcc_recipients = ?2 WHERE id = ?1",
                params![id, bcc],
            )
            .unwrap();
    }

    /// Asserts every field of `row` against the message it was stored from.
    fn assert_row_is(row: &MessageRow, m: &Message, folder_id: &str, bcc: &str) {
        assert_eq!(row.id, m.id);
        assert_eq!(row.folder_id, folder_id);
        assert_eq!(row.conversation_id, m.conversation_id);
        assert_eq!(row.subject, m.subject);
        assert_eq!(row.from_name, m.from.name);
        assert_eq!(row.from_addr, m.from.address);
        assert!(row.to_recipients.contains("t@x"), "{}", row.to_recipients);
        assert!(!row.to_recipients.contains("c@x"), "{}", row.to_recipients);
        assert!(row.cc_recipients.contains("c@x"), "{}", row.cc_recipients);
        assert!(!row.cc_recipients.contains("t@x"), "{}", row.cc_recipients);
        assert_eq!(row.received_at, m.received);
        assert_eq!(row.sent_at, m.sent);
        assert_eq!(row.is_read, m.is_read);
        assert_eq!(row.is_flagged, m.is_flagged);
        assert_eq!(row.has_attachments, m.has_attachments);
        assert_eq!(row.importance, m.importance);
        assert_eq!(row.preview, m.preview);
        assert_eq!(row.is_draft, m.is_draft);
        assert_eq!(row.bcc_recipients, bcc);
        assert_eq!(row.is_meeting_request, m.is_meeting_request);
        assert_eq!(row.categories, m.categories);
    }

    #[test]
    fn message_row_reads_a_bodiless_row_as_messages_in_folder_does() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1");
        let stored: Vec<Message> = ["01", "02", "03"]
            .iter()
            .zip(RICH_FLAGS)
            .map(|(id, flags)| rich_msg(id, flags))
            .collect();
        for m in &stored {
            s.upsert_message("F1", m).unwrap();
            set_bcc(&s, &m.id, "b@x");
        }
        s.put_body(
            "02",
            &Body {
                content_type: "text".into(),
                content: "hi".into(),
            },
        )
        .unwrap();
        let listed = s.messages_in_folder("F1", 10, 0).unwrap();
        assert_eq!(listed.len(), 3);
        for want in &listed {
            assert_eq!(s.message_row(&want.id).unwrap().as_ref(), Some(want));
        }
        // The rows' fields are the stored messages', not only the list's
        // (the list selects the same `MESSAGE_ROW_COLUMNS`).
        for m in &stored {
            assert_row_is(&s.message_row(&m.id).unwrap().unwrap(), m, "F1", "b@x");
        }

        assert_eq!(s.message_row("missing").unwrap(), None);
        // Exact current ids: a moved message's old id is not followed.
        assert!(s.follow_move("01", "01m").unwrap());
        assert_eq!(s.message_row("01").unwrap(), None);
        assert_eq!(s.message_row("01m").unwrap().unwrap().subject, "s01");
    }

    #[test]
    fn message_rows_returns_only_stored_ids_once() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1");
        folder(&s, "F2");
        s.upsert_message("F1", &rich_msg("01", RICH_FLAGS[0]))
            .unwrap();
        s.upsert_message("F1", &msg("02", false)).unwrap();
        let in_f2 = rich_msg("03", RICH_FLAGS[1]);
        s.upsert_message("F2", &in_f2).unwrap();
        s.put_body(
            "03",
            &Body {
                content_type: "html".into(),
                content: "<p>x</p>".into(),
            },
        )
        .unwrap();
        let mut want: Vec<MessageRow> = s
            .messages_in_folder("F1", 10, 0)
            .unwrap()
            .into_iter()
            .chain(s.messages_in_folder("F2", 10, 0).unwrap())
            .filter(|r| r.id != "02")
            .collect();
        want.sort_by(|a, b| a.id.cmp(&b.id));
        let mut got = s
            .message_rows(&["03", "nope", "01", "03", "", "01"])
            .unwrap();
        got.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(got, want);
        assert_row_is(&got[1], &in_f2, "F2", "");
        assert!(s.message_rows(&[]).unwrap().is_empty());
        assert!(s.message_rows(&["nope"]).unwrap().is_empty());
    }

    #[test]
    fn message_rows_takes_more_ids_than_the_variable_limit() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F1");
        for id in ["05", "17", "28"] {
            s.upsert_message("F1", &msg(id, false)).unwrap();
        }
        // More than SQLite's default SQLITE_MAX_VARIABLE_NUMBER (32766).
        let ids: Vec<String> = (0..40_000).map(|i| format!("{i:02}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut got: Vec<String> = s
            .message_rows(&refs)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        got.sort();
        assert_eq!(got, ["05", "17", "28"]);
    }

    #[test]
    fn message_rows_reads_messages_by_its_primary_key() {
        let s = Store::open_in_memory().unwrap();
        let mut stmt = s
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {}", message_rows_sql()))
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map(params!["[]"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|d| d.contains("SEARCH messages") && d.contains("(id=?)")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|d| d.contains("SCAN messages")),
            "{plan:?}"
        );
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
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: vec![],
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
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
        m.is_reminder_on = true;
        m.reminder_at = "2026-07-13T08:00:00Z".into();
        m.reminder_signal_at = "2026-07-13T09:00:00Z".into();
        m.flag_due_at = "2026-07-14T00:00:00Z".into();
        m.meeting_message_type = Some(MeetingMessageType::Declined);
        m.protection = MessageProtection {
            sensitivity: Sensitivity::Private,
            has_label: true,
            label_name: "General".into(),
            is_rights_protected: true,
        };
        m.meeting_reminder_on = true;
        m.internet_message_id = "<13@x>".into();
        m.in_reply_to = "<12@x>".into();
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
            ("is_reminder_on", V::Integer(1)),
            ("reminder_at", t("2026-07-13T08:00:00Z")),
            ("reminder_signal_at", t("2026-07-13T09:00:00Z")),
            ("flag_due_at", t("2026-07-14T00:00:00Z")),
            ("meeting_message_type", t("meetingDeclined")),
            ("sensitivity", V::Integer(2)),
            ("has_sensitivity_label", V::Integer(1)),
            ("sensitivity_label_name", t("General")),
            ("is_rights_protected", V::Integer(1)),
            ("meeting_reminder_on", V::Integer(1)),
            ("internet_message_id", t("<13@x>")),
            ("in_reply_to", t("<12@x>")),
        ];
        expect(&s, "13", &first);

        // Over the row, with every column changed (to empty / false where
        // a value can be), moved to G; bcc (not a Graph column here) kept,
        // and so are the two ids (#2096: a merge never clears a stored one).
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
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: vec![],
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
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
                ("is_reminder_on", V::Integer(0)),
                ("reminder_at", t("")),
                ("reminder_signal_at", t("")),
                ("flag_due_at", t("")),
                ("meeting_message_type", V::Null),
                ("sensitivity", V::Integer(0)),
                ("has_sensitivity_label", V::Integer(0)),
                ("sensitivity_label_name", t("")),
                ("is_rights_protected", V::Integer(0)),
                ("meeting_reminder_on", V::Integer(0)),
                ("internet_message_id", t("<13@x>")),
                ("in_reply_to", t("<12@x>")),
            ],
        );
    }

    /// The `MessageFields` of a complete Graph item without `isDraft` (one
    /// built in code; a delta item carries it since #2096) and without
    /// `@odata.type` (a plain message).
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
        // wildcard would. An unescaped "a_b" LIKE '%a_b%' matches both rows
        // (the `_` matches any single character, including "x"); the match
        // is a literal substring since #1890's `contact_match`.
        let got = s.search_contacts("a_b", 10).unwrap();
        let addrs: Vec<&str> = got.iter().map(|c| c.address.as_str()).collect();
        assert_eq!(addrs, ["a_b@x.com"]); // "axb@x.com" must NOT show up here

        // The reverse query, "axb", must match only the row that literally
        // contains "axb" — not the underscore row (a literal "_" isn't "x").
        let got = s.search_contacts("axb", 10).unwrap();
        let addrs: Vec<&str> = got.iter().map(|c| c.address.as_str()).collect();
        assert_eq!(addrs, ["axb@x.com"]);
    }

    /// A local contact with `name`, `address` and `frequency`, for the
    /// contact search tests (#1890).
    fn search_contact(name: &str, address: &str, frequency: i64) -> Contact {
        Contact {
            name: name.into(),
            address: address.into(),
            source: "local".into(),
            last_seen: "2026-07-01T00:00:00Z".into(),
            frequency,
            relevance: None,
        }
    }

    fn found_addresses(s: &Store, query: &str, limit: i64) -> Vec<String> {
        s.search_contacts(query, limit)
            .unwrap()
            .into_iter()
            .map(|c| c.address)
            .collect()
    }

    #[test]
    fn search_contacts_folds_any_script() {
        // #1890: SQLite's `lower()`/`LIKE` fold ASCII only, so none of these
        // was found by its lowercase spelling, nor even typed as stored.
        let s = Store::open_in_memory().unwrap();
        s.upsert_contact(&search_contact("Борис Кудряшов", "boris@x.com", 1))
            .unwrap();
        s.upsert_contact(&search_contact("Élise Martin", "elise@x.com", 1))
            .unwrap();
        s.upsert_contact(&search_contact("Οδυσσέας Ελύτης", "odysseas@x.com", 1))
            .unwrap();
        for q in ["борис", "кудр", "Борис", "БОРИС", "Кудряшов"] {
            assert_eq!(found_addresses(&s, q, 6), ["boris@x.com"], "query {q:?}");
        }
        for q in ["élise", "Élise", "ÉLISE"] {
            assert_eq!(found_addresses(&s, q, 6), ["elise@x.com"], "query {q:?}");
        }
        // `to_lowercase` writes a word-final capital Σ as ς ("οδυς"), which
        // only the fold's ς -> σ step finds in "οδυσσέας".
        for q in ["ΟΔΥΣ", "οδυσ", "Οδυσσέας", "ελύτ"] {
            assert_eq!(found_addresses(&s, q, 6), ["odysseas@x.com"], "query {q:?}");
        }
    }

    #[test]
    fn search_contacts_ranks_word_prefix_with_name_prefix() {
        // #1890: "Wei Li" matches "li" at a word start, so it ranks with
        // whole-name prefixes, ahead of 60 frequent mid-word matches that a
        // small limit used to keep instead.
        let s = Store::open_in_memory().unwrap();
        for i in 0..60 {
            s.upsert_contact(&search_contact(
                &format!("Amelia N{i}"),
                &format!("amelia{i}@x.com"),
                50,
            ))
            .unwrap();
        }
        // "li" only in the domain: a word start there never ranks first.
        s.upsert_contact(&search_contact("Zed", "zed@linkedin.com", 100))
            .unwrap();
        s.upsert_contact(&search_contact("Wei Li", "wei@x.com", 1))
            .unwrap();
        let got = found_addresses(&s, "li", 6);
        assert_eq!(got.len(), 6);
        assert_eq!(got[0], "wei@x.com");
        // The rest are frequency-ordered interior matches: Zed first.
        assert_eq!(got[1], "zed@linkedin.com");
    }

    #[test]
    fn search_contacts_empty_query_and_negative_limit() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_contact(&search_contact("Ann Lee", "ann@x.com", 1))
            .unwrap();
        s.upsert_contact(&search_contact("Борис", "boris@x.com", 9))
            .unwrap();
        s.upsert_contact(&search_contact("", "noname@x.com", 5))
            .unwrap();
        // An empty query matches every contact, in the tie order.
        assert_eq!(
            found_addresses(&s, "", 10),
            ["boris@x.com", "noname@x.com", "ann@x.com"]
        );
        // A negative limit means no limit.
        assert_eq!(found_addresses(&s, "", -1).len(), 3);
        assert_eq!(found_addresses(&s, "x.com", -1).len(), 3);
        assert_eq!(found_addresses(&s, "", 2).len(), 2);
    }

    #[test]
    fn all_contacts_lists_every_row_in_rank_order() {
        let s = Store::open_in_memory().unwrap();
        let mut graph = search_contact("Graph Two", "g2@x.com", 0);
        graph.relevance = Some(2);
        s.upsert_contact(&graph).unwrap();
        let mut graph = search_contact("Graph One", "g1@x.com", 0);
        graph.relevance = Some(1);
        s.upsert_contact(&graph).unwrap();
        s.upsert_contact(&search_contact("Rare", "rare@x.com", 1))
            .unwrap();
        s.upsert_contact(&search_contact("Often", "often@x.com", 7))
            .unwrap();
        let mut older = search_contact("Often Too", "older@x.com", 7);
        older.last_seen = "2026-06-01T00:00:00Z".into();
        s.upsert_contact(&older).unwrap();
        let mut b = search_contact("Bea", "bea@x.com", 3);
        b.last_seen = "2026-05-01T00:00:00Z".into();
        s.upsert_contact(&b).unwrap();
        let mut a = search_contact("Abe", "abe@x.com", 3);
        a.last_seen = "2026-05-01T00:00:00Z".into();
        s.upsert_contact(&a).unwrap();
        let all: Vec<String> = s
            .all_contacts()
            .unwrap()
            .into_iter()
            .map(|c| c.address)
            .collect();
        let order = [
            "g1@x.com",    // relevance 1
            "g2@x.com",    // relevance 2
            "often@x.com", // nulls last; frequency 7, newer
            "older@x.com", // frequency 7, older
            "abe@x.com",   // frequency 3, same date: name
            "bea@x.com",
            "rare@x.com", // frequency 1
        ];
        assert_eq!(all, order);
        // The same order as an empty search.
        assert_eq!(found_addresses(&s, "", -1), order);
    }

    #[test]
    fn contact_match_rank_word_starts_and_fold() {
        let rank = |name: &str, address: &str, q: &str| {
            contact_match_rank(name, address, &fold_contact_text(q))
        };
        // Word starts in the name: at 0, after a space, after `-` / `'` / `,`.
        assert_eq!(rank("Wei Li", "w@x.com", "wei"), Some(0));
        assert_eq!(rank("Wei Li", "w@x.com", "li"), Some(0));
        assert_eq!(rank("Jean-Luc Picard", "j@x.com", "luc"), Some(0));
        assert_eq!(rank("Sean O'Brien", "s@x.com", "brien"), Some(0));
        assert_eq!(rank("Li, Wei", "w@x.com", "wei"), Some(0));
        assert_eq!(rank("Борис-Кудряшов", "b@x.com", "кудр"), Some(0));
        // Mid-word only: an interior match.
        assert_eq!(rank("Amelia", "a@x.com", "li"), Some(1));
        // A mid-word occurrence does not hide a later word-start one.
        assert_eq!(rank("Amelia Lin", "a@x.com", "li"), Some(0));
        // A query spanning a word start; one that a non-overlapping scan
        // would skip ("a a" first matches at 1, the word start is at 3).
        assert_eq!(rank("xa a a", "q@x.com", "a a"), Some(0));
        // The address: start and a `.` in the local part are word starts…
        assert_eq!(rank("", "john.smith@x.com", "john"), Some(0));
        assert_eq!(rank("", "john.smith@x.com", "smith"), Some(0));
        assert_eq!(rank("", "ann@x.com", "ann@x"), Some(0));
        // …the domain never is.
        assert_eq!(rank("Zed", "zed@linkedin.com", "li"), Some(1));
        assert_eq!(rank("Zed", "zed@linkedin.com", "@li"), Some(1));
        // A name that is itself an address takes the address rule.
        assert_eq!(rank("zed@linkedin.com", "zed@linkedin.com", "li"), Some(1));
        assert_eq!(rank("Zed (zed@linkedin.com)", "z@x.com", "linked"), Some(0));
        // No match, an empty name, an empty query.
        assert_eq!(rank("Wei Li", "w@x.com", "bob"), None);
        assert_eq!(rank("", "w@x.com", "bob"), None);
        assert_eq!(rank("", "", ""), Some(0));
        assert_eq!(rank("Wei Li", "w@x.com", ""), Some(0));
        // Final sigma: "ΟΔΥΣ" lowercases to "οδυς"; folded, it is "οδυσ".
        assert_eq!(fold_contact_text("ΟΔΥΣ"), "οδυσ");
        assert_eq!(rank("Οδυσσέας", "o@x.com", "ΟΔΥΣ"), Some(0));
        assert_eq!(rank("Οδυσσέας", "o@x.com", "δυσσεας"), None); // accents are kept
        assert_eq!(rank("Οδυσσέας", "o@x.com", "δυσσέας"), Some(1));
        // `İ` folds to a plain `i` (`to_lowercase` alone gives `i` + U+0307);
        // `ß` stays `ß` (and does not match `ss`). Neither panics.
        assert_eq!(fold_contact_text("İlker"), "ilker");
        assert_eq!(rank("İstanbul Ali", "i@x.com", "ali"), Some(0));
        assert_eq!(rank("İstanbul", "i@x.com", "i"), Some(0));
        assert_eq!(rank("İlker", "i@x.com", "ilk"), Some(0));
        assert_eq!(rank("Straße", "s@x.com", "STRASSE"), None);
        assert_eq!(rank("Straße", "s@x.com", "STRAßE"), Some(0));
        // A combining mark or a joiner never opens a word: the word goes on.
        assert_eq!(rank("İpek", "i@x.com", "pek"), Some(1));
        assert_eq!(rank("i\u{307}pek", "i@x.com", "pek"), Some(1));
        assert_eq!(rank("E\u{301}lise Martin", "e@x.com", "lise"), Some(1));
        assert_eq!(rank("E\u{301}lise Martin", "e@x.com", "mart"), Some(0));
        // Devanagari क्षमा: क, virama U+094D, then षमा.
        assert_eq!(rank("क्षमा", "k@x.com", "षमा"), Some(1));
        // Persian نیک‌نام: a ZWNJ inside one word.
        assert_eq!(rank("نیک\u{200C}نام", "n@x.com", "نام"), Some(1));
        // Real separators still open words, `’` too.
        assert_eq!(rank("Sean O’Brien", "s@x.com", "brien"), Some(0));
        assert_eq!(rank("Wei (Li)", "w@x.com", "li"), Some(0));
        assert_eq!(rank("", "wei.li@x.com", "li"), Some(0));
        // A separator after a mark still separates.
        assert_eq!(rank("Re\u{301} Li", "r@x.com", "li"), Some(0));
    }

    #[test]
    fn search_contacts_finds_a_turkish_dotted_capital_by_any_case() {
        // #1890 r1: `to_lowercase` writes `İ` as `i` + U+0307, so `ilk` was
        // not in "i̇lker kaya" and only a query typed with `İ` found it.
        let s = Store::open_in_memory().unwrap();
        // The address holds no "ilk", so only the name can find the row.
        s.upsert_contact(&search_contact("İlker Kaya", "kaya@x.com", 1))
            .unwrap();
        // A frequent mid-word "ilk", found by every spelling, ranks after.
        s.upsert_contact(&search_contact("Bilkis Ay", "bilkis@x.com", 9))
            .unwrap();
        for q in ["ilk", "İlk", "ILK"] {
            assert_eq!(
                found_addresses(&s, q, 6),
                ["kaya@x.com", "bilkis@x.com"],
                "query {q:?}"
            );
        }
    }

    #[test]
    fn contact_match_reads_a_null_argument_as_empty() {
        // The SQL function never fails the query on a NULL column.
        let s = Store::open_in_memory().unwrap();
        let got: Option<i64> = s
            .conn
            .query_row("SELECT contact_match(NULL, 'w@x.com', 'w')", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(got, Some(0));
        let got: Option<i64> = s
            .conn
            .query_row("SELECT contact_match('Wei', NULL, NULL)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(got, Some(0));
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

    /// A contact record with one address, its id derived from it.
    fn fc(name: &str, address: &str) -> FolderContact {
        contact_record(&format!("C-{}", address.trim()), name, &[address])
    }

    fn contact_record(id: &str, display_name: &str, addresses: &[&str]) -> FolderContact {
        FolderContact {
            id: id.into(),
            change_key: format!("ck-{id}"),
            fields: ContactFields {
                display_name: display_name.into(),
                email_addresses: addresses
                    .iter()
                    .map(|a| Recipient {
                        name: display_name.into(),
                        address: (*a).into(),
                    })
                    .collect(),
                ..ContactFields::default()
            },
        }
    }

    fn full_fields() -> ContactFields {
        ContactFields {
            given_name: "Ann".into(),
            middle_name: "B".into(),
            surname: "Cole".into(),
            display_name: "Ann Cole".into(),
            file_as: "Cole, Ann".into(),
            email_addresses: vec![
                Recipient {
                    name: "Ann".into(),
                    address: "Ann@X.com".into(),
                },
                Recipient {
                    name: "Home".into(),
                    address: "ann@home.example".into(),
                },
            ],
            business_phones: vec!["+1 555 0100".into(), "+1 555 0101".into()],
            home_phones: vec!["+1 555 0200".into()],
            mobile_phone: "+1 555 0300".into(),
            company_name: "Contoso".into(),
            job_title: "Engineer".into(),
            department: "R&D".into(),
            business_address: PhysicalAddress {
                street: "1 Main St".into(),
                city: "Springfield".into(),
                state: "IL".into(),
                postal_code: "62701".into(),
                country_or_region: "United States".into(),
            },
            home_address: PhysicalAddress {
                street: "2 Elm St".into(),
                city: "Shelbyville".into(),
                ..PhysicalAddress::default()
            },
            birthday: "1980-05-17".into(),
            personal_notes: "likes tea".into(),
            categories: vec!["Friends".into(), "Work".into()],
        }
    }

    fn contact_ids(s: &Store) -> Vec<String> {
        s.folder_contacts()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect()
    }

    #[test]
    fn is_contact_matches_only_folder_contacts() {
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
        s.replace_contacts(&[fc("Ann", "Ann@X.com")]).unwrap();
        // A `/me/people` person and a mined sender are not Contacts.
        s.upsert_contact(&Contact {
            name: "Bob".into(),
            address: "bob@x.com".into(),
            source: "graph".into(),
            last_seen: String::new(),
            frequency: 0,
            relevance: Some(0),
        })
        .unwrap();
        let mut m = msg("1", false);
        m.from = Recipient {
            name: "Carol".into(),
            address: "carol@x.com".into(),
        };
        s.upsert_message("inbox", &m).unwrap();
        s.refresh_local_contacts().unwrap();
        assert_eq!(s.search_contacts("carol", 1).unwrap().len(), 1);

        for yes in ["ann@x.com", "ANN@x.COM", " ann@x.com "] {
            assert!(s.is_contact(yes).unwrap(), "{yes:?}");
        }
        for no in [
            "bob@x.com",
            "carol@x.com",
            "nn@x.com",
            "ann@x",
            "ann@x.co",
            "ann@x.com.evil",
            "",
            "   ",
            "%",
            "_nn@x.com",
        ] {
            assert!(!s.is_contact(no).unwrap(), "{no:?}");
        }

        // The tables do not touch each other: a people upsert of a Contact's
        // address leaves it a Contact, and its `contacts` row stays `graph`.
        s.upsert_contact(&Contact {
            name: "Ann".into(),
            address: "ann@x.com".into(),
            source: "graph".into(),
            last_seen: String::new(),
            frequency: 0,
            relevance: Some(1),
        })
        .unwrap();
        assert!(s.is_contact("ann@x.com").unwrap());
        assert_eq!(s.search_contacts("ann@", 1).unwrap()[0].source, "graph");
        // Mining again does not write Contacts either.
        s.refresh_local_contacts().unwrap();
        assert!(!s.is_contact("carol@x.com").unwrap());
    }

    #[test]
    fn is_contact_folds_ascii_case_only() {
        let s = Store::open_in_memory().unwrap();
        s.replace_contacts(&[fc("Kate", "kate@x.com"), fc("Ä", "Ärger@x.com")])
            .unwrap();
        // U+212A KELVIN SIGN lowercases to ASCII `k` in Unicode: no match.
        assert!(!s.is_contact("\u{212A}ate@x.com").unwrap());
        assert!(s.is_contact("KATE@X.COM").unwrap());
        // Non-ASCII letters are compared as they are; the ASCII ones re-cased.
        assert!(s.is_contact("Ärger@x.com").unwrap());
        assert!(s.is_contact("ÄRGER@X.com").unwrap());
        assert!(!s.is_contact("ärger@x.com").unwrap());
    }

    #[test]
    fn replace_contacts_drops_absent_and_never_matches_blank() {
        let s = Store::open_in_memory().unwrap();
        s.replace_contacts(&[fc("A", "a@x.com"), fc("B", "b@x.com")])
            .unwrap();
        assert!(s.is_contact("a@x.com").unwrap());
        assert!(s.is_contact("b@x.com").unwrap());

        s.replace_contacts(&[fc("B", "b@x.com"), fc("Blank", "  ")])
            .unwrap();
        assert!(!s.is_contact("a@x.com").unwrap());
        assert!(s.is_contact("b@x.com").unwrap());
        assert!(!s.is_contact("").unwrap());
        assert_eq!(
            contact_ids(&s),
            ["C-b@x.com", "C-"],
            "a blank address is kept"
        );

        s.replace_contacts(&[]).unwrap();
        assert!(!s.is_contact("b@x.com").unwrap());
        assert!(contact_ids(&s).is_empty());

        // Two contacts may share an address; an X500 address (a
        // directory-saved contact) is stored for display, never matched.
        let x500 = "/o=ExchangeLabs/ou=Exchange Administrative Group/cn=Recipients/cn=ann";
        s.replace_contacts(&[
            contact_record("C1", "Bea", &[" b@X.com "]),
            contact_record("C2", "Other", &["b@x.com"]),
            contact_record("C3", "Ann", &[x500]),
        ])
        .unwrap();
        assert!(s.is_contact("B@X.COM").unwrap());
        assert!(!s.is_contact(x500).unwrap());
        let ann = s.folder_contact("C3").unwrap().unwrap();
        assert_eq!(ann.fields.email_addresses[0].address, x500);
    }

    #[test]
    fn is_contact_on_a_database_from_before_the_table() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-folder-contacts-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        {
            let s = Store::open(&path).unwrap();
            s.conn
                .execute_batch(
                    "DROP TABLE contact_emails; DROP TABLE contact_records;
                     DROP TABLE contact_aliases; DROP TABLE folder_contacts;",
                )
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert!(!s.is_contact("ann@x.com").unwrap());
        assert!(s.folder_contacts().unwrap().is_empty());
        s.replace_contacts(&[fc("Ann", "ann@x.com")]).unwrap();
        assert!(s.is_contact("ann@x.com").unwrap());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_contact_reads_legacy_folder_contacts_until_the_first_replace() {
        let s = Store::open_in_memory().unwrap();
        // What #1934 stored, on a database the records have not reached yet.
        s.conn
            .execute(
                "INSERT INTO folder_contacts (address, name) VALUES ('ann@x.com', 'Ann')",
                [],
            )
            .unwrap();
        assert!(s.is_contact("ANN@x.com").unwrap(), "no window of false");
        assert!(s.folder_contacts().unwrap().is_empty());
        // The first records fetch replaces it.
        s.replace_contacts(&[fc("Bob", "bob@x.com")]).unwrap();
        assert!(!s.is_contact("ann@x.com").unwrap());
        assert!(s.is_contact("bob@x.com").unwrap());
        let legacy: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM folder_contacts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(legacy, 0);
    }

    #[test]
    fn replace_contacts_stores_records_and_drops_absent() {
        let s = Store::open_in_memory().unwrap();
        let ann = FolderContact {
            id: "C1".into(),
            change_key: "ck1".into(),
            fields: full_fields(),
        };
        let bob = contact_record("C2", "Bob", &["bob@x.com"]);
        s.replace_contacts(&[ann.clone(), bob.clone(), contact_record("C1", "dup", &[])])
            .unwrap();
        assert_eq!(
            s.folder_contact("C1").unwrap(),
            Some(ann.clone()),
            "every field"
        );
        assert_eq!(s.folder_contact("C2").unwrap(), Some(bob));
        assert!(s.is_contact("ann@home.example").unwrap());

        // Bob is gone from the folder; Ann changed.
        let mut ann2 = ann.clone();
        ann2.change_key = "ck2".into();
        ann2.fields.job_title = "Manager".into();
        ann2.fields.email_addresses.pop();
        ann2.fields.home_phones.clear();
        s.replace_contacts(&[ann2.clone()]).unwrap();
        assert_eq!(contact_ids(&s), ["C1"]);
        assert_eq!(s.folder_contact("C1").unwrap(), Some(ann2));
        assert!(!s.is_contact("bob@x.com").unwrap());
        assert!(
            !s.is_contact("ann@home.example").unwrap(),
            "its email row went"
        );
        let emails: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM contact_emails", [], |r| r.get(0))
            .unwrap();
        assert_eq!(emails, 1);
    }

    #[test]
    fn folder_contacts_orders_by_file_as_then_display_name() {
        let s = Store::open_in_memory().unwrap();
        let with_file_as = |id: &str, display: &str, file_as: &str| {
            let mut c = contact_record(id, display, &[]);
            c.fields.file_as = file_as.into();
            c
        };
        s.replace_contacts(&[
            with_file_as("C1", "Zed Adams", "adams, zed"),
            with_file_as("C2", "carol", ""),
            with_file_as("C3", "Bob", ""),
            with_file_as("C4", "Aaron", "Young, Aaron"),
            with_file_as("C0", "bob", ""),
        ])
        .unwrap();
        assert_eq!(contact_ids(&s), ["C1", "C0", "C3", "C2", "C4"]);
    }

    #[test]
    fn folder_contact_by_id_and_by_local_alias() {
        let s = Store::open_in_memory().unwrap();
        let local = s.create_local_contact(&full_fields()).unwrap();
        assert!(local.starts_with("local:"), "{local}");
        assert_eq!(
            s.folder_contact(&local).unwrap().unwrap().fields,
            full_fields()
        );
        assert!(s.is_contact("ann@x.com").unwrap(), "counts at once");
        assert_eq!(s.resolve_contact_id(&local).unwrap(), local);

        let mut created = contact_record("G1", "", &[]);
        created.change_key = "ckG".into();
        s.reconcile_created_contact(&local, &created).unwrap();
        assert_eq!(s.resolve_contact_id(&local).unwrap(), "G1");
        let by_local = s.folder_contact(&local).unwrap().unwrap();
        assert_eq!(by_local.id, "G1");
        assert_eq!(s.folder_contact("G1").unwrap(), Some(by_local));
        assert!(s.folder_contact("nope").unwrap().is_none());

        // Edits and deletes by the `local:` id reach the re-keyed row.
        let mut f = full_fields();
        f.job_title = "Manager".into();
        assert_eq!(
            s.update_contact_fields(&local, &f).unwrap(),
            ContactFieldSet::JOB_TITLE
        );
        assert_eq!(
            s.folder_contact("G1").unwrap().unwrap().fields.job_title,
            "Manager"
        );
        assert!(s.delete_contact(&local).unwrap());
        assert!(s.folder_contact("G1").unwrap().is_none());
        assert!(!s.is_contact("ann@x.com").unwrap(), "its email rows went");
        assert!(!s.delete_contact(&local).unwrap());
    }

    #[test]
    fn update_contact_fields_returns_only_changed() {
        let s = Store::open_in_memory().unwrap();
        s.replace_contacts(&[FolderContact {
            id: "C1".into(),
            change_key: "ck1".into(),
            fields: full_fields(),
        }])
        .unwrap();
        assert_eq!(
            s.update_contact_fields("C1", &full_fields()).unwrap(),
            ContactFieldSet::default(),
            "nothing changed"
        );
        let mut f = full_fields();
        f.surname = "Cole-Smith".into();
        f.business_address.postal_code = "62702".into();
        f.email_addresses[1].address = "ann@new.example".into();
        f.birthday.clear();
        let mut want = ContactFieldSet::SURNAME;
        want.insert(ContactFieldSet::BUSINESS_ADDRESS);
        want.insert(ContactFieldSet::EMAIL_ADDRESSES);
        want.insert(ContactFieldSet::BIRTHDAY);
        assert_eq!(s.update_contact_fields("C1", &f).unwrap(), want);
        let stored = s.folder_contact("C1").unwrap().unwrap();
        assert_eq!(stored.fields, f);
        assert_eq!(stored.change_key, "ck1", "kept");
        assert!(s.is_contact("ann@new.example").unwrap());
        assert!(!s.is_contact("ann@home.example").unwrap());
        // No such contact: nothing written, nothing changed.
        assert!(s.update_contact_fields("C9", &f).unwrap().is_empty());
        assert!(s.folder_contact("C9").unwrap().is_none());
    }

    #[test]
    fn reconcile_created_contact_rekeys_and_keeps_local_edits() {
        let s = Store::open_in_memory().unwrap();
        let mut f = full_fields();
        f.display_name.clear();
        f.file_as.clear();
        let local = s.create_local_contact(&f).unwrap();
        // An edit while the POST was in flight.
        let mut edited = f.clone();
        edited.job_title = "Manager".into();
        s.update_contact_fields(&local, &edited).unwrap();

        let created = FolderContact {
            id: "G1".into(),
            change_key: "ckG".into(),
            fields: ContactFields {
                display_name: "Ann Cole".into(),
                file_as: "Cole, Ann".into(),
                job_title: "Engineer".into(),
                ..f.clone()
            },
        };
        s.reconcile_created_contact(&local, &created).unwrap();
        assert!(s.folder_contact("G1").unwrap().is_some());
        assert_eq!(contact_ids(&s), ["G1"], "the local row is gone");
        let row = s.folder_contact("G1").unwrap().unwrap();
        assert_eq!(row.change_key, "ckG");
        assert_eq!(row.fields.job_title, "Manager", "the local edit is kept");
        assert_eq!(
            row.fields.display_name, "Ann Cole",
            "Graph's, the local one empty"
        );
        assert_eq!(row.fields.file_as, "Cole, Ann");
        // The email rows followed the re-key.
        assert!(s.is_contact("ann@x.com").unwrap());
        let keyed: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM contact_emails WHERE contact_id = 'G1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(keyed, 2);

        // A stored name is not overwritten by Graph's.
        let local2 = s.create_local_contact(&full_fields()).unwrap();
        let mut created2 = created.clone();
        created2.id = "G2".into();
        created2.fields.display_name = "Graph's name".into();
        s.reconcile_created_contact(&local2, &created2).unwrap();
        assert_eq!(
            s.folder_contact("G2").unwrap().unwrap().fields.display_name,
            "Ann Cole"
        );
    }

    #[test]
    fn reconcile_created_contact_of_a_deleted_row_still_aliases() {
        let s = Store::open_in_memory().unwrap();
        let local = s.create_local_contact(&full_fields()).unwrap();
        assert!(s.delete_contact(&local).unwrap());
        s.reconcile_created_contact(&local, &contact_record("G1", "Ann", &[]))
            .unwrap();
        assert!(s.folder_contacts().unwrap().is_empty(), "no row comes back");
        assert_eq!(s.resolve_contact_id(&local).unwrap(), "G1");
    }

    #[test]
    fn replace_contacts_keeps_rows_with_pending_ops() {
        let s = Store::open_in_memory().unwrap();
        s.replace_contacts(&[
            contact_record("E1", "Edited", &["e@x.com"]),
            contact_record("D1", "Deleted", &["d@x.com"]),
            contact_record("R1", "Removed elsewhere", &["r@x.com"]),
            contact_record("P1", "Plain", &["p@x.com"]),
        ])
        .unwrap();
        // A create not sent yet, an edit of a contact created by a local id
        // since re-keyed, and a delete: each store write queues its op.
        let local = s.create_local_contact(&full_fields()).unwrap();
        let edited_local = s.create_local_contact(&ContactFields::default()).unwrap();
        s.reconcile_created_contact(&edited_local, &contact_record("G2", "", &[]))
            .unwrap();
        // Its create went through.
        let create_seq = s
            .pending_ops()
            .unwrap()
            .into_iter()
            .find(|r| {
                r.op == OutboxOp::CreateContact {
                    id: edited_local.clone(),
                }
            })
            .unwrap()
            .seq;
        s.drop_op(create_seq);
        let mut e = contact_record("E1", "Edited here", &["e@x.com"]).fields;
        s.update_contact_fields("E1", &e).unwrap();
        e.job_title = "Lead".into();
        s.update_contact_fields(&edited_local, &e).unwrap();
        s.delete_contact("D1").unwrap();

        // Graph's answer: none of the local writes yet, and R1 gone.
        s.replace_contacts(&[
            contact_record("E1", "Edited", &["e@x.com"]),
            contact_record("D1", "Deleted", &["d@x.com"]),
            contact_record("G2", "", &[]),
            contact_record("P1", "Plain (renamed)", &["p@x.com"]),
        ])
        .unwrap();
        let mut ids = contact_ids(&s);
        ids.sort();
        let mut want = vec!["E1".to_string(), "G2".into(), "P1".into(), local.clone()];
        want.sort();
        assert_eq!(
            ids, want,
            "R1 removed, D1 not written back, the local row kept"
        );
        assert_eq!(
            s.folder_contact("E1").unwrap().unwrap().fields.display_name,
            "Edited here",
            "the queued edit is not undone"
        );
        assert_eq!(
            s.folder_contact("G2").unwrap().unwrap().fields.job_title,
            "Lead"
        );
        assert_eq!(
            s.folder_contact("P1").unwrap().unwrap().fields.display_name,
            "Plain (renamed)"
        );
        assert!(!s.is_contact("d@x.com").unwrap());

        // With the queue empty, the server's version wins again.
        for row in s.pending_ops().unwrap() {
            s.drop_op(row.seq);
        }
        s.replace_contacts(&[contact_record("E1", "Edited", &["e@x.com"])])
            .unwrap();
        let mut ids = contact_ids(&s);
        ids.sort();
        assert_eq!(
            ids,
            vec!["E1".to_string(), local],
            "a local row is never removed"
        );
        assert_eq!(
            s.folder_contact("E1").unwrap().unwrap().fields.display_name,
            "Edited"
        );
    }

    #[test]
    fn contact_writes_queue_their_op_in_the_same_transaction() {
        let s = Store::open_in_memory().unwrap();
        let ops = |s: &Store| -> Vec<OutboxOp> {
            s.pending_ops().unwrap().into_iter().map(|r| r.op).collect()
        };
        s.replace_contacts(&[fc("Ann", "ann@x.com")]).unwrap();
        assert!(ops(&s).is_empty(), "a fetch queues nothing");

        let local = s.create_local_contact(&full_fields()).unwrap();
        assert_eq!(ops(&s), [OutboxOp::CreateContact { id: local.clone() }]);

        let mut f = s.folder_contact("C-ann@x.com").unwrap().unwrap().fields;
        assert!(
            s.update_contact_fields("C-ann@x.com", &f)
                .unwrap()
                .is_empty()
        );
        assert_eq!(ops(&s).len(), 1, "a no-change edit queues nothing");
        f.surname = "Cole".into();
        let changed = s.update_contact_fields("C-ann@x.com", &f).unwrap();
        assert_eq!(changed, ContactFieldSet::SURNAME);
        assert_eq!(
            ops(&s)[1],
            OutboxOp::UpdateContact {
                id: "C-ann@x.com".into(),
                fields: ContactFieldSet::SURNAME
            }
        );
        // An edit by the `local:` id after the re-key queues the Graph id.
        s.reconcile_created_contact(&local, &contact_record("G1", "", &[]))
            .unwrap();
        let mut g = full_fields();
        g.department = "Ops".into();
        s.update_contact_fields(&local, &g).unwrap();
        assert_eq!(
            ops(&s)[2],
            OutboxOp::UpdateContact {
                id: "G1".into(),
                fields: ContactFieldSet::DEPARTMENT
            }
        );
        assert!(s.update_contact_fields("C9", &g).unwrap().is_empty());
        assert_eq!(ops(&s).len(), 3, "no row, nothing queued");

        assert!(s.delete_contact("C-ann@x.com").unwrap());
        assert_eq!(
            ops(&s)[3],
            OutboxOp::DeleteContact {
                id: "C-ann@x.com".into()
            }
        );
        assert!(!s.delete_contact("C-ann@x.com").unwrap());
        assert_eq!(ops(&s).len(), 4, "nothing removed, nothing queued");

        // The engine's reverts queue nothing.
        let before = ops(&s).len();
        assert!(
            s.revert_folder_contact("C-bob", Some(&fc("Bob", "bob@x.com")))
                .unwrap()
        );
        s.discard_contact_row("G1").unwrap();
        assert!(s.revert_folder_contact("C-bob", None).unwrap());
        assert_eq!(ops(&s).len(), before);
    }

    #[test]
    fn blank_phone_and_category_entries_are_not_stored_or_compared() {
        let s = Store::open_in_memory().unwrap();
        let mut f = full_fields();
        f.business_phones = vec!["".into(), "+1 555 0100".into(), "  ".into()];
        f.categories = vec![" ".into()];
        let id = s.create_local_contact(&f).unwrap();
        let stored = s.folder_contact(&id).unwrap().unwrap().fields;
        assert_eq!(stored.business_phones, ["+1 555 0100"]);
        assert!(stored.categories.is_empty());
        // Saving the same form again changes nothing and queues nothing.
        let queued = s.pending_ops().unwrap().len();
        assert!(s.update_contact_fields(&id, &f).unwrap().is_empty());
        f.home_phones.push(" ".into());
        assert!(s.update_contact_fields(&id, &f).unwrap().is_empty());
        assert_eq!(s.pending_ops().unwrap().len(), queued);
    }

    #[test]
    fn revert_folder_contact_waits_while_an_op_names_it() {
        let s = Store::open_in_memory().unwrap();
        s.replace_contacts(&[contact_record("C1", "Ann", &["ann@x.com"])])
            .unwrap();
        s.owe_contact_reread("C1").unwrap();
        s.owe_contact_reread("C1").unwrap();
        assert_eq!(
            s.owed_contact_rereads().unwrap(),
            [("C1".to_string(), false)]
        );
        let mut f = s.folder_contact("C1").unwrap().unwrap().fields;
        f.job_title = "Lead".into();
        s.update_contact_fields("C1", &f).unwrap();
        assert_eq!(
            s.owed_contact_rereads().unwrap(),
            [("C1".to_string(), true)]
        );
        let server = contact_record("C1", "Ann (server)", &["ann@x.com"]);
        assert!(!s.revert_folder_contact("C1", Some(&server)).unwrap());
        assert_eq!(
            s.folder_contact("C1").unwrap().unwrap().fields.job_title,
            "Lead",
            "the queued edit stands"
        );
        assert_eq!(s.owed_contact_rereads().unwrap().len(), 1, "still owed");
        for row in s.pending_ops().unwrap() {
            s.drop_op(row.seq);
        }
        assert!(s.revert_folder_contact("C1", Some(&server)).unwrap());
        assert_eq!(s.folder_contact("C1").unwrap(), Some(server));
        assert!(s.owed_contact_rereads().unwrap().is_empty());
        // A 404 deletes the row; a given-up re-read is forgotten.
        s.owe_contact_reread("C1").unwrap();
        assert!(s.revert_folder_contact("C1", None).unwrap());
        assert!(s.folder_contact("C1").unwrap().is_none());
        s.owe_contact_reread("C2").unwrap();
        s.forget_contact_reread("C2").unwrap();
        assert!(s.owed_contact_rereads().unwrap().is_empty());
    }

    #[test]
    fn owed_contact_reread_survives_reopening_the_store() {
        let dir = std::env::temp_dir().join(format!(
            "mailcore-contact-owed-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        Store::open(&path)
            .unwrap()
            .owe_contact_reread("C1")
            .unwrap();
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.owed_contact_rereads().unwrap(),
            [("C1".to_string(), false)]
        );
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contact_ops_round_trip_json() {
        let mut fields = ContactFieldSet::SURNAME;
        fields.insert(ContactFieldSet::HOME_ADDRESS);
        let ops = [
            OutboxOp::CreateContact {
                id: "local:ab".into(),
            },
            OutboxOp::UpdateContact {
                id: "G1".into(),
                fields,
            },
            OutboxOp::DeleteContact { id: "G1".into() },
        ];
        for op in ops {
            let back = OutboxOp::from_json(&json::parse(&op.to_json().to_string()).unwrap());
            assert_eq!(back, Some(op));
        }
        assert_eq!(
            OutboxOp::UpdateContact {
                id: "G1".into(),
                fields
            }
            .to_json()
            .to_string(),
            r#"{"kind":"updateContact","id":"G1","fields":["surname","homeAddress"]}"#
        );
        // An unknown name is dropped; a row without `fields` is corrupt.
        let v = json::parse(r#"{"kind":"updateContact","id":"G1","fields":["surname","later"]}"#)
            .unwrap();
        assert_eq!(
            OutboxOp::from_json(&v),
            Some(OutboxOp::UpdateContact {
                id: "G1".into(),
                fields: ContactFieldSet::SURNAME
            })
        );
        let v = json::parse(r#"{"kind":"updateContact","id":"G1"}"#).unwrap();
        assert_eq!(OutboxOp::from_json(&v), None);
        // Contact ops name no message.
        let s = Store::open_in_memory().unwrap();
        s.enqueue_op(&OutboxOp::DeleteContact { id: "G1".into() })
            .unwrap();
        assert!(s.pending_message_ids().unwrap().is_empty());
    }
}

#[cfg(test)]
mod calendar_tests {
    use super::*;

    /// Two connections to one WAL file, as the engine and a host hold
    /// (#2023 r3): while one holds the write lock and then commits, the
    /// other's read-then-write transactions wait it out instead of failing
    /// with `database is locked` (a DEFERRED transaction's stale snapshot,
    /// `SQLITE_BUSY_SNAPSHOT`, which no busy timeout waits out).
    #[test]
    fn writes_wait_out_another_connections_write() {
        let dir = std::env::temp_dir().join(format!(
            "mailcore-write-tx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        let a = Store::open(&path).unwrap();
        a.upsert_event(&ev("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"))
            .unwrap();
        // Each of A's writes runs while another connection holds the write
        // lock, and that connection commits a change after A began.
        let while_b_writes = |write: &dyn Fn(&Store)| {
            let (locked, is_locked) = std::sync::mpsc::channel();
            let p = path.clone();
            let b = std::thread::spawn(move || {
                let b = Connection::open(&p).unwrap();
                b.execute_batch(
                    "BEGIN IMMEDIATE;
                     UPDATE events SET location = 'by B' WHERE id = 'E1';",
                )
                .unwrap();
                locked.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(400));
                b.execute_batch("COMMIT").unwrap();
            });
            is_locked.recv().unwrap();
            write(&a);
            b.join().unwrap();
        };
        while_b_writes(&|a| {
            let row = a.event("E1").unwrap().unwrap();
            a.update_event_fields(
                "E1",
                &LocalEventFields {
                    subject: "by A".into(),
                    start_utc: row.start_utc,
                    end_utc: row.end_utc,
                    location: "by A".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        });
        let row = a.event("E1").unwrap().unwrap();
        assert_eq!(
            (row.subject.as_str(), row.location.as_str()),
            ("by A", "by A")
        );
        while_b_writes(&|a| {
            a.create_local_event(&sample_fields(), "Me", "me@x")
                .unwrap();
        });
        drop(a);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        s.set_event_response("1", "accepted", "2026-08-13T19:19:00Z");
        let rows = s
            .events_in_window("2026-07-17T00:00:00Z", "2026-07-18T00:00:00Z")
            .unwrap();
        assert_eq!(rows[0].response_status, "accepted");
        // ...with the time it was given (#2068).
        assert_eq!(rows[0].response_time, "2026-08-13T19:19:00Z");
        s.set_event_response("1", "declined", "2026-08-14T07:00:00Z");
        let row = s.event("1").unwrap().unwrap();
        assert_eq!(row.response_status, "declined");
        assert_eq!(row.response_time, "2026-08-14T07:00:00Z");
    }

    #[test]
    fn event_response_time_and_created_utc_round_trip() {
        // #2068: both columns are written on insert, read back by `event`
        // and `events_in_window`, and replaced by a later upsert.
        let s = Store::open_in_memory().unwrap();
        let mut e = ev("1", "2026-07-17T10:00:00Z", "2026-07-17T11:00:00Z");
        e.response_time = "2026-07-10T08:30:00Z".into();
        e.created_utc = "2026-07-01T12:00:05Z".into();
        s.upsert_event(&e).unwrap();
        let row = s.event("1").unwrap().unwrap();
        assert_eq!(row.response_time, "2026-07-10T08:30:00Z");
        assert_eq!(row.created_utc, "2026-07-01T12:00:05Z");
        let rows = s
            .events_in_window("2026-07-17T00:00:00Z", "2026-07-18T00:00:00Z")
            .unwrap();
        assert_eq!(rows[0].response_time, "2026-07-10T08:30:00Z");
        assert_eq!(rows[0].created_utc, "2026-07-01T12:00:05Z");
        // The `ON CONFLICT` update path.
        e.response_time = "2026-07-11T09:00:00Z".into();
        e.created_utc = "2026-07-02T00:00:00Z".into();
        s.upsert_event(&e).unwrap();
        let row = s.event("1").unwrap().unwrap();
        assert_eq!(row.response_time, "2026-07-11T09:00:00Z");
        assert_eq!(row.created_utc, "2026-07-02T00:00:00Z");
        // Graph's "never" arrives as `""` and clears a stored time.
        e.response_time = String::new();
        s.upsert_event(&e).unwrap();
        assert_eq!(s.event("1").unwrap().unwrap().response_time, "");
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
        assert_eq!(sent.series_recurrence, None);

        // A non-recurring event round-trips to None.
        let id2 = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        let sent2 = s.event_for_send(&id2).unwrap().unwrap();
        assert_eq!(sent2.recurrence, None);
        assert_eq!(sent2.series_recurrence, None);
    }

    /// A lossless recurrence none of the create-side `Recurrence` can hold
    /// (#2034): relativeYearly, a Monday week, a numbered range, a zone.
    fn lossless_recurrence() -> EventRecurrence {
        use crate::graph::model::{RecurrencePattern, RecurrenceRange};
        EventRecurrence {
            pattern: RecurrencePattern {
                r#type: "relativeYearly".into(),
                interval: 1,
                month: 11,
                day_of_month: 0,
                days_of_week: vec!["thursday".into()],
                first_day_of_week: "monday".into(),
                index: "fourth".into(),
            },
            range: RecurrenceRange {
                r#type: "numbered".into(),
                start_date: "2026-11-26".into(),
                end_date: String::new(),
                number_of_occurrences: 5,
                recurrence_time_zone: "Pacific Standard Time".into(),
            },
        }
    }

    /// The local row `id` as `events_in_window` lists it.
    fn local_row(s: &Store, id: &str) -> EventRow {
        s.events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap()
    }

    #[test]
    fn create_local_event_stores_a_lossless_recurrence() {
        let s = Store::open_in_memory().unwrap();
        let rec = lossless_recurrence();
        let id = s
            .create_local_event(
                &LocalEventFields {
                    series_recurrence: Some(rec.clone()),
                    ..sample_fields()
                },
                "Me",
                "me@x",
            )
            .unwrap();
        let sent = s.event_for_send(&id).unwrap().unwrap();
        assert_eq!(sent.series_recurrence, Some(rec.clone()));
        assert_eq!(sent.recurrence, None);
        let row = local_row(&s, &id);
        assert_eq!(row.recurrence, Some(rec));
        assert_eq!(row.event_type, "seriesMaster");
    }

    #[test]
    fn create_local_event_lossless_recurrence_wins() {
        use crate::graph::model::{Recurrence, RecurrenceKind};
        let s = Store::open_in_memory().unwrap();
        let rec = lossless_recurrence();
        let id = s
            .create_local_event(
                &LocalEventFields {
                    recurrence: Some(Recurrence {
                        kind: RecurrenceKind::Daily,
                        interval: 3,
                        days_of_week: Vec::new(),
                        day_of_month: 0,
                        start_date: "2026-07-20".into(),
                        until: None,
                    }),
                    series_recurrence: Some(rec.clone()),
                    ..sample_fields()
                },
                "Me",
                "me@x",
            )
            .unwrap();
        let sent = s.event_for_send(&id).unwrap().unwrap();
        assert_eq!(sent.series_recurrence, Some(rec.clone()));
        assert_eq!(sent.recurrence, None);
        assert_eq!(local_row(&s, &id).recurrence, Some(rec));
    }

    #[test]
    fn update_event_fields_never_changes_a_lossless_recurrence() {
        let s = Store::open_in_memory().unwrap();
        let rec = lossless_recurrence();
        let id = s
            .create_local_event(
                &LocalEventFields {
                    series_recurrence: Some(rec.clone()),
                    ..sample_fields()
                },
                "Me",
                "me@x",
            )
            .unwrap();
        let mut other = rec.clone();
        other.pattern.r#type = "daily".into();
        other.range.r#type = "noEnd".into();
        let changed = s
            .update_event_fields(
                &id,
                &LocalEventFields {
                    subject: "Renamed".into(),
                    series_recurrence: Some(other),
                    ..sample_fields()
                },
            )
            .unwrap();
        assert!(!changed.contains(EventFieldSet::RECURRENCE));
        assert!(changed.contains(EventFieldSet::SUBJECT));
        let sent = s.event_for_send(&id).unwrap().unwrap();
        assert_eq!(sent.series_recurrence, Some(rec.clone()));
        assert_eq!(local_row(&s, &id).recurrence, Some(rec));
    }

    #[test]
    fn event_for_send_reads_neither_shape_as_no_recurrence() {
        let s = Store::open_in_memory().unwrap();
        let id = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        for text in [
            "{\"pattern\":",
            "{\"graphRecurrence\":1}",
            "{\"graphRecurrence\":{\"range\":{}}}",
            "{\"pattern\":{\"type\":\"relativeYearly\"},\"range\":{}}",
            "[1,2]",
        ] {
            s.conn
                .execute(
                    "UPDATE events SET recurrence = ?2 WHERE id = ?1",
                    params![id, text],
                )
                .unwrap();
            let sent = s.event_for_send(&id).unwrap().unwrap();
            assert_eq!(sent.recurrence, None, "{text}");
            assert_eq!(sent.series_recurrence, None, "{text}");
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
        let local = s.event(&id).unwrap().unwrap();
        assert_eq!(local.created_utc, "", "a local event has no Graph time");
        let created = Event {
            id: "EV1".into(),
            created_utc: "2026-08-01T08:00:05Z".into(),
            ..Event::default()
        };
        s.reconcile_created_event(&id, &created).unwrap();
        assert!(s.event_for_send(&id).unwrap().is_none()); // old id gone
        let send = s.event_for_send("EV1").unwrap().unwrap(); // under graph id
        assert_eq!(send.attendees.len(), 1); // attendees moved too
        // Graph's creation time is kept at once (#2068): the "Sent" date.
        let row = s.event("EV1").unwrap().unwrap();
        assert_eq!(row.created_utc, "2026-08-01T08:00:05Z");
        assert_eq!(row.response_time, "");
    }

    #[test]
    fn event_by_id_returns_the_row() {
        let s = Store::open_in_memory().unwrap();
        let mut e = client_event("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z");
        e.is_organizer = true;
        s.upsert_event(&e).unwrap();
        let mut other = ev("E2", "2026-07-20T11:00:00Z", "2026-07-20T12:00:00Z");
        other.is_organizer = false;
        s.upsert_event(&other).unwrap();

        let row = s.event("E1").unwrap().unwrap();
        assert_eq!(row.id, "E1");
        assert_client_fields(&row, &e);
        // The row is the one the window read gives (a series master with no
        // occurrence stored is listed, #2064).
        let listed = s
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap();
        assert_eq!(Some(&row), listed.iter().find(|r| r.id == "E1"));
        assert!(!s.event("E2").unwrap().unwrap().is_organizer);
        assert_eq!(s.event("nope").unwrap(), None);
        // A local event is the user's own meeting.
        let local = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        assert!(s.event(&local).unwrap().unwrap().is_organizer);
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
            proposed_start_utc: "2026-07-21T14:00:00Z".into(),
            proposed_end_utc: "2026-07-21T15:00:00Z".into(),
        };
        s.replace_events_in_window(
            from,
            to,
            std::slice::from_ref(&e),
            &[("E1".into(), vec![attendee.clone()])],
        )
        .unwrap();
        // Read by id: `client_event` is a series master, which the grid
        // queries list only while no row of its series is stored (#2064).
        assert_client_fields(&s.event("E1").unwrap().unwrap(), &e);
        let atts = s.event_attendees("E1").unwrap();
        assert_eq!(atts[0].r#type, "resource");
        assert_eq!(atts[0].response, "accepted");
        assert_eq!(atts[0].response_time, "2026-07-18T08:01:02Z");
        assert_eq!(atts[0].proposed_start_utc, "2026-07-21T14:00:00Z");
        assert_eq!(atts[0].proposed_end_utc, "2026-07-21T15:00:00Z");

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
        // Read by id: `client_event` is a series master, which the grid
        // queries list only while no row of its series is stored (#2064).
        assert_client_fields(&s.event("E1").unwrap().unwrap(), &moved);

        let again = NewEvent {
            show_as: "free".into(),
            categories: vec!["Blue category".into()],
            recurrence: Some(graph_recurrence("weekly", 9)),
            response_requested: true,
            calendar_id: "CAL2".into(),
            ..moved
        };
        s.upsert_event(&again).unwrap();
        assert_client_fields(&s.event("E1").unwrap().unwrap(), &again);
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
        let row = s.event("E1").unwrap().unwrap();
        assert_eq!(row.recurrence, None);
        assert_eq!(row.show_as, "oof", "the rest of the row still reads");
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
                ..NewAttendee::default()
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

    /// An attendee's proposed new time (#2023) survives a local attendee
    /// edit, as their response does; a new attendee has none.
    #[test]
    fn update_event_fields_keeps_attendee_proposals() {
        let s = Store::open_in_memory().unwrap();
        let untouched = synced_event(&s);
        // `synced_event`'s Bob, now proposing a new time.
        let bob = NewAttendee {
            name: "Bob".into(),
            addr: "Bob@X.com".into(),
            r#type: "optional".into(),
            response: "tentativelyAccepted".into(),
            response_time: "2026-07-19T08:00:00Z".into(),
            proposed_start_utc: "2026-07-21T14:00:00Z".into(),
            proposed_end_utc: "2026-07-21T15:00:00Z".into(),
        };
        s.put_event_attendees("G1", &[bob]).unwrap();
        let mut two = untouched.clone();
        two.attendees.push(AttendeeInput {
            name: "Ann".into(),
            addr: "ann@x.com".into(),
            r#type: String::new(),
        });
        assert_eq!(
            s.update_event_fields("G1", &two).unwrap(),
            EventFieldSet::ATTENDEES
        );
        let atts = s.event_attendees("G1").unwrap();
        assert_eq!(atts.len(), 2);
        assert_eq!(
            (
                atts[0].proposed_start_utc.as_str(),
                atts[0].proposed_end_utc.as_str()
            ),
            ("2026-07-21T14:00:00Z", "2026-07-21T15:00:00Z"),
            "Bob keeps his proposal"
        );
        assert_eq!(atts[1].addr, "ann@x.com");
        assert_eq!(
            (
                atts[1].proposed_start_utc.as_str(),
                atts[1].proposed_end_utc.as_str()
            ),
            ("", "")
        );
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

    /// `sample_fields` named in two zones (#2039): 09:00 in Berlin's winter
    /// (UTC+1) is 08:00 UTC; the end, 08:30 in London (UTC+0).
    fn zoned_fields() -> LocalEventFields {
        LocalEventFields {
            start_utc: "2026-01-12T08:00:00Z".into(),
            end_utc: "2026-01-12T08:30:00Z".into(),
            start_time_zone: "W. Europe Standard Time".into(),
            start_local: "2026-01-12T09:00:00".into(),
            end_time_zone: "Europe/London".into(),
            end_local: "2026-01-12T08:30:00".into(),
            ..sample_fields()
        }
    }

    /// A stored event's four zone fields, start then end.
    fn zones_of(s: &Store, id: &str) -> [String; 4] {
        let d = s.event_for_send(id).unwrap().unwrap();
        [
            d.start_time_zone,
            d.start_local,
            d.end_time_zone,
            d.end_local,
        ]
    }

    fn zoned_pairs() -> [String; 4] {
        [
            "W. Europe Standard Time".into(),
            "2026-01-12T09:00:00".into(),
            "Europe/London".into(),
            "2026-01-12T08:30:00".into(),
        ]
    }

    #[test]
    fn create_local_event_stores_named_zones() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_event(&zoned_fields(), "Me", "me@x").unwrap();
        assert_eq!(zones_of(&s, &id), zoned_pairs());
        // The stored instant is still UTC.
        let d = s.event_for_send(&id).unwrap().unwrap();
        assert_eq!(d.start_utc, "2026-01-12T08:00:00Z");
        // No zone: a stray wall clock is not kept.
        let id = s
            .create_local_event(
                &LocalEventFields {
                    start_local: "2026-07-20T13:00:00".into(),
                    ..sample_fields()
                },
                "Me",
                "me@x",
            )
            .unwrap();
        assert_eq!(zones_of(&s, &id), <[String; 4]>::default());
    }

    #[test]
    fn create_local_event_refuses_a_malformed_wall_clock() {
        let s = Store::open_in_memory().unwrap();
        let bad_start = |local: &str| LocalEventFields {
            start_local: local.into(),
            ..zoned_fields()
        };
        for f in [
            bad_start("2026-01-12 09:00:00"),
            bad_start("2026-02-30T09:00:00"),
            bad_start("2026-01-12T09:00:00Z"),
            // A day ahead, and 13 h behind: outside UTC−12..UTC+14.
            bad_start("2026-01-13T09:00:00"),
            bad_start("2026-01-11T19:00:00"),
            LocalEventFields {
                end_local: "".into(),
                ..zoned_fields()
            },
            // A named end needs a UTC time it can be checked against.
            LocalEventFields {
                start_utc: "2026-01-12T08:00:00".into(),
                ..zoned_fields()
            },
        ] {
            let err = s.create_local_event(&f, "Me", "me@x").unwrap_err();
            assert!(
                err.to_string().contains("named time zone") || err.to_string().contains("UTC-12"),
                "{err}"
            );
        }
        let n: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "nothing written");
    }

    #[test]
    fn update_event_fields_keeps_zones_when_times_unchanged() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_event(&zoned_fields(), "Me", "me@x").unwrap();
        // A host that sends no zone, editing the title only.
        let changed = s
            .update_event_fields(
                &id,
                &LocalEventFields {
                    subject: "Sync v2".into(),
                    start_time_zone: String::new(),
                    start_local: String::new(),
                    end_time_zone: String::new(),
                    end_local: String::new(),
                    ..zoned_fields()
                },
            )
            .unwrap();
        assert_eq!(changed, EventFieldSet::SUBJECT);
        assert_eq!(
            zones_of(&s, &id),
            zoned_pairs(),
            "the pending create keeps them"
        );
    }

    #[test]
    fn update_event_fields_zone_alone_does_not_mark_times() {
        let s = Store::open_in_memory().unwrap();
        let untouched = synced_event(&s);
        // A form always has a zone; the synced row stores none. 11:00 UTC is
        // 13:00 in Berlin's summer (UTC+2).
        let form = LocalEventFields {
            subject: "Sync v2".into(),
            start_time_zone: "W. Europe Standard Time".into(),
            start_local: "2026-07-20T13:00:00".into(),
            end_time_zone: "W. Europe Standard Time".into(),
            end_local: "2026-07-20T14:00:00".into(),
            ..untouched
        };
        assert_eq!(
            s.update_event_fields("G1", &form).unwrap(),
            EventFieldSet::SUBJECT
        );
        // The named pair is written all the same, for a later `TIMES` send.
        assert_eq!(
            zones_of(&s, "G1"),
            [
                "W. Europe Standard Time".to_string(),
                "2026-07-20T13:00:00".into(),
                "W. Europe Standard Time".into(),
                "2026-07-20T14:00:00".into(),
            ]
        );
    }

    #[test]
    fn update_event_fields_clears_zones_when_times_move_without_one() {
        let no_zone = |f: LocalEventFields| LocalEventFields {
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            ..f
        };
        // The instant moves, or the event becomes all-day: each clears.
        for f in [
            LocalEventFields {
                start_utc: "2026-01-12T07:00:00Z".into(),
                ..zoned_fields()
            },
            LocalEventFields {
                is_all_day: true,
                ..zoned_fields()
            },
        ] {
            let s = Store::open_in_memory().unwrap();
            let id = s.create_local_event(&zoned_fields(), "Me", "me@x").unwrap();
            assert_eq!(
                s.update_event_fields(&id, &no_zone(f)).unwrap(),
                EventFieldSet::TIMES
            );
            assert_eq!(zones_of(&s, &id), <[String; 4]>::default());
        }
        // A move that names the start only: the start's new pair, and the
        // unnamed end cleared.
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_event(&zoned_fields(), "Me", "me@x").unwrap();
        let moved = LocalEventFields {
            start_utc: "2026-01-12T07:00:00Z".into(),
            start_local: "2026-01-12T08:00:00".into(),
            end_time_zone: String::new(),
            ..zoned_fields()
        };
        assert_eq!(
            s.update_event_fields(&id, &moved).unwrap(),
            EventFieldSet::TIMES
        );
        assert_eq!(
            zones_of(&s, &id),
            [
                "W. Europe Standard Time".to_string(),
                "2026-01-12T08:00:00".into(),
                String::new(),
                String::new(),
            ]
        );
    }

    #[test]
    fn update_event_fields_refuses_a_malformed_wall_clock() {
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_event(&zoned_fields(), "Me", "me@x").unwrap();
        let err = s
            .update_event_fields(
                &id,
                &LocalEventFields {
                    subject: "Sync v2".into(),
                    start_utc: "2026-01-12T07:00:00Z".into(),
                    end_local: "2026-01-13T08:30:00".into(),
                    ..zoned_fields()
                },
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("event end in Europe/London"),
            "{err}"
        );
        let d = s.event_for_send(&id).unwrap().unwrap();
        assert_eq!(
            (d.subject.as_str(), d.start_utc.as_str()),
            ("Sync", "2026-01-12T08:00:00Z"),
            "nothing written"
        );
        assert_eq!(zones_of(&s, &id), zoned_pairs());
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
            assert_eq!(atts[0].proposed_start_utc, "", "no proposal (#2023)");
            assert_eq!(atts[0].proposed_end_utc, "");
            // `reminder_unset` (#1994) reads 0: the old row's reminder
            // came from Graph.
            assert!(!s.event_for_send("E1").unwrap().unwrap().reminder_unset);
            // No named zone (#2039): the old row is sent in UTC.
            assert_eq!(zones_of(&s, "E1"), <[String; 4]>::default());
        }
        // The migrated table takes every new column, `reminder_unset` too.
        let s = Store::open(&path).unwrap();
        let local = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        assert!(s.event_for_send(&local).unwrap().unwrap().reminder_unset);
        s.delete_event(&local).unwrap();
        // ...and each end's zone (#2039).
        let local = s.create_local_event(&zoned_fields(), "Me", "me@x").unwrap();
        assert_eq!(zones_of(&s, &local), zoned_pairs());
        s.delete_event(&local).unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        let e = client_event("E1", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z");
        s.upsert_event(&e).unwrap();
        assert_client_fields(&s.event("E1").unwrap().unwrap(), &e);
        // ...and `event_attendees` takes an attendee's proposal (#2023).
        let proposing = NewAttendee {
            name: "Bob".into(),
            addr: "bob@x".into(),
            r#type: "optional".into(),
            response: "tentativelyAccepted".into(),
            response_time: String::new(),
            proposed_start_utc: "2026-07-21T14:00:00Z".into(),
            proposed_end_utc: "2026-07-21T15:00:00Z".into(),
        };
        s.put_event_attendees("E1", std::slice::from_ref(&proposing))
            .unwrap();
        let atts = s.event_attendees("E1").unwrap();
        assert_eq!(atts[0].proposed_start_utc, proposing.proposed_start_utc);
        assert_eq!(atts[0].proposed_end_utc, proposing.proposed_end_utc);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Every calendar and calendar queries (#1984) ---

    fn cal(id: &str, default: bool, owner: &str) -> Calendar {
        Calendar {
            id: id.into(),
            name: format!("n{id}"),
            color: "auto".into(),
            hex_color: String::new(),
            is_default: default,
            can_edit: true,
            owner_name: String::new(),
            owner_addr: owner.into(),
        }
    }

    fn in_cal(id: &str, cal: &str, start: &str, end: &str) -> NewEvent {
        NewEvent {
            calendar_id: cal.into(),
            ..ev(id, start, end)
        }
    }

    fn fetched(cal: &str, events: Vec<NewEvent>) -> FetchedCalendar {
        FetchedCalendar {
            calendar_id: cal.into(),
            attendees: events.iter().map(|e| (e.id.clone(), Vec::new())).collect(),
            events,
        }
    }

    /// An initial round of `cal`'s window, ending with deltaLink `L-{cal}`.
    fn window(cal: &str, events: Vec<NewEvent>) -> SyncedCalendar {
        SyncedCalendar {
            fetched: fetched(cal, events),
            round: CalendarRound::Window,
            delta_link: format!("L-{cal}"),
        }
    }

    fn ids(rows: &[EventRow]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    fn all_events(s: &Store) -> Vec<EventRow> {
        s.events_in_window("0001-01-01T00:00:00Z", "9999-12-31T00:00:00Z")
            .unwrap()
    }

    const FROM: &str = "2026-07-13T00:00:00Z";
    const TO: &str = "2026-08-19T00:00:00Z";
    /// A fetch at 1000 with nothing pruned.
    const STAMP: RangeStamp = RangeStamp {
        at: 1000,
        fresh_since: 0,
    };

    #[test]
    fn replace_calendars_upserts_deletes_and_keeps_order() {
        let s = Store::open_in_memory().unwrap();
        assert!(
            s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
                .unwrap()
        );
        // Same list again: nothing changed.
        assert!(
            !s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
                .unwrap()
        );
        // Reordered: changed.
        assert!(
            s.replace_calendars(&[cal("B", false, "me@x"), cal("A", true, "me@x")])
                .unwrap()
        );
        let order: Vec<String> = s
            .calendars()
            .unwrap()
            .into_iter()
            .map(|c| c.calendar.id)
            .collect();
        assert_eq!(order, ["B", "A"]);
        // A field changed, C added, B gone; a repeated id keeps its first place.
        let mut a = cal("A", true, "me@x");
        a.hex_color = "#ff0000".into();
        assert!(
            s.replace_calendars(&[a.clone(), cal("C", false, "me@x"), cal("A", false, "")])
                .unwrap()
        );
        let rows = s.calendars().unwrap();
        assert_eq!(
            rows.iter().map(|c| c.calendar.clone()).collect::<Vec<_>>(),
            vec![a, cal("C", false, "me@x")]
        );
        assert!(rows.iter().all(|c| c.syncs));
    }

    #[test]
    fn own_calendars_follow_the_default_calendars_owner() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[
            cal("A", true, " Me@X.com "),
            cal("B", false, "me@x.COM"),
            cal("Shared", false, "boss@x.com"),
            cal("NoOwner", false, ""),
        ])
        .unwrap();
        assert_eq!(s.synced_calendar_ids().unwrap(), ["A", "B"]);
        let syncs: Vec<(String, bool)> = s
            .calendars()
            .unwrap()
            .into_iter()
            .map(|c| (c.calendar.id, c.syncs))
            .collect();
        assert_eq!(
            syncs,
            [
                ("A".to_string(), true),
                ("B".to_string(), true),
                ("Shared".to_string(), false),
                ("NoOwner".to_string(), false)
            ]
        );
        // ASCII case only: a Kelvin sign is not `k`.
        s.replace_calendars(&[
            cal("A", true, "kate@x.com"),
            cal("K", false, "\u{212A}ate@x.com"),
        ])
        .unwrap();
        assert_eq!(s.synced_calendar_ids().unwrap(), ["A"]);
        // The default calendar is own even when its owner address is blank,
        // and then every calendar is (nothing to tell them apart by).
        s.replace_calendars(&[cal("A", true, ""), cal("Shared", false, "boss@x.com")])
            .unwrap();
        assert_eq!(s.synced_calendar_ids().unwrap(), ["A", "Shared"]);
        // No default calendar: every calendar.
        s.replace_calendars(&[cal("X", false, "a@x"), cal("Y", false, "b@x")])
            .unwrap();
        assert_eq!(s.synced_calendar_ids().unwrap(), ["X", "Y"]);
    }

    #[test]
    fn calendar_selection_intersects_the_list_and_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-calendar-selection-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        {
            let s = Store::open(&path).unwrap();
            s.replace_calendars(&[cal("A", true, "me@x"), cal("Shared", false, "boss@x")])
                .unwrap();
            let chosen = vec!["Shared".to_string(), "Later".to_string()];
            assert!(s.set_calendar_selection(Some(&chosen)).unwrap());
            // Same set again: unchanged.
            assert!(!s.set_calendar_selection(Some(&chosen)).unwrap());
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.calendar_selection().unwrap(),
            Some(vec!["Shared".to_string(), "Later".to_string()])
        );
        // `Later` is not listed: it syncs nothing until it is.
        assert_eq!(s.synced_calendar_ids().unwrap(), ["Shared"]);
        s.replace_calendars(&[
            cal("A", true, "me@x"),
            cal("Later", false, "me@x"),
            cal("Shared", false, "boss@x"),
        ])
        .unwrap();
        assert_eq!(s.synced_calendar_ids().unwrap(), ["Later", "Shared"]);
        // `None` is back to the user's own.
        assert!(s.set_calendar_selection(None).unwrap());
        assert_eq!(s.calendar_selection().unwrap(), None);
        assert_eq!(s.synced_calendar_ids().unwrap(), ["A", "Later"]);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_selection_reads_as_none() {
        let s = Store::open_in_memory().unwrap();
        s.conn
            .execute(
                "INSERT INTO meta (key, value) VALUES ('calendar_selection', '{\"a\":1}')",
                [],
            )
            .unwrap();
        assert_eq!(s.calendar_selection().unwrap(), None);
    }

    #[test]
    fn set_calendar_selection_deletes_only_stored_deselected_calendars_events() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        for e in [
            in_cal("a1", "A", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"),
            in_cal("b1", "B", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"),
            // B's history, outside any window: goes too.
            in_cal("b0", "B", "2025-01-01T09:00:00Z", "2025-01-01T10:00:00Z"),
            in_cal(
                "local:1",
                "B",
                "2026-07-20T09:00:00Z",
                "2026-07-20T10:00:00Z",
            ),
            in_cal("old", "", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"),
            in_cal(
                "gone",
                "Unlisted",
                "2026-07-20T09:00:00Z",
                "2026-07-20T10:00:00Z",
            ),
        ] {
            s.upsert_event(&e).unwrap();
        }
        assert!(s.set_calendar_selection(Some(&["A".to_string()])).unwrap());
        let rows = all_events(&s);
        let mut left = ids(&rows);
        left.sort();
        assert_eq!(left, ["a1", "gone", "local:1", "old"]);
    }

    #[test]
    fn set_calendars_before_the_first_list_deletes_nothing() {
        let s = Store::open_in_memory().unwrap();
        for e in [
            in_cal("old", "", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"),
            in_cal("a1", "A", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"),
        ] {
            s.upsert_event(&e).unwrap();
        }
        assert!(!s.set_calendar_selection(Some(&["A".to_string()])).unwrap());
        assert!(!s.set_calendar_selection(None).unwrap());
        assert_eq!(all_events(&s).len(), 2);
    }

    #[test]
    fn apply_calendar_sync_replaces_each_calendars_window_apart() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        for e in [
            in_cal(
                "a-stale",
                "A",
                "2026-07-20T09:00:00Z",
                "2026-07-20T10:00:00Z",
            ),
            in_cal(
                "b-kept",
                "B",
                "2026-07-20T09:00:00Z",
                "2026-07-20T10:00:00Z",
            ),
            in_cal(
                "a-history",
                "A",
                "2026-05-01T09:00:00Z",
                "2026-05-01T10:00:00Z",
            ),
        ] {
            s.upsert_event(&e).unwrap();
        }
        // Only A was fetched (B's view failed): B keeps its rows. A's event
        // claiming another calendar is stored under A.
        s.apply_calendar_sync(
            FROM,
            TO,
            &[window(
                "A",
                vec![in_cal(
                    "a-new",
                    "WRONG",
                    "2026-07-21T09:00:00Z",
                    "2026-07-21T10:00:00Z",
                )],
            )],
            STAMP,
        )
        .unwrap();
        let rows = all_events(&s);
        assert_eq!(ids(&rows), ["a-history", "b-kept", "a-new"]);
        assert_eq!(rows[2].calendar_id, "A");
        assert_eq!(
            ids(&s.events_in_range(FROM, TO, &["A".to_string()]).unwrap()),
            ["a-new"]
        );
    }

    #[test]
    fn apply_calendar_sync_adopts_rows_without_a_calendar_into_the_default() {
        let s = Store::open_in_memory().unwrap();
        for e in [
            in_cal("E1", "", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"),
            in_cal(
                "history",
                "",
                "2026-05-01T09:00:00Z",
                "2026-05-01T10:00:00Z",
            ),
            in_cal(
                "local:1",
                "",
                "2026-07-22T09:00:00Z",
                "2026-07-22T10:00:00Z",
            ),
        ] {
            s.upsert_event(&e).unwrap();
        }
        // No calendar list yet: no default, nothing adopted, nothing deleted.
        s.apply_calendar_sync(FROM, TO, &[], STAMP).unwrap();
        assert!(all_events(&s).iter().all(|e| e.calendar_id.is_empty()));

        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        s.apply_calendar_sync(
            FROM,
            TO,
            &[
                window(
                    "A",
                    vec![in_cal(
                        "E1",
                        "A",
                        "2026-07-20T09:00:00Z",
                        "2026-07-20T10:00:00Z",
                    )],
                ),
                window("B", vec![]),
            ],
            STAMP,
        )
        .unwrap();
        let rows = all_events(&s);
        assert_eq!(ids(&rows), ["history", "E1", "local:1"]);
        assert!(rows.iter().all(|e| e.calendar_id == "A"), "{rows:?}");
    }

    #[test]
    fn apply_calendar_sync_deletes_unsynced_calendars_but_not_local_or_blank_rows() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("Shared", false, "boss@x")])
            .unwrap();
        for e in [
            in_cal(
                "shared",
                "Shared",
                "2026-07-20T09:00:00Z",
                "2026-07-20T10:00:00Z",
            ),
            in_cal(
                "vanished",
                "Gone",
                "2025-07-20T09:00:00Z",
                "2025-07-20T10:00:00Z",
            ),
            in_cal(
                "local:1",
                "Gone",
                "2026-07-20T09:00:00Z",
                "2026-07-20T10:00:00Z",
            ),
        ] {
            s.upsert_event(&e).unwrap();
        }
        s.apply_calendar_sync(FROM, TO, &[window("A", vec![])], STAMP)
            .unwrap();
        assert_eq!(ids(&all_events(&s)), ["local:1"]);
    }

    // --- Calendar ranges on demand (#2014) ---

    fn stamp(at: i64, fresh_since: i64) -> RangeStamp {
        RangeStamp { at, fresh_since }
    }

    fn gaps(s: &Store, cal: &str, from: &str, to: &str, fresh_since: i64) -> Vec<(String, String)> {
        s.calendar_range_gaps(cal, from, to, fresh_since).unwrap()
    }

    fn gap(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    fn range_rows(s: &Store, cal: &str) -> Vec<(String, String, i64)> {
        s.conn
            .prepare(
                "SELECT from_utc, to_utc, fetched_at FROM calendar_ranges
                 WHERE calendar_id = ?1 ORDER BY from_utc",
            )
            .unwrap()
            .query_map(params![cal], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    const D1: &str = "2026-01-01T00:00:00Z";
    const D2: &str = "2026-01-02T00:00:00Z";
    const D3: &str = "2026-01-03T00:00:00Z";
    const D4: &str = "2026-01-04T00:00:00Z";
    const D5: &str = "2026-01-05T00:00:00Z";
    const D6: &str = "2026-01-06T00:00:00Z";

    #[test]
    fn calendar_range_gaps_finds_the_parts_not_fetched_fresh() {
        let s = Store::open_in_memory().unwrap();
        // Nothing stored: the whole range.
        assert_eq!(gaps(&s, "A", D1, D6, 0), [gap(D1, D6)]);
        s.apply_calendar_range(D2, D3, &fetched("A", vec![]), stamp(100, 0))
            .unwrap();
        s.apply_calendar_range(D4, D5, &fetched("A", vec![]), stamp(100, 0))
            .unwrap();
        // Covering: none. Overlap at either end, a hole in the middle.
        assert!(gaps(&s, "A", D2, D3, 0).is_empty());
        assert_eq!(
            gaps(&s, "A", D1, D6, 0),
            [gap(D1, D2), gap(D3, D4), gap(D5, D6)]
        );
        assert_eq!(gaps(&s, "A", D2, D4, 0), [gap(D3, D4)]);
        // Another calendar's ranges do not count.
        assert_eq!(gaps(&s, "B", D2, D3, 0), [gap(D2, D3)]);
        // Stale: the whole range.
        assert_eq!(gaps(&s, "A", D2, D3, 200), [gap(D2, D3)]);
    }

    #[test]
    fn calendar_range_gaps_freshness_is_strict() {
        let s = Store::open_in_memory().unwrap();
        s.apply_calendar_range(D1, D2, &fetched("A", vec![]), stamp(100, 0))
            .unwrap();
        assert!(gaps(&s, "A", D1, D2, 99).is_empty());
        // Fetched exactly at the cutoff: stale, so a zero freshness
        // re-fetches within the same second.
        assert_eq!(gaps(&s, "A", D1, D2, 100), [gap(D1, D2)]);
    }

    #[test]
    fn recording_a_range_splits_older_intervals() {
        let s = Store::open_in_memory().unwrap();
        s.apply_calendar_range(D1, D5, &fetched("A", vec![]), stamp(100, 0))
            .unwrap();
        s.apply_calendar_range(D2, D3, &fetched("A", vec![]), stamp(200, 0))
            .unwrap();
        assert_eq!(
            range_rows(&s, "A"),
            [
                (D1.to_string(), D2.to_string(), 100),
                (D2.to_string(), D3.to_string(), 200),
                (D3.to_string(), D5.to_string(), 100),
            ]
        );
        // Between the two fetch times only the newer piece is fresh.
        assert_eq!(gaps(&s, "A", D1, D5, 150), [gap(D1, D2), gap(D3, D5)]);
        assert!(gaps(&s, "A", D1, D5, 50).is_empty());
    }

    #[test]
    fn recording_prunes_rows_that_can_no_longer_be_fresh() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        // A window pass, then another an hour later shifted by a day: the
        // first one's sliver is past the cutoff and goes.
        s.apply_calendar_sync(D1, D4, &[window("A", vec![])], stamp(100, 0))
            .unwrap();
        s.apply_calendar_sync(D2, D5, &[window("A", vec![])], stamp(3800, 200))
            .unwrap();
        assert_eq!(
            range_rows(&s, "A"),
            [(D2.to_string(), D5.to_string(), 3800)]
        );
        // Another calendar's old rows are its own recording's business.
        s.apply_calendar_range(D1, D2, &fetched("B", vec![]), stamp(100, 0))
            .unwrap();
        s.apply_calendar_range(D5, D6, &fetched("A", vec![]), stamp(9000, 8000))
            .unwrap();
        assert_eq!(range_rows(&s, "B").len(), 1);
        assert_eq!(
            range_rows(&s, "A"),
            [(D5.to_string(), D6.to_string(), 9000)]
        );
    }

    #[test]
    fn apply_calendar_sync_records_the_window_of_each_fetched_calendar() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        // B's view failed: B records nothing.
        s.apply_calendar_sync(FROM, TO, &[window("A", vec![])], STAMP)
            .unwrap();
        assert!(gaps(&s, "A", FROM, TO, 0).is_empty());
        assert_eq!(gaps(&s, "B", FROM, TO, 0), [gap(FROM, TO)]);
    }

    #[test]
    fn apply_calendar_range_replaces_only_that_calendar_in_the_range() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        for e in [
            in_cal(
                "a-stale",
                "A",
                "2026-01-02T09:00:00Z",
                "2026-01-02T10:00:00Z",
            ),
            in_cal("a-out", "A", "2026-01-05T09:00:00Z", "2026-01-05T10:00:00Z"),
            in_cal(
                "local:1",
                "A",
                "2026-01-02T09:00:00Z",
                "2026-01-02T10:00:00Z",
            ),
            in_cal("b-in", "B", "2026-01-02T09:00:00Z", "2026-01-02T10:00:00Z"),
            // Pre-#1984: adopted into the default, as a pass would.
            in_cal("old", "", "2025-01-02T09:00:00Z", "2025-01-02T10:00:00Z"),
        ] {
            s.upsert_event(&e).unwrap();
        }
        s.apply_calendar_range(
            D1,
            D4,
            &fetched(
                "A",
                vec![in_cal(
                    "a-new",
                    "WRONG",
                    "2026-01-03T09:00:00Z",
                    "2026-01-03T10:00:00Z",
                )],
            ),
            STAMP,
        )
        .unwrap();
        let mut a = ids_of(&s, "A");
        a.sort();
        assert_eq!(a, ["a-new", "a-out", "local:1", "old"]);
        assert_eq!(ids_of(&s, "B"), ["b-in"]);
        assert!(gaps(&s, "A", D1, D4, 0).is_empty());
    }

    fn ids_of(s: &Store, cal: &str) -> Vec<String> {
        s.events_in_range(
            "0001-01-01T00:00:00Z",
            "9999-12-31T00:00:00Z",
            &[cal.to_string()],
        )
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect()
    }

    #[test]
    fn deselecting_a_calendar_forgets_its_ranges() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        s.apply_calendar_range(D1, D2, &fetched("B", vec![]), STAMP)
            .unwrap();
        s.set_calendar_selection(Some(&["A".to_string()])).unwrap();
        s.set_calendar_selection(None).unwrap();
        assert_eq!(gaps(&s, "B", D1, D2, 0), [gap(D1, D2)]);
    }

    #[test]
    fn apply_calendar_sync_forgets_ranges_of_a_dropped_empty_calendar() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("Empty", false, "me@x")])
            .unwrap();
        // Empty has no events, only a fetched range.
        s.apply_calendar_sync(
            FROM,
            TO,
            &[window("A", vec![]), window("Empty", vec![])],
            STAMP,
        )
        .unwrap();
        assert!(gaps(&s, "Empty", FROM, TO, 0).is_empty());
        // Graph stops listing it: the next pass forgets its range.
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        s.apply_calendar_sync(FROM, TO, &[window("A", vec![])], STAMP)
            .unwrap();
        assert_eq!(gaps(&s, "Empty", FROM, TO, 0), [gap(FROM, TO)]);
        assert!(gaps(&s, "A", FROM, TO, 0).is_empty());
        // No calendar syncs at all: every range goes.
        s.replace_calendars(&[]).unwrap();
        s.apply_calendar_sync(FROM, TO, &[], STAMP).unwrap();
        assert!(range_rows(&s, "A").is_empty());
    }

    // --- calendarView delta over the kept window (#2013) ---

    /// A delta round of `cal`: `events` added or changed, `removed` gone,
    /// ending with deltaLink `link`.
    fn delta(cal: &str, events: Vec<NewEvent>, removed: &[&str], link: &str) -> SyncedCalendar {
        SyncedCalendar {
            fetched: fetched(cal, events),
            round: CalendarRound::Delta {
                removed: removed.iter().map(|r| r.to_string()).collect(),
                gone_masters: Vec::new(),
            },
            delta_link: link.into(),
        }
    }

    fn sorted_ids_of(s: &Store, cal: &str) -> Vec<String> {
        let mut ids = ids_of(s, cal);
        ids.sort();
        ids
    }

    #[test]
    fn apply_calendar_sync_delta_upserts_and_removes_only_that_calendar() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        s.apply_calendar_sync(
            FROM,
            TO,
            &[
                window(
                    "A",
                    vec![
                        in_cal(
                            "a-keep",
                            "A",
                            "2026-07-20T09:00:00Z",
                            "2026-07-20T10:00:00Z",
                        ),
                        in_cal(
                            "a-change",
                            "A",
                            "2026-07-20T11:00:00Z",
                            "2026-07-20T12:00:00Z",
                        ),
                        in_cal(
                            "shared-id",
                            "A",
                            "2026-07-20T13:00:00Z",
                            "2026-07-20T14:00:00Z",
                        ),
                        // History outside the window: a delta never touches it.
                        in_cal("a-old", "A", "2026-05-01T09:00:00Z", "2026-05-01T10:00:00Z"),
                    ],
                ),
                window(
                    "B",
                    vec![in_cal(
                        "b-keep",
                        "B",
                        "2026-07-20T09:00:00Z",
                        "2026-07-20T10:00:00Z",
                    )],
                ),
            ],
            STAMP,
        )
        .unwrap();
        let mut changed = in_cal(
            "a-change",
            "A",
            "2026-07-22T11:00:00Z",
            "2026-07-22T12:00:00Z",
        );
        changed.subject = "moved".into();
        // A's round: one new event (claiming another calendar), one changed,
        // one removed; B's id in A's removals deletes nothing of B.
        s.apply_calendar_sync(
            FROM,
            TO,
            &[delta(
                "A",
                vec![
                    in_cal(
                        "a-new",
                        "WRONG",
                        "2026-07-21T09:00:00Z",
                        "2026-07-21T10:00:00Z",
                    ),
                    changed,
                ],
                &["shared-id", "b-keep", "never-stored"],
                "LA2",
            )],
            stamp(2000, 0),
        )
        .unwrap();
        assert_eq!(
            sorted_ids_of(&s, "A"),
            ["a-change", "a-keep", "a-new", "a-old"]
        );
        assert_eq!(ids_of(&s, "B"), ["b-keep"]);
        let rows = s.events_in_range(FROM, TO, &["A".to_string()]).unwrap();
        let moved = rows.iter().find(|r| r.id == "a-change").unwrap();
        assert_eq!(
            (moved.subject.as_str(), moved.start_utc.as_str()),
            ("moved", "2026-07-22T11:00:00Z")
        );
        // The round brought the window up to date, and its link is stored.
        assert_eq!(
            range_rows(&s, "A"),
            [(FROM.to_string(), TO.to_string(), 2000)]
        );
        assert_eq!(s.calendar_delta("A").unwrap().unwrap().delta_link, "LA2");
        assert_eq!(s.calendar_delta("B").unwrap().unwrap().delta_link, "L-B");
    }

    #[test]
    fn apply_calendar_sync_delta_removed_series_master_removes_its_occurrences() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        let occurrence = |id: &str, cal: &str, master: &str, day: u32| NewEvent {
            series_master_id: Some(master.into()),
            ..in_cal(
                id,
                cal,
                &format!("2026-07-{day}T09:00:00Z"),
                &format!("2026-07-{day}T10:00:00Z"),
            )
        };
        for e in [
            occurrence("o1", "A", "M", 20),
            occurrence("o2", "A", "M", 27),
            occurrence("other", "A", "N", 20),
            occurrence("b-o1", "B", "M", 20),
            in_cal(
                "single",
                "A",
                "2026-07-21T09:00:00Z",
                "2026-07-21T10:00:00Z",
            ),
        ] {
            s.upsert_event(&e).unwrap();
        }
        s.apply_calendar_sync(FROM, TO, &[delta("A", vec![], &["M"], "L")], STAMP)
            .unwrap();
        assert_eq!(sorted_ids_of(&s, "A"), ["other", "single"]);
        assert_eq!(ids_of(&s, "B"), ["b-o1"], "another calendar's series stays");
    }

    #[test]
    fn delete_event_series_removes_master_and_occurrences() {
        let s = Store::open_in_memory().unwrap();
        let occurrence = |id: &str, cal: &str, master: &str, day: u32| NewEvent {
            series_master_id: Some(master.into()),
            ..in_cal(
                id,
                cal,
                &format!("2026-07-{day}T09:00:00Z"),
                &format!("2026-07-{day}T10:00:00Z"),
            )
        };
        for e in [
            // A row with the master's own id (a local series is its own row).
            in_cal("M", "A", "2026-07-13T09:00:00Z", "2026-07-13T10:00:00Z"),
            occurrence("o1", "A", "M", 20),
            occurrence("o2", "A", "M", 27),
            occurrence("b-o1", "B", "M", 20),
            occurrence("other", "A", "N", 20),
            in_cal(
                "single",
                "A",
                "2026-07-21T09:00:00Z",
                "2026-07-21T10:00:00Z",
            ),
        ] {
            s.upsert_event(&e).unwrap();
        }
        let bob = NewAttendee {
            name: "Bob".into(),
            addr: "bob@x".into(),
            r#type: "required".into(),
            ..Default::default()
        };
        for id in ["o1", "M", "other"] {
            s.put_event_attendees(id, std::slice::from_ref(&bob))
                .unwrap();
        }
        assert_eq!(s.delete_event_series("M").unwrap(), 4);
        assert_eq!(sorted_ids_of(&s, "A"), ["other", "single"]);
        assert!(
            ids_of(&s, "B").is_empty(),
            "every calendar's occurrences go"
        );
        assert!(s.event_attendees("o1").unwrap().is_empty());
        assert!(s.event_attendees("M").unwrap().is_empty());
        assert_eq!(
            s.event_attendees("other").unwrap().len(),
            1,
            "another series keeps its"
        );
        // A master with nothing stored is a no-op.
        assert_eq!(s.delete_event_series("GONE").unwrap(), 0);
        assert_eq!(s.series_calendar_id("N").unwrap().as_deref(), Some("A"));
        assert_eq!(s.series_calendar_id("M").unwrap(), None);
        assert_eq!(sorted_ids_of(&s, "A"), ["other", "single"]);
    }

    #[test]
    fn apply_calendar_sync_delta_cascade_drops_the_delta_state_and_records_no_range() {
        // A master Graph lists as removed may only have changed: after the
        // cascade the next pass must run an initial round.
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        s.apply_calendar_sync(FROM, TO, &[window("A", vec![])], stamp(1000, 0))
            .unwrap();
        s.upsert_event(&NewEvent {
            series_master_id: Some("M".into()),
            ..in_cal("o1", "A", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z")
        })
        .unwrap();
        s.apply_calendar_sync(
            FROM,
            TO,
            &[delta(
                "A",
                vec![in_cal(
                    "new",
                    "A",
                    "2026-07-21T09:00:00Z",
                    "2026-07-21T10:00:00Z",
                )],
                &["M"],
                "LA2",
            )],
            stamp(2000, 0),
        )
        .unwrap();
        assert_eq!(
            sorted_ids_of(&s, "A"),
            ["new"],
            "the round's own writes land"
        );
        assert_eq!(s.calendar_delta("A").unwrap(), None);
        assert_eq!(
            range_rows(&s, "A"),
            [(FROM.to_string(), TO.to_string(), 1000)],
            "the window is not recorded again"
        );
    }

    #[test]
    fn apply_calendar_sync_delta_plain_removal_stores_the_link() {
        // A removed id with no occurrences stored is no cascade, even if it
        // names a master: the link and the window are stored.
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        s.upsert_event(&in_cal(
            "gone",
            "A",
            "2026-07-20T09:00:00Z",
            "2026-07-20T10:00:00Z",
        ))
        .unwrap();
        s.apply_calendar_sync(
            FROM,
            TO,
            &[delta("A", vec![], &["gone", "M-without-rows"], "LA2")],
            stamp(2000, 0),
        )
        .unwrap();
        assert!(ids_of(&s, "A").is_empty());
        assert_eq!(s.calendar_delta("A").unwrap().unwrap().delta_link, "LA2");
        assert_eq!(
            range_rows(&s, "A"),
            [(FROM.to_string(), TO.to_string(), 2000)]
        );
    }

    #[test]
    fn apply_calendar_sync_delta_removes_only_rows_in_the_window() {
        // Graph's calendarView delta also lists events outside the range as
        // `@removed`: rows outside the window are not the round's to delete.
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        let occurrence = |id: &str, start: &str, end: &str| NewEvent {
            series_master_id: Some("M".into()),
            ..in_cal(id, "A", start, end)
        };
        for e in [
            in_cal("in", "A", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"),
            // A FetchCalendarRange row past the window, and history.
            in_cal("later", "A", "2026-09-20T09:00:00Z", "2026-09-20T10:00:00Z"),
            in_cal(
                "history",
                "A",
                "2026-05-01T09:00:00Z",
                "2026-05-01T10:00:00Z",
            ),
            // Overlapping the window's start: in it.
            in_cal("edge", "A", "2026-07-12T23:00:00Z", "2026-07-13T01:00:00Z"),
            occurrence("m-in", "2026-07-21T09:00:00Z", "2026-07-21T10:00:00Z"),
            occurrence("m-later", "2026-09-21T09:00:00Z", "2026-09-21T10:00:00Z"),
        ] {
            s.upsert_event(&e).unwrap();
        }
        s.apply_calendar_sync(
            FROM,
            TO,
            &[delta(
                "A",
                vec![],
                &["in", "later", "history", "edge", "M"],
                "L",
            )],
            STAMP,
        )
        .unwrap();
        assert_eq!(sorted_ids_of(&s, "A"), ["history", "later", "m-later"]);
    }

    #[test]
    fn apply_calendar_sync_delta_keeps_a_series_the_round_names_occurrences_of() {
        // A master starting before the window is "outside the range" when
        // its series is edited: Graph may list it as `@removed` beside the
        // changed occurrences. The series is not gone.
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        let occurrence = |id: &str, day: u32| NewEvent {
            series_master_id: Some("S1".into()),
            ..in_cal(
                id,
                "A",
                &format!("2026-07-{day}T09:00:00Z"),
                &format!("2026-07-{day}T10:00:00Z"),
            )
        };
        for e in [occurrence("o1", 20), occurrence("o2", 27)] {
            s.upsert_event(&e).unwrap();
        }
        let mut changed = occurrence("o1", 20);
        changed.subject = "edited".into();
        s.apply_calendar_sync(FROM, TO, &[delta("A", vec![changed], &["S1"], "L")], STAMP)
            .unwrap();
        assert_eq!(sorted_ids_of(&s, "A"), ["o1", "o2"]);
    }

    #[test]
    fn apply_calendar_sync_delta_keeps_local_rows() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        for e in [
            in_cal(
                "local:1",
                "A",
                "2026-07-20T09:00:00Z",
                "2026-07-20T10:00:00Z",
            ),
            NewEvent {
                series_master_id: Some("M".into()),
                ..in_cal(
                    "local:2",
                    "A",
                    "2026-07-20T09:00:00Z",
                    "2026-07-20T10:00:00Z",
                )
            },
        ] {
            s.upsert_event(&e).unwrap();
        }
        s.apply_calendar_sync(
            FROM,
            TO,
            &[delta("A", vec![], &["local:1", "M"], "L")],
            STAMP,
        )
        .unwrap();
        assert_eq!(sorted_ids_of(&s, "A"), ["local:1", "local:2"]);
    }

    #[test]
    fn apply_calendar_sync_stores_and_forgets_delta_state() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        assert_eq!(s.calendar_delta("A").unwrap(), None);
        s.apply_calendar_sync(FROM, TO, &[window("A", vec![]), window("B", vec![])], STAMP)
            .unwrap();
        assert_eq!(
            s.calendar_delta("A").unwrap(),
            Some(CalendarDeltaState {
                from_utc: FROM.into(),
                to_utc: TO.into(),
                delta_link: "L-A".into(),
            })
        );
        // A calendar whose round failed keeps its state.
        s.apply_calendar_sync(FROM, TO, &[delta("A", vec![], &[], "LA2")], STAMP)
            .unwrap();
        assert_eq!(s.calendar_delta("B").unwrap().unwrap().delta_link, "L-B");
        assert_eq!(s.calendar_delta("A").unwrap().unwrap().delta_link, "LA2");

        // Deselected: its events and state go, so selected again its next
        // round is initial, not a replay of its link onto no rows.
        s.set_calendar_selection(Some(&["A".to_string()])).unwrap();
        assert_eq!(s.calendar_delta("B").unwrap(), None);
        s.set_calendar_selection(None).unwrap();
        assert_eq!(s.calendar_delta("B").unwrap(), None);
        assert!(s.calendar_delta("A").unwrap().is_some());

        // Graph stops listing A: the next pass (step 3) forgets its state.
        s.apply_calendar_sync(FROM, TO, &[window("B", vec![])], STAMP)
            .unwrap();
        s.replace_calendars(&[cal("B", true, "me@x")]).unwrap();
        s.apply_calendar_sync(FROM, TO, &[], STAMP).unwrap();
        assert_eq!(s.calendar_delta("A").unwrap(), None);
        assert!(s.calendar_delta("B").unwrap().is_some());
    }

    #[test]
    fn clear_calendar_deltas_clears_every_calendar() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        s.apply_calendar_sync(FROM, TO, &[window("A", vec![]), window("B", vec![])], STAMP)
            .unwrap();
        s.clear_calendar_deltas().unwrap();
        assert_eq!(s.calendar_delta("A").unwrap(), None);
        assert_eq!(s.calendar_delta("B").unwrap(), None);
    }

    #[test]
    fn events_in_range_filters_by_calendar() {
        let s = Store::open_in_memory().unwrap();
        for e in [
            in_cal("a", "A", "2026-07-20T09:00:00Z", "2026-07-20T10:00:00Z"),
            in_cal("b", "B", "2026-07-20T08:00:00Z", "2026-07-20T09:00:00Z"),
            in_cal("c", "C", "2026-07-20T07:00:00Z", "2026-07-20T08:00:00Z"),
            in_cal("a-out", "A", "2026-07-25T09:00:00Z", "2026-07-25T10:00:00Z"),
        ] {
            s.upsert_event(&e).unwrap();
        }
        let range = |cals: &[&str]| {
            let cals: Vec<String> = cals.iter().map(|c| c.to_string()).collect();
            s.events_in_range("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z", &cals)
                .unwrap()
                .into_iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(range(&["A", "B"]), ["b", "a"]);
        assert_eq!(range(&["C"]), ["c"]);
        assert!(range(&[]).is_empty());
        assert!(range(&["Z"]).is_empty());
    }

    #[test]
    fn days_with_events_marks_local_days() {
        let s = Store::open_in_memory().unwrap();
        let all_day = |id: &str, start: &str, end: &str| NewEvent {
            is_all_day: true,
            ..in_cal(id, "A", start, end)
        };
        for e in [
            // Timed, Oct 30 22:00 .. Nov 2 01:00 UTC: across the month start.
            in_cal("multi", "A", "2026-10-30T22:00:00Z", "2026-11-02T01:00:00Z"),
            // Nov 10 23:30 UTC: Nov 11 at +60, Nov 10 at 0.
            in_cal("late", "A", "2026-11-10T23:30:00Z", "2026-11-10T23:45:00Z"),
            // Nov 20 00:30 UTC: Nov 19 at -60.
            in_cal("early", "A", "2026-11-20T00:30:00Z", "2026-11-20T00:45:00Z"),
            // Ends exactly at midnight UTC: Nov 25 only, at offset 0.
            in_cal(
                "to-midnight",
                "A",
                "2026-11-25T22:00:00Z",
                "2026-11-26T00:00:00Z",
            ),
            // Zero length on Nov 28.
            in_cal("zero", "A", "2026-11-28T12:00:00Z", "2026-11-28T12:00:00Z"),
            // All day Nov 15 and 16: no offset moves it.
            all_day("allday", "2026-11-15T00:00:00Z", "2026-11-17T00:00:00Z"),
            // All day Nov 30 .. Dec 1: only Nov 30 is in November.
            all_day("allday-end", "2026-11-30T00:00:00Z", "2026-12-02T00:00:00Z"),
            // Another calendar's event never counts.
            in_cal("other", "B", "2026-11-05T09:00:00Z", "2026-11-05T10:00:00Z"),
        ] {
            s.upsert_event(&e).unwrap();
        }
        let a = ["A".to_string()];
        assert_eq!(
            s.days_with_events(2026, 11, 0, &a).unwrap(),
            [1, 2, 10, 15, 16, 20, 25, 28, 30]
        );
        // +60: "multi" ends Nov 2 02:00 local, "late" moves to Nov 11, and
        // "to-midnight" now ends at 01:00 on Nov 26.
        assert_eq!(
            s.days_with_events(2026, 11, 60, &a).unwrap(),
            [1, 2, 11, 15, 16, 20, 25, 26, 28, 30]
        );
        // -60: "multi" ends Nov 2 00:00 local (Nov 1 only), "early" moves
        // to Nov 19, "to-midnight" is 21:00..23:00 on Nov 25.
        assert_eq!(
            s.days_with_events(2026, 11, -60, &a).unwrap(),
            [1, 10, 15, 16, 19, 25, 28, 30]
        );
        // The query span follows the offset at both month edges, in a
        // calendar of their own: Oct 31 23:30Z is Nov 1 at +60, Dec 1
        // 00:30Z is Nov 30 at -60, and neither is November at 0.
        for e in [
            in_cal(
                "oct-edge",
                "E",
                "2026-10-31T23:30:00Z",
                "2026-10-31T23:45:00Z",
            ),
            in_cal(
                "dec-edge",
                "E",
                "2026-12-01T00:30:00Z",
                "2026-12-01T00:45:00Z",
            ),
            // Zero length at midnight on the 1st marks the 1st.
            in_cal(
                "zero-first",
                "Z",
                "2026-11-01T00:00:00Z",
                "2026-11-01T00:00:00Z",
            ),
        ] {
            s.upsert_event(&e).unwrap();
        }
        let edge = ["E".to_string()];
        assert_eq!(s.days_with_events(2026, 11, 60, &edge).unwrap(), [1]);
        assert_eq!(s.days_with_events(2026, 11, -60, &edge).unwrap(), [30]);
        assert!(s.days_with_events(2026, 11, 0, &edge).unwrap().is_empty());
        let zero = ["Z".to_string()];
        assert_eq!(s.days_with_events(2026, 11, 0, &zero).unwrap(), [1]);
        assert_eq!(s.days_with_events(2026, 11, 60, &zero).unwrap(), [1]);
        // October holds "multi"'s first two days.
        assert_eq!(s.days_with_events(2026, 10, 0, &a).unwrap(), [30, 31]);
        assert!(s.days_with_events(2026, 11, 0, &[]).unwrap().is_empty());
        assert!(s.days_with_events(2026, 13, 0, &a).unwrap().is_empty());
    }

    #[test]
    fn date_helpers_round_trip() {
        for (y, m, d) in [(1970, 1, 1), (2024, 2, 29), (2026, 12, 31), (1969, 12, 31)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 12), 31);
        let t = "2026-11-07T15:48:53Z";
        assert_eq!(unix_secs_to_iso8601(iso8601_to_unix_secs(t).unwrap()), t);
        assert_eq!(iso8601_to_unix_secs("0000-00-00T00:00:00Z"), None);
        assert_eq!(iso8601_to_unix_secs("2026-11-07"), None);
    }

    #[test]
    fn create_local_event_takes_the_default_calendar() {
        let s = Store::open_in_memory().unwrap();
        let before = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        s.replace_calendars(&[cal("B", false, "me@x"), cal("A", true, "me@x")])
            .unwrap();
        let after = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        let rows = all_events(&s);
        let cal_of = |id: &str| {
            rows.iter()
                .find(|r| r.id == id)
                .unwrap()
                .calendar_id
                .clone()
        };
        assert_eq!(cal_of(&before), "");
        assert_eq!(cal_of(&after), "A");
        let a = ["A".to_string()];
        assert_eq!(
            s.events_in_range("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z", &a)
                .unwrap()
                .len(),
            1
        );
    }

    /// A default `CAL1`, an editable `CAL2` and a read-only `RO` (#2048).
    fn store_with_calendars() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[
            cal("CAL1", true, "me@x"),
            cal("CAL2", false, "me@x"),
            Calendar {
                can_edit: false,
                ..cal("RO", false, "me@x")
            },
        ])
        .unwrap();
        s
    }

    fn in_calendar(id: &str) -> LocalEventFields {
        LocalEventFields {
            calendar_id: Some(id.into()),
            ..sample_fields()
        }
    }

    fn calendar_of(s: &Store, id: &str) -> String {
        all_events(s)
            .into_iter()
            .find(|r| r.id == id)
            .unwrap()
            .calendar_id
    }

    #[test]
    fn create_local_event_takes_a_chosen_calendar() {
        let s = store_with_calendars();
        let id = s
            .create_local_event(&in_calendar("CAL2"), "Me", "me@x")
            .unwrap();
        assert_eq!(calendar_of(&s, &id), "CAL2");
        // `None` and `""` are the default calendar, as before #2048.
        for chosen in [None, Some(String::new())] {
            let f = LocalEventFields {
                calendar_id: chosen,
                ..sample_fields()
            };
            let id = s.create_local_event(&f, "Me", "me@x").unwrap();
            assert_eq!(calendar_of(&s, &id), "CAL1");
        }
        // Reconciling the create keeps the calendar.
        let created = Event {
            id: "EV1".into(),
            ..Event::default()
        };
        s.reconcile_created_event(&id, &created).unwrap();
        assert_eq!(calendar_of(&s, "EV1"), "CAL2");
        let cal2 = ["CAL2".to_string()];
        let ids: Vec<String> = s
            .events_in_range("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z", &cal2)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, ["EV1"]);
    }

    #[test]
    fn create_local_event_refuses_an_unknown_or_read_only_calendar() {
        let counts = |s: &Store| {
            let n = |table: &str| -> i64 {
                s.conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                    .unwrap()
            };
            (
                n("events"),
                n("event_attendees"),
                s.pending_ops().unwrap().len(),
            )
        };
        let s = store_with_calendars();
        for chosen in ["NOPE", "RO"] {
            let err = s.create_local_event(&in_calendar(chosen), "Me", "me@x");
            assert!(
                matches!(&err, Err(StoreError::Sql(m)) if m.contains(chosen)),
                "{chosen}: {err:?}"
            );
            assert_eq!(counts(&s), (0, 0, 0), "{chosen} wrote something");
        }
        // With no calendar list stored, no named calendar is known.
        let empty = Store::open_in_memory().unwrap();
        assert!(
            empty
                .create_local_event(&in_calendar("CAL2"), "Me", "me@x")
                .is_err()
        );
        assert_eq!(counts(&empty), (0, 0, 0));
        // The attendee count checks something: a create that is let through
        // writes one.
        s.create_local_event(&in_calendar("CAL2"), "Me", "me@x")
            .unwrap();
        assert_eq!(counts(&s), (1, 1, 0));
    }

    #[test]
    fn event_create_calendar_is_none_for_blank_and_default() {
        let s = Store::open_in_memory().unwrap();
        let blank = s
            .create_local_event(&sample_fields(), "Me", "me@x")
            .unwrap();
        assert_eq!(s.event_create_calendar(&blank).unwrap(), None);
        s.replace_calendars(&[cal("CAL1", true, "me@x"), cal("CAL2", false, "me@x")])
            .unwrap();
        let default = s
            .create_local_event(&in_calendar("CAL1"), "Me", "me@x")
            .unwrap();
        let other = s
            .create_local_event(&in_calendar("CAL2"), "Me", "me@x")
            .unwrap();
        assert_eq!(s.event_create_calendar(&blank).unwrap(), None);
        assert_eq!(s.event_create_calendar(&default).unwrap(), None);
        assert_eq!(
            s.event_create_calendar(&other).unwrap(),
            Some("CAL2".to_string())
        );
        assert_eq!(s.event_create_calendar("missing").unwrap(), None);
    }

    #[test]
    fn update_event_fields_ignores_calendar_id() {
        let s = store_with_calendars();
        let untouched = synced_event(&s);
        s.conn
            .execute("UPDATE events SET calendar_id = 'CAL1' WHERE id = 'G1'", [])
            .unwrap();
        let moved = LocalEventFields {
            calendar_id: Some("CAL2".into()),
            ..untouched.clone()
        };
        assert_eq!(
            s.update_event_fields("G1", &moved).unwrap(),
            EventFieldSet::default()
        );
        assert_eq!(calendar_of(&s, "G1"), "CAL1");
        let renamed = LocalEventFields {
            subject: "Sync v2".into(),
            ..moved
        };
        assert_eq!(
            s.update_event_fields("G1", &renamed).unwrap(),
            EventFieldSet::SUBJECT
        );
        assert_eq!(calendar_of(&s, "G1"), "CAL1");
    }
    // --- Occurrences take their series' fields (#2064) ---

    /// A synced series master of `cal`, with every field a series gives
    /// its occurrences set.
    fn master(id: &str, cal: &str, start: &str, end: &str) -> NewEvent {
        NewEvent {
            subject: "Standup".into(),
            location: "Room 1".into(),
            organizer_name: "Ann".into(),
            organizer_addr: "ann@x".into(),
            response_status: "accepted".into(),
            response_time: "2026-07-01T09:30:00Z".into(),
            created_utc: "2026-06-30T16:00:00Z".into(),
            body_preview: "Daily".into(),
            body_html: "<p>Join the standup</p>".into(),
            reminder_minutes: 10,
            is_reminder_on: true,
            show_as: "busy".into(),
            is_online_meeting: true,
            online_meeting_provider: "teamsForBusiness".into(),
            join_url: "https://teams.microsoft.com/l/meetup-join/9".into(),
            categories: vec!["Blue category".into()],
            sensitivity: "private".into(),
            importance: "high".into(),
            event_type: "seriesMaster".into(),
            is_organizer: false,
            response_requested: false,
            allow_new_time_proposals: false,
            has_attachments: true,
            original_start_time_zone: "W. Europe Standard Time".into(),
            original_end_time_zone: "W. Europe Standard Time".into(),
            ..in_cal(id, cal, start, end)
        }
    }

    /// An occurrence of `master` as calendarView delta returns it: id,
    /// type, times and `seriesMasterId`, nothing else.
    fn minimal(id: &str, master: &str, cal: &str, start: &str, end: &str) -> NewEvent {
        NewEvent {
            id: id.into(),
            start_utc: start.into(),
            end_utc: end.into(),
            series_master_id: Some(master.into()),
            event_type: "occurrence".into(),
            calendar_id: cal.into(),
            ..Default::default()
        }
    }

    fn attendee(addr: &str) -> NewAttendee {
        NewAttendee {
            name: format!("n-{addr}"),
            addr: addr.into(),
            r#type: "required".into(),
            response: "accepted".into(),
            ..Default::default()
        }
    }

    const M_START: &str = "2026-07-20T09:00:00Z";
    const M_END: &str = "2026-07-20T09:15:00Z";

    /// Master `M` with attendees, occurrences `O1`/`O2` (O1 on the
    /// master's own start) and exception `X` of calendar `A`: a window
    /// round's events.
    fn series_round() -> SyncedCalendar {
        let mut x = minimal(
            "X",
            "M",
            "A",
            "2026-07-22T10:00:00Z",
            "2026-07-22T10:15:00Z",
        );
        x.event_type = "exception".into();
        x.subject = "Moved standup".into();
        x.organizer_name = "Ann".into();
        x.organizer_addr = "ann@x".into();
        let mut round = window(
            "A",
            vec![
                master("M", "A", M_START, M_END),
                minimal("O1", "M", "A", M_START, M_END),
                minimal(
                    "O2",
                    "M",
                    "A",
                    "2026-07-21T09:00:00Z",
                    "2026-07-21T09:15:00Z",
                ),
                x,
            ],
        );
        round.fetched.attendees[0].1 = vec![attendee("ann@x"), attendee("bob@x")];
        round
    }

    fn series_store() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        s.apply_calendar_sync(FROM, TO, &[series_round()], STAMP)
            .unwrap();
        s
    }

    fn row(s: &Store, id: &str) -> EventRow {
        s.event(id).unwrap().unwrap()
    }

    fn addrs(s: &Store, id: &str) -> Vec<String> {
        s.event_attendees(id)
            .unwrap()
            .into_iter()
            .map(|a| a.addr)
            .collect()
    }

    /// Every field an occurrence takes from its master, as the master has it.
    fn assert_filled_from(o: &EventRow, m: &EventRow) {
        assert_eq!(o.subject, m.subject);
        assert_eq!(o.location, m.location);
        assert_eq!(o.organizer_name, m.organizer_name);
        assert_eq!(o.organizer_addr, m.organizer_addr);
        assert_eq!(o.response_status, m.response_status);
        // The user's answer to the series and its creation time (#2068).
        assert_eq!(o.response_time, m.response_time);
        assert_eq!(o.created_utc, m.created_utc);
        assert_eq!(o.is_all_day, m.is_all_day);
        assert_eq!(o.reminder_minutes, m.reminder_minutes);
        assert_eq!(o.is_reminder_on, m.is_reminder_on);
        assert_eq!(o.show_as, m.show_as);
        assert_eq!(o.is_online_meeting, m.is_online_meeting);
        assert_eq!(o.online_meeting_provider, m.online_meeting_provider);
        assert_eq!(o.join_url, m.join_url);
        assert_eq!(o.categories, m.categories);
        assert_eq!(o.sensitivity, m.sensitivity);
        assert_eq!(o.importance, m.importance);
        assert_eq!(o.is_cancelled, m.is_cancelled);
        assert_eq!(o.is_organizer, m.is_organizer);
        assert_eq!(o.response_requested, m.response_requested);
        assert_eq!(o.allow_new_time_proposals, m.allow_new_time_proposals);
        assert_eq!(o.has_attachments, m.has_attachments);
        assert_eq!(o.original_start_time_zone, m.original_start_time_zone);
        assert_eq!(o.original_end_time_zone, m.original_end_time_zone);
    }

    #[test]
    fn apply_calendar_sync_fills_occurrences_from_their_master() {
        let s = series_store();
        let m = row(&s, "M");
        assert_eq!(m.response_time, "2026-07-01T09:30:00Z");
        assert_eq!(m.created_utc, "2026-06-30T16:00:00Z");
        for id in ["O1", "O2"] {
            let o = row(&s, id);
            assert_filled_from(&o, &m);
            // Its own identity, times and type stay.
            assert_eq!(o.event_type, "occurrence");
            assert_eq!(o.series_master_id.as_deref(), Some("M"));
            assert_eq!(o.recurrence, None, "the series' recurrence is the master's");
            assert_eq!(addrs(&s, id), ["ann@x", "bob@x"]);
        }
        assert_eq!(row(&s, "O2").start_utc, "2026-07-21T09:00:00Z");
        let body_preview: String = s
            .conn
            .query_row("SELECT body_preview FROM events WHERE id = 'O1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(body_preview, "Daily");
        // The body is referred to, not copied.
        let own_body: String = s
            .conn
            .query_row("SELECT body_html FROM events WHERE id = 'O1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(own_body, "");
    }

    #[test]
    fn apply_calendar_sync_fill_leaves_exceptions_alone() {
        let s = series_store();
        let x = row(&s, "X");
        assert_eq!(x.subject, "Moved standup");
        assert_eq!(x.location, "");
        assert!(addrs(&s, "X").is_empty());
        // A changed master leaves it alone too.
        let changed = NewEvent {
            subject: "Renamed".into(),
            ..master("M", "A", M_START, M_END)
        };
        s.apply_calendar_sync(FROM, TO, &[delta("A", vec![changed], &[], "L2")], STAMP)
            .unwrap();
        assert_eq!(row(&s, "X").subject, "Moved standup");
        assert_eq!(row(&s, "O1").subject, "Renamed");
    }

    #[test]
    fn apply_calendar_sync_fill_keeps_an_untouched_occurrences_local_write() {
        let s = series_store();
        // An optimistic RSVP on O1, its op not yet sent.
        s.set_event_response("O1", "declined", "2026-08-13T19:19:00Z");
        // A round that touches neither O1 nor M keeps it; O2, re-sent
        // minimal, is filled again.
        s.apply_calendar_sync(
            FROM,
            TO,
            &[delta(
                "A",
                vec![minimal(
                    "O2",
                    "M",
                    "A",
                    "2026-07-21T09:00:00Z",
                    "2026-07-21T09:15:00Z",
                )],
                &[],
                "L2",
            )],
            STAMP,
        )
        .unwrap();
        assert_eq!(row(&s, "O1").response_status, "declined");
        // ...with its own response time (#2068), not the master's.
        assert_eq!(row(&s, "O1").response_time, "2026-08-13T19:19:00Z");
        assert_filled_from(&row(&s, "O2"), &row(&s, "M"));
        assert_eq!(addrs(&s, "O2"), ["ann@x", "bob@x"]);
        // An empty round too.
        s.apply_calendar_sync(FROM, TO, &[delta("A", vec![], &[], "L3")], STAMP)
            .unwrap();
        assert_eq!(row(&s, "O1").response_status, "declined");
        assert_eq!(row(&s, "O1").response_time, "2026-08-13T19:19:00Z");
    }

    #[test]
    fn apply_calendar_sync_delta_master_change_refreshes_its_occurrences() {
        let s = series_store();
        s.set_event_response("O1", "declined", "2026-08-13T19:19:00Z");
        let changed = NewEvent {
            subject: "Renamed".into(),
            location: "Room 2".into(),
            ..master("M", "A", M_START, M_END)
        };
        let mut round = delta("A", vec![changed], &[], "L2");
        round.fetched.attendees[0].1 = vec![attendee("cid@x")];
        s.apply_calendar_sync(FROM, TO, &[round], STAMP).unwrap();
        for id in ["O1", "O2"] {
            let o = row(&s, id);
            assert_eq!(
                (o.subject.as_str(), o.location.as_str()),
                ("Renamed", "Room 2")
            );
            assert_eq!(addrs(&s, id), ["cid@x"]);
        }
        // Server truth for the series wins over the pending write, its
        // response time with it (#2068).
        assert_eq!(row(&s, "O1").response_status, "accepted");
        assert_eq!(row(&s, "O1").response_time, "2026-07-01T09:30:00Z");
    }

    /// The issue's store: occurrences stored blank before #2064, beside
    /// their stored master; a round that changes nothing repairs them.
    #[test]
    fn apply_calendar_sync_repairs_blank_occurrences_from_a_stored_master() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        s.upsert_event(&master("M", "A", M_START, M_END)).unwrap();
        s.put_event_attendees("M", &[attendee("ann@x")]).unwrap();
        s.upsert_event(&minimal("O1", "M", "A", M_START, M_END))
            .unwrap();
        s.upsert_event(&minimal(
            "O2",
            "M",
            "A",
            "2026-07-21T09:00:00Z",
            "2026-07-21T09:15:00Z",
        ))
        .unwrap();
        assert_eq!(s.blank_occurrence_masters("A").unwrap(), ["M"]);
        s.apply_calendar_sync(FROM, TO, &[delta("A", vec![], &[], "L2")], STAMP)
            .unwrap();
        for id in ["O1", "O2"] {
            assert_filled_from(&row(&s, id), &row(&s, "M"));
            assert_eq!(addrs(&s, id), ["ann@x"]);
        }
        assert!(s.blank_occurrence_masters("A").unwrap().is_empty());
    }

    #[test]
    fn apply_calendar_sync_fill_needs_the_master_in_the_same_calendar() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        s.apply_calendar_sync(
            FROM,
            TO,
            &[
                window("B", vec![master("M", "B", M_START, M_END)]),
                window("A", vec![minimal("O1", "M", "A", M_START, M_END)]),
            ],
            STAMP,
        )
        .unwrap();
        assert_eq!(row(&s, "O1").subject, "");
        assert!(!s.has_series_master("A", "M").unwrap());
        assert!(s.has_series_master("B", "M").unwrap());
        assert!(!s.has_series_master("B", "O1").unwrap(), "not a master");
        assert_eq!(s.blank_occurrence_masters("A").unwrap(), ["M"]);
    }

    #[test]
    fn events_in_range_leaves_out_synced_series_masters_but_not_local_ones() {
        use crate::graph::model::{Recurrence, RecurrenceKind};
        let s = series_store();
        // O1 starts with M: the range over that start gives O1, not M.
        let rows = s
            .events_in_range(M_START, M_END, &["A".to_string()])
            .unwrap();
        assert_eq!(ids(&rows), ["O1"]);
        assert!(s.event("M").unwrap().is_some(), "the master stays stored");
        // A local series (#2034) is its own row until its occurrences sync.
        let mut f = in_calendar("A");
        f.recurrence = Some(Recurrence {
            kind: RecurrenceKind::Weekly,
            interval: 1,
            days_of_week: vec!["monday".into()],
            day_of_month: 0,
            start_date: "2026-07-20".into(),
            until: None,
        });
        let local = s.create_local_event(&f, "Me", "me@x").unwrap();
        assert_eq!(row(&s, &local).event_type, "seriesMaster");
        let rows = s.events_in_range(FROM, TO, &["A".to_string()]).unwrap();
        assert!(ids(&rows).contains(&local.as_str()));
        assert!(!ids(&rows).contains(&"M"));
    }

    #[test]
    fn events_in_window_leaves_out_series_masters() {
        let s = series_store();
        assert_eq!(ids(&all_events(&s)), ["O1", "O2", "X"]);
    }

    #[test]
    fn events_in_range_leaves_out_cancelled_series_members() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        let day = |d: u32| {
            (
                format!("2026-07-{d:02}T09:00:00Z"),
                format!("2026-07-{d:02}T10:00:00Z"),
            )
        };
        let cancelled = |id: &str, ty: &str, d: u32| {
            let (a, b) = day(d);
            NewEvent {
                event_type: ty.into(),
                is_cancelled: true,
                series_master_id: (ty != "singleInstance").then(|| "M".to_string()),
                ..in_cal(id, "A", &a, &b)
            }
        };
        s.apply_calendar_sync(
            FROM,
            TO,
            &[window(
                "A",
                vec![
                    cancelled("CO", "occurrence", 20),
                    cancelled("CX", "exception", 21),
                    cancelled("CS", "singleInstance", 22),
                ],
            )],
            STAMP,
        )
        .unwrap();
        let a = ["A".to_string()];
        let rows = s.events_in_range(FROM, TO, &a).unwrap();
        assert_eq!(ids(&rows), ["CS"]);
        assert_eq!(ids(&all_events(&s)), ["CS"]);
        assert_eq!(s.days_with_events(2026, 7, 0, &a).unwrap(), [22]);
    }

    /// A master alone is the series' only row (a new series before its
    /// occurrences sync): it is drawn and marks its day. Once a row of its
    /// series is stored, the occurrences are drawn instead.
    #[test]
    fn a_series_master_leaves_the_grid_once_its_series_has_a_row() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        s.apply_calendar_sync(
            FROM,
            TO,
            &[window("A", vec![master("M", "A", M_START, M_END)])],
            STAMP,
        )
        .unwrap();
        let a = ["A".to_string()];
        assert_eq!(ids(&s.events_in_range(FROM, TO, &a).unwrap()), ["M"]);
        assert_eq!(s.days_with_events(2026, 7, 0, &a).unwrap(), [20]);
        s.apply_calendar_sync(
            FROM,
            TO,
            &[delta(
                "A",
                vec![minimal(
                    "O2",
                    "M",
                    "A",
                    "2026-07-21T09:00:00Z",
                    "2026-07-21T09:15:00Z",
                )],
                &[],
                "L2",
            )],
            STAMP,
        )
        .unwrap();
        assert_eq!(ids(&s.events_in_range(FROM, TO, &a).unwrap()), ["O2"]);
        assert_eq!(s.days_with_events(2026, 7, 0, &a).unwrap(), [21]);
        // A cancelled exception of the series hides it too: the series has
        // a row, even one the grid leaves out.
        let s2 = Store::open_in_memory().unwrap();
        s2.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        let mut x = minimal(
            "X",
            "M",
            "A",
            "2026-07-22T09:00:00Z",
            "2026-07-22T09:15:00Z",
        );
        x.event_type = "exception".into();
        x.is_cancelled = true;
        s2.apply_calendar_sync(
            FROM,
            TO,
            &[window("A", vec![master("M", "A", M_START, M_END), x])],
            STAMP,
        )
        .unwrap();
        assert!(s2.events_in_range(FROM, TO, &a).unwrap().is_empty());
    }

    /// A new recurring series stays on the grid when its `CreateEvent`
    /// reconciles: the row takes its Graph id, still a `seriesMaster`, and
    /// no occurrence is stored until the next calendar pass.
    #[test]
    fn a_created_series_stays_on_the_grid_through_its_reconcile() {
        use crate::graph::model::{Recurrence, RecurrenceKind};
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        let mut f = in_calendar("A");
        f.recurrence = Some(Recurrence {
            kind: RecurrenceKind::Weekly,
            interval: 1,
            days_of_week: vec!["monday".into()],
            day_of_month: 0,
            start_date: "2026-07-20".into(),
            until: None,
        });
        let local = s.create_local_event(&f, "Me", "me@x").unwrap();
        let a = ["A".to_string()];
        assert_eq!(
            ids(&s.events_in_range(FROM, TO, &a).unwrap()),
            [local.as_str()]
        );
        let created = Event {
            id: "G-SERIES".into(),
            event_type: "seriesMaster".into(),
            subject: f.subject.clone(),
            start_utc: f.start_utc.clone(),
            end_utc: f.end_utc.clone(),
            ..Default::default()
        };
        s.reconcile_created_event(&local, &created).unwrap();
        assert_eq!(row(&s, "G-SERIES").event_type, "seriesMaster");
        assert_eq!(ids(&s.events_in_range(FROM, TO, &a).unwrap()), ["G-SERIES"]);
    }

    /// An all-day series' minimal occurrence reads `isAllDay` false; the
    /// fill takes the master's, so its day is not shifted by the offset.
    #[test]
    fn a_filled_all_day_occurrence_marks_its_utc_date() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        let all_day = NewEvent {
            is_all_day: true,
            ..master("M", "A", "2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
        };
        s.apply_calendar_sync(
            FROM,
            TO,
            &[window(
                "A",
                vec![
                    all_day,
                    minimal(
                        "O1",
                        "M",
                        "A",
                        "2026-07-22T00:00:00Z",
                        "2026-07-23T00:00:00Z",
                    ),
                ],
            )],
            STAMP,
        )
        .unwrap();
        assert!(row(&s, "O1").is_all_day);
        let a = ["A".to_string()];
        assert_eq!(s.days_with_events(2026, 7, -300, &a).unwrap(), [22]);
    }

    #[test]
    fn event_body_of_an_occurrence_falls_back_to_its_master() {
        let s = series_store();
        assert_eq!(
            s.event_body("O1").unwrap().as_deref(),
            Some("<p>Join the standup</p>")
        );
        // An exception is a full item: its own (empty) body.
        assert_eq!(s.event_body("X").unwrap().as_deref(), Some(""));
        // An occurrence with a body of its own reads that.
        s.conn
            .execute(
                "UPDATE events SET body_html = '<p>own</p>' WHERE id = 'O2'",
                [],
            )
            .unwrap();
        assert_eq!(s.event_body("O2").unwrap().as_deref(), Some("<p>own</p>"));
        assert_eq!(s.event_body("nope").unwrap(), None);
        // `event_for_send` reads the row's own body, never the master's,
        // which is stored (r4: a cleared body is sent cleared).
        s.conn
            .execute("UPDATE events SET body_html = '' WHERE id = 'O2'", [])
            .unwrap();
        assert_eq!(
            s.event_body("O2").unwrap().as_deref(),
            Some("<p>Join the standup</p>")
        );
        assert_eq!(s.event_for_send("O2").unwrap().unwrap().body_html, "");
        // Its master gone, an occurrence reads its own empty body.
        s.delete_event("M").unwrap();
        assert_eq!(s.event_body("O1").unwrap().as_deref(), Some(""));
    }

    #[test]
    fn blank_occurrence_masters_lists_distinct_ids() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        for e in [
            minimal("O1", "M2", "A", M_START, M_END),
            minimal(
                "O2",
                "M2",
                "A",
                "2026-07-21T09:00:00Z",
                "2026-07-21T09:15:00Z",
            ),
            minimal(
                "O3",
                "M1",
                "A",
                "2026-07-22T09:00:00Z",
                "2026-07-22T09:15:00Z",
            ),
            // Another calendar's, a filled one and an exception: not listed.
            minimal("B1", "MB", "B", M_START, M_END),
            NewEvent {
                subject: "filled".into(),
                ..minimal("O4", "M3", "A", M_START, M_END)
            },
            NewEvent {
                event_type: "exception".into(),
                ..minimal("X1", "M4", "A", M_START, M_END)
            },
        ] {
            s.upsert_event(&e).unwrap();
        }
        assert_eq!(s.blank_occurrence_masters("A").unwrap(), ["M1", "M2"]);
        assert_eq!(s.blank_occurrence_masters("B").unwrap(), ["MB"]);
        assert!(s.blank_occurrence_masters("C").unwrap().is_empty());
    }

    #[test]
    fn apply_calendar_range_keeps_a_stored_series_master() {
        let s = series_store();
        // A plain calendarView range over the master's start: full
        // occurrences, never the master.
        let full = NewEvent {
            subject: "Standup".into(),
            organizer_name: "Ann".into(),
            ..minimal("O1", "M", "A", M_START, M_END)
        };
        s.apply_calendar_range(
            "2026-07-20T00:00:00Z",
            "2026-07-21T00:00:00Z",
            &fetched("A", vec![full]),
            STAMP,
        )
        .unwrap();
        assert!(s.has_series_master("A", "M").unwrap());
        // A window round that does not return it still removes it.
        s.apply_calendar_sync(FROM, TO, &[window("A", vec![])], STAMP)
            .unwrap();
        assert!(!s.has_series_master("A", "M").unwrap());
    }

    #[test]
    fn update_event_fields_on_a_filled_occurrence_does_not_mark_the_masters_body() {
        let s = series_store();
        let o = row(&s, "O1");
        let edit = LocalEventFields {
            subject: "Just this one".into(),
            start_utc: o.start_utc.clone(),
            end_utc: o.end_utc.clone(),
            location: o.location.clone(),
            // What a host shows: `event_body`, the master's.
            body_html: s.event_body("O1").unwrap().unwrap(),
            attendees: s
                .event_attendees("O1")
                .unwrap()
                .into_iter()
                .map(|a| AttendeeInput {
                    name: a.name,
                    addr: a.addr,
                    r#type: a.r#type,
                })
                .collect(),
            ..Default::default()
        };
        assert_eq!(
            s.update_event_fields("O1", &edit).unwrap(),
            EventFieldSet::SUBJECT
        );
    }

    /// Clearing a filled occurrence's shown body (#2064 r4): `BODY` is
    /// marked, and the send reads the row's own (now empty) body, not the
    /// master's. Until the next sync stores the occurrence as an
    /// exception, `event_body` still shows the master's.
    #[test]
    fn update_event_fields_clearing_an_occurrences_body_sends_it_empty() {
        let s = series_store();
        assert_eq!(s.event_for_send("O1").unwrap().unwrap().body_html, "");
        let o = row(&s, "O1");
        let edit = LocalEventFields {
            subject: o.subject.clone(),
            start_utc: o.start_utc.clone(),
            end_utc: o.end_utc.clone(),
            location: o.location.clone(),
            body_html: String::new(),
            attendees: s
                .event_attendees("O1")
                .unwrap()
                .into_iter()
                .map(|a| AttendeeInput {
                    name: a.name,
                    addr: a.addr,
                    r#type: a.r#type,
                })
                .collect(),
            ..Default::default()
        };
        assert_eq!(
            s.update_event_fields("O1", &edit).unwrap(),
            EventFieldSet::BODY
        );
        assert_eq!(s.event_for_send("O1").unwrap().unwrap().body_html, "");
        assert_eq!(
            s.event_body("O1").unwrap().as_deref(),
            Some("<p>Join the standup</p>"),
            "the known transient"
        );
    }

    /// An edit that leaves an occurrence's shown body alone keeps the row's
    /// own (#2064 r5): a subject-only edit of a filled occurrence keeps
    /// `''`, so it still follows its master, and one after a queued clear
    /// does not write the master's body back for the clear to send.
    #[test]
    fn update_event_fields_leaving_an_occurrences_body_keeps_its_own() {
        let s = series_store();
        let own_body = |id: &str| -> String {
            s.conn
                .query_row("SELECT body_html FROM events WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        let edit = |s: &Store, id: &str, subject: &str, body: String| {
            let o = s.event(id).unwrap().unwrap();
            let f = LocalEventFields {
                subject: subject.into(),
                start_utc: o.start_utc.clone(),
                end_utc: o.end_utc.clone(),
                location: o.location.clone(),
                body_html: body,
                attendees: s
                    .event_attendees(id)
                    .unwrap()
                    .into_iter()
                    .map(|a| AttendeeInput {
                        name: a.name,
                        addr: a.addr,
                        r#type: a.r#type,
                    })
                    .collect(),
                ..Default::default()
            };
            s.update_event_fields(id, &f).unwrap()
        };
        // Never cleared: a subject-only edit keeps the own `''`.
        let shown = s.event_body("O2").unwrap().unwrap();
        assert_eq!(edit(&s, "O2", "Only this", shown), EventFieldSet::SUBJECT);
        assert_eq!(own_body("O2"), "");
        assert_eq!(
            s.event_body("O2").unwrap().as_deref(),
            Some("<p>Join the standup</p>")
        );
        // Cleared, then a subject-only edit from a form showing the
        // master's body (the transient): the row keeps `''`, so the queued
        // clear sends `''`.
        assert_eq!(
            edit(&s, "O1", "Standup", String::new()),
            EventFieldSet::BODY
        );
        let shown = s.event_body("O1").unwrap().unwrap();
        assert_eq!(shown, "<p>Join the standup</p>");
        assert_eq!(edit(&s, "O1", "Renamed", shown), EventFieldSet::SUBJECT);
        assert_eq!(own_body("O1"), "");
        assert_eq!(s.event_for_send("O1").unwrap().unwrap().body_html, "");
        assert_eq!(row(&s, "O1").subject, "Renamed");
    }

    #[test]
    fn apply_calendar_sync_gone_masters_deletes_only_that_calendars_master() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x"), cal("B", false, "me@x")])
            .unwrap();
        // A master starting before the window, another calendar's copy of
        // its id, and an occurrence named by mistake.
        let before = ("2026-05-04T09:00:00Z", "2026-05-04T09:15:00Z");
        s.upsert_event(&master("M", "A", before.0, before.1))
            .unwrap();
        s.upsert_event(&master("MB", "B", before.0, before.1))
            .unwrap();
        s.upsert_event(&minimal("O1", "M", "A", M_START, M_END))
            .unwrap();
        let mut round = delta("A", vec![], &["M"], "L2");
        if let CalendarRound::Delta { gone_masters, .. } = &mut round.round {
            *gone_masters = vec!["M".into(), "MB".into(), "O1".into()];
        }
        s.apply_calendar_sync(FROM, TO, &[round], STAMP).unwrap();
        assert!(s.event("M").unwrap().is_none());
        assert!(s.event("MB").unwrap().is_some(), "another calendar's");
        // O1 went with the cascade of the removed M, not as a gone master.
        assert!(s.event("O1").unwrap().is_none());
        let s2 = Store::open_in_memory().unwrap();
        s2.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        s2.upsert_event(&minimal("O1", "M", "A", M_START, M_END))
            .unwrap();
        let mut round = delta("A", vec![], &[], "L2");
        if let CalendarRound::Delta { gone_masters, .. } = &mut round.round {
            *gone_masters = vec!["O1".into()];
        }
        s2.apply_calendar_sync(FROM, TO, &[round], STAMP).unwrap();
        assert!(s2.event("O1").unwrap().is_some(), "not a master");
    }
    /// A master a round brings unchanged (the same `last_modified`) fills
    /// only the round's own occurrences, not its others of any date, which
    /// may hold a pending local write (#2064 r3); a changed one fills them
    /// all. Here Graph's initial round lists the master itself.
    #[test]
    fn apply_calendar_sync_unchanged_master_fills_only_the_rounds_occurrences() {
        let s = Store::open_in_memory().unwrap();
        s.replace_calendars(&[cal("A", true, "me@x")]).unwrap();
        let m = |modified: &str, subject: &str| NewEvent {
            last_modified: modified.into(),
            subject: subject.into(),
            ..master("M", "A", M_START, M_END)
        };
        let far = ("2026-12-07T09:00:00Z", "2026-12-07T09:15:00Z");
        s.apply_calendar_sync(
            FROM,
            TO,
            &[window(
                "A",
                vec![
                    m("2026-07-01T00:00:00Z", "Standup"),
                    minimal("O1", "M", "A", M_START, M_END),
                ],
            )],
            STAMP,
        )
        .unwrap();
        // An occurrence outside the window (a range fetch's), with an RSVP
        // not yet sent.
        s.upsert_event(&minimal("O-FAR", "M", "A", far.0, far.1))
            .unwrap();
        s.set_event_response("O-FAR", "tentative", "2026-08-13T19:19:00Z");
        let next_day =
            |master: NewEvent| window("A", vec![master, minimal("O1", "M", "A", M_START, M_END)]);
        s.apply_calendar_sync(
            FROM,
            TO,
            &[next_day(m("2026-07-01T00:00:00Z", "Standup"))],
            STAMP,
        )
        .unwrap();
        assert_eq!(row(&s, "O-FAR").response_status, "tentative");
        assert_eq!(
            row(&s, "O1").subject,
            "Standup",
            "the round's own is filled"
        );
        // A changed master is server truth for every occurrence.
        s.apply_calendar_sync(
            FROM,
            TO,
            &[next_day(m("2026-07-02T00:00:00Z", "Renamed"))],
            STAMP,
        )
        .unwrap();
        let o = row(&s, "O-FAR");
        assert_eq!(
            (o.subject.as_str(), o.response_status.as_str()),
            ("Renamed", "accepted")
        );
        // A master with no `last_modified` counts as changed.
        s.set_event_response("O-FAR", "tentative", "2026-08-13T19:19:00Z");
        s.apply_calendar_sync(FROM, TO, &[next_day(m("", "Renamed"))], STAMP)
            .unwrap();
        assert_eq!(row(&s, "O-FAR").response_status, "accepted");
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
                send_response: true,
                proposed_start_utc: None,
                proposed_end_utc: None,
            }
            .to_json()
            .to_string(),
            r#"{"kind":"respondEvent","id":"E1","rsvp":"accepted","comment":"looking forward to it","sendResponse":true,"proposedStart":null,"proposedEnd":null}"#
        );
        assert_eq!(
            OutboxOp::RespondEvent {
                id: "E1".into(),
                kind: "declined".into(),
                comment: None,
                send_response: false,
                proposed_start_utc: None,
                proposed_end_utc: None,
            }
            .to_json()
            .to_string(),
            r#"{"kind":"respondEvent","id":"E1","rsvp":"declined","comment":null,"sendResponse":false,"proposedStart":null,"proposedEnd":null}"#
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
                send_response: true,
                proposed_start_utc: None,
                proposed_end_utc: None,
            },
            OutboxOp::RespondEvent {
                id: "E2".into(),
                kind: "tentativelyAccepted".into(),
                comment: None,
                send_response: false,
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
                send_response: true,
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
    fn respond_event_queued_before_send_response_reads_as_sending() {
        // A row queued before #2023 has no `sendResponse` key: it was always
        // sent, so it still is.
        let v = json::parse(
            r#"{"kind":"respondEvent","id":"E1","rsvp":"declined","comment":null,"proposedStart":null,"proposedEnd":null}"#,
        )
        .unwrap();
        assert_eq!(
            OutboxOp::from_json(&v),
            Some(OutboxOp::RespondEvent {
                id: "E1".into(),
                kind: "declined".into(),
                comment: None,
                send_response: true,
                proposed_start_utc: None,
                proposed_end_utc: None,
            })
        );
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
    fn outbox_op_update_event_series_round_trips() {
        let op = OutboxOp::UpdateEventSeries {
            id: "M/1".into(),
            body: r#"{"subject":"Review","recurrence":{"pattern":{"type":"daily","interval":1}}}"#
                .into(),
        };
        let stored = op.to_json().to_string();
        assert_eq!(
            stored,
            r#"{"kind":"updateEventSeries","id":"M/1","body":"{\"subject\":\"Review\",\"recurrence\":{\"pattern\":{\"type\":\"daily\",\"interval\":1}}}"}"#
        );
        assert_eq!(
            OutboxOp::from_json(&json::parse(&stored).unwrap()),
            Some(op.clone())
        );
        // It survives the outbox table, as a restart reads it.
        let s = Store::open_in_memory().unwrap();
        s.enqueue_op(&op).unwrap();
        assert_eq!(s.pending_ops().unwrap()[0].op, op);
        // A row without a body, or without an id, is corrupt.
        for corrupt in [
            r#"{"kind":"updateEventSeries","id":"M"}"#,
            r#"{"kind":"updateEventSeries","body":"{}"}"#,
        ] {
            assert_eq!(OutboxOp::from_json(&json::parse(corrupt).unwrap()), None);
        }
    }

    #[test]
    fn outbox_op_cancel_and_forward_round_trip() {
        let cancel = OutboxOp::CancelEvent {
            id: "EV1".into(),
            comment: Some("Moved".into()),
        };
        assert_eq!(
            cancel.to_json().to_string(),
            r#"{"kind":"cancelEvent","id":"EV1","comment":"Moved"}"#
        );
        let forward = OutboxOp::ForwardEvent {
            id: "EV1".into(),
            to: vec![
                Recipient {
                    name: "Ann".into(),
                    address: "ann@x".into(),
                },
                Recipient {
                    name: "".into(),
                    address: "bob@y".into(),
                },
            ],
            comment: None,
        };
        assert_eq!(
            forward.to_json().to_string(),
            r#"{"kind":"forwardEvent","id":"EV1","to":[{"name":"Ann","address":"ann@x"},{"name":"","address":"bob@y"}],"comment":null}"#
        );
        for op in [
            cancel,
            forward,
            OutboxOp::CancelEvent {
                id: "local:e1".into(),
                comment: None,
            },
            OutboxOp::ForwardEvent {
                id: "EV2".into(),
                to: vec![],
                comment: Some("FYI".into()),
            },
        ] {
            let stored = op.to_json().to_string();
            assert_eq!(
                OutboxOp::from_json(&json::parse(&stored).unwrap()),
                Some(op),
                "{stored}"
            );
        }
        // A recipient without an address, or no `to` at all, is a corrupt
        // row; a missing name reads as empty.
        for corrupt in [
            r#"{"kind":"forwardEvent","id":"EV1","to":[{"name":"Ann"}],"comment":null}"#,
            r#"{"kind":"forwardEvent","id":"EV1","comment":null}"#,
            r#"{"kind":"cancelEvent","comment":null}"#,
        ] {
            assert_eq!(OutboxOp::from_json(&json::parse(corrupt).unwrap()), None);
        }
        let v =
            json::parse(r#"{"kind":"forwardEvent","id":"EV1","to":[{"address":"a@x"}]}"#).unwrap();
        assert_eq!(
            OutboxOp::from_json(&v),
            Some(OutboxOp::ForwardEvent {
                id: "EV1".into(),
                to: vec![Recipient {
                    name: "".into(),
                    address: "a@x".into(),
                }],
                comment: None,
            })
        );
    }

    #[test]
    fn outbox_op_dismiss_and_snooze_round_trip() {
        let dismiss = OutboxOp::DismissReminder { id: "EV1".into() };
        assert_eq!(
            dismiss.to_json().to_string(),
            r#"{"kind":"dismissReminder","id":"EV1"}"#
        );
        let snooze = OutboxOp::SnoozeReminder {
            id: "EV1".into(),
            new_reminder_utc: "2026-11-08T10:15:00Z".into(),
        };
        assert_eq!(
            snooze.to_json().to_string(),
            r#"{"kind":"snoozeReminder","id":"EV1","newReminderUtc":"2026-11-08T10:15:00Z"}"#
        );
        for op in [dismiss, snooze] {
            let stored = op.to_json().to_string();
            assert_eq!(
                OutboxOp::from_json(&json::parse(&stored).unwrap()),
                Some(op),
                "{stored}"
            );
        }
        // No id, or a snooze without its time, is a corrupt row.
        for corrupt in [
            r#"{"kind":"dismissReminder"}"#,
            r#"{"kind":"snoozeReminder","newReminderUtc":"2026-11-08T10:15:00Z"}"#,
            r#"{"kind":"snoozeReminder","id":"EV1"}"#,
            r#"{"kind":"snoozeReminder","id":"EV1","newReminderUtc":null}"#,
        ] {
            assert_eq!(
                OutboxOp::from_json(&json::parse(corrupt).unwrap()),
                None,
                "{corrupt}"
            );
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
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: Vec::new(),
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
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
    fn upload_session_is_stored_and_cleared_on_upload() {
        // #1941: a send's upload session is kept on the row with its digest,
        // follows the draft to its Graph id, and is gone once uploaded.
        let s = Store::open_in_memory().unwrap();
        let id = s.create_local_draft("Sub", "", "", "body").unwrap();
        s.add_outbound_attachment(&id, "/tmp/a.pdf", "a.pdf", 10)
            .unwrap();
        s.add_outbound_attachment(&id, "/tmp/b.pdf", "b.pdf", 10)
            .unwrap();
        let session = |draft: &str, i: usize| {
            let got = s.outbound_attachments(draft).unwrap();
            (got[i].upload_url.clone(), got[i].upload_digest.clone())
        };
        assert_eq!(session(&id, 0), (None, None), "none before a send");

        s.set_outbound_attachment_upload_session(&id, "/tmp/a.pdf", Some(("https://u/1", "10:ab")))
            .unwrap();
        s.set_outbound_attachment_upload_session(&id, "/tmp/b.pdf", Some(("https://u/2", "10:cd")))
            .unwrap();
        let want_a = (Some("https://u/1".to_string()), Some("10:ab".to_string()));
        assert_eq!(session(&id, 0), want_a);

        s.reconcile_id(&id, "GRAPH-1").unwrap();
        assert_eq!(session("GRAPH-1", 0), want_a, "follows the Graph id");
        assert_eq!(session(&id, 0), want_a, "the local id still reaches it");

        // Forgotten (Graph refused it): both columns cleared.
        s.set_outbound_attachment_upload_session(&id, "/tmp/b.pdf", None)
            .unwrap();
        assert_eq!(session("GRAPH-1", 1), (None, None));

        // Uploaded: the session is done.
        s.mark_outbound_attachment_uploaded("GRAPH-1", "/tmp/a.pdf")
            .unwrap();
        let got = s.outbound_attachments("GRAPH-1").unwrap();
        assert!(got[0].uploaded);
        assert_eq!(
            (got[0].upload_url.clone(), got[0].upload_digest.clone()),
            (None, None)
        );

        // An absent row is a no-op.
        s.set_outbound_attachment_upload_session("GRAPH-1", "/tmp/z", Some(("u", "d")))
            .unwrap();
        assert_eq!(s.outbound_attachments("GRAPH-1").unwrap().len(), 2);
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
        // #1941's columns are added too, and read as no session.
        assert_eq!(got[0].upload_url, None);
        assert_eq!(got[0].upload_digest, None);
        s.set_outbound_attachment_upload_session("G", "/tmp/a.pdf", Some(("https://u", "1:ff")))
            .unwrap();
        assert_eq!(
            s.outbound_attachments("G").unwrap()[0]
                .upload_url
                .as_deref(),
            Some("https://u")
        );
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
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: Vec::new(),
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
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
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: Vec::new(),
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
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
        // A row that reads `is_draft = 0` (stored before #2096, or written
        // by a code-built `Message`): the Drafts folder still says it is a
        // draft.
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
    fn message_header_keeps_msip_labels() {
        // #2015: the sensitivity label is stored beside Exchange's stamps,
        // written under any case and read back under any case, its value
        // trimmed and otherwise verbatim.
        const LABEL: &str = "MSIP_Label_6f0b_Enabled=true; MSIP_Label_6f0b_Name=Confidential; \
            MSIP_Label_6f0b_ContentBits=0";
        let s = store_with_message();
        s.put_fetched_body_with_headers(
            "M",
            &text_body("hi"),
            BodyFormat::Text,
            &[
                header(AUTH_AS, "Internal"),
                header("MSIP_Labels", &format!(" {LABEL} ")),
            ],
        )
        .unwrap();
        for name in ["msip_labels", "MSIP_LABELS", "MSIP_Labels"] {
            assert_eq!(
                s.message_header("M", name).unwrap().as_deref(),
                Some(LABEL),
                "{name}"
            );
        }
        assert_eq!(
            s.message_header("M", AUTH_AS).unwrap().as_deref(),
            Some("Internal")
        );
        assert_eq!(headers_rows(&s), 2);
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

/// A draft's unpushed body kept against a fetched one (#2082).
#[cfg(test)]
mod kept_body_tests {
    use super::*;

    const AUTH_AS: &str = "X-MS-Exchange-Organization-AuthAs";

    fn html(content: &str) -> Body {
        Body {
            content_type: "html".into(),
            content: content.into(),
        }
    }

    fn header(value: &str) -> Vec<MessageHeader> {
        vec![MessageHeader {
            name: AUTH_AS.into(),
            value: value.into(),
        }]
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
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: Vec::new(),
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
        }
    }

    /// A store with the Drafts folder holding draft `D` and the Inbox
    /// holding message `M`, neither with a body.
    fn store() -> Store {
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
        s.upsert_message("DRAFTS", &message("D", true)).unwrap();
        s.upsert_message("INBOX", &message("M", false)).unwrap();
        s
    }

    fn body(s: &Store, id: &str) -> Option<String> {
        s.get_body(id).unwrap().map(|b| b.content)
    }

    /// Both fetched writers, each refused or each taking `server` (the
    /// headers' writer last, so a taken fetch ends with its body).
    fn fetch_both(s: &Store, id: &str, server: &str) -> (bool, bool) {
        let plain = s
            .put_fetched_body(id, &html(server), BodyFormat::Html)
            .unwrap();
        let with_headers = s
            .put_fetched_body_with_headers(id, &html(server), BodyFormat::Html, &header("Server"))
            .unwrap();
        (plain, with_headers)
    }

    #[test]
    fn a_fetched_body_keeps_a_locally_edited_drafts_body() {
        let s = store();
        assert!(
            s.put_fetched_body_with_headers(
                "D",
                &html("<p>quote</p>"),
                BodyFormat::Html,
                &header("Internal"),
            )
            .unwrap()
        );
        s.update_draft_fields("D", "S", "bob@x", "", "", "<p>mine</p>")
            .unwrap();

        assert_eq!(fetch_both(&s, "D", "<p>quote</p>"), (false, false));
        assert_eq!(body(&s, "D").as_deref(), Some("<p>mine</p>"));
        assert_eq!(
            s.message_header("D", AUTH_AS).unwrap().as_deref(),
            Some("Internal"),
            "a kept body keeps its headers too"
        );

        // Pushed: Graph has the user's body now, and a fetch writes again.
        let generation = s.local_edit_generation("D").unwrap();
        assert!(s.clear_local_edit("D", generation).unwrap());
        assert_eq!(fetch_both(&s, "D", "<p>graph's</p>"), (true, true));
        assert_eq!(body(&s, "D").as_deref(), Some("<p>graph's</p>"));
        assert_eq!(
            s.message_header("D", AUTH_AS).unwrap().as_deref(),
            Some("Server")
        );
    }

    #[test]
    fn a_queued_send_or_save_keeps_the_body() {
        for op in [
            OutboxOp::SaveDraft { id: "D".into() },
            OutboxOp::SendDraft { id: "D".into() },
        ] {
            let s = store();
            s.put_body("D", &html("<p>mine</p>")).unwrap();
            assert_eq!(s.local_edit_generation("D").unwrap(), 0);
            let seq = s.enqueue_op(&op).unwrap();

            assert_eq!(
                fetch_both(&s, "D", "<p>quote</p>"),
                (false, false),
                "{op:?}"
            );
            assert_eq!(body(&s, "D").as_deref(), Some("<p>mine</p>"), "{op:?}");

            s.drop_op(seq);
            assert_eq!(
                fetch_both(&s, "D", "<p>graph's</p>"),
                (true, true),
                "{op:?}"
            );
            assert_eq!(body(&s, "D").as_deref(), Some("<p>graph's</p>"), "{op:?}");
        }
    }

    #[test]
    fn a_draft_op_under_the_local_id_protects_the_reconciled_row() {
        let s = store();
        let local = s
            .create_local_draft("S", "bob@x", "", "<p>mine</p>")
            .unwrap();
        s.enqueue_op(&OutboxOp::SendDraft { id: local.clone() })
            .unwrap();
        s.reconcile_id(&local, "G").unwrap();
        assert_eq!(s.local_edit_generation("G").unwrap(), 0);

        assert_eq!(fetch_both(&s, "G", "<p>quote</p>"), (false, false));
        assert_eq!(body(&s, "G").as_deref(), Some("<p>mine</p>"));
    }

    #[test]
    fn a_mark_read_op_does_not_keep_a_body() {
        let s = store();
        s.put_body("M", &html("<p>old</p>")).unwrap();
        for op in [
            OutboxOp::MarkRead {
                id: "M".into(),
                read: true,
            },
            OutboxOp::Delete { id: "M".into() },
        ] {
            s.enqueue_op(&op).unwrap();
        }

        assert_eq!(fetch_both(&s, "M", "<p>fetched</p>"), (true, true));
        assert_eq!(body(&s, "M").as_deref(), Some("<p>fetched</p>"));
    }

    #[test]
    fn a_protected_row_without_a_body_takes_the_fetch() {
        let s = store();
        s.enqueue_op(&OutboxOp::SendDraft { id: "D".into() })
            .unwrap();
        s.conn
            .execute("UPDATE messages SET local_edit = 3 WHERE id = 'D'", [])
            .unwrap();

        assert!(
            s.put_fetched_body_with_headers(
                "D",
                &html("<p>graph's</p>"),
                BodyFormat::Html,
                &header("Server"),
            )
            .unwrap()
        );
        assert_eq!(body(&s, "D").as_deref(), Some("<p>graph's</p>"));
    }

    #[test]
    fn put_body_still_writes_over_a_protected_draft() {
        let s = store();
        s.update_draft_fields("D", "S", "bob@x", "", "", "<p>mine</p>")
            .unwrap();
        s.enqueue_op(&OutboxOp::SendDraft { id: "D".into() })
            .unwrap();

        s.put_body("D", &html("<p>newer local</p>")).unwrap();
        assert_eq!(body(&s, "D").as_deref(), Some("<p>newer local</p>"));
    }

    #[test]
    fn an_undecodable_outbox_row_does_not_stop_a_fetched_write() {
        let s = store();
        s.put_body("M", &html("<p>old</p>")).unwrap();
        s.put_body("D", &html("<p>mine</p>")).unwrap();
        s.conn
            .execute(
                "INSERT INTO outbox (op, payload) VALUES ('futureOp', 'not json')",
                [],
            )
            .unwrap();
        s.enqueue_op(&OutboxOp::SaveDraft { id: "D".into() })
            .unwrap();

        assert_eq!(fetch_both(&s, "M", "<p>fetched</p>"), (true, true));
        assert_eq!(body(&s, "M").as_deref(), Some("<p>fetched</p>"));
        // The draft op after the bad row still protects its draft.
        assert_eq!(fetch_both(&s, "D", "<p>quote</p>"), (false, false));
        assert_eq!(body(&s, "D").as_deref(), Some("<p>mine</p>"));
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
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: Vec::new(),
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
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

        s.set_head_link("F1", "HEAD", false).unwrap();
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
        s.set_head_link("F1", "HEAD", false).unwrap();
        s.set_backfill_progress("F1", "PAGE3", 100, SCOPE_ALL)
            .unwrap();
        s.complete_backfill("F1", "FULL", SCOPE_ALL, 120, None)
            .unwrap();
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
        s.set_head_link("F1", "HEAD", false).unwrap();
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
        s.set_head_link("F1", "HEAD", false).unwrap();
        s.set_backfill_progress("F1", "PAGE3", 100, SCOPE_ALL)
            .unwrap();
        s.complete_backfill("F2", "FULL", SCOPE_WINDOW, 10, None)
            .unwrap();
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
        s.set_head_link("F1", "HEAD", false).unwrap();
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
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: vec![],
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
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
        s.set_head_link("F1", "HEAD", false).unwrap();
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
        s.complete_backfill("F1", "FULL", SCOPE_WINDOW, 1, None)
            .unwrap();
        assert_eq!(history(&s, "F1").backfill_scope, None);
        s.set_backfill_progress("F1", "P2", 1, SCOPE_WINDOW)
            .unwrap();
        s.clear_delta_links().unwrap();
        assert_eq!(history(&s, "F1").backfill_scope, None);
    }
}

#[cfg(test)]
mod moved_id_tests {
    use super::*;
    use crate::graph::model::{Body, MailFolder, Message, Recipient};

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

    fn message(id: &str, subject: &str) -> Message {
        Message {
            id: id.into(),
            conversation_id: String::new(),
            subject: subject.into(),
            from: Recipient::default(),
            to: Vec::new(),
            cc: Vec::new(),
            received: "2026-01-01T00:00:00Z".into(),
            sent: String::new(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: String::new(),
            is_draft: false,
            is_meeting_request: false,
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: Vec::new(),
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
        }
    }

    /// A store with folders `SRC` and `DST` and one message `OLD1` in `SRC`.
    fn store_with(id: &str) -> Store {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "SRC");
        folder(&s, "DST");
        s.upsert_message("SRC", &message(id, "Quarterly zebra report"))
            .unwrap();
        s
    }

    fn ids(s: &Store) -> Vec<String> {
        let mut stmt = s
            .conn
            .prepare("SELECT id FROM messages ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    }

    fn aliases(s: &Store) -> Vec<(String, String)> {
        let mut stmt = s
            .conn
            .prepare("SELECT old_id, message_id FROM moved_ids ORDER BY old_id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn follow_move_rekeys_the_row_body_headers_and_attachments() {
        let s = store_with("OLD1");
        s.put_fetched_body_with_headers(
            "OLD1",
            &Body {
                content_type: "text".into(),
                content: "the giraffe minutes".into(),
            },
            BodyFormat::Text,
            &[MessageHeader {
                name: "x-ms-exchange-organization-authas".into(),
                value: "Internal".into(),
            }],
        )
        .unwrap();
        s.put_attachments(
            "OLD1",
            &[AttachmentMeta {
                id: "a1".into(),
                name: "a.pdf".into(),
                content_type: "application/pdf".into(),
                size: 3,
                is_inline: false,
                content_id: None,
                kind: AttachmentKind::File,
                source_url: None,
            }],
        )
        .unwrap();
        s.move_message("OLD1", "DST").unwrap();

        assert!(s.follow_move("OLD1", "NEW1").unwrap());

        assert_eq!(ids(&s), ["NEW1"]);
        let row = &s.messages_in_folder("DST", 10, 0).unwrap()[0];
        assert_eq!(row.id, "NEW1");
        assert_eq!(row.subject, "Quarterly zebra report");
        assert_eq!(
            s.get_body("NEW1").unwrap().unwrap().content,
            "the giraffe minutes"
        );
        assert!(s.get_body("OLD1").unwrap().is_none());
        assert_eq!(
            s.message_header("NEW1", "x-ms-exchange-organization-authas")
                .unwrap()
                .as_deref(),
            Some("Internal")
        );
        assert_eq!(s.attachments("NEW1").unwrap().len(), 1);
        assert!(s.attachments("OLD1").unwrap().is_empty());
        // The index follows the row: subject and body both find it.
        for q in ["zebra", "giraffe"] {
            let hits = s.search(q, 10).unwrap();
            assert_eq!(hits.len(), 1, "{q}");
            assert_eq!(hits[0].id, "NEW1", "{q}");
        }
        assert_eq!(s.current_id("OLD1").unwrap(), "NEW1");
        assert_eq!(s.current_id("NEW1").unwrap(), "NEW1");
        assert!(s.is_moved_alias("OLD1").unwrap());
        assert!(!s.is_moved_alias("NEW1").unwrap());
    }

    #[test]
    fn follow_move_does_nothing_for_the_same_or_an_empty_id() {
        let s = store_with("OLD1");
        assert!(!s.follow_move("OLD1", "OLD1").unwrap());
        assert!(!s.follow_move("OLD1", "").unwrap());
        assert_eq!(ids(&s), ["OLD1"]);
        assert!(aliases(&s).is_empty());
    }

    #[test]
    fn follow_move_of_a_message_no_longer_stored_still_records_the_alias() {
        // Deleted locally while its move was in flight: the queued (or a
        // later) `Delete` by the old id must reach the id Graph knows now.
        let s = store_with("OLD1");
        s.delete_message("OLD1").unwrap();
        assert!(s.follow_move("OLD1", "NEW1").unwrap());
        assert!(ids(&s).is_empty());
        assert_eq!(s.current_id("OLD1").unwrap(), "NEW1");
        assert!(s.is_moved_alias("OLD1").unwrap());
        // A chain through it still flattens.
        assert!(s.follow_move("NEW1", "NEW2").unwrap());
        assert_eq!(s.current_id("OLD1").unwrap(), "NEW2");
    }

    #[test]
    fn follow_move_chain_resolves_every_old_id_to_the_newest() {
        let s = store_with("OLD1");
        s.follow_move("OLD1", "NEW1").unwrap();
        s.follow_move("NEW1", "NEW2").unwrap();
        assert_eq!(ids(&s), ["NEW2"]);
        assert_eq!(s.current_id("OLD1").unwrap(), "NEW2");
        assert_eq!(s.current_id("NEW1").unwrap(), "NEW2");
        assert_eq!(
            aliases(&s),
            [
                ("NEW1".to_string(), "NEW2".to_string()),
                ("OLD1".to_string(), "NEW2".to_string())
            ]
        );
    }

    #[test]
    fn follow_move_onto_an_existing_row_keeps_one_row() {
        let s = store_with("OLD1");
        s.follow_move("OLD1", "NEW1").unwrap();
        // A delta stored the moved copy before the re-key.
        s.upsert_message("DST", &message("NEW2", "From the delta"))
            .unwrap();
        s.follow_move("NEW1", "NEW2").unwrap();
        assert_eq!(ids(&s), ["NEW2"]);
        assert_eq!(
            s.messages_in_folder("DST", 10, 0).unwrap()[0].subject,
            "From the delta"
        );
        // The chain was flattened before the old row went.
        assert_eq!(s.current_id("OLD1").unwrap(), "NEW2");
        assert_eq!(s.current_id("NEW1").unwrap(), "NEW2");
    }

    #[test]
    fn follow_move_onto_an_existing_row_keeps_the_drafts_own_state() {
        // A reconciled draft with a body, a header, an attachment and an
        // outbound attachment; a delta stored the moved copy first, with
        // a header of its own.
        let s = Store::open_in_memory().unwrap();
        let local = s
            .create_local_draft("Subj", "a@x", "", "<p>draft</p>")
            .unwrap();
        s.reconcile_id(&local, "G1").unwrap();
        s.add_outbound_attachment(&local, "/tmp/a.pdf", "a.pdf", 1)
            .unwrap();
        s.put_attachments(
            "G1",
            &[AttachmentMeta {
                id: "a1".into(),
                name: "a.pdf".into(),
                content_type: "application/pdf".into(),
                size: 1,
                is_inline: false,
                content_id: None,
                kind: AttachmentKind::File,
                source_url: None,
            }],
        )
        .unwrap();
        let folder_id = s.draft("G1").unwrap().unwrap().0.folder_id;
        s.upsert_message(&folder_id, &message("G2", "From the delta"))
            .unwrap();

        assert!(s.follow_move("G1", "G2").unwrap());

        assert_eq!(ids(&s), ["G2"]);
        assert_eq!(s.current_id(&local).unwrap(), "G2");
        let (row, body) = s.draft(&local).unwrap().expect("local id still loads");
        assert_eq!(row.id, "G2");
        assert_eq!(body.content, "<p>draft</p>");
        assert_eq!(s.outbound_attachments("G2").unwrap().len(), 1);
        assert_eq!(s.attachments("G2").unwrap().len(), 1);
        let orphans: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM outbound_attachments WHERE draft_id = 'G1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn follow_move_onto_an_existing_row_keeps_that_rows_own_body() {
        let s = store_with("OLD1");
        s.put_body(
            "OLD1",
            &Body {
                content_type: "text".into(),
                content: "old".into(),
            },
        )
        .unwrap();
        s.upsert_message("DST", &message("NEW1", "From the delta"))
            .unwrap();
        s.put_body(
            "NEW1",
            &Body {
                content_type: "text".into(),
                content: "new".into(),
            },
        )
        .unwrap();
        assert!(s.follow_move("OLD1", "NEW1").unwrap());
        assert_eq!(ids(&s), ["NEW1"]);
        assert_eq!(s.get_body("NEW1").unwrap().unwrap().content, "new");
        let bodies: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM bodies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bodies, 1);
    }

    #[test]
    fn follow_move_back_to_an_earlier_id_drops_that_alias() {
        let s = store_with("OLD1");
        s.follow_move("OLD1", "NEW1").unwrap();
        s.follow_move("NEW1", "OLD1").unwrap();
        assert_eq!(ids(&s), ["OLD1"]);
        assert_eq!(s.current_id("NEW1").unwrap(), "OLD1");
        assert_eq!(s.current_id("OLD1").unwrap(), "OLD1");
        assert!(!s.is_moved_alias("OLD1").unwrap());
        assert_eq!(aliases(&s), [("NEW1".to_string(), "OLD1".to_string())]);
        // A delta upsert of the live id merges into its row.
        s.upsert_message("SRC", &message("OLD1", "Merged")).unwrap();
        assert_eq!(
            s.messages_in_folder("SRC", 10, 0).unwrap()[0].subject,
            "Merged"
        );
    }

    #[test]
    fn follow_move_of_a_reconciled_draft_keeps_its_local_id() {
        let s = Store::open_in_memory().unwrap();
        let local = s
            .create_local_draft("Subj", "a@x", "", "<p>draft</p>")
            .unwrap();
        s.reconcile_id(&local, "G1").unwrap();
        s.add_outbound_attachment(&local, "/tmp/a.pdf", "a.pdf", 1)
            .unwrap();
        s.follow_move("G1", "G2").unwrap();
        assert_eq!(s.current_id(&local).unwrap(), "G2");
        assert_eq!(s.current_id("G1").unwrap(), "G2");
        let (row, body) = s.draft(&local).unwrap().expect("local id still loads");
        assert_eq!(row.id, "G2");
        assert_eq!(body.content, "<p>draft</p>");
        assert_eq!(s.outbound_attachments("G2").unwrap().len(), 1);
    }

    #[test]
    fn delete_message_by_a_moved_alias_deletes_nothing() {
        // The source folder's delta `@removed` of the pre-move id.
        let s = store_with("OLD1");
        s.follow_move("OLD1", "NEW1").unwrap();
        s.delete_message("OLD1").unwrap();
        assert_eq!(s.delete_message_resolved("OLD1").unwrap(), None);
        assert_eq!(ids(&s), ["NEW1"]);
    }

    #[test]
    fn moved_aliases_outlive_a_deleted_message() {
        let s = store_with("OLD1");
        s.follow_move("OLD1", "NEW1").unwrap();
        s.follow_move("NEW1", "NEW2").unwrap();
        s.delete_message("NEW2").unwrap();
        assert!(ids(&s).is_empty());
        assert_eq!(aliases(&s).len(), 2);
        assert_eq!(s.current_id("OLD1").unwrap(), "NEW2");
        assert_eq!(s.current_id("NEW1").unwrap(), "NEW2");
        assert!(s.is_moved_alias("OLD1").unwrap());
    }

    #[test]
    fn pending_message_ids_resolves_moved_ids() {
        let s = store_with("OLD1");
        s.upsert_message("SRC", &message("OTHER", "x")).unwrap();
        s.enqueue_op(&OutboxOp::MarkRead {
            id: "OLD1".into(),
            read: true,
        })
        .unwrap();
        s.enqueue_op(&OutboxOp::SetFlag {
            id: "OTHER".into(),
            flagged: true,
        })
        .unwrap();
        s.follow_move("OLD1", "NEW1").unwrap();
        s.enqueue_op(&OutboxOp::Move {
            id: "NEW1".into(),
            dest: "SRC".into(),
        })
        .unwrap();
        assert_eq!(s.pending_message_ids().unwrap(), ["NEW1", "OTHER"]);
    }
}

#[cfg(test)]
mod message_reminder_tests {
    //! A flagged message's reminder (#2054): its columns, the migration,
    //! the flag invariant, the due query and the outbox ops.
    use super::*;

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

    /// A flagged message with a reminder, set for `at` and signalling at
    /// `signal`.
    fn reminded(id: &str, at: &str, signal: &str) -> Message {
        Message {
            id: id.into(),
            conversation_id: format!("C{id}"),
            subject: format!("subject {id}"),
            from: Recipient {
                name: "A".into(),
                address: "a@x".into(),
            },
            to: vec![],
            cc: vec![],
            received: "2026-11-01T00:00:00Z".into(),
            sent: String::new(),
            is_read: false,
            is_flagged: true,
            has_attachments: false,
            importance: "normal".into(),
            preview: "p".into(),
            is_draft: false,
            is_meeting_request: false,
            meeting_message_type: None,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: vec![],
            is_reminder_on: true,
            reminder_at: at.into(),
            reminder_signal_at: signal.into(),
            flag_due_at: "2026-11-09T17:00:00Z".into(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
        }
    }

    fn row(s: &Store, id: &str) -> MessageRow {
        s.message_row(id).unwrap().unwrap()
    }

    /// The reminder columns of `r`.
    fn reminder(r: &MessageRow) -> (bool, &str, &str, &str) {
        (
            r.is_reminder_on,
            r.reminder_at.as_str(),
            r.reminder_signal_at.as_str(),
            r.flag_due_at.as_str(),
        )
    }

    const ON: (bool, &str, &str, &str) = (
        true,
        "2026-11-08T10:00:00Z",
        "2026-11-08T10:15:00Z",
        "2026-11-09T17:00:00Z",
    );

    #[test]
    fn message_reminder_columns_round_trip() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        let mut m = reminded("M1", "2026-11-08T10:00:00Z", "2026-11-08T10:15:00Z");
        m.is_draft = true;
        s.upsert_message("F", &m).unwrap();
        s.put_body(
            "M1",
            &Body {
                content_type: "text".into(),
                content: "b".into(),
            },
        )
        .unwrap();
        assert_eq!(reminder(&row(&s, "M1")), ON, "message_row");
        let in_folder = s.messages_in_folder("F", 50, 0).unwrap();
        assert_eq!(reminder(&in_folder[0]), ON, "messages_in_folder");
        let rows = s.message_rows(&["M1"]).unwrap();
        assert_eq!(reminder(&rows[0]), ON, "message_rows");
        let hits = s.search("subject", 10).unwrap();
        assert_eq!(reminder(&hits[0]), ON, "search");
        let conv = s.conversations_in_folder("F", 50, 0).unwrap();
        assert_eq!(reminder(&conv[0]), ON, "conversations_in_folder");
        let (draft, body) = s.draft("M1").unwrap().unwrap();
        assert_eq!(reminder(&draft), ON, "draft");
        assert_eq!(
            body.content, "b",
            "the body columns moved past the new ones"
        );
    }

    #[test]
    fn partial_merge_keeps_the_reminder_complete_merge_clears_it() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message(
            "F",
            &reminded("M1", "2026-11-08T10:00:00Z", "2026-11-08T10:15:00Z"),
        )
        .unwrap();
        // A partial item (`{id, isRead}`) carries no reminder: kept.
        let partial = crate::json::parse(r#"{"id":"M1","isRead":true}"#).unwrap();
        let m = Message::from_json(&partial).unwrap();
        s.merge_message("F", &m, &MessageFields::of(&partial))
            .unwrap();
        assert_eq!(reminder(&row(&s, "M1")), ON);
        // A partial item carrying the properties writes them.
        let partial = crate::json::parse(
            r#"{"id":"M1","singleValueExtendedProperties":[
                {"id":"Boolean {00062008-0000-0000-C000-000000000046} Id 0x8503","value":"true"},
                {"id":"SystemTime {00062008-0000-0000-C000-000000000046} Id 0x8560","value":"2026-11-08T11:00:00Z"}]}"#,
        )
        .unwrap();
        let m = Message::from_json(&partial).unwrap();
        s.merge_message("F", &m, &MessageFields::of(&partial))
            .unwrap();
        let r = row(&s, "M1");
        assert_eq!(
            reminder(&r),
            (true, "", "2026-11-08T11:00:00Z", "2026-11-09T17:00:00Z"),
            "the flag (and its due time) was not carried: kept"
        );
        // A complete item without the properties: no reminder.
        let complete = crate::json::parse(
            r#"{"id":"M1","subject":"s","from":{"emailAddress":{"address":"a@x"}},
                "receivedDateTime":"2026-11-01T00:00:00Z","flag":{"flagStatus":"flagged"}}"#,
        )
        .unwrap();
        let m = Message::from_json(&complete).unwrap();
        s.merge_message("F", &m, &MessageFields::of(&complete))
            .unwrap();
        assert_eq!(reminder(&row(&s, "M1")), (false, "", "", ""));
    }

    #[test]
    fn unflagged_delta_item_with_reminder_set_stays_off() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        // Exchange may keep `PidLidReminderSet` on a message it unflagged
        // (or completed): the row's reminder stays off.
        for status in ["notFlagged", "complete"] {
            let v = crate::json::parse(&format!(
                r#"{{"id":"M1","subject":"s","from":{{"emailAddress":{{"address":"a@x"}}}},
                    "receivedDateTime":"2026-11-01T00:00:00Z","flag":{{"flagStatus":"{status}"}},
                    "singleValueExtendedProperties":[
                    {{"id":"Boolean {{00062008-0000-0000-C000-000000000046}} Id 0x8503","value":"true"}},
                    {{"id":"SystemTime {{00062008-0000-0000-C000-000000000046}} Id 0x8560","value":"2026-11-08T10:15:00Z"}}]}}"#
            ))
            .unwrap();
            let m = Message::from_json(&v).unwrap();
            assert!(m.is_reminder_on, "the model reads what Graph sent");
            // Inserted, and over a flagged row with a reminder.
            s.merge_message("F", &m, &MessageFields::of(&v)).unwrap();
            let r = row(&s, "M1");
            assert!(!r.is_flagged && !r.is_reminder_on, "{status}");
            assert_eq!(r.reminder_signal_at, "2026-11-08T10:15:00Z", "times kept");
            s.upsert_message(
                "F",
                &reminded("M1", "2026-11-08T10:00:00Z", "2026-11-08T10:15:00Z"),
            )
            .unwrap();
            assert!(row(&s, "M1").is_reminder_on);
            s.merge_message("F", &m, &MessageFields::of(&v)).unwrap();
            assert!(!row(&s, "M1").is_reminder_on, "{status} over a flagged row");
        }
    }

    #[test]
    fn partial_reminder_props_on_an_unflagged_row_stay_off() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        let mut m = reminded("M1", "", "");
        m.is_flagged = false;
        m.is_reminder_on = false;
        s.upsert_message("F", &m).unwrap();
        // A partial item carrying only the reminder (no flag): the stored
        // flag, off, decides.
        let partial = crate::json::parse(
            r#"{"id":"M1","singleValueExtendedProperties":[
                {"id":"Boolean {00062008-0000-0000-C000-000000000046} Id 0x8503","value":"true"},
                {"id":"SystemTime {00062008-0000-0000-C000-000000000046} Id 0x8560","value":"2026-11-08T10:15:00Z"}]}"#,
        )
        .unwrap();
        let pm = Message::from_json(&partial).unwrap();
        s.merge_message("F", &pm, &MessageFields::of(&partial))
            .unwrap();
        let r = row(&s, "M1");
        assert!(!r.is_reminder_on);
        assert_eq!(r.reminder_signal_at, "2026-11-08T10:15:00Z");
        // Once flagged (a partial item carrying the flag alone), the stored
        // reminder decides: its time is there, but `is_reminder_on` was
        // stored off, so it stays off until Graph says it is on.
        let flag = crate::json::parse(r#"{"id":"M1","flag":{"flagStatus":"flagged"}}"#).unwrap();
        let fm = Message::from_json(&flag).unwrap();
        s.merge_message("F", &fm, &MessageFields::of(&flag))
            .unwrap();
        let r = row(&s, "M1");
        assert!(r.is_flagged && !r.is_reminder_on);
    }

    #[test]
    fn set_flag_false_clears_the_reminder() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message(
            "F",
            &reminded("M1", "2026-11-08T10:00:00Z", "2026-11-08T10:15:00Z"),
        )
        .unwrap();
        s.set_flag("M1", true);
        assert_eq!(
            reminder(&row(&s, "M1")),
            ON,
            "flagging again changes nothing"
        );
        s.set_flag("M1", false);
        let r = row(&s, "M1");
        assert!(!r.is_flagged);
        assert_eq!(
            reminder(&r),
            (
                false,
                "2026-11-08T10:00:00Z",
                "2026-11-08T10:15:00Z",
                "2026-11-09T17:00:00Z"
            ),
            "off, its times kept"
        );
        s.set_flag("M1", true);
        assert!(
            !row(&s, "M1").is_reminder_on,
            "a re-flag does not turn it on"
        );
    }

    #[test]
    fn dismiss_and_snooze_write_the_row() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message(
            "F",
            &reminded("M1", "2026-11-08T10:00:00Z", "2026-11-08T10:15:00Z"),
        )
        .unwrap();
        s.snooze_message_reminder("M1", "2026-11-08T11:00:00Z");
        let r = row(&s, "M1");
        assert_eq!(
            (
                r.is_reminder_on,
                r.reminder_at.as_str(),
                r.reminder_signal_at.as_str()
            ),
            (true, "2026-11-08T10:00:00Z", "2026-11-08T11:00:00Z"),
            "the snooze moves the signal only"
        );
        s.dismiss_message_reminder("M1");
        assert!(!row(&s, "M1").is_reminder_on);
        // An unknown id is no error.
        s.dismiss_message_reminder("NOPE");
        s.snooze_message_reminder("NOPE", "2026-11-08T11:00:00Z");
    }

    #[test]
    fn due_message_reminders_returns_only_flagged_due_reminders_across_folders() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        folder(&s, "G");
        let now = "2026-11-08T12:00:00Z";
        // Due: a signal in the past, in two folders; a signal exactly now;
        // no signal time but a due reminder time.
        s.upsert_message(
            "F",
            &reminded("A", "2026-11-08T09:00:00Z", "2026-11-08T11:00:00Z"),
        )
        .unwrap();
        s.upsert_message(
            "G",
            &reminded("B", "2026-11-08T08:00:00Z", "2026-11-08T10:00:00Z"),
        )
        .unwrap();
        s.upsert_message("F", &reminded("C", "2026-11-08T07:00:00Z", now))
            .unwrap();
        s.upsert_message("G", &reminded("D", "2026-11-08T11:30:00Z", ""))
            .unwrap();
        // Not due: a snooze past now (the reminder time alone is past), and
        // Outlook's "never" sentinel.
        s.upsert_message(
            "F",
            &reminded("E", "2026-11-08T09:00:00Z", "2026-11-08T12:00:01Z"),
        )
        .unwrap();
        s.upsert_message(
            "F",
            &reminded("N", "2026-11-08T09:00:00Z", "4501-01-01T00:00:00Z"),
        )
        .unwrap();
        // Not due: the reminder off, no time at all, unflagged with a
        // reminder set (stored directly, as an old row might be).
        let mut off = reminded("O", "2026-11-08T09:00:00Z", "2026-11-08T09:00:00Z");
        off.is_reminder_on = false;
        s.upsert_message("F", &off).unwrap();
        s.upsert_message("F", &reminded("T", "", "")).unwrap();
        s.upsert_message(
            "G",
            &reminded("U", "2026-11-08T09:00:00Z", "2026-11-08T09:00:00Z"),
        )
        .unwrap();
        s.conn
            .execute(
                "UPDATE messages SET is_flagged = 0, is_reminder_on = 1 WHERE id = 'U'",
                [],
            )
            .unwrap();
        let due: Vec<String> = s
            .due_message_reminders(now)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(due, ["B", "A", "D", "C"], "soonest first, across folders");
        // `now` is normalised like the stored times.
        let due = s
            .due_message_reminders("2026-11-08T12:00:00.0000000")
            .unwrap();
        assert_eq!(due.len(), 4);
        assert!(
            s.due_message_reminders("2026-11-08T09:59:59Z")
                .unwrap()
                .is_empty()
        );
        // A `now` that is not a UTC time is the caller's bug: an error, not
        // an empty list (an offset would be dropped without being applied).
        for bad in ["2026-11-08T12:00:00+02:00", "garbage", ""] {
            assert!(
                matches!(s.due_message_reminders(bad), Err(StoreError::Invalid(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_merge_read_without_the_reminder_keeps_it() {
        // `reminder_unasked` (#2054: Graph refused the expand): a complete
        // item without the key keeps the stored reminder; one with it writes.
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message(
            "F",
            &reminded("M1", "2026-11-08T10:00:00Z", "2026-11-08T10:15:00Z"),
        )
        .unwrap();
        let complete = crate::json::parse(
            r#"{"id":"M1","subject":"new","from":{"emailAddress":{"address":"a@x"}},
                "receivedDateTime":"2026-11-01T00:00:00Z","flag":{"flagStatus":"flagged"}}"#,
        )
        .unwrap();
        let m = Message::from_json(&complete).unwrap();
        let mut fields = MessageFields::of(&complete);
        fields.reminder_unasked = true;
        s.merge_message("F", &m, &fields).unwrap();
        let r = row(&s, "M1");
        assert_eq!(r.subject, "new");
        assert_eq!(
            (
                r.is_reminder_on,
                r.reminder_at.as_str(),
                r.reminder_signal_at.as_str()
            ),
            (true, "2026-11-08T10:00:00Z", "2026-11-08T10:15:00Z")
        );
        assert_eq!(r.flag_due_at, "", "the due time still follows the flag");
        // An unasked item that carries the key writes it.
        let carried = crate::json::parse(
            r#"{"id":"M1","subject":"new","from":{"emailAddress":{"address":"a@x"}},
                "receivedDateTime":"2026-11-01T00:00:00Z","flag":{"flagStatus":"flagged"},
                "singleValueExtendedProperties":[]}"#,
        )
        .unwrap();
        let mut fields = MessageFields::of(&carried);
        fields.reminder_unasked = true;
        assert!(fields.writes_reminder());
        // The flag still rules: unflagged, the kept reminder is off.
        let unflagged = crate::json::parse(
            r#"{"id":"M1","subject":"new","from":{"emailAddress":{"address":"a@x"}},
                "receivedDateTime":"2026-11-01T00:00:00Z","flag":{"flagStatus":"notFlagged"}}"#,
        )
        .unwrap();
        let mut fields = MessageFields::of(&unflagged);
        fields.reminder_unasked = true;
        s.merge_message("F", &Message::from_json(&unflagged).unwrap(), &fields)
            .unwrap();
        assert!(!row(&s, "M1").is_reminder_on);
    }

    #[test]
    fn the_expand_refused_mark_goes_with_the_links() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        folder(&s, "G");
        assert!(!s.reminder_expand_refused("F").unwrap());
        assert!(!s.reminder_expand_refused("NOPE").unwrap());
        s.set_reminder_expand_refused("F", true).unwrap();
        assert!(s.reminder_expand_refused("F").unwrap());
        assert!(!s.reminder_expand_refused("G").unwrap());
        // A round's new link (minted from the same walk) keeps it.
        s.set_delta_link("F", "LINK").unwrap();
        assert!(s.reminder_expand_refused("F").unwrap());
        s.set_reminder_expand_refused("F", false).unwrap();
        assert!(!s.reminder_expand_refused("F").unwrap());
        // Clearing the links clears both marks.
        s.set_reminder_expand_refused("F", true).unwrap();
        s.set_backfill_expand_refused("F").unwrap();
        s.clear_delta_links().unwrap();
        assert!(!s.reminder_expand_refused("F").unwrap());
        assert!(!s.backfill_expand_refused("F").unwrap());
    }

    #[test]
    fn a_link_and_its_expand_refused_mark_are_written_together() {
        // #2054 r2 (the Windows race): the head's and a small folder's link
        // carry their mark in the same statement, so no reader sees one
        // without the other.
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        folder(&s, "G");
        s.set_head_link("F", "HEAD", true).unwrap();
        assert_eq!(s.get_delta_link("F").unwrap().as_deref(), Some("HEAD"));
        assert!(s.reminder_expand_refused("F").unwrap());
        // An accepted head over a refused one clears the mark.
        s.set_head_link("F", "HEAD2", false).unwrap();
        assert!(!s.reminder_expand_refused("F").unwrap());

        // A small folder walked whole: the caller's mark wins over the
        // (clear) backfill record, both ways.
        s.complete_backfill("G", "FULL", SCOPE_ALL, 3, Some(true))
            .unwrap();
        assert_eq!(s.get_delta_link("G").unwrap().as_deref(), Some("FULL"));
        assert!(s.reminder_expand_refused("G").unwrap());
        s.set_backfill_expand_refused("G").unwrap();
        s.complete_backfill("G", "FULL2", SCOPE_ALL, 3, Some(false))
            .unwrap();
        assert!(!s.reminder_expand_refused("G").unwrap());
        assert!(!s.backfill_expand_refused("G").unwrap(), "cleared");
    }

    #[test]
    fn the_backfill_expand_refusal_moves_with_its_link() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        assert!(!s.backfill_expand_refused("NOPE").unwrap());
        // A refused head, then a backfill whose first call was accepted:
        // its link replaces the head's, and so does its (clear) record.
        s.set_head_link("F", "HEAD", true).unwrap();
        s.set_backfill_progress("F", "PAGE2", 10, SCOPE_ALL)
            .unwrap();
        assert!(
            s.reminder_expand_refused("F").unwrap(),
            "the head's link is still stored"
        );
        s.complete_backfill("F", "FULL", SCOPE_ALL, 20, None)
            .unwrap();
        assert!(!s.reminder_expand_refused("F").unwrap());
        // The reverse: an accepted head, a refused backfill under way, then
        // its link stored.
        s.set_backfill_expand_refused("F").unwrap();
        assert!(!s.reminder_expand_refused("F").unwrap());
        assert!(s.backfill_expand_refused("F").unwrap());
        s.complete_backfill("F", "FULL2", SCOPE_ALL, 20, None)
            .unwrap();
        assert!(s.reminder_expand_refused("F").unwrap());
        assert!(!s.backfill_expand_refused("F").unwrap(), "moved");
        // `reset_backfill` drops the walk's record, and keeps the folder's.
        s.set_backfill_expand_refused("F").unwrap();
        s.reset_backfill("F").unwrap();
        assert!(!s.backfill_expand_refused("F").unwrap());
        assert!(s.reminder_expand_refused("F").unwrap());
    }

    #[test]
    fn due_message_reminders_uses_the_partial_index() {
        let s = Store::open_in_memory().unwrap();
        let plan: Vec<String> = s
            .conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                due_message_reminders_sql()
            ))
            .unwrap()
            .query_map(["2026-11-08T12:00:00Z"], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter().any(|p| p.contains("idx_messages_reminder")),
            "{plan:?}"
        );
    }

    #[test]
    fn messages_without_2054_columns_migrate_and_reset_delta_links() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-reminder-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        {
            let s = Store::open(&path).unwrap();
            folder(&s, "F");
            folder(&s, "G");
            s.upsert_message(
                "F",
                &reminded("M1", "2026-11-08T10:00:00Z", "2026-11-08T10:15:00Z"),
            )
            .unwrap();
            s.complete_backfill("F", "https://graph/F?$deltatoken=1", SCOPE_ALL, 7, None)
                .unwrap();
            s.set_head_link("G", "https://graph/G?$deltatoken=2", false)
                .unwrap();
            s.set_backfill_progress("G", "https://graph/G?$skiptoken=3", 4, SCOPE_ALL)
                .unwrap();
            // Back to a store from before #2054.
            s.conn
                .execute_batch(
                    "DROP INDEX idx_messages_reminder;
                     ALTER TABLE messages DROP COLUMN is_reminder_on;
                     ALTER TABLE messages DROP COLUMN reminder_at;
                     ALTER TABLE messages DROP COLUMN reminder_signal_at;
                     ALTER TABLE messages DROP COLUMN flag_due_at;
                     ALTER TABLE folders DROP COLUMN reminder_expand_refused;
                     ALTER TABLE folders DROP COLUMN backfill_expand_refused;",
                )
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        // The columns are there, empty, and no folder is marked.
        assert_eq!(reminder(&row(&s, "M1")), (false, "", "", ""));
        assert!(!s.reminder_expand_refused("F").unwrap());
        // Every folder's delta and backfill state is reset, with no
        // reconverge (nothing was quarantined).
        for f in ["F", "G"] {
            assert_eq!(s.get_delta_link(f).unwrap(), None, "{f}");
            let (backfill, synced, history, scope): (
                Option<String>,
                i64,
                Option<String>,
                Option<String>,
            ) = s
                .conn
                .query_row(
                    "SELECT backfill_link, backfill_synced, history_scope, backfill_scope
                     FROM folders WHERE id = ?1",
                    [f],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                (backfill, synced, history, scope),
                (None, 0, None, None),
                "{f}"
            );
        }
        assert!(!s.reconverge_pending().unwrap());
        // The index came with them.
        let indexed: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_messages_reminder'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1);
        // A link stored after the migration survives the next open.
        s.set_head_link("F", "https://graph/F?$deltatoken=new", false)
            .unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.get_delta_link("F").unwrap().as_deref(),
            Some("https://graph/F?$deltatoken=new")
        );
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_store_is_not_migrated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema::SCHEMA_SQL).unwrap();
        assert!(!migrate_message_reminders(&conn).unwrap());
    }

    #[test]
    fn outbox_op_message_reminder_round_trip() {
        let dismiss = OutboxOp::DismissMessageReminder { id: "M1".into() };
        assert_eq!(
            dismiss.to_json().to_string(),
            r#"{"kind":"dismissMessageReminder","id":"M1"}"#
        );
        let snooze = OutboxOp::SnoozeMessageReminder {
            id: "M1".into(),
            new_reminder_utc: "2026-11-08T10:15:00Z".into(),
        };
        assert_eq!(
            snooze.to_json().to_string(),
            r#"{"kind":"snoozeMessageReminder","id":"M1","newReminderUtc":"2026-11-08T10:15:00Z"}"#
        );
        for op in [dismiss, snooze] {
            let back = OutboxOp::from_json(&crate::json::parse(&op.to_json().to_string()).unwrap());
            assert_eq!(back, Some(op));
        }
        for raw in [
            r#"{"kind":"dismissMessageReminder"}"#,
            r#"{"kind":"snoozeMessageReminder","id":"M1"}"#,
            r#"{"kind":"snoozeMessageReminder","newReminderUtc":"2026-11-08T10:15:00Z"}"#,
        ] {
            assert_eq!(
                OutboxOp::from_json(&crate::json::parse(raw).unwrap()),
                None,
                "{raw}"
            );
        }
        // Through the queue too.
        let s = Store::open_in_memory().unwrap();
        s.enqueue_op(&OutboxOp::DismissMessageReminder { id: "M1".into() })
            .unwrap();
        assert_eq!(
            s.pending_ops().unwrap()[0].op,
            OutboxOp::DismissMessageReminder { id: "M1".into() }
        );
    }

    #[test]
    fn pending_message_ids_include_message_reminder_ops() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message("F", &reminded("NEW", "", "")).unwrap();
        s.follow_move("OLD", "NEW").unwrap();
        for op in [
            OutboxOp::DismissMessageReminder { id: "OLD".into() },
            OutboxOp::SnoozeMessageReminder {
                id: "M2".into(),
                new_reminder_utc: "2026-11-08T10:15:00Z".into(),
            },
            OutboxOp::DismissReminder { id: "EV".into() },
        ] {
            s.enqueue_op(&op).unwrap();
        }
        assert_eq!(s.pending_message_ids().unwrap(), ["M2", "NEW"]);
    }
}

#[cfg(test)]
mod meeting_message_type_tests {
    //! Each message's meeting message type on its row (#2081): the column
    //! through every row read, the merge rule, the migration and
    //! `set_meeting_message_type`.
    use super::*;
    use MeetingMessageType as T;

    const TYPES: [Option<MeetingMessageType>; 6] = [
        Some(T::Request),
        Some(T::Cancelled),
        Some(T::Accepted),
        Some(T::TentativelyAccepted),
        Some(T::Declined),
        None,
    ];

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

    fn msg(id: &str, t: Option<MeetingMessageType>) -> Message {
        Message {
            id: id.into(),
            conversation_id: format!("C{id}"),
            subject: format!("budget {id}"),
            from: Recipient {
                name: "A".into(),
                address: "a@x".into(),
            },
            to: vec![],
            cc: vec![],
            received: format!("2026-11-01T00:00:0{}Z", id.len() % 10),
            sent: String::new(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: "p".into(),
            is_draft: false,
            is_meeting_request: t == Some(T::Request),
            meeting_message_type: t,
            protection: Default::default(),
            meeting_reminder_on: false,
            categories: vec![],
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
        }
    }

    fn type_of(s: &Store, id: &str) -> Option<MeetingMessageType> {
        s.message_row(id).unwrap().unwrap().meeting_message_type
    }

    /// The ids `M0`..`M5`, one per entry of `TYPES`.
    fn ids() -> Vec<String> {
        (0..TYPES.len()).map(|i| format!("M{i}")).collect()
    }

    #[test]
    fn message_round_trips_meeting_message_type() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        for (id, t) in ids().iter().zip(TYPES) {
            s.upsert_message("F", &msg(id, t)).unwrap();
        }
        let want: Vec<(String, Option<MeetingMessageType>)> =
            ids().into_iter().zip(TYPES).collect();
        let sorted = |rows: Vec<MessageRow>| {
            let mut got: Vec<_> = rows
                .into_iter()
                .map(|r| (r.id, r.meeting_message_type))
                .collect();
            got.sort_by(|a, b| a.0.cmp(&b.0));
            got
        };
        // `message_row`.
        for (id, t) in &want {
            assert_eq!(type_of(&s, id), *t, "{id}");
        }
        // `message_rows`.
        let id_refs: Vec<&str> = want.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(sorted(s.message_rows(&id_refs).unwrap()), want);
        // The folder rows.
        assert_eq!(sorted(s.messages_in_folder("F", 50, 0).unwrap()), want);
        assert_eq!(sorted(s.conversations_in_folder("F", 50, 0).unwrap()), want);
        // `search` (the local full-text index).
        assert_eq!(sorted(s.search("budget", 50).unwrap()), want);
        // `draft`, whose body columns follow the row's.
        let mut d = msg("D1", Some(T::Cancelled));
        d.is_draft = true;
        s.upsert_message("F", &d).unwrap();
        s.put_body(
            "D1",
            &Body {
                content_type: "html".into(),
                content: "<p>hi</p>".into(),
            },
        )
        .unwrap();
        let (row, body) = s.draft("D1").unwrap().unwrap();
        assert_eq!(row.meeting_message_type, Some(T::Cancelled));
        assert_eq!(row.subject, "budget D1");
        assert_eq!(body.content_type, "html");
        assert_eq!(body.content, "<p>hi</p>");
    }

    #[test]
    fn due_message_reminders_carry_meeting_message_type() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        let mut m = msg("M1", Some(T::Request));
        m.is_flagged = true;
        m.is_reminder_on = true;
        m.reminder_at = "2026-11-08T10:00:00Z".into();
        s.upsert_message("F", &m).unwrap();
        let due = s.due_message_reminders("2026-11-08T12:00:00Z").unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].meeting_message_type, Some(T::Request));
    }

    #[test]
    fn an_unknown_stored_value_reads_none() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message("F", &msg("M1", None)).unwrap();
        s.conn
            .execute(
                "UPDATE messages SET meeting_message_type = 'meetingSomethingNew'",
                [],
            )
            .unwrap();
        assert_eq!(type_of(&s, "M1"), None);
    }

    #[test]
    fn partial_merge_keeps_meeting_message_type() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message("F", &msg("M1", Some(T::Cancelled)))
            .unwrap();
        // `{id, isRead}`: partial, carries no type.
        let read = Message {
            is_read: true,
            ..msg("M1", None)
        };
        let fields = MessageFields {
            is_read: true,
            ..MessageFields::default()
        };
        s.merge_message("F", &read, &fields).unwrap();
        assert_eq!(type_of(&s, "M1"), Some(T::Cancelled));
        assert!(s.message_row("M1").unwrap().unwrap().is_read);
        // A complete item read without the expand (`reminder_unasked`).
        let unasked = MessageFields {
            reminder: false,
            reminder_unasked: true,
            meeting_type: false,
            ..MessageFields::all()
        };
        s.merge_message("F", &msg("M1", None), &unasked).unwrap();
        assert_eq!(type_of(&s, "M1"), Some(T::Cancelled));
        // A complete item asked for it that came without the class.
        let bare = MessageFields {
            meeting_type: false,
            ..MessageFields::all()
        };
        s.merge_message("F", &msg("M1", None), &bare).unwrap();
        assert_eq!(type_of(&s, "M1"), Some(T::Cancelled));
        // An item that carries the class writes it, a plain one too.
        let carried = MessageFields::all();
        s.merge_message("F", &msg("M1", Some(T::Declined)), &carried)
            .unwrap();
        assert_eq!(type_of(&s, "M1"), Some(T::Declined));
        s.merge_message("F", &msg("M1", None), &carried).unwrap();
        assert_eq!(type_of(&s, "M1"), None);
        // A missing row is inserted with what the item carried.
        s.merge_message("F", &msg("M2", Some(T::Accepted)), &carried)
            .unwrap();
        assert_eq!(type_of(&s, "M2"), Some(T::Accepted));
    }

    #[test]
    fn set_meeting_message_type_follows_a_move_and_reports_a_change() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        folder(&s, "G");
        s.upsert_message("F", &msg("OLD", None)).unwrap();
        assert!(s.follow_move("OLD", "NEW").unwrap());
        s.conn
            .execute("UPDATE messages SET folder_id = 'G' WHERE id = 'NEW'", [])
            .unwrap();
        // The old id reaches the moved row; its folder is reported.
        assert_eq!(
            s.set_meeting_message_type("OLD", Some(T::Cancelled))
                .unwrap()
                .as_deref(),
            Some("G")
        );
        assert_eq!(type_of(&s, "NEW"), Some(T::Cancelled));
        // The same value again changes nothing.
        assert_eq!(
            s.set_meeting_message_type("NEW", Some(T::Cancelled))
                .unwrap(),
            None
        );
        // Another value does, and `None` clears it.
        assert_eq!(
            s.set_meeting_message_type("NEW", Some(T::Request))
                .unwrap()
                .as_deref(),
            Some("G")
        );
        assert_eq!(
            s.set_meeting_message_type("NEW", None).unwrap().as_deref(),
            Some("G")
        );
        assert_eq!(type_of(&s, "NEW"), None);
        // No such row.
        assert_eq!(
            s.set_meeting_message_type("NOPE", Some(T::Request))
                .unwrap(),
            None
        );
    }

    #[test]
    fn messages_without_2081_column_migrate_and_reset_delta_links() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-meeting-type-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        {
            let s = Store::open(&path).unwrap();
            folder(&s, "F");
            folder(&s, "G");
            s.upsert_message("F", &msg("M1", Some(T::Request))).unwrap();
            s.complete_backfill("F", "https://graph/F?$deltatoken=1", SCOPE_ALL, 7, None)
                .unwrap();
            s.set_head_link("G", "https://graph/G?$deltatoken=2", false)
                .unwrap();
            s.set_backfill_progress("G", "https://graph/G?$skiptoken=3", 4, SCOPE_ALL)
                .unwrap();
            s.set_reminder_expand_refused("G", true).unwrap();
            // Back to a store from before #2081.
            s.conn
                .execute_batch("ALTER TABLE messages DROP COLUMN meeting_message_type;")
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        // The column is there and the old row reads `None`, the rest of it
        // kept.
        let row = s.message_row("M1").unwrap().unwrap();
        assert_eq!(row.meeting_message_type, None);
        assert!(row.is_meeting_request);
        assert_eq!(row.subject, "budget M1");
        // Every folder's delta and backfill state is reset, the expand
        // refusals with it, with no reconverge.
        for f in ["F", "G"] {
            assert_eq!(s.get_delta_link(f).unwrap(), None, "{f}");
            let (backfill, synced, history, scope): (
                Option<String>,
                i64,
                Option<String>,
                Option<String>,
            ) = s
                .conn
                .query_row(
                    "SELECT backfill_link, backfill_synced, history_scope, backfill_scope
                     FROM folders WHERE id = ?1",
                    [f],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                (backfill, synced, history, scope),
                (None, 0, None, None),
                "{f}"
            );
            assert!(!s.reminder_expand_refused(f).unwrap(), "{f}");
            assert!(!s.backfill_expand_refused(f).unwrap(), "{f}");
        }
        assert!(!s.reconverge_pending().unwrap());
        // The type is written from now on, and a link stored after the
        // migration survives the next open.
        s.set_meeting_message_type("M1", Some(T::Request)).unwrap();
        s.set_head_link("F", "https://graph/F?$deltatoken=new", false)
            .unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.get_delta_link("F").unwrap().as_deref(),
            Some("https://graph/F?$deltatoken=new")
        );
        assert_eq!(type_of(&s, "M1"), Some(T::Request));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_store_from_before_2054_runs_every_migration_in_order() {
        // `migrate_meeting_message_type`, `migrate_message_protection`
        // (#2088) and `migrate_reply_links` (#2096) reset the expand-refused
        // marks that `migrate_message_reminders` adds, so a store without
        // any of them opens only with the reminders' run first.
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-pre-2054-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        {
            let s = Store::open(&path).unwrap();
            folder(&s, "F");
            folder(&s, "G");
            s.upsert_message("F", &msg("M1", Some(T::Request))).unwrap();
            s.complete_backfill("F", "https://graph/F?$deltatoken=1", SCOPE_ALL, 7, None)
                .unwrap();
            s.set_head_link("G", "https://graph/G?$deltatoken=2", false)
                .unwrap();
            s.set_backfill_progress("G", "https://graph/G?$skiptoken=3", 4, SCOPE_ALL)
                .unwrap();
            // Back to a store from before #2054 (and so before #2081,
            // #2088, #2089 and #2096).
            s.conn
                .execute_batch(
                    "DROP INDEX idx_messages_reminder;
                     DROP INDEX idx_messages_reply_drafts;
                     ALTER TABLE messages DROP COLUMN internet_message_id;
                     ALTER TABLE messages DROP COLUMN in_reply_to;
                     ALTER TABLE messages DROP COLUMN meeting_reminder_on;
                     ALTER TABLE messages DROP COLUMN is_reminder_on;
                     ALTER TABLE messages DROP COLUMN reminder_at;
                     ALTER TABLE messages DROP COLUMN reminder_signal_at;
                     ALTER TABLE messages DROP COLUMN flag_due_at;
                     ALTER TABLE messages DROP COLUMN meeting_message_type;
                     ALTER TABLE messages DROP COLUMN sensitivity;
                     ALTER TABLE messages DROP COLUMN has_sensitivity_label;
                     ALTER TABLE messages DROP COLUMN sensitivity_label_name;
                     ALTER TABLE messages DROP COLUMN is_rights_protected;
                     ALTER TABLE folders DROP COLUMN reminder_expand_refused;
                     ALTER TABLE folders DROP COLUMN backfill_expand_refused;",
                )
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        // Every migration's columns are there; the old row reads empty
        // reminder fields, no type, no protection, no meeting reminder and
        // no ids, the rest of it kept.
        let row = s.message_row("M1").unwrap().unwrap();
        assert_eq!(
            (row.internet_message_id.as_str(), row.in_reply_to.as_str()),
            ("", "")
        );
        assert_eq!(row.meeting_message_type, None);
        assert_eq!(row.protection, MessageProtection::default());
        assert!(!row.meeting_reminder_on);
        assert!(!row.is_reminder_on);
        assert_eq!(
            (
                row.reminder_at.as_str(),
                row.reminder_signal_at.as_str(),
                row.flag_due_at.as_str()
            ),
            ("", "", "")
        );
        assert_eq!(row.subject, "budget M1");
        // Every folder's delta and backfill state is reset.
        for f in ["F", "G"] {
            assert_eq!(s.get_delta_link(f).unwrap(), None, "{f}");
            let (backfill, synced, history, scope): (
                Option<String>,
                i64,
                Option<String>,
                Option<String>,
            ) = s
                .conn
                .query_row(
                    "SELECT backfill_link, backfill_synced, history_scope, backfill_scope
                     FROM folders WHERE id = ?1",
                    [f],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                (backfill, synced, history, scope),
                (None, 0, None, None),
                "{f}"
            );
            assert!(!s.reminder_expand_refused(f).unwrap(), "{f}");
            assert!(!s.backfill_expand_refused(f).unwrap(), "{f}");
        }
        assert!(!s.reconverge_pending().unwrap());
        // No migration runs again: a later open keeps a new link.
        assert!(!migrate_message_reminders(&s.conn).unwrap());
        assert!(!migrate_meeting_message_type(&s.conn).unwrap());
        assert!(!migrate_message_protection(&s.conn).unwrap());
        assert!(!migrate_meeting_reminder(&s.conn).unwrap());
        assert!(!migrate_reply_links(&s.conn).unwrap());
        s.set_head_link("F", "https://graph/F?$deltatoken=new", false)
            .unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.get_delta_link("F").unwrap().as_deref(),
            Some("https://graph/F?$deltatoken=new")
        );
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_store_has_the_column_and_is_not_migrated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema::SCHEMA_SQL).unwrap();
        assert!(!migrate_meeting_message_type(&conn).unwrap());
    }

    #[test]
    fn a_store_without_the_column_migrates_once() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema::SCHEMA_SQL).unwrap();
        conn.execute_batch("ALTER TABLE messages DROP COLUMN meeting_message_type;")
            .unwrap();
        assert!(migrate_meeting_message_type(&conn).unwrap());
        assert!(!migrate_meeting_message_type(&conn).unwrap());
    }
}

#[cfg(test)]
mod message_protection_tests {
    //! Each message's protection on its row (#2088): the columns through
    //! every row read, the merge rule, the migration.
    use super::*;

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

    fn msg(id: &str, p: MessageProtection) -> Message {
        Message {
            id: id.into(),
            conversation_id: format!("C{id}"),
            subject: format!("budget {id}"),
            from: Recipient {
                name: "A".into(),
                address: "a@x".into(),
            },
            to: vec![],
            cc: vec![],
            received: "2026-11-01T00:00:00Z".into(),
            sent: String::new(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: "p".into(),
            is_draft: false,
            is_meeting_request: false,
            meeting_message_type: None,
            protection: p,
            meeting_reminder_on: false,
            categories: vec![],
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
        }
    }

    /// One value per case: a named label, an unnamed one, each
    /// Sensitivity, IRM, all at once and none.
    fn cases() -> Vec<(String, MessageProtection)> {
        let p = |sensitivity, has_label, name: &str, irm| MessageProtection {
            sensitivity,
            has_label,
            label_name: name.into(),
            is_rights_protected: irm,
        };
        use Sensitivity as S;
        [
            p(S::Normal, true, "Confidential", false),
            p(S::Normal, true, "", false),
            p(S::Personal, false, "", false),
            p(S::Private, false, "", false),
            p(S::Confidential, false, "", false),
            p(S::Normal, false, "", true),
            p(S::Confidential, true, "Highly Confidential", true),
            MessageProtection::default(),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, p)| (format!("M{i}"), p))
        .collect()
    }

    fn protection_of(s: &Store, id: &str) -> MessageProtection {
        s.message_row(id).unwrap().unwrap().protection
    }

    fn labelled() -> MessageProtection {
        MessageProtection {
            sensitivity: Sensitivity::Confidential,
            has_label: true,
            label_name: "Confidential".into(),
            is_rights_protected: true,
        }
    }

    #[test]
    fn message_round_trips_protection_on_every_row_read() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        let want = cases();
        for (id, p) in &want {
            s.upsert_message("F", &msg(id, p.clone())).unwrap();
        }
        let sorted = |rows: Vec<MessageRow>| {
            let mut got: Vec<_> = rows.into_iter().map(|r| (r.id, r.protection)).collect();
            got.sort_by(|a, b| a.0.cmp(&b.0));
            got
        };
        // `message_row`.
        for (id, p) in &want {
            assert_eq!(&protection_of(&s, id), p, "{id}");
        }
        // `message_rows`.
        let id_refs: Vec<&str> = want.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(sorted(s.message_rows(&id_refs).unwrap()), want);
        // The folder rows.
        assert_eq!(sorted(s.messages_in_folder("F", 50, 0).unwrap()), want);
        assert_eq!(sorted(s.conversations_in_folder("F", 50, 0).unwrap()), want);
        // `search` (the local full-text index).
        assert_eq!(sorted(s.search("budget", 50).unwrap()), want);
        // `due_message_reminders`.
        let mut r = msg("R1", labelled());
        r.is_flagged = true;
        r.is_reminder_on = true;
        r.reminder_at = "2026-11-08T10:00:00Z".into();
        s.upsert_message("F", &r).unwrap();
        let due = s.due_message_reminders("2026-11-08T12:00:00Z").unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].protection, labelled());
        // `draft`, whose body columns follow the row's.
        let mut d = msg("D1", labelled());
        d.is_draft = true;
        s.upsert_message("F", &d).unwrap();
        s.put_body(
            "D1",
            &Body {
                content_type: "html".into(),
                content: "<p>hi</p>".into(),
            },
        )
        .unwrap();
        let (row, body) = s.draft("D1").unwrap().unwrap();
        assert_eq!(row.protection, labelled());
        assert_eq!(row.subject, "budget D1");
        assert_eq!(body.content_type, "html");
        assert_eq!(body.content, "<p>hi</p>");
    }

    #[test]
    fn an_unknown_stored_sensitivity_reads_normal() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message("F", &msg("M1", labelled())).unwrap();
        s.conn
            .execute("UPDATE messages SET sensitivity = 9", [])
            .unwrap();
        let p = protection_of(&s, "M1");
        assert_eq!(p.sensitivity, Sensitivity::Normal);
        assert!(p.has_label && p.is_rights_protected);
    }

    #[test]
    fn partial_merge_keeps_protection() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message("F", &msg("M1", labelled())).unwrap();
        // `{id, isRead}`: partial, carries no class.
        let read = Message {
            is_read: true,
            ..msg("M1", MessageProtection::default())
        };
        let fields = MessageFields {
            is_read: true,
            ..MessageFields::default()
        };
        s.merge_message("F", &read, &fields).unwrap();
        assert_eq!(protection_of(&s, "M1"), labelled());
        assert!(s.message_row("M1").unwrap().unwrap().is_read);
        // A complete item read without the expand (`reminder_unasked`).
        let unasked = MessageFields {
            reminder: false,
            reminder_unasked: true,
            meeting_type: false,
            protection: false,
            ..MessageFields::all()
        };
        s.merge_message("F", &msg("M1", MessageProtection::default()), &unasked)
            .unwrap();
        assert_eq!(protection_of(&s, "M1"), labelled());
        // A complete item whose expand came without the class.
        let bare = MessageFields {
            protection: false,
            ..MessageFields::all()
        };
        s.merge_message("F", &msg("M1", MessageProtection::default()), &bare)
            .unwrap();
        assert_eq!(protection_of(&s, "M1"), labelled());
        // An item that carries the class writes it, a plain one clears it.
        let carried = MessageFields::all();
        let irm = MessageProtection {
            is_rights_protected: true,
            ..MessageProtection::default()
        };
        s.merge_message("F", &msg("M1", irm.clone()), &carried)
            .unwrap();
        assert_eq!(protection_of(&s, "M1"), irm);
        s.merge_message("F", &msg("M1", MessageProtection::default()), &carried)
            .unwrap();
        assert_eq!(protection_of(&s, "M1"), MessageProtection::default());
        // A missing row is inserted with what the item carried.
        s.merge_message("F", &msg("M2", labelled()), &carried)
            .unwrap();
        assert_eq!(protection_of(&s, "M2"), labelled());
    }

    #[test]
    fn messages_without_2088_columns_migrate_and_reset_delta_links() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-protection-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        {
            let s = Store::open(&path).unwrap();
            folder(&s, "F");
            folder(&s, "G");
            let mut m = msg("M1", MessageProtection::default());
            m.meeting_message_type = Some(MeetingMessageType::Request);
            s.upsert_message("F", &m).unwrap();
            s.complete_backfill("F", "https://graph/F?$deltatoken=1", SCOPE_ALL, 7, None)
                .unwrap();
            s.set_head_link("G", "https://graph/G?$deltatoken=2", false)
                .unwrap();
            s.set_backfill_progress("G", "https://graph/G?$skiptoken=3", 4, SCOPE_ALL)
                .unwrap();
            s.set_reminder_expand_refused("G", true).unwrap();
            // Back to a store from before #2088 (after #2081).
            s.conn
                .execute_batch(
                    "ALTER TABLE messages DROP COLUMN sensitivity;
                     ALTER TABLE messages DROP COLUMN has_sensitivity_label;
                     ALTER TABLE messages DROP COLUMN sensitivity_label_name;
                     ALTER TABLE messages DROP COLUMN is_rights_protected;",
                )
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        // The columns are there and the old row reads unprotected, the
        // rest of it kept.
        let row = s.message_row("M1").unwrap().unwrap();
        assert_eq!(row.protection, MessageProtection::default());
        assert_eq!(row.meeting_message_type, Some(MeetingMessageType::Request));
        assert_eq!(row.subject, "budget M1");
        // Every folder's delta and backfill state is reset, the expand
        // refusals with it, with no reconverge.
        for f in ["F", "G"] {
            assert_eq!(s.get_delta_link(f).unwrap(), None, "{f}");
            let (backfill, synced, history, scope): (
                Option<String>,
                i64,
                Option<String>,
                Option<String>,
            ) = s
                .conn
                .query_row(
                    "SELECT backfill_link, backfill_synced, history_scope, backfill_scope
                     FROM folders WHERE id = ?1",
                    [f],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                (backfill, synced, history, scope),
                (None, 0, None, None),
                "{f}"
            );
            assert!(!s.reminder_expand_refused(f).unwrap(), "{f}");
            assert!(!s.backfill_expand_refused(f).unwrap(), "{f}");
        }
        assert!(!s.reconverge_pending().unwrap());
        // The protection is written from now on, and a link stored after
        // the migration survives the next open.
        s.upsert_message("F", &msg("M1", labelled())).unwrap();
        s.set_head_link("F", "https://graph/F?$deltatoken=new", false)
            .unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.get_delta_link("F").unwrap().as_deref(),
            Some("https://graph/F?$deltatoken=new")
        );
        assert_eq!(protection_of(&s, "M1"), labelled());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_store_has_the_columns_and_is_not_migrated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema::SCHEMA_SQL).unwrap();
        assert!(!migrate_message_protection(&conn).unwrap());
    }

    #[test]
    fn a_store_without_the_columns_migrates_once() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema::SCHEMA_SQL).unwrap();
        conn.execute_batch(
            "ALTER TABLE messages DROP COLUMN sensitivity;
             ALTER TABLE messages DROP COLUMN has_sensitivity_label;
             ALTER TABLE messages DROP COLUMN sensitivity_label_name;
             ALTER TABLE messages DROP COLUMN is_rights_protected;",
        )
        .unwrap();
        assert!(migrate_message_protection(&conn).unwrap());
        assert!(!migrate_message_protection(&conn).unwrap());
    }
}

#[cfg(test)]
mod meeting_reminder_tests {
    //! A meeting message's own reminder on its row (#2089): the column
    //! through every row read, the merge rule (no flag term), that it never
    //! fires, dismiss, and the migration without a delta reset.
    use super::*;

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

    fn msg(id: &str, on: bool) -> Message {
        Message {
            id: id.into(),
            conversation_id: format!("C{id}"),
            subject: format!("standup {id}"),
            from: Recipient {
                name: "A".into(),
                address: "a@x".into(),
            },
            to: vec![],
            cc: vec![],
            received: "2026-11-01T00:00:00Z".into(),
            sent: String::new(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: "p".into(),
            is_draft: false,
            is_meeting_request: true,
            meeting_message_type: Some(MeetingMessageType::Request),
            protection: Default::default(),
            meeting_reminder_on: on,
            categories: vec![],
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: String::new(),
            in_reply_to: String::new(),
        }
    }

    fn on(s: &Store, id: &str) -> bool {
        s.message_row(id).unwrap().unwrap().meeting_reminder_on
    }

    #[test]
    fn meeting_reminder_round_trips_on_every_row_read() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        let want = vec![("M0".to_string(), true), ("M1".to_string(), false)];
        for (id, v) in &want {
            s.upsert_message("F", &msg(id, *v)).unwrap();
        }
        let sorted = |rows: Vec<MessageRow>| {
            let mut got: Vec<_> = rows
                .into_iter()
                .map(|r| (r.id, r.meeting_reminder_on))
                .collect();
            got.sort_by(|a, b| a.0.cmp(&b.0));
            got
        };
        // `message_row`.
        for (id, v) in &want {
            assert_eq!(on(&s, id), *v, "{id}");
        }
        // `message_rows`.
        assert_eq!(sorted(s.message_rows(&["M0", "M1"]).unwrap()), want);
        // The folder rows.
        assert_eq!(sorted(s.messages_in_folder("F", 50, 0).unwrap()), want);
        assert_eq!(sorted(s.conversations_in_folder("F", 50, 0).unwrap()), want);
        // `search` (the local full-text index).
        assert_eq!(sorted(s.search("standup", 50).unwrap()), want);
        // `due_message_reminders`: a flagged meeting request whose flag
        // reminder is due carries its meeting reminder too.
        let mut r = msg("R1", true);
        r.is_flagged = true;
        r.is_reminder_on = true;
        r.reminder_at = "2026-11-08T10:00:00Z".into();
        s.upsert_message("F", &r).unwrap();
        let due = s.due_message_reminders("2026-11-08T12:00:00Z").unwrap();
        assert_eq!(due.len(), 1);
        assert!(due[0].meeting_reminder_on);
        // `draft`, whose body columns follow the row's.
        let mut d = msg("D1", true);
        d.is_draft = true;
        s.upsert_message("F", &d).unwrap();
        s.put_body(
            "D1",
            &Body {
                content_type: "html".into(),
                content: "<p>hi</p>".into(),
            },
        )
        .unwrap();
        let (row, body) = s.draft("D1").unwrap().unwrap();
        assert!(row.meeting_reminder_on);
        assert_eq!(row.subject, "standup D1");
        assert_eq!(body.content_type, "html");
        assert_eq!(body.content, "<p>hi</p>");
    }

    #[test]
    fn merge_writes_the_meeting_reminder_only_when_carried() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message("F", &msg("M1", true)).unwrap();
        // `{id, isRead}`: partial, carries no class.
        let read = Message {
            is_read: true,
            ..msg("M1", false)
        };
        let fields = MessageFields {
            is_read: true,
            ..MessageFields::default()
        };
        s.merge_message("F", &read, &fields).unwrap();
        assert!(on(&s, "M1"));
        assert!(s.message_row("M1").unwrap().unwrap().is_read);
        // A complete item read without the expand (`reminder_unasked`).
        let unasked = MessageFields {
            reminder: false,
            reminder_unasked: true,
            meeting_type: false,
            protection: false,
            meeting_reminder: false,
            ..MessageFields::all()
        };
        s.merge_message("F", &msg("M1", false), &unasked).unwrap();
        assert!(on(&s, "M1"));
        // A complete item whose expand came without the class.
        let bare = MessageFields {
            meeting_reminder: false,
            ..MessageFields::all()
        };
        s.merge_message("F", &msg("M1", false), &bare).unwrap();
        assert!(on(&s, "M1"));
        // A carried item writes it: off, then on again.
        let carried = MessageFields::all();
        s.merge_message("F", &msg("M1", false), &carried).unwrap();
        assert!(!on(&s, "M1"));
        s.merge_message("F", &msg("M1", true), &carried).unwrap();
        assert!(on(&s, "M1"));
        // A missing row is inserted with what the item carried, and the
        // fresh-delta merge writes it too.
        s.merge_message("F", &msg("M2", true), &carried).unwrap();
        assert!(on(&s, "M2"));
        assert!(
            s.merge_message_unless_edited("F", &msg("M2", false), &carried)
                .unwrap()
        );
        assert!(!on(&s, "M2"));
    }

    #[test]
    fn the_meeting_reminder_has_no_flag_term_and_never_fires() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        // Unflagged, its own reminder set: stored on, and the flag
        // reminder stays off whatever `is_reminder_on` the item had.
        let mut m = msg("M1", true);
        m.is_reminder_on = true;
        m.reminder_at = "2026-11-08T10:00:00Z".into();
        s.upsert_message("F", &m).unwrap();
        let row = s.message_row("M1").unwrap().unwrap();
        assert!(row.meeting_reminder_on);
        assert!(!row.is_reminder_on);
        assert!(
            s.due_message_reminders("2100-01-01T00:00:00Z")
                .unwrap()
                .is_empty()
        );
        // Clearing the flag leaves it alone (`set_flag`), as a snooze does.
        s.set_flag("M1", true);
        s.set_flag("M1", false);
        s.snooze_message_reminder("M1", "2026-11-09T10:00:00Z");
        assert!(on(&s, "M1"));
        // A flagged one with a due flag reminder is due exactly as before.
        let mut f = msg("M2", false);
        f.is_flagged = true;
        f.is_reminder_on = true;
        f.reminder_at = "2026-11-08T10:00:00Z".into();
        s.upsert_message("F", &f).unwrap();
        let due: Vec<_> = s
            .due_message_reminders("2100-01-01T00:00:00Z")
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(due, ["M2"]);
    }

    #[test]
    fn dismiss_turns_the_meeting_reminder_off() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message("F", &msg("M1", true)).unwrap();
        s.upsert_message("F", &msg("M2", true)).unwrap();
        s.dismiss_message_reminder("M1");
        assert!(!on(&s, "M1"));
        assert!(on(&s, "M2"));
    }

    #[test]
    fn messages_without_2089_column_migrate_without_a_delta_reset() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-meeting-reminder-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        {
            let s = Store::open(&path).unwrap();
            folder(&s, "F");
            folder(&s, "G");
            s.upsert_message("F", &msg("M1", true)).unwrap();
            s.complete_backfill("F", "https://graph/F?$deltatoken=1", SCOPE_ALL, 7, None)
                .unwrap();
            s.set_head_link("G", "https://graph/G?$deltatoken=2", false)
                .unwrap();
            s.set_backfill_progress("G", "https://graph/G?$skiptoken=3", 4, SCOPE_ALL)
                .unwrap();
            s.set_reminder_expand_refused("G", true).unwrap();
            // Back to a store from before #2089 (after #2088).
            s.conn
                .execute_batch("ALTER TABLE messages DROP COLUMN meeting_reminder_on;")
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        // The column is there and the old row reads false, the rest of it
        // kept.
        let row = s.message_row("M1").unwrap().unwrap();
        assert!(!row.meeting_reminder_on);
        assert_eq!(row.meeting_message_type, Some(MeetingMessageType::Request));
        assert_eq!(row.subject, "standup M1");
        // No folder's delta or backfill state is touched: the links
        // already carry the expand that brings the bit.
        assert_eq!(
            s.get_delta_link("F").unwrap().as_deref(),
            Some("https://graph/F?$deltatoken=1")
        );
        assert_eq!(
            s.get_delta_link("G").unwrap().as_deref(),
            Some("https://graph/G?$deltatoken=2")
        );
        let (link, synced): (Option<String>, i64) = s
            .conn
            .query_row(
                "SELECT backfill_link, backfill_synced FROM folders WHERE id = 'G'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (link.as_deref(), synced),
            (Some("https://graph/G?$skiptoken=3"), 4)
        );
        assert!(s.reminder_expand_refused("G").unwrap());
        assert!(!s.reconverge_pending().unwrap());
        // It is written from now on, and a second open is a no-op.
        s.upsert_message("F", &msg("M1", true)).unwrap();
        assert!(!migrate_meeting_reminder(&s.conn).unwrap());
        drop(s);
        let s = Store::open(&path).unwrap();
        assert!(on(&s, "M1"));
        assert_eq!(
            s.get_delta_link("F").unwrap().as_deref(),
            Some("https://graph/F?$deltatoken=1")
        );
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_store_has_the_column_and_is_not_migrated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema::SCHEMA_SQL).unwrap();
        assert!(!migrate_meeting_reminder(&conn).unwrap());
    }

    #[test]
    fn a_store_without_the_column_migrates_once() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema::SCHEMA_SQL).unwrap();
        conn.execute_batch("ALTER TABLE messages DROP COLUMN meeting_reminder_on;")
            .unwrap();
        assert!(migrate_meeting_reminder(&conn).unwrap());
        assert!(!migrate_meeting_reminder(&conn).unwrap());
    }
}

#[cfg(test)]
mod reply_link_tests {
    //! Which message a reply draft answers (#2096): each message's Internet
    //! message id and In-Reply-To through every row read, the merge rule (a
    //! stored id is never cleared), the migration with its delta reset,
    //! and `reply_draft_links` over its partial index.
    use super::*;

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

    fn msg(id: &str, conversation: &str, imid: &str, irt: &str) -> Message {
        Message {
            id: id.into(),
            conversation_id: conversation.into(),
            subject: format!("budget {id}"),
            from: Recipient {
                name: "A".into(),
                address: "a@x".into(),
            },
            to: vec![],
            cc: vec![],
            received: "2026-11-01T00:00:00Z".into(),
            sent: String::new(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: "p".into(),
            is_draft: false,
            is_meeting_request: false,
            meeting_message_type: None,
            protection: MessageProtection::default(),
            meeting_reminder_on: false,
            categories: vec![],
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
            internet_message_id: imid.into(),
            in_reply_to: irt.into(),
        }
    }

    fn draft(id: &str, conversation: &str, irt: &str) -> Message {
        Message {
            is_draft: true,
            ..msg(id, conversation, &format!("<{id}@x>"), irt)
        }
    }

    fn ids_of(s: &Store, id: &str) -> (String, String) {
        let row = s.message_row(id).unwrap().unwrap();
        (row.internet_message_id, row.in_reply_to)
    }

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn both_ids_round_trip_on_every_row_read() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        let want = [
            ("M1", "<m1@x>", ""),
            ("M2", "<m2@x>", "<m1@x>"),
            ("M3", "", ""),
        ];
        for (id, imid, irt) in want {
            s.upsert_message("F", &msg(id, &format!("C{id}"), imid, irt))
                .unwrap();
        }
        let want: Vec<(String, String, String)> = want
            .iter()
            .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
            .collect();
        let sorted = |rows: Vec<MessageRow>| {
            let mut got: Vec<_> = rows
                .into_iter()
                .map(|r| (r.id, r.internet_message_id, r.in_reply_to))
                .collect();
            got.sort();
            got
        };
        for (id, imid, irt) in &want {
            assert_eq!(ids_of(&s, id), (imid.clone(), irt.clone()), "{id}");
        }
        assert_eq!(sorted(s.message_rows(&["M1", "M2", "M3"]).unwrap()), want);
        assert_eq!(sorted(s.messages_in_folder("F", 50, 0).unwrap()), want);
        assert_eq!(sorted(s.conversations_in_folder("F", 50, 0).unwrap()), want);
        assert_eq!(sorted(s.search("budget", 50).unwrap()), want);
        // `due_message_reminders`.
        let mut r = msg("R1", "CR", "<r1@x>", "<m2@x>");
        r.is_flagged = true;
        r.is_reminder_on = true;
        r.reminder_at = "2026-11-08T10:00:00Z".into();
        s.upsert_message("F", &r).unwrap();
        let due = s.due_message_reminders("2026-11-08T12:00:00Z").unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(
            (
                due[0].internet_message_id.as_str(),
                due[0].in_reply_to.as_str()
            ),
            ("<r1@x>", "<m2@x>")
        );
        // `draft`, whose body columns follow the row's.
        s.upsert_message("F", &draft("D1", "C1", "<m1@x>")).unwrap();
        s.put_body(
            "D1",
            &Body {
                content_type: "html".into(),
                content: "<p>hi</p>".into(),
            },
        )
        .unwrap();
        let (row, body) = s.draft("D1").unwrap().unwrap();
        assert_eq!(
            (row.internet_message_id.as_str(), row.in_reply_to.as_str()),
            ("<D1@x>", "<m1@x>")
        );
        assert_eq!(row.subject, "budget D1");
        assert_eq!(body.content_type, "html");
        assert_eq!(body.content, "<p>hi</p>");
    }

    #[test]
    fn a_merge_never_clears_a_stored_id() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        s.upsert_message("F", &draft("D1", "C1", "<m1@x>")).unwrap();
        let kept = || pair("<D1@x>", "<m1@x>");
        // A partial item (`{id, isRead}`) carries neither.
        let fields = MessageFields {
            is_read: true,
            ..MessageFields::default()
        };
        s.merge_message("F", &msg("D1", "C1", "", ""), &fields)
            .unwrap();
        assert_eq!(ids_of(&s, "D1"), kept());
        // A complete item without the key or the property (read without the
        // `$expand`, a silently dropped one).
        let without = MessageFields {
            internet_message_id: false,
            in_reply_to: false,
            ..MessageFields::all()
        };
        s.merge_message("F", &draft("D1", "C1", ""), &without)
            .unwrap();
        assert_eq!(ids_of(&s, "D1"), kept());
        // An item carrying `""`, and a `Message` built in code
        // (`upsert_message`, every field carried), keep it too.
        s.merge_message("F", &msg("D1", "C1", "", ""), &MessageFields::all())
            .unwrap();
        assert_eq!(ids_of(&s, "D1"), kept());
        s.upsert_message("F", &msg("D1", "C1", "", "")).unwrap();
        assert_eq!(ids_of(&s, "D1"), kept());
        // A carried new value writes.
        s.upsert_message("F", &msg("D1", "C1", "<d1b@x>", "<m9@x>"))
            .unwrap();
        assert_eq!(ids_of(&s, "D1"), pair("<d1b@x>", "<m9@x>"));
        // A carried value the flags say was not carried does not.
        s.merge_message("F", &msg("D1", "C1", "<no@x>", "<no@x>"), &without)
            .unwrap();
        assert_eq!(ids_of(&s, "D1"), pair("<d1b@x>", "<m9@x>"));
        // A missing row is inserted with what the item has, even on flags
        // that say it carried nothing (an insert has nothing to keep).
        s.merge_message("F", &msg("M2", "C2", "<m2@x>", "<m1@x>"), &without)
            .unwrap();
        assert_eq!(ids_of(&s, "M2"), pair("<m2@x>", "<m1@x>"));
        s.merge_message("F", &msg("M3", "C3", "", ""), &MessageFields::all())
            .unwrap();
        assert_eq!(ids_of(&s, "M3"), pair("", ""));
    }

    #[test]
    fn reply_draft_links_name_unsent_reply_drafts_in_any_folder() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "Drafts");
        folder(&s, "Inbox");
        // Two reply drafts in Drafts, one in another folder, and a second
        // draft answering the same message (the pair comes once).
        s.upsert_message("Drafts", &draft("D1", "C1", "<m1@x>"))
            .unwrap();
        s.upsert_message("Drafts", &draft("D2", "C2", "<m2@x>"))
            .unwrap();
        s.upsert_message("Inbox", &draft("D3", "C3", "<m3@x>"))
            .unwrap();
        s.upsert_message("Drafts", &draft("D4", "C1", "<m1@x>"))
            .unwrap();
        // Left out: a forward draft (no In-Reply-To), a non-draft reply,
        // and a draft that was sent.
        s.upsert_message("Drafts", &draft("F1", "C1", "")).unwrap();
        s.upsert_message("Inbox", &msg("R1", "C1", "<r1@x>", "<m1@x>"))
            .unwrap();
        s.upsert_message("Drafts", &draft("S1", "C5", "<m5@x>"))
            .unwrap();
        s.mark_sent("S1");
        assert_eq!(
            s.reply_draft_links().unwrap(),
            [
                pair("C1", "<m1@x>"),
                pair("C2", "<m2@x>"),
                pair("C3", "<m3@x>"),
            ]
        );
        // A failed send makes it a draft again (#1874), and it is back.
        s.return_to_drafts("S1", "Drafts").unwrap();
        assert!(
            s.reply_draft_links()
                .unwrap()
                .contains(&pair("C5", "<m5@x>"))
        );
        assert!(
            Store::open_in_memory()
                .unwrap()
                .reply_draft_links()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_reply_draft_in_deleted_items_is_left_out() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "Drafts");
        folder(&s, "Archive");
        s.upsert_folder(&MailFolder {
            id: "Trash".into(),
            display_name: "Deleted Items".into(),
            parent_id: None,
            total_count: 0,
            unread_count: 0,
            well_known_name: Some("deleteditems".into()),
        })
        .unwrap();
        s.upsert_message("Drafts", &draft("D1", "C1", "<m1@x>"))
            .unwrap();
        s.upsert_message("Archive", &draft("D2", "C2", "<m2@x>"))
            .unwrap();
        s.upsert_message("Drafts", &draft("D3", "C3", "<m3@x>"))
            .unwrap();
        let all = [
            pair("C1", "<m1@x>"),
            pair("C2", "<m2@x>"),
            pair("C3", "<m3@x>"),
        ];
        assert_eq!(s.reply_draft_links().unwrap(), all);
        // D3 discarded: Graph moves it to Deleted Items, still a draft.
        s.move_message("D3", "Trash").unwrap();
        assert!(s.message_row("D3").unwrap().unwrap().is_draft);
        assert_eq!(
            s.reply_draft_links().unwrap(),
            [pair("C1", "<m1@x>"), pair("C2", "<m2@x>")]
        );
        // Restored to Drafts, it is back.
        s.move_message("D3", "Drafts").unwrap();
        assert_eq!(s.reply_draft_links().unwrap(), all);
    }

    #[test]
    fn reply_draft_links_uses_the_partial_index() {
        let s = Store::open_in_memory().unwrap();
        let plan: Vec<String> = s
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {REPLY_DRAFT_LINKS_SQL}"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        // The messages side reads the partial index (covering: every
        // column it needs is in it), never a scan of the table.
        assert!(
            plan.iter()
                .any(|p| p.contains("SCAN m USING COVERING INDEX idx_messages_reply_drafts")),
            "{plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|p| p.trim() == "SCAN m" || p.trim() == "SCAN messages"),
            "{plan:?}"
        );
    }

    #[test]
    fn messages_without_2096_columns_migrate_and_reset_delta_links() {
        let dir = std::env::temp_dir().join(format!(
            "lookxy-store-reply-links-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        let local_id;
        {
            let s = Store::open(&path).unwrap();
            folder(&s, "F");
            folder(&s, "G");
            let mut m = draft("M1", "C1", "<m0@x>");
            m.meeting_reminder_on = true;
            s.upsert_message("F", &m).unwrap();
            s.complete_backfill("F", "https://graph/F?$deltatoken=1", SCOPE_ALL, 7, None)
                .unwrap();
            s.set_head_link("G", "https://graph/G?$deltatoken=2", false)
                .unwrap();
            s.set_backfill_progress("G", "https://graph/G?$skiptoken=3", 4, SCOPE_ALL)
                .unwrap();
            s.set_reminder_expand_refused("G", true).unwrap();
            s.set_backfill_expand_refused("F").unwrap();
            // Bodies for the one-off fixes: pre-#1901 compose bodies (html
            // under `text`) on drafts become html; a pre-#1901 text body of
            // a draft (a rendering FetchBody stored with no format, or a
            // plain-text `createReply`) is marked fetched, and so is every
            // other row's NULL text body, in any folder (a draft made
            // elsewhere reads `is_draft = 0` before the re-enumeration,
            // wherever it is filed); left alone: html-as-text on a
            // non-draft (no relabel), a fetched body, a row holding an
            // unpushed edit, a `local:` draft.
            s.upsert_folder(&MailFolder {
                id: "DR".into(),
                display_name: "Drafts".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("drafts".into()),
            })
            .unwrap();
            s.upsert_message("F", &draft("L1", "CL", "")).unwrap();
            s.upsert_message("F", &draft("L2", "CL", "")).unwrap();
            s.upsert_message("F", &msg("L3", "CL", "", "")).unwrap();
            s.upsert_message("F", &draft("L4", "CL", "")).unwrap();
            s.upsert_message("F", &draft("L5", "CL", "")).unwrap();
            s.upsert_message("DR", &msg("L6", "CL", "", "")).unwrap();
            s.upsert_message("F", &draft("L7", "CL", "")).unwrap();
            s.upsert_message("F", &msg("L8", "CL", "", "")).unwrap();
            s.upsert_folder(&MailFolder {
                id: "TR".into(),
                display_name: "Deleted Items".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("deleteditems".into()),
            })
            .unwrap();
            // Drafts made elsewhere, stored before `isDraft`, outside
            // Drafts: in a user folder (G) and in Deleted Items.
            s.upsert_message("G", &msg("L9", "CL", "", "")).unwrap();
            s.upsert_message("TR", &msg("L10", "CL", "", "")).unwrap();
            let local = s.create_local_draft("s", "a@x", "", "x").unwrap();
            s.conn
                .execute(
                    "UPDATE bodies SET content_type = 'text', content = 'plain', fetched_as = NULL
                     WHERE message_id = ?1",
                    [&local],
                )
                .unwrap();
            s.conn
                .execute("UPDATE messages SET local_edit = 1 WHERE id = 'L7'", [])
                .unwrap();
            s.conn
                .execute_batch(
                    "INSERT INTO bodies (message_id, content_type, content, fetched_as) VALUES
                       ('L1', 'text', '<p>edited</p><ul><li>a</li></ul>', NULL),
                       ('L2', 'text', 'Hi

<p@x> wrote:', NULL),
                       ('L3', 'text', '<p>not a draft</p>', NULL),
                       ('L4', 'text', '<p>fetched</p>', 'text'),
                       ('L5', 'text', '<ol><li>one</li></ol>', NULL),
                       ('L6', 'text', 'rendered elsewhere', NULL),
                       ('L7', 'text', 'edited here', NULL),
                       ('L8', 'text', 'plain mail', NULL),
                       ('L9', 'text', 'rendered, user folder', NULL),
                       ('L10', 'text', 'rendered, deleted', NULL);",
                )
                .unwrap();
            local_id = local;
            // Back to a store from before #2096 (after #2089).
            s.conn
                .execute_batch(
                    "DROP INDEX idx_messages_reply_drafts;
                     ALTER TABLE messages DROP COLUMN internet_message_id;
                     ALTER TABLE messages DROP COLUMN in_reply_to;",
                )
                .unwrap();
        }
        let s = Store::open(&path).unwrap();
        // The columns are there and the old row reads `""`, the rest of it
        // kept; the index is made on the migrated table.
        let row = s.message_row("M1").unwrap().unwrap();
        assert_eq!(
            (row.internet_message_id.as_str(), row.in_reply_to.as_str()),
            ("", "")
        );
        assert!(row.meeting_reminder_on && row.is_draft);
        assert_eq!(row.subject, "budget M1");
        // The one-off body fixes: `(content_type, fetched_as, local)`.
        let body = |id: &str| -> (String, Option<String>, bool) {
            let (t, f) = s
                .conn
                .query_row(
                    "SELECT content_type, fetched_as FROM bodies WHERE message_id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            (t, f, s.draft_body_is_local(id).unwrap())
        };
        let b = |t: &str, f: Option<&str>, local: bool| (t.to_string(), f.map(String::from), local);
        let ids = [
            "L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10", &local_id,
        ];
        let got: Vec<_> = ids.iter().map(|id| body(id)).collect();
        assert_eq!(
            got,
            [
                b("html", None, true),          // L1: legacy compose html
                b("text", Some("text"), false), // L2: a draft's text, marked fetched
                b("text", Some("text"), false), // L3: not a draft, not relabelled
                b("text", Some("text"), false), // L4: already fetched
                b("html", None, true),          // L5: legacy compose list
                b("text", Some("text"), false), // L6: in Drafts, `is_draft = 0`
                b("text", None, true),          // L7: an unpushed edit, untouched
                b("text", Some("text"), false), // L8: plain mail, marked fetched
                b("text", Some("text"), false), // L9: a user folder
                b("text", Some("text"), false), // L10: Deleted Items
                b("text", None, true),          // a `local:` draft, untouched
            ],
            "{ids:?}"
        );
        // A non-draft's body behaves as before: a text ask gets it, an html
        // ask does not.
        assert!(s.get_body_for("L8", BodyFormat::Text).unwrap().is_some());
        assert!(s.get_body_for("L8", BodyFormat::Html).unwrap().is_none());
        let index: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'idx_messages_reply_drafts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index, 1);
        // Every folder's delta and backfill state is reset, the expand
        // refusals with it, with no reconverge.
        for f in ["F", "G"] {
            assert_eq!(s.get_delta_link(f).unwrap(), None, "{f}");
            let (backfill, synced, history, scope): (
                Option<String>,
                i64,
                Option<String>,
                Option<String>,
            ) = s
                .conn
                .query_row(
                    "SELECT backfill_link, backfill_synced, history_scope, backfill_scope
                     FROM folders WHERE id = ?1",
                    [f],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                (backfill, synced, history, scope),
                (None, 0, None, None),
                "{f}"
            );
            assert!(!s.reminder_expand_refused(f).unwrap(), "{f}");
            assert!(!s.backfill_expand_refused(f).unwrap(), "{f}");
        }
        assert!(!s.reconverge_pending().unwrap());
        // The ids are written from now on, and a link stored after the
        // migration survives the next open.
        s.upsert_message("F", &draft("M1", "C1", "<m0@x>")).unwrap();
        s.set_head_link("F", "https://graph/F?$deltatoken=new", false)
            .unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(
            s.get_delta_link("F").unwrap().as_deref(),
            Some("https://graph/F?$deltatoken=new")
        );
        assert_eq!(ids_of(&s, "M1"), pair("<M1@x>", "<m0@x>"));
        assert_eq!(s.reply_draft_links().unwrap(), [pair("C1", "<m0@x>")]);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_store_has_the_columns_and_is_not_migrated() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema::SCHEMA_SQL).unwrap();
        assert!(!migrate_reply_links(&conn).unwrap());
    }

    #[test]
    fn draft_body_is_local_only_for_a_body_written_here() {
        let s = Store::open_in_memory().unwrap();
        folder(&s, "F");
        let body = |t: &str| Body {
            content_type: t.into(),
            content: "x".into(),
        };
        // No row, and a row without a body.
        assert!(!s.draft_body_is_local("nope").unwrap());
        s.upsert_message("F", &draft("D1", "C1", "")).unwrap();
        assert!(!s.draft_body_is_local("D1").unwrap());
        // A body `FetchBody` stored (a draft made elsewhere), as text or
        // html.
        for (format, t) in [(BodyFormat::Text, "text"), (BodyFormat::Html, "html")] {
            s.put_fetched_body_with_headers("D1", &body(t), format, &[])
                .unwrap();
            assert!(!s.draft_body_is_local("D1").unwrap(), "{t}");
        }
        // A composed reply's `put_body`.
        s.upsert_message("F", &draft("D2", "C2", "")).unwrap();
        s.put_body("D2", &body("text")).unwrap();
        assert!(s.draft_body_is_local("D2").unwrap());
        // Compose's own writes, by the `local:` id and after `reconcile_id`
        // under both ids.
        let id = s.create_local_draft("s", "a@x", "", "<p>hi</p>").unwrap();
        assert!(s.draft_body_is_local(&id).unwrap());
        s.reconcile_id(&id, "G1").unwrap();
        assert!(s.draft_body_is_local(&id).unwrap());
        assert!(s.draft_body_is_local("G1").unwrap());
        // A fetched body, then compose's save over it: local again.
        s.update_draft_fields("D1", "s", "a@x", "", "", "<p>edited</p>")
            .unwrap();
        assert!(s.draft_body_is_local("D1").unwrap());
    }

    #[test]
    fn a_store_without_the_columns_migrates_once() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(schema::SCHEMA_SQL).unwrap();
        conn.execute_batch(
            "ALTER TABLE messages DROP COLUMN internet_message_id;
             ALTER TABLE messages DROP COLUMN in_reply_to;",
        )
        .unwrap();
        assert!(migrate_reply_links(&conn).unwrap());
        assert!(!migrate_reply_links(&conn).unwrap());
    }
}

/// Folder writes through the outbox (#1910).
#[cfg(test)]
mod folder_write_tests {
    use super::*;
    use crate::graph::model::MailFolder;

    fn listed(id: &str, parent: Option<&str>, name: &str) -> MailFolder {
        MailFolder {
            id: id.into(),
            display_name: name.into(),
            parent_id: parent.map(str::to_string),
            total_count: 0,
            unread_count: 0,
            well_known_name: None,
        }
    }

    /// A mailbox as a listing stores it: Inbox and Archive under ROOT, A
    /// under Inbox, B under A.
    fn mailbox() -> Store {
        let s = Store::open_in_memory().unwrap();
        for (id, parent) in [
            ("INBOX", "ROOT"),
            ("ARCH", "ROOT"),
            ("A", "INBOX"),
            ("B", "A"),
        ] {
            s.upsert_folder(&listed(id, Some(parent), id)).unwrap();
        }
        s
    }

    fn message(s: &Store, id: &str, folder: &str) {
        s.conn
            .execute(
                "INSERT INTO messages (id, folder_id) VALUES (?1, ?2)",
                params![id, folder],
            )
            .unwrap();
    }

    fn row(s: &Store, id: &str) -> Option<FolderRow> {
        s.folders().unwrap().into_iter().find(|f| f.id == id)
    }

    fn folder_of(s: &Store, message: &str) -> Option<String> {
        s.conn
            .query_row(
                "SELECT folder_id FROM messages WHERE id = ?1",
                params![message],
                |r| r.get(0),
            )
            .optional()
            .unwrap()
    }

    fn ops(s: &Store) -> Vec<OutboxOp> {
        s.pending_ops().unwrap().into_iter().map(|r| r.op).collect()
    }

    fn written(r: FolderWriteResult) -> String {
        match r {
            FolderWriteResult::Written(id) => id,
            other => panic!("not written: {other:?}"),
        }
    }

    #[test]
    fn folder_ops_round_trip_json() {
        let snapshot = FolderSnapshot {
            id: "A".into(),
            parent_id: Some("INBOX".into()),
            display_name: "A \"x\"".into(),
            total_count: 4,
            unread_count: 2,
            well_known_name: Some("archive".into()),
            sort_order: Some(3),
            is_expanded: true,
        };
        for op in [
            OutboxOp::CreateFolder {
                id: "local:1".into(),
                parent: Some("P".into()),
                name: "N".into(),
            },
            OutboxOp::CreateFolder {
                id: "local:1".into(),
                parent: None,
                name: "N".into(),
            },
            OutboxOp::RenameFolder {
                id: "F".into(),
                name: "New".into(),
                prev_name: "Old".into(),
            },
            OutboxOp::MoveFolder {
                id: "F".into(),
                dest: Some("D".into()),
                parent: Some("D".into()),
                prev_parent: Some("P".into()),
            },
            OutboxOp::MoveFolder {
                id: "F".into(),
                dest: None,
                parent: None,
                prev_parent: None,
            },
            OutboxOp::DeleteFolder {
                id: "A".into(),
                removed: vec![
                    snapshot.clone(),
                    FolderSnapshot {
                        parent_id: None,
                        well_known_name: None,
                        sort_order: None,
                        is_expanded: false,
                        ..snapshot.clone()
                    },
                ],
            },
        ] {
            let text = op.to_json().to_string();
            let back = OutboxOp::from_json(&json::parse(&text).unwrap());
            assert_eq!(back.as_ref(), Some(&op), "{text}");
        }
    }

    #[test]
    fn folder_writes_queue_their_op_in_the_same_transaction() {
        let s = mailbox();
        let local = written(s.create_local_folder(Some("A"), "New").unwrap());
        assert!(local.starts_with("local:"), "{local}");
        written(s.rename_folder("A", "A2").unwrap());
        written(s.move_folder("B", Some("ARCH")).unwrap());
        written(s.delete_folder("ARCH").unwrap().0);
        assert_eq!(
            ops(&s),
            [
                OutboxOp::CreateFolder {
                    id: local.clone(),
                    parent: Some("A".into()),
                    name: "New".into(),
                },
                OutboxOp::RenameFolder {
                    id: "A".into(),
                    name: "A2".into(),
                    prev_name: "A".into(),
                },
                OutboxOp::MoveFolder {
                    id: "B".into(),
                    dest: Some("ARCH".into()),
                    parent: Some("ARCH".into()),
                    prev_parent: Some("A".into()),
                },
                OutboxOp::DeleteFolder {
                    id: "ARCH".into(),
                    removed: vec![
                        FolderSnapshot {
                            id: "ARCH".into(),
                            parent_id: Some("ROOT".into()),
                            display_name: "ARCH".into(),
                            total_count: 0,
                            unread_count: 0,
                            well_known_name: None,
                            sort_order: None,
                            is_expanded: false,
                        },
                        FolderSnapshot {
                            id: "B".into(),
                            parent_id: Some("ARCH".into()),
                            display_name: "B".into(),
                            total_count: 0,
                            unread_count: 0,
                            well_known_name: None,
                            sort_order: None,
                            is_expanded: false,
                        },
                    ],
                },
            ]
        );
        let new = row(&s, &local).unwrap();
        assert_eq!(new.parent_id.as_deref(), Some("A"));
        assert_eq!(new.display_name, "New");
        assert_eq!(
            s.queued_folder_writes().unwrap(),
            QueuedFolderWrites {
                renamed: ["A".to_string()].into(),
                moved: ["B".to_string()].into(),
                // B was moved under ARCH: the delete took it too.
                deleted: ["ARCH".to_string(), "B".to_string()].into(),
            }
        );
    }

    #[test]
    fn create_at_the_top_stores_the_listed_root_as_parent() {
        let s = mailbox();
        let top = written(s.create_local_folder(None, "Top").unwrap());
        assert_eq!(row(&s, &top).unwrap().parent_id.as_deref(), Some("ROOT"));
        assert_eq!(
            ops(&s)[0],
            OutboxOp::CreateFolder {
                id: top,
                parent: None,
                name: "Top".into(),
            }
        );
        // Before any listing there is no root to give it.
        let empty = Store::open_in_memory().unwrap();
        let top = written(empty.create_local_folder(None, "Top").unwrap());
        assert_eq!(row(&empty, &top).unwrap().parent_id, None);
    }

    #[test]
    fn folder_writes_refuse_unknown_folders_and_the_drafts_sentinel() {
        let s = mailbox();
        let sentinel = s.drafts_folder_id().unwrap();
        assert_eq!(sentinel, LOCAL_DRAFTS_SENTINEL_FOLDER_ID);
        assert_eq!(row(&s, &sentinel).unwrap().parent_id, None);
        let unknown = || FolderWriteResult::Refused(FolderRefusal::Unknown("NOPE".into()));
        let drafts = FolderWriteResult::Refused(FolderRefusal::DraftsSentinel);
        assert_eq!(s.create_local_folder(Some("NOPE"), "x").unwrap(), unknown());
        assert_eq!(s.rename_folder("NOPE", "x").unwrap(), unknown());
        assert_eq!(s.move_folder("NOPE", None).unwrap(), unknown());
        assert_eq!(s.move_folder("A", Some("NOPE")).unwrap(), unknown());
        assert_eq!(s.delete_folder("NOPE").unwrap().0, unknown());
        assert_eq!(s.create_local_folder(Some(&sentinel), "x").unwrap(), drafts);
        assert_eq!(s.rename_folder(&sentinel, "x").unwrap(), drafts);
        assert_eq!(s.move_folder(&sentinel, Some("A")).unwrap(), drafts);
        assert_eq!(s.move_folder("A", Some(&sentinel)).unwrap(), drafts);
        assert_eq!(s.delete_folder(&sentinel).unwrap().0, drafts);
        assert!(ops(&s).is_empty());
        let kept = row(&s, &sentinel).unwrap();
        assert_ne!(kept.display_name, "x");
        assert_eq!(kept.parent_id, None);
    }

    #[test]
    fn move_into_itself_or_a_folder_inside_it_is_refused() {
        let s = mailbox();
        let into = FolderWriteResult::Refused(FolderRefusal::IntoItself);
        assert_eq!(s.move_folder("A", Some("A")).unwrap(), into);
        assert_eq!(s.move_folder("A", Some("B")).unwrap(), into);
        assert_eq!(s.move_folder("INBOX", Some("B")).unwrap(), into);
        assert!(ops(&s).is_empty());
        assert_eq!(row(&s, "A").unwrap().parent_id.as_deref(), Some("INBOX"));
    }

    #[test]
    fn rename_and_move_keep_sort_order_and_expanded() {
        let s = mailbox();
        s.set_folder_expanded("A", true).unwrap();
        s.conn
            .execute("UPDATE folders SET sort_order = 7 WHERE id = 'A'", [])
            .unwrap();
        s.set_delta_link("A", "LINK").unwrap();
        written(s.rename_folder("A", "Renamed").unwrap());
        written(s.move_folder("A", None).unwrap());
        let a = row(&s, "A").unwrap();
        assert_eq!(a.display_name, "Renamed");
        assert_eq!(a.parent_id.as_deref(), Some("ROOT"), "the top");
        assert!(a.is_expanded);
        assert_eq!(a.sort_order, Some(7));
        assert_eq!(a.delta_link.as_deref(), Some("LINK"));
        // Nothing to do: nothing queued.
        assert_eq!(
            s.rename_folder("A", "Renamed").unwrap(),
            FolderWriteResult::Unchanged
        );
        assert_eq!(
            s.move_folder("A", None).unwrap(),
            FolderWriteResult::Unchanged
        );
        assert_eq!(
            s.move_folder("B", Some("A")).unwrap(),
            FolderWriteResult::Unchanged
        );
        assert_eq!(ops(&s).len(), 2);
    }

    #[test]
    fn delete_folder_removes_subtree_messages_and_delta_links() {
        let s = mailbox();
        let local = written(s.create_local_folder(Some("B"), "L").unwrap());
        for id in ["A", "B"] {
            s.set_delta_link(id, "LINK").unwrap();
        }
        message(&s, "m-a", "A");
        message(&s, "m-b", "B");
        message(&s, "m-inbox", "INBOX");
        s.set_folder_expanded("B", true).unwrap();
        written(s.delete_folder("A").unwrap().0);
        for id in ["A", "B", local.as_str()] {
            assert!(row(&s, id).is_none(), "{id} removed");
        }
        assert_eq!(folder_of(&s, "m-a"), None);
        assert_eq!(folder_of(&s, "m-b"), None);
        assert_eq!(folder_of(&s, "m-inbox").as_deref(), Some("INBOX"));
        let Some(OutboxOp::DeleteFolder { id, removed }) = ops(&s).pop() else {
            panic!("no delete queued")
        };
        assert_eq!(id, "A");
        let ids: Vec<&str> = removed.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["A", "B"], "the local: folder is not snapshot");

        // A refused delete puts the rows back with their local state, and
        // without a delta link.
        assert!(s.restore_folders(&removed).unwrap());
        let b = row(&s, "B").unwrap();
        assert_eq!(b.parent_id.as_deref(), Some("A"));
        assert!(b.is_expanded);
        assert_eq!(b.delta_link, None);
        assert!(row(&s, "A").is_some());
        assert!(!s.restore_folders(&removed).unwrap(), "already back");
    }

    #[test]
    fn delete_folder_holding_unsent_mail_is_refused() {
        let unsent = FolderWriteResult::Refused(FolderRefusal::HoldsUnsentMail);
        // A local draft, in a folder under the one deleted.
        let s = mailbox();
        message(&s, "local:d1", "B");
        assert_eq!(s.delete_folder("A").unwrap().0, unsent);
        // A send queued: SendDraft re-filed the draft into this folder.
        let s = mailbox();
        message(&s, "G1", "ARCH");
        s.enqueue_op(&OutboxOp::SendDraft { id: "G1".into() })
            .unwrap();
        assert_eq!(s.delete_folder("ARCH").unwrap().0, unsent);
        // A queued save by the `local:` id a draft was reconciled from.
        let s = mailbox();
        message(&s, "G2", "ARCH");
        s.conn
            .execute(
                "UPDATE messages SET local_id = 'local:x' WHERE id = 'G2'",
                [],
            )
            .unwrap();
        s.enqueue_op(&OutboxOp::SaveDraft {
            id: "local:x".into(),
        })
        .unwrap();
        assert_eq!(s.delete_folder("ARCH").unwrap().0, unsent);
        // An unpushed edit (#2082).
        let s = mailbox();
        message(&s, "G3", "ARCH");
        s.conn
            .execute("UPDATE messages SET local_edit = 2 WHERE id = 'G3'", [])
            .unwrap();
        assert_eq!(s.delete_folder("ARCH").unwrap().0, unsent);
        assert!(row(&s, "ARCH").is_some());
        assert_eq!(folder_of(&s, "G3").as_deref(), Some("ARCH"));
        assert_eq!(ops(&s).len(), 0);
        // Mail Graph has, with other ops queued, is no reason.
        let s = mailbox();
        message(&s, "G4", "ARCH");
        s.enqueue_op(&OutboxOp::MarkRead {
            id: "G4".into(),
            read: true,
        })
        .unwrap();
        written(s.delete_folder("ARCH").unwrap().0);
    }

    #[test]
    fn deleting_a_local_folder_drops_the_ops_of_mail_moved_into_it() {
        // L, created here, holds M (moved in from INBOX) and sits under
        // ARCH; deleting L, or ARCH, takes M, which Graph still has in INBOX.
        for target in ["L", "ARCH"] {
            let s = mailbox();
            let local = written(s.create_local_folder(Some("ARCH"), "L").unwrap());
            message(&s, "M", &local);
            message(&s, "N", "INBOX");
            for op in [
                OutboxOp::MarkRead {
                    id: "M".into(),
                    read: true,
                },
                OutboxOp::Move {
                    id: "M".into(),
                    dest: local.clone(),
                },
                OutboxOp::SetFlag {
                    id: "N".into(),
                    flagged: true,
                },
            ] {
                s.enqueue_op(&op).unwrap();
            }
            let id = if target == "L" {
                local.as_str()
            } else {
                "ARCH"
            };
            let (result, dropped) = s.delete_folder(id).unwrap();
            written(result);
            assert!(dropped, "{target}: M went with {local}");
            assert!(folder_of(&s, "M").is_none());
            let left: Vec<&'static str> = ops(&s).iter().map(|op| op.kind()).collect();
            assert_eq!(
                left,
                ["createFolder", "setFlag", "deleteFolder"],
                "{target}: M's ops go, N's stays"
            );
        }
        // A local folder with no mail drops nothing.
        let s = mailbox();
        let local = written(s.create_local_folder(None, "Empty").unwrap());
        let (result, dropped) = s.delete_folder(&local).unwrap();
        written(result);
        assert!(!dropped);
        // Nor does a Graph folder's own mail.
        let s = mailbox();
        message(&s, "A1", "A");
        s.enqueue_op(&OutboxOp::MarkRead {
            id: "A1".into(),
            read: true,
        })
        .unwrap();
        let (_, dropped) = s.delete_folder("A").unwrap();
        assert!(!dropped);
        assert_eq!(ops(&s).len(), 2, "its op is kept");
    }

    #[test]
    fn a_refused_create_files_unsent_mail_back_in_drafts() {
        let s = mailbox();
        let local = written(s.create_local_folder(Some("INBOX"), "L").unwrap());
        let kid = written(s.create_local_folder(Some(&local), "Kid").unwrap());
        message(&s, "local:d1", &local);
        message(&s, "G-edited", &kid);
        s.conn
            .execute(
                "UPDATE messages SET local_edit = 1 WHERE id = 'G-edited'",
                [],
            )
            .unwrap();
        message(&s, "M", &local);
        for op in [
            OutboxOp::SaveDraft {
                id: "local:d1".into(),
            },
            OutboxOp::Move {
                id: "local:d1".into(),
                dest: local.clone(),
            },
            OutboxOp::Move {
                id: "M".into(),
                dest: local.clone(),
            },
        ] {
            s.enqueue_op(&op).unwrap();
        }
        let discard = s.discard_local_folder(&local).unwrap();
        // No Drafts folder is stored: the sentinel takes them.
        assert_eq!(
            discard,
            LocalFolderDiscard {
                had_mail: true,
                refiled_into: Some(LOCAL_DRAFTS_SENTINEL_FOLDER_ID.to_string()),
            }
        );
        assert_eq!(
            folder_of(&s, "local:d1").as_deref(),
            Some(LOCAL_DRAFTS_SENTINEL_FOLDER_ID)
        );
        assert_eq!(
            folder_of(&s, "G-edited").as_deref(),
            Some(LOCAL_DRAFTS_SENTINEL_FOLDER_ID)
        );
        assert_eq!(folder_of(&s, "M"), None, "a reconverge brings it back");
        let left: Vec<&'static str> = ops(&s).iter().map(|op| op.kind()).collect();
        assert_eq!(
            left,
            ["createFolder", "createFolder", "saveDraft"],
            "the draft's save stays; both moves into L go"
        );

        // With Drafts stored, they go there.
        let s = mailbox();
        s.upsert_folder(&MailFolder {
            well_known_name: Some("drafts".into()),
            ..listed("DRAFTS", Some("ROOT"), "Drafts")
        })
        .unwrap();
        let local = written(s.create_local_folder(None, "L").unwrap());
        message(&s, "local:d2", &local);
        s.enqueue_op(&OutboxOp::Move {
            id: "local:d2".into(),
            dest: local.clone(),
        })
        .unwrap();
        let discard = s.discard_local_folder(&local).unwrap();
        assert_eq!(discard.refiled_into.as_deref(), Some("DRAFTS"));
        assert!(!discard.had_mail);
        assert_eq!(folder_of(&s, "local:d2").as_deref(), Some("DRAFTS"));
        assert!(
            !ops(&s).iter().any(|op| matches!(op, OutboxOp::Move { .. })),
            "the move into L goes although no other mail was there"
        );
    }

    #[test]
    fn a_graph_folder_moved_under_a_deleted_local_folder_gets_its_own_delete() {
        // OTHER (with a child) is moved under L, which is under ARCH;
        // deleting L, or ARCH, cannot send OTHER's move: OTHER gets a
        // delete of its own.
        for target in ["L", "ARCH"] {
            let s = mailbox();
            s.upsert_folder(&listed("OTHER", Some("ROOT"), "OTHER"))
                .unwrap();
            s.upsert_folder(&listed("OKID", Some("OTHER"), "OKID"))
                .unwrap();
            let local = written(s.create_local_folder(Some("ARCH"), "L").unwrap());
            written(s.move_folder("OTHER", Some(&local)).unwrap());
            let id = if target == "L" {
                local.clone()
            } else {
                "ARCH".into()
            };
            written(s.delete_folder(&id).unwrap().0);
            let deletes: Vec<(String, Vec<String>)> = ops(&s)
                .into_iter()
                .filter_map(|op| match op {
                    OutboxOp::DeleteFolder { id, removed } => {
                        Some((id, removed.into_iter().map(|f| f.id).collect()))
                    }
                    _ => None,
                })
                .collect();
            let own: Vec<String> = if target == "ARCH" {
                vec!["ARCH".into()]
            } else {
                vec![]
            };
            assert_eq!(
                deletes,
                [
                    (
                        "OTHER".to_string(),
                        vec!["OKID".to_string(), "OTHER".to_string()]
                    ),
                    (id.clone(), own)
                ],
                "{target}"
            );
            // A refusal restores OTHER under the parent Graph has it under.
            let Some(OutboxOp::DeleteFolder { removed, .. }) = ops(&s)
                .into_iter()
                .find(|op| matches!(op, OutboxOp::DeleteFolder { id, .. } if id == "OTHER"))
            else {
                unreachable!()
            };
            let parents: Vec<(String, Option<String>)> =
                removed.into_iter().map(|f| (f.id, f.parent_id)).collect();
            assert_eq!(
                parents,
                [
                    ("OKID".to_string(), Some("OTHER".to_string())),
                    ("OTHER".to_string(), Some("ROOT".to_string()))
                ],
                "{target}"
            );
            assert!(
                !ops(&s)
                    .iter()
                    .any(|op| matches!(op, OutboxOp::MoveFolder { .. })),
                "{target}: OTHER's move goes"
            );
            let deleted = s.queued_folder_writes().unwrap().deleted;
            assert!(deleted.contains("OTHER") && deleted.contains("OKID"));
        }
    }

    #[test]
    fn deleting_a_local_folder_drops_every_move_into_it() {
        // (a) OTHER moved under L, then deleted itself, then L deleted.
        let s = mailbox();
        s.upsert_folder(&listed("OTHER", Some("ROOT"), "OTHER"))
            .unwrap();
        let local = written(s.create_local_folder(None, "L").unwrap());
        written(s.move_folder("OTHER", Some(&local)).unwrap());
        written(s.delete_folder("OTHER").unwrap().0);
        written(s.delete_folder(&local).unwrap().0);
        assert!(
            !ops(&s)
                .iter()
                .any(|op| matches!(op, OutboxOp::MoveFolder { .. })),
            "(a) {:?}",
            ops(&s)
        );
        // (b) OTHER moved under L, then on under ARCH, then L deleted: only
        // the move into L goes.
        let s = mailbox();
        s.upsert_folder(&listed("OTHER", Some("ROOT"), "OTHER"))
            .unwrap();
        let local = written(s.create_local_folder(None, "L").unwrap());
        written(s.move_folder("OTHER", Some(&local)).unwrap());
        written(s.move_folder("OTHER", Some("ARCH")).unwrap());
        written(s.delete_folder(&local).unwrap().0);
        let moves: Vec<Option<String>> = ops(&s)
            .into_iter()
            .filter_map(|op| match op {
                OutboxOp::MoveFolder { dest, .. } => Some(dest),
                _ => None,
            })
            .collect();
        assert_eq!(moves, [Some("ARCH".to_string())], "(b)");
        assert_eq!(row(&s, "OTHER").unwrap().parent_id.as_deref(), Some("ARCH"));
    }

    #[test]
    fn a_queued_delete_names_every_folder_it_removed() {
        let s = mailbox();
        written(s.move_folder("ARCH", Some("B")).unwrap());
        written(s.delete_folder("A").unwrap().0);
        let deleted = s.queued_folder_writes().unwrap().deleted;
        let mut deleted: Vec<String> = deleted.into_iter().collect();
        deleted.sort();
        assert_eq!(deleted, ["A", "ARCH", "B"]);
    }

    #[test]
    fn reconcile_created_folder_rekeys_messages_children_and_aliases() {
        let s = mailbox();
        let local = written(s.create_local_folder(Some("INBOX"), "New").unwrap());
        let child = written(s.create_local_folder(Some(&local), "Kid").unwrap());
        message(&s, "m1", &local);
        s.set_folder_expanded(&local, true).unwrap();
        written(s.rename_folder(&local, "Renamed").unwrap());
        assert!(
            s.reconcile_created_folder(&local, &listed("G1", Some("INBOX"), "New"))
                .unwrap()
        );
        assert!(row(&s, &local).is_none());
        let g = row(&s, "G1").unwrap();
        assert_eq!(g.display_name, "Renamed", "a later rename is kept");
        assert_eq!(g.parent_id.as_deref(), Some("INBOX"));
        assert!(g.is_expanded);
        assert_eq!(folder_of(&s, "m1").as_deref(), Some("G1"));
        assert_eq!(row(&s, &child).unwrap().parent_id.as_deref(), Some("G1"));
        assert_eq!(s.current_folder_id(&local).unwrap(), "G1");
        assert_eq!(s.current_folder_id("G1").unwrap(), "G1");
        assert_eq!(
            s.queued_folder_writes().unwrap().renamed,
            ["G1".to_string()].into(),
            "a queued op by the local: id resolves"
        );
    }

    #[test]
    fn reconcile_keeps_the_mail_of_a_stale_row_under_the_graph_id() {
        let s = mailbox();
        let local = written(s.create_local_folder(None, "New").unwrap());
        s.upsert_folder(&listed("G1", Some("ROOT"), "stale"))
            .unwrap();
        s.upsert_folder(&listed("UNDER", Some("G1"), "under"))
            .unwrap();
        message(&s, "m-stale", "G1");
        message(&s, "m-local", &local);
        s.reconcile_created_folder(&local, &listed("G1", Some("ROOT"), "New"))
            .unwrap();
        assert_eq!(row(&s, "G1").unwrap().display_name, "New", "local wins");
        assert_eq!(folder_of(&s, "m-stale").as_deref(), Some("G1"));
        assert_eq!(folder_of(&s, "m-local").as_deref(), Some("G1"));
        assert_eq!(row(&s, "UNDER").unwrap().parent_id.as_deref(), Some("G1"));
    }

    #[test]
    fn reconcile_of_a_deleted_row_still_aliases() {
        let s = mailbox();
        let local = written(s.create_local_folder(None, "New").unwrap());
        written(s.delete_folder(&local).unwrap().0);
        assert!(
            !s.reconcile_created_folder(&local, &listed("G1", Some("ROOT"), "New"))
                .unwrap()
        );
        assert!(row(&s, "G1").is_none());
        assert_eq!(s.current_folder_id(&local).unwrap(), "G1");
    }

    #[test]
    fn current_folder_id_follows_chained_aliases() {
        let s = mailbox();
        let local = written(s.create_local_folder(None, "New").unwrap());
        s.reconcile_created_folder(&local, &listed("G1", Some("ROOT"), "New"))
            .unwrap();
        // Graph answers a move with a new id.
        let changed = s
            .apply_moved_folder(&local, Some("ARCH"), &listed("G2", Some("ARCH"), "New"))
            .unwrap();
        assert!(changed);
        assert_eq!(s.current_folder_id(&local).unwrap(), "G2");
        assert_eq!(s.current_folder_id("G1").unwrap(), "G2");
        assert!(row(&s, "G2").is_some());
        // And back to an id it had.
        s.apply_moved_folder("G2", None, &listed("G1", Some("ROOT"), "New"))
            .unwrap();
        assert_eq!(s.current_folder_id(&local).unwrap(), "G1");
        assert_eq!(s.current_folder_id("G2").unwrap(), "G1");
        assert_eq!(s.current_folder_id("G1").unwrap(), "G1", "no cycle");
    }

    #[test]
    fn a_move_answered_with_a_new_id_rekeys_and_clears_the_links() {
        let s = mailbox();
        s.set_delta_link("B", "LINK").unwrap();
        s.set_folder_expanded("B", true).unwrap();
        s.upsert_folder(&listed("C", Some("B"), "C")).unwrap();
        message(&s, "m1", "B");
        written(s.move_folder("B", Some("ARCH")).unwrap());
        assert!(
            s.apply_moved_folder("B", Some("ARCH"), &listed("B2", Some("ARCH"), "B"))
                .unwrap()
        );
        assert!(row(&s, "B").is_none());
        let b = row(&s, "B2").unwrap();
        assert_eq!(b.parent_id.as_deref(), Some("ARCH"));
        assert!(b.is_expanded);
        assert_eq!(b.delta_link, None, "the link names the old id");
        assert_eq!(folder_of(&s, "m1").as_deref(), Some("B2"));
        assert_eq!(row(&s, "C").unwrap().parent_id.as_deref(), Some("B2"));
        assert_eq!(s.current_folder_id("B").unwrap(), "B2");
    }

    #[test]
    fn a_move_answer_gives_the_parent_unless_a_later_move_wrote_one() {
        let s = mailbox();
        written(s.move_folder("A", None).unwrap());
        // Graph's root id replaces the one the store guessed.
        assert!(
            s.apply_moved_folder("A", Some("ROOT"), &listed("A", Some("REALROOT"), "A"))
                .unwrap()
        );
        assert_eq!(row(&s, "A").unwrap().parent_id.as_deref(), Some("REALROOT"));
        // A later queued move wrote its own parent: kept.
        written(s.move_folder("B", Some("ARCH")).unwrap());
        written(s.move_folder("B", Some("INBOX")).unwrap());
        assert!(
            !s.apply_moved_folder("B", Some("ARCH"), &listed("B", Some("ARCH"), "B"))
                .unwrap()
        );
        assert_eq!(row(&s, "B").unwrap().parent_id.as_deref(), Some("INBOX"));
    }

    #[test]
    fn revert_folder_name_keeps_a_later_rename() {
        let s = mailbox();
        written(s.rename_folder("A", "One").unwrap());
        assert!(s.revert_folder_name("A", "A", "One").unwrap());
        assert_eq!(row(&s, "A").unwrap().display_name, "A");
        written(s.rename_folder("A", "One").unwrap());
        written(s.rename_folder("A", "Two").unwrap());
        assert!(!s.revert_folder_name("A", "A", "One").unwrap());
        assert_eq!(row(&s, "A").unwrap().display_name, "Two");
    }

    #[test]
    fn revert_folder_parent_keeps_a_later_move() {
        let s = mailbox();
        written(s.move_folder("B", Some("ARCH")).unwrap());
        assert!(
            s.revert_folder_parent("B", Some("A"), Some("ARCH"))
                .unwrap()
        );
        assert_eq!(row(&s, "B").unwrap().parent_id.as_deref(), Some("A"));
        written(s.move_folder("B", Some("ARCH")).unwrap());
        written(s.move_folder("B", Some("INBOX")).unwrap());
        assert!(
            !s.revert_folder_parent("B", Some("A"), Some("ARCH"))
                .unwrap()
        );
        assert_eq!(row(&s, "B").unwrap().parent_id.as_deref(), Some("INBOX"));
    }

    #[test]
    fn discard_local_folder_leaves_folders_graph_has() {
        let s = mailbox();
        let local = written(s.create_local_folder(None, "New").unwrap());
        let kid = written(s.create_local_folder(Some(&local), "Kid").unwrap());
        written(s.move_folder("B", Some(&local)).unwrap());
        message(&s, "m-b", "B");
        // K, created here under B (a Graph folder moved under L), would
        // still reach Graph under B: it stays with B.
        let k = written(s.create_local_folder(Some("B"), "K").unwrap());
        assert!(
            !s.discard_local_folder(&local).unwrap().had_mail,
            "no mail went"
        );
        assert!(row(&s, &local).is_none());
        assert!(row(&s, &kid).is_none());
        assert!(row(&s, "B").is_some(), "its own move's refusal reverts it");
        assert_eq!(row(&s, &k).unwrap().parent_id.as_deref(), Some("B"));
        assert_eq!(folder_of(&s, "m-b").as_deref(), Some("B"));

        let local = written(s.create_local_folder(None, "New").unwrap());
        message(&s, "m-moved", &local);
        assert!(s.discard_local_folder(&local).unwrap().had_mail);
        assert_eq!(folder_of(&s, "m-moved"), None);
        assert!(
            s.discard_local_folder("A").unwrap() == LocalFolderDiscard::default(),
            "not local:"
        );
        assert!(row(&s, "A").is_some());
    }
}
