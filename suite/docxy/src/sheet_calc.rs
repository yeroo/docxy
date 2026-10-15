//! Formulas › Calculation (#674): Calculation Options (Automatic, Automatic
//! Except for Data Tables, Manual), Calculate Now (F9, every sheet), Calculate
//! Sheet (Shift+F9, the active sheet), the `Calculate` status word, and More
//! Calculation Options..., one dialog for Set precision as displayed,
//! iterative calculation, the calculation threads and Transition formula
//! evaluation. The engine is `gridcore::calc`'s.
//!
//! Only Set precision as displayed is an undo step, as in Excel: it rewrites
//! constants. The mode and the other settings are the workbook's, not
//! history's; an undo of a cell edit leaves them as they are.

use crate::dialog::catalog;
use crate::dialog::{ButtonRole, Control, ControlKind, Dialog, DialogOwner, Value};
use crate::sheet_step::SheetStep;
use crate::{DocTab, SheetView, Surface};
use gridcore::calc::{self, CalcPatch};
use gridcore::sheet::{CalcMode, Workbook};

/// A Formulas › Calculation command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CalcAct {
    /// Calculation Options: Automatic, Automatic Except for Data Tables, Manual.
    Mode(CalcMode),
    /// Calculate Now (F9).
    Now,
    /// Calculate Sheet (Shift+F9).
    Sheet,
    /// More Calculation Options...
    Dialog,
}

/// Whether `act` draws ticked: the mode in force.
pub(crate) fn is_on(act: CalcAct, wb: &Workbook) -> bool {
    matches!(act, CalcAct::Mode(m) if wb.calc.mode == m)
}

/// The status-bar word while values are out of date.
pub(crate) fn status_word(v: &SheetView) -> Option<&'static str> {
    calc::status_word(&v.engine)
}

/// Run a Formulas › Calculation command on the tab.
pub(crate) fn run(tab: &mut DocTab, act: CalcAct) {
    let done = match act {
        CalcAct::Mode(m) => set_mode(tab, m),
        CalcAct::Now => calculate_now(tab),
        CalcAct::Sheet => calculate_sheet(tab),
        CalcAct::Dialog => options_dialog(tab).map(|d| tab.dialogs.push(d)),
    };
    if let Err(e) = done {
        tab.status = e.into();
    }
}

fn sheet_view(tab: &mut DocTab) -> Result<&mut SheetView, String> {
    match &mut tab.surface {
        Surface::Sheet(v) => Ok(v),
        _ => Err("calculation needs a spreadsheet".into()),
    }
}

fn mode_name(m: CalcMode) -> &'static str {
    match m {
        CalcMode::Auto => "Automatic",
        CalcMode::AutoNoTable => "Automatic Except for Data Tables",
        CalcMode::Manual => "Manual",
    }
}

fn set_mode(tab: &mut DocTab, m: CalcMode) -> Result<(), String> {
    let v = sheet_view(tab)?;
    if v.pkg.workbook.calc.mode == m {
        return Ok(());
    }
    calc::set_mode(&mut v.pkg.workbook, &mut v.engine, m);
    tab.set_dirty();
    tab.status = format!("Calculation: {}", mode_name(m)).into();
    Ok(())
}

/// Calculate Now: every formula of every sheet, pivots refreshed from their
/// sources, as xlsxy's F9 does.
fn calculate_now(tab: &mut DocTab) -> Result<(), String> {
    let v = sheet_view(tab)?;
    let was_stale = v.engine.is_stale();
    calc::calculate_now(&mut v.pkg.workbook, &mut v.engine);
    let outcome = gridcore::pivot::refresh_pivots(&mut v.pkg.workbook);
    if !outcome.changed.is_empty() {
        v.engine.recalc_from(&mut v.pkg.workbook, &outcome.changed);
    }
    if was_stale || !outcome.changed.is_empty() {
        tab.set_dirty();
    }
    tab.status = "Calculated".into();
    Ok(())
}

/// Calculate Sheet: the active sheet's stale and volatile formulas.
fn calculate_sheet(tab: &mut DocTab) -> Result<(), String> {
    let v = sheet_view(tab)?;
    let s = v.active;
    let was_stale = v.engine.stale_on_sheet(s) > 0;
    v.engine.recalc_sheet(&mut v.pkg.workbook, s);
    let name = v.pkg.workbook.sheets[s].name.clone();
    if was_stale {
        tab.set_dirty();
    }
    tab.status = format!("Calculated {name}").into();
    Ok(())
}

/// The options dialog's fields.
const PRECISION: &str = "precision";
const ITERATE: &str = "iterate";
const MAX_ITERATIONS: &str = "max-iterations";
const MAX_CHANGE: &str = "max-change";
const THREADS: &str = "threads";
const CONCURRENT: &str = "concurrent";
const TRANSITION: &str = "transition";

fn check(name: &'static str, label: &str, on: bool) -> Control {
    Control::new(name, label, ControlKind::Checkbox, Value::Bool(on))
}

fn number(name: &'static str, label: &str, text: String) -> Control {
    Control::new(name, label, ControlKind::Number, Value::Text(text))
}

/// A number as a field shows it: `0.001`, not `0.0010000`.
fn shown(v: f64) -> String {
    let s = format!("{v:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// More Calculation Options..., as the workbook stands.
pub(crate) fn options_dialog(tab: &DocTab) -> Result<Dialog, String> {
    let Surface::Sheet(v) = &tab.surface else {
        return Err("calculation needs a spreadsheet".into());
    };
    let i = calc::info(&v.pkg.workbook, &v.engine);
    let mut d = Dialog::message(
        catalog::CALC_OPTIONS,
        "Calculation Options",
        String::new(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::SheetCalcOptions,
    );
    d.text = None;
    d.controls = vec![
        check(
            PRECISION,
            "Set precision as displayed",
            i.precision_as_displayed,
        ),
        check(ITERATE, "Enable iterative calculation", i.iterate),
        number(
            MAX_ITERATIONS,
            "Maximum Iterations:",
            i.max_iterations.to_string(),
        ),
        number(MAX_CHANGE, "Maximum Change:", shown(i.max_change)),
        number(
            THREADS,
            "Number of calculation threads (blank: all processors):",
            i.threads.map(|n| n.to_string()).unwrap_or_default(),
        ),
        check(
            CONCURRENT,
            "Enable multi-threaded calculation",
            i.concurrent,
        ),
        check(
            TRANSITION,
            "Transition formula evaluation",
            i.transition_eval,
        ),
    ];
    d.mark_opened();
    Ok(d)
}

fn control<'a>(d: &'a Dialog, name: &str) -> Option<&'a Control> {
    d.controls.iter().find(|c| c.name == name)
}

fn checked(d: &Dialog, name: &str) -> bool {
    control(d, name).is_some_and(|c| c.value == Value::Bool(true))
}

fn text(d: &Dialog, name: &str) -> String {
    control(d, name).map(|c| c.text()).unwrap_or_default()
}

/// What the options dialog's OK asks for. Only a changed field is read, so an
/// untouched OK leaves every setting as it is.
fn staged(d: &Dialog) -> Result<CalcPatch, String> {
    let mut p = CalcPatch::default();
    if d.changed(PRECISION) {
        p.precision_as_displayed = Some(checked(d, PRECISION));
    }
    if d.changed(ITERATE) {
        p.iterate = Some(checked(d, ITERATE));
    }
    if d.changed(MAX_ITERATIONS) {
        let t = text(d, MAX_ITERATIONS);
        p.max_iterations = Some(t.trim().parse().map_err(|_| {
            format!(
                "Maximum Iterations must be a whole number, not '{}'",
                t.trim()
            )
        })?);
    }
    if d.changed(MAX_CHANGE) {
        let t = text(d, MAX_CHANGE);
        p.max_change = Some(
            t.trim()
                .parse()
                .map_err(|_| format!("Maximum Change must be a number, not '{}'", t.trim()))?,
        );
    }
    if d.changed(THREADS) {
        let t = text(d, THREADS);
        p.threads = Some(if t.trim().is_empty() {
            None
        } else {
            Some(t.trim().parse().map_err(|_| {
                format!(
                    "The number of calculation threads must be a whole number, not '{}'",
                    t.trim()
                )
            })?)
        });
    }
    if d.changed(CONCURRENT) {
        p.concurrent = Some(checked(d, CONCURRENT));
    }
    if d.changed(TRANSITION) {
        p.transition_eval = Some(checked(d, TRANSITION));
    }
    Ok(p)
}

/// Set precision as displayed's warning: OK goes ahead, Cancel changes nothing.
fn warning_dialog() -> Dialog {
    let mut d = Dialog::message(
        catalog::PRECISION_WARNING,
        "Set precision as displayed",
        calc::PRECISION_WARNING.into(),
        &[("OK", ButtonRole::Accept), ("Cancel", ButtonRole::Cancel)],
        DialogOwner::SheetPrecisionWarning,
    );
    d.mark_opened();
    d
}

/// Switch Set precision as displayed on: the constants take their displayed
/// values and formula results round, as one undo step that also puts the
/// switch back.
fn precision_on(tab: &mut DocTab) -> Result<(), String> {
    let v = sheet_view(tab)?;
    if v.pkg.workbook.calc.precision_as_displayed {
        return Ok(());
    }
    let c = &v.pkg.workbook.calc;
    let was = (c.precision_as_displayed, c.precision_file_off);
    let mut snap = v.snapshot();
    snap.precision = Some(was);
    calc::set_precision_as_displayed(&mut v.pkg.workbook, &mut v.engine, true);
    v.push_undo_snapshot(snap, SheetStep::Named("Set Precision as Displayed"));
    tab.set_dirty();
    tab.status = "Set precision as displayed is on".into();
    Ok(())
}

/// A press the calculation dialogs handle themselves: the options dialog's OK
/// applies what changed (a precision switch-on asks its warning first), the
/// warning's OK switches it on. `None` for any other dialog or button
/// (Cancel closes through the stack).
pub(crate) fn click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let ok = button.replace('&', "").trim().eq_ignore_ascii_case("OK");
    match top.owner {
        DialogOwner::SheetCalcOptions if ok => {
            let patch = match staged(top) {
                Ok(p) => p,
                Err(e) => return Some(Err(e)),
            };
            let v = match sheet_view(tab) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let converts = patch.converts(&v.pkg.workbook);
            // The precision switch-on waits for its warning; the rest goes now.
            let mut now = patch;
            if converts {
                now.precision_as_displayed = None;
            }
            if let Err(e) = calc::check_patch(&v.pkg.workbook, &now) {
                return Some(Err(e));
            }
            let before = calc::info(&v.pkg.workbook, &v.engine);
            calc::apply_settings(&mut v.pkg.workbook, &mut v.engine, &now);
            let changed = calc::info(&v.pkg.workbook, &v.engine) != before;
            tab.dialogs.pop();
            if converts {
                tab.dialogs.push(warning_dialog());
            }
            if changed {
                tab.set_dirty();
                tab.status = "Calculation options changed".into();
            }
            Some(Ok(()))
        }
        DialogOwner::SheetPrecisionWarning if ok => {
            tab.dialogs.pop();
            Some(precision_on(tab))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "sheet_calc_tests.rs"]
mod tests;
