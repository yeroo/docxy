//! The background sync engine: one `std::thread` that owns a [`Store`] and
//! talks to Microsoft Graph, driven by (and reporting to) the UI over
//! `std::sync::mpsc` channels — no async runtime.
//!
//! [`spawn`] launches the thread and hands back a [`SyncHandle`]
//! (`cmd_tx` down, `evt_rx` up). The thread loads the cached token, does an
//! initial folder + delta backfill, then blocks on
//! `cmd_rx.recv_timeout(tick)`: each tick (or an explicit `Refresh`) lists
//! the folder tree (#1886: names, parents, counts, new folders, and the
//! removal of folders deleted on the server), re-runs per-folder delta sync
//! and drains the outbox; a full pass (startup, `Refresh`, sign-in) also
//! refreshes people and master categories. Mutation commands write the
//! store optimistically, enqueue an outbox op, and drain. Graph calls go
//! through [`Engine::with_auth`], which refreshes the token once on a 401 and
//! retries. Throttling (`Retry-After`) is honored by rescheduling; transport
//! failures drop to `Offline` with exponential back-off while the UI keeps
//! reading the store.
//!
//! A folder's first sync is in two phases (#1871). In the sync pass it gets
//! a *head* delta, only its recent mail (`HEAD_WINDOW_DAYS`), newest first,
//! whose deltaLink keeps the folder fresh from then on. Then, one page per
//! idle step of the main loop (no command queued, no deadline due), a *full*
//! delta walks the rest of the folder newest first, persisting its next page
//! so a restart resumes; its deltaLink replaces the head's when it ends.
//! `backfill_days <= 0` ([`WHOLE_MAILBOX`]) downloads everything; a positive
//! value keeps that window (older items of the full delta are skipped).
//! Requests are one at a time: the engine is one thread.
//!
//! An opt-in fast cycle (`EngineOptions::fast_tick`, #1885) syncs only the
//! folders a caller names (`SyncCommand::SetFastFolders`) between two normal
//! ticks, and `SyncCommand::SyncFolder` syncs one folder on demand.
//!
//! Bodies (#1978): a round on a folder's stored delta link fetches the bodies
//! of the messages it added, in the same cycle, and the idle loop prefetches
//! the bodies of the newest `EngineOptions::prefetch_body_days` of mail, a
//! small batch per step, before any backfill page. A `SyncCommand::FetchBody`
//! never waits for a whole pass: long work calls `Engine::checkpoint` between
//! folders, delta pages and body fetches, which serves it at once and keeps
//! every other command for after the work, in order.
//!
//! All Graph/auth base URLs are injectable (see `spawn_with_bases`) so tests
//! drive the whole engine against the in-process fake server with no network.
//! Secrets (access/refresh tokens) are never logged.

use crate::auth::{self, AuthConfig, TokenSet};
use crate::graph::client::{DeltaCursor, GraphClient, GraphError, RsvpKind, is_expired_sync_state};
pub use crate::graph::model::BodyFormat;
use crate::graph::model::{AutomaticReplies, DeltaItem, MailFolder, Me, Message, ScheduleEntry};
use crate::store::{
    FolderHistory, LOCAL_DRAFTS_SENTINEL_FOLDER_ID, NewAttendee, NewEvent, OutboxOp, OutboxRow,
    SCOPE_ALL, SCOPE_WINDOW, Store, StoreError,
};
use crate::sync::outbox::{apply_op, apply_send, send_error_text};
use crate::tokencache;
use std::collections::{HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A command sent from the UI down to the sync thread.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncCommand {
    /// Re-enumerate folders, re-run every folder's delta, and drain the outbox.
    Refresh,
    /// Sync one folder now (#1885, "Update Folder"): drain the outbox, run
    /// that folder's delta (its head, phase 1 of #1871, when it never
    /// synced; the backfill of older mail follows in idle time), and re-read
    /// its counts
    /// (`GET /me/mailFolders/{id}`). Emits `MessagesUpdated { folder_id }`
    /// and, when the counts changed, `FoldersUpdated`. No other folder is
    /// touched. Signed out or inside a throttle/back-off window it does
    /// nothing, as a tick does there; a folder id not in the store is an
    /// [`SyncEvent::Error`] and makes no Graph call.
    SyncFolder { id: String },
    /// The folders the fast cycle keeps fresh (#1885), replacing the set
    /// whole (duplicates dropped, order kept): a UI sends its Inbox and the
    /// folder it has open, and again when the open folder changes. Each
    /// fast tick drains the outbox and syncs these folders as `SyncFolder`
    /// does, skipping one that never synced (the normal pass runs its head;
    /// a fast tick never runs a backfill page, #1871).
    /// Without [`EngineOptions::fast_tick`] the set is kept but has no
    /// effect.
    SetFastFolders { ids: Vec<String> },
    /// Start the interactive browser sign-in flow (see [`Engine::sign_in`]).
    SignIn,
    /// Mark a message read/unread (optimistic local write + queued Graph op).
    MarkRead { id: String, read: bool },
    /// Flag/unflag a message (optimistic local write + queued Graph op).
    SetFlag { id: String, flagged: bool },
    /// Set a message's category names (optimistic local write + queued Graph op).
    SetCategories { id: String, categories: Vec<String> },
    /// Fetch the master category list (`GraphClient::get_master_categories`),
    /// store it, and emit [`SyncEvent::CategoriesUpdated`]. Direct call.
    RefreshCategories,
    /// Fetch attendee availability (`GraphClient::get_schedule`) for a window
    /// and emit [`SyncEvent::ScheduleFetched`]. Direct call, no store.
    FetchSchedule {
        schedules: Vec<String>,
        start_utc: String,
        end_utc: String,
        interval_minutes: i64,
    },
    /// Move a message to another folder: optimistic local re-file plus a queued
    /// Graph op. Graph mints a new id on move, which the next delta reconciles
    /// (old id `@removed`, new id added).
    Move { id: String, dest: String },
    /// Delete a message (optimistic local delete + queued Graph op).
    Delete { id: String },
    /// Fetch and store a message body in the engine's
    /// [`EngineOptions::body_format`], then emit [`SyncEvent::BodyReady`].
    /// Served ahead of a running pass, between two of its requests (#1978).
    FetchBody { id: String },
    /// Fetch attachment metadata (`GraphClient::list_attachments`) for a
    /// message and store it, then emit [`SyncEvent::AttachmentsUpdated`].
    /// `Store::put_attachments` is otherwise never populated in production
    /// (only test fixtures write it directly), so the attachments popup has
    /// nothing to show on a real mailbox until this runs.
    FetchAttachments { message_id: String },
    /// Fetch an attachment's bytes (`GraphClient::get_attachment_bytes`) and
    /// write them to `dest`, then emit [`SyncEvent::AttachmentSaved`]. `dest`
    /// is a full file path (the UI has already resolved the Downloads
    /// directory and sanitized the attachment's name into a single path
    /// component) — the engine only writes to it, it doesn't derive it.
    SaveAttachment {
        message_id: String,
        attachment_id: String,
        dest: PathBuf,
    },
    /// Download a nested `itemAttachment` (`GraphClient::get_attachment_raw_value`)
    /// and write it to `{dest_base}.ics` or `{dest_base}.eml` — the extension
    /// is chosen by sniffing the bytes (`BEGIN:VCALENDAR` → calendar item).
    /// `dest_base` is the Downloads path WITHOUT an extension (the UI can't
    /// know it until the bytes are sniffed). Emits [`SyncEvent::AttachmentSaved`]
    /// with the extended path, the same event `SaveAttachment` uses.
    SaveItemAttachment {
        message_id: String,
        attachment_id: String,
        dest_base: PathBuf,
    },
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
        comment: Option<String>,
        proposed_start_utc: Option<String>,
        proposed_end_utc: Option<String>,
    },
    /// Fetch the mailbox's automatic-replies config
    /// (`GraphClient::get_automatic_replies`) and emit
    /// [`SyncEvent::AutomaticRepliesFetched`]. Direct call, no outbox.
    FetchAutomaticReplies,
    /// Write the mailbox's automatic-replies config
    /// (`GraphClient::set_automatic_replies`) and emit
    /// [`SyncEvent::AutomaticRepliesUpdated`]. Direct call, no outbox.
    SetAutomaticReplies { replies: AutomaticReplies },
    /// Fetch who is signed in (`GraphClient::me`) and emit
    /// [`SyncEvent::ProfileFetched`]. Direct call, no outbox.
    FetchProfile,
    /// Fetch the signed-in user's photo (`GraphClient::my_photo`) and emit
    /// [`SyncEvent::PhotoFetched`] (`None` when the user has none). Direct
    /// call, no outbox.
    FetchPhoto,
    /// Sign out: drop the in-memory token and delete the saved one
    /// (`tokencache::clear`), then emit [`SyncEvent::SignInRequired`], so the
    /// next `SignIn` starts a fresh sign-in. `State(SignInRequired)` comes
    /// only when the state changes: a SignOut while already signed out still
    /// clears the file and emits `SignInRequired`, but no `State`, so a
    /// client confirms a sign-out by `SignInRequired`. The mail store and the
    /// outbox are kept: queued ops drain after the next sign-in.
    SignOut,
    /// Fetch an inline image's bytes (`GraphClient::get_attachment_bytes`)
    /// into memory for rendering in the reading pane — distinct from
    /// `SaveAttachment`, which writes to disk. `content_id` is echoed back on
    /// [`SyncEvent::InlineImageReady`] so the UI can key the bytes by the
    /// `cid:` the body references.
    FetchInlineImage {
        message_id: String,
        attachment_id: String,
        content_id: String,
    },
    /// Push a draft's currently-stored fields to Graph (optimistic write
    /// already applied by the caller — compose autosaves via
    /// `Store::update_draft_fields` directly): enqueue
    /// [`crate::store::OutboxOp::SaveDraft`] and drain. A `local:` id not yet
    /// on Graph is created there and reconciled (see `sync::outbox::apply_op`);
    /// a Graph id, or a `local:` id already reconciled to one (#1884), is
    /// patched in place.
    SaveDraft { id: String },
    /// Hand a draft to Graph for delivery: enqueue
    /// [`crate::store::OutboxOp::SendDraft`], optimistically mark it sent and
    /// (if the Sent folder has synced) move it there locally, emit
    /// [`SyncEvent::Sending`], and drain. Emits [`SyncEvent::Sent`] once the
    /// drain actually delivers it, or [`SyncEvent::SendFailed`] when the send
    /// fails for good (any failure but a 5xx, 401, 429/503 or transport
    /// error, which keep the op queued): the message is then back in Drafts
    /// as a draft (#1874). A send whose `/send` POST may have gone out is
    /// first checked for delivery on its next attempt, and is never returned
    /// to Drafts.
    SendDraft { id: String },
    /// Fetch a pre-quoted reply draft for `id` (`createReply`/
    /// `createReplyAll` depending on `all`), store it, and emit
    /// [`SyncEvent::DraftReady`] so the UI can open the compose editor on it.
    ComposeReply { id: String, all: bool },
    /// Fetch a pre-quoted forward draft for `id` (`createForward`), store
    /// it, and emit [`SyncEvent::DraftReady`].
    ComposeForward { id: String },
    /// Fetch the calendar window (`calendar_window`, anchored at
    /// `SystemTime::now()`) via `calendar_view`, upsert every event and its
    /// attendees into the store, and emit [`SyncEvent::CalendarUpdated`].
    /// Also run automatically on the periodic tick (see `Engine::on_tick`),
    /// interleaved with the mail sync pass.
    RefreshCalendar,
    /// Respond to a calendar event's invite: optimistic local
    /// `Store::set_event_response` (immediately reflected, before Graph is
    /// even reached) plus [`SyncEvent::CalendarUpdated`], then enqueue
    /// [`crate::store::OutboxOp::RespondEvent`] and drain. `kind` is the
    /// response_status vocabulary `Store::set_event_response` accepts
    /// (`"accepted"`/`"declined"`/`"tentativelyAccepted"`); `comment` is an
    /// optional note sent alongside the RSVP.
    RespondEvent {
        id: String,
        kind: String,
        comment: Option<String>,
        proposed_start_utc: Option<String>,
        proposed_end_utc: Option<String>,
    },
    /// Push a locally-created event (`Store::create_local_event`, already
    /// applied by the caller) to Graph: enqueue
    /// [`crate::store::OutboxOp::CreateEvent`] and drain, then emit
    /// [`SyncEvent::CalendarUpdated`]. A `local:` id is reconciled to the
    /// Graph-minted id by `sync::outbox::apply_op`, same pattern as
    /// `SaveDraft`.
    CreateEvent { id: String },
    /// Push an edited event's currently-stored fields
    /// (`Store::update_event_fields`, already applied by the caller) to
    /// Graph: enqueue [`crate::store::OutboxOp::UpdateEvent`] and drain,
    /// then emit [`SyncEvent::CalendarUpdated`].
    UpdateEvent { id: String },
    /// Delete an event (optimistic local delete already applied by the
    /// caller via `Store::delete_event`): enqueue
    /// [`crate::store::OutboxOp::DeleteEvent`] and drain, then emit
    /// [`SyncEvent::CalendarUpdated`]. A `local:` id that never reached
    /// Graph is a no-op on the Graph side (see `sync::outbox::apply_op`).
    DeleteEvent { id: String },
    /// Exit the sync thread cleanly.
    Shutdown,
}

/// The engine's coarse status, surfaced to the UI via [`SyncEvent::State`].
#[derive(Debug, Clone, PartialEq)]
pub enum SyncState {
    Idle,
    Syncing,
    Offline,
    PendingOps(usize),
    SignInRequired,
}

/// An event sent from the sync thread up to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncEvent {
    /// The folder set in the store changed; re-read `Store::folders`.
    FoldersUpdated,
    /// Messages in `folder_id` changed; re-read `Store::messages_in_folder`.
    MessagesUpdated { folder_id: String },
    /// A message body was fetched and stored; re-read `Store::get_body`.
    /// Also sent for a body the engine fetched on its own (#1978: new mail,
    /// the recent-mail prefetch), so it may name a message nobody opened.
    BodyReady { id: String },
    /// Attachment metadata for `message_id` was fetched and stored;
    /// re-read `Store::attachments(message_id)`.
    AttachmentsUpdated { message_id: String },
    /// An attachment's bytes were fetched and written to `path` (the `dest`
    /// from the triggering [`SyncCommand::SaveAttachment`]).
    AttachmentSaved { path: PathBuf },
    /// A meeting invite was RSVP'd (from [`SyncCommand::RespondMeeting`]); the
    /// UI shows a confirmation and marks the message read.
    MeetingResponded { message_id: String, kind: RsvpKind },
    /// The mailbox's automatic-replies config was fetched (from
    /// [`SyncCommand::FetchAutomaticReplies`]); the UI prefills its OOF form.
    AutomaticRepliesFetched { replies: AutomaticReplies },
    /// The automatic-replies config was written (from
    /// [`SyncCommand::SetAutomaticReplies`]); the UI closes its OOF form.
    AutomaticRepliesUpdated,
    /// Who is signed in (from [`SyncCommand::FetchProfile`]); the UI shows
    /// `me.display_name` and `me.address()`.
    ProfileFetched { me: Me },
    /// The signed-in user's photo as `(bytes, content type)`, or `None` when
    /// the user has none (from [`SyncCommand::FetchPhoto`]); the UI then
    /// draws initials.
    PhotoFetched { photo: Option<(Vec<u8>, String)> },
    /// The master category list changed; the UI re-reads `Store::master_categories`.
    CategoriesUpdated,
    /// Attendee availability (from [`SyncCommand::FetchSchedule`]); the UI fills
    /// its free/busy grid.
    ScheduleFetched { entries: Vec<ScheduleEntry> },
    /// An inline image's bytes were fetched (from
    /// [`SyncCommand::FetchInlineImage`]); the UI caches them by `content_id`.
    InlineImageReady {
        message_id: String,
        content_id: String,
        bytes: Vec<u8>,
    },
    /// A reply/forward draft (from [`SyncCommand::ComposeReply`]/
    /// [`SyncCommand::ComposeForward`]) was stored; the UI should load it
    /// (`Store::draft`) and open the compose editor.
    DraftReady { id: String },
    /// The draft `id` (the same id the triggering [`SyncCommand::SendDraft`]
    /// was addressed with — not necessarily its final Graph id, if it was
    /// still a `local:` id when send started) was successfully handed to
    /// Graph for delivery.
    Sent { id: String },
    /// The send of draft `id` was queued (#1874): the
    /// [`SyncCommand::SendDraft`] arm emits it once the op is in the outbox
    /// and before the drain, so it precedes that send's [`SyncEvent::Sent`]
    /// or [`SyncEvent::SendFailed`]. `id` is the id the command carried.
    /// Signed out, inside a back-off or behind a 5xx, neither may follow
    /// until a later drain runs. A send left queued by an earlier run gets
    /// no `Sending`.
    Sending { id: String },
    /// The send of draft `id` failed for good (#1874): a 4xx (a 403 when the
    /// token lacks `Mail.Send`, a 400 on the PATCH), a 404 on an attempt
    /// whose `/send` never went out (the Graph draft is gone; the Drafts
    /// delta may then remove the row), or a failure before Graph (an
    /// attachment file that cannot be read, a store error). After a `/send`
    /// whose answer was lost (a 500, 504 or dropped connection) a 404 means
    /// delivered instead: the next attempt checks, and emits
    /// [`SyncEvent::Sent`]. The op is dropped, and the message is back in Drafts as a
    /// draft with its fields and attachments. `id` is the id the
    /// [`SyncCommand::SendDraft`] carried, as for [`SyncEvent::Sent`]: the
    /// draft is still reachable by it (#1884). `error` is Graph's
    /// `error.message` when the response has one, else the status or the
    /// failure's own text.
    SendFailed { id: String, error: String },
    /// The events store changed (a `RefreshCalendar`/`RespondEvent` just
    /// landed); re-read `Store::events_in_window`/`Store::event_attendees`.
    CalendarUpdated,
    /// A status transition.
    State(SyncState),
    /// A non-fatal error worth surfacing (a skipped/quarantined op, a
    /// per-folder failure, etc.). Never contains a secret. It also carries
    /// the notice that a folder holds messages stored without headers,
    /// which the sync is fetching (#1899).
    Error(String),
    /// No valid token; the UI must trigger `SyncCommand::SignIn`.
    SignInRequired,
    /// Sign-in began; the UI opens `authorize_url` in the system browser.
    SignInStarted { authorize_url: String },
    /// The scopes the signed-in token was granted (#1875), as the token
    /// response spelled them, and the scopes of `AuthConfig::scope` it lacks
    /// ([`crate::auth::missing_scopes`]: case-insensitive, without the
    /// `https://graph.microsoft.com/` prefix, never `offline_access`,
    /// `openid`, `profile`, `email` or a `.default` scope). Sent after the
    /// token is loaded at startup, refreshed or redeemed at sign-in, only
    /// when the grant is known and is not the one last sent (compared as a
    /// set, case and prefix aside): a later refresh (after a 401, about
    /// hourly) with the same grant sends none. A sign-in-required (a sign-out, a failed refresh) forgets
    /// the last one, so the next session sends it again. A token whose
    /// grant is unknown (a cache from before #1875 and a refresh that names
    /// no scopes) sends nothing.
    Scopes {
        granted: Vec<String>,
        missing: Vec<String>,
    },
    /// A folder's backfill of older mail moved on (#1871): after every page
    /// of its full delta, and once when it completed (`complete: true`).
    /// `synced` is the persisted count of items that delta has walked past:
    /// its upserts, merged or kept as a local edit (not `@removed`; in
    /// window mode not the items older than the window). `total` is the
    /// folder's `total_count` in whole-mailbox mode and `None` in window mode (the window's size is
    /// unknown). `synced` can exceed `total` (the full delta re-sends the
    /// head's mail, mail arrives meanwhile, delta can repeat an item), so a
    /// UI clamps. `Store::folder_history` holds the same state before any
    /// event.
    FolderProgress {
        folder_id: String,
        synced: u64,
        total: Option<i64>,
        complete: bool,
    },
}

/// The two channel endpoints the UI keeps: commands down, events up. Dropping
/// the handle closes `cmd_tx`, which the thread observes as a disconnect and
/// shuts down on.
pub struct SyncHandle {
    pub cmd_tx: Sender<SyncCommand>,
    pub evt_rx: Receiver<SyncEvent>,
}

/// Refresh the access token when it's within this window of expiring.
const EXPIRY_SKEW_SECS: u64 = 300;
/// Quarantine an outbox op after this many failed attempts on a non-retryable
/// (4xx other than 401/429) error, so one bad op can't block the queue. A
/// `SendDraft` whose `/send` never went out is never quarantined: its first
/// such failure returns the message to Drafts (`Engine::fail_send`, #1874).
/// One whose `/send` may have gone out and whose delivery check keeps
/// failing is quarantined as before, and stays where it is.
const MAX_OP_ATTEMPTS: i64 = 5;
/// At most this many header-less rows of a folder are fetched from Graph per
/// sync pass (`Engine::repair_headerless`, #1899); the rest wait for the next.
const REPAIR_LIMIT: usize = 50;
/// `backfill_days` for the whole mailbox (#1871): every value `<= 0` means
/// no age limit, and the engine downloads every folder's whole history (the
/// recent mail first, the rest in the background).
pub const WHOLE_MAILBOX: i64 = 0;
/// How many days of recent mail a folder's head delta asks for (#1871): the
/// window's own size when `backfill_days` is smaller.
const HEAD_WINDOW_DAYS: i64 = 14;
/// A folder with at most this many messages (`total_count`, one delta page)
/// skips the head: its first sync runs the full delta inline (#1871).
const SMALL_FOLDER: i64 = 50;

/// The default of [`EngineOptions::prefetch_body_days`] (#1978).
pub const PREFETCH_BODY_DAYS: u32 = 14;
/// At most this many bodies of the messages one delta round on a stored
/// link added are fetched in that round (#1978); the prefetch takes the rest.
const NEW_BODY_LIMIT: usize = 50;
/// How many bodies one idle prefetch step fetches (#1978).
const PREFETCH_BATCH: usize = 10;

/// How `Engine::run_delta` ended: the last page's deltaLink, if it had one,
/// the upserts it walked past, the ids it added that were not stored before
/// (a round on a stored link only, with bodies on: #1978), and whether a
/// checkpoint stopped it between two pages (no deltaLink then: its pages
/// stay merged, and nothing may record the walk as complete).
struct DeltaRun {
    delta_link: Option<String>,
    walked: u64,
    new_ids: Vec<String>,
    stopped: bool,
}

/// What `Engine::background_body` did with one id.
enum BackgroundBody {
    /// It needs no body (any more), or it is skipped: no request was made.
    NotNeeded,
    /// A request was made; the work goes on.
    Fetched,
    /// An auth, throttle or transport failure (`react` has run), or a
    /// checkpoint said stop: the caller stops.
    Stop,
    /// The store failed, or Graph answered a 5xx other than a throttle's
    /// 503: the engine's own body fetches are parked until the next pass
    /// (`body_parked`), the id is not skipped, and the caller's other work
    /// goes on.
    Parked,
}

/// What `Engine::repair_headerless` did to a folder.
struct Repair {
    /// An auth, throttle or transport failure: the sync pass stops.
    abort: bool,
    /// A row was completed or deleted, so the folder needs a reload.
    changed: bool,
}
/// How `Engine::drain_outbox` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drain {
    /// Every queued op was tried: applied, or bumped/quarantined on a 4xx.
    Done,
    /// An op got a 5xx: it stays queued (attempts bumped) and the drain
    /// stopped there, keeping the order, under the outbox's own back-off
    /// (`op_retry`); or that back-off is still running and nothing was
    /// sent. The caller's sync carries on.
    Deferred,
    /// Auth, throttle or transport, or the post-send sync stopping: the
    /// caller stops too and keeps the back-off.
    Aborted,
}

impl Drain {
    /// Whether the caller's sync goes on after the drain.
    fn carry_on(self) -> bool {
        self != Drain::Aborted
    }
}

/// Offline back-off bounds (exponential between them).
const BACKOFF_MIN: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// How long `listen_for_code` waits for the browser to complete the
/// loopback redirect before giving up. Long enough for a human to actually
/// finish an interactive sign-in (pick an account, enter a password, maybe
/// MFA), short enough that the sync thread can't hang forever on a user who
/// never comes back to the browser tab.
const SIGNIN_TIMEOUT: Duration = Duration::from_secs(180);
/// How long the loopback listener waits to read the redirect request off an
/// accepted connection once the browser has connected.
const SIGNIN_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawns the sync thread with production Graph/auth endpoints, returning
/// the channels to drive it. `backfill_days` is the age limit of the mail
/// downloaded: [`WHOLE_MAILBOX`] (any value `<= 0`) for everything, else
/// the last that many days (#1871). `tick` is how long the thread waits for a
/// command before running a periodic sync pass on its own (an explicit
/// [`SyncCommand::Refresh`] also runs one immediately, regardless of how
/// much of the tick has elapsed) — lookxy passes its `Config::refresh_secs`
/// here, so the configured interval genuinely governs how often the engine
/// syncs on its own.
///
/// Runs with [`EngineOptions::default`] (plain-text bodies, the newest
/// [`PREFETCH_BODY_DAYS`] days of bodies fetched on its own); see
/// [`spawn_with_options`].
pub fn spawn(
    store_path: PathBuf,
    token_path: PathBuf,
    cfg: AuthConfig,
    backfill_days: i64,
    tick: Duration,
) -> SyncHandle {
    spawn_with_options(
        store_path,
        token_path,
        cfg,
        backfill_days,
        tick,
        EngineOptions::default(),
    )
}

/// Engine behaviour chosen once, at spawn (#1901). Build it from the
/// default so a later field does not break the caller:
/// `EngineOptions { body_format: BodyFormat::Html, ..Default::default() }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    /// The format [`SyncCommand::FetchBody`] asks Graph for (default
    /// `Text`, what lookxy shows). Bodies stored under the other format are
    /// told apart with `Store::get_body_for`.
    pub body_format: BodyFormat,
    /// The fast cycle's interval (#1885). `None` (the default) or zero
    /// keeps the single tick: the wait restarts on every command, as
    /// `spawn` always did. With `Some`, the engine keeps two deadlines: the
    /// normal pass (every folder's delta and the calendar) every `tick`,
    /// and between them a fast tick every `fast_tick` that drains the
    /// outbox and syncs only the [`SyncCommand::SetFastFolders`] folders.
    /// Commands do not move either deadline, except `Refresh` and `SignIn`,
    /// whose full pass reschedules both. A fast
    /// tick never runs while the normal pass is due, nor inside a
    /// throttle/back-off window.
    pub fast_tick: Option<Duration>,
    /// How many days of the newest mail get their bodies without a
    /// [`SyncCommand::FetchBody`] (#1978; default [`PREFETCH_BODY_DAYS`]):
    /// the messages a round on a stored delta link adds, in that round,
    /// and the rest of the window in idle time, newest first, a few per
    /// step. Drafts are never fetched. `0` turns every body fetch the
    /// engine makes on its own off: bodies come only from `FetchBody`.
    pub prefetch_body_days: u32,
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            body_format: BodyFormat::default(),
            fast_tick: None,
            prefetch_body_days: PREFETCH_BODY_DAYS,
        }
    }
}

#[cfg(test)]
impl EngineOptions {
    /// The default with the engine's own body fetches off (#1978): what the
    /// engine tests from before them run with. Their fixtures are dated
    /// (some inside any recent window, some relative to now), so with the
    /// default on, what they assert would change with the day they run.
    /// The #1978 tests turn bodies on with now-relative dates.
    fn without_prefetch() -> Self {
        EngineOptions {
            prefetch_body_days: 0,
            ..Default::default()
        }
    }
}

/// Like [`spawn`], with the engine's [`EngineOptions`].
pub fn spawn_with_options(
    store_path: PathBuf,
    token_path: PathBuf,
    cfg: AuthConfig,
    backfill_days: i64,
    tick: Duration,
    options: EngineOptions,
) -> SyncHandle {
    spawn_with_bases_and_options(
        store_path,
        token_path,
        cfg,
        backfill_days,
        "https://graph.microsoft.com/v1.0".to_string(),
        "https://login.microsoftonline.com".to_string(),
        tick,
        options,
    )
}

/// Like [`spawn`], but with the Graph base URL, auth base URL, and tick
/// injected — the seam integration tests use to point the whole engine at the
/// in-process fake server. Runs with `EngineOptions::without_prefetch`.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_with_bases(
    store_path: PathBuf,
    token_path: PathBuf,
    cfg: AuthConfig,
    backfill_days: i64,
    graph_base: String,
    auth_base: String,
    tick: Duration,
) -> SyncHandle {
    spawn_with_bases_and_options(
        store_path,
        token_path,
        cfg,
        backfill_days,
        graph_base,
        auth_base,
        tick,
        EngineOptions::without_prefetch(),
    )
}

/// [`spawn_with_bases`] with the engine's [`EngineOptions`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_with_bases_and_options(
    store_path: PathBuf,
    token_path: PathBuf,
    cfg: AuthConfig,
    backfill_days: i64,
    graph_base: String,
    auth_base: String,
    tick: Duration,
    options: EngineOptions,
) -> SyncHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (evt_tx, evt_rx) = mpsc::channel();
    let config = EngineConfig {
        store_path,
        token_path,
        cfg,
        backfill_days,
        graph_base,
        auth_base,
        tick,
        options,
    };
    thread::spawn(move || run(config, cmd_rx, evt_tx));
    SyncHandle { cmd_tx, evt_rx }
}

/// Immutable configuration handed to the sync thread at spawn time.
struct EngineConfig {
    store_path: PathBuf,
    token_path: PathBuf,
    cfg: AuthConfig,
    backfill_days: i64,
    graph_base: String,
    auth_base: String,
    tick: Duration,
    options: EngineOptions,
}

/// Thread entry point: open the store, build the engine, run startup, then the
/// command loop. A store that won't open is fatal (nothing else can proceed),
/// so it's reported and the thread exits.
fn run(config: EngineConfig, cmd_rx: Receiver<SyncCommand>, evt_tx: Sender<SyncEvent>) {
    let store = match Store::open(&config.store_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = evt_tx.send(SyncEvent::Error(format!("cannot open store: {e}")));
            return;
        }
    };
    // A quarantine reconverge an earlier run left unfinished (#1871).
    let reconverge_pending = store.reconverge_pending().unwrap_or(false);
    let mut engine = Engine {
        store,
        token: None,
        config,
        evt_tx,
        state: SyncState::Idle,
        next_retry: None,
        backoff: BACKOFF_MIN,
        backfill_done: false,
        reconverge_pending,
        parked: Vec::new(),
        expired: Vec::new(),
        next_tick: Instant::now(),
        fast_folders: Vec::new(),
        next_full: Instant::now(),
        next_fast: Instant::now(),
        hold_full: false,
        op_retry: None,
        op_backoff: BACKOFF_MIN,
        announced_scopes: None,
        cmd_rx,
        deferred: VecDeque::new(),
        shutting_down: false,
        body_skip: HashSet::new(),
        body_parked: false,
    };
    engine.startup();
    engine.main_loop();
}

/// Owns the store, the current token, and the sync state machine; all the
/// per-command and per-tick behavior hangs off it.
struct Engine {
    store: Store,
    /// `None` means "not signed in" — the engine has emitted `SignInRequired`
    /// and is waiting for a `SignIn` command.
    token: Option<TokenSet>,
    config: EngineConfig,
    evt_tx: Sender<SyncEvent>,
    state: SyncState,
    /// When set, sync passes are suspended until this instant (throttle
    /// `Retry-After` or offline back-off); commands are still serviced.
    next_retry: Option<Instant>,
    /// Current offline back-off, doubled on each consecutive transport error.
    backoff: Duration,
    /// Whether a folder enumeration has ever completed successfully.
    /// False at startup, after a failed first backfill (e.g. launched
    /// offline), and after a quarantine reconverge — while false
    /// (`needs_full_pass`), a tick runs a FULL pass (people and master
    /// categories too, and `FoldersUpdated` after every list) and the fast
    /// schedule promotes its next deadline to one, so the client recovers
    /// instead of running deltas over an empty/stale folder set. Every pass
    /// lists the folders either way (#1886).
    backfill_done: bool,
    /// Set when an outbox op is quarantined: the next full backfill pass must
    /// re-fetch *all* current server messages regardless of age (ignore the
    /// `backfill_days` cutoff) so it re-adds anything the dropped op wrongly
    /// removed locally — including messages that have since aged past the
    /// sliding window. Cleared once a pass ran every folder's delta and no
    /// folder's backfill remains (#1871: old mail comes back through the
    /// backfill), else by the backfill step that completes the last folder,
    /// or by the one that parks the last walk left (parked walks do not
    /// count, `backfill_remains`; `end_reconverge`). Persisted (`Store::clear_delta_links` sets it,
    /// `run` loads it), so a restart in the middle keeps the cutoff off.
    reconverge_pending: bool,
    /// Folders whose backfill a failure stopped until the next sync pass
    /// (#1871): one request per park, never a hot loop.
    parked: Vec<String>,
    /// Folders whose backfill found its sync state expired and started
    /// over since the last pass: a second expiry parks the folder instead,
    /// so a link that keeps expiring costs two requests a pass, not a loop.
    expired: Vec<String>,
    /// Without a fast tick: when the next tick is due (`tick` after the
    /// last tick, the startup pass or a command). Backfill steps do not move
    /// it.
    next_tick: Instant,
    /// The fast cycle's folders (`SyncCommand::SetFastFolders`), in order.
    fast_folders: Vec<String>,
    /// With a fast tick only: when the normal pass is next due.
    next_full: Instant,
    /// With a fast tick only: when the next fast tick is due.
    next_fast: Instant,
    /// With a fast tick only: the full pass that last ran (normal, startup,
    /// `Refresh`, sign-in) ran to its end and its folder list still never
    /// succeeded (`needs_full_pass`: it failed non-fatally, a 403, a 5xx
    /// other than 503, a parse error), so the fast deadlines do
    /// not repeat it before `next_full`; a quarantine since
    /// (`reconverge_pending`) still promotes one.
    hold_full: bool,
    /// When set, a drain sends no op before this instant: the first queued
    /// op got a 5xx (#1885). Only the outbox waits; the folders still sync.
    op_retry: Option<Instant>,
    /// The outbox's own back-off after a 5xx, doubled per consecutive 5xx
    /// (`BACKOFF_MIN`..`BACKOFF_MAX`), reset when an op goes through.
    op_backoff: Duration,
    /// The grant the last [`SyncEvent::Scopes`] announced, as a sorted,
    /// de-duplicated set of normalised scopes; `None` after
    /// `enter_signin`, so a new session announces again.
    announced_scopes: Option<Vec<String>>,
    /// The commands from the UI. The main loop waits on it; long work
    /// drains it at each `checkpoint` (#1978).
    cmd_rx: Receiver<SyncCommand>,
    /// Commands a `checkpoint` took off the channel and kept for later
    /// (every one but `FetchBody`), in arrival order: the main loop runs
    /// them before it reads the channel again or does any idle work.
    deferred: VecDeque<SyncCommand>,
    /// A `Shutdown` came, or the UI dropped its handle, while work was
    /// running: the work stops at its next checkpoint, and the main loop
    /// returns once the commands that came before it have run.
    shutting_down: bool,
    /// Ids whose body a fetch the engine made on its own (#1978) failed
    /// for a reason a retry would not fix (a 404, another 4xx, a parse
    /// error): neither the new-mail fetch nor the prefetch asks for them
    /// again in this run. A `FetchBody` still does.
    body_skip: HashSet<String>,
    /// A store failure or a Graph 5xx stopped the engine's own body
    /// fetches (#1978) until the next sync pass, which clears it: one try a
    /// pass, never a loop, and a store or a service that recovered heals.
    body_parked: bool,
}

impl Engine {
    // --- Startup & main loop --------------------------------------------

    /// Loads the cached token (refreshing if near expiry) and, if signed in,
    /// runs the initial folder + delta backfill. Otherwise emits
    /// `SignInRequired` and waits for a `SignIn` command in the loop.
    fn startup(&mut self) {
        match tokencache::load(&self.config.token_path) {
            Ok(Some(t)) => {
                self.token = Some(t);
                self.refresh_if_near_expiry();
                // A refresh just now has announced already; then this is
                // the same grant and sends nothing.
                self.announce_scopes();
            }
            // No cache yet, or a cache that failed to decrypt/parse: either way
            // the user has to (re-)sign in.
            Ok(None) | Err(_) => {
                self.enter_signin();
                return;
            }
        }
        if self.token.is_some() {
            self.sync_pass(true);
        }
    }

    /// Blocks on the command channel, running a sync tick whenever the wait
    /// times out (or a back-off deadline elapses). Returns when a `Shutdown`
    /// arrives or the command channel is dropped.
    ///
    /// While a folder's backfill is ready (#1871), or recent bodies are to
    /// prefetch (#1978), the wait is zero: a queued command runs first, then
    /// a due deadline (`due`), then one prefetch batch, and only when there
    /// is none one backfill page. A `FetchBody` waits for at most the
    /// request in flight (`checkpoint`); any other command, and a due tick,
    /// for the idle step in flight: one prefetch body (the batch then ends),
    /// one backfill page, or a backfill's last step (its last round on the
    /// folder's own link, the repair, and new-mail bodies, which end early).
    ///
    /// The commands a `checkpoint` kept (`deferred`) run first, in order,
    /// before the channel is read again; then a `Shutdown` a checkpoint saw
    /// ends the loop.
    fn main_loop(&mut self) {
        // The startup pass schedules the deadlines like any other.
        self.after_pass_if_fast();
        self.next_tick = Instant::now() + self.config.tick;
        loop {
            if let Some(cmd) = self.deferred.pop_front() {
                self.handle_command(cmd);
                // Without a fast tick the wait restarts on every command.
                self.next_tick = Instant::now() + self.config.tick;
                continue;
            }
            if self.shutting_down {
                return;
            }
            let prefetch = self.next_prefetch();
            let backfill = if prefetch.is_none() {
                self.next_backfill()
            } else {
                None
            };
            let wait = if prefetch.is_some() || backfill.is_some() {
                Duration::ZERO
            } else {
                self.recv_timeout()
            };
            match self.cmd_rx.recv_timeout(wait) {
                Ok(SyncCommand::Shutdown) => return,
                Ok(cmd) => {
                    self.handle_command(cmd);
                    // Without a fast tick the wait restarts on every command.
                    self.next_tick = Instant::now() + self.config.tick;
                }
                Err(RecvTimeoutError::Timeout) => match (prefetch, backfill) {
                    (Some((since, ids)), _) if !self.due() => self.prefetch_step(&since, &ids),
                    (None, Some(h)) if !self.due() => self.backfill_step(h),
                    _ => self.on_timer(),
                },
                // The UI dropped its `SyncHandle`: no one is listening anymore.
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Takes every command waiting on the channel, between two requests of
    /// long work (#1978): a `FetchBody` is served at once, so a click costs
    /// one round trip plus the request in flight, not a whole pass; every
    /// other command is kept (`deferred`) for the main loop, in order; a
    /// `Shutdown` or a dropped handle sets `shutting_down`. Returns whether
    /// the work goes on: `false` when shutting down, or when a `FetchBody`
    /// served here hit an auth, throttle or transport failure (`react`
    /// returned true). A back-off set before the checkpoint does not stop
    /// it: `Refresh` runs a pass during one.
    fn checkpoint(&mut self) -> bool {
        if self.shutting_down {
            return false;
        }
        let mut carry_on = true;
        loop {
            match self.cmd_rx.try_recv() {
                Ok(SyncCommand::FetchBody { id }) => {
                    if self.fetch_body(&id) {
                        carry_on = false;
                    }
                }
                Ok(SyncCommand::Shutdown) | Err(TryRecvError::Disconnected) => {
                    self.shutting_down = true;
                    return false;
                }
                Ok(cmd) => self.deferred.push_back(cmd),
                Err(TryRecvError::Empty) => return carry_on,
            }
        }
    }

    /// A timed wake: the tick (`on_tick`), or with a fast tick a deadline
    /// (`on_deadline`). Then a `next_retry` or `op_retry` that has passed
    /// is dropped, as the tick consumed its wake: one the tick left in the
    /// past (a pass that ended early, a signed-out engine) would otherwise
    /// keep `due` true and run a pass at every zero wait of a backfill. A
    /// passed instant and none mean the same everywhere else.
    fn on_timer(&mut self) {
        match self.fast_tick() {
            None => {
                self.on_tick();
                self.next_tick = Instant::now() + self.config.tick;
            }
            Some(fast) => self.on_deadline(fast),
        }
        let now = Instant::now();
        if self.next_retry.is_some_and(|t| t <= now) {
            self.next_retry = None;
        }
        if self.op_retry.is_some_and(|t| t <= now) {
            self.op_retry = None;
        }
    }

    /// Whether a timed wake is due now (#1871), checked before a backfill
    /// page: the instants `recv_timeout` would wake at. Without a fast tick:
    /// the tick, a passed back-off (`next_retry`: a throttled or offline
    /// pass is retried at its end) or a passed `op_retry` while signed in (a
    /// 5xx-deferred op is resent at its back-off). With one: either
    /// deadline, never inside a back-off.
    fn due(&self) -> bool {
        let now = Instant::now();
        match self.fast_tick() {
            None => {
                now >= self.next_tick
                    || self.next_retry.is_some_and(|t| now >= t)
                    || (self.token.is_some() && self.op_retry.is_some_and(|t| now >= t))
            }
            Some(_) => {
                !self.next_retry.is_some_and(|t| now < t)
                    && now >= self.next_full.min(self.next_fast)
            }
        }
    }

    /// How long to block for the next command: until the back-off deadline if
    /// one is pending, otherwise the next tick (`next_tick`: the configured
    /// tick after the last tick or command), and no later than a
    /// deferred op's `op_retry`. With a fast tick: until the nearer of the
    /// two deadlines, but never before a back-off deadline.
    fn recv_timeout(&self) -> Duration {
        if self.fast_tick().is_some() {
            let mut at = self.next_full.min(self.next_fast);
            if let Some(t) = self.next_retry {
                at = at.max(t);
            }
            return at.saturating_duration_since(Instant::now());
        }
        let now = Instant::now();
        let wait = match self.next_retry {
            Some(t) => t.saturating_duration_since(now),
            None => self.next_tick.saturating_duration_since(now),
        };
        // Also wake for an op deferred by a 5xx (`op_retry`): the tick at
        // that instant drains it, as a back-off deadline did before #1885.
        // Only a future one while signed in, so a passed `op_retry` never
        // makes a zero wait.
        match self.op_retry {
            Some(t) if self.token.is_some() && t > now => wait.min(t - now),
            _ => wait,
        }
    }

    /// The fast cycle's interval, when it is on (`Some(ZERO)` is off).
    fn fast_tick(&self) -> Option<Duration> {
        self.config.options.fast_tick.filter(|d| !d.is_zero())
    }

    /// With a fast tick, `after_full_pass` for a full pass that ran outside
    /// the schedule: startup, `Refresh`, a sign-in. Without one, nothing.
    fn after_pass_if_fast(&mut self) {
        if let Some(fast) = self.fast_tick() {
            self.after_full_pass(fast);
        }
    }

    /// Whether the next normal pass must be a full one: no folder
    /// enumeration has completed (launched offline, or a quarantine
    /// reconverge reset it), or no folder is stored. Every pass lists the
    /// folders (#1886); a full one also refreshes people and master
    /// categories, and the fast schedule runs it at its next deadline.
    fn needs_full_pass(&self) -> bool {
        !self.backfill_done || self.store.folders().map(|f| f.is_empty()).unwrap_or(true)
    }

    /// Whether the full pass that just ran (normal, startup, `Refresh`,
    /// sign-in) ran to its end and still `needs_full_pass`: not cut short
    /// by a lost sign-in or a back-off, so its folder list failed
    /// non-fatally.
    fn full_pass_held(&self) -> bool {
        let cut_short = self.token.is_none() || self.next_retry.is_some_and(|t| t > Instant::now());
        !cut_short && self.needs_full_pass()
    }

    /// A deadline of the fast-tick schedule elapsed. Inside a back-off
    /// window nothing runs. The normal pass runs when due: its deadline, or
    /// one that must be full (`needs_full_pass`), so a quarantine or an offline
    /// start recovers at the next fast deadline rather than a tick later.
    /// When the last pass ran to its end and its folder list still failed
    /// (`hold_full`), only a quarantine since promotes one, so a folder list
    /// that keeps failing is asked for once a tick, not at every fast
    /// deadline. Otherwise a fast tick runs when its deadline passed. An
    /// aborted normal pass is retried at its back-off deadline, as without a
    /// fast tick.
    fn on_deadline(&mut self, fast: Duration) {
        let now = Instant::now();
        if self.next_retry.is_some_and(|t| now < t) {
            return;
        }
        let promote = self.needs_full_pass() && (!self.hold_full || self.reconverge_pending);
        if now >= self.next_full || promote {
            self.on_tick();
            self.after_full_pass(fast);
        } else if now >= self.next_fast {
            self.on_fast_tick();
            self.next_fast = Instant::now() + fast;
        }
    }

    /// With a fast tick: schedules both deadlines after a full pass (a normal
    /// one, the startup one, `Refresh`'s, a sign-in's) and decides
    /// `hold_full`. A pass a throttle or back-off cut short is due again at
    /// its deadline.
    fn after_full_pass(&mut self, fast: Duration) {
        let now = Instant::now();
        self.next_fast = now + fast;
        self.next_full = match self.next_retry {
            Some(t) if self.token.is_some() && t > now => t,
            _ => now + self.config.tick,
        };
        self.hold_full = self.full_pass_held();
    }

    /// Periodic tick: skip while signed out or inside a back-off window,
    /// otherwise list the folder tree, re-run every folder's delta and drain
    /// the outbox (`sync_pass`). When the initial backfill never completed
    /// (launched offline, or a quarantine reconverge reset it) — or the
    /// folder set is empty — the pass is a FULL one (people and master
    /// categories too), so the client recovers under back-off rather than
    /// idling forever on an empty/stale folder set.
    fn on_tick(&mut self) {
        if self.token.is_none() {
            return;
        }
        if let Some(t) = self.next_retry {
            if Instant::now() < t {
                return;
            }
        }
        let needs_full = self.needs_full_pass();
        self.sync_pass(needs_full);
        // Interleave a calendar refresh with the periodic mail sync (see
        // `SyncCommand::RefreshCalendar` for the on-demand path) — but ONLY
        // when the mail pass just above didn't already drop the engine into
        // sign-in (`token` cleared) or a back-off/throttle wait (`next_retry`
        // set). Both `token.is_none()` and `next_retry.is_some()` are false
        // at this point unless `sync_pass` itself just set one of them (see
        // the guards at the top of this function), so this is exactly "did
        // this tick's mail sync just fail the same way `refresh_calendar`
        // would fail too". Skipping it there avoids a second, doomed Graph
        // call in the same tick — which, on a transport error, would call
        // `go_offline` a second time and double the exponential back-off in
        // one offline tick instead of advancing it by one step.
        if self.token.is_some() && self.next_retry.is_none() && self.checkpoint() {
            self.refresh_calendar();
        }
    }

    /// Dispatches one UI command.
    fn handle_command(&mut self, cmd: SyncCommand) {
        match cmd {
            SyncCommand::Refresh => {
                // F9 resends a deferred op now; its back-off keeps growing
                // if it fails again.
                self.op_retry = None;
                self.sync_pass(true);
                self.after_pass_if_fast();
            }
            SyncCommand::SyncFolder { id } => self.sync_one_folder(&id),
            SyncCommand::SetFastFolders { ids } => {
                let mut set: Vec<String> = Vec::with_capacity(ids.len());
                for id in ids {
                    if !set.contains(&id) {
                        set.push(id);
                    }
                }
                self.fast_folders = set;
            }
            SyncCommand::SignIn => self.sign_in(),
            SyncCommand::MarkRead { id, read } => {
                self.store.set_read(&id, read);
                self.enqueue_and_drain(OutboxOp::MarkRead { id, read });
            }
            SyncCommand::SetFlag { id, flagged } => {
                self.store.set_flag(&id, flagged);
                self.enqueue_and_drain(OutboxOp::SetFlag { id, flagged });
            }
            SyncCommand::SetCategories { id, categories } => {
                self.store.set_categories(&id, &categories);
                self.enqueue_and_drain(OutboxOp::SetCategories { id, categories });
            }
            SyncCommand::RefreshCategories => self.refresh_master_categories(),
            SyncCommand::FetchSchedule {
                schedules,
                start_utc,
                end_utc,
                interval_minutes,
            } => self.fetch_schedule(schedules, &start_utc, &end_utc, interval_minutes),
            SyncCommand::Move { id, dest } => {
                // Optimistic local re-file, same pattern as MarkRead/SetFlag/
                // Delete. Graph mints a new id on move; the next delta
                // reconciles it (old id `@removed`, new id added), so we keep
                // the local id and only update its folder here. Graph stays the
                // source of truth, so we enqueue regardless — but a local
                // failure (e.g. `dest` isn't a stored folder yet: `folder_id`
                // is a NOT NULL foreign key) must be surfaced, not swallowed.
                // `id`/`dest` are message/folder ids, never secrets.
                if let Err(e) = self.store.move_message(&id, &dest) {
                    self.emit(SyncEvent::Error(format!(
                        "local move of {id} to {dest} failed: {e}"
                    )));
                }
                self.enqueue_and_drain(OutboxOp::Move { id, dest });
            }
            SyncCommand::Delete { id } => {
                // Resolve and delete in one statement: a reconciled draft's
                // `local:` id leads to its Graph id only while the row exists
                // (#1884), so the queued op carries the id of the row deleted.
                // When the UI already deleted the row (with the id it sends
                // here, as lookxy does), there is nothing to resolve against:
                // `current_id` then leaves the id as it is.
                let target = match self.store.delete_message_resolved(&id) {
                    Ok(Some(deleted)) => deleted,
                    _ => self.store.current_id(&id).unwrap_or(id),
                };
                self.enqueue_and_drain(OutboxOp::Delete { id: target });
            }
            SyncCommand::FetchBody { id } => {
                self.fetch_body(&id);
            }
            SyncCommand::FetchAttachments { message_id } => self.fetch_attachments(&message_id),
            SyncCommand::SaveAttachment {
                message_id,
                attachment_id,
                dest,
            } => self.save_attachment(&message_id, &attachment_id, dest),
            SyncCommand::SaveItemAttachment {
                message_id,
                attachment_id,
                dest_base,
            } => self.save_item_attachment(&message_id, &attachment_id, dest_base),
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
            SyncCommand::FetchAutomaticReplies => self.fetch_automatic_replies(),
            SyncCommand::SetAutomaticReplies { replies } => self.set_automatic_replies(replies),
            SyncCommand::FetchProfile => self.fetch_profile(),
            SyncCommand::FetchPhoto => self.fetch_photo(),
            SyncCommand::SignOut => self.sign_out(),
            SyncCommand::FetchInlineImage {
                message_id,
                attachment_id,
                content_id,
            } => self.fetch_inline_image(&message_id, &attachment_id, &content_id),
            SyncCommand::SaveDraft { id } => {
                self.enqueue_and_drain(OutboxOp::SaveDraft { id });
            }
            SyncCommand::SendDraft { id } => {
                // Queue it first: a send that cannot be queued is a failed
                // send (#1874), reported before anything optimistic touched
                // the row, so the message simply stays a draft.
                if let Err(e) = self
                    .store
                    .enqueue_op(&OutboxOp::SendDraft { id: id.clone() })
                {
                    self.emit(SyncEvent::SendFailed {
                        id,
                        error: e.to_string(),
                    });
                    return;
                }
                // Optimistic local write, same spirit as MarkRead/SetFlag/
                // Move/Delete: mark the row no longer a draft, and — if the
                // Sent folder has synced — re-file it there right away so
                // the UI reflects "sent" before the outbox drain actually
                // reaches Graph. `id` is whatever the caller currently
                // addresses this draft by (a `local:` id if it was never
                // pushed, else its Graph id); `apply_op` resolves/reconciles
                // the Graph side, this only touches the local row. A send
                // that fails for good undoes it (`fail_send`).
                self.store.mark_sent(&id);
                if let Some(sent_id) = self.sent_items_folder_id() {
                    let _ = self.store.move_message(&id, &sent_id);
                }
                self.emit(SyncEvent::Sending { id });
                self.drain_queued();
            }
            SyncCommand::ComposeReply { id, all } => self.compose_reply(&id, all),
            SyncCommand::ComposeForward { id } => self.compose_forward(&id),
            SyncCommand::RefreshCalendar => self.refresh_calendar(),
            SyncCommand::RespondEvent {
                id,
                kind,
                comment,
                proposed_start_utc,
                proposed_end_utc,
            } => {
                // Optimistic local write, same pattern as MarkRead/SetFlag/
                // Move/Delete: reflect the RSVP immediately, before the
                // outbox drain even reaches Graph.
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
            SyncCommand::CreateEvent { id } => {
                self.enqueue_and_drain(OutboxOp::CreateEvent { id });
                self.emit(SyncEvent::CalendarUpdated);
            }
            SyncCommand::UpdateEvent { id } => {
                self.enqueue_and_drain(OutboxOp::UpdateEvent { id });
                self.emit(SyncEvent::CalendarUpdated);
            }
            SyncCommand::DeleteEvent { id } => {
                self.enqueue_and_drain(OutboxOp::DeleteEvent { id });
                self.emit(SyncEvent::CalendarUpdated);
            }
            // Handled in `main_loop` so the thread can actually return.
            SyncCommand::Shutdown => {}
        }
    }

    // --- Sync passes -----------------------------------------------------

    /// One sync pass: re-enumerate folders (#1886: every pass, so names,
    /// parents, new and deleted folders and counts follow the server), then
    /// delta-sync every stored folder and drain the outbox. A `full` pass
    /// (startup, `Refresh`, sign-in, a pass that `needs_full_pass`) also
    /// refreshes people and master categories. Auth/throttle/transport
    /// failures abort the pass (state already set); per-folder 4xx/parse
    /// errors are surfaced and skipped so one folder can't sink the rest.
    fn sync_pass(&mut self, full: bool) {
        if self.token.is_none() {
            return;
        }
        self.set_state(SyncState::Syncing);
        // A pass gives every parked backfill another try (#1871), and the
        // engine's own body fetches after a store failure or a Graph 5xx
        // (#1978).
        self.parked.clear();
        self.expired.clear();
        self.body_parked = false;

        if !self.sync_folders(full) {
            return;
        }
        let folders = match self.store.folders() {
            Ok(f) => f,
            Err(e) => {
                self.emit(SyncEvent::Error(e.to_string()));
                self.settle_state();
                return;
            }
        };
        for f in &folders {
            if !self.checkpoint() || !self.sync_folder_delta(&f.id) {
                return;
            }
        }
        // Every folder's delta just completed without a hard failure, so a
        // pending reconverge (upsert-everything, cutoff ignored) is satisfied
        // once no folder's backfill remains either (#1871: the old mail comes
        // back through it); else the step that completes the last folder,
        // or parks the last walk left (parked walks do not count,
        // `backfill_remains`), clears it. Clear it before draining: if the drain quarantines a
        // fresh op it will re-arm reconverge for the next pass.
        if !self.backfill_remains() {
            self.end_reconverge();
        }
        // A drain that a throttle, an offline error or a lost sign-in cut
        // short keeps its back-off: the pass stops here, as after a folder.
        // An op deferred by a 5xx does not stop the pass; it waits for the
        // next drain.
        if !self.checkpoint() || !self.drain_outbox().carry_on() {
            return;
        }
        // A full pass without a hard failure clears any offline back-off.
        self.clear_backoff();

        // Refresh the local contact index from whatever mail is now stored
        // (cheap, idempotent). Best-effort: a failure here must not fail the pass.
        let _ = self.store.refresh_local_contacts();

        // On a full pass (startup, sign-in, `Refresh`), augment contacts
        // from the corporate directory. Best-effort and
        // gracefully degrading: People.Read may be denied (403), blocked by
        // Conditional Access, or offline — in every case we skip it silently
        // and the locally-mined contacts still power autocomplete.
        if full {
            if !self.checkpoint() {
                return;
            }
            self.sync_people();
            self.refresh_master_categories();
        }

        self.settle_state();
    }

    /// One folder's sync, as `SyncCommand::SyncFolder` and the fast cycle do
    /// it (#1885), without the drain: its delta, then its counts
    /// (`GET /me/mailFolders/{id}`) into the store, with `FoldersUpdated`
    /// when they changed. A non-fatal failure (a 4xx or parse error) is
    /// surfaced and skipped. Returns `false` when the caller must stop (an
    /// auth, throttle or transport failure, already handled by `react`).
    fn refresh_folder(&mut self, folder_id: &str) -> bool {
        if !self.sync_folder_delta(folder_id) {
            return false;
        }
        match self.with_auth(|c| c.get_folder_counts(folder_id)) {
            Ok((total, unread)) => {
                if let Ok(true) = self.store.set_folder_counts(folder_id, total, unread) {
                    self.emit(SyncEvent::FoldersUpdated);
                }
                true
            }
            Err(e) => !self.react(e),
        }
    }

    /// `SyncCommand::SyncFolder`: an unknown id is an `Error` with no Graph
    /// call; signed out or inside a back-off window, nothing (as a tick);
    /// else drain the outbox and sync that folder (`refresh_folder`). A
    /// sync cut short keeps the state its failure set (Offline,
    /// SignInRequired, or settled by a throttle).
    fn sync_one_folder(&mut self, id: &str) {
        match self.store.folders() {
            Ok(folders) if folders.iter().any(|f| f.id == id) => {}
            Ok(_) => {
                self.emit(SyncEvent::Error(format!(
                    "sync folder: unknown folder {id}"
                )));
                return;
            }
            Err(e) => {
                self.emit(SyncEvent::Error(e.to_string()));
                return;
            }
        }
        if self.token.is_none() || self.next_retry.is_some_and(|t| Instant::now() < t) {
            return;
        }
        self.set_state(SyncState::Syncing);
        if self.drain_outbox().carry_on() && self.refresh_folder(id) {
            self.clear_backoff();
            self.settle_state();
        }
    }

    /// One fast tick (#1885): drain the outbox, then sync the fast folders
    /// that already have a delta link (`refresh_folder`). A folder that
    /// never synced is left to the normal pass, which backfills it: the fast
    /// cycle only runs deltas (#1871). The folders are picked after the
    /// drain, so a quarantine in it (which clears every delta link) starts
    /// no reconverge here; the next deadline runs the normal pass. Sets no
    /// `Syncing` state (it runs every few seconds); a complete tick only
    /// settles, and one cut short keeps the state its failure set, so
    /// Offline, PendingOps and SignInRequired still show.
    fn on_fast_tick(&mut self) {
        if self.token.is_none() || self.next_retry.is_some_and(|t| Instant::now() < t) {
            return;
        }
        if !self.drain_outbox().carry_on() {
            return;
        }
        let stored = match self.store.folders() {
            Ok(f) => f,
            Err(e) => {
                self.emit(SyncEvent::Error(e.to_string()));
                return;
            }
        };
        let ids: Vec<String> = self
            .fast_folders
            .iter()
            .filter(|id| {
                stored
                    .iter()
                    .any(|f| &f.id == *id && f.delta_link.is_some())
            })
            .cloned()
            .collect();
        for id in &ids {
            if !self.checkpoint() || !self.refresh_folder(id) {
                return;
            }
        }
        self.clear_backoff();
        self.settle_state();
    }

    /// Fetch attendee availability and emit `ScheduleFetched`; a Graph failure
    /// goes through `react`. Same signed-in guard as `fetch_body`.
    fn fetch_schedule(
        &mut self,
        schedules: Vec<String>,
        start_utc: &str,
        end_utc: &str,
        interval_minutes: i64,
    ) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_schedule(&schedules, start_utc, end_utc, interval_minutes)) {
            Ok(entries) => self.emit(SyncEvent::ScheduleFetched { entries }),
            Err(e) => {
                self.react(e);
            }
        }
    }

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

    /// Best-effort `/me/people` fetch → contact upserts. Any error (insufficient
    /// scope, Conditional Access, offline, parse) is swallowed: directory
    /// suggestions are a bonus on top of local contacts, never a hard
    /// dependency, so this never fails a sync pass or emits an error event.
    fn sync_people(&mut self) {
        let people = match self.with_auth(|c| c.people()) {
            Ok(p) => p,
            Err(_) => return, // graceful degradation — local contacts carry on
        };
        for p in people {
            let _ = self.store.upsert_contact(&crate::store::Contact {
                name: p.name,
                address: p.address,
                source: "graph".to_string(),
                last_seen: String::new(),
                frequency: 0,
                relevance: Some(p.rank),
            });
        }
    }

    /// GET the folder tree, upsert every folder and, after a complete
    /// enumeration, remove the folders it no longer lists (#1886:
    /// `Store::retain_folders`, which keeps the drafts sentinel while it
    /// holds a draft). A partial listing (a `childFolders` call that failed)
    /// still upserts what it listed and removes nothing; an empty one
    /// removes nothing either. Removal runs before the pass's deltas, so a
    /// deleted folder gets no delta. A `full` pass emits `FoldersUpdated`
    /// after every successful list; a normal tick only when a folder row
    /// was added, changed or removed. Returns `false` if the pass should
    /// abort (auth/throttle/transport).
    ///
    /// If the real Drafts folder (`well_known_name = "drafts"`) is present in
    /// this fetch, also re-files (`Store::adopt_sentinel_drafts`) any
    /// messages still sitting under the local drafts sentinel folder — see
    /// `Store::drafts_folder_id` for why that sentinel exists and the Task 6
    /// report for the gap this closes. Harmless (a no-op `UPDATE` matching
    /// zero rows) once already adopted, so this doesn't need a "did we
    /// already do this" flag.
    fn sync_folders(&mut self, full: bool) -> bool {
        match self.with_auth(|c| c.list_folders()) {
            Ok(listing) => {
                let mut changed = false;
                for f in &listing.folders {
                    changed |= self.store.upsert_folder(f).unwrap_or(false);
                }
                if let Some(drafts) = listing
                    .folders
                    .iter()
                    .find(|f| f.well_known_name.as_deref() == Some("drafts"))
                {
                    let _ = self.store.adopt_sentinel_drafts(&drafts.id);
                }
                let mut carry_on = true;
                if listing.complete && !listing.folders.is_empty() {
                    let (keep, go_on) = self.folders_to_keep(&listing.folders);
                    carry_on = go_on;
                    match self.store.retain_folders(&keep) {
                        Ok(removed) => changed |= removed > 0,
                        Err(e) => self.emit(SyncEvent::Error(e.to_string())),
                    }
                }
                // The folders have been enumerated at least once: a pass no
                // longer needs to be a full one for them (`needs_full_pass`).
                self.backfill_done = true;
                if full || changed {
                    self.emit(SyncEvent::FoldersUpdated);
                }
                carry_on
            }
            Err(e) => !self.react(e),
        }
    }

    /// The folder ids `retain_folders` keeps after a complete listing of
    /// `listed`: those listed, plus each stored folder missing from it that
    /// Graph does not confirm deleted (#1886). The tree walk is many
    /// requests, not a snapshot, so a folder moved during it (into a parent
    /// already read, or past an offset `nextLink`) is missing although it
    /// exists: each missing folder is asked for (`folder_exists`) and
    /// removed only on a 404. One that exists, or whose GET fails
    /// non-fatally, is kept for the next tick to list. A fatal failure
    /// (auth, throttle, transport, through `react`) keeps every folder not
    /// yet confirmed gone and returns `false`: the pass stops. The drafts
    /// sentinel is never asked for; `retain_folders` keeps it while it
    /// holds a draft.
    fn folders_to_keep(&mut self, listed: &[MailFolder]) -> (Vec<String>, bool) {
        let mut keep: Vec<String> = listed.iter().map(|f| f.id.clone()).collect();
        let missing: Vec<String> = match self.store.folders() {
            Ok(stored) => stored
                .into_iter()
                .map(|f| f.id)
                .filter(|id| id != LOCAL_DRAFTS_SENTINEL_FOLDER_ID && !keep.contains(id))
                .collect(),
            Err(e) => {
                self.emit(SyncEvent::Error(e.to_string()));
                return (keep, true);
            }
        };
        let mut carry_on = true;
        for id in missing {
            if !carry_on {
                keep.push(id);
                continue;
            }
            match self.with_auth(|c| c.folder_exists(&id)) {
                Ok(false) => {}
                Ok(true) => keep.push(id),
                Err(e) => {
                    carry_on = !self.react(e);
                    keep.push(id);
                }
            }
        }
        (keep, carry_on)
    }

    /// Delta-sync one folder, in a sync pass, `SyncFolder` or a fast tick.
    /// A folder with a stored `deltaLink` (a head, a full or a pre-#1871
    /// link) runs a round on it, upserting/deleting every item with no age
    /// cutoff, and saves the new `deltaLink`. A folder that never synced
    /// gets its phase 1 (#1871): the *head* delta, only its recent mail
    /// (`head_since`, filtered by the server, newest first), whose
    /// deltaLink becomes the folder's (`Store::set_head_link`); its older
    /// mail follows in idle time (`backfill_step`). A folder of at most
    /// `SMALL_FOLDER` messages skips the head: its full delta runs here to
    /// the end, with the `backfill_days` cutoff unless a quarantine
    /// reconverge (`reconverge_pending`) asks for every item, and completes
    /// the folder. Returns `false` if the whole pass should abort.
    ///
    /// A delta item may be partial (#1899): Graph can return only what
    /// changed (`{id, isRead}`), or a bare `{id}` for an item created and
    /// changed between two rounds. Every item is merged
    /// (`Store::merge_message`), so a property the item lacks never blanks a
    /// stored one; a new partial item is inserted header-less and completed
    /// by `repair_headerless` right after the folder's pages. The cutoff
    /// skips only an item whose `receivedDateTime` it can read, so a partial
    /// item in a backfill is kept (and completed) even if old. A partial
    /// update merged into a complete row is not reported; header-less rows
    /// are, by `repair_headerless`.
    ///
    /// `MessagesUpdated` is emitted once the pages are stored, before the
    /// repair (so an aborted repair still shows them), and again after a
    /// repair that changed a row.
    fn sync_folder_delta(&mut self, folder_id: &str) -> bool {
        let stored = self.store.get_delta_link(folder_id).ok().flatten();
        // An unreadable count gets the head, like a big folder.
        let small = stored.is_none()
            && self
                .folder_total(folder_id)
                .is_some_and(|n| n <= SMALL_FOLDER);
        let (cursor, cutoff) = match stored {
            Some(link) => (DeltaCursor::Link(link), None),
            None if small => (
                DeltaCursor::Full(folder_id.to_string()),
                self.backfill_cutoff(),
            ),
            None => (
                DeltaCursor::Head {
                    folder_id: folder_id.to_string(),
                    since: self.head_since(),
                },
                None,
            ),
        };
        // A head or a small folder's full delta, not a round on a stored link.
        // A fresh delta carries every message it covers, so it must not
        // overwrite a local edit (`merge_delta_items`): a reconverge re-runs
        // phase 1 while the user's state is live.
        let started_fresh = !matches!(cursor, DeltaCursor::Link(_));
        let pending = if started_fresh {
            match self.protected_ids() {
                Ok(ids) => Some(ids),
                Err(e) => {
                    self.emit(SyncEvent::Error(e.to_string()));
                    return true;
                }
            }
        } else {
            None
        };
        let run = match self.run_delta(folder_id, cursor, cutoff.as_deref(), pending.as_deref()) {
            Ok(run) => run,
            // `react` returns true when the pass must abort; a non-fatal
            // error (a 4xx/parse on this folder) is surfaced there and we
            // stop this folder but keep the pass going.
            Err(e) => return !self.react(e),
        };
        if run.stopped {
            // A checkpoint stopped the walk between two pages: what it
            // merged shows, no link is stored, and the next round asks again.
            self.emit(SyncEvent::MessagesUpdated {
                folder_id: folder_id.to_string(),
            });
            return false;
        }
        if let Some(delta) = run.delta_link {
            if !started_fresh {
                let _ = self.store.set_delta_link(folder_id, &delta);
            } else if small {
                let scope = if cutoff.is_some() {
                    SCOPE_WINDOW
                } else {
                    SCOPE_ALL
                };
                if self
                    .store
                    .complete_backfill(folder_id, &delta, scope, run.walked as i64)
                    .is_ok()
                {
                    self.emit_progress(folder_id, run.walked, true);
                }
            } else {
                let _ = self.store.set_head_link(folder_id, &delta);
            }
        }
        self.emit(SyncEvent::MessagesUpdated {
            folder_id: folder_id.to_string(),
        });
        let repair = self.repair_headerless(folder_id);
        if repair.changed {
            self.emit(SyncEvent::MessagesUpdated {
                folder_id: folder_id.to_string(),
            });
        }
        // New mail comes with its body (#1978), once the repair gave any
        // partial item its headers.
        !repair.abort && self.fetch_new_bodies(&run.new_ids)
    }

    /// Pages a delta from `cursor` to its end into `folder_id`
    /// (`merge_delta_items` with `cutoff` and `protect`), returning its
    /// deltaLink (if the last page had one) and how many upserts it walked
    /// past, with the ids it added (`merge_delta_items`). A Graph failure is
    /// returned as it is, for the caller to `react` to; the pages before it
    /// stay merged. A `checkpoint` between two pages may stop it (#1978):
    /// `stopped`, with no deltaLink.
    fn run_delta(
        &mut self,
        folder_id: &str,
        mut cursor: DeltaCursor,
        cutoff: Option<&str>,
        protect: Option<&[String]>,
    ) -> Result<DeltaRun, GraphError> {
        let mut walked = 0;
        let mut new_ids = Vec::new();
        loop {
            let page = self.with_auth(|c| c.delta(cursor.clone()))?;
            walked += self.merge_delta_items(folder_id, &page.items, cutoff, protect, &mut new_ids);
            match page.next_link {
                Some(next) => {
                    if !self.checkpoint() {
                        return Ok(DeltaRun {
                            delta_link: None,
                            walked,
                            new_ids,
                            stopped: true,
                        });
                    }
                    cursor = DeltaCursor::Link(next);
                }
                None => {
                    return Ok(DeltaRun {
                        delta_link: page.delta_link,
                        walked,
                        new_ids,
                        stopped: false,
                    });
                }
            }
        }
    }

    /// Merges a delta page's items into `folder_id`: upserts (skipping one
    /// received before `cutoff`, when its `receivedDateTime` is known) and
    /// `@removed` deletes. Returns how many upserts it walked past (within
    /// the cutoff, whatever became of them); a row that fails to write is
    /// skipped.
    ///
    /// With `protect` (`protected_ids`, sorted) the page is a fresh
    /// delta's (#1871: a head, a small folder's full delta, a backfill
    /// page), which carries every message it covers, not only what changed:
    /// it never merges over a local edit. It skips those ids, and merges
    /// every other row with `Store::merge_message_unless_edited`, which
    /// leaves a draft holding an unsaved edit alone in the same statement
    /// as the write (compose saves on lookxy's own connection, at any
    /// time). Every other row is merged, drafts included (a quarantine
    /// reconverge repairs a draft a dropped op left wrong). A round on a
    /// stored link (`None`) merges everything, as Graph reports a change
    /// there.
    ///
    /// On such a round, with bodies on (`prefetch_body_days > 0`), the ids
    /// of upserts that were not stored before go to `new_ids` (#1978): the
    /// new mail, whose bodies the round fetches. This thread is the only
    /// writer of delta rows, so the check right before the write holds.
    fn merge_delta_items(
        &self,
        folder_id: &str,
        items: &[DeltaItem],
        cutoff: Option<&str>,
        protect: Option<&[String]>,
        new_ids: &mut Vec<String>,
    ) -> u64 {
        let note_new = self.config.options.prefetch_body_days > 0;
        let mut upserts = 0;
        for item in items {
            match item {
                DeltaItem::Upsert(m, fields) => {
                    if let Some(cut) = cutoff {
                        if !m.received.is_empty() && m.received.as_str() < cut {
                            continue;
                        }
                    }
                    upserts += 1;
                    match protect {
                        Some(ids) if ids.binary_search(&m.id).is_ok() => {}
                        Some(_) => {
                            let _ = self.store.merge_message_unless_edited(folder_id, m, fields);
                        }
                        None => {
                            let fresh =
                                note_new && !self.store.message_exists(&m.id).unwrap_or(true);
                            if self.store.merge_message(folder_id, m, fields).is_ok()
                                && fresh
                                && !new_ids.contains(&m.id)
                            {
                                new_ids.push(m.id.clone());
                            }
                        }
                    }
                }
                // A `@removed` entry can carry an empty id; deleting "" is a
                // safe no-op in the store.
                DeltaItem::Delete(id) => {
                    let _ = self.store.delete_message(id);
                }
            }
        }
        upserts
    }

    // --- Backfill (#1871) -------------------------------------------------

    /// The ids a fresh delta must not merge over (#1871), sorted: those a
    /// queued outbox op names (`Store::pending_message_ids`: an optimistic
    /// write waiting after a 5xx; a deleted message is not put back). Only
    /// this thread queues ops, so the list cannot go stale during a fetch.
    /// A draft with an unsaved edit is not listed: compose marks it on its
    /// own connection at any time, so `merge_message_unless_edited` checks
    /// it in the statement that writes.
    fn protected_ids(&self) -> Result<Vec<String>, StoreError> {
        self.store.pending_message_ids()
    }

    /// The quarantine reconverge is over: the flag is cleared, in memory and
    /// in the store.
    fn end_reconverge(&mut self) {
        if self.reconverge_pending {
            self.reconverge_pending = false;
            let _ = self.store.clear_reconverge_pending();
        }
    }

    /// Whether `backfill_days` asks for the whole mailbox ([`WHOLE_MAILBOX`]).
    fn whole_mailbox(&self) -> bool {
        self.config.backfill_days <= 0
    }

    /// The age cutoff for a full delta's items: the window's
    /// (`cutoff_iso`, `None` in whole-mailbox mode), and none while a
    /// quarantine reconverge must restore every message.
    fn backfill_cutoff(&self) -> Option<String> {
        if self.reconverge_pending {
            None
        } else {
            self.cutoff_iso()
        }
    }

    /// The head delta's lower bound: `HEAD_WINDOW_DAYS` ago, or the window's
    /// own size when `backfill_days` is smaller.
    fn head_since(&self) -> String {
        let days = if self.whole_mailbox() {
            HEAD_WINDOW_DAYS
        } else {
            self.config.backfill_days.min(HEAD_WINDOW_DAYS)
        };
        unix_to_iso8601(now_unix().saturating_sub(days as u64 * 86400))
    }

    /// A folder's stored `total_count` (from the pass's listing), `None`
    /// when it cannot be read.
    fn folder_total(&self, folder_id: &str) -> Option<i64> {
        self.store
            .folder_history(folder_id)
            .ok()
            .flatten()
            .map(|h| h.total_count)
    }

    /// Sends `FolderProgress` for `folder_id` with `synced` items so far.
    fn emit_progress(&self, folder_id: &str, synced: u64, complete: bool) {
        let total = if self.whole_mailbox() {
            self.folder_total(folder_id)
        } else {
            None
        };
        self.emit(SyncEvent::FolderProgress {
            folder_id: folder_id.to_string(),
            synced,
            total,
            complete,
        });
    }

    /// The folders whose history is not complete for this mode, in
    /// `Store::folders` order: phase 1 done (a delta link), and the scope
    /// short of what the mode asks for (`history_complete`).
    fn incomplete_histories(&self) -> Result<Vec<FolderHistory>, StoreError> {
        let whole = self.whole_mailbox();
        Ok(self
            .store
            .folder_histories()?
            .into_iter()
            .filter(|h| h.has_delta_link && !history_complete(h.scope.as_deref(), whole))
            .collect())
    }

    /// Whether some folder's backfill is still to run, not counting a
    /// parked one: a page that fails is parked, and a parked walk must not
    /// hold a reconverge (and its persisted flag) open. A store error
    /// counts as none.
    fn backfill_remains(&self) -> bool {
        self.incomplete_histories()
            .map(|h| h.iter().any(|h| !self.parked.contains(&h.folder_id)))
            .unwrap_or(false)
    }

    /// The folder whose backfill page runs next, if one may run now: signed
    /// in, outside a back-off, after a folder list succeeded
    /// (`backfill_done`), and the first incomplete folder that is not
    /// parked. The main loop and `backfill_step` share it, so a ready
    /// backfill always makes a request or parks (never a spin). A store
    /// error stops the backfill until the next pass reads it again.
    fn next_backfill(&self) -> Option<FolderHistory> {
        if self.token.is_none()
            || !self.backfill_done
            || self.next_retry.is_some_and(|t| Instant::now() < t)
        {
            return None;
        }
        self.incomplete_histories()
            .ok()?
            .into_iter()
            .find(|h| !self.parked.contains(&h.folder_id))
    }

    /// Parks `folder_id`'s backfill until the next pass. A reconverge whose
    /// only walks left are parked ends here (`backfill_remains`).
    fn park(&mut self, folder_id: &str) {
        if !self.parked.iter().any(|f| f == folder_id) {
            self.parked.push(folder_id.to_string());
        }
        if self.reconverge_pending && !self.backfill_remains() {
            self.end_reconverge();
        }
    }

    /// Reports a store failure of a backfill step and parks the folder, so
    /// the page is not fetched again at once.
    fn park_on_store_error(&mut self, folder_id: &str, e: StoreError) {
        self.emit(SyncEvent::Error(format!(
            "mail sync: cannot record the download of folder {}: {e}",
            self.folder_label(folder_id)
        )));
        self.park(folder_id);
    }

    /// One page of a folder's full delta (#1871, phase 2), from its
    /// persisted `nextLink` or a fresh `DeltaCursor::Full` (also when a
    /// walk a window engine began resumes in whole-mailbox mode: its pages
    /// ran with the window's cutoff). Its items are applied by
    /// `merge_delta_items` (merged, never over a local edit; older than
    /// the window skipped in window mode, unless a reconverge is pending),
    /// then the next link, the running count and the walk's scope are
    /// stored and `MessagesUpdated` and `FolderProgress` sent. When the
    /// deltaLink comes, one last round on the folder's own link runs first,
    /// then the deltaLink becomes the folder's `delta_link` in the same
    /// statement that marks it complete (`Store::complete_backfill`);
    /// `repair_headerless` follows. Sets no state: a failed page keeps what
    /// `react` set. An expired sync state starts the folder over, once a
    /// pass (`is_expired_sync_state`); a second expiry, any other non-fatal
    /// failure or a store error parks the folder until the next pass.
    fn backfill_step(&mut self, mut h: FolderHistory) {
        let id = h.folder_id.clone();
        if self.whole_mailbox() && h.backfill_scope.as_deref() == Some(SCOPE_WINDOW) {
            if let Err(e) = self.store.reset_backfill(&id) {
                self.park_on_store_error(&id, e);
                return;
            }
            h.backfill_link = None;
            h.synced = 0;
            h.backfill_scope = None;
        }
        let cursor = match h.backfill_link {
            Some(link) => DeltaCursor::Link(link),
            None => DeltaCursor::Full(id.clone()),
        };
        let page = match self.with_auth(|c| c.delta(cursor.clone())) {
            Ok(p) => p,
            Err(e) => {
                if is_expired_sync_state(&e) && !self.expired.contains(&id) {
                    self.expired.push(id.clone());
                    if let Err(se) = self.store.reset_backfill(&id) {
                        self.park_on_store_error(&id, se);
                    }
                    return;
                }
                // Auth, throttle and transport stop the backfill through
                // `react` (sign-in, `next_retry`); the rest park the folder.
                if !self.react(e) {
                    self.park(&id);
                }
                return;
            }
        };
        let cutoff = self.backfill_cutoff();
        let page_scope = if cutoff.is_some() {
            SCOPE_WINDOW
        } else {
            SCOPE_ALL
        };
        let pending = match self.protected_ids() {
            Ok(ids) => ids,
            Err(e) => {
                self.park_on_store_error(&id, e);
                return;
            }
        };
        let walked = self.merge_delta_items(
            &id,
            &page.items,
            cutoff.as_deref(),
            Some(&pending),
            &mut Vec::new(),
        );
        let synced = u64::try_from(h.synced).unwrap_or(0) + walked;
        self.emit(SyncEvent::MessagesUpdated {
            folder_id: id.clone(),
        });
        if let Some(next) = page.next_link {
            match self
                .store
                .set_backfill_progress(&id, &next, synced as i64, page_scope)
            {
                Ok(()) => self.emit_progress(&id, synced, false),
                Err(e) => self.park_on_store_error(&id, e),
            }
            return;
        }
        let Some(delta) = page.delta_link else {
            // Graph sent a page with neither link: nothing to resume from.
            self.react(GraphError::Parse(
                "delta page without a next or delta link".to_string(),
            ));
            self.park(&id);
            return;
        };
        // One last round on the folder's own link (the head's, or a link
        // from before #1871) before the walk's deltaLink replaces it: it
        // brings recent mail changed during the walk up to date, which the
        // walk's link would not report. A link whose sync state expired
        // (`is_expired_sync_state`) is skipped: the walk's deltaLink replaces
        // it, so the folder heals. Any other failed round does not complete:
        // a fatal failure stops through `react`, any other parks the folder,
        // and the next step fetches the last page again (its link is still
        // the stored one).
        let mut new_ids = Vec::new();
        match self.store.get_delta_link(&id) {
            Ok(Some(link)) => match self.run_delta(&id, DeltaCursor::Link(link), None, None) {
                // A checkpoint stopped the round (#1978): not complete; the
                // next step fetches the last page again.
                Ok(run) if run.stopped => {
                    self.emit(SyncEvent::MessagesUpdated {
                        folder_id: id.clone(),
                    });
                    return;
                }
                Ok(run) => {
                    if let Some(head) = run.delta_link {
                        let _ = self.store.set_delta_link(&id, &head);
                    }
                    new_ids = run.new_ids;
                }
                Err(e) if is_expired_sync_state(&e) => {}
                Err(e) => {
                    if !self.react(e) {
                        self.park(&id);
                    }
                    return;
                }
            },
            Ok(None) => {}
            Err(e) => {
                self.park_on_store_error(&id, e);
                return;
            }
        }
        // A walk any page of which ran with the window's cutoff covers the
        // window only.
        let scope =
            if page_scope == SCOPE_WINDOW || h.backfill_scope.as_deref() == Some(SCOPE_WINDOW) {
                SCOPE_WINDOW
            } else {
                SCOPE_ALL
            };
        if let Err(e) = self
            .store
            .complete_backfill(&id, &delta, scope, synced as i64)
        {
            self.park_on_store_error(&id, e);
            return;
        }
        self.emit(SyncEvent::MessagesUpdated {
            folder_id: id.clone(),
        });
        self.emit_progress(&id, synced, true);
        if !self.backfill_remains() {
            self.end_reconverge();
        }
        let repair = self.repair_headerless(&id);
        if repair.changed {
            self.emit(SyncEvent::MessagesUpdated { folder_id: id });
        }
        // The last round's new mail comes with its body (#1978).
        if !repair.abort {
            self.fetch_new_bodies(&new_ids);
        }
    }

    /// Completes up to `REPAIR_LIMIT` of `folder_id`'s header-less rows
    /// (`Store::headerless_message_ids`: no `received_at`, not a row docxy
    /// made itself) from `GET /me/messages/{id}` (#1899): rows a partial
    /// delta item of this pass inserted, and rows an older build blanked.
    /// When it finds any, it says so in one `SyncEvent::Error` with their
    /// count (a partial delta item that left a row header-less, or a row an
    /// older build blanked). A 404 deletes the row (Graph no longer has that
    /// id; a moved message comes back under its new id through delta). An
    /// auth, throttle or transport failure aborts the pass like the delta
    /// call; any other failure skips that id, so one bad id cannot hold up
    /// the rows behind it, and is reported once for the folder.
    fn repair_headerless(&mut self, folder_id: &str) -> Repair {
        let mut repair = Repair {
            abort: false,
            changed: false,
        };
        let Ok(ids) = self.store.headerless_message_ids(folder_id, REPAIR_LIMIT) else {
            return repair;
        };
        if ids.is_empty() {
            return repair;
        }
        self.emit(SyncEvent::Error(format!(
            "mail sync: {} message(s) in folder {} were stored without headers; fetching them",
            ids.len(),
            self.folder_label(folder_id)
        )));
        let mut failed = 0usize;
        let mut last_error = None;
        for id in ids {
            if !self.checkpoint() {
                repair.abort = true;
                return repair;
            }
            match self.with_auth(|c| c.get_message(&id)) {
                Ok((m, fields)) => {
                    // Graph answers for the id asked; keep the row's id even
                    // if the response's differs or is missing.
                    let m = Message {
                        id: id.clone(),
                        ..m
                    };
                    if self.store.merge_message(folder_id, &m, &fields).is_ok() {
                        repair.changed = true;
                    }
                }
                Err(GraphError::NotFound) => {
                    if let Ok(Some(_)) = self.store.delete_message_resolved(&id) {
                        repair.changed = true;
                    }
                }
                Err(
                    e @ (GraphError::Unauthorized
                    | GraphError::Throttled { .. }
                    | GraphError::Transport(_)),
                ) => {
                    self.react(e);
                    repair.abort = true;
                    return repair;
                }
                Err(e @ (GraphError::Http { .. } | GraphError::Parse(_))) => {
                    failed += 1;
                    last_error = Some(e);
                }
            }
        }
        if let Some(e) = last_error {
            self.emit(SyncEvent::Error(format!(
                "mail sync: could not fetch {failed} message(s) in folder {}: {e}",
                self.folder_label(folder_id)
            )));
        }
        repair
    }

    /// A folder's display name for a message, else its id.
    fn folder_label(&self, folder_id: &str) -> String {
        self.store
            .folders()
            .ok()
            .and_then(|fs| fs.into_iter().find(|f| f.id == folder_id))
            .map(|f| f.display_name)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| folder_id.to_string())
    }

    // --- Outbox ----------------------------------------------------------

    /// Optimistic local write already applied by the caller: persist the op,
    /// reflect the pending count, and (if signed in) try to push it now.
    fn enqueue_and_drain(&mut self, op: OutboxOp) {
        if let Err(e) = self.store.enqueue_op(&op) {
            self.emit(SyncEvent::Error(e.to_string()));
            return;
        }
        self.drain_queued();
    }

    /// The half of `enqueue_and_drain` after the op is queued: reflect the
    /// pending count, and (if signed in) try to push it now.
    fn drain_queued(&mut self) {
        self.set_state(SyncState::PendingOps(self.pending_count()));
        // A 5xx defers the op under the outbox's own back-off (`op_retry`),
        // not an engine-wide Offline: the folders keep syncing.
        if self.token.is_some() {
            self.drain_outbox();
        }
        self.settle_state();
    }

    /// Drains queued outbox ops in `seq` order via `apply_op`. Success drops
    /// the op; auth/throttle/transport stop the drain (handled centrally);
    /// a non-retryable 4xx/parse bumps the attempt count and, past
    /// [`MAX_OP_ATTEMPTS`], quarantines the op (drops it + emits `Error`) so
    /// the rest of the queue proceeds, except a `SendDraft`, whose first
    /// such failure returns the message to Drafts (`fail_send`, #1874) and
    /// the queue proceeds too; a 5xx keeps the op and stops the
    /// drain there ([`Drain::Deferred`]): a 5xx op is never quarantined, so
    /// it must not stop the caller's sync either. It arms the outbox's own
    /// back-off (`op_retry`, 5 s doubling to 300 s, reset when an op goes
    /// through or the queue is empty): until then a drain sends nothing and
    /// is `Deferred` too.
    ///
    /// After a drain that sent a draft, Drafts and then Sent Items are
    /// synced at once (`sync_after_send`, #1885). Returns
    /// [`Drain::Aborted`] when the drain was cut short (auth, throttle,
    /// transport, or that sync stopping), so the caller stops too and keeps
    /// the back-off.
    fn drain_outbox(&mut self) -> Drain {
        let ops = match self.store.pending_ops() {
            Ok(o) => o,
            // A row that won't deserialize can't be pinpointed through the
            // public `Store` API (`pending_ops` is all-or-nothing), so we
            // surface it and skip this drain rather than crash. In practice the
            // engine only ever writes the outbox via `enqueue_op`, so this
            // can't arise from our own writes.
            Err(StoreError::Decode(m)) => {
                self.emit(SyncEvent::Error(format!(
                    "outbox decode error (drain skipped): {m}"
                )));
                return Drain::Done;
            }
            Err(e) => {
                self.emit(SyncEvent::Error(e.to_string()));
                return Drain::Done;
            }
        };
        if ops.is_empty() {
            self.clear_op_backoff();
            return Drain::Done;
        }
        if self.op_retry.is_some_and(|t| Instant::now() < t) {
            return Drain::Deferred;
        }

        let mut sent = Vec::new();
        let mut deferred = false;
        for row in ops {
            match self.apply_op_with_retry(&row) {
                Ok(()) => {
                    self.store.drop_op(row.seq);
                    self.clear_op_backoff();
                    if let OutboxOp::SendDraft { id } = &row.op {
                        self.emit(SyncEvent::Sent { id: id.clone() });
                        sent.push(id.clone());
                    }
                }
                Err(GraphError::Unauthorized) => {
                    self.enter_signin();
                    return Drain::Aborted;
                }
                Err(GraphError::Throttled { retry_after_secs }) => {
                    // As `react` does: wait out the throttle, out of `Syncing`.
                    self.schedule_retry(Duration::from_secs(retry_after_secs));
                    self.settle_state();
                    return Drain::Aborted;
                }
                Err(GraphError::Transport(_)) => {
                    self.go_offline();
                    return Drain::Aborted;
                }
                Err(other) => {
                    // 4xx (incl. 404) and parse failures are non-retryable;
                    // 5xx is a transient server error: keep the op, in order,
                    // for the next drain.
                    let retryable_5xx =
                        matches!(&other, GraphError::Http { status, .. } if *status >= 500);
                    if retryable_5xx {
                        self.store.bump_op_attempts(row.seq, &other.to_string());
                        self.op_retry = Some(Instant::now() + self.op_backoff);
                        self.op_backoff = (self.op_backoff * 2).min(BACKOFF_MAX);
                        deferred = true;
                        break;
                    }
                    if let OutboxOp::SendDraft { id } = &row.op {
                        // Retrying a 403 changes nothing: the first such
                        // failure is final for a send whose `/send` never
                        // went out. One that may have gone out (its
                        // delivery check failed) is never returned to
                        // Drafts: it keeps the attempts/quarantine policy
                        // below, and the row stays where it is.
                        if !self.store.send_posted(row.seq).unwrap_or(true) {
                            self.fail_send(row.seq, id, &other);
                            continue;
                        }
                    }
                    let attempts_after = row.attempts + 1;
                    if attempts_after >= MAX_OP_ATTEMPTS {
                        self.store.drop_op(row.seq);
                        // Reconverge with server truth: the op's optimistic
                        // local write (e.g. a Delete that hid a message Graph
                        // still has) would otherwise linger, and an unchanged
                        // message is never re-reported by delta. Rather than
                        // tracking prior state to revert precisely, clear all
                        // delta links and reset `backfill_done` so the next
                        // tick runs a full re-enumeration + re-upsert, re-adding
                        // anything the dropped op wrongly removed locally. The
                        // reconverge must ignore the `backfill_days` age cutoff
                        // (see `reconverge_pending`) so it restores the message
                        // even if it has aged past the window during the retry
                        // back-off.
                        let _ = self.store.clear_delta_links();
                        self.backfill_done = false;
                        self.reconverge_pending = true;
                        let is_respond_event = matches!(row.op, OutboxOp::RespondEvent { .. });
                        self.emit(SyncEvent::Error(format!(
                            "quarantined outbox op seq {} after {attempts_after} attempts: {other}",
                            row.seq
                        )));
                        // A quarantined `RespondEvent` leaves the optimistic
                        // local `response_status` wrong (Graph never got the
                        // RSVP) — unlike mail, there's no delta re-sync to
                        // fix that up later, and the mail reconverge above
                        // doesn't touch the `events` table at all. Refresh
                        // the calendar right away so `replace_events_in_
                        // window` (see `Store::replace_events_in_window`)
                        // pulls the true status back from the server instead
                        // of leaving a phantom RSVP on the books.
                        if is_respond_event {
                            self.refresh_calendar();
                        }
                    } else {
                        self.store.bump_op_attempts(row.seq, &other.to_string());
                    }
                    // Keep draining the rest of the queue.
                }
            }
        }
        if !sent.is_empty() && !self.sync_after_send(&sent) {
            return Drain::Aborted;
        }
        if deferred {
            Drain::Deferred
        } else {
            Drain::Done
        }
    }

    /// A send failed for good (#1874): every failure reaching it is final,
    /// whatever the op's attempt count. The drain calls it only for an op
    /// whose `/send` POST never went out (`Store::send_posted` false; a
    /// refused POST clears it), so the message cannot have been delivered.
    /// That is an `Http` 4xx (401 and 429 never get here), `NotFound` (the
    /// Graph draft is gone; the Drafts delta may then remove the row this
    /// returns to Drafts) and any `Parse`, which includes an attachment file
    /// that cannot be read and a store error `apply_send` maps to `Parse`.
    /// A 404 after a POST whose answer was lost is not a failure:
    /// `apply_send`'s delivery check reads it as delivered. Drops the op, undoes the
    /// `SendDraft` arm's optimistic write (`Store::return_to_drafts`: a draft
    /// again, in the Drafts folder, or the local sentinel before Drafts has
    /// synced; nothing else of the message changes), emits `SendFailed`, and
    /// reloads Drafts and Sent Items. The revert is exact, so unlike a
    /// quarantine there is no delta reconverge and no `Error`.
    fn fail_send(&mut self, seq: i64, id: &str, err: &GraphError) {
        self.store.drop_op(seq);
        // The sync thread resolves the sentinel itself, so `retain_folders`
        // cannot remove it in between (#1886).
        let drafts = self.store.drafts_folder_id().ok();
        if let Some(drafts) = &drafts {
            let _ = self.store.return_to_drafts(id, drafts);
        }
        self.emit(SyncEvent::SendFailed {
            id: id.to_string(),
            error: send_error_text(err),
        });
        if let Some(folder_id) = drafts {
            self.emit(SyncEvent::MessagesUpdated { folder_id });
        }
        if let Some(folder_id) = self.sent_items_folder_id() {
            self.emit(SyncEvent::MessagesUpdated { folder_id });
        }
    }

    /// Right after a drain sent the drafts `sent` (the ids their ops
    /// carried) (#1885): sync Drafts, then Sent Items, each only when stored
    /// with a delta link (this never backfills). Drafts first: Graph gives a
    /// sent message a new id in Sent Items, so the Drafts delta's `@removed`
    /// of the draft's id removes the row `SendDraft` re-filed under Sent,
    /// and Sent never shows it twice. Graph moves the draft and files the
    /// Sent copy asynchronously, so either may only come with a later delta:
    /// a Drafts delta that still reports a sent draft (an upsert, which
    /// re-files the row) is undone by re-filing it under Sent Items again,
    /// until the `@removed` deletes it whatever its folder. Returns `false`
    /// when a sync must stop (`refresh_folder`).
    fn sync_after_send(&mut self, sent: &[String]) -> bool {
        let Ok(folders) = self.store.folders() else {
            return true;
        };
        let find = |well_known: &str| {
            folders
                .iter()
                .find(|f| f.well_known_name.as_deref() == Some(well_known))
        };
        let (drafts, sent_items) = (find("drafts"), find("sentitems"));
        if let Some(d) = drafts.filter(|d| d.delta_link.is_some()) {
            if !self.refresh_folder(&d.id) {
                return false;
            }
            if let Some(s) = sent_items {
                for id in sent {
                    let in_drafts = matches!(
                        self.store.draft(id),
                        Ok(Some((row, _))) if row.folder_id == d.id
                    );
                    if in_drafts {
                        let _ = self.store.move_message(id, &s.id);
                    }
                }
            }
        }
        if let Some(s) = sent_items.filter(|s| s.delta_link.is_some()) {
            if !self.refresh_folder(&s.id) {
                return false;
            }
        }
        true
    }

    /// Count of ops still queued (0 if the count can't be read).
    fn pending_count(&self) -> usize {
        self.store.pending_ops().map(|v| v.len()).unwrap_or(0)
    }

    /// Applies one outbox op via `sync::outbox::apply_op`, refreshing the
    /// token once and retrying on a 401 — the same policy as `with_auth`,
    /// duplicated rather than reused because `apply_op` (unlike every other
    /// Graph call in this engine) also needs `&self.store`. `with_auth`'s
    /// closure parameter can't borrow `self.store` itself: it's an argument
    /// evaluated at the call site of `self.with_auth(...)`, which needs
    /// `&mut self` for the whole method call, so a closure over
    /// `&self.store` captured there would alias that mutable borrow for as
    /// long as `with_auth` runs. Building the client and calling `apply_op`
    /// directly in this method's own body sidesteps that: each borrow of
    /// `self.store` here ends at the end of its statement, well before the
    /// next `&mut self` use (`self.try_refresh()`).
    ///
    /// A `SendDraft` goes through `apply_send` with the row's `seq`, so its
    /// `/send` POST is recorded and a later attempt checks delivery first
    /// (#1874).
    fn apply_op_with_retry(&mut self, row: &OutboxRow) -> Result<(), GraphError> {
        let apply = |client: &GraphClient, store: &Store| match &row.op {
            OutboxOp::SendDraft { id } => apply_send(client, store, id.as_str(), Some(row.seq)),
            op => apply_op(client, store, op),
        };
        let client = self.client();
        match apply(&client, &self.store) {
            Err(GraphError::Unauthorized) => {
                if self.try_refresh() {
                    let client = self.client();
                    apply(&client, &self.store)
                } else {
                    Err(GraphError::Unauthorized)
                }
            }
            other => other,
        }
    }

    /// The Sent Items folder's local id, once it has synced
    /// (`well_known_name = "sentitems"`) — `None` before that first sync, so
    /// `SendDraft`'s optimistic local re-file is skipped rather than moving a
    /// message into a folder id that doesn't exist yet (which the
    /// `messages.folder_id` foreign key would reject anyway).
    fn sent_items_folder_id(&self) -> Option<String> {
        self.store
            .folders()
            .ok()?
            .into_iter()
            .find(|f| f.well_known_name.as_deref() == Some("sentitems"))
            .map(|f| f.id)
    }

    // --- Bodies ----------------------------------------------------------

    /// Fetch a message body in the engine's `EngineOptions::body_format`,
    /// store it, and emit `BodyReady`. `Text` asks Graph for plain text
    /// (best for a TUI); `Html` sends no `Prefer` and gets each body in its
    /// native format (html, or text for a plain-text message). The body is
    /// stored with the format *asked for*, not the content type it came back
    /// as (`Store::put_fetched_body`), so `Store::get_body_for` can tell a
    /// native-text answer to an `Html` ask from a body to refetch. Every
    /// `FetchBody` goes to Graph: the caller decides when one is needed.
    /// Returns `react`'s answer to a failure (true: an auth, throttle or
    /// transport failure), for `checkpoint` to stop the work it serves in.
    fn fetch_body(&mut self, id: &str) -> bool {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return false;
        }
        match self.fetch_and_store_body(id) {
            Ok(stored) => {
                // As before #1978, `BodyReady` answers a `FetchBody` even
                // when the store refused the body.
                if !stored {
                    self.emit(SyncEvent::BodyReady { id: id.to_string() });
                }
                false
            }
            Err(e) => self.react(e),
        }
    }

    /// The one body fetch `FetchBody` and the engine's own fetches (#1978)
    /// share, so a reader cannot tell their bodies apart: `get_body` in the
    /// engine's format, `Store::put_fetched_body` (the format asked for in
    /// `fetched_as`), then `BodyReady`. `Ok(false)`: the store refused the
    /// body (no `BodyReady` sent).
    fn fetch_and_store_body(&mut self, id: &str) -> Result<bool, GraphError> {
        let format = self.config.options.body_format;
        let body = self.with_auth(|c| c.get_body(id, format == BodyFormat::Text))?;
        if self.store.put_fetched_body(id, &body, format).is_err() {
            return Ok(false);
        }
        self.emit(SyncEvent::BodyReady { id: id.to_string() });
        Ok(true)
    }

    /// The lower bound of the bodies the engine fetches on its own
    /// (`prefetch_body_days`, #1978), or `None` when that is off.
    fn body_window_since(&self) -> Option<String> {
        let days = self.config.options.prefetch_body_days;
        (days > 0).then(|| unix_to_iso8601(now_unix().saturating_sub(u64::from(days) * 86400)))
    }

    /// Fetches the bodies of the messages a delta round on a stored link
    /// added (#1978): at most `NEW_BODY_LIMIT` requests, each after a
    /// checkpoint and only for an id that still needs one
    /// (`background_body`); the rest are the prefetch's, as are all of them
    /// while the fetches are parked (`body_parked`) or once a checkpoint
    /// kept a command, so the work the command waits for ends sooner. (Not
    /// on a due tick: a tick's own pass runs while its deadline is past.)
    /// Returns `false` when the caller must stop.
    fn fetch_new_bodies(&mut self, ids: &[String]) -> bool {
        let Some(since) = self.body_window_since() else {
            return true;
        };
        let mut fetched = 0;
        for id in ids {
            if fetched == NEW_BODY_LIMIT || self.body_parked {
                break;
            }
            if !self.checkpoint() {
                return false;
            }
            if !self.deferred.is_empty() {
                break;
            }
            match self.background_body(id, &since) {
                BackgroundBody::NotNeeded => {}
                BackgroundBody::Fetched => fetched += 1,
                BackgroundBody::Stop => return false,
                BackgroundBody::Parked => break,
            }
        }
        true
    }

    /// The prefetch's next batch (#1978), with the window it was picked
    /// in: signed in, outside a back-off, after a folder list succeeded
    /// (`backfill_done`), with bodies on and not parked (`body_parked`),
    /// the newest `PREFETCH_BATCH` messages that need a body
    /// (`Store::body_candidates`), not counting `body_skip`. A store error
    /// stops the prefetch until the next step reads it again.
    fn next_prefetch(&self) -> Option<(String, Vec<String>)> {
        if self.token.is_none()
            || !self.backfill_done
            || self.body_parked
            || self.next_retry.is_some_and(|t| Instant::now() < t)
        {
            return None;
        }
        let since = self.body_window_since()?;
        let ids = self
            .store
            .body_candidates(
                self.config.options.body_format,
                &since,
                PREFETCH_BATCH,
                &self.body_skip,
            )
            .ok()?;
        (!ids.is_empty()).then_some((since, ids))
    }

    /// One prefetch batch (#1978): each id after a checkpoint, if it still
    /// needs a body (`background_body`). It ends early when a checkpoint
    /// kept a command or a timed wake is due, so those wait for one body at
    /// most, as for one backfill page. A failure that stops it keeps what
    /// `react` set: a throttle's `next_retry` holds the prefetch until
    /// `Retry-After` ends. Sets no state, like a backfill step.
    fn prefetch_step(&mut self, since: &str, ids: &[String]) {
        for id in ids {
            // A command a checkpoint kept, or a tick come due, ends the
            // batch: the main loop runs it, then the rest of the window.
            if !self.checkpoint() || !self.deferred.is_empty() || self.due() {
                return;
            }
            if let BackgroundBody::Stop | BackgroundBody::Parked = self.background_body(id, since) {
                return;
            }
        }
    }

    /// One body the engine fetches on its own (#1978): a checkpoint first
    /// (it may serve a click on this very message), then the store's
    /// re-check (`Store::needs_body`: a draft, a row deleted or a body
    /// stored since needs none), then `fetch_and_store_body`. An auth,
    /// throttle or transport failure goes through `react` and stops the
    /// caller. A 404 (moved or deleted since), another 4xx or a parse error
    /// is not reported, as the user asked for nothing, and puts the id in
    /// `body_skip`, so it is not asked for again in this run: never a loop.
    /// A 5xx (other than a throttle's 503) or a store failure parks these
    /// fetches until the next pass (`body_parked`), the id not skipped: one
    /// request a pass while Graph or the store is failing, and the id is
    /// fetched once it works again.
    fn background_body(&mut self, id: &str, since: &str) -> BackgroundBody {
        if !self.checkpoint() {
            return BackgroundBody::Stop;
        }
        if self.body_skip.contains(id) {
            return BackgroundBody::NotNeeded;
        }
        match self
            .store
            .needs_body(id, self.config.options.body_format, since)
        {
            Ok(true) => {}
            Ok(false) => return BackgroundBody::NotNeeded,
            Err(_) => {
                self.body_parked = true;
                return BackgroundBody::Parked;
            }
        }
        match self.fetch_and_store_body(id) {
            Ok(true) => BackgroundBody::Fetched,
            // A row deleted since (the UI deletes on its own connection)
            // fails the body's foreign key: nothing is wrong with the store.
            Ok(false) if !self.store.message_exists(id).unwrap_or(true) => BackgroundBody::Fetched,
            Ok(false) => {
                self.body_parked = true;
                BackgroundBody::Parked
            }
            Err(
                e @ (GraphError::Unauthorized
                | GraphError::Throttled { .. }
                | GraphError::Transport(_)),
            ) => {
                self.react(e);
                BackgroundBody::Stop
            }
            Err(GraphError::Http { status, .. }) if status >= 500 => {
                self.body_parked = true;
                BackgroundBody::Parked
            }
            Err(GraphError::NotFound | GraphError::Http { .. } | GraphError::Parse(_)) => {
                self.body_skip.insert(id.to_string());
                BackgroundBody::Fetched
            }
        }
    }

    /// Fetch attachment metadata for `message_id` (`GraphClient::list_attachments`)
    /// and store it (`Store::put_attachments`, which replaces the full set for
    /// that message), then emit `SyncEvent::AttachmentsUpdated`. Same
    /// signed-in guard and `with_auth` retry-on-401 as `fetch_body`; a store
    /// write failure is surfaced rather than silently dropped, since it's the
    /// only way the UI would ever find out the popup has nothing to show.
    fn fetch_attachments(&mut self, message_id: &str) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.list_attachments(message_id)) {
            Ok(metas) => {
                if let Err(e) = self.store.put_attachments(message_id, &metas) {
                    self.emit(SyncEvent::Error(format!(
                        "failed to store attachments for {message_id}: {e}"
                    )));
                    return;
                }
                self.emit(SyncEvent::AttachmentsUpdated {
                    message_id: message_id.to_string(),
                });
            }
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Fetch one attachment's bytes and write them to `dest`, then emit
    /// `SyncEvent::AttachmentSaved` — or `SyncEvent::Error` on either a
    /// Graph failure (via `react`, so auth/throttle/transport get the same
    /// central handling as every other Graph call) or a filesystem failure
    /// writing `dest`. Same signed-in guard as `fetch_body`.
    fn save_attachment(&mut self, message_id: &str, attachment_id: &str, dest: PathBuf) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_attachment_bytes(message_id, attachment_id)) {
            Ok(bytes) => {
                if let Some(parent) = dest.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        self.emit(SyncEvent::Error(format!(
                            "failed to create {}: {e}",
                            parent.display()
                        )));
                        return;
                    }
                }
                match std::fs::write(&dest, &bytes) {
                    Ok(()) => self.emit(SyncEvent::AttachmentSaved { path: dest }),
                    Err(e) => self.emit(SyncEvent::Error(format!(
                        "failed to save attachment to {}: {e}",
                        dest.display()
                    ))),
                }
            }
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Fetch one nested-item attachment's raw bytes
    /// (`GraphClient::get_attachment_raw_value`) and write them to
    /// `{dest_base}.ics`/`{dest_base}.eml` (sniffed via `looks_like_icalendar`),
    /// then emit `SyncEvent::AttachmentSaved` — same error handling and
    /// signed-in guard as `save_attachment`.
    fn save_item_attachment(&mut self, message_id: &str, attachment_id: &str, dest_base: PathBuf) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_attachment_raw_value(message_id, attachment_id)) {
            Ok(bytes) => {
                let ext = if looks_like_icalendar(&bytes) {
                    "ics"
                } else {
                    "eml"
                };
                // Append the extension (do NOT use with_extension — dest_base
                // may legitimately contain dots we must keep).
                let mut os = dest_base.into_os_string();
                os.push(".");
                os.push(ext);
                let dest = std::path::PathBuf::from(os);
                if let Some(parent) = dest.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        self.emit(SyncEvent::Error(format!(
                            "failed to create {}: {e}",
                            parent.display()
                        )));
                        return;
                    }
                }
                match std::fs::write(&dest, &bytes) {
                    Ok(()) => self.emit(SyncEvent::AttachmentSaved { path: dest }),
                    Err(e) => self.emit(SyncEvent::Error(format!(
                        "failed to save attachment to {}: {e}",
                        dest.display()
                    ))),
                }
            }
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Resolve a meeting-invite message's underlying event
    /// (`GraphClient::meeting_event_id`) and RSVP to it via `respond_event`
    /// (`send_response = true`, no comment) — a direct Graph call like
    /// `save_attachment`, not an outbox op. On success emit
    /// `SyncEvent::MeetingResponded`; a message that resolves to no event
    /// emits `SyncEvent::Error` ("not a meeting invite"); a Graph failure goes
    /// through `react` for the standard auth/throttle/transport handling. Same
    /// signed-in guard as `fetch_body`.
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

    /// Fetch the mailbox's automatic-replies config and emit
    /// `AutomaticRepliesFetched`; a Graph failure goes through `react`. Same
    /// signed-in guard as `fetch_body`.
    fn fetch_automatic_replies(&mut self) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_automatic_replies()) {
            Ok(replies) => self.emit(SyncEvent::AutomaticRepliesFetched { replies }),
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Fetch the signed-in user's profile and emit `ProfileFetched`; a Graph
    /// failure goes through `react`. Same signed-in guard as `fetch_body`.
    fn fetch_profile(&mut self) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.me()) {
            Ok(me) => self.emit(SyncEvent::ProfileFetched { me }),
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Fetch the signed-in user's photo and emit `PhotoFetched` (`None`: no
    /// photo); a Graph failure goes through `react`. Same signed-in guard as
    /// `fetch_body`.
    fn fetch_photo(&mut self) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.my_photo()) {
            Ok(photo) => self.emit(SyncEvent::PhotoFetched { photo }),
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Sign out: delete the saved token and drop the in-memory one. A file
    /// that cannot be deleted is reported, but the engine still signs out:
    /// keeping the token because of it would make Sign out do nothing. A
    /// throttle or offline deadline belonged to the old session and goes; the
    /// outbox's own back-off stays with the outbox, which is kept.
    fn sign_out(&mut self) {
        if let Err(e) = tokencache::clear(&self.config.token_path) {
            self.emit(SyncEvent::Error(format!(
                "could not delete the saved sign-in ({e}); it may be used again at the next start"
            )));
        }
        self.clear_backoff();
        self.enter_signin();
    }

    /// Write the mailbox's automatic-replies config and emit
    /// `AutomaticRepliesUpdated`; a Graph failure goes through `react`. Same
    /// signed-in guard as `fetch_body`.
    fn set_automatic_replies(&mut self, replies: AutomaticReplies) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.set_automatic_replies(&replies)) {
            Ok(()) => self.emit(SyncEvent::AutomaticRepliesUpdated),
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Fetch one inline image's bytes and emit `SyncEvent::InlineImageReady`
    /// with them, keyed by `content_id` — same Graph call as
    /// `save_attachment`, but the bytes stay in memory for the reading pane
    /// to render rather than being written to disk. Same signed-in guard and
    /// `with_auth`/`react` handling as `save_attachment`.
    fn fetch_inline_image(&mut self, message_id: &str, attachment_id: &str, content_id: &str) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_attachment_bytes(message_id, attachment_id)) {
            Ok(bytes) => self.emit(SyncEvent::InlineImageReady {
                message_id: message_id.to_string(),
                content_id: content_id.to_string(),
                bytes,
            }),
            Err(e) => {
                self.react(e);
            }
        }
    }

    // --- Compose (reply/forward) ------------------------------------------

    /// Fetch a pre-quoted reply draft for `id` from Graph (`createReply`, or
    /// `createReplyAll` when `all`) and store it — see `store_composed_draft`.
    fn compose_reply(&mut self, id: &str, all: bool) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.create_reply(id, all)) {
            Ok(draft) => self.store_composed_draft(draft),
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Fetch a pre-quoted forward draft for `id` from Graph (`createForward`)
    /// and store it — see `store_composed_draft`.
    fn compose_forward(&mut self, id: &str) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.create_forward(id)) {
            Ok(draft) => self.store_composed_draft(draft),
            Err(e) => {
                self.react(e);
            }
        }
    }

    /// Finishes storing a reply/forward draft `create_reply`/`create_forward`
    /// just fetched: fetches its body (neither Graph call's response carries
    /// one — `Message` has no body field, see `graph::model` — so this is a
    /// second round-trip), files the message under the local Drafts folder
    /// (`Store::drafts_folder_id`, the same resolution `create_local_draft`
    /// uses — real folder if synced, sentinel otherwise), stores the body,
    /// and emits `DraftReady` so the UI can open the compose editor on it.
    fn store_composed_draft(&mut self, draft: Message) {
        let body = match self.with_auth(|c| c.get_body(&draft.id, false)) {
            Ok(b) => b,
            Err(e) => {
                self.react(e);
                return;
            }
        };
        let folder_id = match self.store.drafts_folder_id() {
            Ok(id) => id,
            Err(e) => {
                self.emit(SyncEvent::Error(e.to_string()));
                return;
            }
        };
        if let Err(e) = self.store.upsert_message(&folder_id, &draft) {
            self.emit(SyncEvent::Error(e.to_string()));
            return;
        }
        if let Err(e) = self.store.put_body(&draft.id, &body) {
            self.emit(SyncEvent::Error(e.to_string()));
            return;
        }
        self.emit(SyncEvent::DraftReady { id: draft.id });
    }

    // --- Calendar ----------------------------------------------------------

    /// Fetches `calendar_window(SystemTime::now())` via `calendar_view` and
    /// replaces the window's stored contents with it
    /// (`Store::replace_events_in_window`) in one atomic transaction, then
    /// emits `SyncEvent::CalendarUpdated`. Unlike mail's delta sync, Graph's
    /// `/me/calendarView` isn't a delta endpoint — there's no link to resume
    /// from, so every call re-fetches the whole window; replacing (rather
    /// than only upserting) is what prunes an event that Graph no longer
    /// returns for the window — cancelled, or moved outside it — instead of
    /// leaving it stuck locally forever. (`Store::calendar_delta_link`/
    /// `set_calendar_delta_link` are there for a future delta-capable
    /// endpoint; nothing here has a link to write into them.)
    ///
    /// Same signed-in guard as `fetch_body` et al.; a Graph failure goes
    /// through `react` for the standard auth/throttle/transport handling (a
    /// 4xx/parse is surfaced via `SyncEvent::Error`, not fatal to the
    /// caller). Also called by `drain_outbox` right after a `RespondEvent`
    /// op quarantines, to reconverge the optimistic local RSVP with server
    /// truth (see the quarantine branch there).
    fn refresh_calendar(&mut self) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        let (from, to) = calendar_window(SystemTime::now());
        match self.with_auth(|c| c.calendar_view(&from, &to)) {
            Ok(events) => {
                let new_events: Vec<NewEvent> = events.iter().map(NewEvent::from).collect();
                let attendees: Vec<(String, Vec<NewAttendee>)> = events
                    .iter()
                    .map(|e| {
                        (
                            e.id.clone(),
                            e.attendees.iter().map(NewAttendee::from).collect(),
                        )
                    })
                    .collect();
                if let Err(e) =
                    self.store
                        .replace_events_in_window(&from, &to, &new_events, &attendees)
                {
                    self.emit(SyncEvent::Error(e.to_string()));
                    return;
                }
                self.emit(SyncEvent::CalendarUpdated);
            }
            Err(e) => {
                self.react(e);
            }
        }
    }

    // --- Auth ------------------------------------------------------------

    /// Runs a Graph call with the current token; on a 401, refreshes the token
    /// once and retries. A 401 still escaping means refresh didn't help — the
    /// caller treats that as "sign-in required".
    fn with_auth<T>(
        &mut self,
        f: impl Fn(&GraphClient) -> Result<T, GraphError>,
    ) -> Result<T, GraphError> {
        let first = {
            let client = self.client();
            f(&client)
        };
        match first {
            Err(GraphError::Unauthorized) => {
                if self.try_refresh() {
                    let client = self.client();
                    f(&client)
                } else {
                    Err(GraphError::Unauthorized)
                }
            }
            other => other,
        }
    }

    /// Builds a `GraphClient` bound to the current access token (empty when
    /// signed out — such a call 401s and drives the sign-in path).
    fn client(&self) -> GraphClient {
        let token = self
            .token
            .as_ref()
            .map(|t| t.access_token.as_str())
            .unwrap_or("");
        GraphClient::new(&self.config.graph_base, token)
    }

    /// Refreshes the access token if it's missing or within the expiry skew.
    fn refresh_if_near_expiry(&mut self) {
        let now = now_unix();
        let near = self
            .token
            .as_ref()
            .map(|t| t.expires_at_unix <= now + EXPIRY_SKEW_SECS)
            .unwrap_or(true);
        if near && !self.try_refresh() {
            self.enter_signin();
        }
    }

    /// Exchanges the refresh token for a fresh token set and persists it.
    /// Returns false on any failure (no token to refresh, or the endpoint
    /// rejected it). Token values are never logged. A response that names
    /// no scopes (omitted, or empty) keeps the previous token's grant
    /// (#1875): Entra always names the granted scopes, so such an answer
    /// tells nothing new, and the last grant the server did name is kept.
    /// RFC 6749 §5.1 would read an omitted `scope` as everything requested,
    /// which is not taken: claiming scopes the server never named would hide
    /// a missing one until a 403.
    fn try_refresh(&mut self) -> bool {
        let Some((refresh_token, previous_scopes)) = self
            .token
            .as_ref()
            .map(|t| (t.refresh_token.clone(), t.scopes.clone()))
        else {
            return false;
        };
        match auth::refresh(&self.config.cfg, &self.config.auth_base, &refresh_token) {
            Ok(mut t) => {
                if t.scopes.is_none() {
                    t.scopes = previous_scopes;
                }
                let _ = tokencache::save(&self.config.token_path, &t);
                self.token = Some(t);
                self.announce_scopes();
                true
            }
            Err(_) => false,
        }
    }

    /// The interactive sign-in seam. Binds a loopback listener on
    /// `127.0.0.1:0` FIRST so the OS-assigned port can be baked into the
    /// `redirect_uri` handed to `begin_auth` — there's no race between
    /// emitting `SignInStarted` (which the UI reacts to by opening the
    /// browser) and the listener being ready to accept the redirect, because
    /// the bind happens before either. `listen_for_code` then blocks (with a
    /// timeout) for the browser to land on that port; once it returns a code,
    /// the redeem + cache-save + resync path runs unchanged from Task 11.
    fn sign_in(&mut self) {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => {
                self.emit(SyncEvent::Error(format!(
                    "could not open sign-in listener: {e}"
                )));
                self.set_state(SyncState::SignInRequired);
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(e) => {
                self.emit(SyncEvent::Error(format!(
                    "could not read sign-in listener port: {e}"
                )));
                self.set_state(SyncState::SignInRequired);
                return;
            }
        };
        // The redirect host must be `localhost`, NOT `127.0.0.1`: Entra matches
        // a public client's loopback redirect host *literally*, and the
        // Microsoft Graph CLI client we ride registers `http://localhost` (with
        // a dynamic port) but not `http://127.0.0.1` — sending the raw IP fails
        // with AADSTS50011 (redirect URI mismatch). We still bind the listener
        // on IPv4 `127.0.0.1`; the browser reaches it because it resolves
        // `localhost` to the loopback address (falling back across ::1/127.0.0.1
        // as needed). This mirrors what the Azure CLI and `mgc` do.
        let redirect_uri = format!("http://localhost:{port}");
        let req = auth::begin_auth(&self.config.cfg, &redirect_uri);
        self.emit(SyncEvent::SignInStarted {
            authorize_url: req.authorize_url.clone(),
        });

        let code = match listen_for_code(&listener, &req.state) {
            Ok(code) => code,
            Err(e) => {
                // A timeout, a malformed redirect, a state mismatch, or the
                // provider reporting `?error=...` all land here. None of
                // them are secrets, so the message is safe to surface as-is.
                // Re-entering sign-in-required lets the UI offer another
                // attempt rather than leaving the engine silently stuck.
                self.emit(SyncEvent::Error(format!("sign-in failed: {e}")));
                self.enter_signin();
                return;
            }
        };
        match auth::redeem_code(&self.config.cfg, &self.config.auth_base, &req, &code) {
            Ok(t) => {
                let _ = tokencache::save(&self.config.token_path, &t);
                self.token = Some(t);
                self.announce_scopes();
                self.clear_backoff();
                // Emit an explicit past-sign-in signal BEFORE the first sync
                // pass. If that pass fails transiently it only emits `Offline`
                // (never `Idle`/`FoldersUpdated`), and the UI clears the
                // sign-in modal on any non-`SignInRequired` state — so this
                // `Syncing` guarantees the modal clears the moment auth
                // succeeded, rather than staying stuck if the first sync fails.
                self.set_state(SyncState::Syncing);
                self.sync_pass(true);
                self.after_pass_if_fast();
            }
            Err(e) => {
                self.emit(SyncEvent::Error(format!("sign-in failed: {e}")));
                self.enter_signin();
            }
        }
    }

    // --- State transitions ----------------------------------------------

    /// Central reaction to a Graph error. Returns `true` when the current sync
    /// pass should abort. Auth → sign-in; throttle → reschedule; transport →
    /// offline back-off; everything else (4xx/parse) is a non-fatal, surfaced
    /// error the caller can skip past.
    fn react(&mut self, e: GraphError) -> bool {
        match e {
            GraphError::Unauthorized => {
                self.enter_signin();
                true
            }
            GraphError::Throttled { retry_after_secs } => {
                self.schedule_retry(Duration::from_secs(retry_after_secs));
                self.settle_state();
                true
            }
            GraphError::Transport(_) => {
                self.go_offline();
                true
            }
            GraphError::NotFound | GraphError::Http { .. } | GraphError::Parse(_) => {
                self.emit(SyncEvent::Error(e.to_string()));
                false
            }
        }
    }

    /// Sends [`SyncEvent::Scopes`] for the current token's grant when it is
    /// known and differs (as a normalised set) from the last one sent.
    fn announce_scopes(&mut self) {
        let Some(granted) = self.token.as_ref().and_then(|t| t.scopes.clone()) else {
            return;
        };
        let mut set: Vec<String> = granted.iter().map(|s| auth::normalise_scope(s)).collect();
        set.sort();
        set.dedup();
        if self.announced_scopes.as_ref() == Some(&set) {
            return;
        }
        self.announced_scopes = Some(set);
        let missing = auth::missing_scopes(&self.config.cfg.scope, &granted);
        self.emit(SyncEvent::Scopes { granted, missing });
    }

    /// Drop to signed-out: clear the token and announce `SignInRequired`.
    /// The announced grant is forgotten, so the next session announces it.
    fn enter_signin(&mut self) {
        self.token = None;
        self.announced_scopes = None;
        self.emit(SyncEvent::SignInRequired);
        self.set_state(SyncState::SignInRequired);
    }

    /// Suspend syncing until `after` from now (throttle `Retry-After`), without
    /// touching the offline back-off.
    fn schedule_retry(&mut self, after: Duration) {
        self.next_retry = Some(Instant::now() + after);
    }

    /// Enter `Offline` and set the next retry using the current back-off, then
    /// double it (capped) for the next consecutive failure.
    fn go_offline(&mut self) {
        self.next_retry = Some(Instant::now() + self.backoff);
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        self.set_state(SyncState::Offline);
    }

    /// An op went through, or none is queued: the outbox's own 5xx back-off
    /// (`op_retry`) is over.
    fn clear_op_backoff(&mut self) {
        self.op_retry = None;
        self.op_backoff = BACKOFF_MIN;
    }

    /// A clean pass resets the back-off and clears any pending retry deadline.
    fn clear_backoff(&mut self) {
        self.next_retry = None;
        self.backoff = BACKOFF_MIN;
    }

    /// Settle into a resting state after work: signed-out → `SignInRequired`,
    /// else `PendingOps(n)` when the outbox is non-empty, else `Idle`.
    fn settle_state(&mut self) {
        if self.token.is_none() {
            self.set_state(SyncState::SignInRequired);
            return;
        }
        let n = self.pending_count();
        self.set_state(if n > 0 {
            SyncState::PendingOps(n)
        } else {
            SyncState::Idle
        });
    }

    // --- Cutoff / events -------------------------------------------------

    /// The ISO-8601 lower bound for the initial backfill (`now - backfill_days`),
    /// or `None` when `backfill_days <= 0` (no limit). Compared lexically
    /// against Graph's `receivedDateTime`, which is valid because ISO-8601 UTC
    /// timestamps sort in chronological order.
    fn cutoff_iso(&self) -> Option<String> {
        if self.config.backfill_days <= 0 {
            return None;
        }
        let cutoff = now_unix().saturating_sub((self.config.backfill_days as u64) * 86400);
        Some(unix_to_iso8601(cutoff))
    }

    /// Sends an event, ignoring a dropped receiver (the command channel's
    /// disconnect is what actually stops the thread).
    fn emit(&self, e: SyncEvent) {
        let _ = self.evt_tx.send(e);
    }

    /// Emits `State(s)` only on an actual change, so the UI isn't spammed with
    /// identical transitions.
    fn set_state(&mut self, s: SyncState) {
        if self.state != s {
            self.state = s.clone();
            self.emit(SyncEvent::State(s));
        }
    }
}

/// The HTML served back to the browser once the redirect carried a valid
/// `code`. No script, no external resources — just enough for the user to
/// see it's safe to switch back to the terminal.
const SIGNIN_SUCCESS_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>lookxy</title></head><body><h3>lookxy: sign-in complete</h3><p>You can close this tab and return to the terminal.</p></body></html>";
/// Served instead when the redirect carried `?error=...`, had no `code`, or
/// its `state` didn't match — so the browser tab tells the user something
/// went wrong rather than just going blank.
const SIGNIN_ERROR_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>lookxy</title></head><body><h3>lookxy: sign-in failed</h3><p>You can close this tab and try again from the terminal.</p></body></html>";

/// Blocks (with [`SIGNIN_TIMEOUT`]) for the browser to land on the loopback
/// redirect `listener` is already bound to, accepts exactly one connection,
/// and extracts the authorization `code` from its request line — or an
/// error, on a timeout, a malformed request, a provider-reported
/// `?error=...`, or a `state` that doesn't match `expected_state` (the
/// anti-CSRF check `begin_auth` set up). Never logs the code or any secret;
/// the returned `Err` messages are diagnostic text only (parse failures,
/// the provider's `error` and `error_description` when the redirect's
/// `state` matches, else only a bare OAuth error code, or "timed out"),
/// safe to surface via `SyncEvent::Error`.
///
/// Uses a non-blocking `accept` polled against a deadline rather than a
/// dedicated timeout API (std's `TcpListener` has none) — simple, and the
/// listener is only ever used for this one accept.
fn listen_for_code(listener: &TcpListener, expected_state: &str) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("could not configure sign-in listener: {e}"))?;
    let deadline = Instant::now() + SIGNIN_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => return handle_redirect(stream, expected_state),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err("timed out waiting for the browser to complete sign-in".to_string());
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("sign-in listener accept failed: {e}")),
        }
    }
}

/// The largest request line `read_request_line` will accept before giving
/// up — generously past any real loopback redirect (Entra ID's own query
/// string, plus `GET `/`HTTP/1.1`, is a few hundred bytes at most), but
/// small enough that a misbehaving connection can't grow the buffer
/// unboundedly.
const SIGNIN_REQUEST_LINE_MAX: usize = 8192;

/// Reads one HTTP request line (up to and including its terminating `\n`)
/// off `stream`, byte at a time, bounded by BOTH `max_bytes` and an overall
/// wall-clock `deadline` — not just a per-`read()` timeout. A per-read
/// timeout alone doesn't bound total elapsed time: a connection that
/// trickles one byte every few seconds keeps each individual `read()` call
/// under any fixed per-call timeout, so `read_line` (or repeated `read`
/// calls with the timeout reset each time) could otherwise be strung along
/// indefinitely. Instead, the socket's read timeout is re-set on every
/// iteration to whatever remains of `deadline`, so the *last* read a slow
/// client could provoke still expires exactly when the overall budget runs
/// out. Reading a byte at a time is deliberately simple rather than
/// efficient — this is a one-time, at-most-few-hundred-byte request line on
/// a loopback socket, not a hot path.
fn read_request_line(
    stream: &TcpStream,
    max_bytes: usize,
    deadline: Instant,
) -> Result<String, String> {
    let mut stream = stream
        .try_clone()
        .map_err(|e| format!("could not clone loopback stream: {e}"))?;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if buf.len() >= max_bytes {
            return Err("redirect request line exceeded the size limit".to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("timed out reading the redirect request".to_string());
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|e| format!("could not set read timeout: {e}"))?;
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err("connection closed before a full request line was read".to_string());
            }
            Ok(_) => {
                let b = byte[0];
                buf.push(b);
                if b == b'\n' {
                    break;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err("timed out reading the redirect request".to_string());
            }
            Err(e) => return Err(format!("could not read redirect request: {e}")),
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Reads the HTTP request line off one accepted loopback connection, pulls
/// `code`/`state` (or `error`) out of its query string, writes back a small
/// HTML page, and returns the code (or an error — see [`listen_for_code`]).
fn handle_redirect(stream: TcpStream, expected_state: &str) -> Result<String, String> {
    let request_line = read_request_line(
        &stream,
        SIGNIN_REQUEST_LINE_MAX,
        Instant::now() + SIGNIN_READ_TIMEOUT,
    )?;
    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "malformed redirect request".to_string())?;
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let params = parse_query(query);

    let param = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let state_matches = param("state").map(String::as_str) == Some(expected_state);

    if let Some(err) = param("error") {
        write_html_response(stream, SIGNIN_ERROR_HTML);
        // Microsoft's `error_description` (`AADSTSnnnnn: …`) says what to do,
        // but anything can send the browser to this port during the sign-in
        // window, so free text (the description, or an `error` that is not a
        // bare code) is shown only from the redirect that carries our
        // `state` (Entra echoes it on an error, RFC 6749 §4.1.2.1).
        if !state_matches {
            return Err(if is_oauth_error_code(err) {
                format!("authorization denied: {err}")
            } else {
                "authorization denied (redirect state did not match)".to_string()
            });
        }
        let err = one_line(err);
        let desc = param("error_description")
            .map(|d| one_line(d))
            .filter(|d| !d.is_empty());
        return Err(match (err.is_empty(), desc) {
            (true, None) => "authorization denied".to_string(),
            (true, Some(desc)) => format!("authorization denied: {desc}"),
            (false, Some(desc)) => format!("authorization denied: {err}: {desc}"),
            (false, None) => format!("authorization denied: {err}"),
        });
    }
    if !state_matches {
        write_html_response(stream, SIGNIN_ERROR_HTML);
        return Err("redirect state did not match".to_string());
    }
    let Some(code) = params
        .iter()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.clone())
    else {
        write_html_response(stream, SIGNIN_ERROR_HTML);
        return Err("redirect had no authorization code".to_string());
    };
    write_html_response(stream, SIGNIN_SUCCESS_HTML);
    Ok(code)
}

/// An RFC 6749 error code as Entra sends one (`access_denied`,
/// `consent_required`, `interaction_required`): non-empty, at most 64
/// characters of ASCII `a-z` and `_`. Anything else in `error` is free text.
fn is_oauth_error_code(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// `s` on one line: every run of control characters and whitespace (Entra's
/// description has `\r\n` before its `Trace ID:` and `Correlation ID:`
/// lines) becomes one space, and the ends are trimmed.
fn one_line(s: &str) -> String {
    s.split(|c: char| c.is_control() || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Writes a minimal `HTTP/1.1 200` HTML response and closes the connection
/// (`Connection: close`, then the stream is dropped) — best-effort; a write
/// failure here (the user already closed the tab) doesn't change the code
/// that was already parsed. Drains whatever's left unread on the socket
/// first (see `drain_remaining_request`), so closing right after doesn't
/// look like a dropped connection to the client.
fn write_html_response(mut stream: TcpStream, body: &str) {
    drain_remaining_request(&stream);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Best-effort: discards whatever request bytes are still unread on the
/// socket after `read_request_line` stops at the first `\n` (the header
/// lines and any body we deliberately never parse). Reading the request
/// line one byte at a time only consumes exactly those bytes from the
/// kernel's socket receive buffer — anything the browser sent in the same
/// packet after that (e.g. `Host:`/`Connection:` headers) is left queued.
/// Some platforms (Windows in particular) respond to a `close()` on a
/// socket that still has unread inbound data queued with a hard RST instead
/// of a graceful FIN, which would make the client's read of our perfectly
/// well-formed response fail with a connection-reset error — even though
/// the code was parsed correctly and the response was written. Draining
/// first avoids that. Bounded by both a short read timeout and a byte cap,
/// so a client that keeps streaming data forever can't turn this into
/// another unbounded read; giving up quietly either way just means we might
/// still occasionally race a slow/unusual client, which is no worse than
/// today's behavior.
fn drain_remaining_request(stream: &TcpStream) {
    let Ok(mut stream) = stream.try_clone() else {
        return;
    };
    if stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .is_err()
    {
        return;
    }
    let mut buf = [0u8; 1024];
    let mut total = 0usize;
    while total < 64 * 1024 {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => break, // timed out (nothing more pending) or otherwise: give up quietly
        }
    }
}

/// Parses an `application/x-www-form-urlencoded` query string into
/// `(key, value)` pairs, percent-decoding each. A tiny local counterpart to
/// `pkce::form_urlencode` (which only encodes) — kept here rather than
/// shared, since this is the one place anything in `mailcore` decodes a
/// query string.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

/// Percent-decodes a query-string component (`%XX` → byte, `+` → space).
/// Invalid `%` escapes (not followed by two hex digits) pass through
/// literally rather than erroring — this is best-effort parsing of a
/// browser redirect, not a strict validator.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push((h << 4) | l);
                    i += 3;
                }
                _ => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Whether a folder's history is complete for the mode (#1871), from what
/// its `delta_link` covers (`Store::folder_history`'s `scope`), for a
/// folder that has one (phase 1 done): a link from before #1871 (`None`)
/// or a `window` one is complete in window mode only; an `all` one always;
/// a `head` one never.
fn history_complete(scope: Option<&str>, whole: bool) -> bool {
    match scope {
        None => !whole,
        Some(s) if s == SCOPE_WINDOW => !whole,
        Some(s) => s == SCOPE_ALL,
    }
}

/// Current wall-clock time as a Unix timestamp (seconds).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Formats a Unix timestamp as an ISO-8601 UTC string
/// (`YYYY-MM-DDTHH:MM:SSZ`) — no dependency, used only for the backfill cutoff
/// comparison.
fn unix_to_iso8601(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// `RefreshCalendar`'s window: this many days into the past...
const CALENDAR_WINDOW_PAST_DAYS: u64 = 7;
/// ...and this many days into the future, both anchored at the base time
/// `calendar_window` is called with.
const CALENDAR_WINDOW_FUTURE_DAYS: u64 = 30;

/// Computes the `[from, to]` ISO-8601 UTC bounds `RefreshCalendar` fetches:
/// `base - 7d` .. `base + 30d`. `base` is an injectable anchor so tests can
/// pin it to a fixed instant; production (`Engine::refresh_calendar`) always
/// passes `SystemTime::now()`.
fn calendar_window(base: SystemTime) -> (String, String) {
    let base_secs = base
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let from = base_secs.saturating_sub(CALENDAR_WINDOW_PAST_DAYS * 86400);
    let to = base_secs + CALENDAR_WINDOW_FUTURE_DAYS * 86400;
    (unix_to_iso8601(from), unix_to_iso8601(to))
}

/// Converts a count of days since the Unix epoch into a `(year, month, day)`
/// civil date — Howard Hinnant's well-known `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
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

/// Whether `bytes` begin (after optional BOM/leading whitespace) with an
/// iCalendar header, so a nested item should be saved as `.ics` rather than
/// `.eml`.
fn looks_like_icalendar(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(64)];
    let s = String::from_utf8_lossy(head);
    let s = s.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    s.to_ascii_uppercase().starts_with("BEGIN:VCALENDAR")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthConfig, TokenSet};
    use crate::store::Store;
    use crate::testserver::{FakeServer, Route};
    use crate::tokencache;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    fn unique_dir(tag: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("lookxy-sync-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_cfg() -> AuthConfig {
        AuthConfig {
            authority: "x/organizations".into(),
            client_id: "cid".into(),
            scope: "Mail.ReadWrite offline_access".into(),
        }
    }

    fn seed_token(path: &std::path::Path) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let t = TokenSet {
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at_unix: now + 10 * 365 * 86400,
            account: "me@epam.com".into(),
            scopes: None,
        };
        tokencache::save(path, &t).unwrap();
    }

    /// A token that expires in `expires_in` seconds and was granted `scopes`.
    fn seed_token_with_scopes(path: &std::path::Path, expires_in: u64, scopes: &[&str]) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let t = TokenSet {
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at_unix: now + expires_in,
            account: "me@epam.com".into(),
            scopes: Some(scopes.iter().map(|s| s.to_string()).collect()),
        };
        tokencache::save(path, &t).unwrap();
    }

    /// Upper bound for every "wait until the engine thread has done X" in these tests. Generous on
    /// purpose: a loaded windows-latest CI runner has taken more than 5s to emit the first event
    /// (#1000). A passing test returns as soon as its condition holds, so this costs nothing there.
    const ENGINE_WAIT: Duration = Duration::from_secs(30);

    /// Polls `done` every 20ms until it returns true (-> true) or `budget` runs out (-> false).
    fn wait_until(budget: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Collects events until `pred` returns true, or panics after `ENGINE_WAIT`.
    fn wait_for(
        rx: &Receiver<SyncEvent>,
        mut pred: impl FnMut(&SyncEvent) -> bool,
    ) -> Vec<SyncEvent> {
        let deadline = Instant::now() + ENGINE_WAIT;
        let mut seen = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!("timed out waiting; saw: {seen:?}");
            }
            match rx.recv_timeout(remaining) {
                Ok(ev) => {
                    let done = pred(&ev);
                    seen.push(ev);
                    if done {
                        return seen;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!("timed out waiting; saw: {seen:?}"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("engine thread exited; saw: {seen:?}")
                }
            }
        }
    }

    // Routes for: top-level folders (F1/Inbox), F1's (empty) child folders, and
    // F1's first messages/delta page (one message + a deltaLink). The deltaLink
    // is a *relative* path on purpose: `GraphClient` joins any non-http link
    // onto its injected base, so a follow-up (incremental) delta lands back on
    // this fake server rather than a real host. Order matters: the fake server
    // matches the FIRST route whose prefix matches, so the specific
    // `/me/mailFolders/F1/...` routes precede the generic `/me/mailFolders`.
    fn backfill_routes() -> Vec<Route> {
        vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/childFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/messages/delta".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"M1","conversationId":"C1","subject":"Hello","from":{"emailAddress":{"name":"Alice","address":"alice@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"2026-07-16T10:00:00Z","sentDateTime":"2026-07-16T09:00:00Z","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"hi"}],"@odata.deltaLink":"/me/mailFolders/F1/messages/delta?token=D1"}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"F1","displayName":"Inbox","parentFolderId":null,"totalItemCount":1,"unreadItemCount":1,"wellKnownName":"inbox"}]}"#.into(),
            },
        ]
    }

    #[test]
    fn backfill_populates_store_and_emits_events() {
        let srv = FakeServer::start(backfill_routes());
        let base = srv.base_url.clone();

        let dir = unique_dir("backfill");
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

        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );

        let store = Store::open(&store_path).unwrap();
        let msgs = store.messages_in_folder("F1", 50, 0).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, "M1");
        assert_eq!(msgs[0].subject, "Hello");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn get_route(prefix: &str, status: u16, body: &str) -> Route {
        Route {
            method: "GET".into(),
            path_prefix: prefix.into(),
            status,
            headers: vec![],
            body: body.into(),
        }
    }

    /// A complete Graph message as `MESSAGE_SELECT` returns it.
    fn full_message_json(id: &str, subject: &str, received: &str) -> String {
        format!(
            r#"{{"id":"{id}","conversationId":"C-{id}","subject":"{subject}","from":{{"emailAddress":{{"name":"Alice","address":"alice@x"}}}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"{received}","sentDateTime":"{received}","isRead":false,"flag":{{"flagStatus":"notFlagged"}},"hasAttachments":false,"importance":"normal","bodyPreview":"preview {id}","categories":[]}}"#
        )
    }

    /// Collects events until `n` `MessagesUpdated` for `folder` have come.
    fn wait_for_updates(rx: &Receiver<SyncEvent>, folder: &str, n: usize) -> Vec<SyncEvent> {
        let mut seen = 0;
        wait_for(rx, |e| {
            if matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == folder) {
                seen += 1;
            }
            seen == n
        })
    }

    /// Partial delta items (#1899): pass 1 backfills M1 and M2 complete;
    /// pass 2 (the stored deltaLink) returns `{id, isRead}` for M1 and a
    /// bare `{id}` for the new M3. M1 is merged (headers kept, no fetch), M3
    /// is fetched and stored complete, and the partial items are reported.
    #[test]
    fn partial_delta_item_keeps_headers_and_fetches_new_item() {
        let pass1 = format!(
            r#"{{"value":[{},{}],"@odata.deltaLink":"/me/mailFolders/F1/messages/delta?token=D1"}}"#,
            full_message_json("M1", "Hello", "2026-07-16T10:00:00Z"),
            full_message_json("M2", "Second", "2026-07-16T11:00:00Z"),
        );
        let pass2 = r#"{"value":[{"id":"M1","isRead":true},{"id":"M3"}],"@odata.deltaLink":"/me/mailFolders/F1/messages/delta?token=D2"}"#;
        let mut routes = vec![
            get_route("/me/mailFolders/F1/messages/delta?token=D1", 200, pass2),
            get_route(
                "/me/mailFolders/F1/messages/delta?token=D2",
                200,
                r#"{"value":[],"@odata.deltaLink":"/me/mailFolders/F1/messages/delta?token=D2"}"#,
            ),
            get_route("/me/mailFolders/F1/messages/delta", 200, &pass1),
            get_route(
                "/me/messages/M3",
                200,
                &full_message_json("M3", "New one", "2026-07-17T08:00:00Z"),
            ),
        ];
        // The folder routes, without backfill_routes' own delta page.
        routes.extend(
            backfill_routes()
                .into_iter()
                .filter(|r| !r.path_prefix.contains("/messages/delta")),
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("partial-delta");
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
        let store = Store::open(&store_path).unwrap();
        assert_eq!(store.messages_in_folder("F1", 50, 0).unwrap().len(), 2);

        handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
        // Once for the delta pages, once for the repair that completed M3.
        let pass2_events = wait_for_updates(&handle.evt_rx, "F1", 2);

        let msgs = store.messages_in_folder("F1", 50, 0).unwrap();
        let by_id = |id: &str| msgs.iter().find(|m| m.id == id).unwrap().clone();
        let m1 = by_id("M1");
        assert!(m1.is_read, "isRead merged");
        assert_eq!(m1.subject, "Hello");
        assert_eq!(m1.from_name, "Alice");
        assert_eq!(m1.from_addr, "alice@x");
        assert_eq!(m1.received_at, "2026-07-16T10:00:00Z");
        assert_eq!(m1.sent_at, "2026-07-16T10:00:00Z");
        assert_eq!(m1.preview, "preview M1");
        assert_eq!(m1.conversation_id, "C-M1");
        let m3 = by_id("M3");
        assert_eq!(m3.subject, "New one");
        assert_eq!(m3.from_addr, "alice@x");
        assert_eq!(m3.received_at, "2026-07-17T08:00:00Z");
        assert_eq!(msgs.len(), 3);

        let paths: Vec<String> = srv.requests().into_iter().map(|r| r.path).collect();
        assert!(
            !paths
                .iter()
                .any(|p| p == "/me/messages/M1" || p.starts_with("/me/messages/M1?")),
            "M1 was merged, not fetched: {paths:?}"
        );
        assert!(
            paths.iter().any(
                |p| p.starts_with("/me/messages/M3?$select=") && p.contains("receivedDateTime")
            ),
            "{paths:?}"
        );
        let notices: Vec<&String> = pass2_events
            .iter()
            .filter_map(|e| match e {
                SyncEvent::Error(m) => Some(m),
                _ => None,
            })
            .collect();
        // Only M3 was left header-less; M1's merged read change is not
        // reported.
        assert_eq!(
            notices,
            vec![
                "mail sync: 1 message(s) in folder Inbox were stored without headers; fetching them"
            ],
        );
        assert!(
            notices
                .iter()
                .all(|m| !m.contains("AT") && !m.contains("alice"))
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs one sync pass (a backfill returning only M1) over a store seeded
    /// with header-less rows `seed` in F1, as an older build left them, with
    /// `routes` answering their fetches, until `updates` `MessagesUpdated`
    /// for F1 have come (2 when the repair changes a row) and the pass has
    /// settled. Returns the store,
    /// the pass's events and the paths requested.
    fn repair_pass(
        tag: &str,
        seed: &[&str],
        routes: Vec<Route>,
        updates: usize,
    ) -> (Store, Vec<SyncEvent>, Vec<String>, PathBuf) {
        let mut all = routes;
        all.extend(backfill_routes());
        let srv = FakeServer::start(all);
        let base = srv.base_url.clone();
        let dir = unique_dir(tag);
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);
        {
            let store = Store::open(&store_path).unwrap();
            store
                .upsert_folder(&crate::graph::model::MailFolder {
                    id: "F1".into(),
                    display_name: "Inbox".into(),
                    parent_id: None,
                    total_count: 0,
                    unread_count: 0,
                    well_known_name: Some("inbox".into()),
                })
                .unwrap();
            for id in seed {
                store
                    .merge_message(
                        "F1",
                        &Message {
                            id: (*id).into(),
                            conversation_id: String::new(),
                            subject: String::new(),
                            from: Default::default(),
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
                        },
                        &crate::graph::model::MessageFields::default(),
                    )
                    .unwrap();
            }
        }
        let handle = spawn_with_bases(
            store_path.clone(),
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );
        let mut events = wait_for_updates(&handle.evt_rx, "F1", updates);
        // The pass then settles (also after a repair abort), so every
        // request it made has been recorded.
        events.extend(wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::State(st) if *st != SyncState::Syncing),
        ));
        let paths = srv.requests().into_iter().map(|r| r.path).collect();
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        (Store::open(&store_path).unwrap(), events, paths, dir)
    }

    #[test]
    fn headerless_row_is_repaired_on_next_pass() {
        let (store, _, paths, dir) = repair_pass(
            "repair",
            &["M9"],
            vec![get_route(
                "/me/messages/M9",
                200,
                &full_message_json("M9", "Was blank", "2026-07-15T08:00:00Z"),
            )],
            2,
        );
        let msgs = store.messages_in_folder("F1", 50, 0).unwrap();
        let m9 = msgs.iter().find(|m| m.id == "M9").unwrap();
        assert_eq!(m9.subject, "Was blank");
        assert_eq!(m9.from_addr, "alice@x");
        assert_eq!(m9.received_at, "2026-07-15T08:00:00Z");
        assert!(!m9.is_read, "the fetched isRead replaces the stored one");
        assert!(
            paths.iter().any(
                |p| p.starts_with("/me/messages/M9?$select=") && p.contains("receivedDateTime")
            ),
            "{paths:?}"
        );
        assert!(store.headerless_message_ids("F1", 10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repair_deletes_row_graph_no_longer_has() {
        let (store, _, paths, dir) = repair_pass(
            "repair-404",
            &["M7"],
            vec![get_route("/me/messages/M7", 404, "{}")],
            2,
        );
        assert!(paths.iter().any(|p| p.starts_with("/me/messages/M7?")));
        let ids: Vec<String> = store
            .messages_in_folder("F1", 50, 0)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec!["M1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repair fetch that aborts the pass (here a 429) must not hide the
    /// delta changes already stored: `MessagesUpdated` for the folder came
    /// before the repair, and the row stays header-less for a later pass.
    #[test]
    fn throttled_repair_still_reports_the_folders_delta() {
        let mut throttled = get_route("/me/messages/M9", 429, "{}");
        throttled.headers = vec![("Retry-After".into(), "3600".into())];
        let (store, events, paths, dir) = repair_pass("repair-429", &["M9"], vec![throttled], 1);
        assert!(paths.iter().any(|p| p.starts_with("/me/messages/M9?")));
        let pos = |pred: &dyn Fn(&SyncEvent) -> bool| events.iter().position(pred);
        let notice = pos(&|e| matches!(e, SyncEvent::Error(m) if m.contains("without headers")));
        let updated =
            pos(&|e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"));
        assert!(updated.is_some() && updated < notice, "{events:?}");
        // The delta's M1 is stored; M9 waits for the next pass.
        assert!(
            store
                .messages_in_folder("F1", 50, 0)
                .unwrap()
                .iter()
                .any(|m| m.id == "M1")
        );
        assert_eq!(store.headerless_message_ids("F1", 10).unwrap(), vec!["M9"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repair_continues_past_a_failing_id() {
        let (store, events, paths, dir) = repair_pass(
            "repair-400",
            &["M8", "M9"],
            vec![
                get_route(
                    "/me/messages/M8",
                    400,
                    r#"{"error":{"code":"ErrorInvalidIdMalformed"}}"#,
                ),
                get_route(
                    "/me/messages/M9",
                    200,
                    &full_message_json("M9", "Still repaired", "2026-07-15T08:00:00Z"),
                ),
            ],
            2,
        );
        let p8 = paths.iter().position(|p| p.starts_with("/me/messages/M8?"));
        let p9 = paths.iter().position(|p| p.starts_with("/me/messages/M9?"));
        assert!(p8.is_some() && p8 < p9, "M8 first, then M9: {paths:?}");
        let msgs = store.messages_in_folder("F1", 50, 0).unwrap();
        assert_eq!(
            msgs.iter().find(|m| m.id == "M9").unwrap().subject,
            "Still repaired"
        );
        // M8 stays header-less for a later pass.
        assert_eq!(store.headerless_message_ids("F1", 10).unwrap(), vec!["M8"]);
        assert!(
            events.iter().any(|e| matches!(
                e,
                SyncEvent::Error(m) if m.contains("could not fetch 1 message(s)") && m.contains("Inbox")
            )),
            "{events:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mutation_optimistically_writes_enqueues_and_drains() {
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

        let dir = unique_dir("mutate");
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

        // Let the backfill land first so M1 exists locally.
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );

        handle
            .cmd_tx
            .send(SyncCommand::MarkRead {
                id: "M1".into(),
                read: true,
            })
            .unwrap();

        // The optimistic local write + drain should PATCH the message on Graph.
        if !wait_until(ENGINE_WAIT, || {
            srv.requests()
                .iter()
                .any(|r| r.method == "PATCH" && r.path.starts_with("/me/messages/M1"))
        }) {
            panic!("no PATCH observed; requests: {:?}", srv.requests());
        }

        let patch = srv
            .requests()
            .into_iter()
            .find(|r| r.method == "PATCH")
            .unwrap();
        assert_eq!(patch.body, r#"{"isRead":true}"#);

        // Local row reflects the optimistic write immediately.
        let store = Store::open(&store_path).unwrap();
        assert!(store.messages_in_folder("F1", 50, 0).unwrap()[0].is_read);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Routes for a two-folder mailbox (F1 + DEST) plus a `/move` endpoint, so a
    // Move command's optimistic local re-file has a real destination folder to
    // land in (the schema FKs `messages.folder_id` to `folders.id`).
    fn move_routes() -> Vec<Route> {
        vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/M1/move".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"M1-NEW"}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/childFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/messages/delta".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"M1","conversationId":"C1","subject":"Hello","from":{"emailAddress":{"name":"Alice","address":"alice@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"2026-07-16T10:00:00Z","sentDateTime":"2026-07-16T09:00:00Z","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"hi"}],"@odata.deltaLink":"/me/mailFolders/F1/messages/delta?token=D1"}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/DEST/childFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/DEST/messages/delta".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[],"@odata.deltaLink":"/me/mailFolders/DEST/messages/delta?token=D2"}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"F1","displayName":"Inbox","parentFolderId":null,"totalItemCount":1,"unreadItemCount":1,"wellKnownName":"inbox"},{"id":"DEST","displayName":"Archive","parentFolderId":null,"totalItemCount":0,"unreadItemCount":0,"wellKnownName":"archive"}]}"#.into(),
            },
        ]
    }

    #[test]
    fn move_command_optimistically_refiles_locally() {
        let srv = FakeServer::start(move_routes());
        let base = srv.base_url.clone();

        let dir = unique_dir("move");
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

        // Wait for the backfill of F1 so M1 exists locally.
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );

        handle
            .cmd_tx
            .send(SyncCommand::Move {
                id: "M1".into(),
                dest: "DEST".into(),
            })
            .unwrap();

        // The optimistic local re-file happens before the outbox drain POSTs
        // the move, so once the POST is observed the local row is already in
        // DEST.
        if !wait_until(ENGINE_WAIT, || {
            srv.requests()
                .iter()
                .any(|r| r.method == "POST" && r.path.starts_with("/me/messages/M1/move"))
        }) {
            panic!("no move POST observed; requests: {:?}", srv.requests());
        }

        let store = Store::open(&store_path).unwrap();
        assert!(store.messages_in_folder("F1", 50, 0).unwrap().is_empty());
        assert_eq!(store.messages_in_folder("DEST", 50, 0).unwrap()[0].id, "M1");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Routes for the contacts tests: one folder (F1) with one message from
    // alice@x to bob@x (so local mining captures both correspondents), plus
    // `/me/people` — `people_status`/`people_body` let each test control what
    // the directory endpoint answers with (a 200 with a person, or a 403).
    fn contacts_routes(people_status: u16, people_body: &str) -> Vec<Route> {
        vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/people".into(),
                status: people_status,
                headers: vec![],
                body: people_body.to_string(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/childFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/messages/delta".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"M1","conversationId":"C1","subject":"Hello","from":{"emailAddress":{"name":"Alice","address":"alice@x"}},"toRecipients":[{"emailAddress":{"name":"Bob","address":"bob@x"}}],"ccRecipients":[],"receivedDateTime":"2026-07-16T10:00:00Z","sentDateTime":"2026-07-16T09:00:00Z","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"hi"}],"@odata.deltaLink":"/me/mailFolders/F1/messages/delta?token=D1"}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"F1","displayName":"Inbox","parentFolderId":null,"totalItemCount":1,"unreadItemCount":1,"wellKnownName":"inbox"}]}"#.into(),
            },
        ]
    }

    /// Polls the store for a contact search to return a hit. `sync_pass`
    /// runs `refresh_local_contacts`/`sync_people` AFTER the per-folder loop
    /// (and its `MessagesUpdated` emissions), so there's no event marking
    /// "contacts are ready" — this is the same poll-until-observed pattern
    /// `move_command_optimistically_refiles_locally` uses to wait for the
    /// move POST to land.
    fn wait_for_contact(store_path: &std::path::Path, query: &str) -> Vec<crate::store::Contact> {
        let mut hits = Vec::new();
        let found = wait_until(ENGINE_WAIT, || {
            let store = Store::open(store_path).unwrap();
            hits = store.search_contacts(query, 1).unwrap();
            !hits.is_empty()
        });
        if !found {
            panic!("timed out waiting for a contact matching {query:?}");
        }
        hits
    }

    #[test]
    fn sync_pass_populates_contacts_from_local_mail_and_people() {
        let people_body = r#"{"value":[{"displayName":"Carol","scoredEmailAddresses":[{"address":"carol@x.com","relevanceScore":5.0}]}]}"#;
        let srv = FakeServer::start(contacts_routes(200, people_body));
        let base = srv.base_url.clone();

        let dir = unique_dir("contacts-ok");
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

        // Local mining captured the correspondents.
        assert!(!wait_for_contact(&store_path, "alice").is_empty());
        assert!(!wait_for_contact(&store_path, "bob").is_empty());

        // /me/people augmented with an org person the user never emailed.
        let carol = wait_for_contact(&store_path, "carol");
        assert_eq!(
            carol.first().map(|c| c.address.as_str()),
            Some("carol@x.com")
        );
        assert_eq!(carol[0].source, "graph");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn people_403_does_not_fail_the_sync_pass() {
        let srv = FakeServer::start(contacts_routes(
            403,
            r#"{"error":{"code":"Authorization_RequestDenied"}}"#,
        ));
        let base = srv.base_url.clone();

        let dir = unique_dir("contacts-403");
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

        // The pass still completes and local contacts are present; no panic,
        // no error surfaced despite the 403 from `/me/people`.
        assert!(!wait_for_contact(&store_path, "alice").is_empty());

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quarantine_drops_op_and_clears_delta_links_for_reconverge() {
        // A Delete op whose Graph call always 404s (a non-retryable 4xx). After
        // MAX_OP_ATTEMPTS drains it must be quarantined (dropped + Error), and
        // the reconverge must clear stored delta links so the next full pass
        // re-fetches server truth.
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "DELETE".into(),
                path_prefix: "/me/messages/M1".into(),
                status: 404,
                headers: vec![],
                body: "{}".into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("quarantine");
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

        // Backfill first, so F1 gets a stored (followable) delta link.
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );

        // The Delete command drains once (attempt 1); each Refresh drains again
        // (attempts 2..5). The 4th Refresh is the 5th failed drain and
        // quarantines. Exactly 4 Refreshes on purpose: a trailing Refresh would
        // re-store F1's delta link (its folder-delta step precedes the drain
        // that clears links), defeating the reconverge assertion below.
        handle
            .cmd_tx
            .send(SyncCommand::Delete { id: "M1".into() })
            .unwrap();
        for _ in 0..4 {
            handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
        }

        let events = wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::Error(m) if m.contains("quarantined")),
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::Error(m) if m.contains("quarantined"))),
            "expected a quarantine Error event, saw: {events:?}"
        );

        // The op was dropped and every folder's delta link was nulled so the
        // next pass reconverges from the server.
        let store = Store::open(&store_path).unwrap();
        assert!(store.pending_ops().unwrap().is_empty());
        assert!(store.get_delta_link("F1").unwrap().is_none());

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reconverge_ignores_age_cutoff_and_restores_old_message() {
        // Fix A: a windowed backfill skips messages older than `now -
        // backfill_days`, but the quarantine reconverge must restore a
        // wrongly-removed message of ANY age. Here F1's delta always returns a
        // 2020-dated M1 with NO deltaLink, so every ordinary pass is a windowed
        // backfill that SKIPS M1 (older than a 30-day cutoff). Only the
        // reconverge pass (cutoff ignored) upserts it — so M1 appearing locally
        // proves the reconverge ran with the cutoff disabled.
        let routes = vec![
            Route {
                method: "DELETE".into(),
                path_prefix: "/me/messages/M1".into(),
                status: 404,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/childFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/messages/delta".into(),
                status: 200,
                headers: vec![],
                // Old message, and no deltaLink → every pass stays a first-time
                // (windowed) backfill.
                body: r#"{"value":[{"id":"M1","conversationId":"C1","subject":"Ancient","from":{"emailAddress":{"name":"Alice","address":"alice@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"2020-01-01T00:00:00Z","sentDateTime":"2020-01-01T00:00:00Z","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"old"}]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"F1","displayName":"Inbox","parentFolderId":null,"totalItemCount":1,"unreadItemCount":1,"wellKnownName":"inbox"}]}"#.into(),
            },
        ];
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("reconverge");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);

        // 30-day window: the 2020 message is far outside it.
        let handle = spawn_with_bases(
            store_path.clone(),
            token_path,
            test_cfg(),
            30,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );

        // Backfill lands (M1 skipped as too old — the store is empty).
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );
        assert!(
            Store::open(&store_path)
                .unwrap()
                .messages_in_folder("F1", 50, 0)
                .unwrap()
                .is_empty(),
            "windowed backfill should have skipped the 2020 message"
        );

        // Delete op always 404s → quarantine after 5 failed drains (Delete +
        // 4 Refreshes). A 5th Refresh then runs the reconverge pass. No stored
        // deltaLink means a trailing Refresh is safe (nothing to re-store).
        handle
            .cmd_tx
            .send(SyncCommand::Delete { id: "M1".into() })
            .unwrap();
        for _ in 0..5 {
            handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
        }
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::Error(m) if m.contains("quarantined")),
        );

        // The reconverge pass ignores the cutoff and re-adds the old M1.
        if !wait_until(ENGINE_WAIT, || {
            Store::open(&store_path)
                .unwrap()
                .messages_in_folder("F1", 50, 0)
                .unwrap()
                .iter()
                .any(|m| m.id == "M1")
        }) {
            panic!("reconverge did not restore the aged-out message");
        }

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tick_reenumerates_folders_when_folder_set_is_empty() {
        // Fix 2 decision-logic coverage: when the local folder set is empty
        // (the observable proxy for "initial backfill never landed", e.g.
        // launched offline), a periodic tick must run a FULL pass and re-hit
        // `/me/mailFolders` rather than the incremental delta path. A server
        // that returns an empty folder list keeps `folders()` empty across
        // ticks, so `/me/mailFolders` should be requested more than once.
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/mailFolders".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[]}"#.into(),
        }]);
        let base = srv.base_url.clone();

        let dir = unique_dir("reenum");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);

        // Short tick so ticks fire quickly; no error path means no back-off
        // gate, so ticks run steadily.
        let handle = spawn_with_bases(
            store_path,
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_millis(100),
        );

        // Startup enumeration lands first.
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));

        // At least one tick must re-request the folder list (>1 total).
        if !wait_until(ENGINE_WAIT, || {
            srv.requests()
                .iter()
                .filter(|r| r.method == "GET" && r.path.starts_with("/me/mailFolders"))
                .count()
                >= 2
        }) {
            panic!(
                "tick did not re-enumerate folders; requests: {:?}",
                srv.requests()
            );
        }

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_single_offline_tick_advances_backoff_by_one_step_not_two() {
        // Final v2 review, Fix 2: `on_tick` used to call `refresh_calendar()`
        // unconditionally after `sync_pass`. When `sync_pass` itself hit a
        // transport error, it already called `go_offline` (doubling the
        // back-off) and returned early — but the unconditional
        // `refresh_calendar()` right after made its OWN doomed Graph call,
        // hit the same transport error, and called `go_offline` a SECOND
        // time in the same tick, advancing the back-off two steps instead of
        // one and emitting a spurious extra `Offline` transition.
        //
        // `FakeServer` always answers with a valid canned HTTP response, so
        // it can't produce a transport-level failure; this test instead uses
        // a bare TCP listener that accepts each connection and drops it
        // without writing anything, forcing `ureq` (and thus `GraphClient`)
        // to see `GraphError::Transport` — exactly the class `go_offline`
        // reacts to. Counting *accepted connections* is a reliable proxy for
        // "how many Graph calls did this tick make" without reaching into
        // the engine's private back-off state.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback port");
        let port = listener.local_addr().expect("local_addr").port();
        let attempts = Arc::new(AtomicUsize::new(0));
        let dead_shutdown = Arc::new(AtomicBool::new(false));
        let thread_attempts = Arc::clone(&attempts);
        let thread_shutdown = Arc::clone(&dead_shutdown);
        let dead_server = thread::spawn(move || {
            for stream in listener.incoming() {
                if thread_shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                thread_attempts.fetch_add(1, Ordering::SeqCst);
                drop(stream); // close without responding: forces a transport error
            }
        });
        let base = format!("http://127.0.0.1:{port}");

        let dir = unique_dir("offline-single-backoff-step");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);

        // Short tick: the first attempt is `startup`'s own sync pass (which
        // goes offline immediately, scheduling `next_retry` at BACKOFF_MIN),
        // so the tick itself fires once that elapses; a short tick here just
        // avoids adding extra wall-clock time on top of that.
        let handle = spawn_with_bases(
            store_path,
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_millis(50),
        );

        // Wait for 2 accepted connections: startup's sync_pass, then the
        // first tick's sync_pass (gated behind BACKOFF_MIN, ~5s).
        if !wait_until(BACKOFF_MIN + ENGINE_WAIT, || {
            attempts.load(Ordering::SeqCst) >= 2
        }) {
            panic!(
                "did not observe 2 connection attempts in time (saw {})",
                attempts.load(Ordering::SeqCst)
            );
        }

        // Give a (buggy) extra `refresh_calendar` attempt in that same tick a
        // moment to land, then assert it never does: exactly 2 attempts
        // total (1 from startup + 1 from the tick's mail sync), never 3.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "refresh_calendar must be skipped in the same tick sync_pass just went offline in \
             (a 3rd attempt means it ran anyway and doubled the back-off twice)"
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        dead_shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", port));
        let _ = dead_server.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_attachment_writes_bytes_and_emits_saved_path() {
        // "aGVsbG8=" is the base64 Graph would send for a `fileAttachment`
        // whose bytes are "hello" (same fixture `GraphClient`'s own
        // `get_attachment_bytes_decodes_base64` test uses).
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/M1/attachments/A1".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"A1","contentBytes":"aGVsbG8="}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("save-attachment");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);
        let dest = dir.join("downloads").join("f.txt");

        let handle = spawn_with_bases(
            store_path,
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );

        // Let the backfill land first so M1 exists locally (not strictly
        // required for the Graph fetch itself, but keeps the fixture
        // realistic and lets us wait on a well-defined signal first).
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );

        handle
            .cmd_tx
            .send(SyncCommand::SaveAttachment {
                message_id: "M1".into(),
                attachment_id: "A1".into(),
                dest: dest.clone(),
            })
            .unwrap();

        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::AttachmentSaved { .. })
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::AttachmentSaved { path } if path == &dest))
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_item_attachment_picks_ics_or_eml_by_content() {
        // Two nested-item attachments on the same message: A1's raw `/$value`
        // body is an iCalendar invite, A2's is an RFC822-ish message — the
        // engine must sniff the bytes and pick `.ics`/`.eml` accordingly.
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/M1/attachments/A1/$value".into(),
                status: 200,
                headers: vec![],
                body: "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".into(),
            },
        );
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/M1/attachments/A2/$value".into(),
                status: 200,
                headers: vec![],
                body: "Received: x\r\n\r\nbody".into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("save-item-attachment");
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

        // Let the backfill land first so M1 exists locally (same rationale as
        // `save_attachment_writes_bytes_and_emits_saved_path`).
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );

        // ICS case.
        handle
            .cmd_tx
            .send(SyncCommand::SaveItemAttachment {
                message_id: "M1".into(),
                attachment_id: "A1".into(),
                dest_base: dir.join("downloads").join("invite"),
            })
            .unwrap();
        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::AttachmentSaved { .. })
        });
        let ics_path = events
            .iter()
            .find_map(|e| match e {
                SyncEvent::AttachmentSaved { path } => Some(path.clone()),
                _ => None,
            })
            .expect("AttachmentSaved");
        assert_eq!(ics_path.extension().unwrap(), "ics");
        assert!(ics_path.exists());
        assert_eq!(
            std::fs::read(&ics_path).unwrap(),
            b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"
        );

        // EML case.
        handle
            .cmd_tx
            .send(SyncCommand::SaveItemAttachment {
                message_id: "M1".into(),
                attachment_id: "A2".into(),
                dest_base: dir.join("downloads").join("message"),
            })
            .unwrap();
        let events = wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::AttachmentSaved { path } if path.extension().unwrap() == "eml"),
        );
        let eml_path = events
            .iter()
            .find_map(|e| match e {
                SyncEvent::AttachmentSaved { path } if path.extension().unwrap() == "eml" => {
                    Some(path.clone())
                }
                _ => None,
            })
            .expect("AttachmentSaved");
        assert_eq!(eml_path.extension().unwrap(), "eml");
        assert!(eml_path.exists());
        assert_eq!(
            std::fs::read(&eml_path).unwrap(),
            b"Received: x\r\n\r\nbody"
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
                comment: None,
                proposed_start_utc: None,
                proposed_end_utc: None,
            })
            .unwrap();
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MeetingResponded { message_id, kind }
                if message_id == "M1" && *kind == RsvpKind::Accept)
        });
        // The accept POST actually hit Graph.
        assert!(srv.requests().iter().any(|r| r.path.ends_with("/accept")));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { .. })
        });
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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MeetingResponded { .. })
        });
        let posted = srv
            .requests()
            .into_iter()
            .find(|r| r.path.ends_with("/decline"))
            .unwrap();
        let sent = crate::json::parse(&posted.body).unwrap();
        assert!(sent.get("proposedNewTime").is_some());
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
                comment: None,
                proposed_start_utc: None,
                proposed_end_utc: None,
            })
            .unwrap();
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::Error(_)));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_automatic_replies_emits_fetched() {
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailboxSettings".into(),
                status: 200,
                headers: vec![],
                body: r#"{"automaticRepliesSetting":{"status":"alwaysEnabled","externalAudience":"all","internalReplyMessage":"hi","externalReplyMessage":"bye"}}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir("fetch-oof");
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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { .. })
        });
        handle
            .cmd_tx
            .send(SyncCommand::FetchAutomaticReplies)
            .unwrap();
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::AutomaticRepliesFetched { replies }
                if replies.status == crate::graph::model::OofStatus::AlwaysEnabled
                    && replies.internal_message == "hi")
        });
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Spawns an engine signed in with `seed_token` against `routes` placed
    /// ahead of `backfill_routes`, and waits for its first backfill.
    fn spawn_signed_in(tag: &str, routes: Vec<Route>) -> (FakeServer, SyncHandle, PathBuf) {
        let mut all = routes;
        all.extend(backfill_routes());
        let srv = FakeServer::start(all);
        let base = srv.base_url.clone();
        let dir = unique_dir(tag);
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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { .. })
        });
        (srv, handle, dir)
    }

    #[test]
    fn fetch_profile_emits_profile_fetched() {
        let (srv, handle, dir) = spawn_signed_in(
            "fetch-profile",
            vec![get_route(
                "/me?$select",
                200,
                r#"{"displayName":"Ada Lovelace","mail":null,"userPrincipalName":"ada@outlook.com"}"#,
            )],
        );
        handle.cmd_tx.send(SyncCommand::FetchProfile).unwrap();
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::ProfileFetched { me }
                if me.display_name == "Ada Lovelace" && me.address() == "ada@outlook.com")
        });
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.path == "/me?$select=displayName,mail,userPrincipalName")
        );
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_photo_emits_photo_fetched() {
        let (_srv, handle, dir) = spawn_signed_in(
            "fetch-photo",
            vec![Route {
                method: "GET".into(),
                path_prefix: "/me/photo/$value".into(),
                status: 200,
                headers: vec![("Content-Type".into(), "image/jpeg".into())],
                body: "JPEGDATA".into(),
            }],
        );
        handle.cmd_tx.send(SyncCommand::FetchPhoto).unwrap();
        let seen = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::PhotoFetched { .. })
        });
        assert_eq!(
            seen.last(),
            Some(&SyncEvent::PhotoFetched {
                photo: Some((b"JPEGDATA".to_vec(), "image/jpeg".to_string()))
            })
        );
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_photo_none_when_no_photo() {
        let (srv, handle, dir) = spawn_signed_in(
            "fetch-no-photo",
            vec![
                get_route("/me/photo/$value", 404, "{}"),
                get_route("/me/photos/48x48/$value", 404, "{}"),
            ],
        );
        handle.cmd_tx.send(SyncCommand::FetchPhoto).unwrap();
        let seen = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::PhotoFetched { .. })
        });
        assert_eq!(seen.last(), Some(&SyncEvent::PhotoFetched { photo: None }));
        // A missing photo is not an error.
        assert!(
            !seen.iter().any(|e| matches!(e, SyncEvent::Error(_))),
            "{seen:?}"
        );
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.path == "/me/photos/48x48/$value")
        );
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_profile_and_photo_signed_out_report_not_signed_in() {
        let srv = FakeServer::start(vec![]);
        let base = srv.base_url.clone();
        let dir = unique_dir("profile-signed-out");
        let handle = spawn_with_bases(
            dir.join("mail.db"),
            dir.join("token.bin"),
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));
        for cmd in [SyncCommand::FetchProfile, SyncCommand::FetchPhoto] {
            handle.cmd_tx.send(cmd).unwrap();
            wait_for(
                &handle.evt_rx,
                |e| matches!(e, SyncEvent::Error(m) if m == "not signed in"),
            );
        }
        assert!(srv.requests().is_empty(), "{:?}", srv.requests());
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sign_out_removes_token_keeps_store() {
        let (srv, handle, dir) = spawn_signed_in(
            "sign-out",
            vec![get_route(
                "/me?$select",
                200,
                r#"{"displayName":"Ada","mail":"ada@x","userPrincipalName":"ada@x"}"#,
            )],
        );
        let token_path = dir.join("token.bin");
        let store_path = dir.join("mail.db");
        assert!(token_path.exists());

        handle.cmd_tx.send(SyncCommand::SignOut).unwrap();
        let seen = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::State(SyncState::SignInRequired))
        });
        assert!(
            seen.iter().any(|e| matches!(e, SyncEvent::SignInRequired)),
            "{seen:?}"
        );
        assert!(!token_path.exists());
        assert!(tokencache::load(&token_path).unwrap().is_none());

        // The mail store and its rows are kept.
        let store = Store::open(&store_path).unwrap();
        let msgs = store.messages_in_folder("F1", 50, 0).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, "M1");

        // A later direct call reports sign-in required, with no request.
        handle.cmd_tx.send(SyncCommand::FetchProfile).unwrap();
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::Error(m) if m == "not signed in"),
        );
        assert!(!srv.requests().iter().any(|r| r.path.starts_with("/me?")));

        // Signing out again is harmless: still signed out, still says so with
        // `SignInRequired`, but no `State`, as the state did not change. The
        // `FetchProfile` after it marks the end of what that SignOut emitted.
        handle.cmd_tx.send(SyncCommand::SignOut).unwrap();
        handle.cmd_tx.send(SyncCommand::FetchProfile).unwrap();
        let seen = wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::Error(m) if m == "not signed in"),
        );
        assert!(
            seen.iter().any(|e| matches!(e, SyncEvent::SignInRequired)),
            "{seen:?}"
        );
        assert!(
            !seen.iter().any(|e| matches!(e, SyncEvent::State(_))),
            "{seen:?}"
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);

        // The next start on the same paths finds no saved sign-in.
        let base = srv.base_url.clone();
        let again = spawn_with_bases(
            store_path.clone(),
            token_path.clone(),
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );
        let seen = wait_for(&again.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));
        assert!(
            !seen.iter().any(|e| matches!(e, SyncEvent::FoldersUpdated)),
            "{seen:?}"
        );
        let _ = again.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_automatic_replies_patches_and_emits_updated() {
        use crate::graph::model::{AutomaticReplies, ExternalAudience, OofStatus};
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/mailboxSettings".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir("set-oof");
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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { .. })
        });
        handle
            .cmd_tx
            .send(SyncCommand::SetAutomaticReplies {
                replies: AutomaticReplies {
                    status: OofStatus::Disabled,
                    external_audience: ExternalAudience::All,
                    internal_message: "x".into(),
                    external_message: "y".into(),
                    scheduled_start_utc: "".into(),
                    scheduled_end_utc: "".into(),
                },
            })
            .unwrap();
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::AutomaticRepliesUpdated)
        });
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.method == "PATCH" && r.path.contains("/me/mailboxSettings"))
        );
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_schedule_emits_entries() {
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "POST".into(),
                path_prefix: "/me/calendar/getSchedule".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"scheduleId":"me@x","availabilityView":"000222"}]}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir("fetch-schedule");
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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { .. })
        });
        handle
            .cmd_tx
            .send(SyncCommand::FetchSchedule {
                schedules: vec!["me@x".into()],
                start_utc: "2026-07-21T08:00:00Z".into(),
                end_utc: "2026-07-21T18:00:00Z".into(),
                interval_minutes: 30,
            })
            .unwrap();
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::ScheduleFetched { entries } if entries[0].email == "me@x"),
        );
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::CategoriesUpdated)
        });
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
        // No completion event and no re-sync (tick is 3600s). Poll for the
        // drain's PATCH — the definitive "command fully processed" signal,
        // since the optimistic local write happens earlier in the same
        // `handle_command` call. Asserting the PATCH immediately would
        // race the drain on a slow CI machine.
        let patched = wait_until(ENGINE_WAIT, || {
            srv.requests()
                .iter()
                .any(|r| r.method == "PATCH" && r.path.contains("/me/messages/M1"))
        });
        assert!(patched, "SetCategories did not PATCH");
        // The PATCH ran, so the earlier optimistic local write is committed.
        let store = Store::open(&store_path).unwrap();
        let rows = store.messages_in_folder("F1", 50, 0).unwrap();
        assert_eq!(
            rows.iter().find(|m| m.id == "M1").unwrap().categories,
            vec!["Work".to_string()]
        );
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_inline_image_emits_bytes_with_content_id() {
        // Same route fixture as save_attachment's test: GET the attachment
        // returns base64 "hello".
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/M1/attachments/A1".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"A1","contentBytes":"aGVsbG8="}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("fetch-inline-image");
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
            .send(SyncCommand::FetchInlineImage {
                message_id: "M1".into(),
                attachment_id: "A1".into(),
                content_id: "logo".into(),
            })
            .unwrap();

        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::InlineImageReady { .. })
        });
        let evt = events
            .into_iter()
            .find(|e| matches!(e, SyncEvent::InlineImageReady { .. }))
            .unwrap();
        match evt {
            SyncEvent::InlineImageReady {
                message_id,
                content_id,
                bytes,
            } => {
                assert_eq!(message_id, "M1");
                assert_eq!(content_id, "logo");
                assert_eq!(bytes, b"hello");
            }
            other => panic!("expected InlineImageReady, got {other:?}"),
        }

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetch_attachments_populates_store_and_emits_updated() {
        // Nothing in production ever calls `list_attachments`/`put_attachments`
        // otherwise, so this is the only path that ever gets real attachment
        // metadata into the store for a real mailbox.
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/M1/attachments".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"A1","name":"f.txt","contentType":"text/plain","size":3,"isInline":false}]}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("fetch-attachments");
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

        // Let the backfill land first so M1 exists locally.
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );

        handle
            .cmd_tx
            .send(SyncCommand::FetchAttachments {
                message_id: "M1".into(),
            })
            .unwrap();

        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::AttachmentsUpdated { .. })
        });
        assert!(events.iter().any(
            |e| matches!(e, SyncEvent::AttachmentsUpdated { message_id } if message_id == "M1")
        ));

        let store = Store::open(&store_path).unwrap();
        let atts = store.attachments("M1").unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].name, "f.txt");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Body format (#1901) ------------------------------------------------

    /// `backfill_routes` with `extra` routes ahead of them (the fake server
    /// matches the first prefix), an engine spawned with `options` on it,
    /// and the backfill landed (so M1 exists locally).
    fn body_engine(
        tag: &str,
        extra: Vec<Route>,
        options: EngineOptions,
    ) -> (FakeServer, SyncHandle, PathBuf, PathBuf) {
        let mut routes = extra;
        routes.extend(backfill_routes());
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir(tag);
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);
        let handle = spawn_with_bases_and_options(
            store_path.clone(),
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
            options,
        );
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"),
        );
        (srv, handle, store_path, dir)
    }

    fn html_options() -> EngineOptions {
        EngineOptions {
            body_format: BodyFormat::Html,
            ..EngineOptions::without_prefetch()
        }
    }

    /// M1's body GET (`get_body`'s path), answering `content_type`/`content`.
    fn body_route(content_type: &str, content: &str) -> Route {
        get_route(
            "/me/messages/M1?$select=body",
            200,
            &format!(r#"{{"body":{{"contentType":"{content_type}","content":"{content}"}}}}"#),
        )
    }

    fn fetch_m1_body(handle: &SyncHandle) {
        handle
            .cmd_tx
            .send(SyncCommand::FetchBody { id: "M1".into() })
            .unwrap();
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::BodyReady { id } if id == "M1"),
        );
    }

    /// The body GETs the server saw for M1.
    fn m1_body_gets(srv: &FakeServer) -> Vec<crate::testserver::RecordedRequest> {
        srv.requests()
            .into_iter()
            .filter(|r| r.method == "GET" && r.path.starts_with("/me/messages/M1?$select=body"))
            .collect()
    }

    fn prefers_text(r: &crate::testserver::RecordedRequest) -> bool {
        r.headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("prefer") && v == "outlook.body-content-type=\"text\""
        })
    }

    #[test]
    fn a_default_engine_fetches_bodies_as_text_and_records_it() {
        let (srv, handle, store_path, dir) = body_engine(
            "body-text",
            vec![body_route("text", "hi")],
            // The default format; bodies of its own off (#1978), as in
            // every test from before them.
            EngineOptions {
                body_format: EngineOptions::default().body_format,
                ..EngineOptions::without_prefetch()
            },
        );
        fetch_m1_body(&handle);

        let gets = m1_body_gets(&srv);
        assert_eq!(gets.len(), 1);
        assert!(prefers_text(&gets[0]), "headers: {:?}", gets[0].headers);
        let store = Store::open(&store_path).unwrap();
        assert_eq!(store.get_body("M1").unwrap().unwrap().content, "hi");
        assert!(
            store
                .get_body_for("M1", BodyFormat::Text)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_body_for("M1", BodyFormat::Html)
                .unwrap()
                .is_none()
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_html_engine_fetches_native_bodies_and_stores_html() {
        let (srv, handle, store_path, dir) = body_engine(
            "body-html",
            vec![body_route("html", "<p>hi</p>")],
            html_options(),
        );
        fetch_m1_body(&handle);

        let gets = m1_body_gets(&srv);
        assert_eq!(gets.len(), 1);
        assert!(
            !gets[0]
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("prefer")),
            "headers: {:?}",
            gets[0].headers
        );
        let store = Store::open(&store_path).unwrap();
        let body = store.get_body_for("M1", BodyFormat::Html).unwrap().unwrap();
        assert_eq!(body.content_type, "html");
        assert_eq!(body.content, "<p>hi</p>");
        assert!(
            store
                .get_body_for("M1", BodyFormat::Text)
                .unwrap()
                .is_none()
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_body_stored_as_text_is_fetched_as_html_once() {
        let (srv, handle, store_path, dir) = body_engine(
            "body-upgrade",
            vec![body_route("html", "<p>rich</p>")],
            html_options(),
        );
        // As an engine from before #1901 left it: fetched as text, no format.
        let store = Store::open(&store_path).unwrap();
        store
            .put_body(
                "M1",
                &crate::graph::model::Body {
                    content_type: "text".into(),
                    content: "plain".into(),
                },
            )
            .unwrap();

        // The embedder's gate, run as on two opens of the message.
        for _ in 0..2 {
            if store
                .get_body_for("M1", BodyFormat::Html)
                .unwrap()
                .is_none()
            {
                fetch_m1_body(&handle);
            }
        }

        assert_eq!(m1_body_gets(&srv).len(), 1);
        let body = store.get_body_for("M1", BodyFormat::Html).unwrap().unwrap();
        assert_eq!(body.content_type, "html");
        assert_eq!(body.content, "<p>rich</p>");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_native_text_body_fetched_as_html_is_not_fetched_again() {
        // A plain-text message's native body is text: it answers the `Html`
        // ask, so the gate must not send it back to Graph on every open.
        let (srv, handle, store_path, dir) = body_engine(
            "body-native-text",
            vec![body_route("text", "just text")],
            html_options(),
        );
        let store = Store::open(&store_path).unwrap();
        for _ in 0..2 {
            if store
                .get_body_for("M1", BodyFormat::Html)
                .unwrap()
                .is_none()
            {
                fetch_m1_body(&handle);
            }
        }

        assert_eq!(m1_body_gets(&srv).len(), 1);
        let body = store.get_body_for("M1", BodyFormat::Html).unwrap().unwrap();
        assert_eq!(body.content_type, "text");
        assert_eq!(body.content, "just text");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_plain_text_reply_draft_is_not_a_refetch_candidate() {
        // `createReply` of a plain-text message comes back as text, stored
        // with no fetch format; a FetchBody over the draft compose just
        // opened would lose the user's edits. The response here carries no
        // `isDraft` (so `is_draft` is 0, as a later complete delta item
        // leaves it anyway): the Drafts folder is what marks it a draft.
        let (srv, handle, store_path, dir) = body_engine(
            "body-reply-draft",
            vec![
                Route {
                    method: "POST".into(),
                    path_prefix: "/me/messages/M1/createReply".into(),
                    status: 200,
                    headers: vec![],
                    body: r#"{"id":"DRAFT1","conversationId":"C1","subject":"Re: Hello","from":{"emailAddress":{"name":"Alice","address":"alice@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":true,"hasAttachments":false,"importance":"normal","bodyPreview":""}"#.into(),
                },
                get_route(
                    "/me/messages/DRAFT1?$select=body",
                    200,
                    r#"{"body":{"contentType":"text","content":"> quoted"}}"#,
                ),
            ],
            html_options(),
        );
        handle
            .cmd_tx
            .send(SyncCommand::ComposeReply {
                id: "M1".into(),
                all: false,
            })
            .unwrap();
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::DraftReady { id } if id == "DRAFT1"),
        );

        let store = Store::open(&store_path).unwrap();
        assert!(!store.draft("DRAFT1").unwrap().unwrap().0.is_draft);
        let body = store
            .get_body_for("DRAFT1", BodyFormat::Html)
            .unwrap()
            .expect("a reply draft's body satisfies Html");
        assert_eq!(body.content_type, "text");
        assert_eq!(body.content, "> quoted");
        assert!(
            store
                .get_body_for("DRAFT1", BodyFormat::Text)
                .unwrap()
                .is_some()
        );
        let draft_gets = srv
            .requests()
            .into_iter()
            .filter(|r| r.path.starts_with("/me/messages/DRAFT1"))
            .count();
        assert_eq!(draft_gets, 1, "only store_composed_draft's own GET");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_html_engine_stores_inline_attachments_content_id() {
        let (_srv, handle, store_path, dir) = body_engine(
            "body-inline",
            vec![get_route(
                "/me/messages/M1/attachments",
                200,
                r##"{"value":[{"@odata.type":"#microsoft.graph.fileAttachment","id":"A1","name":"logo.png","contentType":"image/png","size":3,"isInline":true,"contentId":"logo@x"}]}"##,
            )],
            html_options(),
        );
        handle
            .cmd_tx
            .send(SyncCommand::FetchAttachments {
                message_id: "M1".into(),
            })
            .unwrap();
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::AttachmentsUpdated { message_id } if message_id == "M1"),
        );

        let store = Store::open(&store_path).unwrap();
        let atts = store.attachments("M1").unwrap();
        assert_eq!(atts.len(), 1);
        assert!(atts[0].is_inline);
        assert_eq!(atts[0].content_id.as_deref(), Some("logo@x"));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_sync_adopts_sentinel_drafts_into_the_real_drafts_folder() {
        // A draft created before the first folder sync lands under the local
        // sentinel folder id (see `Store::drafts_folder_id`). Once the sync
        // engine fetches a real Drafts folder, it must re-file that draft —
        // otherwise it stays permanently invisible in the real Drafts folder
        // view.
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/REAL-DRAFTS/childFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/REAL-DRAFTS/messages/delta".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[],"@odata.deltaLink":"/me/mailFolders/REAL-DRAFTS/messages/delta?token=D1"}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"REAL-DRAFTS","displayName":"Drafts","parentFolderId":null,"totalItemCount":0,"unreadItemCount":0,"wellKnownName":"drafts"}]}"#.into(),
            },
        ]);
        let base = srv.base_url.clone();

        let dir = unique_dir("adopt-sentinel");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);

        // Seed the sentinel-filed draft BEFORE the engine (and its own
        // `Store::open`) starts up.
        let local_id = {
            let store = Store::open(&store_path).unwrap();
            store
                .create_local_draft("Hi", "a@x", "", "<p>hi</p>")
                .unwrap()
        };

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
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "REAL-DRAFTS"),
        );

        let store = Store::open(&store_path).unwrap();
        let rows = store.messages_in_folder("REAL-DRAFTS", 50, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, local_id);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_reply_stores_draft_and_emits_ready() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/M1/createReply".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"DRAFT1","conversationId":"C1","subject":"Re: Hi","from":{"emailAddress":{"name":"Alice","address":"alice@x"}},"toRecipients":[{"emailAddress":{"name":"Bob","address":"bob@x"}}],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"","isDraft":true}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/DRAFT1".into(),
                status: 200,
                headers: vec![],
                body: r#"{"body":{"contentType":"html","content":"<p>quoted</p>"}}"#.into(),
            },
        ]);
        let base = srv.base_url.clone();

        let dir = unique_dir("compose-reply");
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

        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));

        handle
            .cmd_tx
            .send(SyncCommand::ComposeReply {
                id: "M1".into(),
                all: false,
            })
            .unwrap();

        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::DraftReady { .. })
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::DraftReady { id } if id == "DRAFT1"))
        );

        let store = Store::open(&store_path).unwrap();
        let (row, body) = store
            .draft("DRAFT1")
            .unwrap()
            .expect("reply draft should be stored");
        assert_eq!(row.subject, "Re: Hi");
        assert!(row.is_draft);
        assert_eq!(body.content, "<p>quoted</p>");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The IIS page an Exchange Online front end answers a bodiless POST
    /// with (#1954).
    const IIS_411_HTML: &str = "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"http://www.w3.org/TR/html4/strict.dtd\">\r\n<HTML><HEAD><TITLE>Length Required</TITLE>\r\n<META HTTP-EQUIV=\"Content-Type\" Content=\"text/html; charset=us-ascii\"></HEAD>\r\n<BODY><h2>Length Required</h2>\r\n<hr><p>HTTP Error 411. The request must be chunked or have a content length.</p>\r\n</BODY></HTML>\r\n";

    /// A Reply All refused with a 411 HTML page reaches the host as one
    /// line without markup, and opens no draft (#1954).
    #[test]
    fn compose_reply_all_411_emits_one_line_error() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/M1/createReplyAll".into(),
                status: 411,
                headers: vec![("Content-Type".into(), "text/html".into())],
                body: IIS_411_HTML.into(),
            },
        ]);
        let base = srv.base_url.clone();

        let dir = unique_dir("compose-reply-all-411");
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

        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));

        handle
            .cmd_tx
            .send(SyncCommand::ComposeReply {
                id: "M1".into(),
                all: true,
            })
            .unwrap();

        let events = wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::Error(m) if m.contains("411")),
        );
        let errors: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                SyncEvent::Error(m) if m.contains("411") => Some(m.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(errors, ["HTTP 411 Length Required"]);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SyncEvent::DraftReady { .. })),
            "a refused Reply All opens no draft: {events:?}"
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_forward_stores_draft_and_emits_ready() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/M1/createForward".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"DRAFT2","conversationId":"C1","subject":"Fwd: Hi","from":{"emailAddress":{"name":"Alice","address":"alice@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"","isDraft":true}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/DRAFT2".into(),
                status: 200,
                headers: vec![],
                body: r#"{"body":{"contentType":"html","content":"<p>fwd body</p>"}}"#.into(),
            },
        ]);
        let base = srv.base_url.clone();

        let dir = unique_dir("compose-forward");
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

        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));

        handle
            .cmd_tx
            .send(SyncCommand::ComposeForward { id: "M1".into() })
            .unwrap();

        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::DraftReady { .. })
        });
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::DraftReady { id } if id == "DRAFT2"))
        );

        let store = Store::open(&store_path).unwrap();
        let (row, body) = store
            .draft("DRAFT2")
            .unwrap()
            .expect("forward draft should be stored");
        assert_eq!(row.subject, "Fwd: Hi");
        assert_eq!(body.content, "<p>fwd body</p>");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Routes for a mailbox with only a synced Sent Items folder — enough for
    // `send_draft_of_a_local_draft_...` to exercise SendDraft's optimistic
    // "move toward Sent" against a real (FK-satisfying) folder id.
    fn sent_folder_routes() -> Vec<Route> {
        vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/SENT1/childFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/SENT1/messages/delta".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[],"@odata.deltaLink":"/me/mailFolders/SENT1/messages/delta?token=D1"}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"SENT1","displayName":"Sent Items","parentFolderId":null,"totalItemCount":0,"unreadItemCount":0,"wellKnownName":"sentitems"}]}"#.into(),
            },
        ]
    }

    #[test]
    fn send_draft_of_a_local_draft_creates_reconciles_sends_and_files_under_sent() {
        let mut routes = sent_folder_routes();
        // Specific routes precede the generic `/me/messages` POST route, same
        // ordering rationale as `move_routes()` above: the fake server
        // matches the FIRST route whose method+prefix matches.
        routes.insert(
            0,
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-42/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
        );
        routes.insert(
            1,
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages".into(),
                status: 201,
                headers: vec![],
                body: r#"{"id":"GRAPH-42","conversationId":"C","subject":"Hi","from":{"emailAddress":{"name":"","address":""}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"","isDraft":true}"#.into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("send-draft");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);

        // Seed a local draft directly through the store BEFORE the engine
        // opens it, so the row (and its sentinel Drafts folder) already
        // exist when the sync thread starts up.
        let local_id = {
            let store = Store::open(&store_path).unwrap();
            store
                .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
                .unwrap()
        };

        let handle = spawn_with_bases(
            store_path.clone(),
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );

        // Backfill first, so SENT1 exists locally and `sent_items_folder_id`
        // can resolve it.
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "SENT1"),
        );

        handle
            .cmd_tx
            .send(SyncCommand::SendDraft {
                id: local_id.clone(),
            })
            .unwrap();

        let events = wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::Sent { .. }));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::Sent { id } if id == &local_id))
        );

        let reqs = srv.requests();
        assert!(
            reqs.iter()
                .any(|r| r.method == "POST" && r.path == "/me/messages")
        );
        assert!(
            reqs.iter()
                .any(|r| r.method == "POST" && r.path.starts_with("/me/messages/GRAPH-42/send"))
        );

        let store = Store::open(&store_path).unwrap();
        // The local id still reaches the sent row, now stored under its
        // Graph id (#1884).
        let (row, _) = store.draft(&local_id).unwrap().unwrap();
        assert_eq!(row.id, "GRAPH-42");
        assert_eq!(row.folder_id, "SENT1");
        assert!(!row.is_draft);
        let sent_rows = store.messages_in_folder("SENT1", 50, 0).unwrap();
        assert_eq!(sent_rows.len(), 1);
        assert_eq!(sent_rows[0].id, "GRAPH-42");
        assert!(!sent_rows[0].is_draft);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_query_decodes_pairs() {
        let params = parse_query("code=ABC%2FDEF&state=xyz");
        assert_eq!(
            params,
            vec![
                ("code".to_string(), "ABC/DEF".to_string()),
                ("state".to_string(), "xyz".to_string()),
            ]
        );
    }

    // --- Granted scopes and error_description (#1875) ---------------------

    /// The `Scopes` events among `events`, as `(granted, missing)`.
    fn scopes_events(events: &[SyncEvent]) -> Vec<(Vec<String>, Vec<String>)> {
        events
            .iter()
            .filter_map(|e| match e {
                SyncEvent::Scopes { granted, missing } => Some((granted.clone(), missing.clone())),
                _ => None,
            })
            .collect()
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn token_route(body: &str) -> Route {
        Route {
            method: "POST".into(),
            path_prefix: "/organizations/oauth2/v2.0/token".into(),
            status: 200,
            headers: vec![],
            body: body.into(),
        }
    }

    fn empty_folders_route(status: u16) -> Route {
        Route {
            method: "GET".into(),
            path_prefix: "/me/mailFolders".into(),
            status,
            headers: vec![],
            body: if status == 200 {
                r#"{"value":[]}"#.into()
            } else {
                r#"{"error":{"code":"InvalidAuthenticationToken"}}"#.into()
            },
        }
    }

    const SEEDED: &[&str] = &[
        "https://graph.microsoft.com/Mail.ReadWrite",
        "offline_access",
    ];

    /// Starts an engine on `routes` with `token_path` already seeded, and
    /// returns the events up to the startup pass's `FoldersUpdated`.
    fn startup_events(
        tag: &str,
        routes: Vec<(Route, Option<usize>)>,
        cfg: AuthConfig,
        seed: impl FnOnce(&std::path::Path),
    ) -> (Vec<SyncEvent>, PathBuf, SyncHandle, FakeServer) {
        let srv = FakeServer::start_limited(routes);
        let base = srv.base_url.clone();
        let dir = unique_dir(tag);
        let token_path = dir.join("token.bin");
        seed(&token_path);
        let handle = spawn_with_bases(
            dir.join("mail.db"),
            token_path.clone(),
            cfg,
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );
        let events = wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        (events, dir, handle, srv)
    }

    fn finish(handle: SyncHandle, dir: PathBuf) {
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_announces_granted_scopes() {
        let (events, dir, handle, _srv) = startup_events(
            "scopes-startup",
            vec![(empty_folders_route(200), None)],
            test_cfg(),
            |p| seed_token_with_scopes(p, 3600, SEEDED),
        );
        assert_eq!(
            scopes_events(&events),
            vec![(strings(SEEDED), Vec::new())],
            "{events:?}"
        );
        finish(handle, dir);
    }

    #[test]
    fn startup_announces_missing_scopes() {
        let cfg = AuthConfig {
            scope: "Mail.ReadWrite Mail.Send offline_access".into(),
            ..test_cfg()
        };
        let (events, dir, handle, _srv) = startup_events(
            "scopes-missing",
            vec![(empty_folders_route(200), None)],
            cfg,
            |p| seed_token_with_scopes(p, 3600, SEEDED),
        );
        assert_eq!(
            scopes_events(&events),
            vec![(strings(SEEDED), strings(&["Mail.Send"]))],
            "{events:?}"
        );
        finish(handle, dir);
    }

    #[test]
    fn startup_without_known_scopes_announces_nothing() {
        let (events, dir, handle, _srv) = startup_events(
            "scopes-unknown",
            vec![(empty_folders_route(200), None)],
            test_cfg(),
            seed_token,
        );
        assert!(scopes_events(&events).is_empty(), "{events:?}");
        finish(handle, dir);
    }

    #[test]
    fn refresh_without_scope_keeps_previous_scopes() {
        // Near expiry at startup: refreshed before the first pass, by a
        // response that names no scopes, so the old grant still holds.
        let (events, dir, handle, _srv) = startup_events(
            "scopes-refresh-keep",
            vec![
                (
                    token_route(
                        r#"{"access_token":"AT2","refresh_token":"RT2","expires_in":3600}"#,
                    ),
                    None,
                ),
                (empty_folders_route(200), None),
            ],
            test_cfg(),
            |p| seed_token_with_scopes(p, 10, SEEDED),
        );
        // Loaded, then refreshed with the same grant: announced once.
        assert_eq!(
            scopes_events(&events),
            vec![(strings(SEEDED), Vec::new())],
            "{events:?}"
        );
        let cached = tokencache::load(&dir.join("token.bin")).unwrap().unwrap();
        assert_eq!(cached.access_token, "AT2");
        assert_eq!(cached.scopes, Some(strings(SEEDED)));
        finish(handle, dir);
    }

    #[test]
    fn refresh_on_401_with_same_grant_announces_nothing_new() {
        // The folder list 401s once; the refresh answers the same grant,
        // reordered and re-cased.
        let (events, dir, handle, _srv) = startup_events(
            "scopes-401-same",
            vec![
                (
                    token_route(
                        r#"{"access_token":"AT2","expires_in":3600,"scope":"OFFLINE_ACCESS mail.readwrite"}"#,
                    ),
                    None,
                ),
                (empty_folders_route(401), Some(1)),
                (empty_folders_route(200), None),
            ],
            test_cfg(),
            |p| seed_token_with_scopes(p, 3600, SEEDED),
        );
        assert_eq!(
            scopes_events(&events),
            vec![(strings(SEEDED), Vec::new())],
            "{events:?}"
        );
        let cached = tokencache::load(&dir.join("token.bin")).unwrap().unwrap();
        assert_eq!(
            cached.access_token, "AT2",
            "the 401 was retried after a refresh"
        );
        assert_eq!(
            cached.scopes,
            Some(strings(&["OFFLINE_ACCESS", "mail.readwrite"]))
        );
        finish(handle, dir);
    }

    #[test]
    fn refresh_on_401_with_wider_grant_announces_again() {
        let cfg = AuthConfig {
            scope: "Mail.ReadWrite Mail.Send People.Read offline_access".into(),
            ..test_cfg()
        };
        let wider = [
            "https://graph.microsoft.com/Mail.ReadWrite",
            "https://graph.microsoft.com/Mail.Send",
            "offline_access",
        ];
        let (events, dir, handle, _srv) = startup_events(
            "scopes-401-wider",
            vec![
                (
                    token_route(&format!(
                        r#"{{"access_token":"AT2","expires_in":3600,"scope":"{}"}}"#,
                        wider.join(" ")
                    )),
                    None,
                ),
                (empty_folders_route(401), Some(1)),
                (empty_folders_route(200), None),
            ],
            cfg,
            |p| seed_token_with_scopes(p, 3600, SEEDED),
        );
        assert_eq!(
            scopes_events(&events),
            vec![
                (strings(SEEDED), strings(&["Mail.Send", "People.Read"])),
                (strings(&wider), strings(&["People.Read"])),
            ],
            "{events:?}"
        );
        finish(handle, dir);
    }

    #[test]
    fn sign_in_announces_and_caches_granted_scopes() {
        let granted = "https://graph.microsoft.com/Mail.ReadWrite openid profile offline_access";
        let srv = FakeServer::start(vec![
            token_route(&format!(
                r#"{{"access_token":"AT","refresh_token":"RT","expires_in":3600,"scope":"{granted}"}}"#
            )),
            empty_folders_route(200),
        ]);
        let base = srv.base_url.clone();
        let dir = unique_dir("scopes-signin");
        let token_path = dir.join("token.bin");
        let handle = spawn_with_bases(
            dir.join("mail.db"),
            token_path.clone(),
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));
        sign_in_through_loopback(&handle);
        let events = wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        let expected: Vec<String> = granted.split(' ').map(str::to_string).collect();
        assert_eq!(
            scopes_events(&events),
            vec![(expected.clone(), Vec::new())],
            "{events:?}"
        );
        let cached = tokencache::load(&token_path).unwrap().unwrap();
        assert_eq!(cached.scopes, Some(expected));
        finish(handle, dir);
    }

    #[test]
    fn a_new_session_announces_the_same_grant_again() {
        // Sign out and back in with the very grant announced before: the
        // sign-in-required forgot it, so the new session announces it.
        let granted = SEEDED.join(" ");
        let (events, dir, handle, _srv) = startup_events(
            "scopes-new-session",
            vec![
                (
                    token_route(&format!(
                        r#"{{"access_token":"AT2","refresh_token":"RT2","expires_in":3600,"scope":"{granted}"}}"#
                    )),
                    None,
                ),
                (empty_folders_route(200), None),
            ],
            test_cfg(),
            |p| seed_token_with_scopes(p, 3600, SEEDED),
        );
        assert_eq!(scopes_events(&events).len(), 1, "{events:?}");
        handle.cmd_tx.send(SyncCommand::SignOut).unwrap();
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));
        sign_in_through_loopback(&handle);
        let events = wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        assert_eq!(
            scopes_events(&events),
            vec![(strings(SEEDED), Vec::new())],
            "{events:?}"
        );
        finish(handle, dir);
    }

    #[test]
    fn sign_in_refused_reports_error_description() {
        let srv = FakeServer::start(vec![token_route(
            r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#,
        )]);
        let base = srv.base_url.clone();
        let dir = unique_dir("signin-refused");
        let token_path = dir.join("token.bin");
        let handle = spawn_with_bases(
            dir.join("mail.db"),
            token_path.clone(),
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));
        let (_, response) = sign_in_through_loopback_with(&handle, |state| {
            format!(
                "error=consent_required&error_description=AADSTS65001%3A+The+user+or+administrator+has+not+consented%0D%0ATrace+ID%3A+abc&state={state}"
            )
        });
        assert!(response.to_lowercase().contains("failed"));
        let events = wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));
        assert!(
            events.contains(&SyncEvent::Error(
                "sign-in failed: authorization denied: consent_required: AADSTS65001: The user or administrator has not consented Trace ID: abc"
                    .into()
            )),
            "{events:?}"
        );
        assert!(tokencache::load(&token_path).unwrap().is_none());
        finish(handle, dir);
    }

    /// `handle_redirect` over a real loopback connection that sends
    /// `GET /?<query>`.
    fn redirect_result(query: &str, expected_state: &str) -> Result<String, String> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(format!("GET /?{query} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .unwrap();
        let (stream, _addr) = listener.accept().unwrap();
        let result = handle_redirect(stream, expected_state);
        drop(client);
        result
    }

    const REFUSED: &str = "error=consent_required&error_description=AADSTS65001%3A+The+user+or+administrator+has+not+consented%0D%0ATrace+ID%3A+abc%0D%0A";

    #[test]
    fn handle_redirect_reports_error_description() {
        assert_eq!(
            redirect_result(&format!("{REFUSED}&state=st"), "st"),
            Err(
                "authorization denied: consent_required: AADSTS65001: The user or administrator has not consented Trace ID: abc"
                    .to_string()
            )
        );
        // An empty (or blank) description keeps the bare error.
        assert_eq!(
            redirect_result(
                "error=access_denied&error_description=+%0D%0A&state=st",
                "st"
            ),
            Err("authorization denied: access_denied".to_string())
        );
        assert_eq!(
            redirect_result("error=access_denied&state=st", "st"),
            Err("authorization denied: access_denied".to_string())
        );
    }

    #[test]
    fn handle_redirect_hides_free_text_error_on_a_state_mismatch() {
        // An `error` that is not a bare code is free text too.
        let phishing = "error=Your+account+is+locked.+Call+%2B1-800-555-0100";
        assert_eq!(
            redirect_result(phishing, "st"),
            Err("authorization denied (redirect state did not match)".to_string())
        );
        assert_eq!(
            redirect_result(&format!("{phishing}&state=wrong"), "st"),
            Err("authorization denied (redirect state did not match)".to_string())
        );
        assert_eq!(
            redirect_result("error=consent_required%0D%0A&state=wrong", "st"),
            Err("authorization denied (redirect state did not match)".to_string())
        );
        // A bare code is shown as before.
        assert_eq!(
            redirect_result("error=consent_required&state=wrong", "st"),
            Err("authorization denied: consent_required".to_string())
        );
        // With our state, `error` is shown, on one line.
        assert_eq!(
            redirect_result("error=server+error%0D%0Aretry+later&state=st", "st"),
            Err("authorization denied: server error retry later".to_string())
        );
    }

    #[test]
    fn handle_redirect_drops_error_description_on_a_state_mismatch() {
        // Anything can send the browser here: free text only with our state.
        assert_eq!(
            redirect_result(&format!("{REFUSED}&state=wrong"), "st"),
            Err("authorization denied: consent_required".to_string())
        );
        assert_eq!(
            redirect_result(REFUSED, "st"),
            Err("authorization denied: consent_required".to_string())
        );
    }

    #[test]
    fn handle_redirect_errors_on_a_connection_closed_before_any_request_line() {
        // A client that connects and disappears without ever sending a
        // request line (EOF on the very first byte read) must be reported
        // as an error rather than panicking — `read_request_line` seeing
        // `Ok(0)` on its very first read is exactly this case.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        drop(client); // closes before writing anything

        let (stream, _addr) = listener.accept().unwrap();
        let result = handle_redirect(stream, "expected-state");
        assert!(result.is_err());
    }

    #[test]
    fn read_request_line_times_out_on_a_stalled_partial_line() {
        // A client that sends a partial request line (no trailing `\n`) and
        // then simply stops — without closing the connection — must not
        // hang past the overall deadline. A per-`read()` timeout alone
        // wouldn't catch this if it were reset to a fresh window on every
        // successful read; `read_request_line`'s `deadline` parameter must
        // still expire the read once the total budget runs out, regardless
        // of how many individual bytes trickled in before then.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.write_all(b"GET /?code=AB").unwrap(); // no newline; stays open

        let (stream, _addr) = listener.accept().unwrap();
        let start = Instant::now();
        let result = read_request_line(
            &stream,
            SIGNIN_REQUEST_LINE_MAX,
            Instant::now() + Duration::from_millis(200),
        );
        assert!(result.is_err(), "expected a timeout error, got {result:?}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "read_request_line did not respect its deadline: took {:?}",
            start.elapsed()
        );

        drop(client); // keep the connection alive until the assertions above run
    }

    #[test]
    fn read_request_line_errors_when_the_size_limit_is_exceeded() {
        // A connection that keeps sending bytes with no `\n` must be capped
        // by `max_bytes`, not just by the deadline — otherwise a fast,
        // never-terminating stream could grow the buffer unboundedly for as
        // long as the deadline allows.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.write_all(&[b'a'; 20]).unwrap(); // no newline

        let (stream, _addr) = listener.accept().unwrap();
        // A generous deadline, so the size cap (not the deadline) is what's
        // actually under test here.
        let result = read_request_line(&stream, 10, Instant::now() + Duration::from_secs(5));
        assert!(result.is_err());

        drop(client);
    }

    /// Sends `SignIn` and plays the browser: lands on the `redirect_uri` of
    /// `SignInStarted` with a code and its `state`. Returns that
    /// `redirect_uri` and the loopback listener's response.
    fn sign_in_through_loopback(handle: &SyncHandle) -> (String, String) {
        sign_in_through_loopback_with(handle, |state| format!("code=THECODE&state={state}"))
    }

    /// [`sign_in_through_loopback`] landing on `/?<query(state)>`.
    fn sign_in_through_loopback_with(
        handle: &SyncHandle,
        redirect_query: impl Fn(&str) -> String,
    ) -> (String, String) {
        handle.cmd_tx.send(SyncCommand::SignIn).unwrap();
        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::SignInStarted { .. })
        });
        let authorize_url = events
            .iter()
            .find_map(|e| match e {
                SyncEvent::SignInStarted { authorize_url } => Some(authorize_url.clone()),
                _ => None,
            })
            .expect("SignInStarted carries the authorize_url");
        let query = authorize_url.split_once('?').map(|(_, q)| q).unwrap_or("");
        let params = parse_query(query);
        let param = |name: &str| {
            params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("authorize_url carries {name}"))
        };
        let (redirect_uri, state) = (param("redirect_uri"), param("state"));
        let port: u16 = redirect_uri
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .expect("redirect_uri ends with the loopback port");
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect to loopback");
        let request = format!(
            "GET /?{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
            redirect_query(&state)
        );
        client.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        (redirect_uri, response)
    }

    #[test]
    fn sign_in_completes_via_loopback_redirect_and_caches_token() {
        // End-to-end proof of the port/redirect wiring: `begin_auth` must be
        // called with the SAME port `listen_for_code` accepts on, or this
        // "browser" (a raw TcpStream) would have nowhere to connect to.
        let routes = vec![
            Route {
                method: "POST".into(),
                path_prefix: "/organizations/oauth2/v2.0/token".into(),
                status: 200,
                headers: vec![],
                body: r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
        ];
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("signin-loopback");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin"); // no seed_token: starts signed out

        let handle = spawn_with_bases(
            store_path,
            token_path.clone(),
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );

        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));

        let (redirect_uri, response) = sign_in_through_loopback(&handle);
        // Entra matches the loopback redirect host literally and the Graph CLI
        // client registers `http://localhost` (not `http://127.0.0.1`), so the
        // redirect we send MUST use the `localhost` host or sign-in fails with
        // AADSTS50011.
        assert!(
            redirect_uri.starts_with("http://localhost:"),
            "redirect_uri must use the localhost host, got {redirect_uri}"
        );
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.to_lowercase().contains("close this tab"));

        // The redeem + cache-save + resync path then runs to completion.
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        let cached = tokencache::load(&token_path)
            .unwrap()
            .expect("token cached after sign-in");
        assert_eq!(cached.access_token, "AT");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sign_in_reenters_signin_required_when_the_redirect_state_mismatches() {
        // A redirect whose `state` doesn't match must be rejected rather
        // than redeemed — otherwise the loopback listener would accept a
        // code from anyone who can hit 127.0.0.1 during the sign-in window.
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/organizations/oauth2/v2.0/token".into(),
            status: 200,
            headers: vec![],
            body: r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#.into(),
        }]);
        let base = srv.base_url.clone();

        let dir = unique_dir("signin-state-mismatch");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");

        let handle = spawn_with_bases(
            store_path,
            token_path.clone(),
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );

        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));
        handle.cmd_tx.send(SyncCommand::SignIn).unwrap();

        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::SignInStarted { .. })
        });
        let authorize_url = events
            .iter()
            .find_map(|e| match e {
                SyncEvent::SignInStarted { authorize_url } => Some(authorize_url.clone()),
                _ => None,
            })
            .unwrap();
        let query = authorize_url.split_once('?').map(|(_, q)| q).unwrap_or("");
        let redirect_uri = parse_query(query)
            .into_iter()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v)
            .unwrap();
        let port: u16 = redirect_uri.rsplit(':').next().unwrap().parse().unwrap();

        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(b"GET /?code=THECODE&state=wrong-state HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.to_lowercase().contains("failed"));

        // No token was ever cached, and the engine falls back to
        // sign-in-required rather than getting stuck.
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));
        assert!(tokencache::load(&token_path).unwrap().is_none());

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calendar_window_defaults_to_7_days_back_and_30_days_forward() {
        // 2026-07-18T00:00:00Z, a fixed base so this doesn't depend on wall
        // clock — the whole point of `calendar_window` taking `base` as a
        // parameter rather than reading `SystemTime::now()` itself.
        let base = UNIX_EPOCH + Duration::from_secs(1_784_332_800);
        let (from, to) = calendar_window(base);
        assert_eq!(from, "2026-07-11T00:00:00Z");
        assert_eq!(to, "2026-08-17T00:00:00Z");
    }

    /// A Graph calendar-event JSON body shaped like `Event::from_json`
    /// expects — same shape `graph::client`'s own `sample_event_json` test
    /// fixture uses, duplicated here since that one is private to that module.
    fn calendar_event_json(id: &str, start: &str, end: &str) -> String {
        format!(
            r#"{{"id":"{id}","subject":"Standup",
            "start":{{"dateTime":"{start}.0000000","timeZone":"UTC"}},
            "end":{{"dateTime":"{end}.0000000","timeZone":"UTC"}},
            "isAllDay":false,
            "location":{{"displayName":"Room"}},
            "organizer":{{"emailAddress":{{"name":"Org","address":"org@x"}}}},
            "responseStatus":{{"response":"none"}},
            "seriesMasterId":null,
            "bodyPreview":"p","webLink":"https://x/{id}",
            "lastModifiedDateTime":"2026-07-17T00:00:00Z",
            "body":{{"contentType":"html","content":"agenda"}},
            "attendees":[{{"type":"required","status":{{"response":"none"}},"emailAddress":{{"name":"Bob","address":"bob@x"}}}}]
            }}"#
        )
    }

    #[test]
    fn refresh_calendar_populates_store_and_emits_event() {
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendarView".into(),
                status: 200,
                headers: vec![],
                body: format!(
                    r#"{{"value":[{}]}}"#,
                    calendar_event_json("E1", "2026-07-18T09:00:00", "2026-07-18T09:30:00")
                ),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("refresh-calendar");
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

        // Let the mail backfill land first (not strictly required — this
        // engine only fetches the calendar on an explicit `RefreshCalendar`
        // or a periodic tick, never as part of a mail `Refresh` — but it
        // keeps the fixture realistic and gives us a well-defined signal to
        // wait on first).
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));

        handle.cmd_tx.send(SyncCommand::RefreshCalendar).unwrap();
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::CalendarUpdated));

        let store = Store::open(&store_path).unwrap();
        let rows = store
            .events_in_window("2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "E1");
        assert_eq!(rows[0].subject, "Standup");
        assert_eq!(rows[0].start_utc, "2026-07-18T09:00:00Z");

        let attendees = store.event_attendees("E1").unwrap();
        assert_eq!(attendees.len(), 1);
        assert_eq!(attendees[0].name, "Bob");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1937: `refresh_calendar` stores Graph's `isReminderOn` /
    /// `reminderMinutesBeforeStart`; before the fix
    /// `Store::replace_events_in_window` wrote neither column, so every
    /// synced event read back as 0 minutes, reminder off.
    #[test]
    fn refresh_calendar_keeps_reminder_settings() {
        let plain = calendar_event_json("E1", "2026-07-18T09:00:00", "2026-07-18T09:30:00");
        let event = plain.replacen(
            r#""isAllDay":false"#,
            r#""isAllDay":false,"isReminderOn":true,"reminderMinutesBeforeStart":15"#,
            1,
        );
        assert_ne!(event, plain, "the reminder keys were not added");
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendarView".into(),
                status: 200,
                headers: vec![],
                body: format!(r#"{{"value":[{event}]}}"#),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("refresh-calendar-reminders");
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
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));

        handle.cmd_tx.send(SyncCommand::RefreshCalendar).unwrap();
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::CalendarUpdated));

        let store = Store::open(&store_path).unwrap();
        let rows = store
            .events_in_window("2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "E1");
        assert_eq!(rows[0].reminder_minutes, 15);
        assert!(rows[0].is_reminder_on);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn respond_event_optimistically_flips_status_and_drains_to_accept() {
        let mut routes = backfill_routes();
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
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("respond-event");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);

        // Seed the event locally BEFORE the engine (and its own `Store::open`)
        // starts up — same pattern `folder_sync_adopts_sentinel_drafts_...`
        // uses for a local draft — so `RespondEvent`'s optimistic write has a
        // row to flip.
        {
            let store = Store::open(&store_path).unwrap();
            store
                .upsert_event(&NewEvent {
                    id: "E1".into(),
                    subject: "Standup".into(),
                    start_utc: "2026-07-18T09:00:00Z".into(),
                    end_utc: "2026-07-18T09:30:00Z".into(),
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
                })
                .unwrap();
        }

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
            .send(SyncCommand::RespondEvent {
                id: "E1".into(),
                kind: "accepted".into(),
                comment: Some("see you there".into()),
                proposed_start_utc: None,
                proposed_end_utc: None,
            })
            .unwrap();

        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::CalendarUpdated));

        // The optimistic local write lands before the drain even starts.
        let store = Store::open(&store_path).unwrap();
        let rows = store
            .events_in_window("2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(rows[0].response_status, "accepted");

        // The drain reaches Graph too.
        if !wait_until(ENGINE_WAIT, || {
            srv.requests()
                .iter()
                .any(|r| r.method == "POST" && r.path.starts_with("/me/events/E1/accept"))
        }) {
            panic!("no accept POST observed; requests: {:?}", srv.requests());
        }
        let accept = srv
            .requests()
            .into_iter()
            .find(|r| r.path.starts_with("/me/events/E1/accept"))
            .unwrap();
        assert_eq!(
            accept.body,
            r#"{"comment":"see you there","sendResponse":true}"#
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn respond_event_quarantines_after_repeated_4xx() {
        // Same drain policy as mail ops — `drain_outbox` doesn't distinguish
        // op kind — so this mirrors `quarantine_drops_op_and_clears_delta_
        // links_for_reconverge` above, just against a `RespondEvent` op whose
        // Graph call always 404s.
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "POST".into(),
                path_prefix: "/me/events/E1/accept".into(),
                status: 404,
                headers: vec![],
                body: "{}".into(),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("respond-event-quarantine");
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

        // The RespondEvent command drains once (attempt 1); each Refresh
        // drains again (attempts 2..5). The 4th Refresh is the 5th failed
        // drain and quarantines.
        handle
            .cmd_tx
            .send(SyncCommand::RespondEvent {
                id: "E1".into(),
                kind: "accepted".into(),
                comment: None,
                proposed_start_utc: None,
                proposed_end_utc: None,
            })
            .unwrap();
        for _ in 0..4 {
            handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
        }

        let events = wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::Error(m) if m.contains("quarantined")),
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SyncEvent::Error(m) if m.contains("quarantined"))),
            "expected a quarantine Error event, saw: {events:?}"
        );
        assert!(
            Store::open(&store_path)
                .unwrap()
                .pending_ops()
                .unwrap()
                .is_empty()
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_calendar_prunes_events_no_longer_in_the_fetched_window() {
        // A cancelled (or moved-out-of-window) event: plain upserting would
        // never remove it, since it's simply never returned by a later
        // `calendar_view` fetch. `replace_events_in_window` (Store) is what
        // prunes it — this proves `refresh_calendar` actually uses that
        // replace path, not a plain upsert loop.
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendarView".into(),
                status: 200,
                headers: vec![],
                // A DIFFERENT set that does NOT include the previously-
                // stored OLD1 event below.
                body: format!(
                    r#"{{"value":[{}]}}"#,
                    calendar_event_json("NEW1", "2026-07-18T09:00:00", "2026-07-18T09:30:00")
                ),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("refresh-calendar-prune");
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);

        // Seed an "OLD" event dated to fall well inside whatever window
        // `calendar_window(SystemTime::now())` computes at test-run time
        // (a day from now) — computed off the real clock, not hardcoded,
        // so this test doesn't depend on which date it happens to run on —
        // so `replace_events_in_window`'s window-bounded DELETE actually
        // captures it, proving the prune rather than an unrelated gap.
        {
            let store = Store::open(&store_path).unwrap();
            let soon = now_unix() + 86400;
            store
                .upsert_event(&NewEvent {
                    id: "OLD1".into(),
                    subject: "Cancelled".into(),
                    start_utc: unix_to_iso8601(soon),
                    end_utc: unix_to_iso8601(soon + 1800),
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
                })
                .unwrap();
        }

        let handle = spawn_with_bases(
            store_path.clone(),
            token_path,
            test_cfg(),
            3650,
            base.clone(),
            base,
            Duration::from_secs(3600),
        );

        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));

        handle.cmd_tx.send(SyncCommand::RefreshCalendar).unwrap();
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::CalendarUpdated));

        let store = Store::open(&store_path).unwrap();
        let rows = store
            .events_in_window("2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z")
            .unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"NEW1"), "expected NEW1, got: {ids:?}");
        assert!(
            !ids.contains(&"OLD1"),
            "the cancelled/replaced event should have been pruned, got: {ids:?}"
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn respond_event_quarantine_reconverges_local_status_from_server_truth() {
        // The optimistic `RespondEvent` write sets `response_status` to
        // "accepted" locally, but the Graph call always 404s, so it never
        // actually reaches the server. Once quarantined (repeated 4xx), the
        // engine must refresh the calendar so the local status reverts to
        // whatever the server actually has ("none" here) — otherwise a
        // phantom RSVP stays on the books forever with nothing to correct
        // it (unlike mail, which reconverges via delta links).
        let mut routes = backfill_routes();
        routes.insert(
            0,
            Route {
                method: "POST".into(),
                path_prefix: "/me/events/E1/accept".into(),
                status: 404,
                headers: vec![],
                body: "{}".into(),
            },
        );
        routes.insert(
            0,
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendarView".into(),
                status: 200,
                headers: vec![],
                // Server truth: E1 is still unanswered ("none"), never
                // "accepted" — the optimistic local write never landed.
                body: format!(
                    r#"{{"value":[{}]}}"#,
                    calendar_event_json("E1", "2026-07-18T09:00:00", "2026-07-18T09:30:00")
                ),
            },
        );
        let srv = FakeServer::start(routes);
        let base = srv.base_url.clone();

        let dir = unique_dir("respond-event-reconverge");
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
            .send(SyncCommand::RespondEvent {
                id: "E1".into(),
                kind: "accepted".into(),
                comment: None,
                proposed_start_utc: None,
                proposed_end_utc: None,
            })
            .unwrap();
        for _ in 0..4 {
            handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
        }

        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::Error(m) if m.contains("quarantined")),
        );
        // The quarantine branch triggers a calendar refresh immediately
        // (see `Engine::drain_outbox`), so this lands right after.
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::CalendarUpdated));

        let store = Store::open(&store_path).unwrap();
        let rows = store
            .events_in_window("2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].response_status, "none",
            "local status should have reconverged to server truth, not stayed \"accepted\""
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wait_for_tolerates_an_engine_slower_than_five_seconds() {
        let (tx, rx) = std::sync::mpsc::channel::<SyncEvent>();
        let slow = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(6));
            let _ = tx.send(SyncEvent::FoldersUpdated);
        });
        let seen = wait_for(&rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        assert_eq!(seen.len(), 1);
        slow.join().unwrap();
    }

    #[test]
    fn wait_until_sees_a_side_effect_that_lands_after_five_seconds() {
        let dir = unique_dir("slow-side-effect");
        let target = dir.join("done");
        let slow = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(6));
            std::fs::write(target, b"done").unwrap();
        });
        assert!(wait_until(ENGINE_WAIT, || dir.join("done").exists()));
        slow.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- A draft stays addressable by its local: id (#1884, #1881) --------

    const DRAFT_G: &str = "GRAPH-77";

    /// The routes a draft's create, patch and send need (most specific
    /// first: the fake server answers with the first route whose method and
    /// prefix match and that has uses left), then the Sent Items routes the
    /// startup pass syncs. With `send_fails_once`, the first send answers 503,
    /// and the delivery check then made says it is still a draft.
    fn draft_routes(send_fails_once: bool) -> Vec<(Route, Option<usize>)> {
        let route = |method: &str, path: String, status: u16, body: &str| Route {
            method: method.into(),
            path_prefix: path,
            status,
            headers: vec![],
            body: body.into(),
        };
        let mut routes = Vec::new();
        if send_fails_once {
            routes.push((
                route("POST", format!("/me/messages/{DRAFT_G}/send"), 503, "busy"),
                Some(1),
            ));
        }
        routes.push((
            route("POST", format!("/me/messages/{DRAFT_G}/send"), 202, ""),
            None,
        ));
        routes.push((
            route("PATCH", format!("/me/messages/{DRAFT_G}"), 200, "{}"),
            None,
        ));
        // The delivery check a send whose `/send` may have gone out makes
        // first (#1874): Graph still holds it as a draft.
        routes.push((
            route(
                "GET",
                format!("/me/messages/{DRAFT_G}?$select=isDraft"),
                200,
                &format!(r#"{{"id":"{DRAFT_G}","isDraft":true}}"#),
            ),
            None,
        ));
        let created = format!(
            r#"{{"id":"{DRAFT_G}","conversationId":"C","subject":"Hi","from":{{"emailAddress":{{"name":"","address":""}}}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"","isDraft":true}}"#
        );
        routes.push((route("POST", "/me/messages".into(), 201, &created), None));
        routes.extend(sent_folder_routes().into_iter().map(|r| (r, None)));
        routes
    }

    /// How many recorded requests had exactly this method and path.
    fn hits(srv: &FakeServer, method: &str, path: &str) -> usize {
        srv.requests()
            .iter()
            .filter(|r| r.method == method && r.path == path)
            .count()
    }

    fn send_path() -> String {
        format!("/me/messages/{DRAFT_G}/send")
    }

    fn patch_path() -> String {
        format!("/me/messages/{DRAFT_G}")
    }

    /// A temp store holding one local draft (seeded before the engine
    /// starts, as `send_draft_of_a_local_draft_…` does), plus `ops` queued
    /// for it. Returns `(dir, store path, token path, local id)`.
    fn seeded_draft(
        tag: &str,
        ops: impl Fn(&str) -> Vec<OutboxOp>,
    ) -> (PathBuf, PathBuf, PathBuf, String) {
        let dir = unique_dir(tag);
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);
        let store = Store::open(&store_path).unwrap();
        let local = store
            .create_local_draft("Hi", "bob@x", "", "<p>first</p>")
            .unwrap();
        for op in ops(&local) {
            store.enqueue_op(&op).unwrap();
        }
        (dir, store_path, token_path, local)
    }

    fn spawn_on(srv: &FakeServer, store_path: &std::path::Path, token_path: PathBuf) -> SyncHandle {
        spawn_with_bases(
            store_path.to_path_buf(),
            token_path,
            test_cfg(),
            3650,
            srv.base_url.clone(),
            srv.base_url.clone(),
            Duration::from_secs(3600),
        )
    }

    /// Waits until the engine has settled at `Idle` (after a startup pass or
    /// a command's drain), collecting every event on the way.
    fn wait_idle(handle: &SyncHandle, seen: &mut Vec<SyncEvent>) {
        seen.extend(wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::State(SyncState::Idle))
        }));
    }

    /// Edits the draft by its local id through a second connection, as the
    /// UI does with its own `Store`.
    fn edit_draft_as_the_ui(store_path: &std::path::Path, local: &str) {
        let ui = Store::open(store_path).unwrap();
        ui.update_draft_fields(local, "Edited", "bob@x", "", "", "<p>second</p>")
            .unwrap();
    }

    /// Asserts the one PATCH carried the edits.
    fn assert_patch_carries_the_edits(srv: &FakeServer) {
        let patch = srv
            .requests()
            .into_iter()
            .find(|r| r.method == "PATCH" && r.path == patch_path())
            .expect("a PATCH of the Graph draft");
        let sent = crate::json::parse(&patch.body).unwrap();
        assert_eq!(
            sent.get("subject").and_then(crate::json::Value::as_str),
            Some("Edited")
        );
        assert_eq!(
            sent.get("body")
                .and_then(|b| b.get("content"))
                .and_then(crate::json::Value::as_str),
            Some("<p>second</p>")
        );
    }

    fn no_quarantine(seen: &[SyncEvent]) {
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, SyncEvent::Error(m) if m.contains("quarantined"))),
            "{seen:?}"
        );
    }

    #[test]
    fn save_edit_send_by_the_local_id_patches_and_sends_the_graph_draft() {
        let srv = FakeServer::start_limited(draft_routes(false));
        let (dir, store_path, token_path, local) = seeded_draft("draft-save-send", |_| vec![]);
        let handle = spawn_on(&srv, &store_path, token_path);
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        handle
            .cmd_tx
            .send(SyncCommand::SaveDraft { id: local.clone() })
            .unwrap();
        wait_idle(&handle, &mut seen);
        assert_eq!(hits(&srv, "POST", "/me/messages"), 1);

        // The compose window stayed open: the UI still holds the local id.
        edit_draft_as_the_ui(&store_path, &local);
        handle
            .cmd_tx
            .send(SyncCommand::SendDraft { id: local.clone() })
            .unwrap();
        seen.extend(wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::Sent { .. })
        }));

        assert!(
            seen.iter()
                .any(|e| matches!(e, SyncEvent::Sent { id } if id == &local))
        );
        assert_eq!(hits(&srv, "POST", "/me/messages"), 1);
        assert_eq!(hits(&srv, "PATCH", &patch_path()), 1);
        assert_eq!(hits(&srv, "POST", &send_path()), 1);
        assert_patch_carries_the_edits(&srv);
        let store = Store::open(&store_path).unwrap();
        let (row, body) = store.draft(DRAFT_G).unwrap().unwrap();
        assert_eq!(row.subject, "Edited");
        assert_eq!(body.content, "<p>second</p>");
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_edit_save_by_the_local_id_patches_and_leaves_nothing_queued() {
        let srv = FakeServer::start_limited(draft_routes(false));
        let (dir, store_path, token_path, local) = seeded_draft("draft-save-save", |_| vec![]);
        let handle = spawn_on(&srv, &store_path, token_path);
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        handle
            .cmd_tx
            .send(SyncCommand::SaveDraft { id: local.clone() })
            .unwrap();
        wait_idle(&handle, &mut seen);
        assert_eq!(hits(&srv, "POST", "/me/messages"), 1);

        edit_draft_as_the_ui(&store_path, &local);
        handle
            .cmd_tx
            .send(SyncCommand::SaveDraft { id: local.clone() })
            .unwrap();
        wait_idle(&handle, &mut seen);

        assert_eq!(hits(&srv, "POST", "/me/messages"), 1);
        assert_eq!(hits(&srv, "PATCH", &patch_path()), 1);
        assert_patch_carries_the_edits(&srv);
        let store = Store::open(&store_path).unwrap();
        assert!(store.pending_ops().unwrap().is_empty());
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_queued_saves_of_a_local_draft_create_once_then_patch() {
        // Offline: both SaveDrafts were queued before the engine got online.
        let srv = FakeServer::start_limited(draft_routes(false));
        let (dir, store_path, token_path, _local) =
            seeded_draft("draft-two-saves", |local: &str| {
                vec![
                    OutboxOp::SaveDraft { id: local.into() },
                    OutboxOp::SaveDraft { id: local.into() },
                ]
            });
        let handle = spawn_on(&srv, &store_path, token_path);
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        assert_eq!(hits(&srv, "POST", "/me/messages"), 1);
        assert_eq!(hits(&srv, "PATCH", &patch_path()), 1);
        let store = Store::open(&store_path).unwrap();
        assert!(store.pending_ops().unwrap().is_empty());
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_send_that_failed_after_the_create_is_retried_without_a_second_create() {
        // #1881: the first send answers 503 after the draft was created.
        let srv = FakeServer::start_limited(draft_routes(true));
        let (dir, store_path, token_path, local) = seeded_draft("draft-send-503", |_| vec![]);
        let handle = spawn_on(&srv, &store_path, token_path);
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        handle
            .cmd_tx
            .send(SyncCommand::SendDraft { id: local.clone() })
            .unwrap();
        // A 503 is `Throttled`: the drain stops with the op queued and its
        // attempts untouched, and retries only after Retry-After (30 s
        // without the header), emitting no event. Commands run in order, so a
        // FetchBody of an unknown id is the barrier: its 404 error comes only
        // once the SendDraft has been handled.
        handle
            .cmd_tx
            .send(SyncCommand::FetchBody {
                id: "barrier".into(),
            })
            .unwrap();
        seen.extend(wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::Error(_))
        }));
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.method == "GET" && r.path.starts_with("/me/messages/barrier"))
        );
        assert_eq!(hits(&srv, "POST", &send_path()), 1);
        assert!(!seen.iter().any(|e| matches!(e, SyncEvent::Sent { .. })));
        let queued = Store::open(&store_path).unwrap().pending_ops().unwrap();
        assert_eq!(queued.len(), 1);
        assert!(matches!(&queued[0].op, OutboxOp::SendDraft { id } if id == &local));
        assert_eq!(queued[0].attempts, 0);

        handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
        seen.extend(wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::Sent { .. })
        }));
        wait_idle(&handle, &mut seen);

        assert_eq!(
            seen.iter()
                .filter(|e| matches!(e, SyncEvent::Sent { id } if id == &local))
                .count(),
            1
        );
        assert_eq!(hits(&srv, "POST", "/me/messages"), 1);
        assert_eq!(hits(&srv, "PATCH", &patch_path()), 1);
        assert_eq!(hits(&srv, "POST", &send_path()), 2);
        // A 503 may follow a send the back end accepted (#1874): the retry
        // asked first, and Graph still had the draft.
        assert_eq!(
            hits(
                &srv,
                "GET",
                &format!("/me/messages/{DRAFT_G}?$select=isDraft")
            ),
            1
        );
        let store = Store::open(&store_path).unwrap();
        assert!(store.pending_ops().unwrap().is_empty());
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_queued_save_then_send_of_a_local_draft_sends_it() {
        // #1881's second case: the SaveDraft reconciles first, the SendDraft
        // behind it must still find the draft.
        let srv = FakeServer::start_limited(draft_routes(false));
        let (dir, store_path, token_path, local) =
            seeded_draft("draft-save-then-send", |local: &str| {
                vec![
                    OutboxOp::SaveDraft { id: local.into() },
                    OutboxOp::SendDraft { id: local.into() },
                ]
            });
        let handle = spawn_on(&srv, &store_path, token_path);
        let mut seen = wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::Sent { .. }));
        wait_idle(&handle, &mut seen);

        assert!(
            seen.iter()
                .any(|e| matches!(e, SyncEvent::Sent { id } if id == &local))
        );
        assert_eq!(hits(&srv, "POST", "/me/messages"), 1);
        assert_eq!(hits(&srv, "PATCH", &patch_path()), 1);
        assert_eq!(hits(&srv, "POST", &send_path()), 1);
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_by_a_reconciled_local_id_deletes_the_graph_draft() {
        let mut routes = draft_routes(false);
        routes.insert(
            0,
            (
                Route {
                    method: "DELETE".into(),
                    path_prefix: "/me/messages/".into(),
                    status: 204,
                    headers: vec![],
                    body: String::new(),
                },
                None,
            ),
        );
        let srv = FakeServer::start_limited(routes);
        let (dir, store_path, token_path, local) = seeded_draft("draft-delete", |local: &str| {
            vec![OutboxOp::SaveDraft { id: local.into() }]
        });
        let handle = spawn_on(&srv, &store_path, token_path);
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);
        assert_eq!(hits(&srv, "POST", "/me/messages"), 1);

        handle
            .cmd_tx
            .send(SyncCommand::Delete { id: local.clone() })
            .unwrap();
        wait_idle(&handle, &mut seen);

        assert_eq!(hits(&srv, "DELETE", &patch_path()), 1);
        let deletes: Vec<String> = srv
            .requests()
            .into_iter()
            .filter(|r| r.method == "DELETE")
            .map(|r| r.path)
            .collect();
        assert_eq!(deletes, vec![patch_path()]);
        let store = Store::open(&store_path).unwrap();
        assert!(store.draft(DRAFT_G).unwrap().is_none());
        assert!(store.pending_ops().unwrap().is_empty());
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Sync one folder, the fast cycle, Sent Items after a send (#1885) ---

    /// A delta page holding `items` and ending in `link`.
    fn delta_page(items: &[String], link: &str) -> String {
        format!(
            r#"{{"value":[{}],"@odata.deltaLink":"{link}"}}"#,
            items.join(",")
        )
    }

    /// A folder as `/me/mailFolders` lists it.
    fn folder_json(id: &str, name: &str, well_known: Option<&str>) -> String {
        let wk = well_known.map_or("null".to_string(), |w| format!("\"{w}\""));
        format!(
            r#"{{"id":"{id}","displayName":"{name}","parentFolderId":null,"totalItemCount":0,"unreadItemCount":0,"wellKnownName":{wk}}}"#
        )
    }

    /// Routes for the folders `(id, name, well_known)` listed by
    /// `/me/mailFolders`, each with no child folders, an empty first delta
    /// (so each holds a delta link after startup) and counts. `next` answers
    /// a folder's incremental deltas in order, each once, before the empty
    /// page every later one gets; `counts` overrides a folder's count body.
    fn folder_routes(
        folders: &[(&str, &str, Option<&str>)],
        next: &[(&str, &str)],
        counts: &[(&str, &str)],
    ) -> Vec<(Route, Option<usize>)> {
        let mut routes = Vec::new();
        for (id, _, _) in folders {
            let delta = format!("/me/mailFolders/{id}/messages/delta");
            let link = format!("{delta}?token=D-{id}");
            routes.push((
                get_route(
                    &format!("/me/mailFolders/{id}/childFolders"),
                    200,
                    r#"{"value":[]}"#,
                ),
                None,
            ));
            for (fid, body) in next {
                if fid == id {
                    routes.push((get_route(&link, 200, body), Some(1)));
                }
            }
            routes.push((get_route(&delta, 200, &delta_page(&[], &link)), None));
            let count = counts
                .iter()
                .find(|(fid, _)| fid == id)
                .map_or(r#"{"totalItemCount":0,"unreadItemCount":0}"#, |(_, c)| c);
            routes.push((
                get_route(&format!("/me/mailFolders/{id}?"), 200, count),
                None,
            ));
        }
        let listed: Vec<String> = folders
            .iter()
            .map(|(id, name, wk)| folder_json(id, name, *wk))
            .collect();
        routes.push((
            get_route(
                "/me/mailFolders?",
                200,
                &format!(r#"{{"value":[{}]}}"#, listed.join(",")),
            ),
            None,
        ));
        routes
    }

    const F1_F2: &[(&str, &str, Option<&str>)] =
        &[("F1", "Inbox", Some("inbox")), ("F2", "Projects", None)];

    /// Spawns an engine on `srv` with a seeded token, the given normal tick
    /// and fast tick; `seed` runs on the store before the engine opens it.
    fn spawn_fast(
        srv: &FakeServer,
        tag: &str,
        tick: Duration,
        fast_tick: Option<Duration>,
        seed: impl FnOnce(&Store),
    ) -> (SyncHandle, PathBuf, PathBuf) {
        let dir = unique_dir(tag);
        let store_path = dir.join("mail.db");
        let token_path = dir.join("token.bin");
        seed_token(&token_path);
        seed(&Store::open(&store_path).unwrap());
        let handle = spawn_with_bases_and_options(
            store_path.clone(),
            token_path,
            test_cfg(),
            3650,
            srv.base_url.clone(),
            srv.base_url.clone(),
            tick,
            EngineOptions {
                fast_tick,
                ..EngineOptions::without_prefetch()
            },
        );
        (handle, store_path, dir)
    }

    /// The paths of the requests from index `from` on.
    fn paths_from(srv: &FakeServer, from: usize) -> Vec<String> {
        srv.requests()
            .into_iter()
            .skip(from)
            .map(|r| r.path)
            .collect()
    }

    fn is_delta(path: &str, folder: &str) -> bool {
        path.starts_with(&format!("/me/mailFolders/{folder}/messages/delta"))
    }

    fn is_counts(path: &str, folder: &str) -> bool {
        path.starts_with(&format!("/me/mailFolders/{folder}?"))
    }

    fn is_folder_list(path: &str) -> bool {
        path.starts_with("/me/mailFolders?")
    }

    fn is_calendar(path: &str) -> bool {
        path.starts_with("/me/calendarView")
    }

    fn count(srv: &FakeServer, pred: impl Fn(&str) -> bool) -> usize {
        paths_from(srv, 0).iter().filter(|p| pred(p)).count()
    }

    const HOUR: Duration = Duration::from_secs(3600);
    const FAST: Duration = Duration::from_millis(200);

    #[test]
    fn fast_tick_syncs_only_the_fast_folders() {
        let new = delta_page(
            &[full_message_json("NEW1", "New", "2026-07-16T10:00:00Z")],
            "/me/mailFolders/F1/messages/delta?token=D-F1",
        );
        let srv = FakeServer::start_limited(folder_routes(F1_F2, &[("F1", &new)], &[]));
        let (handle, store_path, dir) = spawn_fast(&srv, "fast-only", HOUR, Some(FAST), |_| {});
        handle
            .cmd_tx
            .send(SyncCommand::SetFastFolders {
                ids: vec!["F1".into()],
            })
            .unwrap();
        // The startup pass reports F1 once; the fast tick that fetches NEW1
        // reports it again.
        wait_for_updates(&handle.evt_rx, "F1", 2);
        let store = Store::open(&store_path).unwrap();
        assert!(
            wait_until(ENGINE_WAIT, || store
                .messages_in_folder("F1", 50, 0)
                .unwrap()
                .iter()
                .any(|m| m.id == "NEW1")),
            "NEW1 reached the store"
        );
        // Several more fast intervals: still only F1.
        std::thread::sleep(Duration::from_millis(1000));
        let f1_deltas = count(&srv, |p| is_delta(p, "F1"));
        assert!(f1_deltas >= 3, "startup + fast deltas of F1: {f1_deltas}");
        assert!(count(&srv, |p| is_counts(p, "F1")) >= 1);
        assert_eq!(
            count(&srv, |p| is_delta(p, "F2")),
            1,
            "F2: the startup delta only"
        );
        assert_eq!(count(&srv, |p| is_counts(p, "F2")), 0);
        assert_eq!(count(&srv, is_folder_list), 1, "the startup list only");
        assert_eq!(count(&srv, is_calendar), 0, "no calendar on a fast tick");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_folder_fetches_one_folder_and_its_counts() {
        let new = delta_page(
            &[full_message_json("NEW1", "New", "2026-07-16T10:00:00Z")],
            "/me/mailFolders/F1/messages/delta?token=D-F1",
        );
        let srv = FakeServer::start_limited(folder_routes(
            F1_F2,
            &[("F1", &new)],
            &[(
                "F1",
                r#"{"id":"F1","totalItemCount":2,"unreadItemCount":1}"#,
            )],
        ));
        let (handle, store_path, dir) = spawn_fast(&srv, "sync-folder", HOUR, None, |_| {});
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        let mark = srv.requests().len();
        handle
            .cmd_tx
            .send(SyncCommand::SyncFolder { id: "F1".into() })
            .unwrap();
        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::State(SyncState::Idle))
        });
        assert!(events.contains(&SyncEvent::MessagesUpdated {
            folder_id: "F1".into()
        }));
        assert!(events.contains(&SyncEvent::FoldersUpdated), "{events:?}");
        let paths = paths_from(&srv, mark);
        assert!(paths.iter().any(|p| is_delta(p, "F1")), "{paths:?}");
        assert!(paths.iter().any(|p| is_counts(p, "F1")), "{paths:?}");
        assert!(
            paths
                .iter()
                .all(|p| is_delta(p, "F1") || is_counts(p, "F1")),
            "only F1's delta and counts: {paths:?}"
        );
        let store = Store::open(&store_path).unwrap();
        assert!(
            store
                .messages_in_folder("F1", 50, 0)
                .unwrap()
                .iter()
                .any(|m| m.id == "NEW1")
        );
        let f1 = store
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.id == "F1")
            .unwrap();
        assert_eq!((f1.total_count, f1.unread_count), (2, 1));
        assert_eq!(f1.display_name, "Inbox", "the counts GET leaves the name");

        // Same counts again: no FoldersUpdated.
        let mark = srv.requests().len();
        handle
            .cmd_tx
            .send(SyncCommand::SyncFolder { id: "F1".into() })
            .unwrap();
        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::State(SyncState::Idle))
        });
        assert!(!events.contains(&SyncEvent::FoldersUpdated), "{events:?}");
        let paths = paths_from(&srv, mark);
        assert!(
            !paths.is_empty()
                && paths
                    .iter()
                    .all(|p| is_delta(p, "F1") || is_counts(p, "F1")),
            "{paths:?}"
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_folder_unknown_id_is_an_error() {
        let srv = FakeServer::start_limited(folder_routes(F1_F2, &[], &[]));
        let (handle, _store_path, dir) = spawn_fast(&srv, "sync-unknown", HOUR, None, |_| {});
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        let mark = srv.requests().len();
        handle
            .cmd_tx
            .send(SyncCommand::SyncFolder { id: "NOPE".into() })
            .unwrap();
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::Error(m) if m.contains("unknown folder NOPE")),
        );
        // A known folder's sync after it, as a barrier: everything NOPE could
        // have caused has reached the server by the time this one settles.
        handle
            .cmd_tx
            .send(SyncCommand::SyncFolder { id: "F1".into() })
            .unwrap();
        wait_idle(&handle, &mut seen);
        let paths = paths_from(&srv, mark);
        assert!(
            paths
                .iter()
                .all(|p| is_delta(p, "F1") || is_counts(p, "F1")),
            "NOPE made no Graph request: {paths:?}"
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_fast_folders_replaces_the_set() {
        let srv = FakeServer::start_limited(folder_routes(F1_F2, &[], &[]));
        let (handle, _store_path, dir) = spawn_fast(&srv, "fast-replace", HOUR, Some(FAST), |_| {});
        handle
            .cmd_tx
            .send(SyncCommand::SetFastFolders {
                ids: vec!["F1".into()],
            })
            .unwrap();
        assert!(wait_until(ENGINE_WAIT, || count(&srv, |p| is_delta(
            p, "F1"
        )) >= 2));

        handle
            .cmd_tx
            .send(SyncCommand::SetFastFolders {
                ids: vec!["F2".into(), "F2".into()],
            })
            .unwrap();
        // F2's first fast delta shows the switch.
        assert!(wait_until(ENGINE_WAIT, || count(&srv, |p| is_delta(
            p, "F2"
        )) >= 2));
        let mark = srv.requests().len();
        std::thread::sleep(Duration::from_millis(1000));
        let paths = paths_from(&srv, mark);
        assert!(paths.iter().any(|p| is_delta(p, "F2")), "{paths:?}");
        assert!(!paths.iter().any(|p| is_delta(p, "F1")), "{paths:?}");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_draft_syncs_drafts_and_sent_items_at_once() {
        let folders: &[(&str, &str, Option<&str>)] = &[
            ("DRAFTS1", "Drafts", Some("drafts")),
            ("SENT1", "Sent Items", Some("sentitems")),
        ];
        // After the send, Drafts reports the draft gone and Sent Items has
        // the server's copy under a new id.
        let drafts_next = delta_page(
            &[r#"{"id":"GRAPH-42","@removed":{"reason":"deleted"}}"#.to_string()],
            "/me/mailFolders/DRAFTS1/messages/delta?token=D-DRAFTS1",
        );
        let sent_next = delta_page(
            &[full_message_json("NEW-42", "Hi", "2026-07-16T10:00:00Z")],
            "/me/mailFolders/SENT1/messages/delta?token=D-SENT1",
        );
        let mut routes = vec![
            (
                Route {
                    method: "POST".into(),
                    path_prefix: "/me/messages/GRAPH-42/send".into(),
                    status: 202,
                    headers: vec![],
                    body: "".into(),
                },
                None,
            ),
            (
                Route {
                    method: "POST".into(),
                    path_prefix: "/me/messages".into(),
                    status: 201,
                    headers: vec![],
                    body: r#"{"id":"GRAPH-42","conversationId":"C","subject":"Hi","from":{"emailAddress":{"name":"","address":""}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"","isDraft":true}"#.into(),
                },
                None,
            ),
        ];
        routes.extend(folder_routes(
            folders,
            &[("DRAFTS1", &drafts_next), ("SENT1", &sent_next)],
            &[("SENT1", r#"{"totalItemCount":1,"unreadItemCount":0}"#)],
        ));
        let srv = FakeServer::start_limited(routes);
        let mut local = String::new();
        let (handle, store_path, dir) = spawn_fast(&srv, "send-sync", HOUR, None, |s| {
            local = s
                .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
                .unwrap();
        });
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        let mark = srv.requests().len();
        handle
            .cmd_tx
            .send(SyncCommand::SendDraft { id: local.clone() })
            .unwrap();
        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::State(SyncState::Idle))
        });
        assert!(
            events.contains(&SyncEvent::Sent { id: local.clone() }),
            "{events:?}"
        );

        let paths = paths_from(&srv, mark);
        let at = |pred: &dyn Fn(&str) -> bool| paths.iter().position(|p| pred(p));
        let send = at(&|p| p.starts_with("/me/messages/GRAPH-42/send")).expect("sent");
        let drafts = at(&|p| is_delta(p, "DRAFTS1")).expect("Drafts delta after the send");
        let drafts_counts = at(&|p| is_counts(p, "DRAFTS1")).expect("Drafts counts");
        let sent = at(&|p| is_delta(p, "SENT1")).expect("Sent Items delta after the send");
        let sent_counts = at(&|p| is_counts(p, "SENT1")).expect("Sent Items counts");
        assert!(
            send < drafts && drafts < drafts_counts && drafts_counts < sent && sent < sent_counts,
            "{paths:?}"
        );

        let store = Store::open(&store_path).unwrap();
        let ids: Vec<String> = store
            .messages_in_folder("SENT1", 50, 0)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec!["NEW-42".to_string()], "one row in Sent Items");
        let sent_folder = store
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.id == "SENT1")
            .unwrap();
        assert_eq!(sent_folder.total_count, 1);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drain_throttle_in_a_tick_is_honoured() {
        // A queued MarkRead whose first PATCH is throttled for 4 s. The tick
        // (300 ms) is shorter than the throttle, so a pass that forgot the
        // throttle would reach Graph again inside the window.
        let mut routes = vec![
            (
                Route {
                    method: "PATCH".into(),
                    path_prefix: "/me/messages/M1".into(),
                    status: 429,
                    headers: vec![("Retry-After".into(), "4".into())],
                    body: "{}".into(),
                },
                Some(1),
            ),
            (
                Route {
                    method: "PATCH".into(),
                    path_prefix: "/me/messages/M1".into(),
                    status: 200,
                    headers: vec![],
                    body: "{}".into(),
                },
                None,
            ),
        ];
        routes.extend(folder_routes(F1_F2, &[], &[]));
        let srv = FakeServer::start_limited(routes);
        let (handle, store_path, dir) = spawn_fast(
            &srv,
            "drain-throttle",
            Duration::from_millis(300),
            None,
            |s| {
                s.enqueue_op(&OutboxOp::MarkRead {
                    id: "M1".into(),
                    read: true,
                })
                .unwrap();
            },
        );
        let patches = || {
            srv.requests()
                .iter()
                .filter(|r| r.method == "PATCH")
                .count()
        };
        assert!(
            wait_until(ENGINE_WAIT, || patches() >= 1),
            "the throttled PATCH"
        );
        assert_eq!(patches(), 1, "the throttled PATCH only");
        let mark = srv.requests().len();
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(
            paths_from(&srv, mark),
            Vec::<String>::new(),
            "no Graph request inside the throttle window"
        );
        // After the deadline the op goes through.
        assert!(wait_until(ENGINE_WAIT, || patches() == 2));
        let store = Store::open(&store_path).unwrap();
        assert!(wait_until(ENGINE_WAIT, || store
            .pending_ops()
            .unwrap()
            .is_empty()));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fast_mode_quarantine_runs_a_full_pass_soon() {
        // An op one failure away from quarantine, answered 400: the startup
        // drain quarantines it, which clears every delta link.
        let mut routes = vec![(
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/messages/M1".into(),
                status: 400,
                headers: vec![],
                body: r#"{"error":"bad"}"#.into(),
            },
            None,
        )];
        routes.extend(folder_routes(F1_F2, &[], &[]));
        let srv = FakeServer::start_limited(routes);
        let (handle, _store_path, dir) =
            spawn_fast(&srv, "fast-quarantine", HOUR, Some(FAST), |s| {
                let seq = s
                    .enqueue_op(&OutboxOp::MarkRead {
                        id: "M1".into(),
                        read: true,
                    })
                    .unwrap();
                for _ in 0..MAX_OP_ATTEMPTS - 1 {
                    s.bump_op_attempts(seq, "bad");
                }
            });
        handle
            .cmd_tx
            .send(SyncCommand::SetFastFolders {
                ids: vec!["F1".into()],
            })
            .unwrap();
        wait_for(
            &handle.evt_rx,
            |e| matches!(e, SyncEvent::Error(m) if m.starts_with("quarantined")),
        );
        // The next fast deadline runs the normal pass (it re-lists the
        // folders), not the hour-long tick.
        assert!(
            wait_until(Duration::from_secs(10), || count(&srv, is_folder_list) >= 2),
            "a full pass soon after the quarantine: {:?}",
            paths_from(&srv, 0)
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fast_tick_honours_retry_after() {
        let throttled = (
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/messages/delta?token=".into(),
                status: 429,
                headers: vec![("Retry-After".into(), "4".into())],
                body: "{}".into(),
            },
            Some(1),
        );
        let mut routes = vec![throttled];
        routes.extend(folder_routes(F1_F2, &[], &[]));
        let srv = FakeServer::start_limited(routes);
        let (handle, _store_path, dir) =
            spawn_fast(&srv, "fast-throttle", HOUR, Some(FAST), |_| {});
        handle
            .cmd_tx
            .send(SyncCommand::SetFastFolders {
                ids: vec!["F1".into()],
            })
            .unwrap();
        // The first fast delta of F1 (its startup delta was the first call)
        // is the throttled one.
        assert!(wait_until(ENGINE_WAIT, || count(&srv, |p| is_delta(
            p, "F1"
        )) >= 2));
        let mark = srv.requests().len();
        handle
            .cmd_tx
            .send(SyncCommand::SyncFolder { id: "F1".into() })
            .unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(
            paths_from(&srv, mark),
            Vec::<String>::new(),
            "no fast tick and no SyncFolder call inside the window"
        );
        // After the deadline, fast ticks resume.
        assert!(wait_until(ENGINE_WAIT, || paths_from(&srv, mark)
            .iter()
            .any(|p| is_delta(p, "F1"))));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normal_tick_without_fast_option_is_unchanged() {
        let mut routes = folder_routes(F1_F2, &[], &[]);
        routes.insert(
            0,
            (get_route("/me/calendarView", 200, r#"{"value":[]}"#), None),
        );
        let srv = FakeServer::start_limited(routes);
        let (handle, _store_path, dir) = spawn_fast(
            &srv,
            "legacy-tick",
            Duration::from_millis(300),
            None,
            |_| {},
        );
        assert!(wait_until(ENGINE_WAIT, || count(&srv, is_calendar) >= 1));
        // The startup pass, then one tick: the folder tree (#1886), its
        // folders' deltas and the calendar; no people or categories, and no
        // counts GET of a single folder.
        let kinds: Vec<&str> = paths_from(&srv, 0)
            .iter()
            .map(|p| {
                if is_folder_list(p) {
                    "list"
                } else if p.starts_with("/me/mailFolders/F1/childFolders") {
                    "children F1"
                } else if p.starts_with("/me/mailFolders/F2/childFolders") {
                    "children F2"
                } else if is_delta(p, "F1") {
                    "delta F1"
                } else if is_delta(p, "F2") {
                    "delta F2"
                } else if p.starts_with("/me/people") {
                    "people"
                } else if p.starts_with("/me/outlook/masterCategories") {
                    "categories"
                } else if is_calendar(p) {
                    "calendar"
                } else {
                    "other"
                }
            })
            .take_while(|k| *k != "calendar")
            .collect();
        assert_eq!(
            kinds,
            vec![
                "list",
                "children F1",
                "children F2",
                "delta F1",
                "delta F2",
                "people",
                "categories",
                "list",
                "children F1",
                "children F2",
                "delta F1",
                "delta F2",
            ]
        );
        assert_eq!(count(&srv, |p| is_counts(p, "F1") || is_counts(p, "F2")), 0);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A queued MarkRead of M1 that Graph answers 500 on every attempt.
    fn failing_5xx_op_routes() -> Vec<(Route, Option<usize>)> {
        let mut routes = vec![
            (
                Route {
                    method: "PATCH".into(),
                    path_prefix: "/me/messages/M1".into(),
                    status: 500,
                    headers: vec![],
                    body: r#"{"error":"boom"}"#.into(),
                },
                None,
            ),
            (get_route("/me/calendarView", 200, r#"{"value":[]}"#), None),
        ];
        routes.extend(folder_routes(F1_F2, &[], &[]));
        routes
    }

    fn seed_mark_read(s: &Store) {
        s.enqueue_op(&OutboxOp::MarkRead {
            id: "M1".into(),
            read: true,
        })
        .unwrap();
    }

    fn patch_count(srv: &FakeServer) -> usize {
        srv.requests()
            .iter()
            .filter(|r| r.method == "PATCH")
            .count()
    }

    #[test]
    fn a_5xx_op_does_not_stall_the_normal_tick() {
        // A 5xx op is never quarantined: if it stopped the pass, the
        // calendar would never refresh again and the engine would sit
        // Offline under a growing back-off. The op waits under the outbox's
        // own back-off (5 s first) instead, and is not resent every tick.
        let srv = FakeServer::start_limited(failing_5xx_op_routes());
        let (handle, store_path, dir) = spawn_fast(
            &srv,
            "5xx-tick",
            Duration::from_millis(300),
            None,
            seed_mark_read,
        );
        assert!(
            wait_until(Duration::from_secs(3), || count(&srv, is_calendar) >= 3),
            "ticks go on past the 5xx op: {:?}",
            paths_from(&srv, 0)
        );
        assert_eq!(patch_count(&srv), 1, "the op waits for its back-off");
        // The pass settles at PendingOps, not Offline.
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::State(SyncState::PendingOps(1)))
        });
        let store = Store::open(&store_path).unwrap();
        assert_eq!(store.pending_ops().unwrap().len(), 1, "the op stays queued");
        // After the back-off a tick sends it again.
        assert!(wait_until(ENGINE_WAIT, || patch_count(&srv) >= 2));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_5xx_op_does_not_stall_the_fast_cycle() {
        let srv = FakeServer::start_limited(failing_5xx_op_routes());
        let (handle, _store_path, dir) =
            spawn_fast(&srv, "5xx-fast", HOUR, Some(FAST), seed_mark_read);
        handle
            .cmd_tx
            .send(SyncCommand::SetFastFolders {
                ids: vec!["F1".into()],
            })
            .unwrap();
        // Fast ticks keep syncing F1 every fast interval, while the op waits
        // for its own back-off (5 s) rather than being resent each time.
        assert!(
            wait_until(Duration::from_secs(3), || count(&srv, |p| is_delta(
                p, "F1"
            )) >= 6),
            "fast ticks go on past the 5xx op: {:?}",
            paths_from(&srv, 0)
        );
        assert!(
            patch_count(&srv) <= 2,
            "the op is not resent at every fast tick: {}",
            patch_count(&srv)
        );
        // A command's op goes into the same queue behind it: no engine-wide
        // Offline, and the folders still sync.
        handle
            .cmd_tx
            .send(SyncCommand::MarkRead {
                id: "M2".into(),
                read: true,
            })
            .unwrap();
        let before = count(&srv, |p| is_delta(p, "F1"));
        assert!(wait_until(Duration::from_secs(3), || count(&srv, |p| {
            is_delta(p, "F1")
        }) >= before + 3));
        assert!(patch_count(&srv) <= 2, "{}", patch_count(&srv));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fast_tick_cut_short_offline_stays_offline() {
        let srv = FakeServer::start_limited(folder_routes(F1_F2, &[], &[]));
        let (handle, _store_path, dir) = spawn_fast(&srv, "fast-offline", HOUR, Some(FAST), |_| {});
        handle
            .cmd_tx
            .send(SyncCommand::SetFastFolders {
                ids: vec!["F1".into()],
            })
            .unwrap();
        assert!(wait_until(ENGINE_WAIT, || count(&srv, |p| is_delta(
            p, "F1"
        )) >= 2));
        // The server goes away: the next fast delta is a transport error.
        drop(srv);
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::State(SyncState::Offline))
        });
        // Nothing settles it back to Idle inside the back-off (5 s).
        let deadline = Instant::now() + Duration::from_millis(1000);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match handle.evt_rx.recv_timeout(left) {
                Ok(SyncEvent::State(s)) => panic!("state left Offline: {s:?}"),
                Ok(_) => {}
                Err(_) => break,
            }
        }

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failing_folder_list_is_not_asked_for_at_every_fast_deadline() {
        // The folder list answers 403 (non-fatal: no back-off), so the
        // folders still need enumerating after every pass. The fast
        // deadlines must not turn into full passes: the list waits for the
        // normal tick.
        let srv = FakeServer::start(vec![get_route(
            "/me/mailFolders?",
            403,
            r#"{"error":"no"}"#,
        )]);
        let (handle, _store_path, dir) =
            spawn_fast(&srv, "fast-list-403", HOUR, Some(FAST), |_| {});
        handle
            .cmd_tx
            .send(SyncCommand::SetFastFolders {
                ids: vec!["F1".into()],
            })
            .unwrap();
        assert!(wait_until(ENGINE_WAIT, || count(&srv, is_folder_list) >= 1));
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(
            count(&srv, is_folder_list),
            1,
            "the startup list only: {:?}",
            paths_from(&srv, 0)
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F1/F2 routes where F2's first (initial) delta is throttled once for
    /// 2 s, so a full pass is cut short after the folder list and F1. F2
    /// lists no messages, so its first delta is the full one (#1871: a small
    /// folder skips the head).
    fn f2_initial_delta_throttled_routes() -> Vec<(Route, Option<usize>)> {
        let mut routes = vec![(
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F2/messages/delta?$orderby".into(),
                status: 429,
                headers: vec![("Retry-After".into(), "2".into())],
                body: "{}".into(),
            },
            Some(1),
        )];
        routes.extend(folder_routes(F1_F2, &[], &[]));
        routes
    }

    fn f2_delta_link(store_path: &std::path::Path) -> Option<String> {
        Store::open(store_path)
            .unwrap()
            .folders()
            .unwrap()
            .into_iter()
            .find(|f| f.id == "F2")
            .and_then(|f| f.delta_link)
    }

    #[test]
    fn a_throttled_startup_pass_is_retried_at_its_deadline_in_fast_mode() {
        let srv = FakeServer::start_limited(f2_initial_delta_throttled_routes());
        let (handle, store_path, dir) =
            spawn_fast(&srv, "fast-startup-retry", HOUR, Some(FAST), |_| {});
        // The startup pass stops at F2's 429; at the deadline the normal
        // pass runs again (not a fast tick that only clears the back-off),
        // so F2 gets its delta well before the hour-long tick.
        assert!(
            wait_until(Duration::from_secs(10), || count(&srv, |p| is_delta(
                p, "F2"
            )) >= 2),
            "F2's delta retried: {:?}",
            paths_from(&srv, 0)
        );
        assert!(wait_until(ENGINE_WAIT, || f2_delta_link(&store_path).is_some()));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_throttled_sign_in_pass_is_retried_at_its_deadline_in_fast_mode() {
        let mut routes = vec![(
            Route {
                method: "POST".into(),
                path_prefix: "/organizations/oauth2/v2.0/token".into(),
                status: 200,
                headers: vec![],
                body: r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600}"#.into(),
            },
            None,
        )];
        routes.extend(f2_initial_delta_throttled_routes());
        let srv = FakeServer::start_limited(routes);
        let dir = unique_dir("fast-signin-retry");
        let store_path = dir.join("mail.db");
        let handle = spawn_with_bases_and_options(
            store_path.clone(),
            dir.join("token.bin"), // no seed_token: starts signed out
            test_cfg(),
            3650,
            srv.base_url.clone(),
            srv.base_url.clone(),
            HOUR,
            EngineOptions {
                fast_tick: Some(FAST),
                ..EngineOptions::without_prefetch()
            },
        );
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::SignInRequired));
        let (_, response) = sign_in_through_loopback(&handle);
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(
            wait_until(Duration::from_secs(10), || count(&srv, |p| is_delta(
                p, "F2"
            )) >= 2),
            "F2's delta retried after the sign-in pass: {:?}",
            paths_from(&srv, 0)
        );
        assert!(wait_until(ENGINE_WAIT, || f2_delta_link(&store_path).is_some()));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sent_draft_the_drafts_delta_still_reports_stays_in_sent_items() {
        let folders: &[(&str, &str, Option<&str>)] = &[
            ("DRAFTS1", "Drafts", Some("drafts")),
            ("SENT1", "Sent Items", Some("sentitems")),
        ];
        // Graph has not moved the draft yet: the Drafts delta right after the
        // send reports it as changed (an upsert into Drafts), and Sent Items
        // has nothing new.
        let drafts_next = delta_page(
            &[full_message_json("GRAPH-42", "Hi", "2026-07-16T10:00:00Z")],
            "/me/mailFolders/DRAFTS1/messages/delta?token=D-DRAFTS1",
        );
        let mut routes = vec![
            (
                Route {
                    method: "POST".into(),
                    path_prefix: "/me/messages/GRAPH-42/send".into(),
                    status: 202,
                    headers: vec![],
                    body: "".into(),
                },
                None,
            ),
            (
                Route {
                    method: "POST".into(),
                    path_prefix: "/me/messages".into(),
                    status: 201,
                    headers: vec![],
                    body: r#"{"id":"GRAPH-42","conversationId":"C","subject":"Hi","from":{"emailAddress":{"name":"","address":""}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"","isDraft":true}"#.into(),
                },
                None,
            ),
        ];
        routes.extend(folder_routes(folders, &[("DRAFTS1", &drafts_next)], &[]));
        let srv = FakeServer::start_limited(routes);
        let mut local = String::new();
        let (handle, store_path, dir) = spawn_fast(&srv, "send-upsert", HOUR, None, |s| {
            local = s
                .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
                .unwrap();
        });
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        handle
            .cmd_tx
            .send(SyncCommand::SendDraft { id: local.clone() })
            .unwrap();
        wait_idle(&handle, &mut seen);
        assert!(
            seen.contains(&SyncEvent::Sent { id: local.clone() }),
            "{seen:?}"
        );
        assert!(
            srv.requests().iter().any(|r| r
                .path
                .starts_with("/me/mailFolders/DRAFTS1/messages/delta?token=")),
            "the post-send Drafts delta ran"
        );

        let store = Store::open(&store_path).unwrap();
        let (row, _) = store.draft(&local).unwrap().expect("the sent row");
        assert_eq!(row.id, "GRAPH-42");
        assert_eq!(
            row.folder_id, "SENT1",
            "the sent row stays under Sent Items"
        );
        assert!(!row.is_draft, "and is no draft");
        let at = |want: &SyncEvent| seen.iter().position(|e| e == want);
        assert!(
            at(&SyncEvent::Sending { id: local.clone() })
                < at(&SyncEvent::Sent { id: local.clone() }),
            "Sending precedes Sent: {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, SyncEvent::SendFailed { .. }))
        );
        assert!(
            store
                .messages_in_folder("DRAFTS1", 50, 0)
                .unwrap()
                .is_empty()
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- A send that fails goes back to Drafts (#1874) --------------------

    const DRAFTS_SENT: &[(&str, &str, Option<&str>)] = &[
        ("DRAFTS1", "Drafts", Some("drafts")),
        ("SENT1", "Sent Items", Some("sentitems")),
    ];

    const ACCESS_DENIED: &str = "Access is denied. Check credentials and try again.";

    fn post_route(path: &str, status: u16, body: &str) -> Route {
        Route {
            method: "POST".into(),
            path_prefix: path.into(),
            status,
            headers: vec![],
            body: body.into(),
        }
    }

    fn access_denied_body() -> String {
        format!(r#"{{"error":{{"code":"ErrorAccessDenied","message":"{ACCESS_DENIED}"}}}}"#)
    }

    /// The routes a send of the local draft created as `GRAPH-42` needs, most
    /// specific first: `sends` answer `/send` in order (each with its use
    /// limit), then the attachment upload, the PATCH, the create, and the
    /// Drafts and Sent Items folders (an empty delta each).
    fn send_routes(sends: Vec<(Route, Option<usize>)>) -> Vec<(Route, Option<usize>)> {
        let mut routes = sends;
        routes.push((
            post_route("/me/messages/GRAPH-42/attachments", 201, "{}"),
            None,
        ));
        routes.push((
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/messages/GRAPH-42".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
            None,
        ));
        routes.push((
            post_route(
                "/me/messages",
                201,
                r#"{"id":"GRAPH-42","conversationId":"C","subject":"Hi","from":{"emailAddress":{"name":"","address":""}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"hasAttachments":false,"importance":"normal","bodyPreview":"","isDraft":true}"#,
            ),
            None,
        ));
        routes.extend(folder_routes(DRAFTS_SENT, &[], &[]));
        routes
    }

    /// Seeds a local draft with `attachment` (a path) as its pending file,
    /// and returns the local id through `local`.
    fn seed_draft_with<'a>(local: &'a mut String, attachment: &str) -> impl FnOnce(&Store) + 'a {
        let attachment = attachment.to_string();
        move |s: &Store| {
            *local = s
                .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
                .unwrap();
            s.add_outbound_attachment(local, &attachment, "note.txt", 5)
                .unwrap();
        }
    }

    /// Sends a `FetchBody` of an unknown id and waits for its error: commands
    /// run in order, so everything before it has been handled.
    fn barrier(handle: &SyncHandle, seen: &mut Vec<SyncEvent>) {
        handle
            .cmd_tx
            .send(SyncCommand::FetchBody {
                id: "barrier".into(),
            })
            .unwrap();
        seen.extend(wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::Error(_))
        }));
    }

    fn position(seen: &[SyncEvent], pred: impl Fn(&SyncEvent) -> bool) -> Option<usize> {
        seen.iter().position(pred)
    }

    #[test]
    fn a_403_send_returns_the_draft_to_drafts_with_send_failed() {
        let dir = unique_dir("send-403-file");
        let file = dir.join("note.txt");
        std::fs::write(&file, b"hello").unwrap();
        let srv = FakeServer::start_limited(send_routes(vec![
            (
                post_route("/me/messages/GRAPH-42/send", 403, &access_denied_body()),
                Some(1),
            ),
            (post_route("/me/messages/GRAPH-42/send", 202, ""), None),
        ]));
        let mut local = String::new();
        let (handle, store_path, store_dir) = spawn_fast(
            &srv,
            "send-403",
            HOUR,
            None,
            seed_draft_with(&mut local, file.to_str().unwrap()),
        );
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);
        let startup = seen.len();

        handle
            .cmd_tx
            .send(SyncCommand::SendDraft { id: local.clone() })
            .unwrap();
        wait_idle(&handle, &mut seen);
        let send = &seen[startup..];

        let failed = SyncEvent::SendFailed {
            id: local.clone(),
            error: ACCESS_DENIED.into(),
        };
        let at_failed = position(send, |e| e == &failed).expect("SendFailed");
        let at_sending =
            position(send, |e| e == &SyncEvent::Sending { id: local.clone() }).expect("Sending");
        assert!(at_sending < at_failed, "{send:?}");
        // The tail lookxy's `a_failed_send_stays_shown_after_the_engine_settles`
        // replays: the settle to `Idle` comes after `SendFailed`.
        assert_eq!(
            send[at_failed..],
            [
                failed.clone(),
                SyncEvent::MessagesUpdated {
                    folder_id: "DRAFTS1".into()
                },
                SyncEvent::MessagesUpdated {
                    folder_id: "SENT1".into()
                },
                SyncEvent::State(SyncState::Idle),
            ],
            "{send:?}"
        );
        assert!(
            !send
                .iter()
                .any(|e| matches!(e, SyncEvent::Sent { .. } | SyncEvent::Error(_))),
            "{send:?}"
        );
        assert!(send.contains(&SyncEvent::MessagesUpdated {
            folder_id: "DRAFTS1".into()
        }));
        no_quarantine(&seen);
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/send"), 1);
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/attachments"), 1);

        let store = Store::open(&store_path).unwrap();
        let (row, body) = store.draft(&local).unwrap().expect("the draft");
        assert_eq!(row.id, "GRAPH-42");
        assert_eq!(row.folder_id, "DRAFTS1", "back in Drafts");
        assert!(row.is_draft);
        assert_eq!(row.subject, "Hi");
        assert_eq!(row.to_recipients, "bob@x");
        assert_eq!(body.content, "<p>hello</p>");
        let atts = store.outbound_attachments(&local).unwrap();
        assert_eq!(atts.len(), 1, "the pending attachment is kept");
        assert!(atts[0].uploaded, "and known to be on the Graph draft");
        assert!(
            store.pending_ops().unwrap().is_empty(),
            "the send is dropped"
        );
        assert!(
            store.get_delta_link("DRAFTS1").unwrap().is_some(),
            "no delta reconverge"
        );

        // #1874 1b: the resend PATCHes and sends, uploading nothing again.
        let resend = seen.len();
        handle
            .cmd_tx
            .send(SyncCommand::SendDraft { id: local.clone() })
            .unwrap();
        wait_idle(&handle, &mut seen);
        assert!(
            seen[resend..].contains(&SyncEvent::Sent { id: local.clone() }),
            "{seen:?}"
        );
        assert_eq!(hits(&srv, "POST", "/me/messages"), 1);
        assert_eq!(hits(&srv, "PATCH", "/me/messages/GRAPH-42"), 1);
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/send"), 2);
        assert_eq!(
            hits(&srv, "POST", "/me/messages/GRAPH-42/attachments"),
            1,
            "the file is attached once over both sends"
        );
        assert!(store.outbound_attachments(&local).unwrap().is_empty());
        let (row, _) = store.draft(&local).unwrap().unwrap();
        assert_eq!(row.folder_id, "SENT1");
        assert!(!row.is_draft);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_500_send_stays_queued_and_unchanged() {
        let srv = FakeServer::start_limited(send_routes(vec![(
            post_route("/me/messages/GRAPH-42/send", 500, "boom"),
            None,
        )]));
        let mut local = String::new();
        let (handle, store_path, dir) = spawn_fast(&srv, "send-500", HOUR, None, |s| {
            local = s
                .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
                .unwrap();
        });
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        handle
            .cmd_tx
            .send(SyncCommand::SendDraft { id: local.clone() })
            .unwrap();
        barrier(&handle, &mut seen);

        assert!(hits(&srv, "POST", "/me/messages/GRAPH-42/send") >= 1);
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, SyncEvent::SendFailed { .. } | SyncEvent::Sent { .. })),
            "{seen:?}"
        );
        let store = Store::open(&store_path).unwrap();
        let queued = store.pending_ops().unwrap();
        assert_eq!(queued.len(), 1);
        assert!(matches!(&queued[0].op, OutboxOp::SendDraft { id } if id == &local));
        assert!(queued[0].attempts >= 1, "attempts bumped");
        let (row, _) = store.draft(&local).unwrap().unwrap();
        assert_eq!(row.folder_id, "SENT1", "the optimistic write stays");
        assert!(!row.is_draft);
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The GET a posted send's delivery check makes (`is_draft`), answering
    /// `status` with `body`.
    fn draft_check_route(status: u16, body: &str) -> (Route, Option<usize>) {
        (get_route("/me/messages/GRAPH-42?", status, body), None)
    }

    /// Sends the local draft, whose `/send` fails first (a 5xx or a 503), then
    /// `between` runs, then a Refresh resends the deferred op at once; waits
    /// for the `Sent`. Returns the events seen.
    fn send_failing_then_refresh(
        handle: &SyncHandle,
        local: &str,
        between: impl FnOnce(),
        until: impl FnMut(&SyncEvent) -> bool,
    ) -> Vec<SyncEvent> {
        let mut seen = Vec::new();
        wait_idle(handle, &mut seen);
        handle
            .cmd_tx
            .send(SyncCommand::SendDraft {
                id: local.to_string(),
            })
            .unwrap();
        barrier(handle, &mut seen);
        between();
        handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
        seen.extend(wait_for(&handle.evt_rx, until));
        seen
    }

    #[test]
    fn a_5xx_send_still_a_draft_is_sent_again_uploading_each_attachment_once() {
        // FIX r1 (b): the 500 did not deliver it: Graph still has the draft,
        // so the next attempt PATCHes and sends again.
        let dir = unique_dir("send-5xx-file");
        let file = dir.join("note.txt");
        std::fs::write(&file, b"hello").unwrap();
        let srv = FakeServer::start_limited(send_routes(vec![
            (
                post_route("/me/messages/GRAPH-42/send", 500, "boom"),
                Some(1),
            ),
            (post_route("/me/messages/GRAPH-42/send", 202, ""), None),
            draft_check_route(200, r#"{"id":"GRAPH-42","isDraft":true}"#),
        ]));
        let mut local = String::new();
        let (handle, _store_path, store_dir) = spawn_fast(
            &srv,
            "send-5xx-once",
            HOUR,
            None,
            seed_draft_with(&mut local, file.to_str().unwrap()),
        );
        let seen = send_failing_then_refresh(
            &handle,
            &local,
            || {},
            |e| matches!(e, SyncEvent::Sent { .. }),
        );

        assert!(seen.contains(&SyncEvent::Sent { id: local.clone() }));
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, SyncEvent::SendFailed { .. }))
        );
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/send"), 2);
        assert_eq!(hits(&srv, "PATCH", "/me/messages/GRAPH-42"), 1);
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/attachments"), 1);
        assert_eq!(
            srv.requests()
                .iter()
                .filter(|r| r.method == "GET" && r.path.starts_with("/me/messages/GRAPH-42?"))
                .count(),
            1,
            "the delivery check ran once"
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_5xx_send_graph_delivered_is_sent_not_returned_to_drafts() {
        // FIX r1 (a): Graph delivered it despite the 500 and moved it to
        // Sent Items under a new id: the draft id 404s, the PATCH would too.
        let srv = FakeServer::start_limited(send_routes(vec![
            (
                post_route("/me/messages/GRAPH-42/send", 500, "boom"),
                Some(1),
            ),
            (
                Route {
                    method: "PATCH".into(),
                    path_prefix: "/me/messages/GRAPH-42".into(),
                    status: 404,
                    headers: vec![],
                    body: "".into(),
                },
                None,
            ),
            draft_check_route(404, ""),
        ]));
        let mut local = String::new();
        let (handle, store_path, dir) = spawn_fast(&srv, "send-5xx-delivered", HOUR, None, |s| {
            local = s
                .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
                .unwrap();
        });
        let mut seen = send_failing_then_refresh(
            &handle,
            &local,
            || {},
            |e| matches!(e, SyncEvent::Sent { .. } | SyncEvent::SendFailed { .. }),
        );
        wait_idle(&handle, &mut seen);

        assert!(
            seen.contains(&SyncEvent::Sent { id: local.clone() }),
            "{seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, SyncEvent::SendFailed { .. }))
        );
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/send"), 1);
        assert_eq!(hits(&srv, "PATCH", "/me/messages/GRAPH-42"), 0);
        let store = Store::open(&store_path).unwrap();
        assert!(store.pending_ops().unwrap().is_empty());
        let (row, _) = store.draft(&local).unwrap().unwrap();
        assert_eq!(row.folder_id, "SENT1", "not returned to Drafts");
        assert!(!row.is_draft);
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_503_send_graph_delivered_is_sent_not_returned_to_drafts() {
        // FIX r2: a gateway 503 can follow a send the back end accepted.
        // The 503 is `Throttled`, yet the retry must still check delivery.
        let srv = FakeServer::start_limited(send_routes(vec![
            (
                post_route("/me/messages/GRAPH-42/send", 503, "busy"),
                Some(1),
            ),
            (
                Route {
                    method: "PATCH".into(),
                    path_prefix: "/me/messages/GRAPH-42".into(),
                    status: 404,
                    headers: vec![],
                    body: "".into(),
                },
                None,
            ),
            draft_check_route(404, ""),
        ]));
        let mut local = String::new();
        let (handle, store_path, dir) = spawn_fast(&srv, "send-503-delivered", HOUR, None, |s| {
            local = s
                .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
                .unwrap();
        });
        let mut seen = send_failing_then_refresh(
            &handle,
            &local,
            || {},
            |e| matches!(e, SyncEvent::Sent { .. } | SyncEvent::SendFailed { .. }),
        );
        wait_idle(&handle, &mut seen);

        assert!(
            seen.contains(&SyncEvent::Sent { id: local.clone() }),
            "{seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, SyncEvent::SendFailed { .. }))
        );
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/send"), 1);
        assert_eq!(hits(&srv, "PATCH", "/me/messages/GRAPH-42"), 0);
        let store = Store::open(&store_path).unwrap();
        assert!(store.pending_ops().unwrap().is_empty());
        let (row, _) = store.draft(&local).unwrap().unwrap();
        assert_eq!(row.folder_id, "SENT1", "not returned to Drafts");
        assert!(!row.is_draft);
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_5xx_send_whose_row_a_delta_removed_is_sent() {
        // FIX r1 (c): between the two drains a Drafts delta `@removed` the
        // draft (Graph moved it): the row is gone, so it was delivered.
        let srv = FakeServer::start_limited(send_routes(vec![(
            post_route("/me/messages/GRAPH-42/send", 500, "boom"),
            None,
        )]));
        let mut local = String::new();
        let (handle, store_path, dir) = spawn_fast(&srv, "send-5xx-removed", HOUR, None, |s| {
            local = s
                .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
                .unwrap();
        });
        let ui_path = store_path.clone();
        let removed = local.clone();
        let seen = send_failing_then_refresh(
            &handle,
            &local,
            move || {
                Store::open(&ui_path)
                    .unwrap()
                    .delete_message(&removed)
                    .unwrap()
            },
            |e| matches!(e, SyncEvent::Sent { .. } | SyncEvent::SendFailed { .. }),
        );

        assert!(
            seen.contains(&SyncEvent::Sent { id: local.clone() }),
            "{seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, SyncEvent::SendFailed { .. }))
        );
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/send"), 1);
        assert!(
            !srv.requests()
                .iter()
                .any(|r| r.method == "GET" && r.path.starts_with("/me/messages/GRAPH-42")),
            "no check needed"
        );
        let store = Store::open(&store_path).unwrap();
        assert!(store.pending_ops().unwrap().is_empty());
        assert!(store.draft(&local).unwrap().is_none());

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_posted_send_whose_check_fails_is_never_returned_to_drafts() {
        // The delivery check answers 403: the send may have gone out, so it
        // keeps the attempts policy and its row stays under Sent Items.
        let srv = FakeServer::start_limited(send_routes(vec![
            (post_route("/me/messages/GRAPH-42/send", 500, "boom"), None),
            draft_check_route(403, &access_denied_body()),
        ]));
        let mut local = String::new();
        let (handle, store_path, dir) = spawn_fast(&srv, "send-check-403", HOUR, None, |s| {
            local = s
                .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
                .unwrap();
        });
        let mut seen = send_failing_then_refresh(
            &handle,
            &local,
            || {},
            |e| {
                matches!(
                    e,
                    SyncEvent::State(SyncState::PendingOps(_)) | SyncEvent::Error(_)
                )
            },
        );
        barrier(&handle, &mut seen);

        assert!(
            srv.requests()
                .iter()
                .any(|r| r.method == "GET" && r.path.starts_with("/me/messages/GRAPH-42?")),
            "{seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, SyncEvent::SendFailed { .. } | SyncEvent::Sent { .. })),
            "{seen:?}"
        );
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/send"), 1);
        let store = Store::open(&store_path).unwrap();
        let queued = store.pending_ops().unwrap();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].attempts >= 2, "the 500, then the check's 403");
        let (row, _) = store.draft(&local).unwrap().unwrap();
        assert_eq!(row.folder_id, "SENT1");
        assert!(!row.is_draft);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_send_with_a_missing_attachment_file_returns_to_drafts() {
        let srv = FakeServer::start_limited(send_routes(vec![(
            post_route("/me/messages/GRAPH-42/send", 202, ""),
            None,
        )]));
        let mut local = String::new();
        let (handle, store_path, dir) = spawn_fast(
            &srv,
            "send-missing-file",
            HOUR,
            None,
            seed_draft_with(&mut local, "/does/not/exist/missing.pdf"),
        );
        let mut seen = Vec::new();
        wait_idle(&handle, &mut seen);

        handle
            .cmd_tx
            .send(SyncCommand::SendDraft { id: local.clone() })
            .unwrap();
        wait_idle(&handle, &mut seen);

        let error = seen
            .iter()
            .find_map(|e| match e {
                SyncEvent::SendFailed { id, error } if id == &local => Some(error.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no SendFailed: {seen:?}"));
        assert!(
            error.starts_with("cannot read attachment /does/not/exist/missing.pdf"),
            "{error}"
        );
        assert_eq!(hits(&srv, "POST", "/me/messages/GRAPH-42/send"), 0);
        let store = Store::open(&store_path).unwrap();
        let (row, _) = store.draft(&local).unwrap().unwrap();
        assert_eq!(row.folder_id, "DRAFTS1");
        assert!(row.is_draft);
        let atts = store.outbound_attachments(&local).unwrap();
        assert_eq!(atts.len(), 1);
        assert!(!atts[0].uploaded);
        assert!(store.pending_ops().unwrap().is_empty());
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_drain_sends_a_and_returns_b_to_drafts() {
        // Both sends and a MarkRead behind them were queued while offline,
        // after the `SendDraft` arm's optimistic write (filed under Sent
        // Items, not a draft). Graph has moved neither draft yet: the Drafts
        // delta right after the send reports both, so only an id in the
        // drain's `sent` list is re-filed under Sent Items (#1885).
        let drafts_next = delta_page(
            &[
                full_message_json("GRAPH-A", "A", "2026-07-16T10:00:00Z"),
                full_message_json("GRAPH-B", "B", "2026-07-16T10:00:00Z"),
            ],
            "/me/mailFolders/DRAFTS1/messages/delta?token=D-DRAFTS1",
        );
        let patch = |path: &str| Route {
            method: "PATCH".into(),
            path_prefix: path.into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        };
        let mut routes = vec![
            (post_route("/me/messages/GRAPH-A/send", 202, ""), None),
            (
                post_route("/me/messages/GRAPH-B/send", 403, &access_denied_body()),
                None,
            ),
            (patch("/me/messages/GRAPH-A"), None),
            (patch("/me/messages/GRAPH-B"), None),
            (patch("/me/messages/M1"), None),
        ];
        routes.extend(folder_routes(
            DRAFTS_SENT,
            &[("DRAFTS1", &drafts_next)],
            &[],
        ));
        let srv = FakeServer::start_limited(routes);
        let (mut a, mut b) = (String::new(), String::new());
        let (handle, store_path, dir) = spawn_fast(&srv, "send-a-b", HOUR, None, |s| {
            for (id, wk, name) in [
                ("DRAFTS1", "drafts", "Drafts"),
                ("SENT1", "sentitems", "Sent Items"),
            ] {
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
            for (local, subject, graph) in [(&mut a, "A", "GRAPH-A"), (&mut b, "B", "GRAPH-B")] {
                *local = s
                    .create_local_draft(subject, "bob@x", "", "<p>x</p>")
                    .unwrap();
                s.reconcile_id(local, graph).unwrap();
                s.mark_sent(local);
                s.move_message(local, "SENT1").unwrap();
                s.enqueue_op(&OutboxOp::SendDraft { id: local.clone() })
                    .unwrap();
            }
            s.enqueue_op(&OutboxOp::MarkRead {
                id: "M1".into(),
                read: true,
            })
            .unwrap();
        });
        let mut seen = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::SendFailed { .. })
        });
        wait_idle(&handle, &mut seen);

        assert!(
            seen.contains(&SyncEvent::Sent { id: a.clone() }),
            "{seen:?}"
        );
        assert!(seen.contains(&SyncEvent::SendFailed {
            id: b.clone(),
            error: ACCESS_DENIED.into(),
        }));
        assert!(!seen.contains(&SyncEvent::Sent { id: b.clone() }));
        assert_eq!(
            hits(&srv, "PATCH", "/me/messages/M1"),
            1,
            "the drain went on"
        );
        let store = Store::open(&store_path).unwrap();
        assert!(store.pending_ops().unwrap().is_empty());
        assert!(
            srv.requests().iter().any(|r| r
                .path
                .starts_with("/me/mailFolders/DRAFTS1/messages/delta?token=")),
            "the post-send Drafts delta ran"
        );
        let (row_a, _) = store.draft(&a).unwrap().unwrap();
        assert_eq!(row_a.folder_id, "SENT1", "A was sent");
        let (row_b, _) = store.draft(&b).unwrap().unwrap();
        assert_eq!(
            row_b.folder_id, "DRAFTS1",
            "B is not in the post-send sync's sent list"
        );
        no_quarantine(&seen);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A queued MarkRead of M1 whose first PATCH gets a 500 and every later
    /// one succeeds.
    fn one_5xx_then_ok_routes() -> Vec<(Route, Option<usize>)> {
        let patch = |status: u16| Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status,
            headers: vec![],
            body: "{}".into(),
        };
        let mut routes = vec![(patch(500), Some(1)), (patch(200), None)];
        routes.extend(folder_routes(F1_F2, &[], &[]));
        routes
    }

    #[test]
    fn a_deferred_op_is_resent_at_its_back_off_without_a_fast_tick() {
        // An hour-long tick: only the op's own back-off (5 s) can bring the
        // second PATCH within the wait.
        let srv = FakeServer::start_limited(one_5xx_then_ok_routes());
        let (handle, store_path, dir) =
            spawn_fast(&srv, "op-retry-legacy", HOUR, None, seed_mark_read);
        assert!(wait_until(ENGINE_WAIT, || patch_count(&srv) >= 1));
        assert!(
            wait_until(ENGINE_WAIT, || patch_count(&srv) >= 2),
            "the op is resent at its back-off, not after the tick"
        );
        let store = Store::open(&store_path).unwrap();
        assert!(wait_until(ENGINE_WAIT, || store
            .pending_ops()
            .unwrap()
            .is_empty()));

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refresh_resends_a_deferred_op_at_once() {
        let srv = FakeServer::start_limited(one_5xx_then_ok_routes());
        let (handle, _store_path, dir) =
            spawn_fast(&srv, "op-retry-refresh", HOUR, None, seed_mark_read);
        assert!(wait_until(ENGINE_WAIT, || patch_count(&srv) >= 1));
        let sent_at = Instant::now();
        handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
        assert!(wait_until(ENGINE_WAIT, || patch_count(&srv) >= 2));
        assert!(
            sent_at.elapsed() < Duration::from_secs(4),
            "Refresh resent it before its 5 s back-off: {:?}",
            sent_at.elapsed()
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Folders and counts on every tick (#1886) ---

    /// A folder as `/me/mailFolders` or a `childFolders` list gives it, with
    /// its counts and `childFolderCount`.
    fn tree_folder(
        id: &str,
        name: &str,
        parent: Option<&str>,
        counts: (i64, i64),
        children: i64,
    ) -> String {
        let parent = parent.map_or("null".to_string(), |p| format!("\"{p}\""));
        let wk = if id == "F1" { r#""inbox""# } else { "null" };
        format!(
            r#"{{"id":"{id}","displayName":"{name}","parentFolderId":{parent},"totalItemCount":{},"unreadItemCount":{},"childFolderCount":{children},"wellKnownName":{wk}}}"#,
            counts.0, counts.1
        )
    }

    fn list(folders: &[String]) -> String {
        format!(r#"{{"value":[{}]}}"#, folders.join(","))
    }

    /// `path`'s answer for the startup pass (once), then `later` for every
    /// pass after it.
    fn startup_then(path: &str, startup: &str, later: &str) -> Vec<(Route, Option<usize>)> {
        vec![
            (get_route(path, 200, startup), Some(1)),
            (get_route(path, 200, later), None),
        ]
    }

    /// A folder's deltas: the startup delta holds `startup` and ends in a
    /// link; the first incremental delta answers `next` (if any), every
    /// later one an empty page.
    fn deltas(id: &str, startup: &[String], next: &[String]) -> Vec<(Route, Option<usize>)> {
        let delta = format!("/me/mailFolders/{id}/messages/delta");
        let link = format!("{delta}?token=D-{id}");
        let mut routes = Vec::new();
        if !next.is_empty() {
            routes.push((get_route(&link, 200, &delta_page(next, &link)), Some(1)));
        }
        routes.push((get_route(&link, 200, &delta_page(&[], &link)), None));
        routes.push((get_route(&delta, 200, &delta_page(startup, &link)), None));
        routes
    }

    /// The calendar a tick refreshes: empty.
    fn calendar_route() -> (Route, Option<usize>) {
        (get_route("/me/calendarView", 200, r#"{"value":[]}"#), None)
    }

    fn message_json(id: &str, read: bool, flagged: bool, categories: &[&str]) -> String {
        let flag = if flagged { "flagged" } else { "notFlagged" };
        let cats: Vec<String> = categories.iter().map(|c| format!("\"{c}\"")).collect();
        format!(
            r#"{{"id":"{id}","conversationId":"C-{id}","subject":"S {id}","from":{{"emailAddress":{{"name":"Alice","address":"alice@x"}}}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"2026-07-16T10:00:00Z","sentDateTime":"2026-07-16T10:00:00Z","isRead":{read},"flag":{{"flagStatus":"{flag}"}},"hasAttachments":false,"importance":"normal","bodyPreview":"p","categories":[{}]}}"#,
            cats.join(",")
        )
    }

    fn stored_folder(store: &Store, id: &str) -> Option<crate::store::FolderRow> {
        store.folders().unwrap().into_iter().find(|f| f.id == id)
    }

    fn sorted_folder_ids(store: &Store) -> Vec<String> {
        let mut ids: Vec<String> = store.folders().unwrap().into_iter().map(|f| f.id).collect();
        ids.sort();
        ids
    }

    /// Long enough for a missed change to show, short of `ENGINE_WAIT`.
    const TICK_WAIT: Duration = Duration::from_secs(10);
    const TICK: Duration = Duration::from_millis(300);

    #[test]
    fn a_tick_follows_message_changes_and_folder_counts() {
        // After startup the server marks M1 read, flags it and gives it a
        // category, and moves M2 from F1 to F2; its folder list now has
        // the new counts.
        let mut routes = vec![calendar_route()];
        routes.extend(deltas(
            "F1",
            &[
                message_json("M1", false, false, &[]),
                message_json("M2", false, false, &[]),
            ],
            &[
                message_json("M1", true, true, &["Red"]),
                r#"{"id":"M2","@removed":{"reason":"deleted"}}"#.to_string(),
            ],
        ));
        routes.extend(deltas("F2", &[], &[message_json("M2", false, false, &[])]));
        routes.extend(startup_then(
            "/me/mailFolders?",
            &list(&[
                tree_folder("F1", "Inbox", None, (2, 2), 0),
                tree_folder("F2", "Projects", None, (0, 0), 0),
            ]),
            &list(&[
                tree_folder("F1", "Inbox", None, (1, 0), 0),
                tree_folder("F2", "Projects", None, (1, 1), 0),
            ]),
        ));
        let srv = FakeServer::start_limited(routes);
        let (handle, store_path, dir) = spawn_fast(&srv, "tick-counts", TICK, None, |_| {});
        let store = Store::open(&store_path).unwrap();
        assert!(
            wait_until(TICK_WAIT, || {
                let f1 = store.messages_in_folder("F1", 50, 0).unwrap();
                let f2 = store.messages_in_folder("F2", 50, 0).unwrap();
                let counts =
                    |id| stored_folder(&store, id).map(|f| (f.total_count, f.unread_count));
                f1.len() == 1
                    && f1[0].id == "M1"
                    && f1[0].is_read
                    && f1[0].is_flagged
                    && f1[0].categories == vec!["Red".to_string()]
                    && f2.len() == 1
                    && f2[0].id == "M2"
                    && counts("F1") == Some((1, 0))
                    && counts("F2") == Some((1, 1))
            }),
            "folders: {:?}\nrequests: {:?}",
            store.folders().unwrap(),
            paths_from(&srv, 0)
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tick_follows_renamed_moved_and_created_folders() {
        // Startup: F1 (with child C1), F2, F3. Then on the server F2 is
        // renamed, F3 moves under F1, NEW is created at the top and NESTED
        // under C1.
        let mut routes = vec![calendar_route()];
        for id in ["F1", "F2", "F3", "C1", "NEW", "NESTED"] {
            routes.extend(deltas(id, &[], &[]));
        }
        routes.extend(startup_then(
            "/me/mailFolders/F1/childFolders",
            &list(&[tree_folder("C1", "Sub", Some("F1"), (0, 0), 0)]),
            &list(&[
                tree_folder("C1", "Sub", Some("F1"), (0, 0), 1),
                tree_folder("F3", "Old place", Some("F1"), (0, 0), 0),
            ]),
        ));
        routes.push((
            get_route(
                "/me/mailFolders/C1/childFolders",
                200,
                &list(&[tree_folder("NESTED", "Nested", Some("C1"), (0, 0), 0)]),
            ),
            None,
        ));
        routes.extend(startup_then(
            "/me/mailFolders?",
            &list(&[
                tree_folder("F1", "Inbox", None, (0, 0), 1),
                tree_folder("F2", "Projects", None, (0, 0), 0),
                tree_folder("F3", "Old place", None, (0, 0), 0),
            ]),
            &list(&[
                tree_folder("F1", "Inbox", None, (0, 0), 2),
                tree_folder("F2", "Projects 2026", None, (0, 0), 0),
                tree_folder("NEW", "Fresh", None, (0, 0), 0),
            ]),
        ));
        let srv = FakeServer::start_limited(routes);
        // F2 and F3 are expanded locally before the engine starts.
        let (handle, store_path, dir) = spawn_fast(&srv, "tick-tree", TICK, None, |s| {
            for (id, name) in [("F2", "Projects"), ("F3", "Old place")] {
                s.upsert_folder(&crate::graph::model::MailFolder {
                    id: id.into(),
                    display_name: name.into(),
                    parent_id: None,
                    total_count: 0,
                    unread_count: 0,
                    well_known_name: None,
                })
                .unwrap();
                s.set_folder_expanded(id, true).unwrap();
            }
        });
        let store = Store::open(&store_path).unwrap();
        assert!(
            wait_until(TICK_WAIT, || {
                let f2 = stored_folder(&store, "F2");
                let f3 = stored_folder(&store, "F3");
                let new = stored_folder(&store, "NEW");
                let nested = stored_folder(&store, "NESTED");
                f2.is_some_and(|f| f.display_name == "Projects 2026" && f.is_expanded)
                    && f3.is_some_and(|f| f.parent_id.as_deref() == Some("F1") && f.is_expanded)
                    && new.is_some_and(|f| f.parent_id.is_none() && f.display_name == "Fresh")
                    && nested.is_some_and(|f| f.parent_id.as_deref() == Some("C1"))
            }),
            "folders: {:?}\nrequests: {:?}",
            store.folders().unwrap(),
            paths_from(&srv, 0)
        );
        // The startup pass emits FoldersUpdated, and so does the tick that
        // renamed, moved and created folders.
        let mut updates = 0;
        wait_for(&handle.evt_rx, |e| {
            updates += usize::from(matches!(e, SyncEvent::FoldersUpdated));
            updates == 2
        });
        // At startup C1 said it had no children (`childFolderCount: 0`),
        // so the startup pass did not ask for them; the tick, where it says
        // it has one, did.
        let paths = paths_from(&srv, 0);
        let second_list = paths
            .iter()
            .enumerate()
            .filter(|(_, p)| is_folder_list(p))
            .nth(1)
            .map(|(i, _)| i)
            .unwrap();
        let is_c1_children = |p: &String| p.starts_with("/me/mailFolders/C1/childFolders");
        assert!(
            !paths[..second_list].iter().any(is_c1_children),
            "{paths:?}"
        );
        assert!(paths[second_list..].iter().any(is_c1_children), "{paths:?}");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tick_removes_a_folder_deleted_on_the_server_with_its_child() {
        let mut routes = vec![calendar_route()];
        routes.extend(deltas("F1", &[], &[]));
        routes.extend(deltas(
            "GONE",
            &[message_json("G1", false, false, &[])],
            &[],
        ));
        routes.extend(deltas("KID", &[message_json("G2", false, false, &[])], &[]));
        // Graph confirms both gone (#1886: a missing folder is asked for).
        for id in ["GONE", "KID"] {
            routes.push((
                get_route(&format!("/me/mailFolders/{id}?"), 404, "{}"),
                None,
            ));
        }
        routes.push((
            get_route(
                "/me/mailFolders/GONE/childFolders",
                200,
                &list(&[tree_folder("KID", "Kid", Some("GONE"), (1, 1), 0)]),
            ),
            None,
        ));
        routes.extend(startup_then(
            "/me/mailFolders?",
            &list(&[
                tree_folder("F1", "Inbox", None, (0, 0), 0),
                tree_folder("GONE", "Old", None, (1, 1), 1),
            ]),
            &list(&[tree_folder("F1", "Inbox", None, (0, 0), 0)]),
        ));
        let srv = FakeServer::start_limited(routes);
        let (handle, store_path, dir) = spawn_fast(&srv, "tick-delete", TICK, None, |_| {});
        let store = Store::open(&store_path).unwrap();
        // The startup pass stores both folders and their messages.
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        // Two ticks after startup.
        assert!(wait_until(TICK_WAIT, || count(&srv, is_folder_list) >= 3));
        assert_eq!(sorted_folder_ids(&store), vec!["F1"]);
        assert!(store.search("S G1", 10).unwrap().is_empty());
        assert!(store.search("S G2", 10).unwrap().is_empty());
        let n: usize = ["GONE", "KID"]
            .iter()
            .map(|id| store.messages_in_folder(id, 50, 0).unwrap().len())
            .sum();
        assert_eq!(n, 0);
        // Their startup deltas only: none on the tick that removed them,
        // none after.
        assert_eq!(count(&srv, |p| is_delta(p, "GONE")), 1);
        assert_eq!(count(&srv, |p| is_delta(p, "KID")), 1);
        // Each was confirmed gone before it was removed, once.
        for id in ["GONE", "KID"] {
            let confirm = format!("/me/mailFolders/{id}?$select=id");
            assert_eq!(count(&srv, |p| p == confirm), 1, "{id}");
        }
        // The removal tick emitted FoldersUpdated; nothing emitted an error.
        let events: Vec<SyncEvent> = handle.evt_rx.try_iter().collect();
        assert!(events.contains(&SyncEvent::FoldersUpdated), "{events:?}");
        assert!(
            !events.iter().any(|e| matches!(e, SyncEvent::Error(_))),
            "{events:?}"
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// After startup the listing no longer has X (expanded, holding XM),
    /// and Graph answers `status` when asked for X. Returns the store and
    /// the events after the startup pass, two ticks later.
    fn run_missing_folder(tag: &str, status: u16) -> (Store, Vec<SyncEvent>, FakeServer) {
        let mut routes = vec![calendar_route()];
        routes.extend(deltas("F1", &[], &[]));
        routes.extend(deltas("X", &[message_json("XM", false, false, &[])], &[]));
        let body = if status == 200 {
            r#"{"id":"X"}"#
        } else {
            "boom"
        };
        routes.push((get_route("/me/mailFolders/X?", status, body), None));
        routes.extend(startup_then(
            "/me/mailFolders?",
            &list(&[
                tree_folder("F1", "Inbox", None, (0, 0), 0),
                tree_folder("X", "Moving", None, (1, 1), 0),
            ]),
            &list(&[tree_folder("F1", "Inbox", None, (0, 0), 0)]),
        ));
        let srv = FakeServer::start_limited(routes);
        let (handle, store_path, dir) = spawn_fast(&srv, tag, TICK, None, |s| {
            s.upsert_folder(&MailFolder {
                id: "X".into(),
                display_name: "Moving".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: None,
            })
            .unwrap();
            s.set_folder_expanded("X", true).unwrap();
        });
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        let confirm = "/me/mailFolders/X?$select=id";
        assert!(wait_until(TICK_WAIT, || count(&srv, |p| p == confirm) >= 2));
        // By the third list, the second tick's confirm has been answered.
        assert!(wait_until(TICK_WAIT, || count(&srv, is_folder_list) >= 4));
        let events: Vec<SyncEvent> = handle.evt_rx.try_iter().collect();
        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let store = Store::open(&store_path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        (store, events, srv)
    }

    fn assert_x_kept(store: &Store) {
        let x = stored_folder(store, "X").expect("X kept");
        assert!(x.is_expanded);
        assert_eq!(store.messages_in_folder("X", 50, 0).unwrap()[0].id, "XM");
    }

    #[test]
    fn a_folder_missing_from_a_complete_listing_but_still_on_the_server_is_kept() {
        // Moved during the walk (e.g. into a parent already read): Graph
        // still has it, so it keeps its messages and local state, and no
        // tick reports a folder change.
        let (store, events, _srv) = run_missing_folder("tick-missing-200", 200);
        assert_x_kept(&store);
        assert!(!events.contains(&SyncEvent::FoldersUpdated), "{events:?}");
        assert!(
            !events.iter().any(|e| matches!(e, SyncEvent::Error(_))),
            "{events:?}"
        );
    }

    #[test]
    fn a_missing_folder_whose_confirm_get_fails_is_kept() {
        // A 500 is non-fatal: X stays for a later tick, the error shows,
        // and the pass goes on.
        let (store, events, srv) = run_missing_folder("tick-missing-500", 500);
        assert_x_kept(&store);
        assert!(
            events.iter().any(|e| matches!(e, SyncEvent::Error(_))),
            "{events:?}"
        );
        assert!(
            count(&srv, |p| p
                .starts_with("/me/mailFolders/F1/messages/delta?token="))
                >= 2
        );
    }

    #[test]
    fn a_failing_later_folder_page_removes_nothing() {
        // After startup the list's second page answers 500: the tick
        // neither renames F1 (its first page) nor removes F2.
        let mut routes = vec![
            calendar_route(),
            (
                get_route("/me/mailFolders?$top=100&$skiptoken=P2", 500, "boom"),
                None,
            ),
        ];
        routes.extend(deltas("F1", &[], &[]));
        routes.extend(deltas("F2", &[], &[]));
        routes.extend(startup_then(
            "/me/mailFolders?",
            &list(&[
                tree_folder("F1", "Inbox", None, (0, 0), 0),
                tree_folder("F2", "Projects", None, (0, 0), 0),
            ]),
            &format!(
                r#"{{"value":[{}],"@odata.nextLink":"/me/mailFolders?$top=100&$skiptoken=P2"}}"#,
                tree_folder("F1", "Renamed", None, (0, 0), 0)
            ),
        ));
        let srv = FakeServer::start_limited(routes);
        let (handle, store_path, dir) = spawn_fast(&srv, "tick-page-500", TICK, None, |_| {});
        let store = Store::open(&store_path).unwrap();
        // Two ticks that failed on the second page.
        assert!(wait_until(TICK_WAIT, || count(&srv, |p| p
            .contains("skiptoken=P2"))
            >= 2));
        assert_eq!(sorted_folder_ids(&store), vec!["F1", "F2"]);
        assert_eq!(stored_folder(&store, "F1").unwrap().display_name, "Inbox");
        // The folders still sync.
        assert!(count(&srv, |p| is_delta(p, "F2")) >= 2);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failing_child_folders_call_removes_nothing() {
        // After startup F1's childFolders answers 500 and the top list no
        // longer has F2: the listing is incomplete, so neither C1 (unseen)
        // nor F2 (gone) is removed, and F1's new name is still stored.
        let mut routes = vec![calendar_route()];
        for id in ["F1", "F2", "C1"] {
            routes.extend(deltas(id, &[], &[]));
        }
        routes.push((
            get_route(
                "/me/mailFolders/F1/childFolders",
                200,
                &list(&[tree_folder("C1", "Sub", Some("F1"), (0, 0), 0)]),
            ),
            Some(1),
        ));
        routes.push((
            get_route("/me/mailFolders/F1/childFolders", 500, "boom"),
            None,
        ));
        routes.extend(startup_then(
            "/me/mailFolders?",
            &list(&[
                tree_folder("F1", "Inbox", None, (0, 0), 1),
                tree_folder("F2", "Projects", None, (0, 0), 0),
            ]),
            &list(&[tree_folder("F1", "Renamed", None, (0, 0), 1)]),
        ));
        let srv = FakeServer::start_limited(routes);
        let (handle, store_path, dir) = spawn_fast(&srv, "tick-child-500", TICK, None, |_| {});
        let store = Store::open(&store_path).unwrap();
        assert!(wait_until(TICK_WAIT, || count(&srv, is_folder_list) >= 3));
        assert_eq!(sorted_folder_ids(&store), vec!["C1", "F1", "F2"]);
        assert_eq!(stored_folder(&store, "F1").unwrap().display_name, "Renamed");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_folder_list_removes_nothing() {
        let mut routes = vec![calendar_route()];
        routes.extend(deltas("F1", &[], &[]));
        routes.extend(startup_then(
            "/me/mailFolders?",
            &list(&[tree_folder("F1", "Inbox", None, (0, 0), 0)]),
            &list(&[]),
        ));
        let srv = FakeServer::start_limited(routes);
        let (handle, store_path, dir) = spawn_fast(&srv, "tick-empty", TICK, None, |_| {});
        let store = Store::open(&store_path).unwrap();
        assert!(wait_until(TICK_WAIT, || count(&srv, is_folder_list) >= 3));
        assert_eq!(sorted_folder_ids(&store), vec!["F1"]);

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_drafts_sentinel_survives_an_enumeration_that_does_not_list_it() {
        // A draft made before any folder synced sits under the sentinel.
        // The startup enumeration lists no Drafts: the sentinel stays with
        // the draft. A later one (Refresh) lists Drafts: the draft is
        // adopted and the empty sentinel goes.
        let mut routes = vec![calendar_route()];
        routes.extend(deltas("F1", &[], &[]));
        routes.extend(deltas("DRAFTS", &[], &[]));
        let mut drafts = tree_folder("DRAFTS", "Drafts", None, (0, 0), 0);
        drafts = drafts.replace(r#""wellKnownName":null"#, r#""wellKnownName":"drafts""#);
        routes.extend(startup_then(
            "/me/mailFolders?",
            &list(&[tree_folder("F1", "Inbox", None, (0, 0), 0)]),
            &list(&[tree_folder("F1", "Inbox", None, (0, 0), 0), drafts]),
        ));
        let srv = FakeServer::start_limited(routes);
        let mut draft_id = String::new();
        let (handle, store_path, dir) = spawn_fast(&srv, "tick-sentinel", HOUR, None, |s| {
            draft_id = s.create_local_draft("Hi", "", "", "<p>x</p>").unwrap();
        });
        let store = Store::open(&store_path).unwrap();
        let sentinel = store.drafts_folder_id().unwrap();
        assert_eq!(sentinel, "local:drafts-pending");
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        assert_eq!(sorted_folder_ids(&store), vec!["F1", sentinel.as_str()]);
        assert_eq!(
            store.messages_in_folder(&sentinel, 50, 0).unwrap()[0].id,
            draft_id
        );

        handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
        assert!(
            wait_until(TICK_WAIT, || sorted_folder_ids(&store)
                == vec!["DRAFTS", "F1"]),
            "{:?}",
            store.folders().unwrap()
        );
        assert_eq!(
            store.messages_in_folder("DRAFTS", 50, 0).unwrap()[0].id,
            draft_id
        );

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tick_with_no_folder_change_emits_no_folders_updated() {
        let mut routes = vec![calendar_route()];
        routes.extend(deltas("F1", &[], &[]));
        routes.push((
            get_route(
                "/me/mailFolders?",
                200,
                &list(&[tree_folder("F1", "Inbox", None, (3, 1), 0)]),
            ),
            None,
        ));
        let srv = FakeServer::start_limited(routes);
        let (handle, _store_path, dir) = spawn_fast(&srv, "tick-quiet", TICK, None, |_| {});
        // The startup pass emits it.
        wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::FoldersUpdated));
        // By the fourth list, the second and third passes have emitted
        // whatever they would.
        assert!(wait_until(TICK_WAIT, || count(&srv, is_folder_list) >= 4));
        let events: Vec<SyncEvent> = handle.evt_rx.try_iter().collect();
        assert!(!events.contains(&SyncEvent::FoldersUpdated), "{events:?}");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole-mailbox download (#1871): a head delta per folder in the
    /// pass, then the full delta one page per idle step.
    mod backfill {
        use super::*;
        use crate::store::{FolderHistory, SCOPE_HEAD};

        /// Above `SMALL_FOLDER`, so a folder gets a head first.
        const BIG: i64 = 500;

        /// A folder of the fake mailbox: listed with `total`, its head
        /// answering `head`, and its full delta paged as `pages`.
        struct Fold {
            id: &'static str,
            name: &'static str,
            well_known: Option<&'static str>,
            total: i64,
            head: Vec<String>,
            pages: Vec<Vec<String>>,
        }

        fn fold(id: &'static str, name: &'static str, well_known: Option<&'static str>) -> Fold {
            Fold {
                id,
                name,
                well_known,
                total: BIG,
                head: Vec::new(),
                pages: vec![Vec::new()],
            }
        }

        fn inbox() -> Fold {
            fold("F1", "Inbox", Some("inbox"))
        }

        /// `n` pages of one message each (`P{id}-{k}`), all received on
        /// 2025-06-01 (the fake server does not order them).
        fn pages_of(id: &str, n: usize) -> Vec<Vec<String>> {
            (1..=n)
                .map(|k| vec![msg(&format!("P{id}-{k}"), "2025-06-01T00:00:00Z")])
                .collect()
        }

        fn msg(id: &str, received: &str) -> String {
            full_message_json(id, id, received)
        }

        fn head_link(id: &str) -> String {
            format!("/me/mailFolders/{id}/messages/delta?token=H-{id}")
        }

        /// The full delta's page `k` (2-based: page 1 is the first call).
        fn page_link(id: &str, k: usize) -> String {
            format!("/me/mailFolders/{id}/messages/delta?skiptoken={id}_{k:03}")
        }

        fn full_link(id: &str) -> String {
            format!("/me/mailFolders/{id}/messages/delta?token=A-{id}")
        }

        fn legacy_link(id: &str) -> String {
            format!("/me/mailFolders/{id}/messages/delta?token=L-{id}")
        }

        fn page(items: &[String], next: Option<&str>, delta: Option<&str>) -> String {
            let mut body = format!(r#"{{"value":[{}]"#, items.join(","));
            if let Some(n) = next {
                body.push_str(&format!(r#","@odata.nextLink":"{n}""#));
            }
            if let Some(d) = delta {
                body.push_str(&format!(r#","@odata.deltaLink":"{d}""#));
            }
            body.push('}');
            body
        }

        type Paced = (Route, Option<usize>, Duration);

        fn route(prefix: &str, status: u16, body: &str) -> Route {
            get_route(prefix, status, body)
        }

        fn throttle(prefix: &str, secs: u64) -> Route {
            Route {
                method: "GET".into(),
                path_prefix: prefix.into(),
                status: 429,
                headers: vec![("Retry-After".into(), secs.to_string())],
                body: "{}".into(),
            }
        }

        /// Every route of the fake mailbox, `extra` first (so they win). A
        /// full delta page waits `delay` before it answers.
        fn mailbox(
            folders: &[Fold],
            extra: Vec<(Route, Option<usize>)>,
            delay: Duration,
        ) -> Vec<Paced> {
            let mut routes: Vec<Paced> = extra
                .into_iter()
                .map(|(r, n)| (r, n, Duration::ZERO))
                .collect();
            let now = |r: Route| (r, None, Duration::ZERO);
            let slow = |r: Route| (r, None, delay);
            let mut listed = Vec::new();
            for f in folders {
                let id = f.id;
                let delta = format!("/me/mailFolders/{id}/messages/delta");
                routes.push(now(route(
                    &format!("/me/mailFolders/{id}/childFolders"),
                    200,
                    r#"{"value":[]}"#,
                )));
                routes.push(now(route(
                    &format!("{delta}?$filter"),
                    200,
                    &page(&f.head, None, Some(&head_link(id))),
                )));
                for link in [head_link(id), full_link(id), legacy_link(id)] {
                    routes.push(now(route(&link, 200, &page(&[], None, Some(&link)))));
                }
                let n = f.pages.len();
                for (i, items) in f.pages.iter().enumerate() {
                    let k = i + 1;
                    let (next, last) = if k < n {
                        (Some(page_link(id, k + 1)), None)
                    } else {
                        (None, Some(full_link(id)))
                    };
                    let body = page(items, next.as_deref(), last.as_deref());
                    let prefix = if k == 1 {
                        format!("{delta}?$orderby")
                    } else {
                        page_link(id, k)
                    };
                    routes.push(slow(route(&prefix, 200, &body)));
                }
                routes.push(now(route(
                    &format!("/me/mailFolders/{id}?"),
                    200,
                    &format!(r#"{{"totalItemCount":{},"unreadItemCount":0}}"#, f.total),
                )));
                let wk = f
                    .well_known
                    .map_or("null".to_string(), |w| format!("\"{w}\""));
                listed.push(format!(
                    r#"{{"id":"{id}","displayName":"{}","parentFolderId":null,"totalItemCount":{},"unreadItemCount":0,"wellKnownName":{wk}}}"#,
                    f.name, f.total
                ));
            }
            routes.push(now(route(
                "/me/mailFolders?",
                200,
                &format!(r#"{{"value":[{}]}}"#, listed.join(",")),
            )));
            routes
        }

        /// A fresh engine directory with a signed-in token.
        fn new_dir(tag: &str) -> PathBuf {
            let dir = unique_dir(tag);
            seed_token(&dir.join("token.bin"));
            dir
        }

        fn store_in(dir: &std::path::Path) -> Store {
            Store::open(&dir.join("mail.db")).unwrap()
        }

        fn spawn_in(
            srv: &FakeServer,
            dir: &std::path::Path,
            days: i64,
            tick: Duration,
            fast_tick: Option<Duration>,
        ) -> SyncHandle {
            spawn_with_bases_and_options(
                dir.join("mail.db"),
                dir.join("token.bin"),
                test_cfg(),
                days,
                srv.base_url.clone(),
                srv.base_url.clone(),
                tick,
                EngineOptions {
                    fast_tick,
                    ..EngineOptions::without_prefetch()
                },
            )
        }

        /// Stops the engine and waits for its thread to end (its event
        /// channel closes), so a second engine can open the same store.
        fn stop(handle: SyncHandle) {
            let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
            let deadline = Instant::now() + ENGINE_WAIT;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match handle.evt_rx.recv_timeout(left) {
                    Ok(_) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => panic!("engine did not stop"),
                }
            }
        }

        fn is_complete(e: &SyncEvent, id: &str) -> bool {
            matches!(e, SyncEvent::FolderProgress { folder_id, complete: true, .. } if folder_id == id)
        }

        /// Events until `id`'s backfill completes.
        fn until_complete(handle: &SyncHandle, id: &str) -> Vec<SyncEvent> {
            wait_for(&handle.evt_rx, |e| is_complete(e, id))
        }

        /// `(synced, total, complete)` of every `FolderProgress` of `id`.
        fn progress(events: &[SyncEvent], id: &str) -> Vec<(u64, Option<i64>, bool)> {
            events
                .iter()
                .filter_map(|e| match e {
                    SyncEvent::FolderProgress {
                        folder_id,
                        synced,
                        total,
                        complete,
                    } if folder_id == id => Some((*synced, *total, *complete)),
                    _ => None,
                })
                .collect()
        }

        fn is_head(p: &str, id: &str) -> bool {
            p.starts_with(&format!("/me/mailFolders/{id}/messages/delta?$filter"))
        }

        /// A request of `id`'s full delta: its first call or a page link.
        fn is_backfill(p: &str, id: &str) -> bool {
            p.starts_with(&format!("/me/mailFolders/{id}/messages/delta?$orderby"))
                || p.starts_with(&format!("/me/mailFolders/{id}/messages/delta?skiptoken="))
        }

        fn is_full_first(p: &str, id: &str) -> bool {
            p.starts_with(&format!("/me/mailFolders/{id}/messages/delta?$orderby"))
        }

        fn positions(srv: &FakeServer, pred: impl Fn(&str) -> bool) -> Vec<usize> {
            paths_from(srv, 0)
                .iter()
                .enumerate()
                .filter(|(_, p)| pred(p))
                .map(|(i, _)| i)
                .collect()
        }

        fn history(dir: &std::path::Path, id: &str) -> FolderHistory {
            store_in(dir).folder_history(id).unwrap().unwrap()
        }

        fn stored_ids(dir: &std::path::Path, id: &str) -> Vec<String> {
            store_in(dir)
                .messages_in_folder(id, 1000, 0)
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect()
        }

        #[test]
        fn history_complete_by_mode() {
            // scope → (complete in window mode, in whole mode), for a folder
            // with a delta link (`incomplete_histories` asks only then).
            let cases: &[(Option<&str>, bool, bool)] = &[
                (None, true, false),
                (Some(SCOPE_HEAD), false, false),
                (Some(SCOPE_WINDOW), true, false),
                (Some(SCOPE_ALL), true, true),
                (Some("other"), false, false),
            ];
            for &(scope, window, whole) in cases {
                assert_eq!(history_complete(scope, false), window, "{scope:?} window");
                assert_eq!(history_complete(scope, true), whole, "{scope:?} whole");
            }
        }

        #[test]
        fn whole_mailbox_heads_every_folder_inbox_first_before_any_backfill_page() {
            // Graph lists Projects before the Inbox.
            let srv = FakeServer::start_paced(mailbox(
                &[
                    Fold {
                        pages: pages_of("F2", 2),
                        ..fold("F2", "Projects", None)
                    },
                    Fold {
                        pages: pages_of("F1", 2),
                        ..inbox()
                    },
                ],
                vec![],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-order");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let mut done = (false, false);
            wait_for(&handle.evt_rx, |e| {
                done.0 |= is_complete(e, "F1");
                done.1 |= is_complete(e, "F2");
                done.0 && done.1
            });
            let paths = paths_from(&srv, 0);
            let heads = positions(&srv, |p| is_head(p, "F1") || is_head(p, "F2"));
            let pages = positions(&srv, |p| is_backfill(p, "F1") || is_backfill(p, "F2"));
            assert_eq!(heads.len(), 2, "{paths:?}");
            assert_eq!(pages.len(), 4, "{paths:?}");
            assert!(
                heads.iter().all(|h| *h < pages[0]),
                "every head before any page: {paths:?}"
            );
            assert!(
                is_head(&paths[heads[0]], "F1"),
                "the Inbox head first: {paths:?}"
            );
            // The Inbox's backfill runs to its end before the next folder's.
            let f1 = positions(&srv, |p| is_backfill(p, "F1"));
            let f2 = positions(&srv, |p| is_backfill(p, "F2"));
            assert!(f1.last() < f2.first(), "{paths:?}");
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn whole_mailbox_backfill_pages_newest_first_to_completion_with_progress() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    head: vec![msg("NEW", "2026-11-05T00:00:00Z")],
                    pages: vec![
                        vec![
                            msg("NEW", "2026-11-05T00:00:00Z"),
                            msg("A", "2026-01-01T00:00:00Z"),
                        ],
                        vec![msg("B", "2015-01-01T00:00:00Z")],
                        vec![msg("OLD", "2001-01-01T00:00:00Z")],
                    ],
                    ..inbox()
                }],
                vec![],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-pages");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let events = until_complete(&handle, "F1");
            assert_eq!(
                progress(&events, "F1"),
                vec![
                    (2, Some(BIG), false),
                    (3, Some(BIG), false),
                    (4, Some(BIG), true)
                ]
            );
            // The first full-delta call asks for the newest first.
            let firsts = positions(&srv, |p| is_full_first(p, "F1"));
            assert_eq!(firsts.len(), 1);
            assert!(paths_from(&srv, 0)[firsts[0]].contains("$orderby=receivedDateTime%20desc"));
            let mut ids = stored_ids(&dir, "F1");
            ids.sort();
            assert_eq!(ids, ["A", "B", "NEW", "OLD"]);
            let h = history(&dir, "F1");
            assert_eq!(
                (h.scope.as_deref(), h.synced, h.backfill_link),
                (Some(SCOPE_ALL), 4, None)
            );
            assert_eq!(
                store_in(&dir).get_delta_link("F1").unwrap(),
                Some(full_link("F1"))
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn backfill_resumes_from_the_persisted_link_after_restart() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 3),
                    ..inbox()
                }],
                vec![(throttle(&page_link("F1", 2), 3600), Some(1))],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-resume");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            assert!(wait_until(ENGINE_WAIT, || count(&srv, |p| p
                == page_link("F1", 2))
                >= 1));
            stop(handle);
            let h = history(&dir, "F1");
            assert_eq!((h.backfill_link, h.synced), (Some(page_link("F1", 2)), 1));

            let restart = srv.requests().len();
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let events = until_complete(&handle, "F1");
            assert_eq!(progress(&events, "F1").last(), Some(&(3, Some(BIG), true)));
            let after: Vec<String> = paths_from(&srv, restart);
            let first_page = after.iter().find(|p| is_backfill(p, "F1")).unwrap();
            assert_eq!(*first_page, page_link("F1", 2), "{after:?}");
            assert_eq!(
                count(&srv, |p| is_full_first(p, "F1")),
                1,
                "the first call once"
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn backfill_retries_a_429_after_retry_after() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 3),
                    ..inbox()
                }],
                vec![(throttle(&page_link("F1", 2), 1), Some(1))],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-429");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let events = until_complete(&handle, "F1");
            assert_eq!(progress(&events, "F1").last(), Some(&(3, Some(BIG), true)));
            let tries: Vec<Instant> = srv
                .requests()
                .into_iter()
                .filter(|r| r.path == page_link("F1", 2))
                .map(|r| r.at)
                .collect();
            assert_eq!(tries.len(), 2);
            assert!(
                tries[1] - tries[0] >= Duration::from_secs(1),
                "retried after {:?}",
                tries[1] - tries[0]
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_throttled_pass_is_retried_at_expiry_while_a_backfill_waits() {
            // The startup pass stops at F2's head (429, 1 s). At its expiry
            // both the pass and F1's 30 slow pages are ready: the pass runs
            // first, not after the backfill (an hour-long tick).
            let srv = FakeServer::start_paced(mailbox(
                &[
                    Fold {
                        pages: pages_of("F1", 30),
                        ..inbox()
                    },
                    fold("F2", "Projects", None),
                ],
                vec![(
                    throttle("/me/mailFolders/F2/messages/delta?$filter", 1),
                    Some(1),
                )],
                Duration::from_millis(100),
            ));
            let dir = new_dir("bf-retry-pass");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            assert!(wait_until(ENGINE_WAIT, || count(&srv, |p| is_head(
                p, "F2"
            )) >= 2));
            let heads = positions(&srv, |p| is_head(p, "F2"));
            let pages = positions(&srv, |p| is_backfill(p, "F1"));
            assert!(
                pages.iter().filter(|i| **i < heads[1]).count() < 30,
                "F2's head retried before F1's backfill ended: {:?}",
                paths_from(&srv, 0)
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_deferred_op_is_resent_at_its_back_off_while_a_backfill_runs() {
            // The op's 5xx back-off is 5 s; the backfill takes about 9 s and
            // the tick an hour, so only the due `op_retry` resends it in time.
            let patch = |status: u16| Route {
                method: "PATCH".into(),
                path_prefix: "/me/messages/M1".into(),
                status,
                headers: vec![],
                body: "{}".into(),
            };
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 60),
                    ..inbox()
                }],
                vec![(patch(500), Some(1)), (patch(200), None)],
                Duration::from_millis(150),
            ));
            let dir = new_dir("bf-op-retry");
            seed_mark_read(&store_in(&dir));
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            assert!(wait_until(ENGINE_WAIT, || patch_count(&srv) >= 2));
            let patches: Vec<usize> = srv
                .requests()
                .iter()
                .enumerate()
                .filter(|(_, r)| r.method == "PATCH")
                .map(|(i, _)| i)
                .collect();
            let pages = positions(&srv, |p| is_backfill(p, "F1"));
            assert!(
                !pages.is_empty() && pages[0] < patches[1],
                "the backfill ran first"
            );
            assert!(
                pages.iter().filter(|i| **i < patches[1]).count() < 60,
                "the op was resent while the backfill ran"
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_backfill_step_sets_no_syncing_state() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 4),
                    ..inbox()
                }],
                vec![],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-state");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let events = until_complete(&handle, "F1");
            let first = events
                .iter()
                .position(|e| matches!(e, SyncEvent::FolderProgress { .. }))
                .unwrap();
            assert!(
                !events[first..]
                    .iter()
                    .any(|e| matches!(e, SyncEvent::State(_))),
                "no state change during the backfill: {events:?}"
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_small_folder_gets_one_full_delta_and_is_complete() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    total: 0,
                    ..inbox()
                }],
                vec![],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-small");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let mut events = until_complete(&handle, "F1");
            // Give a backfill step the time to run, if one would.
            std::thread::sleep(Duration::from_millis(300));
            events.extend(handle.evt_rx.try_iter());
            let deltas: Vec<String> = paths_from(&srv, 0)
                .into_iter()
                .filter(|p| is_delta(p, "F1"))
                .collect();
            assert_eq!(deltas.len(), 1, "{deltas:?}");
            assert!(is_full_first(&deltas[0], "F1"), "{deltas:?}");
            assert_eq!(progress(&events, "F1"), vec![(0, Some(0), true)]);
            assert_eq!(history(&dir, "F1").scope.as_deref(), Some(SCOPE_ALL));
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_command_preempts_the_remaining_backfill_pages() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    head: vec![msg("M1", "2026-11-05T00:00:00Z")],
                    pages: pages_of("F1", 20),
                    ..inbox()
                }],
                vec![(body_route("text", "hello"), None)],
                Duration::from_millis(200),
            ));
            let dir = new_dir("bf-preempt");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            wait_for(&handle.evt_rx, |e| {
                matches!(
                    e,
                    SyncEvent::FolderProgress {
                        complete: false,
                        ..
                    }
                )
            });
            let sent = Instant::now();
            handle
                .cmd_tx
                .send(SyncCommand::FetchBody { id: "M1".into() })
                .unwrap();
            wait_for(&handle.evt_rx, |e| matches!(e, SyncEvent::BodyReady { .. }));
            let reqs = srv.requests();
            let body = reqs
                .iter()
                .position(|r| r.path.starts_with("/me/messages/M1"))
                .unwrap();
            let between = reqs[..body]
                .iter()
                .filter(|r| r.at >= sent && is_backfill(&r.path, "F1"))
                .count();
            assert!(between <= 1, "{between} pages ran before the command");
            assert!(
                !reqs[..body].iter().any(|r| r.path.contains("token=A-F1"))
                    && reqs[..body]
                        .iter()
                        .filter(|r| is_backfill(&r.path, "F1"))
                        .count()
                        < 20,
                "pages remained when the command ran"
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn new_mail_arrives_during_a_backfill() {
            let new = page(
                &[msg("NEW", "2026-11-06T00:00:00Z")],
                None,
                Some(&head_link("F1")),
            );
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 15),
                    ..inbox()
                }],
                vec![(route(&head_link("F1"), 200, &new), Some(1))],
                Duration::from_millis(100),
            ));
            let dir = new_dir("bf-new-mail");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, Duration::from_millis(300), None);
            until_complete(&handle, "F1");
            let pages = positions(&srv, |p| is_backfill(p, "F1"));
            let ticks = positions(&srv, |p| p == head_link("F1"));
            assert!(
                ticks
                    .iter()
                    .any(|t| pages.first() < Some(t) && Some(t) < pages.last()),
                "a tick's head round between two pages: {:?}",
                paths_from(&srv, 0)
            );
            assert!(stored_ids(&dir, "F1").contains(&"NEW".to_string()));
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn window_mode_skips_old_items_and_keeps_the_head_at_14_days() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: vec![vec![
                        msg("RECENT", &unix_to_iso8601(now_unix() - 86400)),
                        msg("OLD", "2001-01-01T00:00:00Z"),
                    ]],
                    ..inbox()
                }],
                vec![],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-window");
            let handle = spawn_in(&srv, &dir, 30, HOUR, None);
            let events = until_complete(&handle, "F1");
            assert_eq!(progress(&events, "F1"), vec![(1, None, true)]);
            assert_eq!(stored_ids(&dir, "F1"), ["RECENT"]);
            assert_eq!(history(&dir, "F1").scope.as_deref(), Some(SCOPE_WINDOW));
            assert_since_days_ago(&srv, 14);
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// The head request's `since` is `days` ago (within two minutes).
        fn assert_since_days_ago(srv: &FakeServer, days: u64) {
            let head = paths_from(srv, 0)
                .into_iter()
                .find(|p| is_head(p, "F1"))
                .unwrap();
            let since = head
                .split("ge%20")
                .nth(1)
                .unwrap()
                .split('&')
                .next()
                .unwrap()
                .to_string();
            let at = now_unix() - days * 86400;
            assert!(
                unix_to_iso8601(at - 120) <= since && since <= unix_to_iso8601(at + 120),
                "{since}"
            );
        }

        #[test]
        fn a_window_shorter_than_14_days_is_the_head() {
            let srv = FakeServer::start_paced(mailbox(&[inbox()], vec![], Duration::ZERO));
            let dir = new_dir("bf-short-window");
            let handle = spawn_in(&srv, &dir, 3, HOUR, None);
            until_complete(&handle, "F1");
            assert_since_days_ago(&srv, 3);
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A store holding F1 as synced before #1871: a delta link, no scope.
        fn seed_legacy(dir: &std::path::Path) {
            let s = store_in(dir);
            s.upsert_folder(&MailFolder {
                id: "F1".into(),
                display_name: "Inbox".into(),
                parent_id: None,
                total_count: BIG,
                unread_count: 0,
                well_known_name: Some("inbox".into()),
            })
            .unwrap();
            s.set_delta_link("F1", &legacy_link("F1")).unwrap();
        }

        #[test]
        fn window_mode_keeps_a_legacy_folder_complete() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 2),
                    ..inbox()
                }],
                vec![],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-legacy-window");
            seed_legacy(&dir);
            let handle = spawn_in(&srv, &dir, 30, HOUR, None);
            wait_for(&handle.evt_rx, |e| {
                matches!(e, SyncEvent::State(SyncState::Idle))
            });
            std::thread::sleep(Duration::from_millis(300));
            let deltas: Vec<String> = paths_from(&srv, 0)
                .into_iter()
                .filter(|p| is_delta(p, "F1"))
                .collect();
            assert_eq!(deltas, [legacy_link("F1")]);
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn whole_mode_backfills_a_legacy_folder_using_its_link_as_head() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 10),
                    ..inbox()
                }],
                vec![],
                Duration::from_millis(100),
            ));
            let dir = new_dir("bf-legacy-whole");
            seed_legacy(&dir);
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, Duration::from_millis(300), None);
            until_complete(&handle, "F1");
            let pages = positions(&srv, |p| is_backfill(p, "F1"));
            let legacy = positions(&srv, |p| p == legacy_link("F1"));
            assert_eq!(pages.len(), 10);
            assert_eq!(
                count(&srv, |p| is_head(p, "F1")),
                0,
                "the legacy link is the head"
            );
            assert!(
                legacy
                    .iter()
                    .any(|t| pages.first() < Some(t) && Some(t) < pages.last()),
                "ticks keep the legacy link fresh during the backfill: {:?}",
                paths_from(&srv, 0)
            );
            assert_eq!(history(&dir, "F1").scope.as_deref(), Some(SCOPE_ALL));
            assert_eq!(
                store_in(&dir).get_delta_link("F1").unwrap(),
                Some(full_link("F1"))
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A store where F1's head ran and its backfill reached page 2.
        fn seed_at_page_2(dir: &std::path::Path) {
            seed_legacy(dir);
            let s = store_in(dir);
            s.set_head_link("F1", &head_link("F1")).unwrap();
            s.set_backfill_progress("F1", &page_link("F1", 2), 50, SCOPE_ALL)
                .unwrap();
        }

        #[test]
        fn an_expired_backfill_link_restarts_from_a_fresh_full_delta() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 3),
                    ..inbox()
                }],
                vec![(route(&page_link("F1", 2), 410, "{}"), Some(1))],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-410");
            seed_at_page_2(&dir);
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let events = until_complete(&handle, "F1");
            let backfill: Vec<String> = paths_from(&srv, 0)
                .into_iter()
                .filter(|p| is_backfill(p, "F1"))
                .collect();
            assert_eq!(backfill.len(), 4, "{backfill:?}");
            assert_eq!(backfill[0], page_link("F1", 2));
            assert!(is_full_first(&backfill[1], "F1"), "{backfill:?}");
            // The count started over.
            assert_eq!(
                progress(&events, "F1"),
                vec![
                    (1, Some(BIG), false),
                    (2, Some(BIG), false),
                    (3, Some(BIG), true)
                ]
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_second_expiry_parks_the_folder() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 3),
                    ..inbox()
                }],
                vec![(route(&page_link("F1", 2), 410, "{}"), None)],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-410-twice");
            seed_at_page_2(&dir);
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            wait_for(
                &handle.evt_rx,
                |e| matches!(e, SyncEvent::Error(m) if m.contains("410")),
            );
            std::thread::sleep(Duration::from_millis(300));
            let backfill: Vec<String> = paths_from(&srv, 0)
                .into_iter()
                .filter(|p| is_backfill(p, "F1"))
                .collect();
            // page 2 (expired), the fresh first call, page 2 (expired): parked.
            assert_eq!(backfill.len(), 3, "{backfill:?}");
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_failing_backfill_page_parks_until_the_next_pass() {
            let bad = r#"{"error":{"code":"ErrorInvalidRequest","message":"bad page"}}"#;
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 3),
                    ..inbox()
                }],
                vec![(route(&page_link("F1", 2), 400, bad), None)],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-park");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            wait_for(
                &handle.evt_rx,
                |e| matches!(e, SyncEvent::Error(m) if m.contains("bad page")),
            );
            std::thread::sleep(Duration::from_millis(300));
            let hits = |srv: &FakeServer| count(srv, |p| p == page_link("F1", 2));
            assert_eq!(hits(&srv), 1, "one request per park");
            handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
            assert!(
                wait_until(ENGINE_WAIT, || hits(&srv) >= 2),
                "a pass unparks it"
            );
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(hits(&srv), 2);
            assert_eq!(history(&dir, "F1").scope.as_deref(), Some(SCOPE_HEAD));
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// Stores `json` (a Graph message) as a row of `folder`.
        fn seed_row(s: &Store, folder: &str, json: &str) {
            let m = Message::from_json(&crate::json::parse(json).unwrap()).unwrap();
            s.upsert_message(folder, &m).unwrap();
        }

        fn row(dir: &std::path::Path, folder: &str, id: &str) -> Option<crate::store::MessageRow> {
            store_in(dir)
                .messages_in_folder(folder, 1000, 0)
                .unwrap()
                .into_iter()
                .find(|m| m.id == id)
        }

        #[test]
        fn a_backfill_page_never_overwrites_a_local_edit() {
            // F1 and Drafts were synced before #1871, so whole mode walks
            // both. R1 was marked read here and X deleted here, each op held
            // by a 5xx; D1's subject was edited by compose (no op yet, so
            // marked unsaved by `update_draft_fields`). The
            // pages carry the server's old R1, X and D1, and a new row.
            let failing = |method: &str, path: &str| Route {
                method: method.into(),
                path_prefix: path.into(),
                status: 500,
                headers: vec![],
                body: "{}".into(),
            };
            let srv = FakeServer::start_paced(mailbox(
                &[
                    Fold {
                        pages: vec![vec![
                            msg("R1", "2026-01-02T00:00:00Z"),
                            msg("X", "2026-01-01T00:00:00Z"),
                            msg("NEWROW", "2025-01-01T00:00:00Z"),
                        ]],
                        ..inbox()
                    },
                    Fold {
                        pages: vec![vec![msg("D1", "2026-01-03T00:00:00Z")]],
                        ..fold("F3", "Drafts", Some("drafts"))
                    },
                ],
                vec![
                    (failing("PATCH", "/me/messages/R1"), None),
                    (failing("DELETE", "/me/messages/X"), None),
                ],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-local-edits");
            seed_legacy(&dir);
            {
                let s = store_in(&dir);
                s.upsert_folder(&MailFolder {
                    id: "F3".into(),
                    display_name: "Drafts".into(),
                    parent_id: None,
                    total_count: BIG,
                    unread_count: 0,
                    well_known_name: Some("drafts".into()),
                })
                .unwrap();
                s.set_delta_link("F3", &legacy_link("F3")).unwrap();
                seed_row(&s, "F1", &msg("R1", "2026-01-02T00:00:00Z"));
                s.set_read("R1", true);
                s.enqueue_op(&OutboxOp::MarkRead {
                    id: "R1".into(),
                    read: true,
                })
                .unwrap();
                s.enqueue_op(&OutboxOp::Delete { id: "X".into() }).unwrap();
                // Compose's edit: unsaved until a push (#1871).
                seed_row(&s, "F3", &msg("D1", "2026-01-03T00:00:00Z"));
                s.update_draft_fields("D1", "local edit", "", "", "", "<p>x</p>")
                    .unwrap();
            }
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let mut done = (false, false);
            let events = wait_for(&handle.evt_rx, |e| {
                done.0 |= is_complete(e, "F1");
                done.1 |= is_complete(e, "F3");
                done.0 && done.1
            });
            assert!(
                row(&dir, "F1", "R1").unwrap().is_read,
                "R1's local read flag kept"
            );
            assert!(
                row(&dir, "F1", "X").is_none(),
                "X stays deleted while its op waits"
            );
            assert!(
                row(&dir, "F1", "NEWROW").is_some(),
                "a missing row is inserted"
            );
            assert_eq!(row(&dir, "F3", "D1").unwrap().subject, "local edit");
            // Every item walked past counts, kept or inserted.
            assert_eq!(progress(&events, "F1").last(), Some(&(3, Some(BIG), true)));
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_backfill_page_corrects_stale_rows() {
            // A1 was inserted by a walk that then expired, and O1 by an
            // earlier sync; both are older than the head's 14 days, so only
            // the walk carries their server state. Both were left stale.
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: vec![
                        vec![msg("A1", "2025-01-01T00:00:00Z")],
                        vec![msg("P2", "2025-01-01T00:00:00Z")],
                        vec![msg("O1", "2024-01-01T00:00:00Z")],
                    ],
                    ..inbox()
                }],
                vec![(route(&page_link("F1", 2), 410, "{}"), Some(1))],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-stale");
            seed_at_page_2(&dir);
            {
                let s = store_in(&dir);
                seed_row(
                    &s,
                    "F1",
                    &full_message_json("A1", "stale", "2025-01-01T00:00:00Z"),
                );
                seed_row(
                    &s,
                    "F1",
                    &full_message_json("O1", "stale", "2024-01-01T00:00:00Z"),
                );
            }
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            until_complete(&handle, "F1");
            assert_eq!(row(&dir, "F1", "A1").unwrap().subject, "A1");
            assert_eq!(row(&dir, "F1", "O1").unwrap().subject, "O1");
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn the_last_round_brings_head_mail_changed_during_the_walk() {
            // H1 came with the head; the server changed it during the walk,
            // whose pages still carried the old one, and no tick came.
            let changed = page(
                &[full_message_json("H1", "changed", "2026-11-05T00:00:00Z")],
                None,
                Some(&head_link("F1")),
            );
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    head: vec![msg("H1", "2026-11-05T00:00:00Z")],
                    pages: vec![
                        vec![msg("H1", "2026-11-05T00:00:00Z")],
                        vec![msg("P2", "2025-01-01T00:00:00Z")],
                    ],
                    ..inbox()
                }],
                vec![(route(&head_link("F1"), 200, &changed), Some(1))],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-last-round");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            until_complete(&handle, "F1");
            assert_eq!(row(&dir, "F1", "H1").unwrap().subject, "changed");
            let pages = positions(&srv, |p| is_backfill(p, "F1"));
            let rounds = positions(&srv, |p| p == head_link("F1"));
            assert_eq!(rounds.len(), 1, "{:?}", paths_from(&srv, 0));
            assert!(rounds[0] > *pages.last().unwrap(), "after the last page");
            assert_eq!(
                store_in(&dir).get_delta_link("F1").unwrap(),
                Some(full_link("F1"))
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_failed_last_round_completes_on_a_later_step() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 2),
                    ..inbox()
                }],
                vec![(throttle(&head_link("F1"), 1), Some(1))],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-last-round-429");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let events = until_complete(&handle, "F1");
            // The last page is asked again after the throttle, and counted once.
            assert_eq!(count(&srv, |p| p == page_link("F1", 2)), 2);
            assert_eq!(progress(&events, "F1").last(), Some(&(2, Some(BIG), true)));
            assert_eq!(
                store_in(&dir).get_delta_link("F1").unwrap(),
                Some(full_link("F1"))
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_reconverge_an_earlier_run_left_keeps_the_cutoff_off() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: vec![
                        vec![msg("RECENT", &unix_to_iso8601(now_unix() - 86400))],
                        vec![msg("ANCIENT", "2020-01-01T00:00:00Z")],
                    ],
                    ..inbox()
                }],
                vec![],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-reconverge-restart");
            // The run before quarantined an op and stopped before the
            // reconverge ended.
            seed_legacy(&dir);
            store_in(&dir).clear_delta_links().unwrap();
            let handle = spawn_in(&srv, &dir, 30, HOUR, None);
            until_complete(&handle, "F1");
            assert!(row(&dir, "F1", "ANCIENT").is_some(), "no cutoff");
            assert!(
                !store_in(&dir).reconverge_pending().unwrap(),
                "and it ended"
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn an_expired_head_link_on_the_last_round_is_replaced_by_the_walk() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 2),
                    ..inbox()
                }],
                vec![(route(&head_link("F1"), 410, "{}"), None)],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-last-round-410");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let events = until_complete(&handle, "F1");
            assert_eq!(progress(&events, "F1").last(), Some(&(2, Some(BIG), true)));
            assert_eq!(count(&srv, |p| p == head_link("F1")), 1);
            assert_eq!(
                store_in(&dir).get_delta_link("F1").unwrap(),
                Some(full_link("F1"))
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn an_expired_legacy_link_is_replaced_when_the_walk_ends() {
            let expired = r#"{"error":{"code":"SyncStateNotFound","message":"gone"}}"#;
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 2),
                    ..inbox()
                }],
                vec![(route(&legacy_link("F1"), 400, expired), None)],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-legacy-expired");
            seed_legacy(&dir);
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            until_complete(&handle, "F1");
            assert_eq!(
                store_in(&dir).get_delta_link("F1").unwrap(),
                Some(full_link("F1"))
            );
            assert_eq!(history(&dir, "F1").scope.as_deref(), Some(SCOPE_ALL));
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_parked_walk_does_not_hold_a_reconverge_open() {
            let bad = r#"{"error":{"code":"ErrorInvalidRequest","message":"bad page"}}"#;
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 3),
                    ..inbox()
                }],
                vec![(route(&page_link("F1", 2), 400, bad), None)],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-reconverge-parked");
            seed_legacy(&dir);
            store_in(&dir).clear_delta_links().unwrap();
            let handle = spawn_in(&srv, &dir, 30, HOUR, None);
            wait_for(
                &handle.evt_rx,
                |e| matches!(e, SyncEvent::Error(m) if m.contains("bad page")),
            );
            assert!(
                wait_until(ENGINE_WAIT, || !store_in(&dir)
                    .reconverge_pending()
                    .unwrap()),
                "the reconverge ended with only a parked walk left"
            );
            assert_eq!(history(&dir, "F1").scope.as_deref(), Some(SCOPE_HEAD));
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_reconverge_keeps_a_compose_edit_in_a_small_drafts_folder() {
            // A quarantine cleared the links: Drafts (one message, so no
            // head) runs its full delta inline, which carries D1 as the
            // server has it, while compose's edit has no op yet.
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    total: 1,
                    pages: vec![vec![msg("D1", "2026-11-01T00:00:00Z")]],
                    ..fold("F3", "Drafts", Some("drafts"))
                }],
                vec![],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-reconverge-draft");
            {
                let s = store_in(&dir);
                s.upsert_folder(&MailFolder {
                    id: "F3".into(),
                    display_name: "Drafts".into(),
                    parent_id: None,
                    total_count: 1,
                    unread_count: 0,
                    well_known_name: Some("drafts".into()),
                })
                .unwrap();
                // Compose's edit: unsaved until a push (#1871).
                seed_row(&s, "F3", &msg("D1", "2026-11-01T00:00:00Z"));
                s.update_draft_fields("D1", "local edit", "", "", "", "<p>x</p>")
                    .unwrap();
                s.clear_delta_links().unwrap();
            }
            let handle = spawn_in(&srv, &dir, 30, HOUR, None);
            until_complete(&handle, "F3");
            assert_eq!(row(&dir, "F3", "D1").unwrap().subject, "local edit");
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// Drafts (F3), listed with `total` messages, as a store holds it.
        fn seed_drafts(s: &Store, total: i64) {
            s.upsert_folder(&MailFolder {
                id: "F3".into(),
                display_name: "Drafts".into(),
                parent_id: None,
                total_count: total,
                unread_count: 0,
                well_known_name: Some("drafts".into()),
            })
            .unwrap();
        }

        fn drafts_with(pages: Vec<Vec<String>>) -> Fold {
            Fold {
                total: 1,
                pages,
                ..fold("F3", "Drafts", Some("drafts"))
            }
        }

        #[test]
        fn a_clean_draft_whose_server_copy_changed_is_merged() {
            // D1 has no unsaved edit (no `update_draft_fields` since its
            // last push); the server's copy changed. Drafts never synced, so
            // its first (full, small) delta carries it.
            let srv = FakeServer::start_paced(mailbox(
                &[drafts_with(vec![vec![msg("D1", "2026-11-01T00:00:00Z")]])],
                vec![],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-clean-draft");
            {
                let s = store_in(&dir);
                seed_drafts(&s, 1);
                seed_row(
                    &s,
                    "F3",
                    &full_message_json("D1", "old", "2026-11-01T00:00:00Z"),
                );
            }
            let handle = spawn_in(&srv, &dir, 30, HOUR, None);
            until_complete(&handle, "F3");
            assert_eq!(row(&dir, "F3", "D1").unwrap().subject, "D1");
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_reconverge_repairs_a_draft_a_quarantined_op_left_wrong() {
            // MarkRead of the draft D1 is refused (400) until quarantined:
            // the store keeps it read, Graph has it unread. The reconverge's
            // fresh delta of Drafts repairs it: the op is gone and D1 holds
            // no unsaved edit.
            let srv = FakeServer::start_paced(mailbox(
                &[drafts_with(vec![vec![msg("D1", "2026-11-01T00:00:00Z")]])],
                vec![(
                    Route {
                        method: "PATCH".into(),
                        path_prefix: "/me/messages/D1".into(),
                        status: 400,
                        headers: vec![],
                        body: "{}".into(),
                    },
                    None,
                )],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-reconverge-draft-op");
            let handle = spawn_in(&srv, &dir, 30, HOUR, None);
            until_complete(&handle, "F3");
            handle
                .cmd_tx
                .send(SyncCommand::MarkRead {
                    id: "D1".into(),
                    read: true,
                })
                .unwrap();
            for _ in 0..4 {
                handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
            }
            wait_for(
                &handle.evt_rx,
                |e| matches!(e, SyncEvent::Error(m) if m.contains("quarantined")),
            );
            assert!(
                row(&dir, "F3", "D1").unwrap().is_read,
                "the dropped op's write"
            );
            handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
            until_complete(&handle, "F3");
            assert!(
                !row(&dir, "F3", "D1").unwrap().is_read,
                "repaired from Graph"
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_window_walk_resumed_in_whole_mode_starts_over() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: vec![
                        vec![
                            msg("RECENT", &unix_to_iso8601(now_unix() - 86400)),
                            msg("OLD1", "2001-01-01T00:00:00Z"),
                        ],
                        vec![msg("P2", "2025-06-01T00:00:00Z")],
                        vec![msg("P3", "2025-06-01T00:00:00Z")],
                    ],
                    ..inbox()
                }],
                vec![(throttle(&page_link("F1", 2), 3600), Some(1))],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-mode-switch");
            let handle = spawn_in(&srv, &dir, 30, HOUR, None);
            assert!(wait_until(ENGINE_WAIT, || count(&srv, |p| p
                == page_link("F1", 2))
                >= 1));
            stop(handle);
            let h = history(&dir, "F1");
            assert_eq!(
                (h.backfill_link, h.backfill_scope.as_deref()),
                (Some(page_link("F1", 2)), Some(SCOPE_WINDOW))
            );
            assert!(row(&dir, "F1", "OLD1").is_none(), "the window skipped it");

            let restart = srv.requests().len();
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, None);
            let events = until_complete(&handle, "F1");
            let after = paths_from(&srv, restart);
            let first_page = after.iter().find(|p| is_backfill(p, "F1")).unwrap();
            assert!(is_full_first(first_page, "F1"), "started over: {after:?}");
            assert!(row(&dir, "F1", "OLD1").is_some());
            assert_eq!(progress(&events, "F1").last(), Some(&(4, Some(BIG), true)));
            assert_eq!(history(&dir, "F1").scope.as_deref(), Some(SCOPE_ALL));
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn the_fast_cycle_keeps_a_folder_fresh_during_a_backfill_and_never_backfills() {
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: pages_of("F1", 15),
                    ..inbox()
                }],
                vec![],
                Duration::from_millis(100),
            ));
            let dir = new_dir("bf-fast");
            let handle = spawn_in(&srv, &dir, WHOLE_MAILBOX, HOUR, Some(FAST));
            handle
                .cmd_tx
                .send(SyncCommand::SetFastFolders {
                    ids: vec!["F1".into()],
                })
                .unwrap();
            until_complete(&handle, "F1");
            let paths = paths_from(&srv, 0);
            let pages = positions(&srv, |p| is_backfill(p, "F1"));
            let rounds = positions(&srv, |p| p == head_link("F1"));
            // The hour-long tick never came: these rounds are fast ticks.
            assert!(
                rounds
                    .iter()
                    .any(|t| pages.first() < Some(t) && Some(t) < pages.last()),
                "a fast tick between two pages: {paths:?}"
            );
            // A fast tick is its folder's round and counts, never a page:
            // each page was asked for once, by the idle steps.
            assert_eq!(pages.len(), 15, "{paths:?}");
            for k in 2..=15 {
                assert_eq!(count(&srv, |p| p == page_link("F1", k)), 1, "page {k}");
            }
            // The round right after the last page is the walk's own last
            // round, before it completes.
            for t in rounds.iter().filter(|t| Some(*t) < pages.last()) {
                assert!(
                    is_counts(&paths[t + 1], "F1"),
                    "a round is followed by its counts: {paths:?}"
                );
            }
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn reconverge_in_window_mode_restores_an_old_message_through_the_backfill() {
            // F1's full delta holds a 2020 message a 30-day window skips. A
            // quarantined op (a Delete Graph keeps refusing) clears every
            // link; the reconverge's backfill ignores the cutoff and stores
            // it. It merges, so it also corrects MID (read here with no op,
            // as a dropped op would leave it), but not MID2, whose MarkRead
            // is still queued (held by a 5xx).
            let mid = unix_to_iso8601(now_unix() - 20 * 86400);
            let srv = FakeServer::start_paced(mailbox(
                &[Fold {
                    pages: vec![
                        vec![msg("RECENT", &unix_to_iso8601(now_unix() - 86400))],
                        vec![
                            msg("MID", &mid),
                            msg("MID2", &mid),
                            msg("ANCIENT", "2020-01-01T00:00:00Z"),
                        ],
                    ],
                    ..inbox()
                }],
                vec![
                    (
                        Route {
                            method: "DELETE".into(),
                            path_prefix: "/me/messages/GONE".into(),
                            status: 404,
                            headers: vec![],
                            body: "{}".into(),
                        },
                        None,
                    ),
                    (
                        Route {
                            method: "PATCH".into(),
                            path_prefix: "/me/messages/MID2".into(),
                            status: 500,
                            headers: vec![],
                            body: "{}".into(),
                        },
                        None,
                    ),
                ],
                Duration::ZERO,
            ));
            let dir = new_dir("bf-reconverge");
            let handle = spawn_in(&srv, &dir, 30, HOUR, None);
            until_complete(&handle, "F1");
            let mut ids = stored_ids(&dir, "F1");
            ids.sort();
            assert_eq!(ids, ["MID", "MID2", "RECENT"]);

            handle
                .cmd_tx
                .send(SyncCommand::Delete { id: "GONE".into() })
                .unwrap();
            for _ in 0..4 {
                handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
            }
            wait_for(
                &handle.evt_rx,
                |e| matches!(e, SyncEvent::Error(m) if m.contains("quarantined")),
            );
            store_in(&dir).set_read("MID", true);
            handle
                .cmd_tx
                .send(SyncCommand::MarkRead {
                    id: "MID2".into(),
                    read: true,
                })
                .unwrap();
            handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
            until_complete(&handle, "F1");
            let mut ids = stored_ids(&dir, "F1");
            ids.sort();
            assert_eq!(ids, ["ANCIENT", "MID", "MID2", "RECENT"]);
            assert!(
                !row(&dir, "F1", "MID").unwrap().is_read,
                "corrected from the server"
            );
            assert!(
                row(&dir, "F1", "MID2").unwrap().is_read,
                "its queued op kept"
            );
            assert_eq!(
                count(&srv, |p| is_head(p, "F1")),
                2,
                "a head again after the quarantine"
            );
            stop(handle);
            let _ = std::fs::remove_dir_all(&dir);
        }

        // --- New mail with its body, prefetch, FetchBody first (#1978) ---

        mod bodies {
            use super::*;

            /// A small folder (one full delta inline, no backfill), so the
            /// next round on its link is the only later delta.
            fn small(f: Fold) -> Fold {
                Fold { total: 5, ..f }
            }

            fn ago(secs: u64) -> String {
                unix_to_iso8601(now_unix() - secs)
            }

            fn body_path(id: &str) -> String {
                format!("/me/messages/{id}?$select=body")
            }

            fn body_route(id: &str) -> Route {
                route(
                    &body_path(id),
                    200,
                    &format!(r#"{{"body":{{"contentType":"text","content":"body of {id}"}}}}"#),
                )
            }

            /// The routes of `ids`' bodies, answered at once, any number
            /// of times.
            fn bodies(ids: &[&str]) -> Vec<(Route, Option<usize>)> {
                ids.iter().map(|id| (body_route(id), None)).collect()
            }

            /// `id`'s next round on its (full delta's) link brings `items`,
            /// once.
            fn next_round(id: &str, items: &[String]) -> (Route, Option<usize>) {
                (
                    route(
                        &full_link(id),
                        200,
                        &page(items, None, Some(&full_link(id))),
                    ),
                    Some(1),
                )
            }

            /// The ids of the body GETs the server saw, in order, with when.
            fn body_gets(srv: &FakeServer) -> Vec<(String, Instant)> {
                srv.requests()
                    .into_iter()
                    .filter(|r| r.method == "GET" && r.path.ends_with("?$select=body"))
                    .map(|r| {
                        let id = r.path
                            ["/me/messages/".len()..r.path.len() - "?$select=body".len()]
                            .to_string();
                        (id, r.at)
                    })
                    .collect()
            }

            fn body_get_ids(srv: &FakeServer) -> Vec<String> {
                body_gets(srv).into_iter().map(|(id, _)| id).collect()
            }

            fn has_body(dir: &std::path::Path, id: &str, format: BodyFormat) -> bool {
                store_in(dir).get_body_for(id, format).unwrap().is_some()
            }

            /// Collects events until a `BodyReady` came for every one of
            /// `ids`, and `also` held for some event when given. The tests
            /// read the store only once the engine stopped: a second
            /// connection opening it while the engine writes finds it busy.
            fn wait_ready(
                handle: &SyncHandle,
                ids: &[&str],
                also: Option<&dyn Fn(&SyncEvent) -> bool>,
            ) -> Vec<SyncEvent> {
                let mut left: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
                let mut seen_also = also.is_none();
                wait_for(&handle.evt_rx, |e| {
                    if let SyncEvent::BodyReady { id } = e {
                        left.retain(|l| l != id);
                    }
                    if let Some(f) = also {
                        seen_also |= f(e);
                    }
                    left.is_empty() && seen_also
                })
            }

            /// The engine with its own body fetches as `options` says,
            /// whole-mailbox mode, an hour's tick.
            fn spawn_with(
                srv: &FakeServer,
                dir: &std::path::Path,
                options: EngineOptions,
            ) -> SyncHandle {
                spawn_with_bases_and_options(
                    dir.join("mail.db"),
                    dir.join("token.bin"),
                    test_cfg(),
                    WHOLE_MAILBOX,
                    srv.base_url.clone(),
                    srv.base_url.clone(),
                    HOUR,
                    options,
                )
            }

            fn is_idle(e: &SyncEvent) -> bool {
                matches!(e, SyncEvent::State(SyncState::Idle))
            }

            /// Sends `Refresh` and returns its pass's events, up to the
            /// `Idle` that settles it.
            fn refresh(handle: &SyncHandle) -> Vec<SyncEvent> {
                handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
                wait_for(&handle.evt_rx, is_idle)
            }

            fn body_ready_ids(events: &[SyncEvent]) -> Vec<String> {
                events
                    .iter()
                    .filter_map(|e| match e {
                        SyncEvent::BodyReady { id } => Some(id.clone()),
                        _ => None,
                    })
                    .collect()
            }

            #[test]
            fn engine_options_default_prefetches_14_days() {
                assert_eq!(EngineOptions::default().prefetch_body_days, 14);
                assert_eq!(PREFETCH_BODY_DAYS, 14);
            }

            #[test]
            fn a_delta_round_that_adds_messages_stores_their_bodies_in_the_same_cycle() {
                // An html engine and text answers: a body stored with the
                // format asked for (`fetched_as` html) satisfies html and not
                // text; one stored without a format would satisfy text only.
                let new: Vec<String> = ["N1", "N2", "N3"]
                    .iter()
                    .zip(1..)
                    .map(|(id, k)| msg(id, &ago(60 * k)))
                    .collect();
                let mut extra = vec![next_round("F1", &new)];
                extra.extend(bodies(&["N1", "N2", "N3"]));
                let srv = FakeServer::start_paced(mailbox(
                    &[small(Fold {
                        pages: vec![vec![msg("OLD", "2020-01-01T00:00:00Z")]],
                        ..inbox()
                    })],
                    extra,
                    Duration::ZERO,
                ));
                let dir = new_dir("bodies-new-mail");
                let handle = spawn_with(
                    &srv,
                    &dir,
                    EngineOptions {
                        body_format: BodyFormat::Html,
                        ..Default::default()
                    },
                );
                wait_for(&handle.evt_rx, is_idle);
                assert!(body_get_ids(&srv).is_empty(), "nothing new yet");

                let events = refresh(&handle);
                let mut ready = body_ready_ids(&events);
                ready.sort();
                assert_eq!(
                    ready,
                    ["N1", "N2", "N3"],
                    "before the pass settled: {events:?}"
                );
                let mut gets = body_get_ids(&srv);
                gets.sort();
                assert_eq!(gets, ["N1", "N2", "N3"]);
                stop(handle);
                for id in ["N1", "N2", "N3"] {
                    assert!(has_body(&dir, id, BodyFormat::Html), "{id}");
                    assert!(
                        !has_body(&dir, id, BodyFormat::Text),
                        "{id} recorded as html"
                    );
                }
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_fetch_body_is_served_between_folders_of_a_slow_pass() {
                let folders: Vec<Fold> = [("F1", "Inbox"), ("F2", "B"), ("F3", "C"), ("F4", "D")]
                    .into_iter()
                    .map(|(id, name)| {
                        small(Fold {
                            pages: vec![vec![msg(&format!("M-{id}"), "2020-01-01T00:00:00Z")]],
                            ..fold(id, name, if id == "F1" { Some("inbox") } else { None })
                        })
                    })
                    .collect();
                let srv = FakeServer::start_paced(mailbox(
                    &folders,
                    bodies(&["M-F1"]),
                    Duration::from_millis(300),
                ));
                let dir = new_dir("bodies-click-in-pass");
                let handle = spawn_with(&srv, &dir, EngineOptions::without_prefetch());
                // The startup pass is under way: some folder's delta is out.
                assert!(wait_until(ENGINE_WAIT, || {
                    paths_from(&srv, 0)
                        .iter()
                        .any(|p| p.contains("/messages/delta?$orderby"))
                }));
                let asked = "M-F1";
                handle
                    .cmd_tx
                    .send(SyncCommand::FetchBody { id: asked.into() })
                    .unwrap();
                let events = wait_for(&handle.evt_rx, is_idle);
                assert_eq!(
                    body_ready_ids(&events),
                    [asked],
                    "before the pass ended: {events:?}"
                );
                let gets = body_gets(&srv);
                assert_eq!(gets.len(), 1);
                let last_delta = srv
                    .requests()
                    .into_iter()
                    .filter(|r| r.path.contains("/messages/delta?$orderby"))
                    .map(|r| r.at)
                    .max()
                    .unwrap();
                assert!(
                    gets[0].1 < last_delta,
                    "the body GET came before the last folder's delta"
                );
                stop(handle);
                let _ = std::fs::remove_dir_all(&dir);
            }

            fn prefetch_waits_out_a_throttle(status: u16) {
                let mut extra = vec![(
                    Route {
                        status,
                        ..throttle(&body_path("R1"), 2)
                    },
                    Some(1),
                )];
                extra.extend(bodies(&["R1", "R2", "R3"]));
                let srv = FakeServer::start_paced(mailbox(
                    &[small(Fold {
                        pages: vec![vec![
                            msg("R1", &ago(3600)),
                            msg("R2", &ago(7200)),
                            msg("R3", &ago(10800)),
                        ]],
                        ..inbox()
                    })],
                    extra,
                    Duration::ZERO,
                ));
                let dir = new_dir(&format!("bodies-throttle-{status}"));
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                wait_ready(&handle, &["R1", "R2", "R3"], None);
                let gets = body_gets(&srv);
                let ids: Vec<&str> = gets.iter().map(|(id, _)| id.as_str()).collect();
                // R1 throttled, then (after the pass its end runs) the batch
                // again, newest first, R1 included.
                assert_eq!(ids, ["R1", "R1", "R2", "R3"]);
                assert!(
                    gets[1].1.duration_since(gets[0].1) >= Duration::from_secs(2),
                    "no body GET inside Retry-After"
                );
                stop(handle);
                for id in ["R1", "R2", "R3"] {
                    assert!(has_body(&dir, id, BodyFormat::Text), "{id}");
                }
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn prefetch_stops_on_a_429_and_resumes_after_retry_after() {
                prefetch_waits_out_a_throttle(429);
            }

            #[test]
            fn prefetch_stops_on_a_503_and_resumes_after_retry_after() {
                prefetch_waits_out_a_throttle(503);
            }

            #[test]
            fn prefetch_body_days_zero_fetches_no_body_on_its_own() {
                let mut extra = vec![next_round("F1", &[msg("N1", &ago(60))])];
                extra.extend(bodies(&["R1", "N1"]));
                let srv = FakeServer::start_paced(mailbox(
                    &[small(Fold {
                        pages: vec![vec![msg("R1", &ago(3600))]],
                        ..inbox()
                    })],
                    extra,
                    Duration::ZERO,
                ));
                let dir = new_dir("bodies-off");
                let handle = spawn_with(
                    &srv,
                    &dir,
                    EngineOptions {
                        prefetch_body_days: 0,
                        ..Default::default()
                    },
                );
                wait_for(&handle.evt_rx, is_idle);
                let events = refresh(&handle);
                assert!(
                    events.iter().any(|e| matches!(
                        e,
                        SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"
                    )),
                    "the round ran"
                );
                assert!(body_ready_ids(&events).is_empty());
                // Idle steps have had their chance to prefetch.
                std::thread::sleep(Duration::from_millis(500));
                assert!(body_get_ids(&srv).is_empty(), "no body fetched on its own");
                handle
                    .cmd_tx
                    .send(SyncCommand::FetchBody { id: "R1".into() })
                    .unwrap();
                wait_for(
                    &handle.evt_rx,
                    |e| matches!(e, SyncEvent::BodyReady { id } if id == "R1"),
                );
                assert_eq!(body_get_ids(&srv), ["R1"]);
                stop(handle);
                assert!(stored_ids(&dir, "F1").contains(&"N1".to_string()));
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_refresh_during_a_back_off_still_syncs_every_folder() {
                // The startup folder list is throttled for an hour: the
                // engine is inside a back-off when F9 comes, and the pass F9
                // runs must not stop at its checkpoints.
                let srv = FakeServer::start_paced(mailbox(
                    &[
                        small(Fold {
                            pages: vec![vec![msg("A", "2020-01-01T00:00:00Z")]],
                            ..inbox()
                        }),
                        small(Fold {
                            pages: vec![vec![msg("B", "2020-01-01T00:00:00Z")]],
                            ..fold("F2", "Projects", None)
                        }),
                    ],
                    vec![(throttle("/me/mailFolders?", 3600), Some(1))],
                    Duration::ZERO,
                ));
                let dir = new_dir("bodies-refresh-in-back-off");
                let handle = spawn_with(&srv, &dir, EngineOptions::without_prefetch());
                wait_for(&handle.evt_rx, is_idle);
                assert!(positions(&srv, |p| is_full_first(p, "F1")).is_empty());
                refresh(&handle);
                assert_eq!(positions(&srv, |p| is_full_first(p, "F1")).len(), 1);
                assert_eq!(positions(&srv, |p| is_full_first(p, "F2")).len(), 1);
                stop(handle);
                assert_eq!(stored_ids(&dir, "F1"), ["A"]);
                assert_eq!(stored_ids(&dir, "F2"), ["B"]);
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn prefetch_fetches_recent_bodies_newest_first_and_skips_drafts_and_old_mail() {
                let srv = FakeServer::start_paced(mailbox(
                    &[
                        small(Fold {
                            pages: vec![vec![
                                msg("R2", &ago(7200)),
                                msg("OLD", &ago(20 * 86400)),
                                msg("R3", &ago(10800)),
                                msg("R1", &ago(3600)),
                            ]],
                            ..inbox()
                        }),
                        small(Fold {
                            pages: vec![vec![msg("D1", &ago(600))]],
                            ..fold("F2", "Drafts", Some("drafts"))
                        }),
                    ],
                    bodies(&["R1", "R2", "R3", "OLD", "D1"]),
                    Duration::ZERO,
                ));
                let dir = new_dir("bodies-prefetch-order");
                // R3's body is already stored as the engine asks for it.
                {
                    let s = store_in(&dir);
                    s.upsert_folder(&MailFolder {
                        id: "F1".into(),
                        display_name: "Inbox".into(),
                        parent_id: None,
                        total_count: 5,
                        unread_count: 0,
                        well_known_name: Some("inbox".into()),
                    })
                    .unwrap();
                    seed_row(&s, "F1", &msg("R3", &ago(10800)));
                    s.put_fetched_body(
                        "R3",
                        &crate::graph::model::Body {
                            content_type: "text".into(),
                            content: "seeded".into(),
                        },
                        BodyFormat::Text,
                    )
                    .unwrap();
                }
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                wait_ready(&handle, &["R1", "R2"], None);
                std::thread::sleep(Duration::from_millis(500));
                assert_eq!(body_get_ids(&srv), ["R1", "R2"]);
                stop(handle);
                assert_eq!(
                    store_in(&dir).get_body("R3").unwrap().unwrap().content,
                    "seeded"
                );
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_failing_prefetch_body_is_not_asked_for_again() {
                let mut extra = vec![
                    (route(&body_path("A"), 404, "{}"), None),
                    (route(&body_path("B"), 400, "{}"), None),
                    (route(&body_path("C"), 200, "not json"), None),
                ];
                extra.extend(bodies(&["OK"]));
                let srv = FakeServer::start_paced(mailbox(
                    &[small(Fold {
                        pages: vec![vec![
                            msg("A", &ago(3600)),
                            msg("B", &ago(7200)),
                            msg("C", &ago(10800)),
                            msg("OK", &ago(14400)),
                        ]],
                        ..inbox()
                    })],
                    extra,
                    Duration::ZERO,
                ));
                let dir = new_dir("bodies-failing");
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                wait_ready(&handle, &["OK"], None);
                // Another pass, and idle steps after it.
                refresh(&handle);
                std::thread::sleep(Duration::from_millis(500));
                assert_eq!(body_get_ids(&srv), ["A", "B", "C", "OK"]);
                stop(handle);
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_clicked_message_in_a_prefetch_batch_is_fetched_once() {
                let mut routes: Vec<Paced> =
                    vec![(body_route("R1"), None, Duration::from_millis(600))];
                routes.extend(mailbox(
                    &[small(Fold {
                        pages: vec![vec![
                            msg("R1", &ago(3600)),
                            msg("R2", &ago(7200)),
                            msg("R3", &ago(10800)),
                        ]],
                        ..inbox()
                    })],
                    bodies(&["R2", "R3"]),
                    Duration::ZERO,
                ));
                let srv = FakeServer::start_paced(routes);
                let dir = new_dir("bodies-click-in-batch");
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                // R1's GET is in flight (slow) when the click on R2 comes.
                assert!(wait_until(ENGINE_WAIT, || body_get_ids(&srv) == ["R1"]));
                handle
                    .cmd_tx
                    .send(SyncCommand::FetchBody { id: "R2".into() })
                    .unwrap();
                wait_ready(&handle, &["R1", "R2", "R3"], None);
                std::thread::sleep(Duration::from_millis(300));
                assert_eq!(body_get_ids(&srv), ["R1", "R2", "R3"]);
                stop(handle);
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_shutdown_during_a_pass_stops_it_after_the_commands_before_it() {
                let folders: Vec<Fold> = [
                    ("F1", "Inbox"),
                    ("F2", "B"),
                    ("F3", "C"),
                    ("F4", "D"),
                    ("F5", "E"),
                ]
                .into_iter()
                .map(|(id, name)| {
                    small(Fold {
                        pages: vec![vec![msg(&format!("M-{id}"), "2020-01-01T00:00:00Z")]],
                        ..fold(id, name, if id == "F1" { Some("inbox") } else { None })
                    })
                })
                .collect();
                let srv =
                    FakeServer::start_paced(mailbox(&folders, vec![], Duration::from_millis(400)));
                let dir = new_dir("bodies-shutdown");
                let handle = spawn_with(&srv, &dir, EngineOptions::without_prefetch());
                // The first folder's delta is out.
                assert!(wait_until(ENGINE_WAIT, || {
                    !positions(&srv, |p| p.contains("/messages/delta?$orderby")).is_empty()
                }));
                let first = paths_from(&srv, 0)
                    .into_iter()
                    .find(|p| p.contains("/messages/delta?$orderby"))
                    .unwrap();
                let folder = first
                    .trim_start_matches("/me/mailFolders/")
                    .split('/')
                    .next()
                    .unwrap()
                    .to_string();
                let first_msg = format!("M-{folder}");
                handle
                    .cmd_tx
                    .send(SyncCommand::MarkRead {
                        id: first_msg.clone(),
                        read: true,
                    })
                    .unwrap();
                handle.cmd_tx.send(SyncCommand::Shutdown).unwrap();
                stop(handle);
                let deltas = positions(&srv, |p| p.contains("/messages/delta?$orderby"));
                assert!(deltas.len() < 5, "the pass stopped early: {deltas:?}");
                // The MarkRead sent before the Shutdown still ran.
                let row = row(&dir, &folder, &first_msg).expect("the first folder's message");
                assert!(row.is_read);
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn prefetch_runs_before_a_whole_mailbox_backfill_and_both_finish() {
                let srv = FakeServer::start_paced(mailbox(
                    &[Fold {
                        head: vec![msg("R1", &ago(3600)), msg("R2", &ago(7200))],
                        pages: vec![
                            vec![msg("R1", &ago(3600)), msg("R2", &ago(7200))],
                            vec![msg("OLD", "2020-01-01T00:00:00Z")],
                        ],
                        ..inbox()
                    }],
                    bodies(&["R1", "R2", "OLD"]),
                    Duration::ZERO,
                ));
                let dir = new_dir("bodies-with-backfill");
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                wait_ready(&handle, &["R1", "R2"], Some(&|e| is_complete(e, "F1")));
                assert_eq!(body_get_ids(&srv), ["R1", "R2"]);
                // Both bodies came before the backfill's first page.
                let paths = paths_from(&srv, 0);
                let first_page = positions(&srv, |p| is_full_first(p, "F1"))[0];
                let body_at: Vec<usize> = paths
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.ends_with("?$select=body"))
                    .map(|(i, _)| i)
                    .collect();
                assert!(body_at.iter().all(|&i| i < first_page), "{paths:?}");
                stop(handle);
                let mut ids = stored_ids(&dir, "F1");
                ids.sort();
                assert_eq!(ids, ["OLD", "R1", "R2"]);
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_failing_store_parks_the_prefetch_until_the_next_pass() {
                // R1's body is slow; while it is in flight a trigger makes
                // every body write fail. The prefetch parks: R2 is not
                // fetched, and R1 is not skipped. The next pass, with the
                // store working again, fetches both.
                let mut routes: Vec<Paced> =
                    vec![(body_route("R1"), None, Duration::from_millis(400))];
                routes.extend(mailbox(
                    &[small(Fold {
                        pages: vec![vec![msg("R1", &ago(3600)), msg("R2", &ago(7200))]],
                        ..inbox()
                    })],
                    bodies(&["R2"]),
                    Duration::ZERO,
                ));
                let srv = FakeServer::start_paced(routes);
                let dir = new_dir("bodies-failing-store");
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                assert!(wait_until(ENGINE_WAIT, || body_get_ids(&srv) == ["R1"]));
                let conn = rusqlite::Connection::open(dir.join("mail.db")).unwrap();
                conn.execute_batch(
                    "CREATE TRIGGER fail_bodies BEFORE INSERT ON bodies
                     BEGIN SELECT RAISE(ABORT, 'store failure'); END;",
                )
                .unwrap();
                // The response lands and the write fails; idle steps follow.
                std::thread::sleep(Duration::from_millis(1000));
                assert_eq!(body_get_ids(&srv), ["R1"], "parked: no R2, no retry loop");
                conn.execute_batch("DROP TRIGGER fail_bodies").unwrap();
                drop(conn);
                refresh(&handle);
                wait_ready(&handle, &["R1", "R2"], None);
                assert_eq!(body_get_ids(&srv), ["R1", "R1", "R2"]);
                stop(handle);
                assert!(has_body(&dir, "R1", BodyFormat::Text));
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_5xx_on_a_prefetch_body_parks_until_the_next_pass_and_is_fetched_then() {
                // A Graph 500 is a failing service, not a bad id: the
                // prefetch parks (R2 waits, no loop), R1 is not skipped, and
                // the next pass fetches both.
                let mut extra = vec![(route(&body_path("R1"), 500, "{}"), Some(1))];
                extra.extend(bodies(&["R1", "R2"]));
                let srv = FakeServer::start_paced(mailbox(
                    &[small(Fold {
                        pages: vec![vec![msg("R1", &ago(3600)), msg("R2", &ago(7200))]],
                        ..inbox()
                    })],
                    extra,
                    Duration::ZERO,
                ));
                let dir = new_dir("bodies-5xx");
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                assert!(wait_until(ENGINE_WAIT, || body_get_ids(&srv) == ["R1"]));
                std::thread::sleep(Duration::from_millis(500));
                assert_eq!(body_get_ids(&srv), ["R1"], "parked: no R2, no retry loop");
                refresh(&handle);
                wait_ready(&handle, &["R1", "R2"], None);
                assert_eq!(body_get_ids(&srv), ["R1", "R1", "R2"]);
                stop(handle);
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_command_sent_during_a_prefetch_batch_runs_before_the_rest_of_it() {
                // R1's body is slow; a MarkRead sent meanwhile is kept by the
                // checkpoint before R2, which ends the batch: its PATCH goes
                // out before R2's and R3's GETs.
                let mut routes: Vec<Paced> =
                    vec![(body_route("R1"), None, Duration::from_millis(500))];
                routes.extend(mailbox(
                    &[small(Fold {
                        pages: vec![vec![
                            msg("R1", &ago(3600)),
                            msg("R2", &ago(7200)),
                            msg("R3", &ago(10800)),
                        ]],
                        ..inbox()
                    })],
                    bodies(&["R2", "R3"]),
                    Duration::ZERO,
                ));
                let srv = FakeServer::start_paced(routes);
                let dir = new_dir("bodies-command-in-batch");
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                assert!(wait_until(ENGINE_WAIT, || body_get_ids(&srv) == ["R1"]));
                handle
                    .cmd_tx
                    .send(SyncCommand::MarkRead {
                        id: "R3".into(),
                        read: true,
                    })
                    .unwrap();
                wait_ready(&handle, &["R1", "R2", "R3"], None);
                let order: Vec<String> = srv
                    .requests()
                    .into_iter()
                    .filter(|r| r.path.ends_with("?$select=body") || r.method == "PATCH")
                    .map(|r| format!("{} {}", r.method, r.path))
                    .collect();
                assert_eq!(
                    order,
                    [
                        "GET /me/messages/R1?$select=body",
                        "PATCH /me/messages/R3",
                        "GET /me/messages/R2?$select=body",
                        "GET /me/messages/R3?$select=body",
                    ]
                );
                stop(handle);
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn an_oversize_prefetch_body_is_skipped_without_going_offline() {
                // A body over `BODY_MAX` is one the client refuses (`Parse`),
                // not a lost connection: asked for once, no Offline, and the
                // next id is fetched.
                let big = format!(
                    r#"{{"body":{{"contentType":"text","content":"{}"}}}}"#,
                    "x".repeat(crate::graph::client::BODY_MAX)
                );
                let mut extra = vec![(route(&body_path("R1"), 200, &big), None)];
                extra.extend(bodies(&["R2"]));
                let srv = FakeServer::start_paced(mailbox(
                    &[small(Fold {
                        pages: vec![vec![msg("R1", &ago(3600)), msg("R2", &ago(7200))]],
                        ..inbox()
                    })],
                    extra,
                    Duration::ZERO,
                ));
                let dir = new_dir("bodies-oversize");
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                let mut events = wait_ready(&handle, &["R2"], None);
                events.extend(refresh(&handle));
                std::thread::sleep(Duration::from_millis(300));
                assert_eq!(body_get_ids(&srv), ["R1", "R2"]);
                assert!(
                    !events
                        .iter()
                        .any(|e| matches!(e, SyncEvent::State(SyncState::Offline))),
                    "{events:?}"
                );
                stop(handle);
                assert!(!has_body(&dir, "R1", BodyFormat::Text));
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn parked_body_fetches_hold_new_mail_rounds_until_the_next_pass() {
                // R1's body answers 500 once: the fetches park. A SyncFolder
                // round that brings N1 then fetches no body (a single-folder
                // sync is not a pass); the next pass ends the park, and the
                // prefetch takes both, newest first.
                let mut extra = vec![
                    next_round("F1", &[msg("N1", &ago(60))]),
                    (route(&body_path("R1"), 500, "{}"), Some(1)),
                ];
                extra.extend(bodies(&["R1", "N1"]));
                let srv = FakeServer::start_paced(mailbox(
                    &[small(Fold {
                        pages: vec![vec![msg("R1", &ago(3600))]],
                        ..inbox()
                    })],
                    extra,
                    Duration::ZERO,
                ));
                let dir = new_dir("bodies-parked-new-mail");
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                // The startup pass's events first, so the wait below sees
                // the SyncFolder's own.
                wait_for(&handle.evt_rx, is_idle);
                assert!(wait_until(ENGINE_WAIT, || body_get_ids(&srv) == ["R1"]));
                handle
                    .cmd_tx
                    .send(SyncCommand::SyncFolder { id: "F1".into() })
                    .unwrap();
                let events = wait_for(&handle.evt_rx, is_idle);
                assert!(
                    events.iter().any(|e| matches!(
                        e,
                        SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"
                    )),
                    "the SyncFolder round ran: {events:?}"
                );
                assert_eq!(body_get_ids(&srv), ["R1"], "parked: no new-mail body");
                refresh(&handle);
                wait_ready(&handle, &["R1", "N1"], None);
                assert_eq!(body_get_ids(&srv), ["R1", "N1", "R1"]);
                stop(handle);
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_command_sent_during_new_mail_bodies_ends_them() {
                // A round brings N1..N3; N1's body is slow, and a MarkRead
                // comes meanwhile. The checkpoint before N2 keeps it and ends
                // the new-mail bodies: the pass finishes, the MarkRead's
                // PATCH goes out, and the prefetch takes N2 and N3.
                let new: Vec<String> = ["N1", "N2", "N3"]
                    .iter()
                    .zip(1..)
                    .map(|(id, k)| msg(id, &ago(60 * k)))
                    .collect();
                let mut routes: Vec<Paced> =
                    vec![(body_route("N1"), None, Duration::from_millis(500))];
                let mut extra = vec![next_round("F1", &new)];
                extra.extend(bodies(&["N2", "N3"]));
                routes.extend(mailbox(
                    &[small(Fold {
                        pages: vec![vec![msg("OLD", "2020-01-01T00:00:00Z")]],
                        ..inbox()
                    })],
                    extra,
                    Duration::ZERO,
                ));
                let srv = FakeServer::start_paced(routes);
                let dir = new_dir("bodies-command-in-new-mail");
                let handle = spawn_with(&srv, &dir, EngineOptions::default());
                wait_for(&handle.evt_rx, is_idle);
                handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
                assert!(wait_until(ENGINE_WAIT, || body_get_ids(&srv) == ["N1"]));
                handle
                    .cmd_tx
                    .send(SyncCommand::MarkRead {
                        id: "N3".into(),
                        read: true,
                    })
                    .unwrap();
                wait_ready(&handle, &["N1", "N2", "N3"], None);
                let order: Vec<String> = srv
                    .requests()
                    .into_iter()
                    .filter(|r| r.path.ends_with("?$select=body") || r.method == "PATCH")
                    .map(|r| format!("{} {}", r.method, r.path))
                    .collect();
                assert_eq!(
                    order,
                    [
                        "GET /me/messages/N1?$select=body",
                        "PATCH /me/messages/N3",
                        "GET /me/messages/N2?$select=body",
                        "GET /me/messages/N3?$select=body",
                    ]
                );
                stop(handle);
                let _ = std::fs::remove_dir_all(&dir);
            }

            /// Sends `Shutdown` and returns every event until the engine's
            /// thread ends.
            fn shut_down(handle: SyncHandle) -> Vec<SyncEvent> {
                handle.cmd_tx.send(SyncCommand::Shutdown).unwrap();
                let deadline = Instant::now() + ENGINE_WAIT;
                let mut seen = Vec::new();
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match handle.evt_rx.recv_timeout(left) {
                        Ok(e) => seen.push(e),
                        Err(RecvTimeoutError::Disconnected) => return seen,
                        Err(RecvTimeoutError::Timeout) => panic!("engine did not stop"),
                    }
                }
            }

            #[test]
            fn a_backfill_whose_last_round_is_stopped_between_pages_is_not_complete() {
                // The walk's only page brings the deltaLink; the last round
                // on the folder's own (head) link has two pages, the first
                // slow. A Shutdown during it stops the round at the
                // checkpoint between its pages: the backfill must not be
                // recorded complete, and the head's link stays.
                let second = "/me/mailFolders/F1/messages/delta?token=HN-F1";
                let mut routes: Vec<Paced> = vec![
                    (
                        route(&head_link("F1"), 200, &page(&[], Some(second), None)),
                        Some(1),
                        Duration::from_millis(600),
                    ),
                    (
                        route(second, 200, &page(&[], None, Some(&head_link("F1")))),
                        None,
                        Duration::ZERO,
                    ),
                ];
                routes.extend(mailbox(
                    &[Fold {
                        pages: vec![vec![msg("A", "2020-01-01T00:00:00Z")]],
                        ..inbox()
                    }],
                    vec![],
                    Duration::ZERO,
                ));
                let srv = FakeServer::start_paced(routes);
                let dir = new_dir("bodies-stopped-last-round");
                let handle = spawn_with(&srv, &dir, EngineOptions::without_prefetch());
                assert!(wait_until(ENGINE_WAIT, || {
                    paths_from(&srv, 0).iter().any(|p| p == &head_link("F1"))
                }));
                let events = shut_down(handle);
                assert!(
                    !events.iter().any(|e| is_complete(e, "F1")),
                    "not complete: {events:?}"
                );
                assert!(!paths_from(&srv, 0).iter().any(|p| p == second));
                assert_eq!(
                    store_in(&dir).get_delta_link("F1").unwrap(),
                    Some(head_link("F1"))
                );
                assert_eq!(history(&dir, "F1").scope.as_deref(), Some(SCOPE_HEAD));
                let _ = std::fs::remove_dir_all(&dir);
            }

            #[test]
            fn a_round_on_a_stored_link_stopped_between_pages_stores_no_link() {
                // After startup F1 holds its full delta's link. The next
                // round has two pages, the first slow and bringing N1; a
                // Shutdown during it stops the round between its pages: N1
                // is merged and shown, the link is not replaced.
                let second = "/me/mailFolders/F1/messages/delta?token=NX-F1";
                let mut routes: Vec<Paced> = vec![
                    (
                        route(
                            &full_link("F1"),
                            200,
                            &page(&[msg("N1", "2020-01-02T00:00:00Z")], Some(second), None),
                        ),
                        Some(1),
                        Duration::from_millis(600),
                    ),
                    (
                        route(
                            second,
                            200,
                            &page(
                                &[],
                                None,
                                Some("/me/mailFolders/F1/messages/delta?token=NEW"),
                            ),
                        ),
                        None,
                        Duration::ZERO,
                    ),
                ];
                routes.extend(mailbox(
                    &[small(Fold {
                        pages: vec![vec![msg("A", "2020-01-01T00:00:00Z")]],
                        ..inbox()
                    })],
                    vec![],
                    Duration::ZERO,
                ));
                let srv = FakeServer::start_paced(routes);
                let dir = new_dir("bodies-stopped-round");
                let handle = spawn_with(&srv, &dir, EngineOptions::without_prefetch());
                wait_for(&handle.evt_rx, is_idle);
                handle.cmd_tx.send(SyncCommand::Refresh).unwrap();
                assert!(wait_until(ENGINE_WAIT, || {
                    paths_from(&srv, 0).iter().any(|p| p == &full_link("F1"))
                }));
                let events = shut_down(handle);
                assert!(
                    events.iter().any(|e| matches!(
                        e,
                        SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1"
                    )),
                    "{events:?}"
                );
                assert!(!paths_from(&srv, 0).iter().any(|p| p == second));
                assert_eq!(
                    store_in(&dir).get_delta_link("F1").unwrap(),
                    Some(full_link("F1"))
                );
                let mut ids = stored_ids(&dir, "F1");
                ids.sort();
                assert_eq!(ids, ["A", "N1"]);
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}
