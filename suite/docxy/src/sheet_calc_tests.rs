//! Formulas › Calculation (#674) on a `DocTab`, without a window.
//! `uiharness/cases/sheet-calc.uit` drives the same commands through the
//! ribbon and the F9 keys.

use super::*;
use crate::{Kind, SheetAct, multi_area_ok, new_sheet_surface, protected_view_allows_act};
use ctlcore::json::Json;
use gridcore::sheet::CellValue;

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

/// Type `text` into `cell` and commit it, as the window does.
fn type_into(t: &mut DocTab, cell: &str, text: &str) {
    let v = view(t);
    let at = gridcore::sheet::parse_cell_name(cell).unwrap();
    v.sel = at;
    v.anchor = at;
    v.begin_cell_edit(Some(String::new()));
    v.edit_caret = 0;
    for ch in text.chars() {
        v.edit_type(&ch.to_string());
    }
    assert!(v.commit_edit(), "{cell} took {text}");
    t.sheet_edited();
}

fn value(t: &mut DocTab, cell: &str) -> CellValue {
    let (r, c) = gridcore::sheet::parse_cell_name(cell).unwrap();
    view(t)
        .sheet()
        .cell(r, c)
        .map(|c| c.value.clone())
        .unwrap_or_default()
}

fn mode(t: &mut DocTab) -> CalcMode {
    view(t).pkg.workbook.calc.mode
}

fn word(t: &mut DocTab) -> Option<&'static str> {
    status_word(view(t))
}

/// A1 = 5, A2 = A1*2, then manual.
fn manual_book() -> DocTab {
    let mut t = tab();
    type_into(&mut t, "A1", "5");
    type_into(&mut t, "A2", "=A1*2");
    run(&mut t, CalcAct::Mode(CalcMode::Manual));
    t
}

#[test]
fn manual_mode_leaves_dependents_stale_until_calculate_now() {
    let mut t = manual_book();
    assert_eq!(mode(&mut t), CalcMode::Manual);
    assert_eq!(word(&mut t), None);
    type_into(&mut t, "A1", "7");
    assert_eq!(value(&mut t, "A2"), CellValue::Number(10.0));
    assert_eq!(word(&mut t), Some("Calculate"));
    run(&mut t, CalcAct::Now);
    assert_eq!(value(&mut t, "A2"), CellValue::Number(14.0));
    assert_eq!(word(&mut t), None);
    assert_eq!(t.status.as_ref(), "Calculated");
}

#[test]
fn calculate_sheet_calculates_the_active_sheet_only() {
    let mut t = tab();
    {
        let v = view(&mut t);
        v.pkg.add_sheet("Two");
    }
    type_into(&mut t, "A1", "5");
    type_into(&mut t, "A2", "=A1*2");
    view(&mut t).active = 1;
    type_into(&mut t, "B1", "=Sheet1!A2+1");
    view(&mut t).active = 0;
    run(&mut t, CalcAct::Mode(CalcMode::Manual));
    type_into(&mut t, "A1", "7");
    assert_eq!(word(&mut t), Some("Calculate"));
    run(&mut t, CalcAct::Sheet);
    assert_eq!(value(&mut t, "A2"), CellValue::Number(14.0));
    assert_eq!(t.status.as_ref(), "Calculated Sheet1");
    // The other sheet's formula, which reads A2, keeps its old value and is
    // still reported.
    assert_eq!(word(&mut t), Some("Calculate"));
    view(&mut t).active = 1;
    assert_eq!(value(&mut t, "B1"), CellValue::Number(11.0));
    run(&mut t, CalcAct::Sheet);
    assert_eq!(value(&mut t, "B1"), CellValue::Number(15.0));
    assert_eq!(word(&mut t), None);
}

#[test]
fn the_mode_is_ticked_and_automatic_catches_up() {
    let mut t = manual_book();
    let wb = view(&mut t).pkg.workbook.clone();
    assert!(is_on(CalcAct::Mode(CalcMode::Manual), &wb));
    assert!(!is_on(CalcAct::Mode(CalcMode::Auto), &wb));
    assert!(!is_on(CalcAct::Now, &wb));
    type_into(&mut t, "A1", "7");
    // Back to Automatic brings everything up to date, as Excel does.
    run(&mut t, CalcAct::Mode(CalcMode::Auto));
    assert_eq!(value(&mut t, "A2"), CellValue::Number(14.0));
    assert_eq!(word(&mut t), None);
    assert_eq!(t.status.as_ref(), "Calculation: Automatic");
}

#[test]
fn the_mode_is_not_history() {
    let mut t = tab();
    type_into(&mut t, "A1", "5");
    run(&mut t, CalcAct::Mode(CalcMode::Manual));
    let steps = view(&mut t).undo.len();
    assert_eq!(steps, 1, "only the entry is a step");
    type_into(&mut t, "A1", "6");
    assert!(view(&mut t).undo_step());
    assert_eq!(mode(&mut t), CalcMode::Manual, "undo keeps the mode");
}

#[test]
fn commands_classify_as_the_workbooks_not_the_selections() {
    for act in [
        CalcAct::Now,
        CalcAct::Sheet,
        CalcAct::Dialog,
        CalcAct::Mode(CalcMode::Manual),
    ] {
        let a = SheetAct::Calc(act);
        assert!(multi_area_ok(a), "{act:?}");
        assert!(!crate::act_targets_cells(a), "{act:?}");
    }
    assert!(protected_view_allows_act(SheetAct::Calc(CalcAct::Now)));
    assert!(protected_view_allows_act(SheetAct::Calc(CalcAct::Sheet)));
    assert!(!protected_view_allows_act(SheetAct::Calc(CalcAct::Dialog)));
    assert!(!protected_view_allows_act(SheetAct::Calc(CalcAct::Mode(
        CalcMode::Manual
    ))));
}

// ---- More Calculation Options -----------------------------------------------

fn set(t: &mut DocTab, control: &str, value: Json) {
    t.dialogs
        .set(control, &Json::obj(vec![("value", value)]))
        .unwrap();
}

fn open(t: &mut DocTab) {
    run(t, CalcAct::Dialog);
    assert_eq!(t.dialogs.top().map(|d| d.id.as_str()), Some("calc-options"));
}

fn press(t: &mut DocTab, button: &str) {
    crate::dialog_host::dialog_click(t, button).unwrap();
}

#[test]
fn the_dialog_sets_iteration_threads_and_transition() {
    let mut t = tab();
    open(&mut t);
    set(&mut t, "iterate", Json::Bool(true));
    set(&mut t, "max-iterations", Json::Str("50".into()));
    set(&mut t, "max-change", Json::Str("0.01".into()));
    set(&mut t, "threads", Json::Str("2".into()));
    set(&mut t, "transition", Json::Bool(true));
    press(&mut t, "OK");
    assert!(t.dialogs.top().is_none());
    let wb = &view(&mut t).pkg.workbook;
    assert_eq!(wb.iterate, Some((50, 0.01)));
    assert_eq!(wb.calc.num_threads, Some(2));
    assert!(wb.calc.transition_eval && wb.calc.transition_eval_changed);
    assert!(t.dirty);
    // Settings are not history.
    assert_eq!(view(&mut t).undo.len(), 0);
    // Reopened, it shows what OK applied.
    open(&mut t);
    let text = |t: &DocTab, name: &str| {
        let d = t.dialogs.top().unwrap();
        d.controls.iter().find(|c| c.name == name).unwrap().text()
    };
    assert_eq!(text(&t, "max-iterations"), "50");
    assert_eq!(text(&t, "max-change"), "0.01");
    assert_eq!(text(&t, "threads"), "2");
}

#[test]
fn a_refused_value_keeps_the_dialog_open_and_changes_nothing() {
    let mut t = tab();
    open(&mut t);
    set(&mut t, "iterate", Json::Bool(true));
    set(&mut t, "max-iterations", Json::Str("0".into()));
    let err = crate::dialog_host::dialog_click(&mut t, "OK").unwrap_err();
    assert!(err.contains("Maximum Iterations"), "{err}");
    assert!(t.dialogs.top().is_some());
    assert_eq!(view(&mut t).pkg.workbook.iterate, None);
}

#[test]
fn an_untouched_ok_changes_nothing() {
    let mut t = tab();
    open(&mut t);
    press(&mut t, "OK");
    assert!(!t.dirty);
    assert_eq!(view(&mut t).pkg.workbook.calc, Default::default());
}

fn precision_book() -> DocTab {
    let mut t = tab();
    type_into(&mut t, "A1", "3.14659");
    type_into(&mut t, "A2", "2.71928");
    // A1 shows two decimals.
    {
        let v = view(&mut t);
        let mut xf = v.pkg.workbook.styles.xf(0);
        xf.set_code(Some("0.00".into()));
        let ix = v.pkg.workbook.styles.intern(xf);
        v.pkg.workbook.sheets[0]
            .cells
            .get_mut(&(0, 0))
            .unwrap()
            .style = ix;
    }
    t
}

#[test]
fn precision_as_displayed_warns_then_rounds_in_one_undo_step() {
    let mut t = precision_book();
    open(&mut t);
    set(&mut t, "precision", Json::Bool(true));
    press(&mut t, "OK");
    // The warning, with Excel's words; nothing has changed yet.
    let d = t.dialogs.top().unwrap();
    assert_eq!(d.id.as_str(), "precision-warning");
    assert_eq!(
        d.text.as_deref(),
        Some("Data will permanently lose accuracy.")
    );
    assert_eq!(value(&mut t, "A1"), CellValue::Number(3.14659));
    assert!(!view(&mut t).pkg.workbook.calc.precision_as_displayed);

    // Cancel changes nothing.
    press(&mut t, "Cancel");
    assert!(t.dialogs.top().is_none());
    assert_eq!(value(&mut t, "A1"), CellValue::Number(3.14659));
    assert_eq!(view(&mut t).undo.len(), 2);

    // OK on both: A1 takes its displayed value, General A2 is left.
    open(&mut t);
    set(&mut t, "precision", Json::Bool(true));
    press(&mut t, "OK");
    press(&mut t, "OK");
    assert_eq!(value(&mut t, "A1"), CellValue::Number(3.15));
    assert_eq!(value(&mut t, "A2"), CellValue::Number(2.71928));
    assert!(view(&mut t).pkg.workbook.calc.precision_as_displayed);
    assert_eq!(view(&mut t).undo.len(), 3);
    assert_eq!(view(&mut t).undo_names()[0], "Set Precision as Displayed");

    // One undo puts the constant and the switch back; redo does both again.
    assert!(view(&mut t).undo_step());
    assert_eq!(value(&mut t, "A1"), CellValue::Number(3.14659));
    assert!(!view(&mut t).pkg.workbook.calc.precision_as_displayed);
    assert!(view(&mut t).redo_step());
    assert_eq!(value(&mut t, "A1"), CellValue::Number(3.15));
    assert!(view(&mut t).pkg.workbook.calc.precision_as_displayed);
}

#[test]
fn undoing_an_older_step_does_not_change_precision() {
    let mut t = precision_book();
    open(&mut t);
    set(&mut t, "precision", Json::Bool(true));
    press(&mut t, "OK");
    press(&mut t, "OK");
    type_into(&mut t, "C1", "1");
    // Switched off outside history, after the entry in C1.
    open(&mut t);
    set(&mut t, "precision", Json::Bool(false));
    press(&mut t, "OK");
    assert!(!view(&mut t).pkg.workbook.calc.precision_as_displayed);
    assert!(view(&mut t).undo_step()); // the entry in C1, taken with it on
    assert!(
        !view(&mut t).pkg.workbook.calc.precision_as_displayed,
        "an older step's undo does not turn it back on"
    );
    assert!(view(&mut t).undo_step()); // the switch
    assert!(!view(&mut t).pkg.workbook.calc.precision_as_displayed);
    assert_eq!(value(&mut t, "A1"), CellValue::Number(3.14659));
}

#[test]
fn an_unrelated_edit_and_undo_keep_the_stale_formulas_stale() {
    let mut t = manual_book();
    type_into(&mut t, "A1", "7");
    assert_eq!(word(&mut t), Some("Calculate"));
    type_into(&mut t, "B1", "1");
    assert!(view(&mut t).undo_step()); // B1; the engine is rebuilt
    assert_eq!(value(&mut t, "A2"), CellValue::Number(10.0));
    assert_eq!(word(&mut t), Some("Calculate"), "A2 is still out of date");
    run(&mut t, CalcAct::Sheet);
    assert_eq!(value(&mut t, "A2"), CellValue::Number(14.0));
    assert_eq!(word(&mut t), None);
}

#[test]
fn undo_after_calculate_restores_the_stale_values_and_says_so() {
    let mut t = manual_book();
    type_into(&mut t, "A1", "7");
    type_into(&mut t, "B1", "1");
    run(&mut t, CalcAct::Now);
    assert_eq!(value(&mut t, "A2"), CellValue::Number(14.0));
    assert_eq!(word(&mut t), None);
    assert!(view(&mut t).undo_step()); // B1, taken while A2 was stale
    assert_eq!(value(&mut t, "A2"), CellValue::Number(10.0));
    assert_eq!(word(&mut t), Some("Calculate"));
    run(&mut t, CalcAct::Sheet);
    assert_eq!(value(&mut t, "A2"), CellValue::Number(14.0));
    assert_eq!(word(&mut t), None);
}

#[test]
fn undo_after_going_automatic_calculates_what_it_restores() {
    let mut t = manual_book();
    type_into(&mut t, "A1", "7");
    type_into(&mut t, "B1", "1");
    run(&mut t, CalcAct::Mode(CalcMode::Auto));
    assert_eq!(value(&mut t, "A2"), CellValue::Number(14.0));
    assert!(view(&mut t).undo_step());
    assert_eq!(mode(&mut t), CalcMode::Auto);
    assert_eq!(value(&mut t, "A2"), CellValue::Number(14.0));
    assert_eq!(word(&mut t), None);
}

#[test]
fn redo_of_a_calculated_state_is_not_stale() {
    let mut t = manual_book();
    type_into(&mut t, "A1", "7");
    type_into(&mut t, "B1", "1");
    run(&mut t, CalcAct::Now);
    assert!(view(&mut t).undo_step());
    assert_eq!(word(&mut t), Some("Calculate"));
    run(&mut t, CalcAct::Now);
    assert!(view(&mut t).redo_step()); // B1 = 1 with A2 = 14, as it was taken
    assert_eq!(value(&mut t, "A2"), CellValue::Number(14.0));
    assert_eq!(word(&mut t), None);
}

/// D1 = D1+1: a circle, with iteration off an error until it is enabled.
fn circle_book() -> DocTab {
    let mut t = tab();
    type_into(&mut t, "D1", "=D1+1");
    t
}

fn set_iteration(t: &mut DocTab, on: bool, max: &str) {
    open(t);
    set(t, "iterate", Json::Bool(on));
    set(t, "max-iterations", Json::Str(max.into()));
    press(t, "OK");
}

#[test]
fn undo_recalculates_values_made_under_other_iteration_settings() {
    // Iteration off: the circle is an error. C1 is an unrelated edit.
    let mut t = circle_book();
    let off = value(&mut t, "D1");
    type_into(&mut t, "C1", "1");
    set_iteration(&mut t, true, "10");
    run(&mut t, CalcAct::Now);
    let on = value(&mut t, "D1");
    assert_ne!(on, off, "iterating changes the circle's value");
    // C1's snapshot holds the circle as it was with iteration off; the
    // iteration setting is not history's, so the circle is calculated again.
    assert!(view(&mut t).undo_step());
    assert!(view(&mut t).pkg.workbook.iterate.is_some());
    assert!(
        matches!(value(&mut t, "D1"), CellValue::Number(_)),
        "calculated under iteration, not the error the snapshot holds: {off:?}"
    );
}

#[test]
fn undo_after_iteration_is_turned_off_calculates_without_it() {
    let mut t = circle_book();
    set_iteration(&mut t, true, "10");
    run(&mut t, CalcAct::Now);
    type_into(&mut t, "C1", "1");
    let iterated = value(&mut t, "D1");
    set_iteration(&mut t, false, "10");
    let plain = value(&mut t, "D1");
    assert!(view(&mut t).undo_step()); // C1, taken while iterating
    assert_eq!(value(&mut t, "D1"), plain, "not the iterated {iterated:?}");
}

#[test]
fn undo_after_a_max_iterations_change_calculates_with_the_new_limit() {
    let mut t = circle_book();
    set_iteration(&mut t, true, "3");
    run(&mut t, CalcAct::Now);
    type_into(&mut t, "C1", "1");
    let three = value(&mut t, "D1");
    set_iteration(&mut t, true, "9");
    run(&mut t, CalcAct::Now);
    let nine = value(&mut t, "D1");
    assert_ne!(three, nine);
    assert!(view(&mut t).undo_step());
    // The circle was recalculated under the kept limit (9), not restored at 3.
    assert_ne!(value(&mut t, "D1"), three);
}

#[test]
fn a_manual_file_with_an_uncached_circle_opens_after_one_pass() {
    let mut pkg = gridcore::xlsx::new_xlsx();
    pkg.workbook.calc.mode = CalcMode::Manual;
    pkg.workbook.set_iteration(true, 100, 0.001);
    // No cached value, and every pass adds 1.
    pkg.workbook.sheets[0].set_cell(0, 3, gridcore::sheet::Cell::formula("D1+1"));
    let path = std::env::temp_dir().join(format!("docxy-674-circle-{}.xlsx", std::process::id()));
    std::fs::write(&path, gridcore::xlsx::save_xlsx(&pkg)).unwrap();
    let (surface, _) = crate::sheet_from_path_mode(&path, false);
    let _ = std::fs::remove_file(&path);
    let mut t = tab();
    t.surface = surface;
    assert_eq!(value(&mut t, "D1"), CellValue::Number(1.0), "one pass");
}

#[test]
fn a_manual_file_opens_fresh_without_the_calculate_word() {
    let mut pkg = gridcore::xlsx::new_xlsx();
    pkg.workbook.calc.mode = CalcMode::Manual;
    let sh = &mut pkg.workbook.sheets[0];
    sh.set_cell(0, 0, gridcore::sheet::Cell::number(5.0));
    let mut stale = gridcore::sheet::Cell::formula("A1*2");
    stale.value = CellValue::Number(1.0);
    sh.set_cell(1, 0, stale);
    let path = std::env::temp_dir().join(format!("docxy-674-manual-{}.xlsx", std::process::id()));
    std::fs::write(&path, gridcore::xlsx::save_xlsx(&pkg)).unwrap();
    let (surface, _) = crate::sheet_from_path_mode(&path, false);
    let _ = std::fs::remove_file(&path);
    let mut t = tab();
    t.surface = surface;
    assert_eq!(mode(&mut t), CalcMode::Manual);
    assert_eq!(value(&mut t, "A2"), CellValue::Number(10.0));
    assert_eq!(word(&mut t), None);
}

#[test]
fn calculate_now_in_protected_view_keeps_pivots_and_the_tab_clean() {
    use gridcore::pivot::{DataField, Pivot, PivotSource};
    let mut t = tab();
    {
        let wb = &mut view(&mut t).pkg.workbook;
        let sh = &mut wb.sheets[0];
        sh.set_cell(0, 0, gridcore::sheet::Cell::text("Region"));
        sh.set_cell(0, 1, gridcore::sheet::Cell::text("Sales"));
        for (i, (r, v)) in [("East", 10.0), ("West", 20.0)].iter().enumerate() {
            sh.set_cell(i as u32 + 1, 0, gridcore::sheet::Cell::text(r));
            sh.set_cell(i as u32 + 1, 1, gridcore::sheet::Cell::number(*v));
        }
        wb.pivots.push(Pivot {
            name: "P".into(),
            sheet: 0,
            location: (0, 3, 0, 3),
            source: PivotSource::Range {
                sheet: "Sheet1".into(),
                rect: (0, 0, 2, 1),
            },
            fields: vec!["Region".into(), "Sales".into()],
            row_fields: vec![0],
            col_fields: vec![],
            data_fields: vec![DataField {
                name: "Sum of Sales".into(),
                field: 1,
                agg: gridcore::frame::Agg::Sum,
            }],
            field_items: Vec::new(),
            hidden: Vec::new(),
            page: Vec::new(),
            items_order: Vec::new(),
            calc_formulas: Vec::new(),
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
            data_on_rows: false,
            unsupported: false,
            edited: false,
            refreshed: false,
            part: String::new(),
            cache_part: String::new(),
        });
    }
    t.dirty = false;
    t.access.protected = true;
    run(&mut t, CalcAct::Now);
    assert_eq!(
        value(&mut t, "E2"),
        CellValue::Empty,
        "the pivot is not refreshed"
    );
    assert!(!t.dirty, "and the tab is not dirty");
    t.access.protected = false;
    run(&mut t, CalcAct::Now);
    assert_eq!(value(&mut t, "E2"), CellValue::Number(10.0));
    assert!(t.dirty);
}

// ---- #675: what Error Checking found, and what a recalculation keeps -------

#[test]
fn the_error_flags_refresh_when_manual_mode_goes_back_to_automatic() {
    let mut t = tab();
    type_into(&mut t, "B1", "1");
    type_into(&mut t, "A1", "=1/B1");
    run(&mut t, CalcAct::Mode(CalcMode::Manual));
    type_into(&mut t, "B1", "0");
    // The stale 1 is still what A1 shows, and the flags are read from it.
    assert!(view(&mut t).cell_flags(0, 0).is_empty());
    run(&mut t, CalcAct::Mode(CalcMode::Auto));
    assert_eq!(
        view(&mut t).cell_flags(0, 0),
        vec![gridcore::audit::Rule::ErrorValue],
        "#DIV/0! now"
    );
    // Calculate Now refreshes them too, in both directions.
    run(&mut t, CalcAct::Mode(CalcMode::Manual));
    type_into(&mut t, "B1", "2");
    assert!(
        !view(&mut t).cell_flags(0, 0).is_empty(),
        "stale: still an error"
    );
    run(&mut t, CalcAct::Now);
    assert!(view(&mut t).cell_flags(0, 0).is_empty());
    type_into(&mut t, "B1", "0");
    run(&mut t, CalcAct::Sheet);
    assert!(!view(&mut t).cell_flags(0, 0).is_empty());
}

#[test]
fn calculate_now_and_sheet_leave_a_pending_cut_and_the_fill_buttons_alone() {
    let mut t = manual_book();
    let v = view(&mut t);
    v.anchor = (0, 0);
    v.sel = (1, 0);
    let clip = v.grid_clip(true).expect("something to cut");
    assert!(!clip.stale_in(v));
    let before = v.edit_gen;
    run(&mut t, CalcAct::Now);
    run(&mut t, CalcAct::Sheet);
    run(&mut t, CalcAct::Mode(CalcMode::Auto));
    let v = view(&mut t);
    assert_eq!(v.edit_gen, before, "no undo step, no edit");
    assert!(!clip.stale_in(v), "the cut is still pending");
    assert!(v.cut_still_live(&clip));
}
