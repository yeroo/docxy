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
//! All Graph/auth base URLs are injectable (see `spawn_with_bases`) so tests
//! drive the whole engine against the in-process fake server with no network.
//! Secrets (access/refresh tokens) are never logged.

use crate::auth::{self, AuthConfig, TokenSet};
use crate::graph::client::{DeltaCursor, GraphClient, GraphError, RsvpKind};
use crate::graph::model::{AutomaticReplies, DeltaItem, Message};
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
    /// Start the interactive browser sign-in flow (see [`Engine::sign_in`]).
    SignIn,
    /// Mark a message read/unread (optimistic local write + queued Graph op).
    MarkRead { id: String, read: bool },
    /// Flag/unflag a message (optimistic local write + queued Graph op).
    SetFlag { id: String, flagged: bool },
    /// Move a message to another folder: optimistic local re-file plus a queued
    /// Graph op. Graph mints a new id on move, which the next delta reconciles
    /// (old id `@removed`, new id added).
    Move { id: String, dest: String },
    /// Delete a message (optimistic local delete + queued Graph op).
    Delete { id: String },
    /// Fetch and store a message body, then emit [`SyncEvent::BodyReady`].
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
    RespondMeeting { message_id: String, kind: RsvpKind },
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
    /// [`crate::store::OutboxOp::SaveDraft`] and drain. A `local:` id is
    /// created on Graph and reconciled (see `sync::outbox::apply_op`); an
    /// already-synced id is patched in place.
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
    /// per-folder failure, etc.). Never contains a secret.
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
pub fn spawn(
    store_path: PathBuf,
    token_path: PathBuf,
    cfg: AuthConfig,
    backfill_days: i64,
    tick: Duration,
) -> SyncHandle {
    spawn_with_bases(
        store_path,
        token_path,
        cfg,
        backfill_days,
        "https://graph.microsoft.com/v1.0".to_string(),
        "https://login.microsoftonline.com".to_string(),
        tick,
    )
}

/// Like [`spawn`], but with the Graph base URL, auth base URL, and tick
/// injected — the seam integration tests use to point the whole engine at the
/// in-process fake server.
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
        loop {
            match cmd_rx.recv_timeout(self.recv_timeout()) {
                Ok(SyncCommand::Shutdown) => return,
                Ok(cmd) => self.handle_command(cmd),
                Err(RecvTimeoutError::Timeout) => self.on_tick(),
                // The UI dropped its `SyncHandle`: no one is listening anymore.
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// How long to block for the next command: until the back-off deadline if
    /// one is pending, otherwise the configured tick.
    fn recv_timeout(&self) -> Duration {
        match self.next_retry {
            Some(t) => t.saturating_duration_since(Instant::now()),
            None => self.config.tick,
        }
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
        let needs_full =
            !self.backfill_done || self.store.folders().map(|f| f.is_empty()).unwrap_or(true);
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
            SyncCommand::Refresh => self.sync_pass(true),
            SyncCommand::SignIn => self.sign_in(),
            SyncCommand::MarkRead { id, read } => {
                self.store.set_read(&id, read);
                self.enqueue_and_drain(OutboxOp::MarkRead { id, read });
            }
            SyncCommand::SetFlag { id, flagged } => {
                self.store.set_flag(&id, flagged);
                self.enqueue_and_drain(OutboxOp::SetFlag { id, flagged });
            }
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
                let _ = self.store.delete_message(&id);
                self.enqueue_and_drain(OutboxOp::Delete { id });
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
            SyncCommand::RespondMeeting { message_id, kind } => {
                self.respond_meeting(&message_id, kind)
            }
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
            SyncCommand::RespondEvent { id, kind, comment } => {
                // Optimistic local write, same pattern as MarkRead/SetFlag/
                // Move/Delete: reflect the RSVP immediately, before the
                // outbox drain even reaches Graph.
                self.store.set_event_response(&id, &kind);
                self.emit(SyncEvent::CalendarUpdated);
                self.enqueue_and_drain(OutboxOp::RespondEvent { id, kind, comment });
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

    /// One sync pass: optionally re-enumerate folders, then delta-sync every
    /// stored folder and drain the outbox. Auth/throttle/transport failures
    /// abort the pass (state already set); per-folder 4xx/parse errors are
    /// surfaced and skipped so one folder can't sink the rest.
    fn sync_pass(&mut self, include_folders: bool) {
        if self.token.is_none() {
            return;
        }
        self.set_state(SyncState::Syncing);

        if include_folders && !self.sync_folders() {
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
        self.drain_outbox();
        // A full pass without a hard failure clears any offline back-off.
        self.clear_backoff();

        // Refresh the local contact index from whatever mail is now stored
        // (cheap, idempotent). Best-effort: a failure here must not fail the pass.
        let _ = self.store.refresh_local_contacts();

        // On a full pass (folders were re-enumerated, e.g. after sign-in),
        // augment contacts from the corporate directory. Best-effort and
        // gracefully degrading: People.Read may be denied (403), blocked by
        // Conditional Access, or offline — in every case we skip it silently
        // and the locally-mined contacts still power autocomplete.
        if include_folders {
            self.sync_people();
        }

        self.settle_state();
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

    /// GET the folder tree, upsert every folder, emit `FoldersUpdated`.
    /// Returns `false` if the pass should abort (auth/throttle/transport).
    ///
    /// If the real Drafts folder (`well_known_name = "drafts"`) is present in
    /// this fetch, also re-files (`Store::adopt_sentinel_drafts`) any
    /// messages still sitting under the local drafts sentinel folder — see
    /// `Store::drafts_folder_id` for why that sentinel exists and the Task 6
    /// report for the gap this closes. Harmless (a no-op `UPDATE` matching
    /// zero rows) once already adopted, so this doesn't need a "did we
    /// already do this" flag.
    fn sync_folders(&mut self) -> bool {
        match self.with_auth(|c| c.list_folders()) {
            Ok(folders) => {
                for f in &folders {
                    let _ = self.store.upsert_folder(f);
                }
                if let Some(drafts) = folders
                    .iter()
                    .find(|f| f.well_known_name.as_deref() == Some("drafts"))
                {
                    let _ = self.store.adopt_sentinel_drafts(&drafts.id);
                }
                // The initial enumeration has now succeeded at least once, so
                // ticks can safely drop to incremental deltas.
                self.backfill_done = true;
                self.emit(SyncEvent::FoldersUpdated);
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
                    DeltaItem::Upsert(m) => {
                        if let Some(cut) = &cutoff {
                            if !m.received.is_empty() && m.received.as_str() < cut.as_str() {
                                continue;
                            }
                        }
                        let _ = self.store.upsert_message(folder_id, m);
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
        true
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
        if self.token.is_some() {
            self.drain_outbox();
        }
        self.settle_state();
    }

    /// Drains queued outbox ops in `seq` order via `apply_op`. Success drops
    /// the op; auth/throttle/transport stop the drain (handled centrally);
    /// a non-retryable 4xx/parse bumps the attempt count and, past
    /// [`MAX_OP_ATTEMPTS`], quarantines the op (drops it + emits `Error`) so
    /// the rest of the queue proceeds; a 5xx backs off and retries later.
    fn drain_outbox(&mut self) {
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
                return;
            }
            Err(e) => {
                self.emit(SyncEvent::Error(e.to_string()));
                return;
            }
        };

        for row in ops {
            match self.apply_op_with_retry(&row.op) {
                Ok(()) => {
                    self.store.drop_op(row.seq);
                    if let OutboxOp::SendDraft { id } = &row.op {
                        self.emit(SyncEvent::Sent { id: id.clone() });
                    }
                }
                Err(GraphError::Unauthorized) => {
                    self.enter_signin();
                    return;
                }
                Err(GraphError::Throttled { retry_after_secs }) => {
                    self.schedule_retry(Duration::from_secs(retry_after_secs));
                    return;
                }
                Err(GraphError::Transport(_)) => {
                    self.go_offline();
                    return;
                }
                Err(other) => {
                    // 4xx (incl. 404) and parse failures are non-retryable;
                    // 5xx is a transient server error worth backing off on.
                    let retryable_5xx =
                        matches!(&other, GraphError::Http { status, .. } if *status >= 500);
                    if retryable_5xx {
                        self.store.bump_op_attempts(row.seq, &other.to_string());
                        self.go_offline();
                        return;
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

    /// Fetch a message body (plain text, best for a TUI), store it, and emit
    /// `BodyReady`.
    fn fetch_body(&mut self, id: &str) {
        if self.token.is_none() {
            self.emit(SyncEvent::Error("not signed in".to_string()));
            return;
        }
        match self.with_auth(|c| c.get_body(id, true)) {
            Ok(body) => {
                let _ = self.store.put_body(id, &body);
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
    fn respond_meeting(&mut self, message_id: &str, kind: RsvpKind) {
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
        match self.with_auth(|c| c.respond_event(&event_id, kind, None, true)) {
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
        // The redirect host must match the bind (`127.0.0.1`), NOT `localhost`:
        // if `localhost` resolved to `::1` (IPv6) first, the browser redirect
        // would hit nothing (the listener is IPv4-only) and sign-in would hang
        // to the timeout. `127.0.0.1` is an equally valid Entra public-client
        // loopback redirect host.
        let redirect_uri = format!("http://127.0.0.1:{port}");
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

    /// Collects events until `pred` returns true, or panics after 5s.
    fn wait_for(
        rx: &Receiver<SyncEvent>,
        mut pred: impl FnMut(&SyncEvent) -> bool,
    ) -> Vec<SyncEvent> {
        let deadline = Instant::now() + Duration::from_secs(5);
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if srv
                .requests()
                .iter()
                .any(|r| r.method == "PATCH" && r.path.starts_with("/me/messages/M1"))
            {
                break;
            }
            if Instant::now() >= deadline {
                panic!("no PATCH observed; requests: {:?}", srv.requests());
            }
            std::thread::sleep(Duration::from_millis(20));
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if srv
                .requests()
                .iter()
                .any(|r| r.method == "POST" && r.path.starts_with("/me/messages/M1/move"))
            {
                break;
            }
            if Instant::now() >= deadline {
                panic!("no move POST observed; requests: {:?}", srv.requests());
            }
            std::thread::sleep(Duration::from_millis(20));
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let store = Store::open(store_path).unwrap();
            let hits = store.search_contacts(query, 1).unwrap();
            if !hits.is_empty() {
                return hits;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for a contact matching {query:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let restored = Store::open(&store_path)
                .unwrap()
                .messages_in_folder("F1", 50, 0)
                .unwrap()
                .iter()
                .any(|m| m.id == "M1");
            if restored {
                break;
            }
            if Instant::now() >= deadline {
                panic!("reconverge did not restore the aged-out message");
            }
            std::thread::sleep(Duration::from_millis(20));
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let n = srv
                .requests()
                .iter()
                .filter(|r| r.method == "GET" && r.path.starts_with("/me/mailFolders"))
                .count();
            if n >= 2 {
                break;
            }
            if Instant::now() >= deadline {
                panic!(
                    "tick did not re-enumerate folders; requests: {:?}",
                    srv.requests()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
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
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if attempts.load(Ordering::SeqCst) >= 2 {
                break;
            }
            if Instant::now() >= deadline {
                panic!(
                    "did not observe 2 connection attempts in time (saw {})",
                    attempts.load(Ordering::SeqCst)
                );
            }
            std::thread::sleep(Duration::from_millis(20));
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
        handle.cmd_tx.send(SyncCommand::FetchAutomaticReplies).unwrap();
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
        assert!(store.draft(&local_id).unwrap().is_none());
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
        let redirect_uri = params
            .iter()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.clone())
            .expect("authorize_url carries redirect_uri");
        let state = params
            .iter()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.clone())
            .expect("authorize_url carries state");
        let port: u16 = redirect_uri
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .expect("redirect_uri ends with the loopback port");

        // Simulate the browser landing on the loopback redirect.
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect to loopback");
        let request =
            format!("GET /?code=THECODE&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        client.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
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
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if srv
                .requests()
                .iter()
                .any(|r| r.method == "POST" && r.path.starts_with("/me/events/E1/accept"))
            {
                break;
            }
            if Instant::now() >= deadline {
                panic!("no accept POST observed; requests: {:?}", srv.requests());
            }
            std::thread::sleep(Duration::from_millis(20));
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
}
