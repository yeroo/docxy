//! A sheet's undo steps carry Excel's names, which the Quick Access Toolbar
//! Undo and Redo tips and drop-downs show (#1149), exercised on `SheetView`
//! without a window. The steps that start on `Docxy` (the Delete key's
//! Clear, Ctrl+V, Bold, Font) are driven through real keys and the ribbon by
//! `uiharness/cases/sheet-undo-names.uit`.

use super::*;
use core::prelude::v1::test;
use gridcore::sheet::{Cell, CellValue};

fn view() -> SheetView {
    let Surface::Sheet(v) = new_sheet_surface() else {
        panic!("a new sheet surface")
    };
    v
}

fn select(v: &mut SheetView, (r0, c0): (u32, u32), (r1, c1): (u32, u32)) {
    v.anchor = (r0, c0);
    v.sel = (r1, c1);
}

/// Type `text` into cell `(r, c)` and commit it, as Enter does.
fn type_at(v: &mut SheetView, (r, c): (u32, u32), text: &str) {
    select(v, (r, c), (r, c));
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    for ch in text.chars() {
        v.edit_type(&ch.to_string());
    }
    assert!(v.commit_edit(), "{text}");
}

fn value(v: &SheetView, r: u32, c: u32) -> CellValue {
    v.sheet()
        .cell(r, c)
        .map(|c| c.value.clone())
        .unwrap_or_default()
}

fn by_column(key: u32) -> [gridcore::edit::SortLevel; 1] {
    [gridcore::edit::SortLevel {
        key,
        on: gridcore::edit::SortOn::Value {
            asc: true,
            list: None,
        },
    }]
}

#[test]
fn sheet_undo_steps_are_named_1149() {
    let mut v = view();
    // E1:E3 to sort, written before the history starts.
    for (r, n) in [3.0, 1.0, 2.0].into_iter().enumerate() {
        v.engine
            .set_cell(&mut v.pkg.workbook, (0, r as u32, 4), Cell::number(n));
    }
    type_at(&mut v, (0, 0), "abc");
    // Ctrl+D down A1:A3.
    select(&mut v, (0, 0), (2, 0));
    assert!(v.fill_selection(gridcore::edit::FillDir::Down));
    // A paste's core, into C1.
    select(&mut v, (0, 2), (0, 2));
    v.write_block((0, 2), &[vec![Cell::number(3.0)]], "Paste")
        .unwrap();
    // Home › Clear › Clear All on C1.
    assert_eq!(v.clear_what(gridcore::edit::ClearWhat::All), Ok(true));
    // Insert and delete sheet rows on row 2.
    select(&mut v, (1, 0), (1, 0));
    assert!(v.structural_edit(StructOp::InsertRow));
    assert!(v.structural_edit(StructOp::DeleteRow));
    // Sort E1:E3.
    crate::sheet_sort::sort_view(&mut v, (0, 4, 2, 4), &by_column(4), &Default::default()).unwrap();
    assert_eq!(value(&v, 0, 4), CellValue::Number(1.0));
    // Excel's names, newest first.
    assert_eq!(
        v.undo_names(),
        [
            "Sort",
            "Delete",
            "Insert Cells",
            "Clear",
            "Paste",
            "Fill",
            "Typing 'abc' in A1",
        ]
    );
    assert!(v.redo_names().is_empty());
}

#[test]
fn ctrl_enter_names_the_range_it_typed_into_1149() {
    let mut v = view();
    select(&mut v, (0, 0), (2, 1));
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    v.edit_type("7");
    assert!(v.commit_edit_to_selection());
    assert_eq!(v.undo_names(), ["Typing '7' in A1:B3"]);
}

#[test]
fn ctrl_enter_names_every_area_it_typed_into_1149() {
    let mut v = view();
    // A1:A3, then C5 added with Ctrl+click: C5 is the active area.
    select(&mut v, (0, 0), (2, 0));
    v.add_area((4, 2));
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    v.edit_type("x");
    assert!(v.commit_edit_to_selection());
    assert_eq!(value(&v, 1, 0), CellValue::Text("x".into()));
    assert_eq!(value(&v, 4, 2), CellValue::Text("x".into()));
    assert_eq!(v.undo_names(), ["Typing 'x' in A1:A3,C5"]);
}

#[test]
fn a_tables_autocorrect_step_is_named_for_what_it_did_1149() {
    let mut v = view();
    for (c, h) in ["Qty", "Price"].iter().enumerate() {
        v.engine
            .set_cell(&mut v.pkg.workbook, (0, 0, c as u32), Cell::text(h));
    }
    v.engine
        .set_cell(&mut v.pkg.workbook, (0, 1, 0), Cell::number(2.0));
    v.pkg
        .add_table(0, (0, 0, 1, 1), true, "TableStyleMedium2")
        .unwrap();
    v.rebuild_engine();
    // Typed under the table: it grows to take the row.
    type_at(&mut v, (2, 0), "5");
    assert_eq!(v.undo_names(), ["Table AutoExpansion", "Typing '5' in A3"]);
}

#[test]
fn sheet_redo_keeps_the_step_name_1149() {
    let mut v = view();
    type_at(&mut v, (0, 0), "a");
    type_at(&mut v, (1, 0), "b");
    type_at(&mut v, (2, 0), "c");
    assert!(v.undo_step());
    assert!(v.undo_step());
    // The next one Redo redoes comes first.
    assert_eq!(v.redo_names(), ["Typing 'b' in A2", "Typing 'c' in A3"]);
    assert_eq!(v.undo_names(), ["Typing 'a' in A1"]);
    assert!(v.redo_step());
    assert_eq!(v.redo_names(), ["Typing 'c' in A3"]);
    assert_eq!(v.undo_names(), ["Typing 'b' in A2", "Typing 'a' in A1"]);
    // An undo then a redo leaves both lists as they were.
    assert!(v.undo_step());
    assert!(v.redo_step());
    assert_eq!(v.redo_names(), ["Typing 'c' in A3"]);
    assert_eq!(v.undo_names(), ["Typing 'b' in A2", "Typing 'a' in A1"]);
}

#[test]
fn sheet_undo_to_undoes_k_steps_1149() {
    let mut v = view();
    for (r, text) in ["1", "2", "3", "4"].iter().enumerate() {
        type_at(&mut v, (r as u32, 0), text);
    }
    // The Undo list's third entry: back to before A2's entry.
    assert!(v.undo_steps(3));
    assert_eq!(value(&v, 0, 0), CellValue::Number(1.0));
    for r in 1..4 {
        assert_eq!(value(&v, r, 0), CellValue::Empty, "row {r}");
    }
    assert_eq!(v.undo_names(), ["Typing '1' in A1"]);
    // The Redo list's second entry: A2 and A3 come back, A4 waits.
    assert!(v.redo_steps(2));
    assert_eq!(value(&v, 2, 0), CellValue::Number(3.0));
    assert_eq!(value(&v, 3, 0), CellValue::Empty);
    assert_eq!(v.redo_names(), ["Typing '4' in A4"]);
    // More than there are stops at the end; none at all changes nothing.
    assert!(v.redo_steps(5));
    assert!(v.redo_names().is_empty());
    assert!(!v.redo_steps(1));
    assert!(v.undo_steps(10));
    assert!(!v.undo_steps(1));
}

#[test]
fn repeat_format_names_its_step_1149() {
    let mut v = view();
    let bold = v.toggle_setter(|x| x.bold, |x, on| x.bold = on);
    v.push_undo(SheetStep::Named("Bold"));
    v.apply_format(bold, SheetStep::Named("Bold"));
    select(&mut v, (0, 3), (0, 3));
    // F4 on D1 repeats it as a step of the same name.
    assert!(v.repeat_format());
    assert_eq!(v.undo_names(), ["Bold", "Bold"]);
}

#[test]
fn sheet_qat_reads_named_steps_1149() {
    let read = |undo: Option<&str>, redo: Option<&str>, repeat: Option<&str>| {
        qat_entries(QatState::Sheet {
            undo: undo.map(String::from),
            redo: redo.map(String::from),
            repeat: repeat.map(String::from),
        })
        .into_iter()
        .map(|e| (e.label, e.tip.to_string(), e.enabled, e.menu))
        .collect::<Vec<_>>()
    };
    let row =
        |label: &'static str, tip: &str, enabled: bool| (label, tip.to_string(), enabled, true);
    // The issue's: a typed entry to undo, nothing to redo.
    assert_eq!(
        read(Some("Typing 'abc' in A1"), None, None),
        [
            row("Undo", "Undo Typing 'abc' in A1 (Ctrl+Z)", true),
            row("Redo", "Can't Redo (Ctrl+Y)", false),
        ]
    );
    // An empty history.
    assert_eq!(
        read(None, None, None)[0],
        row("Undo", "Can't Undo (Ctrl+Z)", false)
    );
    // A redo waiting outranks a repeat.
    assert_eq!(
        read(Some("Typing 'abc' in A1"), Some("Fill"), Some("Bold"))[1],
        row("Redo", "Redo Fill (Ctrl+Y)", true)
    );
    // Nothing to redo, a format to repeat: Repeat, as the Word tab's (#618).
    assert_eq!(
        read(Some("Bold"), None, Some("Bold"))[1],
        row("Repeat", "Repeat Bold (Ctrl+Y)", true)
    );
}
