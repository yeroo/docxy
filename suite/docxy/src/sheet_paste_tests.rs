//! Paste Special, Paste Options and the Office Clipboard (#669), on
//! `SheetView` without a window. `uiharness/cases/clipboard.uit` drives the
//! same steps through the gallery, the dialog and the pane.

use super::*;
use crate::{GridPasted, Surface, new_sheet_surface};
use gridcore::sheet::{CellValue, parse_cell_name};

fn view() -> SheetView {
    let Surface::Sheet(v) = new_sheet_surface() else {
        panic!("a new sheet surface")
    };
    v
}

fn at(name: &str) -> (u32, u32) {
    parse_cell_name(name).unwrap()
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

fn copy(v: &mut SheetView, from: &str, to: &str) -> crate::GridClip {
    v.anchor = at(from);
    v.sel = at(to);
    v.clear_areas();
    v.grid_clip(false).expect("something to copy")
}

fn select(v: &mut SheetView, name: &str) {
    v.anchor = at(name);
    v.sel = at(name);
    v.clear_areas();
}

#[test]
fn paste_values_and_transpose_are_one_step_each() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(2.0));
    put(&mut v, "A2", Cell::formula("A1*3"));
    let clip = copy(&mut v, "A1", "A2");
    select(&mut v, "C1");
    let rect = v
        .paste_special_at(&clip.block, &PasteSpec::of(PasteWhat::Values), at("C1"))
        .unwrap();
    assert_eq!(rect, (0, 2, 1, 2));
    assert_eq!(value(&v, "C2"), CellValue::Number(6.0));
    assert_eq!(formula(&v, "C2"), None, "a value, not the formula");
    assert_eq!(v.undo.len(), 1);
    assert_eq!(v.range(), rect, "the pasted range is selected");
    // Transposed: A1:A2 lands across E1:F1, A2's =A1*3 reading E1.
    let spec = PasteSpec {
        transpose: true,
        ..PasteSpec::default()
    };
    v.paste_special_at(&clip.block, &spec, at("E1")).unwrap();
    assert_eq!(formula(&v, "F1").as_deref(), Some("E1*3"));
    assert_eq!(value(&v, "F1"), CellValue::Number(6.0));
    assert_eq!(v.undo.len(), 2);
}

#[test]
fn an_operation_combines_with_the_destination() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.05));
    put(&mut v, "B9", Cell::number(4.0));
    put(&mut v, "C9", Cell::formula("B9*2"));
    let clip = copy(&mut v, "A1", "A1");
    let spec = PasteSpec {
        op: PasteOp::Multiply,
        ..PasteSpec::default()
    };
    v.paste_special_at(&clip.block, &spec, at("C9")).unwrap();
    assert_eq!(formula(&v, "C9").as_deref(), Some("(B9*2)*1.05"));
    assert_eq!(value(&v, "C9"), CellValue::Number(8.4));
}

#[test]
fn notes_paste_through_the_package_and_undo_takes_them_back() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.0));
    assert!(v.pkg.set_comment(0, 0, 0, "Ann", "a note"));
    let clip = copy(&mut v, "A1", "A1");
    assert_eq!(clip.block.notes.len(), 1);
    v.paste_special_at(&clip.block, &PasteSpec::of(PasteWhat::Comments), at("B3"))
        .unwrap();
    assert!(v.note_cells().contains(&(2, 1)));
    assert_eq!(value(&v, "B3"), CellValue::Empty, "notes only");
    assert!(v.undo_step());
    assert!(!v.note_cells().contains(&(2, 1)));
}

#[test]
fn validation_pastes_as_a_rule_over_the_destination() {
    let mut v = view();
    put(&mut v, "A1", Cell::text("Yes"));
    assert!(
        v.pkg
            .add_data_validation(0, (0, 0, 0, 0), "list", "", "\"Yes,No\"", None)
    );
    let clip = copy(&mut v, "A1", "A1");
    v.paste_special_at(&clip.block, &PasteSpec::of(PasteWhat::Validation), at("D4"))
        .unwrap();
    assert!(v.sheet().validations.iter().any(|dv| dv.covers(3, 3)));
    assert_eq!(value(&v, "D4"), CellValue::Empty);
}

#[test]
fn paste_link_reads_the_copy() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(7.0));
    let clip = copy(&mut v, "A1", "A1");
    v.paste_link_at(&clip.block, at("C1")).unwrap();
    assert_eq!(formula(&v, "C1").as_deref(), Some("$A$1"));
    assert_eq!(value(&v, "C1"), CellValue::Number(7.0));
}

#[test]
fn paste_options_repaste_in_place_as_one_step() {
    let mut v = view();
    put(&mut v, "A1", Cell::number(2.0));
    put(&mut v, "B1", Cell::formula("A1*10"));
    let clip = copy(&mut v, "B1", "B1");
    select(&mut v, "C1");
    assert_eq!(v.paste_grid_clip(&clip), Ok(GridPasted::Done));
    assert_eq!(formula(&v, "C1").as_deref(), Some("B1*10"));
    let opts = PasteOptions {
        view: v.id,
        edit_gen: v.edit_gen,
        block: clip.block.clone(),
        at: at("C1"),
        rect: v.range(),
        item: PasteItem::Paste,
    };
    v.repaste(&opts, PasteItem::Values).unwrap();
    assert_eq!(formula(&v, "C1"), None);
    assert_eq!(value(&v, "C1"), CellValue::Number(20.0));
    assert_eq!(v.undo.len(), 1, "the values took the paste's step");
    assert!(v.undo_step());
    assert_eq!(value(&v, "C1"), CellValue::Empty, "one undo: before either");
    // Any other edit since retires the button.
    let mut v = view();
    put(&mut v, "A1", Cell::number(1.0));
    let clip = copy(&mut v, "A1", "A1");
    v.paste_special_at(&clip.block, &PasteSpec::default(), at("B1"))
        .unwrap();
    let stale = PasteOptions {
        view: v.id,
        edit_gen: v.edit_gen,
        block: clip.block,
        at: at("B1"),
        rect: (0, 1, 0, 1),
        item: PasteItem::Paste,
    };
    v.push_undo();
    assert!(v.repaste(&stale, PasteItem::Values).is_err());
}

#[test]
fn text_from_another_program_pastes_as_values_and_transposed() {
    let mut v = view();
    let block = v.text_clip_block("1\t2\n3\t4\n", at("A1"));
    let spec = PasteSpec {
        transpose: true,
        ..PasteSpec::of(PasteWhat::Values)
    };
    v.paste_special_at(&block, &spec, at("A1")).unwrap();
    assert_eq!(value(&v, "B1"), CellValue::Number(3.0));
    assert_eq!(value(&v, "A2"), CellValue::Number(2.0));
}

#[test]
fn the_dialog_offers_text_only_what_text_takes() {
    let d = paste_special_dialog(false);
    assert!(
        d.buttons
            .iter()
            .any(|b| b.label == "Paste Link" && !b.enabled)
    );
    assert_eq!(staged_spec(&d), Ok(PasteSpec::default()));
    let mut d = paste_special_dialog(false);
    d.controls[0].value = Value::Choice(Some(1)); // Formulas
    assert!(staged_spec(&d).is_err());
    let mut d = paste_special_dialog(true);
    d.controls[0].value = Value::Choice(Some(2)); // Values
    d.controls[1].value = Value::Choice(Some(1)); // Add
    d.controls[2].value = Value::Bool(true);
    assert_eq!(
        staged_spec(&d),
        Ok(PasteSpec {
            what: PasteWhat::Values,
            op: PasteOp::Add,
            skip_blanks: true,
            transpose: false
        })
    );
}

#[test]
fn the_office_clipboard_keeps_24_newest_first() {
    let mut oc = OfficeClipboard::default();
    for i in 0..30 {
        oc.push(&format!("item {i}\n"));
    }
    assert_eq!(oc.items.len(), OFFICE_CLIPBOARD_CAP);
    assert_eq!(oc.items[0], "item 29\n");
    assert_eq!(oc.items[23], "item 6\n");
    let mut oc = OfficeClipboard::default();
    oc.push("a\tb\n");
    oc.push("c\n");
    assert_eq!(oc.all_text(), "c\na\tb");
    assert_eq!(OfficeClipboard::preview("a\tb\nc\n"), "a  b\u{2026}");
    oc.push("");
    assert_eq!(oc.items.len(), 2, "an empty copy adds nothing");
}
