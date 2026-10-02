//! Opening a Word 97-2003 `.doc` (#634), without a window: the import, the
//! Compatibility Mode caption, the save gate that never writes the binary
//! original, Convert, and the session round trip.

use crate::doc_import::{
    self, BINARY_TARGET_REFUSED, COMPAT_SUFFIX, DocImport, IMPORTED_STATUS, convert_tab,
    is_binary_doc_path, save_name,
};
use crate::open_mode_tests::Scratch;
use crate::{
    DocSaveTarget, DocTab, Kind, OPEN_EXTENSIONS, Session, Surface, doc_save_target, is_imported,
    persist_tab, restore_tab, save_doc_tab, tab_from_path,
};
use std::path::{Path, PathBuf};

/// A minimal Word 97 document holding `text` (ASCII, one compressed piece):
/// enough for the importer, built here so these tests don't depend on the
/// Word-made corpus.
fn tiny_doc(text: &str) -> Vec<u8> {
    let text = text.as_bytes();
    let mut word = vec![0u8; 0x400 + text.len()];
    let put16 =
        |b: &mut Vec<u8>, at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let put32 =
        |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put16(&mut word, 0, 0xA5EC); // wIdent
    put16(&mut word, 2, 0x00C1); // nFib: Word 97
    put16(&mut word, 10, 0x0200); // fWhichTblStm: 1Table
    put16(&mut word, 32, 14); // csw
    put16(&mut word, 62, 22); // cslw
    put32(&mut word, 64 + 12, text.len() as u32); // ccpText
    put16(&mut word, 152, 93); // cbRgFcLcb
    word[0x400..].copy_from_slice(text);
    // The Clx: one piece, compressed, at 0x400.
    let mut table = vec![0x02];
    table.extend(20u32.to_le_bytes());
    table.extend(0u32.to_le_bytes());
    table.extend((text.len() as u32).to_le_bytes());
    table.extend(0u16.to_le_bytes());
    table.extend((0x800u32 | 0x4000_0000).to_le_bytes());
    table.extend(0u16.to_le_bytes());
    put32(&mut word, 154 + 33 * 8, 0); // fcClx
    put32(&mut word, 154 + 33 * 8 + 4, table.len() as u32); // lcbClx
    opccore::cfb::write_cfb(&[("WordDocument", word), ("1Table", table)])
}

const TEXT: &str = "Written by Word 97.\rSecond paragraph.\r";

fn write_doc(dir: &Scratch, name: &str) -> PathBuf {
    let path = dir.path(name);
    std::fs::write(&path, tiny_doc(TEXT)).unwrap();
    path
}

fn text_of(tab: &DocTab) -> String {
    let Surface::Doc(ed) = &tab.surface else {
        panic!("not a document tab")
    };
    ed.doc.plain_text()
}

fn edit(tab: &mut DocTab) {
    let Surface::Doc(ed) = &mut tab.surface else {
        panic!("not a document tab")
    };
    ed.insert_str("Edited. ");
    tab.dirty = true;
}

fn saved_mode(path: &Path) -> Option<u32> {
    docxcore::package::load_package(&std::fs::read(path).unwrap())
        .unwrap()
        .compatibility_mode()
}

#[test]
fn a_doc_opens_imported_in_compatibility_mode() {
    let dir = Scratch::new();
    let path = write_doc(&dir, "Report.doc");
    let tab = tab_from_path(&path);
    assert!(tab.kind == Kind::Docx);
    assert!(!tab.load_failed, "{}", tab.status);
    assert_eq!(tab.status.as_ref(), IMPORTED_STATUS);
    assert_eq!(tab.import, DocImport::IMPORTED);
    // The title stays the file name (it seeds Save As); the caption says
    // what Word's title bar says.
    assert_eq!(tab.title.as_ref(), "Report.doc");
    assert_eq!(tab.caption(), "Report.doc [Compatibility Mode]");
    assert!(is_imported(&tab));
    assert!(!tab.dirty);
    assert!(text_of(&tab).contains("Written by Word 97."));
    assert!(text_of(&tab).contains("Second paragraph."));
    assert_eq!(tab.pkg.as_ref().unwrap().compatibility_mode(), Some(11));
}

/// Routing goes by content: a `.doc` renamed `.docx` imports too.
#[test]
fn a_doc_renamed_docx_still_imports() {
    let dir = Scratch::new();
    let path = write_doc(&dir, "renamed.docx");
    let tab = tab_from_path(&path);
    assert!(!tab.load_failed, "{}", tab.status);
    assert_eq!(tab.import, DocImport::IMPORTED);
    assert_eq!(tab.caption(), "renamed.docx [Compatibility Mode]");
    assert!(is_imported(&tab));
}

/// A docx is not an import, and an unreadable `.doc` says why.
#[test]
fn a_docx_is_not_imported_and_a_broken_doc_reports_its_error() {
    let dir = Scratch::new();
    let docx = dir.path("plain.docx");
    let pkg = docxcore::package::new_package(docxcore::markdown::from_markdown("Hi\n"));
    std::fs::write(&docx, docxcore::package::save_package(&pkg)).unwrap();
    let tab = tab_from_path(&docx);
    assert_eq!(tab.import, DocImport::default());
    assert_eq!(tab.caption(), "plain.docx");
    assert!(!is_imported(&tab));

    let broken = dir.path("no-table.doc");
    let mut bytes = tiny_doc(TEXT);
    // Rename the table stream in the directory, so the document's piece
    // table is gone.
    let name: Vec<u8> = "1Table".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let at = bytes
        .windows(name.len())
        .position(|w| w == name)
        .expect("the 1Table directory entry");
    bytes[at] = b'2';
    std::fs::write(&broken, bytes).unwrap();
    let tab = tab_from_path(&broken);
    assert!(tab.load_failed);
    assert!(
        tab.status
            .starts_with("load error: damaged Word 97-2003 document"),
        "{}",
        tab.status
    );
    assert_eq!(tab.import, DocImport::default());
}

/// Ctrl+S on an imported document asks where the `.docx` goes (refused in
/// words in a harness), suggesting `<stem>.docx`; nothing writes the `.doc`.
#[test]
fn save_never_writes_the_binary_original() {
    let dir = Scratch::new();
    let path = write_doc(&dir, "Report.doc");
    let before = std::fs::read(&path).unwrap();
    let mut tab = tab_from_path(&path);
    edit(&mut tab);
    let source = tab.import.binary_source;
    assert_eq!(
        doc_save_target(tab.path.as_deref(), source, false),
        DocSaveTarget::NeedsDialog
    );
    assert_eq!(
        doc_save_target(tab.path.as_deref(), source, true),
        DocSaveTarget::RefuseHarness
    );
    assert!(doc_import::IMPORTED_HARNESS.ends_with("use the harness save-as verb"));
    assert_eq!(save_name(&tab), "Report.docx");

    // In place, and Save As to any .doc or .dot (in any case), refuse.
    assert!(!save_doc_tab(&mut tab, None));
    assert!(tab.status.contains("use Save As"), "{}", tab.status);
    for name in ["Report.doc", "Other.DOC", "Template.dot"] {
        let target = dir.path(name);
        assert!(!save_doc_tab(&mut tab, Some(target.clone())), "{name}");
        assert_eq!(tab.status.as_ref(), BINARY_TARGET_REFUSED);
        assert_eq!(target.exists(), name == "Report.doc", "{name}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(tab.dirty);
    assert_eq!(tab.path.as_deref(), Some(path.as_path()));
}

/// Save As `.docx` without converting keeps Compatibility Mode (11 in the
/// file, the caption's suffix) and rebinds the tab, whose next Save is in
/// place.
#[test]
fn save_as_docx_keeps_compatibility_mode_and_rebinds() {
    let dir = Scratch::new();
    let path = write_doc(&dir, "Report.doc");
    let before = std::fs::read(&path).unwrap();
    let mut tab = tab_from_path(&path);
    edit(&mut tab);
    let target = dir.path("Report.docx");
    assert!(
        save_doc_tab(&mut tab, Some(target.clone())),
        "{}",
        tab.status
    );
    assert_eq!(saved_mode(&target), Some(11));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        tab.import,
        DocImport {
            binary_source: false,
            compat: true
        }
    );
    assert_eq!(tab.caption(), "Report.docx [Compatibility Mode]");
    assert!(!is_imported(&tab) && !tab.dirty);
    assert_eq!(
        doc_save_target(tab.path.as_deref(), tab.import.binary_source, true),
        DocSaveTarget::InPlace
    );
    let reopened = tab_from_path(&target);
    assert!(text_of(&reopened).contains("Edited. Written by Word 97."));
}

/// File > Info > Convert: compatibilityMode 15, the suffix gone, the tab
/// dirty; the original still never written. A second Convert, or one on a
/// document never in Compatibility Mode, changes nothing and says so.
#[test]
fn convert_leaves_compatibility_mode_and_saves_15() {
    let dir = Scratch::new();
    let path = write_doc(&dir, "Report.doc");
    let before = std::fs::read(&path).unwrap();
    let mut tab = tab_from_path(&path);
    let status = convert_tab(&mut tab).unwrap();
    assert_eq!(tab.status.as_ref(), status);
    assert!(tab.dirty);
    assert_eq!(tab.caption(), "Report.doc");
    assert_eq!(
        tab.import,
        DocImport {
            binary_source: true,
            compat: false
        }
    );
    assert!(is_imported(&tab));
    assert_eq!(
        doc_save_target(tab.path.as_deref(), tab.import.binary_source, false),
        DocSaveTarget::NeedsDialog
    );
    assert!(!save_doc_tab(&mut tab, None));
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let again = convert_tab(&mut tab).unwrap_err();
    assert!(again.contains("not in Compatibility Mode"), "{again}");

    let target = dir.path("Report.docx");
    assert!(
        save_doc_tab(&mut tab, Some(target.clone())),
        "{}",
        tab.status
    );
    assert_eq!(saved_mode(&target), Some(15));
    assert_eq!(tab.caption(), "Report.docx");

    // A document that was never in Compatibility Mode has nothing to convert.
    let mut plain = tab_from_path(&target);
    assert!(convert_tab(&mut plain).is_err());
    assert!(!plain.dirty);
}

/// A `.doc` renamed `.docx`, converted: its file is still the binary
/// original, so Ctrl+S asks rather than writing over it.
#[test]
fn a_renamed_doc_converted_still_asks_before_saving() {
    let dir = Scratch::new();
    let path = write_doc(&dir, "renamed.docx");
    let before = std::fs::read(&path).unwrap();
    let mut tab = tab_from_path(&path);
    convert_tab(&mut tab).unwrap();
    assert_eq!(
        doc_save_target(tab.path.as_deref(), tab.import.binary_source, false),
        DocSaveTarget::NeedsDialog
    );
    assert!(!save_doc_tab(&mut tab, None));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(save_name(&tab), "renamed.docx");
}

/// A dirty imported tab restores from its `.docx` sidecar, which says
/// nothing about the import: the session does. Restored, it still never
/// saves over the `.doc` and still shows Compatibility Mode; a converted
/// one comes back converted.
#[test]
fn a_dirty_imported_tab_survives_hot_exit() {
    let dir = Scratch::new();
    let path = write_doc(&dir, "Report.doc");
    let before = std::fs::read(&path).unwrap();
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    for convert in [false, true] {
        let mut tab = tab_from_path(&path);
        edit(&mut tab);
        if convert {
            convert_tab(&mut tab).unwrap();
        }
        let persisted = persist_tab(&hot, 0, &tab);
        let json = serde_json::to_vec(&Session {
            tabs: vec![persisted],
            ..Session::default()
        })
        .unwrap();
        let session: Session = serde_json::from_slice(&json).unwrap();
        let mut restored = restore_tab(&session.tabs[0]);
        assert!(restored.dirty);
        assert!(text_of(&restored).contains("Edited."));
        assert_eq!(
            restored.import,
            DocImport {
                binary_source: true,
                compat: !convert
            }
        );
        let caption = if convert {
            "Report.doc".to_string()
        } else {
            format!("Report.doc{COMPAT_SUFFIX}")
        };
        assert_eq!(restored.caption(), caption);
        assert_eq!(
            doc_save_target(restored.path.as_deref(), true, true),
            DocSaveTarget::RefuseHarness
        );
        assert!(!save_doc_tab(&mut restored, None));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

/// A session written before #634 has neither flag; a clean `.doc` tab
/// reloads its file, which says it is an import.
#[test]
fn an_old_session_reimports_its_doc() {
    let dir = Scratch::new();
    let path = write_doc(&dir, "Report.doc");
    let json = format!(
        r#"{{"tabs":[{{"kind":"Docx","title":"Report.doc","path":{}}}],"active":0}}"#,
        serde_json::to_string(&path.display().to_string()).unwrap()
    );
    let session: Session = serde_json::from_str(&json).unwrap();
    let restored = restore_tab(&session.tabs[0]);
    assert_eq!(restored.import, DocImport::IMPORTED);
    assert!(is_imported(&restored));
}

#[test]
fn binary_paths_and_the_open_filter() {
    for p in ["a.doc", "A.DOC", "t.dot", "dir.x/b.Doc"] {
        assert!(is_binary_doc_path(Path::new(p)), "{p}");
    }
    for p in ["a.docx", "a.docm", "doc", "a.doc.html", "a.md"] {
        assert!(!is_binary_doc_path(Path::new(p)), "{p}");
    }
    assert!(OPEN_EXTENSIONS.contains(&"doc"));
    assert!(OPEN_EXTENSIONS.contains(&"docx"));
}
