//! Word's Mark as Final (#617), without a window: the open, the save gate,
//! the backstop, Edit Anyway and the session. The key, ribbon and pointer
//! gates are Protected View's own (`locked`), driven through the app in
//! `uiharness/cases/word-lifecycle.uit`.

use crate::open_mode::{MARKED_FINAL_STATUS, PROTECTED_STATUS};
use crate::open_mode_tests::Scratch;
use crate::{
    DocTab, Surface, edit_anyway_tab, persist_tab, restore_tab, save_doc_tab, tab_from_path,
};
use std::path::{Path, PathBuf};

fn final_docx(dir: &Scratch, name: &str) -> PathBuf {
    let path = dir.path(name);
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/final.docx"),
        &path,
    )
    .unwrap();
    path
}

fn text(tab: &DocTab) -> String {
    match &tab.surface {
        Surface::Doc(ed) => docxcore::import::paragraph_texts(&ed.doc).join("\n"),
        _ => panic!("not a document"),
    }
}

fn leak_edit(tab: &mut DocTab, s: &str) {
    let Surface::Doc(ed) = &mut tab.surface else {
        panic!("not a document")
    };
    ed.insert_str(s);
}

fn saved_is_final(path: &Path) -> bool {
    docxcore::package::load_package(&std::fs::read(path).unwrap())
        .unwrap()
        .marked_final()
}

#[test]
fn a_final_document_opens_locked_and_says_so() {
    let dir = Scratch::new();
    let tab = tab_from_path(&final_docx(&dir, "final.docx"));
    assert!(tab.access.marked_final);
    assert!(tab.access.locked());
    assert!(!tab.access.protected, "not Protected View");
    assert_eq!(tab.caption(), "final.docx [Read-Only]");
    assert_eq!(tab.access.locked_status(), MARKED_FINAL_STATUS);
    assert_eq!(text(&tab), "Final text.");
    // An ordinary document is neither.
    let plain = tab_from_path(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
    );
    assert!(!plain.access.marked_final && !plain.access.locked());
}

#[test]
fn a_final_document_saves_nothing_until_edit_anyway() {
    let dir = Scratch::new();
    let src = final_docx(&dir, "final.docx");
    let before = std::fs::read(&src).unwrap();
    let mut tab = tab_from_path(&src);
    let other = dir.path("copy.docx");
    for target in [None, Some(src.clone()), Some(other.clone())] {
        assert!(!save_doc_tab(&mut tab, target));
        assert_eq!(tab.status.as_ref(), MARKED_FINAL_STATUS);
    }
    assert_eq!(std::fs::read(&src).unwrap(), before);
    assert!(!other.exists());

    // Edit Anyway: not an edit, and a save then writes no mark.
    assert!(edit_anyway_tab(&mut tab));
    assert!(!tab.access.marked_final && !tab.access.locked());
    assert!(!tab.dirty);
    assert_eq!(tab.caption(), "final.docx");
    leak_edit(&mut tab, "Edited ");
    tab.mark_dirty();
    assert!(tab.dirty, "editable now");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert!(!saved_is_final(&src));
    // Only once.
    assert!(!edit_anyway_tab(&mut tab));
}

#[test]
fn a_leaked_edit_to_a_final_document_is_rolled_back() {
    let dir = Scratch::new();
    let mut tab = tab_from_path(&final_docx(&dir, "final.docx"));
    leak_edit(&mut tab, "LEAKED ");
    tab.mark_dirty();
    assert!(!tab.dirty, "the tab stays clean");
    assert_eq!(text(&tab), "Final text.");
    assert_eq!(tab.status.as_ref(), MARKED_FINAL_STATUS);
    assert!(tab.access.marked_final, "the reload is still final");
}

#[test]
fn protected_view_comes_first_and_edit_anyway_waits_for_it() {
    let dir = Scratch::new();
    let mut tab = tab_from_path(&final_docx(&dir, "final.docx"));
    tab.access.protected = true;
    assert_eq!(tab.access.locked_status(), PROTECTED_STATUS);
    assert_eq!(tab.caption(), "final.docx [Protected View]");
    assert!(!edit_anyway_tab(&mut tab));
    assert!(tab.access.marked_final);
    // Enable Editing leaves it final.
    tab.access.protected = false;
    assert_eq!(tab.access.locked_status(), MARKED_FINAL_STATUS);
}

#[test]
fn the_session_keeps_the_packages_answer() {
    let dir = Scratch::new();
    let src = final_docx(&dir, "final.docx");
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();

    // Unedited: its hot-exit copy carries the mark, and so does its file.
    let tab = tab_from_path(&src);
    let mut persisted = persist_tab(&hot, 0, 0, &tab);
    assert!(persisted.hot.is_some());
    assert!(restore_tab(&persisted).access.marked_final);
    persisted.hot = None;
    assert!(restore_tab(&persisted).access.marked_final);

    // After Edit Anyway the copy has no mark: it restores editable, while
    // the file on disk, never saved, is still final.
    let mut tab = tab_from_path(&src);
    edit_anyway_tab(&mut tab);
    let persisted = persist_tab(&hot, 0, 1, &tab);
    let back = restore_tab(&persisted);
    assert!(!back.access.marked_final);
    assert!(!back.access.locked());
    assert!(saved_is_final(&src));
}

#[test]
fn recovered_edits_of_a_final_document_are_kept() {
    // A sidecar written by a build before #617, which let edits through: the
    // mark is still in it, and so is unsaved work.
    let dir = Scratch::new();
    let src = final_docx(&dir, "final.docx");
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    let mut tab = tab_from_path(&src);
    leak_edit(&mut tab, "Recovered ");
    tab.dirty = true;
    let persisted = persist_tab(&hot, 0, 0, &tab);
    assert!(persisted.dirty && persisted.hot.is_some());

    let mut back = restore_tab(&persisted);
    assert!(back.dirty);
    assert!(
        !back.access.locked(),
        "the render backstop would roll it back"
    );
    assert!(text(&back).starts_with("Recovered "));
    assert!(!back.pkg.as_ref().unwrap().marked_final());
    // An edit lands as an edit, and the work survives the next persist.
    back.mark_dirty();
    assert!(text(&back).starts_with("Recovered "));
    let again = restore_tab(&persist_tab(&hot, 0, 1, &back));
    assert!(again.dirty && text(&again).starts_with("Recovered "));
    // The file itself is untouched and still final.
    assert!(saved_is_final(&src));
}
