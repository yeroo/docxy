//! Recent files (#1194): the files a person opened or saved under a new
//! name, newest first, as Word's and Excel's File › Open › Recent list. The
//! Backstage's Open page shows them and the harness reads them
//! (`recent-files`).
//!
//! A record is a path, spelled as a person would type it: never `\\?\`
//! (Windows' canonical form), which [`plain`] strips. Two spellings of one
//! file are one record: they are compared by their canonical path when the
//! comparison is made, so a file that no longer exists still compares by the
//! spelling it was stored with.
//!
//! The list lives in its own `recent.json`, not in `session.json`: every
//! window rewrites the session wholesale (#587), and a list kept there would
//! be overwritten by whichever window persisted last. Here each record has
//! one writer, the window that opened or saved the file.
//!
//! The store fails to *empty*: a missing, unreadable or malformed file lists
//! nothing. Two instances that each read, add and write can lose one of the
//! two records; a recent list that misses one file is not worth locking for,
//! as with `trusted.json`. No locking or retry code.
//!
//! Only `impl Docxy` code records. The free save writers (`save_doc_tab`,
//! `finish_sheet_save`, `project::apply_save`) are called by unit tests with
//! no `DOCXY_CONFIG_DIR` set, and must not reach the real config directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// At most this many files are kept; recording one more drops the oldest.
pub(crate) const MAX_RECENT: usize = 25;

/// The recent files, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RecentStore {
    files: Vec<String>,
}

/// `<root>/docxy/recent.json`, beside `session.json` and `trusted.json`.
pub(crate) fn store_path_in(root: &Path) -> PathBuf {
    root.join("docxy").join("recent.json")
}

/// `path` without Windows' verbatim prefix: `\\?\C:\a` is `C:\a` and
/// `\\?\UNC\server\share` is `\\server\share`. Anything else is unchanged.
/// A string rule, so it is tested on every OS.
pub(crate) fn plain(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn key(path: &Path) -> PathBuf {
    crate::canonical(path)
}

impl RecentStore {
    /// The store under `root`, or an empty one when there is none or it
    /// cannot be read.
    pub(crate) fn load(root: &Path) -> RecentStore {
        std::fs::read(store_path_in(root))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Write the store under `root`.
    pub(crate) fn save(&self, root: &Path) -> std::io::Result<()> {
        let p = store_path_in(root);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        opccore::fsio::write_atomic(&p, json.as_bytes())
    }

    /// The files, newest first.
    pub(crate) fn files(&self) -> Vec<PathBuf> {
        self.files.iter().map(PathBuf::from).collect()
    }

    /// Make `path` the newest file. A record of the same file, however it
    /// was spelled, is dropped; past [`MAX_RECENT`] the oldest go.
    pub(crate) fn push(&mut self, path: &Path) {
        self.remove(path);
        self.files
            .insert(0, plain(path).to_string_lossy().into_owned());
        self.files.truncate(MAX_RECENT);
    }

    /// Drop `path`'s record, however it was spelled; whether there was one.
    pub(crate) fn remove(&mut self, path: &Path) -> bool {
        let key = key(path);
        let before = self.files.len();
        self.files.retain(|f| crate::canonical(Path::new(f)) != key);
        self.files.len() != before
    }
}

/// Record `path` as the newest file in the store under `root`.
pub(crate) fn note(root: &Path, path: &Path) -> std::io::Result<()> {
    let mut store = RecentStore::load(root);
    store.push(path);
    store.save(root)
}

/// Drop `path` from the store under `root`, writing only when it was there.
pub(crate) fn forget(root: &Path, path: &Path) -> std::io::Result<()> {
    let mut store = RecentStore::load(root);
    if store.remove(path) {
        store.save(root)?;
    }
    Ok(())
}

/// The files the store under `root` holds, newest first.
pub(crate) fn list(root: &Path) -> Vec<PathBuf> {
    RecentStore::load(root).files()
}

/// Whether a save moved a tab to another file, so the new one is recorded:
/// Save As and a first save do, a Save in place does not. Paths are
/// compared canonically.
pub(crate) fn path_changed(prev: Option<&Path>, new: Option<&Path>) -> bool {
    match (prev, new) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(a), Some(b)) => key(a) != key(b),
    }
}

/// The status a click on a file that no longer exists leaves.
pub(crate) fn missing_status(path: &Path) -> String {
    format!(
        "\"{}\" could not be found, so it was removed from the recent list",
        crate::file_name(path)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("docxy-recent-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn file(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, b"x").unwrap();
            p
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_added_again_moves_to_the_top_once() {
        let dir = Scratch::new("top");
        let a = dir.file("a.docx");
        let b = dir.file("b.docx");
        let mut store = RecentStore::default();
        store.push(&a);
        store.push(&b);
        assert_eq!(store.files(), vec![b.clone(), a.clone()]);
        store.push(&a);
        assert_eq!(store.files(), vec![a, b]);
    }

    #[test]
    fn the_list_is_bounded_and_drops_the_oldest() {
        let dir = Scratch::new("bound");
        let mut store = RecentStore::default();
        let files: Vec<PathBuf> = (0..MAX_RECENT + 3)
            .map(|i| dir.file(&format!("f{i}.docx")))
            .collect();
        for f in &files {
            store.push(f);
        }
        let kept = store.files();
        assert_eq!(kept.len(), MAX_RECENT);
        assert_eq!(kept[0], files[MAX_RECENT + 2]);
        assert_eq!(kept[MAX_RECENT - 1], files[3]);
    }

    #[test]
    fn two_spellings_of_one_file_are_one_record() {
        let dir = Scratch::new("spell");
        let a = dir.file("a.docx");
        let other = dir.0.join("sub").join("..").join("a.docx");
        std::fs::create_dir_all(dir.0.join("sub")).unwrap();
        let mut store = RecentStore::default();
        store.push(&a);
        store.push(&other);
        assert_eq!(store.files().len(), 1);
        assert!(store.remove(&a));
        assert!(store.files().is_empty());
    }

    #[test]
    fn a_missing_file_is_removed_by_the_spelling_it_was_stored_with() {
        let dir = Scratch::new("missing");
        let a = dir.file("a.docx");
        let mut store = RecentStore::default();
        store.push(&a);
        let stored = store.files()[0].clone();
        std::fs::remove_file(&a).unwrap();
        assert!(store.remove(&stored));
        assert!(store.files().is_empty());
    }

    #[test]
    fn the_verbatim_prefix_is_stripped() {
        assert_eq!(
            plain(Path::new(r"\\?\C:\Users\me\a.docx")),
            PathBuf::from(r"C:\Users\me\a.docx")
        );
        assert_eq!(
            plain(Path::new(r"\\?\UNC\server\share\a.docx")),
            PathBuf::from(r"\\server\share\a.docx")
        );
        assert_eq!(
            plain(Path::new("/home/a.docx")),
            PathBuf::from("/home/a.docx")
        );
        let mut store = RecentStore::default();
        store.push(Path::new(r"\\?\Q:\nowhere\a.docx"));
        assert_eq!(store.files, vec![r"Q:\nowhere\a.docx".to_string()]);
    }

    #[test]
    fn the_store_round_trips_under_its_root() {
        let dir = Scratch::new("round");
        let a = dir.file("a.docx");
        let b = dir.file("b.xlsx");
        note(&dir.0, &a).unwrap();
        note(&dir.0, &b).unwrap();
        assert_eq!(list(&dir.0), vec![b.clone(), a.clone()]);
        forget(&dir.0, &b).unwrap();
        assert_eq!(list(&dir.0), vec![a]);
    }

    #[test]
    fn a_missing_or_malformed_store_lists_nothing() {
        let dir = Scratch::new("malformed");
        assert!(list(&dir.0).is_empty());
        std::fs::create_dir_all(dir.0.join("docxy")).unwrap();
        std::fs::write(store_path_in(&dir.0), b"{ not json").unwrap();
        assert!(list(&dir.0).is_empty());
        // Recording over a malformed store replaces it with a valid one.
        let a = dir.file("a.docx");
        note(&dir.0, &a).unwrap();
        assert_eq!(list(&dir.0), vec![a]);
    }

    #[test]
    fn a_store_that_cannot_be_written_says_so() {
        let dir = Scratch::new("unwritable");
        // A file where the `docxy` directory should be.
        std::fs::write(dir.0.join("docxy"), b"x").unwrap();
        let a = dir.file("a.docx");
        assert!(note(&dir.0, &a).is_err());
        assert!(list(&dir.0).is_empty());
    }

    #[test]
    fn only_a_save_to_another_file_is_recorded() {
        let dir = Scratch::new("changed");
        let a = dir.file("a.docx");
        let b = dir.file("b.docx");
        let a_again = dir.0.join("sub").join("..").join("a.docx");
        std::fs::create_dir_all(dir.0.join("sub")).unwrap();
        assert!(path_changed(None, Some(&a)), "a first save");
        assert!(path_changed(Some(&a), Some(&b)), "Save As");
        assert!(!path_changed(Some(&a), Some(&a)), "Save in place");
        assert!(!path_changed(Some(&a), Some(&a_again)), "Save in place");
        assert!(!path_changed(Some(&a), None), "a failed first save");
        assert!(!path_changed(None, None));
    }

    #[test]
    fn an_open_records_only_a_tab_bound_to_a_file_that_loaded() {
        let mut tab = crate::blank_docx_tab("Document1");
        assert_eq!(
            crate::records_open(&tab),
            None,
            "untitled, as a template opens"
        );
        let path = PathBuf::from("dir").join("a.docx");
        tab.path = Some(path.clone());
        assert_eq!(crate::records_open(&tab), Some(path));
        tab.load_failed = true;
        assert_eq!(crate::records_open(&tab), None, "a failed load");
    }

    #[test]
    fn the_missing_status_names_the_file() {
        assert_eq!(
            missing_status(Path::new("dir").join("Budget.xlsx").as_path()),
            "\"Budget.xlsx\" could not be found, so it was removed from the recent list"
        );
    }
}
