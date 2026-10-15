//! Set precision as displayed (#674): constants take the value their number
//! format shows, formula results round as they are written, one undo step.

use gridcore::calc;
use gridcore::engine::Engine;
use gridcore::sheet::{CalcMode, Cell, CellValue, Sheet, Workbook, Xf, parse_cell_name};

const GENERAL: u32 = 0;
const TWO: u32 = 1;
const PCT: u32 = 2;
const DATE: u32 = 3;
const MILLIONS: u32 = 4;
const ONE: u32 = 5;

fn book() -> Workbook {
    let xf = |code: &str| {
        let mut x = Xf::default();
        x.set_code(Some(code.to_string()));
        x
    };
    let mut wb = Workbook {
        sheets: vec![Sheet {
            name: "Sheet1".into(),
            ..Sheet::default()
        }],
        ..Workbook::default()
    };
    wb.styles.xfs = vec![
        Xf::default(),
        xf("0.00"),
        xf("0.0%"),
        xf("yyyy-mm-dd"),
        xf("0.0,,"),
        xf("0.0"),
    ];
    wb
}

fn key(name: &str) -> (usize, u32, u32) {
    let (r, c) = parse_cell_name(name).unwrap();
    (0, r, c)
}

fn put(eng: &mut Engine, wb: &mut Workbook, name: &str, value: CellValue, style: u32) {
    let cell = Cell {
        value,
        style,
        ..Cell::default()
    };
    assert!(eng.set_cell(wb, key(name), cell));
}

fn put_formula(eng: &mut Engine, wb: &mut Workbook, name: &str, f: &str, style: u32) {
    let cell = Cell {
        style,
        ..Cell::formula(f)
    };
    assert!(eng.set_cell(wb, key(name), cell));
}

fn num(wb: &Workbook, name: &str) -> f64 {
    let (_, r, c) = key(name);
    match wb.sheets[0].cell(r, c).map(|c| c.value.clone()) {
        Some(CellValue::Number(n)) => n,
        other => panic!("{name}: {other:?}"),
    }
}

fn filled() -> (Workbook, Engine) {
    let mut wb = book();
    let mut eng = Engine::new(&wb);
    let n = CellValue::Number;
    put(&mut eng, &mut wb, "A1", n(3.14659), TWO);
    put(&mut eng, &mut wb, "A2", n(2.71928), GENERAL);
    put(&mut eng, &mut wb, "A3", n(0.12345), PCT);
    put(&mut eng, &mut wb, "A4", n(45000.5), DATE);
    put(&mut eng, &mut wb, "A5", CellValue::Text("x".into()), TWO);
    put(&mut eng, &mut wb, "A6", n(1234567.0), MILLIONS);
    put(&mut eng, &mut wb, "A7", n(-2.346), TWO);
    put_formula(&mut eng, &mut wb, "B1", "A1+A2", TWO);
    (wb, eng)
}

#[test]
fn precision_as_displayed_rounds_constants_not_general() {
    let (mut wb, mut eng) = filled();
    assert_eq!(num(&wb, "B1"), 3.14659 + 2.71928, "full precision before");
    let conv = calc::set_precision_as_displayed(&mut wb, &mut eng, true).expect("converted");
    assert!(wb.calc.precision_as_displayed);
    assert_eq!(num(&wb, "A1"), 3.15);
    assert_eq!(num(&wb, "A2"), 2.71928, "General untouched");
    assert_eq!(num(&wb, "A3"), 0.123);
    assert_eq!(num(&wb, "A4"), 45000.5, "a date is untouched");
    assert_eq!(num(&wb, "A6"), 1200000.0);
    assert_eq!(num(&wb, "A7"), -2.35);
    assert_eq!(
        wb.sheets[0].cell(4, 0).unwrap().value,
        CellValue::Text("x".into())
    );
    // The formula stays; its result is rounded as it evaluates (3.14 + 2.71928).
    assert_eq!(
        wb.sheets[0].cell(0, 1).unwrap().formula.as_deref(),
        Some("A1+A2")
    );
    assert_eq!(num(&wb, "B1"), 5.87);
    assert_eq!(conv.cells.len(), 4, "A1, A3, A6, A7 changed");
}

#[test]
fn precision_as_displayed_undo_restores() {
    let (mut wb, mut eng) = filled();
    let conv = calc::set_precision_as_displayed(&mut wb, &mut eng, true).unwrap();
    calc::undo_precision_conversion(&mut wb, &mut eng, &conv);
    assert!(!wb.calc.precision_as_displayed);
    assert_eq!(num(&wb, "A1"), 3.14659);
    assert_eq!(num(&wb, "A3"), 0.12345);
    assert_eq!(num(&wb, "A7"), -2.346);
    assert_eq!(num(&wb, "B1"), 3.14659 + 2.71928);
}

#[test]
fn turning_it_off_changes_no_data() {
    let (mut wb, mut eng) = filled();
    calc::set_precision_as_displayed(&mut wb, &mut eng, true).unwrap();
    assert!(calc::set_precision_as_displayed(&mut wb, &mut eng, false).is_none());
    assert_eq!(num(&wb, "A1"), 3.15);
}

#[test]
fn typed_constants_round_but_a_restored_snapshot_does_not() {
    let (mut wb, mut eng) = filled();
    calc::set_precision_as_displayed(&mut wb, &mut eng, true).unwrap();
    put(&mut eng, &mut wb, "C1", CellValue::Number(9.8765), TWO);
    assert_eq!(num(&wb, "C1"), 9.88);
    let exact = Cell {
        value: CellValue::Number(9.8765),
        style: TWO,
        ..Cell::default()
    };
    eng.restore_cell(&mut wb, key("C1"), exact);
    assert_eq!(num(&wb, "C1"), 9.8765, "undo/redo put back what was there");
}

#[test]
fn spilled_results_round_to_their_cells_formats() {
    let mut wb = book();
    let mut eng = Engine::new(&wb);
    wb.calc.set_precision_as_displayed(true);
    // Styled blanks the spill lands in.
    for name in ["D1", "D2", "D3"] {
        put(&mut eng, &mut wb, name, CellValue::Empty, TWO);
    }
    let mut seq = Cell {
        style: TWO,
        ..Cell::formula("SEQUENCE(3)/3")
    };
    seq.f_attrs = Some("t=\"array\"".to_string());
    assert!(eng.set_cell(&mut wb, key("D1"), seq));
    assert_eq!(
        [num(&wb, "D1"), num(&wb, "D2"), num(&wb, "D3")],
        [0.33, 0.67, 1.0]
    );
}

#[test]
fn iteration_passes_round_each_pass() {
    let mut wb = book();
    wb.set_iteration(true, 100, 1e-9);
    let mut eng = Engine::new(&wb);
    put_formula(&mut eng, &mut wb, "D1", "D1/2+5", ONE);
    wb.calc.mode = CalcMode::Manual;
    wb.calc.set_precision_as_displayed(true);
    wb.sheets[0].cells.get_mut(&(0, 3)).unwrap().value = CellValue::Number(0.0);
    let mut seen = Vec::new();
    for _ in 0..3 {
        calc::calculate_now(&mut wb, &mut eng);
        seen.push(num(&wb, "D1"));
    }
    assert_eq!(seen, [5.0, 7.5, 8.8]);
}
