//! Formulas › Error Checking (#675) on a `DocTab`, without a window.
//! `uiharness/cases/sheet-audit.uit` drives the same walk through the ribbon.

use super::*;
use crate::dialog::DialogOwner;
use crate::sheet_audit::{AuditAct, COMPLETE, click, open_error_checking};
use crate::{Kind, SheetAct, multi_area_ok, new_sheet_surface, protected_view_allows_act};
use core::prelude::v1::test;
use gridcore::sheet::{Cell, parse_cell_name};

fn tab() -> DocTab {
    DocTab {
        kind: Kind::Xlsx,
        title: "book.xlsx".into(),
        path: None,
        surface: new_sheet_surface(),
        dirty: false,
        status: "".into(),
        comments: vec![],
        tracked_comment_ids: Default::default(),
        comments_removed_all: false,
        used_comment_ids: Default::default(),
        pkg: None,
        notes: vec![],
        markdown: false,
        hf_edit: None,
        bundle_html: None,
        load_failed: false,
        dialogs: crate::dialog::DialogStack::default(),
        access: crate::open_mode::Access::default(),
        last_hot: Default::default(),
        converted_docx: None,
        pending_conversion: false,
        mail: Default::default(),
        field_codes: false,
        import: Default::default(),
    }
}

fn view(t: &mut DocTab) -> &mut SheetView {
    match &mut t.surface {
        Surface::Sheet(v) => v,
        _ => panic!("a sheet"),
    }
}

fn at(name: &str) -> (u32, u32) {
    parse_cell_name(name).unwrap()
}

fn put(t: &mut DocTab, name: &str, cell: Cell) {
    let (r, c) = at(name);
    let v = view(t);
    let s = v.active;
    v.engine.set_cell(&mut v.pkg.workbook, (s, r, c), cell);
}

fn select(t: &mut DocTab, name: &str) {
    let v = view(t);
    v.sel = at(name);
    v.anchor = at(name);
}

/// The cell the open Error Checking dialog shows.
fn shown(t: &DocTab) -> Option<(u32, u32)> {
    match t.dialogs.top()?.owner {
        DialogOwner::ErrorChecking { cur, .. } => Some(cur),
        _ => None,
    }
}

/// Flagged cells B2 (an error), D1 (a number as text), and B4 (an error).
fn flagged_sheet() -> DocTab {
    let mut t = tab();
    put(&mut t, "B2", Cell::formula("1/0"));
    put(&mut t, "D1", Cell::text("42"));
    put(&mut t, "B4", Cell::formula("NA()"));
    put(&mut t, "A1", Cell::number(1.0));
    t
}

#[test]
fn error_checking_starts_after_the_active_cell_and_wraps() {
    let mut t = flagged_sheet();
    select(&mut t, "B2");
    open_error_checking(&mut t).unwrap();
    // After B2, row by row: B4. The wrap comes back to D1 and then to B2.
    assert_eq!(shown(&t), Some(at("B4")));
    assert_eq!(view(&mut t).sel, at("B4"), "the cell is selected");
    assert_eq!(click(&mut t, "Next"), Some(Ok(())));
    assert_eq!(shown(&t), Some(at("D1")));
    assert_eq!(click(&mut t, "Next"), Some(Ok(())));
    assert_eq!(shown(&t), Some(at("B2")), "the cell it began on comes last");
    assert_eq!(click(&mut t, "Next"), Some(Ok(())));
    assert_eq!(shown(&t), None, "the walk is over");
    assert_eq!(t.dialogs.top().unwrap().text.as_deref(), Some(COMPLETE));
    assert_eq!(t.dialogs.depth(), 1);
}

#[test]
fn previous_steps_back_and_is_off_at_the_first_error() {
    let mut t = flagged_sheet();
    select(&mut t, "A1");
    open_error_checking(&mut t).unwrap();
    // From A1, row by row: D1 (row 1), B2, B4.
    assert_eq!(shown(&t), Some(at("D1")));
    let previous = |t: &DocTab| {
        t.dialogs
            .top()
            .unwrap()
            .buttons
            .iter()
            .find(|b| b.label == "Previous")
            .unwrap()
            .enabled
    };
    assert!(!previous(&t), "D1 is the first");
    assert_eq!(
        click(&mut t, "Previous"),
        None,
        "an off button presses nothing"
    );
    click(&mut t, "Next");
    click(&mut t, "Next");
    assert_eq!(shown(&t), Some(at("B4")));
    assert!(previous(&t));
    assert_eq!(click(&mut t, "Previous"), Some(Ok(())));
    assert_eq!(shown(&t), Some(at("B2")));
    assert_eq!(click(&mut t, "Previous"), Some(Ok(())));
    assert_eq!(shown(&t), Some(at("D1")));
}

#[test]
fn error_checking_skips_ignored_errors() {
    let mut t = flagged_sheet();
    select(&mut t, "B2");
    view(&mut t).audit_ignore().unwrap();
    select(&mut t, "A1");
    open_error_checking(&mut t).unwrap();
    assert_eq!(shown(&t), Some(at("D1")));
    click(&mut t, "Next");
    assert_eq!(shown(&t), Some(at("B4")), "B2 is ignored");
    click(&mut t, "Next");
    assert_eq!(shown(&t), None, "B2 never comes");
}

#[test]
fn a_sheet_with_nothing_flagged_says_the_check_is_complete() {
    let mut t = tab();
    put(&mut t, "A1", Cell::number(1.0));
    open_error_checking(&mut t).unwrap();
    assert_eq!(shown(&t), None);
    assert_eq!(t.dialogs.top().unwrap().text.as_deref(), Some(COMPLETE));
    assert_eq!(t.dialogs.top().unwrap().owner, DialogOwner::ErrorCheckDone);
}

#[test]
fn the_dialog_names_the_cell_the_formula_and_the_rule() {
    let mut t = flagged_sheet();
    select(&mut t, "A1");
    open_error_checking(&mut t).unwrap();
    let text = t.dialogs.top().unwrap().text.clone().unwrap();
    assert_eq!(
        text,
        "Error in cell D1\nNumber Stored as Text\nThe number in this cell is formatted as text or preceded by an apostrophe."
    );
    click(&mut t, "Next");
    let text = t.dialogs.top().unwrap().text.clone().unwrap();
    assert_eq!(
        text,
        "Error in cell B2\n=1/0\nDivide by Zero Error\nThe formula in this cell results in an error."
    );
}

#[test]
fn ignore_error_in_the_dialog_ignores_the_cell_and_goes_on() {
    let mut t = flagged_sheet();
    select(&mut t, "A1");
    open_error_checking(&mut t).unwrap();
    assert_eq!(shown(&t), Some(at("D1")));
    assert_eq!(click(&mut t, "Ignore Error"), Some(Ok(())));
    assert_eq!(shown(&t), Some(at("B2")), "on to the next");
    assert!(t.dirty, "the sheet changed");
    assert_eq!(view(&mut t).sheet().ignored_errors.len(), 1);
    // One undo step takes it back.
    assert!(view(&mut t).undo_step());
    assert!(view(&mut t).sheet().ignored_errors.is_empty());
}

#[test]
fn ignore_error_on_the_last_one_ends_the_check() {
    let mut t = tab();
    put(&mut t, "A1", Cell::formula("1/0"));
    select(&mut t, "C3");
    open_error_checking(&mut t).unwrap();
    assert_eq!(shown(&t), Some(at("A1")));
    click(&mut t, "Ignore Error");
    assert_eq!(shown(&t), None);
    assert_eq!(t.dialogs.top().unwrap().owner, DialogOwner::ErrorCheckDone);
}

#[test]
fn edit_in_formula_bar_closes_the_dialog_and_opens_the_editor() {
    let mut t = flagged_sheet();
    select(&mut t, "B2");
    open_error_checking(&mut t).unwrap();
    assert_eq!(shown(&t), Some(at("B4")));
    assert_eq!(click(&mut t, "Edit in Formula Bar"), Some(Ok(())));
    assert!(t.dialogs.top().is_none());
    let v = view(&mut t);
    assert_eq!(v.sel, at("B4"));
    assert_eq!(v.editing.as_deref(), Some("=NA()"));
}

#[test]
fn help_says_what_the_error_is_and_keeps_the_dialog() {
    let mut t = flagged_sheet();
    select(&mut t, "A1");
    open_error_checking(&mut t).unwrap();
    assert_eq!(click(&mut t, "Help on this error"), Some(Ok(())));
    assert!(t.status.contains("formatted as text"), "{}", t.status);
    assert_eq!(shown(&t), Some(at("D1")));
}

#[test]
fn another_dialog_or_button_is_not_error_checkings() {
    let mut t = flagged_sheet();
    select(&mut t, "A1");
    open_error_checking(&mut t).unwrap();
    assert_eq!(
        click(&mut t, "Close"),
        None,
        "Close closes through the stack"
    );
    assert_eq!(click(&mut t, "Banana"), None);
}

#[test]
fn the_smart_tag_commands_follow_the_sheet_command_rules() {
    let convert = SheetAct::Audit(AuditAct::ConvertToNumber);
    assert!(!protected_view_allows_act(convert), "they edit");
    assert!(!multi_area_ok(convert), "they act on the active cell");
    assert!(!protected_view_allows_act(SheetAct::Audit(
        AuditAct::ErrorChecking
    )));
}
