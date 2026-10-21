//! Saving hyperlinks made in-session (#1025): new and retyped links reach
//! the worksheet and its rels part; a sheet whose links are as loaded keeps
//! both parts byte for byte.

use super::*;

const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const RNS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const SHEET: &str = "xl/worksheets/sheet1.xml";
const RELS: &str = "xl/worksheets/_rels/sheet1.xml.rels";

/// A loaded workbook whose Sheet1 is `worksheet`, with `rels` as its rels
/// part when given.
fn book(worksheet: &str, rels: Option<&str>) -> SheetPackage {
    let mut parts = new_xlsx().parts;
    for (name, bytes) in parts.iter_mut() {
        if name == SHEET {
            *bytes = worksheet.as_bytes().to_vec();
        }
    }
    if let Some(rels) = rels {
        parts.push((RELS.to_string(), rels.as_bytes().to_vec()));
    }
    load_xlsx(&write_zip(&parts)).expect("fixture loads")
}

/// A rels part holding `rels`.
fn rels_part(rels: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>"
    )
}

fn text(pkg: &SheetPackage, part: &str) -> String {
    String::from_utf8_lossy(pkg.part(part).unwrap_or_else(|| panic!("{part} missing"))).into_owned()
}

/// Save and reload.
fn resaved(pkg: &SheetPackage) -> SheetPackage {
    load_xlsx(&save_xlsx(pkg)).expect("saved file reloads")
}

fn link(pkg: &mut SheetPackage, cell: &str, target: &str) {
    let at = parse_cell_name(cell).unwrap();
    pkg.workbook.sheets[0]
        .hyperlinks
        .insert(at, target.to_string());
}

/// The reloaded links are the model's, exactly.
fn assert_links_kept(pkg: &SheetPackage, re: &SheetPackage) {
    assert_eq!(
        re.workbook.sheets[0].hyperlinks,
        pkg.workbook.sheets[0].hyperlinks
    );
}

#[test]
fn new_hyperlink_is_saved_with_an_external_rel() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\"><sheetData></sheetData><printOptions gridLines=\"1\"/>\
         <pageMargins left=\"0.7\" right=\"0.7\" top=\"0.75\" bottom=\"0.75\" header=\"0.3\" footer=\"0.3\"/></worksheet>"
    );
    let mut pkg = book(&ws, None);
    link(&mut pkg, "B2", "https://example.com/a");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let rels = text(&re, RELS);
    assert!(
        rels.contains(&format!(
            "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://example.com/a\" TargetMode=\"External\"/>"
        )),
        "{rels}"
    );
    let xml = text(&re, SHEET);
    assert_eq!(xml.matches("xmlns:r=").count(), 1, "{xml}");
    assert!(xml.contains(&format!("xmlns:r=\"{RNS}\"")), "{xml}");
    assert!(xml.contains("<hyperlinks><hyperlink ref=\"B2\" r:id=\"rId1\"/></hyperlinks>"));
    let at = |s: &str| xml.find(s).unwrap_or_else(|| panic!("{s} in {xml}"));
    assert!(at("</sheetData>") < at("<hyperlinks>"));
    assert!(at("<hyperlinks>") < at("<printOptions"), "{xml}");
    assert!(at("<printOptions") < at("<pageMargins"), "{xml}");
    // Saved again unchanged, both parts stay as they are.
    let again = resaved(&re);
    assert_eq!(text(&again, SHEET), xml);
    assert_eq!(text(&again, RELS), rels);
}

#[test]
fn new_workbook_hyperlink_creates_sheet_rels() {
    let mut pkg = new_xlsx();
    assert!(pkg.part(RELS).is_none(), "a new sheet has no rels part");
    link(&mut pkg, "A1", "https://example.com");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(text(&re, RELS).contains("TargetMode=\"External\""));
    // A second save of the reloaded file adds nothing.
    let again = resaved(&re);
    assert_eq!(text(&again, RELS), text(&re, RELS));
}

const ORIGINAL_LINKS: &str = "<hyperlink ref=\"A1\" r:id=\"rId1\" display=\"Example\" tooltip=\"Go\"/>\
     <hyperlink ref=\"B2\" location=\"Sheet1!C3\" display=\"Here\"/>";

fn linked_book() -> (SheetPackage, String, Vec<String>) {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\"><sheetData></sheetData><hyperlinks>{ORIGINAL_LINKS}</hyperlinks></worksheet>"
    );
    let rels = vec![
        format!(
            "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://example.com/?a=1&amp;b=2\" TargetMode=\"External\"/>"
        ),
        format!(
            "<Relationship Id=\"rId3\" Type=\"{RNS}/hyperlink\" Target=\"https://unused.example\" TargetMode=\"External\"/>"
        ),
    ];
    let pkg = book(&ws, Some(&rels_part(&rels.concat())));
    (pkg, ws, rels)
}

#[test]
fn added_hyperlink_keeps_original_links_and_rels_bytes() {
    let (mut pkg, _, rels) = linked_book();
    assert_eq!(
        pkg.workbook.sheets[0]
            .hyperlinks
            .get(&(0, 0))
            .map(String::as_str),
        Some("https://example.com/?a=1&b=2")
    );
    link(&mut pkg, "A5", "https://new.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let xml = text(&re, SHEET);
    assert!(
        xml.contains(&format!(
            "<hyperlinks>{ORIGINAL_LINKS}<hyperlink ref=\"A5\" r:id=\"rId4\"/></hyperlinks>"
        )),
        "{xml}"
    );
    assert_eq!(xml.matches("xmlns:r=").count(), 1, "{xml}");
    let saved_rels = text(&re, RELS);
    for rel in &rels {
        assert!(
            saved_rels.contains(rel.as_str()),
            "{rel} kept in {saved_rels}"
        );
    }
    assert!(saved_rels.contains("Id=\"rId4\""), "{saved_rels}");
}

#[test]
fn a_new_link_to_a_target_already_related_reuses_its_relationship() {
    let (mut pkg, _, _) = linked_book();
    link(&mut pkg, "C3", "https://unused.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(text(&re, SHEET).contains("<hyperlink ref=\"C3\" r:id=\"rId3\"/>"));
    assert!(!text(&re, RELS).contains("rId4"));
}

#[test]
fn retyped_link_target_is_saved() {
    // A single-cell link retyped.
    let (mut pkg, _, _) = linked_book();
    link(&mut pkg, "A1", "https://other.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let xml = text(&re, SHEET);
    assert!(
        !xml.contains("r:id=\"rId1\""),
        "the stale element went: {xml}"
    );
    assert!(xml.contains("<hyperlink ref=\"B2\" location=\"Sheet1!C3\" display=\"Here\"/>"));

    // One cell of a range link retyped: the range goes, and its other cells
    // keep their link, one element each.
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\"><sheetData></sheetData><hyperlinks>\
         <hyperlink ref=\"A1:B2\" r:id=\"rId1\"/><hyperlink ref=\"D4\" location=\"Sheet1!A1\"/></hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://range.example\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    assert_eq!(pkg.workbook.sheets[0].hyperlinks.len(), 5);
    link(&mut pkg, "A1", "https://other.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let xml = text(&re, SHEET);
    assert!(!xml.contains("A1:B2"), "{xml}");
    assert!(xml.contains("<hyperlink ref=\"D4\" location=\"Sheet1!A1\"/>"));
    for cell in ["A2", "B1", "B2"] {
        assert!(
            xml.contains(&format!("<hyperlink ref=\"{cell}\" r:id=\"rId1\"/>")),
            "{cell}: {xml}"
        );
    }
}

#[test]
fn internal_hyperlink_is_saved_as_location() {
    let mut pkg = new_xlsx();
    link(&mut pkg, "A1", "#Sheet2!B3");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let xml = text(&re, SHEET);
    assert!(
        xml.contains("<hyperlinks><hyperlink ref=\"A1\" location=\"Sheet2!B3\"/></hyperlinks>"),
        "{xml}"
    );
    assert!(
        !xml.contains("xmlns:r="),
        "no relationship, no r prefix: {xml}"
    );
    assert!(re.part(RELS).is_none(), "no rels part for an internal link");
}

#[test]
fn location_with_ampersand_round_trips() {
    let mut pkg = new_xlsx();
    link(&mut pkg, "A1", "#'A&B'!A1");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(text(&re, SHEET).contains("location=\"'A&amp;B'!A1\""));

    // A file's escaped location loads decoded and saves untouched.
    let ws = format!(
        "<worksheet xmlns=\"{SML}\"><sheetData></sheetData><hyperlinks><hyperlink ref=\"C1\" location=\"'R&amp;D'!A1\"/></hyperlinks></worksheet>"
    );
    let pkg = book(&ws, None);
    assert_eq!(
        pkg.workbook.sheets[0]
            .hyperlinks
            .get(&(0, 2))
            .map(String::as_str),
        Some("#'R&D'!A1")
    );
    assert_eq!(text(&resaved(&pkg), SHEET), text(&pkg, SHEET));
}

#[test]
fn hyperlink_target_is_escaped_and_round_trips() {
    let targets = [
        "https://x.example/?a=1&b=\"q\"<z>",
        "\\\\server\\share\\f.xlsx",
        "mailto:a@b.example",
        "http://www.example.com",
    ];
    let mut pkg = new_xlsx();
    for (i, t) in targets.iter().enumerate() {
        pkg.workbook.sheets[0]
            .hyperlinks
            .insert((i as u32, 0), t.to_string());
    }
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let rels = text(&re, RELS);
    assert!(
        rels.contains("Target=\"https://x.example/?a=1&amp;b=&quot;q&quot;&lt;z&gt;\""),
        "{rels}"
    );
    // A UNC path is written as typed (#1025 m1).
    assert!(
        rels.contains("Target=\"\\\\server\\share\\f.xlsx\""),
        "{rels}"
    );
}

/// Elements the loader reads oddly or not at all: one with no target, one
/// whose relationship is missing, overlapping and duplicate refs, a range
/// past the spread limit.
const ODD_LINKS: &str = "<hyperlink ref=\"A1\" display=\"no target\"/>\
     <hyperlink ref=\"A2\" r:id=\"rId9\"/>\
     <hyperlink ref=\"A3:A4\" location=\"Sheet1!A1\"/><hyperlink ref=\"A4\" location=\"Sheet1!B1\"/>\
     <hyperlink ref=\"A5\" location=\"Sheet1!C1\"/><hyperlink ref=\"A5\" location=\"Sheet1!D1\"/>\
     <hyperlink ref=\"C1:C10000\" location=\"Sheet1!E1\"/>\
     <hyperlink ref=\"nonsense\" location=\"Sheet1!F1\"/>";

fn odd_book() -> SheetPackage {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\"><sheetData></sheetData><hyperlinks>{ODD_LINKS}</hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://example.com\" TargetMode=\"External\"/>"
    ));
    book(&ws, Some(&rels))
}

#[test]
fn untouched_odd_hyperlinks_keep_bytes() {
    let pkg = odd_book();
    let re = resaved(&pkg);
    assert_eq!(text(&re, SHEET), text(&pkg, SHEET));
    assert_eq!(text(&re, RELS), text(&pkg, RELS));

    // A link added beside them leaves every one of them as it was.
    let mut pkg = odd_book();
    link(&mut pkg, "E7", "https://new.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(text(&re, SHEET).contains(ODD_LINKS));
}

#[test]
fn save_without_link_changes_keeps_sheet_and_rels_bytes() {
    let (pkg, _, _) = linked_book();
    let re = resaved(&pkg);
    assert_eq!(text(&re, SHEET), text(&pkg, SHEET));
    assert_eq!(text(&re, RELS), text(&pkg, RELS));
}

#[test]
fn existing_block_with_foreign_r_prefix_gets_bound() {
    // The root binds no `r`: it gets one binding.
    let ws = format!(
        "<worksheet xmlns=\"{SML}\"><sheetData></sheetData><hyperlinks><hyperlink ref=\"A1\" location=\"Sheet1!B1\"/></hyperlinks></worksheet>"
    );
    let mut pkg = book(&ws, None);
    link(&mut pkg, "A2", "https://example.com");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let xml = text(&re, SHEET);
    assert_eq!(
        xml.matches(&format!("xmlns:r=\"{RNS}\"")).count(),
        1,
        "{xml}"
    );

    // The root binds `r` to something else: the new element binds its own.
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"urn:other\"><sheetData></sheetData><hyperlinks><hyperlink ref=\"A1\" location=\"Sheet1!B1\"/></hyperlinks></worksheet>"
    );
    let mut pkg = book(&ws, None);
    link(&mut pkg, "A2", "https://example.com");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let xml = text(&re, SHEET);
    assert!(
        xml.contains(&format!(
            "<hyperlink xmlns:r=\"{RNS}\" ref=\"A2\" r:id=\"rId1\"/>"
        )),
        "{xml}"
    );
}

#[test]
fn new_rels_part_needs_a_rels_default() {
    let mut parts = new_xlsx().parts;
    for (name, bytes) in parts.iter_mut() {
        if name == "[Content_Types].xml" {
            let ct = String::from_utf8_lossy(bytes).replace(
                "<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>",
                "<Override PartName=\"/_rels/.rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
                 <Override PartName=\"/xl/_rels/workbook.xml.rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>",
            );
            assert!(!ct.contains("Extension=\"rels\""));
            *bytes = ct.into_bytes();
        }
    }
    let mut pkg = load_xlsx(&write_zip(&parts)).expect("loads");
    link(&mut pkg, "A1", "https://example.com");
    link(&mut pkg, "A2", "#Sheet1!B1");
    let re = resaved(&pkg);
    assert!(re.part(RELS).is_none(), "no untyped rels part");
    let xml = text(&re, SHEET);
    assert!(!xml.contains("r:id"), "no rId that names nothing: {xml}");
    let links = &re.workbook.sheets[0].hyperlinks;
    assert_eq!(links.get(&(1, 0)).map(String::as_str), Some("#Sheet1!B1"));
    assert!(!links.contains_key(&(0, 0)));
}

#[test]
fn a_broken_rels_part_takes_no_new_relationship() {
    let ws = format!("<worksheet xmlns=\"{SML}\"><sheetData></sheetData></worksheet>");
    let mut pkg = book(
        &ws,
        Some(
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"/>",
        ),
    );
    link(&mut pkg, "A1", "https://example.com");
    let re = resaved(&pkg);
    assert!(!text(&re, SHEET).contains("r:id"));
    assert!(re.workbook.sheets[0].hyperlinks.is_empty());
}

/// #707 r6 M2's bound, for links added: 20,000 new ones save in linear time.
#[test]
fn adding_many_links_saves_in_linear_time() {
    let mut pkg = new_xlsx();
    for r in 0..20_000 {
        pkg.workbook.sheets[0]
            .hyperlinks
            .insert((r, 0), format!("https://example.com/{r}"));
    }
    let t = std::time::Instant::now();
    let bytes = save_xlsx(&pkg);
    assert!(t.elapsed() < crate::edit::PERF_BOUND, "{:?}", t.elapsed());
    let re = load_xlsx(&bytes).unwrap();
    assert_links_kept(&pkg, &re);
}

/// FIX r1 m2: a worksheet `r:id` with no relationship behind it is never
/// handed to a new link, or the dangling one would start opening its URL.
#[test]
fn a_dangling_worksheet_rid_is_not_taken_by_a_new_link() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\"><sheetData></sheetData><hyperlinks>\
         <hyperlink ref=\"A1\" r:id=\"rId1\"/><hyperlink ref=\"A2\" r:id=\"rId2\"/></hyperlinks>\
         <drawing r:id=\"rId3\"/></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://one.example\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    link(&mut pkg, "E7", "https://new.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(
        !re.workbook.sheets[0].hyperlinks.contains_key(&(1, 0)),
        "A2 stays unlinked"
    );
    assert!(text(&re, SHEET).contains("<hyperlink ref=\"E7\" r:id=\"rId4\"/>"));
}

/// FIX r1 m1: ids are read from the parsed rels part, so a single-quoted
/// one counts, and the highest possible `rIdN` doesn't overflow.
#[test]
fn new_rids_follow_the_parsed_relationships() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\"><sheetData></sheetData><hyperlinks>\
         <hyperlink ref=\"A1\" r:id=\"rId1\"/></hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id='rId1' Type='{RNS}/hyperlink' Target='https://one.example' TargetMode='External'/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    link(&mut pkg, "E7", "https://new.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(text(&re, SHEET).contains("<hyperlink ref=\"E7\" r:id=\"rId2\"/>"));

    let rels = rels_part(&format!(
        "<Relationship Id=\"rId4294967295\" Type=\"{RNS}/hyperlink\" Target=\"https://one.example\" TargetMode=\"External\"/>"
    ));
    let ws = ws.replace("\"rId1\"", "\"rId4294967295\"");
    let mut pkg = book(&ws, Some(&rels));
    link(&mut pkg, "E7", "https://new.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(text(&re, SHEET).contains("r:id=\"rId4294967296\""));
}

/// A loaded sheet with `links` (`<hyperlink>` elements, external ones on
/// rId1), edited by `edit` and saved: the reloaded links.
fn links_after(links: &str, edit: impl FnOnce(&mut Workbook)) -> Vec<(String, String)> {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\"><sheetData></sheetData><hyperlinks>{links}</hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://one.example\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    edit(&mut pkg.workbook);
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    re.workbook.sheets[0]
        .hyperlinks
        .iter()
        .map(|(&(r, c), t)| (cell_name(r, c), t.clone()))
        .collect()
}

fn at(cell: &str) -> (String, String) {
    (cell.to_string(), "https://one.example".to_string())
}

#[test]
fn a_link_follows_an_inserted_row_through_a_save() {
    let got = links_after("<hyperlink ref=\"A3\" r:id=\"rId1\"/>", |wb| {
        crate::edit::insert_rows(wb, 0, 0, 1)
    });
    assert_eq!(got, vec![at("A4")]);
}

#[test]
fn a_link_follows_a_deleted_row_and_one_in_it_goes() {
    let links = "<hyperlink ref=\"A3\" r:id=\"rId1\"/><hyperlink ref=\"B5\" r:id=\"rId1\"/>";
    let got = links_after(links, |wb| crate::edit::delete_rows(wb, 0, 2, 1));
    assert_eq!(got, vec![at("B4")]);
}

#[test]
fn a_link_follows_an_inserted_and_a_deleted_column() {
    let links = "<hyperlink ref=\"C1\" r:id=\"rId1\"/>";
    let got = links_after(links, |wb| crate::edit::insert_cols(wb, 0, 0, 2));
    assert_eq!(got, vec![at("E1")]);
    let got = links_after(links, |wb| crate::edit::delete_cols(wb, 0, 0, 1));
    assert_eq!(got, vec![at("B1")]);
}

#[test]
fn a_range_link_cut_by_an_insert_is_saved_cell_by_cell() {
    let got = links_after("<hyperlink ref=\"A1:A3\" r:id=\"rId1\"/>", |wb| {
        crate::edit::insert_rows(wb, 0, 1, 1)
    });
    assert_eq!(got, vec![at("A1"), at("A3"), at("A4")]);
}

#[test]
fn a_removed_link_stays_removed_after_a_structural_edit() {
    let links = "<hyperlink ref=\"A3\" r:id=\"rId1\"/><hyperlink ref=\"B3\" r:id=\"rId1\"/>";
    let got = links_after(links, |wb| {
        let s = &mut wb.sheets[0];
        s.hyperlinks.remove(&(2, 0));
        s.hyperlink_refs.remove(&(2, 0));
        s.hyperlinks_removed.push((2, 0, 2, 0));
        crate::edit::insert_rows(wb, 0, 0, 1);
    });
    assert_eq!(got, vec![at("B4")]);
}
