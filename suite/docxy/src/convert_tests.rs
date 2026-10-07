//! Opening RTF, Web Pages, PDFs and damaged documents (#633), without a
//! window: the format by content, the converted tab, its save gate, Recover
//! Text from Any File and the session.

use crate::convert_child::{self, What};
use crate::open_mode::{Access, Converted, OpenMode, converted_refusal};
use crate::open_mode_tests::Scratch;
use crate::trusted::TrustStore;
use crate::{
    DocTab, Kind, Surface, convert_doc_with, doc_save_as_name, doc_target_allowed,
    finish_pending_conversion, persist_tab, restore_tab, save_doc_tab, tab_from_path,
    tab_from_path_mode,
};
use docxcore::model::Block;
use std::path::{Path, PathBuf};

fn texts(tab: &DocTab) -> Vec<String> {
    match &tab.surface {
        Surface::Doc(ed) => docxcore::import::paragraph_texts(&ed.doc),
        _ => panic!("not a document tab"),
    }
}

fn write(dir: &Scratch, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

const RTF: &[u8] = br"{\rtf1\ansi\ansicpg1252{\stylesheet{\s0 Normal;}{\s1 heading 1;}}\pard\s1 Letter\par\pard Dear {\b reader}, caf\'e9.\par}";

/// A docx on disk with `n` paragraphs, its parts stored, so cutting it in
/// half cuts into `word/document.xml`.
fn long_docx(dir: &Scratch, name: &str, n: usize) -> PathBuf {
    let md: String = (1..=n)
        .map(|i| format!("Paragraph number {i} of the long document.\n\n"))
        .collect();
    let doc = docxcore::markdown::from_markdown(&md);
    let bytes = docxcore::package::save_package(&docxcore::package::new_package(doc));
    write(dir, name, &bytes)
}

#[test]
fn rtf_named_docx_opens_converted_from_rtf() {
    let dir = Scratch::new();
    let path = write(&dir, "letter.docx", RTF);
    let tab = tab_from_path(&path);
    assert_eq!(tab.status.as_ref(), "loaded (converted from RTF)");
    assert_eq!(tab.access.converted, Some(Converted::Rtf));
    assert!(!tab.load_failed);
    assert_eq!(texts(&tab), ["Letter", "Dear reader, caf\u{e9}."]);
    assert_eq!(tab.path.as_deref(), Some(path.as_path()), "keeps its path");
}

#[test]
fn a_word_web_page_in_windows_1252_opens_converted_from_html() {
    let dir = Scratch::new();
    let page = b"<html><head><meta http-equiv=Content-Type content=\"text/html; charset=windows-1252\"></head><body><h1>Title</h1><p class=MsoNormal>Caf\xe9 \x80</p></body></html>";
    let path = write(&dir, "page.htm", page);
    let tab = tab_from_path(&path);
    assert_eq!(tab.status.as_ref(), "loaded (converted from HTML)");
    assert_eq!(tab.access.converted, Some(Converted::Html));
    assert!(tab.bundle_html.is_none());
    assert_eq!(texts(&tab), ["Title", "Caf\u{e9} \u{20ac}"]);
}

#[test]
fn a_docxy_bundle_still_opens_as_a_bundle() {
    let dir = Scratch::new();
    let docx = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
    )
    .unwrap();
    let page = htmlbundle::wrap(
        &htmlbundle::docx_assets(),
        b"\0asm stand-in",
        "docx",
        "basic.docx",
        &docx,
        "test",
        "2026-10-02T00:00:00Z",
    )
    .unwrap();
    // Found by its content, whatever it is called.
    for name in ["basic.docx.html", "basic.htm"] {
        let tab = tab_from_path(&write(&dir, name, page.as_bytes()));
        assert!(
            tab.status.starts_with("loaded (editable HTML)"),
            "{}",
            tab.status
        );
        assert!(tab.bundle_html.is_some());
        assert_eq!(tab.access.converted, None);
    }
}

#[test]
fn a_pdf_opens_converted_from_pdf() {
    let dir = Scratch::new();
    let doc = docxcore::markdown::from_markdown("Hello from a PDF.\n\nSecond paragraph.\n");
    let pdf = docxcore::export::to_pdf(&doc, &docxcore::export::PdfOptions::default());
    let tab = tab_from_path(&write(&dir, "report.pdf", &pdf));
    assert_eq!(tab.status.as_ref(), "loaded (converted from PDF)");
    assert_eq!(tab.access.converted, Some(Converted::Pdf));
    let text = texts(&tab).join("\n");
    assert!(text.contains("Hello from a PDF."), "{text}");
    // A PDF with no text says so, and is a failed load.
    let empty = docxcore::export::to_pdf(
        &docxcore::markdown::from_markdown(""),
        &docxcore::export::PdfOptions::default(),
    );
    let tab = tab_from_path(&write(&dir, "blank.pdf", &empty));
    assert!(tab.load_failed, "{}", tab.status);
    assert!(tab.status.starts_with("load error"), "{}", tab.status);
}

#[test]
fn a_word_97_document_says_it_is_not_supported() {
    let dir = Scratch::new();
    let mut cfb = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    cfb.resize(512, 0);
    let tab = tab_from_path(&write(&dir, "old.doc", &cfb));
    assert!(tab.load_failed);
    assert!(tab.status.contains("Word 97-2003"), "{}", tab.status);
}

#[test]
fn a_converted_tab_never_writes_its_source() {
    let dir = Scratch::new();
    let src = write(&dir, "letter.rtf", RTF);
    let mut tab = tab_from_path(&src);
    tab.dirty = true;
    assert!(tab.access.save_needs_dialog());
    // In place, and Save As onto the source, are refused.
    for target in [None, Some(src.clone())] {
        assert!(!save_doc_tab(&mut tab, target));
        assert_eq!(tab.status.as_ref(), converted_refusal("letter.rtf"));
    }
    assert_eq!(std::fs::read(&src).unwrap(), RTF);
    // Nor any name no format is written under.
    assert!(!save_doc_tab(&mut tab, Some(dir.path("other.pdf"))));
    assert!(
        tab.status.contains("cannot save a document as"),
        "{}",
        tab.status
    );
    assert!(!dir.path("other.pdf").exists());
    // Save As suggests the same name as a Word document.
    assert_eq!(doc_save_as_name(&tab), "letter.docx");
    // Saved as .docx, it is that document from then on.
    let docx = dir.path("letter.docx");
    assert!(save_doc_tab(&mut tab, Some(docx.clone())), "{}", tab.status);
    assert_eq!(tab.access.converted, None);
    assert_eq!(tab.path.as_deref(), Some(docx.as_path()));
    assert!(tab.pkg.is_some());
    assert!(save_doc_tab(&mut tab, None), "in place: {}", tab.status);
    assert_eq!(std::fs::read(&src).unwrap(), RTF, "the RTF is untouched");
}

#[test]
fn a_converted_heading_keeps_its_style_through_save() {
    let dir = Scratch::new();
    let mut tab = tab_from_path(&write(&dir, "letter.rtf", RTF));
    let docx = dir.path("letter.docx");
    assert!(save_doc_tab(&mut tab, Some(docx.clone())), "{}", tab.status);
    // And again in place, from the package the first save wrote.
    assert!(save_doc_tab(&mut tab, None), "{}", tab.status);
    let pkg = docxcore::package::load_package(&std::fs::read(&docx).unwrap()).unwrap();
    let styles = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
    assert!(styles.contains("w:styleId=\"Heading1\""), "{styles}");
    let Some(Block::Paragraph(p)) = pkg.document.body.first() else {
        panic!("{:?}", pkg.document.body)
    };
    assert_eq!(p.props.style_id.as_deref(), Some("Heading1"));
}

#[test]
fn only_names_a_format_is_written_under_are_save_targets() {
    for ok in [
        "a.docx",
        "a.DOCX",
        "a.docm",
        "a.dotx",
        "a.dotm",
        "a.md",
        "a.markdown",
        "a.mdown",
        "a.htm",
        "a.html",
        // Plain Text and Rich Text (#635).
        "a.rtf",
        "a.TXT",
    ] {
        assert!(doc_target_allowed(Path::new(ok)), "{ok}");
    }
    for bad in ["a.pdf", "a.doc", "a.dot", "a.odt", "noext"] {
        assert!(!doc_target_allowed(Path::new(bad)), "{bad}");
    }
}

#[test]
fn a_truncated_docx_opens_as_its_recovered_text() {
    let dir = Scratch::new();
    let whole = long_docx(&dir, "whole.docx", 60);
    let full = texts(&tab_from_path(&whole));
    let bytes = std::fs::read(&whole).unwrap();
    let cut = write(&dir, "cut.docx", &bytes[..bytes.len() / 2]);
    let mut tab = tab_from_path(&cut);
    assert!(
        tab.status
            .starts_with("recovered text from a damaged file ("),
        "{}",
        tab.status
    );
    assert_eq!(tab.access.converted, Some(Converted::Recovered));
    assert!(!tab.load_failed);
    let got = texts(&tab);
    assert!(got.len() > 5 && got.len() < full.len(), "{}", got.len());
    let (last, whole_paras) = got.split_last().unwrap();
    assert_eq!(whole_paras, &full[..whole_paras.len()]);
    assert!(full[whole_paras.len()].starts_with(last.as_str()));
    // Never saved over the damaged file.
    tab.dirty = true;
    assert!(!save_doc_tab(&mut tab, None));
    assert_eq!(std::fs::read(&cut).unwrap(), &bytes[..bytes.len() / 2]);
}

#[test]
fn nothing_recoverable_keeps_the_load_error() {
    let dir = Scratch::new();
    let tab = tab_from_path(&write(&dir, "junk.docx", b"not a zip at all"));
    assert!(tab.load_failed);
    assert!(tab.status.starts_with("load error"), "{}", tab.status);
    assert_eq!(tab.access.converted, None);
}

#[test]
fn recover_text_from_any_file() {
    let dir = Scratch::new();
    assert_eq!(OpenMode::parse("recover-text"), Some(OpenMode::RecoverText));
    let mut bin = vec![0u8, 1, 2, 3];
    bin.extend(b"Readable words here\x00\x01");
    for u in "Wide text \u{416}".encode_utf16() {
        bin.extend(u.to_le_bytes());
    }
    let path = write(&dir, "blob.bin", &bin);
    let tab = tab_from_path_mode(&path, OpenMode::RecoverText, &TrustStore::default()).unwrap();
    assert_eq!(tab.status.as_ref(), "recovered text (2 paragraphs)");
    assert_eq!(tab.access.converted, Some(Converted::RecoveredText));
    assert_eq!(texts(&tab), ["Readable words here", "Wide text \u{416}"]);
    assert!(tab.access.opened_as(OpenMode::RecoverText));
    assert!(!tab.access.opened_as(OpenMode::Normal));
    // A whole .docx through Recover Text gives its text, converted.
    let docx = long_docx(&dir, "fine.docx", 3);
    let tab = tab_from_path_mode(&docx, OpenMode::RecoverText, &TrustStore::default()).unwrap();
    assert_eq!(tab.access.converted, Some(Converted::RecoveredText));
    assert_eq!(texts(&tab).len(), 3);
    // A workbook ignores it.
    let mut pkg = gridcore::xlsx::new_xlsx();
    pkg.workbook.sheets[0].set_cell(0, 0, gridcore::sheet::Cell::number(1.0));
    let book = write(&dir, "book.xlsx", &gridcore::xlsx::save_xlsx(&pkg));
    let tab = tab_from_path_mode(&book, OpenMode::RecoverText, &TrustStore::default()).unwrap();
    assert!(matches!(tab.surface, Surface::Sheet(_)));
    assert_eq!(tab.access, Access::default());
}

#[test]
fn a_converted_tab_comes_back_converted_from_the_session() {
    let dir = Scratch::new();
    let src = write(&dir, "letter.rtf", RTF);
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    // Dirty: restored from its .docx sidecar, still converted.
    let mut tab = tab_from_path(&src);
    tab.dirty = true;
    let persisted = persist_tab(&hot, 0, &tab);
    assert_eq!(persisted.converted, Some(Converted::Rtf));
    let mut back = restore_tab(&persisted);
    assert_eq!(back.access.converted, Some(Converted::Rtf));
    assert!(back.dirty);
    assert!(!save_doc_tab(&mut back, None));
    assert_eq!(std::fs::read(&src).unwrap(), RTF);
    // Clean, with no sidecar: converted again from the file, when its tab
    // is first in front (FIX r4 M2), not at restore.
    let mut clean = persist_tab(&hot, 1, &tab_from_path(&src));
    clean.hot = None;
    clean.dirty = false;
    let mut back = restore_tab(&clean);
    assert_eq!(back.access.converted, Some(Converted::Rtf));
    assert!(back.pending_conversion);
    finish_pending_conversion(&mut back);
    assert!(!back.pending_conversion);
    assert_eq!(texts(&back), ["Letter", "Dear reader, caf\u{e9}."]);
    // A Recover Text tab restores as recovered text.
    let blob = write(&dir, "blob.bin", b"\x00\x01Some readable text\x00");
    let rt = tab_from_path_mode(&blob, OpenMode::RecoverText, &TrustStore::default()).unwrap();
    let mut p = persist_tab(&hot, 2, &rt);
    p.hot = None;
    let mut back = restore_tab(&p);
    assert_eq!(back.access.converted, Some(Converted::RecoveredText));
    finish_pending_conversion(&mut back);
    assert_eq!(texts(&back), ["Some readable text"]);
}

#[test]
fn an_old_session_of_a_damaged_file_still_refuses_save_over_it() {
    // A session from before #209/#633 records neither the load failure nor
    // the conversion: the file is asked, and one that only opens recovered
    // is not written over.
    let dir = Scratch::new();
    let whole = long_docx(&dir, "whole.docx", 40);
    let bytes = std::fs::read(&whole).unwrap();
    let cut = write(&dir, "cut.docx", &bytes[..bytes.len() / 2]);
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    let mut tab = tab_from_path(&cut);
    tab.dirty = true;
    let mut p = persist_tab(&hot, 0, &tab);
    p.load_failed = None;
    p.converted = None;
    let mut back = restore_tab(&p);
    assert!(back.load_failed, "{}", back.status);
    assert!(!save_doc_tab(&mut back, None));
    assert_eq!(std::fs::read(&cut).unwrap(), &bytes[..bytes.len() / 2]);
}

/// FIX r1 M3, r4 M1: a converted tab has the package its conversion wrote
/// (the Markdown package, which defines the heading styles), so attaching
/// a recipient list keeps that package and Save As keeps `Heading1`
/// defined.
#[test]
fn a_converted_tab_given_a_mail_merge_package_keeps_its_heading_style() {
    let dir = Scratch::new();
    let mut tab = tab_from_path(&write(&dir, "letter.rtf", RTF));
    assert!(tab.pkg.is_some(), "the conversion's package");
    let list = write(&dir, "people.csv", b"Name,City\nAda,London\n");
    crate::mailings_tab::attach(&mut tab, &list).unwrap();
    assert!(tab.pkg.is_some());
    let docx = dir.path("letter.docx");
    assert!(save_doc_tab(&mut tab, Some(docx.clone())), "{}", tab.status);
    let pkg = docxcore::package::load_package(&std::fs::read(&docx).unwrap()).unwrap();
    let styles = String::from_utf8_lossy(pkg.part("word/styles.xml").unwrap()).into_owned();
    assert!(styles.contains("w:styleId=\"Heading1\""), "{styles}");
}

/// FIX r4 M1: `convert_doc` through the real converting child (this test
/// binary run as `docxy --convert-import`'s twin) builds the tab every open
/// builds: converted, its status, the conversion's package, and Save never
/// over the source. Recovery of a truncated .docx goes the same way.
#[test]
fn convert_doc_through_the_real_child_builds_the_production_tab() {
    let dir = Scratch::new();
    let rtf = write(&dir, "letter.rtf", RTF);
    let l = convert_doc_with(
        &rtf,
        RTF,
        What::Rtf,
        convert_child::tests::test_child_runner,
    )
    .unwrap();
    let mut tab = l.into_tab(Kind::Docx, "letter.rtf".into(), Some(rtf.clone()), false);
    assert_eq!(tab.access.converted, Some(Converted::Rtf));
    assert_eq!(tab.status.as_ref(), "loaded (converted from RTF)");
    assert!(tab.pkg.is_some());
    assert!(tab.converted_docx.is_some());
    assert_eq!(texts(&tab), ["Letter", "Dear reader, caf\u{e9}."]);
    tab.dirty = true;
    assert!(!save_doc_tab(&mut tab, None));
    assert!(!save_doc_tab(&mut tab, Some(rtf.clone())));
    assert_eq!(std::fs::read(&rtf).unwrap(), RTF);

    let whole = long_docx(&dir, "whole.docx", 60);
    let bytes = std::fs::read(&whole).unwrap();
    let cut = write(&dir, "cut.docx", &bytes[..bytes.len() / 2]);
    let cut_bytes = std::fs::read(&cut).unwrap();
    let l = convert_doc_with(
        &cut,
        &cut_bytes,
        What::Recover,
        convert_child::tests::test_child_runner,
    )
    .unwrap();
    let mut tab = l.into_tab(Kind::Docx, "cut.docx".into(), Some(cut.clone()), false);
    assert_eq!(tab.access.converted, Some(Converted::Recovered));
    assert!(
        tab.status
            .starts_with("recovered text from a damaged file ("),
        "{}",
        tab.status
    );
    assert!(tab.pkg.is_some());
    tab.dirty = true;
    assert!(!save_doc_tab(&mut tab, None));
    assert_eq!(std::fs::read(&cut).unwrap(), cut_bytes);
    // Nothing to recover: no tab from the child either (the caller keeps
    // its own load error).
    let junk = write(&dir, "junk.docx", b"not a zip");
    assert!(
        convert_doc_with(
            &junk,
            b"not a zip",
            What::Recover,
            convert_child::tests::test_child_runner
        )
        .is_none()
    );
}

/// FIX r4 M1: the in-process runner (unit tests) builds the same tab shape
/// as the child: from the converted .docx, with its package.
#[test]
fn the_in_process_runner_builds_the_same_tab() {
    let dir = Scratch::new();
    let rtf = write(&dir, "letter.rtf", RTF);
    let l = convert_doc_with(&rtf, RTF, What::Rtf, convert_child::in_process_runner).unwrap();
    let tab = l.into_tab(Kind::Docx, "letter.rtf".into(), Some(rtf), false);
    assert!(tab.pkg.is_some());
    assert!(tab.converted_docx.is_some());
    assert_eq!(tab.access.converted, Some(Converted::Rtf));
}

/// FIX r4 M2: Protected View's rollback of a converted tab restores what it
/// was converted to, without converting the file again (here, the file is
/// gone by then).
#[test]
fn a_converted_tabs_rollback_never_converts_again() {
    let dir = Scratch::new();
    let src = write(&dir, "letter.rtf", RTF);
    let mut tab = tab_from_path(&src);
    tab.access.protected = true;
    std::fs::remove_file(&src).unwrap();
    let Surface::Doc(ed) = &mut tab.surface else {
        panic!()
    };
    ed.insert_str("LEAKED ");
    tab.mark_dirty();
    assert!(!tab.dirty);
    assert_eq!(texts(&tab), ["Letter", "Dear reader, caf\u{e9}."]);
    assert_eq!(tab.access.converted, Some(Converted::Rtf));
}

/// FIX r4 M2: a converted tab restored from the session with no readable
/// sidecar (missing or damaged) waits to be converted; meanwhile it keeps
/// no sidecar and refuses every save.
#[test]
fn a_restored_converted_tab_waits_and_saves_nothing() {
    let dir = Scratch::new();
    let src = write(&dir, "letter.rtf", RTF);
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    let mut p = persist_tab(&hot, 0, &tab_from_path(&src));
    p.hot = None;
    p.dirty = false;
    // The file is unreadable at restore: restore does not even look at it.
    std::fs::remove_file(&src).unwrap();
    let mut back = restore_tab(&p);
    assert!(back.pending_conversion);
    assert!(persist_tab(&hot, 1, &back).hot.is_none());
    assert!(!save_doc_tab(&mut back, Some(dir.path("copy.docx"))));
    assert!(
        back.status.contains("not been converted yet"),
        "{}",
        back.status
    );
    std::fs::write(&src, RTF).unwrap();
    finish_pending_conversion(&mut back);
    assert_eq!(texts(&back), ["Letter", "Dear reader, caf\u{e9}."]);
    assert!(back.converted_docx.is_some());
}

/// FIX r5 m1: converting a waiting tab replaces its whole document: what
/// reached the placeholder is dropped with it, the tab is clean, and no
/// header/footer editor of the placeholder survives.
#[test]
fn finishing_a_pending_conversion_replaces_the_placeholder_cleanly() {
    let dir = Scratch::new();
    let src = write(&dir, "letter.rtf", RTF);
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    let mut p = persist_tab(&hot, 0, &tab_from_path(&src));
    p.hot = None;
    let mut back = restore_tab(&p);
    assert!(back.pending_conversion);
    let Surface::Doc(ed) = &mut back.surface else {
        panic!()
    };
    ed.insert_str("typed into the placeholder");
    back.dirty = true;
    finish_pending_conversion(&mut back);
    assert!(!back.dirty);
    assert!(back.hf_edit.is_none());
    assert_eq!(texts(&back), ["Letter", "Dear reader, caf\u{e9}."]);
}

/// FIX r5 m4: a converted tab whose file no longer converted when it was
/// finished keeps its converted mark but no converted bytes; a rollback
/// then reloads the file (never converting), so a leaked edit is undone.
#[test]
fn a_converted_tab_without_kept_bytes_rolls_back_from_its_file() {
    let dir = Scratch::new();
    let src = write(&dir, "letter.rtf", RTF);
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    let mut p = persist_tab(&hot, 0, &tab_from_path(&src));
    p.hot = None;
    let mut back = restore_tab(&p);
    // The file was replaced by a Word document meanwhile.
    let docx = long_docx(&dir, "real.docx", 2);
    std::fs::copy(&docx, &src).unwrap();
    finish_pending_conversion(&mut back);
    assert_eq!(back.access.converted, Some(Converted::Rtf));
    assert!(back.converted_docx.is_none());
    let before = texts(&back);
    back.access.protected = true;
    let Surface::Doc(ed) = &mut back.surface else {
        panic!()
    };
    ed.insert_str("LEAKED ");
    back.mark_dirty();
    assert!(!back.dirty);
    assert_eq!(texts(&back), before);
}
