//! Calculation options as hosts surface them (Formulas > Calculation): the
//! `Calculate` status word, Calculate Now / Sheet, the mode, and Set precision
//! as displayed with its warning. Every host (the
//! desktop suite, `xlsxy`, the wasm bridge) calls these, so each shows and
//! changes the settings the same way.

use crate::engine::Engine;
use crate::pivot::RefreshOutcome;
use crate::sheet::{CalcMode, Workbook, round_cell_to_display};

/// The status-bar word while values are out of date (manual mode, after an
/// edit), like Excel's `Calculate`.
pub fn status_word(eng: &Engine) -> Option<&'static str> {
    eng.is_stale().then_some("Calculate")
}

/// The warning Excel shows before Set precision as displayed is switched on;
/// OK goes ahead, Cancel changes nothing.
pub const PRECISION_WARNING: &str = "Data will permanently lose accuracy.";

/// Calculate Now (F9): every formula of every sheet. With `pivots`, pivot
/// tables are refreshed from their sources first and what reads their output
/// is calculated too (in manual mode as well); without, the outcome is empty
/// (Protected View keeps the cached pivot values).
pub fn calculate_now(wb: &mut Workbook, eng: &mut Engine, pivots: bool) -> RefreshOutcome {
    eng.recalc_all(wb);
    if !pivots {
        return RefreshOutcome::default();
    }
    let outcome = crate::pivot::refresh_pivots(wb);
    if !outcome.changed.is_empty() {
        eng.recalc_from_forced(wb, &outcome.changed);
    }
    outcome
}

/// Calculation Options > Automatic / Automatic Except Data Tables / Manual.
/// Going back to automatic brings everything up to date at once, as Excel does.
pub fn set_mode(wb: &mut Workbook, eng: &mut Engine, mode: CalcMode) {
    let was = wb.calc.mode;
    wb.calc.mode = mode;
    if was == CalcMode::Manual && mode != CalcMode::Manual && eng.is_stale() {
        eng.recalc_all(wb);
    }
}

/// Replace every number constant whose cell format shows a fixed number of
/// decimals by the value shown (the data half of switching Set precision as
/// displayed on, for hosts that snapshot the workbook for their undo step).
/// Formulas, dates, text and General cells stay. Returns how many changed.
pub fn convert_constants(wb: &mut Workbook) -> usize {
    let xfs = wb.styles.xfs.clone();
    let mut n = 0;
    for sheet in &mut wb.sheets {
        for cell in sheet.cells.values_mut() {
            if cell.formula.is_none() && cell.f_attrs.is_none() && round_cell_to_display(&xfs, cell)
            {
                n += 1;
            }
        }
    }
    n
}

/// Turn Set precision as displayed on or off. On (the caller has shown
/// [`PRECISION_WARNING`] and been told OK) replaces the constants
/// ([`convert_constants`]), then recalculates so formula results round too;
/// the host holds the cells as they were for its undo step. Off changes no
/// data. Returns how many constants were replaced.
pub fn set_precision_as_displayed(wb: &mut Workbook, eng: &mut Engine, on: bool) -> usize {
    if !on {
        wb.calc.set_precision_as_displayed(false);
        return 0;
    }
    let n = convert_constants(wb);
    wb.calc.set_precision_as_displayed(true);
    eng.recalc_all(wb);
    n
}

/// The `calcMode` names the control surface uses.
pub fn mode_name(mode: CalcMode) -> &'static str {
    match mode {
        CalcMode::Auto => "auto",
        CalcMode::AutoNoTable => "autoNoTable",
        CalcMode::Manual => "manual",
    }
}

/// The mode a control-surface name means: `auto`, `autoNoTable` or `manual`.
pub fn parse_mode(name: &str) -> Option<CalcMode> {
    match name {
        "auto" => Some(CalcMode::Auto),
        "autoNoTable" => Some(CalcMode::AutoNoTable),
        "manual" => Some(CalcMode::Manual),
        _ => None,
    }
}

/// The settings File > Options > Formulas shows, as one snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct CalcInfo {
    pub mode: CalcMode,
    pub precision_as_displayed: bool,
    pub iterate: bool,
    pub max_iterations: u32,
    pub max_change: f64,
    pub threads: Option<u32>,
    pub concurrent: bool,
    pub transition_eval: bool,
    /// Formulas whose values are out of date (manual mode).
    pub stale: usize,
}

pub fn info(wb: &Workbook, eng: &Engine) -> CalcInfo {
    let c = &wb.calc;
    CalcInfo {
        mode: c.mode,
        precision_as_displayed: c.precision_as_displayed,
        iterate: wb.iterate.is_some(),
        max_iterations: wb.iterate.map_or(c.iterate_count, |(n, _)| n),
        max_change: wb.iterate.map_or(c.iterate_delta, |(_, d)| d),
        threads: c.num_threads,
        concurrent: c.concurrent_calc,
        transition_eval: c.transition_eval,
        stale: eng.stale_count(),
    }
}

/// A change to the settings; what is `None` stays.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalcPatch {
    pub mode: Option<CalcMode>,
    pub precision_as_displayed: Option<bool>,
    /// The "Data will permanently lose accuracy." warning was answered OK.
    pub accept_loss: bool,
    pub iterate: Option<bool>,
    pub max_iterations: Option<u32>,
    pub max_change: Option<f64>,
    /// `Some(None)` = all processors.
    pub threads: Option<Option<u32>>,
    pub concurrent: Option<bool>,
    pub transition_eval: Option<bool>,
}

impl CalcPatch {
    /// Whether the patch switches Set precision as displayed on (it is off):
    /// the one change that rewrites data, and so the one with an undo step.
    pub fn converts(&self, wb: &Workbook) -> bool {
        self.precision_as_displayed == Some(true) && !wb.calc.precision_as_displayed
    }
}

/// Excel's limits (Maximum Iterations 1 to 32767, Maximum Change 0 or more,
/// threads 1 to 1024), and the warning: switching Set precision as displayed
/// on is refused until it is accepted. Nothing changes on an error.
pub fn check_patch(wb: &Workbook, p: &CalcPatch) -> Result<(), String> {
    if let Some(n) = p.max_iterations {
        if !(1..=32767).contains(&n) {
            return Err("Maximum Iterations is a whole number from 1 to 32767".to_string());
        }
    }
    if let Some(d) = p.max_change {
        if !d.is_finite() || d < 0.0 {
            return Err("Maximum Change is a number, 0 or more".to_string());
        }
    }
    if let Some(Some(n)) = p.threads {
        if !(1..=1024).contains(&n) {
            return Err("Number of calculation threads is from 1 to 1024".to_string());
        }
    }
    if p.converts(wb) && !p.accept_loss {
        return Err(format!(
            "{PRECISION_WARNING} Send acceptLoss:true to turn Set precision as displayed on."
        ));
    }
    Ok(())
}

/// Apply every setting of `p` that is not the precision switch-on (which
/// rewrites data: the host snapshots for its undo step, runs
/// [`convert_constants`] and sets the flag). Changing the mode, the iteration
/// limits or turning precision off are not undo steps, as in Excel.
pub fn apply_settings(wb: &mut Workbook, eng: &mut Engine, p: &CalcPatch) {
    let now = info(wb, eng);
    if p.iterate.is_some() || p.max_iterations.is_some() || p.max_change.is_some() {
        wb.set_iteration(
            p.iterate.unwrap_or(now.iterate),
            p.max_iterations.unwrap_or(now.max_iterations),
            p.max_change.unwrap_or(now.max_change),
        );
        // Circles are iterated, or errors, from the next calculation on; in
        // manual mode, from the next Calculate.
        if wb.calc.mode != CalcMode::Manual {
            eng.recalc_all(wb);
        }
    }
    if let Some(t) = p.threads {
        wb.calc.num_threads = t;
    }
    if let Some(c) = p.concurrent {
        wb.calc.concurrent_calc = c;
    }
    if let Some(t) = p.transition_eval {
        wb.calc.set_transition_eval(t);
    }
    if p.precision_as_displayed == Some(false) {
        wb.calc.set_precision_as_displayed(false);
    }
    if let Some(m) = p.mode {
        set_mode(wb, eng, m);
    }
}
