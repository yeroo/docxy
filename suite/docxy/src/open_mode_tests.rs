//! Opening a workbook read-only, as a copy, repaired or in Protected View
//! (#610), without a window: the open, the save gate, the reopen question,
//! the backstop and the session.

use crate::dialog_host::dialog_click;
use crate::open_mode::{Access, OpenMode, PROTECTED_STATUS, read_only_refusal};
// `Stamp` is used only by the Windows-only Protected View tests.
#[cfg(windows)]
use crate::trusted::Stamp;
use crate::trusted::TrustStore;
use crate::{
    DocTab, PersistTab, SHEET_READ_ONLY_HARNESS, Surface, finish_sheet_save, persist_tab,
    protected_rollback, reopen_dialog, restore_tab, save_sheet_tab, save_sheet_to,
    tab_from_path_mode,
};
use gridcore::sheet::{Cell, CellValue};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct Scratch(PathBuf);
impl Scratch {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/open-mode-suite-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A workbook on disk whose A1 is 1.
fn book(dir: &Scratch, name: &str) -> PathBuf {
    let mut pkg = gridcore::xlsx::new_xlsx();
    pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(1.0));
    let path = dir.path(name);
    std::fs::write(&path, gridcore::xlsx::save_xlsx(&pkg)).unwrap();
    path
}

/// [`book`] with its styles part's ZIP entry made unreadable, so a strict
/// load fails and Open and Repair empties it.
pub(crate) fn damaged_book(dir: &Scratch, name: &str) -> PathBuf {
    let path = book(dir, name);
    let mut bytes = std::fs::read(&path).unwrap();
    let offset = opccore::zip::ZipArchive::open(&bytes)
        .unwrap()
        .find("xl/styles.xml")
        .unwrap()
        .local_offset as usize;
    bytes[offset..offset + 4].copy_from_slice(&[0; 4]);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Type `n` into A1, as an edit that dirties the tab would.
fn edit(tab: &mut DocTab, n: f64) {
    let Surface::Sheet(v) = &mut tab.surface else {
        panic!("not a workbook: {}", tab.status);
    };
    v.push_undo();
    v.pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(n));
    tab.dirty = true;
}

fn a1(tab: &DocTab) -> CellValue {
    let Surface::Sheet(v) = &tab.surface else {
        panic!("not a workbook: {}", tab.status);
    };
    v.pkg.workbook.sheets[0].cells[&(0, 0)].value.clone()
}

fn a1_on_disk(path: &Path) -> CellValue {
    let pkg = gridcore::xlsx::load_xlsx(&std::fs::read(path).unwrap()).unwrap();
    pkg.workbook.sheets[0].cells[&(0, 0)].value.clone()
}

/// Save with a Save As dialog that answers `pick`, recording what it was
/// offered; never asks about macros.
fn save(tab: &mut DocTab, harness: bool, pick: Option<PathBuf>) -> (bool, Option<String>) {
    let mut offered = None;
    let saved = save_sheet_tab(
        tab,
        harness,
        false,
        |suggested| {
            offered = Some(suggested);
            pick
        },
        |_| true,
    );
    (saved, offered)
}

// ---- read-only ----------------------------------------------------------

#[test]
fn read_only_save_goes_to_save_as() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    let before = std::fs::read(&src).unwrap();
    let mut tab = tab_from_path_mode(&src, OpenMode::ReadOnly, &TrustStore::default()).unwrap();
    assert!(tab.access.read_only);
    assert_eq!(tab.caption(), "book.xlsx [Read-Only]");
    assert_eq!(
        tab.title.as_ref(),
        "book.xlsx",
        "the title still seeds Save As"
    );
    edit(&mut tab, 2.0);
    // Ctrl+S, the backstage's Save and the close dialog's Save all come here.
    let (_, offered) = save(&mut tab, false, None);
    assert_eq!(offered.as_deref(), Some("book.xlsx"), "Save opened Save As");
    assert_eq!(tab.status.as_ref(), "save cancelled");
    assert_eq!(std::fs::read(&src).unwrap(), before);
    assert!(tab.dirty && tab.access.read_only);
}

#[test]
fn read_only_save_as_over_source_is_refused_and_source_unchanged() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    let before = std::fs::read(&src).unwrap();
    let mut tab = tab_from_path_mode(&src, OpenMode::ReadOnly, &TrustStore::default()).unwrap();
    edit(&mut tab, 2.0);
    let refusal = read_only_refusal("book.xlsx");

    // The Save As dialog's answer, through Save.
    let (saved, _) = save(&mut tab, false, Some(src.clone()));
    assert!(saved, "the dialog ran");
    assert_eq!(tab.status.as_ref(), refusal);
    assert_eq!(std::fs::read(&src).unwrap(), before);

    // The harness's save-as, and the same file spelled another way.
    assert!(!save_sheet_to(&mut tab, &src));
    assert_eq!(tab.status.as_ref(), refusal);
    let dotted = dir.0.join(".").join("book.xlsx");
    assert!(!save_sheet_to(&mut tab, &dotted));
    assert_eq!(tab.status.as_ref(), refusal);
    // A picked name without an extension resolves to the same file.
    assert!(!save_sheet_to(&mut tab, &dir.path("book")));
    assert_eq!(std::fs::read(&src).unwrap(), before);
    assert_eq!(tab.path.as_deref(), Some(src.as_path()));
    assert!(tab.dirty && tab.access.read_only);
}

#[test]
fn read_only_save_as_elsewhere_clears_read_only() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    let before = std::fs::read(&src).unwrap();
    let mut tab = tab_from_path_mode(&src, OpenMode::ReadOnly, &TrustStore::default()).unwrap();
    edit(&mut tab, 2.0);
    let other = dir.path("mine.xlsx");
    assert!(save_sheet_to(&mut tab, &other), "{}", tab.status);
    assert_eq!(a1_on_disk(&other), CellValue::Number(2.0));
    assert_eq!(std::fs::read(&src).unwrap(), before);
    assert_eq!(tab.path.as_deref(), Some(other.as_path()));
    assert_eq!(tab.title.as_ref(), "mine.xlsx");
    assert!(!tab.dirty);
    assert_eq!(tab.access, Access::default());
    // Now an ordinary tab: Save writes in place.
    edit(&mut tab, 3.0);
    let (saved, offered) = save(&mut tab, false, None);
    assert!(saved && offered.is_none());
    assert_eq!(a1_on_disk(&other), CellValue::Number(3.0));
}

#[test]
fn read_only_save_refused_in_harness() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    let before = std::fs::read(&src).unwrap();
    let mut tab = tab_from_path_mode(&src, OpenMode::ReadOnly, &TrustStore::default()).unwrap();
    edit(&mut tab, 2.0);
    let (saved, offered) = save(&mut tab, true, Some(src.clone()));
    assert!(!saved);
    assert!(offered.is_none(), "a harness never opens the dialog");
    assert_eq!(tab.status.as_ref(), SHEET_READ_ONLY_HARNESS);
    assert_eq!(std::fs::read(&src).unwrap(), before);
}

// ---- repair -------------------------------------------------------------

#[test]
fn repaired_save_goes_to_save_as_and_may_pick_source() {
    let dir = Scratch::new();
    let src = damaged_book(&dir, "book.xlsx");
    let mut tab = tab_from_path_mode(&src, OpenMode::Repair, &TrustStore::default()).unwrap();
    assert!(tab.access.repaired && !tab.access.read_only);
    assert_eq!(tab.caption(), "book.xlsx [Repaired]");
    assert!(
        tab.status.contains("emptied xl/styles.xml"),
        "the status names the part: {}",
        tab.status
    );
    edit(&mut tab, 2.0);
    let (saved, offered) = save(&mut tab, false, Some(src.clone()));
    assert!(saved, "{}", tab.status);
    assert_eq!(offered.as_deref(), Some("book.xlsx"), "Save opened Save As");
    // Over its own source: the user chose to.
    assert_eq!(a1_on_disk(&src), CellValue::Number(2.0));
    assert_eq!(tab.access, Access::default());
    assert!(!tab.dirty);
}

#[test]
fn a_sound_workbook_opens_repaired_with_nothing_to_repair() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    let tab = tab_from_path_mode(&src, OpenMode::Repair, &TrustStore::default()).unwrap();
    assert!(tab.access.repaired);
    assert!(tab.status.starts_with("loaded"), "{}", tab.status);
    assert!(
        tab.status.contains("nothing needed repairing"),
        "{}",
        tab.status
    );
    assert_eq!(a1(&tab), CellValue::Number(1.0));
}

#[test]
fn a_damaged_workbook_opened_normally_still_fails() {
    let dir = Scratch::new();
    let src = damaged_book(&dir, "book.xlsx");
    let tab = tab_from_path_mode(&src, OpenMode::Normal, &TrustStore::default()).unwrap();
    assert!(matches!(tab.surface, Surface::Placeholder));
    assert!(!tab.status.starts_with("loaded"), "{}", tab.status);
}

// ---- Protected View -----------------------------------------------------

fn protected_tab(dir: &Scratch) -> (PathBuf, DocTab) {
    let src = book(dir, "book.xlsx");
    let mut tab = tab_from_path_mode(&src, OpenMode::Normal, &TrustStore::default()).unwrap();
    tab.access.protected = true;
    (src, tab)
}

#[test]
fn protected_save_and_save_as_write_nothing() {
    let dir = Scratch::new();
    let (src, mut tab) = protected_tab(&dir);
    let before = std::fs::read(&src).unwrap();
    assert_eq!(tab.caption(), "book.xlsx [Protected View]");
    tab.dirty = true;
    for harness in [false, true] {
        let (saved, offered) = save(&mut tab, harness, Some(dir.path("other.xlsx")));
        assert!(!saved && offered.is_none());
        assert_eq!(tab.status.as_ref(), PROTECTED_STATUS);
        assert!(!save_sheet_tab(&mut tab, harness, true, |_| None, |_| true));
        assert_eq!(tab.status.as_ref(), PROTECTED_STATUS);
    }
    assert!(!save_sheet_to(&mut tab, &dir.path("other.xlsx")));
    assert!(!save_sheet_to(&mut tab, &src));
    // The last word, too, for a path that got past the gates.
    assert!(!finish_sheet_save(&mut tab, Some(&dir.path("other.xlsx"))));
    assert_eq!(tab.status.as_ref(), PROTECTED_STATUS);
    assert_eq!(std::fs::read(&src).unwrap(), before);
    assert!(!dir.path("other.xlsx").exists());
}

#[test]
fn a_leaked_edit_on_a_protected_tab_is_rolled_back() {
    let dir = Scratch::new();
    let (_, mut tab) = protected_tab(&dir);
    // Two edits got past every gate; each took its snapshot first.
    edit(&mut tab, 2.0);
    edit(&mut tab, 3.0);
    protected_rollback(&mut tab);
    assert_eq!(
        a1(&tab),
        CellValue::Number(1.0),
        "back to the file as opened"
    );
    let Surface::Sheet(v) = &tab.surface else {
        unreachable!()
    };
    assert!(v.undo.is_empty() && v.redo.is_empty());
    assert!(!tab.dirty);
    assert_eq!(tab.status.as_ref(), PROTECTED_STATUS);
}

/// #610 r2: a rename (or an AutoFilter) takes no undo snapshot, so a leak of
/// one left nothing to restore. The backstop now loads the workbook again
/// from the tab's file, and the hot-exit sidecar written after it holds the
/// file's sheet name, not the leaked one.
#[test]
fn a_leak_without_a_snapshot_is_undone_from_the_file() {
    let dir = Scratch::new();
    let (_, mut tab) = protected_tab(&dir);
    let Surface::Sheet(v) = &mut tab.surface else {
        unreachable!()
    };
    assert!(v.pkg.rename_sheet(0, "Leaked"));
    assert!(v.undo.is_empty(), "the rename took no snapshot");
    protected_rollback(&mut tab);
    let Surface::Sheet(v) = &tab.surface else {
        unreachable!()
    };
    assert_eq!(v.pkg.workbook.sheets[0].name, "Sheet1");
    assert!(!tab.dirty);
    assert_eq!(tab.status.as_ref(), PROTECTED_STATUS);
    assert!(tab.access.protected, "still protected");

    let hd = dir.path("hot");
    std::fs::create_dir_all(&hd).unwrap();
    let persisted = persist_tab(&hd, 0, 0, &tab);
    let hot = std::fs::read(persisted.hot.as_deref().expect("a sidecar")).unwrap();
    let pkg = gridcore::xlsx::load_xlsx(&hot).unwrap();
    assert_eq!(pkg.workbook.sheets[0].name, "Sheet1");
}

/// #610 r3: a repaired tab is loaded again the way it was opened, through
/// repair; a strict load of its damaged file would give a placeholder.
#[test]
fn a_leak_on_a_repaired_protected_tab_reloads_through_repair() {
    let dir = Scratch::new();
    let src = damaged_book(&dir, "book.xlsx");
    let mut tab = tab_from_path_mode(&src, OpenMode::Repair, &TrustStore::default()).unwrap();
    tab.access.protected = true;
    let Surface::Sheet(v) = &mut tab.surface else {
        panic!("{}", tab.status)
    };
    assert!(v.pkg.rename_sheet(0, "Leaked"));
    protected_rollback(&mut tab);
    let Surface::Sheet(v) = &tab.surface else {
        panic!("the reload fell back to a placeholder: {}", tab.status)
    };
    assert_eq!(v.pkg.workbook.sheets[0].name, "Sheet1");
    assert!(!tab.dirty);
    assert!(tab.access.repaired && tab.access.protected);
}

/// With no file to read (a template opened from a download is untitled),
/// the backstop falls back to the oldest snapshot.
#[test]
fn a_protected_tab_without_a_file_falls_back_to_its_oldest_snapshot() {
    let dir = Scratch::new();
    let (_, mut tab) = protected_tab(&dir);
    tab.path = None;
    edit(&mut tab, 2.0);
    edit(&mut tab, 3.0);
    protected_rollback(&mut tab);
    assert_eq!(a1(&tab), CellValue::Number(1.0));
    assert!(!tab.dirty);
}

/// Mark `path` as downloaded from the Internet, or `None` when this volume
/// keeps no alternate data streams.
#[cfg(windows)]
pub(crate) fn mark_downloaded(path: &Path) -> Option<()> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    match std::fs::write(PathBuf::from(stream), "[ZoneTransfer]\r\nZoneId=3\r\n") {
        Ok(()) => Some(()),
        Err(e) => {
            eprintln!("SKIP: no alternate data streams on this volume ({e})");
            None
        }
    }
}

#[cfg(windows)]
#[test]
fn downloaded_workbook_opens_protected() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    if mark_downloaded(&src).is_none() {
        return;
    }
    for mode in [OpenMode::Normal, OpenMode::ReadOnly, OpenMode::Repair] {
        let tab = tab_from_path_mode(&src, mode, &TrustStore::default()).unwrap();
        assert!(tab.access.protected, "{mode:?}");
        assert_eq!(tab.caption(), "book.xlsx [Protected View]");
        assert_eq!(tab.access.read_only, mode == OpenMode::ReadOnly);
        assert_eq!(tab.access.repaired, mode == OpenMode::Repair);
    }
    // The same file without the stream opens editable.
    let local = book(&dir, "local.xlsx");
    assert!(
        !tab_from_path_mode(&local, OpenMode::Normal, &TrustStore::default())
            .unwrap()
            .access
            .protected
    );
}

#[cfg(windows)]
#[test]
fn copy_of_downloaded_workbook_is_protected() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    if mark_downloaded(&src).is_none() {
        return;
    }
    let tab = tab_from_path_mode(&src, OpenMode::Copy, &TrustStore::default()).unwrap();
    let copy = dir.path("Copy (1)book.xlsx");
    assert_eq!(tab.path.as_deref(), Some(copy.as_path()));
    assert!(
        tab.access.protected,
        "a copy is no way around Protected View"
    );
    // The copy is still downloaded: it carries the source's stream (#610
    // r5), so closing its tab and opening it again, in any mode, is
    // protected too, as it is for any tool that reads the mark.
    assert_eq!(
        crate::open_mode::zone_id(&copy),
        crate::open_mode::zone_id(&src)
    );
    drop(tab);
    for mode in [OpenMode::Normal, OpenMode::ReadOnly, OpenMode::Repair] {
        let again = tab_from_path_mode(&copy, mode, &TrustStore::default()).unwrap();
        assert!(
            again.access.protected,
            "{mode:?}: the reopened copy is protected"
        );
    }
}

// ---- trusted documents (#882) --------------------------------------------

/// A protected tab keeps its file's stamp from the open; that is what Enable
/// Editing trusts. An unprotected tab has none.
#[cfg(windows)]
#[test]
fn a_protected_tab_keeps_the_stamp_it_opened_with() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    if mark_downloaded(&src).is_none() {
        return;
    }
    let tab = tab_from_path_mode(&src, OpenMode::Normal, &TrustStore::default()).unwrap();
    assert!(tab.access.protected);
    assert_eq!(tab.access.stamp, Stamp::of(&src));
    assert!(tab.access.stamp.is_some());
    let local = book(&dir, "local.xlsx");
    let plain = tab_from_path_mode(&local, OpenMode::Normal, &TrustStore::default()).unwrap();
    assert_eq!(plain.access.stamp, None);
}

/// A downloaded file trusted as it is now opens editable in every mode. A
/// copy of it opens editable too, but the copy is not trusted itself: opened
/// again from disk it is protected.
#[cfg(windows)]
#[test]
fn a_trusted_download_opens_without_protected_view() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    if mark_downloaded(&src).is_none() {
        return;
    }
    let mut trusted = TrustStore::default();
    trusted.trust(&src, Stamp::of(&src).unwrap());
    for mode in [OpenMode::Normal, OpenMode::ReadOnly, OpenMode::Repair] {
        let tab = tab_from_path_mode(&src, mode, &trusted).unwrap();
        assert!(!tab.access.protected, "{mode:?}");
        assert_eq!(tab.access.stamp, None, "{mode:?}");
        assert_eq!(tab.access.read_only, mode == OpenMode::ReadOnly);
        assert_eq!(tab.access.repaired, mode == OpenMode::Repair);
    }
    let copy_tab = tab_from_path_mode(&src, OpenMode::Copy, &trusted).unwrap();
    assert!(!copy_tab.access.protected, "a copy of a trusted file");
    let copy = dir.path("Copy (1)book.xlsx");
    let again = tab_from_path_mode(&copy, OpenMode::Normal, &trusted).unwrap();
    assert!(again.access.protected, "the copy itself is not trusted");
}

/// A file replaced at a trusted path (downloaded again) is protected again.
#[cfg(windows)]
#[test]
fn a_download_replaced_at_a_trusted_path_is_protected() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    if mark_downloaded(&src).is_none() {
        return;
    }
    let mut trusted = TrustStore::default();
    trusted.trust(&src, Stamp::of(&src).unwrap());
    let mut bytes = std::fs::read(&src).unwrap();
    bytes.push(0);
    std::fs::write(&src, &bytes).unwrap();
    mark_downloaded(&src).unwrap();
    let tab = tab_from_path_mode(&src, OpenMode::Normal, &trusted).unwrap();
    assert!(tab.access.protected);
}

/// Restore takes protection away from a file trusted since the session was
/// written, keeps it for one that is not, and never adds it.
#[cfg(windows)]
#[test]
fn restore_drops_protection_only_for_a_trusted_file() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    if mark_downloaded(&src).is_none() {
        return;
    }
    let opened = Stamp::of(&src);
    let persisted = |protected: bool, stamp: Option<Stamp>| -> PersistTab {
        let json = format!(
            r#"{{"kind":"Xlsx","title":"book.xlsx","path":{},"protected":{protected},"stamp":{}}}"#,
            serde_json::to_string(&src.display().to_string()).unwrap(),
            serde_json::to_string(&stamp).unwrap()
        );
        serde_json::from_str(&json).unwrap()
    };
    let none = TrustStore::default();
    let mut trusted = TrustStore::default();
    trusted.trust(&src, opened.unwrap());
    let restore = |t: &PersistTab, s: &TrustStore| crate::restore_tab_sourced(t, s).0;
    assert!(restore(&persisted(true, opened), &none).access.protected);
    assert!(!restore(&persisted(true, opened), &trusted).access.protected);
    // No persisted stamp: what the tab shows cannot be matched, so it stays.
    assert!(restore(&persisted(true, None), &trusted).access.protected);
    // Enable Editing before the restart is kept, and the zone is not re-read.
    assert!(!restore(&persisted(false, opened), &none).access.protected);
}

/// r4 m5: the tab shows the content it opened with; a file rewritten since
/// (and trusted as it is now) does not unprotect it.
#[cfg(windows)]
#[test]
fn restore_keeps_protection_when_the_file_changed_since_it_opened() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    if mark_downloaded(&src).is_none() {
        return;
    }
    let opened = Stamp::of(&src).unwrap();
    let json = format!(
        r#"{{"kind":"Xlsx","title":"book.xlsx","path":{},"protected":true,"stamp":{}}}"#,
        serde_json::to_string(&src.display().to_string()).unwrap(),
        serde_json::to_string(&opened).unwrap()
    );
    let t: PersistTab = serde_json::from_str(&json).unwrap();
    let mut bytes = std::fs::read(&src).unwrap();
    bytes.push(0);
    std::fs::write(&src, &bytes).unwrap();
    let now = Stamp::of(&src).unwrap();
    assert_ne!(now, opened);
    let mut trusted = TrustStore::default();
    trusted.trust(&src, now);
    let tab = crate::restore_tab_sourced(&t, &trusted).0;
    assert!(tab.access.protected);
    assert_eq!(tab.access.stamp, Some(opened));
}

/// A protected tab's stamp survives the session, so Enable Editing after a
/// restart still trusts the file as it was opened.
#[cfg(windows)]
#[test]
fn a_protected_tab_keeps_its_stamp_across_the_session() {
    let dir = Scratch::new();
    let hd = dir.path("hot");
    std::fs::create_dir_all(&hd).unwrap();
    let src = book(&dir, "book.xlsx");
    if mark_downloaded(&src).is_none() {
        return;
    }
    let tab = tab_from_path_mode(&src, OpenMode::Normal, &TrustStore::default()).unwrap();
    let back = round_trip(&tab, &hd);
    assert!(back.access.protected);
    assert_eq!(back.access.stamp, tab.access.stamp);
}

// ---- copy ---------------------------------------------------------------

#[test]
fn open_as_copy_opens_an_editable_copy_and_leaves_source() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    let before = std::fs::read(&src).unwrap();
    let mut tab = tab_from_path_mode(&src, OpenMode::Copy, &TrustStore::default()).unwrap();
    let copy = dir.path("Copy (1)book.xlsx");
    assert_eq!(tab.path.as_deref(), Some(copy.as_path()));
    assert_eq!(tab.title.as_ref(), "Copy (1)book.xlsx");
    assert_eq!(tab.access, Access::default());
    edit(&mut tab, 2.0);
    let (saved, offered) = save(&mut tab, false, None);
    assert!(saved && offered.is_none(), "an ordinary Save in place");
    assert_eq!(a1_on_disk(&copy), CellValue::Number(2.0));
    assert_eq!(std::fs::read(&src).unwrap(), before);
    // A second copy takes the next name.
    let again = tab_from_path_mode(&src, OpenMode::Copy, &TrustStore::default()).unwrap();
    assert_eq!(again.title.as_ref(), "Copy (2)book.xlsx");
}

#[test]
fn a_copy_that_cannot_be_written_opens_nothing() {
    let dir = Scratch::new();
    let err = tab_from_path_mode(
        &dir.path("gone.xlsx"),
        OpenMode::Copy,
        &TrustStore::default(),
    )
    .err()
    .expect("no tab");
    assert!(err.contains("could not copy \"gone.xlsx\""), "{err}");
}

// ---- templates ----------------------------------------------------------

#[test]
fn template_read_only_or_repair_opens_as_normal_untitled() {
    let dir = Scratch::new();
    let template = dir.path("Budget.xltx");
    std::fs::write(
        &template,
        gridcore::xlsx::save_xlsx_as(
            &gridcore::xlsx::new_xlsx(),
            gridcore::xlsx::SpreadsheetKind::Template,
        ),
    )
    .unwrap();
    let plain = tab_from_path_mode(&template, OpenMode::Normal, &TrustStore::default()).unwrap();
    for mode in [OpenMode::ReadOnly, OpenMode::Repair, OpenMode::Copy] {
        let tab = tab_from_path_mode(&template, mode, &TrustStore::default()).unwrap();
        assert_eq!(tab.path, None, "{mode:?}: a new workbook from the template");
        assert_eq!(tab.access, Access::default(), "{mode:?}");
        assert!(matches!(tab.surface, Surface::Sheet(_)), "{}", tab.status);
        assert_eq!(tab.title, plain.title, "{mode:?}");
    }
    // #610 r6: Open as Copy of a template writes no copy beside it.
    let files: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, ["Budget.xltx"], "no Copy (k) file was left behind");
}

// ---- the reopen question ------------------------------------------------

#[test]
fn reopen_no_keeps_the_edit_and_yes_reloads_in_the_mode() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    let mut tab = tab_from_path_mode(&src, OpenMode::Normal, &TrustStore::default()).unwrap();
    edit(&mut tab, 2.0);

    tab.dialogs.push(reopen_dialog(&src, OpenMode::ReadOnly));
    let question = tab.dialogs.top().unwrap().text.clone().unwrap();
    assert_eq!(
        question,
        "\"book.xlsx\" is already open. Reopening will cause any changes you made to be discarded. Do you want to reopen \"book.xlsx\"?"
    );
    dialog_click(&mut tab, "No").unwrap();
    assert!(!tab.dialogs.is_open());
    assert!(tab.dirty);
    assert_eq!(a1(&tab), CellValue::Number(2.0), "No keeps the edit");

    tab.dialogs.push(reopen_dialog(&src, OpenMode::ReadOnly));
    dialog_click(&mut tab, "Yes").unwrap();
    assert!(!tab.dialogs.is_open());
    assert!(!tab.dirty);
    assert_eq!(a1(&tab), CellValue::Number(1.0), "Yes discards it");
    assert!(
        tab.access.read_only,
        "reloaded in the mode that was asked for"
    );
}

// ---- the session --------------------------------------------------------

fn round_trip(tab: &DocTab, hd: &Path) -> DocTab {
    let persisted = persist_tab(hd, 0, 0, tab);
    let json = serde_json::to_string(&persisted).unwrap();
    let back: PersistTab = serde_json::from_str(&json).unwrap();
    restore_tab(&back)
}

#[test]
fn persist_tab_round_trips_access() {
    let dir = Scratch::new();
    let hd = dir.path("hot");
    std::fs::create_dir_all(&hd).unwrap();
    let src = book(&dir, "book.xlsx");
    for access in [
        Access {
            read_only: true,
            ..Access::default()
        },
        Access {
            protected: true,
            ..Access::default()
        },
        Access {
            repaired: true,
            protected: true,
            ..Access::default()
        },
    ] {
        let mut tab = tab_from_path_mode(&src, OpenMode::Normal, &TrustStore::default()).unwrap();
        tab.access = access;
        let back = round_trip(&tab, &hd);
        assert_eq!(back.access, access);
        assert_eq!(back.path.as_deref(), Some(src.as_path()));
    }
}

#[test]
fn a_repaired_tab_without_its_sidecar_reopens_through_repair() {
    let dir = Scratch::new();
    let src = damaged_book(&dir, "book.xlsx");
    let json = format!(
        r#"{{"kind":"Xlsx","title":"book.xlsx","path":{},"repaired":true}}"#,
        serde_json::to_string(&src.display().to_string()).unwrap()
    );
    let t: PersistTab = serde_json::from_str(&json).unwrap();
    let tab = restore_tab(&t);
    assert!(matches!(tab.surface, Surface::Sheet(_)), "{}", tab.status);
    assert!(tab.access.repaired);
}

#[test]
fn old_session_loads_unprotected() {
    let dir = Scratch::new();
    let src = book(&dir, "book.xlsx");
    let json = format!(
        r#"{{"kind":"Xlsx","title":"book.xlsx","path":{}}}"#,
        serde_json::to_string(&src.display().to_string()).unwrap()
    );
    let t: PersistTab = serde_json::from_str(&json).unwrap();
    assert!(!t.read_only && !t.protected && !t.repaired);
    assert_eq!(restore_tab(&t).access, Access::default());
}
