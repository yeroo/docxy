//! Section-property commands on a document whose body ends in its own
//! `w:sectPr` (every file Word writes) survive Save: the body editor's copy is
//! the one `doc_to_docx` writes (#639). The Layout tab's commands edit that
//! copy directly (#649); header creation and ruler drags edit the package's
//! and mirror it into the editor.

use super::*;
use core::prelude::v1::test;
use docxcore::model::SectionProperties;

const SECT: &str = r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/><w:cols w:space="720"/></w:sectPr>"#;

/// A clean tab loaded from a saved .docx whose body ends in `SECT` and whose
/// section has no header/footer reference.
fn tab_with_sect_pr(name: &str) -> DocTab {
    let mut source = tab_from_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
    );
    let Surface::Doc(ed) = &mut source.surface else {
        panic!("basic.docx is a document")
    };
    ed.doc.set_trailing_section_properties(SectionProperties {
        raw: SECT.to_string(),
        property_change: None,
    });
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/sect-pr-tests")
        .join(format!("{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("with-sect-pr.docx");
    std::fs::write(
        &path,
        doc_to_docx(&ed.doc, &source.comments, source.pkg.as_ref()),
    )
    .unwrap();
    let t = tab_from_path(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!t.dirty, "{}", t.status);
    assert_eq!(
        editor(&t).doc.trailing_section_properties().unwrap().raw,
        SECT
    );
    assert!(!final_sect_pr(&t).unwrap().contains("Reference"));
    t
}

/// Layout > Columns > Two on the caret's section (the final one here).
fn two_columns(t: &mut DocTab) {
    use crate::layout_tab::{ColumnsPreset, LayoutAct, layout_apply};
    layout_apply(t, LayoutAct::Columns(ColumnsPreset::Two)).unwrap();
}

fn editor(t: &DocTab) -> &Editor {
    let Surface::Doc(ed) = &t.surface else {
        panic!()
    };
    ed
}

fn editor_mut(t: &mut DocTab) -> &mut Editor {
    let Surface::Doc(ed) = &mut t.surface else {
        panic!()
    };
    ed
}

/// What Save writes for the tab, reloaded: the open header/footer is flushed
/// first, as Save does.
fn save_and_reload(t: &mut DocTab) -> Package {
    flush_hf_tab(t);
    let bytes = doc_to_docx(&editor(t).doc, &t.comments, t.pkg.as_ref());
    docxcore::package::load_package(&bytes).unwrap()
}

fn editor_sect(t: &DocTab) -> String {
    editor(t)
        .doc
        .trailing_section_properties()
        .unwrap()
        .raw
        .clone()
}

#[test]
fn columns_survive_save_on_a_document_with_its_own_sect_pr() {
    let mut t = tab_with_sect_pr("columns");
    two_columns(&mut t);
    assert_eq!(t.status.as_ref(), "Columns: Two");
    assert!(t.dirty);
    let saved = save_and_reload(&mut t);
    assert!(
        saved.sect_pr().contains("w:num=\"2\""),
        "{}",
        saved.sect_pr()
    );
    assert_eq!(saved.columns(), 2);
}

#[test]
fn a_new_header_survives_save_with_its_reference() {
    let mut t = tab_with_sect_pr("header");
    assert!(open_hf_tab(&mut t, true, "default"));
    assert!(t.dirty);
    t.hf_edit.as_mut().unwrap().editor.insert_str("HDR");
    exit_hf_tab(&mut t);
    let saved = save_and_reload(&mut t);
    let sect = saved.sect_pr();
    let rid = docxcore::load::header_footer_ref_rid(sect, "headerReference", "default")
        .unwrap_or_else(|| panic!("no default headerReference: {sect}"));
    let part = hf_part_name_typed(&saved, sect, true, "default").unwrap();
    assert!(!rid.is_empty());
    let text: String = parse_hf_part(&saved, &part)
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => p.plain_text(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(text, "HDR");
}

#[test]
fn different_first_page_survives_save_both_ways() {
    let mut t = tab_with_sect_pr("title-pg");
    assert!(toggle_title_pg_tab(&mut t));
    let saved = save_and_reload(&mut t);
    assert!(
        saved.sect_pr().contains("<w:titlePg/>"),
        "{}",
        saved.sect_pr()
    );
    assert!(!toggle_title_pg_tab(&mut t));
    let saved = save_and_reload(&mut t);
    assert!(!saved.sect_pr().contains("titlePg"), "{}", saved.sect_pr());
}

#[test]
fn a_columns_change_is_one_undo_step_and_reads_follow_the_editor() {
    let mut t = tab_with_sect_pr("columns-undo");
    two_columns(&mut t);
    assert!(editor_sect(&t).contains("w:num=\"2\""));
    // Give the package a stale two-column copy, as an older path left it:
    // reads must follow the editor's, not the package's.
    let stale = editor_sect(&t);
    t.pkg.as_mut().unwrap().set_sect_pr(stale);
    assert!(editor_mut(&mut t).undo());
    assert_eq!(editor_sect(&t), SECT);
    assert_eq!(t.pkg.as_ref().unwrap().columns(), 2, "the package is stale");
    assert_eq!(final_page_geom(&t).cols, 1, "the editor's copy wins");
    two_columns(&mut t);
    assert_eq!(t.status.as_ref(), "Columns: Two");
    assert!(editor_sect(&t).contains("w:num=\"2\""));
    let saved = save_and_reload(&mut t);
    assert_eq!(saved.columns(), 2);
}

#[test]
fn one_ruler_margin_drag_survives_save_and_undoes_in_one_step() {
    let mut t = tab_with_sect_pr("margins");
    assert!(set_page_margins_tab(&mut t, true, (1440, 1080, 1440, 1800)));
    assert!(set_page_margins_tab(&mut t, false, (1440, 720, 1440, 2160)));
    assert!(t.dirty);
    let saved = save_and_reload(&mut t);
    let geom = saved.page_geom();
    assert_eq!((geom.ml, geom.mr), (2160, 720), "{}", saved.sect_pr());
    assert_eq!(
        (final_page_geom(&t).ml, final_page_geom(&t).mr),
        (2160, 720)
    );
    assert!(editor_mut(&mut t).undo());
    assert_eq!(editor_sect(&t), SECT);
    assert_eq!(
        (final_page_geom(&t).ml, final_page_geom(&t).mr),
        (1440, 1440)
    );
}

#[test]
fn reads_follow_the_editor_after_undoing_a_header_creation() {
    let mut t = tab_with_sect_pr("header-undo");
    assert!(open_hf_tab(&mut t, true, "default"));
    exit_hf_tab(&mut t);
    assert!(final_sect_pr(&t).unwrap().contains("headerReference"));
    assert!(editor_mut(&mut t).undo());
    let sect = final_sect_pr(&t).unwrap();
    assert!(!sect.contains("headerReference"), "{sect}");
    let pkg = t.pkg.as_ref().unwrap();
    assert!(hf_part_name_typed(pkg, sect, true, "default").is_none());
    assert!(header_footer_blocks_typed(pkg, sect, true, "default").is_empty());
    // Opening the header again creates a reference the editor carries.
    assert!(open_hf_tab(&mut t, true, "default"));
    assert!(editor_sect(&t).contains("headerReference"));
    let saved = save_and_reload(&mut t);
    assert!(saved.sect_pr().contains("headerReference"));
}

/// A clean tab on a .docx whose body has no `w:sectPr` at all. Loading gives
/// the body editor an explicit (empty) final section, so every undo snapshot
/// has one.
fn tab_without_sect_pr() -> DocTab {
    let t = tab_from_path(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures/basic.docx"),
    );
    let pkg = t.pkg.as_ref().unwrap();
    assert!(pkg.document.trailing_section_properties().is_none());
    assert_eq!(pkg.sect_pr(), "");
    assert_eq!(editor_sect(&t), "<w:sectPr></w:sectPr>");
    t
}

#[test]
fn undoing_columns_past_an_earlier_edit_is_undone_for_reads_and_save() {
    let mut t = tab_without_sect_pr();
    editor_mut(&mut t).insert_str("x");
    two_columns(&mut t);
    assert_eq!(final_page_geom(&t).cols, 2);
    assert!(editor_mut(&mut t).undo());
    assert!(editor_mut(&mut t).undo());
    let sect = final_sect_pr(&t).unwrap();
    assert!(!sect.contains("w:num"), "{sect}");
    let saved = save_and_reload(&mut t);
    assert_eq!(saved.columns(), 1, "{}", saved.sect_pr());
    two_columns(&mut t);
    assert_eq!(t.status.as_ref(), "Columns: Two");
}

#[test]
fn undoing_columns_on_a_document_without_a_sect_pr_is_undone_for_reads_and_save() {
    let mut t = tab_without_sect_pr();
    two_columns(&mut t);
    assert_eq!(final_page_geom(&t).cols, 2);
    assert!(editor_mut(&mut t).undo());
    let sect = final_sect_pr(&t).unwrap();
    assert!(!sect.contains("w:num"), "{sect}");
    assert_eq!(final_page_geom(&t).cols, 1);
    let saved = save_and_reload(&mut t);
    assert_eq!(saved.columns(), 1, "{}", saved.sect_pr());
    two_columns(&mut t);
    assert_eq!(t.status.as_ref(), "Columns: Two");
}

#[test]
fn undoing_a_ruler_drag_on_a_document_without_a_sect_pr_takes_one_step() {
    let mut t = tab_without_sect_pr();
    let before = final_page_geom(&t);
    assert!(set_page_margins_tab(&mut t, true, (1440, 1080, 1440, 1800)));
    assert!(set_page_margins_tab(&mut t, false, (1440, 720, 1440, 2160)));
    assert!(editor_mut(&mut t).undo());
    let after = final_page_geom(&t);
    assert_eq!((after.ml, after.mr), (before.ml, before.mr));
    let saved = save_and_reload(&mut t);
    let geom = saved.page_geom();
    assert_eq!((geom.ml, geom.mr), (before.ml, before.mr));
}

#[test]
fn undoing_a_header_creation_on_a_document_without_a_sect_pr_leaves_no_reference() {
    let mut t = tab_without_sect_pr();
    assert!(open_hf_tab(&mut t, true, "default"));
    t.hf_edit.as_mut().unwrap().editor.insert_str("HDR");
    exit_hf_tab(&mut t);
    assert!(final_sect_pr(&t).unwrap().contains("headerReference"));
    assert!(editor_mut(&mut t).undo());
    assert!(!final_sect_pr(&t).unwrap().contains("headerReference"));
    let saved = save_and_reload(&mut t);
    assert!(
        !saved.sect_pr().contains("headerReference"),
        "{}",
        saved.sect_pr()
    );
}
