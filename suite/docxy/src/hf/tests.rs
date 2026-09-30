//! Header/footer editing on the caret's section (#640), pagination at section
//! breaks and per-page header resolution, over the PAG-CASE-019 shape: three
//! sections "One" / "Two" / "Three", section 1's default header "Header A",
//! section 2 linked to it (no reference), section 3's own "Header C", and a
//! footer "Footer A" on section 1 only.

use super::*;
use crate::*;
use core::prelude::v1::test;
use docxcore::package::load_package;

pub(crate) const MARGINS: &str = r#"<w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/>"#;

/// A sectPr with the Letter page, the given references and extra children.
pub(crate) fn sect(refs: &str, extra: &str) -> String {
    format!("<w:sectPr>{refs}{MARGINS}{extra}</w:sectPr>")
}

fn para(text: &str, sect_pr: Option<&str>) -> String {
    let ppr = sect_pr
        .map(|s| format!("<w:pPr>{s}</w:pPr>"))
        .unwrap_or_default();
    format!("<w:p>{ppr}<w:r><w:t>{text}</w:t></w:r></w:p>")
}

/// Parse body XML (paragraphs and a trailing sectPr) into blocks.
fn body_blocks(xml: &str) -> Vec<Block> {
    docxcore::load::parse_header_footer(
        &format!(
            "<w:hdr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">{xml}</w:hdr>"
        ),
        &Default::default(),
    )
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/hf-tests")
        .join(format!("{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The bytes of a three-section document. `headers` builds the PAG-CASE-019
/// header/footer shape; without it no section has a reference. `styles`
/// defines Word's Header/Footer styles, as Word's own files do.
pub(crate) fn three_sections_docx(headers: bool, styles: bool) -> Vec<u8> {
    let base = tab_from_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
    );
    let mut pkg = base.pkg.clone().unwrap();
    if styles {
        pkg.ensure_styles(&["Header", "Footer"]);
    }
    let (mut r1, mut r3) = (String::new(), String::new());
    if headers {
        let hp = |t: &str, s: &str| {
            format!("<w:p><w:pPr><w:pStyle w:val=\"{s}\"/></w:pPr><w:r><w:t>{t}</w:t></w:r></w:p>")
        };
        let (a, _) = pkg.create_hf_part(true, &hp("Header A", "Header")).unwrap();
        let (c, _) = pkg.create_hf_part(true, &hp("Header C", "Header")).unwrap();
        let (fa, _) = pkg
            .create_hf_part(false, &hp("Footer A", "Footer"))
            .unwrap();
        r1 = format!(
            "<w:headerReference w:type=\"default\" r:id=\"{a}\"/><w:footerReference w:type=\"default\" r:id=\"{fa}\"/>"
        );
        r3 = format!("<w:headerReference w:type=\"default\" r:id=\"{c}\"/>");
    }
    let body = format!(
        "{}{}{}{}",
        para("One", Some(&sect(&r1, ""))),
        para("Two", Some(&sect("", ""))),
        para("Three", None),
        sect(&r3, "")
    );
    let doc = docxcore::model::Document {
        body: body_blocks(&body),
    };
    doc_to_docx(&doc, &[], Some(&pkg))
}

/// A tab on [`three_sections_docx`].
fn three_sections(name: &str, headers: bool) -> DocTab {
    let dir = tmp_dir(name);
    let path = dir.join("in.docx");
    std::fs::write(&path, three_sections_docx(headers, true)).unwrap();
    let t = tab_from_path(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!t.dirty, "{}", t.status);
    assert_eq!(ed(&t).sections().len(), 3);
    t
}

fn ed(t: &DocTab) -> &Editor {
    let Surface::Doc(ed) = &t.surface else {
        panic!("a document")
    };
    ed
}

fn ed_mut(t: &mut DocTab) -> &mut Editor {
    let Surface::Doc(ed) = &mut t.surface else {
        panic!("a document")
    };
    ed
}

/// Put the body caret at the start of body block `i`.
fn caret_in(t: &mut DocTab, i: usize) {
    ed_mut(t).caret = docxcore::editor::Caret {
        path: vec![i],
        offset: 0,
    };
}

fn text_of(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(Block::plain_text)
        .collect::<Vec<_>>()
        .join("|")
}

fn part_text(pkg: &Package, part: &str) -> String {
    text_of(&parse_hf_part(pkg, part))
}

/// What Save writes, reloaded (the open header/footer flushed first).
fn saved(t: &mut DocTab) -> Package {
    flush_hf_tab(t);
    load_package(&doc_to_docx(&ed(t).doc, &t.comments, t.pkg.as_ref())).unwrap()
}

/// The saved package's sectPrs in document order.
fn saved_sections(pkg: &Package) -> Vec<String> {
    let mut ed = Editor::new(pkg.document.clone());
    ed.caret = docxcore::editor::Caret::default();
    ed.sections()
}

/// The text a saved section's resolved header (`is_header`) or footer shows.
fn saved_hf(pkg: &Package, section: usize, is_header: bool) -> String {
    let parts = section_header_parts(&saved_sections(pkg), &pkg.document_rels());
    parts[section]
        .get(is_header, HeaderVariant::Default)
        .map(|a| part_text(pkg, &a.part_name))
        .unwrap_or_default()
}

fn edited_text(t: &DocTab) -> String {
    text_of(&t.hf_edit.as_ref().unwrap().editor.doc.body)
}

#[test]
fn section_breaks_start_pages_unless_the_next_section_is_continuous() {
    let t = three_sections("pages", true);
    let body = &ed(&t).doc.body;
    let pages = paginate(body, 10_000.0, 600.0);
    assert_eq!(pages.len(), 3, "{pages:?}");
    let firsts: Vec<usize> = pages.iter().map(|p| p.0).collect();
    let slots = page_slots(ed(&t), &firsts, false);
    let secs: Vec<usize> = slots.iter().map(|s| s.section).collect();
    assert_eq!(secs, vec![0, 1, 2]);
    // Columns flow breaks the same way.
    assert_eq!(paginate_cols(body, 10_000.0, 300.0, 2).len(), 3);

    // Section 2 starting continuously keeps "One" and "Two" on one page:
    // the break after "One" reads section 2's w:type.
    let cont = body_blocks(&format!(
        "{}{}{}",
        para("One", Some(&sect("", ""))),
        para("Two", Some(&sect("", "<w:type w:val=\"continuous\"/>"))),
        sect("", "")
    ));
    assert_eq!(section_page_ends(&cont), vec![false, true, false]);
    assert_eq!(paginate(&cont, 10_000.0, 600.0).len(), 2);
    // A trailing continuous section after a next-page one.
    let tail = body_blocks(&format!(
        "{}{}",
        para("One", Some(&sect("", ""))),
        sect("", "<w:type w:val=\"continuous\"/>")
    ));
    assert_eq!(section_page_ends(&tail), vec![false, false]);
}

#[test]
fn every_page_resolves_its_own_sections_header() {
    let t = three_sections("per-page", true);
    let pkg = t.pkg.as_ref().unwrap();
    let parts = resolve(ed(&t), pkg);
    let slots = page_slots(ed(&t), &[0, 1, 2], false);
    let shown: Vec<String> = slots
        .iter()
        .map(|s| part_text(pkg, slot_part(&parts, *s, true).unwrap()))
        .collect();
    assert_eq!(shown, vec!["Header A", "Header A", "Header C"]);
    let footers: Vec<String> = slots
        .iter()
        .map(|s| part_text(pkg, slot_part(&parts, *s, false).unwrap()))
        .collect();
    assert_eq!(footers, vec!["Footer A"; 3]);
    assert!(
        parts[1]
            .get(true, HeaderVariant::Default)
            .unwrap()
            .inherited
    );
    assert_eq!(
        parts[1]
            .get(true, HeaderVariant::Default)
            .unwrap()
            .from_section,
        0
    );
}

#[test]
fn page_variants_follow_title_page_per_section_and_even_pages() {
    let mut t = three_sections("variants", true);
    ed_mut(&mut t).edit_sections(&[1], |raw| docxcore::sect::set_flag(raw, "w:titlePg", true));
    // Pages: s0, s1 (first), s1, s2.
    let slots = page_slots(ed(&t), &[0, 1, 1, 2], true);
    let got: Vec<(usize, HeaderVariant)> = slots.iter().map(|s| (s.section, s.variant)).collect();
    assert_eq!(
        got,
        vec![
            (0, HeaderVariant::Default),
            (1, HeaderVariant::First),
            (1, HeaderVariant::Default),
            (2, HeaderVariant::Even),
        ]
    );
    assert_eq!(edit_page(&slots, 1, HeaderVariant::Default), 2);
    assert_eq!(edit_page(&slots, 2, HeaderVariant::First), 3, "falls back");
    // One page with even/odd on has no even page.
    let one = page_slots(ed(&t), &[0], true);
    assert_eq!(one[0].variant, HeaderVariant::Default);
}

/// PAG-CASE-019 / #640: the caret in "Two" edits the header section 2
/// inherits, never section 3's, and writes no reference into section 2.
#[test]
fn edit_header_in_a_linked_section_edits_the_inherited_part() {
    let mut t = three_sections("caret-two", true);
    caret_in(&mut t, 1);
    assert!(open_hf_tab(&mut t, true, HeaderVariant::Default));
    let hf = t.hf_edit.as_ref().unwrap();
    assert_eq!((hf.section, hf.variant), (1, HeaderVariant::Default));
    assert_eq!(edited_text(&t), "Header A");
    let edited_part = hf.part_name.clone();
    let editor = &mut t.hf_edit.as_mut().unwrap().editor;
    editor.caret = docxcore::editor::Caret {
        path: vec![0],
        offset: 0,
    };
    editor.insert_str("Z");
    exit_hf_tab(&mut t);
    let pkg = saved(&mut t);
    let sections = saved_sections(&pkg);
    assert!(!sections[1].contains("headerReference"), "{}", sections[1]);
    assert_eq!(saved_hf(&pkg, 0, true), "ZHeader A");
    assert_eq!(saved_hf(&pkg, 1, true), "ZHeader A");
    assert_eq!(saved_hf(&pkg, 2, true), "Header C");
    assert_eq!(part_text(&pkg, &edited_part), "ZHeader A");
}

#[test]
fn edit_header_and_footer_follow_the_caret_section() {
    for (block, header, footer) in [
        (0, "Header A", "Footer A"),
        (1, "Header A", "Footer A"),
        (2, "Header C", "Footer A"),
    ] {
        let mut t = three_sections("caret", true);
        caret_in(&mut t, block);
        assert!(open_hf_tab(&mut t, true, HeaderVariant::Default));
        assert_eq!(edited_text(&t), header, "header, caret in block {block}");
        assert_eq!(t.hf_edit.as_ref().unwrap().section, block);
        exit_hf_tab(&mut t);
        assert!(open_hf_tab(&mut t, false, HeaderVariant::Default));
        assert_eq!(edited_text(&t), footer, "footer, caret in block {block}");
        assert!(!t.dirty, "opening an existing part changes nothing");
        // Switching region keeps the edited section, not the caret's.
        caret_in(&mut t, 0);
        assert!(open_hf_tab(&mut t, true, HeaderVariant::Default));
        assert_eq!(t.hf_edit.as_ref().unwrap().section, block);
    }
}

/// Criterion 3: with no header anywhere, the new part is referenced from
/// section 0 only, so the caret's section and the ones linked to it show it.
#[test]
fn a_new_header_is_referenced_from_the_first_section_only() {
    let mut t = three_sections("create", false);
    caret_in(&mut t, 1);
    assert!(open_hf_tab(&mut t, true, HeaderVariant::Default));
    assert!(t.dirty);
    let part = t.hf_edit.as_ref().unwrap().part_name.clone();
    let sections = ed(&t).sections();
    assert!(docxcore::sect::hf_reference(&sections[0], true, "default").is_some());
    assert!(!sections[1].contains("Reference"), "{}", sections[1]);
    assert!(!sections[2].contains("Reference"), "{}", sections[2]);
    let parts = resolve(ed(&t), t.pkg.as_ref().unwrap());
    for p in &parts {
        assert_eq!(p.get(true, HeaderVariant::Default).unwrap().part_name, part);
    }
    // A first-page header goes to section 0 too, every variant alike.
    exit_hf_tab(&mut t);
    caret_in(&mut t, 2);
    assert!(open_hf_tab(&mut t, false, HeaderVariant::First));
    let sections = ed(&t).sections();
    assert!(docxcore::sect::hf_reference(&sections[0], false, "first").is_some());
    assert!(!sections[2].contains("footerReference"));
    // The new paragraph is in the Footer style.
    let body = &t.hf_edit.as_ref().unwrap().editor.doc.body;
    let Block::Paragraph(p) = &body[0] else {
        panic!()
    };
    assert_eq!(p.props.style_id.as_deref(), Some("Footer"));
    // One undo (in the body) takes the reference back out.
    exit_hf_tab(&mut t);
    assert!(ed_mut(&mut t).undo());
    assert!(docxcore::sect::hf_reference(&ed(&t).sections()[0], false, "first").is_none());
}

#[test]
fn a_new_part_goes_where_an_unresolvable_reference_already_is() {
    let mut t = three_sections("broken", false);
    ed_mut(&mut t).edit_sections(&[1], |raw| {
        docxcore::sect::set_hf_reference(raw, true, "default", Some("rIdMissing"))
    });
    caret_in(&mut t, 2);
    assert!(open_hf_tab(&mut t, true, HeaderVariant::Default));
    let sections = ed(&t).sections();
    assert!(!sections[0].contains("headerReference"));
    assert_ne!(
        docxcore::sect::hf_reference(&sections[1], true, "default").as_deref(),
        Some("rIdMissing")
    );
    let parts = resolve(ed(&t), t.pkg.as_ref().unwrap());
    assert!(parts[2].get(true, HeaderVariant::Default).is_some());
}

/// Criterion 4: Different First Page acts on the edited (caret's) section.
#[test]
fn different_first_page_acts_on_the_caret_section() {
    let mut t = three_sections("title-pg", true);
    caret_in(&mut t, 1);
    assert!(toggle_title_pg_tab(&mut t));
    let sections = ed(&t).sections();
    let flags: Vec<bool> = sections
        .iter()
        .map(|s| docxcore::sect::has_flag(s, "w:titlePg"))
        .collect();
    assert_eq!(flags, vec![false, true, false]);
    // While editing section 3's header, it acts on section 3.
    caret_in(&mut t, 2);
    assert!(open_hf_tab(&mut t, true, HeaderVariant::Default));
    caret_in(&mut t, 0);
    assert!(toggle_title_pg_tab(&mut t));
    assert!(docxcore::sect::has_flag(&ed(&t).sections()[2], "w:titlePg"));
    assert!(!docxcore::sect::has_flag(
        &ed(&t).sections()[0],
        "w:titlePg"
    ));
    assert!(ed_mut(&mut t).undo());
    assert!(!docxcore::sect::has_flag(
        &ed(&t).sections()[2],
        "w:titlePg"
    ));
}

#[test]
fn distances_default_to_half_an_inch() {
    assert_eq!(distance("<w:sectPr/>", true), 720);
    let s = r#"<w:sectPr><w:pgMar w:top="1440" w:header="360" w:footer="1080"/></w:sectPr>"#;
    assert_eq!((distance(s, true), distance(s, false)), (360, 1080));
}
