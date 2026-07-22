//! `App` — the TUI's in-memory state: the local mail store, the background
//! sync handle, where the three panes (folders/list/reading) currently
//! point, the triage actions (`m`/`u`/`f`/`d`/`v`) that mutate them, and
//! compose's entry points (`c`/`r`/`R`/`F`, drafts resume, send/save/discard
//! wiring — see the "Compose" section below).

use std::path::{Path, PathBuf};

use crate::ui::compose::{Compose, ComposeAction, ComposeField};
use editcore::ops::Editor;
use mailcore::compose_html;
use mailcore::graph::client::RsvpKind;
use mailcore::graph::model::{
    AttachmentKind, AttachmentMeta, AutomaticReplies, Body, MasterCategory, OofStatus, Recurrence,
    RecurrenceKind,
};
use mailcore::store::{EventRow, FolderRow, MessageRow, Store};
use mailcore::sync::engine::{SyncCommand, SyncEvent, SyncHandle, SyncState};

/// Which pane currently has keyboard focus. Tab cycles `Folders` → `List` →
/// `Reading` → `Folders` (see `ui::handle_key`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// Level 0: the Mail/Calendar rail. Left of the folder pane.
    Rail,
    Folders,
    List,
    Reading,
}

/// The top-level view: the mail three-pane layout, or the calendar agenda +
/// detail view (`ui::calendar`). `g` toggles between the two (see
/// `App::toggle_mode`) — free to bind: unlike the brief's forward key
/// (bare `f`, already claimed by flag-toggle — see `App::compose_forward`'s
/// doc comment), no existing triage/pane key already uses `g`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mail,
    Calendar,
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
    /// The folder pane's rendered tree: `folders` flattened depth-first with
    /// only the rows reachable through expanded ancestors (see
    /// `rebuild_visible_folders`). `folder_index` indexes THIS, not `folders`.
    pub visible_folders: Vec<crate::ui::foldertree::VisibleFolder>,
    /// Index into `visible_folders` of the currently highlighted row.
    pub folder_index: usize,
    pub messages: Vec<MessageRow>,
    /// Index into `messages` of the currently highlighted row.
    pub msg_index: usize,
    /// Whether the folder view groups messages into conversations. Seeded
    /// from `Config::threaded`; toggled by `t`.
    pub threaded: bool,
    /// Path used to persist the `threaded` toggle. `None` in tests (no disk
    /// write); `Some` in production (set by `main`). Read by
    /// `toggle_threaded` when persisting the `t`-keybinding choice.
    pub config_path: Option<PathBuf>,
    /// Seeded from `Config::signature`. Appended to a brand-new message's
    /// body by `compose_new` (via `signature_body_html`); reply/forward
    /// bodies come from Graph untouched, so nothing else reads this.
    pub signature: String,
    /// The threaded view-model, built by `reload_messages` when `threaded`.
    pub threads: Vec<ThreadView>,
    /// Flattened header+message rows for render + navigation in threaded mode.
    pub visible_rows: Vec<Row>,
    /// Cursor into `visible_rows` (threaded mode's equivalent of `msg_index`).
    pub row_index: usize,
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
    /// The top-level view — mail three-pane, or the calendar agenda +
    /// detail (`ui::calendar`). Toggled by `g` (see `toggle_mode`).
    pub mode: Mode,
    /// The events in the current agenda window (`ui::calendar::agenda_window`),
    /// as read from `Store::events_in_window` — the calendar's equivalent of
    /// `messages`. Reloaded on entering Calendar mode and whenever
    /// `SyncEvent::CalendarUpdated` lands (see `reload_agenda`).
    pub agenda: Vec<EventRow>,
    /// Index into `agenda` of the currently highlighted row — the calendar's
    /// equivalent of `msg_index`. Navigated with clamped (not wrapping)
    /// bounds (see `move_agenda_selection`), since headers aren't part of
    /// this vec at all (`ui::calendar::agenda_lines` inserts them purely for
    /// display), this can index straight into `agenda` without needing to
    /// skip anything.
    pub agenda_index: usize,
    /// The event opened in the detail pane (Enter on the agenda), if any —
    /// the calendar's equivalent of `selected_msg`. Independent of
    /// `agenda_index`, same as the reading pane's `selected_msg` doesn't
    /// track `msg_index` — see `open_selected_event`.
    pub selected_event: Option<String>,
    /// The in-progress RSVP comment prompt (`a`/`d`/`t` on the highlighted
    /// agenda row), if any — see `start_rsvp`. Captures which event and
    /// which response kind up front (same "capture the target before the
    /// popup can be renavigated out from under it" shape as `MovePicker`),
    /// so typing into the comment can never end up attached to a different
    /// event than the one the key was pressed on.
    pub rsvp_prompt: Option<RsvpPrompt>,
    /// A pending destructive-action confirmation, if any (whole-thread delete
    /// or move). `Some` blocks other keys until answered — see `ui::handle_key`.
    pub confirm: Option<ConfirmModal>,
    /// The open file-picker popup (opened to choose a file to attach to the
    /// message being composed), if any — see `ui::filepicker`. `Some` takes
    /// keys ahead of the compose view it's drawn over (see `ui::handle_key`).
    pub file_picker: Option<crate::ui::filepicker::FilePicker>,
    /// The open create/edit event form (`c`/`e` in Calendar mode — bound in a
    /// later task alongside this module's own key handling), if any — see
    /// `ui::eventform`. `Some` is drawn as an overlay over the calendar
    /// (`ui::draw`'s Calendar branch); populated by `open_new_event`/
    /// `open_edit_event`, cleared by `save_event_form`/Esc (later tasks).
    pub event_form: Option<crate::ui::eventform::EventForm>,
    /// The automatic-replies (out-of-office) editor overlay, when open
    /// (opened by `O`; see `App::open_oof_form`).
    pub oof_form: Option<crate::ui::oofform::OofForm>,
    /// The mailbox's master category list (name→color), for rendering category
    /// dots/chips and the picker's choices. Loaded from the store on
    /// `CategoriesUpdated` and at startup.
    pub master_categories: Vec<MasterCategory>,
    /// The category picker overlay (assign or filter), when open.
    pub category_picker: Option<crate::ui::categorypicker::CategoryPicker>,
    /// The active category filter (`L`), or `None`. When set, `reload_messages`
    /// shows only messages carrying this category (flat view).
    pub category_filter: Option<String>,
    /// When true, a firing reminder also raises an agwinterm overlay (see
    /// `notify_agwinterm`). Set from `Config::reminders_notify`; default false.
    pub reminders_notify: bool,
    /// Whether the one-time folder-tree default (expand the Inbox) has run.
    /// Loaded from `Config::folder_tree_initialized`; set true and persisted the
    /// first time `ensure_folder_tree_initialized` expands the Inbox, so a later
    /// user collapse is respected across restarts.
    pub folder_tree_initialized: bool,
    /// Event ids already alerted this session (fire-once de-dup).
    pub alerted_reminders: std::collections::HashSet<String>,
    /// Pending reminder banner lines (front = currently shown).
    pub reminder_queue: std::collections::VecDeque<String>,
    #[cfg(test)]
    pub agwinterm_notify_invocations: std::cell::Cell<u32>,
    /// The free/busy availability overlay (opened by `Ctrl-B` in the event
    /// form), when open.
    pub free_busy: Option<crate::ui::freebusy::FreeBusyView>,
    /// Whether the read-only help overlay (`F1`/`?`) is open. A modal like the
    /// others: while open it captures every key and closes on Esc/F1/?/q.
    pub help: bool,
    /// The reading pane's vertical scroll offset, in body rows — reset to `0`
    /// whenever a different message is opened (`open_message`). Clamped by
    /// `reading_scroll_by`/`reading_scroll_page`/`reading_scroll_home`/
    /// `reading_scroll_end` against `reading_content_rows`/`reading_viewport`.
    pub reading_scroll: usize,
    /// The reading pane body's visible height in rows, as last recorded by
    /// `ui::reading::draw` (which has `&mut App` for exactly this). Used only
    /// to clamp `reading_scroll`.
    pub reading_viewport: usize,
    /// The reading pane body's total row count (text lines plus each inline
    /// image's reserved `IMAGE_BOX_ROWS` band), as last recorded by
    /// `ui::reading::draw`. Used only to clamp `reading_scroll`.
    pub reading_content_rows: usize,
    /// Inline-image bytes fetched for the currently opened HTML body, keyed
    /// by `content_id` (the part after `cid:` in an `<img>` `src`) — see
    /// `request_inline_images`. Cleared whenever a different message is
    /// opened (`open_message`); painting the boxes onto pixels is a later
    /// task's concern, this is just the in-memory cache it'll read.
    pub inline_images: std::collections::HashMap<String, Vec<u8>>,
    /// `content_id`s for which `SyncCommand::FetchInlineImage` has already
    /// been sent for the currently opened message, so `request_inline_images`
    /// doesn't re-fire on every call (e.g. once per `AttachmentsUpdated`).
    /// Cleared alongside `inline_images` in `open_message`.
    requested_inline: std::collections::HashSet<String>,
    /// Whether `SyncCommand::FetchAttachments` has already been sent for the
    /// currently opened message's inline-image resolution. `open_message`
    /// calls `request_inline_images` once itself and once more indirectly
    /// through `reload_body`'s cache-hit branch; both calls see empty
    /// attachment metadata on a message whose metadata hasn't landed yet, so
    /// without this guard each would fire its own `FetchAttachments`. Reset
    /// to `false` in `open_message` alongside `inline_images`/`requested_inline`.
    inline_attachments_requested: bool,
    /// The terminal graphics capability, detected once at startup
    /// (`main::main`) via `Picker::from_query_stdio` (falling back to a fixed
    /// font-cell size). `None` in every test `App` (`for_test_with_seeded_store`/
    /// `for_test_with_empty_store` never set it), so tests always exercise
    /// `ui::reading`'s fallback-box path; real pixels only ever paint on the
    /// live TUI path. See `ui::reading::paint_inline_image`.
    pub picker: Option<ratatui_image::picker::Picker>,
    /// Decoded+encoded protocol cache for painted inline images, keyed by
    /// `ui::reading::paint_inline_image`'s cache key (content-id/data-hash
    /// plus the band's rendered dimensions) — so re-encoding only happens
    /// once per (image, size), not every frame. Cleared alongside
    /// `inline_images` in `open_message`, since a newly opened message's cids
    /// are unrelated to whatever was cached for the previous one.
    pub image_protocols: std::collections::HashMap<String, ratatui_image::protocol::Protocol>,
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
    /// The full set of a thread's message ids, captured at open time, when
    /// the picker was opened on a threaded multi-message conversation
    /// (`None` for a flat/single-message target). `confirm_move` uses this
    /// captured set rather than re-deriving `threaded_target_ids()` at Enter
    /// time, so a background sync reload between open and confirm (which
    /// rebuilds `threads`/`visible_rows` under an unchanged `row_index`)
    /// can't retarget the move to a different thread than the one the user
    /// opened the picker on.
    pub thread_ids: Option<Vec<String>>,
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

/// Which RSVP surface a prompt is for: a calendar `Event` (→ `RespondEvent`)
/// or a mail-reader meeting invite `Message` (→ `RespondMeeting`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RsvpTarget {
    Event(String),
    Message(String),
}

/// The focused field in the RSVP prompt. Proposed-time fields only apply to
/// decline/tentative (accept shows only `Comment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsvpField {
    ProposedStart,
    ProposedEnd,
    Comment,
}

/// State for the RSVP prompt opened by `a`/`d`/`t` in Calendar mode or `D`/`T`
/// on a meeting invite in the mail reader. `kind` is the response-status
/// vocabulary (`"accepted"`/`"declined"`/`"tentativelyAccepted"`). The
/// proposed-time fields hold local-time text (`""` = no proposal) and only
/// apply to decline/tentative.
pub struct RsvpPrompt {
    pub target: RsvpTarget,
    pub kind: String,
    pub comment: String,
    pub proposed_start: String,
    pub proposed_end: String,
    pub focus: RsvpField,
}

impl RsvpPrompt {
    /// True when this RSVP kind can carry a proposed new time (decline or
    /// tentative — never accept).
    pub fn proposes(&self) -> bool {
        self.kind == "declined" || self.kind == "tentativelyAccepted"
    }
}

/// A conversation in the threaded folder view, plus whether it's expanded.
pub struct ThreadView {
    pub thread: mailcore::thread::Thread,
    pub expanded: bool,
}

/// A pending destructive confirmation (whole-conversation delete/move).
pub struct ConfirmModal {
    pub prompt: String,
    pub action: ConfirmAction,
}

pub enum ConfirmAction {
    DeleteThread(Vec<String>),
    MoveThread(Vec<String>, String), // (message ids, destination folder id)
    DeleteEvent(String),
}

/// One visible line in the threaded list: a collapsible conversation header,
/// or (only under an expanded header) one of its messages. A single-message
/// conversation is represented directly as a `Message` row with no header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Header(usize),         // index into `threads`
    Message(usize, usize), // (thread index, message index within the thread)
}

/// Normalizes a Content-ID for `cid:`-to-attachment comparison: trims
/// surrounding whitespace, then strips one leading `<` and one trailing `>`
/// if present. Graph's `contentId` may come back either bare (`logo@x`) or
/// angle-bracketed (`<logo@x>`); an HTML body's `cid:` token is usually bare
/// but can also be bracketed. Comparing the two forms directly with `==`
/// fails whenever they disagree, silently falling back every cid image on
/// the message to the bordered box. Comparison stays case-sensitive —
/// Content-IDs are case-sensitive per RFC 2045.
fn normalize_cid(s: &str) -> &str {
    let trimmed = s.trim();
    let no_prefix = trimmed.strip_prefix('<').unwrap_or(trimmed);
    no_prefix.strip_suffix('>').unwrap_or(no_prefix)
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
            visible_folders: Vec::new(),
            folder_index: 0,
            messages: Vec::new(),
            msg_index: 0,
            threaded: false,
            config_path: None,
            signature: String::new(),
            threads: Vec::new(),
            visible_rows: Vec::new(),
            row_index: 0,
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
            mode: Mode::Mail,
            agenda: Vec::new(),
            agenda_index: 0,
            selected_event: None,
            rsvp_prompt: None,
            confirm: None,
            file_picker: None,
            event_form: None,
            oof_form: None,
            master_categories: Vec::new(),
            category_picker: None,
            category_filter: None,
            reminders_notify: false,
            folder_tree_initialized: false,
            alerted_reminders: std::collections::HashSet::new(),
            reminder_queue: std::collections::VecDeque::new(),
            #[cfg(test)]
            agwinterm_notify_invocations: std::cell::Cell::new(0),
            free_busy: None,
            help: false,
            reading_scroll: 0,
            reading_viewport: 0,
            reading_content_rows: 0,
            inline_images: std::collections::HashMap::new(),
            requested_inline: std::collections::HashSet::new(),
            inline_attachments_requested: false,
            picker: None,
            image_protocols: std::collections::HashMap::new(),
        };
        app.reload_folders();
        app.reload_account();
        app.reload_master_categories();
        app
    }

    /// Re-reads the master category list from the store (`Store::master_categories`).
    pub fn reload_master_categories(&mut self) {
        self.master_categories = self.store.master_categories().unwrap_or_default();
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
                // First real folder sync is where a fresh install finally has an
                // Inbox to auto-expand (the `App::new` load saw an empty store).
                self.ensure_folder_tree_initialized();
            }
            SyncEvent::MessagesUpdated { folder_id }
                if self.selected_folder.as_deref() == Some(folder_id.as_str()) =>
            {
                self.reload_messages();
            }
            SyncEvent::BodyReady { id } if self.selected_msg.as_deref() == Some(id.as_str()) => {
                self.reload_body();
            }
            SyncEvent::AttachmentsUpdated { message_id } => {
                self.reload_attachments(&message_id);
                // Metadata just landed — cids can resolve now, but only if
                // this is the message currently open in the reading pane
                // (`request_inline_images` acts on `selected_msg`, not on
                // `message_id`, so guard here rather than let it silently
                // re-resolve for whatever else happens to be open).
                if self.selected_msg.as_deref() == Some(message_id.as_str()) {
                    self.request_inline_images();
                }
            }
            SyncEvent::AttachmentSaved { path } => self.finish_attachment_save(path),
            SyncEvent::MeetingResponded { message_id, kind } => {
                if self.selected_msg.as_deref() == Some(message_id.as_str()) {
                    self.attachment_notice = Some(
                        match kind {
                            RsvpKind::Accept => "Accepted the invite",
                            RsvpKind::Decline => "Declined the invite",
                            RsvpKind::Tentative => "Tentatively accepted the invite",
                        }
                        .to_string(),
                    );
                }
                // Mark the invite read locally (a small courtesy) and push the
                // change to Graph via the existing read path.
                self.store.set_read(&message_id, true);
                self.reload_messages();
                let _ = self.sync.cmd_tx.send(SyncCommand::MarkRead {
                    id: message_id,
                    read: true,
                });
            }
            // A reply/forward draft (`SyncCommand::ComposeReply`/
            // `ComposeForward`) just landed in the store — open the composer
            // on it (see `open_draft`).
            SyncEvent::DraftReady { id } => self.open_draft(&id),
            SyncEvent::SignInRequired => self.signin_modal = Some(SignInModal::Required),
            SyncEvent::SignInStarted { authorize_url } => {
                self.open_url_with_os_handler(&authorize_url);
                self.signin_modal = Some(SignInModal::Started { authorize_url });
            }
            SyncEvent::AutomaticRepliesFetched { replies } => {
                if let Some(form) = self.oof_form.as_mut() {
                    let off = crate::ui::calendar::local_offset_minutes();
                    form.prefill(&replies, off);
                    form.loading = false;
                }
            }
            SyncEvent::AutomaticRepliesUpdated => {
                self.oof_form = None;
                self.attachment_notice = Some("Automatic replies updated".to_string());
            }
            SyncEvent::CategoriesUpdated => self.reload_master_categories(),
            SyncEvent::ScheduleFetched { entries } => {
                if let Some(v) = self.free_busy.as_mut() {
                    v.entries = entries;
                    v.loading = false;
                }
            }
            SyncEvent::Error(msg) => {
                // A fetch failure while the OOF form / free-busy overlay is open
                // must clear its loading state so it isn't stuck on "loading…".
                if let Some(form) = self.oof_form.as_mut() {
                    form.loading = false;
                }
                if let Some(v) = self.free_busy.as_mut() {
                    v.loading = false;
                }
                self.error_notice = Some(msg);
            }
            // The events store changed (a `RefreshCalendar`/`RespondEvent`
            // just landed) — re-read the agenda window so the calendar view
            // reflects it. Unlike `MessagesUpdated`'s folder check, this
            // doesn't gate on `mode == Calendar`: reloading a view that
            // isn't currently showing is harmless, and keeps `agenda` correct
            // for the moment `g` shows it again rather than reloading late.
            SyncEvent::CalendarUpdated => self.reload_agenda(),
            // `Sent` has no TUI consumer yet: the composer already closes
            // optimistically the moment Send is pressed (`apply_compose_action`),
            // before the engine's outbox drain even reaches Graph, so by the
            // time this lands there's nothing left open for it to affect —
            // it's purely a "yes, it actually got delivered" confirmation.
            // Folded into the existing catch-all rather than given a
            // dedicated arm so this compiles now without inventing a
            // toast/notice mechanism the brief doesn't ask for.
            // Cache the bytes by `content_id` — but only if they're still
            // for the message currently open; a slow fetch for a message
            // the user has since navigated away from is simply dropped
            // (matches `BodyReady`'s guard above).
            SyncEvent::InlineImageReady {
                message_id,
                content_id,
                bytes,
            } if self.selected_msg.as_deref() == Some(message_id.as_str()) => {
                self.inline_images.insert(content_id, bytes);
            }
            SyncEvent::InlineImageReady { .. } => {} // for a message no longer open — drop
            SyncEvent::MessagesUpdated { .. }
            | SyncEvent::BodyReady { .. }
            | SyncEvent::Sent { .. } => {}
        }
    }

    /// Whether a modal is currently capturing keystrokes — the search prompt
    /// (`/`), the compose view's fields/body, the RSVP comment prompt
    /// (`a`/`d`/`t` in Calendar mode), the OOF form, the event form's text
    /// fields, or the read-only free/busy overlay. The event loop consults
    /// this so a global hotkey like `q`-to-quit doesn't steal a character the
    /// user is typing into the query, a compose field, an RSVP comment, or a
    /// meeting title (searching for "quarterly", composing a message that
    /// mentions "quit", declining with "can't make it, quick call instead",
    /// or naming an event "Q3 review" must not quit the app). The free/busy
    /// overlay captures no text but is modal: `q` should not quit the app out
    /// from under it — only `Esc` closes it.
    pub fn is_capturing_text(&self) -> bool {
        self.search.is_some()
            || self.compose.is_some()
            || self.rsvp_prompt.is_some()
            || self.oof_form.is_some()
            || self.event_form.is_some()
            || self.free_busy.is_some()
            || self.help
    }

    /// Opens the read-only help overlay (`F1`/`?`).
    pub fn open_help(&mut self) {
        self.help = true;
    }

    /// Closes the help overlay.
    pub fn close_help(&mut self) {
        self.help = false;
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
        self.rebuild_visible_folders();
        self.reload_messages();
    }

    /// Rebuilds `visible_folders` (the flattened, collapse-aware tree) from
    /// `folders`, then reconciles the selection: keep `folder_index` pointing at
    /// `selected_folder` when that row is still visible; otherwise clamp the
    /// index into range and re-derive `selected_folder` from it (so a collapse
    /// that hid the selection, or a folder that vanished on sync, still leaves a
    /// valid highlighted row). Called by `reload_folders` and after every
    /// expand/collapse.
    pub fn rebuild_visible_folders(&mut self) {
        self.visible_folders = crate::ui::foldertree::build_visible(&self.folders);
        let pos = self
            .selected_folder
            .as_ref()
            .and_then(|id| self.visible_folders.iter().position(|v| &v.row.id == id));
        match pos {
            Some(idx) => self.folder_index = idx,
            None => {
                self.folder_index = self
                    .folder_index
                    .min(self.visible_folders.len().saturating_sub(1));
                self.selected_folder = self
                    .visible_folders
                    .get(self.folder_index)
                    .map(|v| v.row.id.clone());
            }
        }
    }

    /// Persists a folder's expand flag, updates the cached `folders` row to
    /// match (so `rebuild_visible_folders`, which reads the cache, sees the new
    /// state without a store round-trip), and rebuilds the visible tree.
    fn set_folder_expanded(&mut self, id: &str, expanded: bool) {
        let _ = self.store.set_folder_expanded(id, expanded);
        if let Some(f) = self.folders.iter_mut().find(|f| f.id == id) {
            f.is_expanded = expanded;
        }
        self.rebuild_visible_folders();
    }

    /// One-time first-run default: expand the Inbox so its subfolders are
    /// visible out of the box. A no-op once `folder_tree_initialized` is set (so
    /// a later user collapse sticks), and a no-op until the Inbox has actually
    /// synced in — so on a fresh install it stays pending across the empty
    /// `App::new` load and fires when `FoldersUpdated` first brings folders.
    /// Called from `main` after config is wired and from the `FoldersUpdated`
    /// handler.
    pub fn ensure_folder_tree_initialized(&mut self) {
        if self.folder_tree_initialized {
            return;
        }
        let Some(inbox) = self
            .folders
            .iter()
            .find(|f| f.well_known_name.as_deref() == Some("inbox"))
        else {
            return; // Inbox not synced yet — try again on the next reload.
        };
        let id = inbox.id.clone();
        self.set_folder_expanded(&id, true);
        self.folder_tree_initialized = true;
        if let Some(path) = &self.config_path {
            let _ = crate::config::persist_folder_tree_initialized_to(path, true);
        }
    }

    /// Folder pane `→`/`l`: expand the selected folder if it has collapsed
    /// children.
    pub fn expand_selected(&mut self) {
        let Some(v) = self.visible_folders.get(self.folder_index) else {
            return;
        };
        if v.has_children && !v.expanded {
            let id = v.row.id.clone();
            self.set_folder_expanded(&id, true);
        }
    }

    /// Folder pane `←`/`h`: if the selected folder is expanded, collapse it;
    /// otherwise move the selection up to its parent (Outlook behavior). A
    /// no-op on a collapsed top-level leaf.
    pub fn collapse_or_parent(&mut self) {
        let Some(v) = self.visible_folders.get(self.folder_index) else {
            return;
        };
        if v.has_children && v.expanded {
            let id = v.row.id.clone();
            self.set_folder_expanded(&id, false);
        } else if let Some(parent) = v.row.parent_id.clone() {
            if let Some(idx) = self.visible_folders.iter().position(|x| x.row.id == parent) {
                self.folder_index = idx;
                self.selected_folder = Some(parent);
                self.msg_index = 0;
                self.reload_messages();
            }
        }
    }

    /// Folder pane `Space`: flip the selected folder's expand state (no-op on a
    /// leaf).
    pub fn toggle_selected_folder(&mut self) {
        let Some(v) = self.visible_folders.get(self.folder_index) else {
            return;
        };
        if v.has_children {
            let (id, expanded) = (v.row.id.clone(), v.expanded);
            self.set_folder_expanded(&id, !expanded);
        }
    }

    /// True when the threaded folder view is what's on screen: threading is on,
    /// no search is active (search results stay flat), and we're in Mail mode.
    pub fn threaded_active(&self) -> bool {
        self.threaded && self.search.is_none() && self.mode == Mode::Mail
    }

    /// Rebuilds `visible_rows` from `threads` + their expanded flags, and
    /// clamps `row_index` into range. A single-message thread contributes one
    /// bare `Message` row (no header); a multi-message thread contributes a
    /// `Header` and, when expanded, its child `Message` rows.
    pub fn rebuild_visible_rows(&mut self) {
        let mut rows = Vec::new();
        for (t, tv) in self.threads.iter().enumerate() {
            if tv.thread.messages.len() == 1 {
                rows.push(Row::Message(t, 0));
            } else {
                rows.push(Row::Header(t));
                if tv.expanded {
                    for m in 0..tv.thread.messages.len() {
                        rows.push(Row::Message(t, m));
                    }
                }
            }
        }
        if self.row_index >= rows.len() {
            self.row_index = rows.len().saturating_sub(1);
        }
        self.visible_rows = rows;
    }

    /// Re-reads the selected folder's messages from the store. In flat mode
    /// this fills `messages` (newest first) and clamps `msg_index`. In
    /// threaded mode it instead builds `threads` (cross-folder conversations)
    /// and rebuilds `visible_rows`, preserving each thread's expanded state by
    /// conversation key across the rebuild.
    pub fn reload_messages(&mut self) {
        let Some(folder) = self.selected_folder.clone() else {
            self.messages.clear();
            self.threads.clear();
            self.visible_rows.clear();
            self.msg_index = 0;
            self.row_index = 0;
            return;
        };
        if self.threaded && self.category_filter.is_none() {
            let expanded: std::collections::HashSet<String> = self
                .threads
                .iter()
                .filter(|tv| tv.expanded)
                .map(|tv| tv.thread.key.clone())
                .collect();
            let rows = self
                .store
                .conversations_in_folder(&folder, MESSAGE_PAGE_SIZE, 0)
                .unwrap_or_default();
            self.threads = mailcore::thread::build_threads(&rows)
                .into_iter()
                .map(|thread| {
                    let expanded = expanded.contains(&thread.key);
                    ThreadView { thread, expanded }
                })
                .collect();
            self.rebuild_visible_rows();
        } else {
            self.messages = self
                .store
                .messages_in_folder(&folder, MESSAGE_PAGE_SIZE, 0)
                .unwrap_or_default();
            if let Some(cat) = &self.category_filter {
                self.messages
                    .retain(|m| m.categories.iter().any(|c| c == cat));
            }
            if self.msg_index >= self.messages.len() {
                self.msg_index = self.messages.len().saturating_sub(1);
            }
        }
    }

    /// The `visible_rows` entry the cursor is on, if any.
    fn selected_row(&self) -> Option<Row> {
        self.visible_rows.get(self.row_index).copied()
    }

    /// Moves the threaded-list cursor by `delta`, clamped to `[0, len)` (no
    /// wrap — a header and its children read as one block, so wrapping the
    /// cursor off either end would be disorienting).
    pub fn move_thread_selection(&mut self, delta: isize) {
        let len = self.visible_rows.len();
        if len == 0 {
            return;
        }
        let max = (len - 1) as isize;
        let next = (self.row_index as isize + delta).clamp(0, max);
        self.row_index = next as usize;
    }

    /// Enter on the highlighted threaded row. On a header: toggle expansion,
    /// and when it becomes expanded, open the thread's latest message in the
    /// reading pane. On a message row: open that message (a draft opens in the
    /// composer, matching the flat list's activate behavior).
    pub fn activate_thread_row(&mut self) {
        match self.selected_row() {
            Some(Row::Header(t)) => {
                let expanding = !self.threads[t].expanded;
                self.threads[t].expanded = expanding;
                if expanding {
                    if let Some(latest) = self.threads[t].thread.messages.last() {
                        let (id, is_draft) = (latest.id.clone(), latest.is_draft);
                        if is_draft {
                            self.open_draft(&id);
                        } else {
                            self.open_message(&id);
                            self.mark_message_read(&id);
                            self.focus = Pane::Reading;
                        }
                    }
                }
                self.rebuild_visible_rows();
            }
            Some(Row::Message(t, m)) => {
                if let Some(msg) = self.threads[t].thread.messages.get(m) {
                    let (id, is_draft) = (msg.id.clone(), msg.is_draft);
                    if is_draft {
                        self.open_draft(&id);
                    } else {
                        self.open_message(&id);
                        self.mark_message_read(&id);
                        self.focus = Pane::Reading;
                    }
                }
            }
            None => {}
        }
    }

    /// Marks exactly `id` read (optimistic store write + `MarkRead` command +
    /// `reload_messages`) — the activate path's targeted mark-read, distinct
    /// from `mark_read`, which is cursor-based and reads a whole thread when the
    /// cursor sits on a header.
    fn mark_message_read(&mut self, id: &str) {
        self.store.set_read(id, true);
        let _ = self.sync.cmd_tx.send(SyncCommand::MarkRead {
            id: id.to_string(),
            read: true,
        });
        self.reload_messages();
    }

    /// Enter / activate on the current selection: `Folders` → enter the list;
    /// `List` → open the highlighted message into the reader (marking it read)
    /// or, in threaded mode, run the header/message activate. Drafts open in the
    /// composer. `Rail`/`Reading` have nothing to activate.
    pub fn activate_selected(&mut self) {
        match self.focus {
            Pane::Folders => self.focus = Pane::List,
            Pane::List => {
                if self.threaded_active() {
                    self.activate_thread_row();
                } else if let Some(msg) = self.messages.get(self.msg_index) {
                    let (id, is_draft) = (msg.id.clone(), msg.is_draft);
                    if is_draft {
                        self.open_draft(&id);
                    } else {
                        self.open_message(&id);
                        self.mark_message_read(&id);
                        self.focus = Pane::Reading;
                    }
                }
            }
            Pane::Rail | Pane::Reading => {}
        }
    }

    /// Right-arrow on the message list: on a collapsed thread header, expand it
    /// and drop to its first child (staying in the list); on a message row (or
    /// the flat list), activate it into the reader. Mirrors the folder pane's
    /// "expand first, enter second" feel.
    pub fn list_right(&mut self) {
        if self.threaded_active() {
            if let Some(Row::Header(t)) = self.selected_row() {
                if !self.threads[t].expanded {
                    self.threads[t].expanded = true;
                    self.rebuild_visible_rows();
                }
                self.move_thread_selection(1); // to the first child
                return;
            }
        }
        self.activate_selected();
    }

    /// The currently-open (`selected_msg`) message's row, resolved from the
    /// loaded flat list or, in threaded mode, the built threads. `None` when
    /// nothing is open or that row isn't loaded. Shared by the reader's
    /// meeting banner and the RSVP-key guard so both agree on what's open.
    pub(crate) fn selected_message_row(&self) -> Option<&MessageRow> {
        let id = self.selected_msg.as_deref()?;
        if let Some(m) = self.messages.iter().find(|m| m.id == id) {
            return Some(m);
        }
        self.threads
            .iter()
            .flat_map(|tv| tv.thread.messages.iter())
            .find(|m| m.id == id)
    }

    /// RSVP to the opened meeting-invite email: no-op unless the opened
    /// message is a meeting request (so `A`/`D`/`T` never act on ordinary
    /// mail). Sends `SyncCommand::RespondMeeting`; the confirmation + mark-read
    /// happen when `SyncEvent::MeetingResponded` lands (see `on_sync_event`).
    pub fn respond_meeting(&mut self, kind: RsvpKind) {
        let Some(message_id) = self
            .selected_message_row()
            .filter(|m| m.is_meeting_request)
            .map(|m| m.id.clone())
        else {
            return;
        };
        self.attachment_notice = Some("Responding…".to_string());
        let _ = self.sync.cmd_tx.send(SyncCommand::RespondMeeting {
            message_id,
            kind,
            comment: None,
            proposed_start_utc: None,
            proposed_end_utc: None,
        });
    }

    /// `O`: open the automatic-replies editor and fetch the current config
    /// (the form shows "loading…" until `AutomaticRepliesFetched` prefills it).
    pub fn open_oof_form(&mut self) {
        self.oof_form = Some(crate::ui::oofform::OofForm::loading_default());
        let _ = self.sync.cmd_tx.send(SyncCommand::FetchAutomaticReplies);
    }

    /// Validate and write the automatic-replies form. When `status ==
    /// Scheduled`, the Start/End text is parsed to UTC (inline error on a bad
    /// value or an end at/before the start); other statuses ignore and clear
    /// the window. Sends `SetAutomaticReplies` and leaves the form open — it
    /// closes on `AutomaticRepliesUpdated`.
    pub fn save_oof_form(&mut self) {
        let Some(form) = self.oof_form.as_ref() else {
            return;
        };
        let (start_utc, end_utc) = if form.status == OofStatus::Scheduled {
            let now = local_now();
            let off = crate::ui::calendar::local_offset_minutes();
            let Some(start) = crate::datetime::parse_start(&form.start, now, off) else {
                self.set_oof_error("Invalid start time");
                return;
            };
            let Some(end) = crate::datetime::parse_end(&form.end, &start, now, off) else {
                self.set_oof_error("Invalid end time");
                return;
            };
            if end <= start {
                self.set_oof_error("End must be after start");
                return;
            }
            (start, end)
        } else {
            (String::new(), String::new())
        };
        let form = self.oof_form.as_ref().unwrap();
        let replies = AutomaticReplies {
            status: form.status,
            external_audience: form.audience,
            internal_message: form.internal.clone(),
            external_message: form.external.clone(),
            scheduled_start_utc: start_utc,
            scheduled_end_utc: end_utc,
        };
        self.attachment_notice = Some("Saving…".to_string());
        let _ = self
            .sync
            .cmd_tx
            .send(SyncCommand::SetAutomaticReplies { replies });
    }

    /// Sets the OOF form's inline footer error (no-op if the form isn't open).
    fn set_oof_error(&mut self, msg: &str) {
        if let Some(form) = self.oof_form.as_mut() {
            form.error = Some(msg.to_string());
        }
    }

    /// Opens message `id` in the reading pane: records it as `selected_msg`
    /// and loads its body (see `reload_body`).
    pub fn open_message(&mut self, id: &str) {
        self.selected_msg = Some(id.to_string());
        self.reading_scroll = 0;
        // A new message is open — any previously cached/requested inline
        // images belonged to whatever was open before. Clear these BEFORE
        // reload_body(): a cache-hit body triggers request_inline_images()
        // synchronously as part of reload_body(), and it must see a fresh
        // `requested_inline` so it (not this trailing call) is the one that
        // marks cids and sends the fetches — otherwise both calls would see
        // an empty set in turn and each cid gets fetched twice.
        self.inline_images.clear();
        self.requested_inline.clear();
        self.inline_attachments_requested = false;
        self.image_protocols.clear();
        self.reload_body();
        self.request_inline_images();
    }

    // --- Reading pane scroll -------------------------------------------

    /// The furthest `reading_scroll` can go without scrolling past the last
    /// content row — `reading_content_rows - reading_viewport`, floored at 0
    /// (a body shorter than the viewport has nothing to scroll).
    fn reading_max_scroll(&self) -> usize {
        self.reading_content_rows
            .saturating_sub(self.reading_viewport)
    }

    /// `j`/`k`/↓/↑ while the reading pane has focus: moves `reading_scroll`
    /// by `delta` rows, clamped to `[0, reading_max_scroll()]`.
    pub fn reading_scroll_by(&mut self, delta: isize) {
        let max = self.reading_max_scroll() as isize;
        self.reading_scroll = (self.reading_scroll as isize + delta).clamp(0, max) as usize;
    }

    /// PageUp/PageDown while the reading pane has focus: scrolls by one
    /// viewport's worth of rows (at least 1, so a not-yet-drawn viewport of
    /// height 0 still moves).
    pub fn reading_scroll_page(&mut self, dir: isize) {
        let page = self.reading_viewport.max(1) as isize;
        self.reading_scroll_by(dir * page);
    }

    /// Home while the reading pane has focus: jumps to the top.
    pub fn reading_scroll_home(&mut self) {
        self.reading_scroll = 0;
    }

    /// End while the reading pane has focus: jumps to the bottom.
    pub fn reading_scroll_end(&mut self) {
        self.reading_scroll = self.reading_max_scroll();
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
                let is_html = body.content_type.eq_ignore_ascii_case("html");
                self.body = Some(body);
                self.body_loading = false;
                if is_html {
                    // A late `BodyReady` (the body wasn't cached when this
                    // message was opened) means `open_message`'s own call
                    // never saw a body to scan for `cid:`s — do it now.
                    self.request_inline_images();
                }
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

    /// For the opened HTML message, resolve each `cid:` image to its
    /// attachment and fetch its bytes into `inline_images` (once). Needs the
    /// message's attachment metadata; if that isn't loaded yet, kicks off
    /// `FetchAttachments` and returns — `on_sync_event`'s `AttachmentsUpdated`
    /// arm calls this again once it lands. `data:` images carry their own
    /// bytes and need no fetch; remote/unsupported are skipped.
    pub fn request_inline_images(&mut self) {
        let Some(id) = self.selected_msg.clone() else {
            return;
        };
        let Some(body) = &self.body else {
            return;
        };
        if !body.content_type.eq_ignore_ascii_case("html") {
            return;
        }
        let refs = mailcore::htmlrender::image_refs(&body.content);
        let cids: Vec<String> = refs
            .iter()
            .filter_map(|r| match &r.src {
                mailcore::htmlrender::ImageSource::Cid(c) => Some(c.clone()),
                _ => None,
            })
            .collect();
        if cids.is_empty() {
            return;
        }
        let metas = self.store.attachments(&id).unwrap_or_default();
        if metas.is_empty() {
            // No metadata yet — fetch it; AttachmentsUpdated re-enters here.
            // `open_message` calls this function twice in a row (see its
            // comment), and both calls can land here before either sees
            // metadata, so guard on `inline_attachments_requested` rather
            // than sending unconditionally — otherwise the message gets two
            // identical `FetchAttachments` in flight.
            if !self.inline_attachments_requested {
                self.inline_attachments_requested = true;
                let _ = self
                    .sync
                    .cmd_tx
                    .send(SyncCommand::FetchAttachments { message_id: id });
            }
            return;
        }
        for cid in cids {
            if self.requested_inline.contains(&cid) {
                continue;
            }
            if let Some(att) = metas.iter().find(|a| {
                a.content_id
                    .as_deref()
                    .is_some_and(|stored| normalize_cid(stored) == normalize_cid(&cid))
            }) {
                self.requested_inline.insert(cid.clone());
                let _ = self.sync.cmd_tx.send(SyncCommand::FetchInlineImage {
                    message_id: id.clone(),
                    attachment_id: att.id.clone(),
                    content_id: cid,
                });
            }
        }
    }

    // --- Calendar -----------------------------------------------------

    /// `g`: switches between the mail three-pane view and the calendar
    /// agenda + detail view (`ui::calendar::draw_calendar`). Entering
    /// Calendar reloads the agenda from whatever's already in the store and
    /// fires a fire-and-forget `SyncCommand::RefreshCalendar` so the view
    /// feels responsive right when it's opened, rather than waiting on the
    /// engine's own periodic tick — the same "show cached state, kick off a
    /// refresh" shape `open_message`/`reload_body` already use for mail
    /// bodies. Leaving Calendar is a plain mode flip: the agenda/selection
    /// state is left as-is so re-entering doesn't lose the user's place.
    pub fn toggle_mode(&mut self) {
        match self.mode {
            Mode::Mail => self.set_mode(Mode::Calendar),
            Mode::Calendar => self.set_mode(Mode::Mail),
        }
    }

    /// Switches to `mode` (the rail's Up/Down). Entering Calendar refreshes the
    /// agenda window and asks the sync engine for fresh calendar data, exactly
    /// as `toggle_mode` did; a no-op when already in `mode`.
    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == Mode::Calendar {
            self.reload_agenda();
            let _ = self.sync.cmd_tx.send(SyncCommand::RefreshCalendar);
        }
    }

    /// Re-reads the agenda window (`ui::calendar::agenda_window`, anchored at
    /// `SystemTime::now()`) from the store, clamping `agenda_index` if the
    /// list got shorter — the calendar equivalent of `reload_messages`.
    /// Called on entering Calendar mode (`toggle_mode`) and whenever
    /// `SyncEvent::CalendarUpdated` lands.
    pub fn reload_agenda(&mut self) {
        let (from, to) = crate::ui::calendar::agenda_window();
        self.agenda = self.store.events_in_window(&from, &to).unwrap_or_default();
        if self.agenda_index >= self.agenda.len() {
            self.agenda_index = self.agenda.len().saturating_sub(1);
        }
    }

    /// ↑/↓/j/k while in Calendar mode: moves `agenda_index` by `delta`,
    /// clamped (not wrapping) into `[0, agenda.len())` — a no-op on an empty
    /// agenda, so this can never index past the end. Clamped rather than
    /// wrapped (unlike the mail list's `ui::wrapped`) per the brief's
    /// bounds-safety note; nothing about the agenda calls for wrap-around.
    pub fn move_agenda_selection(&mut self, delta: isize) {
        if self.agenda.is_empty() {
            return;
        }
        let last = self.agenda.len() as isize - 1;
        let next = (self.agenda_index as isize + delta).clamp(0, last);
        self.agenda_index = next as usize;
    }

    /// Enter, in Calendar mode: opens the detail pane on the currently
    /// highlighted agenda row. A no-op (`selected_event` stays `None`) on an
    /// empty agenda.
    pub fn open_selected_event(&mut self) {
        self.selected_event = self.agenda.get(self.agenda_index).map(|e| e.id.clone());
    }

    /// The id of the event currently highlighted in the agenda, if any (an
    /// empty agenda yields `None` — the RSVP keys are then a no-op rather
    /// than a panic) — the calendar equivalent of `highlighted_message_id`.
    fn highlighted_event_id(&self) -> Option<String> {
        self.agenda.get(self.agenda_index).map(|e| e.id.clone())
    }

    /// `a`/`d`/`t` in Calendar mode: opens the RSVP comment prompt over the
    /// highlighted agenda row, with `kind` (one of `"accepted"`/
    /// `"declined"`/`"tentativelyAccepted"`) already decided by which key was
    /// pressed. A no-op on an empty agenda. Nothing is written to the store
    /// or sent to the sync engine yet — that happens on submit
    /// (`submit_rsvp`/`cancel_rsvp_comment`), once the (optional) comment is
    /// known.
    pub fn start_rsvp(&mut self, kind: &str) {
        let Some(event_id) = self.highlighted_event_id() else {
            return;
        };
        let focus = if kind == "declined" || kind == "tentativelyAccepted" {
            RsvpField::ProposedStart
        } else {
            RsvpField::Comment
        };
        self.rsvp_prompt = Some(RsvpPrompt {
            target: RsvpTarget::Event(event_id),
            kind: kind.to_string(),
            comment: String::new(),
            proposed_start: String::new(),
            proposed_end: String::new(),
            focus,
        });
    }

    /// Mail reader `D`/`T` on an opened meeting invite: open the RSVP prompt
    /// (with proposed-time fields) targeting the message. A no-op unless the
    /// opened message is a meeting request (same guard as `respond_meeting`).
    pub fn start_meeting_rsvp(&mut self, kind: &str) {
        let Some(message_id) = self
            .selected_message_row()
            .filter(|m| m.is_meeting_request)
            .map(|m| m.id.clone())
        else {
            return;
        };
        self.rsvp_prompt = Some(RsvpPrompt {
            target: RsvpTarget::Message(message_id),
            kind: kind.to_string(),
            comment: String::new(),
            proposed_start: String::new(),
            proposed_end: String::new(),
            focus: RsvpField::ProposedStart,
        });
    }

    /// Types into the focused RSVP field (comment or a proposed-time field).
    pub fn type_rsvp_comment(&mut self, s: &str) {
        if let Some(p) = &mut self.rsvp_prompt {
            match p.focus {
                RsvpField::ProposedStart => p.proposed_start.push_str(s),
                RsvpField::ProposedEnd => p.proposed_end.push_str(s),
                RsvpField::Comment => p.comment.push_str(s),
            }
        }
    }

    /// Backspaces the focused RSVP field.
    pub fn backspace_rsvp_comment(&mut self) {
        if let Some(p) = &mut self.rsvp_prompt {
            match p.focus {
                RsvpField::ProposedStart => {
                    p.proposed_start.pop();
                }
                RsvpField::ProposedEnd => {
                    p.proposed_end.pop();
                }
                RsvpField::Comment => {
                    p.comment.pop();
                }
            }
        }
    }

    /// Tab in the RSVP prompt: cycle focus. Accept skips the proposed-time
    /// fields (Comment only).
    pub fn cycle_rsvp_focus(&mut self) {
        if let Some(p) = &mut self.rsvp_prompt {
            p.focus = if p.proposes() {
                match p.focus {
                    RsvpField::ProposedStart => RsvpField::ProposedEnd,
                    RsvpField::ProposedEnd => RsvpField::Comment,
                    RsvpField::Comment => RsvpField::ProposedStart,
                }
            } else {
                RsvpField::Comment
            };
        }
    }

    /// Enter on the RSVP prompt: parse the proposed window (both-or-neither;
    /// `end > start`) when this kind proposes, then dispatch by target. A
    /// parse/validation failure surfaces an error notice and leaves the prompt
    /// open.
    pub fn submit_rsvp(&mut self) {
        let Some(prompt) = self.rsvp_prompt.as_ref() else {
            return;
        };
        let proposed: Option<(String, String)> = if prompt.proposes()
            && (!prompt.proposed_start.trim().is_empty() || !prompt.proposed_end.trim().is_empty())
        {
            let now = local_now();
            let off = crate::ui::calendar::local_offset_minutes();
            let Some(start) = crate::datetime::parse_start(prompt.proposed_start.trim(), now, off)
            else {
                self.set_rsvp_error();
                return;
            };
            let Some(end) =
                crate::datetime::parse_end(prompt.proposed_end.trim(), &start, now, off)
            else {
                self.set_rsvp_error();
                return;
            };
            if end <= start {
                self.set_rsvp_error();
                return;
            }
            Some((start, end))
        } else {
            None
        };
        let prompt = self.rsvp_prompt.take().unwrap();
        let comment = (!prompt.comment.is_empty()).then_some(prompt.comment.clone());
        self.dispatch_rsvp(prompt.target, prompt.kind, comment, proposed);
    }

    /// Esc, on the RSVP prompt: send the plain RSVP (no comment, no proposal).
    pub fn cancel_rsvp_comment(&mut self) {
        let Some(prompt) = self.rsvp_prompt.take() else {
            return;
        };
        self.dispatch_rsvp(prompt.target, prompt.kind, None, None);
    }

    /// Surfaces a proposed-time validation error (the prompt has no inline
    /// error field of its own; the transient notice suffices).
    fn set_rsvp_error(&mut self) {
        self.error_notice = Some("Invalid proposed time".to_string());
    }

    /// The shared dispatch: `Event` → optimistic `set_event_response` +
    /// `reload_agenda` + `RespondEvent`; `Message` → `RespondMeeting` (kind
    /// mapped from the status string to `RsvpKind`). The UI never enqueues the
    /// outbox op itself.
    fn dispatch_rsvp(
        &mut self,
        target: RsvpTarget,
        kind: String,
        comment: Option<String>,
        proposed: Option<(String, String)>,
    ) {
        let (proposed_start_utc, proposed_end_utc) = match proposed {
            Some((s, e)) => (Some(s), Some(e)),
            None => (None, None),
        };
        match target {
            RsvpTarget::Event(id) => {
                self.store.set_event_response(&id, &kind);
                self.reload_agenda();
                let _ = self.sync.cmd_tx.send(SyncCommand::RespondEvent {
                    id,
                    kind,
                    comment,
                    proposed_start_utc,
                    proposed_end_utc,
                });
            }
            RsvpTarget::Message(message_id) => {
                let rsvp = match kind.as_str() {
                    "declined" => RsvpKind::Decline,
                    "tentativelyAccepted" => RsvpKind::Tentative,
                    _ => RsvpKind::Accept,
                };
                let _ = self.sync.cmd_tx.send(SyncCommand::RespondMeeting {
                    message_id,
                    kind: rsvp,
                    comment,
                    proposed_start_utc,
                    proposed_end_utc,
                });
            }
        }
    }

    // --- Event form: open new/edit -----------------------------------------

    /// `Ctrl-B` in the event form: fetch and show attendees' + the organizer's
    /// availability for the form's Start date (08:00–18:00 local, 30-min
    /// slots). Read-only.
    pub fn open_free_busy(&mut self) {
        let Some(form) = self.event_form.as_ref() else {
            return;
        };
        // Emails: the organizer (own account) first, then attendee addresses.
        let mut schedules: Vec<String> = Vec::new();
        if let Some(me) = self.account.clone() {
            if !me.is_empty() {
                schedules.push(me);
            }
        }
        for (_, addr) in parse_attendee_pairs(&form.attendees) {
            if !addr.is_empty() && !schedules.contains(&addr) {
                schedules.push(addr);
            }
        }
        // Window: the Start field's date (first 10 chars if `YYYY-MM-DD…`, else
        // today) at 08:00–18:00 local → UTC.
        let now = local_now();
        let off = crate::ui::calendar::local_offset_minutes();
        let date = form
            .start
            .get(..10)
            .filter(|d| d.len() == 10 && d.as_bytes()[4] == b'-')
            .map(str::to_string)
            .unwrap_or_else(|| format!("{:04}-{:02}-{:02}", now.year, now.month, now.day));
        let start_utc =
            crate::datetime::parse_start(&format!("{date} 08:00"), now, off).unwrap_or_default();
        let end_utc =
            crate::datetime::parse_start(&format!("{date} 18:00"), now, off).unwrap_or_default();
        let (y, m, d) = crate::ui::calendar::date_of_utc(&start_utc);
        let _ = self.sync.cmd_tx.send(SyncCommand::FetchSchedule {
            schedules,
            start_utc,
            end_utc,
            interval_minutes: 30,
        });
        self.free_busy = Some(crate::ui::freebusy::FreeBusyView {
            day_label: crate::ui::calendar::day_label(y, m, d),
            slot_count: 20, // (18-8)*60/30
            entries: Vec::new(),
            loading: true,
        });
    }

    /// Esc in the free/busy overlay: close it (back to the event form).
    pub fn close_free_busy(&mut self) {
        self.free_busy = None;
    }

    /// `c` in Calendar mode: opens a blank event form. Start/End are
    /// prefilled in LOCAL time — Start to the next full hour, End to +1h
    /// after it — via `local_now`/`datetime::add_minutes`/
    /// `datetime::format_local`; nothing is written to the store, and
    /// `datetime::parse_start`/`parse_end` re-parse this same display text
    /// on Ctrl-Enter (`save_event_form`).
    pub fn open_new_event(&mut self) {
        let now = local_now();
        // Round up to the next full hour: if already exactly on the hour,
        // stays put; otherwise advances into the next one (`add_minutes`
        // handles the hour/day/month rollover via its epoch-minute math).
        let minutes_to_next_hour = if now.min == 0 { 0 } else { 60 - now.min as i64 };
        let start_dt = crate::datetime::add_minutes(now, minutes_to_next_hour);
        let end_dt = crate::datetime::add_minutes(start_dt, 60);
        self.event_form = Some(crate::ui::eventform::EventForm {
            editing_id: None,
            title: String::new(),
            start: crate::datetime::format_local(start_dt),
            end: crate::datetime::format_local(end_dt),
            all_day: false,
            repeat: None,
            interval: "1".into(),
            days: [false; 7],
            until: String::new(),
            location: String::new(),
            attendees: String::new(),
            body: String::new(),
            focus: crate::ui::eventform::EventField::Title,
            autocomplete: None,
            error: None,
        });
    }

    /// `e` in Calendar mode: opens the event form prefilled from
    /// `self.selected_event` (falling back to the highlighted agenda row if
    /// nothing's been opened into the detail pane yet) for editing. Refused —
    /// a status notice instead of opening the form — when the event is part
    /// of a recurring series (`series_master_id.is_some()`): recurring events
    /// stay read + RSVP only (see the calendar-edit design spec). A no-op if
    /// nothing is selected/highlighted, the resolved id isn't in the
    /// currently-loaded `agenda` window, or `event_for_send` has nothing for
    /// it (a stale/foreign id).
    pub fn open_edit_event(&mut self) {
        let Some(id) = self
            .selected_event
            .clone()
            .or_else(|| self.highlighted_event_id())
        else {
            return;
        };
        let Some(row) = self.agenda.iter().find(|e| e.id == id) else {
            return;
        };
        if row.series_master_id.is_some() {
            self.error_notice = Some("Recurring events can't be edited here.".to_string());
            return;
        }
        let Ok(Some(send)) = self.store.event_for_send(&id) else {
            return;
        };
        let attendees = self.store.event_attendees(&id).unwrap_or_default();
        let (start, end) = if send.is_all_day {
            // All-day dates are floating — never offset-converted (see
            // `datetime::all_day_bounds`'s doc comment). Prefill as the exact
            // inverse of `all_day_bounds`: Start is `start_utc`'s date;
            // `end_utc` is the EXCLUSIVE next-day midnight after the last
            // inclusive day, so End is `end_utc`'s date MINUS one day. Using
            // `utc_iso_to_local` here (as the timed path below does) would
            // prefill End from the exclusive boundary itself, and
            // `all_day_bounds` would add another day to it on save — the
            // event would grow by a day on every edit (BUG 1, whole-branch
            // review).
            let (sy, sm, sd) = crate::ui::calendar::date_of_utc(&send.start_utc);
            let (ey, em, ed) = crate::ui::calendar::date_of_utc(&send.end_utc);
            let end_days = crate::ui::calendar::days_from_civil(ey, em, ed) - 1;
            let (ly, lm, ld) = crate::ui::calendar::civil_from_days(end_days);
            (
                format!("{sy:04}-{sm:02}-{sd:02}"),
                format!("{ly:04}-{lm:02}-{ld:02}"),
            )
        } else {
            let offset = crate::ui::calendar::local_offset_minutes();
            let start = crate::datetime::utc_iso_to_local(&send.start_utc, offset)
                .map(crate::datetime::format_local)
                .unwrap_or_default();
            let end = crate::datetime::utc_iso_to_local(&send.end_utc, offset)
                .map(crate::datetime::format_local)
                .unwrap_or_default();
            (start, end)
        };
        let attendees_text = attendees
            .into_iter()
            .map(|a| format!("{} <{}>", a.name, a.addr))
            .collect::<Vec<_>>()
            .join("; ");
        self.event_form = Some(crate::ui::eventform::EventForm {
            editing_id: Some(id),
            title: send.subject,
            start,
            end,
            all_day: send.is_all_day,
            repeat: None,
            interval: "1".into(),
            days: [false; 7],
            until: String::new(),
            location: send.location,
            attendees: attendees_text,
            // Plain text of the stored HTML (mirrors `open_draft`'s
            // `compose_html::from_html(...).plain()`), not the raw HTML
            // source — `save_event_form` re-escapes whatever's typed here
            // with `escape_html`, so loading the raw HTML would double-escape
            // it on save (`<p>x</p>` → `&lt;p&gt;x&lt;/p&gt;`). Loading the
            // plain text instead makes the round trip stable.
            body: compose_html::from_html(&send.body_html).plain(),
            focus: crate::ui::eventform::EventField::Title,
            autocomplete: None,
            error: None,
        });
    }

    /// `x` in Calendar mode: opens the confirm modal to delete the selected
    /// event (falling back to the highlighted agenda row, same resolution
    /// order as `open_edit_event`). Refused — a status notice, no modal —
    /// when the event is part of a recurring series
    /// (`series_master_id.is_some()`): recurring events stay read + RSVP only
    /// (see the calendar-edit design spec). A no-op if nothing is
    /// selected/highlighted or the resolved id isn't in the currently-loaded
    /// `agenda` window. The actual delete happens on `confirm_yes` (see its
    /// `ConfirmAction::DeleteEvent` arm), not here.
    pub fn delete_selected_event(&mut self) {
        let Some(id) = self
            .selected_event
            .clone()
            .or_else(|| self.highlighted_event_id())
        else {
            return;
        };
        let Some(row) = self.agenda.iter().find(|e| e.id == id) else {
            return;
        };
        if row.series_master_id.is_some() {
            self.error_notice = Some("Recurring events can't be deleted here.".to_string());
            return;
        }
        let prompt = format!("Delete event '{}'?", row.subject);
        self.confirm = Some(ConfirmModal {
            prompt,
            action: ConfirmAction::DeleteEvent(id),
        });
    }

    /// Ctrl-Enter in the event form: parse + validate the times, build the
    /// fields, and either create a new local event (+ `CreateEvent`) or
    /// update the edited one (+ `UpdateEvent`). A parse/validation failure
    /// sets an inline error on the form and leaves it open.
    ///
    /// For an edit whose id is still a not-yet-synced `local:` id, this
    /// writes the fields to the store but does NOT also send
    /// `SyncCommand::UpdateEvent` — that event still has a pending
    /// `CreateEvent` op sitting in the outbox (from when it was first
    /// created), which reads `Store::event_for_send` at drain time and so
    /// picks up the just-written fields on its own. Enqueuing an `UpdateEvent`
    /// on top would race it: if the create drains first, it reconciles
    /// `local:X` → the real Graph id, and an `UpdateEvent{id: local:X}`
    /// enqueued after that would find no such row anymore (quarantined as
    /// "event not found"). Only a non-`local:` (already-synced) id gets an
    /// `UpdateEvent` sent.
    pub fn save_event_form(&mut self) {
        let Some(form) = self.event_form.as_ref() else {
            return;
        };
        let now = local_now();
        let off = crate::ui::calendar::local_offset_minutes();
        let (start_utc, end_utc) = if form.all_day {
            match crate::datetime::all_day_bounds(&form.start, &form.end, now) {
                Some(bounds) => bounds,
                None => {
                    self.set_form_error("Invalid date");
                    return;
                }
            }
        } else {
            let Some(start_utc) = crate::datetime::parse_start(&form.start, now, off) else {
                self.set_form_error("Invalid start time");
                return;
            };
            let Some(end_utc) = crate::datetime::parse_end(&form.end, &start_utc, now, off) else {
                self.set_form_error("Invalid end time");
                return;
            };
            if end_utc < start_utc {
                self.set_form_error("End is before start");
                return;
            }
            (start_utc, end_utc)
        };
        // Recurrence is create-only (repeat != None and not editing). Any
        // validation failure sets an inline error and returns without sending.
        let recurrence = if let Some(kind) = form.repeat.filter(|_| form.editing_id.is_none()) {
            let interval: u32 = match form.interval.trim().parse() {
                Ok(n) if n >= 1 => n,
                _ => {
                    self.set_form_error("Invalid interval");
                    return;
                }
            };
            let start_date = start_utc.get(..10).unwrap_or("").to_string();
            let day_of_month: u32 = start_utc
                .get(8..10)
                .and_then(|d| d.parse().ok())
                .unwrap_or(1);
            const NAMES: [&str; 7] = [
                "monday",
                "tuesday",
                "wednesday",
                "thursday",
                "friday",
                "saturday",
                "sunday",
            ];
            let mut days_of_week: Vec<String> = (0..7)
                .filter(|&i| form.days[i])
                .map(|i| NAMES[i].to_string())
                .collect();
            if kind == RecurrenceKind::Weekly && days_of_week.is_empty() {
                days_of_week.push(weekday_name_of(&start_utc));
            }
            let until = if form.until.trim().is_empty() {
                None
            } else {
                let u = form.until.trim().to_string();
                if crate::datetime::parse_start(&u, now, off).is_none() {
                    self.set_form_error("Invalid until date");
                    return;
                }
                if u < start_date {
                    self.set_form_error("Until is before start");
                    return;
                }
                Some(u)
            };
            Some(Recurrence {
                kind,
                interval,
                days_of_week,
                day_of_month,
                start_date,
                until,
            })
        } else {
            None
        };
        let fields = mailcore::store::LocalEventFields {
            subject: form.title.clone(),
            start_utc,
            end_utc,
            is_all_day: form.all_day,
            location: form.location.clone(),
            body_html: compose_html::escape_html(&form.body), // plain text as HTML text
            attendees: parse_attendee_pairs(&form.attendees),
            recurrence,
        };
        let editing = form.editing_id.clone();
        match editing {
            Some(id) => {
                if self.store.update_event_fields(&id, &fields).is_err() {
                    self.set_form_error("Couldn't save the event");
                    return; // keep the form open; no sync command
                }
                // A local:-only event's pending CreateEvent carries the
                // update; don't enqueue UpdateEvent for it.
                if !id.starts_with("local:") {
                    let _ = self.sync.cmd_tx.send(SyncCommand::UpdateEvent { id });
                }
            }
            None => {
                let account = self.account.clone().unwrap_or_default();
                match self.store.create_local_event(&fields, &account, &account) {
                    Ok(id) => {
                        let _ = self.sync.cmd_tx.send(SyncCommand::CreateEvent { id });
                    }
                    Err(_) => {
                        self.set_form_error("Couldn't create the event");
                        return;
                    }
                }
            }
        }
        self.event_form = None;
        self.reload_agenda();
    }

    /// Sets the event form's inline validation error, if the form is open —
    /// a no-op otherwise (defensive; `save_event_form`, the only caller, only
    /// calls this while `event_form` is known to be `Some`).
    fn set_form_error(&mut self, msg: &str) {
        if let Some(form) = self.event_form.as_mut() {
            form.error = Some(msg.to_string());
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
            't' => self.toggle_threaded(),
            'A' => self.respond_meeting(RsvpKind::Accept),
            'D' => self.start_meeting_rsvp("declined"),
            'T' => self.start_meeting_rsvp("tentativelyAccepted"),
            'O' => self.open_oof_form(),
            'l' => self.open_category_picker(crate::ui::categorypicker::PickerMode::Assign),
            'L' => self.open_category_picker(crate::ui::categorypicker::PickerMode::Filter),
            _ => {}
        }
    }

    /// `t`: flips threaded/flat, rebuilds the folder view for the new mode,
    /// resets both cursors to the top, and persists the choice (best-effort;
    /// a `None` `config_path`, as in tests, just skips the write).
    pub fn toggle_threaded(&mut self) {
        self.threaded = !self.threaded;
        self.row_index = 0;
        self.msg_index = 0;
        self.reload_messages();
        if let Some(path) = &self.config_path {
            let _ = crate::config::persist_threaded_to(path, self.threaded);
        }
    }

    /// The (id, has_attachments) of the message the list cursor currently
    /// points at — thread-aware. In threaded mode a Header targets the
    /// conversation's latest message (matching Enter/activate), a Message row
    /// targets that message; in flat mode it's messages[msg_index]. `None` when
    /// nothing is selected.
    fn highlighted_message_fields(&self) -> Option<(String, bool)> {
        if self.threaded_active() {
            return match self.selected_row()? {
                Row::Message(t, m) => self.threads[t]
                    .thread
                    .messages
                    .get(m)
                    .map(|x| (x.id.clone(), x.has_attachments)),
                Row::Header(t) => self.threads[t]
                    .thread
                    .messages
                    .last()
                    .map(|x| (x.id.clone(), x.has_attachments)),
            };
        }
        self.messages
            .get(self.msg_index)
            .map(|m| (m.id.clone(), m.has_attachments))
    }

    /// The id of the message currently highlighted in the list pane, if any
    /// (empty list, or nothing loaded yet, yield `None` — every triage
    /// action is then a no-op rather than a panic). Thread-aware — see
    /// `highlighted_message_fields`, which this reuses so `compose_reply`/
    /// `compose_forward` (the only callers) target the row under the cursor
    /// rather than the stale flat `messages` list while threaded.
    fn highlighted_message_id(&self) -> Option<String> {
        self.highlighted_message_fields().map(|(id, _)| id)
    }

    /// The message ids a triage key targets in threaded mode: every message
    /// of the conversation when the cursor is on a (collapsed or expanded)
    /// header, or the single message when it's on a message row. `None` in
    /// flat mode (callers fall back to the flat single-message path).
    fn threaded_target_ids(&self) -> Option<Vec<String>> {
        if !self.threaded_active() {
            return None;
        }
        match self.selected_row()? {
            Row::Header(t) => Some(
                self.threads[t]
                    .thread
                    .messages
                    .iter()
                    .map(|m| m.id.clone())
                    .collect(),
            ),
            Row::Message(t, m) => self.threads[t]
                .thread
                .messages
                .get(m)
                .map(|msg| vec![msg.id.clone()]),
        }
    }

    /// Marks the highlighted message read/unread: writes it to the store
    /// (so `reload_messages` reflects it immediately, without waiting on the
    /// sync engine), then fires `SyncCommand::MarkRead` so the engine
    /// enqueues the matching outbox op and pushes it to Graph. In threaded
    /// mode, when the cursor is on a conversation header, this acts on every
    /// message of the conversation instead of just the highlighted one.
    pub fn mark_read(&mut self, read: bool) {
        if let Some(ids) = self.threaded_target_ids() {
            for id in &ids {
                self.store.set_read(id, read);
                let _ = self.sync.cmd_tx.send(SyncCommand::MarkRead {
                    id: id.clone(),
                    read,
                });
            }
            self.reload_messages();
            return;
        }
        let Some(id) = self.highlighted_message_id() else {
            return;
        };
        self.store.set_read(&id, read);
        self.reload_messages();
        let _ = self.sync.cmd_tx.send(SyncCommand::MarkRead { id, read });
    }

    /// Toggles the highlighted message's flag, same optimistic-store +
    /// fire-and-forget-command pattern as `mark_read`. In threaded mode, when
    /// the cursor is on a conversation header, this flags/unflags every
    /// message of the conversation instead of just the highlighted one.
    pub fn toggle_flag(&mut self) {
        if let Some(ids) = self.threaded_target_ids() {
            // Flag the whole thread ON if any is currently unflagged, else clear
            // it — so one keypress makes the thread's flag state uniform.
            let want = match self.selected_row() {
                Some(Row::Header(t)) => self.threads[t]
                    .thread
                    .messages
                    .iter()
                    .any(|m| !m.is_flagged),
                Some(Row::Message(t, m)) => !self.threads[t].thread.messages[m].is_flagged,
                None => return,
            };
            for id in &ids {
                self.store.set_flag(id, want);
                let _ = self.sync.cmd_tx.send(SyncCommand::SetFlag {
                    id: id.clone(),
                    flagged: want,
                });
            }
            self.reload_messages();
            return;
        }
        let Some(row) = self.messages.get(self.msg_index) else {
            return;
        };
        let id = row.id.clone();
        let flagged = !row.is_flagged;
        self.store.set_flag(&id, flagged);
        self.reload_messages();
        let _ = self.sync.cmd_tx.send(SyncCommand::SetFlag { id, flagged });
    }

    /// A human count like `5 messages (incl. 2 in Sent)` for a confirm prompt.
    fn describe_thread_scope(&self, ids: &[String]) -> String {
        let sent_id = self
            .store
            .folders()
            .unwrap_or_default()
            .into_iter()
            .find(|f| f.well_known_name.as_deref() == Some("sentitems"))
            .map(|f| f.id);
        let in_sent = match (&sent_id, self.selected_row()) {
            (Some(sid), Some(Row::Header(t))) => self.threads[t]
                .thread
                .messages
                .iter()
                .filter(|m| &m.folder_id == sid)
                .count(),
            _ => 0,
        };
        let n = ids.len();
        if in_sent > 0 {
            format!("{n} messages (incl. {in_sent} in Sent)")
        } else {
            format!("{n} messages")
        }
    }

    /// Deletes the highlighted message: removes it from the store, then
    /// `reload_messages` re-reads the (now shorter) list and clamps
    /// `msg_index` so the selection can't point past the end — the same
    /// bounds-safe pattern `reload_messages` already uses when a folder
    /// switch shrinks the list. In threaded mode, a multi-message conversation
    /// selected via its header opens the confirm modal instead of deleting
    /// outright (see `confirm_yes`); a threaded singleton/message row deletes
    /// directly, same as the flat path.
    pub fn delete_selected(&mut self) {
        if let Some(ids) = self.threaded_target_ids() {
            if ids.len() > 1 {
                let prompt = format!("Delete {}?", self.describe_thread_scope(&ids));
                self.confirm = Some(ConfirmModal {
                    prompt,
                    action: ConfirmAction::DeleteThread(ids),
                });
                return;
            }
            // A singleton / single message row: delete directly.
            if let Some(id) = ids.into_iter().next() {
                let _ = self.store.delete_message(&id);
                self.reload_messages();
                let _ = self.sync.cmd_tx.send(SyncCommand::Delete { id });
            }
            return;
        }
        let Some(id) = self.highlighted_message_id() else {
            return;
        };
        let _ = self.store.delete_message(&id);
        self.reload_messages();
        let _ = self.sync.cmd_tx.send(SyncCommand::Delete { id });
    }

    /// Esc on the confirm modal: dismiss it, doing nothing.
    pub fn cancel_confirm(&mut self) {
        self.confirm = None;
    }

    /// Enter on the confirm modal: carry out the pending action (per-message
    /// optimistic store write + `SyncCommand`), then close it and reload.
    pub fn confirm_yes(&mut self) {
        let Some(modal) = self.confirm.take() else {
            return;
        };
        match modal.action {
            ConfirmAction::DeleteThread(ids) => {
                for id in ids {
                    let _ = self.store.delete_message(&id);
                    let _ = self.sync.cmd_tx.send(SyncCommand::Delete { id });
                }
            }
            ConfirmAction::MoveThread(ids, dest) => {
                for id in ids {
                    if self.store.move_message(&id, &dest).is_ok() {
                        let _ = self.sync.cmd_tx.send(SyncCommand::Move {
                            id,
                            dest: dest.clone(),
                        });
                    }
                }
            }
            ConfirmAction::DeleteEvent(id) => {
                let _ = self.store.delete_event(&id);
                let _ = self.sync.cmd_tx.send(SyncCommand::DeleteEvent { id });
                self.reload_agenda();
            }
        }
        self.reload_messages();
    }

    /// Opens the move-folder popup over the highlighted message. A no-op if
    /// nothing is highlighted, or there are no folders to move it to (an
    /// empty picker would have nothing to select and nowhere for Enter to
    /// land) — so this can never open a popup `confirm_move` can't act on.
    /// In threaded mode, captures the threaded target id (`threaded_target_ids`)
    /// rather than the flat `highlighted_message_id`, since the flat
    /// `messages` list is empty while threaded. Also captures the full
    /// thread id set up front, in `MovePicker::thread_ids`, when the target is
    /// a multi-message conversation — `confirm_move` uses that captured set
    /// rather than re-deriving it at Enter time, so a background sync reload
    /// in between can't retarget the move to a different thread.
    pub fn open_move_picker(&mut self) {
        let target = self.threaded_target_ids();
        let Some(id) = target
            .clone()
            .and_then(|v| v.into_iter().next())
            .or_else(|| self.highlighted_message_id())
        else {
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
            thread_ids: target.filter(|v| v.len() > 1),
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
        // In threaded mode with a multi-message conversation selected, confirm
        // the whole-thread move; otherwise move the single captured message.
        // Uses the id set CAPTURED at `open_move_picker` time (`picker.thread_ids`)
        // rather than re-deriving `threaded_target_ids()` here, so a background
        // sync reload between open and confirm (which rebuilds `threads`/
        // `visible_rows` under an unchanged `row_index`) can't move a different
        // thread than the one the user opened the picker on.
        if let Some(ids) = picker.thread_ids {
            let scope = self.describe_thread_scope(&ids);
            self.confirm = Some(ConfirmModal {
                prompt: format!("Move {scope} to this folder?"),
                action: ConfirmAction::MoveThread(ids, dest),
            });
            return;
        }
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
        let body = signature_body_html(&self.signature);
        if let Ok(id) = self.store.create_local_draft("", "", "", &body) {
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
        let attachments = self.store.outbound_attachments(&row.id).unwrap_or_default();
        self.compose = Some(Compose {
            to: row.to_recipients,
            cc: row.cc_recipients,
            bcc: row.bcc_recipients,
            subject: row.subject,
            editor,
            focus: ComposeField::To,
            draft_id: row.id,
            autocomplete: None,
            attachments,
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
            let _ = self.store.clear_outbound_attachments(&compose.draft_id);
            return;
        }
        let html = compose_html::to_html(&compose.editor.text);
        let _ = self.store.update_draft_fields(
            &compose.draft_id,
            &compose.subject,
            &compose.to,
            &compose.cc,
            &compose.bcc,
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
    /// Scans the loaded agenda for events whose reminder window contains
    /// `now_epoch` (`start − reminderMinutes·60 ≤ now < start`, and
    /// `is_reminder_on`) and that haven't been alerted yet this session; each
    /// fires exactly once — pushing a banner line and, when `reminders_notify`
    /// is set, an agwinterm overlay. Called every main-loop tick.
    pub fn check_due_reminders(&mut self, now_epoch: i64) {
        // Collect first (immutable borrow of self.agenda) so the mutable writes
        // below don't overlap the iteration.
        let due: Vec<(String, String)> = self
            .agenda
            .iter()
            .filter(|e| e.is_reminder_on && !self.alerted_reminders.contains(&e.id))
            .filter_map(|e| {
                let start = utc_to_epoch(&e.start_utc);
                let remind_at = start - e.reminder_minutes.max(0) * 60;
                if remind_at <= now_epoch && now_epoch < start {
                    let phrase = starts_in_phrase(now_epoch, start, &e.start_utc);
                    Some((e.id.clone(), format!("⏰ {} {}", e.subject, phrase)))
                } else {
                    None
                }
            })
            .collect();
        for (id, msg) in due {
            self.alerted_reminders.insert(id);
            self.reminder_queue.push_back(msg.clone());
            if self.reminders_notify {
                self.notify_agwinterm(&msg);
            }
        }
    }

    /// Dismisses the front reminder banner.
    pub fn dismiss_reminder(&mut self) {
        self.reminder_queue.pop_front();
    }

    /// Best-effort agwinterm overlay for a fired reminder. Production: only
    /// inside agwinterm (`AGWINTERM_ENABLED=1`), spawns `agwintermctl notify
    /// {msg} --title lookxy` argv-style (no shell), result ignored.
    #[cfg(not(test))]
    fn notify_agwinterm(&self, msg: &str) {
        if std::env::var("AGWINTERM_ENABLED").as_deref() == Ok("1") {
            let _ = std::process::Command::new("agwintermctl")
                .arg("notify")
                .arg(msg)
                .arg("--title")
                .arg("lookxy")
                .spawn();
        }
    }

    /// Test seam: count calls instead of spawning a process.
    #[cfg(test)]
    fn notify_agwinterm(&self, _msg: &str) {
        self.agwinterm_notify_invocations
            .set(self.agwinterm_notify_invocations.get() + 1);
    }

    /// `l`/`L`: open the category picker. Assign mode seeds each master
    /// category preselected iff the highlighted message already has it (plus any
    /// category on the message that isn't in the master list, so it can still be
    /// toggled off); Filter mode just lists the master categories. Also refreshes
    /// the master list so the choices are current.
    pub fn open_category_picker(&mut self, mode: crate::ui::categorypicker::PickerMode) {
        use crate::ui::categories::color_for;
        use crate::ui::categorypicker::{CategoryItem, CategoryPicker, PickerMode};
        let _ = self.sync.cmd_tx.send(SyncCommand::RefreshCategories);
        let (message_id, current): (Option<String>, Vec<String>) = match mode {
            PickerMode::Assign => match self.highlighted_message_fields() {
                Some((id, _)) => {
                    let cats = self
                        .messages
                        .iter()
                        .find(|m| m.id == id)
                        .map(|m| m.categories.clone())
                        .or_else(|| {
                            self.threads
                                .iter()
                                .flat_map(|t| t.thread.messages.iter())
                                .find(|m| m.id == id)
                                .map(|m| m.categories.clone())
                        })
                        .unwrap_or_default();
                    (Some(id), cats)
                }
                None => return, // nothing highlighted
            },
            PickerMode::Filter => (None, Vec::new()),
        };
        let mut names: Vec<String> = self
            .master_categories
            .iter()
            .map(|c| c.display_name.clone())
            .collect();
        for c in &current {
            if !names.contains(c) {
                names.push(c.clone());
            }
        }
        let items = names
            .into_iter()
            .map(|name| CategoryItem {
                color: color_for(&self.master_categories, &name),
                selected: current.contains(&name),
                name,
            })
            .collect();
        self.category_picker = Some(CategoryPicker {
            mode,
            message_id,
            items,
            index: 0,
        });
    }

    /// Moves the picker's highlight by `delta`, clamped.
    pub fn category_picker_select(&mut self, delta: isize) {
        if let Some(p) = &mut self.category_picker {
            let len = p.items.len();
            if len == 0 {
                return;
            }
            let max = (len - 1) as isize;
            p.index = (p.index as isize + delta).clamp(0, max) as usize;
        }
    }

    /// Space in Assign mode: toggles the highlighted item's `selected`.
    pub fn category_picker_toggle(&mut self) {
        if let Some(p) = &mut self.category_picker {
            if p.mode == crate::ui::categorypicker::PickerMode::Assign {
                if let Some(it) = p.items.get_mut(p.index) {
                    it.selected = !it.selected;
                }
            }
        }
    }

    /// Enter: Assign → send `SetCategories` with the selected names + close;
    /// Filter → set `category_filter` to the highlighted category + reload.
    pub fn apply_category_picker(&mut self) {
        let Some(p) = self.category_picker.as_ref() else {
            return;
        };
        match p.mode {
            crate::ui::categorypicker::PickerMode::Assign => {
                let Some(id) = p.message_id.clone() else {
                    self.category_picker = None;
                    return;
                };
                let names: Vec<String> = p
                    .items
                    .iter()
                    .filter(|it| it.selected)
                    .map(|it| it.name.clone())
                    .collect();
                self.store.set_categories(&id, &names);
                self.reload_messages();
                let _ = self.sync.cmd_tx.send(SyncCommand::SetCategories {
                    id,
                    categories: names,
                });
                self.category_picker = None;
            }
            crate::ui::categorypicker::PickerMode::Filter => {
                if let Some(it) = p.items.get(p.index) {
                    self.category_filter = Some(it.name.clone());
                }
                self.category_picker = None;
                self.reload_messages();
            }
        }
    }

    /// Clears an active category filter (Esc in the folder view). No-op if none.
    pub fn clear_category_filter(&mut self) {
        if self.category_filter.take().is_some() {
            self.reload_messages();
        }
    }

    pub fn open_attachments_popup(&mut self) {
        let Some((id, has_attachments)) = self.highlighted_message_fields() else {
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
        match att.kind {
            AttachmentKind::File => {
                let dest = downloads_dir().join(sanitize_filename(&att.name));
                self.pending_saves.insert(dest.clone(), open_after);
                let _ = self.sync.cmd_tx.send(SyncCommand::SaveAttachment {
                    message_id,
                    attachment_id,
                    dest,
                });
            }
            AttachmentKind::Item => {
                // Extension is chosen by the engine (content sniff); register
                // the open-intent by the extension-less base, matched by
                // stem in `finish_attachment_save`.
                let base_name = strip_item_ext(&sanitize_filename(&att.name)).to_string();
                let dest_base = downloads_dir().join(base_name);
                self.pending_saves.insert(dest_base.clone(), open_after);
                let _ = self.sync.cmd_tx.send(SyncCommand::SaveItemAttachment {
                    message_id,
                    attachment_id,
                    dest_base,
                });
            }
            AttachmentKind::Reference => match att.source_url.clone() {
                Some(url) if is_web_url(&url) => {
                    self.open_with_os_handler(std::path::Path::new(&url));
                    self.attachment_notice = Some(format!("Opened link: {}", att.name));
                    self.attachments = None;
                }
                Some(_) => {
                    self.attachment_notice = Some("Refusing to open non-web link".to_string());
                }
                None => {
                    self.attachment_notice = Some("No link for this attachment".to_string());
                }
            },
        }
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
        // File saves registered the exact path; item saves registered the
        // extension-less base (the engine appended .ics/.eml), so fall back
        // to the stem. Remove whichever matched.
        let open_after = self
            .pending_saves
            .remove(&path)
            .or_else(|| self.pending_saves.remove(&path.with_extension("")))
            .unwrap_or(false);
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

    // --- File picker ----------------------------------------------------

    /// Enter in the file picker: on a file, attach it and close the picker;
    /// on a directory, the picker navigated (stays open) — see
    /// `ui::filepicker::FilePicker::enter`.
    pub fn file_picker_enter(&mut self) {
        let Some(picker) = self.file_picker.as_mut() else {
            return;
        };
        if let Some(path) = picker.enter() {
            self.file_picker = None;
            self.attach_file(&path);
        }
    }

    /// Records `path` as an attachment on the open draft (store + the
    /// composer's in-memory list). A no-op if no composer is open.
    pub fn attach_file(&mut self, path: &Path) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment")
            .to_string();
        let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
        let path_str = path.to_string_lossy().to_string();
        let draft_id = compose.draft_id.clone();
        let _ = self
            .store
            .add_outbound_attachment(&draft_id, &path_str, &name, size);
        if let Some(compose) = self.compose.as_mut() {
            compose.attachments = self
                .store
                .outbound_attachments(&draft_id)
                .unwrap_or_default();
        }
    }

    /// Removes the most-recently-added attachment (the last in attach order) from
    /// the open draft: deletes it in the store, then re-reads `compose.attachments`
    /// from the store so the in-memory list can't drift from the stored truth even
    /// if the delete fails. No-op if nothing is attached / no composer open.
    pub fn remove_last_attachment(&mut self) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let draft_id = compose.draft_id.clone();
        let Some(last) = compose.attachments.last().map(|a| a.path.clone()) else {
            return;
        };
        let _ = self.store.remove_outbound_attachment(&draft_id, &last);
        if let Some(compose) = self.compose.as_mut() {
            compose.attachments = self
                .store
                .outbound_attachments(&draft_id)
                .unwrap_or_default();
        }
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
    pub fn render_contains(&mut self, needle: &str) -> bool {
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
                    is_meeting_request: false,
                    categories: Vec::new(),
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

    /// An `App` seeded with a small folder hierarchy for tree tests:
    /// `Inbox` (well-known) with a child `EPAM`, plus a top-level `Sent`. All
    /// collapsed by default; callers expand what they need and `reload_folders`.
    #[cfg(test)]
    pub fn for_test_with_folder_tree() -> App {
        use mailcore::graph::model::MailFolder;
        use std::sync::mpsc;
        let store = Store::open_in_memory().expect("in-memory store");
        for (id, name, parent, wkn) in [
            ("inbox", "Inbox", None, Some("inbox")),
            ("epam", "EPAM", Some("inbox"), None),
            ("sent", "Sent", None, Some("sentitems")),
        ] {
            store
                .upsert_folder(&MailFolder {
                    id: id.into(),
                    display_name: name.into(),
                    parent_id: parent.map(Into::into),
                    total_count: 0,
                    unread_count: 0,
                    well_known_name: wkn.map(Into::into),
                })
                .expect("seed folder");
        }
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (_evt_tx, evt_rx) = mpsc::channel();
        let mut app = App::new(store, SyncHandle { cmd_tx, evt_rx }, PathBuf::new());
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

/// Strips a trailing `.eml`/`.ics` (case-insensitive) from an item
/// attachment's sanitized name, so appending the sniffed extension can't
/// double it (`Invite.ics` → base `Invite` → `Invite.ics`, not `Invite.ics.ics`).
/// Other names (incl. ones with internal dots) are returned unchanged.
fn strip_item_ext(name: &str) -> &str {
    for ext in [".eml", ".ics"] {
        if name.len() > ext.len() {
            let (_, tail) = name.as_bytes().split_at(name.len() - ext.len());
            if tail.eq_ignore_ascii_case(ext.as_bytes()) {
                return &name[..name.len() - ext.len()]; // matched an ASCII ".eml"/".ics": boundary is '.', safe
            }
        }
    }
    name
}

/// Whether `url` is an `http`/`https` link (case-insensitive scheme) — the
/// only schemes a reference attachment is opened with. A sender-supplied
/// `sourceUrl` with any other scheme (`file:`, a `\\host\share` UNC path, etc.)
/// is refused, since handing it to the OS opener could leak credentials or
/// open a local resource.
fn is_web_url(url: &str) -> bool {
    let u = url.trim_start();
    let lower = u.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The current wall-clock instant as local time — `ui::calendar::local_offset_minutes`
/// (the same system offset the agenda itself renders with) applied to
/// `SystemTime::now()`, then re-derived into calendar fields via
/// `ui::calendar::civil_from_days`. `open_new_event`'s prefill seed, and
/// `save_event_form`'s "now" for `datetime::parse_start`/`parse_end`.
/// Epoch seconds for a canonical-UTC `YYYY-MM-DDTHH:MM:SSZ` timestamp, via the
/// calendar module's civil-day math. Used to compare event starts to `now`.
pub(crate) fn utc_to_epoch(iso: &str) -> i64 {
    let (y, m, d) = crate::ui::calendar::date_of_utc(iso);
    let time = iso.split('T').nth(1).unwrap_or("").trim_end_matches('Z');
    let mut parts = time.splitn(3, ':');
    let h: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mi: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let s: i64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    crate::ui::calendar::days_from_civil(y, m, d) * 86400 + h * 3600 + mi * 60 + s
}

/// "starts now" (when `now >= start`) or "starts in N min (HH:MM)" — the local
/// start time via `ui::calendar::to_local`.
fn starts_in_phrase(now_epoch: i64, start_epoch: i64, start_utc: &str) -> String {
    if now_epoch >= start_epoch {
        return "starts now".to_string();
    }
    let mins = ((start_epoch - now_epoch) / 60).max(1);
    format!(
        "starts in {mins} min ({})",
        crate::ui::calendar::local_hhmm(start_utc)
    )
}

/// The Graph weekday name (`"monday".."sunday"`) of a canonical-UTC start
/// timestamp's date, via the calendar module's civil-day math (matching
/// `ui::calendar::weekday_abbrev`'s `(z + 4).rem_euclid(7)` Sunday-indexed
/// convention). Used to default a weekly recurrence to the start's weekday.
fn weekday_name_of(start_utc: &str) -> String {
    const NAMES: [&str; 7] = [
        "sunday",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
    ];
    let (y, m, d) = crate::ui::calendar::date_of_utc(start_utc);
    let z = crate::ui::calendar::days_from_civil(y, m, d);
    NAMES[(z + 4).rem_euclid(7) as usize].to_string()
}

fn local_now() -> crate::datetime::LocalDateTime {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let local_secs = secs + crate::ui::calendar::local_offset_minutes() * 60;
    let days = local_secs.div_euclid(86_400);
    let rem = local_secs.rem_euclid(86_400);
    let (year, month, day) = crate::ui::calendar::civil_from_days(days);
    crate::datetime::LocalDateTime {
        year,
        month,
        day,
        hour: (rem / 3600) as u32,
        min: ((rem % 3600) / 60) as u32,
    }
}

/// Parses the event form's flat `attendees` text (`"Name <addr>; Name2
/// <addr2>; bare@addr"`) into `(name, address)` pairs for
/// `LocalEventFields::attendees` — the same shape `open_edit_event` formats
/// it back into (`"{name} <{addr}>"` joined by `"; "`), so round-tripping an
/// edited event's attendees through the form is stable.
///
/// Mirrors `mailcore::sync::outbox::parse_recipients`'s shape rather than
/// calling it directly (it's `pub(crate)` to `mailcore`, unreachable from
/// this crate): split on `;` first (the only separator `open_edit_event`'s
/// formatting ever joins on, so always safe to split on — unlike `,`, which
/// a "Surname, Given" display name can itself contain); each part with a
/// `<...>`-wrapped address is one `(name, addr)` pair; a part with no
/// `<...>` is the bare-address shape a hand-typed attendee list uses, where
/// `,` IS a separator, split into one address-only (empty name) pair per
/// non-empty piece.
fn parse_attendee_pairs(field: &str) -> Vec<(String, String)> {
    field
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .flat_map(parse_attendee_part)
        .collect()
}

/// One `;`-separated part of `parse_attendee_pairs`'s input — see that
/// function's doc comment for the `<...>`-wrapped vs. bare-address shapes.
fn parse_attendee_part(part: &str) -> Vec<(String, String)> {
    if let (Some(open), Some(close)) = (part.find('<'), part.rfind('>')) {
        if open < close {
            return vec![(
                part[..open].trim().to_string(),
                part[open + 1..close].trim().to_string(),
            )];
        }
    }
    part.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|addr| (String::new(), addr.to_string()))
        .collect()
}

/// Builds the initial body HTML for a new message: empty when the signature is
/// blank; otherwise an empty first paragraph (where the cursor lands), a `--`
/// separator, then one paragraph per signature line (HTML-escaped). Only new
/// messages get this — reply/forward bodies come from Graph untouched.
fn signature_body_html(sig: &str) -> String {
    if sig.trim().is_empty() {
        return String::new();
    }
    let mut html = String::from("<p></p><p>--</p>");
    for line in sig.lines() {
        html.push_str(&format!(
            "<p>{}</p>",
            mailcore::compose_html::escape_html(line)
        ));
    }
    html
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mailcore::graph::model::AttachmentKind;

    /// Seeds a meeting-invite message into the seeded fixture's inbox, pulls it
    /// into `app.messages` (`reload_messages`), and opens it.
    fn open_meeting_invite(app: &mut App) {
        use mailcore::graph::model::{Message, Recipient};
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "invite1".into(),
                    conversation_id: "c9".into(),
                    subject: "Sprint review".into(),
                    from: Recipient {
                        name: "Boss".into(),
                        address: "boss@x".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-18T10:00:00Z".into(),
                    sent: "2026-07-18T09:00:00Z".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "invite".into(),
                    is_draft: false,
                    is_meeting_request: true,
                    categories: Vec::new(),
                },
            )
            .expect("seed invite");
        app.reload_messages();
        app.open_message("invite1");
        // `open_message` queues a `FetchBody` (the body isn't cached) — drain it
        // so a following RSVP assertion sees only the command it triggered.
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {}
    }

    #[test]
    fn o_opens_the_oof_form_and_fetches() {
        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('O');
        assert!(app.oof_form.as_ref().unwrap().loading);
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::FetchAutomaticReplies) => {}
            other => panic!("expected FetchAutomaticReplies, got {other:?}"),
        }
    }

    fn reminder_row(
        id: &str,
        start_utc: &str,
        minutes: i64,
        on: bool,
    ) -> mailcore::store::EventRow {
        mailcore::store::EventRow {
            id: id.into(),
            subject: "Standup".into(),
            start_utc: start_utc.into(),
            end_utc: "2026-07-20T10:00:00Z".into(),
            is_all_day: false,
            location: String::new(),
            organizer_name: String::new(),
            organizer_addr: String::new(),
            response_status: "organizer".into(),
            series_master_id: None,
            reminder_minutes: minutes,
            is_reminder_on: on,
        }
    }

    #[test]
    fn utc_to_epoch_known_values() {
        assert_eq!(crate::app::utc_to_epoch("1970-01-01T00:00:00Z"), 0);
        assert_eq!(crate::app::utc_to_epoch("1970-01-01T00:01:00Z"), 60);
        assert_eq!(crate::app::utc_to_epoch("1970-01-02T00:00:00Z"), 86400);
    }

    #[test]
    fn check_due_reminders_fires_once_in_window() {
        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E1", "2026-07-20T09:00:00Z", 15, true)];
        let start = crate::app::utc_to_epoch("2026-07-20T09:00:00Z");
        let now = start - 5 * 60; // 5 min before, inside the 15-min window
        app.check_due_reminders(now);
        assert_eq!(app.reminder_queue.len(), 1);
        assert!(app.reminder_queue.front().unwrap().contains("Standup"));
        app.check_due_reminders(now); // de-dup
        assert_eq!(app.reminder_queue.len(), 1);
    }

    #[test]
    fn check_due_reminders_respects_window_and_flag() {
        let start = crate::app::utc_to_epoch("2026-07-20T09:00:00Z");

        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E1", "2026-07-20T09:00:00Z", 15, false)]; // reminder off
        app.check_due_reminders(start - 5 * 60);
        assert!(app.reminder_queue.is_empty());

        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E1", "2026-07-20T09:00:00Z", 15, true)];
        app.check_due_reminders(start - 60 * 60); // before the 15-min window
        assert!(app.reminder_queue.is_empty());
        app.check_due_reminders(start + 60); // after start
        assert!(app.reminder_queue.is_empty());
    }

    #[test]
    fn agwinterm_notify_fires_only_when_flag_on() {
        let now = crate::app::utc_to_epoch("2026-07-20T09:00:00Z") - 5 * 60;

        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E1", "2026-07-20T09:00:00Z", 15, true)];
        app.check_due_reminders(now); // flag off (default)
        assert_eq!(app.agwinterm_notify_invocations.get(), 0);
        assert_eq!(app.reminder_queue.len(), 1);

        let mut app = App::for_test_with_seeded_store();
        app.agenda = vec![reminder_row("E2", "2026-07-20T09:00:00Z", 15, true)];
        app.reminders_notify = true;
        app.check_due_reminders(now);
        assert_eq!(app.agwinterm_notify_invocations.get(), 1);
    }

    #[test]
    fn dismiss_reminder_pops_the_front() {
        let mut app = App::for_test_with_seeded_store();
        app.reminder_queue.push_back("a".into());
        app.reminder_queue.push_back("b".into());
        app.dismiss_reminder();
        assert_eq!(app.reminder_queue.front().map(String::as_str), Some("b"));
        app.dismiss_reminder();
        assert!(app.reminder_queue.is_empty());
    }

    #[test]
    fn open_free_busy_sends_fetch_with_organizer_and_attendees() {
        use crate::ui::eventform::{EventField, EventForm};
        let mut app = App::for_test_with_seeded_store();
        app.account = Some("me@x".into());
        app.event_form = Some(EventForm {
            editing_id: None,
            title: "Sync".into(),
            start: "2026-07-21 14:00".into(),
            end: "2026-07-21 15:00".into(),
            all_day: false,
            repeat: None,
            interval: "1".into(),
            days: [false; 7],
            until: String::new(),
            location: String::new(),
            attendees: "Alice <alice@x>; bob@x".into(),
            body: String::new(),
            focus: EventField::Title,
            autocomplete: None,
            error: None,
        });
        app.open_free_busy();
        assert!(app.free_busy.as_ref().unwrap().loading);
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::FetchSchedule {
                schedules,
                start_utc,
                end_utc,
                interval_minutes,
            }) => {
                assert_eq!(schedules, vec!["me@x", "alice@x", "bob@x"]);
                assert!(start_utc.contains("2026-07-21") && start_utc.ends_with('Z'));
                assert!(end_utc.ends_with('Z'));
                assert_eq!(interval_minutes, 30);
            }
            other => panic!("expected FetchSchedule, got {other:?}"),
        }
    }

    #[test]
    fn schedule_fetched_fills_the_view() {
        use mailcore::graph::model::ScheduleEntry;
        let mut app = App::for_test_with_seeded_store();
        app.free_busy = Some(crate::ui::freebusy::FreeBusyView {
            day_label: "Mon Jul 21".into(),
            slot_count: 20,
            entries: Vec::new(),
            loading: true,
        });
        app.on_sync_event(SyncEvent::ScheduleFetched {
            entries: vec![ScheduleEntry {
                email: "me@x".into(),
                availability: "000222".into(),
            }],
        });
        let v = app.free_busy.as_ref().unwrap();
        assert!(!v.loading);
        assert_eq!(v.entries.len(), 1);
    }

    #[test]
    fn save_event_form_weekly_builds_recurrence() {
        use crate::ui::eventform::{EventField, EventForm};
        use mailcore::graph::model::RecurrenceKind;
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        let mut days = [false; 7];
        days[0] = true; // Mon
        days[2] = true; // Wed
        app.event_form = Some(EventForm {
            editing_id: None,
            title: "Standup".into(),
            start: "2026-07-20 09:00".into(),
            end: "2026-07-20 09:15".into(),
            all_day: false,
            repeat: Some(RecurrenceKind::Weekly),
            interval: "2".into(),
            days,
            until: "2026-12-31".into(),
            location: String::new(),
            attendees: String::new(),
            body: String::new(),
            focus: EventField::Title,
            autocomplete: None,
            error: None,
        });
        app.save_event_form();
        assert!(app.event_form.is_none(), "form should close on success");
        let ev = app
            .store
            .events_in_window("2026-07-01T00:00:00Z", "2026-08-01T00:00:00Z")
            .unwrap();
        let id = ev
            .iter()
            .find(|e| e.subject == "Standup")
            .unwrap()
            .id
            .clone();
        let sent = app.store.event_for_send(&id).unwrap().unwrap();
        let rec = sent.recurrence.unwrap();
        assert_eq!(rec.kind, RecurrenceKind::Weekly);
        assert_eq!(rec.interval, 2);
        assert_eq!(
            rec.days_of_week,
            vec!["monday".to_string(), "wednesday".to_string()]
        );
        assert_eq!(rec.until.as_deref(), Some("2026-12-31"));
    }

    #[test]
    fn save_event_form_invalid_interval_errors_and_sends_nothing() {
        use crate::ui::eventform::{EventField, EventForm};
        use mailcore::graph::model::RecurrenceKind;
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.event_form = Some(EventForm {
            editing_id: None,
            title: "X".into(),
            start: "2026-07-20 09:00".into(),
            end: "2026-07-20 09:15".into(),
            all_day: false,
            repeat: Some(RecurrenceKind::Daily),
            interval: "zero".into(),
            days: [false; 7],
            until: String::new(),
            location: String::new(),
            attendees: String::new(),
            body: String::new(),
            focus: EventField::Title,
            autocomplete: None,
            error: None,
        });
        app.save_event_form();
        assert!(app.event_form.is_some()); // stayed open
        assert_eq!(
            app.event_form.as_ref().unwrap().error.as_deref(),
            Some("Invalid interval")
        );
    }

    #[test]
    fn o_opens_the_oof_form_in_calendar_mode_too() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let mut app = App::for_test_with_seeded_store();
        // Calendar mode routes keys to `calendar::handle_key`, which does NOT
        // fall through to `on_key_char` — so `O` must be bound there too.
        app.mode = crate::app::Mode::Calendar;
        crate::ui::handle_key(&mut app, KeyEvent::from(KeyCode::Char('O')));
        assert!(app.oof_form.is_some());
    }

    #[test]
    fn automatic_replies_fetched_prefills_the_form() {
        use mailcore::graph::model::ExternalAudience;
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        app.on_sync_event(SyncEvent::AutomaticRepliesFetched {
            replies: AutomaticReplies {
                status: OofStatus::AlwaysEnabled,
                external_audience: ExternalAudience::ContactsOnly,
                internal_message: "Away".into(),
                external_message: "Out".into(),
                scheduled_start_utc: "".into(),
                scheduled_end_utc: "".into(),
            },
        });
        let form = app.oof_form.as_ref().unwrap();
        assert!(!form.loading);
        assert_eq!(form.status, OofStatus::AlwaysEnabled);
        assert_eq!(form.internal, "Away");
    }

    #[test]
    fn save_oof_form_scheduled_sends_set_with_parsed_utc() {
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain the fetch
        let form = app.oof_form.as_mut().unwrap();
        form.loading = false;
        form.status = OofStatus::Scheduled;
        form.start = "2026-07-20 09:00".into();
        form.end = "2026-07-27 17:00".into();
        form.internal = "Away".into();
        app.save_oof_form();
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::SetAutomaticReplies { replies }) => {
                assert_eq!(replies.status, OofStatus::Scheduled);
                assert!(replies.scheduled_start_utc.ends_with('Z'));
                assert!(!replies.scheduled_start_utc.is_empty());
                assert!(!replies.scheduled_end_utc.is_empty());
            }
            other => panic!("expected SetAutomaticReplies, got {other:?}"),
        }
    }

    #[test]
    fn save_oof_form_invalid_schedule_errors_and_sends_nothing() {
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain the fetch
        let form = app.oof_form.as_mut().unwrap();
        form.loading = false;
        form.status = OofStatus::Scheduled;
        form.start = "not a time".into();
        app.save_oof_form();
        assert!(app.oof_form.as_ref().unwrap().error.is_some());
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err()); // nothing sent
    }

    #[test]
    fn save_oof_form_disabled_sends_empty_schedule() {
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain the fetch
        let form = app.oof_form.as_mut().unwrap();
        form.loading = false;
        form.status = OofStatus::Disabled;
        form.start = "garbage".into(); // ignored when not Scheduled
        app.save_oof_form();
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::SetAutomaticReplies { replies }) => {
                assert_eq!(replies.status, OofStatus::Disabled);
                assert_eq!(replies.scheduled_start_utc, "");
                assert_eq!(replies.scheduled_end_utc, "");
            }
            other => panic!("expected SetAutomaticReplies, got {other:?}"),
        }
    }

    #[test]
    fn automatic_replies_updated_closes_form_and_notifies() {
        let mut app = App::for_test_with_seeded_store();
        app.open_oof_form();
        app.on_sync_event(SyncEvent::AutomaticRepliesUpdated);
        assert!(app.oof_form.is_none());
        assert_eq!(
            app.attachment_notice.as_deref(),
            Some("Automatic replies updated")
        );
    }

    #[test]
    fn oof_form_captures_text() {
        let mut app = App::for_test_with_seeded_store();
        assert!(!app.is_capturing_text());
        app.open_oof_form();
        assert!(app.is_capturing_text());
    }

    #[test]
    fn l_opens_assign_picker_seeded_from_message_and_applies() {
        use crate::ui::categorypicker::PickerMode;
        let mut app = App::for_test_with_seeded_store();
        app.master_categories = vec![
            mailcore::graph::model::MasterCategory {
                display_name: "Work".into(),
                color: "preset0".into(),
            },
            mailcore::graph::model::MasterCategory {
                display_name: "Urgent".into(),
                color: "preset1".into(),
            },
        ];
        app.open_category_picker(PickerMode::Assign);
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain RefreshCategories
        app.category_picker_toggle(); // toggle the first item on
        app.apply_category_picker();
        assert!(app.category_picker.is_none());
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::SetCategories { categories, .. }) => {
                assert_eq!(categories.len(), 1);
            }
            other => panic!("expected SetCategories, got {other:?}"),
        }
    }

    #[test]
    fn filter_shows_only_matching_and_clears() {
        use crate::ui::categorypicker::PickerMode;
        use mailcore::graph::model::{Message, Recipient};
        let mut app = App::for_test_with_seeded_store();
        app.master_categories = vec![mailcore::graph::model::MasterCategory {
            display_name: "Work".into(),
            color: "preset0".into(),
        }];
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "w".into(),
                    conversation_id: "c2".into(),
                    subject: "Work item".into(),
                    from: Recipient {
                        name: "B".into(),
                        address: "b@x".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-19T11:00:00Z".into(),
                    sent: "".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "p".into(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: vec!["Work".into()],
                },
            )
            .unwrap();
        app.reload_messages();
        app.open_category_picker(PickerMode::Filter);
        app.apply_category_picker(); // highlight = "Work" (only master item)
        assert_eq!(app.category_filter.as_deref(), Some("Work"));
        assert!(
            app.messages
                .iter()
                .all(|m| m.categories.contains(&"Work".to_string()))
        );
        assert_eq!(app.messages.len(), 1);
        app.clear_category_filter();
        assert!(app.category_filter.is_none());
        assert!(app.messages.len() >= 2); // m1 back
    }

    #[test]
    fn calendar_decline_with_proposed_time_sends_respond_event() {
        use crate::app::{RsvpField, RsvpTarget};
        let mut app = App::for_test_with_seeded_store();
        app.rsvp_prompt = Some(crate::app::RsvpPrompt {
            target: RsvpTarget::Event("E1".into()),
            kind: "declined".into(),
            comment: String::new(),
            proposed_start: "2026-07-21 14:00".into(),
            proposed_end: "2026-07-21 15:00".into(),
            focus: RsvpField::ProposedStart,
        });
        app.submit_rsvp();
        let mut found = None;
        while let Ok(cmd) = app.test_cmd_rx.as_ref().unwrap().try_recv() {
            if let SyncCommand::RespondEvent {
                proposed_start_utc, ..
            } = &cmd
            {
                found = proposed_start_utc.clone();
                break;
            }
        }
        assert!(found.is_some_and(|s| s.ends_with('Z')));
        assert!(app.rsvp_prompt.is_none());
    }

    #[test]
    fn mail_d_opens_prompt_and_a_is_instant() {
        use crate::app::{RsvpField, RsvpTarget};
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app);
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain open
        app.on_key_char('A');
        assert!(app.rsvp_prompt.is_none());
        assert!(matches!(
            app.test_cmd_rx.as_ref().unwrap().try_recv(),
            Ok(SyncCommand::RespondMeeting { .. })
        ));
        app.on_key_char('D');
        let p = app.rsvp_prompt.as_ref().unwrap();
        assert!(matches!(p.target, RsvpTarget::Message(_)));
        assert_eq!(p.focus, RsvpField::ProposedStart);
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());
    }

    #[test]
    fn mail_decline_with_proposed_time_sends_respond_meeting() {
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app);
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {}
        app.start_meeting_rsvp("declined");
        let p = app.rsvp_prompt.as_mut().unwrap();
        p.proposed_start = "2026-07-21 14:00".into();
        p.proposed_end = "2026-07-21 15:00".into();
        app.submit_rsvp();
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::RespondMeeting {
                kind,
                proposed_start_utc,
                ..
            }) => {
                assert_eq!(kind, mailcore::graph::client::RsvpKind::Decline);
                assert!(proposed_start_utc.is_some());
            }
            other => panic!("expected RespondMeeting, got {other:?}"),
        }
    }

    #[test]
    fn half_filled_proposed_time_errors_and_sends_nothing() {
        use crate::app::{RsvpField, RsvpTarget};
        let mut app = App::for_test_with_seeded_store();
        app.rsvp_prompt = Some(crate::app::RsvpPrompt {
            target: RsvpTarget::Event("E1".into()),
            kind: "declined".into(),
            comment: String::new(),
            proposed_start: "2026-07-21 14:00".into(),
            proposed_end: String::new(), // half-filled
            focus: RsvpField::ProposedStart,
        });
        app.submit_rsvp();
        assert!(app.rsvp_prompt.is_some()); // stayed open
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());
        assert_eq!(app.error_notice.as_deref(), Some("Invalid proposed time"));
    }

    #[test]
    fn respond_meeting_on_an_invite_sends_command() {
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app);
        app.respond_meeting(RsvpKind::Accept);
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::RespondMeeting {
                message_id, kind, ..
            }) => {
                assert_eq!(message_id, "invite1");
                assert_eq!(kind, RsvpKind::Accept);
            }
            other => panic!("expected RespondMeeting, got {other:?}"),
        }
    }

    #[test]
    fn respond_meeting_on_ordinary_mail_is_a_noop() {
        let mut app = App::for_test_with_seeded_store();
        app.open_message("m1"); // m1 is an ordinary message
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain open's FetchBody
        app.respond_meeting(RsvpKind::Accept);
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err()); // nothing sent
    }

    #[test]
    fn uppercase_a_d_t_route_to_respond_meeting_only_for_invites() {
        // Ordinary mail: A/D/T do nothing (no command, no prompt).
        let mut app = App::for_test_with_seeded_store();
        app.open_message("m1");
        while app.test_cmd_rx.as_ref().unwrap().try_recv().is_ok() {} // drain open's FetchBody
        app.on_key_char('A');
        app.on_key_char('D');
        app.on_key_char('T');
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());
        assert!(app.rsvp_prompt.is_none());

        // Invite: 'D' opens the RSVP prompt with the matching kind (the send
        // happens on submit — see mail_d_opens_prompt_and_a_is_instant).
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app);
        app.on_key_char('D');
        assert_eq!(app.rsvp_prompt.as_ref().unwrap().kind, "declined");
    }

    #[test]
    fn meeting_responded_notice_and_marks_read() {
        let mut app = App::for_test_with_seeded_store();
        open_meeting_invite(&mut app);
        app.on_sync_event(SyncEvent::MeetingResponded {
            message_id: "invite1".into(),
            kind: RsvpKind::Tentative,
        });
        assert_eq!(
            app.attachment_notice.as_deref(),
            Some("Tentatively accepted the invite")
        );
        // Marked read locally.
        let rows = app.store.messages_in_folder("inbox", 50, 0).unwrap();
        assert!(rows.iter().find(|m| m.id == "invite1").unwrap().is_read);
    }

    /// Adds a second message to conversation `c1` (m1's conversation) so `c1`
    /// becomes a 2-message thread: from Bob, newer than m1, unread. Shared
    /// across UI render tests (e.g. `ui::message_list`) that need a
    /// multi-message thread fixture, not just `app`'s own tests.
    pub(crate) fn seed_second_in_c1(app: &App) {
        use mailcore::graph::model::{Message, Recipient};
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m2".into(),
                    conversation_id: "c1".into(),
                    subject: "Re: Hello".into(),
                    from: Recipient {
                        name: "Bob".into(),
                        address: "bob@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-16T11:00:00Z".into(), // newer than m1 (10:00)
                    sent: "".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "re hi".into(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: Vec::new(),
                },
            )
            .expect("seed m2");
    }

    /// Adds a standalone message in its own conversation `c2` (a singleton).
    fn seed_singleton_c2(app: &App) {
        use mailcore::graph::model::{Message, Recipient};
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m3".into(),
                    conversation_id: "c2".into(),
                    subject: "Standalone".into(),
                    from: Recipient {
                        name: "Carol".into(),
                        address: "carol@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-15T10:00:00Z".into(),
                    sent: "".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "alone".into(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: Vec::new(),
                },
            )
            .expect("seed m3");
    }

    #[test]
    fn threaded_reload_groups_into_visible_rows() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 now has m1 + m2 → a 2-message thread
        seed_singleton_c2(&app); // c2 is a 1-message thread
        app.reload_messages();

        // The multi-message thread (c1) is a Header; the singleton (c2) a bare Message.
        assert!(app.visible_rows.iter().any(|r| matches!(r, Row::Header(_))));
        assert!(
            app.visible_rows
                .iter()
                .any(|r| matches!(r, Row::Message(_, _)))
        );
    }

    #[test]
    fn threaded_reload_expands_to_show_children() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app);
        app.reload_messages();
        let pos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        let before = app.visible_rows.len();
        if let Row::Header(t) = app.visible_rows[pos] {
            app.threads[t].expanded = true;
            app.rebuild_visible_rows();
        }
        assert!(app.visible_rows.len() > before); // child rows appeared
    }

    #[test]
    fn thread_navigation_is_clamped_over_visible_rows() {
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 header
        seed_singleton_c2(&app); // c2 message → visible_rows.len() >= 2
        app.reload_messages();
        app.row_index = 0;
        app.move_thread_selection(-1);
        assert_eq!(app.row_index, 0); // clamped at the top
        let last = app.visible_rows.len().saturating_sub(1);
        app.row_index = last;
        app.move_thread_selection(1);
        assert_eq!(app.row_index, last); // clamped at the bottom
    }

    #[test]
    fn activating_a_collapsed_header_expands_and_opens_latest() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = [m1 10:00, m2 11:00]; latest = m2
        app.reload_messages();
        let pos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        app.row_index = pos;
        app.activate_thread_row();
        if let Row::Header(t) = app.visible_rows[pos] {
            assert!(app.threads[t].expanded);
            assert_eq!(app.selected_msg.as_deref(), Some("m2")); // newest opened
        }
    }

    #[test]
    fn reload_with_no_folder_resets_cursors() {
        let mut app = App::for_test_with_seeded_store();
        app.msg_index = 5;
        app.row_index = 5;
        app.selected_folder = None;
        app.reload_messages();

        assert_eq!(app.msg_index, 0);
        assert_eq!(app.row_index, 0);
        assert!(app.messages.is_empty());
        assert!(app.visible_rows.is_empty());
    }

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
    fn mark_read_on_a_collapsed_header_marks_the_whole_thread() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = m1 + m2, both unread
        app.reload_messages();
        let pos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        app.row_index = pos;

        app.mark_read(true);

        // Both messages are now read in the store-backed (reloaded) thread...
        let hpos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        if let Row::Header(t) = app.visible_rows[hpos] {
            assert!(app.threads[t].thread.messages.iter().all(|m| m.is_read));
        }
        // ...and one MarkRead command was enqueued per message.
        let mut count = 0;
        while let Ok(SyncCommand::MarkRead { read: true, .. }) =
            app.test_cmd_rx.as_ref().unwrap().try_recv()
        {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn toggle_flag_on_mixed_thread_flags_all() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = m1 + m2, both start unflagged
        app.store.set_flag("m1", true); // now MIXED: m1 flagged, m2 not
        app.reload_messages();
        let pos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        app.row_index = pos;

        app.toggle_flag();

        // The mixed thread must be completed (every message flagged), not
        // stripped — this is the regression the `!any_flagged` bug got wrong.
        let hpos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        if let Row::Header(t) = app.visible_rows[hpos] {
            assert!(app.threads[t].thread.messages.iter().all(|m| m.is_flagged));
        }
        // ...and one SetFlag { flagged: true } command was enqueued per message.
        let mut count = 0;
        while let Ok(SyncCommand::SetFlag { flagged: true, .. }) =
            app.test_cmd_rx.as_ref().unwrap().try_recv()
        {
            count += 1;
        }
        assert_eq!(count, 2);
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
    fn reading_scroll_clamps_and_resets_on_open() {
        let mut app = App::for_test_with_seeded_store();
        app.reading_scroll = 0;
        app.reading_viewport = 5; // 5 visible body rows
        app.reading_content_rows = 20; // 20 total rows
        app.reading_scroll_by(100); // way past the end
        assert_eq!(app.reading_scroll, 15); // clamped to content(20) - viewport(5)
        app.reading_scroll_by(-100);
        assert_eq!(app.reading_scroll, 0);
        // opening a message resets scroll
        app.reading_scroll = 7;
        app.open_message("m1");
        assert_eq!(app.reading_scroll, 0);
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
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
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
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
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
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
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
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
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
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
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
                        content_id: None,
                        kind: AttachmentKind::File,
                        source_url: None,
                    },
                    AttachmentMeta {
                        id: "a2".into(),
                        name: "two.txt".into(),
                        content_type: "text/plain".into(),
                        size: 1,
                        is_inline: false,
                        content_id: None,
                        kind: AttachmentKind::File,
                        source_url: None,
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

    /// Seeds a single attachment for "m1" (the fixture's highlighted
    /// message) and opens the attachments popup on it — the one-attachment
    /// shorthand for the repeated `put_attachments` + `open_attachments_popup`
    /// pattern above, for tests that only care about a single `kind`.
    fn seed_one_attachment(app: &mut App, meta: AttachmentMeta) {
        app.store
            .put_attachments("m1", &[meta])
            .expect("seed attachment");
        app.open_attachments_popup();
    }

    #[test]
    fn saving_an_item_attachment_sends_save_item_command_with_extensionless_base() {
        let mut app = App::for_test_with_seeded_store();
        seed_one_attachment(
            &mut app,
            AttachmentMeta {
                id: "a1".into(),
                name: "Invite.ics".into(),
                content_type: "".into(),
                size: 0,
                is_inline: false,
                content_id: None,
                kind: AttachmentKind::Item,
                source_url: None,
            },
        );
        app.save_attachment(); // Enter
        match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::SaveItemAttachment { dest_base, .. }) => {
                // extension stripped so the engine can choose .ics/.eml
                assert_eq!(dest_base.extension(), None);
                assert!(
                    dest_base
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("Invite")
                );
            }
            other => panic!("expected SaveItemAttachment, got {other:?}"),
        }
    }

    #[test]
    fn strip_item_ext_handles_non_ascii_names_without_panicking() {
        assert_eq!(strip_item_ext("Hi 😀!"), "Hi 😀!"); // no .eml/.ics suffix → unchanged, must not panic
        assert_eq!(strip_item_ext("Résumé.eml"), "Résumé"); // non-ASCII before an ASCII .eml suffix → stripped
        assert_eq!(strip_item_ext("Invite.ics"), "Invite"); // ASCII case still works
        assert_eq!(strip_item_ext("report"), "report");
        assert_eq!(strip_item_ext("invoice.2024"), "invoice.2024"); // non-.eml/.ics ext untouched
    }

    #[test]
    fn opening_a_reference_attachment_opens_the_link_and_sends_no_command() {
        let mut app = App::for_test_with_seeded_store();
        seed_one_attachment(
            &mut app,
            AttachmentMeta {
                id: "a2".into(),
                name: "Doc".into(),
                content_type: "".into(),
                size: 0,
                is_inline: false,
                content_id: None,
                kind: AttachmentKind::Reference,
                source_url: Some("https://x/y".into()),
            },
        );
        let before = app.open_invocations.get();
        app.save_attachment(); // Enter -> opens the link
        assert_eq!(app.open_invocations.get(), before + 1); // OS handler invoked
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err()); // no command
        assert!(app.attachments.is_none()); // popup closed
    }

    #[test]
    fn opening_a_reference_attachment_with_no_link_leaves_the_popup_open() {
        let mut app = App::for_test_with_seeded_store();
        seed_one_attachment(
            &mut app,
            AttachmentMeta {
                id: "a3".into(),
                name: "Doc".into(),
                content_type: "".into(),
                size: 0,
                is_inline: false,
                content_id: None,
                kind: AttachmentKind::Reference,
                source_url: None,
            },
        );
        let before = app.open_invocations.get();
        app.save_attachment();
        assert_eq!(app.open_invocations.get(), before); // no OS handler invoked
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err()); // no command
        assert!(app.attachments.is_some()); // popup stays open
        assert_eq!(
            app.attachment_notice.as_deref(),
            Some("No link for this attachment")
        );
    }

    #[test]
    fn opening_a_reference_attachment_with_a_non_web_url_refuses_to_open() {
        let mut app = App::for_test_with_seeded_store();
        seed_one_attachment(
            &mut app,
            AttachmentMeta {
                id: "a4".into(),
                name: "Doc".into(),
                content_type: "".into(),
                size: 0,
                is_inline: false,
                content_id: None,
                kind: AttachmentKind::Reference,
                source_url: Some(r"\\attacker\share".into()),
            },
        );
        let before = app.open_invocations.get();
        app.save_attachment();
        assert_eq!(app.open_invocations.get(), before); // no OS handler invoked
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err()); // no command
        assert!(app.attachments.is_some()); // popup stays open
        assert_eq!(
            app.attachment_notice.as_deref(),
            Some("Refusing to open non-web link")
        );
    }

    #[test]
    fn finish_attachment_save_opens_item_file_by_stem() {
        let mut app = App::for_test_with_seeded_store();
        let base = downloads_dir().join("Invite");
        app.pending_saves.insert(base.clone(), true); // open_after = true
        let before = app.open_invocations.get();
        app.finish_attachment_save(base.with_extension("ics")); // engine chose .ics
        assert_eq!(app.open_invocations.get(), before + 1); // opened via stem match
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
                    is_meeting_request: false,
                    categories: Vec::new(),
                },
            )
            .expect("update message to has_attachments=true");
        app.reload_messages();
    }

    /// Seeds a message `id` (into the "inbox" folder `for_test_with_seeded_store`
    /// already creates) with an HTML `Body` whose content is `html_content` —
    /// mirroring `seed_message_with_has_attachments_but_no_local_rows`'s
    /// message-row shape plus `opening_a_message_with_a_cached_body_renders_it_
    /// without_fetching`'s `put_body` — for cid-image-resolution tests.
    fn seed_html_message(app: &mut App, id: &str, html_content: &str) {
        use mailcore::graph::model::{Message, Recipient};
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: id.into(),
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
                    is_meeting_request: false,
                    categories: Vec::new(),
                },
            )
            .expect("seed message");
        app.store
            .put_body(
                id,
                &Body {
                    content_type: "html".into(),
                    content: html_content.into(),
                },
            )
            .expect("seed body");
    }

    #[test]
    fn opening_a_message_with_cid_images_requests_their_bytes() {
        let mut app = App::for_test_with_seeded_store();
        // Seed a message with an HTML body referencing cid:logo, and its
        // attachment metadata.
        seed_html_message(
            &mut app,
            "mimg",
            r#"<p>hi</p><img src="cid:logo"><p>bye</p>"#,
        );
        app.store
            .put_attachments(
                "mimg",
                &[AttachmentMeta {
                    id: "att1".into(),
                    name: "logo.png".into(),
                    content_type: "image/png".into(),
                    size: 3,
                    is_inline: true,
                    content_id: Some("logo".into()),
                    kind: AttachmentKind::File,
                    source_url: None,
                }],
            )
            .unwrap();

        app.open_message("mimg"); // loads body + should request inline images

        // Exactly one FetchInlineImage for att1/logo should be enqueued (a
        // FetchBody and/or FetchAttachments may also be queued — ignore
        // those). Drain the whole queue rather than stopping at the first
        // match, so a duplicate fetch for the same cid doesn't hide behind
        // an early `break`.
        let cmd_rx = app.test_cmd_rx.as_ref().unwrap();
        let mut logo_fetches = Vec::new();
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let SyncCommand::FetchInlineImage {
                ref attachment_id,
                ref content_id,
                ..
            } = cmd
            {
                if content_id == "logo" {
                    assert_eq!(attachment_id, "att1");
                    logo_fetches.push(cmd);
                }
            }
        }
        assert_eq!(
            logo_fetches.len(),
            1,
            "expected exactly one FetchInlineImage for att1/logo, got {logo_fetches:?}"
        );

        // Delivering the bytes caches them by content_id:
        app.on_sync_event(SyncEvent::InlineImageReady {
            message_id: "mimg".into(),
            content_id: "logo".into(),
            bytes: vec![1, 2, 3],
        });
        assert_eq!(
            app.inline_images.get("logo").map(|b| b.as_slice()),
            Some(&[1, 2, 3][..])
        );
    }

    /// Graph's `contentId` may come back angle-bracketed (`<logo@x>`) even
    /// when the body's `cid:` token is bare — `request_inline_images` must
    /// normalize both sides before comparing, or every cid image on a
    /// message whose attachment metadata is bracketed silently falls back to
    /// the box (see `normalize_cid`).
    #[test]
    fn cid_matching_tolerates_angle_bracketed_content_ids() {
        let mut app = App::for_test_with_seeded_store();
        seed_html_message(
            &mut app,
            "mimg2",
            r#"<p>hi</p><img src="cid:logo@x"><p>bye</p>"#,
        );
        app.store
            .put_attachments(
                "mimg2",
                &[AttachmentMeta {
                    id: "att1".into(),
                    name: "logo.png".into(),
                    content_type: "image/png".into(),
                    size: 3,
                    is_inline: true,
                    content_id: Some("<logo@x>".into()),
                    kind: AttachmentKind::File,
                    source_url: None,
                }],
            )
            .unwrap();

        app.open_message("mimg2");

        let cmd_rx = app.test_cmd_rx.as_ref().unwrap();
        let mut logo_fetches = Vec::new();
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let SyncCommand::FetchInlineImage {
                ref attachment_id,
                ref content_id,
                ..
            } = cmd
            {
                if content_id == "logo@x" {
                    assert_eq!(attachment_id, "att1");
                    logo_fetches.push(cmd);
                }
            }
        }
        assert_eq!(
            logo_fetches.len(),
            1,
            "expected exactly one FetchInlineImage for att1/logo@x, got {logo_fetches:?}"
        );
    }

    /// A cached HTML body (cid image, no attachment metadata seeded yet)
    /// makes `request_inline_images` run twice during `open_message`
    /// (once from the trailing call, once again from `reload_body`'s
    /// cache-hit branch) — both see empty `metas` and must not each send
    /// their own `FetchAttachments`; only one should ever go out per
    /// message.
    #[test]
    fn opening_a_cached_html_message_sends_only_one_fetch_attachments() {
        let mut app = App::for_test_with_seeded_store();
        seed_html_message(
            &mut app,
            "mimg3",
            r#"<p>hi</p><img src="cid:logo"><p>bye</p>"#,
        );
        // Deliberately no `put_attachments` call — metadata is unknown.

        app.open_message("mimg3");

        let cmd_rx = app.test_cmd_rx.as_ref().unwrap();
        let mut fetch_attachments = Vec::new();
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let SyncCommand::FetchAttachments { ref message_id } = cmd {
                if message_id == "mimg3" {
                    fetch_attachments.push(cmd);
                }
            }
        }
        assert_eq!(
            fetch_attachments.len(),
            1,
            "expected exactly one FetchAttachments for mimg3, got {fetch_attachments:?}"
        );
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
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
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
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
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
    fn inbox_expands_once_on_first_run_then_respects_user() {
        let mut app = App::for_test_with_folder_tree(); // initialized = false
        let inbox_expanded = |a: &App| a.folders.iter().any(|f| f.id == "inbox" && f.is_expanded);
        assert!(!inbox_expanded(&app)); // seeded collapsed
        app.ensure_folder_tree_initialized();
        assert!(inbox_expanded(&app)); // auto-expanded on first run

        // User collapses the Inbox.
        app.folder_index = app
            .visible_folders
            .iter()
            .position(|v| v.row.id == "inbox")
            .unwrap();
        app.collapse_or_parent();
        assert!(!inbox_expanded(&app));

        // A later init attempt is a no-op — the flag was consumed.
        app.ensure_folder_tree_initialized();
        assert!(!inbox_expanded(&app));
    }

    #[test]
    fn expand_and_collapse_selected_folder() {
        let mut app = App::for_test_with_folder_tree();
        app.focus = Pane::Folders;
        app.folder_index = 0; // Inbox
        app.expand_selected();
        assert!(app.visible_folders.iter().any(|v| v.row.id == "epam"));
        app.collapse_or_parent();
        assert!(!app.visible_folders.iter().any(|v| v.row.id == "epam"));
    }

    #[test]
    fn collapse_on_a_child_moves_selection_to_its_parent() {
        let mut app = App::for_test_with_folder_tree();
        app.store.set_folder_expanded("inbox", true).unwrap();
        app.reload_folders();
        app.folder_index = app
            .visible_folders
            .iter()
            .position(|v| v.row.id == "epam")
            .unwrap();
        app.collapse_or_parent(); // EPAM is a leaf → jump to parent
        assert_eq!(app.selected_folder.as_deref(), Some("inbox"));
    }

    #[test]
    fn reload_folders_builds_the_visible_tree() {
        let mut app = App::for_test_with_folder_tree();
        // Collapsed by default: EPAM (child of Inbox) is hidden.
        let ids: Vec<_> = app
            .visible_folders
            .iter()
            .map(|v| v.row.id.clone())
            .collect();
        assert_eq!(ids, vec!["inbox".to_string(), "sent".to_string()]);
        // Expand Inbox → EPAM becomes visible at depth 1.
        app.store.set_folder_expanded("inbox", true).unwrap();
        app.reload_folders();
        let shape: Vec<_> = app
            .visible_folders
            .iter()
            .map(|v| (v.row.id.clone(), v.depth))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("inbox".to_string(), 0),
                ("epam".to_string(), 1),
                ("sent".to_string(), 0),
            ]
        );
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
    fn saving_a_compose_with_bcc_persists_it_to_the_draft_row() {
        let mut app = App::for_test_with_seeded_store();
        let id = app
            .store
            .create_local_draft("Subj", "a@x", "", "<p>body</p>")
            .unwrap();
        app.compose = Some(Compose {
            to: "a@x".into(),
            cc: "".into(),
            bcc: "secret@x".into(),
            subject: "Subj".into(),
            editor: Editor::from(compose_html::from_html("<p>body</p>")),
            focus: ComposeField::Body,
            draft_id: id.clone(),
            autocomplete: None,
            attachments: Vec::new(),
        });
        app.compose_action = Some(ComposeAction::Save);

        app.apply_compose_action();

        let (row, _) = app.store.draft(&id).unwrap().unwrap();
        assert_eq!(row.bcc_recipients, "secret@x");
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

    /// Regression test for the final-review Critical bug: reply/forward used
    /// to read the stale flat `messages`/`msg_index` even in threaded mode
    /// (only the triage verbs were made thread-aware via
    /// `threaded_target_ids`), so `r`/`R`/`F` could target the wrong message.
    /// `compose_reply`/`compose_forward` both go through
    /// `highlighted_message_id`, so exercising `compose_reply` here covers
    /// both.
    #[test]
    fn reply_in_threaded_mode_targets_message_under_cursor() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = m1@10:00, m2@11:00; latest = m2
        app.reload_messages();

        // Cursor on the (collapsed) header: targets the thread's latest
        // message (m2), matching Enter/activate's behavior — NOT the stale
        // flat `messages[0]`, which is "m1".
        let pos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        app.row_index = pos;

        app.compose_reply(false);
        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(
            last,
            Ok(SyncCommand::ComposeReply { id, all }) if id == "m2" && !all
        ));

        // Expand the header and move the cursor onto m1's own child row —
        // now it must target "m1" specifically, proving this follows the
        // cursor rather than always landing on the flat list's message.
        if let Row::Header(t) = app.visible_rows[pos] {
            app.threads[t].expanded = true;
            app.rebuild_visible_rows();
        }
        let child_pos = app
            .visible_rows
            .iter()
            .position(|r| {
                if let Row::Message(t, m) = r {
                    app.threads[*t].thread.messages[*m].id == "m1"
                } else {
                    false
                }
            })
            .unwrap();
        app.row_index = child_pos;

        app.compose_reply(false);
        let last = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(
            last,
            Ok(SyncCommand::ComposeReply { id, all }) if id == "m1" && !all
        ));
    }

    /// Regression test for the same Critical bug, for the attachments popup
    /// (`a`): it used to index the stale flat `messages[msg_index]` even in
    /// threaded mode. Seeds distinct attachments on m1 and m2 so the test can
    /// tell which message's popup actually opened, not just that some popup did.
    #[test]
    fn attachments_in_threaded_mode_uses_cursor_message() {
        use crate::app::Row;
        use mailcore::graph::model::{AttachmentMeta, Message, Recipient};

        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = m1@10:00, m2@11:00; latest = m2

        // Mark both messages as having attachments (re-upsert; the seeded
        // fixture and `seed_second_in_c1` both start with `has_attachments:
        // false`), and give each a distinct local attachment row.
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
                    is_meeting_request: false,
                    categories: Vec::new(),
                },
            )
            .expect("mark m1 has_attachments");
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m2".into(),
                    conversation_id: "c1".into(),
                    subject: "Re: Hello".into(),
                    from: Recipient {
                        name: "Bob".into(),
                        address: "bob@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-16T11:00:00Z".into(),
                    sent: "".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: true,
                    importance: "normal".into(),
                    preview: "re hi".into(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: Vec::new(),
                },
            )
            .expect("mark m2 has_attachments");
        app.store
            .put_attachments(
                "m1",
                &[AttachmentMeta {
                    id: "a1".into(),
                    name: "one.txt".into(),
                    content_type: "text/plain".into(),
                    size: 1,
                    is_inline: false,
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
                }],
            )
            .expect("seed m1 attachment");
        app.store
            .put_attachments(
                "m2",
                &[AttachmentMeta {
                    id: "a2".into(),
                    name: "two.txt".into(),
                    content_type: "text/plain".into(),
                    size: 1,
                    is_inline: false,
                    content_id: None,
                    kind: AttachmentKind::File,
                    source_url: None,
                }],
            )
            .expect("seed m2 attachment");
        app.reload_messages();

        // Cursor on the (collapsed) header: targets the thread's latest
        // message (m2)'s attachments.
        let pos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        app.row_index = pos;

        app.open_attachments_popup();
        {
            let popup = app
                .attachments
                .as_ref()
                .expect("popup should open for the header's latest message");
            assert_eq!(popup.message_id, "m2");
            assert_eq!(popup.items[0].name, "two.txt");
        }
        app.attachments = None;

        // Expand and move onto m1's own child row: now it must target m1's
        // attachments specifically.
        if let Row::Header(t) = app.visible_rows[pos] {
            app.threads[t].expanded = true;
            app.rebuild_visible_rows();
        }
        let child_pos = app
            .visible_rows
            .iter()
            .position(|r| {
                if let Row::Message(t, m) = r {
                    app.threads[*t].thread.messages[*m].id == "m1"
                } else {
                    false
                }
            })
            .unwrap();
        app.row_index = child_pos;

        app.open_attachments_popup();
        let popup = app
            .attachments
            .as_ref()
            .expect("popup should open for m1's row");
        assert_eq!(popup.message_id, "m1");
        assert_eq!(popup.items[0].name, "one.txt");
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

    #[test]
    fn signature_body_html_wraps_lines_and_is_empty_when_blank() {
        assert_eq!(signature_body_html(""), "");
        assert_eq!(signature_body_html("   "), "");
        let html = signature_body_html("Boris\nEPAM");
        assert!(html.contains("<p>--</p>"));
        assert!(html.contains("<p>Boris</p>"));
        assert!(html.contains("<p>EPAM</p>"));
    }

    #[test]
    fn compose_new_seeds_the_signature_into_the_draft_body() {
        let mut app = App::for_test_with_seeded_store();
        app.signature = "Boris".into();
        app.compose_new();
        let editor_text = app.compose.as_ref().unwrap().editor.text.plain();
        assert!(editor_text.contains("Boris")); // signature landed in the composer body
    }

    #[test]
    fn attaching_a_file_records_it_on_the_draft_and_in_compose() {
        let mut app = App::for_test_with_seeded_store();
        app.compose_new(); // opens a fresh local draft
        let draft_id = app.compose.as_ref().unwrap().draft_id.clone();
        // a real temp file to attach
        let dir = std::env::temp_dir().join(format!("lookxy-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("r.pdf");
        std::fs::write(&file, b"pdfbytes").unwrap();

        app.attach_file(&file);

        // stored on the draft AND reflected in the composer's list
        assert_eq!(app.store.outbound_attachments(&draft_id).unwrap().len(), 1);
        assert_eq!(app.compose.as_ref().unwrap().attachments.len(), 1);
        assert_eq!(app.compose.as_ref().unwrap().attachments[0].name, "r.pdf");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_last_attachment_removes_the_most_recent_and_stays_consistent() {
        let mut app = App::for_test_with_seeded_store();
        app.compose_new(); // opens a fresh local draft
        let draft_id = app.compose.as_ref().unwrap().draft_id.clone();

        // two real temp files, attached in a known order
        let dir = std::env::temp_dir().join(format!("lookxy-remove-last-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_a = dir.join("a.txt");
        let file_b = dir.join("b.txt");
        std::fs::write(&file_a, b"aaa").unwrap();
        std::fs::write(&file_b, b"bbb").unwrap();

        app.attach_file(&file_a);
        app.attach_file(&file_b);

        // the most-recently-attached file is last in attach order
        assert_eq!(
            app.compose
                .as_ref()
                .unwrap()
                .attachments
                .last()
                .unwrap()
                .name,
            "b.txt"
        );

        app.remove_last_attachment();

        // gone from the in-memory list...
        let names: Vec<String> = app
            .compose
            .as_ref()
            .unwrap()
            .attachments
            .iter()
            .map(|a| a.name.clone())
            .collect();
        assert_eq!(names, vec!["a.txt"]);
        // ...and from the store, and the two stay in agreement.
        let stored = app.store.outbound_attachments(&draft_id).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].name, "a.txt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_draft_loads_existing_attachments() {
        let mut app = App::for_test_with_seeded_store();
        app.compose_new();
        let draft_id = app.compose.as_ref().unwrap().draft_id.clone();
        app.store
            .add_outbound_attachment(&draft_id, "/tmp/x.txt", "x.txt", 3)
            .unwrap();
        app.open_draft(&draft_id); // reopen
        assert_eq!(app.compose.as_ref().unwrap().attachments.len(), 1);
    }

    #[test]
    fn t_key_toggles_threaded_and_rebuilds() {
        let mut app = App::for_test_with_seeded_store();
        // starts flat by construction default; config_path is None → no disk write
        seed_second_in_c1(&app);
        app.reload_messages();
        assert!(!app.messages.is_empty()); // flat list populated

        app.on_key_char('t');
        assert!(app.threaded);
        assert!(!app.visible_rows.is_empty()); // threaded view built

        app.on_key_char('t');
        assert!(!app.threaded);
        assert!(!app.messages.is_empty()); // back to flat
    }

    #[test]
    fn deleting_a_thread_confirms_then_deletes_every_message() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app); // c1 = m1 + m2
        app.reload_messages();
        let pos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        app.row_index = pos;

        // First `d` only opens the confirm modal — nothing deleted yet.
        app.delete_selected();
        assert!(app.confirm.is_some());
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());

        // Confirming deletes all messages and enqueues one Delete per message.
        app.confirm_yes();
        assert!(app.confirm.is_none());
        let mut count = 0;
        while let Ok(SyncCommand::Delete { .. }) = app.test_cmd_rx.as_ref().unwrap().try_recv() {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn canceling_the_confirm_deletes_nothing() {
        use crate::app::Row;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        seed_second_in_c1(&app);
        app.reload_messages();
        let pos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        app.row_index = pos;
        app.delete_selected();
        app.cancel_confirm();
        assert!(app.confirm.is_none());
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());
    }

    #[test]
    fn moving_a_thread_confirms_then_moves_every_message() {
        use crate::app::Row;
        use mailcore::graph::model::MailFolder;
        let mut app = App::for_test_with_seeded_store();
        app.threaded = true;
        app.store
            .upsert_folder(&MailFolder {
                id: "archive".into(),
                display_name: "Archive".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("archive".into()),
            })
            .expect("seed archive folder");
        seed_second_in_c1(&app); // c1 = m1 + m2
        app.reload_messages();
        let pos = app
            .visible_rows
            .iter()
            .position(|r| matches!(r, Row::Header(_)))
            .unwrap();
        app.row_index = pos;

        // `v` must open the picker on a thread — this is the regression the
        // threaded-id capture in `open_move_picker` fixed.
        app.open_move_picker();
        assert!(app.move_picker.is_some());

        let archive_pos = app
            .move_picker
            .as_ref()
            .unwrap()
            .folders
            .iter()
            .position(|f| f.id == "archive")
            .unwrap();
        app.move_picker.as_mut().unwrap().index = archive_pos;

        // Enter on the picker only opens the confirm modal for a
        // multi-message thread — nothing moved yet.
        app.confirm_move();
        assert!(app.confirm.is_some());
        assert!(app.test_cmd_rx.as_ref().unwrap().try_recv().is_err());

        // Confirming moves every message and enqueues one Move per message.
        app.confirm_yes();
        assert!(app.confirm.is_none());
        let mut count = 0;
        while let Ok(SyncCommand::Move { dest, .. }) = app.test_cmd_rx.as_ref().unwrap().try_recv()
        {
            assert_eq!(dest, "archive");
            count += 1;
        }
        assert_eq!(count, 2);
    }

    // --- Event form: open new/edit ------------------------------------------

    /// `secs` seconds since the Unix epoch, formatted as the store's
    /// `YYYY-MM-DDTHH:MM:SSZ` — a local copy of `ui::calendar::unix_to_iso8601`
    /// (private to that module) built on the now-`pub(crate)` `civil_from_days`.
    fn unix_secs_to_iso(secs: i64) -> String {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (y, m, d) = crate::ui::calendar::civil_from_days(days);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    /// A 30-minute event `days_offset` days from the real "now" (at noon UTC,
    /// deliberately far from any local midnight rollover), for
    /// `App::reload_agenda` tests — same "anchor at the actual clock" shape
    /// as `ui::calendar`'s own `days_from_now` test helper (private to that
    /// module, so kept as its own copy here).
    fn seeded_event(
        id: &str,
        subject: &str,
        days_offset: i64,
        series_master_id: Option<String>,
    ) -> mailcore::store::NewEvent {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let start_secs = now + days_offset * 86_400 + 12 * 3600;
        let end_secs = start_secs + 1_800;
        mailcore::store::NewEvent {
            id: id.into(),
            subject: subject.into(),
            start_utc: unix_secs_to_iso(start_secs),
            end_utc: unix_secs_to_iso(end_secs),
            is_all_day: false,
            location: "Room 1".into(),
            organizer_name: "Boss".into(),
            organizer_addr: "boss@example.com".into(),
            response_status: "accepted".into(),
            series_master_id,
            body_preview: "".into(),
            web_link: "".into(),
            last_modified: "2020-01-01T00:00:00Z".into(),
            body_html: "<p>agenda</p>".into(),
            reminder_minutes: 0,
            is_reminder_on: false,
        }
    }

    #[test]
    fn open_new_event_starts_a_blank_form_with_prefilled_times() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        let f = app.event_form.as_ref().unwrap();
        assert!(f.editing_id.is_none());
        assert!(f.title.is_empty());
        assert!(!f.start.is_empty() && !f.end.is_empty()); // prefilled (next hour / +1h)
    }

    #[test]
    fn open_edit_event_prefills_from_the_selected_event() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.store
            .upsert_event(&seeded_event("e1", "Standup", 1, None))
            .unwrap();
        app.reload_agenda();
        app.selected_event = Some("e1".into());

        app.open_edit_event();

        let f = app.event_form.as_ref().unwrap();
        assert_eq!(f.editing_id.as_deref(), Some("e1"));
        assert_eq!(f.title, "Standup");
    }

    #[test]
    fn open_edit_event_refuses_a_recurring_event() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.store
            .upsert_event(&seeded_event(
                "e2",
                "Weekly Sync",
                1,
                Some("SERIES1".into()),
            ))
            .unwrap();
        app.reload_agenda();
        app.selected_event = Some("e2".into());

        app.open_edit_event();

        assert!(app.event_form.is_none()); // refused
    }

    #[test]
    fn saving_a_new_event_form_creates_and_enqueues_create_event() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() {
            f.title = "Planning".into();
            f.start = "2026-07-20 14:00".into();
            f.end = "2026-07-20 15:00".into();
        }
        app.save_event_form();
        assert!(app.event_form.is_none()); // form closed on success
        // a CreateEvent was enqueued and a local event exists
        let cmd = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(cmd, Ok(SyncCommand::CreateEvent { .. })));
    }

    #[test]
    fn saving_with_an_invalid_time_keeps_the_form_open() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() {
            f.title = "X".into();
            f.start = "nonsense".into();
        }
        app.save_event_form();
        assert!(app.event_form.is_some()); // still open — invalid start
    }

    #[test]
    fn saving_an_all_day_event_stores_midnight_boundaries_and_enqueues_create() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() {
            f.title = "Holiday".into();
            f.all_day = true;
            f.start = "2026-07-20".into();
            f.end = "2026-07-20".into(); // one-day all-day
        }
        app.save_event_form();
        assert!(app.event_form.is_none()); // saved + closed
        // a CreateEvent was enqueued...
        let draft_id = match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::CreateEvent { id }) => id,
            other => panic!("expected CreateEvent, got {other:?}"),
        };
        // ...and the stored event has midnight boundaries, end = start + 1 day, all-day set
        let send = app.store.event_for_send(&draft_id).unwrap().unwrap();
        assert_eq!(send.start_utc, "2026-07-20T00:00:00Z");
        assert_eq!(send.end_utc, "2026-07-21T00:00:00Z");
        assert!(send.is_all_day);
    }

    #[test]
    fn saving_an_all_day_event_with_an_unparseable_date_keeps_the_form_open() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() {
            f.all_day = true;
            f.start = "nonsense".into();
        }
        app.save_event_form();
        assert!(app.event_form.is_some()); // still open — invalid all-day date
    }

    /// BUG 1 (critical, whole-branch review): re-opening an all-day event for
    /// editing and saving it back unchanged must reproduce the exact same
    /// stored boundaries. `open_edit_event` used to prefill Start/End via
    /// `utc_iso_to_local` (offset conversion), which for an all-day event is
    /// wrong two ways: it prefills End from the *exclusive* `end_utc` (so
    /// `all_day_bounds` treats it as the last inclusive day and adds another
    /// day on save — the event grows by a day every edit), and on negative
    /// UTC offsets it shifts the Start date a day earlier. Dates on an
    /// all-day event are floating and must never be offset-converted — see
    /// `datetime::all_day_bounds`'s doc comment.
    #[test]
    fn editing_an_all_day_event_round_trips_the_same_boundaries() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() {
            f.title = "Holiday".into();
            f.all_day = true;
            f.start = "2026-07-20".into();
            f.end = "2026-07-20".into(); // one-day all-day
        }
        app.save_event_form();
        let id = match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::CreateEvent { id }) => id,
            other => panic!("expected CreateEvent, got {other:?}"),
        };

        // Re-open it for editing and save it back without changing anything.
        app.selected_event = Some(id.clone());
        app.open_edit_event();
        assert!(app.event_form.is_some());
        app.save_event_form();

        // The stored boundaries must be unchanged: editing an all-day event
        // must not grow or shift it.
        let send = app.store.event_for_send(&id).unwrap().unwrap();
        assert_eq!(send.start_utc, "2026-07-20T00:00:00Z");
        assert_eq!(send.end_utc, "2026-07-21T00:00:00Z");
        assert!(send.is_all_day);
    }

    /// SEAM 1: editing a not-yet-synced `local:` event must NOT enqueue an
    /// `UpdateEvent` — that event still has a pending `CreateEvent` op in the
    /// outbox from when it was first saved, and that op reads
    /// `Store::event_for_send` at drain time, so it already carries whatever
    /// this edit just wrote. Enqueuing `UpdateEvent` too would race it: if
    /// the `CreateEvent` drains first, it reconciles `local:X` to the real
    /// Graph id, and an `UpdateEvent{id: local:X}` sent after that finds no
    /// such row anymore. See `App::save_event_form`'s doc comment.
    #[test]
    fn editing_a_local_not_yet_synced_event_enqueues_no_update_event() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.open_new_event();
        if let Some(f) = app.event_form.as_mut() {
            f.title = "Planning".into();
            f.start = "2026-07-20 14:00".into();
            f.end = "2026-07-20 15:00".into();
        }
        app.save_event_form();

        // Drain the CreateEvent the initial save enqueued, and capture the
        // `local:` id it was for.
        let id = match app.test_cmd_rx.as_ref().unwrap().try_recv() {
            Ok(SyncCommand::CreateEvent { id }) => id,
            other => panic!("expected CreateEvent, got {other:?}"),
        };
        assert!(id.starts_with("local:"));

        // Edit that same still-not-yet-synced event.
        app.selected_event = Some(id.clone());
        app.open_edit_event();
        assert!(app.event_form.is_some());
        if let Some(f) = app.event_form.as_mut() {
            f.title = "Planning v2".into();
        }
        app.save_event_form();

        // The fields were written to the store locally...
        let send = app.store.event_for_send(&id).unwrap().unwrap();
        assert_eq!(send.subject, "Planning v2");
        // ...but nothing further was enqueued: no second CreateEvent and,
        // crucially, no UpdateEvent — only `update_event_fields` ran.
        let mut leftover = Vec::new();
        while let Ok(cmd) = app.test_cmd_rx.as_ref().unwrap().try_recv() {
            leftover.push(cmd);
        }
        assert!(
            leftover.is_empty(),
            "expected no SyncCommand from editing a local: event, got {leftover:?}"
        );
    }

    /// SEAM 2: `open_edit_event` loads the PLAIN TEXT of the stored HTML body
    /// (not the raw HTML source) into the form, so `save_event_form`'s
    /// `escape_html(&form.body)` round-trips without double-escaping. Before
    /// this fix, editing an event with an HTML body and re-saving it
    /// unchanged would turn `<p>agenda</p>` into `&lt;p&gt;agenda&lt;/p&gt;`.
    #[test]
    fn editing_an_event_with_an_html_body_round_trips_without_double_escaping() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        // seeded_event's body_html is "<p>agenda</p>".
        app.store
            .upsert_event(&seeded_event("e3", "Planning", 1, None))
            .unwrap();
        app.reload_agenda();
        app.selected_event = Some("e3".into());

        app.open_edit_event();
        // The form must hold the PLAIN TEXT, not the raw HTML source.
        assert_eq!(app.event_form.as_ref().unwrap().body, "agenda");

        // Re-save unchanged.
        app.save_event_form();

        let send = app.store.event_for_send("e3").unwrap().unwrap();
        assert_eq!(send.body_html, "agenda"); // stable — not "&lt;p&gt;agenda&lt;/p&gt;"
    }

    #[test]
    fn deleting_an_event_confirms_then_removes_and_enqueues() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.store
            .upsert_event(&seeded_event("e1", "Standup", 1, None))
            .unwrap();
        app.reload_agenda();
        app.selected_event = Some("e1".into());

        app.delete_selected_event();
        assert!(app.confirm.is_some()); // confirm modal opened, nothing deleted yet
        assert!(app.store.event_for_send("e1").unwrap().is_some());

        app.confirm_yes(); // execute
        assert!(app.confirm.is_none());
        assert!(app.store.event_for_send("e1").unwrap().is_none()); // removed locally
        let cmd = app.test_cmd_rx.as_ref().unwrap().try_recv();
        assert!(matches!(cmd, Ok(SyncCommand::DeleteEvent { .. })));
    }

    #[test]
    fn deleting_a_recurring_event_is_refused() {
        let mut app = App::for_test_with_seeded_store();
        app.mode = crate::app::Mode::Calendar;
        app.store
            .upsert_event(&seeded_event(
                "e2",
                "Weekly Sync",
                1,
                Some("SERIES1".into()),
            ))
            .unwrap();
        app.reload_agenda();
        app.selected_event = Some("e2".into());

        app.delete_selected_event();
        assert!(app.confirm.is_none()); // refused, no modal
    }
}
