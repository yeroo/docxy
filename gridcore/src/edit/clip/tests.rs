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
    // Relative and mixed refs stay; a fully absolute spill ref follows.
    assert_eq!(rule_formula(&wb, 0, 2), "F1+$F1+F$1+$F$20#");
    assert_eq!(rule_formula(&wb, 0, 3), "F20>0");

    // Both corners in the cut: the absolute range follows.
    let mut wb = book(&[("Sheet1", &[])]);
    wb.sheets[0]
        .cond_formats
        .push(cf(&[(0, 0, 9, 3)], expr("SUM($E$1:$F$1)>0")));
    assert!(move_refs_rules_stay(&mut wb, 0, &cut_a1_f1_to_a20()));
    assert_eq!(rule_formula(&wb, 0, 0), "SUM($E$20:$F$20)>0");

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
