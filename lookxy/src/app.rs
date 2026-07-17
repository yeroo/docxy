//! `App` — the TUI's in-memory state: the local mail store, the background
//! sync handle, where the three panes (folders/list/reading) currently
//! point, and the triage actions (`m`/`u`/`f`/`d`/`v`) that mutate them.

use std::path::PathBuf;

use mailcore::graph::model::Body;
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

/// How many messages `reload_messages` pulls per folder. Paging further back
/// is a later task's concern; this is enough to fill a screen many times over.
const MESSAGE_PAGE_SIZE: i64 = 200;

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
    /// (move) is handled separately by `open_move_picker`, since it needs a
    /// folder choice before anything can be sent. Unrecognized characters
    /// are ignored. Called from `ui::handle_key` for every `KeyCode::Char`
    /// not already claimed by pane navigation.
    pub fn on_key_char(&mut self, c: char) {
        match c {
            'm' => self.mark_read(true),
            'u' => self.mark_read(false),
            'f' => self.toggle_flag(),
            'd' => self.delete_selected(),
            'v' => self.open_move_picker(),
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
}
