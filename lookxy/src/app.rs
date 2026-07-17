//! `App` — the TUI's in-memory state: the local mail store, the background
//! sync handle, and where the three panes (folders/list/reading) currently
//! point. The three-pane layout and its navigation land in a later task;
//! this is just the skeleton the run loop drives.

use std::path::PathBuf;

use mailcore::store::Store;
use mailcore::sync::engine::{SyncHandle, SyncState};

/// Which pane currently has keyboard focus. Only `Folders` is reachable
/// until Task 13 wires up focus-switching and the other two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Folders,
    #[allow(dead_code)]
    List,
    #[allow(dead_code)]
    Reading,
}

/// All in-memory TUI state: the local store, the sync channels, and the
/// current selection/focus. `sync` is the UI's half of the background sync
/// engine (`mailcore::sync::spawn`) — commands go down `cmd_tx`, events come
/// up `evt_rx`.
pub struct App {
    pub store: Store,
    pub sync: SyncHandle,
    pub focus: Pane,
    pub selected_folder: Option<String>,
    pub selected_msg: Option<String>,
    pub status: SyncState,
    pub quit: bool,
}

impl App {
    pub fn new(store: Store, sync: SyncHandle) -> App {
        App {
            store,
            sync,
            focus: Pane::Folders,
            selected_folder: None,
            selected_msg: None,
            status: SyncState::Idle,
            quit: false,
        }
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
}
