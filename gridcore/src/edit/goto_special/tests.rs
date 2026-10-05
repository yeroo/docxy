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

fn rect(s: &str) -> Area {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let (r0, c0) = parse_cell_name(a).unwrap();
    let (r1, c1) = parse_cell_name(b).unwrap();
    (r0, c0, r1, c1)
}

fn rects(v: &[&str]) -> Vec<Area> {
    v.iter().map(|s| rect(s)).collect()
}

fn go(
    wb: &Workbook,
    sel: &[&str],
    active: &str,
    kind: GoSpecial,
) -> Result<Vec<Area>, &'static str> {
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

#[test]
fn go_to_resolves_cells_ranges_sheets_and_names() {
    let mut wb = book(&[]);
    wb.sheets.push(Sheet {
        name: "Q 2".into(),
        ..Sheet::default()
    });
    wb.defined_names.push(crate::sheet::DefinedName {
        name: "Totals".into(),
        scope: None,
        formula: "'Q 2'!$B$2:$C$4".into(),
    });
    assert_eq!(resolve_reference(&wb, 0, "b3"), Some((0, rect("B3"))));
    assert_eq!(resolve_reference(&wb, 0, "A1:C2"), Some((0, rect("A1:C2"))));
    assert_eq!(resolve_reference(&wb, 0, "'Q 2'!D4"), Some((1, rect("D4"))));
    assert_eq!(
        resolve_reference(&wb, 0, "totals"),
        Some((1, rect("B2:C4")))
    );
    assert_eq!(resolve_reference(&wb, 0, "Nowhere!A1"), None);
    assert_eq!(resolve_reference(&wb, 0, "banana"), None);
}

/// #707 r3 M3 and r4 M2: whole-sheet scopes stay fast, and give Excel's
/// results: the differences within the used range, Visible cells over the
/// selection itself, a rule's or a precedent's whole range.
#[test]
fn whole_sheet_scopes_are_fast_and_keep_excels_results() {
    use crate::sheet::{MAX_COLS, MAX_ROWS};
    let wb = book(&[
        ("A1", Cell::number(1.0)),
        ("B1", Cell::number(1.0)),
        ("C1", Cell::number(2.0)),
        ("A2", Cell::number(5.0)),
        ("B2", Cell::number(6.0)),
        ("C2", Cell::number(5.0)),
    ]);
    let all = (0, 0, MAX_ROWS - 1, MAX_COLS - 1);
    let t = std::time::Instant::now();
    let found = go_to_special(&wb, 0, &[all], (0, 0), GoSpecial::RowDifferences, &[]);
    assert_eq!(found, Ok(rects(&["C1", "B2"])));
    let found = go_to_special(&wb, 0, &[all], (0, 0), GoSpecial::VisibleCells, &[]);
    assert_eq!(found, Ok(vec![all]), "the selection, hidden nothing");
    // A whole-column rule, Same, from a cell outside the data.
    let mut wb = wb;
    wb.sheets[0].cond_formats.push(CondFormat {
        ranges: vec![(0, 0, MAX_ROWS - 1, 1)],
        rules: Vec::new(),
        ix: None,
    });
    let found = go_to_special(
        &wb,
        0,
        &[rect("A1")],
        (0, 0),
        GoSpecial::ConditionalFormats { same: true },
        &[],
    );
    assert_eq!(found, Ok(vec![(0, 0, MAX_ROWS - 1, 1)]));
    wb.sheets[0].set_cell(0, 3, Cell::formula("SUM(A:A)"));
    let found = go_to_special(
        &wb,
        0,
        &[rect("D1")],
        (0, 3),
        GoSpecial::Precedents { all: true },
        &[],
    );
    assert_eq!(found, Ok(vec![(0, 0, MAX_ROWS - 1, 0)]));
    assert!(
        t.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        t.elapsed()
    );
}

/// #707 r4 M2: the planner's three cases.
#[test]
fn visible_cells_rules_and_precedents_reach_past_the_data() {
    // Visible cells over A1:D20 with row 5 hidden.
    let mut wb = book(&[("A1", Cell::number(1.0))]);
    wb.sheets[0]
        .row_attrs
        .insert(4, " hidden=\"1\"".to_string());
    assert_eq!(
        go(&wb, &["A1:D20"], "A1", GoSpecial::VisibleCells),
        Ok(rects(&["A1:D4", "A6:D20"]))
    );
    // Validation on B2:B100 of an empty form, Same.
    let mut wb = book(&[]);
    wb.sheets[0].validations.push(DataValidation {
        ranges: vec![rect("B2:B100")],
        kind: "whole".into(),
        operator: String::new(),
        formula1: "1".into(),
        formula2: String::new(),
        prompt: None,
        ix: None,
    });
    assert_eq!(
        go(&wb, &["B2"], "B2", GoSpecial::DataValidation { same: true }),
        Ok(rects(&["B2:B100"]))
    );
    // =SUM(A1:A10) with A1:A3 filled.
    let mut wb = book(&[
        ("A1", Cell::number(1.0)),
        ("A2", Cell::number(2.0)),
        ("A3", Cell::number(3.0)),
    ]);
    wb.sheets[0].set_cell(0, 2, Cell::formula("SUM(A1:A10)"));
    assert_eq!(
        go(&wb, &["C1"], "C1", GoSpecial::Precedents { all: false }),
        Ok(rects(&["A1:A10"]))
    );
}

/// #707 r4 M3: the current region of a long column is found fast.
#[test]
fn the_current_region_of_a_long_column_is_fast() {
    let mut wb = book(&[]);
    for r in 0..50_000 {
        wb.sheets[0].set_cell(r, 0, Cell::number(f64::from(r)));
    }
    let t = std::time::Instant::now();
    assert_eq!(
        go(&wb, &["A1"], "A1", GoSpecial::CurrentRegion),
        Ok(vec![(0, 0, 49_999, 0)])
    );
    assert!(
        t.elapsed() < std::time::Duration::from_secs(1),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn union_rects_merges_without_walking_cells() {
    use crate::sheet::MAX_ROWS;
    assert_eq!(
        union_rects(&[(0, 0, 9, 0), (5, 0, MAX_ROWS - 1, 0), (0, 2, 0, 2)]),
        vec![(0, 0, MAX_ROWS - 1, 0), (0, 2, 0, 2)]
    );
    assert_eq!(
        union_rects(&[(0, 0, 1, 1), (0, 0, 1, 1)]),
        vec![(0, 0, 1, 1)]
    );
    assert!(union_rects(&[]).is_empty());
}

// ---- #707 r5: worst-case costs, on realistic large sheets ----------------------

fn fast(t: std::time::Instant, what: &str) {
    assert!(
        t.elapsed() < std::time::Duration::from_secs(3),
        "{what}: {:?}",
        t.elapsed()
    );
}

/// M1: a running total's 50,000 nested ranges merge without walking them.
#[test]
fn precedents_of_a_50k_running_total_are_fast() {
    let mut wb = book(&[]);
    for r in 1..=50_000u32 {
        wb.sheets[0].set_cell(r, 0, Cell::number(1.0));
        wb.sheets[0].set_cell(r, 1, Cell::formula(&format!("SUM($A$2:A{})", r + 1)));
    }
    let t = std::time::Instant::now();
    let found = go_to_special(
        &wb,
        0,
        &[rect("B2:B50001")],
        (1, 1),
        GoSpecial::Precedents { all: true },
        &[],
    );
    fast(t, "precedents");
    assert_eq!(found, Ok(rects(&["A2:A50001"])));
}

/// M1: 5,000 nested 2-D rectangles, and 5,000 staircase ones, merge fast.
#[test]
fn union_of_thousands_of_nested_and_staggered_rects_is_fast() {
    let nested: Vec<Area> = (0..5_000).map(|i| (0, 0, i, i)).collect();
    let t = std::time::Instant::now();
    assert_eq!(union_rects(&nested), vec![(0, 0, 4_999, 4_999)]);
    fast(t, "nested");
    let stairs: Vec<Area> = (0..5_000).map(|i| (i, i, i + 2, i + 2)).collect();
    let t = std::time::Instant::now();
    let u = union_rects(&stairs);
    fast(t, "stairs");
    // Every cell of every input is covered, and nothing else.
    let covered = |r: u32, c: u32| u.iter().any(|&a| inside(r, c, a));
    assert!(
        stairs
            .iter()
            .all(|&(r0, c0, ..)| covered(r0, c0) && covered(r0 + 2, c0 + 2))
    );
    assert!(!covered(0, 3) && !covered(3, 0));
}

/// M2: a 50,000-long chain and derived columns, All levels, fast.
#[test]
fn dependents_of_a_50k_chain_and_derived_columns_are_fast() {
    let mut wb = book(&[("A1", Cell::number(1.0))]);
    wb.sheets[0].set_cell(0, 1, Cell::formula("A1"));
    for r in 1..50_000u32 {
        wb.sheets[0].set_cell(r, 0, Cell::number(1.0));
        wb.sheets[0].set_cell(r, 1, Cell::formula(&format!("B{}+A{}", r, r + 1)));
    }
    let t = std::time::Instant::now();
    let found = go_to_special(
        &wb,
        0,
        &[rect("A1")],
        (0, 0),
        GoSpecial::Dependents { all: true },
        &[],
    );
    fast(t, "chain");
    assert_eq!(found, Ok(rects(&["B1:B50000"])));
    let direct = go_to_special(
        &wb,
        0,
        &[rect("A1")],
        (0, 0),
        GoSpecial::Dependents { all: false },
        &[],
    );
    assert_eq!(direct, Ok(rects(&["B1"])));

    let mut wb = book(&[]);
    for r in 0..50_000u32 {
        wb.sheets[0].set_cell(r, 0, Cell::number(1.0));
        wb.sheets[0].set_cell(r, 1, Cell::formula(&format!("A{}*2", r + 1)));
        wb.sheets[0].set_cell(r, 2, Cell::formula(&format!("B{}+1", r + 1)));
    }
    let t = std::time::Instant::now();
    let found = go_to_special(
        &wb,
        0,
        &[rect("A1:A50000")],
        (0, 0),
        GoSpecial::Dependents { all: true },
        &[],
    );
    fast(t, "derived");
    assert_eq!(found, Ok(rects(&["B1:C50000"])));
}

/// #707 r6 M1: Dependents over 50k-row formula columns that read ranges:
/// a moving average, a running total, a percent of a total, and rows read
/// across hundreds of columns; each range reference examined once.
#[test]
fn dependents_through_50k_range_readers_are_fast() {
    let n = 50_000u32;
    type Make = fn(u32, u32) -> String;
    // Each C reads B; the walk from A1 reaches B1 and the Cs reading it.
    let cases: [(&str, Make, u32); 3] = [
        (
            "moving average",
            |r, _| format!("AVERAGE(B{}:B{})", r.saturating_sub(8).max(1), r + 1),
            10,
        ),
        ("running total", |r, _| format!("SUM($B$1:B{})", r + 1), n),
        (
            "percent of total",
            |r, n| format!("B{}/SUM($B$1:$B${n})", r + 1),
            n,
        ),
    ];
    for (what, make, cs) in cases {
        let mut wb = book(&[]);
        for r in 0..n {
            wb.sheets[0].set_cell(r, 0, Cell::number(1.0));
            wb.sheets[0].set_cell(r, 1, Cell::formula(&format!("A{}*2", r + 1)));
            wb.sheets[0].set_cell(r, 2, Cell::formula(&make(r, n)));
        }
        let t = std::time::Instant::now();
        let found = go_to_special(
            &wb,
            0,
            &[rect("A1")],
            (0, 0),
            GoSpecial::Dependents { all: true },
            &[],
        );
        fast(t, what);
        // A1 → B1 → every C that reads B1.
        let found = found.unwrap();
        let covered = |r: u32, c: u32| found.iter().any(|&a| inside(r, c, a));
        assert!(covered(0, 1), "{what}: B1");
        assert!((0..cs).all(|r| covered(r, 2)), "{what}: C1:C{cs}");
        assert!(!covered(cs, 2) || cs == n, "{what}: no further");
    }
    // Rows read across 702 columns: =SUM($A2:$ZZ2) down a column at 50k.
    let mut wb = book(&[]);
    for r in 0..n {
        wb.sheets[0].set_cell(r, 0, Cell::number(1.0));
        wb.sheets[0].set_cell(
            r,
            702,
            Cell::formula(&format!("SUM($A{}:$ZZ{})", r + 1, r + 1)),
        );
        wb.sheets[0].set_cell(r, 703, Cell::formula(&format!("AAA{}+1", r + 1)));
    }
    let t = std::time::Instant::now();
    let found = go_to_special(
        &wb,
        0,
        &[rect("A1:A50000")],
        (0, 0),
        GoSpecial::Dependents { all: true },
        &[],
    );
    fast(t, "wide rows");
    assert_eq!(found, Ok(vec![(0, 702, n - 1, 703)]));
}

/// #707 r6 M1: a range huge both ways is found by any cell in it, through
/// its whole bands and its ragged edges alike.
#[test]
fn a_huge_range_is_found_from_every_part() {
    let mut wb = book(&[]);
    // B2:KZ600 (300+ columns, 599 rows), read once.
    wb.sheets[0].set_cell(0, 0, Cell::formula("SUM(B2:KZ600)"));
    for (r, c) in [(1, 1), (300, 255), (300, 256), (599, 311), (5, 300)] {
        let found = go_to_special(
            &wb,
            0,
            &[(r, c, r, c)],
            (r, c),
            GoSpecial::Dependents { all: false },
            &[],
        );
        assert_eq!(found, Ok(vec![(0, 0, 0, 0)]), "({r}, {c})");
    }
    // Taken once per line it is indexed on: a second stab in the same
    // column, row or whole band finds nothing. (Each edge column of a range
    // huge both ways is a line of its own.)
    let mut out = Vec::new();
    for (f, first, again) in [
        // Edge column B of B2:KZ600 (narrower than a whole band).
        ("SUM(B2:KZ600)", (1, 1), (500, 1)),
        // Band 1 (IW:SR), whole in B2:AAA600.
        ("SUM(B2:AAA600)", (300, 300), (5, 400)),
        // Row 5 of a one-row range.
        ("SUM(B5:ZZ5)", (4, 3), (4, 600)),
    ] {
        let mut one = book(&[]);
        one.sheets[0].set_cell(0, 0, Cell::formula(f));
        let mut ix = ReadIndex::new(&one, 0, &one.sheets[0]);
        out.clear();
        ix.take_readers(first, &mut out);
        assert_eq!(out, vec![0], "{f} {first:?}");
        out.clear();
        ix.take_readers(again, &mut out);
        assert!(out.is_empty(), "{f}: {again:?} after {first:?}");
    }
    // Outside it: nothing.
    let mut ix = ReadIndex::new(&wb, 0, &wb.sheets[0]);
    for (r, c) in [(0, 1), (1, 0), (600, 5), (5, 312)] {
        out.clear();
        ix.take_readers((r, c), &mut out);
        assert!(out.is_empty(), "({r}, {c})");
    }
    let mut ix = ReadIndex::new(&wb, 0, &wb.sheets[0]);
    for (r, c) in [(1, 1), (300, 255), (300, 256), (599, 311), (5, 300)] {
        out.clear();
        ix.take_readers((r, c), &mut out);
        assert!(out.contains(&0), "({r}, {c}) inside");
        ix = ReadIndex::new(&wb, 0, &wb.sheets[0]);
    }
}

/// #707 r6: a selection of 10,000 areas (5,000 whole-height column strips
/// and 5,000 single cells) over 50,000 formulas, notes and hidden rows: the
/// areas are looked up through an index, never scanned per cell or per
/// reference.
#[test]
fn ten_thousand_areas_over_50k_cells_are_fast() {
    let n = 50_000u32;
    let mut wb = book(&[]);
    let s = &mut wb.sheets[0];
    for r in 0..n {
        s.set_cell(r, 0, Cell::number(1.0));
        s.set_cell(r, 1, Cell::formula(&format!("A{}*2", r + 1)));
        if r % 2 == 1 {
            s.row_attrs.insert(r, "hidden=\"1\"".into());
        }
    }
    let mut areas: Vec<Area> = (0..5_000u32)
        .map(|k| (0, 2 * k + 2, n - 1, 2 * k + 2))
        .collect();
    areas.extend((0..5_000u32).map(|k| (k * 10, 0, k * 10, 0)));
    let notes: Vec<(u32, u32)> = (0..n).map(|r| (r, 0)).collect();
    let t = std::time::Instant::now();
    let deps = go_to_special(
        &wb,
        0,
        &areas,
        (0, 0),
        GoSpecial::Dependents { all: false },
        &[],
    )
    .unwrap();
    assert_eq!(deps.len(), 5_000);
    let noted = go_to_special(&wb, 0, &areas, (0, 0), GoSpecial::Notes, &notes).unwrap();
    assert_eq!(noted.len(), 5_000);
    let consts = go_to_special(
        &wb,
        0,
        &areas,
        (0, 0),
        GoSpecial::Constants(Types::ALL),
        &[],
    )
    .unwrap();
    assert_eq!(consts.len(), 5_000);
    // Every other row hidden: each strip cut into 25,000 runs is too many.
    assert_eq!(
        go_to_special(&wb, 0, &areas[..2], (0, 0), GoSpecial::VisibleCells, &[]),
        Err(TOO_MANY_AREAS)
    );
    fast(t, "10k areas");
}
