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
        CfKind::IconSet { .. } | CfKind::Other { .. } => false,
    }
}

/// A cell's displayed fill or font colour, as filtering and sorting by
/// colour compare it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// No fill, or the automatic font colour.
    None,
    Rgb((u8, u8, u8)),
    /// A theme or indexed colour we don't resolve: it is some colour, so it
    /// matches neither No Fill nor any colour.
    Unknown,
}

impl Shown {
    /// Whether this is the colour a filter or sort level names (`None`: No
    /// Fill, or the automatic font colour).
    pub fn is(self, rgb: Option<(u8, u8, u8)>) -> bool {
        match (self, rgb) {
            (Shown::None, None) => true,
            (Shown::Rgb(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// As a criterion's colour: `None` for no colour; an unknown one has none.
    pub fn rgb(self) -> Option<Option<(u8, u8, u8)>> {
        match self {
            Shown::None => Some(None),
            Shown::Rgb(c) => Some(Some(c)),
            Shown::Unknown => None,
        }
    }
}

/// The fill a cell shows: conditional formatting's when a matching rule sets
/// one, else its own.
pub fn cell_fill(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Shown {
    shown_color(wb, sheet, row, col, true)
}

/// The font colour a cell shows, conditional formatting's first.
pub fn cell_font_color(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Shown {
    shown_color(wb, sheet, row, col, false)
}

fn shown_color(wb: &Workbook, sheet: usize, row: u32, col: u32, fill: bool) -> Shown {
    if let Some(d) = cell_dxf(wb, sheet, row, col) {
        let (rgb, unresolved) = if fill {
            (d.fill, d.fill_unresolved)
        } else {
            (d.color, d.color_unresolved)
        };
        match rgb {
            Some(c) => return Shown::Rgb(c),
            // A theme or indexed colour the rule sets: some colour.
            None if unresolved => return Shown::Unknown,
            None => {}
        }
    }
    let style = wb
        .sheets
        .get(sheet)
        .and_then(|s| s.cell(row, col))
        .map_or(0, |c| c.style);
    let xf = wb.styles.xf(style);
    let (rgb, unresolved) = if fill {
        (xf.fill, xf.fill_unresolved)
    } else {
        (xf.color, xf.color_unresolved)
    };
    match rgb {
        Some(c) => Shown::Rgb(c),
        None if unresolved => Shown::Unknown,
        None => Shown::None,
    }
}

/// The conditional-formatting icon a cell shows, as (`iconSet`, `iconId`):
/// from the highest-precedence icon-set rule over it. `iconId` 0 is the
/// set's first icon (for `3Arrows`, the red down arrow), which goes to the
/// lowest values unless the rule is `reverse`. Only a number gets an icon.
/// For many cells, keep one [`Icons`] instead: it reads each rule's range
/// once.
pub fn cell_icon(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Option<(String, u32)> {
    Icons::new(wb, sheet).icon(row, col)
}

/// The icons of one sheet's cells, for a command that asks about many
/// (filtering or sorting by icon): each icon-set rule's numbers, which
/// percent and percentile thresholds need, are read and sorted once.
pub struct Icons<'a> {
    wb: &'a Workbook,
    sheet: usize,
    /// Each conditional-format block's numbers, sorted, by block index.
    nums: std::cell::RefCell<std::collections::HashMap<usize, std::rc::Rc<Vec<f64>>>>,
}

impl<'a> Icons<'a> {
    pub fn new(wb: &'a Workbook, sheet: usize) -> Self {
        Icons {
            wb,
            sheet,
            nums: Default::default(),
        }
    }

    /// The numbers of block `i`'s ranges, sorted.
    fn numbers(&self, i: usize) -> std::rc::Rc<Vec<f64>> {
        if let Some(n) = self.nums.borrow().get(&i) {
            return n.clone();
        }
        let (wb, sheet) = (self.wb, self.sheet);
        let s = &wb.sheets[sheet];
        let mut nums: Vec<f64> = Vec::new();
        let last_row = s.used_size().0.saturating_sub(1);
        for &(r1, c1, r2, c2) in &s.cond_formats[i].ranges {
            for r in r1..=r2.min(last_row) {
                for (&(_, c), _) in s.cells.range((r, c1)..=(r, c2)) {
                    if let Value::Num(n) = cell_value_at(wb, sheet, r, c) {
                        nums.push(n);
                    }
                }
            }
        }
        nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        let nums = std::rc::Rc::new(nums);
        self.nums.borrow_mut().insert(i, nums.clone());
        nums
    }

    /// See [`cell_icon`].
    pub fn icon(&self, row: u32, col: u32) -> Option<(String, u32)> {
        let (wb, sheet) = (self.wb, self.sheet);
        let s = wb.sheets.get(sheet)?;
        let mut best: Option<(i32, usize, &CfKind)> = None;
        for (i, cf) in s.cond_formats.iter().enumerate() {
            let covers = cf
                .ranges
                .iter()
                .any(|&(r1, c1, r2, c2)| row >= r1 && row <= r2 && col >= c1 && col <= c2);
            if !covers {
                continue;
            }
            for rule in &cf.rules {
                if matches!(rule.kind, CfKind::IconSet { .. })
                    && best.is_none_or(|(p, _, _)| rule.priority < p)
                {
                    best = Some((rule.priority, i, &rule.kind));
                }
            }
        }
        let (
            _,
            i,
            CfKind::IconSet {
                set,
                reverse,
                cfvos,
                ..
            },
        ) = best?
        else {
            return None;
        };
        let Value::Num(v) = cell_value_at(wb, sheet, row, col) else {
            return None;
        };
        let nums = self.numbers(i);
        let (lo, hi) = (*nums.first()?, *nums.last()?);
        let anchor = s.cond_formats[i]
            .ranges
            .iter()
            .fold((u32::MAX, u32::MAX), |(r, c), &(r1, c1, ..)| {
                (r.min(r1), c.min(c1))
            });
        let threshold = |c: &crate::sheet::Cfvo| -> Option<f64> {
            let num = || match c.val.trim().parse::<f64>() {
                Ok(n) => Some(n),
                Err(_) => match eval_cf(wb, sheet, row, col, anchor, &c.val) {
                    Value::Num(n) => Some(n),
                    _ => None,
                },
            };
            match c.kind.as_str() {
                "num" | "formula" => num(),
                "percent" => Some(lo + (hi - lo) * num()? / 100.0),
                "percentile" => Some(percentile(&nums, num()? / 100.0)),
                "min" => Some(lo),
                "max" => Some(hi),
                _ => None,
            }
        };
        let n = cfvos.len();
        // The highest band whose threshold the value reaches; the first cfvo
        // is the floor of band 0.
        let mut band = 0;
        for (k, c) in cfvos.iter().enumerate().skip(1) {
            let Some(t) = threshold(c) else { continue };
            if if c.gte { v >= t } else { v > t } {
                band = k;
            }
        }
        let id = if *reverse {
            n.saturating_sub(1) - band
        } else {
            band
        };
        Some((set.clone(), id as u32))
    }
}

/// Excel's `PERCENTILE.INC` of sorted `nums` at `p` (0..=1).
fn percentile(nums: &[f64], p: f64) -> f64 {
    if nums.is_empty() {
        return 0.0;
    }
    let rank = p.clamp(0.0, 1.0) * (nums.len() - 1) as f64;
    let (i, frac) = (rank.floor() as usize, rank.fract());
    match nums.get(i + 1) {
        Some(next) => nums[i] + (next - nums[i]) * frac,
        None => nums[i],
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
            ix: None,
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
            ix: None,
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
            ix: None,
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
            ix: None,
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
            ix: None,
            ranges: vec![(0, 0, 9, 0)],
            rules: vec![CfRule {
                kind: CfKind::Other { formulas: vec![] },
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
            ix: None,
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
            ix: None,
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

    fn icon_wb(vals: &[f64], set: &str, reverse: bool, cfvos: &[(&str, &str)]) -> Workbook {
        let cells: Vec<(String, f64)> = vals
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("A{}", i + 1), *v))
            .collect();
        let cells: Vec<(&str, f64)> = cells.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        let cf = CondFormat {
            ix: None,
            ranges: vec![(0, 0, 99, 0)],
            rules: vec![CfRule {
                kind: CfKind::IconSet {
                    set: set.into(),
                    reverse,
                    cfvos: cfvos
                        .iter()
                        .map(|(k, v)| crate::sheet::Cfvo {
                            kind: (*k).into(),
                            val: (*v).into(),
                            gte: true,
                        })
                        .collect(),
                    formulas: Vec::new(),
                },
                dxf_id: None,
                priority: 1,
            }],
        };
        wb_with_cf(&cells, cf, Vec::new())
    }

    fn icons(wb: &Workbook, n: u32) -> Vec<u32> {
        (0..n).map(|r| cell_icon(wb, 0, r, 0).unwrap().1).collect()
    }

    #[test]
    fn icon_sets_by_percent_percentile_and_number() {
        // Excel's default 3Arrows thresholds: 33% and 67% of the range.
        let vals = [0.0, 10.0, 33.0, 34.0, 66.0, 67.0, 100.0];
        let wb = icon_wb(
            &vals,
            "3Arrows",
            false,
            &[("percent", "0"), ("percent", "33"), ("percent", "67")],
        );
        assert_eq!(icons(&wb, 7), vec![0, 0, 1, 1, 1, 2, 2]);
        // Reversed, the top values get the first icon.
        let wb = icon_wb(
            &vals,
            "3Arrows",
            true,
            &[("percent", "0"), ("percent", "33"), ("percent", "67")],
        );
        assert_eq!(icons(&wb, 7), vec![2, 2, 1, 1, 1, 0, 0]);
        // Percentile of 1..=5: the 50th is 3.
        let wb = icon_wb(
            &[1.0, 2.0, 3.0, 4.0, 5.0],
            "3TrafficLights1",
            false,
            &[("percent", "0"), ("percentile", "50"), ("num", "5")],
        );
        assert_eq!(icons(&wb, 5), vec![0, 0, 1, 1, 2]);
        // Text and blanks get no icon.
        let mut wb = icon_wb(&[1.0], "3Arrows", false, &[("percent", "0"), ("num", "1")]);
        wb.sheets[0].set_cell(1, 0, Cell::text("x"));
        assert_eq!(cell_icon(&wb, 0, 1, 0), None);
        assert_eq!(cell_icon(&wb, 0, 5, 0), None);
        assert_eq!(cell_icon(&wb, 0, 0, 1), None);
    }

    #[test]
    fn displayed_colours_come_from_conditional_formatting_first() {
        let red = Dxf {
            fill: Some((255, 0, 0)),
            color: Some((0, 0, 255)),
            ..Dxf::default()
        };
        let mut wb = wb_with_cf(
            &[("A1", 10.0), ("A2", 3.0)],
            cell_is("greaterThan", &["5"]),
            vec![red],
        );
        wb.styles.xfs.push(crate::sheet::Xf::default());
        let green = wb.styles.intern(crate::sheet::Xf {
            fill: Some((0, 176, 80)),
            ..crate::sheet::Xf::default()
        });
        let themed = wb.styles.intern(crate::sheet::Xf {
            fill_unresolved: true,
            color_unresolved: true,
            ..crate::sheet::Xf::default()
        });
        wb.sheets[0].cells.get_mut(&(1, 0)).unwrap().style = green;
        wb.sheets[0].set_cell(
            2,
            0,
            Cell {
                style: themed,
                ..Cell::number(1.0)
            },
        );
        assert_eq!(cell_fill(&wb, 0, 0, 0), Shown::Rgb((255, 0, 0)));
        assert_eq!(cell_font_color(&wb, 0, 0, 0), Shown::Rgb((0, 0, 255)));
        assert_eq!(cell_fill(&wb, 0, 1, 0), Shown::Rgb((0, 176, 80)));
        assert_eq!(cell_font_color(&wb, 0, 1, 0), Shown::None);
        assert_eq!(cell_fill(&wb, 0, 2, 0), Shown::Unknown);
        assert!(!cell_fill(&wb, 0, 2, 0).is(None));
        assert!(cell_fill(&wb, 0, 9, 9).is(None));
    }
}
