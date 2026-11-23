//! #831: `add_rel` and `add_workbook_rel` write every relationship id they
//! hand back, whatever shape the rels part has: spliced in before its
//! `</Relationships>`, into a self-closed root they open, or into a part
//! rebuilt from the relationships the loader reads in it.

use super::*;
use crate::sheet::{Cell, CellValue};

const PKG: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const WB_RELS: &str = "xl/_rels/workbook.xml.rels";
const SHEET: &str = "xl/worksheets/sheet1.xml";
const SHEET_RELS: &str = "xl/worksheets/_rels/sheet1.xml.rels";
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

fn text_of(parts: &[(String, Vec<u8>)], name: &str) -> String {
    let (_, bytes) = parts
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("{name} missing"));
    String::from_utf8(bytes.clone()).unwrap()
}

fn set(parts: &mut Vec<(String, Vec<u8>)>, name: &str, xml: &str) {
    match parts.iter_mut().find(|(n, _)| n == name) {
        Some(p) => p.1 = xml.as_bytes().to_vec(),
        None => parts.push((name.to_string(), xml.as_bytes().to_vec())),
    }
}

/// `xml` cut short just before its `</Relationships>`.
fn cut_short(xml: &str) -> String {
    let at = xml.rfind("</Relationships>").expect("a rels part");
    xml[..at].to_string()
}

/// One relationship as [`parse_rels_mode`] reads it.
fn rel(
    id: &str,
    ty: &str,
    target: &str,
    mode: Option<&str>,
) -> (String, String, String, Option<String>) {
    (id.into(), ty.into(), target.into(), mode.map(Into::into))
}

#[test]
fn add_rel_opens_a_self_closed_rels_root() {
    let mut parts = Vec::new();
    set(
        &mut parts,
        "x.rels",
        &format!("{DECL}<Relationships xmlns=\"{PKG}\"/>"),
    );
    let rid = add_rel(&mut parts, "x.rels", "urn:t", "a.xml");
    assert_eq!(rid, "rId1");
    let xml = text_of(&parts, "x.rels");
    assert_eq!(
        parse_rels(&xml),
        vec![("rId1".into(), "urn:t".into(), "a.xml".into())],
        "{xml}"
    );
    assert!(
        xml.starts_with(&format!(
            "{DECL}<Relationships xmlns=\"{PKG}\"><Relationship "
        )),
        "{xml}"
    );
    assert!(xml.ends_with("</Relationships>"), "{xml}");
}

#[test]
fn add_rel_rebuilds_a_cut_short_rels_part() {
    // The last element is cut inside its attributes, so the loader never
    // read it; its rId9 is still not handed out again. The one with no Id
    // is kept under a fresh one: the loader finds styles by their type.
    let xml = format!(
        "{DECL}<Relationships xmlns=\"{PKG}\">\
         <Relationship Id=\"rId1\" Type=\"{R}/worksheet\" Target=\"worksheets/sheet1.xml\"/>\
         <Relationship Id=\"rId7\" Type=\"{R}/hyperlink\" Target=\"https://e.example/?a=1&amp;b=&quot;2&quot;\" TargetMode=\"External\"/>\
         <Relationship Type=\"{R}/styles\" Target=\"styles.xml\"/>\
         <Relationship Id=\"rId9\" Type=\"{R}/theme\" Targ"
    );
    let mut parts = Vec::new();
    set(&mut parts, "x.rels", &xml);
    let before = parse_rels_mode(&xml);
    assert_eq!(before.len(), 3, "{before:?}");

    let rid = add_rel(
        &mut parts,
        "x.rels",
        &format!("{R}/theme"),
        "theme/theme1.xml",
    );
    assert_eq!(rid, "rId10");
    let out = text_of(&parts, "x.rels");
    assert_eq!(
        parse_rels_mode(&out),
        vec![
            rel(
                "rId1",
                &format!("{R}/worksheet"),
                "worksheets/sheet1.xml",
                None
            ),
            rel(
                "rId7",
                &format!("{R}/hyperlink"),
                "https://e.example/?a=1&b=\"2\"",
                Some("External")
            ),
            rel("rId11", &format!("{R}/styles"), "styles.xml", None),
            rel("rId10", &format!("{R}/theme"), "theme/theme1.xml", None),
        ],
        "{out}"
    );
    assert!(
        out.starts_with(&format!("{DECL}<Relationships xmlns=\"{PKG}\">")),
        "{out}"
    );
    assert!(out.ends_with("</Relationships>"), "{out}");
    assert!(!out.contains("Id=\"\""), "{out}");
}

#[test]
fn add_rel_rebuilds_a_prefixed_rels_part() {
    let xml = format!(
        "{DECL}<r:Relationships xmlns:r=\"{PKG}\">\
         <r:Relationship Id=\"rId1\" Type=\"{R}/worksheet\" Target=\"worksheets/sheet1.xml\"/>\
         </r:Relationships>"
    );
    let mut parts = Vec::new();
    set(&mut parts, "x.rels", &xml);
    let rid = add_rel(&mut parts, "x.rels", &format!("{R}/styles"), "styles.xml");
    assert_eq!(rid, "rId2");
    let out = text_of(&parts, "x.rels");
    assert_eq!(
        parse_rels_mode(&out),
        vec![
            rel(
                "rId1",
                &format!("{R}/worksheet"),
                "worksheets/sheet1.xml",
                None
            ),
            rel("rId2", &format!("{R}/styles"), "styles.xml", None),
        ],
        "{out}"
    );
    assert!(
        out.contains(&format!("<Relationships xmlns=\"{PKG}\">")),
        "{out}"
    );
    assert!(!out.contains("r:Relationship"), "{out}");
}

#[test]
fn add_rel_on_a_well_formed_part_keeps_its_bytes() {
    let xml = format!(
        "{DECL}<Relationships xmlns=\"{PKG}\"><!-- kept --><Relationship Id=\"rId1\" Type=\"urn:a\" Target=\"a.xml\"/></Relationships>\n"
    );
    let mut parts = Vec::new();
    set(&mut parts, "x.rels", &xml);
    assert_eq!(add_rel(&mut parts, "x.rels", "urn:b", "b.xml"), "rId2");
    assert_eq!(
        text_of(&parts, "x.rels"),
        xml.replace(
            "</Relationships>",
            "<Relationship Id=\"rId2\" Type=\"urn:b\" Target=\"b.xml\"/></Relationships>"
        )
    );
    // Already related: nothing written, "" back.
    assert_eq!(add_rel(&mut parts, "x.rels", "urn:b", "b.xml"), "");
}

#[test]
fn add_workbook_rel_creates_a_missing_workbook_rels_part() {
    let mut parts = new_xlsx().parts;
    parts.retain(|(n, _)| n != WB_RELS);
    let rid = add_workbook_rel(&mut parts, &format!("{R}/styles"), "xl/styles.xml");
    // workbook.xml still names rId1 for its sheet, whose rel went with the
    // part: the new rel takes the next id.
    assert_eq!(rid, "rId2");
    assert_eq!(
        parse_rels(&text_of(&parts, WB_RELS)),
        vec![("rId2".into(), format!("{R}/styles"), "styles.xml".into())]
    );
}

#[test]
fn add_sheet_with_a_cut_short_workbook_rels_reaches_the_new_sheet() {
    let mut pkg = new_xlsx();
    pkg.workbook.sheets[0].set_cell(0, 0, Cell::number(7.0));
    let mut parts = load_xlsx(&save_xlsx(&pkg)).unwrap().parts;
    let rels = cut_short(&text_of(&parts, WB_RELS));
    set(&mut parts, WB_RELS, &rels);
    let mut pkg = load_xlsx(&write_zip(&parts)).expect("a cut-short workbook rels part loads");
    assert_eq!(pkg.workbook.sheets.len(), 1);

    let at = pkg.add_sheet("Added");
    pkg.workbook.sheets[at].set_cell(1, 1, Cell::number(3.0));
    let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
    let names: Vec<&str> = re.workbook.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Sheet1", "Added"]);
    assert_eq!(
        re.workbook.sheets[0].cell(0, 0).unwrap().value,
        CellValue::Number(7.0)
    );
    assert_eq!(
        re.workbook.sheets[1].cell(1, 1).unwrap().value,
        CellValue::Number(3.0)
    );
}

#[test]
fn add_sheet_skips_a_part_name_a_dangling_rel_targets() {
    // workbook.xml still names "Gone", whose rel targets a sheet2.xml the
    // package lacks: the loader skips it, the text keeps both.
    let mut parts = new_xlsx().parts;
    let wb = text_of(&parts, "xl/workbook.xml").replace(
        "</sheets>",
        "<sheet name=\"Gone\" sheetId=\"2\" r:id=\"rId5\"/></sheets>",
    );
    set(&mut parts, "xl/workbook.xml", &wb);
    let rels = text_of(&parts, WB_RELS).replace(
        "</Relationships>",
        &format!(
            "<Relationship Id=\"rId5\" Type=\"{R}/worksheet\" Target=\"worksheets/sheet2.xml\"/></Relationships>"
        ),
    );
    set(&mut parts, WB_RELS, &rels);
    let mut pkg = load_xlsx(&write_zip(&parts)).unwrap();
    assert_eq!(pkg.workbook.sheets.len(), 1);

    let at = pkg.add_sheet("Added");
    assert_eq!(pkg.sheet_parts[at], "xl/worksheets/sheet3.xml");
    pkg.workbook.sheets[at].set_cell(0, 0, Cell::number(5.0));
    let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
    let wb = text_of(&re.parts, "xl/workbook.xml");
    let mut rids: Vec<String> = Vec::new();
    let mut p = XmlParser::new(&wb);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "sheet" => rids.push(decode(p.attr("r:id"))),
            Event::Eof => break,
            _ => {}
        }
    }
    assert!(rids.iter().all(|r| !r.is_empty()), "{wb}");
    let mut unique = rids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), rids.len(), "one <sheet> per r:id: {wb}");
    let names: Vec<&str> = re.workbook.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Sheet1", "Added"]);
    assert_eq!(
        re.workbook.sheets[1].cell(0, 0).unwrap().value,
        CellValue::Number(5.0)
    );
}

/// A loaded workbook whose Sheet1 holds `extra` after its data, with a
/// rels part relating rId4 to a table part the package lacks.
fn with_a_dangling_table_rel(extra: &str) -> SheetPackage {
    let mut parts = new_xlsx().parts;
    set(
        &mut parts,
        SHEET,
        &format!(
            "{DECL}<worksheet xmlns=\"{SML}\" xmlns:r=\"{R}\"><sheetData/>{extra}</worksheet>"
        ),
    );
    set(
        &mut parts,
        SHEET_RELS,
        &format!(
            "{DECL}<Relationships xmlns=\"{PKG}\"><Relationship Id=\"rId4\" Type=\"{R}/table\" Target=\"../tables/table1.xml\"/></Relationships>"
        ),
    );
    let mut pkg = load_xlsx(&write_zip(&parts)).unwrap();
    assert!(pkg.workbook.tables.is_empty());
    let sheet = &mut pkg.workbook.sheets[0];
    for (r, row) in [["Region", "Sales"], ["East", "10"], ["West", "30"]]
        .iter()
        .enumerate()
    {
        for (c, v) in row.iter().enumerate() {
            sheet.set_cell(r as u32, c as u32, Cell::text(v));
        }
    }
    pkg
}

#[test]
fn a_table_whose_part_a_dangling_rel_names_is_reached() {
    let mut pkg = with_a_dangling_table_rel("");
    pkg.add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
        .expect("add_table");
    let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
    let ws = text_of(&re.parts, SHEET);
    assert!(ws.contains("<tablePart r:id=\"rId4\"/>"), "{ws}");
    assert_eq!(re.workbook.tables.len(), 1, "{ws}");
    assert_eq!(re.workbook.tables[0].range, (0, 0, 2, 1));
}

#[test]
fn a_table_reuses_a_dangling_rel_its_sheet_already_names() {
    let mut pkg = with_a_dangling_table_rel(
        "<tableParts count=\"1\"><tablePart r:id=\"rId4\"/></tableParts>",
    );
    pkg.add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
        .expect("add_table");
    let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
    let ws = text_of(&re.parts, SHEET);
    assert_eq!(ws.matches("<tablePart ").count(), 1, "{ws}");
    assert!(ws.contains("<tablePart r:id=\"rId4\"/>"), "{ws}");
    assert_eq!(re.workbook.tables.len(), 1, "{ws}");
}

#[test]
fn strings_survive_a_save_into_a_cut_short_workbook_rels() {
    // No shared-strings part, so the save makes one and relates it.
    let mut parts = new_xlsx().parts;
    parts.retain(|(n, _)| n != "xl/sharedStrings.xml");
    let rels: String = text_of(&parts, WB_RELS);
    let at = rels.find("<Relationship Id=\"rId3\"").unwrap();
    let end = rels[at..].find("/>").unwrap() + at + 2;
    assert!(rels[at..end].contains("sharedStrings"), "{rels}");
    let rels = cut_short(&format!("{}{}", &rels[..at], &rels[end..]));
    set(&mut parts, WB_RELS, &rels);
    let mut pkg = load_xlsx(&write_zip(&parts)).unwrap();
    pkg.workbook.sheets[0].set_cell(0, 0, Cell::text("hello"));
    let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
    assert_eq!(
        re.workbook.sheets[0].cell(0, 0).map(|c| &c.value),
        Some(&CellValue::Text("hello".into()))
    );
}

/// The `r:id` of every `<sheet>` in a workbook part, in order.
fn sheet_rids(wb: &str) -> Vec<String> {
    let mut rids = Vec::new();
    let mut p = XmlParser::new(wb);
    loop {
        match p.next() {
            Event::Start if local(p.name()) == "sheet" => rids.push(decode(p.attr("r:id"))),
            Event::Eof => return rids,
            _ => {}
        }
    }
}

/// `rids` are all set and all different.
fn assert_distinct(rids: &[String], wb: &str) {
    assert!(rids.iter().all(|r| !r.is_empty()), "{wb}");
    let mut unique = rids.to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), rids.len(), "one <sheet> per r:id: {wb}");
}

/// Add "Added" (A1 = 5) to `parts` loaded, save, reload; the sheet names
/// and r:ids the next open sees.
fn add_sheet_and_reload(parts: &[(String, Vec<u8>)]) -> (SheetPackage, Vec<String>) {
    let mut pkg = load_xlsx(&write_zip(parts)).unwrap();
    let at = pkg.add_sheet("Added");
    pkg.workbook.sheets[at].set_cell(0, 0, Cell::number(5.0));
    let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
    let wb = text_of(&re.parts, "xl/workbook.xml");
    let rids = sheet_rids(&wb);
    assert_distinct(&rids, &wb);
    (re, rids)
}

fn sheet_names(pkg: &SheetPackage) -> Vec<&str> {
    pkg.workbook
        .sheets
        .iter()
        .map(|s| s.name.as_str())
        .collect()
}

#[test]
fn a_new_sheet_takes_no_rid_workbook_xml_names_for_a_lost_rel() {
    // FIX r1 M1: three sheets (rId1, then rId4 and rId5 past styles' rId2
    // and shared strings' rId3), the workbook rels cut short after rId3.
    // Sheet2 and Sheet3 are skipped, but workbook.xml still names rId4 and
    // rId5: a new rel under rId4 would bring Sheet2 back, reading Added's
    // part.
    let mut pkg = new_xlsx();
    pkg.add_sheet("Sheet2");
    pkg.add_sheet("Sheet3");
    let mut parts = load_xlsx(&save_xlsx(&pkg)).unwrap().parts;
    let rels = text_of(&parts, WB_RELS);
    let third = rels.find("Id=\"rId3\"").unwrap();
    let cut = rels[third..].find("/>").unwrap() + third + 2;
    assert!(!rels[..cut].contains("rId4"), "{rels}");
    assert!(rels[cut..].contains("Id=\"rId4\""), "{rels}");
    set(&mut parts, WB_RELS, &rels[..cut]);
    assert_eq!(
        sheet_names(&load_xlsx(&write_zip(&parts)).unwrap()),
        ["Sheet1"]
    );

    let (re, rids) = add_sheet_and_reload(&parts);
    assert!(!rids[..3].contains(&rids[3]), "{rids:?}");
    assert_eq!(sheet_names(&re), ["Sheet1", "Added"]);
    assert_eq!(
        re.workbook.sheets[1].cell(0, 0).unwrap().value,
        CellValue::Number(5.0)
    );
}

#[test]
fn a_new_sheet_takes_no_rid_a_dangling_sheet_entry_names() {
    // FIX r1 P1: workbook.xml names rId4, which no rel defines at all.
    let mut parts = new_xlsx().parts;
    let wb = text_of(&parts, "xl/workbook.xml").replace(
        "</sheets>",
        "<sheet name=\"Gone\" sheetId=\"2\" r:id=\"rId4\"/></sheets>",
    );
    set(&mut parts, "xl/workbook.xml", &wb);
    let (re, rids) = add_sheet_and_reload(&parts);
    assert_eq!(rids, ["rId1", "rId4", "rId5"]);
    assert_eq!(sheet_names(&re), ["Sheet1", "Added"]);
}

#[test]
fn add_sheet_skips_a_part_name_a_dangling_rel_spells_absolute() {
    // FIX r1 m2: `/xl/worksheets/sheet2.xml` names the same part as
    // `worksheets/sheet2.xml`.
    let mut parts = new_xlsx().parts;
    let wb = text_of(&parts, "xl/workbook.xml").replace(
        "</sheets>",
        "<sheet name=\"Gone\" sheetId=\"2\" r:id=\"rId5\"/></sheets>",
    );
    set(&mut parts, "xl/workbook.xml", &wb);
    let rels = text_of(&parts, WB_RELS).replace(
        "</Relationships>",
        &format!(
            "<Relationship Id=\"rId5\" Type=\"{R}/worksheet\" Target=\"/xl/worksheets/sheet2.xml\"/></Relationships>"
        ),
    );
    set(&mut parts, WB_RELS, &rels);
    let mut pkg = load_xlsx(&write_zip(&parts)).unwrap();
    let at = pkg.add_sheet("Added");
    assert_eq!(pkg.sheet_parts[at], "xl/worksheets/sheet3.xml");
    let (re, _) = add_sheet_and_reload(&parts);
    assert_eq!(sheet_names(&re), ["Sheet1", "Added"]);
}

#[test]
fn a_new_table_rel_takes_no_rid_the_worksheet_names_for_a_lost_rel() {
    // FIX r1 M1, worksheet side: `<drawing r:id="rId2">` lost its rel.
    let mut pkg = with_a_dangling_table_rel("<drawing r:id=\"rId2\"/>");
    // The table goes in as table2.xml's rel would: drop the fixture's
    // table rel so only the drawing's lost id is in play.
    pkg.set_part(
        SHEET_RELS,
        format!("{DECL}<Relationships xmlns=\"{PKG}\"/>").into_bytes(),
    );
    pkg.add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
        .expect("add_table");
    let ws = text_of(&pkg.parts, SHEET);
    assert!(ws.contains("<tablePart r:id=\"rId3\"/>"), "{ws}");
    let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
    assert_eq!(re.workbook.tables.len(), 1);
}

#[test]
fn a_rel_cut_inside_its_element_relates_nothing() {
    // FIX r1 m1: the loader never reads the cut element, so its Target
    // does not count as related, and its rId4 is not handed out again.
    let mut pkg = with_a_dangling_table_rel("");
    pkg.set_part(
        SHEET_RELS,
        format!(
            "{DECL}<Relationships xmlns=\"{PKG}\"><Relationship Id=\"rId4\" Type=\"{R}/table\" Target=\"../tables/table1.xml\""
        )
        .into_bytes(),
    );
    pkg.add_table(0, (0, 0, 2, 1), true, "TableStyleMedium2")
        .expect("add_table");
    let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
    let ws = text_of(&re.parts, SHEET);
    assert!(ws.contains("<tablePart r:id=\"rId5\"/>"), "{ws}");
    assert_eq!(re.workbook.tables.len(), 1, "{ws}");
}

#[test]
fn a_new_sheet_takes_no_single_quoted_rid() {
    // FIX r2 P1: the loader reads `Id='rId2'` as it reads `Id="rId2"`; a
    // second rId2 would resolve the new sheet to the styles part.
    let mut parts = new_xlsx().parts;
    let rels = format!(
        "{DECL}<Relationships xmlns=\"{PKG}\">\
         <Relationship Id='rId1' Type='{R}/worksheet' Target='worksheets/sheet1.xml'/>\
         <Relationship Id = 'rId2' Type='{R}/styles' Target='styles.xml'/>\
         <Relationship Id='rId3' Type='{R}/sharedStrings' Target='sharedStrings.xml'/>\
         </Relationships>"
    );
    set(&mut parts, WB_RELS, &rels);
    let (re, rids) = add_sheet_and_reload(&parts);
    assert_eq!(rids, ["rId1", "rId4"]);
    let ids: Vec<String> = parse_rels(&text_of(&re.parts, WB_RELS))
        .into_iter()
        .map(|(id, ..)| id)
        .collect();
    assert_eq!(ids, ["rId1", "rId2", "rId3", "rId4"]);
    assert_eq!(sheet_names(&re), ["Sheet1", "Added"]);
    assert_eq!(
        re.workbook.sheets[1].cell(0, 0).unwrap().value,
        CellValue::Number(5.0)
    );
    assert!(re.part("xl/styles.xml").is_some());
}

#[test]
fn a_huge_quoted_rid_in_cell_text_mints_past_it() {
    // FIX r2 I1: a formula's text quotes `"rId4294967295"`, past u32.
    let mut parts = new_xlsx().parts;
    set(
        &mut parts,
        SHEET,
        &format!(
            "{DECL}<worksheet xmlns=\"{SML}\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"str\"><f>\"rId4294967295\"</f><v>rId4294967295</v></c></row></sheetData></worksheet>"
        ),
    );
    let mut pkg = load_xlsx(&write_zip(&parts)).unwrap();
    assert!(pkg.set_comment(0, 1, 1, "A", "note"));
    let rels = text_of(&pkg.parts, SHEET_RELS);
    let ids: Vec<String> = parse_rels(&rels).into_iter().map(|(id, ..)| id).collect();
    assert_eq!(ids, ["rId4294967296", "rId4294967297"], "{rels}");
    let re = load_xlsx(&save_xlsx(&pkg)).unwrap();
    assert_eq!(re.comments().len(), 1);
}
