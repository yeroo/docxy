//! Word templates (#636), without a window: the personal templates folder,
//! what File > New lists, and the untitled document a template opens as.

use crate::doc_templates::{
    Platform, doc_template_title, personal_templates, template_label, template_tab,
    templates_dir_from,
};
use crate::open_mode::OpenMode;
use crate::open_mode_tests::Scratch;
use crate::trusted::TrustStore;
use crate::{Surface, doc_save_as_name, save_doc_tab, tab_from_path, tab_from_path_mode};
use docxcore::package::{DocKind, load_package, save_package};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn basic() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"))
        .unwrap()
}

/// A template of `kind` in `dir` named `name`, made from `basic.docx`.
fn template(dir: &Scratch, name: &str, kind: DocKind) -> PathBuf {
    let mut pkg = load_package(&basic()).unwrap();
    pkg.document = docxcore::markdown::from_markdown("# Letter\n\nDear Sir,\n");
    pkg.set_main_kind(kind);
    let path = dir.path(name);
    std::fs::write(&path, save_package(&pkg)).unwrap();
    path
}

fn content_type(path: &Path) -> String {
    load_package(&std::fs::read(path).unwrap())
        .unwrap()
        .main_content_type()
        .unwrap()
}

#[test]
fn the_override_moves_the_templates_folder_on_every_os() {
    let docs = Some(PathBuf::from("/home/u/Documents"));
    let home = Some(PathBuf::from("/home/u"));
    let config = Some(PathBuf::from("/home/u/.config"));
    for os in [Platform::Windows, Platform::Mac, Platform::Other] {
        assert_eq!(
            templates_dir_from(
                Some(OsString::from("/tmp/run")),
                docs.clone(),
                home.clone(),
                config.clone(),
                os
            ),
            Path::new("/tmp/run/docxy/templates"),
            "{os:?}"
        );
    }
}

#[test]
fn the_templates_folder_is_words_own() {
    let docs = Some(PathBuf::from("/u/Documents"));
    let home = Some(PathBuf::from("/u"));
    let config = Some(PathBuf::from("/u/.config"));
    let at = |over: Option<&str>, os| {
        templates_dir_from(
            over.map(OsString::from),
            docs.clone(),
            home.clone(),
            config.clone(),
            os,
        )
    };
    assert_eq!(
        at(None, Platform::Windows),
        Path::new("/u/Documents/Custom Office Templates")
    );
    assert_eq!(
        at(None, Platform::Mac),
        Path::new("/u/Library/Group Containers/UBF8T346G9.Office/User Content/Templates")
    );
    assert_eq!(
        at(None, Platform::Other),
        Path::new("/u/.config/docxy/templates")
    );
    // An exported-but-empty override is no override.
    assert_eq!(
        at(Some(""), Platform::Windows),
        Path::new("/u/Documents/Custom Office Templates")
    );
    // An OS folder that is not known falls back to the app's own.
    assert_eq!(
        templates_dir_from(None, None, None, config.clone(), Platform::Windows),
        Path::new("/u/.config/docxy/templates")
    );
}

#[test]
fn new_lists_the_templates_in_the_folder_without_making_it() {
    let dir = Scratch::new();
    let missing = dir.path("Templates");
    assert!(personal_templates(&missing).is_empty());
    assert!(!missing.exists(), "listing made the folder");
    std::fs::create_dir_all(&missing).unwrap();
    for name in [
        "memo.DOTX",
        "Letter.dotx",
        "budget.dotm",
        "notes.docx",
        "old.dot",
    ] {
        std::fs::write(missing.join(name), b"x").unwrap();
    }
    std::fs::create_dir_all(missing.join("folder.dotx")).unwrap();
    let names: Vec<String> = personal_templates(&missing)
        .iter()
        .map(|p| template_label(p))
        .collect();
    assert_eq!(names, ["budget", "Letter", "memo"]);
}

#[test]
fn a_template_titles_the_first_free_new_document() {
    let dir = Scratch::new();
    let letter = dir.path("Letter.dotx");
    assert_eq!(doc_template_title(&letter).as_deref(), Some("Letter1.docx"));
    std::fs::write(dir.path("Letter1.docx"), b"x").unwrap();
    assert_eq!(doc_template_title(&letter).as_deref(), Some("Letter2.docx"));
    assert_eq!(
        doc_template_title(&dir.path("Macro.DOTM")).as_deref(),
        Some("Macro1.docm")
    );
    for not in ["Letter.docx", "Letter.docm", "Letter.dot", "Letter"] {
        assert_eq!(doc_template_title(&dir.path(not)), None, "{not}");
    }
}

/// Opening a template, by the command line, the Open dialog or File > New,
/// gives a new, untitled document with its content: the first Save asks for
/// a name and the template is never written.
#[test]
fn a_template_opens_as_a_new_untitled_document() {
    let dir = Scratch::new();
    let path = template(&dir, "Letter.dotx", DocKind::Template);
    let before = std::fs::read(&path).unwrap();
    for mut tab in [
        tab_from_path(&path),
        tab_from_path_mode(&path, OpenMode::Normal, &TrustStore::default()).unwrap(),
    ] {
        assert!(matches!(tab.surface, Surface::Doc(_)), "{}", tab.status);
        assert_eq!(tab.path, None);
        assert_eq!(tab.title.as_ref(), "Letter1.docx");
        assert!(!tab.dirty);
        assert!(
            tab.status
                .contains("new document from template Letter.dotx"),
            "{}",
            tab.status
        );
        assert!(tab.pkg.is_some(), "the template's package and styles");
        let Surface::Doc(ed) = &tab.surface else {
            unreachable!()
        };
        let template_doc = load_package(&before).unwrap().document;
        assert_eq!(
            docxcore::import::paragraph_texts(&ed.doc),
            docxcore::import::paragraph_texts(&template_doc)
        );
        tab.set_dirty();
        assert!(!save_doc_tab(&mut tab, None));
        assert!(tab.status.contains("never been saved"), "{}", tab.status);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

/// AC5: the new document saved as `.docx` is a document, not a template.
#[test]
fn a_document_from_a_template_saves_as_a_document() {
    let dir = Scratch::new();
    let path = template(&dir, "Letter.dotx", DocKind::Template);
    let mut tab = tab_from_path(&path);
    let out = dir.path("Mine.docx");
    assert!(save_doc_tab(&mut tab, Some(out.clone())), "{}", tab.status);
    assert_eq!(content_type(&out), DocKind::Document.main_content_type());
    assert_eq!(content_type(&path), DocKind::Template.main_content_type());
}

#[test]
fn a_macro_template_suggests_a_macro_document() {
    let dir = Scratch::new();
    let path = template(&dir, "Macro.dotm", DocKind::MacroTemplate);
    let tab = tab_from_path(&path);
    assert_eq!(tab.title.as_ref(), "Macro1.docm");
    assert!(
        doc_save_as_name(&tab).ends_with(".docm"),
        "{}",
        doc_save_as_name(&tab)
    );
    // A plain template's document suggests a .docx.
    let tab = tab_from_path(&template(&dir, "Plain.dotx", DocKind::Template));
    assert!(
        doc_save_as_name(&tab).ends_with(".docx"),
        "{}",
        doc_save_as_name(&tab)
    );
}

/// A template that will not load stays bound to its file, whose load-failed
/// guard keeps Save off it; Recover Text keeps its file too.
#[test]
fn a_broken_template_or_recovered_text_keeps_its_file() {
    let dir = Scratch::new();
    let broken = dir.path("broken.dotx");
    std::fs::write(&broken, b"not a zip").unwrap();
    let tab = tab_from_path(&broken);
    assert_eq!(tab.path.as_deref(), Some(broken.as_path()));
    let path = template(&dir, "Letter.dotx", DocKind::Template);
    let tab = tab_from_path_mode(&path, OpenMode::RecoverText, &TrustStore::default()).unwrap();
    assert_eq!(tab.path.as_deref(), Some(path.as_path()));
}

/// An empty `.dotx` holds no Word package: it never becomes an untitled
/// document, and File > New refuses it in words (FIX r3 #6). Nor does a
/// file that is not a template, or one that will not load.
#[test]
fn file_new_refuses_what_is_no_loadable_template() {
    let dir = Scratch::new();
    let empty = dir.path("Empty.dotx");
    std::fs::write(&empty, b"").unwrap();
    let tab = tab_from_path(&empty);
    assert_eq!(tab.path.as_deref(), Some(empty.as_path()), "{}", tab.status);
    let trusted = TrustStore::default();
    let refusal = |path: &Path| match template_tab(path, &trusted) {
        Err(e) => e,
        Ok(tab) => panic!("{} opened: {}", path.display(), tab.status),
    };
    let err = refusal(&empty);
    assert_eq!(
        err,
        "cannot open the template \"Empty.dotx\": it holds no Word document"
    );
    let broken = dir.path("Broken.dotx");
    std::fs::write(&broken, b"not a zip").unwrap();
    assert!(refusal(&broken).starts_with("cannot open the template \"Broken.dotx\": "),);
    let plain = template(&dir, "Plain.docx", DocKind::Document);
    assert!(refusal(&plain).contains("is not a Word template"));
    let good = template(&dir, "Good.dotx", DocKind::Template);
    let tab = template_tab(&good, &trusted).unwrap();
    assert_eq!((tab.path, tab.title.as_ref()), (None, "Good1.docx"));
}
