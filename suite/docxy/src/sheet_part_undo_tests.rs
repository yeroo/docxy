//! Undo and Redo of the edits that change a sheet's package parts bring the
//! parts back, not only the model (#827): conditional formatting and a table.
//! `Docxy::sheet_try_edit` runs them through `SheetView::try_edit`.

use super::*;
use core::prelude::v1::test;
use gridcore::sheet::Cell;

fn view() -> SheetView {
    let Surface::Sheet(mut v) = new_sheet_surface() else {
        panic!("a new sheet surface")
    };
    for (i, h) in ["Name", "Score"].iter().enumerate() {
        v.pkg.workbook.sheets[0].set_cell(0, i as u32, Cell::text(h));
    }
    v.pkg.workbook.sheets[0].set_cell(1, 0, Cell::text("Ann"));
    v.pkg.workbook.sheets[0].set_cell(1, 1, Cell::number(700.0));
    v
}

/// The first worksheet's saved XML, as the file would carry it.
fn saved_sheet(v: &SheetView) -> String {
    let saved = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&v.pkg)).unwrap();
    String::from_utf8_lossy(saved.part("xl/worksheets/sheet1.xml").unwrap()).into_owned()
}

fn saved_has_table(v: &SheetView) -> bool {
    let saved = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&v.pkg)).unwrap();
    saved.workbook.tables.len() == 1 && saved.part("xl/tables/table1.xml").is_some()
}

fn add_cf(v: &mut SheetView) -> bool {
    v.try_edit(SheetStep::Named(CF_STEP), |v| {
        v.pkg.add_conditional_format(
            0,
            (1, 1, 9, 1),
            "greaterThan",
            "500",
            None,
            gridcore::sheet::Dxf::default(),
        )
    })
}

fn clear_cf(v: &mut SheetView) -> bool {
    v.try_edit(SheetStep::Named(CF_STEP), |v| {
        v.pkg.clear_conditional_formats(0)
    })
}

#[test]
fn undoing_an_added_conditional_format_takes_it_out_of_the_saved_sheet() {
    let mut v = view();
    assert!(add_cf(&mut v));
    assert!(saved_sheet(&v).contains("<conditionalFormatting"));
    assert!(v.undo_step());
    assert!(
        !saved_sheet(&v).contains("<conditionalFormatting"),
        "{}",
        saved_sheet(&v)
    );
    assert!(v.redo_step());
    assert!(saved_sheet(&v).contains("<conditionalFormatting"));
}

#[test]
fn undoing_a_clear_puts_the_conditional_format_back_in_the_saved_sheet() {
    let mut v = view();
    assert!(add_cf(&mut v));
    assert!(clear_cf(&mut v));
    assert!(!saved_sheet(&v).contains("<conditionalFormatting"));
    assert!(v.undo_step());
    assert!(saved_sheet(&v).contains("<conditionalFormatting"));
    assert!(v.redo_step());
    assert!(!saved_sheet(&v).contains("<conditionalFormatting"));
}

#[test]
fn undoing_format_as_table_takes_the_table_part_out_of_the_saved_file() {
    let mut v = view();
    assert!(v.try_edit(SheetStep::Named("Format as Table"), |v| {
        v.pkg
            .add_table(0, (0, 0, 1, 1), true, "TableStyleMedium2")
            .is_ok()
    }));
    assert!(saved_has_table(&v));
    assert!(v.undo_step());
    let saved = gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&v.pkg)).unwrap();
    assert!(saved.workbook.tables.is_empty());
    assert!(!saved.part("xl/tables/table1.xml").is_some());
    assert!(v.redo_step());
    assert!(saved_has_table(&v));
}

#[test]
fn a_refused_edit_records_no_step() {
    let mut v = view();
    assert!(!v.try_edit(SheetStep::Named(CF_STEP), |_| false));
    assert!(v.undo.is_empty());
}
