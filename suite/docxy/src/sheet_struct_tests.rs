//! Whole rows and the structural edits on them (#860), on `SheetView`
//! without a window: Shift+Space's row selection, and Insert/Delete Sheet
//! Rows and Columns over every row or column the selection touches, as one
//! undo step. `uiharness/cases/sheet-row-keys.uit` drives the keys.

use super::*;
use core::prelude::v1::test;
use gridcore::outline::Axis;
use gridcore::sheet::{Cell, CellValue, MAX_COLS};

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

fn value(v: &SheetView, name: &str) -> CellValue {
    let (r, c) = at(name);
    v.sheet()
        .cell(r, c)
        .map(|c| c.value.clone())
        .unwrap_or_default()
}

fn formula(v: &SheetView, name: &str) -> Option<String> {
    let (r, c) = at(name);
    v.sheet().cell(r, c).and_then(|c| c.formula.clone())
}

fn select(v: &mut SheetView, from: &str, to: &str) {
    v.anchor = at(from);
    v.sel = at(to);
    v.clear_areas();
}

/// A1:A`n` holding 1..=n, as the issue's repro types them.
fn numbered(n: u32) -> SheetView {
    let mut v = view();
    for i in 1..=n {
        put(&mut v, &format!("A{i}"), Cell::number(f64::from(i)));
    }
    v
}

fn num(n: u32) -> CellValue {
    CellValue::Number(f64::from(n))
}

const LAST: u32 = MAX_COLS - 1;

#[test]
fn shift_space_selects_the_whole_rows_and_keeps_the_active_row() {
    let mut v = view();
    // Anchored at B2, the active cell at C3: rows 2 and 3.
    select(&mut v, "B2", "C3");
    v.select_rows();
    assert_eq!(v.range(), (1, 0, 2, LAST));
    // The active cell keeps its row and moves to column A, never to the
    // last column, which the grid would scroll to.
    assert_eq!(v.sel, (2, 0));
    assert_eq!(v.anchor, (1, LAST));
    assert!(v.editing.is_none());
    assert!(matches!(
        sheet_outline::selected_axis(&v),
        Some((Axis::Rows, 1, 2))
    ));
}

#[test]
fn shift_space_widens_every_area_and_the_selection_stays_multi_area() {
    let mut v = view();
    select(&mut v, "B2", "B2");
    v.add_area(at("D5"));
    v.extend_active(at("E6"));
    v.select_rows();
    assert!(v.multi_area());
    assert_eq!(v.areas_all(), vec![(1, 0, 1, LAST), (4, 0, 5, LAST)]);
}

#[test]
fn inserting_a_whole_row_shifts_the_row_down_as_one_undo_step() {
    // The issue's repro: A2 selected, Shift+Space, Ctrl+Shift+=.
    let mut v = numbered(3);
    select(&mut v, "A2", "A2");
    v.select_rows();
    let selected = (v.sel, v.anchor);
    let steps = v.undo.len();
    assert!(v.structural_edit(StructOp::InsertRow));
    assert_eq!(value(&v, "A2"), CellValue::Empty);
    assert_eq!(value(&v, "A3"), num(2));
    assert_eq!(value(&v, "A4"), num(3));
    assert_eq!(v.undo.len(), steps + 1);
    // The inserted row stays selected, as Excel's does.
    assert_eq!((v.sel, v.anchor), selected);
    // Undo puts the row back and selects it again.
    v.sel = (0, 0);
    v.anchor = (0, 0);
    assert!(v.undo_step());
    assert_eq!(value(&v, "A2"), num(2));
    assert_eq!(value(&v, "A3"), num(3));
    assert_eq!((v.sel, v.anchor), selected);
}

#[test]
fn inserting_two_whole_rows_inserts_two_and_formulas_follow() {
    let mut v = numbered(3);
    put(&mut v, "B10", Cell::formula("SUM(A1:A3)"));
    select(&mut v, "A2", "A3");
    v.select_rows();
    assert!(v.structural_edit(StructOp::InsertRow));
    assert_eq!(value(&v, "A1"), num(1));
    assert_eq!(value(&v, "A2"), CellValue::Empty);
    assert_eq!(value(&v, "A3"), CellValue::Empty);
    assert_eq!(value(&v, "A4"), num(2));
    assert_eq!(value(&v, "A5"), num(3));
    assert_eq!(formula(&v, "B12").as_deref(), Some("SUM(A1:A5)"));
}

#[test]
fn deleting_a_whole_row_pulls_the_rows_below_up() {
    // The issue's repro: A2 selected, Shift+Space, Ctrl+-.
    let mut v = numbered(3);
    select(&mut v, "A2", "A2");
    v.select_rows();
    let steps = v.undo.len();
    assert!(v.structural_edit(StructOp::DeleteRow));
    assert_eq!(value(&v, "A2"), num(3));
    assert_eq!(v.undo.len(), steps + 1);
    assert!(v.undo_step());
    assert_eq!(value(&v, "A2"), num(2));
}

#[test]
fn delete_row_deletes_every_row_a_cell_range_touches() {
    // Home › Delete Row with A2:A3 selected deleted only row 3.
    let mut v = numbered(5);
    put(&mut v, "B10", Cell::formula("A5"));
    select(&mut v, "A2", "A3");
    assert!(v.structural_edit(StructOp::DeleteRow));
    assert_eq!(value(&v, "A1"), num(1));
    assert_eq!(value(&v, "A2"), num(4));
    assert_eq!(value(&v, "A3"), num(5));
    assert_eq!(formula(&v, "B8").as_deref(), Some("A3"));
}

#[test]
fn insert_row_inserts_as_many_rows_as_a_cell_range_touches() {
    let mut v = numbered(3);
    // Anchored below the active cell: the range is what counts.
    select(&mut v, "B3", "A2");
    assert!(v.structural_edit(StructOp::InsertRow));
    assert_eq!(value(&v, "A1"), num(1));
    assert_eq!(value(&v, "A4"), num(2));
    assert_eq!(value(&v, "A5"), num(3));
}

#[test]
fn insert_and_delete_column_act_on_every_column_touched() {
    let mut v = view();
    for (i, name) in ["A1", "B1", "C1", "D1"].into_iter().enumerate() {
        put(&mut v, name, Cell::number(i as f64 + 1.0));
    }
    select(&mut v, "B1", "C5");
    assert!(v.structural_edit(StructOp::InsertCol));
    assert_eq!(value(&v, "B1"), CellValue::Empty);
    assert_eq!(value(&v, "C1"), CellValue::Empty);
    assert_eq!(value(&v, "D1"), num(2));
    assert!(v.structural_edit(StructOp::DeleteCol));
    assert_eq!(value(&v, "B1"), num(2));
    assert!(v.structural_edit(StructOp::DeleteCol));
    assert_eq!(value(&v, "A1"), num(1));
    assert_eq!(value(&v, "B1"), num(4));
}

#[test]
fn a_protected_sheet_refuses_inserting_and_deleting_rows_and_columns() {
    let mut v = numbered(3);
    let s = v.active;
    v.pkg.workbook.sheets[s].set_protected(true);
    select(&mut v, "A2", "A2");
    v.select_rows();
    let steps = v.undo.len();
    for op in [
        StructOp::InsertRow,
        StructOp::DeleteRow,
        StructOp::InsertCol,
        StructOp::DeleteCol,
    ] {
        assert!(!v.structural_edit(op));
        assert_eq!(v.entry_error.take().as_deref(), Some(STRUCT_PROTECTED));
    }
    assert_eq!(value(&v, "A2"), num(2));
    assert_eq!(value(&v, "A3"), num(3));
    assert_eq!(v.undo.len(), steps);
}

#[test]
fn a_chart_follows_a_deleted_block_of_rows() {
    let mut v = numbered(7);
    for r in 0..7 {
        put(&mut v, &format!("B{}", r + 1), Cell::number(2.0 * r as f64));
    }
    let sh = v.sheet();
    // A5:B7, below the rows that go.
    let data = gridcore::sheet::chart_from_range(sh, &sh.name, (4, 0, 6, 1), "column", false)
        .expect("chart data");
    v.charts.push(ChartView {
        sheet: 0,
        from: (0, 3),
        to: (10, 9),
        data,
    });
    select(&mut v, "A2", "A3");
    v.select_rows();
    assert!(v.structural_edit(StructOp::DeleteRow));
    let source = v.charts[0].data.source.as_ref().expect("range-backed");
    assert_eq!(source.range, (2, 0, 4, 1));
}

#[test]
fn a_new_sheet_takes_the_next_free_sheet_name() {
    let mut v = view();
    let first = v.pkg.workbook.next_sheet_name();
    let idx = v.add_sheet(&first);
    assert_eq!(v.pkg.workbook.sheets[idx].name, first);
    assert_eq!(v.active, idx);
    assert_ne!(v.pkg.workbook.next_sheet_name(), first);
}

/// A1:C3 holding numbers, as the review's case has it.
fn block() -> SheetView {
    let mut v = view();
    for r in 1..=3 {
        for c in ["A", "B", "C"] {
            put(&mut v, &format!("{c}{r}"), Cell::number(f64::from(r)));
        }
    }
    v
}

#[test]
fn insert_col_on_whole_rows_refuses_to_push_data_off_the_sheet() {
    // Shift+Space, then Home › Insert Col: 16,384 columns before A.
    let mut v = block();
    select(&mut v, "B2", "B2");
    v.select_rows();
    let (cells, steps) = (v.sheet().cells.clone(), v.undo.len());
    assert!(!v.structural_edit(StructOp::InsertCol));
    assert_eq!(
        v.entry_error.take().as_deref(),
        Some(gridcore::edit::SHIFT_OFF_SHEET)
    );
    assert_eq!(v.sheet().cells, cells);
    assert_eq!(v.undo.len(), steps);
}

#[test]
fn insert_row_on_whole_columns_refuses_to_push_data_off_the_sheet() {
    let mut v = block();
    // Column B, whole.
    v.anchor = (0, 1);
    v.sel = (gridcore::sheet::MAX_ROWS - 1, 1);
    v.clear_areas();
    let (cells, steps) = (v.sheet().cells.clone(), v.undo.len());
    assert!(!v.structural_edit(StructOp::InsertRow));
    assert_eq!(
        v.entry_error.take().as_deref(),
        Some(gridcore::edit::SHIFT_OFF_SHEET)
    );
    assert_eq!(v.sheet().cells, cells);
    assert_eq!(v.undo.len(), steps);
}

#[test]
fn a_sheet_wide_insert_with_nothing_to_lose_still_runs() {
    let mut v = view();
    select(&mut v, "A2", "A2");
    v.select_rows();
    let steps = v.undo.len();
    assert!(v.structural_edit(StructOp::InsertCol));
    assert!(v.entry_error.is_none());
    assert_eq!(v.undo.len(), steps + 1);
}

#[test]
fn delete_col_on_whole_rows_deletes_every_column() {
    // Excel's Delete Sheet Columns on a whole row empties the sheet.
    let mut v = block();
    select(&mut v, "B2", "B2");
    v.select_rows();
    assert!(v.structural_edit(StructOp::DeleteCol));
    assert!(v.sheet().cells.values().all(Cell::is_blank));
    assert!(v.undo_step());
    assert_eq!(value(&v, "C3"), num(3));
}
