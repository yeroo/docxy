use super::*;
use crate::formula::{CellMove, move_block_formula};
use crate::sheet::{Cell, DefinedName, Sheet, parse_cell_name};

/// Source columns from `c` on, one per copied column.
fn from(c: u32) -> Vec<u32> {
    (c..c + 16).collect()
}

fn f(text: &str) -> Cell {
    Cell {
        formula: Some(text.to_string()),
        ..Cell::default()
    }
}

fn formula(wb: &Workbook, s: usize, name: &str) -> Option<String> {
    let (r, c) = parse_cell_name(name).unwrap();
    wb.sheets[s].cell(r, c).and_then(|c| c.formula.clone())
}

fn book(sheets: &[(&str, &[(&str, Cell)])]) -> Workbook {
    let sheets = sheets
        .iter()
        .map(|(name, cells)| {
            let mut sheet = Sheet {
                name: name.to_string(),
                ..Sheet::default()
            };
            for (at, cell) in cells.iter() {
                let (r, c) = parse_cell_name(at).unwrap();
                sheet.set_cell(r, c, cell.clone());
            }
            sheet
        })
        .collect();
    Workbook {
        sheets,
        ..Workbook::default()
    }
}

// ---- paste_tiles -------------------------------------------------------------

#[test]
fn a_paste_area_tiles_by_whole_copies_and_refuses_any_other_shape() {
    let none = (0, 0);
    // One cell: one copy, whatever its size.
    assert_eq!(paste_tiles((2, 1), (0, 3, 0, 3), none), Some((1, 1)));
    // D1:D6 under a 2x1 copy: three copies down.
    assert_eq!(paste_tiles((2, 1), (0, 3, 5, 3), none), Some((3, 1)));
    // D1:E2 under a 1x1 copy: four copies.
    assert_eq!(paste_tiles((1, 1), (0, 3, 1, 4), none), Some((2, 2)));
    // D1:D3 is not a whole number of 2-row copies.
    assert_eq!(paste_tiles((2, 1), (0, 3, 2, 3), none), None);
    // A row one cell deep but three wide takes one copy down, three across.
    assert_eq!(paste_tiles((2, 1), (0, 3, 0, 5), none), Some((1, 3)));
    // Narrower than the copy (but not one cell) is refused.
    assert_eq!(paste_tiles((1, 3), (0, 0, 0, 1), none), None);
}

#[test]
fn a_whole_column_paste_area_reaches_the_used_rows_in_whole_copies() {
    let col_d = (0, 3, MAX_ROWS - 1, 3);
    // Used rows 1..6: three 2-row copies.
    assert_eq!(paste_tiles((2, 1), col_d, (6, 8)), Some((3, 1)));
    // Used rows 1..5: rounded up to three copies.
    assert_eq!(paste_tiles((2, 1), col_d, (5, 8)), Some((3, 1)));
    // An empty sheet still takes one copy.
    assert_eq!(paste_tiles((2, 1), col_d, (0, 0)), Some((1, 1)));
    // A whole row the same way, across.
    let row_1 = (0, 0, 0, MAX_COLS - 1);
    assert_eq!(paste_tiles((1, 2), row_1, (3, 5)), Some((1, 3)));
}

// ---- translated_block / tiled_block -----------------------------------------

#[test]
fn copied_formulas_translate_to_where_they_land() {
    let cells = vec![vec![f("A1*10")], vec![f("$A2+A$1+$B$1")]];
    let out = translated_block(&cells, &[0, 1], &from(1), (0, 4));
    assert_eq!(out[0][0].formula.as_deref(), Some("D1*10"));
    assert_eq!(out[1][0].formula.as_deref(), Some("$A2+D$1+$B$1"));
    // Pushed off the grid: #REF!.
    let out = translated_block(&[vec![f("A1")]], &[1], &from(0), (0, 0));
    assert_eq!(out[0][0].formula.as_deref(), Some("#REF!"));
}

#[test]
fn a_copy_pasted_where_it_came_from_keeps_its_text_exactly() {
    // Reprinting would drop the spaces and the lower case.
    let cells = vec![vec![f("sum( a1 , 2 )")], vec![f("not a formula ((")]];
    let out = translated_block(&cells, &[0, 1], &from(1), (0, 1));
    assert_eq!(out[0][0].formula.as_deref(), Some("sum( a1 , 2 )"));
    assert_eq!(out[1][0].formula.as_deref(), Some("not a formula (("));
    // Moved, an unparseable formula keeps its text too.
    let out = translated_block(&cells, &[0, 1], &from(1), (5, 5));
    assert_eq!(out[1][0].formula.as_deref(), Some("not a formula (("));
}

#[test]
fn each_row_of_a_filtered_copy_translates_by_its_own_offset() {
    // Rows 1, 2, 4, 6 of a filter (3 and 5 hidden), pasted at D1.
    let cells = vec![vec![f("A1")], vec![f("A2")], vec![f("A4")], vec![f("A6")]];
    let out = translated_block(&cells, &[0, 1, 3, 5], &from(1), (0, 3));
    let got: Vec<_> = out.iter().map(|r| r[0].formula.clone().unwrap()).collect();
    // Row 4 lands on row 3: one row up, two columns across.
    assert_eq!(got, ["C1", "C2", "C3", "C4"]);
}

#[test]
fn tiles_each_translate_to_their_own_corner() {
    let cells = vec![vec![f("A1*10")], vec![f("A2*10")]];
    let block = tiled_block(&cells, &[0, 1], &from(1), (0, 3), (3, 1));
    let got: Vec<_> = block
        .iter()
        .map(|r| r[0].formula.clone().unwrap())
        .collect();
    assert_eq!(got, ["C1*10", "C2*10", "C3*10", "C4*10", "C5*10", "C6*10"]);
    // Across, with a short row padded so the next tile keeps its column.
    let cells = vec![vec![f("A1"), f("B1")], vec![f("A2")]];
    let block = tiled_block(&cells, &[0, 1], &from(0), (0, 0), (1, 2));
    assert_eq!(block[0].len(), 4);
    assert_eq!(block[1].len(), 4);
    assert_eq!(block[0][2].formula.as_deref(), Some("C1"));
    assert_eq!(block[1][2].formula.as_deref(), Some("C2"));
    assert_eq!(block[1][1].formula, None);
}

#[test]
fn a_tiled_block_stops_at_the_grids_edge() {
    let cells = vec![vec![Cell::number(1.0)], vec![Cell::number(2.0)]];
    let block = tiled_block(&cells, &[0, 1], &from(0), (MAX_ROWS - 3, 0), (2, 1));
    assert_eq!(block.len(), 3);
}

// ---- move_refs / move_block_formula -----------------------------------------

/// A1:B1 on `src` cut and pasted at A10 on `dst`.
fn cut_a1_b1_to_a10<'a>(src: &'a str, dst: &'a str) -> CellMove<'a> {
    CellMove {
        src,
        dst,
        rect: (0, 0, 0, 1),
        dr: 9,
        dc: 0,
    }
}

#[test]
fn references_to_cut_cells_follow_them() {
    let mut wb = book(&[(
        "Sheet1",
        &[
            ("A1", Cell::number(1.0)),
            ("B1", f("A1*10")),
            ("H1", f("A1+B1")),
            ("H2", f("SUM(A1:B1)")),
            ("H3", f("SUM(A1:C1)")),
            ("H4", f("$A$1")),
            ("H5", f("C1")),
        ],
    )]);
    move_refs(&mut wb, 0, &cut_a1_b1_to_a10("Sheet1", "Sheet1"));
    assert_eq!(formula(&wb, 0, "H1").as_deref(), Some("A10+B10"));
    assert_eq!(formula(&wb, 0, "H2").as_deref(), Some("SUM(A10:B10)"));
    // Only one corner moved: the range stays.
    assert_eq!(formula(&wb, 0, "H3").as_deref(), Some("SUM(A1:C1)"));
    // Absolute too: the cell moved, not the formula.
    assert_eq!(formula(&wb, 0, "H4").as_deref(), Some("$A$10"));
    assert_eq!(formula(&wb, 0, "H5").as_deref(), Some("C1"));
    // The moved cells are the caller's.
    assert_eq!(formula(&wb, 0, "B1").as_deref(), Some("A1*10"));
}

#[test]
fn a_move_to_another_sheet_qualifies_the_references_that_follow() {
    let mut wb = book(&[
        ("Sheet1", &[("A1", Cell::number(1.0)), ("H1", f("A1+B1"))]),
        ("Sheet2", &[("C1", f("Sheet1!A1")), ("C2", f("Sheet1!C1"))]),
        ("Other", &[("A1", f("A1"))]),
    ]);
    wb.defined_names.push(DefinedName {
        name: "Base".into(),
        scope: None,
        formula: "Sheet1!$A$1".into(),
    });
    move_refs(&mut wb, 0, &cut_a1_b1_to_a10("Sheet1", "Sheet2"));
    assert_eq!(
        formula(&wb, 0, "H1").as_deref(),
        Some("Sheet2!A10+Sheet2!B10")
    );
    assert_eq!(formula(&wb, 1, "C1").as_deref(), Some("Sheet2!A10"));
    assert_eq!(formula(&wb, 1, "C2").as_deref(), Some("Sheet1!C1"));
    // Another sheet's unqualified A1 is its own A1.
    assert_eq!(formula(&wb, 2, "A1").as_deref(), Some("A1"));
    assert_eq!(wb.defined_names[0].formula, "Sheet2!$A$10");
}

#[test]
fn move_refs_reports_whether_it_rewrote_a_formula() {
    let mut wb = book(&[("Sheet1", &[("H1", f("A1+1")), ("H2", f("C1"))])]);
    assert!(move_refs(&mut wb, 0, &cut_a1_b1_to_a10("Sheet1", "Sheet1")));
    // Nothing reads the moved cells any more.
    assert!(!move_refs(
        &mut wb,
        0,
        &cut_a1_b1_to_a10("Sheet1", "Sheet1")
    ));
    let mut wb = book(&[("Sheet1", &[("H1", f("sum( c1 , 2 )"))])]);
    assert!(!move_refs(
        &mut wb,
        0,
        &cut_a1_b1_to_a10("Sheet1", "Sheet1")
    ));
    // A defined name alone counts.
    let mut wb = book(&[("Sheet1", &[("H1", f("C1"))])]);
    wb.defined_names.push(DefinedName {
        name: "Base".into(),
        scope: None,
        formula: "Sheet1!$A$1".into(),
    });
    assert!(move_refs(&mut wb, 0, &cut_a1_b1_to_a10("Sheet1", "Sheet1")));
}

#[test]
fn a_move_leaves_formulas_it_does_not_reach_byte_for_byte() {
    let mut wb = book(&[("Sheet1", &[("H1", f("sum( c1 , 2 )"))])]);
    move_refs(&mut wb, 0, &cut_a1_b1_to_a10("Sheet1", "Sheet1"));
    assert_eq!(formula(&wb, 0, "H1").as_deref(), Some("sum( c1 , 2 )"));
}

#[test]
fn a_moved_cells_own_references_follow_the_block_and_keep_the_rest() {
    let same = cut_a1_b1_to_a10("Sheet1", "Sheet1");
    assert_eq!(
        move_block_formula("A1*10", &same).as_deref(),
        Some("A10*10")
    );
    assert_eq!(
        move_block_formula("C1+A1", &same).as_deref(),
        Some("C1+A10")
    );
    // Nothing reached: the text stays as it was.
    assert_eq!(move_block_formula("c1 + 2", &same), None);

    let other = cut_a1_b1_to_a10("Sheet1", "Sheet2");
    // Moved to Sheet2, a ref to a moved cell names its new home unqualified.
    assert_eq!(
        move_block_formula("A1*10", &other).as_deref(),
        Some("A10*10")
    );
    assert_eq!(
        move_block_formula("Sheet1!A1*10", &other).as_deref(),
        Some("A10*10")
    );
    // Everything else keeps reading Sheet1.
    assert_eq!(
        move_block_formula("C1", &other).as_deref(),
        Some("Sheet1!C1")
    );
    assert_eq!(
        move_block_formula("SUM(A1:C1)", &other).as_deref(),
        Some("SUM(Sheet1!A1:C1)")
    );
    assert_eq!(
        move_block_formula("SUM(C:C)", &other).as_deref(),
        Some("SUM(Sheet1!C:C)")
    );
    assert_eq!(move_block_formula("Other!C1", &other), None);
}

fn cf(ranges: &[(u32, u32, u32, u32)], kind: crate::sheet::CfKind) -> crate::sheet::CondFormat {
    crate::sheet::CondFormat {
        ranges: ranges.to_vec(),
        rules: vec![crate::sheet::CfRule {
            kind,
            dxf_id: None,
            priority: 1,
        }],
        ix: None,
        copy_of: None,
    }
}

fn expr(formula: &str) -> crate::sheet::CfKind {
    crate::sheet::CfKind::Expression {
        formula: formula.into(),
    }
}

fn rule_formula(wb: &Workbook, s: usize, i: usize) -> String {
    wb.sheets[s].cond_formats[i].rules[0].formulas()[0].clone()
}

fn local(name: &str, scope: Option<usize>, formula: &str) -> DefinedName {
    DefinedName {
        name: name.into(),
        scope,
        formula: formula.into(),
    }
}

fn table1(sheet: usize, range: (u32, u32, u32, u32)) -> crate::sheet::Table {
    crate::sheet::Table {
        name: "Table1".into(),
        sheet,
        range,
        header_rows: 1,
        totals_rows: 0,
        columns: vec!["Qty".into(), "Price".into()],
        ..Default::default()
    }
}

/// A1:F1 on Sheet1 cut to A20 (#1687's r2 example).
fn cut_a1_f1_to_a20<'a>() -> CellMove<'a> {
    CellMove {
        src: "Sheet1",
        dst: "Sheet1",
        rect: (0, 0, 0, 5),
        dr: 19,
        dc: 0,
    }
}

#[test]
fn move_refs_rules_stay_follows_absolute_refs_in_rules_left_in_place() {
    let mut wb = book(&[("Sheet1", &[("F1", Cell::number(1.0))])]);
    let sheet = &mut wb.sheets[0];
    // Rules over A1:D10, which the cut A1:F1 meets: left in place.
    sheet
        .cond_formats
        .push(cf(&[(0, 0, 9, 3)], expr("$F$1=\"on\"")));
    sheet
        .cond_formats
        .push(cf(&[(0, 0, 9, 3)], expr("SUM($F$1:$G$1)+SUM($A$1:$G$1)")));
    sheet
        .cond_formats
        .push(cf(&[(0, 0, 9, 3)], expr("F1+$F1+F$1+$F$1#")));
    // A rule the cut doesn't meet is re-pointed whole.
    sheet.cond_formats.push(cf(&[(5, 7, 9, 9)], expr("F1>0")));
    assert!(move_refs_rules_stay(&mut wb, 0, &cut_a1_f1_to_a20()));
    assert_eq!(rule_formula(&wb, 0, 0), "$F$20=\"on\"");
    // A fully absolute range follows when both corners moved; one reaching
    // past the cut stays.
    assert_eq!(
        rule_formula(&wb, 0, 1),
        "SUM($F$1:$G$1)+SUM($A$1:$G$1)",
        "G1 is not cut: neither range has both corners moved"
    );
    // In the cut cells relative and mixed refs stay, and a fully absolute
    // spill ref follows. Beside the cut, A2:A10 read F1 through `F$1`, so
    // they follow it; B2:D10 read G1:I1 there, and are split from them
    // (#1458).
    assert_eq!(rule_formula(&wb, 0, 2), "F1+$F1+F$1+$F$20#");
    // Beside the cut, only H6 read F1 (#1458).
    assert_eq!(
        blocks(&wb, 0)[2..],
        [
            block("A1:D1", "F1+$F1+F$1+$F$20#", None, None),
            block("A2:A10", "F2+$F2+F$20+$F$20#", None, None),
            block("B2:D10", "G2+$F2+G$1+$F$20#", None, None),
            block("H6", "F20>0", None, None),
            block("I6:J10", "G1>0", None, None),
            block("H7:H10", "F2>0", None, None),
        ]
    );

    // Both corners in the cut: the absolute range follows.
    let mut wb = book(&[("Sheet1", &[])]);
    wb.sheets[0]
        .cond_formats
        .push(cf(&[(0, 0, 9, 3)], expr("SUM($E$1:$F$1)>0")));
    assert!(move_refs_rules_stay(&mut wb, 0, &cut_a1_f1_to_a20()));
    assert_eq!(rule_formula(&wb, 0, 0), "SUM($E$20:$F$20)>0");

    // An absolute reference into the rule's own cells reads them, and they
    // stay with the rule (#880): A1:F1 cut, a rule on A1:D10.
    let mut wb = book(&[("Sheet1", &[])]);
    wb.sheets[0]
        .cond_formats
        .push(cf(&[(0, 0, 9, 3)], expr("A1>$B$1+SUM($A$1:$B$1)+$F$1")));
    assert!(move_refs_rules_stay(&mut wb, 0, &cut_a1_f1_to_a20()));
    assert_eq!(rule_formula(&wb, 0, 0), "A1>$B$1+SUM($A$1:$B$1)+$F$20");

    // A rule left in place with only relative refs: nothing is rewritten.
    let mut wb = book(&[("Sheet1", &[])]);
    wb.sheets[0]
        .cond_formats
        .push(cf(&[(0, 0, 99, 0)], expr("A1>5")));
    let cut_a1_to_c1 = CellMove {
        src: "Sheet1",
        dst: "Sheet1",
        rect: (0, 0, 0, 0),
        dr: 0,
        dc: 2,
    };
    assert!(!move_refs_rules_stay(&mut wb, 0, &cut_a1_to_c1));
    assert_eq!(rule_formula(&wb, 0, 0), "A1>5");
}

/// Every place the rewrite walks, each alone in a workbook: the dry run
/// says what [`move_refs_rules_stay`] then does, and writes nothing.
#[test]
fn cut_moves_refs_agrees_with_move_refs_rules_stay() {
    use crate::sheet::{CfKind, Cfvo, ExtKind, ExtRule};
    let mv = cut_a1_f1_to_a20();
    let ext = |kind, formula: &str| ExtRule {
        kind,
        ranges: vec![(4, 7, 9, 9)],
        formulas: vec![formula.into()],
        ix: 0,
        validation: None,
    };
    let cfvo_rule = |val: &str| CfKind::Other {
        rule_type: "colorScale".into(),
        formulas: Vec::new(),
        cfvos: vec![Cfvo {
            kind: "formula".into(),
            val: val.into(),
            gte: true,
        }],
    };
    let mut array = f("A1*2");
    array.f_attrs = Some("t=\"array\" ref=\"H1\"".into());
    let mut shared = f("A1*2");
    shared.f_attrs = Some("t=\"shared\" ref=\"H1:H2\" si=\"0\"".into());
    type Case = (&'static str, bool, Box<dyn Fn(&mut Workbook)>);
    let cases: Vec<Case> = vec![
        ("nothing reads the cut", false, Box::new(|_| {})),
        (
            "a cell outside the cut",
            true,
            Box::new(|wb| wb.sheets[0].set_cell(5, 7, f("A1+1"))),
        ),
        (
            "a cell reading past the cut",
            false,
            Box::new(|wb| wb.sheets[0].set_cell(5, 7, f("H1+1"))),
        ),
        (
            "an array formula",
            true,
            Box::new(move |wb| wb.sheets[0].set_cell(5, 7, array.clone())),
        ),
        (
            "a shared formula held verbatim",
            false,
            Box::new(move |wb| wb.sheets[0].set_cell(5, 7, shared.clone())),
        ),
        (
            "a cell of the cut itself",
            false,
            Box::new(|wb| wb.sheets[0].set_cell(0, 1, f("A1*2"))),
        ),
        (
            "a rule's formula",
            true,
            Box::new(|wb| (wb.sheets[0].cond_formats).push(cf(&[(4, 7, 9, 9)], expr("A1>0")))),
        ),
        (
            "a rule split by the cut (#1458)",
            true,
            Box::new(|wb| (wb.sheets[0].cond_formats).push(cf(&[(5, 7, 9, 9)], expr("F1>0")))),
        ),
        (
            "a rule over the cut split where a cell beside it reads it (#1458)",
            true,
            Box::new(|wb| (wb.sheets[0].cond_formats).push(cf(&[(0, 0, 9, 3)], expr("F$1=1")))),
        ),
        (
            "a rule over the cut whose cells beside it read past it",
            false,
            Box::new(|wb| (wb.sheets[0].cond_formats).push(cf(&[(0, 0, 9, 3)], expr("G$1=1")))),
        ),
        (
            "a rule whose corner alone reads the cut (#1458 r1)",
            true,
            Box::new(|wb| {
                (wb.sheets[0].cond_formats).push(cf(&[(0, 7, 4, 7), (5, 6, 9, 6)], expr("F1>0")))
            }),
        ),
        (
            "a rule's cfvo formula",
            true,
            Box::new(move |wb| {
                (wb.sheets[0].cond_formats).push(cf(&[(4, 7, 9, 9)], cfvo_rule("$A$1")))
            }),
        ),
        (
            "an x14 rule",
            true,
            Box::new(move |wb| wb.sheets[0].ext_rules.push(ext(ExtKind::Cf, "A1>0"))),
        ),
        (
            "an x14 validation (not walked)",
            false,
            Box::new(move |wb| wb.sheets[0].ext_rules.push(ext(ExtKind::Dv, "A1>0"))),
        ),
        (
            "a rule left in place, absolute",
            true,
            Box::new(|wb| (wb.sheets[0].cond_formats).push(cf(&[(0, 0, 9, 3)], expr("$F$1=1")))),
        ),
        (
            "a rule left in place, relative",
            false,
            Box::new(|wb| (wb.sheets[0].cond_formats).push(cf(&[(0, 0, 9, 3)], expr("F1=1")))),
        ),
        (
            "a defined name",
            true,
            Box::new(|wb| (wb.defined_names).push(local("Base", None, "Sheet1!$A$1"))),
        ),
        (
            "a Consolidate reference",
            true,
            Box::new(|wb| {
                wb.sheets[1].consolidate = Some(crate::edit::ConsolidateSettings {
                    refs: vec!["Sheet1!$A$1:$B$1".into()],
                    ..Default::default()
                })
            }),
        ),
        (
            "a calculated column",
            true,
            Box::new(|wb| {
                let mut t = table1(1, (0, 0, 3, 1));
                t.calculated_formulas = vec![None, Some("Sheet1!A1*2".into())];
                wb.tables.push(t);
            }),
        ),
    ];
    for (what, expected, setup) in cases {
        let mut wb = book(&[("Sheet1", &[]), ("Sheet2", &[])]);
        setup(&mut wb);
        let (sheets, names, tables) = (
            wb.sheets.clone(),
            wb.defined_names.clone(),
            wb.tables.clone(),
        );
        let dry = cut_moves_refs(&mut wb, 0, &mv);
        assert_eq!(dry, expected, "{what}: dry run");
        assert!(
            wb.sheets == sheets && wb.defined_names == names && wb.tables == tables,
            "{what}: the dry run wrote"
        );
        assert_eq!(move_refs_rules_stay(&mut wb, 0, &mv), dry, "{what}");
        assert_eq!(
            wb.sheets != sheets || wb.defined_names != names || wb.tables != tables,
            dry,
            "{what}: the rewrite"
        );
    }
}

/// A1 on Sheet2 cut to A10 on Sheet1.
fn cut_sheet2_a1_to_sheet1_a10<'a>() -> CellMove<'a> {
    CellMove {
        src: "Sheet2",
        dst: "Sheet1",
        rect: (0, 0, 0, 0),
        dr: 9,
        dc: 0,
    }
}

#[test]
fn a_moved_formula_qualifies_its_source_sheets_local_names() {
    let mut wb = book(&[("Sheet1", &[]), ("Sheet2", &[]), ("My Data", &[])]);
    wb.defined_names.extend([
        local("Rate", Some(1), "Sheet2!$B$1"),
        local("Rate", Some(0), "Sheet1!$B$1"),
        local("Rate", None, "0.5"),
        local("Global", None, "2"),
        local("MyFn", Some(1), "LAMBDA(x,x*3)"),
        local("Spaced", Some(2), "7"),
    ]);
    let mv = cut_sheet2_a1_to_sheet1_a10();
    let moved = |f: &str| moved_cell_formula(&wb, (1, (0, 0)), (0, (9, 0)), f, &mv);
    // Sheet2's own name: qualified, in its definition's spelling.
    assert_eq!(moved("rate*2").as_deref(), Some("Sheet2!Rate*2"));
    // A workbook name stays bare; one already qualified stays as written.
    assert_eq!(moved("Global*2"), None);
    assert_eq!(moved("Sheet1!Rate*2"), None);
    // A LET or LAMBDA binding of the same name is the binding.
    assert_eq!(moved("LET(Rate,2,Rate*3)"), None);
    assert_eq!(
        moved("LET(x,Rate,Rate+x)").as_deref(),
        Some("LET(x,Sheet2!Rate,Sheet2!Rate+x)")
    );
    assert_eq!(moved("LAMBDA(Rate,Rate*2)(3)"), None);
    // A called local lambda; a builtin of that name is never one.
    assert_eq!(moved("MyFn(3)+1").as_deref(), Some("Sheet2!MyFn(3)+1"));
    assert_eq!(moved("LET(MyFn,LAMBDA(x,x),MyFn(3))"), None);
    assert_eq!(moved("SUM(1,2)"), None);
    // References keep being qualified as before.
    assert_eq!(moved("Rate*B1").as_deref(), Some("Sheet2!Rate*Sheet2!B1"));
    // A cut within the sheet leaves names alone.
    let same = CellMove {
        dst: "Sheet2",
        ..mv
    };
    assert_eq!(
        moved_cell_formula(&wb, (1, (0, 0)), (1, (9, 0)), "Rate*2", &same),
        None
    );
    // A sheet name that needs quotes.
    let quoted = CellMove {
        src: "My Data",
        ..mv
    };
    assert_eq!(
        moved_cell_formula(&wb, (2, (0, 0)), (0, (9, 0)), "Spaced+1", &quoted).as_deref(),
        Some("'My Data'!Spaced+1")
    );
}

#[test]
fn a_moved_formula_qualifies_bare_table_refs_leaving_its_table() {
    // Table1 on Sheet1 A1:B5 (header row 1); Table2 on Sheet1 E1:F5.
    let mut wb = book(&[("Sheet1", &[]), ("Sheet2", &[])]);
    wb.tables.push(table1(0, (0, 0, 4, 1)));
    let mut t2 = table1(0, (0, 4, 4, 5));
    t2.name = "Table2".into();
    wb.tables.push(t2);
    let cut = |dst, dr, dc| CellMove {
        src: "Sheet1",
        dst,
        rect: (1, 1, 1, 1),
        dr,
        dc,
    };
    // B2 (in Table1) to H10 on the same sheet: out of the table.
    let out = cut("Sheet1", 8, 6);
    assert_eq!(
        moved_cell_formula(&wb, (0, (1, 1)), (0, (9, 7)), "[@Qty]*2", &out).as_deref(),
        Some("Table1[@Qty]*2")
    );
    assert_eq!(
        moved_cell_formula(&wb, (0, (1, 1)), (0, (9, 7)), "SUBTOTAL(109,[Qty])", &out).as_deref(),
        Some("SUBTOTAL(109,Table1[Qty])")
    );
    // To another sheet.
    let other = cut("Sheet2", 0, 0);
    assert_eq!(
        moved_cell_formula(&wb, (0, (1, 1)), (1, (1, 1)), "[@Qty]*2", &other).as_deref(),
        Some("Table1[@Qty]*2")
    );
    // Into another table.
    let into = cut("Sheet1", 1, 3);
    assert_eq!(
        moved_cell_formula(&wb, (0, (1, 1)), (0, (2, 4)), "[@Qty]*2", &into).as_deref(),
        Some("Table1[@Qty]*2")
    );
    // Within Table1 again: still bare.
    let within = cut("Sheet1", 2, -1);
    assert_eq!(
        moved_cell_formula(&wb, (0, (1, 1)), (0, (3, 0)), "[@Qty]*2", &within),
        None
    );
    // A formula from outside any table names no table.
    let plain = CellMove {
        rect: (9, 9, 9, 9),
        ..cut("Sheet1", 1, 1)
    };
    assert_eq!(
        moved_cell_formula(&wb, (0, (9, 9)), (0, (10, 10)), "Qty+1", &plain),
        None
    );
}

// ---- a cut splits a rule its cells read differently (#1458) -----------------

/// The rectangle of A1 text `a` (`B5` or `B5:B20`).
fn area(a: &str) -> (u32, u32, u32, u32) {
    let (p, q) = a.split_once(':').unwrap_or((a, a));
    let ((r1, c1), (r2, c2)) = (parse_cell_name(p).unwrap(), parse_cell_name(q).unwrap());
    (r1, c1, r2, c2)
}

/// A rule `kind` over `sqref` held by the part's element `ix`, with a format
/// and a priority of its own to check the split copies keep.
fn held(sqref: &str, kind: crate::sheet::CfKind, ix: usize) -> crate::sheet::CondFormat {
    crate::sheet::CondFormat {
        ranges: sqref.split(' ').map(area).collect(),
        rules: vec![crate::sheet::CfRule {
            kind,
            dxf_id: Some(3),
            priority: 7,
        }],
        ix: Some(ix),
        copy_of: None,
    }
}

/// Each block on sheet `s`: (sqref, its first formula, `ix`, `copy_of`).
type Block = (String, String, Option<usize>, Option<usize>);

fn blocks(wb: &Workbook, s: usize) -> Vec<Block> {
    wb.sheets[s]
        .cond_formats
        .iter()
        .map(|cf| {
            let f = cf.rules[0]
                .formulas()
                .first()
                .map_or(String::new(), |f| (*f).clone());
            (crate::xlsx::sqref_of(&cf.ranges), f, cf.ix, cf.copy_of)
        })
        .collect()
}

fn block(sqref: &str, formula: &str, ix: Option<usize>, copy_of: Option<usize>) -> Block {
    (sqref.into(), formula.into(), ix, copy_of)
}

/// A5:A10 cut to C5 on Sheet1.
fn cut_a5_a10_to_c5<'a>() -> CellMove<'a> {
    CellMove {
        src: "Sheet1",
        dst: "Sheet1",
        rect: (4, 0, 9, 0),
        dr: 0,
        dc: 2,
    }
}

/// `move_refs_rules_stay` on a one-sheet book holding `rules`, after its dry
/// run said the same and wrote nothing; the book and what both said.
fn cut_with_rules(rules: Vec<crate::sheet::CondFormat>, mv: &CellMove) -> (Workbook, bool) {
    let mut wb = book(&[("Sheet1", &[])]);
    wb.sheets[0].cond_formats = rules;
    let before = wb.sheets.clone();
    let dry = cut_moves_refs(&mut wb, 0, mv);
    assert!(wb.sheets == before, "the dry run wrote");
    let wrote = move_refs_rules_stay(&mut wb, 0, mv);
    assert_eq!(dry, wrote, "the dry run and the rewrite disagree");
    (wb, wrote)
}

#[test]
fn a_cut_splits_a_rule_whose_cells_read_it_differently() {
    let mv = cut_a5_a10_to_c5();
    // The anchor reads the cut: only B5:B10 follow it, B11:B20 keep A11:A20.
    let (wb, wrote) = cut_with_rules(vec![held("B5:B20", expr("A5>0"), 0)], &mv);
    assert!(wrote);
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("B5:B10", "C5>0", Some(0), None),
            block("B11:B20", "A11>0", None, Some(0)),
        ]
    );
    // Every part keeps the rule's format, priority and kind.
    for cf in &wb.sheets[0].cond_formats {
        assert_eq!((cf.rules[0].dxf_id, cf.rules[0].priority), (Some(3), 7));
    }
    // A cellIs rule keeps its operator.
    let cell_is = crate::sheet::CfKind::CellIs {
        op: "between".into(),
        formulas: vec!["A5".into(), "$D$1".into()],
    };
    let (wb, _) = cut_with_rules(vec![held("B5:B20", cell_is, 0)], &mv);
    let kinds: Vec<_> = wb.sheets[0]
        .cond_formats
        .iter()
        .map(|cf| cf.rules[0].kind.clone())
        .collect();
    let between = |a: &str| crate::sheet::CfKind::CellIs {
        op: "between".into(),
        formulas: vec![a.into(), "$D$1".into()],
    };
    assert_eq!(kinds, vec![between("C5"), between("A11")]);

    // The anchor doesn't read the cut (main left the rule alone and said
    // nothing changed): B5:B10 split off and follow it.
    let (wb, wrote) = cut_with_rules(vec![held("B2:B20", expr("A2>0"), 0)], &mv);
    assert!(wrote, "cut_moves_refs sees the split");
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("B2:B4 B11:B20", "A2>0", Some(0), None),
            block("B5:B10", "C5>0", None, Some(0)),
        ]
    );

    // #1687's r2 rule beside the cut: only H6 read F1, so only it follows.
    // The cells reading nothing in the cut can't be one block: its corner
    // would be H6, which reads F1. They are two, each cornered on a cell of
    // its own.
    let (wb, _) = cut_with_rules(vec![held("H6:J10", expr("F1>0"), 0)], &cut_a1_f1_to_a20());
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("H6", "F20>0", Some(0), None),
            block("I6:J10", "G1>0", None, Some(0)),
            block("H7:H10", "F2>0", None, Some(0)),
        ]
    );
}

#[test]
fn a_rule_reading_nothing_in_the_cut_is_not_split() {
    // Another column on the cut's rows: nothing is read in the cut.
    let mv_e5 = CellMove {
        dc: 4,
        ..cut_a5_a10_to_c5()
    };
    let (wb, wrote) = cut_with_rules(vec![held("D2:D20", expr("$D2>0"), 0)], &mv_e5);
    assert!(!wrote);
    assert_eq!(
        blocks(&wb, 0),
        vec![block("D2:D20", "$D2>0", Some(0), None)]
    );
    // A range only partly in the cut never follows it.
    let mv = cut_a5_a10_to_c5();
    let (wb, wrote) = cut_with_rules(vec![held("B1:B20", expr("SUM(A1:A10)>0"), 0)], &mv);
    assert!(!wrote);
    assert_eq!(
        blocks(&wb, 0),
        vec![block("B1:B20", "SUM(A1:A10)>0", Some(0), None)]
    );
    // Two columns, only B reading A: two blocks, not one per line.
    let (wb, _) = cut_with_rules(vec![held("B2:C20", expr("A2>0"), 0)], &mv);
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("B2:B4 C2:C20 B11:B20", "A2>0", Some(0), None),
            block("B5:B10", "C5>0", None, Some(0)),
        ]
    );
    // A pasted where it was moves nothing.
    let still = CellMove {
        dc: 0,
        ..cut_a5_a10_to_c5()
    };
    let (wb, wrote) = cut_with_rules(vec![held("B2:B20", expr("A2>0"), 0)], &still);
    assert!(!wrote);
    assert_eq!(blocks(&wb, 0), vec![block("B2:B20", "A2>0", Some(0), None)]);
}

#[test]
fn a_rule_left_whole_when_its_cells_read_the_cut_alike() {
    let mv = cut_a5_a10_to_c5();
    // Every cell reads the cut: rewritten in place, one block.
    let (wb, wrote) = cut_with_rules(vec![held("B5:B10", expr("A5>0"), 0)], &mv);
    assert!(wrote);
    assert_eq!(blocks(&wb, 0), vec![block("B5:B10", "C5>0", Some(0), None)]);
    // None does: untouched.
    let (wb, wrote) = cut_with_rules(vec![held("B5:B20", expr("H1>0"), 0)], &mv);
    assert!(!wrote);
    assert_eq!(blocks(&wb, 0), vec![block("B5:B20", "H1>0", Some(0), None)]);
    // Judged over the whole range (a color scale with a relative formula
    // threshold), or mixed with such a rule: re-pointed whole, not split.
    let scale = crate::sheet::CfKind::Other {
        rule_type: "colorScale".into(),
        formulas: Vec::new(),
        cfvos: vec![crate::sheet::Cfvo {
            kind: "formula".into(),
            val: "A5".into(),
            gte: true,
        }],
    };
    let (wb, wrote) = cut_with_rules(vec![held("B5:B20", scale.clone(), 0)], &mv);
    assert!(wrote);
    assert_eq!(wb.sheets[0].cond_formats.len(), 1);
    assert_eq!(wb.sheets[0].cond_formats[0].rules[0].cfvos()[0].val, "C5");
    let mut mixed = held("B5:B20", expr("A5>0"), 0);
    mixed.rules.push(crate::sheet::CfRule {
        kind: scale,
        dxf_id: None,
        priority: 8,
    });
    let (wb, _) = cut_with_rules(vec![mixed], &mv);
    assert_eq!(blocks(&wb, 0), vec![block("B5:B20", "C5>0", Some(0), None)]);
}

/// The side effect: after the cut each cell of the rule is formatted by
/// the cell it read, moved or not.
#[test]
fn a_split_rule_formats_each_cell_by_what_it_read() {
    let red = crate::sheet::Dxf {
        fill: Some((255, 0, 0)),
        ..Default::default()
    };
    let mut rule = held("B5:B20", expr("A5>0"), 0);
    rule.rules[0].dxf_id = Some(0);
    let (mut wb, _) = cut_with_rules(vec![rule], &cut_a5_a10_to_c5());
    wb.styles.dxfs = vec![red.clone()];
    // The cells as the cut leaves them: A5 now at C5; A11 stays.
    let s = &mut wb.sheets[0];
    s.set_cell(4, 2, Cell::number(1.0)); // C5
    s.set_cell(10, 0, Cell::number(1.0)); // A11
    s.set_cell(10, 2, Cell::number(-1.0)); // C11
    assert_eq!(
        crate::cf::cell_dxf(&wb, 0, 4, 1),
        Some(red.clone()),
        "B5 by C5"
    );
    assert_eq!(crate::cf::cell_dxf(&wb, 0, 10, 1), Some(red), "B11 by A11");
    assert_eq!(
        crate::cf::cell_dxf(&wb, 0, 5, 1),
        None,
        "B6 by the empty C6"
    );
}

#[test]
fn a_cut_on_another_sheet_splits_a_rule_reading_it() {
    let mut wb = book(&[("Sheet1", &[]), ("Sheet2", &[]), ("Sheet3", &[])]);
    wb.sheets[1]
        .cond_formats
        .push(held("B5:B20", expr("Sheet1!A5>0"), 0));
    let mv = CellMove {
        src: "Sheet1",
        dst: "Sheet3",
        rect: (4, 0, 9, 0),
        dr: 0,
        dc: 0,
    };
    assert!(cut_moves_refs(&mut wb, 0, &mv));
    assert!(move_refs_rules_stay(&mut wb, 0, &mv));
    assert_eq!(
        blocks(&wb, 1),
        vec![
            block("B5:B10", "Sheet3!A5>0", Some(0), None),
            block("B11:B20", "Sheet1!A11>0", None, Some(0)),
        ]
    );
}

#[test]
fn a_cut_splits_a_rule_over_it_where_cells_beside_read_into_it() {
    let mv = cut_a5_a10_to_c5();
    // The item's example: A11 read A10, which moved to C10. The cut cells
    // keep the rule as #1687 left it; the rest read nothing in the cut.
    let (wb, wrote) = cut_with_rules(vec![held("A2:A20", expr("A2>A1"), 0)], &mv);
    assert!(wrote);
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("A2:A4 A12:A20", "A2>A1", Some(0), None),
            block("A5:A10", "A5>A4", None, Some(0)),
            block("A11", "A11>C10", None, Some(0)),
        ]
    );
    // #880: a fully absolute reference into the rule's own cells stays,
    // inside the cut and beside it alike: A1:A4 cut to C1.
    let a1_a4 = CellMove {
        rect: (0, 0, 3, 0),
        ..mv
    };
    let (wb, wrote) = cut_with_rules(vec![held("A1:A10", expr("A1>$A$1"), 0)], &a1_a4);
    assert!(!wrote);
    assert_eq!(
        blocks(&wb, 0),
        vec![block("A1:A10", "A1>$A$1", Some(0), None)]
    );
    let a2_a4 = CellMove {
        rect: (1, 0, 3, 0),
        ..mv
    };
    let (wb, _) = cut_with_rules(vec![held("A2:A10", expr("A2>$A$2+A1"), 0)], &a2_a4);
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("A2:A4", "A2>$A$2+A1", Some(0), None),
            block("A5", "A5>$A$2+C4", None, Some(0)),
            block("A6:A10", "A6>$A$2+A5", None, Some(0)),
        ]
    );
    // A part beside the cut whose corner lies in it: C6:C11 and B11 read
    // $B5:$B10 (moved to E), and their block's corner B6 is a cut cell.
    let b5_b10 = CellMove {
        rect: (4, 1, 9, 1),
        dc: 3,
        ..mv
    };
    let (wb, _) = cut_with_rules(vec![held("B2:C20", expr("$B1>0"), 0)], &b5_b10);
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("B2:B4 C2:C5 B12:C20", "$B1>0", Some(0), None),
            block("B5:B10", "$B4>0", None, Some(0)),
            block("C6:C11 B11", "$E5>0", None, Some(0)),
        ]
    );
}

#[test]
fn a_split_copy_splits_again_and_keeps_naming_its_element() {
    let (mut wb, _) = cut_with_rules(vec![held("B2:B20", expr("A2>0"), 4)], &cut_a5_a10_to_c5());
    // Then C7:C8 (A7:A8 before) cut to E7: the copy over B5:B10 splits.
    let c7_c8 = CellMove {
        src: "Sheet1",
        dst: "Sheet1",
        rect: (6, 2, 7, 2),
        dr: 0,
        dc: 2,
    };
    assert!(move_refs_rules_stay(&mut wb, 0, &c7_c8));
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("B2:B4 B11:B20", "A2>0", Some(4), None),
            block("B5:B6 B9:B10", "C5>0", None, Some(4)),
            block("B7:B8", "E7>0", None, Some(4)),
        ]
    );
}

#[test]
fn a_data_form_delete_splits_a_rule_beside_the_list() {
    let cells: Vec<(String, Cell)> = (1..=10)
        .map(|r| (format!("A{r}"), Cell::number(f64::from(r))))
        .collect();
    let cells: Vec<(&str, Cell)> = cells.iter().map(|(a, c)| (a.as_str(), c.clone())).collect();
    let mut wb = book(&[("Sheet1", &cells)]);
    wb.sheets[0]
        .cond_formats
        .push(held("B2:B10", expr("A2>0"), 0));
    // The record in row 5 of the list A1:A10 goes; A6:A10 move up.
    crate::edit::delete_record(&mut wb, 0, (0, 0, 9, 0), 4);
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("B2:B4", "A2>0", Some(0), None),
            block("B6:B10", "A5>0", None, Some(0)),
            block("B5", "#REF!>0", None, Some(0)),
        ]
    );
}

/// A rule's anchor is the top-left of its ranges, which may be no cell of
/// it: when that corner reads the cut and none of its cells does, the rule
/// is split so that no block is rewritten for a cell it doesn't hold.
#[test]
fn a_rule_whose_corner_alone_reads_the_cut_keeps_what_its_cells_read() {
    // C1:C5 B6:B10 is anchored on B1, which reads A1; C1 reads B1 and B6
    // reads A6, neither cut.
    let a1_to_e1 = CellMove {
        src: "Sheet1",
        dst: "Sheet1",
        rect: (0, 0, 0, 0),
        dr: 0,
        dc: 4,
    };
    let mut rule = held("C1:C5 B6:B10", expr("A1>0"), 0);
    rule.rules[0].dxf_id = Some(0);
    let (mut wb, _) = cut_with_rules(vec![rule], &a1_to_e1);
    assert_eq!(
        blocks(&wb, 0),
        vec![
            block("C1:C5", "B1>0", Some(0), None),
            block("B6:B10", "A6>0", None, Some(0)),
        ]
    );
    // Each cell is still formatted by the cell it read.
    let red = crate::sheet::Dxf {
        fill: Some((255, 0, 0)),
        ..Default::default()
    };
    wb.styles.dxfs = vec![red.clone()];
    wb.sheets[0].set_cell(0, 1, Cell::number(1.0)); // B1
    wb.sheets[0].set_cell(5, 0, Cell::number(1.0)); // A6
    wb.sheets[0].set_cell(0, 4, Cell::number(-1.0)); // E1, where A1 went
    assert_eq!(
        crate::cf::cell_dxf(&wb, 0, 0, 2),
        Some(red.clone()),
        "C1 by B1"
    );
    assert_eq!(crate::cf::cell_dxf(&wb, 0, 5, 1), Some(red), "B6 by A6");
}
