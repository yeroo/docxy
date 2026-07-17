//! `App` — the TUI's in-memory state: the local mail store, the background
//! sync handle, and where the three panes (folders/list/reading) currently
//! point. The three-pane layout and its navigation land in a later task;
//! this is just the skeleton the run loop drives.

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

        // Inert channels: nothing in a render/navigation test drives the sync
        // thread (there isn't one), so the peer ends (`_cmd_rx`/`_evt_tx`) are
        // simply dropped at the end of this function.
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_evt_tx, evt_rx) = mpsc::channel();
        let sync = SyncHandle { cmd_tx, evt_rx };

        App::new(store, sync)
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
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let (_evt_tx, evt_rx) = mpsc::channel();
        let sync = SyncHandle { cmd_tx, evt_rx };
        App::new(store, sync)
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
