//! Track Changes on a Word tab (#624), without a window: the editor records
//! typing and deletions, the save carries `w:trackRevisions` and the
//! revisions, and a file saved with it on opens with it on.

use crate::open_mode_tests::Scratch;
use crate::{DocTab, Surface, save_doc_tab, tab_from_path, track_author};
use docxcore::package::{load_package, new_package, save_package};
use std::path::{Path, PathBuf};

fn docx_tab(dir: &Scratch) -> (DocTab, PathBuf) {
    let pkg = new_package(docxcore::markdown::from_markdown("One two three."));
    let path = dir.path("t.docx");
    std::fs::write(&path, save_package(&pkg)).unwrap();
    (tab_from_path(&path), path)
}

fn editor(tab: &mut DocTab) -> &mut docxcore::editor::Editor {
    match &mut tab.surface {
        Surface::Doc(ed) => ed,
        _ => panic!("not a document tab"),
    }
}

fn track_on(tab: &mut DocTab, name: &str) {
    editor(tab).set_track_changes(Some(track_author(&(name.into(), "JD".into()))));
}

fn saved(path: &Path) -> (String, String) {
    let pkg = load_package(&std::fs::read(path).unwrap()).unwrap();
    let text = |n: &str| pkg.part_text(n).unwrap_or_default();
    (text("word/document.xml"), text("word/settings.xml"))
}

fn type_at(tab: &mut DocTab, offset: usize, s: &str) {
    let ed = editor(tab);
    ed.caret.offset = offset;
    ed.anchor = None;
    ed.insert_str(s);
}

#[test]
fn typing_is_recorded_and_the_save_carries_the_setting() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir);
    track_on(&mut tab, "Jane Doe");
    type_at(&mut tab, 8, "and a half ");
    {
        let ed = editor(&mut tab);
        ed.anchor = Some(docxcore::editor::Caret {
            path: ed.caret.path.clone(),
            offset: 4,
        });
        ed.caret.offset = 8;
        ed.delete_selection();
    }
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, settings) = saved(&path);
    assert!(settings.contains("<w:trackRevisions/>"), "{settings}");
    assert!(doc.contains("<w:ins "), "{doc}");
    assert!(doc.contains("w:author=\"Jane Doe\""), "{doc}");
    assert!(
        doc.contains("<w:delText xml:space=\"preserve\">two </w:delText>"),
        "{doc}"
    );
    // Reopened, it starts with tracking on, as the OS user.
    let mut again = tab_from_path(&path);
    assert!(editor(&mut again).track_changes());
}

#[test]
fn off_removes_the_setting_and_a_file_without_it_opens_untracked() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir);
    assert!(!editor(&mut tab).track_changes());
    track_on(&mut tab, "Jane Doe");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert!(saved(&path).1.contains("trackRevisions"));
    editor(&mut tab).set_track_changes(None);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert!(!saved(&path).1.contains("trackRevisions"));
    assert!(!editor(&mut tab_from_path(&path)).track_changes());
}

#[test]
fn a_new_document_with_tracking_on_saves_the_setting_too() {
    let dir = Scratch::new();
    let (mut tab, _) = docx_tab(&dir);
    tab.pkg = None; // never saved: no package of its own yet
    track_on(&mut tab, "Jane Doe");
    type_at(&mut tab, 0, "Hi ");
    let target = dir.path("new.docx");
    assert!(
        save_doc_tab(&mut tab, Some(target.clone())),
        "{}",
        tab.status
    );
    let (doc, settings) = saved(&target);
    assert!(settings.contains("<w:trackRevisions/>"), "{settings}");
    assert!(doc.contains("<w:ins "), "{doc}");
}

/// A file saved with Track Changes on records as the configured reviewer
/// (the name comments and the toggle use), not the OS user.
#[test]
fn a_loaded_tracked_file_records_as_the_configured_reviewer() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir);
    track_on(&mut tab, "Anyone");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let before = crate::configured_identity_raw();
    crate::set_configured_identity("Jane Doe", "JD");
    let mut again = tab_from_path(&path);
    crate::set_configured_identity(&before.0, &before.1);
    type_at(&mut again, 8, "x");
    assert!(save_doc_tab(&mut again, None), "{}", again.status);
    let (doc, _) = saved(&path);
    assert!(doc.contains("w:author=\"Jane Doe\""), "{doc}");
}

/// Changing the reviewer name re-authors tabs that are recording.
#[test]
fn changing_the_reviewer_reauthors_a_recording_tab() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir);
    track_on(&mut tab, "Old");
    type_at(&mut tab, 8, "a");
    let mut tabs = vec![tab];
    crate::reauthor_tracking(&mut tabs, "New", "N");
    let tab = &mut tabs[0];
    type_at(tab, 0, "b");
    assert!(save_doc_tab(tab, None), "{}", tab.status);
    let (doc, _) = saved(&path);
    assert!(doc.contains("w:author=\"New\""), "{doc}");
    assert!(
        doc.contains("w:author=\"Old\""),
        "earlier text keeps its author: {doc}"
    );
}

/// The selected comment is bound to its tab by title and file: closing an
/// earlier tab leaves it on its own document, and another tab never gets it.
#[test]
fn the_selected_comment_follows_its_tab_not_its_index() {
    use crate::{close, selected_comment_id};
    let (a, _) = docx_tab(&Scratch::new());
    let (mut b, _) = docx_tab(&Scratch::new());
    b.title = "Other.docx".into();
    let selected = Some((
        close::tab_ids(std::slice::from_ref(&b))[0].clone(),
        "3".to_string(),
    ));
    let mut tabs = vec![a, b];
    // Selected in the second tab: it answers for that tab only.
    assert_eq!(selected_comment_id(&tabs, 0, &selected), None);
    assert_eq!(
        selected_comment_id(&tabs, 1, &selected).as_deref(),
        Some("3")
    );
    // The first tab closes: the selection is now on index 0, still its own.
    tabs.remove(0);
    assert_eq!(
        selected_comment_id(&tabs, 0, &selected).as_deref(),
        Some("3")
    );
    // And another document that takes index 0 does not inherit it.
    let (c, _) = docx_tab(&Scratch::new());
    let tabs = vec![c];
    assert_eq!(selected_comment_id(&tabs, 0, &selected), None);
}
