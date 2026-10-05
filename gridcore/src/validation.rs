//! Data validation: checking an entry against the rule of its cell, the
//! cells whose current value breaks one, and editing the rules themselves.
//! Pure model work: the hosts own the alerts, the dialog and the circles.

use crate::engine::{cell_value_at, eval_formula_at};
use crate::formula::{Value, translate_formula};
use crate::sheet::{
    AlertStyle, Cell, CellValue, DataValidation, MAX_COLS, MAX_ROWS, Sheet, Workbook,
};

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

// --- what a Data Validation dialog offers ------------------------------

/// Excel's limit on an input or error title.
pub const TITLE_MAX: usize = 32;
/// Excel's limit on an input or error message.
pub const MESSAGE_MAX: usize = 255;

/// "Allow:" choices, in Excel's order: the rule's `type` (empty is Any value)
/// and its name.
pub const KINDS: [(&str, &str); 8] = [
    ("", "Any value"),
    ("whole", "Whole number"),
    ("decimal", "Decimal"),
    ("list", "List"),
    ("date", "Date"),
    ("time", "Time"),
    ("textLength", "Text length"),
    ("custom", "Custom"),
];

/// "Data:" choices, in Excel's order: the `operator` and its name.
pub const OPERATORS: [(&str, &str); 8] = [
    ("between", "between"),
    ("notBetween", "not between"),
    ("equal", "equal to"),
    ("notEqual", "not equal to"),
    ("greaterThan", "greater than"),
    ("lessThan", "less than"),
    ("greaterThanOrEqual", "greater than or equal to"),
    ("lessThanOrEqual", "less than or equal to"),
];

/// Does a rule of this `type` compare against bounds (so offers a "Data:"
/// operator)?
pub fn takes_operator(kind: &str) -> bool {
    matches!(kind, "whole" | "decimal" | "date" | "time" | "textLength")
}

/// Does `kind` with `operator` need two bounds?
pub fn takes_two(kind: &str, operator: &str) -> bool {
    takes_operator(kind) && matches!(operator, "between" | "notBetween")
}

/// The caption of the first bound's box.
pub fn first_label(kind: &str, operator: &str) -> &'static str {
    match kind {
        "list" => "Source:",
        "custom" => "Formula:",
        _ if takes_two(kind, operator) => "Minimum:",
        "date" => "Date:",
        "time" => "Time:",
        "textLength" => "Length:",
        _ => "Value:",
    }
}

/// What the box for a rule's first formula shows: an inline list as its
/// items, a reference or any formula behind `=` (a list) or as it is.
pub fn first_box(dv: &DataValidation) -> String {
    if dv.kind == "list" {
        return match dv.list_values() {
            Some(items) => items.join(","),
            None => format!("={}", dv.formula1),
        };
    }
    dv.formula1.clone()
}

/// The `(formula1, formula2)` of a rule from the dialog's boxes: a list's
/// text is its items (`Yes, No`) or, behind `=`, a reference or name; any
/// other `=` is the formula bar's and is dropped. The reason a box is
/// missing otherwise.
pub fn formulas_from_boxes(
    kind: &str,
    operator: &str,
    first: &str,
    second: &str,
) -> Result<(String, String), String> {
    let first = first.trim();
    if first.is_empty() {
        return Err(match kind {
            "list" => "Data validation: enter the list's source".to_string(),
            "custom" => "Data validation: enter a formula".to_string(),
            _ if takes_two(kind, operator) => "Data validation: enter a minimum".to_string(),
            _ => "Data validation: enter a value".to_string(),
        });
    }
    let bare = |t: &str| t.strip_prefix('=').unwrap_or(t).trim().to_string();
    let f1 = if kind == "list" {
        match first.strip_prefix('=') {
            Some(f) => f.trim().to_string(),
            None => {
                let items: Vec<&str> = first
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                if items.is_empty() {
                    return Err("Data validation: enter the list's source".to_string());
                }
                format!("\"{}\"", items.join(","))
            }
        }
    } else {
        bare(first)
    };
    if !takes_two(kind, operator) {
        return Ok((f1, String::new()));
    }
    let second = second.trim();
    if second.is_empty() {
        return Err("Data validation: enter a maximum".to_string());
    }
    Ok((f1, bare(second)))
}

// --- editing the rules -------------------------------------------------

type Rect = (u32, u32, u32, u32);

/// `a` without the cells of `cut`: the pieces above, below, left and right
/// of it, in that order (`B2:B10` without `B6` is `B2:B5 B7:B10`).
fn subtract(a: Rect, cut: Rect) -> Vec<Rect> {
    let (r1, c1, r2, c2) = a;
    let (x1, y1, x2, y2) = cut;
    if x2 < r1 || x1 > r2 || y2 < c1 || y1 > c2 {
        return vec![a];
    }
    let mut out = Vec::new();
    if x1 > r1 {
        out.push((r1, c1, x1 - 1, c2));
    }
    if x2 < r2 {
        out.push((x2 + 1, c1, r2, c2));
    }
    let (mid1, mid2) = (r1.max(x1), r2.min(x2));
    if y1 > c1 {
        out.push((mid1, c1, mid2, y1 - 1));
    }
    if y2 < c2 {
        out.push((mid1, y2 + 1, mid2, c2));
    }
    out
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let (r1, c1, r2, c2) = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (r1 <= r2 && c1 <= c2).then_some((r1, c1, r2, c2))
}

/// The ranges of `ranges` that no other one contains, in order.
fn dedupe(ranges: Vec<Rect>) -> Vec<Rect> {
    let inside = |a: &Rect, b: &Rect| a.0 >= b.0 && a.1 >= b.1 && a.2 <= b.2 && a.3 <= b.3;
    let mut out: Vec<Rect> = Vec::new();
    for (i, r) in ranges.iter().enumerate() {
        let dup = ranges
            .iter()
            .enumerate()
            .any(|(j, o)| i != j && inside(r, o) && (r != o || j < i));
        if !dup {
            out.push(*r);
        }
    }
    out
}

/// Take every rule off the cells of `rect`: ranges are split around it and a
/// rule left with none goes, its element named in `dv_removed` for the save.
pub fn clear_validation(sheet: &mut Sheet, rect: Rect) {
    let removed = &mut sheet.dv_removed;
    sheet.validations.retain_mut(|dv| {
        if !dv.ranges.iter().any(|&r| intersect(r, rect).is_some()) {
            return true;
        }
        dv.ranges = dv.ranges.iter().flat_map(|&r| subtract(r, rect)).collect();
        if dv.ranges.is_empty() {
            removed.extend(dv.ix);
            return false;
        }
        true
    });
}

/// Give `rule`'s settings to `ranges`: onto an existing rule with the same
/// settings (one `sqref` list), else as a new rule. A rule that imposes
/// nothing ([`DataValidation::is_meaningful`]) adds nothing. The ranges must
/// already be free of other rules ([`clear_validation`]).
pub fn add_ranges(sheet: &mut Sheet, rule: &DataValidation, ranges: &[Rect]) {
    if ranges.is_empty() || !rule.is_meaningful() {
        return;
    }
    match sheet
        .validations
        .iter_mut()
        .find(|dv| dv.same_settings(rule))
    {
        Some(dv) => {
            dv.ranges.extend_from_slice(ranges);
            dv.ranges = dedupe(std::mem::take(&mut dv.ranges));
        }
        None => {
            let mut dv = rule.clone();
            dv.ranges = dedupe(ranges.to_vec());
            dv.ix = None;
            dv.orig = None;
            sheet.validations.push(dv);
        }
    }
}

/// The Data Validation dialog's OK: `rule`'s settings on `range`. With
/// `apply_to_all`, every cell range whose rule has the same settings as the
/// one the range's top-left cell holds now takes them too. "Any value"
/// without messages ([`DataValidation::is_meaningful`]) leaves the cells
/// with no rule.
pub fn set_validation(sheet: &mut Sheet, range: Rect, rule: &DataValidation, apply_to_all: bool) {
    let base = validation_at(sheet, range.0, range.1).cloned();
    let mut ranges = vec![range];
    if let (true, Some(b)) = (apply_to_all, &base) {
        for dv in sheet.validations.iter().filter(|dv| dv.same_settings(b)) {
            ranges.extend(dv.ranges.iter().copied());
        }
    }
    let ranges = dedupe(ranges);
    // The rule being rewritten whole keeps its element (and any attribute
    // this code doesn't know): when everything it covers is covered again.
    let keep = base
        .as_ref()
        .filter(|b| b.ranges.iter().all(|r| ranges.contains(r)))
        .and_then(|b| {
            sheet
                .validations
                .iter()
                .position(|dv| dv.ix == b.ix && dv.same_settings(b))
        });
    let kept_ix = keep.map(|i| sheet.validations[i].ix);
    let held = keep.map(|i| sheet.validations.remove(i));
    for &r in &ranges {
        clear_validation(sheet, r);
    }
    if !rule.is_meaningful() {
        if let Some(h) = held {
            sheet.dv_removed.extend(h.ix);
        }
        return;
    }
    match held {
        Some(mut h) => {
            h.ranges = ranges;
            h.kind.clone_from(&rule.kind);
            h.operator.clone_from(&rule.operator);
            h.formula1.clone_from(&rule.formula1);
            h.formula2.clone_from(&rule.formula2);
            h.prompt.clone_from(&rule.prompt);
            h.prompt_title.clone_from(&rule.prompt_title);
            h.allow_blank = rule.allow_blank;
            h.show_input = rule.show_input;
            h.show_error = rule.show_error;
            h.show_dropdown = rule.show_dropdown;
            h.error_style = rule.error_style;
            h.error_title.clone_from(&rule.error_title);
            h.error.clone_from(&rule.error);
            h.ix = kept_ix.flatten();
            sheet.validations.push(h);
        }
        None => add_ranges(sheet, rule, &ranges),
    }
}

/// The dialog's Clear All: the cells of `range` lose their rule. With
/// `apply_to_all`, so does every cell with the same settings as the one the
/// range's top-left cell holds.
pub fn clear_all_validation(sheet: &mut Sheet, range: Rect, apply_to_all: bool) {
    let mut ranges = vec![range];
    if let (true, Some(b)) = (
        apply_to_all,
        validation_at(sheet, range.0, range.1).cloned(),
    ) {
        for dv in sheet.validations.iter().filter(|dv| dv.same_settings(&b)) {
            ranges.extend(dv.ranges.iter().copied());
        }
    }
    for r in ranges {
        clear_validation(sheet, r);
    }
}

/// The rules that cover cells of `rect` on `sheet`, their ranges cut to it:
/// what a copy or cut takes along for the paste.
pub fn copy_rules(sheet: &Sheet, rect: Rect) -> Vec<DataValidation> {
    sheet
        .validations
        .iter()
        .filter_map(|dv| {
            let ranges: Vec<Rect> = dv
                .ranges
                .iter()
                .filter_map(|&r| intersect(r, rect))
                .collect();
            if ranges.is_empty() {
                return None;
            }
            let mut piece = dv.clone();
            // Formulas are written for the whole rule's anchor; the piece
            // keeps that meaning by recording how far its own corner is.
            let (ar, ac) = anchor(dv);
            let (pr, pc) = ranges
                .iter()
                .fold((u32::MAX, u32::MAX), |(r, c), &(r1, c1, ..)| {
                    (r.min(r1), c.min(c1))
                });
            let (dr, dc) = (i64::from(pr) - i64::from(ar), i64::from(pc) - i64::from(ac));
            if (dr, dc) != (0, 0) {
                for f in [&mut piece.formula1, &mut piece.formula2] {
                    if !f.is_empty() && !f.starts_with('"') {
                        *f = translate_formula(f, dr, dc).unwrap_or_else(|| f.clone());
                    }
                }
            }
            piece.ranges = ranges;
            piece.ix = None;
            piece.orig = None;
            Some(piece)
        })
        .collect()
}

/// Paste `rules` ([`copy_rules`] of `src_rect`) so that `src_rect`'s corner
/// lands on `dst_origin`, tiled `tiles` times (rows, columns). The pasted
/// area loses the rules it had, whether or not the source had any, and each
/// rule's relative formulas move with it.
pub fn paste_rules(
    dst: &mut Sheet,
    rules: &[DataValidation],
    src_rect: Rect,
    dst_origin: (u32, u32),
    tiles: (u32, u32),
) {
    let (h, w) = (src_rect.2 - src_rect.0 + 1, src_rect.3 - src_rect.1 + 1);
    let area = (
        dst_origin.0,
        dst_origin.1,
        dst_origin.0.saturating_add(h * tiles.0).saturating_sub(1),
        dst_origin.1.saturating_add(w * tiles.1).saturating_sub(1),
    );
    clear_validation(dst, area);
    for ti in 0..tiles.0 {
        for tj in 0..tiles.1 {
            let (dr, dc) = (
                i64::from(dst_origin.0) + i64::from(ti * h) - i64::from(src_rect.0),
                i64::from(dst_origin.1) + i64::from(tj * w) - i64::from(src_rect.1),
            );
            for rule in rules {
                let mut moved = rule.clone();
                for f in [&mut moved.formula1, &mut moved.formula2] {
                    if !f.is_empty() && !f.starts_with('"') {
                        *f = translate_formula(f, dr, dc).unwrap_or_else(|| f.clone());
                    }
                }
                let ranges: Vec<Rect> = rule
                    .ranges
                    .iter()
                    .filter_map(|&(r1, c1, r2, c2)| {
                        let shift = |v: u32, d: i64| u32::try_from(i64::from(v) + d).ok();
                        let moved = (
                            shift(r1, dr)?,
                            shift(c1, dc)?,
                            shift(r2, dr)?,
                            shift(c2, dc)?,
                        );
                        // What the grid has room for: a paste cut short at
                        // its edge doesn't name cells past it.
                        intersect(moved, (0, 0, MAX_ROWS - 1, MAX_COLS - 1))
                    })
                    .collect();
                add_ranges(dst, &moved, &ranges);
            }
        }
    }
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

    fn sq(sheet: &Sheet) -> Vec<Vec<Rect>> {
        sheet.validations.iter().map(|d| d.ranges.clone()).collect()
    }

    #[test]
    fn clearing_a_cell_splits_the_rule_around_it() {
        let mut s = Sheet::default();
        s.validations.push(rule("whole", "between", "10", "90"));
        clear_validation(&mut s, (5, 1, 5, 1)); // B6
        assert_eq!(sq(&s), vec![vec![(1, 1, 4, 1), (6, 1, 9, 1)]]);
        // A target covering a whole range removes it; none left removes the rule.
        s.validations[0].ix = Some(3);
        clear_validation(&mut s, (0, 0, 20, 5));
        assert!(s.validations.is_empty());
        assert_eq!(s.dv_removed, vec![3]);
    }

    #[test]
    fn paste_without_a_source_rule_removes_the_targets() {
        let mut dst = Sheet::default();
        dst.validations.push(rule("whole", "between", "10", "90"));
        paste_rules(&mut dst, &[], (1, 7, 1, 7), (5, 1), (1, 1)); // H2 onto B6
        assert_eq!(sq(&dst), vec![vec![(1, 1, 4, 1), (6, 1, 9, 1)]]);
    }

    #[test]
    fn paste_of_a_validated_cell_gives_the_target_an_equal_rule() {
        let mut src = Sheet::default();
        src.validations.push(rule("whole", "between", "10", "90"));
        let rules = copy_rules(&src, (1, 1, 1, 1)); // B2
        let mut dst = Sheet::default();
        paste_rules(&mut dst, &rules, (1, 1, 1, 1), (3, 5), (1, 1)); // onto F4
        assert_eq!(sq(&dst), vec![vec![(3, 5, 3, 5)]]);
        assert!(dst.validations[0].same_settings(&src.validations[0]));
        // Pasting beside an equal rule joins its sqref.
        paste_rules(&mut dst, &rules, (1, 1, 1, 1), (4, 5), (1, 1));
        assert_eq!(dst.validations.len(), 1);
        assert_eq!(dst.validations[0].ranges, vec![(3, 5, 3, 5), (4, 5, 4, 5)]);
    }

    #[test]
    fn tiled_paste_and_relative_formulas() {
        let mut src = Sheet::default();
        let mut dv = rule("custom", "", "B2>A2", "");
        dv.ranges = vec![(1, 1, 1, 1)];
        src.validations.push(dv);
        let rules = copy_rules(&src, (1, 1, 1, 1));
        let mut dst = Sheet::default();
        paste_rules(&mut dst, &rules, (1, 1, 1, 1), (3, 3), (2, 1)); // D4, D5
        assert_eq!(dst.validations.len(), 2);
        assert_eq!(dst.validations[0].formula1, "D4>C4");
        assert_eq!(dst.validations[1].formula1, "D5>C5");
    }

    #[test]
    fn dialog_ok_apply_to_all_and_clear_all() {
        let mut s = Sheet::default();
        let mut a = rule("whole", "between", "10", "90");
        a.ranges = vec![(1, 1, 3, 1), (1, 3, 3, 3)]; // B2:B4 D2:D4
        s.validations.push(a);
        let mut new = rule("whole", "between", "1", "5");
        // Edit B2 only, applying to every cell with the same settings.
        set_validation(&mut s, (1, 1, 1, 1), &new, true);
        assert_eq!(s.validations.len(), 1);
        assert_eq!(s.validations[0].formula1, "1");
        assert_eq!(s.validations[0].ranges.len(), 2);
        // Without apply-to-all only the selection changes.
        new.formula1 = "2".into();
        set_validation(&mut s, (1, 3, 1, 3), &new, false);
        assert_eq!(s.validations.len(), 2);
        assert!(validation_at(&s, 1, 3).unwrap().formula1 == "2");
        assert!(validation_at(&s, 2, 3).unwrap().formula1 == "1");
        // Clear All with apply-to-all clears the other cells with those settings.
        clear_all_validation(&mut s, (2, 3, 2, 3), true);
        assert!(validation_at(&s, 2, 3).is_none());
        assert!(validation_at(&s, 1, 1).is_none());
        assert!(validation_at(&s, 1, 3).is_some());
    }

    #[test]
    fn any_value_without_messages_removes_the_rule() {
        let mut s = Sheet::default();
        s.validations.push(rule("whole", "between", "10", "90"));
        let any = DataValidation::default();
        set_validation(&mut s, (1, 1, 9, 1), &any, false);
        assert!(s.validations.is_empty());
        let mut msg = DataValidation::default();
        msg.prompt = Some("hello".into());
        msg.show_input = true;
        set_validation(&mut s, (1, 1, 2, 1), &msg, false);
        assert_eq!(s.validations[0].kind, "");
        assert!(s.validations[0].is_meaningful());
    }

    #[test]
    fn editing_a_whole_rule_keeps_its_element() {
        let mut s = Sheet::default();
        let mut a = rule("whole", "between", "10", "90");
        a.ix = Some(2);
        a.orig = Some(Box::new(a.clone()));
        s.validations.push(a);
        let mut new = rule("whole", "between", "1", "5");
        new.ranges.clear();
        set_validation(&mut s, (1, 1, 9, 1), &new, false);
        assert_eq!(s.validations.len(), 1);
        assert_eq!(s.validations[0].ix, Some(2));
        assert!(s.dv_removed.is_empty());
        assert_eq!(s.validations[0].formula1, "1");
    }
}
