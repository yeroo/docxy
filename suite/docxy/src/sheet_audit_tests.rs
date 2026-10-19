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
        pictures: Default::default(),
        run_styles: Default::default(),
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

/// The labels of the open Evaluate Formula dialog, by name.
fn label(t: &DocTab, name: &str) -> String {
    t.dialogs
        .top()
        .unwrap()
        .controls
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.text())
        .unwrap_or_default()
}

fn enabled(t: &DocTab, button: &str) -> bool {
    t.dialogs
        .top()
        .unwrap()
        .buttons
        .iter()
        .find(|b| b.label == button)
        .unwrap_or_else(|| panic!("a {button} button"))
        .enabled
}

fn evaluate_tab() -> DocTab {
    let mut t = tab();
    put(&mut t, "A1", Cell::number(1.0));
    put(&mut t, "A2", Cell::number(2.0));
    put(&mut t, "B1", Cell::formula("SUM(A1:A2)*2"));
    t
}

/// What the dialog shows is the underline in `[ ]`.
fn evaluation(t: &DocTab) -> String {
    label(t, "evaluation").replace('\u{332}', "")
}

#[test]
fn evaluate_formula_underlines_the_next_part_and_steps_to_the_value() {
    let mut t = evaluate_tab();
    select(&mut t, "B1");
    crate::sheet_audit::open_evaluate(&mut t).unwrap();
    let d = t.dialogs.top().unwrap();
    assert_eq!(d.owner, DialogOwner::EvaluateFormula);
    assert_eq!(label(&t, "reference"), "Sheet1!$B$1");
    assert_eq!(evaluation(&t), "=SUM(A1:A2)*2");
    assert_eq!(label(&t, "next"), "A1:A2", "the part underlined");
    // The underline itself: a combining low line after each of its characters.
    assert_eq!(
        label(&t, "evaluation"),
        "=SUM(A\u{332}1\u{332}:\u{332}A\u{332}2\u{332})*2"
    );
    for (next, shown) in [("SUM({1;2})", "=SUM({1;2})*2"), ("3*2", "=3*2")] {
        assert_eq!(click(&mut t, "Evaluate"), Some(Ok(())));
        assert_eq!(evaluation(&t), shown);
        assert_eq!(label(&t, "next"), next);
    }
    assert_eq!(click(&mut t, "Evaluate"), Some(Ok(())));
    assert_eq!(evaluation(&t), "6", "the value, with nothing underlined");
    assert_eq!(label(&t, "next"), "");
    assert!(!enabled(&t, "Evaluate"), "nothing left to calculate");
    assert_eq!(t.dialogs.depth(), 1, "one dialog throughout");
}

#[test]
fn restart_goes_back_to_the_formula() {
    let mut t = evaluate_tab();
    select(&mut t, "B1");
    crate::sheet_audit::open_evaluate(&mut t).unwrap();
    click(&mut t, "Evaluate");
    click(&mut t, "Evaluate");
    assert_ne!(evaluation(&t), "=SUM(A1:A2)*2");
    assert_eq!(click(&mut t, "Restart"), Some(Ok(())));
    assert_eq!(evaluation(&t), "=SUM(A1:A2)*2");
    assert_eq!(label(&t, "next"), "A1:A2");
}

#[test]
fn step_in_and_step_out_follow_a_reference_to_a_formula() {
    let mut t = tab();
    put(&mut t, "A1", Cell::number(2.0));
    put(&mut t, "A2", Cell::formula("A1*10"));
    put(&mut t, "B1", Cell::formula("A2+1"));
    select(&mut t, "B1");
    crate::sheet_audit::open_evaluate(&mut t).unwrap();
    assert!(enabled(&t, "Step In"), "A2 holds a formula");
    assert!(!enabled(&t, "Step Out"));
    assert_eq!(click(&mut t, "Step In"), Some(Ok(())));
    assert_eq!(label(&t, "reference"), "Sheet1!$A$2");
    assert_eq!(evaluation(&t), "=A1*10");
    assert!(enabled(&t, "Step Out") && !enabled(&t, "Step In"));
    assert_eq!(click(&mut t, "Step Out"), Some(Ok(())));
    assert_eq!(label(&t, "reference"), "Sheet1!$B$1");
    assert_eq!(evaluation(&t), "=20+1");
}

#[test]
fn a_volatile_function_and_a_constant_each_get_their_note() {
    let mut t = tab();
    put(&mut t, "A1", Cell::formula("RAND()+1"));
    put(&mut t, "A2", Cell::number(5.0));
    select(&mut t, "A1");
    crate::sheet_audit::open_evaluate(&mut t).unwrap();
    assert!(label(&t, "note").contains("volatile"));
    t.dialogs.pop();
    select(&mut t, "A2");
    crate::sheet_audit::open_evaluate(&mut t).unwrap();
    assert_eq!(
        label(&t, "note"),
        "The cell currently being evaluated contains a constant."
    );
    assert_eq!(evaluation(&t), "5");
    assert!(!enabled(&t, "Evaluate"));
}

#[test]
fn close_is_not_evaluates_and_an_off_button_presses_nothing() {
    let mut t = evaluate_tab();
    select(&mut t, "B1");
    crate::sheet_audit::open_evaluate(&mut t).unwrap();
    assert_eq!(click(&mut t, "Close"), None);
    assert_eq!(click(&mut t, "Step Out"), None, "nothing was stepped into");
}

#[test]
fn show_calculation_steps_opens_evaluate_on_top_of_error_checking() {
    let mut t = flagged_sheet();
    select(&mut t, "A1");
    open_error_checking(&mut t).unwrap();
    // The walk shows D1 first, a number stored as text: no steps for it.
    assert!(!enabled(&t, "Show Calculation Steps\u{2026}"));
    click(&mut t, "Next");
    assert_eq!(shown(&t), Some(at("B2")));
    assert!(enabled(&t, "Show Calculation Steps\u{2026}"));
    assert_eq!(
        click(&mut t, "Show Calculation Steps\u{2026}"),
        Some(Ok(()))
    );
    assert_eq!(t.dialogs.depth(), 2, "on top of Error Checking");
    assert_eq!(label(&t, "reference"), "Sheet1!$B$2");
    assert_eq!(evaluation(&t), "=1/0");
    click(&mut t, "Evaluate");
    assert_eq!(evaluation(&t), "#DIV/0!");
    t.dialogs.pop();
    assert_eq!(shown(&t), Some(at("B2")), "back in Error Checking");
}
