//! Multi-area selections (#670) on `SheetView` without a window: Ctrl+click
//! and Ctrl+drag add areas, Shift extends the active one, and the commands
//! that act on every area do. `uiharness/cases/sheet-selection.uit` drives
//! the same steps through clicks and keys.

use super::*;
use core::prelude::v1::test;
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

fn formula(v: &SheetView, name: &str) -> Option<String> {
    let (r, c) = at(name);
    v.sheet().cell(r, c).and_then(|c| c.formula.clone())
}

fn select(v: &mut SheetView, from: &str, to: &str) {
    v.anchor = at(from);
    v.sel = at(to);
    v.clear_areas();
}

fn areas(v: &SheetView) -> Vec<(u32, u32, u32, u32)> {
    v.areas_all()
}

#[test]
fn ctrl_click_adds_an_area_and_shift_extends_only_the_active_one() {
    let mut v = view();
    select(&mut v, "A1", "A1");
    v.add_area(at("C3"));
    assert_eq!(areas(&v), vec![rect("A1"), rect("C3")]);
    assert!(v.multi_area());
    // R5: Shift+click D5 grows C3 to C3:D5 and keeps A1.
    v.extend_active(at("D5"));
    assert_eq!(areas(&v), vec![rect("A1"), rect("C3:D5")]);
    // A Ctrl+drag: a new area, swept.
    v.add_area(at("F1"));
    v.extend_active(at("F2"));
    assert_eq!(areas(&v), vec![rect("A1"), rect("C3:D5"), rect("F1:F2")]);
}

#[test]
fn any_other_write_to_the_selection_leaves_one_area() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.0));
    put(&mut v, "C3", Cell::number(3.0));
    select(&mut v, "A1", "A1");
    v.add_area(at("C3"));
    // An arrow (move_sel) and a dialog's own write (as ttc_dialog does)
    // both move the selection without the area methods.
    v.move_sel(1, 0);
    assert_eq!(areas(&v), vec![rect("C4")]);
    let mut v2 = view();
    put(&mut v2, "A1", Cell::number(1.0));
    put(&mut v2, "C3", Cell::number(3.0));
    select(&mut v2, "A1", "A1");
    v2.add_area(at("C3"));
    v2.sel = at("C3");
    v2.anchor = at("B3");
    assert_eq!(areas(&v2), vec![rect("B3:C3")]);
    // Delete then clears only what is selected now.
    let changes = v2.clear_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].0, changes[0].1), at("C3"));
}

#[test]
fn delete_clears_every_area() {
    let mut v = view();
    for n in ["A1", "B1", "C3"] {
        put(&mut v, n, Cell::number(1.0));
    }
    select(&mut v, "A1", "A1");
    v.add_area(at("C3"));
    let changes = v.clear_changes();
    let cells: Vec<(u32, u32)> = changes.iter().map(|&(r, c, _)| (r, c)).collect();
    assert_eq!(cells, vec![at("A1"), at("C3")]);
}

#[test]
fn formatting_applies_to_every_area() {
    let mut v = view();
    select(&mut v, "A1", "A1");
    v.add_area(at("C3"));
    v.format_selection(&|xf| xf.bold = true);
    for n in ["A1", "C3"] {
        let (r, c) = at(n);
        let style = v.sheet().cell(r, c).unwrap().style;
        assert!(v.pkg.workbook.styles.xf(style).bold, "{n}");
    }
    assert!(v.sheet().cell(1, 1).is_none(), "B2 is between them");
}

#[test]
fn ctrl_enter_fills_every_area_relative_to_each_cell() {
    let mut v = view();
    select(&mut v, "B5", "B6");
    v.add_area(at("D10"));
    // The editor is on the active cell, D10.
    v.begin_cell_edit(Some("=D9".to_string()));
    assert!(v.commit_edit_to_selection());
    assert_eq!(formula(&v, "D10").as_deref(), Some("D9"));
    assert_eq!(formula(&v, "B5").as_deref(), Some("B4"));
    assert_eq!(formula(&v, "B6").as_deref(), Some("B5"));
    assert_eq!(v.undo.len(), 1, "one undo step");
    assert!(v.multi_area(), "the selection stays");
}

#[test]
fn ctrl_enter_from_a_single_cell_active_area() {
    // The criterion-30 shape: two areas, the active one a single cell.
    let mut v = view();
    select(&mut v, "C2", "C2");
    v.add_area(at("C6"));
    v.sel = at("C2");
    v.anchor = at("C2");
    v.areas = vec![rect("C6:C7")];
    v.stamp_areas();
    v.begin_cell_edit(Some("=C1".to_string()));
    assert!(v.commit_edit_to_selection());
    assert_eq!(formula(&v, "C2").as_deref(), Some("C1"));
    assert_eq!(formula(&v, "C6").as_deref(), Some("C5"));
    assert_eq!(formula(&v, "C7").as_deref(), Some("C6"));
}

#[test]
fn a_multi_area_copy_needs_shared_rows_or_columns() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.0));
    put(&mut v, "C1", Cell::number(3.0));
    select(&mut v, "A1", "A2");
    v.add_area(at("C1"));
    v.extend_active(at("C2"));
    let clip = v.grid_clip(false).expect("same rows: one block");
    assert_eq!(clip.cols, vec![0, 2]);
    assert_eq!(clip.rows, vec![0, 1]);
    assert_eq!(clip.text, "1\t3\n\t\n");
    // A cut of more than one area is refused, whatever its shape.
    assert_eq!(
        v.grid_clip(true).err(),
        Some(gridcore::edit::MULTI_SELECTION)
    );
    // Areas sharing neither rows nor columns are refused.
    select(&mut v, "A1", "A1");
    v.add_area(at("C3"));
    assert_eq!(
        v.grid_clip(false).err(),
        Some(gridcore::edit::MULTI_SELECTION)
    );
}

#[test]
fn a_multi_area_copy_pastes_as_one_block() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.0));
    put(&mut v, "C1", Cell::formula("B1"));
    put(&mut v, "B1", Cell::number(9.0));
    select(&mut v, "A1", "A1");
    v.add_area(at("C1"));
    let clip = v.grid_clip(false).unwrap();
    select(&mut v, "E1", "E1");
    v.paste_grid_clip(&clip).unwrap();
    assert_eq!(value(&v, "E1"), CellValue::Number(1.0));
    // C1 = B1 moved three columns lands in F1 and reads E1 (R2).
    assert_eq!(formula(&v, "F1").as_deref(), Some("E1"));
}

#[test]
fn undo_restores_the_areas() {
    let mut v = view();
    select(&mut v, "A1", "A1");
    v.add_area(at("C3"));
    v.push_undo();
    select(&mut v, "B2", "B2");
    put(&mut v, "B2", Cell::number(2.0));
    assert!(v.undo_step());
    assert_eq!(areas(&v), vec![rect("A1"), rect("C3")]);
}

#[test]
fn the_guard_sorts_every_command() {
    assert!(multi_area_ok(SheetAct::Bold));
    assert!(multi_area_ok(SheetAct::Copy));
    assert!(multi_area_ok(SheetAct::Clear(
        gridcore::edit::ClearWhat::All
    )));
    assert!(!multi_area_ok(SheetAct::Cut));
    assert!(!multi_area_ok(SheetAct::Paste));
    assert!(!multi_area_ok(SheetAct::SortAsc));
    assert!(!multi_area_ok(SheetAct::Fill(
        gridcore::edit::FillDir::Down
    )));
}

/// #707 r1 M3: Down then Up lands on the stamped cell again; the areas
/// must not come back with it (Delete then cleared A1 too).
#[test]
fn areas_once_dropped_never_come_back() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.0));
    put(&mut v, "C3", Cell::number(3.0));
    select(&mut v, "A1", "A1");
    v.add_area(at("C3"));
    v.move_sel(1, 0);
    v.move_sel(-1, 0);
    assert_eq!(areas(&v), vec![rect("C3")]);
    let changes = v.clear_changes();
    assert_eq!(changes.len(), 1, "only C3");
    // A writer that never clears them: the first read of the stale stamp
    // drops them for good, so a return to the stamped cell finds none.
    select(&mut v, "A1", "A1");
    v.add_area(at("C3"));
    v.sel = at("A5");
    v.anchor = at("A5");
    assert_eq!(areas(&v), vec![rect("A5")]);
    v.sel = at("C3");
    v.anchor = at("C3");
    assert_eq!(areas(&v), vec![rect("C3")]);
}
