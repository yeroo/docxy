//! Circular references as the hosts surface them (#660, #759): Excel's
//! warning, the rule for when one is shown, and the cells named as the status
//! bar's `Circular References: <ref>` names them. xlsxy, the suite's Sheet tab
//! and the wasm grid editors all read them from here.

use crate::engine::{Engine, Key};
use crate::sheet::{Workbook, cell_name, quote_sheet_name};

/// Excel's warning when an open or an edit finds a circular reference.
pub const CIRCULAR_WARNING: &str = "There are one or more circular references where a formula refers to its own cell either directly or indirectly. This might cause them to calculate incorrectly.";

/// Whether circles are surfaced (warning, status bar): not when the workbook
/// enables iterative calculation, where Excel treats them as intended.
pub fn circles_shown(engine: &Engine, wb: &Workbook) -> bool {
    wb.iterate.is_none() && engine.has_circles()
}

/// The cells on circular references as A1 refs: bare on the `active` sheet,
/// `Sheet!A1` elsewhere (quoted only where a formula would need it).
/// Active-sheet cells come first. Listed whether or not circles are shown.
pub fn circular_ref_names(engine: &Engine, wb: &Workbook, active: usize) -> Vec<String> {
    let refs = engine.circular_refs();
    let (here, elsewhere): (Vec<_>, Vec<_>) = refs.iter().partition(|k| k.0 == active);
    here.iter()
        .chain(elsewhere.iter())
        .map(|&&k| ref_name(wb, active, k))
        .collect()
}

/// The ref the status bar shows: the first of [`circular_ref_names`], or
/// `None` while circles are not shown.
pub fn first_circular_ref(engine: &Engine, wb: &Workbook, active: usize) -> Option<String> {
    if !circles_shown(engine, wb) {
        return None;
    }
    let mut keys = engine.circular_keys();
    let first = keys
        .clone()
        .find(|k| k.0 == active)
        .or_else(|| keys.next())?;
    Some(ref_name(wb, active, first))
}

fn ref_name(wb: &Workbook, active: usize, (s, r, c): Key) -> String {
    if s == active {
        return cell_name(r, c);
    }
    let sheet = wb
        .sheets
        .get(s)
        .map(|sh| sh.name.as_str())
        .unwrap_or_default();
    format!("{}!{}", quote_sheet_name(sheet), cell_name(r, c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{Cell, Sheet};

    fn book(names: &[&str]) -> Workbook {
        let mut wb = Workbook::default();
        for n in names {
            wb.sheets.push(Sheet {
                name: (*n).into(),
                ..Sheet::default()
            });
        }
        wb
    }

    #[test]
    fn names_put_the_active_sheet_first_and_quote_as_formulas_do() {
        let mut wb = book(&["Sheet1", "My Data", "Data2"]);
        wb.sheets[0].set_cell(3, 1, Cell::formula("B4+1"));
        wb.sheets[1].set_cell(0, 0, Cell::formula("A1*2"));
        wb.sheets[2].set_cell(1, 2, Cell::formula("C2"));
        let eng = Engine::new(&wb);
        assert!(circles_shown(&eng, &wb));
        assert_eq!(
            circular_ref_names(&eng, &wb, 0),
            ["B4", "'My Data'!A1", "Data2!C2"]
        );
        assert_eq!(
            circular_ref_names(&eng, &wb, 2),
            ["C2", "Sheet1!B4", "'My Data'!A1"]
        );
        assert_eq!(first_circular_ref(&eng, &wb, 1).as_deref(), Some("A1"));
        assert_eq!(first_circular_ref(&eng, &wb, 2).as_deref(), Some("C2"));
    }

    #[test]
    fn first_ref_falls_back_to_another_sheet() {
        let mut wb = book(&["Sheet1", "Other"]);
        wb.sheets[1].set_cell(4, 0, Cell::formula("A5"));
        let eng = Engine::new(&wb);
        assert_eq!(
            first_circular_ref(&eng, &wb, 0).as_deref(),
            Some("Other!A5")
        );
    }

    #[test]
    fn nothing_is_shown_without_circles_or_with_iteration() {
        let mut wb = book(&["Sheet1"]);
        wb.sheets[0].set_cell(0, 0, Cell::formula("B1+1"));
        let eng = Engine::new(&wb);
        assert!(!circles_shown(&eng, &wb));
        assert_eq!(first_circular_ref(&eng, &wb, 0), None);
        assert!(circular_ref_names(&eng, &wb, 0).is_empty());

        wb.sheets[0].set_cell(1, 0, Cell::formula("A2"));
        wb.iterate = Some((100, 0.001));
        let eng = Engine::new(&wb);
        assert!(!circles_shown(&eng, &wb));
        assert_eq!(first_circular_ref(&eng, &wb, 0), None);
        // Still listed: `wb.path.circular` reports them either way.
        assert_eq!(circular_ref_names(&eng, &wb, 0), ["A2"]);
    }
}
