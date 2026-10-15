//! Manual calculation mode (#674): edits leave dependents stale until
//! Calculate Now / Calculate Sheet.

use gridcore::engine::Engine;
use gridcore::sheet::{CalcMode, Cell, CellValue, Sheet, Workbook, parse_cell_name};

fn book(sheets: &[&str]) -> Workbook {
    Workbook {
        sheets: sheets
            .iter()
            .map(|n| Sheet {
                name: n.to_string(),
                ..Sheet::default()
            })
            .collect(),
        ..Workbook::default()
    }
}

fn key(s: usize, name: &str) -> (usize, u32, u32) {
    let (r, c) = parse_cell_name(name).unwrap();
    (s, r, c)
}

fn put(eng: &mut Engine, wb: &mut Workbook, s: usize, name: &str, cell: Cell) {
    assert!(eng.set_cell(wb, key(s, name), cell));
}

fn num(wb: &Workbook, s: usize, name: &str) -> f64 {
    let (_, r, c) = key(s, name);
    match wb.sheets[s].cell(r, c).map(|c| c.value.clone()) {
        Some(CellValue::Number(n)) => n,
        other => panic!("{name}: {other:?}"),
    }
}

/// A1 = 5, A2 = A1*2 (10), in manual mode.
fn issue_repro() -> (Workbook, Engine) {
    let mut wb = book(&["Sheet1"]);
    let mut eng = Engine::new(&wb);
    put(&mut eng, &mut wb, 0, "A1", Cell::number(5.0));
    put(&mut eng, &mut wb, 0, "A2", Cell::formula("A1*2"));
    assert_eq!(num(&wb, 0, "A2"), 10.0);
    wb.calc.mode = CalcMode::Manual;
    (wb, eng)
}

#[test]
fn manual_mode_edit_leaves_dependents_stale() {
    let (mut wb, mut eng) = issue_repro();
    assert!(!eng.is_stale());
    put(&mut eng, &mut wb, 0, "A1", Cell::number(7.0));
    assert_eq!(num(&wb, 0, "A2"), 10.0, "A2 is not re-evaluated");
    assert!(eng.is_stale());
    assert_eq!(eng.stale_count(), 1);
}

#[test]
fn automatic_mode_still_recalculates_on_edit() {
    let (mut wb, mut eng) = issue_repro();
    wb.calc.mode = CalcMode::AutoNoTable;
    put(&mut eng, &mut wb, 0, "A1", Cell::number(7.0));
    assert_eq!(num(&wb, 0, "A2"), 14.0);
    assert!(!eng.is_stale());
}

#[test]
fn calculate_now_clears_stale_and_updates() {
    let (mut wb, mut eng) = issue_repro();
    put(&mut eng, &mut wb, 0, "A1", Cell::number(7.0));
    eng.recalc_all(&mut wb);
    assert_eq!(num(&wb, 0, "A2"), 14.0);
    assert!(!eng.is_stale());
}

#[test]
fn manual_new_formula_evaluates_its_own_cell() {
    let (mut wb, mut eng) = issue_repro();
    put(&mut eng, &mut wb, 0, "A1", Cell::number(7.0));
    put(&mut eng, &mut wb, 0, "B1", Cell::formula("A1+1"));
    assert_eq!(num(&wb, 0, "B1"), 8.0, "the typed formula evaluates");
    assert_eq!(num(&wb, 0, "A2"), 10.0, "its sibling stays stale");
    assert_eq!(eng.stale_count(), 1);
}

#[test]
fn overwriting_a_stale_formula_with_a_constant_drops_it() {
    let (mut wb, mut eng) = issue_repro();
    put(&mut eng, &mut wb, 0, "A1", Cell::number(7.0));
    put(&mut eng, &mut wb, 0, "A2", Cell::number(1.0));
    assert!(!eng.is_stale());
}

#[test]
fn calculate_sheet_only_touches_that_sheet() {
    let mut wb = book(&["Sheet1", "Sheet2"]);
    let mut eng = Engine::new(&wb);
    put(&mut eng, &mut wb, 0, "A1", Cell::number(5.0));
    put(&mut eng, &mut wb, 0, "A2", Cell::formula("A1*2"));
    // Reads a formula of the calculated sheet, so it depends on what changes.
    put(&mut eng, &mut wb, 1, "B1", Cell::formula("Sheet1!A2+1"));
    assert_eq!(num(&wb, 1, "B1"), 11.0);
    wb.calc.mode = CalcMode::Manual;
    put(&mut eng, &mut wb, 0, "A1", Cell::number(7.0));
    assert_eq!(eng.stale_on_sheet(0), 1);
    assert_eq!(eng.stale_on_sheet(1), 1);

    eng.recalc_sheet(&mut wb, 0);
    assert_eq!(num(&wb, 0, "A2"), 14.0);
    assert_eq!(num(&wb, 1, "B1"), 11.0, "the other sheet is not calculated");
    assert_eq!(eng.stale_on_sheet(0), 0);
    assert_eq!(eng.stale_on_sheet(1), 1, "and stays reported");

    eng.recalc_sheet(&mut wb, 1);
    assert_eq!(num(&wb, 1, "B1"), 15.0);
    assert!(!eng.is_stale());
}

#[test]
fn calculate_now_in_manual_mode_updates_what_reads_a_refreshed_pivot() {
    use gridcore::pivot::{DataField, Pivot, PivotSource};
    let mut wb = book(&["Sheet1"]);
    let sh = &mut wb.sheets[0];
    sh.set_cell(0, 0, Cell::text("Region"));
    sh.set_cell(0, 1, Cell::text("Sales"));
    for (i, (r, v)) in [("East", 10.0), ("West", 20.0), ("East", 30.0)]
        .iter()
        .enumerate()
    {
        sh.set_cell(i as u32 + 1, 0, Cell::text(r));
        sh.set_cell(i as u32 + 1, 1, Cell::number(*v));
    }
    wb.pivots.push(Pivot {
        name: "P".into(),
        sheet: 0,
        location: (0, 3, 0, 3),
        source: PivotSource::Range {
            sheet: "Sheet1".into(),
            rect: (0, 0, 3, 1),
        },
        fields: vec!["Region".into(), "Sales".into()],
        row_fields: vec![0],
        col_fields: vec![],
        data_fields: vec![DataField {
            name: "Sum of Sales".into(),
            field: 1,
            agg: gridcore::frame::Agg::Sum,
        }],
        field_items: Vec::new(),
        hidden: Vec::new(),
        page: Vec::new(),
        items_order: Vec::new(),
        calc_formulas: Vec::new(),
        grand_rows: true,
        grand_cols: true,
        subtotals: false,
        data_on_rows: false,
        unsupported: false,
        edited: false,
        refreshed: false,
        part: String::new(),
        cache_part: String::new(),
    });
    let mut eng = Engine::new(&wb);
    // G1 reads the pivot's East total (E2).
    put(&mut eng, &mut wb, 0, "G1", Cell::formula("E2*2"));
    gridcore::calc::calculate_now(&mut wb, &mut eng, true);
    assert_eq!(num(&wb, 0, "E2"), 40.0);
    assert_eq!(num(&wb, 0, "G1"), 80.0);

    wb.calc.mode = CalcMode::Manual;
    put(&mut eng, &mut wb, 0, "B2", Cell::number(100.0));
    gridcore::calc::calculate_now(&mut wb, &mut eng, true);
    assert_eq!(num(&wb, 0, "E2"), 130.0, "the pivot refreshed");
    assert_eq!(num(&wb, 0, "G1"), 260.0, "and its reader follows");
    assert!(!eng.is_stale(), "nothing is left to calculate");
}

#[test]
fn manual_edit_does_not_recalc_rand() {
    let mut wb = book(&["Sheet1"]);
    let mut eng = Engine::new(&wb);
    eng.seed = Some(42);
    put(&mut eng, &mut wb, 0, "B1", Cell::formula("RAND()"));
    let first = num(&wb, 0, "B1");
    wb.calc.mode = CalcMode::Manual;
    put(&mut eng, &mut wb, 0, "D1", Cell::number(1.0));
    assert_eq!(num(&wb, 0, "B1"), first, "volatile waits for a Calculate");
    assert!(eng.is_stale(), "and is reported stale");
    eng.recalc_sheet(&mut wb, 0);
    assert_ne!(num(&wb, 0, "B1"), first);
    assert!(!eng.is_stale());
}

#[test]
fn manual_spill_pass_marks_stale_not_evaluates() {
    let mut wb = book(&["Sheet1"]);
    let mut eng = Engine::new(&wb);
    put(&mut eng, &mut wb, 0, "B1", Cell::formula("A2*2"));
    assert_eq!(num(&wb, 0, "B1"), 0.0);
    wb.calc.mode = CalcMode::Manual;
    let mut seq = Cell::formula("SEQUENCE(3)");
    seq.f_attrs = Some("t=\"array\"".to_string());
    put(&mut eng, &mut wb, 0, "A1", seq);
    assert_eq!(num(&wb, 0, "A2"), 2.0, "the typed anchor spilled");
    assert_eq!(num(&wb, 0, "B1"), 0.0, "a reader of the spill is stale");
    assert_eq!(eng.stale_count(), 1);
    eng.recalc_all(&mut wb);
    assert_eq!(
        num(&wb, 0, "B1"),
        4.0,
        "Calculate Now evaluates spill readers"
    );
    assert!(!eng.is_stale());
}

#[test]
fn iterate_one_pass_per_calculate_in_manual() {
    let mut wb = book(&["Sheet1"]);
    wb.set_iteration(true, 100, 1e-9);
    let mut eng = Engine::new(&wb);
    put(&mut eng, &mut wb, 0, "D1", Cell::formula("D1/2+5"));
    assert!(
        (num(&wb, 0, "D1") - 10.0).abs() < 1e-6,
        "automatic converges"
    );

    let mut wb = book(&["Sheet1"]);
    wb.set_iteration(true, 100, 1e-9);
    let mut eng = Engine::new(&wb);
    put(&mut eng, &mut wb, 0, "D1", Cell::formula("D1/2+5"));
    wb.calc.mode = CalcMode::Manual;
    wb.sheets[0].cells.get_mut(&(0, 3)).unwrap().value = CellValue::Number(0.0);
    eng.recalc_all(&mut wb);
    assert_eq!(num(&wb, 0, "D1"), 5.0, "one pass: 0/2+5");
    eng.recalc_all(&mut wb);
    assert_eq!(
        num(&wb, 0, "D1"),
        7.5,
        "the next Calculate takes the next pass"
    );
    assert_eq!(eng.circular_refs(), vec![key(0, "D1")], "still circular");
}

/// D1 = D1/2+5 with iteration off: a circle, in manual mode.
fn manual_circle() -> (Workbook, Engine) {
    let mut wb = book(&["Sheet1"]);
    let mut eng = Engine::new(&wb);
    put(&mut eng, &mut wb, 0, "D1", Cell::formula("D1/2+5"));
    assert_eq!(eng.circular_refs(), vec![key(0, "D1")]);
    wb.calc.mode = CalcMode::Manual;
    (wb, eng)
}

#[test]
fn an_iteration_change_in_manual_mode_waits_for_calculate() {
    let (mut wb, mut eng) = manual_circle();
    let on = gridcore::calc::CalcPatch {
        iterate: Some(true),
        max_iterations: Some(100),
        max_change: Some(1e-9),
        ..Default::default()
    };
    gridcore::calc::apply_settings(&mut wb, &mut eng, &on);
    assert!(eng.is_stale(), "the circle's member waits for a Calculate");
    assert_eq!(wb.iterate.map(|i| i.0), Some(100));
    eng.recalc_sheet(&mut wb, 0);
    assert!(!eng.is_stale());
    assert_eq!(num(&wb, 0, "D1"), 5.0, "one pass");

    // The same settings again change nothing and leave nothing stale.
    gridcore::calc::apply_settings(&mut wb, &mut eng, &on);
    assert!(!eng.is_stale());
}

#[test]
fn going_automatic_in_the_same_call_calculates_the_circle() {
    let (mut wb, mut eng) = manual_circle();
    let p = gridcore::calc::CalcPatch {
        mode: Some(CalcMode::Auto),
        iterate: Some(true),
        max_iterations: Some(100),
        max_change: Some(1e-9),
        ..Default::default()
    };
    gridcore::calc::apply_settings(&mut wb, &mut eng, &p);
    assert!(!eng.is_stale());
    assert!(
        (num(&wb, 0, "D1") - 10.0).abs() < 1e-6,
        "iterated to its fixed point"
    );
}

#[test]
fn one_patch_calculates_once() {
    // Precision off and a new iteration limit in one patch: one recalculation
    // (50 passes of A1 = A1+1), not one for each.
    let mut wb = book(&["Sheet1"]);
    wb.set_iteration(true, 100, 1e-9);
    wb.calc.set_precision_as_displayed(true);
    let mut eng = Engine::new(&wb);
    put(&mut eng, &mut wb, 0, "A1", Cell::formula("A1+1"));
    let before = num(&wb, 0, "A1");
    let p = gridcore::calc::CalcPatch {
        precision_as_displayed: Some(false),
        max_iterations: Some(50),
        ..Default::default()
    };
    gridcore::calc::apply_settings(&mut wb, &mut eng, &p);
    assert_eq!(num(&wb, 0, "A1") - before, 50.0);
}
