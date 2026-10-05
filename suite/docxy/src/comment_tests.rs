//! New comments on a Word tab (#620), without a window: the reviewer name,
//! initials and date they are stamped with, and Add Comment as one undo
//! step that takes the comment out of the saved file and the pane.

use crate::open_mode_tests::Scratch;
use crate::{
    DocTab, Session, Surface, add_doc_comment, delete_doc_comment, live_comments, review_identity,
    save_doc_tab, set_doc_comment_resolved, tab_from_path,
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

/// The comments pane's delete.
fn delete_as_the_pane_does(tab: &mut DocTab, id: i32) {
    delete_doc_comment(tab, &id.to_string());
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
/// back (and, since #971, its record). A new comment must not take that
/// id: its own undo would leave it "live" on the old markers, listed and
/// saved.
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
    let texts: Vec<String> = listed(&tab).into_iter().map(|c| c.text).collect();
    assert_eq!(
        texts,
        ["Loaded"],
        "the loaded one is back, the new one gone (#971)"
    );
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, id), 0, "{doc}");
    assert!(!comments.contains("Colour?"), "{comments}");
}

/// FIX r3 #1: an id freed before the add (Remove All took comment 1's
/// markers, so it is listed and saved no more) must not be taken: undoing
/// the add and then Remove All brings 1's markers back, and the new
/// comment would be live on them.
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

/// A loaded comment Word could have written: two paragraphs, a bold run,
/// a `w14:paraId`. A re-creation from its parsed record would lose all of
/// that, so finding it byte-for-byte in a save proves its XML was kept.
const RICH: &str = "<w:comment w:id=\"1\" w:author=\"Ann\" w:initials=\"A\" \
    w:date=\"2020-01-02T03:04:05Z\" w14:paraId=\"1A2B\"><w:p><w:r><w:rPr><w:b/></w:rPr>\
    <w:t>Bold</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p></w:comment>";

/// One whose producer wrote `w:id` after other attributes.
const REORDERED: &str = "<w:comment w:author=\"Bob\" w:id=\"2\" w:date=\"2020-01-02T03:04:05Z\">\
    <w:p><w:r><w:t>Reordered</w:t></w:r></w:p></w:comment>";

/// [`FOX`] with comments 1 and 2 on it, written as [`RICH`] and [`REORDERED`].
fn rich_package() -> Package {
    let mut ed = docxcore::editor::Editor::new(docxcore::markdown::from_markdown(FOX));
    for id in ["1", "2"] {
        ed.select_all();
        assert!(ed.add_comment(id));
    }
    let mut pkg = new_package(ed.doc);
    pkg.insert_comment_xml(RICH);
    pkg.insert_comment_xml(REORDERED);
    pkg
}

fn listed_ids(tab: &DocTab) -> Vec<String> {
    listed(tab).into_iter().map(|c| c.id).collect()
}

/// #971 A1: Delete Comment, then undo, brings a loaded comment back to the
/// pane and its original XML back to the save.
#[test]
fn undo_delete_loaded_comment_restores_its_xml() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &rich_package());
    delete_doc_comment(&mut tab, "1");
    assert_eq!(listed_ids(&tab), ["2"]);
    assert!(editor(&mut tab).undo(), "the delete");
    assert_eq!(listed_ids(&tab), ["1", "2"]);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, 1), 3, "{doc}");
    assert!(comments.contains(RICH), "{comments}");
}

/// #971 A2: a delete that stays saves neither the comment nor its markers,
/// also after undo and redo.
#[test]
fn delete_comment_saves_without_it() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &rich_package());
    delete_doc_comment(&mut tab, "1");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, 1), 0, "{doc}");
    assert_eq!(comment_ids(&comments), ["2"], "{comments}");
}

#[test]
fn redo_delete_comment_removes_it() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &rich_package());
    delete_doc_comment(&mut tab, "1");
    assert!(editor(&mut tab).undo());
    assert!(editor(&mut tab).redo());
    assert_eq!(listed_ids(&tab), ["2"]);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, 1), 0, "{doc}");
    assert_eq!(comment_ids(&comments), ["2"], "{comments}");
}

/// #971: a save between the delete and its undo does not lose the
/// comment: the base package still holds it.
#[test]
fn delete_save_undo_save_restores_the_comment() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &rich_package());
    delete_doc_comment(&mut tab, "1");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert!(editor(&mut tab).undo());
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, 1), 3, "{doc}");
    assert!(comments.contains(RICH), "{comments}");
}

/// #971 A3: the same for a comment added in the session.
#[test]
fn undo_delete_session_comment_restores_it() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &fox_package());
    let id = comment(&mut tab, "Colour?");
    delete_doc_comment(&mut tab, &id.to_string());
    assert!(listed(&tab).is_empty());
    assert!(editor(&mut tab).undo(), "the delete");
    assert_eq!(listed_ids(&tab), [id.to_string()]);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!(markers(&doc, id), 3, "{doc}");
    assert!(comments.contains("Colour?"), "{comments}");
}

/// #971 A4: Remove All, then undo, writes every loaded comment back as it
/// was, whatever its attribute order.
#[test]
fn undo_remove_all_restores_loaded_comments_xml() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &rich_package());
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    assert!(listed(&tab).is_empty());
    assert!(editor(&mut tab).undo(), "the Remove All");
    assert_eq!(listed_ids(&tab), ["1", "2"]);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!((markers(&doc, 1), markers(&doc, 2)), (3, 3), "{doc}");
    assert!(comments.contains(RICH), "{comments}");
    assert!(comments.contains(REORDERED), "{comments}");
}

/// #971 A5: without the undo, the save has no comment left, the reordered
/// one included (comments.xml is no longer emptied at Remove All).
#[test]
fn remove_all_removes_comments_in_any_attribute_order() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &rich_package());
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    assert!(tab.dirty);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!((markers(&doc, 1), markers(&doc, 2)), (0, 0), "{doc}");
    assert!(!comments.contains("<w:comment "), "{comments}");
}

/// The ids of the `<w:comment>`s in a comments.xml, in order.
fn comment_ids(comments_xml: &str) -> Vec<String> {
    docxcore::comments::parse_comments_xml(comments_xml)
        .into_iter()
        .map(|c| c.id)
        .collect()
}

/// [`rich_package`] with its comments.xml re-encoded as UTF-16LE with a
/// BOM: the loader decodes it, but the comment list (and so the per-id
/// save) sees no comment in it.
fn utf16_comments_package() -> Package {
    let mut pkg = rich_package();
    let xml = pkg.part_text("word/comments.xml").unwrap();
    let mut bytes = vec![0xff, 0xfe];
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    assert!(pkg.set_part("word/comments.xml", bytes));
    pkg
}

/// #971 FIX r1 M1: Remove All leaves no comment in a save, also one in a
/// comments.xml the comment list can't read.
#[test]
fn remove_all_clears_a_utf16_comments_part() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &utf16_comments_package());
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!((markers(&doc, 1), markers(&doc, 2)), (0, 0), "{doc}");
    assert!(!comments.contains("<w:comment "), "{comments}");
}

/// #971 FIX r1 M1: … and its undo keeps the comments whose markers came
/// back, as they were.
#[test]
fn undo_remove_all_keeps_a_utf16_comments_part() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &utf16_comments_package());
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    assert!(editor(&mut tab).undo(), "the Remove All");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!((markers(&doc, 1), markers(&doc, 2)), (3, 3), "{doc}");
    assert!(comments.contains(RICH), "{comments}");
    assert!(comments.contains(REORDERED), "{comments}");
}

/// #971 FIX r1 M1: without a Remove All, a save keeps the comments of a
/// part the comment list can't read.
#[test]
fn a_save_keeps_a_utf16_comments_part() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &utf16_comments_package());
    editor(&mut tab).insert_str("x");
    tab.mark_dirty();
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (_, comments) = saved(&path);
    assert!(comments.contains(RICH), "{comments}");
    assert!(comments.contains(REORDERED), "{comments}");
}

/// #971 FIX r1 M1: an id the per-id save reads as another number (`03` as
/// 3) is still removed by Remove All.
#[test]
fn remove_all_clears_a_comment_whose_id_is_not_canonical() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let mut ed = docxcore::editor::Editor::new(docxcore::markdown::from_markdown(FOX));
    ed.select_all();
    assert!(ed.add_comment("03"));
    let mut pkg = new_package(ed.doc);
    pkg.insert_comment_xml("<w:comment w:id=\"03\" w:author=\"Ann\"><w:p/></w:comment>");
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &pkg);
    assert_eq!(listed_ids(&tab), ["03"]);
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert!(!doc.contains("w:id=\"03\""), "{doc}");
    assert!(!comments.contains("<w:comment "), "{comments}");
}

/// #971 FIX r2 f4: Delete Comment of `w:id="03"` leaves it out of the
/// save and comment 3 (the same number, written plainly) in it.
#[test]
fn delete_comment_matches_the_id_as_written() {
    let three =
        "<w:comment w:id=\"3\" w:author=\"Bob\"><w:p><w:r><w:t>three</w:t></w:r></w:p></w:comment>";
    let mut ed = docxcore::editor::Editor::new(docxcore::markdown::from_markdown(FOX));
    for id in ["03", "3"] {
        ed.select_all();
        assert!(ed.add_comment(id));
    }
    let mut pkg = new_package(ed.doc);
    pkg.insert_comment_xml("<w:comment w:author=\"Ann\" w:id=\"03\"><w:p/></w:comment>");
    pkg.insert_comment_xml(three);
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &pkg);
    delete_doc_comment(&mut tab, "03");
    assert_eq!(listed_ids(&tab), ["3"]);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert!(!doc.contains("w:id=\"03\""), "{doc}");
    assert_eq!(comment_ids(&comments), ["3"], "{comments}");
    assert!(comments.contains(three), "{comments}");
}

/// #971 FIX r2 f2: a comment added to a document whose comments.xml is
/// UTF-16 is saved beside the original, both readable.
#[test]
fn a_new_comment_joins_a_utf16_comments_part() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &utf16_comments_package());
    let id = comment(&mut tab, "Colour?");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let saved_pkg = load_package(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        saved_pkg
            .part("word/comments.xml")
            .unwrap()
            .starts_with(&[0xff, 0xfe])
    );
    let (_, comments) = saved(&path);
    assert!(comments.contains(RICH), "{comments}");
    assert!(comments.contains(REORDERED), "{comments}");
    assert_eq!(
        saved_pkg.comment_ids(),
        ["1".to_string(), "2".to_string(), id.to_string()]
    );
    assert!(comments.contains("Colour?"), "{comments}");
}

/// The saved comments with their resolved state.
fn saved_resolved(path: &Path) -> Vec<(String, bool)> {
    let pkg = load_package(&std::fs::read(path).unwrap()).unwrap();
    docxcore::comments::parse_comments(&pkg)
        .into_iter()
        .map(|c| (c.id, c.resolved))
        .collect()
}

/// #621 A1: Resolve writes `w15:done`, Reopen writes it back, and a loaded
/// resolved comment lists as resolved.
#[test]
fn resolve_and_reopen_a_comment_round_trips() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &rich_package());
    assert_eq!(
        set_doc_comment_resolved(&mut tab, "2", Some(true)),
        Some(true)
    );
    assert_eq!(set_doc_comment_resolved(&mut tab, "9", None), None);
    assert!(tab.dirty);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert_eq!(
        saved_resolved(&path),
        [("1".to_string(), false), ("2".to_string(), true)]
    );
    let pkg = load_package(&std::fs::read(&path).unwrap()).unwrap();
    let ext = pkg.part_text("word/commentsExtended.xml").expect("part");
    assert!(ext.contains("w15:done=\"1\""), "{ext}");
    // Reloaded, it is listed resolved; Reopen (a toggle) clears it.
    let mut tab = tab_from_path(&path);
    assert!(listed(&tab).iter().any(|c| c.id == "2" && c.resolved));
    assert_eq!(set_doc_comment_resolved(&mut tab, "2", None), Some(false));
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert_eq!(
        saved_resolved(&path),
        [("1".to_string(), false), ("2".to_string(), false)]
    );
}

/// #621: a comment added in the tab can be resolved before its first save.
#[test]
fn a_new_comment_resolved_before_the_first_save_saves_resolved() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &fox_package());
    let id = comment(&mut tab, "Colour?");
    set_doc_comment_resolved(&mut tab, &id.to_string(), Some(true));
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert_eq!(saved_resolved(&path), [(id.to_string(), true)]);
}

/// #621: the state follows a comment through Delete and its undo.
#[test]
fn resolved_state_survives_delete_and_undo() {
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &rich_package());
    set_doc_comment_resolved(&mut tab, "1", Some(true));
    delete_doc_comment(&mut tab, "1");
    assert!(editor(&mut tab).undo(), "the delete");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert_eq!(
        saved_resolved(&path),
        [("1".to_string(), true), ("2".to_string(), false)]
    );
}

/// #621 A2: Delete All removes every record, part and marker, keeps the
/// anchored text, and one undo restores markers and records with their
/// resolved state.
#[test]
fn delete_all_comments_removes_every_part_and_one_undo_restores() {
    use crate::inspector::{InspectCategory, inspect_remove};
    let dir = Scratch::new();
    let (mut tab, path) = docx_tab(&dir, &rich_package());
    set_doc_comment_resolved(&mut tab, "2", Some(true));
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    inspect_remove(&mut tab, InspectCategory::Comments).unwrap();
    assert!(listed(&tab).is_empty());
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let pkg = load_package(&std::fs::read(&path).unwrap()).unwrap();
    for part in ["word/comments.xml", "word/commentsExtended.xml"] {
        assert!(pkg.part(part).is_none(), "{part}");
    }
    let doc = pkg.part_text("word/document.xml").unwrap();
    assert!(!doc.contains("comment"), "{doc}");
    assert!(doc.contains(FOX), "{doc}");
    assert!(editor(&mut tab).undo(), "one step");
    assert_eq!(listed_ids(&tab), ["1", "2"]);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let (doc, comments) = saved(&path);
    assert_eq!((markers(&doc, 1), markers(&doc, 2)), (3, 3), "{doc}");
    assert!(comments.contains(RICH), "{comments}");
    assert_eq!(
        saved_resolved(&path),
        [("1".to_string(), false), ("2".to_string(), true)]
    );
}
