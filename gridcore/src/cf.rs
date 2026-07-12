//! Conditional-formatting evaluation. Given a cell, find the differential format
//! ([`crate::sheet::Dxf`]) of the highest-priority matching rule.
//!
//! Only `cellIs` and `expression` rules are evaluated (the common ones); other
//! rule types (colorScale/dataBar/iconSet/top10/…) are ignored for now.

use crate::engine::{cell_value_at, eval_formula_at};
use crate::formula::{Value, compare, translate_formula};
use crate::sheet::{CfKind, Dxf, Workbook};
use std::cmp::Ordering;

/// Evaluate a CF formula at (row, col). CF formula references are relative to the
/// block's top-left `anchor`, so shift them by the cell's offset first (Excel
/// applies the rule cell-by-cell this way).
fn eval_cf(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    anchor: (u32, u32),
    src: &str,
) -> Value {
    let (dr, dc) = (row as i64 - anchor.0 as i64, col as i64 - anchor.1 as i64);
    let translated = if (dr, dc) == (0, 0) {
        src.to_string()
    } else {
        translate_formula(src, dr, dc).unwrap_or_else(|| src.to_string())
    };
    eval_formula_at(wb, sheet, row, col, &translated)
}

/// The differential format conditional formatting applies to cell
/// (sheet, row, col), if any. The lowest-`priority`-number matching rule wins.
pub fn cell_dxf(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Option<Dxf> {
    let s = wb.sheets.get(sheet)?;
    if s.cond_formats.is_empty() {
        return None;
    }
    let mut best: Option<(i32, usize)> = None; // (priority, dxf_id)
    for cf in &s.cond_formats {
        let covers = cf
            .ranges
            .iter()
            .any(|&(r1, c1, r2, c2)| row >= r1 && row <= r2 && col >= c1 && col <= c2);
        if !covers {
            continue;
        }
        // The anchor for relative-reference shifting is the block's top-left.
        let anchor = cf
            .ranges
            .iter()
            .fold((u32::MAX, u32::MAX), |(r, c), &(r1, c1, ..)| {
                (r.min(r1), c.min(c1))
            });
        for rule in &cf.rules {
            let Some(dxf_id) = rule.dxf_id else { continue };
            if best.is_some_and(|(p, _)| rule.priority >= p) {
                continue; // a higher-precedence rule already matched
            }
            if rule_matches(wb, sheet, row, col, anchor, &rule.kind) {
                best = Some((rule.priority, dxf_id));
            }
        }
    }
    best.and_then(|(_, id)| wb.styles.dxfs.get(id).cloned())
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Num(n) => *n != 0.0,
        _ => false,
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Num(n) => format!("{n}"),
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        _ => String::new(),
    }
}

fn rule_matches(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    anchor: (u32, u32),
    kind: &CfKind,
) -> bool {
    match kind {
        CfKind::Expression { formula } => {
            !formula.is_empty() && truthy(&eval_cf(wb, sheet, row, col, anchor, formula))
        }
        CfKind::CellIs { op, formulas } => {
            let cell = cell_value_at(wb, sheet, row, col);
            // An empty cell doesn't satisfy value comparisons.
            if matches!(cell, Value::Empty) {
                return false;
            }
            let a = formulas
                .first()
                .map(|f| eval_cf(wb, sheet, row, col, anchor, f));
            let b = formulas
                .get(1)
                .map(|f| eval_cf(wb, sheet, row, col, anchor, f));
            let cmp_a = |x: &Value| a.as_ref().and_then(|av| compare(x, av).ok());
            match op.as_str() {
                "greaterThan" => cmp_a(&cell) == Some(Ordering::Greater),
                "lessThan" => cmp_a(&cell) == Some(Ordering::Less),
                "greaterThanOrEqual" => {
                    matches!(cmp_a(&cell), Some(Ordering::Greater | Ordering::Equal))
                }
                "lessThanOrEqual" => matches!(cmp_a(&cell), Some(Ordering::Less | Ordering::Equal)),
                "equal" => cmp_a(&cell) == Some(Ordering::Equal),
                "notEqual" => matches!(cmp_a(&cell), Some(o) if o != Ordering::Equal),
                "between" | "notBetween" => {
                    let lo = matches!(cmp_a(&cell), Some(Ordering::Greater | Ordering::Equal));
                    let hi = b
                        .as_ref()
                        .and_then(|bv| compare(&cell, bv).ok())
                        .is_some_and(|o| matches!(o, Ordering::Less | Ordering::Equal));
                    let between = lo && hi;
                    if op == "between" { between } else { !between }
                }
                "containsText" | "notContains" | "beginsWith" | "endsWith" => {
                    let hay = text_of(&cell);
                    let needle = a.as_ref().map(text_of).unwrap_or_default();
                    match op.as_str() {
                        "containsText" => hay.contains(&needle),
                        "notContains" => !hay.contains(&needle),
                        "beginsWith" => hay.starts_with(&needle),
                        _ => hay.ends_with(&needle),
                    }
                }
                _ => false,
            }
        }
        CfKind::Other => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{Cell, CfRule, CondFormat, Sheet, Workbook};

    fn wb_with_cf(cells: &[(&str, f64)], cf: CondFormat, dxfs: Vec<Dxf>) -> Workbook {
        let mut sheet = Sheet {
            name: "S".into(),
            ..Sheet::default()
        };
        for (name, v) in cells {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            sheet.set_cell(r, c, Cell::number(*v));
        }
        sheet.cond_formats.push(cf);
        let mut wb = Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        };
        wb.styles.dxfs = dxfs;
        wb
    }

    #[test]
    fn cell_is_greater_than_applies_dxf() {
        let red = Dxf {
            fill: Some((255, 0, 0)),
            ..Dxf::default()
        };
        let cf = CondFormat {
            ranges: vec![(0, 0, 9, 0)], // A1:A10
            rules: vec![CfRule {
                kind: CfKind::CellIs {
                    op: "greaterThan".into(),
                    formulas: vec!["5".into()],
                },
                dxf_id: Some(0),
                priority: 1,
            }],
        };
        let wb = wb_with_cf(&[("A1", 10.0), ("A2", 3.0)], cf, vec![red.clone()]);
        assert_eq!(cell_dxf(&wb, 0, 0, 0), Some(red)); // A1=10 > 5 → fill
        assert_eq!(cell_dxf(&wb, 0, 1, 0), None); // A2=3 not > 5
        assert_eq!(cell_dxf(&wb, 0, 0, 1), None); // B1 out of range
    }

    #[test]
    fn expression_rule_applies() {
        let hi = Dxf {
            bold: Some(true),
            ..Dxf::default()
        };
        let cf = CondFormat {
            ranges: vec![(0, 0, 4, 0)],
            rules: vec![CfRule {
                kind: CfKind::Expression {
                    formula: "A1>2".into(),
                },
                dxf_id: Some(0),
                priority: 1,
            }],
        };
        let wb = wb_with_cf(&[("A1", 3.0), ("A2", 1.0)], cf, vec![hi.clone()]);
        // The expression's relative refs shift per cell: A1 evaluates `A1>2`
        // (3>2 → match); A2 evaluates the shifted `A2>2` (1>2 → no match).
        assert_eq!(cell_dxf(&wb, 0, 0, 0), Some(hi));
        assert_eq!(cell_dxf(&wb, 0, 1, 0), None);
    }
}
