//! Calculation options (#674): manual mode over the control surface, Calculate
//! Sheet and Calculate Now, the `Calculate` status word, and Set precision as
//! displayed with its warning and its one undo step.

use super::*;
use ctlcore::json::Json;

fn app() -> App {
    let mut a = App::new(new_xlsx(), "calc-test.xlsx");
    a.os_clip = None;
    a
}

fn call(a: &mut App, verb: &str, args: Json) -> Result<Json, String> {
    run_control(a, verb, &args)
}

fn set(a: &mut App, at: &str, text: &str) {
    call(
        a,
        "cell.set",
        Json::obj(vec![
            ("ref", Json::Str(at.into())),
            ("text", Json::Str(text.into())),
        ]),
    )
    .unwrap();
}

fn text(a: &mut App, at: &str) -> String {
    let r = call(
        a,
        "cell.get",
        Json::obj(vec![("ref", Json::Str(at.into()))]),
    )
    .unwrap();
    r.get_str("text").unwrap().to_string()
}

fn calc_set(a: &mut App, pairs: Vec<(&str, Json)>) -> Result<Json, String> {
    call(a, "wb.calc.set", Json::obj(pairs))
}

fn manual() -> Json {
    Json::Str("manual".into())
}

/// The issue's repro: A1 = 5, A2 = A1*2, then manual and A1 = 7.
fn repro() -> App {
    let mut a = app();
    set(&mut a, "A1", "5");
    set(&mut a, "A2", "=A1*2");
    assert_eq!(text(&mut a, "A2"), "10");
    calc_set(&mut a, vec![("mode", manual())]).unwrap();
    set(&mut a, "A1", "7");
    a
}

#[test]
fn ctl_manual_mode_repro_from_issue() {
    let mut a = repro();
    assert_eq!(text(&mut a, "A2"), "10", "A2 is not re-evaluated");
    let c = call(&mut a, "wb.calc", Json::Null).unwrap();
    assert_eq!(c.get_str("mode"), Some("manual"));
    assert_eq!(c.get_usize("stale"), Some(1));
    call(&mut a, "wb.recalc", Json::Null).unwrap();
    assert_eq!(text(&mut a, "A2"), "14");
    let c = call(&mut a, "wb.calc", Json::Null).unwrap();
    assert_eq!(c.get_usize("stale"), Some(0));
}

#[test]
fn wb_recalc_sheet_scope() {
    let mut a = app();
    call(
        &mut a,
        "sheet.add",
        Json::obj(vec![("name", Json::Str("Two".into()))]),
    )
    .unwrap();
    set(&mut a, "A1", "5");
    set(&mut a, "A2", "=A1*2");
    let two = |at: &str, t: &str| {
        Json::obj(vec![
            ("ref", Json::Str(at.into())),
            ("text", Json::Str(t.into())),
            ("sheet", Json::Str("Two".into())),
        ])
    };
    call(&mut a, "cell.set", two("B1", "=Sheet1!A2+1")).unwrap();
    calc_set(&mut a, vec![("mode", manual())]).unwrap();
    set(&mut a, "A1", "7");
    let r = call(
        &mut a,
        "wb.recalc",
        Json::obj(vec![("sheet", Json::Str("Sheet1".into()))]),
    )
    .unwrap();
    assert_eq!(r.get_usize("stale"), Some(1), "Two's cell stays stale: {r}");
    assert_eq!(text(&mut a, "A2"), "14");
    let b1 = call(
        &mut a,
        "cell.get",
        Json::obj(vec![
            ("ref", Json::Str("B1".into())),
            ("sheet", Json::Str("Two".into())),
        ]),
    )
    .unwrap();
    assert_eq!(b1.get_str("text"), Some("11"), "not calculated: {b1}");
    let err = call(
        &mut a,
        "wb.recalc",
        Json::obj(vec![("sheet", Json::Str("Nope".into()))]),
    )
    .unwrap_err();
    assert!(err.contains("no sheet named"), "{err}");
}

#[test]
fn wb_calc_get_set() {
    let mut a = app();
    let c = call(&mut a, "wb.calc", Json::Null).unwrap();
    assert_eq!(c.get_str("mode"), Some("auto"));
    assert_eq!(c.get("precisionAsDisplayed"), Some(&Json::Bool(false)));
    let c = calc_set(
        &mut a,
        vec![
            ("mode", Json::Str("autoNoTable".into())),
            (
                "iterate",
                Json::obj(vec![
                    ("enabled", Json::Bool(true)),
                    ("maxIterations", Json::Num(50.0)),
                    ("maxChange", Json::Num(0.01)),
                ]),
            ),
            ("threads", Json::Num(2.0)),
            ("transitionEvaluation", Json::Bool(true)),
        ],
    )
    .unwrap();
    assert_eq!(c.get_str("mode"), Some("autoNoTable"));
    assert_eq!(
        c.get("iterate").unwrap().get_usize("maxIterations"),
        Some(50)
    );
    assert_eq!(c.get_usize("threads"), Some(2));
    assert_eq!(a.pkg.workbook.iterate, Some((50, 0.01)));
    // Partial: only what is given changes.
    let c = calc_set(&mut a, vec![("threads", Json::Null)]).unwrap();
    assert_eq!(c.get("threads"), Some(&Json::Null));
    assert_eq!(c.get_str("mode"), Some("autoNoTable"));
    // Checked whole before anything is set.
    for bad in [
        vec![("mode", Json::Str("sometimes".into()))],
        vec![(
            "iterate",
            Json::obj(vec![("maxIterations", Json::Num(0.0))]),
        )],
        vec![("threads", Json::Num(5000.0))],
        vec![("bogus", Json::Bool(true))],
    ] {
        let mut pairs = bad;
        pairs.push(("transitionEvaluation", Json::Bool(false)));
        assert!(calc_set(&mut a, pairs).is_err());
        let c = call(&mut a, "wb.calc", Json::Null).unwrap();
        assert_eq!(c.get("transitionEvaluation"), Some(&Json::Bool(true)));
    }
}

#[test]
fn wb_calc_set_is_refused_in_protected_view() {
    assert!(!control::allowed_in_protected_view("wb.calc.set"));
    assert!(control::allowed_in_protected_view("wb.calc"));
}

#[test]
fn shift_f9_calculates_the_sheet_and_the_status_word_follows() {
    let mut a = repro();
    assert_eq!(a.status_words(), ["Calculate"]);
    handle_key(&mut a, KeyEvent::new(KeyCode::F(9), KeyModifiers::SHIFT));
    assert_eq!(text(&mut a, "A2"), "14");
    assert!(a.status_words().is_empty());
    assert_eq!(a.status.as_deref(), Some("Calculated Sheet1"));
    // Ctrl+F9 is Calculate Now.
    set(&mut a, "A1", "8");
    assert_eq!(a.status_words(), ["Calculate"]);
    handle_key(&mut a, KeyEvent::new(KeyCode::F(9), KeyModifiers::CONTROL));
    assert_eq!(text(&mut a, "A2"), "16");
    assert!(a.status_words().is_empty());
}

#[test]
fn precision_as_displayed_needs_the_warning_accepted_and_undoes_in_one_step() {
    let mut a = app();
    set(&mut a, "A1", "3.14659");
    set(&mut a, "A2", "2.71928");
    call(
        &mut a,
        "cell.format",
        Json::obj(vec![
            ("range", Json::Str("A1".into())),
            (
                "patch",
                Json::obj(vec![("numFmt", Json::Str("0.00".into()))]),
            ),
        ]),
    )
    .unwrap();
    let err = calc_set(&mut a, vec![("precisionAsDisplayed", Json::Bool(true))]).unwrap_err();
    assert!(
        err.contains("Data will permanently lose accuracy."),
        "{err}"
    );
    assert!(
        !a.pkg.workbook.calc.precision_as_displayed,
        "Cancel changes nothing"
    );
    assert_eq!(
        a.sheet().cell(0, 0).unwrap().value,
        CellValue::Number(3.14659)
    );

    let c = calc_set(
        &mut a,
        vec![
            ("precisionAsDisplayed", Json::Bool(true)),
            ("acceptLoss", Json::Bool(true)),
        ],
    )
    .unwrap();
    assert_eq!(c.get("precisionAsDisplayed"), Some(&Json::Bool(true)));
    assert_eq!(a.sheet().cell(0, 0).unwrap().value, CellValue::Number(3.15));
    assert_eq!(
        a.sheet().cell(1, 0).unwrap().value,
        CellValue::Number(2.71928),
        "General is untouched"
    );
    a.undo();
    assert_eq!(
        a.sheet().cell(0, 0).unwrap().value,
        CellValue::Number(3.14659)
    );
    assert!(
        !a.pkg.workbook.calc.precision_as_displayed,
        "the switch is undone too"
    );
    a.redo();
    assert_eq!(a.sheet().cell(0, 0).unwrap().value, CellValue::Number(3.15));
    assert!(a.pkg.workbook.calc.precision_as_displayed);
}

#[test]
fn another_structural_undo_leaves_precision_alone() {
    let mut a = app();
    set(&mut a, "A1", "1");
    calc_set(
        &mut a,
        vec![
            ("precisionAsDisplayed", Json::Bool(true)),
            ("acceptLoss", Json::Bool(true)),
        ],
    )
    .unwrap();
    call(
        &mut a,
        "row.insert",
        Json::obj(vec![("at", Json::Num(0.0))]),
    )
    .unwrap();
    // Switched off outside history, between the row insert and its undo.
    calc_set(&mut a, vec![("precisionAsDisplayed", Json::Bool(false))]).unwrap();
    a.undo(); // the row insert, whose snapshot was taken with it on
    assert!(
        !a.pkg.workbook.calc.precision_as_displayed,
        "an older step's undo does not turn it back on"
    );
}

/// A manual-mode workbook as a file holds it: A2 = A1*2 cached as 1 (stale).
fn stale_manual_pkg() -> SheetPackage {
    let mut pkg = new_xlsx();
    pkg.workbook.calc.mode = gridcore::sheet::CalcMode::Manual;
    let sh = &mut pkg.workbook.sheets[0];
    sh.set_cell(0, 0, Cell::number(5.0));
    let mut f = Cell::formula("A1*2");
    f.value = CellValue::Number(1.0);
    sh.set_cell(1, 0, f);
    gridcore::xlsx::load_xlsx(&gridcore::xlsx::save_xlsx(&pkg)).unwrap()
}

#[test]
fn a_manual_file_opens_fresh() {
    let pkg = stale_manual_pkg();
    assert_eq!(
        pkg.workbook.sheets[0].cell(1, 0).unwrap().value,
        CellValue::Number(1.0),
        "the file holds the stale value"
    );
    let mut a = App::new(pkg.clone(), "manual.xlsx");
    assert_eq!(a.sheet().cell(1, 0).unwrap().value, CellValue::Number(10.0));
    assert!(!a.engine.is_stale());
    a.load_pkg(pkg);
    assert_eq!(a.sheet().cell(1, 0).unwrap().value, CellValue::Number(10.0));
}

#[test]
fn headless_recalc_updates_what_reads_a_refreshed_pivot_in_manual_mode() {
    use gridcore::pivot::{DataField, Pivot, PivotSource};
    let mut pkg = new_xlsx();
    pkg.workbook.calc.mode = gridcore::sheet::CalcMode::Manual;
    let sh = &mut pkg.workbook.sheets[0];
    sh.set_cell(0, 0, Cell::text("Region"));
    sh.set_cell(0, 1, Cell::text("Sales"));
    for (i, (r, v)) in [("East", 10.0), ("West", 20.0), ("East", 30.0)]
        .iter()
        .enumerate()
    {
        sh.set_cell(i as u32 + 1, 0, Cell::text(r));
        sh.set_cell(i as u32 + 1, 1, Cell::number(*v));
    }
    sh.set_cell(0, 6, Cell::formula("E2*2"));
    pkg.workbook.pivots.push(Pivot {
        name: "P".into(),
        sheet: 0,
        location: (0, 3, 0, 3),
        source: PivotSource::Range {
            sheet: "Sheet1".into(),
            rect: (0, 0, 3, 1),
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
        page_rows: None,
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
    let out = recalc_package(&mut pkg);
    assert_eq!(out.refreshed, 1);
    let g1 = pkg.workbook.sheets[0].cell(0, 6).unwrap().value.clone();
    assert_eq!(g1, CellValue::Number(80.0), "G1 reads the refreshed E2");
}

#[test]
fn wb_calc_set_that_changes_nothing_leaves_the_workbook_clean() {
    let mut a = app();
    assert!(!a.modified);
    calc_set(&mut a, vec![]).unwrap();
    calc_set(&mut a, vec![("mode", Json::Str("auto".into()))]).unwrap();
    assert!(!a.modified, "no setting changed");
    calc_set(&mut a, vec![("mode", manual())]).unwrap();
    assert!(a.modified);
}

#[test]
fn switching_precision_on_is_refused_with_other_keys() {
    let mut a = app();
    let e = calc_set(
        &mut a,
        vec![
            ("precisionAsDisplayed", Json::Bool(true)),
            ("acceptLoss", Json::Bool(true)),
            ("mode", manual()),
        ],
    )
    .unwrap_err();
    assert!(e.contains("send it alone"), "{e}");
    assert!(!a.pkg.workbook.calc.precision_as_displayed);
    assert_eq!(a.pkg.workbook.calc.mode, gridcore::sheet::CalcMode::Auto);
}
