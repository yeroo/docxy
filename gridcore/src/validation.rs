//! Data validation: checking an entry against the rule of its cell, the
//! cells whose current value breaks one, and editing the rules themselves.
//! Pure model work: the hosts own the alerts, the dialog and the circles.

use crate::engine::{cell_value_at, eval_formula_at};
use crate::formula::{Value, translate_formula};
use crate::sheet::{AlertStyle, Cell, CellValue, DataValidation, Sheet, Workbook};

/// The title of an alert whose rule gives none. Excel's own is "Microsoft
/// Excel"; this one is product-neutral.
pub const DEFAULT_ALERT_TITLE: &str = "Data Validation";

/// The message of an alert whose rule gives none (Excel's text).
pub const DEFAULT_ALERT_MESSAGE: &str =
    "This value doesn't match the data validation restrictions defined for this cell.";

/// An entry that breaks its cell's rule, with the alert to show for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub style: AlertStyle,
    pub title: String,
    pub message: String,
}

/// The rule covering (row, col): the first one in the sheet's order.
pub fn validation_at(sheet: &Sheet, row: u32, col: u32) -> Option<&DataValidation> {
    sheet.validations.iter().find(|dv| dv.covers(row, col))
}

/// The top-left cell of a rule's ranges (the minimum row and column over all
/// of them): the cell its formulas are written for, as conditional
/// formatting's are.
pub fn anchor(dv: &DataValidation) -> (u32, u32) {
    dv.ranges
        .iter()
        .fold((u32::MAX, u32::MAX), |(r, c), &(r1, c1, ..)| {
            (r.min(r1), c.min(c1))
        })
}

/// Check `cell`, about to be entered at (row, col) of `sheet`, against the
/// rule that covers it. `None` when it passes, when no rule covers the cell
/// or when the rule doesn't show an error alert. A formula is checked by its
/// result. A custom rule reads the entry through its own cell, so the cell
/// is put in `wb` while the rule is evaluated and the old one put back.
pub fn check_entry(
    wb: &mut Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    cell: &Cell,
) -> Option<Violation> {
    let dv = validation_at(wb.sheets.get(sheet)?, row, col)?.clone();
    if !dv.show_error {
        return None;
    }
    let value = match &cell.formula {
        Some(f) => to_cell_value(eval_formula_at(wb, sheet, row, col, f)),
        None => cell.value.clone(),
    };
    let custom = dv.kind == "custom";
    let held = custom.then(|| wb.sheets[sheet].cells.remove(&(row, col)));
    if custom {
        wb.sheets[sheet].cells.insert((row, col), cell.clone());
    }
    let mut lists = Lists::default();
    let broke = breaks(wb, sheet, row, col, &dv, &value, &mut lists);
    if let Some(held) = held {
        let cells = &mut wb.sheets[sheet].cells;
        match held {
            Some(old) => cells.insert((row, col), old),
            None => cells.remove(&(row, col)),
        };
    }
    broke.then(|| violation(&dv))
}

fn violation(dv: &DataValidation) -> Violation {
    let or = |s: &str, d: &str| {
        if s.is_empty() {
            d.to_string()
        } else {
            s.to_string()
        }
    };
    Violation {
        style: dv.error_style,
        title: or(&dv.error_title, DEFAULT_ALERT_TITLE),
        message: or(&dv.error, DEFAULT_ALERT_MESSAGE),
    }
}

/// Every cell of `sheet` holding a value that breaks its rule, in row order:
/// typed, pasted or calculated. A cell with no value is not circled, and a
/// rule that shows no error alert still has its cells circled.
pub fn invalid_cells(wb: &Workbook, sheet: usize) -> Vec<(u32, u32)> {
    let Some(s) = wb.sheets.get(sheet) else {
        return Vec::new();
    };
    let mut lists = Lists::default();
    let mut out = Vec::new();
    if s.validations.is_empty() {
        return out;
    }
    for (&(r, c), cell) in &s.cells {
        let Some(dv) = validation_at(s, r, c) else {
            continue;
        };
        if cell.formula.is_none() && cell.value.is_empty() {
            continue;
        }
        let value = to_cell_value(cell_value_at(wb, sheet, r, c));
        if value.is_empty() {
            continue;
        }
        if breaks(wb, sheet, r, c, dv, &value, &mut lists) {
            out.push((r, c));
        }
    }
    out
}

fn to_cell_value(v: Value) -> CellValue {
    match v {
        Value::Empty => CellValue::Empty,
        Value::Num(n) => CellValue::Number(n),
        Value::Str(s) => CellValue::Text(s),
        Value::Bool(b) => CellValue::Bool(b),
        Value::Err(e) => CellValue::Error(e.code().to_string()),
    }
}

/// A rule's evaluated list source, by the reference it resolved to.
#[derive(Default)]
struct Lists(std::collections::HashMap<String, Vec<CellValue>>);

/// The text a value is compared as against a list item.
fn text_of(v: &CellValue) -> String {
    match v {
        CellValue::Text(s) | CellValue::Error(s) => s.clone(),
        CellValue::Number(n) => format!("{n}"),
        CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        CellValue::Empty => String::new(),
    }
}

/// The items of a `list` rule at (row, col): the inline CSV, or the cells of
/// the range or defined name its formula points at. `None` when it can't be
/// read as either.
fn list_items<'a>(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    dv: &DataValidation,
    cache: &'a mut Lists,
    inline: &'a mut Vec<CellValue>,
) -> Option<(&'a [CellValue], bool)> {
    if let Some(items) = dv.list_values() {
        *inline = items.into_iter().map(CellValue::Text).collect();
        return Some((inline, true));
    }
    let (ar, ac) = anchor(dv);
    let src = dv.formula1.trim().trim_start_matches('=');
    let shifted = if (row, col) == (ar, ac) || ar == u32::MAX {
        src.to_string()
    } else {
        translate_formula(
            src,
            i64::from(row) - i64::from(ar),
            i64::from(col) - i64::from(ac),
        )
        .unwrap_or_else(|| src.to_string())
    };
    let key = format!("{sheet}!{shifted}");
    if !cache.0.contains_key(&key) {
        let items = resolve_ref(wb, sheet, &shifted)?;
        cache.0.insert(key.clone(), items);
    }
    cache.0.get(&key).map(|v| (v.as_slice(), false))
}

/// The non-blank values of the range `src` names (a defined name too).
fn resolve_ref(wb: &Workbook, sheet: usize, src: &str) -> Option<Vec<CellValue>> {
    let named = wb
        .defined_names
        .iter()
        .find(|d| d.name.eq_ignore_ascii_case(src))
        .map(|d| d.formula.trim_start_matches('=').to_string());
    let src = named.as_deref().unwrap_or(src);
    let (name, range) = match src.rsplit_once('!') {
        Some((n, r)) => (Some(n.trim_matches(['\'', ' '])), r),
        None => (None, src),
    };
    let at = match name {
        Some(n) => wb
            .sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(n))?,
        None => sheet,
    };
    let clean = range.replace('$', "");
    let (r1, c1, r2, c2) = crate::sheet::parse_range_name(&clean)
        .or_else(|| crate::sheet::parse_cell_name(&clean).map(|(r, c)| (r, c, r, c)))?;
    let s = wb.sheets.get(at)?;
    let mut out = Vec::new();
    for (&(r, c), _) in s.cells.range((r1, 0)..=(r2, u32::MAX)) {
        if c < c1 || c > c2 {
            continue;
        }
        let v = to_cell_value(cell_value_at(wb, at, r, c));
        if !v.is_empty() {
            out.push(v);
        }
    }
    Some(out)
}

/// Evaluate one of a rule's formulas for the cell (row, col), shifted from
/// the rule's anchor.
fn eval_rule(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    dv: &DataValidation,
    src: &str,
) -> Value {
    let (ar, ac) = anchor(dv);
    let (dr, dc) = (
        i64::from(row) - i64::from(ar),
        i64::from(col) - i64::from(ac),
    );
    let src = src.trim().trim_start_matches('=');
    let shifted = if ar == u32::MAX || (dr, dc) == (0, 0) {
        src.to_string()
    } else {
        translate_formula(src, dr, dc).unwrap_or_else(|| src.to_string())
    };
    eval_formula_at(wb, sheet, row, col, &shifted)
}

/// Does `value` at (row, col) break `dv`?
fn breaks(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    dv: &DataValidation,
    value: &CellValue,
    lists: &mut Lists,
) -> bool {
    if value.is_empty() {
        return !dv.allow_blank;
    }
    let bound = |src: &str| match eval_rule(wb, sheet, row, col, dv, src) {
        Value::Num(n) => Some(n),
        Value::Bool(b) => Some(f64::from(u8::from(b))),
        _ => None,
    };
    match dv.kind.as_str() {
        "whole" | "decimal" | "date" | "time" => {
            let CellValue::Number(n) = value else {
                return true;
            };
            if dv.kind == "whole" && n.fract() != 0.0 {
                return true;
            }
            outside(dv, *n, bound(&dv.formula1), bound(&dv.formula2))
        }
        "textLength" => {
            if matches!(value, CellValue::Error(_)) {
                return true;
            }
            let len = text_of(value).chars().count() as f64;
            outside(dv, len, bound(&dv.formula1), bound(&dv.formula2))
        }
        "list" => {
            if matches!(value, CellValue::Error(_)) {
                return true;
            }
            let mut inline = Vec::new();
            let Some((items, is_inline)) = list_items(wb, sheet, row, col, dv, lists, &mut inline)
            else {
                // A list that can't be read allows anything, as Excel does.
                return false;
            };
            let text = text_of(value);
            !items.iter().any(|item| match (item, value) {
                (CellValue::Number(a), CellValue::Number(b)) => a == b,
                _ if is_inline => text_of(item).trim() == text,
                _ => text_of(item).eq_ignore_ascii_case(&text),
            })
        }
        "custom" => match eval_rule(wb, sheet, row, col, dv, &dv.formula1) {
            Value::Bool(b) => !b,
            Value::Num(n) => n == 0.0,
            _ => true,
        },
        _ => false,
    }
}

/// Is `n` outside what the rule's operator allows against the bounds? A
/// bound that isn't a number can't be checked, so nothing breaks.
fn outside(dv: &DataValidation, n: f64, b1: Option<f64>, b2: Option<f64>) -> bool {
    let Some(a) = b1 else {
        return false;
    };
    let ok = match dv.operator.as_str() {
        "notBetween" => match b2 {
            Some(b) => n < a.min(b) || n > a.max(b),
            None => return false,
        },
        "equal" => n == a,
        "notEqual" => n != a,
        "greaterThan" => n > a,
        "lessThan" => n < a,
        "greaterThanOrEqual" => n >= a,
        "lessThanOrEqual" => n <= a,
        // "between", and the default when the operator is omitted.
        _ => match b2 {
            Some(b) => n >= a.min(b) && n <= a.max(b),
            None => return false,
        },
    };
    !ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::entry_cell;

    fn book() -> Workbook {
        let mut wb = Workbook::default();
        wb.sheets.push(Sheet::default());
        wb
    }

    fn rule(kind: &str, op: &str, f1: &str, f2: &str) -> DataValidation {
        DataValidation {
            ranges: vec![(1, 1, 9, 1)], // B2:B10
            kind: kind.into(),
            operator: op.into(),
            formula1: f1.into(),
            formula2: f2.into(),
            allow_blank: true,
            show_error: true,
            error_title: "Score".into(),
            error: "10 to 90 only".into(),
            ..Default::default()
        }
    }

    fn check(wb: &mut Workbook, r: u32, c: u32, text: &str) -> Option<Violation> {
        let cell = entry_cell(wb, 0, r, c, text, None).unwrap();
        check_entry(wb, 0, r, c, &cell)
    }

    #[test]
    fn whole_between_refuses_the_issue_entries() {
        let mut wb = book();
        wb.sheets[0]
            .validations
            .push(rule("whole", "between", "10", "90"));
        let v = check(&mut wb, 1, 1, "250").expect("250 refused");
        assert_eq!(
            (v.title.as_str(), v.message.as_str()),
            ("Score", "10 to 90 only")
        );
        assert_eq!(v.style, AlertStyle::Stop);
        assert!(check(&mut wb, 2, 1, "45.5").is_some());
        assert!(check(&mut wb, 2, 1, "abc").is_some());
        assert!(check(&mut wb, 2, 1, "10").is_none());
        assert!(check(&mut wb, 2, 1, "90").is_none());
        // Outside the rule's range: anything goes.
        assert!(check(&mut wb, 20, 1, "250").is_none());
    }

    #[test]
    fn all_numeric_operators() {
        for (op, f2, pass, fail) in [
            ("between", "9", "5", "10"),
            ("notBetween", "9", "10", "5"),
            ("equal", "", "5", "6"),
            ("notEqual", "", "6", "5"),
            ("greaterThan", "", "6", "5"),
            ("lessThan", "", "4", "5"),
            ("greaterThanOrEqual", "", "5", "4"),
            ("lessThanOrEqual", "", "5", "6"),
        ] {
            let mut wb = book();
            let lo = if op == "between" || op == "notBetween" {
                "3"
            } else {
                "5"
            };
            wb.sheets[0].validations.push(rule("decimal", op, lo, f2));
            assert!(check(&mut wb, 1, 1, pass).is_none(), "{op} passes {pass}");
            assert!(check(&mut wb, 1, 1, fail).is_some(), "{op} refuses {fail}");
        }
    }

    #[test]
    fn decimal_date_time_and_text_length() {
        let mut wb = book();
        wb.sheets[0]
            .validations
            .push(rule("decimal", "greaterThan", "0.5", ""));
        assert!(check(&mut wb, 1, 1, "0.4").is_some());
        assert!(check(&mut wb, 1, 1, "0.6").is_none());

        let mut wb = book();
        // 2020-01-01 is serial 43831.
        wb.sheets[0]
            .validations
            .push(rule("date", "greaterThanOrEqual", "43831", ""));
        assert!(check(&mut wb, 1, 1, "2019-12-31").is_some());
        assert!(check(&mut wb, 1, 1, "2020-01-01").is_none());

        let mut wb = book();
        wb.sheets[0]
            .validations
            .push(rule("time", "lessThan", "0.5", ""));
        assert!(check(&mut wb, 1, 1, "13:00").is_some());
        assert!(check(&mut wb, 1, 1, "11:00").is_none());

        let mut wb = book();
        wb.sheets[0]
            .validations
            .push(rule("textLength", "lessThanOrEqual", "3", ""));
        assert!(check(&mut wb, 1, 1, "abcd").is_some());
        assert!(check(&mut wb, 1, 1, "abc").is_none());
        assert!(check(&mut wb, 1, 1, "1234").is_some());
    }

    #[test]
    fn bounds_may_be_formulas_and_cell_refs() {
        let mut wb = book();
        let s = &mut wb.sheets[0];
        s.set_cell(0, 3, Cell::number(100.0)); // D1
        s.validations
            .push(rule("whole", "lessThanOrEqual", "$D$1*2", ""));
        assert!(check(&mut wb, 1, 1, "200").is_none());
        assert!(check(&mut wb, 1, 1, "201").is_some());
    }

    #[test]
    fn inline_list_is_case_sensitive_and_range_list_is_not() {
        let mut wb = book();
        wb.sheets[0]
            .validations
            .push(rule("list", "", "\"Yes,No\"", ""));
        assert!(check(&mut wb, 1, 1, "Yes").is_none());
        assert!(check(&mut wb, 1, 1, "yes").is_some());
        assert!(check(&mut wb, 1, 1, "Maybe").is_some());

        let mut wb = book();
        let s = &mut wb.sheets[0];
        s.set_cell(0, 5, Cell::text("Red"));
        s.set_cell(1, 5, Cell::text("Green"));
        s.validations.push(rule("list", "", "$F$1:$F$2", ""));
        assert!(check(&mut wb, 1, 1, "Green").is_none());
        assert!(check(&mut wb, 1, 1, "green").is_none());
        assert!(check(&mut wb, 1, 1, "Blue").is_some());
    }

    #[test]
    fn list_from_a_defined_name_and_another_sheet() {
        let mut wb = book();
        let mut other = Sheet::default();
        other.name = "Lists".into();
        other.set_cell(0, 0, Cell::number(1.0));
        other.set_cell(1, 0, Cell::number(2.0));
        wb.sheets.push(other);
        wb.sheets[0].name = "Main".into();
        wb.defined_names.push(crate::sheet::DefinedName {
            name: "Nums".into(),
            scope: None,
            formula: "Lists!$A$1:$A$2".into(),
        });
        wb.sheets[0].validations.push(rule("list", "", "Nums", ""));
        assert!(check(&mut wb, 1, 1, "2").is_none());
        assert!(check(&mut wb, 1, 1, "3").is_some());
        wb.sheets[0].validations[0].formula1 = "Lists!$A$1:$A$2".into();
        assert!(check(&mut wb, 1, 1, "1").is_none());
        assert!(check(&mut wb, 1, 1, "9").is_some());
    }

    #[test]
    fn custom_formula_is_relative_to_the_rules_anchor() {
        let mut wb = book();
        let s = &mut wb.sheets[0];
        s.set_cell(4, 0, Cell::number(10.0)); // A5
        s.validations.push(rule("custom", "", "B2>A2", ""));
        // B5 is checked as B5>A5.
        assert!(check(&mut wb, 4, 1, "11").is_none());
        assert!(check(&mut wb, 4, 1, "9").is_some());
        // B2 against A2 (empty).
        assert!(check(&mut wb, 1, 1, "1").is_none());
    }

    #[test]
    fn blank_follows_allow_blank_and_show_error_off_lets_anything_in() {
        let mut wb = book();
        let mut dv = rule("whole", "between", "10", "90");
        dv.allow_blank = false;
        wb.sheets[0].validations.push(dv);
        let blank = Cell::default();
        assert!(check_entry(&mut wb, 0, 1, 1, &blank).is_some());
        wb.sheets[0].validations[0].allow_blank = true;
        assert!(check_entry(&mut wb, 0, 1, 1, &blank).is_none());
        wb.sheets[0].validations[0].show_error = false;
        assert!(check(&mut wb, 1, 1, "250").is_none());
    }

    #[test]
    fn default_alert_text_and_styles() {
        let mut wb = book();
        let mut dv = rule("whole", "between", "10", "90");
        dv.error_title.clear();
        dv.error.clear();
        dv.error_style = AlertStyle::Warning;
        wb.sheets[0].validations.push(dv);
        let v = check(&mut wb, 1, 1, "1").unwrap();
        assert_eq!(v.title, DEFAULT_ALERT_TITLE);
        assert_eq!(v.message, DEFAULT_ALERT_MESSAGE);
        assert_eq!(v.style, AlertStyle::Warning);
    }

    #[test]
    fn a_formula_entry_is_checked_by_its_result() {
        let mut wb = book();
        wb.sheets[0]
            .validations
            .push(rule("whole", "between", "10", "90"));
        assert!(check(&mut wb, 1, 1, "=50*2").is_some());
        assert!(check(&mut wb, 1, 1, "=50+1").is_none());
    }

    #[test]
    fn invalid_cells_finds_typed_pasted_and_calculated_values() {
        let mut wb = book();
        let s = &mut wb.sheets[0];
        s.validations.push(rule("whole", "between", "10", "90"));
        s.set_cell(1, 1, Cell::number(50.0)); // B2 fine
        s.set_cell(2, 1, Cell::number(250.0)); // B3 pasted in
        s.set_cell(3, 1, Cell::text("x")); // B4
        let mut calc = Cell::default();
        calc.formula = Some("A1*1000".into());
        s.set_cell(4, 1, calc); // B5 calculated (A1 empty -> 0)
        s.set_cell(20, 1, Cell::number(250.0)); // outside the rule
        let mut engine = crate::engine::Engine::new(&wb);
        engine.recalc_all(&mut wb);
        assert_eq!(invalid_cells(&wb, 0), vec![(2, 1), (3, 1), (4, 1)]);
        // Circled whatever the rule's alert setting.
        wb.sheets[0].validations[0].show_error = false;
        assert_eq!(invalid_cells(&wb, 0).len(), 3);
    }
}
