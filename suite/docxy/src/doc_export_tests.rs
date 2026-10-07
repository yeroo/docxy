//! Save As Plain Text, Rich Text and the Word file types (#635, #636),
//! without a window: what each writes, how the tab is rebound, what a later
//! in-place Save writes, and what a restart keeps.

use crate::open_mode::Converted;
use crate::open_mode_tests::Scratch;
use crate::{DocTab, Surface, persist_tab, restore_tab, save_doc_tab, tab_from_path};
use docxcore::package::{DocKind, load_package};
use docxcore::zip::ZipArchive;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../uiharness/fixtures")
            .join(name),
    )
    .unwrap()
}

/// `basic.docx` copied into `dir` as `name`, opened.
fn opened(dir: &Scratch, name: &str) -> (DocTab, PathBuf, Vec<u8>) {
    let path = dir.path(name);
    let bytes = fixture("basic.docx");
    std::fs::write(&path, &bytes).unwrap();
    let mut tab = tab_from_path(&path);
    assert!(matches!(tab.surface, Surface::Doc(_)), "{}", tab.status);
    // The fixture is one empty paragraph: give it words to write.
    type_text(&mut tab, "Hello caf\u{e9} {world}");
    (tab, path, bytes)
}

fn type_text(tab: &mut DocTab, text: &str) {
    let Surface::Doc(ed) = &mut tab.surface else {
        panic!("not a document")
    };
    for ch in text.chars() {
        ed.insert_char(ch);
    }
    tab.set_dirty();
}

fn body_text(tab: &DocTab) -> Vec<String> {
    match &tab.surface {
        Surface::Doc(ed) => docxcore::import::paragraph_texts(&ed.doc),
        _ => panic!("not a document"),
    }
}

fn content_type(bytes: &[u8]) -> String {
    load_package(bytes).unwrap().main_content_type().unwrap()
}

#[test]
fn save_as_plain_text_rebinds_and_saves_text_again() {
    let dir = Scratch::new();
    let (mut tab, source, original) = opened(&dir, "basic.docx");
    let txt = dir.path("basic.txt");
    assert!(save_doc_tab(&mut tab, Some(txt.clone())), "{}", tab.status);
    assert_eq!(tab.path.as_deref(), Some(txt.as_path()));
    assert_eq!(tab.title.as_ref(), "basic.txt");
    assert!(!tab.dirty);
    assert_eq!(
        std::fs::read(&source).unwrap(),
        original,
        "the .docx is untouched"
    );
    let written = std::fs::read(&txt).unwrap();
    let text = String::from_utf8(written.clone()).expect("UTF-8");
    assert!(!written.starts_with(&[0xEF, 0xBB, 0xBF]), "no BOM");
    assert!(!written.starts_with(b"PK"));
    assert!(text.ends_with("\r\n"), "{text:?}");
    assert!(!text.replace("\r\n", "").contains('\n'), "{text:?}");
    assert_eq!(text, "Hello caf\u{e9} {world}\r\n");
    // In place: plain text again, never a Word package under the .txt name.
    type_text(&mut tab, "Typed ");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let again = String::from_utf8(std::fs::read(&txt).unwrap()).unwrap();
    assert_eq!(again, "Hello caf\u{e9} {world}Typed \r\n");
    assert!(!tab.dirty);
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

#[test]
fn save_as_rich_text_rebinds_saves_rtf_again_and_reopens() {
    let dir = Scratch::new();
    let (mut tab, source, original) = opened(&dir, "basic.docx");
    let rtf = dir.path("basic.rtf");
    assert!(save_doc_tab(&mut tab, Some(rtf.clone())), "{}", tab.status);
    assert_eq!(tab.path.as_deref(), Some(rtf.as_path()));
    assert_eq!(tab.title.as_ref(), "basic.rtf");
    assert!(!tab.dirty);
    assert!(std::fs::read(&rtf).unwrap().starts_with(b"{\\rtf1"));
    type_text(&mut tab, "Typed ");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let bytes = std::fs::read(&rtf).unwrap();
    assert!(
        bytes.starts_with(b"{\\rtf1"),
        "in place wrote a Word package"
    );
    assert_eq!(std::fs::read(&source).unwrap(), original);
    // Opened again, it is that text (converted, as any RTF opens).
    let back = tab_from_path(&rtf);
    assert_eq!(back.access.converted, Some(Converted::Rtf));
    assert_eq!(body_text(&back), body_text(&tab));
}

#[test]
fn save_as_a_template_writes_the_template_type_and_keeps_it() {
    let dir = Scratch::new();
    let (mut tab, source, original) = opened(&dir, "basic.docx");
    let dotx = dir.path("basic.dotx");
    assert!(save_doc_tab(&mut tab, Some(dotx.clone())), "{}", tab.status);
    assert_eq!(tab.path.as_deref(), Some(dotx.as_path()));
    assert!(!tab.dirty);
    assert_eq!(
        content_type(&std::fs::read(&dotx).unwrap()),
        DocKind::Template.main_content_type()
    );
    // In place it stays a template, edited or not.
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert_eq!(
        content_type(&std::fs::read(&dotx).unwrap()),
        DocKind::Template.main_content_type()
    );
    type_text(&mut tab, "x");
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    assert_eq!(
        content_type(&std::fs::read(&dotx).unwrap()),
        DocKind::Template.main_content_type()
    );
    // And back to a document.
    let docx = dir.path("again.docx");
    assert!(save_doc_tab(&mut tab, Some(docx.clone())), "{}", tab.status);
    assert_eq!(
        content_type(&std::fs::read(&docx).unwrap()),
        DocKind::Document.main_content_type()
    );
    let dotm = dir.path("basic.dotm");
    assert!(save_doc_tab(&mut tab, Some(dotm.clone())), "{}", tab.status);
    assert_eq!(
        content_type(&std::fs::read(&dotm).unwrap()),
        DocKind::MacroTemplate.main_content_type()
    );
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

/// An unedited save of a file already of its type writes the original
/// bytes still (#1107): the retype is only for a type that disagrees.
#[test]
fn an_unedited_save_of_a_docx_is_still_its_own_bytes() {
    let dir = Scratch::new();
    let (_, source, original) = opened(&dir, "basic.docx");
    let mut tab = tab_from_path(&source);
    assert_eq!(
        content_type(&original),
        DocKind::Document.main_content_type()
    );
    tab.set_dirty();
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let saved = std::fs::read(&source).unwrap();
    let doc_xml = |b: &[u8]| {
        ZipArchive::open(b)
            .unwrap()
            .read("word/document.xml")
            .unwrap()
    };
    let types = |b: &[u8]| {
        ZipArchive::open(b)
            .unwrap()
            .read("[Content_Types].xml")
            .unwrap()
    };
    assert_eq!(doc_xml(&saved), doc_xml(&original));
    assert_eq!(types(&saved), types(&original));
}

/// A `.docm` with a VBA project: the fixture with the project's parts,
/// relationship and content types added.
fn macro_document(path: &Path) {
    let original = fixture("basic.docx");
    let zip = ZipArchive::open(&original).unwrap();
    let mut parts: Vec<(String, Vec<u8>)> = zip
        .entries()
        .iter()
        .map(|e| (e.name.clone(), zip.extract(e).unwrap()))
        .collect();
    for (name, bytes) in &mut parts {
        let text = String::from_utf8_lossy(bytes).into_owned();
        if name == "[Content_Types].xml" {
            *bytes = text
                .replace(
                    DocKind::Document.main_content_type(),
                    DocKind::MacroDocument.main_content_type(),
                )
                .replace(
                    "</Types>",
                    "<Default Extension=\"bin\" ContentType=\"application/vnd.ms-office.vbaProject\"/></Types>",
                )
                .into_bytes();
        } else if name == "word/_rels/document.xml.rels" {
            *bytes = text
                .replace(
                    "</Relationships>",
                    "<Relationship Id=\"rIdVba\" Type=\"http://schemas.microsoft.com/office/2006/relationships/vbaProject\" Target=\"vbaProject.bin\"/></Relationships>",
                )
                .into_bytes();
        }
    }
    parts.push(("word/vbaProject.bin".into(), b"VBA project".to_vec()));
    std::fs::write(path, docxcore::zipwrite::write_zip(&parts)).unwrap();
}

fn has_vba(bytes: &[u8]) -> bool {
    let zip = ZipArchive::open(bytes).unwrap();
    let rels = String::from_utf8(zip.read("word/_rels/document.xml.rels").unwrap()).unwrap();
    zip.find("word/vbaProject.bin").is_some() || rels.contains("vbaProject")
}

#[test]
fn a_macro_free_type_drops_the_macros_and_says_so() {
    let dir = Scratch::new();
    let source = dir.path("macros.docm");
    macro_document(&source);
    let mut tab = tab_from_path(&source);
    assert!(matches!(tab.surface, Surface::Doc(_)), "{}", tab.status);
    for (name, kind) in [
        ("plain.docx", DocKind::Document),
        ("plain.dotx", DocKind::Template),
    ] {
        let mut tab = tab_from_path(&source);
        let target = dir.path(name);
        assert!(
            save_doc_tab(&mut tab, Some(target.clone())),
            "{}",
            tab.status
        );
        assert!(
            tab.status.contains("macros were not saved"),
            "{}",
            tab.status
        );
        let bytes = std::fs::read(&target).unwrap();
        assert!(!has_vba(&bytes), "{name}");
        assert_eq!(content_type(&bytes), kind.main_content_type());
    }
    for (name, kind) in [
        ("kept.docm", DocKind::MacroDocument),
        ("kept.dotm", DocKind::MacroTemplate),
    ] {
        let target = dir.path(name);
        assert!(
            save_doc_tab(&mut tab, Some(target.clone())),
            "{}",
            tab.status
        );
        assert!(!tab.status.contains("macros"), "{}", tab.status);
        let bytes = std::fs::read(&target).unwrap();
        assert!(has_vba(&bytes), "{name}");
        assert_eq!(
            ZipArchive::open(&bytes)
                .unwrap()
                .read("word/vbaProject.bin")
                .unwrap(),
            b"VBA project"
        );
        assert_eq!(content_type(&bytes), kind.main_content_type());
    }
}

#[test]
fn a_failed_write_leaves_the_tab_where_it_was() {
    let dir = Scratch::new();
    let (mut tab, source, _) = opened(&dir, "basic.docx");
    tab.set_dirty();
    for name in ["missing/out.txt", "missing/out.rtf", "missing/out.dotx"] {
        assert!(!save_doc_tab(&mut tab, Some(dir.path(name))), "{name}");
        assert!(tab.status.starts_with("save failed"), "{}", tab.status);
        assert_eq!(tab.path.as_deref(), Some(source.as_path()));
        assert_eq!(tab.title.as_ref(), "basic.docx");
        assert!(tab.dirty);
    }
}

/// A tab converted from an RTF (#633) may be saved as Rich Text under
/// another name; its source is still never written, in place or over.
#[test]
fn a_converted_tab_saves_as_rtf_under_another_name() {
    let dir = Scratch::new();
    let src = dir.path("letter.rtf");
    let rtf: &[u8] = br"{\rtf1\ansi Dear {\b reader}.\par}";
    std::fs::write(&src, rtf).unwrap();
    let mut tab = tab_from_path(&src);
    assert_eq!(tab.access.converted, Some(Converted::Rtf));
    assert!(!save_doc_tab(&mut tab, Some(src.clone())));
    let other = dir.path("copy.rtf");
    assert!(
        save_doc_tab(&mut tab, Some(other.clone())),
        "{}",
        tab.status
    );
    assert_eq!(tab.access.converted, None);
    assert!(tab.pkg.is_some(), "the conversion's package is kept");
    assert_eq!(tab.path.as_deref(), Some(other.as_path()));
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let back = tab_from_path(&other);
    assert_eq!(body_text(&back), ["Dear reader."]);
    assert_eq!(std::fs::read(&src).unwrap(), rtf);
}

/// A restart restores a tab bound to a `.rtf` or `.txt` from its sidecar,
/// and its in-place Save still writes that format.
#[test]
fn after_a_restart_an_in_place_save_keeps_the_format() {
    for (name, magic) in [("out.rtf", &b"{\\rtf1"[..]), ("out.txt", &b""[..])] {
        let dir = Scratch::new();
        let (mut tab, _, _) = opened(&dir, "basic.docx");
        let target = dir.path(name);
        assert!(
            save_doc_tab(&mut tab, Some(target.clone())),
            "{}",
            tab.status
        );
        let hot = dir.path("hot");
        std::fs::create_dir_all(&hot).unwrap();
        let persisted = persist_tab(&hot, 0, &tab);
        assert!(persisted.hot.is_some(), "{name}");
        let mut back = restore_tab(&persisted);
        assert_eq!(back.path.as_deref(), Some(target.as_path()), "{name}");
        assert_eq!(back.access.converted, None, "{name}");
        std::fs::write(&target, b"stale").unwrap();
        assert!(save_doc_tab(&mut back, None), "{name}: {}", back.status);
        let bytes = std::fs::read(&target).unwrap();
        assert!(bytes.starts_with(magic), "{name}");
        assert!(!bytes.starts_with(b"PK"), "{name}");
        assert_ne!(bytes, b"stale", "{name}");
    }
}

/// With no sidecar (Don't Save on quit), the tab reopens the file itself:
/// a `.rtf` converted again, a `.txt` (which no open reads) as a load
/// error. Neither saves over its file in place, and nothing writes a Word
/// package under them.
#[test]
fn without_a_sidecar_the_file_reopens_and_is_not_written_over() {
    for name in ["out.rtf", "out.txt"] {
        let dir = Scratch::new();
        let (mut tab, _, _) = opened(&dir, "basic.docx");
        let target = dir.path(name);
        assert!(
            save_doc_tab(&mut tab, Some(target.clone())),
            "{}",
            tab.status
        );
        let on_disk = std::fs::read(&target).unwrap();
        let hot = dir.path("hot");
        std::fs::create_dir_all(&hot).unwrap();
        let mut persisted = persist_tab(&hot, 0, &tab);
        persisted.hot = None;
        persisted.dirty = false;
        let mut back = restore_tab(&persisted);
        crate::finish_pending_conversion(&mut back);
        if name.ends_with(".rtf") {
            assert_eq!(
                back.access.converted,
                Some(Converted::Rtf),
                "{}",
                back.status
            );
        } else {
            assert!(back.load_failed, "{}", back.status);
        }
        back.set_dirty();
        assert!(!save_doc_tab(&mut back, None), "{name}");
        assert_eq!(std::fs::read(&target).unwrap(), on_disk, "{name}");
    }
}

/// Save As Rich Text resolves the tab's own styles (FIX r1 #3): a run in
/// the document's bold character style comes back bold.
#[test]
fn rich_text_resolves_the_documents_styles() {
    use docxcore::model::{Block, Inline, ParProps, Paragraph, Run, RunProps};
    let dir = Scratch::new();
    let mut pkg = load_package(&fixture("basic.docx")).unwrap();
    let styles = pkg.part_text("word/styles.xml").unwrap();
    let styles = styles.replacen(
        "</w:styles>",
        "<w:style w:type=\"character\" w:styleId=\"Strong\"><w:name w:val=\"Strong\"/><w:rPr><w:b/></w:rPr></w:style></w:styles>",
        1,
    );
    assert!(pkg.set_part_text("word/styles.xml", &styles));
    pkg.document.body = vec![Block::Paragraph(Paragraph {
        props: ParProps::default(),
        content: vec![Inline::Run(Run {
            text: "strong".into(),
            props: RunProps {
                style_id: Some("Strong".into()),
                ..RunProps::default()
            },
        })],
    })];
    let source = dir.path("styled.docx");
    std::fs::write(&source, docxcore::package::save_package(&pkg)).unwrap();
    let mut tab = tab_from_path(&source);
    let rtf = dir.path("styled.rtf");
    assert!(save_doc_tab(&mut tab, Some(rtf.clone())), "{}", tab.status);
    let back = docxcore::import::rtf::import_rtf(&std::fs::read(&rtf).unwrap()).unwrap();
    let Some(Block::Paragraph(p)) = back.body.first() else {
        panic!("{:?}", back.body)
    };
    let Some(Inline::Run(r)) = p.content.first() else {
        panic!("{:?}", p.content)
    };
    assert_eq!(r.text, "strong");
    assert!(r.props.bold, "the Strong style's bold was dropped");
}

/// Plain Text and Rich Text write the final view, a deleted paragraph mark
/// joining two paragraphs, while the tab's document keeps the tracked
/// change (FIX r2 #1).
#[test]
fn text_and_rtf_write_the_final_view_and_leave_the_tab_alone() {
    let dir = Scratch::new();
    let mut pkg = load_package(&fixture("basic.docx")).unwrap();
    let xml = pkg.part_text("word/document.xml").unwrap();
    let body_start = xml.find("<w:body>").unwrap() + "<w:body>".len();
    let body_end = xml.find("</w:body>").unwrap();
    let body = "<w:p><w:pPr><w:rPr><w:del w:id=\"1\" w:author=\"A\" w:date=\"2026-10-07T00:00:00Z\"/></w:rPr></w:pPr><w:r><w:t>Hello</w:t></w:r></w:p><w:p><w:r><w:t xml:space=\"preserve\"> world</w:t></w:r></w:p>";
    let xml = format!("{}{body}{}", &xml[..body_start], &xml[body_end..]);
    assert!(pkg.set_part_text("word/document.xml", &xml));
    let source = dir.path("tracked.docx");
    std::fs::write(
        &source,
        docxcore::package::save_package_preserving_document(&pkg),
    )
    .unwrap();
    let mut tab = tab_from_path(&source);
    let before = body_text(&tab);
    assert_eq!(
        before,
        ["Hello", " world"],
        "the mark is tracked, not applied"
    );
    let txt = dir.path("tracked.txt");
    assert!(save_doc_tab(&mut tab, Some(txt.clone())), "{}", tab.status);
    assert_eq!(std::fs::read(&txt).unwrap(), b"Hello world\r\n");
    let rtf = dir.path("tracked.rtf");
    assert!(save_doc_tab(&mut tab, Some(rtf.clone())), "{}", tab.status);
    let back = docxcore::import::rtf::import_rtf(&std::fs::read(&rtf).unwrap()).unwrap();
    assert_eq!(docxcore::import::paragraph_texts(&back), ["Hello world"]);
    assert_eq!(
        body_text(&tab),
        before,
        "the editor's document is unchanged"
    );
}
