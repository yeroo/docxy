//! New comments on a Word tab (#620), without a window: the reviewer name,
//! initials and date they are stamped with, and Add Comment as one undo
//! step that takes the comment out of the saved file and the pane.

use crate::open_mode_tests::Scratch;
use crate::{
    DocTab, Session, Surface, add_doc_comment, live_comments, review_identity, save_doc_tab,
    tab_from_path,
};
use docxcore::package::{Package, load_package, new_package, save_package};
use std::path::{Path, PathBuf};

const FOX: &str = "The quick brown fox.";

fn write(dir: &Scratch, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A .docx holding `pkg`, open in a tab.
fn docx_tab(dir: &Scratch, pkg: &Package) -> (DocTab, PathBuf) {
    let path = write(dir, "c.docx", &save_package(pkg));
    (tab_from_path(&path), path)
}

fn fox_package() -> Package {
    new_package(docxcore::markdown::from_markdown(FOX))
}

fn editor(tab: &mut DocTab) -> &mut docxcore::editor::Editor {
    match &mut tab.surface {
        Surface::Doc(ed) => ed,
        _ => panic!("not a document tab"),
    }
}

/// What the comments pane lists.
fn listed(tab: &DocTab) -> Vec<docxcore::comments::Comment> {
    match &tab.surface {
        Surface::Doc(ed) => live_comments(tab, &ed.doc),
        _ => panic!("not a document tab"),
    }
}

/// Select the whole document and comment on it, as Jane Doe.
fn comment(tab: &mut DocTab, text: &str) -> i32 {
    editor(tab).select_all();
    add_doc_comment(tab, text.into(), ("Jane Doe".into(), "JD".into())).expect("a selection")
}

/// The saved document.xml and comments.xml (empty when there is none).
fn saved(path: &Path) -> (String, String) {
    let pkg = load_package(&std::fs::read(path).unwrap()).unwrap();
    let text = |name: &str| pkg.part_text(name).unwrap_or_default();
    (text("word/document.xml"), text("word/comments.xml"))
}

fn markers(document_xml: &str, id: i32) -> usize {
    document_xml.matches(&format!("w:id=\"{id}\"")).count()
}

fn is_utc_date_time(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

#[test]
fn new_comment_is_stamped_with_user_name_and_utc_date() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &fox_package());
    editor(&mut tab).select_all();
    let id = add_doc_comment(&mut tab, "Colour?".into(), review_identity("Jane doe", "")).unwrap();
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (_, comments) = saved(&path);
    let parsed = docxcore::comments::parse_comments_xml(&comments);
    assert_eq!(parsed.len(), 1, "{comments}");
    let c = &parsed[0];
    assert_eq!(c.id, id.to_string());
    assert_eq!(c.author, "Jane doe");
    assert_eq!(c.initials, "JD");
    assert!(is_utc_date_time(&c.date), "w:date {:?}", c.date);
}

#[test]
fn review_identity_falls_back_to_the_os_account_then_docxy() {
    assert_eq!(
        review_identity(" Jane Doe ", ""),
        ("Jane Doe".to_string(), "JD".to_string())
    );
    assert_eq!(
        review_identity("Jane Doe", "J.D."),
        ("Jane Doe".to_string(), "J.D.".to_string())
    );
    let os = ["USERNAME", "USER"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| "docxy".to_string());
    let (name, initials) = review_identity("  ", "");
    assert_eq!(name, os);
    assert_eq!(initials, docxcore::comments::initials(&os));
}

#[test]
fn undo_of_new_comment_drops_markers_and_comment_on_save() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &fox_package());
    let id = comment(&mut tab, "Colour?");
    assert!(editor(&mut tab).undo());
    assert!(listed(&tab).is_empty(), "the pane drops it");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, id), 0, "{doc}");
    assert!(!comments.contains("Colour?"), "{comments}");
}

#[test]
fn redo_of_new_comment_restores_it() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &fox_package());
    let id = comment(&mut tab, "Colour?");
    assert!(editor(&mut tab).undo());
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert!(editor(&mut tab).redo());
    assert_eq!(listed(&tab).len(), 1);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, id), 3, "{doc}");
    assert!(comments.contains("Colour?"), "{comments}");
}

/// A converted tab's first save reloads its package from the bytes it
/// wrote, so the comment is in that base; an undo after it still drops it.
#[test]
fn converted_tab_add_save_undo_save_has_no_comment() {
    let dir = Scratch::new();
    let rtf = br"{\rtf1\ansi\pard The quick brown fox.\par}";
    let mut tab = tab_from_path(&write(&dir, "letter.rtf", rtf));
    assert!(tab.access.converted.is_some(), "{}", tab.status);
    let id = comment(&mut tab, "Colour?");
    let docx = dir.path("letter.docx");
    assert!(save_doc_tab(&mut tab, Some(docx.clone())), "{}", tab.status);
    assert!(saved(&docx).1.contains("Colour?"));
    assert!(tab.access.converted.is_none(), "now that Word document");
    assert!(editor(&mut tab).undo());
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&docx);
    assert_eq!(markers(&doc, id), 0, "{doc}");
    assert!(!comments.contains("Colour?"), "{comments}");
}

/// A comment loaded from the file stays, though no marker of it is in the
/// body (one anchored in a header has none there), while a new comment's
/// undo drops the new one.
#[test]
fn loaded_comment_without_body_markers_survives_save() {
    let dir = Scratch::new();
    let mut pkg = fox_package();
    pkg.add_comment(7, "Ann", "A", "2020-01-02T03:04:05Z", "Loaded note");
    let (mut tab, path) = docx_tab(&dir, &pkg);
    assert_eq!(tab.comments.len(), 1);
    let id = comment(&mut tab, "Colour?");
    assert!(editor(&mut tab).undo());
    let texts: Vec<String> = listed(&tab).into_iter().map(|c| c.text).collect();
    assert_eq!(texts, ["Loaded note"]);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, id), 0, "{doc}");
    assert!(comments.contains("Loaded note"), "{comments}");
    assert!(!comments.contains("Colour?"), "{comments}");
}

/// The next comment never reuses an undone one's id: redo brings it back.
#[test]
fn an_undone_comment_keeps_its_id() {
    let dir = Scratch::new();
    let (mut tab, _) = docx_tab(&dir, &fox_package());
    let first = comment(&mut tab, "one");
    assert!(editor(&mut tab).undo());
    let second = comment(&mut tab, "two");
    assert_ne!(first, second);
}

#[test]
fn user_name_round_trips_through_the_session_and_old_sessions_load() {
    let session = Session {
        user_name: "Jane Doe".into(),
        user_initials: "JD".into(),
        ..Session::default()
    };
    let json = serde_json::to_string(&session).unwrap();
    let back: Session = serde_json::from_str(&json).unwrap();
    assert_eq!(back.user_name, "Jane Doe");
    assert_eq!(back.user_initials, "JD");

    let old: Session = serde_json::from_str(r#"{"tabs":[],"active":0}"#).unwrap();
    assert_eq!(old.user_name, "");
    assert_eq!(old.user_initials, "");
}

/// A package whose body carries the markers of each of `comments` (around
/// the whole text) and whose comments.xml holds them, as Word saves them.
fn commented_package(comments: &[(i32, &str)]) -> Package {
    let mut ed = docxcore::editor::Editor::new(docxcore::markdown::from_markdown(FOX));
    for (id, _) in comments {
        ed.select_all();
        assert!(ed.add_comment(&id.to_string()));
    }
    let mut pkg = new_package(ed.doc);
    for (id, text) in comments {
        pkg.add_comment(*id, "Ann", "A", "2020-01-02T03:04:05Z", text);
    }
    pkg
}

/// The comments pane's delete: the markers, then the record.
fn delete_as_the_pane_does(tab: &mut DocTab, id: i32) {
    editor(tab).remove_comment_markers(&id.to_string());
    tab.comments.retain(|c| c.id != id.to_string());
}

/// FIX r1 #1: a deleted loaded comment's id is still in the base package;
/// a new comment that took it would be saved with the old text.
#[test]
fn a_new_comment_never_takes_a_deleted_loaded_comments_id() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &commented_package(&[(1, "First"), (2, "Second")]));
    assert_eq!(tab.comments.len(), 2);
    delete_as_the_pane_does(&mut tab, 2);
    let id = comment(&mut tab, "Colour?");
    assert_eq!(id, 3);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    let parsed = docxcore::comments::parse_comments_xml(&comments);
    let texts: Vec<(String, String)> = parsed.into_iter().map(|c| (c.id, c.text)).collect();
    assert_eq!(
        texts,
        [
            ("1".to_string(), "First".to_string()),
            ("3".to_string(), "Colour?".to_string())
        ]
    );
    assert_eq!(markers(&doc, 2), 0, "{doc}");
    assert_eq!(markers(&doc, 3), 3, "{doc}");
}

/// FIX r1 #4, r3 #2: a new comment deleted before any undo or save is in
/// no list, no marker and no base any more; the next comment still gets a
/// fresh id, so undoing it and then the delete never makes it live on the
/// first one's markers.
#[test]
fn a_comment_added_after_deleting_a_new_one_gets_a_fresh_id() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &fox_package());
    let first = comment(&mut tab, "one");
    delete_as_the_pane_does(&mut tab, first);
    let second = comment(&mut tab, "two");
    assert_ne!(first, second);
    assert!(editor(&mut tab).undo(), "the second add");
    assert!(editor(&mut tab).undo(), "the delete");
    assert!(listed(&tab).iter().all(|c| c.text != "two"));
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (_, comments) = saved(&path);
    assert!(!comments.contains("two"), "{comments}");
}

/// FIX r1 #3: the inspector counts what the pane lists, so an undone new
/// comment is not "found".
#[test]
fn the_inspector_does_not_count_an_undone_comment() {
    use crate::inspector::{InspectCategory, inspect_doc_tab};
    let dir = Scratch::new();
    let (mut tab, _) = docx_tab(&dir, &fox_package());
    comment(&mut tab, "Colour?");
    assert_eq!(inspect_doc_tab(&tab).unwrap().comments, 1);
    assert!(editor(&mut tab).undo());
    let after = inspect_doc_tab(&tab).unwrap();
    assert_eq!(after.comments, 0);
    assert!(!after.found(InspectCategory::Comments));
}

/// FIX r1 #3: add, undo, Remove All, redo, save. With no markers left,
/// Remove All pushes no undo step, so redo still brings the markers back:
/// their comment must come back with them, never bare markers.
#[test]
fn remove_all_after_an_undone_comment_then_redo_saves_it_whole() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &fox_package());
    let id = comment(&mut tab, "Colour?");
    assert!(editor(&mut tab).undo());
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    assert!(
        editor(&mut tab).redo(),
        "redo survives a Remove All that found nothing"
    );
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, id), 3, "{doc}");
    assert!(comments.contains("Colour?"), "orphan markers: {comments}");
}

/// FIX r1 #3: Remove All keeps a new comment's record, so undoing it brings
/// the comment back with its markers.
#[test]
fn undo_of_remove_all_brings_a_new_comment_back_whole() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &fox_package());
    let id = comment(&mut tab, "Colour?");
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    assert!(listed(&tab).is_empty());
    assert!(editor(&mut tab).undo(), "the Remove All");
    assert_eq!(listed(&tab).len(), 1);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, id), 3, "{doc}");
    assert!(comments.contains("Colour?"), "{comments}");
}

/// FIX r2 #1: Remove All, then its undo, puts a loaded comment's markers
/// back without a record. A new comment must not take that id: its own
/// undo would leave it "live" on the old markers, listed and saved.
#[test]
fn a_new_comment_never_takes_an_id_whose_markers_are_in_the_body() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &commented_package(&[(1, "Loaded")]));
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    assert!(editor(&mut tab).undo(), "the Remove All");
    let id = comment(&mut tab, "Colour?");
    assert_ne!(id, 1);
    assert!(editor(&mut tab).undo(), "the new comment");
    assert!(listed(&tab).is_empty());
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, id), 0, "{doc}");
    assert!(!comments.contains("Colour?"), "{comments}");
}

/// FIX r3 #1: an id freed before the add (Remove All emptied comments, the
/// base and the body of comment 1) must not be taken: undoing the add and
/// then Remove All brings 1's markers back, and the new comment would be
/// live on them.
#[test]
fn a_new_comment_never_takes_an_id_freed_by_remove_all() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &commented_package(&[(1, "Loaded")]));
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    let id = comment(&mut tab, "Colour?");
    assert_ne!(id, 1);
    assert!(editor(&mut tab).undo(), "the new comment");
    assert!(editor(&mut tab).undo(), "the Remove All");
    assert!(listed(&tab).iter().all(|c| c.text != "Colour?"));
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (_, comments) = saved(&path);
    assert!(!comments.contains("Colour?"), "{comments}");
}
