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
use crate::graph::client::{DeltaCursor, GraphClient, GraphError};
use crate::graph::model::DeltaItem;
use crate::store::{OutboxOp, Store, StoreError};
use crate::sync::outbox::apply_op;
use crate::tokencache;
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

/// How long to wait for a command before running a periodic sync tick.
const DEFAULT_TICK: Duration = Duration::from_secs(60);
/// Refresh the access token when it's within this window of expiring.
const EXPIRY_SKEW_SECS: u64 = 300;
/// Quarantine an outbox op after this many failed attempts on a non-retryable
/// (4xx other than 401/429) error, so one bad op can't block the queue.
const MAX_OP_ATTEMPTS: i64 = 5;
/// Offline back-off bounds (exponential between them).
const BACKOFF_MIN: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Spawns the sync thread with production Graph/auth endpoints and the default
/// 60s tick, returning the channels to drive it.
pub fn spawn(
    store_path: PathBuf,
    token_path: PathBuf,
    cfg: AuthConfig,
    backfill_days: i64,
) -> SyncHandle {
    spawn_with_bases(
        store_path,
        token_path,
        cfg,
        backfill_days,
        "https://graph.microsoft.com/v1.0".to_string(),
        "https://login.microsoftonline.com".to_string(),
        DEFAULT_TICK,
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
        let needs_full = !self.backfill_done
            || self.store.folders().map(|f| f.is_empty()).unwrap_or(true);
        self.sync_pass(needs_full);
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
        self.settle_state();
    }

    /// GET the folder tree, upsert every folder, emit `FoldersUpdated`.
    /// Returns `false` if the pass should abort (auth/throttle/transport).
    fn sync_folders(&mut self) -> bool {
        match self.with_auth(|c| c.list_folders()) {
            Ok(folders) => {
                for f in &folders {
                    let _ = self.store.upsert_folder(f);
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
            match self.with_auth(|c| apply_op(c, &row.op)) {
                Ok(()) => self.store.drop_op(row.seq),
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
                        self.emit(SyncEvent::Error(format!(
                            "quarantined outbox op seq {} after {attempts_after} attempts: {other}",
                            row.seq
                        )));
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

    /// The interactive sign-in seam. `begin_auth` + `SignInStarted` are wired;
    /// the loopback listener that captures the redirect `code` is Task 17's
    /// job (the UI owns the browser + TCP listener), so `listen_for_code` is a
    /// clearly-marked stub here. Once it returns `Some(code)`, the redeem +
    /// cache-save + resync path below runs unchanged.
    fn sign_in(&mut self) {
        // Task 17 binds a loopback listener on `127.0.0.1:0` and uses its
        // assigned port verbatim here; until then this is a placeholder.
        let redirect_uri = "http://localhost:0".to_string();
        let req = auth::begin_auth(&self.config.cfg, &redirect_uri);
        self.emit(SyncEvent::SignInStarted {
            authorize_url: req.authorize_url.clone(),
        });

        let Some(code) = self.listen_for_code() else {
            // Headless engine can't capture the code itself; stay in the
            // sign-in-required state for the UI to complete the flow.
            return;
        };
        match auth::redeem_code(&self.config.cfg, &self.config.auth_base, &req, &code) {
            Ok(t) => {
                let _ = tokencache::save(&self.config.token_path, &t);
                self.token = Some(t);
                self.clear_backoff();
                self.sync_pass(true);
            }
            Err(e) => {
                self.emit(SyncEvent::Error(format!("sign-in failed: {e}")));
                self.enter_signin();
            }
        }
    }

    /// STUB for Task 17: bind a loopback TCP listener, open the browser, and
    /// read the `code` query param off the redirect. Not implemented in the
    /// headless engine; returns `None` so the engine stays signed out until the
    /// UI layer drives the redeem itself.
    fn listen_for_code(&self) -> Option<String> {
        None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthConfig, TokenSet};
    use crate::store::Store;
    use crate::testserver::{FakeServer, Route};
    use crate::tokencache;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    fn unique_dir(tag: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lookxy-sync-{tag}-{}-{n}", std::process::id()));
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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1")
        });

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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1")
        });

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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1")
        });

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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1")
        });

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

        let events = wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::Error(m) if m.contains("quarantined"))
        });
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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1")
        });
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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::Error(m) if m.contains("quarantined"))
        });

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
                panic!("tick did not re-enumerate folders; requests: {:?}", srv.requests());
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let _ = handle.cmd_tx.send(SyncCommand::Shutdown);
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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1")
        });

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
        assert!(events.iter().any(
            |e| matches!(e, SyncEvent::AttachmentSaved { path } if path == &dest)
        ));
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");

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
        wait_for(&handle.evt_rx, |e| {
            matches!(e, SyncEvent::MessagesUpdated { folder_id } if folder_id == "F1")
        });

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
}
