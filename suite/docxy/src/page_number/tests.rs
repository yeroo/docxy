//! The Page Number menu (#650), after PAG-CASE-016: placing a number from a
//! design replaces the one placed before, Format Page Numbers writes the
//! section's `w:pgNumType`, and Remove Page Numbers leaves hand-typed `PAGE`
//! fields alone.

use super::*;
use crate::hf::tests::{caret_in, ed, ed_mut, saved, saved_hf, saved_sections, three_sections};
use core::prelude::v1::test;
use ctlcore::json::Json;
use docxcore::hf::is_page_number_open;

fn has_page_number(blocks: &[Block]) -> bool {
    blocks.iter().any(is_page_number_open)
}

fn labels(items: &[menu::MenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            menu::MenuItem::Item(e) => e.label.clone(),
            menu::MenuItem::Separator => "-".into(),
            menu::MenuItem::Heading(h) => format!("[{h}]"),
            menu::MenuItem::TableGrid { .. } => "[grid]".into(),
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

/// The saved bytes of a section's resolved header (`is_header`) or footer
/// part, as written: no re-parse to smooth over malformed XML.
fn saved_part_xml(pkg: &Package, section: usize, is_header: bool) -> String {
    let parts = docxcore::package::section_header_parts(&saved_sections(pkg), &pkg.document_rels());
    let part = &parts[section]
        .get(is_header, HeaderVariant::Default)
        .unwrap()
        .part_name;
    String::from_utf8_lossy(pkg.part(part).unwrap()).into_owned()
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
    assert!(format_of(&d, &PageNumberFormat::parse(&ed(&t).sections()[1])).is_err());
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

/// m3: a `w:fmt` the Number format list does not name is shown as itself
/// and survives OK unchanged, with no undo step.
#[test]
fn an_unlisted_number_format_survives_ok() {
    let mut t = three_sections("format-unlisted", true);
    ed_mut(&mut t).edit_sections(&[1], |raw| {
        PageNumberFormat {
            fmt: Some("decimalZero".into()),
            chap_sep: Some("colon".into()),
            chap_style: Some(12),
            ..Default::default()
        }
        .apply(raw)
    });
    let before = ed(&t).sections()[1].clone();
    caret_in(&mut t, 1);
    let d = format_dialog(&t).unwrap();
    let c = |name: &str| d.controls.iter().find(|c| c.name == name).unwrap().text();
    assert_eq!(c("format"), "decimalZero");
    assert_eq!(c("chap_style"), "12");
    assert!(
        !apply_format(ed_mut(&mut t), &d, 1).unwrap(),
        "nothing changed"
    );
    assert_eq!(ed(&t).sections()[1], before);
    // Changing only the start keeps the unlisted format.
    let mut d = format_dialog(&t).unwrap();
    d.set(
        "numbering",
        &Json::obj(vec![("value", Json::Str("Start at:".into()))]),
    )
    .unwrap();
    assert!(apply_format(ed_mut(&mut t), &d, 1).unwrap());
    let f = PageNumberFormat::parse(&ed(&t).sections()[1]);
    assert_eq!(f.fmt.as_deref(), Some("decimalZero"));
    assert_eq!((f.start, f.chap_style), (Some(1), Some(12)));
}

/// M1: rewriting a part keeps its root start tag, so prefixes its drawings
/// use stay declared.
#[test]
fn removing_page_numbers_keeps_the_parts_namespace_declarations() {
    let mut t = three_sections("namespaces", true);
    caret_in(&mut t, 0);
    place(&mut t, false, 0).unwrap();
    exit_hf_tab(&mut t);
    let part = crate::hf_tab::resolved_part(&t, 0, false, HeaderVariant::Default).unwrap();
    // Give the footer Word's root, with a drawing's namespaces, and a drawing.
    let root = "<w:ftr xmlns:wpc=\"http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas\" \
        xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" \
        xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
        xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
        xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
        xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
        xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\" mc:Ignorable=\"w14\">";
    let drawing = "<w:p><w:r><w:drawing><wp:inline><a:graphic><a:graphicData/></a:graphic></wp:inline></w:drawing></w:r></w:p>";
    let pkg = t.pkg.as_mut().unwrap();
    let old = String::from_utf8_lossy(pkg.part(&part).unwrap()).into_owned();
    let inner = &old[old
        .find('>')
        .map(|i| old[i + 1..].find('>').unwrap() + i + 2)
        .unwrap()..old.rfind("</w:ftr>").unwrap()];
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n{root}{drawing}{inner}</w:ftr>"
    );
    pkg.set_part(&part, xml.into_bytes());
    assert!(remove_all(&mut t).unwrap());
    let pkg = saved(&mut t);
    let out = String::from_utf8_lossy(pkg.part(&part).unwrap()).into_owned();
    // The root's own attributes stay as they were; `m`, which it lacked, joins them.
    assert!(
        out.contains(&format!(
            "{} xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\">",
            root.trim_end_matches('>')
        )),
        "the root stays: {out}"
    );
    assert!(out.contains("<wp:inline>"), "{out}");
    assert!(!out.contains("w:sdt"), "{out}");
    assert!(out.trim_end().ends_with("</w:ftr>"));
    // Every prefix used in the part is declared on its root.
    for prefix in ["w:", "wp:", "a:"] {
        let name = prefix.trim_end_matches(':');
        assert!(
            out.contains(&format!("xmlns:{name}=")),
            "{prefix} unbound: {out}"
        );
    }
}

/// C1: Select All + Delete in a footer holding a placed number, first or
/// last, saves a well-formed part (the control goes with its content).
#[test]
fn select_all_delete_over_a_placed_number_saves_a_well_formed_part() {
    for top in [false, true] {
        let mut t = three_sections(if top { "sel-top" } else { "sel-bottom" }, true);
        caret_in(&mut t, 0);
        place(&mut t, top, 0).unwrap();
        let editor = &mut t.hf_edit.as_mut().unwrap().editor;
        editor.select_all();
        editor.delete_forward();
        let pkg = saved(&mut t);
        let xml = saved_part_xml(&pkg, 0, top);
        assert_eq!(
            xml.matches("<w:sdt>").count(),
            xml.matches("</w:sdt>").count(),
            "{xml}"
        );
    }
}

/// C1's net: whatever the editor holds, a flush never writes one boundary
/// of a content control without the other.
#[test]
fn flushing_drops_a_content_control_boundary_left_alone() {
    let mut t = three_sections("flush-net", true);
    caret_in(&mut t, 0);
    place(&mut t, false, 0).unwrap();
    let body = &mut t.hf_edit.as_mut().unwrap().editor.doc.body;
    let open = body.iter().position(is_page_number_open).unwrap();
    body.remove(open);
    let pkg = saved(&mut t);
    let xml = saved_part_xml(&pkg, 0, false);
    assert!(!xml.contains("sdt"), "{xml}");
    assert!(xml.contains(" PAGE "), "the content stays: {xml}");
}
