//! Protected View for a downloaded document (#633), without a window: the
//! open, the save gate, the backstop and the session. The key, ribbon and
//! pointer gates are driven through the app in `uiharness/tests`.

use crate::open_mode::{Converted, OpenMode, PROTECTED_STATUS};
use crate::open_mode_tests::Scratch;
#[cfg(windows)]
use crate::open_mode_tests::mark_downloaded;
#[cfg(windows)]
use crate::trusted::Stamp;
use crate::trusted::TrustStore;
use crate::{
    Act, DocTab, Surface, persist_tab, protected_view_allows_doc_act, restore_tab, save_doc_tab,
    tab_from_path, tab_from_path_mode,
};
use std::path::{Path, PathBuf};

fn basic_docx(dir: &Scratch, name: &str) -> PathBuf {
    let path = dir.path(name);
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
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

/// A document tab put in Protected View by hand (no alternate data streams
/// needed), as `tab_from_path_mode` puts a downloaded one.
fn protected(path: &Path) -> DocTab {
    let mut tab = tab_from_path(&path.to_path_buf());
    tab.access.protected = true;
    tab
}

/// Type `s` into the body the way an edit that slipped past every gate would.
fn leak_edit(tab: &mut DocTab, s: &str) {
    let Surface::Doc(ed) = &mut tab.surface else {
        panic!("not a document")
    };
    ed.insert_str(s);
}

#[test]
fn a_protected_document_saves_nothing() {
    let dir = Scratch::new();
    let src = basic_docx(&dir, "basic.docx");
    let before = std::fs::read(&src).unwrap();
    let mut tab = protected(&src);
    let other = dir.path("copy.docx");
    for target in [None, Some(src.clone()), Some(other.clone())] {
        assert!(!save_doc_tab(&mut tab, target));
        assert_eq!(tab.status.as_ref(), PROTECTED_STATUS);
    }
    assert_eq!(std::fs::read(&src).unwrap(), before);
    assert!(!other.exists());
}

#[test]
fn a_leaked_document_edit_is_rolled_back_from_the_file() {
    let dir = Scratch::new();
    let src = basic_docx(&dir, "basic.docx");
    let mut tab = protected(&src);
    let original = text(&tab);
    leak_edit(&mut tab, "LEAKED ");
    assert_ne!(text(&tab), original);
    tab.mark_dirty();
    assert!(!tab.dirty, "the tab stays clean");
    assert_eq!(text(&tab), original);
    assert_eq!(tab.status.as_ref(), PROTECTED_STATUS);
    assert!(tab.access.protected);
    // An unprotected tab just turns dirty.
    let mut open = tab_from_path(&src);
    leak_edit(&mut open, "x");
    open.mark_dirty();
    assert!(open.dirty);
}

#[test]
fn a_converted_protected_tab_rolls_back_converted() {
    let dir = Scratch::new();
    let src = dir.path("letter.rtf");
    std::fs::write(&src, br"{\rtf1 Hello rtf\par}").unwrap();
    let mut tab = protected(&src);
    leak_edit(&mut tab, "LEAKED ");
    tab.mark_dirty();
    assert_eq!(tab.access.converted, Some(Converted::Rtf));
    assert_eq!(text(&tab), "Hello rtf");
    // A Recover Text tab comes back as recovered text, not as a load error.
    let blob = dir.path("blob.bin");
    std::fs::write(&blob, b"\x00\x01Readable text\x00").unwrap();
    let mut tab = tab_from_path_mode(&blob, OpenMode::RecoverText, &TrustStore::default()).unwrap();
    tab.access.protected = true;
    leak_edit(&mut tab, "LEAKED ");
    tab.mark_dirty();
    assert_eq!(tab.access.converted, Some(Converted::RecoveredText));
    assert_eq!(text(&tab), "Readable text");
}

#[test]
fn only_commands_that_look_pass_protected_view() {
    for act in [
        Act::Copy,
        Act::SelectAll,
        Act::Find,
        Act::ShowHide,
        Act::ToggleNav,
        Act::Markup(docxcore::markup::MarkupView::NoMarkup),
    ] {
        assert!(protected_view_allows_doc_act(act), "{act:?}");
    }
    for act in [
        Act::Cut,
        Act::Paste,
        Act::Bold,
        Act::H1,
        Act::Bullets,
        Act::NewComment,
        Act::FontColor,
        Act::InsertTable,
        Act::PageBreak,
        Act::ClearFmt,
        Act::ResolveComment,
        Act::DeleteAllComments,
        Act::ToggleTrack,
    ] {
        assert!(!protected_view_allows_doc_act(act), "{act:?}");
    }
}

#[test]
fn a_protected_document_comes_back_protected_from_the_session() {
    let dir = Scratch::new();
    let src = basic_docx(&dir, "basic.docx");
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    let tab = protected(&src);
    let persisted = persist_tab(&hot, 0, 0, &tab);
    assert!(persisted.protected);
    let back = restore_tab(&persisted);
    assert!(back.access.protected);
    assert_eq!(back.caption(), "basic.docx [Protected View]");
}

#[cfg(windows)]
#[test]
fn a_downloaded_document_opens_protected_unless_trusted() {
    let dir = Scratch::new();
    let src = basic_docx(&dir, "basic.docx");
    let rtf = dir.path("letter.rtf");
    std::fs::write(&rtf, br"{\rtf1 Hello\par}").unwrap();
    let md = dir.path("notes.md");
    std::fs::write(&md, "# Notes\n").unwrap();
    for path in [&src, &rtf, &md] {
        if mark_downloaded(path).is_none() {
            return;
        }
        let tab = tab_from_path_mode(path, OpenMode::Normal, &TrustStore::default()).unwrap();
        assert!(tab.access.protected, "{}", path.display());
        assert!(tab.access.stamp.is_some());
        assert!(
            tab.caption().ends_with(" [Protected View]"),
            "{}",
            tab.caption()
        );
        assert!(!tab.load_failed, "{}", tab.status);
    }
    // Trusted as it is now (Enable Editing, #882): it opens editable.
    let mut trusted = TrustStore::default();
    trusted.trust(&src, Stamp::of(&src).unwrap());
    let tab = tab_from_path_mode(&src, OpenMode::Normal, &trusted).unwrap();
    assert!(!tab.access.protected);
    // The same document without the mark opens editable.
    let local = basic_docx(&dir, "local.docx");
    let tab = tab_from_path_mode(&local, OpenMode::Normal, &TrustStore::default()).unwrap();
    assert!(!tab.access.protected);
}

/// FIX r5 m2: the rollback's reload is the open's own dispatch, so a 0-byte
/// document (a new one, as Explorer makes) comes back as one, not as a file
/// that failed to load.
#[test]
fn rolling_back_an_empty_document_keeps_it_loadable() {
    let dir = Scratch::new();
    let src = dir.path("new.docx");
    std::fs::write(&src, b"").unwrap();
    let mut tab = protected(&src);
    assert!(!tab.load_failed, "{}", tab.status);
    leak_edit(&mut tab, "x");
    tab.mark_dirty();
    assert!(!tab.load_failed, "{}", tab.status);
    assert!(!tab.dirty);
}
