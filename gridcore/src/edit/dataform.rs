//! Data ▸ Form: Excel's data form over a list, one record (row) at a time.
//!
//! The list is an [`Area`] whose first row labels the fields; its records
//! are the rows below it. The form fixes the area when it opens and grows or
//! shrinks it itself as records are added and deleted, as Excel does: it
//! never looks for the current region again, which would take in a block
//! that a new record made adjacent. These are the form's edits and searches;
//! the app supplies the dialog.

use super::{Area, fill_changes, move_refs};
use crate::engine::cell_to_value;
use crate::formula::{CellMove, Value, db_criterion_matches, move_block_formula};
use crate::sheet::{Cell, CellValue, MAX_ROWS, Sheet, Workbook, is_array_f};

/// Why New was refused: the row below the list is taken (or off the grid).
pub const CANNOT_EXTEND: &str = "Cannot extend list or database";

/// Whether the field in `col` of the record in `row` is computed: the form
/// shows it read-only, and a new record fills it from the record above.
pub fn is_formula_field(s: &Sheet, row: u32, col: u32) -> bool {
    s.cell(row, col).is_some_and(|c| c.formula.is_some())
}

/// Whether `value` meets `criterion`, read as a D-function criteria cell
/// reads it: `>10`, `<=5`, `<>x`, `=abc` (exact), `*`/`?` wildcards, and
/// plain text matching the text values that begin with it, ignoring case.
/// A number is exact (`10` doesn't match 100). An empty criterion matches
/// anything.
pub fn criterion_matches(value: Option<&CellValue>, criterion: &str) -> bool {
    if criterion.is_empty() {
        return true;
    }
    let v = value.map_or(Value::Empty, cell_to_value);
    db_criterion_matches(criterion, &v)
}

/// Whether the record in `row` meets every `(col, criterion)`.
pub fn record_matches(s: &Sheet, row: u32, criteria: &[(u32, String)]) -> bool {
    criteria
        .iter()
        .all(|(c, crit)| criterion_matches(s.cell(row, *c).map(|x| &x.value), crit))
}

/// The record after (`forward`) or before `from` in `area` that meets
/// `criteria` (none: the next record). `None` past the first or last record:
/// Find Next and Find Prev stop at the ends.
pub fn find_record(
    s: &Sheet,
    (top, _, bottom, _): Area,
    from: u32,
    forward: bool,
    criteria: &[(u32, String)],
) -> Option<u32> {
    let hit = |r: &u32| record_matches(s, *r, criteria);
    if forward {
        (from.max(top) + 1..=bottom).find(hit)
    } else {
        (top + 1..from.min(bottom + 1)).rev().find(hit)
    }
}

/// The cells New writes for a record holding `values` (`(col, cell)`, as
/// typed) at the row below `area`: the typed cells, each taking the style of
/// the field above it when it has none of its own, and every formula field
/// of the last record filled down with its relative references moved. No
/// changes when every value is blank. Refused with [`CANNOT_EXTEND`] when
/// that row holds anything in the list's columns or is past the grid.
pub fn new_record_changes(
    s: &Sheet,
    (top, c1, bottom, c2): Area,
    values: Vec<(u32, Cell)>,
) -> Result<Vec<(u32, u32, Cell)>, &'static str> {
    if values.iter().all(|(_, c)| c.is_blank()) {
        return Ok(Vec::new());
    }
    let at = bottom + 1;
    if at >= MAX_ROWS
        || s.cells
            .range((at, c1)..=(at, c2))
            .any(|(_, c)| !c.is_blank())
    {
        return Err(CANNOT_EXTEND);
    }
    let formula_col = |c: u32| bottom > top && is_formula_field(s, bottom, c);
    let mut changes: Vec<(u32, u32, Cell)> = values
        .into_iter()
        .filter(|(c, cell)| (c1..=c2).contains(c) && !formula_col(*c) && !cell.is_blank())
        .map(|(c, mut cell)| {
            if cell.style == 0 && bottom > top {
                cell.style = s.cell(bottom, c).map_or(0, |a| a.style);
            }
            (at, c, cell)
        })
        .collect();
    for c in (c1..=c2).filter(|&c| formula_col(c)) {
        changes.extend(fill_changes(s, (at, c, at, c), true));
    }
    Ok(changes)
}

/// Delete the record in `row` of `area` on sheet `sheet`, as Excel's form
/// does: within the list's columns, the records below move up a row and the
/// list's last row is left blank; cells beside the list stay where they are.
/// A reference to a moved cell follows it, a reference to a deleted cell (a
/// range: both corners) becomes `#REF!`, and the moved cells' own formulas
/// move with them ([`move_refs`], as a cut does). A range only partly in the
/// moved cells keeps its text (`SUM(B3:B4)` with row 3 deleted still reads
/// `B3:B4`). Comments, merges, hyperlinks, and conditional-format and
/// validation ranges don't move. The caller refuses a delete that would
/// change part of an array first.
pub fn delete_record(wb: &mut Workbook, sheet: usize, (_, c1, bottom, c2): Area, row: u32) {
    let name = wb.sheets[sheet].name.clone();
    // Pushed off the grid, a reference to the deleted cells is poisoned.
    let gone = CellMove {
        src: &name,
        dst: &name,
        rect: (row, c1, row, c2),
        dr: -i64::from(MAX_ROWS),
        dc: 0,
    };
    move_refs(wb, sheet, &gone);
    let up = CellMove {
        rect: (row + 1, c1, bottom, c2),
        dr: -1,
        ..gone
    };
    if row < bottom {
        move_refs(wb, sheet, &up);
    }
    let s = &mut wb.sheets[sheet];
    for c in c1..=c2 {
        s.cells.remove(&(row, c));
    }
    for r in row + 1..=bottom {
        for c in c1..=c2 {
            let Some(mut cell) = s.cells.remove(&(r, c)) else {
                continue;
            };
            // A formula held verbatim (a shared one) keeps its text.
            let verbatim = cell.f_attrs.as_deref().is_some_and(|a| !is_array_f(a));
            if !verbatim {
                if let Some(moved) = cell
                    .formula
                    .as_deref()
                    .and_then(|f| move_block_formula(f, &up))
                {
                    cell.formula = Some(moved);
                }
            }
            s.cells.insert((r - 1, c), cell);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Name/Qty/Total over A1:C4, Total = Qty*2, and a note in E3 beside it.
    fn list() -> Workbook {
        let mut wb = Workbook::default();
        let mut s = Sheet {
            name: "Sheet1".into(),
            ..Sheet::default()
        };
        for (c, h) in ["Name", "Qty", "Total"].iter().enumerate() {
            s.set_cell(0, c as u32, Cell::text(h));
        }
        for (i, (n, q)) in [("Ann", 5.0), ("Bob", 12.0), ("Cara", 100.0)]
            .iter()
            .enumerate()
        {
            let r = i as u32 + 1;
            s.set_cell(r, 0, Cell::text(n));
            s.set_cell(r, 1, Cell::number(*q));
            let mut f = Cell::formula(&format!("B{}*2", r + 1));
            f.value = CellValue::Number(q * 2.0);
            s.set_cell(r, 2, f);
        }
        s.set_cell(2, 4, Cell::text("note"));
        wb.sheets.push(s);
        wb
    }

    const AREA: Area = (0, 0, 3, 2);

    fn crit(c: u32, s: &str) -> Vec<(u32, String)> {
        vec![(c, s.to_string())]
    }

    fn formula(wb: &Workbook, r: u32, c: u32) -> Option<String> {
        wb.sheets[0].cell(r, c).and_then(|x| x.formula.clone())
    }

    #[test]
    fn list_records_skip_header() {
        let wb = list();
        let s = &wb.sheets[0];
        // From the header row, the first record is row 2; the ends stop.
        assert_eq!(find_record(s, AREA, 0, true, &[]), Some(1));
        assert_eq!(find_record(s, AREA, 1, false, &[]), None);
        assert_eq!(find_record(s, AREA, 3, true, &[]), None);
        assert_eq!(find_record(s, AREA, 3, false, &[]), Some(2));
        assert!(is_formula_field(s, 1, 2) && !is_formula_field(s, 1, 1));
    }

    #[test]
    fn criteria_begins_with_and_ops() {
        let t = CellValue::Text("Cara".into());
        let n = CellValue::Number(12.0);
        assert!(criterion_matches(Some(&t), "c"));
        assert!(criterion_matches(Some(&t), "CAR"));
        assert!(!criterion_matches(Some(&t), "ar"));
        assert!(
            !criterion_matches(Some(&t), "=car"),
            "= asks for an exact match"
        );
        assert!(criterion_matches(Some(&t), "=cara"));
        assert!(criterion_matches(Some(&t), "<>Bob"));
        assert!(criterion_matches(Some(&n), ">10"));
        assert!(!criterion_matches(Some(&n), "<=5"));
        assert!(criterion_matches(Some(&n), "<>5"));
        assert!(
            criterion_matches(None, ""),
            "an empty criterion is no filter"
        );
        assert!(!criterion_matches(None, "x"));
    }

    #[test]
    fn criteria_wildcards() {
        let t = CellValue::Text("Cara".into());
        assert!(criterion_matches(Some(&t), "?ar"));
        assert!(criterion_matches(Some(&t), "*ra"));
        assert!(!criterion_matches(Some(&t), "=*x*"));
    }

    #[test]
    fn criteria_number_is_exact() {
        assert!(criterion_matches(Some(&CellValue::Number(10.0)), "10"));
        assert!(!criterion_matches(Some(&CellValue::Number(100.0)), "10"));
    }

    #[test]
    fn find_record_forward_backward_with_criteria() {
        let wb = list();
        let s = &wb.sheets[0];
        let big = crit(1, ">10");
        assert_eq!(find_record(s, AREA, 0, true, &big), Some(2));
        assert_eq!(find_record(s, AREA, 2, true, &big), Some(3));
        assert_eq!(find_record(s, AREA, 3, true, &big), None);
        assert_eq!(find_record(s, AREA, 3, false, &big), Some(2));
        assert_eq!(find_record(s, AREA, 2, false, &big), None);
        // Every criterion must hold; a computed field matches its value.
        let both = vec![(0, "b".to_string()), (2, "24".to_string())];
        assert_eq!(find_record(s, AREA, 0, true, &both), Some(2));
        let none = vec![(0, "a".to_string()), (1, ">10".to_string())];
        assert_eq!(find_record(s, AREA, 0, true, &none), None);
    }

    #[test]
    fn new_record_fills_formula_columns() {
        let wb = list();
        let s = &wb.sheets[0];
        let ch = new_record_changes(
            s,
            AREA,
            vec![
                (0, Cell::text("Dan")),
                (1, Cell::number(3.0)),
                (2, Cell::number(9.0)),
            ],
        )
        .unwrap();
        let at = |c: u32| {
            ch.iter()
                .find(|x| x.0 == 4 && x.1 == c)
                .map(|x| x.2.clone())
        };
        assert_eq!(at(0).unwrap().value, CellValue::Text("Dan".into()));
        assert_eq!(at(1).unwrap().value, CellValue::Number(3.0));
        // The formula field ignores what was typed and fills down.
        assert_eq!(at(2).unwrap().formula.as_deref(), Some("B5*2"));
        assert_eq!(ch.len(), 3);
        // All blank: nothing to write.
        assert_eq!(
            new_record_changes(s, AREA, vec![(0, Cell::default())]),
            Ok(vec![])
        );
    }

    #[test]
    fn new_record_carries_style_down() {
        let mut wb = list();
        wb.sheets[0].cells.get_mut(&(3, 1)).unwrap().style = 7;
        let ch = new_record_changes(&wb.sheets[0], AREA, vec![(1, Cell::number(1.0))]).unwrap();
        let qty = ch.iter().find(|x| x.1 == 1).unwrap();
        assert_eq!(qty.2.style, 7);
        let mut own = Cell::number(1.0);
        own.style = 3;
        let ch = new_record_changes(&wb.sheets[0], AREA, vec![(1, own)]).unwrap();
        assert_eq!(
            ch.iter().find(|x| x.1 == 1).unwrap().2.style,
            3,
            "its own style wins"
        );
    }

    #[test]
    fn new_record_refused_when_row_below_used() {
        let mut wb = list();
        wb.sheets[0].set_cell(4, 2, Cell::text("x"));
        let r = new_record_changes(&wb.sheets[0], AREA, vec![(0, Cell::text("Dan"))]);
        assert_eq!(r, Err(CANNOT_EXTEND));
        // Beside the list's columns doesn't count.
        let mut wb = list();
        wb.sheets[0].set_cell(4, 4, Cell::text("x"));
        assert!(new_record_changes(&wb.sheets[0], AREA, vec![(0, Cell::text("Dan"))]).is_ok());
    }

    #[test]
    fn new_record_refused_at_grid_edge() {
        let mut s = Sheet::default();
        s.set_cell(MAX_ROWS - 2, 0, Cell::text("H"));
        s.set_cell(MAX_ROWS - 1, 0, Cell::text("v"));
        let area = (MAX_ROWS - 2, 0, MAX_ROWS - 1, 0);
        let r = new_record_changes(&s, area, vec![(0, Cell::text("w"))]);
        assert_eq!(r, Err(CANNOT_EXTEND));
    }

    #[test]
    fn delete_record_shifts_list_only() {
        let mut wb = list();
        delete_record(&mut wb, 0, AREA, 1);
        let s = &wb.sheets[0];
        let v = |r, c| s.cell(r, c).map(|x| x.value.clone());
        assert_eq!(v(1, 0), Some(CellValue::Text("Bob".into())));
        assert_eq!(v(2, 0), Some(CellValue::Text("Cara".into())));
        assert_eq!(v(3, 0), None, "the list's last row is cleared");
        assert_eq!(v(3, 1), None);
        assert_eq!(v(3, 2), None);
        assert_eq!(
            v(2, 4),
            Some(CellValue::Text("note".into())),
            "column E stays"
        );
        assert_eq!(v(0, 0), Some(CellValue::Text("Name".into())));
    }

    #[test]
    fn delete_record_rewrites_refs() {
        let mut wb = list();
        wb.sheets[0].set_cell(10, 4, Cell::formula("B4"));
        wb.sheets[0].set_cell(11, 4, Cell::formula("B2+1"));
        wb.sheets[0].set_cell(12, 4, Cell::formula("SUM(A2:C2)"));
        // A moved record's formula referring to the deleted record.
        wb.sheets[0].set_cell(3, 0, Cell::formula("B2"));
        delete_record(&mut wb, 0, AREA, 1);
        assert_eq!(formula(&wb, 10, 4).as_deref(), Some("B3"));
        assert_eq!(formula(&wb, 11, 4).as_deref(), Some("#REF!+1"));
        // Printed as a deleted row prints a range it took (`delete_rows`).
        assert_eq!(formula(&wb, 12, 4).as_deref(), Some("SUM(#REF!:#REF!)"));
        // The moved records' own formulas move with them.
        assert_eq!(formula(&wb, 1, 2).as_deref(), Some("B2*2"));
        assert_eq!(formula(&wb, 2, 2).as_deref(), Some("B3*2"));
        assert_eq!(formula(&wb, 2, 0).as_deref(), Some("#REF!"));
    }

    #[test]
    fn delete_last_record_clears_row() {
        let mut wb = list();
        wb.sheets[0].set_cell(10, 4, Cell::formula("C4"));
        delete_record(&mut wb, 0, AREA, 3);
        let s = &wb.sheets[0];
        assert!((0..3).all(|c| s.cell(3, c).is_none()));
        assert_eq!(
            s.cell(2, 0).map(|x| x.value.clone()),
            Some(CellValue::Text("Bob".into()))
        );
        assert_eq!(formula(&wb, 10, 4).as_deref(), Some("#REF!"));
    }

    #[test]
    fn delete_partly_covered_range_kept() {
        let mut wb = list();
        wb.sheets[0].set_cell(10, 4, Cell::formula("SUM(B3:B4)"));
        wb.sheets[0].set_cell(11, 4, Cell::formula("SUM(B2:B4)"));
        delete_record(&mut wb, 0, AREA, 2);
        assert_eq!(formula(&wb, 10, 4).as_deref(), Some("SUM(B3:B4)"));
        assert_eq!(formula(&wb, 11, 4).as_deref(), Some("SUM(B2:B4)"));
    }

    #[test]
    fn delete_keeps_a_verbatim_formula_text() {
        let mut wb = list();
        let c = wb.sheets[0].cells.get_mut(&(3, 2)).unwrap();
        c.f_attrs = Some("t=\"shared\" si=\"0\"".into());
        delete_record(&mut wb, 0, AREA, 1);
        assert_eq!(formula(&wb, 2, 2).as_deref(), Some("B4*2"));
    }
}
