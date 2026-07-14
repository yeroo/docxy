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

    /// Build a workbook whose sheet holds arbitrary cells (numbers *and* text),
    /// plus one conditional-formatting block and its dxf table.
    fn wb_with_cells(cells: &[(&str, Cell)], cf: CondFormat, dxfs: Vec<Dxf>) -> Workbook {
        let mut sheet = Sheet {
            name: "S".into(),
            ..Sheet::default()
        };
        for (name, cell) in cells {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            sheet.set_cell(r, c, cell.clone());
        }
        sheet.cond_formats.push(cf);
        let mut wb = Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        };
        wb.styles.dxfs = dxfs;
        wb
    }

    /// A single-rule `cellIs` block over A1:A10, dxf 0, priority 1.
    fn cell_is(op: &str, formulas: &[&str]) -> CondFormat {
        CondFormat {
            ranges: vec![(0, 0, 9, 0)],
            rules: vec![CfRule {
                kind: CfKind::CellIs {
                    op: op.into(),
                    formulas: formulas.iter().map(|s| (*s).to_string()).collect(),
                },
                dxf_id: Some(0),
                priority: 1,
            }],
        }
    }

    #[test]
    fn cell_is_numeric_operators() {
        let d = Dxf {
            bold: Some(true),
            ..Dxf::default()
        };
        // lessThan.
        let wb = wb_with_cells(
            &[("A1", Cell::number(3.0))],
            cell_is("lessThan", &["5"]),
            vec![d.clone()],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), Some(d.clone()));
        let wb = wb_with_cells(
            &[("A1", Cell::number(9.0))],
            cell_is("lessThan", &["5"]),
            vec![d.clone()],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), None);

        // greaterThanOrEqual: boundary (equal) matches.
        let wb = wb_with_cells(
            &[("A1", Cell::number(5.0))],
            cell_is("greaterThanOrEqual", &["5"]),
            vec![d.clone()],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), Some(d.clone()));
        // lessThanOrEqual: boundary matches.
        let wb = wb_with_cells(
            &[("A1", Cell::number(5.0))],
            cell_is("lessThanOrEqual", &["5"]),
            vec![d.clone()],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), Some(d.clone()));

        // equal / notEqual.
        let wb = wb_with_cells(
            &[("A1", Cell::number(7.0))],
            cell_is("equal", &["7"]),
            vec![d.clone()],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), Some(d.clone()));
        let wb = wb_with_cells(
            &[("A1", Cell::number(7.0))],
            cell_is("notEqual", &["7"]),
            vec![d.clone()],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), None);
        let wb = wb_with_cells(
            &[("A1", Cell::number(8.0))],
            cell_is("notEqual", &["7"]),
            vec![d.clone()],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), Some(d));
    }

    #[test]
    fn cell_is_between_and_not_between() {
        let d = Dxf {
            italic: Some(true),
            ..Dxf::default()
        };
        // between is inclusive on both ends.
        for (v, hit) in [
            (2.0, false),
            (3.0, true),
            (6.0, true),
            (7.0, true),
            (8.0, false),
        ] {
            let wb = wb_with_cells(
                &[("A1", Cell::number(v))],
                cell_is("between", &["3", "7"]),
                vec![d.clone()],
            );
            let got = cell_dxf(&wb, 0, 0, 0);
            assert_eq!(got.is_some(), hit, "between value {v}");
        }
        // notBetween is the negation.
        let wb = wb_with_cells(
            &[("A1", Cell::number(10.0))],
            cell_is("notBetween", &["3", "7"]),
            vec![d.clone()],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), Some(d.clone()));
        let wb = wb_with_cells(
            &[("A1", Cell::number(5.0))],
            cell_is("notBetween", &["3", "7"]),
            vec![d],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), None);
    }

    #[test]
    fn cell_is_text_operators() {
        let d = Dxf {
            color: Some((0, 0, 255)),
            ..Dxf::default()
        };
        // The operand formula is a quoted string literal.
        let cases: [(&str, &str, &str, bool); 6] = [
            ("containsText", "\"ell\"", "hello", true),
            ("containsText", "\"xyz\"", "hello", false),
            ("notContains", "\"xyz\"", "hello", true),
            ("beginsWith", "\"he\"", "hello", true),
            ("beginsWith", "\"lo\"", "hello", false),
            ("endsWith", "\"lo\"", "hello", true),
        ];
        for (op, operand, cell_text, hit) in cases {
            let wb = wb_with_cells(
                &[("A1", Cell::text(cell_text))],
                cell_is(op, &[operand]),
                vec![d.clone()],
            );
            assert_eq!(
                cell_dxf(&wb, 0, 0, 0).is_some(),
                hit,
                "{op} {operand} on {cell_text}"
            );
        }
    }

    #[test]
    fn empty_cell_never_matches_cell_is() {
        let d = Dxf {
            bold: Some(true),
            ..Dxf::default()
        };
        // A2 is empty; a cellIs comparison must not fire on it.
        let wb = wb_with_cells(
            &[("A1", Cell::number(10.0))],
            cell_is("greaterThan", &["-1"]),
            vec![d],
        );
        assert_eq!(cell_dxf(&wb, 0, 1, 0), None); // A2 empty
    }

    #[test]
    fn unknown_operator_and_missing_dxf_and_other_kind() {
        let d = Dxf {
            bold: Some(true),
            ..Dxf::default()
        };
        // Unrecognised operator → no match.
        let wb = wb_with_cells(
            &[("A1", Cell::number(5.0))],
            cell_is("weirdOp", &["1"]),
            vec![d.clone()],
        );
        assert_eq!(cell_dxf(&wb, 0, 0, 0), None);

        // A rule with no dxf_id is skipped even when it would match.
        let cf = CondFormat {
            ranges: vec![(0, 0, 9, 0)],
            rules: vec![CfRule {
                kind: CfKind::CellIs {
                    op: "greaterThan".into(),
                    formulas: vec!["0".into()],
                },
                dxf_id: None,
                priority: 1,
            }],
        };
        let wb = wb_with_cells(&[("A1", Cell::number(5.0))], cf, vec![d.clone()]);
        assert_eq!(cell_dxf(&wb, 0, 0, 0), None);

        // CfKind::Other is never evaluated.
        let cf = CondFormat {
            ranges: vec![(0, 0, 9, 0)],
            rules: vec![CfRule {
                kind: CfKind::Other,
                dxf_id: Some(0),
                priority: 1,
            }],
        };
        let wb = wb_with_cells(&[("A1", Cell::number(5.0))], cf, vec![d]);
        assert_eq!(cell_dxf(&wb, 0, 0, 0), None);
    }

    #[test]
    fn lowest_priority_number_wins() {
        let red = Dxf {
            fill: Some((255, 0, 0)),
            ..Dxf::default()
        };
        let green = Dxf {
            fill: Some((0, 255, 0)),
            ..Dxf::default()
        };
        // Two matching rules; priority 2 (red, dxf 0) vs priority 1 (green, dxf 1).
        // Lower priority number = higher precedence → green wins.
        let cf = CondFormat {
            ranges: vec![(0, 0, 9, 0)],
            rules: vec![
                CfRule {
                    kind: CfKind::CellIs {
                        op: "greaterThan".into(),
                        formulas: vec!["0".into()],
                    },
                    dxf_id: Some(0),
                    priority: 2,
                },
                CfRule {
                    kind: CfKind::CellIs {
                        op: "greaterThan".into(),
                        formulas: vec!["0".into()],
                    },
                    dxf_id: Some(1),
                    priority: 1,
                },
            ],
        };
        let wb = wb_with_cells(&[("A1", Cell::number(5.0))], cf, vec![red, green.clone()]);
        assert_eq!(cell_dxf(&wb, 0, 0, 0), Some(green));
    }

    #[test]
    fn empty_expression_formula_does_not_match() {
        let d = Dxf {
            bold: Some(true),
            ..Dxf::default()
        };
        let cf = CondFormat {
            ranges: vec![(0, 0, 4, 0)],
            rules: vec![CfRule {
                kind: CfKind::Expression {
                    formula: String::new(),
                },
                dxf_id: Some(0),
                priority: 1,
            }],
        };
        let wb = wb_with_cells(&[("A1", Cell::number(3.0))], cf, vec![d]);
        assert_eq!(cell_dxf(&wb, 0, 0, 0), None);
    }
}
