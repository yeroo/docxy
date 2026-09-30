//! The Page Number menu (#650), after PAG-CASE-016: placing a number from a
//! design replaces the one placed before, Format Page Numbers writes the
//! section's `w:pgNumType`, and Remove Page Numbers leaves hand-typed `PAGE`
//! fields alone.

use super::*;
use crate::hf::tests::{caret_in, ed, ed_mut, saved, saved_hf, saved_sections, three_sections};
use core::prelude::v1::test;
use ctlcore::json::Json;
use docxcore::hf::has_page_number;

fn labels(items: &[menu::MenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            menu::MenuItem::Item(e) => e.label.clone(),
            menu::MenuItem::Separator => "-".into(),
            menu::MenuItem::Heading(h) => format!("[{h}]"),
        })
        .collect()
}

/// The blocks of a saved section's resolved header (`is_header`) or footer.
fn saved_blocks(pkg: &Package, section: usize, is_header: bool) -> Vec<Block> {
    let parts = docxcore::package::section_header_parts(&saved_sections(pkg), &pkg.document_rels());
    parts[section]
        .get(is_header, HeaderVariant::Default)
        .map(|a| parse_hf_part(pkg, &a.part_name))
        .unwrap_or_default()
}

fn page_fields(blocks: &[Block]) -> usize {
    docxcore::serialize::blocks_to_xml(blocks)
        .matches(" PAGE ")
        .count()
}

/// Criterion 15 / PAG-052 / Screen PAG14.
#[test]
fn the_menu_lists_words_galleries_then_format_and_remove() {
    let items = menu_items(None);
    assert_eq!(
        labels(&items),
        [
            "Top of Page",
            "Bottom of Page",
            "Page Margins",
            "Current Position",
            "-",
            "Format Page Numbers...",
            "Remove Page Numbers"
        ]
    );
    let menu::MenuItem::Item(bottom) = &items[1] else {
        panic!()
    };
    assert_eq!(
        labels(&bottom.submenu),
        [
            "Plain Number 1",
            "Plain Number 2",
            "Plain Number 3",
            "Page X of Y"
        ]
    );
    let menu::MenuItem::Item(margins) = &items[2] else {
        panic!()
    };
    assert!(!margins.enabled, "Page Margins is not available yet");
    // Every design can also be clicked by name on the ribbon.
    let ribbon: Vec<&str> = ribbon_items().iter().map(|c| c.label).collect();
    assert!(ribbon.contains(&"Bottom of Page: Plain Number 2"));
    assert!(ribbon.contains(&"Remove Page Numbers"));
}

/// Criterion 16 / PAG-CASE-016 steps 2-3 / PAG-056: another design replaces
/// the first, never a second number.
#[test]
fn a_second_design_replaces_the_first_and_moving_it_leaves_one() {
    let mut t = three_sections("place", true);
    caret_in(&mut t, 0);
    place(&mut t, false, 0).unwrap();
    let h = t.hf_edit.as_ref().unwrap();
    assert_eq!((h.section, h.is_header), (0, false), "the footer is open");
    let pkg = saved(&mut t);
    let footer = saved_blocks(&pkg, 0, false);
    assert!(has_page_number(&footer));
    assert_eq!(page_fields(&footer), 1);
    // "Footer A" stays, the number goes after it.
    assert!(saved_hf(&pkg, 0, false).starts_with("Footer A"));
    place(&mut t, false, 1).unwrap();
    let pkg = saved(&mut t);
    let footer = saved_blocks(&pkg, 0, false);
    assert_eq!(page_fields(&footer), 1, "one number, not two");
    let xml = docxcore::serialize::blocks_to_xml(&footer);
    assert!(xml.contains("w:jc w:val=\"center\""), "{xml}");
    assert!(xml.contains("Page Numbers (Bottom of Page)"));
    // Top of Page moves it into the header.
    place(&mut t, true, 3).unwrap();
    let pkg = saved(&mut t);
    assert_eq!(page_fields(&saved_blocks(&pkg, 0, false)), 0);
    let header = saved_blocks(&pkg, 0, true);
    assert_eq!(page_fields(&header), 1);
    assert!(docxcore::serialize::blocks_to_xml(&header).contains("NUMPAGES"));
    assert!(
        saved_hf(&pkg, 0, true).ends_with("Header A"),
        "number first"
    );
}

#[test]
fn placing_in_a_document_without_a_footer_creates_one_from_section_one() {
    let mut t = three_sections("place-new", false);
    caret_in(&mut t, 2);
    place(&mut t, false, 1).unwrap();
    let sections = ed(&t).sections();
    assert!(docxcore::sect::hf_reference(&sections[0], false, "default").is_some());
    assert!(!sections[2].contains("footerReference"));
    let pkg = saved(&mut t);
    let footer = saved_blocks(&pkg, 2, false);
    // Only the number: the new part's empty paragraph is replaced.
    assert_eq!(footer.len(), 3, "boundary, paragraph, boundary");
    assert_eq!(saved_hf(&pkg, 2, false), "|1|");
}

/// Criterion 17 / PAG-058 / Screen PAG15 / PAG-CASE-016 step 4.
#[test]
fn format_page_numbers_opens_on_words_defaults_and_writes_pg_num_type() {
    let mut t = three_sections("format", true);
    caret_in(&mut t, 1);
    let mut d = format_dialog(&t).unwrap();
    assert_eq!(d.title, "Page Number Format");
    let c = |d: &Dialog, name: &str| d.controls.iter().find(|c| c.name == name).unwrap().clone();
    assert_eq!(c(&d, "format").text(), "1, 2, 3, ...");
    assert_eq!(c(&d, "chapter").value, Value::Bool(false));
    assert_eq!(c(&d, "chap_style").text(), "Heading 1");
    assert_eq!(c(&d, "chap_sep").text(), "- (hyphen)");
    for name in ["chap_style", "chap_sep", "examples"] {
        assert!(!c(&d, name).enabled, "{name} starts disabled");
    }
    assert_eq!(c(&d, "numbering").text(), "Continue from previous section");
    assert!(!c(&d, "start").enabled);
    assert_eq!(c(&d, "start").text(), "");
    // i, ii, iii and Start at 5.
    d.set(
        "format",
        &Json::obj(vec![("value", Json::Str("i, ii, iii, ...".into()))]),
    )
    .unwrap();
    d.set(
        "numbering",
        &Json::obj(vec![("value", Json::Str("Start at:".into()))]),
    )
    .unwrap();
    assert!(c(&d, "start").enabled);
    assert_eq!(c(&d, "start").text(), "1", "Start at fills in 1");
    d.set("start", &Json::obj(vec![("value", Json::Str("5".into()))]))
        .unwrap();
    assert!(apply_format(ed_mut(&mut t), &d, 1).unwrap());
    let sections = ed(&t).sections();
    assert!(
        sections[1].contains("<w:pgNumType w:fmt=\"lowerRoman\" w:start=\"5\"/>"),
        "{}",
        sections[1]
    );
    assert!(
        !sections[0].contains("pgNumType"),
        "only the caret's section"
    );
    let pkg = saved(&mut t);
    assert!(saved_sections(&pkg)[1].contains("w:fmt=\"lowerRoman\" w:start=\"5\""));
    // The dialog reads the section back; chapter numbers enable their controls.
    let mut d = format_dialog(&t).unwrap();
    assert_eq!(c(&d, "format").text(), "i, ii, iii, ...");
    assert_eq!(c(&d, "start").text(), "5");
    d.set("chapter", &Json::obj(vec![("value", Json::Bool(true))]))
        .unwrap();
    assert!(c(&d, "chap_style").enabled && c(&d, "chap_sep").enabled);
    assert!(apply_format(ed_mut(&mut t), &d, 1).unwrap());
    assert!(ed(&t).sections()[1].contains("w:chapStyle=\"1\" w:chapSep=\"hyphen\""));
    // Back to the defaults removes the element; one undo step each.
    let mut d = format_dialog(&t).unwrap();
    d.set("chapter", &Json::obj(vec![("value", Json::Bool(false))]))
        .unwrap();
    d.set(
        "format",
        &Json::obj(vec![("value", Json::Str("1, 2, 3, ...".into()))]),
    )
    .unwrap();
    d.set(
        "numbering",
        &Json::obj(vec![(
            "value",
            Json::Str("Continue from previous section".into()),
        )]),
    )
    .unwrap();
    assert!(apply_format(ed_mut(&mut t), &d, 1).unwrap());
    assert!(!ed(&t).sections()[1].contains("pgNumType"));
    assert!(ed_mut(&mut t).undo());
    assert!(ed(&t).sections()[1].contains("w:chapStyle"));
    // A bad Start at refuses OK.
    let mut d = format_dialog(&t).unwrap();
    d.set(
        "start",
        &Json::obj(vec![("value", Json::Str(String::new()))]),
    )
    .unwrap();
    assert!(format_of(&d).is_err());
}

/// Criterion 18 / PAG-061 / PAG-CASE-016 step 5.
#[test]
fn remove_page_numbers_removes_only_placed_numbers() {
    let mut t = three_sections("remove", true);
    // A PAGE field typed into the body and one typed into section 3's header.
    let field = || Inline::Field {
        raw: "<w:fldSimple w:instr=\" PAGE \"><w:r><w:t>1</w:t></w:r></w:fldSimple>".into(),
        text: "1".into(),
    };
    caret_in(&mut t, 0);
    ed_mut(&mut t).paste(&Clip {
        paras: vec![vec![field()]],
    });
    caret_in(&mut t, 2);
    crate::hf_tab::hf_apply(&mut t, crate::hf_tab::HfAct::Edit(true)).unwrap();
    t.hf_edit.as_mut().unwrap().editor.paste(&Clip {
        paras: vec![vec![field()]],
    });
    exit_hf_tab(&mut t);
    // Placed numbers in section 1's footer and section 3's header.
    caret_in(&mut t, 0);
    place(&mut t, false, 0).unwrap();
    exit_hf_tab(&mut t);
    caret_in(&mut t, 2);
    place(&mut t, true, 2).unwrap();
    let pkg = saved(&mut t);
    assert_eq!(page_fields(&saved_blocks(&pkg, 2, true)), 2);
    assert!(remove_all(&mut t).unwrap());
    let pkg = saved(&mut t);
    assert_eq!(page_fields(&saved_blocks(&pkg, 0, false)), 0);
    assert_eq!(saved_hf(&pkg, 0, false), "Footer A");
    let header3 = saved_blocks(&pkg, 2, true);
    assert_eq!(page_fields(&header3), 1, "the hand-typed one stays");
    assert!(!has_page_number(&header3));
    // The body's field stays.
    let body = docxcore::serialize::blocks_to_xml(&pkg.document.body);
    assert_eq!(body.matches(" PAGE ").count(), 1);
    // The open header shows the removal.
    let h = t.hf_edit.as_ref().unwrap();
    assert!(!has_page_number(&h.editor.doc.body));
    assert!(!remove_all(&mut t).unwrap(), "nothing left to remove");
}
