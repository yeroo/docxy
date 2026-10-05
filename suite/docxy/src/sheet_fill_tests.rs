//! Home › Fill and the fill handle (#668), on `SheetView` without a window.
//! `uiharness/cases/sheet-fill.uit` drives the same steps through the
//! ribbon, the menus and the fill handle.

use super::*;
use core::prelude::v1::test;
use gridcore::edit::FillDir;
use gridcore::sheet::{Cell, CellValue};

fn view() -> SheetView {
    let Surface::Sheet(v) = new_sheet_surface() else {
        panic!("a new sheet surface")
    };
    v
}

fn at(name: &str) -> (u32, u32) {
    gridcore::sheet::parse_cell_name(name).unwrap()
}

fn put(v: &mut SheetView, name: &str, cell: Cell) {
    let (r, c) = at(name);
    let s = v.active;
    v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
}

fn formula(v: &SheetView, name: &str) -> Option<String> {
    let (r, c) = at(name);
    v.sheet().cell(r, c).and_then(|c| c.formula.clone())
}

fn value(v: &SheetView, name: &str) -> CellValue {
    let (r, c) = at(name);
    v.sheet()
        .cell(r, c)
        .map(|c| c.value.clone())
        .unwrap_or_default()
}

fn select(v: &mut SheetView, from: &str, to: &str) {
    v.anchor = at(from);
    v.sel = at(to);
}

#[test]
fn fill_up_and_left_copy_the_last_row_and_column() {
    let mut v = view();
    put(&mut v, "A3", Cell::number(7.0));
    put(&mut v, "B3", Cell::formula("A3*2"));
    select(&mut v, "A1", "B3");
    assert!(v.fill_selection(FillDir::Up));
    assert_eq!(value(&v, "A1"), CellValue::Number(7.0));
    assert_eq!(formula(&v, "B1").as_deref(), Some("A1*2"));
    assert_eq!(v.undo.len(), 1, "one undo step");

    let mut v = view();
    put(&mut v, "C1", Cell::formula("D1+1"));
    select(&mut v, "A1", "C1");
    assert!(v.fill_selection(FillDir::Left));
    assert_eq!(formula(&v, "A1").as_deref(), Some("B1+1"));
    // One cell pulls from its neighbour after it.
    let mut v = view();
    put(&mut v, "B2", Cell::number(3.0));
    select(&mut v, "B1", "B1");
    assert!(v.fill_selection(FillDir::Up));
    assert_eq!(value(&v, "B1"), CellValue::Number(3.0));
}
