//! A workbook with a volatile formula is recalculated when it opens, and that
//! is a change, in automatic mode only (#805).

use gridcore::calc::{open_volatile, volatile_on_open};
use gridcore::engine::Engine;
use gridcore::sheet::{CalcMode, Cell, CellValue, Sheet, Workbook, parse_cell_name};

fn num(wb: &Workbook, name: &str) -> f64 {
    let (r, c) = parse_cell_name(name).unwrap();
    match wb.sheets[0].cell(r, c).map(|c| c.value.clone()) {
        Some(CellValue::Number(n)) => n,
        other => panic!("{name}: {other:?}"),
    }
}

/// A1 = NOW(), B1 = A1+1, both cached at serial 100, the clock at 50000.5.
fn stale_book(mode: CalcMode) -> (Workbook, Engine) {
    let mut wb = Workbook {
        sheets: vec![Sheet {
            name: "Sheet1".into(),
            ..Sheet::default()
        }],
        ..Workbook::default()
    };
    let mut eng = Engine::new(&wb);
    eng.clock = Some(50000.5);
    for (name, cell) in [
        ("A1", Cell::formula("NOW()")),
        ("B1", Cell::formula("A1+1")),
    ] {
        assert!(eng.set_cell(&mut wb, (0, 0, parse_cell_name(name).unwrap().1), cell));
    }
    for name in ["A1", "B1"] {
        let (r, c) = parse_cell_name(name).unwrap();
        wb.sheets[0].cells.get_mut(&(r, c)).unwrap().value = CellValue::Number(100.0);
    }
    wb.calc.mode = mode;
    (wb, eng)
}

#[test]
fn open_refreshes_now_and_its_dependent() {
    for mode in [CalcMode::Auto, CalcMode::AutoNoTable] {
        let (mut wb, mut eng) = stale_book(mode);
        assert!(volatile_on_open(&wb, &mut eng));
        assert!(open_volatile(&mut wb, &mut eng));
        assert!(num(&wb, "A1") >= 50000.5);
        assert_eq!(num(&wb, "B1"), num(&wb, "A1") + 1.0);
    }
}

#[test]
fn a_manual_workbook_is_no_change_and_keeps_its_values() {
    let (mut wb, mut eng) = stale_book(CalcMode::Manual);
    assert!(!volatile_on_open(&wb, &mut eng));
    assert!(!open_volatile(&mut wb, &mut eng));
    assert_eq!(num(&wb, "A1"), 100.0);
}

#[test]
fn no_volatile_formula_is_no_change() {
    let mut wb = Workbook {
        sheets: vec![Sheet {
            name: "Sheet1".into(),
            ..Sheet::default()
        }],
        ..Workbook::default()
    };
    let mut eng = Engine::new(&wb);
    assert!(eng.set_cell(&mut wb, (0, 0, 0), Cell::number(5.0)));
    assert!(eng.set_cell(&mut wb, (0, 1, 0), Cell::formula("A1*2")));
    wb.sheets[0].cells.get_mut(&(1, 0)).unwrap().value = CellValue::Number(1.0);
    assert!(!open_volatile(&mut wb, &mut eng));
    assert_eq!(num(&wb, "A2"), 1.0);
}
