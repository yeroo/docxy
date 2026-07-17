//! Structural workbook edits: insert/delete rows & columns, sheet renames.
//!
//! These are the operations that are really *reference rewriting* in
//! disguise: every formula in the workbook (and every defined name) must
//! shift with the grid, exactly as Excel does — references into deleted
//! cells become `#REF!`, ranges stretch when rows are inserted inside them,
//! and `$` anchoring plays no role (the grid itself moved).
//!
//! Formulas that don't parse (the unsupported/preserved kind) are left
//! untouched: their text may go stale, but we never corrupt it. That is the
//! same stale-not-wrong contract the engine keeps everywhere else.

use std::collections::BTreeMap;

use crate::formula::{EditShift, ExcelError, adjust_formula_for_edit, rename_sheet_in_formula};
use crate::sheet::{Cell, CellValue, MAX_COLS, MAX_ROWS, Sheet, Workbook};

/// Interpret typed input as Excel would: formulas, numbers (incl. percent),
/// booleans, error constants, text.
pub fn parse_input(text: &str) -> Cell {
    if let Some(body) = text.strip_prefix('=') {
        if !body.is_empty() {
            return Cell::formula(body);
        }
    }
    if text.is_empty() {
        return Cell::default();
    }
    let t = text.trim();
    if let Ok(n) = t.parse::<f64>() {
        if n.is_finite() {
            return Cell::number(n);
        }
    }
    if let Some(pct) = t.strip_suffix('%') {
        if let Ok(n) = pct.trim().parse::<f64>() {
            let v = n / 100.0;
            if v.is_finite() {
                return Cell::number(v);
            }
        }
    }
    if t.eq_ignore_ascii_case("TRUE") {
        return Cell {
            value: CellValue::Bool(true),
            ..Cell::default()
        };
    }
    if t.eq_ignore_ascii_case("FALSE") {
        return Cell {
            value: CellValue::Bool(false),
            ..Cell::default()
        };
    }
    if ExcelError::from_code(t).is_some() {
        return Cell {
            value: CellValue::Error(t.to_ascii_uppercase()),
            ..Cell::default()
        };
    }
    Cell::text(text)
}

/// Insert `count` blank rows before 0-based row `at` on sheet `idx`.
pub fn insert_rows(wb: &mut Workbook, idx: usize, at: u32, count: u32) {
    structural_edit(
        wb,
        idx,
        EditShift {
            rows: true,
            at,
            delta: count as i64,
        },
    );
}

/// Delete `count` rows starting at 0-based row `at` on sheet `idx`.
pub fn delete_rows(wb: &mut Workbook, idx: usize, at: u32, count: u32) {
    structural_edit(
        wb,
        idx,
        EditShift {
            rows: true,
            at,
            delta: -(count as i64),
        },
    );
}

/// Insert `count` blank columns before 0-based column `at` on sheet `idx`.
pub fn insert_cols(wb: &mut Workbook, idx: usize, at: u32, count: u32) {
    structural_edit(
        wb,
        idx,
        EditShift {
            rows: false,
            at,
            delta: count as i64,
        },
    );
}

/// Delete `count` columns starting at 0-based column `at` on sheet `idx`.
pub fn delete_cols(wb: &mut Workbook, idx: usize, at: u32, count: u32) {
    structural_edit(
        wb,
        idx,
        EditShift {
            rows: false,
            at,
            delta: -(count as i64),
        },
    );
}

/// Rename a sheet and rewrite every reference to it (formulas on all sheets
/// plus defined-name definitions), as Excel does.
pub fn rename_sheet(wb: &mut Workbook, idx: usize, new_name: &str) {
    let Some(old) = wb.sheets.get(idx).map(|s| s.name.clone()) else {
        return;
    };
    if old.eq_ignore_ascii_case(new_name) {
        wb.sheets[idx].name = new_name.to_string();
        return;
    }
    for sheet in &mut wb.sheets {
        for cell in sheet.cells.values_mut() {
            if cell.f_attrs.is_some() {
                continue; // preserved verbatim
            }
            if let Some(src) = &cell.formula {
                if let Some(updated) = rename_sheet_in_formula(src, &old, new_name) {
                    cell.formula = Some(updated);
                }
            }
        }
    }
    for dn in &mut wb.defined_names {
        if let Some(updated) = rename_sheet_in_formula(&dn.formula, &old, new_name) {
            dn.formula = updated;
        }
    }
    wb.sheets[idx].name = new_name.to_string();
}

/// The shared core: move the grid on the target sheet, then rewrite every
/// formula and defined name in the workbook.
fn structural_edit(wb: &mut Workbook, idx: usize, shift: EditShift) {
    let Some(target_name) = wb.sheets.get(idx).map(|s| s.name.clone()) else {
        return;
    };
    if shift.delta == 0 {
        return;
    }

    shift_grid(&mut wb.sheets[idx], &shift);

    for (s, sheet) in wb.sheets.iter_mut().enumerate() {
        let home_is_target = s == idx;
        for cell in sheet.cells.values_mut() {
            if cell.f_attrs.is_some() {
                continue; // preserved verbatim; stale is acceptable, corrupt is not
            }
            if let Some(src) = &cell.formula {
                if let Some(updated) =
                    adjust_formula_for_edit(src, home_is_target, &target_name, &shift)
                {
                    cell.formula = Some(updated);
                }
            }
        }
    }
    for dn in &mut wb.defined_names {
        // Defined names have no home sheet; only sheet-qualified refs shift.
        if let Some(updated) = adjust_formula_for_edit(&dn.formula, false, &target_name, &shift) {
            dn.formula = updated;
        }
    }

    // Table regions follow the grid. Row edits stretch/shift freely; column
    // edits move a table only when they fall entirely to its left — resizing
    // a table's column set would desync it from its tableColumns definition
    // (a later refinement), so intersecting column edits leave it in place.
    for t in &mut wb.tables {
        if t.sheet != idx {
            continue;
        }
        let (r1, c1, r2, c2) = t.range;
        if shift.rows {
            if let Some((lo, hi)) = span(r1, r2, &shift) {
                // Keep at least the header row alive.
                if hi >= lo {
                    t.range = (lo, c1, hi, c2);
                }
            }
        } else if shift.at <= c1 {
            let edge = if shift.delta < 0 {
                shift.at as i64 - shift.delta // first surviving column
            } else {
                shift.at as i64
            };
            if edge <= c1 as i64 {
                let d = shift.delta;
                let nc1 = (c1 as i64 + d).max(0) as u32;
                let nc2 = (c2 as i64 + d).max(0) as u32;
                if nc2 < MAX_COLS {
                    t.range = (r1, nc1, r2, nc2);
                }
            }
        }
    }
}

/// One coordinate through the shift; None = deleted.
fn point(v: u32, shift: &EditShift) -> Option<u32> {
    let v = v as i64;
    let at = shift.at as i64;
    if shift.delta >= 0 {
        let n = if v >= at { v + shift.delta } else { v };
        (n < if shift.rows { MAX_ROWS } else { MAX_COLS } as i64).then_some(n as u32)
    } else {
        let n = -shift.delta;
        if v < at {
            Some(v as u32)
        } else if v < at + n {
            None
        } else {
            Some((v - n) as u32)
        }
    }
}

/// A span through the shift (deletes clamp); None = span fully deleted.
fn span(a: u32, b: u32, shift: &EditShift) -> Option<(u32, u32)> {
    let at = shift.at;
    let lo = point(a.min(b), shift).unwrap_or(at);
    let hi = match point(a.max(b), shift) {
        Some(h) => h,
        None => at.checked_sub(1)?,
    };
    (lo <= hi).then_some((lo, hi))
}

fn shift_grid(sheet: &mut Sheet, shift: &EditShift) {
    // Cells.
    let cells = std::mem::take(&mut sheet.cells);
    sheet.cells = cells
        .into_iter()
        .filter_map(|((r, c), cell)| {
            let key = if shift.rows {
                point(r, shift).map(|nr| (nr, c))
            } else {
                point(c, shift).map(|nc| (r, nc))
            };
            key.map(|k| (k, cell))
        })
        .collect();

    // Row attributes move with their rows (only for row edits).
    if shift.rows {
        let attrs = std::mem::take(&mut sheet.row_attrs);
        sheet.row_attrs = attrs
            .into_iter()
            .filter_map(|(r, a)| point(r, shift).map(|nr| (nr, a)))
            .collect::<BTreeMap<_, _>>();
    } else {
        // Column definitions move with their columns (only for column edits).
        let defs = std::mem::take(&mut sheet.col_defs);
        sheet.col_defs = defs
            .into_iter()
            .filter_map(|mut d| {
                let (lo, hi) = span(d.min, d.max, shift)?;
                d.min = lo;
                d.max = hi;
                Some(d)
            })
            .collect();
    }

    // Merged regions stretch/clamp on the edited axis; fully-deleted ones go.
    let merges = std::mem::take(&mut sheet.merges);
    sheet.merges = merges
        .into_iter()
        .filter_map(|(r1, c1, r2, c2)| {
            if shift.rows {
                span(r1, r2, shift).map(|(a, b)| (a, c1, b, c2))
            } else {
                span(c1, c2, shift).map(|(a, b)| (r1, a, r2, b))
            }
        })
        // A 1×1 "merge" left over after clamping is meaningless.
        .filter(|&(r1, c1, r2, c2)| !(r1 == r2 && c1 == c2))
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::sheet::{Cell, CellValue, parse_cell_name};

    fn wb(cells: &[(&str, Cell)]) -> Workbook {
        let mut sheet = Sheet {
            name: "Sheet1".to_string(),
            ..Sheet::default()
        };
        for (name, cell) in cells {
            let (r, c) = parse_cell_name(name).unwrap();
            sheet.set_cell(r, c, cell.clone());
        }
        Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        }
    }

    fn formula_at(wb: &Workbook, name: &str) -> String {
        let (r, c) = parse_cell_name(name).unwrap();
        wb.sheets[0]
            .cell(r, c)
            .and_then(|cl| cl.formula.clone())
            .unwrap_or_default()
    }

    fn value_at(wb: &Workbook, name: &str) -> CellValue {
        let (r, c) = parse_cell_name(name).unwrap();
        wb.sheets[0]
            .cell(r, c)
            .map(|cl| cl.value.clone())
            .unwrap_or(CellValue::Empty)
    }

    #[test]
    fn insert_rows_shifts_cells_and_formulas() {
        let mut w = wb(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("B1", Cell::formula("SUM(A1:A3)")),
            ("B3", Cell::formula("A3*2")),
        ]);
        insert_rows(&mut w, 0, 1, 2); // two rows before row 2
        // Values moved.
        assert_eq!(value_at(&w, "A1"), CellValue::Number(1.0));
        assert_eq!(value_at(&w, "A2"), CellValue::Empty);
        assert_eq!(value_at(&w, "A4"), CellValue::Number(2.0));
        assert_eq!(value_at(&w, "A5"), CellValue::Number(3.0));
        // Formulas rewrote: the range stretched, the point ref followed.
        assert_eq!(formula_at(&w, "B1"), "SUM(A1:A5)");
        assert_eq!(formula_at(&w, "B5"), "A5*2");
        // And it still computes.
        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(value_at(&w, "B1"), CellValue::Number(6.0));
    }

    #[test]
    fn delete_rows_pins_refs_and_poisons_deleted() {
        let mut w = wb(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("A4", Cell::number(4.0)),
            ("B1", Cell::formula("SUM(A1:A4)")),
            ("B4", Cell::formula("A2+A4")),
        ]);
        delete_rows(&mut w, 0, 1, 1); // delete row 2 (the B4 formula moves up)
        assert_eq!(value_at(&w, "A2"), CellValue::Number(3.0));
        assert_eq!(value_at(&w, "A3"), CellValue::Number(4.0));
        // Range shrank; the ref into the deleted row is #REF!.
        assert_eq!(formula_at(&w, "B1"), "SUM(A1:A3)");
        assert_eq!(formula_at(&w, "B3"), "#REF!+A3");
        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(value_at(&w, "B1"), CellValue::Number(8.0));
        assert_eq!(value_at(&w, "B3"), CellValue::Error("#REF!".into()));
    }

    #[test]
    fn insert_cols_shifts_everything() {
        let mut w = wb(&[
            ("A1", Cell::number(5.0)),
            ("B1", Cell::number(6.0)),
            ("C1", Cell::formula("A1*B1")),
        ]);
        w.sheets[0].set_col_width(1, 20.0);
        insert_cols(&mut w, 0, 1, 1); // one column before B
        assert_eq!(value_at(&w, "C1"), CellValue::Number(6.0));
        assert_eq!(formula_at(&w, "D1"), "A1*C1");
        // The width definition moved with its column.
        assert_eq!(w.sheets[0].col_width(2), 20.0);
        assert_eq!(w.sheets[0].col_width(1), crate::sheet::DEFAULT_COL_WIDTH);
    }

    #[test]
    fn delete_cols_clamps_ranges_and_merges() {
        let mut w = wb(&[
            ("A1", Cell::number(1.0)),
            ("B1", Cell::number(2.0)),
            ("C1", Cell::number(3.0)),
            ("E1", Cell::formula("SUM(A1:C1)")),
        ]);
        w.sheets[0].merges.push((0, 0, 0, 2)); // A1:C1 merged
        delete_cols(&mut w, 0, 1, 1); // delete column B
        assert_eq!(formula_at(&w, "D1"), "SUM(A1:B1)");
        assert_eq!(w.sheets[0].merges, vec![(0, 0, 0, 1)]);
        // Whole-column refs shift too.
        w.sheets[0].set_cell(4, 5, Cell::formula("SUM(B:B)"));
        delete_cols(&mut w, 0, 0, 1); // delete column A
        let f = w.sheets[0].cell(4, 4).unwrap().formula.clone().unwrap();
        assert_eq!(f, "SUM(A:A)");
    }

    #[test]
    fn cross_sheet_refs_shift_only_for_target_sheet() {
        let mut data = Sheet {
            name: "Data".to_string(),
            ..Sheet::default()
        };
        data.set_cell(1, 0, Cell::number(7.0)); // Data!A2
        let mut calc = Sheet {
            name: "Calc".to_string(),
            ..Sheet::default()
        };
        calc.set_cell(0, 0, Cell::formula("Data!A2*2")); // Calc!A1
        calc.set_cell(1, 0, Cell::formula("A1+1")); // Calc!A2, local ref
        let mut w = Workbook {
            sheets: vec![data, calc],
            ..Workbook::default()
        };
        insert_rows(&mut w, 0, 0, 3); // rows on Data only
        let f0 = w.sheets[1].cell(0, 0).unwrap().formula.clone().unwrap();
        let f1 = w.sheets[1].cell(1, 0).unwrap().formula.clone().unwrap();
        assert_eq!(f0, "Data!A5*2"); // followed the shift on Data
        assert_eq!(f1, "A1+1"); // untouched: Calc didn't move
    }

    #[test]
    fn defined_names_shift_with_their_sheet() {
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        w.defined_names.push(crate::sheet::DefinedName {
            name: "Spot".to_string(),
            scope: None,
            formula: "Sheet1!$A$1".to_string(),
        });
        insert_rows(&mut w, 0, 0, 2);
        assert_eq!(w.defined_names[0].formula, "Sheet1!$A$3");
    }

    #[test]
    fn rename_sheet_rewrites_references() {
        let mut data = Sheet {
            name: "Data".to_string(),
            ..Sheet::default()
        };
        data.set_cell(0, 0, Cell::number(1.0));
        let mut calc = Sheet {
            name: "Calc".to_string(),
            ..Sheet::default()
        };
        calc.set_cell(0, 0, Cell::formula("Data!A1+SUM(Data!A1:A9)"));
        let mut w = Workbook {
            sheets: vec![data, calc],
            ..Workbook::default()
        };
        w.defined_names.push(crate::sheet::DefinedName {
            name: "D".to_string(),
            scope: None,
            formula: "Data!$A$1".to_string(),
        });
        rename_sheet(&mut w, 0, "Numbers Etc");
        assert_eq!(w.sheets[0].name, "Numbers Etc");
        let f = w.sheets[1].cell(0, 0).unwrap().formula.clone().unwrap();
        assert_eq!(f, "'Numbers Etc'!A1+SUM('Numbers Etc'!A1:A9)");
        assert_eq!(w.defined_names[0].formula, "'Numbers Etc'!$A$1");
        // Still evaluates.
        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(
            w.sheets[1].cell(0, 0).unwrap().value,
            CellValue::Number(2.0)
        );
    }
}
