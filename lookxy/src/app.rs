//! `App` — the TUI's in-memory state: the local mail store, the background
//! sync handle, where the three panes (folders/list/reading) currently
//! point, the triage actions (`m`/`u`/`f`/`d`/`v`) that mutate them, and
//! compose's entry points (`c`/`r`/`R`/`F`, drafts resume, send/save/discard
//! wiring — see the "Compose" section below).

use std::path::{Path, PathBuf};

use crate::ui::compose::{Compose, ComposeAction, ComposeField};
use editcore::ops::Editor;
use mailcore::compose_html;
use mailcore::graph::model::{AttachmentMeta, Body};
use mailcore::store::{FolderRow, MessageRow, Store};
use mailcore::sync::engine::{SyncCommand, SyncEvent, SyncHandle, SyncState};

/// Which pane currently has keyboard focus. Tab cycles `Folders` → `List` →
/// `Reading` → `Folders` (see `ui::handle_key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Folders,
    List,
    Reading,
}

/// All in-memory TUI state: the local store, the sync channels, the cached
/// folder/message lists the three panes render, and the current
/// selection/focus. `sync` is the UI's half of the background sync engine
/// (`mailcore::sync::spawn`) — commands go down `cmd_tx`, events come up
/// `evt_rx`.
///
/// `folders`/`messages` are read from `store` on construction and whenever
/// `reload_folders`/`reload_messages` is called (on a selection change, or
/// when the sync engine reports `FoldersUpdated`/`MessagesUpdated` for the
/// visible folder) — the panes never query the store mid-render.
pub struct App {
    pub store: Store,
    pub sync: SyncHandle,
    pub focus: Pane,
    pub folders: Vec<FolderRow>,
    /// Index into `folders` of the currently highlighted row.
    pub folder_index: usize,
    pub messages: Vec<MessageRow>,
    /// Index into `messages` of the currently highlighted row.
    pub msg_index: usize,
    pub selected_folder: Option<String>,
    pub selected_msg: Option<String>,
    /// The opened (`selected_msg`) message's body, once it's in the store.
    /// `None` while nothing is opened, or while it's opened but not yet
    /// fetched — see `body_loading` to tell those two apart.
    pub body: Option<Body>,
    /// `true` from the moment a message is opened whose body isn't yet in
    /// the store (a `SyncCommand::FetchBody` has been sent for it) until
    /// `SyncEvent::BodyReady` lands and `reload_body` re-reads the store.
    /// The reading pane shows a "loading…" placeholder while this is set.
    pub body_loading: bool,
    pub status: SyncState,
    pub quit: bool,
    /// The open move-folder popup (`v`), if any — see `open_move_picker`.
    pub move_picker: Option<MovePicker>,
    /// The in-progress/submitted search prompt (`/`), if any — see
    /// `start_search`. `None` is the normal folder view; `visible_messages`
    /// is the one seam that reads through either state.
    pub search: Option<SearchState>,
    /// The open attachments popup (`a`), if any — see `open_attachments_popup`.
    pub attachments: Option<AttachmentsPopup>,
    /// The last attachment save/open outcome (e.g. "Saved: C:\...\f.txt"),
    /// for the status bar to show. `None` until the first save completes.
    pub attachment_notice: Option<String>,
    /// The last surfaced `SyncEvent::Error` message (a failed attachment save,
    /// a quarantined triage op, a per-folder sync failure, …), for the status
    /// bar to show in a distinct error style — taking precedence over
    /// `attachment_notice` so a failure can't masquerade as a success. Set on
    /// `SyncEvent::Error`, cleared on the next user key press (see
    /// `main::run`) or the next successful sync state (`State(Idle)`/
    /// `FoldersUpdated`) — see `on_sync_event`.
    pub error_notice: Option<String>,
    /// In-flight `SaveAttachment` requests keyed by their `dest` path, each
    /// mapped to whether it should be opened once it completes (`o`) or
    /// merely reported (Enter). Keyed per-request (rather than one shared
    /// flag) so that saving attachment A with `o` and attachment B with
    /// Enter before A's `SyncEvent::AttachmentSaved` lands can't cross-wire
    /// — each completion looks up (and removes) its own path's intent. See
    /// `finish_attachment_save`.
    pending_saves: std::collections::HashMap<PathBuf, bool>,
    /// Path to the on-disk token cache. The engine is the one that actually
    /// writes it (on sign-in and on refresh); the UI keeps this only so it
    /// can re-read the account name for the status bar once a sync pass
    /// completes (`reload_account`, called from `on_sync_event`) — nothing
    /// else in `SyncEvent` carries it.
    token_path: PathBuf,
    /// The signed-in account (a UPN like `me@epam.com`), once known. `None`
    /// before the first sign-in ever completes (or if the token cache can't
    /// be read) — the status bar shows a placeholder then.
    pub account: Option<String>,
    /// The sign-in modal's state, if it's currently showing: `Some` from the
    /// moment `SyncEvent::SignInRequired`/`SignInStarted` lands until the
    /// next successful sync (`FoldersUpdated` or `State(Idle)`) clears it —
    /// see `on_sync_event`.
    pub signin_modal: Option<SignInModal>,
    /// Test-only: counts calls to the OS-open seam (`open_with_os_handler`),
    /// which is a no-op under `cfg(test)` — see that function's doc comment.
    /// `None` for a production `App`.
    #[cfg(test)]
    pub open_invocations: std::cell::Cell<u32>,
    /// Test-only: counts calls to the browser-open seam
    /// (`open_url_with_os_handler`), a no-op under `cfg(test)` — the
    /// sign-in equivalent of `open_invocations`. See
    /// `browser_open_was_requested`.
    #[cfg(test)]
    pub browser_open_invocations: std::cell::Cell<u32>,
    /// Test-only: sees every `SyncCommand` this `App` has sent, so tests can
    /// assert on it (see `last_sent_command_is_mark_read`). `None` for a
    /// production `App`; `for_test_with_seeded_store`/`for_test_with_empty_store`
    /// populate it by keeping the receiver end of a fresh channel instead of
    /// dropping it.
    #[cfg(test)]
    pub test_cmd_rx: Option<std::sync::mpsc::Receiver<SyncCommand>>,
    /// The open compose view (new message, reply, forward, or a resumed
    /// draft), if any — see `ui::compose`. `Some` takes over the whole
    /// screen (`ui::draw`) and all key handling (`ui::handle_key`) ahead of
    /// every other pane/popup. Entry points that populate this (new/reply/
    /// forward/resume-a-draft) are a later task's concern; this field and
    /// `ui::compose`'s own key handling are what they'll set/drive.
    pub compose: Option<crate::ui::compose::Compose>,
    /// What the compose view's key handling last requested — Send
    /// (Ctrl-Enter), Save (Esc), or Discard (Ctrl-D) — for a later task's
    /// wiring to act on (send via Graph, `SyncCommand::SaveDraft`, discard +
    /// close) and then clear. `ui::compose::handle_key` only ever *records*
    /// the request here; it never performs the action or closes `compose`
    /// itself. `None` in the steady state.
    pub compose_action: Option<crate::ui::compose::ComposeAction>,
}

/// Which sign-in modal is currently showing (see `App::signin_modal`):
/// `Required` right after `SyncEvent::SignInRequired` (Enter sends
/// `SyncCommand::SignIn`); `Started` once `SyncEvent::SignInStarted` has
/// opened the browser (nothing left for the user to press — just a "go
/// finish it over there" message). `Started` carries the `authorize_url` so
/// the modal can also show it as a fallback (`ui::signin::draw`) — the
/// browser-open is a best-effort, fire-and-forget shell-out, so if it
/// silently fails to launch anything, the user still has something to copy
/// into a browser by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInModal {
    Required,
    Started { authorize_url: String },
}

/// State for the move-folder popup opened by `v`: the candidate folders (as
/// read from `Store::folders` when the picker opened), which one is
/// highlighted, and the message being moved — captured up front so that
/// navigating the popup with ↑/↓ can't retarget it mid-pick.
pub struct MovePicker {
    pub folders: Vec<FolderRow>,
    pub index: usize,
    pub message_id: String,
}

/// State for the search prompt opened by `/`: the in-progress query text,
/// and the results of the last `submit_search` (`None` until the first
/// Enter — the prompt can be open with nothing searched yet).
pub struct SearchState {
    pub query: String,
    pub results: Option<Vec<MessageRow>>,
}

/// State for the attachments popup opened by `a`: the message it lists
/// attachments for, the attachment metadata (`Store::attachments`), and
/// which one is highlighted — same captured-up-front shape as `MovePicker`.
/// `loading` is set when the popup opened before local metadata existed and
/// a `SyncCommand::FetchAttachments` is in flight (`items` is empty then,
/// too) — see `open_attachments_popup`/`reload_attachments`.
pub struct AttachmentsPopup {
    pub message_id: String,
    pub items: Vec<AttachmentMeta>,
    pub index: usize,
    pub loading: bool,
}

/// How many messages `reload_messages` pulls per folder. Paging further back
/// is a later task's concern; this is enough to fill a screen many times over.
const MESSAGE_PAGE_SIZE: i64 = 200;
/// How many results `submit_search` pulls from the FTS index — generous
/// enough to cover realistic queries without an unbounded scan.
const SEARCH_PAGE_SIZE: i64 = 200;

impl App {
    /// `token_path` is only kept for `reload_account` (see `App::token_path`'s
    /// doc comment) — the engine itself is handed its own copy separately at
    /// spawn time and is the only thing that ever writes it.
    pub fn new(store: Store, sync: SyncHandle, token_path: PathBuf) -> App {
        let mut app = App {
            store,
            sync,
            focus: Pane::Folders,
            folders: Vec::new(),
            folder_index: 0,
            messages: Vec::new(),
            msg_index: 0,
            selected_folder: None,
            selected_msg: None,
            body: None,
            body_loading: false,
            status: SyncState::Idle,
            quit: false,
            move_picker: None,
            search: None,
            attachments: None,
            attachment_notice: None,
            error_notice: None,
            pending_saves: std::collections::HashMap::new(),
            token_path,
            account: None,
            signin_modal: None,
            #[cfg(test)]
            open_invocations: std::cell::Cell::new(0),
            #[cfg(test)]
            browser_open_invocations: std::cell::Cell::new(0),
            #[cfg(test)]
            test_cmd_rx: None,
            compose: None,
            compose_action: None,
        };
        app.reload_folders();
        app.reload_account();
        app
    }

    /// Re-reads the account name from the token cache (`None` if it can't be
    /// read yet, e.g. before the first sign-in ever completes). Called on
    /// construction and whenever a sync pass completes successfully — see
    /// `on_sync_event`.
    fn reload_account(&mut self) {
        self.account = mailcore::tokencache::load(&self.token_path)
            .ok()
            .flatten()
            .map(|t| t.account)
            .filter(|a| !a.is_empty());
    }

    /// Dispatches one `SyncEvent` from the background sync engine: reloads
    /// whatever cached state it invalidated, and drives the sign-in modal
    /// (`signin_modal`). `main::drain_events` calls this for every event the
    /// sync handle's channel yields; tests call it directly to drive the
    /// sign-in flow without a real sync thread.
    ///
    /// The sign-in modal opens on `SignInRequired`/`SignInStarted` and
    /// clears on the next successful sync — `FoldersUpdated` (a full pass
    /// just completed) or `State(Idle)` (the resting state once signed in
    /// with nothing pending) both mean "we're past sign-in now".
    pub fn on_sync_event(&mut self, evt: SyncEvent) {
        match evt {
            SyncEvent::State(s) => {
                // Any state that isn't `SignInRequired` means we're past
                // sign-in — auth already succeeded — so clear the modal. This
                // covers the transient case where the first sync pass right
                // after a successful redeem fails (engine emits `Syncing` then
                // `Offline`, never `Idle`/`FoldersUpdated`): the modal would
                // otherwise stay stuck blocking all keys forever. A resting
                // `Idle` additionally clears any error notice.
                if !matches!(s, SyncState::SignInRequired) {
                    self.signin_modal = None;
                }
                if matches!(s, SyncState::Idle) {
                    self.error_notice = None;
                }
                self.status = s;
            }
            SyncEvent::FoldersUpdated => {
                self.signin_modal = None;
                self.error_notice = None;
                self.reload_account();
                self.reload_folders();
            }
            SyncEvent::MessagesUpdated { folder_id }
                if self.selected_folder.as_deref() == Some(folder_id.as_str()) =>
            {
                self.reload_messages();
            }
            SyncEvent::BodyReady { id } if self.selected_msg.as_deref() == Some(id.as_str()) => {
                self.reload_body();
            }
            SyncEvent::AttachmentsUpdated { message_id } => self.reload_attachments(&message_id),
            SyncEvent::AttachmentSaved { path } => self.finish_attachment_save(path),
            // A reply/forward draft (`SyncCommand::ComposeReply`/
            // `ComposeForward`) just landed in the store — open the composer
            // on it (see `open_draft`).
            SyncEvent::DraftReady { id } => self.open_draft(&id),
            SyncEvent::SignInRequired => self.signin_modal = Some(SignInModal::Required),
            SyncEvent::SignInStarted { authorize_url } => {
                self.open_url_with_os_handler(&authorize_url);
                self.signin_modal = Some(SignInModal::Started { authorize_url });
            }
            SyncEvent::Error(msg) => self.error_notice = Some(msg),
            // `Sent` has no TUI consumer yet: the composer already closes
            // optimistically the moment Send is pressed (`apply_compose_action`),
            // before the engine's outbox drain even reaches Graph, so by the
            // time this lands there's nothing left open for it to affect —
            // it's purely a "yes, it actually got delivered" confirmation.
            // Folded into the existing catch-all rather than given a
            // dedicated arm so this compiles now without inventing a
            // toast/notice mechanism the brief doesn't ask for.
            // `CalendarUpdated` (from `SyncCommand::RefreshCalendar`/
            // `RespondEvent`) has no TUI consumer yet — the calendar view
            // lands in a later task. Folded into this catch-all for the same
            // reason `Sent` is: compiles now without inventing a view this
            // brief doesn't ask for.
            SyncEvent::MessagesUpdated { .. }
            | SyncEvent::BodyReady { .. }
            | SyncEvent::Sent { .. }
            | SyncEvent::CalendarUpdated => {}
        }
    }

    /// Whether a text-input context is currently capturing keystrokes — the
    /// search prompt (`/`), or the compose view's fields/body. The event
    /// loop consults this so a global hotkey like `q`-to-quit doesn't steal
    /// a character the user is typing into the query or a compose field
    /// (searching for "quarterly", or composing a message that mentions
    /// "quit", must not quit the app).
    pub fn is_capturing_text(&self) -> bool {
        self.search.is_some() || self.compose.is_some()
    }

    /// Enter, while the sign-in modal is showing: only the `Required` prompt
    /// has anything for Enter to do (send `SyncCommand::SignIn`); `Started`
    /// is purely informational (the browser is already open) so Enter is a
    /// no-op there. A no-op too if the modal isn't open at all — callers
    /// (`ui::handle_key`) only reach this while it is, but tests call it
    /// directly.
    pub fn on_key_enter(&mut self) {
        if matches!(self.signin_modal, Some(SignInModal::Required)) {
            let _ = self.sync.cmd_tx.send(SyncCommand::SignIn);
        }
    }

    /// Re-reads the folder list from the store (well-known folders already
    /// come pre-ranked). Keeps `selected_folder` if it still exists in the
    /// new list; otherwise defaults to the first folder. Always follows up
    /// with `reload_messages` so the message list stays in step.
    pub fn reload_folders(&mut self) {
        self.folders = self.store.folders().unwrap_or_default();
        match &self.selected_folder {
            Some(id) => {
                if let Some(idx) = self.folders.iter().position(|f| &f.id == id) {
                    self.folder_index = idx;
                } else {
                    self.selected_folder = self.folders.first().map(|f| f.id.clone());
                    self.folder_index = 0;
                }
            }
            None => {
                self.selected_folder = self.folders.first().map(|f| f.id.clone());
                self.folder_index = 0;
            }
        }
        self.reload_messages();
    }

    /// Re-reads the message list for `selected_folder` (newest first),
    /// clamping `msg_index` if the list got shorter.
    pub fn reload_messages(&mut self) {
        self.messages = match &self.selected_folder {
            Some(id) => self
                .store
                .messages_in_folder(id, MESSAGE_PAGE_SIZE, 0)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        if self.msg_index >= self.messages.len() {
            self.msg_index = self.messages.len().saturating_sub(1);
        }
    }

    /// Opens message `id` in the reading pane: records it as `selected_msg`
    /// and loads its body (see `reload_body`).
    pub fn open_message(&mut self, id: &str) {
        self.selected_msg = Some(id.to_string());
        self.reload_body();
    }

    /// Re-reads the opened message's body from the store. If it's already
    /// cached there, it's shown immediately; otherwise a
    /// `SyncCommand::FetchBody` is sent and `body_loading` is set so the
    /// reading pane can show a placeholder until `SyncEvent::BodyReady`
    /// (handled in `main::drain_events`) prompts another call to this.
    pub fn reload_body(&mut self) {
        let Some(id) = self.selected_msg.clone() else {
            self.body = None;
            self.body_loading = false;
            return;
        };
        match self.store.get_body(&id) {
            Ok(Some(body)) => {
                self.body = Some(body);
                self.body_loading = false;
            }
            Ok(None) => {
                self.body = None;
                self.body_loading = true;
                let _ = self.sync.cmd_tx.send(SyncCommand::FetchBody { id });
            }
            Err(_) => {
                // The store itself is broken; nothing a re-fetch can fix.
                self.body = None;
                self.body_loading = false;
            }
        }
    }

    // --- Triage actions ---------------------------------------------------

    /// Dispatches a single-character triage key against the message
    /// currently highlighted in the list pane (`messages[msg_index]`):
    /// `m`/`u` mark read/unread, `f` toggles the flag, `d` deletes. `v`
    /// (move) and `a` (attachments) are handled separately by
    /// `open_move_picker`/`open_attachments_popup`, since they need a
    /// picker/popup opened before anything can be sent. `/` opens the search
    /// prompt (`start_search`). Unrecognized characters are ignored. Called
    /// from `ui::handle_key` for every `KeyCode::Char` not already claimed by
    /// pane navigation or an open popup/prompt.
    pub fn on_key_char(&mut self, c: char) {
        match c {
            'm' => self.mark_read(true),
            'u' => self.mark_read(false),
            'f' => self.toggle_flag(),
            'd' => self.delete_selected(),
            'v' => self.open_move_picker(),
            'a' => self.open_attachments_popup(),
            '/' => self.start_search(),
            'c' => self.compose_new(),
            'r' => self.compose_reply(false),
            'R' => self.compose_reply(true),
            // `F`, not the brief's bare `f` — lowercase `f` already means
            // "toggle flag" (see the `'f'` arm above, shipped before this
            // task existed), so forward is bound to its uppercase shift
            // variant instead, the same lower/upper pairing the brief itself
            // already uses for reply vs. reply-all (`r`/`R`). See
            // `compose_forward`'s doc comment for the same note.
            'F' => self.compose_forward(),
            _ => {}
        }
    }

    /// The id of the message currently highlighted in the list pane, if any
    /// (empty list, or nothing loaded yet, yield `None` — every triage
    /// action is then a no-op rather than a panic).
    fn highlighted_message_id(&self) -> Option<String> {
        self.messages.get(self.msg_index).map(|m| m.id.clone())
    }

    /// Marks the highlighted message read/unread: writes it to the store
    /// (so `reload_messages` reflects it immediately, without waiting on the
    /// sync engine), then fires `SyncCommand::MarkRead` so the engine
    /// enqueues the matching outbox op and pushes it to Graph.
    pub fn mark_read(&mut self, read: bool) {
        let Some(id) = self.highlighted_message_id() else {
            return;
        };
        self.store.set_read(&id, read);
        self.reload_messages();
        let _ = self.sync.cmd_tx.send(SyncCommand::MarkRead { id, read });
    }

    /// Toggles the highlighted message's flag, same optimistic-store +
    /// fire-and-forget-command pattern as `mark_read`.
    pub fn toggle_flag(&mut self) {
        let Some(row) = self.messages.get(self.msg_index) else {
            return;
        };
        let id = row.id.clone();
        let flagged = !row.is_flagged;
        self.store.set_flag(&id, flagged);
        self.reload_messages();
        let _ = self.sync.cmd_tx.send(SyncCommand::SetFlag { id, flagged });
    }

    /// Deletes the highlighted message: removes it from the store, then
    /// `reload_messages` re-reads the (now shorter) list and clamps
    /// `msg_index` so the selection can't point past the end — the same
    /// bounds-safe pattern `reload_messages` already uses when a folder
    /// switch shrinks the list.
    pub fn delete_selected(&mut self) {
        let Some(id) = self.highlighted_message_id() else {
            return;
        };
        let _ = self.store.delete_message(&id);
        self.reload_messages();
        let _ = self.sync.cmd_tx.send(SyncCommand::Delete { id });
    }

    /// Opens the move-folder popup over the highlighted message. A no-op if
    /// nothing is highlighted, or there are no folders to move it to (an
    /// empty picker would have nothing to select and nowhere for Enter to
    /// land) — so this can never open a popup `confirm_move` can't act on.
    pub fn open_move_picker(&mut self) {
        let Some(id) = self.highlighted_message_id() else {
            return;
        };
        let folders = self.store.folders().unwrap_or_default();
        if folders.is_empty() {
            return;
        }
        self.move_picker = Some(MovePicker {
            folders,
            index: 0,
            message_id: id,
        });
    }

    /// Moves the picker's highlighted folder by `delta`, wrapping — the same
    /// wrap-around shape as `ui::wrapped`, kept as a tiny standalone copy
    /// here since the picker lives in `App` state and that helper is
    /// private to the `ui` module. A no-op if the picker isn't open or (in
    /// principle) has no folders.
    pub fn move_picker_select(&mut self, delta: isize) {
        if let Some(picker) = &mut self.move_picker {
            let len = picker.folders.len();
            if len == 0 {
                return;
            }
            let len = len as isize;
            picker.index = (((picker.index as isize + delta) % len + len) % len) as usize;
        }
    }

    /// Esc: closes the popup without moving anything.
    pub fn cancel_move_picker(&mut self) {
        self.move_picker = None;
    }

    /// Enter: re-files the captured message into the highlighted folder —
    /// store write, list reload, then `SyncCommand::Move` — and closes the
    /// popup either way. A local store failure (e.g. a stale/foreign folder
    /// id) skips the reload and the command send, since nothing downstream
    /// could act on it either; the popup still closes rather than getting
    /// stuck on a destination that can't work.
    pub fn confirm_move(&mut self) {
        let Some(picker) = self.move_picker.take() else {
            return;
        };
        let Some(dest) = picker.folders.get(picker.index).map(|f| f.id.clone()) else {
            return;
        };
        if self.store.move_message(&picker.message_id, &dest).is_ok() {
            self.reload_messages();
            let _ = self.sync.cmd_tx.send(SyncCommand::Move {
                id: picker.message_id,
                dest,
            });
        }
    }

    // --- Compose: entry points, drafts resume, send/save/discard ----------

    /// `c`: starts a brand-new message — an empty local draft
    /// (`Store::create_local_draft`, filed under Drafts even while offline)
    /// opened straight into the composer via `open_draft`, so this and the
    /// drafts-resume path (`ui::activate`) share the exact same load logic
    /// rather than two slightly different ways of constructing a `Compose`.
    /// A no-op if the store write itself fails (e.g. disk full) — nothing
    /// downstream could open anyway.
    pub fn compose_new(&mut self) {
        if let Ok(id) = self.store.create_local_draft("", "", "", "") {
            self.open_draft(&id);
        }
    }

    /// `r`/`R`: fires `SyncCommand::ComposeReply` for the highlighted
    /// message (`all` picks reply vs. reply-all). This does not open the
    /// composer itself — the engine fetches a pre-quoted draft from Graph
    /// and emits `SyncEvent::DraftReady`, which is what actually opens it
    /// (see `on_sync_event`). A no-op if nothing is highlighted.
    pub fn compose_reply(&mut self, all: bool) {
        let Some(id) = self.highlighted_message_id() else {
            return;
        };
        let _ = self.sync.cmd_tx.send(SyncCommand::ComposeReply { id, all });
    }

    /// Forward the highlighted message (`SyncCommand::ComposeForward`) —
    /// same shape as `compose_reply`. Bound to uppercase `F` rather than the
    /// brief's bare `f`: lowercase `f` was already the flag-toggle key (see
    /// `on_key_char`) before this task existed, and reusing it here would
    /// have silently broken that shipped, tested behavior. `F` pairs with
    /// `f` the same way the brief's own `r`/`R` (reply/reply-all) already
    /// pairs lower/upper case for a related pair of actions, so this follows
    /// the same convention rather than inventing a new one.
    pub fn compose_forward(&mut self) {
        let Some(id) = self.highlighted_message_id() else {
            return;
        };
        let _ = self.sync.cmd_tx.send(SyncCommand::ComposeForward { id });
    }

    /// Loads draft/reply/forward-draft `id` from the store and opens the
    /// composer on it: `Store::draft` for the message row + body,
    /// `compose_html::from_html` to parse the body back into an editable
    /// `RichText`, `editcore::ops::Editor::from` to wrap it. Three callers
    /// share this: `compose_new` (a just-created empty draft),
    /// `on_sync_event`'s `DraftReady` handling (a reply/forward draft the
    /// engine just fetched), and `ui::activate` (resuming a message already
    /// sitting in Drafts, via `MessageRow::is_draft`). A no-op if the store
    /// has nothing for `id` — a stale/foreign id, or (for `DraftReady`) a
    /// race where the row was deleted before this ran.
    pub fn open_draft(&mut self, id: &str) {
        let Ok(Some((row, body))) = self.store.draft(id) else {
            return;
        };
        let editor = Editor::from(compose_html::from_html(&body.content));
        self.compose = Some(Compose {
            to: row.to_recipients,
            cc: row.cc_recipients,
            subject: row.subject,
            editor,
            focus: ComposeField::To,
            draft_id: row.id,
        });
    }

    /// Acts on the last request the compose view's key handling recorded on
    /// `App::compose_action` — Ctrl-Enter (`Send`), Esc (`Save`), or Ctrl-D
    /// (`Discard`) — and closes the composer either way, clearing the
    /// request so it can't be replayed on the next tick. Called from
    /// `ui::handle_key` right after every keystroke while the composer is
    /// open (`ui::compose::handle_key` only ever *records* the request,
    /// never acts on it — see that module's doc comment). A no-op if
    /// nothing was requested.
    ///
    /// Send/Save both serialize the editor's body to HTML
    /// (`compose_html::to_html`) and write it back to the store
    /// (`Store::update_draft_fields`) before firing the matching
    /// `SyncCommand` (`SendDraft`/`SaveDraft`), so the engine's outbox drain
    /// reads the fields the user actually typed rather than whatever the
    /// draft row held before this edit; the composer then closes
    /// optimistically, without waiting for the engine to confirm anything.
    /// Discard drops the in-progress edit without writing or sending
    /// anything at all — the local draft row (if any) is left exactly as it
    /// was, the same way closing an Outlook compose window without sending
    /// leaves the autosaved draft behind rather than deleting it.
    pub fn apply_compose_action(&mut self) {
        let Some(action) = self.compose_action.take() else {
            return;
        };
        let Some(compose) = self.compose.take() else {
            return;
        };
        if action == ComposeAction::Discard {
            return;
        }
        let html = compose_html::to_html(&compose.editor.text);
        let _ = self.store.update_draft_fields(
            &compose.draft_id,
            &compose.subject,
            &compose.to,
            &compose.cc,
            &html,
        );
        let cmd = if action == ComposeAction::Send {
            SyncCommand::SendDraft {
                id: compose.draft_id,
            }
        } else {
            SyncCommand::SaveDraft {
                id: compose.draft_id,
            }
        };
        let _ = self.sync.cmd_tx.send(cmd);
    }

    // --- Search -------------------------------------------------------

    /// `/`: opens the search prompt with an empty query and no results yet.
    /// Resets `msg_index` to 0 so the eventual results list starts at the
    /// top rather than wherever the folder view's selection happened to be.
    pub fn start_search(&mut self) {
        self.search = Some(SearchState {
            query: String::new(),
            results: None,
        });
        self.msg_index = 0;
    }

    /// Appends `s` to the in-progress query; a no-op if the prompt isn't
    /// open. Used both directly (typing a whole query in one call, as the
    /// `search_prompt_filters_results` test does) and, one character at a
    /// time, by `ui::handle_key` while the prompt has focus.
    pub fn type_query(&mut self, s: &str) {
        if let Some(search) = &mut self.search {
            search.query.push_str(s);
        }
    }

    /// Removes the last character of the in-progress query, if any. A no-op
    /// if the prompt isn't open or the query is already empty.
    pub fn backspace_query(&mut self) {
        if let Some(search) = &mut self.search {
            search.query.pop();
        }
    }

    /// Enter: runs the in-progress query against the store's FTS index
    /// (`Store::search`, capped at `SEARCH_PAGE_SIZE`) and stores the
    /// results as a virtual message list; `msg_index` is clamped in case a
    /// previous result set (or the folder view) was longer. A no-op if the
    /// prompt isn't open.
    pub fn submit_search(&mut self) {
        let Some(query) = self.search.as_ref().map(|s| s.query.clone()) else {
            return;
        };
        let results = self
            .store
            .search(&query, SEARCH_PAGE_SIZE)
            .unwrap_or_default();
        if let Some(search) = &mut self.search {
            search.results = Some(results);
        }
        let len = self.visible_messages().len();
        if self.msg_index >= len {
            self.msg_index = len.saturating_sub(1);
        }
    }

    /// Moves the selection within whatever `visible_messages` currently
    /// shows, wrapping — the search-mode equivalent of `ui::move_selection`'s
    /// `Pane::List` branch (kept as its own copy for the same reason
    /// `move_picker_select` is: `ui::wrapped` is private to the `ui` module).
    pub fn move_search_selection(&mut self, delta: isize) {
        let len = self.visible_messages().len();
        if len == 0 {
            return;
        }
        let len = len as isize;
        self.msg_index = (((self.msg_index as isize + delta) % len + len) % len) as usize;
    }

    /// Esc: closes the search prompt/results and returns to the normal
    /// folder view (`visible_messages` falls back to `messages` once
    /// `search` is `None`). Clamps `msg_index` in case the folder view's
    /// message list is shorter than wherever the virtual list had it.
    pub fn cancel_search(&mut self) {
        self.search = None;
        if self.msg_index >= self.messages.len() {
            self.msg_index = self.messages.len().saturating_sub(1);
        }
    }

    /// The messages the list pane should currently render and navigate:
    /// the search results once a query has been submitted, otherwise the
    /// selected folder's messages.
    pub fn visible_messages(&self) -> &[MessageRow] {
        match self.search.as_ref().and_then(|s| s.results.as_ref()) {
            Some(r) => r,
            None => &self.messages,
        }
    }

    /// `visible_messages().len()` — how many rows the list pane currently
    /// shows, whichever source they came from.
    pub fn visible_message_count(&self) -> usize {
        self.visible_messages().len()
    }

    // --- Attachments ----------------------------------------------------

    /// `a`: opens the attachments popup over the highlighted message. If
    /// `Store::attachments(id)` already has rows (the common case once
    /// they've been fetched at least once), shows them immediately.
    /// Otherwise, if the row's `has_attachments` flag says Graph has some,
    /// opens the popup in a loading state and fires
    /// `SyncCommand::FetchAttachments` — `reload_attachments` (called from
    /// `main::drain_events` on `SyncEvent::AttachmentsUpdated`) fills it in
    /// once that lands. If the message genuinely has no attachments, this
    /// is a no-op — same "don't open a popup that can't work" pattern as
    /// `open_move_picker`.
    pub fn open_attachments_popup(&mut self) {
        let Some((id, has_attachments)) = self
            .messages
            .get(self.msg_index)
            .map(|m| (m.id.clone(), m.has_attachments))
        else {
            return;
        };
        let items = self.store.attachments(&id).unwrap_or_default();
        if !items.is_empty() {
            self.attachments = Some(AttachmentsPopup {
                message_id: id,
                items,
                index: 0,
                loading: false,
            });
            return;
        }
        if !has_attachments {
            return;
        }
        self.attachments = Some(AttachmentsPopup {
            message_id: id.clone(),
            items: Vec::new(),
            index: 0,
            loading: true,
        });
        let _ = self
            .sync
            .cmd_tx
            .send(SyncCommand::FetchAttachments { message_id: id });
    }

    /// Called from `main::drain_events` when `SyncEvent::AttachmentsUpdated`
    /// lands: re-reads `Store::attachments` into the popup, if it's still
    /// open for that same message (a no-op otherwise — the popup may have
    /// been closed, or reopened for a different message, before the fetch
    /// completed). If the store still has nothing for it (an empty result,
    /// or the fetch failed and never updated it), closes the popup rather
    /// than leaving it stuck showing "Loading…" forever.
    pub fn reload_attachments(&mut self, message_id: &str) {
        let is_open_for_this_message = self
            .attachments
            .as_ref()
            .is_some_and(|p| p.message_id == message_id);
        if !is_open_for_this_message {
            return;
        }
        let items = self.store.attachments(message_id).unwrap_or_default();
        if items.is_empty() {
            self.attachments = None;
            return;
        }
        if let Some(popup) = &mut self.attachments {
            popup.loading = false;
            let len = items.len();
            popup.items = items;
            if popup.index >= len {
                popup.index = len.saturating_sub(1);
            }
        }
    }

    /// Moves the popup's highlighted attachment by `delta`, wrapping — same
    /// shape as `move_picker_select`. A no-op if the popup isn't open (or has
    /// nothing in it yet, e.g. still loading).
    pub fn attachments_select(&mut self, delta: isize) {
        if let Some(popup) = &mut self.attachments {
            let len = popup.items.len();
            if len == 0 {
                return;
            }
            let len = len as isize;
            popup.index = (((popup.index as isize + delta) % len + len) % len) as usize;
        }
    }

    /// Esc: closes the popup without saving anything.
    pub fn cancel_attachments_popup(&mut self) {
        self.attachments = None;
    }

    /// Enter: saves the highlighted attachment's bytes to the Downloads
    /// directory. A no-op if the popup isn't open or has no highlighted row
    /// (e.g. still loading).
    pub fn save_attachment(&mut self) {
        self.send_save_attachment_command(false);
    }

    /// `o`: same save, but also opens the file with the OS handler once the
    /// save completes — see `finish_attachment_save`, which
    /// `main::drain_events` calls when `SyncEvent::AttachmentSaved` lands.
    pub fn save_and_open_attachment(&mut self) {
        self.send_save_attachment_command(true);
    }

    /// Resolves the highlighted attachment's destination path (Downloads
    /// dir + a sanitized filename), records whether *this* save should open
    /// the file once it completes (keyed by `dest`, so a second save started
    /// before the first completes can't steal or lose the first one's
    /// open-intent — see `finish_attachment_save`), and fires
    /// `SyncCommand::SaveAttachment`.
    fn send_save_attachment_command(&mut self, open_after: bool) {
        let Some(popup) = &self.attachments else {
            return;
        };
        let Some(att) = popup.items.get(popup.index) else {
            return;
        };
        let message_id = popup.message_id.clone();
        let attachment_id = att.id.clone();
        let dest = downloads_dir().join(sanitize_filename(&att.name));
        self.pending_saves.insert(dest.clone(), open_after);
        let _ = self.sync.cmd_tx.send(SyncCommand::SaveAttachment {
            message_id,
            attachment_id,
            dest,
        });
    }

    /// Called from `main::drain_events` when `SyncEvent::AttachmentSaved`
    /// lands: looks up (and removes) `path`'s own open-intent from
    /// `pending_saves` — so an unrelated in-flight save's completion can
    /// never trigger *this* one's open handler, or vice versa — opens the
    /// file with the OS handler iff that intent was `o`, and records a
    /// status notice. The popup only auto-closes once every in-flight save
    /// has resolved, so one save finishing can't yank the popup out from
    /// under another that's still pending.
    pub fn finish_attachment_save(&mut self, path: PathBuf) {
        let open_after = self.pending_saves.remove(&path).unwrap_or(false);
        if open_after {
            self.open_with_os_handler(&path);
        }
        self.attachment_notice = Some(format!("Saved: {}", path.display()));
        if self.pending_saves.is_empty() {
            self.attachments = None;
        }
    }

    /// Shells out to the OS's "open" handler for `path` (`rundll32.exe
    /// url.dll,FileProtocolHandler` on Windows, `open` on macOS, `xdg-open`
    /// elsewhere), fire-and-forget. Compiled out under `cfg(test)` in favor
    /// of a counter (`open_invocations`) — so a test exercising `o` can
    /// assert the seam was reached without ever launching a real OS handler.
    ///
    /// On Windows this deliberately does NOT go through `cmd /c start`:
    /// `std::process::Command` only quotes an argument for the *target*
    /// process if it contains a space/tab/quote, so a path or URL with none
    /// of those (but containing `&`, `|`, `^`, etc.) reaches `cmd.exe`
    /// unquoted — and cmd.exe's own shell parsing then splits on `&` as a
    /// command separator, silently truncating the argument (reproduced: an
    /// authorize URL's query string got cut at its first `&`). `rundll32`
    /// is not a shell — it receives `path`/`url` as a single literal argv
    /// element with no re-parsing — so this is safe for any path or URL,
    /// including ones `sanitize_filename` still allows to contain `&`. It
    /// also has no `%VAR%` env-expansion behavior, unlike `cmd /c start`.
    #[cfg(not(test))]
    fn open_with_os_handler(&self, path: &Path) {
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("rundll32.exe")
                .args(["url.dll,FileProtocolHandler"])
                .arg(path)
                .spawn();
        }
        #[cfg(not(windows))]
        {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let _ = std::process::Command::new(opener).arg(path).spawn();
        }
    }

    #[cfg(test)]
    fn open_with_os_handler(&self, _path: &Path) {
        self.open_invocations.set(self.open_invocations.get() + 1);
    }

    /// Shells out to the OS's "open" handler for a URL — the sign-in flow's
    /// equivalent of `open_with_os_handler` (see its doc comment for why
    /// Windows goes through `rundll32.exe url.dll,FileProtocolHandler`
    /// rather than `cmd /c start`: an authorize URL's query string is full
    /// of `&`, which `cmd.exe` would otherwise treat as a command
    /// separator and truncate). Called from `on_sync_event` when
    /// `SyncEvent::SignInStarted` lands. Fire-and-forget: the browser is
    /// launched detached, and whether it actually got there isn't something
    /// this process can (or needs to) observe — the loopback listener on the
    /// engine side is what actually completes the flow, and the modal also
    /// shows the raw URL as a fallback (see `ui::signin::draw`) in case the
    /// browser never opens. Compiled out under `cfg(test)` in favor of a
    /// counter (`browser_open_invocations`), same pattern as
    /// `open_with_os_handler` — so no test run ever pops open a real
    /// browser.
    #[cfg(not(test))]
    fn open_url_with_os_handler(&self, url: &str) {
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("rundll32.exe")
                .args(["url.dll,FileProtocolHandler"])
                .arg(url)
                .spawn();
        }
        #[cfg(not(windows))]
        {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let _ = std::process::Command::new(opener).arg(url).spawn();
        }
    }

    #[cfg(test)]
    fn open_url_with_os_handler(&self, _url: &str) {
        self.browser_open_invocations
            .set(self.browser_open_invocations.get() + 1);
    }

    /// Test-only: whether `open_url_with_os_handler` has been reached at
    /// least once (see `browser_open_invocations`) — `SyncEvent::SignInStarted`
    /// is the only thing that calls it.
    #[cfg(test)]
    pub fn browser_open_was_requested(&self) -> bool {
        self.browser_open_invocations.get() > 0
    }

    /// Test-only: drains `test_cmd_rx` and reports whether the last command
    /// seen was a `MarkRead`. Always `false` when nothing populated the
    /// channel (a production `App`, or a test that hasn't wired it up).
    #[cfg(test)]
    pub fn last_sent_command_is_mark_read(&self) -> bool {
        let mut last = None;
        if let Some(rx) = &self.test_cmd_rx {
            while let Ok(cmd) = rx.try_recv() {
                last = Some(cmd);
            }
        }
        matches!(last, Some(SyncCommand::MarkRead { .. }))
    }

    /// Test-only: same as `last_sent_command_is_mark_read`, but for
    /// `SyncCommand::SignIn` — what `on_key_enter` sends while the
    /// `Required` sign-in modal is showing.
    #[cfg(test)]
    pub fn last_sent_command_is_signin(&self) -> bool {
        let mut last = None;
        if let Some(rx) = &self.test_cmd_rx {
            while let Ok(cmd) = rx.try_recv() {
                last = Some(cmd);
            }
        }
        matches!(last, Some(SyncCommand::SignIn))
    }

    /// Test-only: renders the whole UI (`ui::draw`) to an off-screen buffer
    /// and reports whether `needle` appears anywhere in it, case-insensitively
    /// (so a test doesn't have to match the exact capitalization the UI
    /// happens to use). A generously-sized buffer (120x40) so nothing the
    /// sign-in modal or status bar draws gets clipped out of the check.
    #[cfg(test)]
    pub fn render_contains(&self, needle: &str) -> bool {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut term = Terminal::new(TestBackend::new(120, 40)).expect("test backend");
        term.draw(|f| crate::ui::draw(f, self)).expect("draw");
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        text.to_lowercase().contains(&needle.to_lowercase())
    }

    /// Builds an `App` over an in-memory `Store` seeded with a folder
    /// "Inbox" and one message, wired to a `SyncHandle` whose channels are
    /// inert (no sync thread spawned) — for render/navigation tests that
    /// need a real `App` without touching the network or a real database.
    #[cfg(test)]
    pub fn for_test_with_seeded_store() -> App {
        use mailcore::graph::model::{MailFolder, Message, Recipient};
        use std::sync::mpsc;

        let store = Store::open_in_memory().expect("in-memory store");
        store
            .upsert_folder(&MailFolder {
                id: "inbox".into(),
                display_name: "Inbox".into(),
                parent_id: None,
                total_count: 1,
                unread_count: 1,
                well_known_name: Some("inbox".into()),
            })
            .expect("seed folder");
        store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m1".into(),
                    conversation_id: "c1".into(),
                    subject: "Hello".into(),
                    from: Recipient {
                        name: "Alice".into(),
                        address: "alice@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-16T10:00:00Z".into(),
                    sent: "2026-07-16T09:00:00Z".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "hi there".into(),
                    is_draft: false,
                },
            )
            .expect("seed message");

        // Nothing in a render/navigation test drives a real sync thread (there
        // isn't one), so `_evt_tx` is simply dropped at the end of this
        // function. `cmd_rx` is kept, though, and wired into `test_cmd_rx` so
        // triage-action tests can inspect what got sent (see
        // `last_sent_command_is_mark_read`).
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (_evt_tx, evt_rx) = mpsc::channel();
        let sync = SyncHandle { cmd_tx, evt_rx };

        // No real token cache backs a test `App`; `reload_account` reading
        // nothing just leaves `account` as `None`, which is what a
        // never-signed-in test fixture should show anyway.
        let mut app = App::new(store, sync, PathBuf::new());
        app.test_cmd_rx = Some(cmd_rx);
        app
    }

    /// Builds an `App` over an empty in-memory `Store` — no folders, no
    /// messages — wired to inert `SyncHandle` channels like
    /// `for_test_with_seeded_store` (no sync thread spawned). For tests
    /// asserting the UI degrades gracefully on the empty-mailbox state: the
    /// #1 TUI crash risk (a `.get()`/`nonzero` check swapped for direct
    /// indexing on an empty `folders`/`messages` list) has no coverage
    /// otherwise.
    #[cfg(test)]
    pub fn for_test_with_empty_store() -> App {
        use std::sync::mpsc;

        let store = Store::open_in_memory().expect("in-memory store");
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (_evt_tx, evt_rx) = mpsc::channel();
        let sync = SyncHandle { cmd_tx, evt_rx };
        let mut app = App::new(store, sync, PathBuf::new());
        app.test_cmd_rx = Some(cmd_rx);
        app
    }
}

/// `%LOCALAPPDATA%\lookxy` (or, off Windows, `$HOME/.local/share/lookxy`).
pub fn lookxy_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(base) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(base).join("lookxy");
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("lookxy")
}

/// The single mail database path: `<lookxy_dir>\mail.db`. v1 is single-account
/// (the token cache `token.bin` is likewise not per-account), so the store is
/// one fixed file rather than keyed on the signed-in account — the account is
/// only a status-bar display detail read from the token, never a DB-path key.
/// Per-account DBs (a subdirectory per account) are a future multi-account
/// concern, deliberately out of scope for v1.
pub fn store_path_for() -> PathBuf {
    lookxy_dir().join("mail.db")
}

/// The OS "Downloads" directory attachments are saved into:
/// `%USERPROFILE%\Downloads` on Windows, `$HOME/Downloads` elsewhere.
pub fn downloads_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(base) = std::env::var("USERPROFILE") {
            return PathBuf::from(base).join("Downloads");
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join("Downloads")
}

/// Sanitizes an attachment's `name` so it's safe to use as a single path
/// component under `downloads_dir()`, and as an argument to the `o` open
/// handler: path separators (`/`, `\`), drive/ADS colons, and control
/// characters become `_`; `%` is stripped too, since `cmd /c start` (the
/// Windows opener) expands `%VAR%` in its arguments and that expansion is
/// NOT protected by cmd's quoting — an attachment named e.g.
/// `%USERPROFILE%\x` could otherwise rewrite the path that ends up opened.
/// Any `..` (which would otherwise still read as a parent-directory
/// reference even with separators stripped, e.g. on a bare `..` component)
/// is neutralized too. Falls back to a fixed name if nothing usable is
/// left, so a malicious or empty attachment name can never escape
/// Downloads, rewrite the opened path, or produce a path `std::fs::write`
/// would choke on.
///
/// `pub(crate)` (rather than private) so `control.rs`'s `mail.save-attachment`
/// verb can reuse this exact sanitization instead of duplicating it.
pub(crate) fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '%' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.replace("..", "__");
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "attachment".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mark_read_updates_store_and_enqueues_command() {
        let mut app = App::for_test_with_seeded_store(); // has one unread message selected
        app.on_key_char('m');
        // store row now read
        let rows = app
            .store
            .messages_in_folder(app.selected_folder.as_ref().unwrap(), 50, 0)
            .unwrap();
        assert!(rows[0].is_read);
        // a MarkRead command was sent to the (test) sync handle
        assert!(app.last_sent_command_is_mark_read());
    }

    #[test]
    fn store_path_is_the_single_fixed_db_under_local_appdata() {
        // v1 is single-account: one fixed DB file, no per-account component.
        let p = store_path_for();
        let s = p.to_string_lossy();
        assert!(s.contains("lookxy"));
        assert!(s.ends_with("mail.db"));
        assert!(!s.contains("me_epam.com"));
        assert!(!s.contains('@'));
        // The DB sits directly under `lookxy_dir()`, with no extra path segment.
        assert_eq!(p, lookxy_dir().join("mail.db"));
    }

    #[test]
    fn opening_a_message_with_no_cached_body_requests_a_fetch() {
        use std::sync::mpsc;

        let mut app = App::for_test_with_seeded_store();
        // `for_test_with_seeded_store` wires up an inert channel pair (its
        // receiver is dropped immediately), so a send would just fail
        // silently. Swap in one whose receiver we keep, to observe what
        // `open_message` sends down it.
        let (cmd_tx, cmd_rx) = mpsc::channel();
        app.sync.cmd_tx = cmd_tx;

        app.open_message("m1");

        assert!(app.body_loading);
        assert!(app.body.is_none());
        match cmd_rx.try_recv() {
            Ok(SyncCommand::FetchBody { id }) => assert_eq!(id, "m1"),
            other => panic!("expected a FetchBody command, got {other:?}"),
        }
    }

    #[test]
    fn opening_a_message_with_a_cached_body_renders_it_without_fetching() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_body(
                "m1",
                &Body {
                    content_type: "text".into(),
                    content: "hello body".into(),
                },
            )
            .expect("seed body");

        app.open_message("m1");

        assert!(!app.body_loading);
        assert_eq!(
            app.body.as_ref().map(|b| b.content.as_str()),
            Some("hello body")
        );
    }

    #[test]
    fn reload_body_clears_state_when_nothing_is_selected() {
        let mut app = App::for_test_with_seeded_store();
        app.open_message("m1");
        assert!(app.body_loading);

        app.selected_msg = None;
        app.reload_body();

        assert!(!app.body_loading);
        assert!(app.body.is_none());
    }

    #[test]
    fn sanitize_filename_strips_separators_and_traversal() {
        assert_eq!(sanitize_filename("report.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_filename("a\\b/c"), "a_b_c");
        assert_eq!(sanitize_filename("C:\\evil"), "C__evil");
        assert_eq!(sanitize_filename(""), "attachment");
        assert_eq!(sanitize_filename("   "), "attachment");
    }

    #[test]
    fn sanitize_filename_strips_percent_to_block_cmd_var_expansion() {
        // `cmd /c start` (the Windows `o` opener) expands `%VAR%` in its
        // argument unprotected by quoting, so `%` must never survive into a
        // path handed to it.
        assert_eq!(sanitize_filename("%USERPROFILE%\\x"), "_USERPROFILE__x");
        assert_eq!(sanitize_filename("100%.txt"), "100_.txt");
    }

    #[test]
    fn downloads_dir_ends_with_downloads() {
        let d = downloads_dir();
        assert_eq!(d.file_name().unwrap(), "Downloads");
    }

    #[test]
    fn opening_attachments_popup_lists_stored_attachments() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "notes.txt".into(),
                    content_type: "text/plain".into(),
                    size: 12,
                    is_inline: false,
                }],
            )
            .expect("seed attachment");

        app.open_attachments_popup();

        let popup = app.attachments.as_ref().expect("popup open");
        assert_eq!(popup.message_id, "m1");
        assert_eq!(popup.items.len(), 1);
        assert_eq!(popup.items[0].name, "notes.txt");
    }

    #[test]
    fn attachments_popup_does_not_open_with_no_attachments() {
        let mut app = App::for_test_with_seeded_store();
        app.open_attachments_popup();
        assert!(app.attachments.is_none());
    }

    #[test]
    fn esc_closes_the_attachments_popup() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "notes.txt".into(),
                    content_type: "text/plain".into(),
                    size: 12,
                    is_inline: false,
                }],
            )
            .expect("seed attachment");
        app.open_attachments_popup();
        assert!(app.attachments.is_some());

        app.cancel_attachments_popup();

        assert!(app.attachments.is_none());
    }

    #[test]
    fn enter_saves_the_highlighted_attachment_via_sync_command() {
        use std::sync::mpsc;

        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "notes.txt".into(),
                    content_type: "text/plain".into(),
                    size: 12,
                    is_inline: false,
                }],
            )
            .expect("seed attachment");
        let (cmd_tx, cmd_rx) = mpsc::channel();
        app.sync.cmd_tx = cmd_tx;
        app.open_attachments_popup();

        app.save_attachment();

        let dest = match cmd_rx.try_recv() {
            Ok(SyncCommand::SaveAttachment {
                message_id,
                attachment_id,
                dest,
            }) => {
                assert_eq!(message_id, "m1");
                assert_eq!(attachment_id, "a1");
                assert_eq!(dest.file_name().unwrap(), "notes.txt");
                assert!(dest.starts_with(downloads_dir()));
                dest
            }
            other => panic!("expected a SaveAttachment command, got {other:?}"),
        };
        // `save_attachment` (Enter, not `o`) must not schedule an open once
        // the save completes.
        app.finish_attachment_save(dest);
        assert_eq!(app.open_invocations.get(), 0);
    }

    #[test]
    fn o_marks_the_save_to_open_once_it_completes() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "notes.txt".into(),
                    content_type: "text/plain".into(),
                    size: 12,
                    is_inline: false,
                }],
            )
            .expect("seed attachment");
        app.open_attachments_popup();

        app.save_and_open_attachment();
        let dest = downloads_dir().join("notes.txt");
        assert_eq!(app.pending_saves.get(&dest), Some(&true));

        // Once the sync engine reports the save, `finish_attachment_save`
        // must reach the (test-mode, no-op) OS-open seam exactly once — and
        // never a real process, since that would hang/pop up in CI.
        app.finish_attachment_save(dest.clone());

        assert!(app.pending_saves.is_empty());
        assert!(app.attachments.is_none());
        assert_eq!(app.open_invocations.get(), 1);
        assert_eq!(
            app.attachment_notice.as_deref(),
            Some(format!("Saved: {}", dest.display()).as_str())
        );
    }

    #[test]
    fn enter_save_does_not_invoke_the_os_open_seam() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "notes.txt".into(),
                    content_type: "text/plain".into(),
                    size: 12,
                    is_inline: false,
                }],
            )
            .expect("seed attachment");
        app.open_attachments_popup();

        app.save_attachment();
        app.finish_attachment_save(downloads_dir().join("notes.txt"));

        assert_eq!(app.open_invocations.get(), 0);
    }

    #[test]
    fn overlapping_saves_resolve_independent_open_intents() {
        // Saving attachment #1 with `o` and attachment #2 with Enter, before
        // #1's SyncEvent lands, must not cross-wire: #2 finishing first must
        // not open anything (its own intent was Enter) or close the popup
        // out from under #1, which is still pending; #1 finishing must open
        // (its own intent), and only then can the popup close.
        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[
                    AttachmentMeta {
                        id: "a1".into(),
                        name: "one.txt".into(),
                        content_type: "text/plain".into(),
                        size: 1,
                        is_inline: false,
                    },
                    AttachmentMeta {
                        id: "a2".into(),
                        name: "two.txt".into(),
                        content_type: "text/plain".into(),
                        size: 1,
                        is_inline: false,
                    },
                ],
            )
            .expect("seed attachments");
        app.open_attachments_popup();

        app.save_and_open_attachment(); // #1: "one.txt", open-after = true
        app.attachments_select(1);
        app.save_attachment(); // #2: "two.txt", open-after = false

        let dest1 = downloads_dir().join("one.txt");
        let dest2 = downloads_dir().join("two.txt");

        app.finish_attachment_save(dest2);
        assert_eq!(app.open_invocations.get(), 0);
        assert!(
            app.attachments.is_some(),
            "popup must stay open while save #1 is still pending"
        );

        app.finish_attachment_save(dest1);
        assert_eq!(app.open_invocations.get(), 1);
        assert!(app.attachments.is_none());
    }

    /// Builds a seeded-store `App` whose message "m1" has `has_attachments`
    /// set (via a re-upsert — `for_test_with_seeded_store`'s fixture starts
    /// with it `false`), with no local attachment rows for it — the "Graph
    /// says there are attachments, but we haven't fetched metadata yet"
    /// state `open_attachments_popup`'s fetch-on-demand path targets.
    fn seed_message_with_has_attachments_but_no_local_rows(app: &mut App) {
        use mailcore::graph::model::{Message, Recipient};
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m1".into(),
                    conversation_id: "c1".into(),
                    subject: "Hello".into(),
                    from: Recipient {
                        name: "Alice".into(),
                        address: "alice@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-16T10:00:00Z".into(),
                    sent: "2026-07-16T09:00:00Z".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: true,
                    importance: "normal".into(),
                    preview: "hi there".into(),
                    is_draft: false,
                },
            )
            .expect("update message to has_attachments=true");
        app.reload_messages();
    }

    #[test]
    fn a_fetches_attachment_metadata_when_none_stored_locally_but_graph_has_some() {
        let mut app = App::for_test_with_seeded_store();
        seed_message_with_has_attachments_but_no_local_rows(&mut app);
        assert!(app.messages[0].has_attachments);
        assert!(app.store.attachments("m1").unwrap().is_empty());

        app.open_attachments_popup();

        let popup = app
            .attachments
            .as_ref()
            .expect("popup should open in a loading state");
        assert!(popup.loading);
        assert!(popup.items.is_empty());

        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(
            last,
            Ok(SyncCommand::FetchAttachments { message_id }) if message_id == "m1"
        ));
    }

    #[test]
    fn reload_attachments_fills_in_the_popup_once_metadata_lands() {
        let mut app = App::for_test_with_seeded_store();
        seed_message_with_has_attachments_but_no_local_rows(&mut app);
        app.open_attachments_popup();
        assert!(app.attachments.as_ref().unwrap().loading);

        // The sync engine's fetch has now landed and stored the metadata.
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "f.txt".into(),
                    content_type: "text/plain".into(),
                    size: 3,
                    is_inline: false,
                }],
            )
            .expect("seed attachment");

        app.reload_attachments("m1");

        let popup = app.attachments.as_ref().expect("popup stays open");
        assert!(!popup.loading);
        assert_eq!(popup.items.len(), 1);
        assert_eq!(popup.items[0].name, "f.txt");
    }

    #[test]
    fn reload_attachments_ignores_updates_for_a_different_message() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "f.txt".into(),
                    content_type: "text/plain".into(),
                    size: 3,
                    is_inline: false,
                }],
            )
            .expect("seed attachment");
        app.open_attachments_popup();
        assert!(app.attachments.is_some());

        app.reload_attachments("some-other-message-id");

        assert_eq!(app.attachments.as_ref().unwrap().items.len(), 1);
    }

    #[test]
    fn signin_required_shows_prompt_and_enter_sends_signin() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::SignInRequired);
        assert!(app.render_contains("sign in"));
        app.on_key_enter();
        assert!(app.last_sent_command_is_signin());
    }

    #[test]
    fn signin_started_shows_browser_message() {
        let mut app = App::for_test_with_seeded_store();
        // in test mode the browser-open is stubbed (a flag), not actually launched
        app.on_sync_event(SyncEvent::SignInStarted {
            authorize_url:
                "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?x=1".into(),
        });
        assert!(app.render_contains("browser"));
        assert!(app.browser_open_was_requested());
    }

    #[test]
    fn enter_on_the_started_modal_does_not_resend_signin() {
        // Only the `Required` prompt has anything for Enter to do; once the
        // browser is already open (`Started`), Enter must be a no-op rather
        // than re-triggering the sign-in command.
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::SignInStarted {
            authorize_url:
                "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?x=1".into(),
        });
        app.on_key_enter();
        assert!(!app.last_sent_command_is_signin());
    }

    #[test]
    fn folders_updated_clears_the_signin_modal() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::SignInRequired);
        assert!(app.signin_modal.is_some());

        app.on_sync_event(SyncEvent::FoldersUpdated);

        assert!(app.signin_modal.is_none());
    }

    #[test]
    fn idle_state_clears_the_signin_modal() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::SignInStarted {
            authorize_url: "https://login.microsoftonline.com/x?y=1".into(),
        });
        assert!(app.signin_modal.is_some());

        app.on_sync_event(SyncEvent::State(SyncState::Idle));

        assert!(app.signin_modal.is_none());
    }

    #[test]
    fn empty_store_app_has_no_account_and_no_signin_modal() {
        let app = App::for_test_with_empty_store();
        assert!(app.account.is_none());
        assert!(app.signin_modal.is_none());
    }

    #[test]
    fn sync_error_is_surfaced_and_rendered_in_the_status_bar() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::Error("boom".into()));
        assert_eq!(app.error_notice.as_deref(), Some("boom"));
        assert!(app.render_contains("boom"));
    }

    #[test]
    fn a_successful_sync_state_clears_a_prior_error_notice() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::Error("boom".into()));
        assert!(app.error_notice.is_some());
        app.on_sync_event(SyncEvent::State(SyncState::Idle));
        assert!(app.error_notice.is_none());
    }

    #[test]
    fn a_non_signin_state_after_sign_in_clears_the_modal() {
        // A transient failure right after a successful redeem emits `Syncing`
        // then `Offline` — neither is `Idle`/`FoldersUpdated`, but both mean
        // auth already succeeded, so the modal must clear rather than stay
        // stuck blocking all keys.
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::SignInStarted {
            authorize_url: "https://login.microsoftonline.com/x?y=1".into(),
        });
        assert!(app.signin_modal.is_some());

        app.on_sync_event(SyncEvent::State(SyncState::Syncing));
        assert!(app.signin_modal.is_none());
    }

    #[test]
    fn state_signin_required_keeps_the_modal_showing() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::SignInRequired);
        assert!(app.signin_modal.is_some());

        // The `State(SignInRequired)` that `enter_signin` emits alongside the
        // `SignInRequired` event must NOT clear the modal it just opened.
        app.on_sync_event(SyncEvent::State(SyncState::SignInRequired));
        assert!(app.signin_modal.is_some());
    }

    #[test]
    fn search_prompt_captures_text_so_q_is_not_a_global_quit() {
        let mut app = App::for_test_with_seeded_store();
        assert!(!app.is_capturing_text());
        app.start_search();
        assert!(app.is_capturing_text());
        app.cancel_search();
        assert!(!app.is_capturing_text());
    }

    // --- Compose entry points / drafts resume / send-save wiring ----------

    #[test]
    fn c_key_creates_a_fresh_local_draft_and_opens_the_composer() {
        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('c');

        let compose = app.compose.as_ref().expect("compose should be open");
        assert_eq!(compose.to, "");
        assert_eq!(compose.cc, "");
        assert_eq!(compose.subject, "");
        // The composer is editing a real store row, not just UI state.
        assert!(app.store.draft(&compose.draft_id).unwrap().is_some());
    }

    #[test]
    fn r_key_sends_compose_reply_for_the_highlighted_message() {
        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('r');

        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(
            last,
            Ok(SyncCommand::ComposeReply { id, all }) if id == "m1" && !all
        ));
    }

    #[test]
    fn shift_r_key_sends_compose_reply_all_for_the_highlighted_message() {
        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('R');

        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(
            last,
            Ok(SyncCommand::ComposeReply { id, all }) if id == "m1" && all
        ));
    }

    #[test]
    fn shift_f_key_sends_compose_forward_for_the_highlighted_message() {
        // Lowercase 'f' is already the flag-toggle key (see `on_key_char`),
        // so forward is bound to uppercase 'F' instead of the brief's bare
        // `f` — see `App::compose_forward`'s doc comment for the full note.
        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('F');

        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(
            last,
            Ok(SyncCommand::ComposeForward { id }) if id == "m1"
        ));
    }

    #[test]
    fn lowercase_f_still_toggles_the_flag_unaffected_by_the_forward_binding() {
        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('f');
        let rows = app.store.messages_in_folder("inbox", 50, 0).unwrap();
        assert!(rows[0].is_flagged);
    }

    #[test]
    fn draft_ready_event_opens_the_composer_loaded_from_the_store() {
        let mut app = App::for_test_with_seeded_store();
        let id = app
            .store
            .create_local_draft(
                "Re: Hi",
                "alice@example.com",
                "carol@example.com",
                "<p>Hello</p>",
            )
            .unwrap();

        app.on_sync_event(SyncEvent::DraftReady { id: id.clone() });

        let compose = app.compose.as_ref().expect("compose should be open");
        assert_eq!(compose.subject, "Re: Hi");
        assert_eq!(compose.to, "alice@example.com");
        assert_eq!(compose.cc, "carol@example.com");
        assert_eq!(compose.editor.text.plain(), "Hello");
        assert_eq!(compose.draft_id, id);
    }

    #[test]
    fn draft_ready_for_an_unknown_id_does_not_open_the_composer() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::DraftReady {
            id: "no-such-draft".into(),
        });
        assert!(app.compose.is_none());
    }
}
