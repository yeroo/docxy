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
    call(&mut a, "cell.set", two("B1", "=Sheet1!A1+1")).unwrap();
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
    assert_eq!(b1.get_str("text"), Some("6"));
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
    call(
        &mut a,
        "row.insert",
        Json::obj(vec![("at", Json::Num(0.0))]),
    )
    .unwrap();
    calc_set(
        &mut a,
        vec![
            ("precisionAsDisplayed", Json::Bool(true)),
            ("acceptLoss", Json::Bool(true)),
        ],
    )
    .unwrap();
    a.undo(); // the precision step
    a.undo(); // the row insert
    assert!(!a.pkg.workbook.calc.precision_as_displayed);
    a.redo();
    a.redo();
    assert!(a.pkg.workbook.calc.precision_as_displayed);
}
