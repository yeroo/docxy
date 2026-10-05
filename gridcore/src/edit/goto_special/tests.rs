use super::*;
use crate::engine::Engine;
use crate::entry::entry_range_ctx;
use crate::sheet::{CondFormat, DataValidation, parse_cell_name};

fn book(cells: &[(&str, Cell)]) -> Workbook {
    let mut sheet = Sheet {
        name: "Sheet1".to_string(),
        ..Sheet::default()
    };
    for (name, cell) in cells {
        let (r, c) = parse_cell_name(name).unwrap();
        sheet.set_cell(r, c, cell.clone());
    }
    Workbook {
        sheets: vec![sheet],
        ..Workbook::default()
    }
}

fn rect(s: &str) -> Rect {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let (r0, c0) = parse_cell_name(a).unwrap();
    let (r1, c1) = parse_cell_name(b).unwrap();
    (r0, c0, r1, c1)
}

fn rects(v: &[&str]) -> Vec<Rect> {
    v.iter().map(|s| rect(s)).collect()
}

fn go(
    wb: &Workbook,
    sel: &[&str],
    active: &str,
    kind: GoSpecial,
) -> Result<Vec<Rect>, &'static str> {
    go_to_special(
        wb,
        0,
        &rects(sel),
        parse_cell_name(active).unwrap(),
        kind,
        &[],
    )
}

#[test]
fn blanks_in_the_selection() {
    let mut cells = Vec::new();
    for r in [1, 5, 10] {
        cells.push((format!("A{r}"), Cell::text(&format!("g{r}"))));
    }
    let cells: Vec<(&str, Cell)> = cells.iter().map(|(n, c)| (n.as_str(), c.clone())).collect();
    let wb = book(&cells);
    assert_eq!(
        go(&wb, &["A1:A10"], "A1", GoSpecial::Blanks),
        Ok(rects(&["A2:A4", "A6:A9"]))
    );
    // One cell selected: the used range.
    assert_eq!(
        go(&wb, &["C3"], "C3", GoSpecial::Blanks),
        Ok(rects(&["A2:A4", "A6:A9"]))
    );
    assert_eq!(
        go(&wb, &["A1"], "A1", GoSpecial::Constants(Types::ALL)),
        Ok(rects(&["A1:A1", "A5:A5", "A10:A10"]))
    );
}

/// #671's QA case: Go To Special › Blanks, then `=A1` and Ctrl+Enter, fills
/// a report's grouping column from above. The active cell is the first
/// blank (R6), A2, and the entry is translated from it to every blank.
#[test]
fn blanks_then_ctrl_enter_fills_a_grouping_column() {
    let mut wb = book(&[
        ("A1", Cell::text("East")),
        ("A5", Cell::text("West")),
        ("A10", Cell::text("North")),
        ("B10", Cell::number(1.0)),
    ]);
    let found = go(&wb, &["A1:A10"], "A1", GoSpecial::Blanks).unwrap();
    let active = (found[0].0, found[0].1);
    assert_eq!(active, (1, 0), "A2");
    let mut eng = Engine::new(&wb);
    for &area in &found {
        let cells = entry_range_ctx(&mut wb, 0, area, active, "=A1", &Default::default()).unwrap();
        eng.set_cells_prechecked(&mut wb, 0, cells);
    }
    let f = |r: u32| wb.sheets[0].cell(r, 0).and_then(|c| c.formula.clone());
    assert_eq!(
        (1..=3).map(f).collect::<Vec<_>>(),
        ["A1", "A2", "A3"].map(|s| Some(s.to_string()))
    );
    assert_eq!(
        (5..=8).map(f).collect::<Vec<_>>(),
        ["A5", "A6", "A7", "A8"].map(|s| Some(s.to_string()))
    );
}

#[test]
fn constants_and_formulas_by_type() {
    let mut wb = book(&[
        ("A1", Cell::number(1.0)),
        ("A2", Cell::text("t")),
        (
            "A3",
            Cell {
                value: CellValue::Bool(true),
                ..Cell::default()
            },
        ),
    ]);
    let mut f = Cell::formula("A1");
    f.value = CellValue::Number(1.0);
    wb.sheets[0].set_cell(0, 1, f);
    let mut g = Cell::formula("A2");
    g.value = CellValue::Text("t".into());
    wb.sheets[0].set_cell(1, 1, g);
    let numbers = Types {
        numbers: true,
        text: false,
        logicals: false,
        errors: false,
    };
    assert_eq!(
        go(&wb, &["A1"], "A1", GoSpecial::Constants(numbers)),
        Ok(rects(&["A1"]))
    );
    assert_eq!(
        go(&wb, &["A1"], "A1", GoSpecial::Formulas(numbers)),
        Ok(rects(&["B1"]))
    );
    assert_eq!(
        go(&wb, &["A1"], "A1", GoSpecial::Formulas(Types::ALL)),
        Ok(rects(&["B1:B2"]))
    );
    let logicals = Types {
        logicals: true,
        numbers: false,
        text: false,
        errors: false,
    };
    assert_eq!(
        go(&wb, &["A1"], "A1", GoSpecial::Constants(logicals)),
        Ok(rects(&["A3"]))
    );
    assert_eq!(
        go(&wb, &["A1"], "A1", GoSpecial::Formulas(logicals)),
        Err(NO_CELLS)
    );
}

#[test]
fn notes_region_last_cell_and_array() {
    let mut wb = book(&[
        ("B2", Cell::number(1.0)),
        ("C3", Cell::number(1.0)),
        ("E9", Cell::number(1.0)),
    ]);
    assert_eq!(
        go_to_special(
            &wb,
            0,
            &rects(&["A1"]),
            (0, 0),
            GoSpecial::Notes,
            &[(1, 1), (40, 40)]
        ),
        Ok(rects(&["B2"]))
    );
    assert_eq!(
        go(&wb, &["B2"], "B2", GoSpecial::CurrentRegion),
        Ok(rects(&["B2:C3"]))
    );
    assert_eq!(
        go(&wb, &["B2"], "B2", GoSpecial::LastCell),
        Ok(rects(&["E9"]))
    );
    assert_eq!(
        go(&wb, &["B2"], "B2", GoSpecial::CurrentArray),
        Err(NO_CELLS)
    );
    let mut eng = Engine::new(&wb);
    eng.set_cell(&mut wb, (0, 0, 6), Cell::formula("SEQUENCE(3)"));
    assert_eq!(
        go(&wb, &["G2"], "G2", GoSpecial::CurrentArray),
        Ok(rects(&["G1:G3"]))
    );
}

#[test]
fn row_and_column_differences() {
    let wb = book(&[
        ("A1", Cell::number(1.0)),
        ("B1", Cell::number(1.0)),
        ("C1", Cell::number(2.0)),
        ("A2", Cell::number(5.0)),
        ("B2", Cell::number(6.0)),
        ("C2", Cell::number(5.0)),
    ]);
    assert_eq!(
        go(&wb, &["A1:C2"], "A1", GoSpecial::RowDifferences),
        Ok(rects(&["C1", "B2"]))
    );
    assert_eq!(
        go(&wb, &["A1:C2"], "A1", GoSpecial::ColumnDifferences),
        Ok(rects(&["A2:C2"]))
    );
    // A filled-right formula reads the same as its comparison cell.
    let mut wb = book(&[]);
    wb.sheets[0].set_cell(0, 0, Cell::formula("A5"));
    wb.sheets[0].set_cell(0, 1, Cell::formula("B5"));
    wb.sheets[0].set_cell(0, 2, Cell::formula("A5"));
    assert_eq!(
        go(&wb, &["A1:C1"], "A1", GoSpecial::RowDifferences),
        Ok(rects(&["C1"]))
    );
}

#[test]
fn precedents_and_dependents_direct_and_all_levels() {
    let mut wb = book(&[("A1", Cell::number(1.0))]);
    wb.sheets[0].set_cell(0, 1, Cell::formula("A1*2"));
    wb.sheets[0].set_cell(0, 2, Cell::formula("B1+1"));
    wb.sheets[0].set_cell(0, 3, Cell::formula("Other!A1+C1"));
    let p = |all| GoSpecial::Precedents { all };
    let d = |all| GoSpecial::Dependents { all };
    assert_eq!(go(&wb, &["C1"], "C1", p(false)), Ok(rects(&["B1"])));
    assert_eq!(go(&wb, &["C1"], "C1", p(true)), Ok(rects(&["A1:B1"])));
    assert_eq!(
        go(&wb, &["D1"], "D1", p(false)),
        Ok(rects(&["C1"])),
        "other sheets left out"
    );
    assert_eq!(go(&wb, &["A1"], "A1", d(false)), Ok(rects(&["B1"])));
    assert_eq!(go(&wb, &["A1"], "A1", d(true)), Ok(rects(&["B1:D1"])));
    assert_eq!(go(&wb, &["D1"], "D1", d(false)), Err(NO_CELLS));
}

#[test]
fn visible_cells_skip_hidden_rows() {
    let mut wb = book(&[("A1", Cell::number(1.0)), ("B4", Cell::number(1.0))]);
    wb.sheets[0]
        .row_attrs
        .insert(1, " hidden=\"1\"".to_string());
    assert_eq!(
        go(&wb, &["A1:B4"], "A1", GoSpecial::VisibleCells),
        Ok(rects(&["A1:B1", "A3:B4"]))
    );
}

#[test]
fn conditional_formats_and_validation_all_or_same() {
    let mut wb = book(&[("A1", Cell::number(1.0)), ("H20", Cell::number(1.0))]);
    wb.sheets[0].cond_formats.push(CondFormat {
        ranges: vec![rect("A1:A3")],
        rules: Vec::new(),
        ix: None,
    });
    wb.sheets[0].cond_formats.push(CondFormat {
        ranges: vec![rect("C1:C2")],
        rules: Vec::new(),
        ix: None,
    });
    let cf = |same| GoSpecial::ConditionalFormats { same };
    assert_eq!(
        go(&wb, &["A1"], "A1", cf(false)),
        Ok(rects(&["A1:A3", "C1:C2"]))
    );
    assert_eq!(go(&wb, &["A1"], "A1", cf(true)), Ok(rects(&["A1:A3"])));
    wb.sheets[0].validations.push(DataValidation {
        ranges: vec![rect("B5:B6")],
        kind: "whole".into(),
        operator: String::new(),
        formula1: "1".into(),
        formula2: String::new(),
        prompt: None,
        ix: None,
    });
    let dv = |same| GoSpecial::DataValidation { same };
    assert_eq!(go(&wb, &["A1"], "A1", dv(false)), Ok(rects(&["B5:B6"])));
    assert_eq!(go(&wb, &["A1"], "A1", dv(true)), Err(NO_CELLS));
}

#[test]
fn names_parse() {
    assert_eq!(
        GoSpecial::from_name("blanks", Types::ALL, false, false),
        Some(GoSpecial::Blanks)
    );
    assert_eq!(
        GoSpecial::from_name("Precedents", Types::ALL, true, false),
        Some(GoSpecial::Precedents { all: true })
    );
    assert_eq!(
        GoSpecial::from_name("objects", Types::ALL, false, false),
        None
    );
}
