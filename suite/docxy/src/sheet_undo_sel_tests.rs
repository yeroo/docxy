//! Where Undo and Redo leave the sheet tab's selection (#1148), as Excel
//! 2024 does: both select what the step changes, wherever the selection is
//! now, with the active cell at its top-left. `uiharness/cases/
//! sheet-undo-selection.uit` drives the issue's repro through real keys.

use super::*;
use core::prelude::v1::test;
use gridcore::sheet::{Cell, CellValue, MAX_COLS, MAX_ROWS};

fn view() -> SheetView {
    let Surface::Sheet(v) = new_sheet_surface() else {
        panic!("a new sheet surface")
    };
    v
}

fn at(name: &str) -> (u32, u32) {
    gridcore::sheet::parse_cell_name(name).unwrap()
}

fn rect(name: &str) -> (u32, u32, u32, u32) {
    let (a, b) = name.split_once(':').unwrap_or((name, name));
    let (r0, c0) = at(a);
    let (r1, c1) = at(b);
    (r0, c0, r1, c1)
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

/// Select `from:to` with the active cell at `to`, as a drag from `from` does.
fn select(v: &mut SheetView, from: &str, to: &str) {
    v.anchor = at(from);
    v.sel = at(to);
    v.clear_areas();
}

/// Type `text` into the selected cell and press Enter.
fn type_enter(v: &mut SheetView, text: &str) {
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    for ch in text.chars() {
        v.edit_type(&ch.to_string());
    }
    assert_eq!(v.commit_and_move(1, 0), Some(true));
}

/// The selection as (active cell, other corner), with no other areas.
fn selected(v: &SheetView) -> ((u32, u32), (u32, u32)) {
    assert!(!v.multi_area(), "one area");
    (v.sel, v.anchor)
}

#[test]
fn undo_and_redo_of_an_entry_select_its_cell_from_anywhere() {
    // The issue's repro 2: Redo used to go back to D5.
    let mut v = view();
    select(&mut v, "A1", "A1");
    type_enter(&mut v, "one");
    select(&mut v, "A1", "A1");
    type_enter(&mut v, "two");
    select(&mut v, "D5", "D5");
    assert!(v.undo_step());
    assert_eq!(value(&v, "A1"), CellValue::Text("one".into()));
    assert_eq!(selected(&v), (at("A1"), at("A1")));
    select(&mut v, "F9", "F9");
    assert!(v.redo_step());
    assert_eq!(value(&v, "A1"), CellValue::Text("two".into()));
    assert_eq!(selected(&v), (at("A1"), at("A1")));
}

#[test]
fn an_entry_typed_in_a_range_undoes_to_its_cell() {
    let mut v = view();
    // B2:C3 selected, the active cell B2: the entry goes in B2 alone.
    select(&mut v, "C3", "B2");
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    v.edit_type("7");
    assert!(v.commit_edit());
    select(&mut v, "F9", "F9");
    assert!(v.undo_step());
    assert_eq!(selected(&v), (at("B2"), at("B2")));
}

#[test]
fn undo_and_redo_of_a_fill_select_its_range_active_cell_top_left() {
    let mut v = view();
    put(&mut v, "B1", Cell::number(5.0));
    // Dragged from B1 down: the active cell at the far corner, B4.
    select(&mut v, "B1", "B4");
    assert!(v.fill_selection(gridcore::edit::FillDir::Down));
    select(&mut v, "F9", "F9");
    assert!(v.undo_step());
    assert_eq!(value(&v, "B4"), CellValue::Empty);
    assert_eq!(selected(&v), (at("B1"), at("B4")));
    select(&mut v, "F9", "F9");
    assert!(v.redo_step());
    assert_eq!(value(&v, "B4"), CellValue::Number(5.0));
    assert_eq!(selected(&v), (at("B1"), at("B4")));
}

#[test]
fn undo_and_redo_of_a_paste_select_the_pasted_block() {
    let mut v = view();
    for (i, name) in ["A1", "A2", "A3"].into_iter().enumerate() {
        put(&mut v, name, Cell::number(i as f64 + 1.0));
    }
    select(&mut v, "A1", "A3");
    let clip = v.grid_clip(false).expect("something to copy");
    select(&mut v, "D1", "D1");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    select(&mut v, "F9", "F9");
    assert!(v.undo_step());
    assert_eq!(value(&v, "D3"), CellValue::Empty);
    assert_eq!(selected(&v), (at("D1"), at("D3")));
    select(&mut v, "F9", "F9");
    assert!(v.redo_step());
    assert_eq!(value(&v, "D3"), CellValue::Number(3.0));
    assert_eq!(selected(&v), (at("D1"), at("D3")));
}

#[test]
fn undo_and_redo_of_a_cut_pasted_select_where_it_landed() {
    let mut v = view();
    for name in ["A1", "A2", "A3"] {
        put(&mut v, name, Cell::number(1.0));
    }
    select(&mut v, "A1", "A3");
    let clip = v.grid_clip(true).expect("something to cut");
    select(&mut v, "D1", "D1");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    select(&mut v, "F9", "F9");
    assert!(v.undo_step());
    assert_eq!(value(&v, "A3"), CellValue::Number(1.0));
    assert_eq!(selected(&v), (at("D1"), at("D3")));
    select(&mut v, "F9", "F9");
    assert!(v.redo_step());
    assert_eq!(selected(&v), (at("D1"), at("D3")));
}

#[test]
fn undo_and_redo_of_an_inserted_row_select_the_whole_row() {
    let mut v = view();
    select(&mut v, "B2", "B2");
    assert!(v.structural_edit(StructOp::InsertRow));
    let row2 = ((1, 0), (1, MAX_COLS - 1));
    select(&mut v, "F9", "F9");
    assert!(v.undo_step());
    assert_eq!(selected(&v), row2);
    select(&mut v, "F9", "F9");
    assert!(v.redo_step());
    assert_eq!(selected(&v), row2);
}

#[test]
fn undo_and_redo_of_deleted_rows_and_columns_select_them_whole() {
    let mut v = view();
    select(&mut v, "A2", "A3");
    assert!(v.structural_edit(StructOp::DeleteRow));
    let rows = ((1, 0), (2, MAX_COLS - 1));
    select(&mut v, "F9", "F9");
    assert!(v.undo_step());
    assert_eq!(selected(&v), rows);
    select(&mut v, "F9", "F9");
    assert!(v.redo_step());
    assert_eq!(selected(&v), rows);

    let mut v = view();
    select(&mut v, "C1", "B1");
    assert!(v.structural_edit(StructOp::DeleteCol));
    let cols = ((0, 1), (MAX_ROWS - 1, 2));
    select(&mut v, "F9", "F9");
    assert!(v.undo_step());
    assert_eq!(selected(&v), cols);
    select(&mut v, "F9", "F9");
    assert!(v.redo_step());
    assert_eq!(selected(&v), cols);
}

#[test]
fn an_explicit_target_clears_the_other_areas() {
    let mut v = view();
    select(&mut v, "A1", "A1");
    v.add_area(at("C1"));
    assert!(v.multi_area());
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    v.edit_type("x");
    assert!(v.commit_edit());
    // Another multi-area selection, which Undo and Redo replace.
    select(&mut v, "F9", "F9");
    v.add_area(at("H9"));
    assert!(v.undo_step());
    assert_eq!(selected(&v), (at("C1"), at("C1")));
    select(&mut v, "F9", "F9");
    v.add_area(at("H9"));
    assert!(v.redo_step());
    assert_eq!(selected(&v), (at("C1"), at("C1")));
}

#[test]
fn a_step_over_a_multi_area_selection_selects_every_area() {
    let mut v = view();
    select(&mut v, "B5", "B6");
    v.add_area(at("D10"));
    v.begin_cell_edit(Some("=1".to_string()));
    assert!(v.commit_edit_to_selection());
    let all = vec![rect("B5:B6"), rect("D10")];
    select(&mut v, "F9", "F9");
    assert!(v.undo_step());
    assert_eq!(value(&v, "B6"), CellValue::Empty);
    assert_eq!(v.areas_all(), all);
    select(&mut v, "F9", "F9");
    assert!(v.redo_step());
    assert_eq!(v.areas_all(), all);
}

#[test]
fn a_format_undoes_to_its_range_active_cell_top_left() {
    let mut v = view();
    select(&mut v, "A1", "B3");
    v.push_undo();
    v.format_selection(&|x| x.bold = true);
    select(&mut v, "F9", "F9");
    assert!(v.undo_step());
    assert_eq!(selected(&v), (at("A1"), at("B3")));
}

#[test]
fn redo_of_an_added_sheet_goes_back_to_it() {
    let mut v = view();
    let idx = v.add_sheet("Two");
    assert_eq!(v.active, idx);
    assert!(v.undo_step());
    assert_eq!(v.active, 0);
    assert!(v.redo_step());
    assert_eq!(v.active, idx, "the new sheet, as before #1148");
}

#[test]
fn undo_and_redo_of_deleting_the_last_sheet_keep_a_valid_sheet() {
    let mut v = view();
    let idx = v.add_sheet("Two");
    select(&mut v, "C3", "C3");
    assert!(v.delete_sheet(idx));
    assert!(v.undo_step());
    assert_eq!(v.pkg.workbook.sheets.len(), 2);
    assert!(v.redo_step());
    assert_eq!(v.pkg.workbook.sheets.len(), 1);
    assert_eq!(v.active, 0);
}
