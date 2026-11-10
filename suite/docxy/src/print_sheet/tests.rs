//! The embedded view's page plan (#1888) against a document tab's Print
//! Layout, the oracle: the same flow, the same header/footer part on every
//! page.

use super::*;
use crate::hf::tests::{ed, tmp_dir};
use core::prelude::v1::test;
use std::path::PathBuf;

/// What a document tab's Print Layout shows: its flow over the live body,
/// and each page's header and footer part.
type TabPlan = (page_flow::PageFlow, Vec<(Option<String>, Option<String>)>);

fn tab_plan(tab: &DocTab) -> TabPlan {
    let editor = ed(tab);
    let pkg = tab.pkg.as_ref().expect("a package");
    let pf = tab_page_flow(tab, &editor.doc.body);
    let slots = hf::slots_of(editor, &pf, pkg.has_even_odd());
    let parts = hf::resolve(editor, pkg);
    let names = slots
        .iter()
        .map(|s| {
            (
                hf::slot_part(&parts, *s, true).map(str::to_string),
                hf::slot_part(&parts, *s, false).map(str::to_string),
            )
        })
        .collect();
    (pf, names)
}

/// The view's plan of `doc` and `pkg`, in the same shape.
fn view_plan(doc: Document, pkg: &Package) -> (TabPlan, PagePlan, usize) {
    let editor = Editor::new(doc);
    let plan = page_plan(&editor, Some(pkg));
    let names = (0..plan.flow.pages.len())
        .map(|pi| {
            (
                plan.part(pi, true).map(str::to_string),
                plan.part(pi, false).map(str::to_string),
            )
        })
        .collect();
    let count = page_count(&editor, Some(pkg));
    ((plan.flow.clone(), names), plan, count)
}

/// A document tab on `bytes`, as File > Open makes it.
fn tab_of(name: &str, bytes: &[u8]) -> DocTab {
    let dir = tmp_dir(name);
    let path = dir.join("in.docx");
    std::fs::write(&path, bytes).unwrap();
    let tab = tab_from_path(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!tab.dirty, "{}", tab.status);
    tab
}

/// The view's plan of a tab's own document equals the tab's: the oracle.
fn assert_matches_tab(tab: &DocTab) -> (TabPlan, PagePlan) {
    let want = tab_plan(tab);
    let pkg = tab.pkg.as_ref().unwrap();
    let (got, plan, count) = view_plan(ed(tab).doc.clone(), pkg);
    assert_eq!(got.0, want.0, "the flow");
    assert_eq!(got.1, want.1, "each page's header and footer");
    assert_eq!(count, want.0.pages.len(), "page_count()");
    (got, plan)
}

fn part_text(plan: &PagePlan, pi: usize, is_header: bool) -> String {
    plan.blocks(pi, is_header)
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(p.plain_text()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[test]
fn the_views_pages_are_a_document_tabs_for_header_inherit_1888() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../uiharness/fixtures/header-inherit.docx");
    let tab = tab_from_path(&path);
    let ((pf, names), plan) = assert_matches_tab(&tab);
    assert_eq!(pf.pages.len(), 3);
    // Section 2 links to section 1; section 3 has its own header and
    // inherits the footer.
    let headers: Vec<String> = (0..3).map(|pi| part_text(&plan, pi, true)).collect();
    assert_eq!(headers, ["Header A", "Header A", "Header C"]);
    let footers: Vec<String> = (0..3).map(|pi| part_text(&plan, pi, false)).collect();
    assert_eq!(footers, ["Footer A", "Footer A", "Footer A"]);
    // The part names `ext-doc-view.uit` asserts.
    let names: Vec<(Option<&str>, Option<&str>)> = names
        .iter()
        .map(|(h, f)| (h.as_deref(), f.as_deref()))
        .collect();
    assert_eq!(
        names,
        [
            (Some("word/header1.xml"), Some("word/footer1.xml")),
            (Some("word/header1.xml"), Some("word/footer1.xml")),
            (Some("word/header2.xml"), Some("word/footer1.xml")),
        ]
    );
    // The fixture the harness loads into the view is the same document.
    let (doc, pkg) = crate::ext_fixture::named_document("header-inherit").unwrap();
    let (got, _, _) = view_plan(doc, &pkg);
    assert_eq!(got, tab_plan(&tab));
}

#[test]
fn the_views_pages_are_a_document_tabs_for_the_fixture_document_1888() {
    let (doc, pkg) = crate::ext_fixture::fixture_document();
    let tab = tab_of("fixture-doc", &doc_to_docx(&doc, &[], Some(&pkg)));
    let ((pf, names), _) = assert_matches_tab(&tab);
    assert!(pf.pages.len() > 1, "Line 1..80 runs past a page");
    assert!(names.iter().all(|n| *n == (None, None)), "{names:?}");
    // The view's own document splits as its saved copy does (which ends
    // with the sectPr a save writes: one block more on the last page).
    let (got, _, count) = view_plan(doc, &pkg);
    assert_eq!(count, pf.pages.len());
    let starts = |pf: &page_flow::PageFlow| page_flow::first_blocks(&pf.ranges());
    assert_eq!(starts(&got.0), starts(&pf));
}

#[test]
fn the_sections_document_has_three_sheets_of_their_own_geometry_1888() {
    let (doc, pkg) = crate::ext_fixture::sections_document();
    let tab = tab_of("sections-doc", &doc_to_docx(&doc, &[], Some(&pkg)));
    assert_matches_tab(&tab);
    let (got, plan, count) = view_plan(doc, &pkg);
    let pf = &got.0;
    assert_eq!(count, 3);
    let geom: Vec<(i32, i32, i32, i32, usize, bool)> = pf
        .pages
        .iter()
        .map(|p| {
            let s = &pf.sections[p.bands[0].section];
            (s.w, s.h, s.left, s.top, s.col_w.len(), s.sep)
        })
        .collect();
    assert_eq!(
        geom,
        [
            (12240, 15840, 1440, 1440, 1, false),
            (15840, 12240, 720, 720, 1, false),
            (12240, 15840, 1440, 1440, 2, true),
        ]
    );
    // Page 3: section 3's balanced columns, then section 4 below them.
    // The two columns are 4320 twips wide, 720 apart.
    let bands = &pf.pages[2].bands;
    assert_eq!(bands.len(), 2, "{bands:?}");
    let cols = &pf.sections[bands[0].section];
    assert_eq!(
        (cols.col_w.clone(), cols.col_gap[0]),
        (vec![4320, 4320], 720)
    );
    // ColumnSix (block 7) is in the second column.
    let (s, e) = bands[0].cols[1];
    assert!((s..e).contains(&7), "{:?}", bands[0].cols);
    // Every page shows section 1's header and footer, the footer's PAGE
    // number in a frame.
    for pi in 0..3 {
        assert_eq!(part_text(&plan, pi, true), "Sections header");
        let (flow, frames) = docxcore::frame::split_framed(plan.blocks(pi, false));
        assert_eq!((flow.len(), frames.len()), (1, 1));
    }
    assert_eq!(
        plan.page_vals
            .iter()
            .map(|v| v.page.as_str())
            .collect::<Vec<_>>(),
        ["1", "2", "3"]
    );
}

#[test]
fn a_mail_with_no_package_keeps_its_own_page_margins_1888() {
    // An HTML import ends with a sectPr of 225-twip margins (#1928); a
    // document tab's `final_sect_pr` would read none without a package.
    let doc = docxcore::import::html::import_html(b"<p>Hi</p>").unwrap();
    let editor = Editor::new(doc);
    let plan = page_plan(&editor, None);
    let s = &plan.flow.sections[0];
    assert_eq!((s.w, s.left, s.right, s.top), (12240, 225, 225, 225));
    assert_eq!(page_count(&editor, None), 1);
    assert!(plan.parts.is_empty() && plan.part(0, true).is_none());
}

#[test]
fn the_canvas_is_darker_under_a_dark_page_and_grey_otherwise_2079_1888() {
    assert_eq!(canvas_color(true, true), dark_page::CANVAS_DARK);
    assert_eq!(canvas_color(false, true), 0x2b2b2b);
    assert_eq!(canvas_color(false, false), 0x9a9a9a);
}
