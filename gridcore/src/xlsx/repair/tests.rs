use super::super::{load_xlsx, save_xlsx};
use super::*;
use crate::sheet::{CellValue, Xf};
use opccore::zip::ZipArchive;
use opccore::zipwrite::write_zip;

const SML: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Two sheets with every kind of reference repair has to keep valid: a
/// shared string, cell, row and column styles, a conditional format's
/// `dxfId`, a theme, and (added through the API) a drawing with a chart.
fn fixture() -> Vec<u8> {
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/><Override PartName=\"/xl/worksheets/sheet2.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/><Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/><Override PartName=\"/xl/sharedStrings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml\"/><Override PartName=\"/xl/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/></Types>";
    let root = format!(
        "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{R}/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>"
    );
    let wb = format!(
        "<?xml version=\"1.0\"?><workbook xmlns=\"{SML}\" xmlns:r=\"{R}\"><sheets><sheet name=\"One\" sheetId=\"1\" r:id=\"rId1\"/><sheet name=\"Two\" sheetId=\"2\" r:id=\"rId2\"/></sheets></workbook>"
    );
    let wb_rels = format!(
        "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{R}/worksheet\" Target=\"worksheets/sheet1.xml\"/><Relationship Id=\"rId2\" Type=\"{R}/worksheet\" Target=\"worksheets/sheet2.xml\"/><Relationship Id=\"rId3\" Type=\"{R}/styles\" Target=\"styles.xml\"/><Relationship Id=\"rId4\" Type=\"{R}/sharedStrings\" Target=\"sharedStrings.xml\"/><Relationship Id=\"rId5\" Type=\"{R}/theme\" Target=\"theme/theme1.xml\"/></Relationships>"
    );
    let sheet1 = format!(
        "<?xml version=\"1.0\"?><worksheet xmlns=\"{SML}\" xmlns:r=\"{R}\"><cols><col min=\"1\" max=\"1\" width=\"12\" customWidth=\"1\" style=\"2\"/></cols><sheetData><row r=\"1\" s=\"2\" customFormat=\"1\"><c r=\"A1\" t=\"s\" s=\"1\"><v>0</v></c><c r=\"B1\" s=\"2\"><v>5</v></c></row></sheetData><conditionalFormatting sqref=\"B1\"><cfRule type=\"cellIs\" dxfId=\"1\" priority=\"1\" operator=\"greaterThan\"><formula>1</formula></cfRule></conditionalFormatting></worksheet>"
    );
    let sheet2 = format!(
        "<?xml version=\"1.0\"?><worksheet xmlns=\"{SML}\"><sheetData><row r=\"1\"><c r=\"A1\"><v>7</v></c></row></sheetData></worksheet>"
    );
    let styles = format!(
        "<?xml version=\"1.0\"?><styleSheet xmlns=\"{SML}\"><fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts><fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills><borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs><cellXfs count=\"3\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/><xf numFmtId=\"10\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/></cellXfs><cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles><dxfs count=\"2\"><dxf><font><b/></font></dxf><dxf><font><i/></font></dxf></dxfs></styleSheet>"
    );
    let sst = format!(
        "<?xml version=\"1.0\"?><sst xmlns=\"{SML}\" count=\"1\" uniqueCount=\"1\"><si><t>hello</t></si></sst>"
    );
    let theme = "<?xml version=\"1.0\"?><a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Office\"/>";
    let parts: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".into(), ct.into()),
        ("_rels/.rels".into(), root.into()),
        ("xl/workbook.xml".into(), wb.into()),
        ("xl/_rels/workbook.xml.rels".into(), wb_rels.into()),
        ("xl/worksheets/sheet1.xml".into(), sheet1.into()),
        ("xl/worksheets/sheet2.xml".into(), sheet2.into()),
        ("xl/styles.xml".into(), styles.into()),
        ("xl/sharedStrings.xml".into(), sst.into()),
        ("xl/theme/theme1.xml".into(), theme.into()),
    ];
    let mut pkg = load_xlsx(&write_zip(&parts)).expect("the fixture loads");
    let data = crate::sheet::ChartData {
        title: "T".into(),
        kind: "column".into(),
        categories: vec!["a".into()],
        series: vec![crate::sheet::ChartSeries {
            name: "s".into(),
            values: vec![1.0],
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(pkg.add_chart(0, (2, 0), (10, 5), &data));
    let bytes = save_xlsx(&pkg);
    let names = load_xlsx(&bytes).unwrap();
    for part in ["xl/drawings/drawing1.xml", "xl/charts/chart1.xml"] {
        assert!(names.part(part).is_some(), "the fixture has {part}");
    }
    bytes
}

/// `data` with the entry `name` made unreadable: its local header no longer
/// carries the signature, so extracting it fails as a damaged entry does.
fn damage(data: &[u8], name: &str) -> Vec<u8> {
    let offset = ZipArchive::open(data)
        .unwrap()
        .find(name)
        .unwrap_or_else(|| panic!("no entry {name}"))
        .local_offset as usize;
    let mut out = data.to_vec();
    out[offset..offset + 4].copy_from_slice(&[0, 0, 0, 0]);
    out
}

fn parts_of(data: &[u8]) -> Vec<(String, Vec<u8>)> {
    let zip = ZipArchive::open(data).unwrap();
    zip.entries()
        .iter()
        .map(|e| (e.name.clone(), zip.extract(e).expect("a saved entry reads")))
        .collect()
}

/// The values of `attr="…"` in `xml` (an `attr` preceded by a space or a
/// colon, so `s=` does not match `ss=`).
fn attr_values<'a>(xml: &'a str, attr: &str) -> Vec<&'a str> {
    let needle = format!("{attr}=\"");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = xml[from..].find(&needle) {
        let at = from + i;
        let start = at + needle.len();
        let end = start + xml[start..].find('"').unwrap();
        if matches!(xml[..at].chars().last(), Some(' ' | ':')) {
            out.push(&xml[start..end]);
        }
        from = end;
    }
    out
}

/// Every reference in a saved package resolves: each internal relationship
/// target is a part, each `r:…` attribute of each XML part names a
/// relationship in that part's rels, each content-type override is a part,
/// and no cell, row or column style or `dxfId` points past the styles.
fn assert_references_resolve(data: &[u8]) {
    let parts = parts_of(data);
    let has = |name: &str| parts.iter().any(|(n, _)| n == name);
    let text = |name: &str| {
        parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
    };
    for (name, bytes) in &parts {
        let xml = String::from_utf8_lossy(bytes);
        if name.ends_with(".rels") {
            let dir = rels_source_dir(name);
            for (id, _, target) in parse_rels(&xml) {
                if xml.contains(&format!("Id=\"{id}\" TargetMode=\"External\""))
                    || target.contains("://")
                {
                    continue;
                }
                let part = resolve_relative(dir, &target);
                assert!(has(&part), "{name} {id} targets missing {part}");
            }
            continue;
        }
        if name == "[Content_Types].xml" {
            for over in attr_values(&xml, "PartName") {
                assert!(
                    has(over.trim_start_matches('/')),
                    "override for missing {over}"
                );
            }
            continue;
        }
        let ids: Vec<&str> = ["r:id", "r:embed", "r:link", "r:pict"]
            .iter()
            .flat_map(|a| attr_values(&xml, a))
            .collect();
        if ids.is_empty() {
            continue;
        }
        let rels = text(&rels_part_name(name))
            .unwrap_or_else(|| panic!("{name} uses r:ids but has no rels"));
        let rels = parse_rels(&rels);
        for id in ids {
            assert!(
                rels.iter().any(|(rid, _, _)| rid == id),
                "{name} uses {id}, which its rels do not define"
            );
        }
    }
    let styles = text("xl/styles.xml").expect("a saved workbook has styles");
    let count = |tag: &str| -> usize {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        styles
            .find(&open)
            .and_then(|s| styles[s..].find(&close).map(|e| &styles[s..s + e]))
            .map_or(0, |block| {
                block.matches("<xf ").count()
                    + block.matches("<dxf>").count()
                    + block.matches("<dxf/>").count()
            })
    };
    let (xfs, dxfs) = (count("cellXfs"), count("dxfs"));
    for (name, bytes) in parts
        .iter()
        .filter(|(n, _)| n.starts_with("xl/worksheets/") && n.ends_with(".xml"))
    {
        let xml = String::from_utf8_lossy(bytes);
        for s in attr_values(&xml, "s")
            .into_iter()
            .chain(attr_values(&xml, "style"))
        {
            let s: usize = s.parse().unwrap();
            assert!(s < xfs, "{name} uses style {s}, but cellXfs has {xfs}");
        }
        for d in attr_values(&xml, "dxfId") {
            let d: usize = d.parse().unwrap();
            assert!(d < dxfs, "{name} uses dxfId {d}, but dxfs has {dxfs}");
        }
    }
}

/// Repair, save, and load the result strictly, checking every reference.
fn repair_and_resave(data: &[u8]) -> (SheetPackage, Repairs, Vec<u8>) {
    let (pkg, repairs) = load_xlsx_repair(data).expect("repairs");
    let saved = save_xlsx(&pkg);
    load_xlsx(&saved).expect("the repaired save loads strictly");
    assert_references_resolve(&saved);
    (pkg, repairs, saved)
}

#[test]
fn the_oracle_passes_the_sound_fixture() {
    assert_references_resolve(&fixture());
}

#[test]
fn repair_of_sound_workbook_is_byte_identical() {
    let data = fixture();
    let (pkg, repairs) = load_xlsx_repair(&data).unwrap();
    assert!(repairs.is_empty());
    assert_eq!(save_xlsx(&pkg), save_xlsx(&load_xlsx(&data).unwrap()));
}

#[test]
fn repair_styles_stubs_and_formats_survive() {
    let data = damage(&fixture(), "xl/styles.xml");
    assert_eq!(load_xlsx(&data).unwrap_err(), XlsxError::CorruptPart);
    let (mut pkg, repairs, saved) = repair_and_resave(&data);
    assert_eq!(repairs.emptied, ["xl/styles.xml"]);
    assert!(repairs.dropped.is_empty());
    let sheet = &pkg.workbook.sheets[0];
    assert!(sheet.cells.values().all(|c| c.style == 0));
    assert_eq!(sheet.cells[&(0, 0)].value, CellValue::Text("hello".into()));
    // The conditional format kept its dxfId, padded with empty formats.
    let styles = String::from_utf8(
        parts_of(&saved)
            .into_iter()
            .find(|(n, _)| n == "xl/styles.xml")
            .unwrap()
            .1,
    )
    .unwrap();
    assert!(
        styles.contains("<dxfs count=\"2\"><dxf/><dxf/></dxfs>"),
        "{styles}"
    );

    // A format applied after the repair has somewhere to go.
    let bold = pkg.workbook.styles.intern(Xf {
        bold: true,
        ..Xf::default()
    });
    assert!(bold > 0);
    pkg.workbook.sheets[0].cells.get_mut(&(0, 1)).unwrap().style = bold;
    let again = save_xlsx(&pkg);
    assert_references_resolve(&again);
    let back = load_xlsx(&again).unwrap();
    let style = back.workbook.sheets[0].cells[&(0, 1)].style;
    assert!(
        back.workbook.styles.xf(style).bold,
        "the bold format survives the save"
    );
}

#[test]
fn repair_worksheet_stubs_empty_sheet() {
    let data = damage(&fixture(), "xl/worksheets/sheet1.xml");
    let (pkg, repairs, saved) = repair_and_resave(&data);
    assert_eq!(repairs.emptied, ["xl/worksheets/sheet1.xml"]);
    assert_eq!(pkg.workbook.sheets.len(), 2, "the sheet survives, empty");
    assert_eq!(pkg.workbook.sheets[0].name, "One");
    assert!(pkg.workbook.sheets[0].cells.is_empty());
    assert!(pkg.workbook.sheets[0].drawings.is_empty());
    assert_eq!(
        pkg.workbook.sheets[1].cells[&(0, 0)].value,
        CellValue::Number(7.0)
    );
    // Its rels served elements that went with its XML.
    assert!(
        !parts_of(&saved)
            .iter()
            .any(|(n, _)| n == "xl/worksheets/_rels/sheet1.xml.rels")
    );
}

#[test]
fn repair_shared_strings_stubs_empty_table() {
    let data = damage(&fixture(), "xl/sharedStrings.xml");
    let (pkg, repairs, _) = repair_and_resave(&data);
    assert_eq!(repairs.emptied, ["xl/sharedStrings.xml"]);
    assert_eq!(
        pkg.workbook.sheets[0].cells[&(0, 1)].value,
        CellValue::Number(5.0)
    );
}

#[test]
fn repair_drawing_stubs_empty_drawing() {
    let data = damage(&fixture(), "xl/drawings/drawing1.xml");
    let (pkg, repairs, _) = repair_and_resave(&data);
    assert_eq!(repairs.emptied, ["xl/drawings/drawing1.xml"]);
    assert!(pkg.workbook.sheets[0].drawings.is_empty());
}

#[test]
fn repair_theme_drops_and_prunes() {
    let data = damage(&fixture(), "xl/theme/theme1.xml");
    let (pkg, repairs, saved) = repair_and_resave(&data);
    assert_eq!(repairs.dropped, ["xl/theme/theme1.xml"]);
    assert!(repairs.emptied.is_empty());
    assert!(pkg.part("xl/theme/theme1.xml").is_none());
    let parts = parts_of(&saved);
    let text =
        |n: &str| String::from_utf8(parts.iter().find(|(p, _)| p == n).unwrap().1.clone()).unwrap();
    assert!(!text("xl/_rels/workbook.xml.rels").contains("theme"));
    assert!(!text("[Content_Types].xml").contains("theme1"));
}

#[test]
fn repair_damaged_chart_fails_naming_part() {
    let data = damage(&fixture(), "xl/charts/chart1.xml");
    let err = load_xlsx_repair(&data).unwrap_err();
    assert_eq!(err, XlsxError::Unrepairable("xl/charts/chart1.xml".into()));
    assert_eq!(
        err.to_string(),
        "could not repair: xl/charts/chart1.xml is damaged"
    );
}

#[test]
fn repair_still_needs_workbook_part() {
    let data = fixture();
    assert_eq!(
        load_xlsx_repair(&damage(&data, "xl/workbook.xml")).unwrap_err(),
        XlsxError::MissingWorkbook
    );
    for required in [
        "xl/_rels/workbook.xml.rels",
        "[Content_Types].xml",
        "_rels/.rels",
    ] {
        assert_eq!(
            load_xlsx_repair(&damage(&data, required)).unwrap_err(),
            XlsxError::CorruptPart,
            "{required}"
        );
    }
}

#[test]
fn a_damaged_rels_part_needs_its_owner_gone() {
    let data = fixture();
    // The drawing's own rels: the drawing stays and names its chart by r:id.
    assert_eq!(
        load_xlsx_repair(&damage(&data, "xl/drawings/_rels/drawing1.xml.rels")).unwrap_err(),
        XlsxError::Unrepairable("xl/drawings/_rels/drawing1.xml.rels".into())
    );
    // A worksheet emptied with its rels damaged too is fine.
    let both = damage(
        &damage(&data, "xl/worksheets/sheet1.xml"),
        "xl/worksheets/_rels/sheet1.xml.rels",
    );
    let (_, repairs, _) = repair_and_resave(&both);
    assert_eq!(repairs.emptied, ["xl/worksheets/sheet1.xml"]);
}

#[test]
fn without_attrs_keeps_the_leading_spaces() {
    assert_eq!(
        without_attrs(
            " ht=\"15\" s=\"3\" customFormat=\"1\" ss=\"x\"",
            &["s", "customFormat"]
        ),
        " ht=\"15\" ss=\"x\""
    );
    assert_eq!(without_attrs("", &["s"]), "");
}
