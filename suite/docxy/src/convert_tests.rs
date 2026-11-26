//! Opening RTF, Web Pages, PDFs and damaged documents (#633), without a
//! window: the format by content, the converted tab, its save gate, Recover
//! Text from Any File and the session.

use crate::convert_child::{self, What};
use crate::open_mode::{Access, Converted, OpenMode, converted_refusal};
use crate::open_mode_tests::Scratch;
use crate::pdf_view::host::converted_pdf_tab;
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
fn a_web_pages_links_open_as_hyperlinks() {
    // #1879: a plain page (not an editable-HTML bundle) with a link, an
    // anchor link and a `javascript:` one.
    let dir = Scratch::new();
    let path = write(
        &dir,
        "links.htm",
        b"<html><body><p>See <a href=\"https://example.com/a?b=1&amp;c=2\">the docs</a>, \
          <a href=\"#top\">top</a> or <a href=\"javascript:go()\">this</a>.</p></body></html>",
    );
    let tab = tab_from_path(&path);
    assert_eq!(tab.status.as_ref(), "loaded (converted from HTML)");
    assert!(tab.bundle_html.is_none());
    assert_eq!(texts(&tab), ["See the docs, top or this."]);
    let Surface::Doc(ed) = &tab.surface else {
        panic!("not a document tab")
    };
    let Block::Paragraph(p) = &ed.doc.body[0] else {
        panic!("{:?}", ed.doc.body)
    };
    let links: Vec<(Option<&str>, Option<&str>, String)> = p
        .content
        .iter()
        .filter_map(|i| match i {
            docxcore::model::Inline::Hyperlink(h) => Some((
                h.target.as_deref(),
                h.anchor.as_deref(),
                h.runs.iter().map(|r| r.text.as_str()).collect(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        links,
        [
            (
                Some("https://example.com/a?b=1&c=2"),
                None,
                "the docs".to_string()
            ),
            (None, Some("top"), "top".to_string()),
        ]
    );
}

/// A PNG header `image::sniff` reads as `w` x `h`.
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    out.extend(w.to_be_bytes());
    out.extend(h.to_be_bytes());
    out.extend([8, 2, 0, 0, 0, 0, 0, 0, 0]);
    out
}

/// A Word Web Page beside its `page_files` folder (#1880): Word's VML image
/// with its `<![if !vml]>` twin, a plain `<img>` there, a `data:` image and
/// a remote one.
fn web_page_with_images(dir: &Scratch) -> PathBuf {
    std::fs::create_dir_all(dir.path("page_files")).unwrap();
    write(dir, "page_files/image001.png", &png(800, 400));
    write(dir, "page_files/image002.png", &png(64, 32));
    write(dir, "page_files/image003.png", &png(20, 10));
    write(
        dir,
        "page.htm",
        b"<html><body><p class=MsoNormal>Before \
          <!--[if gte vml 1]><v:shape style='width:48pt;height:24pt'><v:imagedata src=\"page_files/image001.png\" o:title=\"\"/></v:shape><![endif]--><![if !vml]><img width=64 height=32 src=\"page_files/image002.png\" v:shapes=\"_x0000_i1025\"><![endif]> after</p>\
          <p><img src=\"page_files/image003.png\" alt=Chart></p>\
          <p><img src=\"data:image/gif;base64,R0lGODlhAwACAAAAACw=\"></p>\
          <p><img src=\"http://example.com/logo.png\" width=40 height=20 alt=Logo></p></body></html>",
    )
}

/// Each picture of `tab`'s body and the bytes its rid names in the tab's
/// package.
fn tab_pictures(tab: &DocTab) -> Vec<((i64, i64), Vec<u8>)> {
    let Surface::Doc(ed) = &tab.surface else {
        panic!("not a document tab")
    };
    let pkg = tab.pkg.as_ref().expect("a converted tab has its package");
    let mut out = Vec::new();
    for b in &ed.doc.body {
        let Block::Paragraph(p) = b else { continue };
        for i in &p.content {
            let docxcore::model::Inline::Raw(raw) = i else {
                continue;
            };
            let Some(rid) = docxcore::image::inline_embed(raw) else {
                continue;
            };
            let part = pkg
                .part_rel_target("word/document.xml", &rid)
                .unwrap_or_else(|| panic!("{rid} resolves in the tab's package"));
            out.push((
                docxcore::image::inline_extent(raw).unwrap(),
                pkg.part(&part).unwrap().to_vec(),
            ));
        }
    }
    out
}

/// The page and margins (twips) a tab's final section gives Print Layout.
fn page_of(tab: &DocTab) -> (i32, i32, i32, i32, i32, i32) {
    let g = crate::final_page_geom(tab);
    (g.w, g.h, g.ml, g.mr, g.mt, g.mb)
}

#[test]
fn an_opened_html_page_has_outlook_like_margins() {
    // #1928: a Web Page opens on a Letter page 15 px (225 twips) in on
    // every side, as a mail client shows it, through the real child: its
    // converted .docx, loaded as the tab's package.
    let dir = Scratch::new();
    let path = write(&dir, "mail.htm", b"<html><body><p>Hello</p></body></html>");
    let outlook = (12_240, 15_840, 225, 225, 225, 225);
    let bytes = std::fs::read(&path).unwrap();
    let l = convert_doc_with(
        &path,
        &bytes,
        What::Html,
        convert_child::tests::test_child_runner,
    )
    .unwrap();
    let child = l.into_tab(Kind::Docx, "mail.htm".into(), Some(path.clone()), false);
    assert_eq!(child.access.converted, Some(Converted::Html));
    assert_eq!(page_of(&child), outlook);
    // And as File > Open makes it.
    let tab = tab_from_path(&path);
    assert_eq!(texts(&tab), ["Hello"]);
    assert_eq!(page_of(&tab), outlook);
    // An RTF keeps the default page: 1" margins.
    let rtf = write(&dir, "letter.rtf", RTF);
    assert_eq!(
        page_of(&tab_from_path(&rtf)),
        (12_240, 15_840, 1440, 1440, 1440, 1440)
    );
}

#[test]
fn a_web_pages_images_open_with_it_and_the_network_is_never_reached() {
    let dir = Scratch::new();
    let path = web_page_with_images(&dir);
    let tab = tab_from_path(&path);
    assert_eq!(tab.access.converted, Some(Converted::Html));
    assert_eq!(texts(&tab)[0], "Before  after");
    let pics = tab_pictures(&tab);
    let px = 9525;
    let sizes: Vec<(i64, i64)> = pics.iter().map(|(s, _)| *s).collect();
    assert_eq!(
        sizes,
        [
            (64 * px, 32 * px),
            (20 * px, 10 * px),
            (3 * px, 2 * px),
            (40 * px, 20 * px)
        ]
    );
    // The `<![if !vml]>` image, not its VML twin; the plain one; the data
    // image; the remote one a placeholder (one grey pixel).
    assert_eq!(pics[0].1, png(64, 32));
    assert_eq!(pics[1].1, png(20, 10));
    assert!(pics[2].1.starts_with(b"GIF89a"));
    let placeholder = docxcore::image::sniff(&pics[3].1).unwrap();
    assert_eq!((placeholder.width, placeholder.height), (1, 1));
    // Through the real child too: the same pictures.
    let bytes = std::fs::read(&path).unwrap();
    let l = convert_doc_with(
        &path,
        &bytes,
        What::Html,
        convert_child::tests::test_child_runner,
    )
    .unwrap();
    let child = l.into_tab(Kind::Docx, "page.htm".into(), Some(path.clone()), false);
    assert_eq!(tab_pictures(&child), pics);
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

/// #2245: a PDF opens as it looks, in a PDF view; Convert to editable
/// document gives the converted tab a PDF opened as before (#633).
#[test]
fn a_pdf_opens_in_a_pdf_view_and_converts_on_request() {
    let dir = Scratch::new();
    let doc = docxcore::markdown::from_markdown("Hello from a PDF.\n\nSecond paragraph.\n");
    let pdf = docxcore::export::to_pdf(&doc, &docxcore::export::PdfOptions::default());
    let path = write(&dir, "report.pdf", &pdf);
    let tab = tab_from_path(&path);
    assert!(tab.kind == Kind::Pdf);
    assert_eq!(tab.status.as_ref(), "loaded (PDF, 1 page)");
    assert_eq!(tab.title.as_ref(), "report.pdf");
    let Surface::Pdf(v) = &tab.surface else {
        panic!("not a PDF view")
    };
    assert_eq!(v.page_count(), 1);
    assert_eq!(tab.access.converted, None);
    // Every way a document opens: Open's modes too.
    let opened = tab_from_path_mode(&path, OpenMode::Normal, &TrustStore::default()).unwrap();
    assert!(matches!(opened.surface, Surface::Pdf(_)));
    // Convert to editable document: the conversion, as a document.
    let converted = converted_pdf_tab(&path, &TrustStore::default()).unwrap();
    assert_eq!(converted.status.as_ref(), "loaded (converted from PDF)");
    assert_eq!(converted.access.converted, Some(Converted::Pdf));
    assert!(converted.kind == Kind::Docx);
    let text = texts(&converted).join("\n");
    assert!(text.contains("Hello from a PDF."), "{text}");
    // A PDF with no text opens as its (blank) page; its conversion says
    // there is no text, a failed load.
    let empty = docxcore::export::to_pdf(
        &docxcore::markdown::from_markdown(""),
        &docxcore::export::PdfOptions::default(),
    );
    let blank = write(&dir, "blank.pdf", &empty);
    assert!(matches!(tab_from_path(&blank).surface, Surface::Pdf(_)));
    let tab = converted_pdf_tab(&blank, &TrustStore::default()).unwrap();
    assert!(tab.load_failed, "{}", tab.status);
    assert!(tab.status.starts_with("load error"), "{}", tab.status);
}

/// #2245: Recover Text from Any File still reads a PDF's text, and a file
/// named .pdf that is no PDF opens as before.
#[test]
fn recover_text_and_a_false_pdf_do_not_open_a_pdf_view() {
    let dir = Scratch::new();
    let doc = docxcore::markdown::from_markdown("Readable words.\n");
    let pdf = write(
        &dir,
        "words.pdf",
        &docxcore::export::to_pdf(&doc, &docxcore::export::PdfOptions::default()),
    );
    let rt = tab_from_path_mode(&pdf, OpenMode::RecoverText, &TrustStore::default()).unwrap();
    assert!(matches!(rt.surface, Surface::Doc(_)));
    assert_eq!(rt.access.converted, Some(Converted::RecoveredText));
    let fake = write(&dir, "notes.pdf", RTF);
    let tab = tab_from_path(&fake);
    assert!(matches!(tab.surface, Surface::Doc(_)));
    assert_eq!(tab.access.converted, Some(Converted::Rtf));
}

/// #2245: a PDF view is a document row in the session, with where it was;
/// it comes back a PDF view there. A row of an older build (a converted
/// PDF tab) still restores converted.
#[test]
fn a_pdf_view_comes_back_from_the_session_and_an_old_converted_row_converts() {
    let dir = Scratch::new();
    // A three-page PDF, so the page the view was on is not the first.
    let slides = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/pdf-view/slides.pdf");
    let path = write(&dir, "slides.pdf", &std::fs::read(slides).unwrap());
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    let mut tab = tab_from_path(&path);
    if let Surface::Pdf(v) = &mut tab.surface {
        v.zoom = 1.5;
        v.restore_page = Some(2);
    }
    let persisted = persist_tab(&hot, 0, 0, &tab);
    assert!(
        persisted.kind == Kind::Docx,
        "an older build reads a document row"
    );
    assert_eq!(persisted.hot, None, "the PDF is never written");
    assert!(!persisted.dirty);
    let at = persisted.pdf_view.expect("where the view was");
    assert_eq!((at.page, at.zoom), (2, 1.5));
    let json = serde_json::to_string(&persisted).unwrap();
    assert!(json.contains("\"kind\":\"Docx\""), "{json}");
    // FIX r2: restored, two PDF tabs wait: no worker starts for either
    // until it is in front.
    let row: crate::PersistTab = serde_json::from_str(&json).unwrap();
    let mut back = restore_tab(&row);
    let other = restore_tab(&row);
    for t in [&back, &other] {
        assert!(t.kind == Kind::Pdf);
        assert!(matches!(t.surface, Surface::PdfPending(_)), "no worker yet");
        assert_eq!(t.status.as_ref(), crate::pdf_view::host::PENDING);
    }
    // Persisted again before it opened: still where it was.
    let again = persist_tab(&hot, 0, 1, &other).pdf_view.unwrap();
    assert_eq!((again.page, again.zoom), (2, 1.5));
    // In front, it opens there.
    finish_pending_conversion(&mut back);
    let Surface::Pdf(v) = &back.surface else {
        panic!("not a PDF view")
    };
    assert_eq!(v.zoom, 1.5);
    assert_eq!(v.restore_page, Some(2));
    assert_eq!(v.page_count(), 3);
    assert_eq!(back.status.as_ref(), "loaded (PDF, 3 pages)");
    // A row an older build wrote for the PDF it converted.
    let doc = docxcore::markdown::from_markdown("Page text.\n");
    let paper = write(
        &dir,
        "paper.pdf",
        &docxcore::export::to_pdf(&doc, &docxcore::export::PdfOptions::default()),
    );
    let old = format!(
        r#"{{"kind":"Docx","title":"paper.pdf","path":{},"converted":"pdf"}}"#,
        serde_json::to_string(&paper.display().to_string()).unwrap()
    );
    let mut back = restore_tab(&serde_json::from_str(&old).unwrap());
    assert_eq!(back.access.converted, Some(Converted::Pdf));
    assert!(back.pending_conversion);
    finish_pending_conversion(&mut back);
    assert!(matches!(back.surface, Surface::Doc(_)));
    assert_eq!(back.status.as_ref(), "loaded (converted from PDF)");
    assert_eq!(texts(&back), ["Page text."]);
}

/// FIX r3: a second open of a PDF goes to its PDF tab (open, failed or
/// not opened yet), whatever the order of a converted tab of it.
#[test]
fn reopening_a_pdf_goes_to_its_pdf_tab() {
    let dir = Scratch::new();
    let doc = docxcore::markdown::from_markdown("Words.\n");
    let path = write(
        &dir,
        "paper.pdf",
        &docxcore::export::to_pdf(&doc, &docxcore::export::PdfOptions::default()),
    );
    let converted = converted_pdf_tab(&path, &TrustStore::default()).unwrap();
    let at = crate::pdf_view::host::PdfAt { page: 1, zoom: 2. };
    let pending = crate::pdf_view::host::pending_pdf_tab(&path, "paper.pdf", at);
    let other = tab_from_path(&write(&dir, "letter.rtf", RTF));
    let tabs = vec![other, converted, pending];
    assert_eq!(crate::open_tab_index(&tabs, &path), Some(2));
    // With no PDF tab, the converted one.
    assert_eq!(crate::open_tab_index(&tabs[..2], &path), Some(1));
}

/// FIX r3: a restored PDF tab whose file cannot be read keeps waiting where
/// it was; one whose file is no PDF any more opens as a document.
#[test]
fn a_restored_pdf_tab_whose_file_is_gone_or_changed() {
    let dir = Scratch::new();
    let at = crate::pdf_view::host::PdfAt { page: 2, zoom: 1.5 };
    let gone = dir.path("gone.pdf");
    let mut tab = crate::pdf_view::host::pending_pdf_tab(&gone, "gone.pdf", at);
    finish_pending_conversion(&mut tab);
    assert!(tab.status.starts_with("read error"), "{}", tab.status);
    assert!(tab.load_failed);
    assert!(matches!(tab.surface, Surface::PdfPending(a) if a == at));
    assert_eq!(
        crate::pdf_view::host::pdf_at(&tab),
        Some(at),
        "the place is kept"
    );
    // Not retried every frame: only an activation clears the mark.
    finish_pending_conversion(&mut tab);
    assert!(matches!(tab.surface, Surface::PdfPending(_)));
    // The file is back: the next activation opens it.
    let slides = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/pdf-view/slides.pdf");
    std::fs::copy(slides, &gone).unwrap();
    crate::activate_pending(&mut tab);
    let Surface::Pdf(v) = &tab.surface else {
        panic!("not opened: {}", tab.status)
    };
    assert_eq!((v.restore_page, v.zoom), (Some(2), 1.5));
    // FIX r4: a worker that cannot start keeps it pending where it was too,
    // and the next activation tries again.
    let mut tab = crate::pdf_view::host::pending_pdf_tab(&gone, "gone.pdf", at);
    let no_worker = crate::pdf_view::worker::Spawn::Command(std::sync::Arc::new(|| {
        std::process::Command::new("/nonexistent/docxy-pdf-worker")
    }));
    crate::pdf_view::host::finish_pending_pdf_with(&mut tab, at, no_worker);
    assert!(tab.load_failed);
    assert!(tab.status.starts_with("load error"), "{}", tab.status);
    assert!(matches!(tab.surface, Surface::PdfPending(a) if a == at));
    assert_eq!(
        crate::pdf_view::host::pdf_at(&tab),
        Some(at),
        "the place is kept"
    );
    crate::activate_pending(&mut tab);
    assert!(matches!(tab.surface, Surface::Pdf(_)), "{}", tab.status);
    // A file that is no PDF any more opens as a document would.
    let changed = write(&dir, "changed.pdf", RTF);
    let mut tab = crate::pdf_view::host::pending_pdf_tab(&changed, "changed.pdf", at);
    finish_pending_conversion(&mut tab);
    assert!(tab.kind == Kind::Docx);
    assert!(matches!(tab.surface, Surface::Doc(_)));
    assert_eq!(tab.access.converted, Some(Converted::Rtf));
    assert_eq!(tab.title.as_ref(), "changed.pdf");
}

/// FIX r2: a PDF that fails to open is still a PDF tab: File only on its
/// ribbon, and Save never writes it.
#[test]
fn a_pdf_that_fails_to_open_keeps_a_pdf_tabs_ribbon() {
    let dir = Scratch::new();
    let path = write(&dir, "broken.pdf", b"%PDF-1.4\nnot a PDF at all\n");
    let tab = tab_from_path(&path);
    assert!(tab.kind == Kind::Pdf);
    assert!(matches!(tab.surface, Surface::Placeholder));
    assert!(tab.load_failed, "{}", tab.status);
    assert!(tab.status.starts_with("load error"), "{}", tab.status);
    let names: Vec<&str> = crate::ribbon_tab_set(tab.kind)
        .iter()
        .map(|(_, name, _)| *name)
        .collect();
    assert_eq!(names, ["File"]);
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

/// #851: a package over its size budget is not damaged: it keeps the load
/// error, in words, and is never opened as recovered text.
#[test]
fn a_too_large_docx_is_not_recovered() {
    let dir = Scratch::new();
    let bytes = std::fs::read(long_docx(&dir, "whole.docx", 3)).unwrap();
    let bytes = declare_size(bytes, "word/document.xml", u32::MAX);
    let tab = tab_from_path(&write(&dir, "big.docx", &bytes));
    assert!(tab.load_failed);
    assert!(
        tab.status.starts_with("load error: document too large"),
        "{}",
        tab.status
    );
    assert_eq!(tab.access.converted, None);
}

/// Set the declared uncompressed size of the entry named `name` in the ZIP's
/// central directory.
fn declare_size(mut zip: Vec<u8>, name: &str, size: u32) -> Vec<u8> {
    let rd16 = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]) as usize;
    let eocd = zip.windows(4).rposition(|s| s == b"PK").unwrap();
    let mut at = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    loop {
        assert_eq!(&zip[at..at + 4], b"PK", "no entry {name}");
        let len = rd16(&zip, at + 28);
        if &zip[at + 46..at + 46 + len] == name.as_bytes() {
            zip[at + 24..at + 28].copy_from_slice(&size.to_le_bytes());
            return zip;
        }
        at += 46 + len + rd16(&zip, at + 30) + rd16(&zip, at + 32);
    }
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
    let persisted = persist_tab(&hot, 0, 0, &tab);
    assert_eq!(persisted.converted, Some(Converted::Rtf));
    let mut back = restore_tab(&persisted);
    assert_eq!(back.access.converted, Some(Converted::Rtf));
    assert!(back.dirty);
    assert!(!save_doc_tab(&mut back, None));
    assert_eq!(std::fs::read(&src).unwrap(), RTF);
    // Clean, with no sidecar: converted again from the file, when its tab
    // is first in front (FIX r4 M2), not at restore.
    let mut clean = persist_tab(&hot, 0, 1, &tab_from_path(&src));
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
    let mut p = persist_tab(&hot, 0, 2, &rt);
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
    let mut p = persist_tab(&hot, 0, 0, &tab);
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
    let mut p = persist_tab(&hot, 0, 0, &tab_from_path(&src));
    p.hot = None;
    p.dirty = false;
    // The file is unreadable at restore: restore does not even look at it.
    std::fs::remove_file(&src).unwrap();
    let mut back = restore_tab(&p);
    assert!(back.pending_conversion);
    assert!(persist_tab(&hot, 0, 1, &back).hot.is_none());
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
    let mut p = persist_tab(&hot, 0, 0, &tab_from_path(&src));
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
    let mut p = persist_tab(&hot, 0, 0, &tab_from_path(&src));
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

/// #2256 criteria 3 and 5: an encrypted PDF opens as a PDF tab whose body
/// is its password prompt; restored from the session it asks again where
/// the view was. A wrong try empties the field and says so; the right one
/// shows the pages there. The session row never holds the password.
#[test]
fn an_encrypted_pdf_asks_for_its_password_and_its_session_row_keeps_none() {
    use crate::pdf_view::Unlocked;
    use crate::pdf_view::host::{PDF_LOCKED, PdfAt, pdf_at, pending_pdf_tab};
    let dir = Scratch::new();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/pdf-view/enc-aes-256.pdf");
    let path = write(&dir, "enc.pdf", &std::fs::read(src).unwrap());
    let hot = dir.path("hot");
    std::fs::create_dir_all(&hot).unwrap();
    // Opened: a PDF tab with the prompt.
    let tab = tab_from_path(&path);
    assert!(tab.kind == Kind::Pdf);
    assert_eq!(tab.status.as_ref(), PDF_LOCKED);
    let Surface::Pdf(v) = &tab.surface else {
        panic!("not a PDF view: {}", tab.status)
    };
    assert!(v.lock.is_some());
    assert_eq!(v.page_count(), 0);
    // Restored from a session where it was on page 1 at 150 %: it waits,
    // then asks, keeping that place.
    let at = PdfAt { page: 0, zoom: 1.5 };
    let mut tab = pending_pdf_tab(&path, "enc.pdf", at);
    finish_pending_conversion(&mut tab);
    assert_eq!(tab.status.as_ref(), PDF_LOCKED);
    assert_eq!(pdf_at(&tab), Some(at), "its place is kept while it asks");
    let Surface::Pdf(v) = &mut tab.surface else {
        panic!("not a PDF view")
    };
    // A wrong try: emptied, focused, wrong.
    v.lock.as_mut().unwrap().field.text = "not-it".into();
    assert_eq!(v.try_unlock(), Unlocked::Wrong);
    let lock = v.lock.as_ref().unwrap();
    assert!(lock.wrong && lock.field.focused && lock.field.text.is_empty());
    let row = serde_json::to_string(&persist_tab(&hot, 0, 0, &tab)).unwrap();
    assert!(!row.contains("not-it"), "{row}");
    // The right one: the pages, where the session left the view.
    let Surface::Pdf(v) = &mut tab.surface else {
        panic!("not a PDF view")
    };
    v.lock.as_mut().unwrap().field.text = "user-aes-256".into();
    assert_eq!(v.try_unlock(), Unlocked::Opened);
    assert!(v.lock.is_none());
    assert_eq!(v.page_count(), 1);
    assert_eq!((v.zoom, v.restore_page), (1.5, Some(0)));
    assert!(v.restricted_note().contains("printing"));
    let row = serde_json::to_string(&persist_tab(&hot, 0, 0, &tab)).unwrap();
    assert!(!row.contains("user-aes-256"), "{row}");
    assert!(!row.contains("password"), "{row}");
    // Save has nothing to write while it asks.
    let mut tab = tab_from_path(&path);
    assert_eq!(
        crate::pdf_view::host::pdf_save_step(&mut tab, Some(dir.path("copy.pdf"))),
        crate::pdf_view::host::PdfSaveStep::Nothing(crate::pdf_view::host::PDF_NOT_OPEN)
    );
}
