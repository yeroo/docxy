//! `App` — the TUI's in-memory state: the local mail store, the background
//! sync handle, where the three panes (folders/list/reading) currently
//! point, and the triage actions (`m`/`u`/`f`/`d`/`v`) that mutate them.

use std::path::{Path, PathBuf};

use mailcore::graph::model::{AttachmentMeta, Body};
use mailcore::store::{FolderRow, MessageRow, Store};
use mailcore::sync::engine::{SyncCommand, SyncHandle, SyncState};

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
    /// In-flight `SaveAttachment` requests keyed by their `dest` path, each
    /// mapped to whether it should be opened once it completes (`o`) or
    /// merely reported (Enter). Keyed per-request (rather than one shared
    /// flag) so that saving attachment A with `o` and attachment B with
    /// Enter before A's `SyncEvent::AttachmentSaved` lands can't cross-wire
    /// — each completion looks up (and removes) its own path's intent. See
    /// `finish_attachment_save`.
    pending_saves: std::collections::HashMap<PathBuf, bool>,
    /// Test-only: counts calls to the OS-open seam (`open_with_os_handler`),
    /// which is a no-op under `cfg(test)` — see that function's doc comment.
    /// `None` for a production `App`.
    #[cfg(test)]
    pub open_invocations: std::cell::Cell<u32>,
    /// Test-only: sees every `SyncCommand` this `App` has sent, so tests can
    /// assert on it (see `last_sent_command_is_mark_read`). `None` for a
    /// production `App`; `for_test_with_seeded_store`/`for_test_with_empty_store`
    /// populate it by keeping the receiver end of a fresh channel instead of
    /// dropping it.
    #[cfg(test)]
    pub test_cmd_rx: Option<std::sync::mpsc::Receiver<SyncCommand>>,
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
    pub fn new(store: Store, sync: SyncHandle) -> App {
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
            pending_saves: std::collections::HashMap::new(),
            #[cfg(test)]
            open_invocations: std::cell::Cell::new(0),
            #[cfg(test)]
            test_cmd_rx: None,
        };
        app.reload_folders();
        app
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
        let results = self.store.search(&query, SEARCH_PAGE_SIZE).unwrap_or_default();
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

    /// Shells out to the OS's "open" handler for `path` (`cmd /c start` on
    /// Windows, `open` on macOS, `xdg-open` elsewhere), fire-and-forget.
    /// Compiled out under `cfg(test)` in favor of a counter
    /// (`open_invocations`) — so a test exercising `o` can assert the seam
    /// was reached without ever launching a real OS handler.
    #[cfg(not(test))]
    fn open_with_os_handler(&self, path: &Path) {
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", ""])
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

        let mut app = App::new(store, sync);
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
        let mut app = App::new(store, sync);
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

/// The per-account mail database path: `<lookxy_dir>\<sanitized-account>\mail.db`.
/// The account (a UPN like `me@epam.com`) is sanitized so it's a valid single
/// path component: `@`, `\`, and `/` become `_`.
pub fn store_path_for(account: &str) -> PathBuf {
    let sanitized: String = account
        .chars()
        .map(|c| if matches!(c, '@' | '\\' | '/') { '_' } else { c })
        .collect();
    lookxy_dir().join(sanitized).join("mail.db")
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
fn sanitize_filename(name: &str) -> String {
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
    fn store_path_is_under_local_appdata_per_account() {
        let p = store_path_for("me@epam.com");
        let s = p.to_string_lossy();
        assert!(s.contains("lookxy"));
        assert!(s.ends_with("mail.db"));
        assert!(s.contains("me_epam.com") || s.contains("me@epam.com"));
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
            .put_body("m1", &Body { content_type: "text".into(), content: "hello body".into() })
            .expect("seed body");

        app.open_message("m1");

        assert!(!app.body_loading);
        assert_eq!(app.body.as_ref().map(|b| b.content.as_str()), Some("hello body"));
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
}
