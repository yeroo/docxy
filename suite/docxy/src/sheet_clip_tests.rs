//! The sheet clipboard (#664): a copy tiles over its paste area with its
//! formulas translated, a cut moves its cells (and the references to them),
//! once, and a copy of a filtered range leaves out the rows the filter
//! hides. Exercised on `SheetView` without a window; the key routing (Enter
//! pastes, Esc ends copy mode) is in `sheet_paste`/`sheet_key`, and
//! `uiharness/cases/clipboard.uit` drives the same steps through keystrokes.

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

/// Select `from` (the anchor) through `to` (the active cell).
fn select(v: &mut SheetView, from: &str, to: &str) {
    v.anchor = at(from);
    v.sel = at(to);
}

/// The issue's book: A1 1, A2 2, B1 `=A1*10`, B2 `=A2*10`, D1:D6 `old`,
/// H1 `=A1+B1`.
fn issue_book() -> SheetView {
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.0));
    put(&mut v, "A2", Cell::number(2.0));
    put(&mut v, "B1", Cell::formula("A1*10"));
    put(&mut v, "B2", Cell::formula("A2*10"));
    for r in 1..=6 {
        put(&mut v, &format!("D{r}"), Cell::text("old"));
    }
    put(&mut v, "H1", Cell::formula("A1+B1"));
    assert_eq!(value(&v, "H1"), CellValue::Number(11.0));
    v
}

fn copy(v: &mut SheetView, from: &str, to: &str) -> GridClip {
    select(v, from, to);
    v.grid_clip(false).expect("something to copy")
}

fn cut(v: &mut SheetView, from: &str, to: &str) -> GridClip {
    select(v, from, to);
    v.grid_clip(true).expect("something to cut")
}

// ---- 1. a copy tiles over a paste area of whole copies ---------------------

#[test]
fn a_copy_tiles_over_a_paste_area_of_whole_copies() {
    // Made top-down and bottom-up: the paste goes at the top-left either way.
    for (from, to) in [("D1", "D6"), ("D6", "D1")] {
        let mut v = issue_book();
        let clip = copy(&mut v, "B1", "B2");
        select(&mut v, from, to);
        assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
        for r in 1..=6 {
            assert_eq!(
                formula(&v, &format!("D{r}")),
                Some(format!("C{r}*10")),
                "D{r}, selected {from}:{to}"
            );
        }
        assert_eq!(v.sheet().cell(6, 3), None, "D7 untouched");
        assert_eq!((v.sel, v.anchor), (at("D1"), at("D6")));
        assert_eq!(v.undo.len(), 1, "one undo step");
    }
}

#[test]
fn a_paste_area_that_is_not_whole_copies_is_refused() {
    let mut v = issue_book();
    let clip = copy(&mut v, "B1", "B2");
    let before = v.sheet().cells.clone();
    select(&mut v, "D1", "D3");
    assert_eq!(
        v.paste_grid_clip(&clip),
        Err(GridPasteError::Refused(gridcore::edit::PASTE_SHAPE.into()))
    );
    assert_eq!(v.sheet().cells, before);
    assert!(v.undo.is_empty());
}

#[test]
fn a_tiled_paste_over_part_of_an_array_is_refused_whole() {
    let mut v = view();
    let s = v.active;
    let mut d1 = Cell::formula("A1:A3*2");
    d1.f_attrs = Some(" t=\"array\" ref=\"D1:D3\"".into());
    v.pkg.workbook.sheets[s].set_cell(0, 3, d1);
    put(&mut v, "F1", Cell::number(7.0));
    v.engine = sheet_engine(&v.pkg.workbook);
    v.engine.recalc_all(&mut v.pkg.workbook);
    let clip = copy(&mut v, "F1", "F1");
    let before = v.sheet().cells.clone();
    // D2:D3 is part of the block (its anchor is D1).
    select(&mut v, "D2", "D3");
    assert_eq!(
        v.paste_grid_clip(&clip),
        Err(GridPasteError::Refused(
            gridcore::engine::PART_OF_ARRAY.into()
        ))
    );
    assert_eq!(v.sheet().cells, before);
}

#[test]
fn a_whole_column_paste_area_fills_the_used_rows() {
    let mut v = issue_book();
    let clip = copy(&mut v, "B1", "B2");
    // Column D selected whole: the used rows are 1..6 (D1:D6, H1).
    v.anchor = (0, 3);
    v.sel = (gridcore::sheet::MAX_ROWS - 1, 3);
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    assert_eq!(formula(&v, "D6").as_deref(), Some("C6*10"));
    assert_eq!(v.sheet().cell(6, 3), None);
    assert_eq!((v.sel, v.anchor), (at("D1"), at("D6")));
}

#[test]
fn a_copy_bigger_than_the_cap_pasted_once_lands() {
    // The cap stops a tiling, not a copy pasted once: 50,001 rows x 2
    // columns is 100,002 cells, written as they were copied.
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.0));
    put(&mut v, "B50001", Cell::formula("A1+1"));
    let clip = copy(&mut v, "A1", "B50001");
    assert!(clip.cells.len() as u64 * 2 > gridcore::edit::MAX_PASTE_CELLS);
    select(&mut v, "D1", "D1");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    assert_eq!(value(&v, "D1"), CellValue::Number(1.0));
    assert_eq!(formula(&v, "E50001").as_deref(), Some("D1+1"));
    assert_eq!((v.sel, v.anchor), (at("D1"), at("E50001")));
}

#[test]
fn a_paste_of_more_than_the_cap_is_refused() {
    let mut v = issue_book();
    let clip = copy(&mut v, "B1", "B2");
    // 200,000 rows of a 2-row copy: 100,000 tiles, 200,000 cells.
    v.anchor = (0, 3);
    v.sel = (199_999, 3);
    assert!(matches!(
        v.paste_grid_clip(&clip),
        Err(GridPasteError::Refused(_))
    ));
    assert!(v.undo.is_empty());
}

// ---- 2. pasted formulas translate ------------------------------------------

#[test]
fn a_pasted_copy_translates_its_formulas() {
    let mut v = issue_book();
    let clip = copy(&mut v, "B1", "B2");
    select(&mut v, "E1", "E1");
    v.paste_grid_clip(&clip).unwrap();
    assert_eq!(formula(&v, "E1").as_deref(), Some("D1*10"));
    assert_eq!(formula(&v, "E2").as_deref(), Some("D2*10"));
    // A single cell, with `$` parts kept.
    put(&mut v, "J1", Cell::formula("$A1+A$1"));
    let clip = copy(&mut v, "J1", "J1");
    select(&mut v, "K3", "K3");
    v.paste_grid_clip(&clip).unwrap();
    assert_eq!(formula(&v, "K3").as_deref(), Some("$A3+B$1"));
}

#[test]
fn a_copy_pasted_where_it_came_from_keeps_its_text() {
    let mut v = view();
    put(&mut v, "B1", Cell::formula("sum( a1 , 2 )"));
    let clip = copy(&mut v, "B1", "B1");
    v.paste_grid_clip(&clip).unwrap();
    assert_eq!(formula(&v, "B1").as_deref(), Some("sum( a1 , 2 )"));
}

#[test]
fn a_copy_pastes_again_and_again() {
    let mut v = issue_book();
    let clip = copy(&mut v, "B1", "B2");
    select(&mut v, "E1", "E1");
    v.paste_grid_clip(&clip).unwrap();
    select(&mut v, "F1", "F1");
    v.paste_grid_clip(&clip).unwrap();
    assert_eq!(formula(&v, "F2").as_deref(), Some("E2*10"));
}

// ---- 3. copy mode ends: an Enter paste, a pasted cut, Esc ------------------

#[test]
fn a_spent_clip_pastes_nothing_while_the_clipboard_holds_its_text() {
    let mut v = issue_book();
    let mut clip = copy(&mut v, "B1", "B2");
    let ours = ClipRead::Text(clip.text.clone());
    assert!(clip.live(&ours));
    clip.spend();
    assert!(!clip.live(&ours));
    assert!(clip.spent_here(&ours), "the text paste leaves it alone too");
    // Another app's copy since pastes as text, as before.
    let other = ClipRead::Text("from another app".into());
    assert!(!clip.live(&other));
    assert!(!clip.spent_here(&other));
}

// ---- 4. a cut moves its cells, and the references to them -----------------

#[test]
fn a_cut_marks_its_cells_and_its_paste_moves_them_and_their_references() {
    let mut v = issue_book();
    let clip = cut(&mut v, "A1", "B1");
    assert!(clip.cut);
    assert_eq!(
        value(&v, "A1"),
        CellValue::Number(1.0),
        "Ctrl+X clears nothing"
    );
    assert!(v.undo.is_empty());
    select(&mut v, "A10", "A10");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    assert_eq!(value(&v, "A10"), CellValue::Number(1.0));
    assert_eq!(formula(&v, "B10").as_deref(), Some("A10*10"));
    assert_eq!(value(&v, "B10"), CellValue::Number(10.0));
    assert_eq!(v.sheet().cell(0, 0), None);
    assert_eq!(v.sheet().cell(0, 1), None);
    assert_eq!(formula(&v, "H1").as_deref(), Some("A10+B10"));
    assert_eq!(value(&v, "H1"), CellValue::Number(11.0));
    assert_eq!((v.sel, v.anchor), (at("A10"), at("B10")));

    // One undo step brings it all back.
    assert_eq!(v.undo.len(), 1);
    assert!(v.undo_step());
    assert_eq!(value(&v, "A1"), CellValue::Number(1.0));
    assert_eq!(formula(&v, "B1").as_deref(), Some("A1*10"));
    assert_eq!(formula(&v, "H1").as_deref(), Some("A1+B1"));
    assert_eq!(value(&v, "H1"), CellValue::Number(11.0));
    assert_eq!(v.sheet().cell(9, 0), None);
    assert_eq!(v.sheet().cell(9, 1), None);
}

#[test]
fn a_cut_over_its_own_cells_keeps_what_it_overwrites() {
    let mut v = issue_book();
    // A1:B1 moved one column right: B1 is both a source and a target.
    let clip = cut(&mut v, "A1", "B1");
    select(&mut v, "B1", "B1");
    v.paste_grid_clip(&clip).unwrap();
    assert_eq!(v.sheet().cell(0, 0), None);
    assert_eq!(value(&v, "B1"), CellValue::Number(1.0));
    assert_eq!(formula(&v, "C1").as_deref(), Some("B1*10"));
    assert_eq!(formula(&v, "H1").as_deref(), Some("B1+C1"));
    assert_eq!(value(&v, "H1"), CellValue::Number(11.0));
}

#[test]
fn a_cut_pastes_into_one_cell_or_its_own_shape_only() {
    let mut v = issue_book();
    let clip = cut(&mut v, "A1", "B1");
    select(&mut v, "A10", "D10");
    assert_eq!(
        v.paste_grid_clip(&clip),
        Err(GridPasteError::Refused(gridcore::edit::PASTE_SHAPE.into()))
    );
    select(&mut v, "A10", "B10");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
}

#[test]
fn a_cut_moved_to_another_sheet_keeps_reading_what_it_read() {
    let mut v = issue_book();
    put(&mut v, "C1", Cell::number(5.0));
    put(&mut v, "B1", Cell::formula("A1*10+C1"));
    let s2 = v.add_sheet("Sheet2");
    v.active = 0;
    let clip = cut(&mut v, "A1", "B1");
    // Switching sheets is not an edit.
    v.active = s2;
    select(&mut v, "A10", "A10");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    let sheet2 = |r, c| v.pkg.workbook.sheets[s2].cell(r, c).cloned();
    assert_eq!(
        sheet2(9, 1).and_then(|c| c.formula).as_deref(),
        Some("A10*10+Sheet1!C1")
    );
    assert_eq!(sheet2(9, 1).map(|c| c.value), Some(CellValue::Number(15.0)));
    let sheet1 = &v.pkg.workbook.sheets[0];
    assert_eq!(sheet1.cell(0, 0), None);
    assert_eq!(
        sheet1.cell(0, 7).and_then(|c| c.formula.clone()).as_deref(),
        Some("Sheet2!A10+Sheet2!B10")
    );
    assert_eq!(
        sheet1.cell(0, 7).map(|c| c.value.clone()),
        Some(CellValue::Number(16.0))
    );
}

#[test]
fn an_edit_after_a_cut_cancels_it() {
    let mut v = issue_book();
    let clip = cut(&mut v, "A1", "B1");
    // Type 5 into A1.
    select(&mut v, "A1", "A1");
    v.begin_cell_edit(Some("5".into()));
    assert_eq!(v.commit_and_move(0, 0), Some(true));
    let before = v.sheet().cells.clone();
    select(&mut v, "A10", "A10");
    assert_eq!(v.paste_grid_clip(&clip), Err(GridPasteError::CutCancelled));
    assert_eq!(v.sheet().cells, before);
    assert_eq!(value(&v, "A1"), CellValue::Number(5.0));
}

#[test]
fn a_row_inserted_after_a_cut_cancels_it() {
    let mut v = issue_book();
    let clip = cut(&mut v, "A1", "B1");
    select(&mut v, "A1", "A1");
    assert!(v.structural_edit(StructOp::InsertRow));
    let before = v.sheet().cells.clone();
    select(&mut v, "A10", "A10");
    assert_eq!(v.paste_grid_clip(&clip), Err(GridPasteError::CutCancelled));
    assert_eq!(v.sheet().cells, before);
}

#[test]
fn a_cut_whose_cells_changed_without_an_undo_step_is_cancelled() {
    let mut v = issue_book();
    let clip = cut(&mut v, "A1", "B1");
    // A path that writes without an undo step still cancels it.
    v.pkg.workbook.sheets[0].set_cell(0, 1, Cell::formula("A1*20"));
    select(&mut v, "A10", "A10");
    assert_eq!(v.paste_grid_clip(&clip), Err(GridPasteError::CutCancelled));
}

#[test]
fn a_recalculated_value_does_not_cancel_a_cut() {
    let mut v = issue_book();
    let clip = cut(&mut v, "B1", "B1");
    // B1's cached value changes (a recalc), its formula doesn't.
    v.pkg.workbook.sheets[0]
        .cells
        .get_mut(&(0, 1))
        .unwrap()
        .value = CellValue::Number(99.0);
    select(&mut v, "B10", "B10");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
}

#[test]
fn a_cut_pasted_in_another_workbook_is_a_copy() {
    let mut src = issue_book();
    let clip = cut(&mut src, "A1", "B1");
    let mut dst = view();
    select(&mut dst, "A10", "A10");
    assert!(matches!(
        dst.paste_grid_clip(&clip),
        Ok(GridPasted::KeptAsCopy(_))
    ));
    assert_eq!(formula(&dst, "B10").as_deref(), Some("A10*10"));
    assert_eq!(
        value(&src, "A1"),
        CellValue::Number(1.0),
        "the source stays"
    );
}

#[test]
fn a_cut_off_a_protected_sheet_is_a_copy() {
    let mut v = issue_book();
    // Cut off a sheet that is already protected: the move would edit a
    // locked sheet. (Protecting it after the cut is an edit, which cancels
    // the cut instead.)
    v.pkg.workbook.sheets[0].set_protected(true);
    let clip = cut(&mut v, "A1", "B1");
    select(&mut v, "A10", "A10");
    assert!(matches!(
        v.paste_grid_clip(&clip),
        Ok(GridPasted::KeptAsCopy(_))
    ));
    assert_eq!(value(&v, "A1"), CellValue::Number(1.0));
    assert_eq!(formula(&v, "B10").as_deref(), Some("A10*10"));
}

// ---- 5. a filtered copy leaves out the rows the filter hides ---------------

/// A1:B6 = 1..6 and `=A<r>*10`, with rows 3 and 5 hidden by a filter.
fn filtered_book() -> SheetView {
    let mut v = view();
    for r in 1..=6 {
        put(&mut v, &format!("A{r}"), Cell::number(f64::from(r)));
        put(&mut v, &format!("B{r}"), Cell::formula(&format!("A{r}*10")));
    }
    let s = v.active;
    v.pkg.workbook.sheets[s].set_row_filtered(2, true);
    v.pkg.workbook.sheets[s].set_row_filtered(4, true);
    v
}

#[test]
fn a_filtered_copy_takes_only_the_rows_the_filter_shows() {
    let mut v = filtered_book();
    let clip = copy(&mut v, "A1", "B6");
    assert_eq!(clip.rows, [0, 1, 3, 5]);
    assert_eq!(clip.text, "1\t10\n2\t20\n4\t40\n6\t60\n");
    select(&mut v, "D1", "D1");
    v.paste_grid_clip(&clip).unwrap();
    // D1:E4, with source row 4 landing on row 3 translated by its own offset.
    assert_eq!(value(&v, "D3"), CellValue::Number(4.0));
    assert_eq!(formula(&v, "E3").as_deref(), Some("D3*10"));
    assert_eq!(value(&v, "E3"), CellValue::Number(40.0));
    assert_eq!(formula(&v, "E4").as_deref(), Some("D4*10"));
    assert_eq!(v.sheet().cell(4, 3), None, "four rows, not six");
    assert_eq!((v.sel, v.anchor), (at("D1"), at("E4")));
}

#[test]
fn a_row_hidden_by_hide_is_copied() {
    let mut v = view();
    for r in 1..=3 {
        put(&mut v, &format!("A{r}"), Cell::number(f64::from(r)));
    }
    let s = v.active;
    v.pkg.workbook.sheets[s].set_row_hidden(1, true);
    let clip = copy(&mut v, "A1", "A3");
    assert_eq!(clip.rows, [0, 1, 2]);
    assert_eq!(clip.text, "1\n2\n3\n");
}

#[test]
fn a_cut_of_a_filtered_range_moves_every_row() {
    let mut v = filtered_book();
    let clip = cut(&mut v, "A1", "B6");
    assert_eq!(clip.rows, [0, 1, 2, 3, 4, 5]);
}

// ---- r1 review: the grid's edge, edits ending copy mode, empty copies -------

#[test]
fn a_cut_that_would_run_past_the_grids_edge_is_refused_and_stays_live() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.0));
    put(&mut v, "A2", Cell::number(2.0));
    put(&mut v, "H1", Cell::formula("A2"));
    let clip = cut(&mut v, "A1", "A2");
    let before = v.sheet().cells.clone();
    // The last row: A2 would land one row past it.
    let last = gridcore::sheet::MAX_ROWS - 1;
    v.sel = (last, 0);
    v.anchor = (last, 0);
    assert_eq!(
        v.paste_grid_clip(&clip),
        Err(GridPasteError::Refused(PAST_THE_EDGE.into()))
    );
    assert_eq!(v.sheet().cells, before);
    assert!(v.undo.is_empty());
    // Past the last column too.
    let edge = gridcore::sheet::MAX_COLS - 1;
    let clip_wide = cut(&mut v, "A1", "B1");
    v.sel = (0, edge);
    v.anchor = (0, edge);
    assert_eq!(
        v.paste_grid_clip(&clip_wide),
        Err(GridPasteError::Refused(PAST_THE_EDGE.into()))
    );
    // Still live: it moves to A10.
    select(&mut v, "A10", "A10");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    assert_eq!(formula(&v, "H1").as_deref(), Some("A11"));
    assert_eq!(value(&v, "H1"), CellValue::Number(2.0));
}

#[test]
fn an_edit_in_its_own_workbook_ends_copy_mode() {
    let mut v = issue_book();
    let clip = copy(&mut v, "A1", "A3");
    assert!(!clip.stale_in(&v));
    // Click C1, type 5, Enter: Enter would now move, not paste.
    select(&mut v, "C1", "C1");
    v.begin_cell_edit(Some("5".into()));
    assert_eq!(v.commit_and_move(1, 0), Some(true));
    assert!(clip.stale_in(&v));
    // Another workbook's edits don't count: its edit_gen differs from the
    // clip's, so only the workbook id tells them apart.
    let mut other = view();
    put(&mut other, "A1", Cell::number(1.0));
    for _ in 0..3 {
        other.push_undo();
    }
    let fresh = copy(&mut v, "A1", "A3");
    assert_ne!(other.edit_gen, fresh.view_gen);
    assert!(!fresh.stale_in(&other));
    assert!(!fresh.stale_in(&v));
}

#[test]
fn a_copy_pasted_into_its_own_workbook_stays_in_copy_mode() {
    let mut v = issue_book();
    let mut clip = copy(&mut v, "B1", "B2");
    select(&mut v, "E1", "E1");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    // The paste's own undo step would read as an edit; the restamp says not.
    assert!(clip.stale_in(&v));
    clip.restamp(&v);
    assert!(!clip.stale_in(&v));
    select(&mut v, "F1", "F1");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    assert_eq!(formula(&v, "F1").as_deref(), Some("E1*10"));
    // A paste into another workbook leaves the stamp alone.
    let mut other = view();
    let gen_before = clip.view_gen;
    for _ in 0..3 {
        other.push_undo();
    }
    assert_ne!(other.edit_gen, gen_before);
    clip.restamp(&other);
    assert_eq!(clip.view_gen, gen_before);
}

#[test]
fn an_edit_after_a_cut_makes_it_stale_for_enter() {
    let mut v = issue_book();
    let clip = cut(&mut v, "A1", "B1");
    select(&mut v, "C1", "C1");
    v.begin_cell_edit(Some("5".into()));
    assert_eq!(v.commit_and_move(1, 0), Some(true));
    assert!(clip.stale_in(&v), "Enter then moves; Ctrl+V pastes nothing");
}

#[test]
fn a_copy_of_only_filtered_out_rows_is_nothing_to_copy() {
    let mut v = filtered_book();
    select(&mut v, "A3", "B3");
    assert!(v.grid_clip(false).is_none());
    // A cut of the same rows still moves them.
    assert!(v.grid_clip(true).is_some());
}
