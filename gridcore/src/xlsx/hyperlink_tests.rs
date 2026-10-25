//! Saving hyperlinks made in-session (#1025): new and retyped links reach
//! the worksheet and its rels part; a sheet whose links are as loaded keeps
//! both parts byte for byte.

use super::hyperlinks::model_target;
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
    // A UNC path is written as Excel writes it (#1456).
    assert!(
        rels.contains("Target=\"file:///\\\\server\\share\\f.xlsx\""),
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

/// The saved worksheet's `<hyperlinks>` block after `edit` on a sheet loaded
/// with `links`.
fn block_after(links: &str, edit: impl FnOnce(&mut Workbook)) -> String {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\"><sheetData></sheetData><hyperlinks>{links}</hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://one.example\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    edit(&mut pkg.workbook);
    let xml = text(&resaved(&pkg), SHEET);
    let start = xml.find("<hyperlinks>").expect("block kept");
    let end = xml.find("</hyperlinks>").expect("block closed");
    xml[start + "<hyperlinks>".len()..end].to_string()
}

#[test]
fn a_big_range_link_moves_in_place_with_an_insert_above() {
    let got = block_after("<hyperlink ref=\"A1:D2000\" r:id=\"rId1\"/>", |wb| {
        crate::edit::insert_rows(wb, 0, 0, 1)
    });
    assert_eq!(got, "<hyperlink ref=\"A2:D2001\" r:id=\"rId1\"/>");
}

#[test]
fn a_big_range_link_stretches_over_an_insert_inside() {
    let got = block_after("<hyperlink ref=\"A1:A5000\" r:id=\"rId1\"/>", |wb| {
        crate::edit::insert_rows(wb, 0, 100, 2)
    });
    assert_eq!(got, "<hyperlink ref=\"A1:A5002\" r:id=\"rId1\"/>");
}

#[test]
fn a_big_range_link_survives_the_delete_of_its_anchor_row() {
    let got = block_after("<hyperlink ref=\"A1:A5000\" r:id=\"rId1\"/>", |wb| {
        crate::edit::delete_rows(wb, 0, 0, 1)
    });
    assert_eq!(got, "<hyperlink ref=\"A1:A4999\" r:id=\"rId1\"/>");
}

#[test]
fn a_moved_link_keeps_its_display_tooltip_and_id() {
    let got = block_after(
        "<hyperlink ref=\"A1\" r:id=\"rId1\" display=\"Example\" tooltip=\"Go\"/>",
        |wb| crate::edit::insert_rows(wb, 0, 0, 1),
    );
    assert_eq!(
        got,
        "<hyperlink ref=\"A2\" r:id=\"rId1\" display=\"Example\" tooltip=\"Go\"/>"
    );
    let got = block_after(
        "<hyperlink ref=\"B2:C3\" location=\"Sheet1!A1\" display=\"Here\"/>",
        |wb| crate::edit::delete_cols(wb, 0, 0, 1),
    );
    assert_eq!(
        got,
        "<hyperlink ref=\"A2:B3\" location=\"Sheet1!A1\" display=\"Here\"/>"
    );
}

#[test]
fn a_moved_link_that_was_retargeted_is_written_afresh() {
    let got = block_after(
        "<hyperlink ref=\"A1\" r:id=\"rId1\" display=\"Example\"/>",
        |wb| {
            crate::edit::insert_rows(wb, 0, 0, 1);
            wb.sheets[0]
                .hyperlinks
                .insert((1, 0), "https://two.example".into());
        },
    );
    assert!(!got.contains("display"), "{got}");
    assert!(got.contains("ref=\"A2\""), "{got}");
}

#[test]
fn removing_a_moved_link_strikes_its_own_element_only() {
    // Element B at A4:B4 is unreadable (no target): never touched.
    let links = "<hyperlink ref=\"A3:B3\" r:id=\"rId1\"/><hyperlink ref=\"A4:B4\"/>";
    let got = block_after(links, |wb| {
        crate::edit::insert_rows(wb, 0, 0, 1);
        let s = &mut wb.sheets[0];
        let plan = crate::edit::clear_plan(
            s,
            &[(3, 0, 3, 0)],
            crate::edit::ClearWhat::RemoveHyperlinks,
            &[],
        )
        .unwrap();
        crate::edit::apply_clear_sheet(s, &plan);
        assert_eq!(s.hyperlinks_removed, vec![(2, 0, 2, 1)]);
    });
    assert_eq!(got, "<hyperlink ref=\"A4:B4\"/>");
}

#[test]
fn a_deleted_link_does_not_survive_beside_the_one_that_moved_onto_it() {
    let x = "<hyperlink ref=\"A1\" r:id=\"rId1\" display=\"X\"/>";
    let y = "<hyperlink ref=\"A2\" location=\"Sheet1!B2\" tooltip=\"Y\"/>";
    let want = "<hyperlink ref=\"A1\" location=\"Sheet1!B2\" tooltip=\"Y\"/>";
    let del_row = |wb: &mut Workbook| crate::edit::delete_rows(wb, 0, 0, 1);
    assert_eq!(block_after(&format!("{x}{y}"), del_row), want);
    // The deleted element later in the block.
    assert_eq!(block_after(&format!("{y}{x}"), del_row), want);
    // The same over columns.
    let x = "<hyperlink ref=\"A1\" r:id=\"rId1\" display=\"X\"/>";
    let y = "<hyperlink ref=\"B1\" location=\"Sheet1!B2\" tooltip=\"Y\"/>";
    let del_col = |wb: &mut Workbook| crate::edit::delete_cols(wb, 0, 0, 1);
    assert_eq!(block_after(&format!("{x}{y}"), del_col), want);
    assert_eq!(block_after(&format!("{y}{x}"), del_col), want);
}

// ---- UNC targets as file URIs, and dropped relationships (#1456) ----

const UNC_WS: &str = "<sheetData></sheetData>";

fn plain_book() -> SheetPackage {
    book(
        &format!("<worksheet xmlns=\"{SML}\">{UNC_WS}</worksheet>"),
        None,
    )
}

/// The `Target` values of the rels part's relationships of `kind`.
fn targets_of(pkg: &SheetPackage, kind: &str) -> Vec<String> {
    parse_rels_mode(&text(pkg, RELS))
        .into_iter()
        .filter(|(_, ty, _, _)| ty.ends_with(kind))
        .map(|(_, _, t, _)| t)
        .collect()
}

fn remove_link(pkg: &mut SheetPackage, cell: (u32, u32)) {
    let s = &mut pkg.workbook.sheets[0];
    let plan = crate::edit::clear_plan(
        s,
        &[(cell.0, cell.1, cell.0, cell.1)],
        crate::edit::ClearWhat::RemoveHyperlinks,
        &[],
    )
    .unwrap();
    crate::edit::apply_clear_sheet(s, &plan);
}

#[test]
fn unc_link_is_written_as_a_file_uri_and_round_trips() {
    let mut pkg = plain_book();
    link(&mut pkg, "A1", "\\\\srv\\share\\a.xlsx");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(
        text(&re, RELS)
            .contains("Target=\"file:///\\\\srv\\share\\a.xlsx\" TargetMode=\"External\"/>"),
        "{}",
        text(&re, RELS)
    );
    // Saved again unchanged, nothing is rewritten.
    let again = resaved(&re);
    assert_eq!(text(&again, RELS), text(&re, RELS));
    assert_eq!(text(&again, SHEET), text(&re, SHEET));
}

#[test]
fn excel_file_uri_unc_loads_as_the_plain_path_and_keeps_bytes() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\">{UNC_WS}<hyperlinks>\
         <hyperlink ref=\"A1\" r:id=\"rId1\"/><hyperlink ref=\"A2\" r:id=\"rId2\"/>\
         <hyperlink ref=\"A3\" r:id=\"rId3\"/></hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"file:///\\\\srv\\share\\a\" TargetMode=\"External\"/>\
         <Relationship Id=\"rId2\" Type=\"{RNS}/hyperlink\" Target=\"FILE:///\\\\srv\\share\\b\" TargetMode=\"External\"/>\
         <Relationship Id=\"rId3\" Type=\"{RNS}/hyperlink\" Target=\"file:///C:/x\" TargetMode=\"External\"/>"
    ));
    let pkg = book(&ws, Some(&rels));
    let links = &pkg.workbook.sheets[0].hyperlinks;
    assert_eq!(links[&(0, 0)], "\\\\srv\\share\\a");
    assert_eq!(links[&(1, 0)], "\\\\srv\\share\\b");
    assert_eq!(links[&(2, 0)], "file:///C:/x");
    let re = resaved(&pkg);
    assert_eq!(text(&re, SHEET), text(&pkg, SHEET));
    assert_eq!(text(&re, RELS), text(&pkg, RELS));
}

#[test]
fn a_raw_unc_rel_from_1025_is_kept_and_reused() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\">{UNC_WS}<hyperlinks>\
         <hyperlink ref=\"A1\" r:id=\"rId1\"/></hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"\\\\srv\\share\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    assert_eq!(pkg.workbook.sheets[0].hyperlinks[&(0, 0)], "\\\\srv\\share");
    let re = resaved(&pkg);
    assert_eq!(text(&re, SHEET), text(&pkg, SHEET));
    assert_eq!(text(&re, RELS), text(&pkg, RELS));
    link(&mut pkg, "B2", "\\\\srv\\share");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(text(&re, SHEET).contains("<hyperlink ref=\"B2\" r:id=\"rId1\"/>"));
    assert_eq!(targets_of(&re, "/hyperlink"), vec!["\\\\srv\\share"]);
}

#[test]
fn non_unc_targets_are_written_as_before() {
    let mut pkg = plain_book();
    let all = [
        "https://example.com/a",
        "mailto:a@b.example",
        "C:\\x\\y.xlsx",
        "file:///C:/x",
        // Not a UNC path: written as typed, and read back as the plain path.
        "file:///\\\\srv\\x",
        // Another spelling of a UNC path (#1726): the same.
        "file://srv/share/x",
    ];
    for (i, t) in all.iter().enumerate() {
        link(&mut pkg, &format!("A{}", i + 1), t);
    }
    let re = resaved(&pkg);
    let mut got = targets_of(&re, "/hyperlink");
    got.sort();
    let mut want: Vec<String> = all.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want);
    let links = &re.workbook.sheets[0].hyperlinks;
    for (i, t) in all.iter().take(4).enumerate() {
        assert_eq!(links[&(i as u32, 0)], *t);
    }
    assert_eq!(links[&(4, 0)], "\\\\srv\\x");
    assert_eq!(links[&(5, 0)], "\\\\srv\\share\\x");
}

#[test]
fn a_removed_link_drops_its_relationship() {
    let (mut pkg, _, _) = linked_book();
    remove_link(&mut pkg, (0, 0));
    let re = resaved(&pkg);
    assert!(!re.workbook.sheets[0].hyperlinks.contains_key(&(0, 0)));
    let rels = text(&re, RELS);
    assert!(!rels.contains("Id=\"rId1\""), "{rels}");
    assert!(!text(&re, SHEET).contains("rId1"));
    // The orphan the file already had is not ours to drop.
    assert!(rels.contains("Id=\"rId3\""), "{rels}");
}

#[test]
fn removing_every_link_drops_the_relationship_and_the_block() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\">{UNC_WS}<hyperlinks>\
         <hyperlink ref=\"A1\" r:id=\"rId1\"/></hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://x.example\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    remove_link(&mut pkg, (0, 0));
    let re = resaved(&pkg);
    assert!(!text(&re, SHEET).contains("hyperlink"));
    assert!(targets_of(&re, "/hyperlink").is_empty());
}

#[test]
fn a_retargeted_link_drops_the_old_relationship() {
    let (mut pkg, _, _) = linked_book();
    link(&mut pkg, "A1", "https://other.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let got = targets_of(&re, "/hyperlink");
    assert!(!got.iter().any(|t| t.contains("example.com")), "{got:?}");
    assert!(
        got.contains(&"https://other.example".to_string()),
        "{got:?}"
    );
    assert!(
        got.contains(&"https://unused.example".to_string()),
        "{got:?}"
    );
}

#[test]
fn a_relationship_still_used_by_a_range_remainder_stays() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\">{UNC_WS}<hyperlinks>\
         <hyperlink ref=\"A1:B2\" r:id=\"rId1\"/></hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://range.example\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    link(&mut pkg, "A1", "https://other.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let got = targets_of(&re, "/hyperlink");
    assert!(
        got.contains(&"https://range.example".to_string()),
        "{got:?}"
    );
    assert!(
        got.contains(&"https://other.example".to_string()),
        "{got:?}"
    );
}

#[test]
fn a_retarget_to_an_already_related_target_drops_nothing_used() {
    let (mut pkg, _, _) = linked_book();
    link(&mut pkg, "A1", "https://unused.example");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(text(&re, SHEET).contains("<hyperlink ref=\"A1\" r:id=\"rId3\"/>"));
    assert_eq!(
        targets_of(&re, "/hyperlink"),
        vec!["https://unused.example"]
    );
}

#[test]
fn unrelated_and_preexisting_orphan_rels_are_untouched() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\">{UNC_WS}<hyperlinks>\
         <hyperlink ref=\"A1\" r:id=\"rId1\"/><hyperlink ref=\"A2\" r:id=\"rId1\"/></hyperlinks>\
         <drawing r:id=\"rId2\"/><legacyDrawing r:id=\"rId4\"/></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"https://x.example\" TargetMode=\"External\"/>\
         <Relationship Id=\"rId2\" Type=\"{RNS}/drawing\" Target=\"../drawings/drawing1.xml\"/>\
         <Relationship Id=\"rId3\" Type=\"{RNS}/hyperlink\" Target=\"https://orphan.example\" TargetMode=\"External\"/>\
         <Relationship Id=\"rId4\" Type=\"{RNS}/vmlDrawing\" Target=\"../drawings/vmlDrawing1.vml\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    remove_link(&mut pkg, (0, 0));
    remove_link(&mut pkg, (1, 0));
    let re = resaved(&pkg);
    let got = text(&re, RELS);
    assert!(!got.contains("Id=\"rId1\""), "{got}");
    for keep in ["Id=\"rId2\"", "Id=\"rId3\"", "Id=\"rId4\""] {
        assert!(got.contains(keep), "{keep} in {got}");
    }
}

#[test]
fn a_cell_edit_alone_keeps_the_rels_part_bytes() {
    let (mut pkg, _, _) = linked_book();
    pkg.workbook.sheets[0].set_cell(5, 5, Cell::default());
    let re = resaved(&pkg);
    assert_eq!(text(&re, RELS), text(&pkg, RELS));
}

#[test]
fn a_removed_link_with_an_unusual_id_drops_its_relationship() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\">{UNC_WS}<hyperlinks>\
         <hyperlink ref=\"A1\" r:id=\"R5f3a\"/><hyperlink ref=\"A2\" r:id=\"hId1\"/></hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"R5f3a\" Type=\"{RNS}/hyperlink\" Target=\"https://x.example\" TargetMode=\"External\"/>\
         <Relationship Id=\"hId1\" Type=\"{RNS}/hyperlink\" Target=\"https://y.example\" TargetMode=\"External\"/>\
         <Relationship Id=\"hId2\" Type=\"{RNS}/hyperlink\" Target=\"https://orphan.example\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    remove_link(&mut pkg, (0, 0));
    remove_link(&mut pkg, (1, 0));
    let re = resaved(&pkg);
    assert_eq!(
        targets_of(&re, "/hyperlink"),
        vec!["https://orphan.example"]
    );
}

#[test]
fn a_mixed_case_file_uri_rel_is_reused_for_the_same_target() {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\">{UNC_WS}<hyperlinks>\
         <hyperlink ref=\"A1\" r:id=\"rId1\"/></hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"rId1\" Type=\"{RNS}/hyperlink\" Target=\"FILE:///\\\\srv\\share\\b\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    link(&mut pkg, "B2", "\\\\srv\\share\\b");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert!(text(&re, SHEET).contains("<hyperlink ref=\"B2\" r:id=\"rId1\"/>"));
    assert_eq!(targets_of(&re, "/hyperlink").len(), 1);
}

#[test]
fn stray_quotes_in_cell_text_do_not_hide_a_reference() {
    // Inline strings with a lone `"` and `'` before the links: the pairing
    // of quotes must not drift, so A2's kept link still counts as named and
    // only A1's goes.
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\"><sheetData>\
         <row r=\"5\"><c r=\"C5\" t=\"inlineStr\"><is><t>say \" and it's</t></is></c></row>\
         </sheetData><hyperlinks>\
         <hyperlink ref=\"A1\" r:id=\"R1\"/><hyperlink ref=\"A2\" r:id=\"R2\"/></hyperlinks></worksheet>"
    );
    let rels = rels_part(&format!(
        "<Relationship Id=\"R1\" Type=\"{RNS}/hyperlink\" Target=\"https://one.example\" TargetMode=\"External\"/>\
         <Relationship Id=\"R2\" Type=\"{RNS}/hyperlink\" Target=\"https://two.example\" TargetMode=\"External\"/>"
    ));
    let mut pkg = book(&ws, Some(&rels));
    remove_link(&mut pkg, (0, 0));
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    assert_eq!(targets_of(&re, "/hyperlink"), vec!["https://two.example"]);
}

#[test]
fn dropping_many_relationships_saves_in_linear_time() {
    let mut pkg = plain_book();
    for r in 0..20_000 {
        link(
            &mut pkg,
            &format!("A{}", r + 1),
            &format!("https://example.com/{r}"),
        );
    }
    let mut pkg = resaved(&pkg);
    assert_eq!(targets_of(&pkg, "/hyperlink").len(), 20_000);
    let s = &mut pkg.workbook.sheets[0];
    let plan = crate::edit::clear_plan(
        s,
        &[(0, 0, 19_999, 0)],
        crate::edit::ClearWhat::RemoveHyperlinks,
        &[],
    )
    .unwrap();
    crate::edit::apply_clear_sheet(s, &plan);
    let t = std::time::Instant::now();
    let bytes = save_xlsx(&pkg);
    assert!(t.elapsed() < crate::edit::PERF_BOUND, "{:?}", t.elapsed());
    let re = load_xlsx(&bytes).unwrap();
    assert!(re.workbook.sheets[0].hyperlinks.is_empty());
    assert!(targets_of(&re, "/hyperlink").is_empty());
}

// ---- Other file URI spellings of a UNC path (#1726) ----

#[test]
fn file_uri_unc_forms_load_as_unc() {
    for (wire, want) in [
        ("file://srv/share/x.xlsx", "\\\\srv\\share\\x.xlsx"),
        ("file:////srv/share/x.xlsx", "\\\\srv\\share\\x.xlsx"),
        ("file://///srv/share/x.xlsx", "\\\\srv\\share\\x.xlsx"),
        ("FILE://srv/share/x.xlsx", "\\\\srv\\share\\x.xlsx"),
        ("File:////srv/share", "\\\\srv\\share"),
        (
            "file://srv/my%20share/a%C3%A9.xlsx",
            "\\\\srv\\my share\\aé.xlsx",
        ),
        ("file:///\\\\srv\\share\\x.xlsx", "\\\\srv\\share\\x.xlsx"),
        // Escapes decode in Excel's spelling too, so both are one model form.
        ("file:///\\\\srv\\my%20share\\x", "\\\\srv\\my share\\x"),
        ("file://srv/my%20share/x", "\\\\srv\\my share\\x"),
        // One that does not decode stays as it is.
        ("file:///\\\\srv\\a%2Fb", "\\\\srv\\a%2Fb"),
    ] {
        assert_eq!(model_target(wire), want, "{wire}");
    }
}

#[test]
fn file_uri_forms_left_raw() {
    for wire in [
        "file:///C:/x",
        // A local POSIX path, not a server.
        "file:///srv/share/x",
        "file://localhost/share/x",
        "file://LOCALHOST/share/x",
        "file:///",
        "file://",
        // No share.
        "file://srv",
        "file://srv/",
        "file:////srv",
        "file://srv/share/x?a=1",
        "file://srv/share/x#top",
        "file://srv/share%2Fx",
        "file://srv/share%5cx",
        "file://srv/share/%FF",
        "file://srv/share/%4",
        "file://srv/share\\x",
        "file://srv/share/%+5",
        // A drive or a port is not a server.
        "file://C:/dir/a.xlsx",
        "file:////C:/x",
        "file://c|/x/y",
        "file://host:8080/x",
        "https://srv/share/x",
    ] {
        assert_eq!(model_target(wire), wire, "{wire}");
    }
}

/// A sheet whose A1..A3 link through rId1..rId3 to `targets`.
fn linked_spellings(targets: &[&str]) -> SheetPackage {
    let ws = format!(
        "<worksheet xmlns=\"{SML}\" xmlns:r=\"{RNS}\">{UNC_WS}<hyperlinks>{}</hyperlinks></worksheet>",
        (1..=targets.len())
            .map(|i| format!("<hyperlink ref=\"A{i}\" r:id=\"rId{i}\"/>"))
            .collect::<String>()
    );
    let rels = rels_part(
        &targets
            .iter()
            .enumerate()
            .map(|(i, t)| {
                format!(
                    "<Relationship Id=\"rId{}\" Type=\"{RNS}/hyperlink\" Target=\"{t}\" TargetMode=\"External\"/>",
                    i + 1
                )
            })
            .collect::<String>(),
    );
    book(&ws, Some(&rels))
}

#[test]
fn unchanged_host_form_link_keeps_bytes() {
    let pkg = linked_spellings(&[
        "file://srv/share/a",
        "file:////srv/share/b",
        "FILE://///srv/share/c",
        "file:///\\\\srv\\my%20share\\d",
    ]);
    let links = &pkg.workbook.sheets[0].hyperlinks;
    assert_eq!(links[&(0, 0)], "\\\\srv\\share\\a");
    assert_eq!(links[&(1, 0)], "\\\\srv\\share\\b");
    assert_eq!(links[&(2, 0)], "\\\\srv\\share\\c");
    assert_eq!(links[&(3, 0)], "\\\\srv\\my share\\d");
    let re = resaved(&pkg);
    assert_eq!(text(&re, SHEET), text(&pkg, SHEET));
    assert_eq!(text(&re, RELS), text(&pkg, RELS));
}

#[test]
fn retyped_link_reuses_host_form_rel() {
    let mut pkg = linked_spellings(&["file://srv/share/a", "https://example.com/"]);
    // A new link to the same UNC path, and A2 retyped to it.
    link(&mut pkg, "B5", "\\\\srv\\share\\a");
    link(&mut pkg, "A2", "\\\\srv\\share\\a");
    let re = resaved(&pkg);
    assert_links_kept(&pkg, &re);
    let sheet = text(&re, SHEET);
    assert!(
        sheet.contains("<hyperlink ref=\"B5\" r:id=\"rId1\"/>"),
        "{sheet}"
    );
    assert!(
        sheet.contains("<hyperlink ref=\"A2\" r:id=\"rId1\"/>"),
        "{sheet}"
    );
    assert_eq!(targets_of(&re, "/hyperlink"), vec!["file://srv/share/a"]);
}

#[test]
fn raw_spelling_in_model_reuses_rel() {
    let mut pkg = linked_spellings(&["file:///\\\\srv\\share\\a"]);
    // Held raw, as nothing but the loader maps it.
    link(&mut pkg, "A1", "file://srv/share/a");
    link(&mut pkg, "B2", "file:////srv/share/a");
    let re = resaved(&pkg);
    // A1 is not changed: its element and the rels part keep their bytes.
    assert!(text(&re, SHEET).contains("<hyperlink ref=\"A1\" r:id=\"rId1\"/>"));
    assert!(text(&re, SHEET).contains("<hyperlink ref=\"B2\" r:id=\"rId1\"/>"));
    assert_eq!(text(&re, RELS), text(&pkg, RELS));
    let links = &re.workbook.sheets[0].hyperlinks;
    assert_eq!(links[&(0, 0)], "\\\\srv\\share\\a");
    assert_eq!(links[&(1, 1)], "\\\\srv\\share\\a");
    // The model holding only the raw A1 is no change at all.
    let mut pkg = linked_spellings(&["file:///\\\\srv\\share\\a"]);
    link(&mut pkg, "A1", "file://srv/share/a");
    let re = resaved(&pkg);
    assert_eq!(text(&re, SHEET), text(&pkg, SHEET));
    assert_eq!(text(&re, RELS), text(&pkg, RELS));
}
