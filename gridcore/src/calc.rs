//! Calculation options as hosts surface them (Formulas > Calculation): the
//! `Calculate` status word, Calculate Now / Sheet, the mode, and Set precision
//! as displayed with its warning and its one undo step. Every host (the
//! desktop suite, `xlsxy`, the wasm bridge) calls these, so each shows and
//! changes the settings the same way.

use crate::engine::Engine;
use crate::sheet::{CalcMode, Cell, CellValue, Workbook, round_to_display};

/// The status-bar word while values are out of date (manual mode, after an
/// edit), like Excel's `Calculate`.
pub fn status_word(eng: &Engine) -> Option<&'static str> {
    eng.is_stale().then_some("Calculate")
}

/// The warning Excel shows before Set precision as displayed is switched on;
/// OK goes ahead, Cancel changes nothing.
pub const PRECISION_WARNING: &str = "Data will permanently lose accuracy.";

/// Calculate Now (F9): every formula of every sheet.
pub fn calculate_now(wb: &mut Workbook, eng: &mut Engine) {
    eng.recalc_all(wb);
}

/// Calculate Sheet (Shift+F9). The sheet is named; `None` = unknown name.
pub fn calculate_sheet(wb: &mut Workbook, eng: &mut Engine, sheet: &str) -> Option<()> {
    let s = wb.sheet_index(sheet)?;
    eng.recalc_sheet(wb, s);
    Some(())
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

/// The constants Set precision as displayed replaced, as they were: the
/// one undo step of switching it on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrecisionConversion {
    pub cells: Vec<(usize, u32, u32, Cell)>,
}

/// Turn Set precision as displayed on or off. On (the caller has shown
/// [`PRECISION_WARNING`] and been told OK) replaces every number constant in
/// a cell whose format shows a fixed number of decimals by the value shown,
/// then recalculates so formula results round too; the result holds the
/// cells as they were for the undo. Off changes no data and returns nothing.
pub fn set_precision_as_displayed(
    wb: &mut Workbook,
    eng: &mut Engine,
    on: bool,
) -> Option<PrecisionConversion> {
    if !on {
        wb.calc.set_precision_as_displayed(false);
        return None;
    }
    let mut conv = PrecisionConversion::default();
    let xfs = wb.styles.xfs.clone();
    for (s, sheet) in wb.sheets.iter_mut().enumerate() {
        let mut keys: Vec<(u32, u32)> = sheet.cells.keys().copied().collect();
        keys.sort_unstable();
        for (r, c) in keys {
            let cell = sheet.cells.get_mut(&(r, c)).expect("listed");
            if cell.formula.is_some() || cell.f_attrs.is_some() {
                continue;
            }
            let Some(xf) = xfs.get(cell.style as usize) else {
                continue;
            };
            let Some(shown) = round_to_display(xf, &cell.value) else {
                continue;
            };
            if CellValue::Number(shown) != cell.value {
                conv.cells.push((s, r, c, cell.clone()));
                cell.value = CellValue::Number(shown);
            }
        }
    }
    wb.calc.set_precision_as_displayed(true);
    eng.recalc_all(wb);
    Some(conv)
}

/// Undo the switch-on: precision as displayed off, the constants back.
pub fn undo_precision_conversion(wb: &mut Workbook, eng: &mut Engine, conv: &PrecisionConversion) {
    wb.calc.set_precision_as_displayed(false);
    let sheets: std::collections::BTreeSet<usize> = conv.cells.iter().map(|c| c.0).collect();
    for s in sheets {
        let cells: Vec<(u32, u32, Cell)> = conv
            .cells
            .iter()
            .filter(|c| c.0 == s)
            .map(|(_, r, c, cell)| (*r, *c, cell.clone()))
            .collect();
        eng.restore_cells(wb, s, &cells);
    }
    eng.recalc_all(wb);
}
