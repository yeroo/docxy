//! Trusted documents (#882): the files a person took out of Protected View
//! with Enable Editing, so the same file opens for editing next time, as
//! Excel's trusted documents list does.
//!
//! A record names a file by its canonical path **and** the length and
//! modified time it had when it was opened. A file replaced at the same path
//! (downloaded again) no longer matches and opens protected again; Excel
//! trusts the path alone, which would let the new download skip Protected
//! View.
//!
//! A Save never needs to trust the file again. The suite writes through
//! `opccore::fsio::write_atomic`, which renames a temporary file over the
//! destination, so the saved file no longer carries the `Zone.Identifier`
//! mark at all: it is not protected whether or not its record still
//! matches. Do not add "re-trust after save".
//!
//! The store fails to *protected*: a missing, unreadable or malformed
//! `trusted.json` trusts nothing. Two instances that each read, add and
//! write can lose one of the two records; that file is then protected
//! again, which is the safe side, so there is no locking. A `remember` in
//! another instance racing a [`clear`] can likewise restore the cleared
//! records; that needs two instances and a window of one load-and-save,
//! and is accepted. No locking or retry code.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// At most this many records are kept; trusting one more drops the oldest.
pub(crate) const MAX_RECORDS: usize = 1000;

/// A file's length and modified time, taken when it was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stamp {
    pub(crate) len: u64,
    /// Nanoseconds since the Unix epoch.
    pub(crate) modified_ns: u64,
}

impl Stamp {
    /// `path`'s stamp now; `None` when it cannot be read, which trusts
    /// nothing.
    pub(crate) fn of(path: &Path) -> Option<Stamp> {
        let meta = std::fs::metadata(path).ok()?;
        let modified = meta.modified().ok()?;
        let ns = modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some(Stamp {
            len: meta.len(),
            modified_ns: u64::try_from(ns).ok()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    /// The canonical path, as [`crate::canonical`] spells it.
    path: String,
    #[serde(flatten)]
    stamp: Stamp,
}

/// The trusted documents, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TrustStore {
    records: Vec<Record>,
}

/// `<root>/docxy/trusted.json`, beside `session.json`.
pub(crate) fn store_path_in(root: &Path) -> PathBuf {
    root.join("docxy").join("trusted.json")
}

fn key(path: &Path) -> String {
    crate::canonical(path).display().to_string()
}

impl TrustStore {
    /// The store under `root`, or an empty one when there is none or it
    /// cannot be read.
    pub(crate) fn load(root: &Path) -> TrustStore {
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

    /// Whether `path` with `stamp` was trusted. A file whose stamp cannot be
    /// read is never trusted.
    pub(crate) fn is_trusted(&self, path: &Path, stamp: Option<Stamp>) -> bool {
        let Some(stamp) = stamp else {
            return false;
        };
        let key = key(path);
        self.records
            .iter()
            .any(|r| r.path == key && r.stamp == stamp)
    }

    /// Trust `path` as it was at `stamp`. A record for the same path is
    /// replaced and moves to the newest end; past [`MAX_RECORDS`] the oldest
    /// go.
    pub(crate) fn trust(&mut self, path: &Path, stamp: Stamp) {
        let key = key(path);
        self.records.retain(|r| r.path != key);
        self.records.push(Record { path: key, stamp });
        let over = self.records.len().saturating_sub(MAX_RECORDS);
        self.records.drain(..over);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }
}

/// Enable Editing's record of `path` as it was opened (`stamp`), added to the
/// store under `root`. The tab status that says how it went: a store that
/// could not be written leaves editing enabled and says why.
pub(crate) fn remember(root: &Path, path: &Path, stamp: Stamp) -> String {
    let mut store = TrustStore::load(root);
    store.trust(path, stamp);
    match store.save(root) {
        Ok(()) => "editing enabled".into(),
        Err(e) => format!("editing enabled (not remembered: {e})"),
    }
}

/// How many records the store under `root` holds; 0 when it is missing or
/// malformed (the fail-to-protected rule). The backstage's Settings row.
pub(crate) fn count(root: &Path) -> usize {
    TrustStore::load(root).records.len()
}

/// Clear every trusted document under `root` (#895), as Excel's Trust
/// Center > Trusted Documents > Clear does. Returns how many records the
/// store held. The empty store is written, not deleted: a malformed file is
/// replaced with a valid empty one, and an unwritable root reports the error
/// through the same `save` path [`remember`] uses, with nothing cleared.
pub(crate) fn clear(root: &Path) -> std::io::Result<usize> {
    let n = count(root);
    TrustStore::default().save(root)?;
    Ok(n)
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
            let dir = std::env::temp_dir().join(format!(
                "docxy-trusted-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn stamp(len: u64, modified_ns: u64) -> Stamp {
        Stamp { len, modified_ns }
    }

    #[test]
    fn a_record_matches_only_the_same_path_length_and_time() {
        let dir = Scratch::new("match");
        let a = dir.file("a.xlsx", b"one");
        let b = dir.file("b.xlsx", b"one");
        let mut store = TrustStore::default();
        store.trust(&a, stamp(3, 100));
        assert!(store.is_trusted(&a, Some(stamp(3, 100))));
        assert!(!store.is_trusted(&a, Some(stamp(4, 100))), "length differs");
        assert!(!store.is_trusted(&a, Some(stamp(3, 101))), "time differs");
        assert!(!store.is_trusted(&b, Some(stamp(3, 100))), "another file");
        assert!(!store.is_trusted(&a, None), "no stamp, no trust");
    }

    #[test]
    fn a_path_is_compared_canonically() {
        let dir = Scratch::new("canon");
        let a = dir.file("a.xlsx", b"one");
        let mut store = TrustStore::default();
        store.trust(&a, stamp(3, 1));
        let roundabout = dir.0.join(".").join("a.xlsx");
        assert!(store.is_trusted(&roundabout, Some(stamp(3, 1))));
    }

    #[test]
    fn a_file_replaced_at_the_same_path_is_not_trusted() {
        let dir = Scratch::new("replace");
        let a = dir.file("a.xlsx", b"first download");
        let mut store = TrustStore::default();
        store.trust(&a, Stamp::of(&a).unwrap());
        assert!(store.is_trusted(&a, Stamp::of(&a)));
        std::fs::write(&a, b"second, longer download").unwrap();
        assert!(!store.is_trusted(&a, Stamp::of(&a)));
    }

    #[test]
    fn trusting_again_replaces_the_record_and_makes_it_newest() {
        let dir = Scratch::new("again");
        let a = dir.file("a.xlsx", b"a");
        let b = dir.file("b.xlsx", b"b");
        let mut store = TrustStore::default();
        store.trust(&a, stamp(1, 1));
        store.trust(&b, stamp(1, 1));
        store.trust(&a, stamp(1, 2));
        assert_eq!(store.len(), 2);
        assert!(
            !store.is_trusted(&a, Some(stamp(1, 1))),
            "the old stamp is gone"
        );
        assert!(store.is_trusted(&a, Some(stamp(1, 2))));
        assert_eq!(store.records.last().unwrap().path, key(&a));
    }

    #[test]
    fn the_store_is_bounded_and_drops_the_oldest() {
        let mut store = TrustStore::default();
        for i in 0..MAX_RECORDS + 5 {
            store.trust(Path::new(&format!("no-such-dir/f{i}.xlsx")), stamp(1, 1));
        }
        assert_eq!(store.len(), MAX_RECORDS);
        assert!(!store.is_trusted(Path::new("no-such-dir/f0.xlsx"), Some(stamp(1, 1))));
        assert!(!store.is_trusted(Path::new("no-such-dir/f4.xlsx"), Some(stamp(1, 1))));
        assert!(store.is_trusted(Path::new("no-such-dir/f5.xlsx"), Some(stamp(1, 1))));
        let last = format!("no-such-dir/f{}.xlsx", MAX_RECORDS + 4);
        assert!(store.is_trusted(Path::new(&last), Some(stamp(1, 1))));
    }

    #[test]
    fn the_store_round_trips_under_its_root() {
        let dir = Scratch::new("round");
        let a = dir.file("a.xlsx", b"a");
        let root = dir.0.join("root");
        assert_eq!(TrustStore::load(&root), TrustStore::default(), "missing");
        assert_eq!(remember(&root, &a, stamp(1, 7)), "editing enabled");
        assert!(store_path_in(&root).is_file());
        assert!(TrustStore::load(&root).is_trusted(&a, Some(stamp(1, 7))));
    }

    #[test]
    fn a_malformed_store_trusts_nothing() {
        let dir = Scratch::new("malformed");
        let a = dir.file("a.xlsx", b"a");
        let root = dir.0.join("root");
        std::fs::create_dir_all(root.join("docxy")).unwrap();
        for junk in [&b"not json"[..], b"{\"records\":[{\"path\":3}]}", b""] {
            std::fs::write(store_path_in(&root), junk).unwrap();
            let store = TrustStore::load(&root);
            assert_eq!(store, TrustStore::default());
            assert!(!store.is_trusted(&a, Stamp::of(&a)));
        }
    }

    /// A store that cannot be written leaves editing enabled and says so,
    /// rather than failing Enable Editing.
    #[test]
    fn a_store_that_cannot_be_written_says_so() {
        let dir = Scratch::new("unwritable");
        let a = dir.file("a.xlsx", b"a");
        // The root is a file, so `<root>/docxy` cannot be made.
        let root = dir.file("root", b"in the way");
        let status = remember(&root, &a, stamp(1, 1));
        assert!(
            status.starts_with("editing enabled (not remembered: "),
            "{status}"
        );
    }

    /// Clearing empties the store and says how many records it held (#895):
    /// afterwards nothing is trusted, and the store file remains, valid and
    /// empty.
    #[test]
    fn clearing_forgets_every_trusted_file() {
        let dir = Scratch::new("clear");
        let a = dir.file("a.xlsx", b"a");
        let b = dir.file("b.xlsx", b"b");
        let root = dir.0.join("root");
        remember(&root, &a, stamp(1, 1));
        remember(&root, &b, stamp(1, 2));
        assert_eq!(clear(&root).ok(), Some(2));
        let store = TrustStore::load(&root);
        assert!(!store.is_trusted(&a, Some(stamp(1, 1))));
        assert!(!store.is_trusted(&b, Some(stamp(1, 2))));
        assert!(store_path_in(&root).is_file());
        assert_eq!(count(&root), 0);
    }

    /// Clearing a store that does not exist is not an error; it leaves a
    /// valid empty store behind (#895).
    #[test]
    fn clearing_a_missing_store_is_zero() {
        let dir = Scratch::new("clear-missing");
        let root = dir.0.join("root");
        assert_eq!(clear(&root).ok(), Some(0));
        assert!(store_path_in(&root).is_file());
    }

    /// Clearing replaces a malformed store with a valid empty one (#895),
    /// so the next read trusts nothing and parses.
    #[test]
    fn clearing_replaces_a_malformed_store() {
        let dir = Scratch::new("clear-malformed");
        let root = dir.0.join("root");
        std::fs::create_dir_all(root.join("docxy")).unwrap();
        std::fs::write(store_path_in(&root), b"not json").unwrap();
        assert_eq!(clear(&root).ok(), Some(0));
        let bytes = std::fs::read(store_path_in(&root)).unwrap();
        let store: TrustStore = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(store, TrustStore::default());
    }

    /// A store that cannot be written is not cleared (#895): the error comes
    /// through the same `save` path `remember` reports.
    #[test]
    fn a_store_that_cannot_be_cleared_says_so() {
        let dir = Scratch::new("clear-unwritable");
        // The root is a file, so `<root>/docxy` cannot be made.
        let root = dir.file("root", b"in the way");
        assert!(clear(&root).is_err());
    }

    /// The count behind the backstage's Settings row: 0 for a missing store,
    /// the record count after remembering (#895).
    #[test]
    fn count_reads_the_store() {
        let dir = Scratch::new("count");
        let a = dir.file("a.xlsx", b"a");
        let root = dir.0.join("root");
        assert_eq!(count(&root), 0, "missing");
        remember(&root, &a, stamp(1, 7));
        assert_eq!(count(&root), 1);
    }
}
