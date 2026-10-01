use super::super::print_setup_tests::book;
use super::super::{SheetPackage, load_xlsx, save_xlsx};
use crate::print::setup::{HfSlot, Orientation, PageOrder, PageSetup};

const PART: &str = "xl/worksheets/sheet1.xml";
const DATA: &str = r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>"#;
const MARGINS: &str =
    r#"<pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#;

/// One sheet, `Report`, whose part is `before` + sheetData + `after`.
fn load(before: &str, after: &str) -> SheetPackage {
    let body = format!("{before}{DATA}{after}");
    load_xlsx(&book("", &[("Report", Some(&body))])).expect("fixture loads")
}

fn saved(pkg: &SheetPackage) -> String {
    let re = load_xlsx(&save_xlsx(pkg)).expect("saved file reloads");
    String::from_utf8_lossy(re.part(PART).expect("part")).into_owned()
}

fn setup(pkg: &mut SheetPackage) -> &mut PageSetup {
    &mut pkg.workbook.sheets[0].page_setup
}

const RICH: &str = concat!(
    r#"<printOptions gridLines="true" headings="1"/>"#,
    r#"<pageMargins left="0.25" right="0.25" top="1" bottom="1" header="0.5" footer="0.5"/>"#,
    r#"<pageSetup paperSize="9" orientation="landscape" fitToHeight="0" horizontalDpi="600" verticalDpi="600" firstPageNumber="1" r:id="rId1"/>"#,
    r#"<headerFooter differentOddEven="1"><oddHeader>&amp;CPage &amp;P of &amp;N</oddHeader><oddFooter>&amp;L&amp;Z&amp;F</oddFooter></headerFooter>"#,
    r#"<legacyDrawingHF r:id="rId2"/>"#,
);
const RICH_PR: &str =
    r#"<sheetPr><tabColor rgb="FFFF0000"/><pageSetUpPr fitToPage="1"/></sheetPr>"#;

#[test]
fn reads_every_field_with_schema_defaults_for_absent_attributes() {
    let pkg = load(RICH_PR, RICH);
    let s = &pkg.workbook.sheets[0].page_setup;
    assert!(s.grid_lines && s.headings && !s.h_centered);
    assert_eq!(
        (s.margins.left, s.margins.top, s.margins.footer),
        (0.25, 1.0, 0.5)
    );
    assert_eq!(s.paper_size, 9);
    assert_eq!(s.orientation, Orientation::Landscape);
    // fitToWidth is absent: 1 page. fitToHeight="0": Automatic.
    assert_eq!((s.fit_width, s.fit_height), (1, 0));
    assert!(s.fit_to_page);
    // firstPageNumber without useFirstPageNumber is Auto.
    assert_eq!(s.first_page_number, None);
    assert!(s.header_footer.different_odd_even);
    assert_eq!(
        s.header_footer.odd_header.as_deref(),
        Some("&CPage &P of &N")
    );
    assert_eq!(s.header_footer.odd_footer.as_deref(), Some("&L&Z&F"));
    assert_eq!(s.header_footer.even_header, None);
    assert_eq!(pkg.workbook.sheets[0].page_setup_loaded, *s);
}

#[test]
fn a_custom_views_page_setup_is_not_the_sheets() {
    let view = r#"<customSheetViews><customSheetView guid="{1}"><pageSetup orientation="landscape"/></customSheetView></customSheetViews>"#;
    let pkg = load("", view);
    assert_eq!(pkg.workbook.sheets[0].page_setup, PageSetup::default());
}

#[test]
fn an_untouched_page_setup_saves_byte_identical() {
    let pkg = load(RICH_PR, RICH);
    let ws = saved(&pkg);
    assert!(ws.contains(RICH), "{ws}");
    assert!(ws.contains(RICH_PR), "{ws}");
}

#[test]
fn one_changed_field_rewrites_only_its_attribute() {
    let mut pkg = load(RICH_PR, RICH);
    setup(&mut pkg).orientation = Orientation::Portrait;
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<pageSetup orientation="portrait" paperSize="9" fitToHeight="0" horizontalDpi="600" verticalDpi="600" firstPageNumber="1" r:id="rId1"/>"#),
        "{ws}"
    );
    // Everything else as it was, foreign spellings included.
    assert!(
        ws.contains(r#"<printOptions gridLines="true" headings="1"/>"#),
        "{ws}"
    );
    assert!(ws.contains(RICH_PR), "{ws}");
}

#[test]
fn a_field_reset_to_its_default_drops_the_attribute_and_then_the_element() {
    let mut pkg = load(
        "",
        &format!(r#"{MARGINS}<pageSetup scale="50" pageOrder="overThenDown"/>"#),
    );
    let s = setup(&mut pkg);
    assert_eq!((s.scale, s.page_order), (50, PageOrder::OverThenDown));
    s.scale = 100;
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<pageSetup pageOrder="overThenDown"/>"#),
        "{ws}"
    );
    setup(&mut pkg).page_order = PageOrder::DownThenOver;
    let ws = saved(&pkg);
    assert!(!ws.contains("pageSetup"), "{ws}");
    assert!(ws.contains(MARGINS), "margins never go: {ws}");
}

#[test]
fn fit_to_automatic_is_written_as_zero_and_one_page_drops_the_attribute() {
    let mut pkg = load("", MARGINS);
    let s = setup(&mut pkg);
    s.fit_to_page = true;
    s.fit_height = 0;
    let ws = saved(&pkg);
    assert!(ws.contains(r#"<pageSetup fitToHeight="0"/>"#), "{ws}");
    assert!(
        ws.contains(r#"<sheetPr><pageSetUpPr fitToPage="1"/></sheetPr>"#),
        "{ws}"
    );
    assert!(
        ws.find("<sheetPr").unwrap() < ws.find("<sheetData").unwrap(),
        "{ws}"
    );
    let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
    setup(&mut pkg).fit_height = 1;
    let ws = saved(&pkg);
    assert!(!ws.contains("<pageSetup"), "{ws}");
}

#[test]
fn a_new_element_lands_at_its_schema_position() {
    let mut pkg = load(
        "",
        &format!(
            r#"{MARGINS}<headerFooter><oddHeader>x</oddHeader></headerFooter><drawing r:id="rId1"/>"#
        ),
    );
    setup(&mut pkg).orientation = Orientation::Landscape;
    setup(&mut pkg).grid_lines = true;
    let ws = saved(&pkg);
    let at = |t: &str| ws.find(t).unwrap_or_else(|| panic!("{t} in {ws}"));
    assert!(at("<printOptions gridLines=\"1\"/>") < at("<pageMargins"));
    assert!(at("<pageMargins") < at(r#"<pageSetup orientation="landscape"/>"#));
    assert!(at("<pageSetup") < at("<headerFooter"));
    assert!(at("<headerFooter") < at("<drawing"));
}

#[test]
fn margins_on_a_sheet_without_them_are_written_whole() {
    let mut pkg = load("", "");
    setup(&mut pkg).margins.left = 1.0;
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<pageMargins left="1" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#),
        "{ws}"
    );
}

#[test]
fn printoptions_turned_all_off_goes() {
    let mut pkg = load("", &format!(r#"<printOptions gridLines="1"/>{MARGINS}"#));
    setup(&mut pkg).grid_lines = false;
    let ws = saved(&pkg);
    assert!(!ws.contains("printOptions"), "{ws}");
}

#[test]
fn first_page_number_writes_both_attributes_and_auto_keeps_the_number() {
    let mut pkg = load(
        "",
        &format!(r#"{MARGINS}<pageSetup firstPageNumber="1" r:id="rId1"/>"#),
    );
    setup(&mut pkg).first_page_number = Some(5);
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<pageSetup useFirstPageNumber="1" firstPageNumber="5" r:id="rId1"/>"#),
        "{ws}"
    );
    let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
    assert_eq!(setup(&mut pkg).first_page_number, Some(5));
    setup(&mut pkg).first_page_number = None;
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<pageSetup firstPageNumber="5" r:id="rId1"/>"#),
        "{ws}"
    );
}

#[test]
fn a_new_header_child_goes_in_sequence_and_the_others_stay_byte_identical() {
    let hf = r#"<headerFooter><oddHeader>&amp;C&amp;"Arial,Bold"Top</oddHeader>  <oddFooter>&amp;R&amp;P</oddFooter><firstFooter>f</firstFooter></headerFooter>"#;
    let mut pkg = load("", &format!("{MARGINS}{hf}"));
    *setup(&mut pkg).header_footer.slot_mut(HfSlot::EvenHeader) = Some("&LEven & odd".into());
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<headerFooter><oddHeader>&amp;C&amp;"Arial,Bold"Top</oddHeader>  <oddFooter>&amp;R&amp;P</oddFooter><evenHeader>&amp;LEven &amp; odd</evenHeader><firstFooter>f</firstFooter></headerFooter>"#),
        "{ws}"
    );
}

#[test]
fn header_flags_and_children_removed_drop_the_element() {
    let hf = r#"<headerFooter differentFirst="1"><firstHeader>a</firstHeader></headerFooter>"#;
    let mut pkg = load("", &format!("{MARGINS}{hf}"));
    let h = &mut setup(&mut pkg).header_footer;
    h.different_first = false;
    h.first_header = None;
    let ws = saved(&pkg);
    assert!(!ws.contains("headerFooter"), "{ws}");
    assert!(ws.contains(MARGINS), "{ws}");
}

#[test]
fn a_header_on_a_sheet_with_none_creates_the_element() {
    let mut pkg = load("", MARGINS);
    let h = &mut setup(&mut pkg).header_footer;
    h.odd_header = Some("&CR&&D &P of &N".into());
    h.scale_with_doc = false;
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<headerFooter scaleWithDoc="0"><oddHeader>&amp;CR&amp;&amp;D &amp;P of &amp;N</oddHeader></headerFooter>"#),
        "{ws}"
    );
}

#[test]
fn fit_to_page_joins_an_existing_sheet_pr_after_tab_color_and_leaves_cleanly() {
    let pr = r#"<sheetPr codeName="S"><tabColor rgb="FF00FF00"/></sheetPr>"#;
    let mut pkg = load(pr, MARGINS);
    setup(&mut pkg).fit_to_page = true;
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<sheetPr codeName="S"><tabColor rgb="FF00FF00"/><pageSetUpPr fitToPage="1"/></sheetPr>"#),
        "{ws}"
    );
    let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
    setup(&mut pkg).fit_to_page = false;
    let ws = saved(&pkg);
    assert!(ws.contains(pr), "{ws}");

    // A self-closing sheetPr is opened up; a bare one goes when cleared.
    let mut pkg = load(r#"<sheetPr filterMode="0"/>"#, MARGINS);
    setup(&mut pkg).fit_to_page = true;
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<sheetPr filterMode="0"><pageSetUpPr fitToPage="1"/></sheetPr>"#),
        "{ws}"
    );
    let mut pkg = load(
        r#"<sheetPr><pageSetUpPr fitToPage="1"/></sheetPr>"#,
        MARGINS,
    );
    setup(&mut pkg).fit_to_page = false;
    let ws = saved(&pkg);
    assert!(!ws.contains("sheetPr"), "{ws}");
}

#[test]
fn a_prefixed_part_gets_prefixed_header_children() {
    let ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let xml = format!(
        r#"<?xml version="1.0"?><x:worksheet xmlns:x="{ns}"><x:sheetData/><x:pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/><x:headerFooter><x:oddHeader>a</x:oddHeader></x:headerFooter></x:worksheet>"#
    );
    let mut pkg = load("", MARGINS);
    let i = pkg.parts.iter().position(|(n, _)| n == PART).unwrap();
    pkg.parts[i].1 = xml.into_bytes();
    let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
    assert_eq!(
        setup(&mut pkg).header_footer.odd_header.as_deref(),
        Some("a")
    );
    setup(&mut pkg).header_footer.odd_footer = Some("b".into());
    let ws = saved(&pkg);
    assert!(
        ws.contains(
            "<x:headerFooter><x:oddHeader>a</x:oddHeader><x:oddFooter>b</x:oddFooter></x:headerFooter>"
        ),
        "{ws}"
    );
}

#[test]
fn a_sheet_added_in_session_writes_nothing_until_set() {
    let mut pkg = load("", MARGINS);
    let i = pkg.add_sheet("New");
    let saved_new = |pkg: &SheetPackage| {
        let re = load_xlsx(&save_xlsx(pkg)).unwrap();
        let part = &re.sheet_parts[i];
        String::from_utf8_lossy(re.part(part).unwrap()).into_owned()
    };
    let ws = saved_new(&pkg);
    assert!(
        !ws.contains("pageSetup") && !ws.contains("pageMargins"),
        "{ws}"
    );
    pkg.workbook.sheets[i].page_setup.orientation = Orientation::Landscape;
    let ws = saved_new(&pkg);
    assert!(
        ws.contains(r#"<pageSetup orientation="landscape"/>"#),
        "{ws}"
    );
}

const AREA: &str =
    r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Report!$A$1:$D$20</definedName>"#;
const TITLES: &str = r#"<definedName name="_xlnm.Print_Titles" localSheetId="0">Report!$A:$A,Report!$1:$2</definedName>"#;
const TOTAL: &str = r#"<definedName name="Total">Report!$B$2</definedName>"#;

fn workbook_xml(pkg: &SheetPackage) -> String {
    let re = load_xlsx(&save_xlsx(pkg)).expect("saved file reloads");
    String::from_utf8_lossy(re.part("xl/workbook.xml").expect("workbook")).into_owned()
}

#[test]
fn a_cleared_print_area_and_titles_leave_workbook_xml() {
    use crate::print::area::{PrintTitles, clear_print_area, set_print_titles};
    let names = format!("{TOTAL}{AREA}{TITLES}");
    let mut pkg = load_xlsx(&book(&names, &[("Report", Some(DATA))])).unwrap();
    assert!(clear_print_area(&mut pkg.workbook, 0));
    let wb = workbook_xml(&pkg);
    assert!(!wb.contains("Print_Area"), "{wb}");
    assert!(wb.contains(TOTAL) && wb.contains(TITLES), "{wb}");
    set_print_titles(&mut pkg.workbook, 0, PrintTitles::default());
    let wb = workbook_xml(&pkg);
    assert!(!wb.contains("Print_Titles"), "{wb}");
    assert!(
        wb.contains(&format!("<definedNames>{TOTAL}</definedNames>")),
        "{wb}"
    );
}

#[test]
fn a_cleared_then_restored_print_area_saves_byte_identical() {
    use crate::print::area::clear_print_area;
    let names = format!("{AREA}{TITLES}");
    let mut pkg = load_xlsx(&book(&names, &[("Report", Some(DATA))])).unwrap();
    let before = workbook_xml(&pkg);
    let snapshot = pkg.workbook.defined_names.clone();
    clear_print_area(&mut pkg.workbook, 0);
    // Undo puts the names back as they were.
    pkg.workbook.defined_names = snapshot;
    assert_eq!(workbook_xml(&pkg), before);
}

#[test]
fn a_set_print_area_is_written_and_a_new_one_added() {
    use crate::print::area::{add_print_area, set_print_area};
    let mut pkg = load_xlsx(&book(AREA, &[("Report", Some(DATA))])).unwrap();
    add_print_area(&mut pkg.workbook, 0, (0, 5, 4, 6));
    let wb = workbook_xml(&pkg);
    assert!(
        wb.contains(r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Report!$A$1:$D$20,Report!$F$1:$G$5</definedName>"#),
        "{wb}"
    );
    let mut pkg = load_xlsx(&book("", &[("Report", Some(DATA))])).unwrap();
    set_print_area(&mut pkg.workbook, 0, &[(0, 0, 9, 2)]);
    let wb = workbook_xml(&pkg);
    assert!(
        wb.contains(r#"<definedName name="_xlnm.Print_Area" localSheetId="0">Report!$A$1:$C$10</definedName>"#),
        "{wb}"
    );
}

#[test]
fn an_unaligned_workbook_keeps_a_print_area_it_cannot_place() {
    use crate::print::area::clear_print_area;
    // The second sheet's part is missing, so localSheetId can't be trusted.
    let mut pkg = load_xlsx(&book(AREA, &[("Report", Some(DATA)), ("Gone", None)])).unwrap();
    clear_print_area(&mut pkg.workbook, 0);
    let wb = workbook_xml(&pkg);
    assert!(wb.contains(AREA), "{wb}");
}

#[test]
fn inserted_breaks_create_their_elements_and_reset_drops_them() {
    // FIL-CASE-046 on a sheet with no breaks.
    use crate::print::area::{insert_page_break, remove_page_break, reset_page_breaks};
    let mut pkg = load("", &format!(r#"{MARGINS}<drawing r:id="rId1"/>"#));
    let sheet = &mut pkg.workbook.sheets[0];
    insert_page_break(sheet, 13, 0);
    insert_page_break(sheet, 0, 3);
    insert_page_break(sheet, 29, 5);
    let ws = saved(&pkg);
    let rows = concat!(
        r#"<rowBreaks count="2" manualBreakCount="2"><brk id="13" max="16383" man="1"/>"#,
        r#"<brk id="29" max="16383" man="1"/></rowBreaks>"#,
        r#"<colBreaks count="2" manualBreakCount="2"><brk id="3" max="1048575" man="1"/>"#,
        r#"<brk id="5" max="1048575" man="1"/></colBreaks><drawing"#,
    );
    assert!(ws.contains(&format!("{MARGINS}{rows}")), "{ws}");

    let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
    remove_page_break(&mut pkg.workbook.sheets[0], 29, 5);
    let ws = saved(&pkg);
    assert!(
        ws.contains(r#"<rowBreaks count="1" manualBreakCount="1"><brk id="13" max="16383" man="1"/></rowBreaks><colBreaks count="1" manualBreakCount="1"><brk id="3" max="1048575" man="1"/></colBreaks>"#),
        "{ws}"
    );
    reset_page_breaks(&mut pkg.workbook.sheets[0]);
    let ws = saved(&pkg);
    assert!(!ws.contains("Breaks") && !ws.contains("<brk"), "{ws}");
}
