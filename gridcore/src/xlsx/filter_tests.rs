//! Saving the AutoFilter the filter commands create or change (#690): the
//! `<autoFilter>` element, `_xlnm._FilterDatabase`, `<sheetPr filterMode>`
//! and the `<dxf>` a colour criterion needs.

use super::*;
use crate::filter::ColumnFilter;
use crate::sheet::{Cell, DefinedName, SheetAutoFilter};

fn text(pkg: &SheetPackage, part: &str) -> String {
    String::from_utf8_lossy(pkg.part(part).expect("part present")).into_owned()
}

/// Save, reload, and the reloaded package with the text of `part`.
fn saved(pkg: &SheetPackage, part: &str) -> (SheetPackage, String) {
    let re = load_xlsx(&save_xlsx(pkg)).expect("saved file reloads");
    let xml = text(&re, part);
    (re, xml)
}

/// A new workbook whose Sheet1 holds the list `A1:B3` (Rep, Units), with
/// `body` replacing the worksheet's `<sheetData/>` tail (`after`) and
/// `dxfs` as the styles part's `<dxfs>` block.
fn list(after: &str, dxfs: &str) -> SheetPackage {
    let mut pkg = new_xlsx();
    for (p, xml) in pkg.parts.iter_mut() {
        if p == "xl/worksheets/sheet1.xml" {
            let s = String::from_utf8_lossy(xml)
                .replace("<sheetData/>", &format!("<sheetData/>{after}"));
            *xml = s.into_bytes();
        }
        if p == "xl/styles.xml" && !dxfs.is_empty() {
            let s = String::from_utf8_lossy(xml)
                .replace("</styleSheet>", &format!("{dxfs}</styleSheet>"));
            *xml = s.into_bytes();
        }
    }
    let mut pkg = load_xlsx(&write_zip(&pkg.parts)).unwrap();
    let s = &mut pkg.workbook.sheets[0];
    s.set_cell(0, 0, Cell::text("Rep"));
    s.set_cell(0, 1, Cell::text("Units"));
    s.set_cell(1, 0, Cell::text("Ann"));
    s.set_cell(1, 1, Cell::number(5.0));
    s.set_cell(2, 0, Cell::text("Bo"));
    s.set_cell(2, 1, Cell::number(9.0));
    pkg
}

fn filter_db(range: &str) -> DefinedName {
    DefinedName {
        name: "_xlnm._FilterDatabase".into(),
        scope: Some(0),
        formula: format!("Sheet1!{range}"),
    }
}

#[test]
fn a_new_auto_filter_saves_its_element_hidden_name_and_filter_mode() {
    let mut pkg = list("", "");
    let s = &mut pkg.workbook.sheets[0];
    s.auto_filter = Some(SheetAutoFilter {
        range: (0, 0, 2, 1),
        criteria: vec![(0, ColumnFilter::values(vec!["Ann".into()]))],
        ..SheetAutoFilter::default()
    });
    s.filter_mode = Some(true);
    s.set_row_filtered(2, true);
    pkg.workbook.defined_names.push(filter_db("$A$1:$B$3"));
    let (re, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
    assert!(
        ws.contains(r#"<autoFilter ref="A1:B3"><filterColumn colId="0"><filters><filter val="Ann"/></filters></filterColumn></autoFilter>"#),
        "{ws}"
    );
    assert!(ws.contains(r#"<sheetPr filterMode="1"/>"#), "{ws}");
    let wb = text(&re, "xl/workbook.xml");
    assert!(
        wb.contains(r#"<definedName name="_xlnm._FilterDatabase" localSheetId="0" hidden="1">Sheet1!$A$1:$B$3</definedName>"#),
        "{wb}"
    );
    let af = re.workbook.sheets[0].auto_filter.as_ref().unwrap();
    assert_eq!(af.range, (0, 0, 2, 1));
    assert_eq!(
        af.criteria,
        pkg.workbook.sheets[0]
            .auto_filter
            .as_ref()
            .unwrap()
            .criteria
    );
    // The hidden row comes back filtered, not hidden by hand.
    assert!(re.workbook.sheets[0].row_filtered(2));
    // Saving the reloaded file again changes nothing.
    assert_eq!(
        text(
            &load_xlsx(&save_xlsx(&re)).unwrap(),
            "xl/worksheets/sheet1.xml"
        ),
        ws
    );
}

#[test]
fn turning_the_filter_off_drops_the_element_the_name_and_filter_mode() {
    let mut pkg = list("", "");
    let s = &mut pkg.workbook.sheets[0];
    s.auto_filter = Some(SheetAutoFilter {
        range: (0, 0, 2, 1),
        ..SheetAutoFilter::default()
    });
    s.filter_mode = Some(true);
    pkg.workbook.defined_names.push(filter_db("$A$1:$B$3"));
    let mut pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
    assert!(text(&pkg, "xl/workbook.xml").contains("_FilterDatabase"));
    pkg.workbook.sheets[0].auto_filter = None;
    pkg.workbook.sheets[0].filter_mode = Some(false);
    pkg.workbook
        .defined_names
        .retain(|d| d.name != "_xlnm._FilterDatabase");
    let (re, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
    assert!(!ws.contains("autoFilter"), "{ws}");
    assert!(!ws.contains("sheetPr"), "{ws}");
    assert!(!text(&re, "xl/workbook.xml").contains("_FilterDatabase"));
}

#[test]
fn a_changed_criterion_rewrites_the_element_keeping_what_we_do_not_model() {
    let filter = concat!(
        r#"<autoFilter ref="A1:C3"><filterColumn colId="0" hiddenButton="1"/>"#,
        r#"<filterColumn colId="2"><customFilters><customFilter operator="greaterThan" val="5"/></customFilters></filterColumn>"#,
        r#"<sortState ref="A2:C3"><sortCondition ref="B2:B3"/></sortState></autoFilter>"#
    );
    let mut pkg = list(filter, "");
    let af = pkg.workbook.sheets[0].auto_filter.as_mut().unwrap();
    assert_eq!(af.criteria.len(), 2);
    af.criteria
        .insert(1, (1, ColumnFilter::values(vec!["5".into()])));
    let (re, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
    assert!(
        ws.contains(concat!(
            r#"<autoFilter ref="A1:C3"><filterColumn colId="0" hiddenButton="1"/>"#,
            r#"<filterColumn colId="1"><filters><filter val="5"/></filters></filterColumn>"#,
            r#"<filterColumn colId="2"><customFilters><customFilter operator="greaterThan" val="5"/></customFilters></filterColumn>"#,
            r#"</autoFilter>"#
        )),
        "{ws}"
    );
    assert_eq!(
        re.workbook.sheets[0]
            .auto_filter
            .as_ref()
            .unwrap()
            .criteria
            .len(),
        3
    );
}

#[test]
fn a_colour_criterion_appends_its_dxf_and_leaves_the_others_byte_for_byte() {
    let cf_dxf = r#"<dxf><font><b/><color theme="5"/></font><numFmt numFmtId="164" formatCode="0.0%"/><border><left style="thin"><color auto="1"/></left></border></dxf>"#;
    let mut pkg = list("", &format!(r#"<dxfs count="1">{cf_dxf}</dxfs>"#));
    pkg.workbook.sheets[0].auto_filter = Some(SheetAutoFilter {
        range: (0, 0, 2, 1),
        criteria: vec![(
            0,
            ColumnFilter::Color {
                cell: true,
                rgb: Some((0, 0xB0, 0x50)),
                dxf_id: None,
            },
        )],
        ..SheetAutoFilter::default()
    });
    let (re, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
    assert!(ws.contains(r#"<colorFilter dxfId="1"/>"#), "{ws}");
    let styles = text(&re, "xl/styles.xml");
    assert!(
        styles.contains(&format!(
            r#"<dxfs count="2">{cf_dxf}{}</dxfs>"#,
            crate::filter::color_dxf_xml(true, Some((0, 0xB0, 0x50)))
        )),
        "{styles}"
    );
    // It loads back as the same colour.
    let af = re.workbook.sheets[0].auto_filter.as_ref().unwrap();
    assert_eq!(
        af.criteria[0].1,
        ColumnFilter::Color {
            cell: true,
            rgb: Some((0, 0xB0, 0x50)),
            dxf_id: Some(1),
        }
    );
    // A second save reuses that dxf rather than adding another.
    let again = text(&load_xlsx(&save_xlsx(&re)).unwrap(), "xl/styles.xml");
    assert_eq!(again.matches("<dxf>").count(), 2);
}

#[test]
fn an_added_auto_filter_leaves_the_other_elements_in_place() {
    let others = concat!(
        r#"<mergeCells count="1"><mergeCell ref="D1:E1"/></mergeCells>"#,
        r#"<conditionalFormatting sqref="B2:B3"><cfRule type="cellIs" dxfId="0" priority="1" operator="greaterThan"><formula>6</formula></cfRule></conditionalFormatting>"#,
        r#"<dataValidations count="1"><dataValidation type="whole" sqref="B2:B3"><formula1>0</formula1></dataValidation></dataValidations>"#,
    );
    let mut pkg = list(others, r#"<dxfs count="1"><dxf/></dxfs>"#);
    pkg.workbook.sheets[0].auto_filter = Some(SheetAutoFilter {
        range: (0, 0, 2, 1),
        ..SheetAutoFilter::default()
    });
    let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
    assert!(
        ws.contains(&format!(r#"<autoFilter ref="A1:B3"/>{others}"#)),
        "{ws}"
    );
}

#[test]
fn an_icon_set_rule_loads_as_one_and_saves_as_it_was() {
    let rule = r#"<conditionalFormatting sqref="B2:B3"><cfRule type="iconSet" priority="1"><iconSet iconSet="3Arrows" reverse="1"><cfvo type="percent" val="0"/><cfvo type="percentile" val="40"/><cfvo type="num" val="8" gte="0"/></iconSet></cfRule></conditionalFormatting>"#;
    let mut pkg = list(rule, "");
    let cf = &pkg.workbook.sheets[0].cond_formats[0];
    match &cf.rules[0].kind {
        crate::sheet::CfKind::IconSet {
            set,
            reverse,
            cfvos,
            ..
        } => {
            assert_eq!((set.as_str(), *reverse, cfvos.len()), ("3Arrows", true, 3));
            assert_eq!(
                cfvos[2],
                crate::sheet::Cfvo {
                    kind: "num".into(),
                    val: "8".into(),
                    gte: false
                }
            );
        }
        k => panic!("{k:?}"),
    }
    // 9 > 8 is the top band, reversed to the first icon.
    assert_eq!(
        crate::cf::cell_icon(&pkg.workbook, 0, 2, 1),
        Some(("3Arrows".to_string(), 0))
    );
    let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
    assert!(ws.contains(rule), "{ws}");
    // A row inserted below it moves nothing.
    crate::edit::insert_rows(&mut pkg.workbook, 0, 10, 1);
    let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
    assert!(ws.contains(rule), "{ws}");
}

#[test]
fn theme_colours_load_as_unresolved_and_save_unchanged() {
    // cellXfs 1: a theme-coloured solid fill and a theme 5 font; the default
    // font's theme 1 colour is the ordinary text colour.
    let mut pkg = new_xlsx();
    for (p, xml) in pkg.parts.iter_mut() {
        if p == "xl/styles.xml" {
            let s = String::from_utf8_lossy(xml)
                .replace(
                    r#"<fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>"#,
                    r#"<fonts count="2"><font><sz val="11"/><color theme="1"/><name val="Calibri"/></font><font><sz val="11"/><color theme="5"/><name val="Calibri"/></font></fonts>"#,
                )
                .replace(
                    r#"<fill><patternFill patternType="gray125"/></fill></fills>"#,
                    r#"<fill><patternFill patternType="gray125"/></fill><fill><patternFill patternType="solid"><fgColor theme="4" tint="0.4"/><bgColor indexed="64"/></patternFill></fill></fills>"#,
                )
                .replace(
                    r#"<cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs>"#,
                    r#"<cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="1" fillId="2" borderId="0" xfId="0" applyFill="1"/></cellXfs>"#,
                );
            *xml = s.into_bytes();
        }
        if p == "xl/worksheets/sheet1.xml" {
            let s = String::from_utf8_lossy(xml).replace(
                "<sheetData/>",
                r#"<sheetData><row r="1"><c r="A1" s="1"><v>1</v></c><c r="B1"><v>2</v></c></row></sheetData>"#,
            );
            *xml = s.into_bytes();
        }
    }
    let file = write_zip(&pkg.parts);
    let pkg = load_xlsx(&file).unwrap();
    let (x0, x1) = (pkg.workbook.styles.xf(0), pkg.workbook.styles.xf(1));
    assert!(!x0.fill_unresolved && !x0.color_unresolved);
    assert!(x1.fill_unresolved && x1.color_unresolved);
    assert_eq!(
        crate::cf::cell_fill(&pkg.workbook, 0, 0, 0),
        crate::cf::Shown::Unknown
    );
    assert_eq!(
        crate::cf::cell_fill(&pkg.workbook, 0, 0, 1),
        crate::cf::Shown::None
    );
    assert_eq!(
        crate::cf::cell_font_color(&pkg.workbook, 0, 0, 1),
        crate::cf::Shown::None
    );
    let before = text(&pkg, "xl/styles.xml");
    let (_, after) = saved(&pkg, "xl/styles.xml");
    assert_eq!(after, before);
}

#[test]
fn a_filter_the_commands_set_survives_a_save_with_its_hidden_rows() {
    let mut pkg = list("", "");
    let wb = &mut pkg.workbook;
    crate::filter::auto_filter_on(wb, 0, (0, 0)).unwrap();
    let top = ColumnFilter::Top10 {
        top: true,
        percent: false,
        val: 1.0,
        filter_val: None,
    };
    let out = crate::filter::set_criterion(wb, 0, 1, Some(top), 45000.0).unwrap();
    assert_eq!(crate::filter::status_text(&out), "1 of 2 records found");
    let (re, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
    assert!(
        ws.contains(r#"<autoFilter ref="A1:B3"><filterColumn colId="1"><top10 val="1" filterVal="9"/></filterColumn></autoFilter>"#),
        "{ws}"
    );
    let (a, b) = (&pkg.workbook.sheets[0], &re.workbook.sheets[0]);
    let (fa, fb) = (
        a.auto_filter.as_ref().unwrap(),
        b.auto_filter.as_ref().unwrap(),
    );
    assert_eq!((fa.range, &fa.criteria), (fb.range, &fb.criteria));
    assert_eq!(a.filtered_rows, b.filtered_rows);
    assert!(b.row_filtered(1) && !b.row_hidden(2));
}

/// A list whose B2 a conditional format fills with theme colour 5, and
/// `filter` after its data; dxf 0 is that theme fill.
fn theme_cf_list(filter: &str) -> SheetPackage {
    let cf = r#"<conditionalFormatting sqref="B2"><cfRule type="expression" dxfId="0" priority="1"><formula>TRUE</formula></cfRule></conditionalFormatting>"#;
    let mut pkg = list(
        &format!("{filter}{cf}"),
        r#"<dxfs count="1"><dxf><fill><patternFill><bgColor theme="5"/></patternFill></fill></dxf></dxfs>"#,
    );
    // The data rows as `list` builds them, and B3's hidden row as a file
    // filtered on that colour would leave it.
    pkg.workbook.sheets[0].set_row_hidden(2, true);
    pkg = load_xlsx(&save_xlsx(&pkg)).unwrap();
    pkg
}

#[test]
fn a_conditional_theme_fill_is_an_unknown_colour() {
    let mut pkg = theme_cf_list("");
    let d = &pkg.workbook.styles.dxfs[0];
    assert!(d.fill.is_none() && d.fill_unresolved);
    let wb = &mut pkg.workbook;
    assert_eq!(crate::cf::cell_fill(wb, 0, 1, 1), crate::cf::Shown::Unknown);
    crate::filter::auto_filter_on(wb, 0, (0, 0)).unwrap();
    // No Fill keeps only B3, whose fill is really none.
    let none = ColumnFilter::Color {
        cell: true,
        rgb: None,
        dxf_id: None,
    };
    crate::filter::set_criterion(wb, 0, 1, Some(none), 45000.0).unwrap();
    assert!(wb.sheets[0].row_hidden(1) && !wb.sheets[0].row_hidden(2));
    // Filter by Selected Cell's Color refuses a colour it can't read.
    assert_eq!(
        crate::filter::filter_by_cell(wb, 0, (1, 1), crate::filter::ByCell::CellColor, 45000.0),
        Err(crate::filter::FilterError::NoColor)
    );
}

#[test]
fn a_loaded_filter_on_a_theme_colour_is_kept_as_it_was() {
    let filter = r#"<autoFilter ref="A1:B3"><filterColumn colId="1"><colorFilter dxfId="0"/></filterColumn></autoFilter>"#;
    let mut pkg = theme_cf_list(filter);
    let af = pkg.workbook.sheets[0].auto_filter.as_ref().unwrap();
    assert!(
        matches!(af.criteria[0].1, ColumnFilter::Raw(_)),
        "{:?}",
        af.criteria
    );
    assert!(pkg.workbook.sheets[0].row_filtered(2));
    // Reapply leaves the row it hid hidden, and the element as it was.
    crate::filter::reapply(&mut pkg.workbook, 0, 45000.0).unwrap();
    assert!(pkg.workbook.sheets[0].row_filtered(2));
    let (_, ws) = saved(&pkg, "xl/worksheets/sheet1.xml");
    assert!(ws.contains(filter), "{ws}");
}
