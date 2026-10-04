use super::*;
use crate::formula::{CellMove, move_block_formula};
use crate::sheet::{Cell, DefinedName, Sheet, parse_cell_name};

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
    let out = translated_block(&cells, &[0, 1], 1, (0, 4));
    assert_eq!(out[0][0].formula.as_deref(), Some("D1*10"));
    assert_eq!(out[1][0].formula.as_deref(), Some("$A2+D$1+$B$1"));
    // Pushed off the grid: #REF!.
    let out = translated_block(&[vec![f("A1")]], &[1], 0, (0, 0));
    assert_eq!(out[0][0].formula.as_deref(), Some("#REF!"));
}

#[test]
fn a_copy_pasted_where_it_came_from_keeps_its_text_exactly() {
    // Reprinting would drop the spaces and the lower case.
    let cells = vec![vec![f("sum( a1 , 2 )")], vec![f("not a formula ((")]];
    let out = translated_block(&cells, &[0, 1], 1, (0, 1));
    assert_eq!(out[0][0].formula.as_deref(), Some("sum( a1 , 2 )"));
    assert_eq!(out[1][0].formula.as_deref(), Some("not a formula (("));
    // Moved, an unparseable formula keeps its text too.
    let out = translated_block(&cells, &[0, 1], 1, (5, 5));
    assert_eq!(out[1][0].formula.as_deref(), Some("not a formula (("));
}

#[test]
fn each_row_of_a_filtered_copy_translates_by_its_own_offset() {
    // Rows 1, 2, 4, 6 of a filter (3 and 5 hidden), pasted at D1.
    let cells = vec![vec![f("A1")], vec![f("A2")], vec![f("A4")], vec![f("A6")]];
    let out = translated_block(&cells, &[0, 1, 3, 5], 1, (0, 3));
    let got: Vec<_> = out.iter().map(|r| r[0].formula.clone().unwrap()).collect();
    // Row 4 lands on row 3: one row up, two columns across.
    assert_eq!(got, ["C1", "C2", "C3", "C4"]);
}

#[test]
fn tiles_each_translate_to_their_own_corner() {
    let cells = vec![vec![f("A1*10")], vec![f("A2*10")]];
    let block = tiled_block(&cells, &[0, 1], 1, (0, 3), (3, 1));
    let got: Vec<_> = block
        .iter()
        .map(|r| r[0].formula.clone().unwrap())
        .collect();
    assert_eq!(got, ["C1*10", "C2*10", "C3*10", "C4*10", "C5*10", "C6*10"]);
    // Across, with a short row padded so the next tile keeps its column.
    let cells = vec![vec![f("A1"), f("B1")], vec![f("A2")]];
    let block = tiled_block(&cells, &[0, 1], 0, (0, 0), (1, 2));
    assert_eq!(block[0].len(), 4);
    assert_eq!(block[1].len(), 4);
    assert_eq!(block[0][2].formula.as_deref(), Some("C1"));
    assert_eq!(block[1][2].formula.as_deref(), Some("C2"));
    assert_eq!(block[1][1].formula, None);
}

#[test]
fn a_tiled_block_stops_at_the_grids_edge() {
    let cells = vec![vec![Cell::number(1.0)], vec![Cell::number(2.0)]];
    let block = tiled_block(&cells, &[0, 1], 0, (MAX_ROWS - 3, 0), (2, 1));
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
