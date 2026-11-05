//! The background sync engine: one `std::thread` that owns a [`Store`] and
//! talks to Microsoft Graph, driven by (and reporting to) the UI over
//! `std::sync::mpsc` channels — no async runtime.
//!
//! [`spawn`] launches the thread and hands back a [`SyncHandle`]
//! (`cmd_tx` down, `evt_rx` up). The thread loads the cached token, does an
//! initial folder + delta backfill, then blocks on
//! `cmd_rx.recv_timeout(tick)`: each tick (or an explicit `Refresh`) re-runs
//! per-folder delta sync and drains the outbox; mutation commands write the
//! store optimistically, enqueue an outbox op, and drain. Graph calls go
//! through [`Engine::with_auth`], which refreshes the token once on a 401 and
//! retries. Throttling (`Retry-After`) is honored by rescheduling; transport
//! failures drop to `Offline` with exponential back-off while the UI keeps
//! reading the store.
//!
//! An opt-in fast cycle (`EngineOptions::fast_tick`, #1885) syncs only the
//! folders a caller names (`SyncCommand::SetFastFolders`) between two normal
//! ticks, and `SyncCommand::SyncFolder` syncs one folder on demand.
//!
//! All Graph/auth base URLs are injectable (see `spawn_with_bases`) so tests
//! drive the whole engine against the in-process fake server with no network.
//! Secrets (access/refresh tokens) are never logged.

use crate::auth::{self, AuthConfig, TokenSet};
use crate::graph::client::{DeltaCursor, GraphClient, GraphError, RsvpKind};
pub use crate::graph::model::BodyFormat;
use crate::graph::model::{AutomaticReplies, DeltaItem, Message, ScheduleEntry};
use crate::store::{NewAttendee, NewEvent, OutboxOp, Store, StoreError};
use crate::sync::outbox::apply_op;
use crate::tokencache;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A command sent from the UI down to the sync thread.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncCommand {
    /// Re-enumerate folders, re-run every folder's delta, and drain the outbox.
    Refresh,
    /// Sync one folder now (#1885, "Update Folder"): drain the outbox, run
    /// that folder's delta (its initial one, with the `backfill_days`
    /// cutoff, when it never synced), and re-read its counts
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
    /// does, skipping one that never synced (the normal pass backfills it).
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
    /// Hand a draft to Graph for delivery: optimistically mark it sent and
    /// (if the Sent folder has synced) move it there locally, then enqueue
    /// [`crate::store::OutboxOp::SendDraft`] and drain. Emits
    /// [`SyncEvent::Sent`] once the drain actually delivers it.
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
/// (4xx other than 401/429) error, so one bad op can't block the queue.
const MAX_OP_ATTEMPTS: i64 = 5;
/// At most this many header-less rows of a folder are fetched from Graph per
/// sync pass (`Engine::repair_headerless`, #1899); the rest wait for the next.
const REPAIR_LIMIT: usize = 50;

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
/// the channels to drive it. `tick` is how long the thread waits for a
/// command before running a periodic sync pass on its own (an explicit
/// [`SyncCommand::Refresh`] also runs one immediately, regardless of how
/// much of the tick has elapsed) — lookxy passes its `Config::refresh_secs`
/// here, so the configured interval genuinely governs how often the engine
/// syncs on its own.
///
/// Runs with [`EngineOptions::default`] (plain-text bodies); see
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
/// in-process fake server.
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
        EngineOptions::default(),
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
    let mut engine = Engine {
        store,
        token: None,
        config,
        evt_tx,
        state: SyncState::Idle,
        next_retry: None,
        backoff: BACKOFF_MIN,
        backfill_done: false,
        reconverge_pending: false,
        fast_folders: Vec::new(),
        next_full: Instant::now(),
        next_fast: Instant::now(),
        hold_full: false,
        op_retry: None,
        op_backoff: BACKOFF_MIN,
    };
    engine.startup();
    engine.main_loop(cmd_rx);
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
    /// Whether an initial folder enumeration has ever completed successfully.
    /// False at startup, after a failed first backfill (e.g. launched
    /// offline), and after a quarantine reconverge — while false, ticks run a
    /// FULL pass (re-enumerate folders) so the client can recover instead of
    /// running incremental deltas over an empty/stale folder set forever.
    backfill_done: bool,
    /// Set when an outbox op is quarantined: the next full backfill pass must
    /// re-fetch *all* current server messages regardless of age (ignore the
    /// `backfill_days` cutoff) so it re-adds anything the dropped op wrongly
    /// removed locally — including messages that have since aged past the
    /// sliding window. Cleared once that reconverge pass completes.
    reconverge_pending: bool,
    /// The fast cycle's folders (`SyncCommand::SetFastFolders`), in order.
    fast_folders: Vec<String>,
    /// With a fast tick only: when the normal pass is next due.
    next_full: Instant,
    /// With a fast tick only: when the next fast tick is due.
    next_fast: Instant,
    /// With a fast tick only: the full pass that last ran (normal, startup,
    /// `Refresh`, sign-in) ran to its end and still
    /// left the folders to re-enumerate (the folder list failed
    /// non-fatally: a 403, a 5xx, a parse error), so the fast deadlines do
    /// not repeat it before `next_full`; a quarantine since
    /// (`reconverge_pending`) still promotes one.
    hold_full: bool,
    /// When set, a drain sends no op before this instant: the first queued
    /// op got a 5xx (#1885). Only the outbox waits; the folders still sync.
    op_retry: Option<Instant>,
    /// The outbox's own back-off after a 5xx, doubled per consecutive 5xx
    /// (`BACKOFF_MIN`..`BACKOFF_MAX`), reset when an op goes through.
    op_backoff: Duration,
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
    fn main_loop(&mut self, cmd_rx: Receiver<SyncCommand>) {
        // The startup pass schedules the deadlines like any other.
        self.after_pass_if_fast();
        loop {
            match cmd_rx.recv_timeout(self.recv_timeout()) {
                Ok(SyncCommand::Shutdown) => return,
                Ok(cmd) => self.handle_command(cmd),
                Err(RecvTimeoutError::Timeout) => match self.fast_tick() {
                    None => self.on_tick(),
                    Some(fast) => self.on_deadline(fast),
                },
                // The UI dropped its `SyncHandle`: no one is listening anymore.
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// How long to block for the next command: until the back-off deadline if
    /// one is pending, otherwise the configured tick, and no later than a
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
            None => self.config.tick,
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

    /// Whether the next pass must re-enumerate folders: the initial
    /// enumeration never completed (launched offline, or a quarantine
    /// reconverge reset it), or no folder is stored.
    fn needs_full_pass(&self) -> bool {
        !self.backfill_done || self.store.folders().map(|f| f.is_empty()).unwrap_or(true)
    }

    /// Whether the full pass that just ran (normal, startup, `Refresh`,
    /// sign-in) ran to its end and still left the folders to re-enumerate:
    /// not cut short by a lost sign-in or a back-off, so its folder list
    /// failed non-fatally.
    fn full_pass_held(&self) -> bool {
        let cut_short = self.token.is_none() || self.next_retry.is_some_and(|t| t > Instant::now());
        !cut_short && self.needs_full_pass()
    }

    /// A deadline of the fast-tick schedule elapsed. Inside a back-off
    /// window nothing runs. The normal pass runs when due: its deadline, or
    /// a pass that must re-enumerate folders, so a quarantine or an offline
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
    /// otherwise re-run every folder's delta and drain the outbox. When the
    /// initial backfill never completed (launched offline, or a quarantine
    /// reconverge reset it) — or the folder set is empty — run a FULL pass so
    /// the client keeps re-attempting folder enumeration under back-off rather
    /// than idling forever on an empty/stale folder set.
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
        if self.token.is_some() && self.next_retry.is_none() {
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
            SyncCommand::FetchBody { id } => self.fetch_body(&id),
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
            SyncCommand::FetchInlineImage {
                message_id,
                attachment_id,
                content_id,
            } => self.fetch_inline_image(&message_id, &attachment_id, &content_id),
            SyncCommand::SaveDraft { id } => {
                self.enqueue_and_drain(OutboxOp::SaveDraft { id });
            }
            SyncCommand::SendDraft { id } => {
                // Optimistic local write, same spirit as MarkRead/SetFlag/
                // Move/Delete: mark the row no longer a draft, and — if the
                // Sent folder has synced — re-file it there right away so
                // the UI reflects "sent" before the outbox drain actually
                // reaches Graph. `id` is whatever the caller currently
                // addresses this draft by (a `local:` id if it was never
                // pushed, else its Graph id); `apply_op` resolves/reconciles
                // the Graph side, this only touches the local row.
                self.store.mark_sent(&id);
                if let Some(sent_id) = self.sent_items_folder_id() {
                    let _ = self.store.move_message(&id, &sent_id);
                }
                self.enqueue_and_drain(OutboxOp::SendDraft { id });
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
            if !self.sync_folder_delta(&f.id) {
                return;
            }
        }
        // Every folder's delta just completed without a hard failure, so any
        // pending reconverge (upsert-everything, cutoff ignored) is satisfied.
        // Clear it before draining: if the drain quarantines a fresh op it will
        // re-arm reconverge for the next pass.
        self.reconverge_pending = false;
        // A drain that a throttle, an offline error or a lost sign-in cut
        // short keeps its back-off: the pass stops here, as after a folder.
        // An op deferred by a 5xx does not stop the pass; it waits for the
        // next drain.
        if !self.drain_outbox().carry_on() {
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
            if !self.refresh_folder(id) {
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
                if listing.complete && !listing.folders.is_empty() {
                    let keep: Vec<String> = listing.folders.iter().map(|f| f.id.clone()).collect();
                    match self.store.retain_folders(&keep) {
                        Ok(removed) => changed |= removed > 0,
                        Err(e) => self.emit(SyncEvent::Error(e.to_string())),
                    }
                }
                // The initial enumeration has now succeeded at least once, so
                // ticks can safely drop to incremental deltas.
                self.backfill_done = true;
                if full || changed {
                    self.emit(SyncEvent::FoldersUpdated);
                }
                true
            }
            Err(e) => !self.react(e),
        }
    }

    /// Delta-sync one folder: page through from the stored `deltaLink` (or a
    /// fresh folder cursor on first sync), upserting/deleting messages and
    /// saving the new `deltaLink`. On the initial backfill, messages older
    /// than the `backfill_days` cutoff are skipped — except during a
    /// quarantine reconverge (`reconverge_pending`), which upserts every item
    /// regardless of age so a wrongly-removed message is restored even if it
    /// has aged past the sliding window. Returns `false` if the whole pass
    /// should abort.
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
        let is_backfill = stored.is_none();
        let cutoff = if is_backfill && !self.reconverge_pending {
            self.cutoff_iso()
        } else {
            None
        };
        let mut cursor = match stored {
            Some(link) => DeltaCursor::Link(link),
            None => DeltaCursor::Folder(folder_id.to_string()),
        };

        loop {
            let page = match self.with_auth(|c| c.delta(cursor.clone())) {
                Ok(p) => p,
                // `react` returns true when the pass must abort; a non-fatal
                // error (a 4xx/parse on this folder) is surfaced there and we
                // stop this folder but keep the pass going.
                Err(e) => return !self.react(e),
            };
            for item in &page.items {
                match item {
                    DeltaItem::Upsert(m, fields) => {
                        if let Some(cut) = &cutoff {
                            if !m.received.is_empty() && m.received.as_str() < cut.as_str() {
                                continue;
                            }
                        }
                        let _ = self.store.merge_message(folder_id, m, fields);
                    }
                    // A `@removed` entry can carry an empty id; deleting "" is a
                    // safe no-op in the store.
                    DeltaItem::Delete(id) => {
                        let _ = self.store.delete_message(id);
                    }
                }
            }
            if let Some(next) = page.next_link {
                cursor = DeltaCursor::Link(next);
                continue;
            }
            if let Some(delta) = page.delta_link {
                let _ = self.store.set_delta_link(folder_id, &delta);
            }
            break;
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
        !repair.abort
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
    /// the rest of the queue proceeds; a 5xx keeps the op and stops the
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
            match self.apply_op_with_retry(&row.op) {
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
    fn apply_op_with_retry(&mut self, op: &OutboxOp) -> Result<(), GraphError> {
        let client = self.client();
        match apply_op(&client, &self.store, op) {
            Err(GraphError::Unauthorized) => {
                if self.try_refresh() {
                    let client = self.client();
                    apply_op(&client, &self.store, op)
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
    fn fetch_body(&mut self, id: &str) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        let format = self.config.options.body_format;
        match self.with_auth(|c| c.get_body(id, format == BodyFormat::Text)) {
            Ok(body) => {
                let _ = self.store.put_fetched_body(id, &body, format);
                self.emit(SyncEvent::BodyReady { id: id.to_string() });
            }
            Err(e) => {
                self.react(e);
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
    /// rejected it). Token values are never logged.
    fn try_refresh(&mut self) -> bool {
        let Some(refresh_token) = self.token.as_ref().map(|t| t.refresh_token.clone()) else {
            return false;
        };
        match auth::refresh(&self.config.cfg, &self.config.auth_base, &refresh_token) {
            Ok(t) => {
                let _ = tokencache::save(&self.config.token_path, &t);
                self.token = Some(t);
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

    /// Drop to signed-out: clear the token and announce `SignInRequired`.
    fn enter_signin(&mut self) {
        self.token = None;
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
/// the provider's own `error` value, or "timed out"), safe to surface via
/// `SyncEvent::Error`.
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

    if let Some(err) = params.iter().find(|(k, _)| k == "error").map(|(_, v)| v) {
        write_html_response(stream, SIGNIN_ERROR_HTML);
        return Err(format!("authorization denied: {err}"));
    }
    let state = params.iter().find(|(k, _)| k == "state").map(|(_, v)| v);
    if state.map(String::as_str) != Some(expected_state) {
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
            ..Default::default()
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
            EngineOptions::default(),
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
        let request =
            format!("GET /?code=THECODE&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n");
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
    /// startup pass syncs. With `send_fails_once`, the first send answers 503.
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
                ..Default::default()
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
    /// 2 s, so a full pass is cut short after the folder list and F1.
    fn f2_initial_delta_throttled_routes() -> Vec<(Route, Option<usize>)> {
        let mut routes = vec![(
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F2/messages/delta?$select".into(),
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
                ..Default::default()
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
        assert!(
            store
                .messages_in_folder("DRAFTS1", 50, 0)
                .unwrap()
                .is_empty()
        );

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
        let errors: Vec<SyncEvent> = handle
            .evt_rx
            .try_iter()
            .filter(|e| matches!(e, SyncEvent::Error(_)))
            .collect();
        assert!(errors.is_empty(), "{errors:?}");

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
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
}
