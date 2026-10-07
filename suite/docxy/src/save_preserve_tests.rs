//! An unedited Word tab saves its original `word/document.xml` bytes, as the
//! CLI does (#1107); any body edit, saved once or more, regenerates it.

use crate::open_mode_tests::Scratch;
use crate::{DocTab, Surface, save_doc_tab, tab_from_path, track_author};
use docxcore::package::{load_package, new_package, save_package_preserving_document};
use std::path::{Path, PathBuf};

/// Valid, but not what the serializer writes: single-quoted attributes,
/// whitespace between elements and a table without `w:tblPr`.
const ODD_DOCUMENT: &str = "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
<w:document xmlns:w='http://schemas.openxmlformats.org/wordprocessingml/2006/main'>\n  \
<w:body>\n    \
<w:p><w:r><w:t>Hello</w:t></w:r></w:p>\n    \
<w:tbl><w:tblGrid><w:gridCol w:w='2000'/></w:tblGrid>\
<w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\n    \
<w:p/>\n  \
</w:body>\n\
</w:document>\n";

fn odd_tab(dir: &Scratch) -> (DocTab, PathBuf) {
    let mut pkg = new_package(docxcore::markdown::from_markdown("placeholder"));
    assert!(pkg.set_part_text("word/document.xml", ODD_DOCUMENT));
    let path = dir.path("odd.docx");
    std::fs::write(&path, save_package_preserving_document(&pkg)).unwrap();
    (tab_from_path(&path), path)
}

fn editor(tab: &mut DocTab) -> &mut docxcore::editor::Editor {
    match &mut tab.surface {
        Surface::Doc(ed) => ed,
        _ => panic!("not a document tab"),
    }
}

fn part(path: &Path, name: &str) -> String {
    let pkg = load_package(&std::fs::read(path).unwrap()).unwrap();
    pkg.part_text(name).unwrap_or_default()
}

#[test]
fn an_unedited_save_writes_the_original_document_bytes() {
    let dir = Scratch::new();
    let (mut tab, path) = odd_tab(&dir);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert_eq!(part(&path, "word/document.xml"), ODD_DOCUMENT);
    let copy = dir.path("copy.docx");
    assert!(save_doc_tab(&mut tab, Some(copy.clone())), "{}", tab.status);
    assert_eq!(part(&copy, "word/document.xml"), ODD_DOCUMENT);
}

#[test]
fn an_edited_save_regenerates_the_document() {
    let dir = Scratch::new();
    let (mut tab, path) = odd_tab(&dir);
    let ed = editor(&mut tab);
    ed.caret.offset = 5;
    ed.anchor = None;
    ed.insert_str(" world");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let doc = part(&path, "word/document.xml");
    assert_ne!(doc, ODD_DOCUMENT);
    assert!(doc.contains("Hello world"), "{doc}");
    assert!(
        doc.contains("<w:tbl><w:tblPr></w:tblPr><w:tblGrid>"),
        "{doc}"
    );
}

/// The tab's package is not rebased after a save: a clean tab whose edit
/// went out in an earlier save must not write the loaded bytes back.
#[test]
fn a_second_save_after_an_edit_keeps_the_edit() {
    let dir = Scratch::new();
    let (mut tab, path) = odd_tab(&dir);
    let ed = editor(&mut tab);
    ed.caret.offset = 5;
    ed.anchor = None;
    ed.insert_str(" world");
    tab.mark_dirty();
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert!(!tab.dirty);
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert!(part(&path, "word/document.xml").contains("Hello world"));
    let copy = dir.path("copy.docx");
    assert!(save_doc_tab(&mut tab, Some(copy.clone())), "{}", tab.status);
    assert!(part(&copy, "word/document.xml").contains("Hello world"));
}

/// Track Changes lives in the settings part: turning it on is saved while
/// the unedited body keeps its bytes.
#[test]
fn a_settings_only_change_keeps_the_document_bytes() {
    let dir = Scratch::new();
    let (mut tab, path) = odd_tab(&dir);
    editor(&mut tab).set_track_changes(Some(track_author(&("Jane Doe".into(), "JD".into()))));
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert_eq!(part(&path, "word/document.xml"), ODD_DOCUMENT);
    let settings = part(&path, "word/settings.xml");
    assert!(settings.contains("<w:trackRevisions/>"), "{settings}");
}
