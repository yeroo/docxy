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
    put(&mut eng, &mut wb, 1, "B1", Cell::formula("Sheet1!A1+1"));
    wb.calc.mode = CalcMode::Manual;
    put(&mut eng, &mut wb, 0, "A1", Cell::number(7.0));
    assert_eq!(eng.stale_on_sheet(0), 1);
    assert_eq!(eng.stale_on_sheet(1), 1);

    eng.recalc_sheet(&mut wb, 0);
    assert_eq!(num(&wb, 0, "A2"), 14.0);
    assert_eq!(num(&wb, 1, "B1"), 6.0, "the other sheet is not calculated");
    assert_eq!(eng.stale_on_sheet(0), 0);
    assert_eq!(eng.stale_on_sheet(1), 1, "and stays reported");

    eng.recalc_sheet(&mut wb, 1);
    assert_eq!(num(&wb, 1, "B1"), 8.0);
    assert!(!eng.is_stale());
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
