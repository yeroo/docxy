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
    anchor_of(&dv.ranges)
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
    today: Option<f64>,
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
        // A typed formula has no value yet: the rule reads its result.
        let mut entry = cell.clone();
        entry.value = value.clone();
        wb.sheets[sheet].cells.insert((row, col), entry);
    }
    let mut lists = Lists::at(today);
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
pub fn invalid_cells(wb: &Workbook, sheet: usize, today: Option<f64>) -> Vec<(u32, u32)> {
    let Some(s) = wb.sheets.get(sheet) else {
        return Vec::new();
    };
    let mut lists = Lists::at(today);
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
struct Lists {
    map: std::collections::HashMap<String, Vec<CellValue>>,
    /// The host's clock, which reads a yearless date the way typing does.
    today: Option<f64>,
}

impl Lists {
    fn at(today: Option<f64>) -> Lists {
        Lists {
            map: Default::default(),
            today,
        }
    }
}

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
    let shifted = list_source(dv, row, col);
    let key = format!("{sheet}!{shifted}");
    if !cache.map.contains_key(&key) {
        let items = resolve_ref(wb, sheet, &shifted)?;
        cache.map.insert(key.clone(), items);
    }
    cache.map.get(&key).map(|v| (v.as_slice(), false))
}

/// The sheet and the cells of the range `src` names (a defined name too).
fn resolve_cells(wb: &Workbook, sheet: usize, src: &str) -> Option<(usize, Vec<(u32, u32)>)> {
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
    let cells = s
        .cells
        .range((r1, 0)..=(r2, u32::MAX))
        .map(|(&(r, c), _)| (r, c))
        .filter(|&(_, c)| c >= c1 && c <= c2)
        .collect();
    Some((at, cells))
}

/// The non-blank values of the range `src` names (a defined name too).
fn resolve_ref(wb: &Workbook, sheet: usize, src: &str) -> Option<Vec<CellValue>> {
    let (at, cells) = resolve_cells(wb, sheet, src)?;
    Some(
        cells
            .into_iter()
            .map(|(r, c)| to_cell_value(cell_value_at(wb, at, r, c)))
            .filter(|v| !v.is_empty())
            .collect(),
    )
}

/// The source formula of a list rule for the cell (row, col): its formula
/// without `=`, relative references shifted from the rule's anchor.
fn list_source(dv: &DataValidation, row: u32, col: u32) -> String {
    formula_at(dv, &dv.formula1, row, col)
}

/// A rule's formula `src` (without any `=`) as it reads at (row, col):
/// written for the rule's anchor, its relative references shifted by how far
/// the cell is from it.
fn formula_at(dv: &DataValidation, src: &str, row: u32, col: u32) -> String {
    let (ar, ac) = anchor(dv);
    let src = src.trim().trim_start_matches('=');
    if ar == u32::MAX || (row, col) == (ar, ac) {
        return src.to_string();
    }
    translate_formula(
        src,
        i64::from(row) - i64::from(ar),
        i64::from(col) - i64::from(ac),
    )
    .unwrap_or_else(|| src.to_string())
}

/// One choice of the in-cell dropdown: what it shows, and (for a choice read
/// from a range) the value it stands for, since a label can round (`3.14` for
/// 3.14127). An inline item has no value: picking it enters its label as
/// typing it would, or stores it as text where typing would make a formula or
/// quote-prefixed text ([`pick_cell`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ListChoice {
    pub label: String,
    /// The value of the source cell; `None` for an inline item.
    pub value: Option<CellValue>,
    /// The number-format code of the source cell, when it isn't General
    /// (range choices only).
    pub code: Option<String>,
}

/// The cell picking `choice` puts at (row, col). A range choice puts its value
/// itself (text read back would round a long number and turn `007` into 7)
/// with the cell's own style, and the source's number format when the cell has
/// none, so a picked date shows as a date. An inline item is entered as typing
/// its label would be: under the cell's own style (`@` keeps `001` text, `0%`
/// reads `10` as 10%), today's date for a yearless one; except that an item
/// typing would make a formula or quote-prefixed text (`=1+1`, `'01`) is
/// stored as its text ([`inline_pick`]). Either way the entry check accepts
/// it.
pub fn pick_cell(
    wb: &mut Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    choice: &ListChoice,
    today: Option<f64>,
) -> Result<Cell, crate::entry::EntryError> {
    let value = match &choice.value {
        Some(v) => v.clone(),
        None => {
            // Typing the label, unless that would make a formula or
            // quote-prefixed text: those are stored as the label's text.
            let xf = target_xf(wb, sheet, row, col);
            if !inline_pick(wb, &xf, &choice.label, today).1 {
                return crate::entry::entry_cell(wb, sheet, row, col, &choice.label, today);
            }
            CellValue::Text(choice.label.clone())
        }
    };
    let mut cell = wb
        .sheets
        .get(sheet)
        .and_then(|s| s.cell(row, col))
        .cloned()
        .unwrap_or_default();
    cell.value = value;
    cell.formula = None;
    cell.f_attrs = None;
    cell.spill = None;
    if let Some(code) = &choice.code {
        let mut xf = wb.styles.xf(cell.style);
        if crate::entry::is_general(&xf) {
            xf.code = Some(code.clone());
            cell.style = wb.styles.intern(xf);
        }
    }
    Ok(cell)
}

/// The choices of the in-cell dropdown at (row, col): the rule's inline items,
/// or the non-blank cells of its source, read from that cell the way the entry
/// check reads them (a relative source shifted, a defined name followed) and
/// shown as the cells show them. Picking enters the choice ([`pick_cell`]).
/// `None` when the cell has no list rule or its source can't be read.
pub fn list_choices(
    wb: &Workbook,
    sheet: usize,
    row: u32,
    col: u32,
    today: Option<f64>,
) -> Option<Vec<ListChoice>> {
    let dv = validation_at(wb.sheets.get(sheet)?, row, col)?;
    if dv.kind != "list" {
        return None;
    }
    if let Some(items) = dv.list_values() {
        // The invariant: what a pick stores passes the check. An item whose
        // pick would not match it (one that reads as an error, say) is not
        // offered.
        let xf = target_xf(wb, sheet, row, col);
        return Some(
            items
                .into_iter()
                .filter(|s| !s.is_empty())
                .filter(|s| {
                    let (stored, _) = inline_pick(wb, &xf, s, today);
                    !matches!(stored, CellValue::Error(_))
                        && inline_matches(wb, &xf, &CellValue::Text(s.clone()), &stored, today)
                })
                .map(|s| ListChoice {
                    label: s,
                    value: None,
                    code: None,
                })
                .collect(),
        );
    }
    let (at, cells) = resolve_cells(wb, sheet, &list_source(dv, row, col))?;
    let sh = &wb.sheets[at];
    let mut out = Vec::new();
    for (r, c) in cells {
        let v = to_cell_value(cell_value_at(wb, at, r, c));
        // An error value is not a choice: the check rejects it too.
        if v.is_empty() || matches!(v, CellValue::Error(_)) {
            continue;
        }
        let xf = wb.styles.xf(sh.cell(r, c).map_or(0, |cl| cl.style));
        let label = crate::sheet::format_with(&xf, &v, wb.date1904);
        if !label.is_empty() {
            let code = xf
                .code
                .clone()
                .filter(|c| !crate::entry::is_general(&xf) && !c.is_empty());
            out.push(ListChoice {
                label,
                value: Some(v),
                code,
            });
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
    let shifted = formula_at(dv, src, row, col);
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
    let today = lists.today;
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
                // An inline item matches as the text it is, or as the value
                // typing it would give (`20%` is 0.2, `TRUE` a boolean).
                _ if is_inline => {
                    inline_matches(wb, &target_xf(wb, sheet, row, col), item, value, today)
                }
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

/// The number format of the cell at (row, col) as typing into it reads it.
fn target_xf(wb: &Workbook, sheet: usize, row: u32, col: u32) -> crate::sheet::Xf {
    let style = wb
        .sheets
        .get(sheet)
        .and_then(|s| s.cell(row, col))
        .map_or(0, |c| c.style);
    wb.styles.xf(style)
}

/// What typing the inline list item `item` into a cell formatted `xf` gives
/// (at the host's clock `today`), when it is a number, boolean, date or error
/// rather than text: the entry check's reading of an inline item. (What a pick
/// stores is [`inline_pick`]'s.)
fn inline_entry(
    wb: &Workbook,
    xf: &crate::sheet::Xf,
    item: &str,
    today: Option<f64>,
) -> Option<CellValue> {
    let ctx = crate::entry::entry_ctx(wb, today);
    let e = crate::entry::parse_entry(item.trim(), xf, &ctx).ok()?;
    (e.cell.formula.is_none() && !e.quote_prefix && !matches!(e.cell.value, CellValue::Text(_)))
        .then_some(e.cell.value)
}

/// Does `value` match the inline list item `item`: as the text it is, or as
/// the value typing it would give?
fn inline_matches(
    wb: &Workbook,
    xf: &crate::sheet::Xf,
    item: &CellValue,
    value: &CellValue,
    today: Option<f64>,
) -> bool {
    let item_text = text_of(item);
    item_text.trim() == text_of(value)
        || inline_entry(wb, xf, &item_text, today).as_ref() == Some(value)
}

/// What picking the inline item `label` into a cell formatted `xf` stores,
/// and whether that is the label as plain text. Typed entry reads it, except
/// that a formula or quote-prefixed result (`=1+1`, `'01`) would not match
/// the item the check compares with: those are stored as the label's text.
fn inline_pick(
    wb: &Workbook,
    xf: &crate::sheet::Xf,
    label: &str,
    today: Option<f64>,
) -> (CellValue, bool) {
    let ctx = crate::entry::entry_ctx(wb, today);
    match crate::entry::parse_entry(label, xf, &ctx) {
        Ok(e) if e.cell.formula.is_none() && !e.quote_prefix => (e.cell.value, false),
        _ => (CellValue::Text(label.to_string()), true),
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

/// The alert styles a dialog offers, in Excel's order.
pub const ALERT_STYLES: [(AlertStyle, &str); 3] = [
    (AlertStyle::Stop, "Stop"),
    (AlertStyle::Warning, "Warning"),
    (AlertStyle::Information, "Information"),
];

/// How a date or time serial is shown: a date as `m/d/yyyy` (with its time of
/// day when it has one), a time as `h:mm:ss AM/PM`, or as hours past a day.
fn serial_text(kind: &str, serial: f64, date1904: bool) -> String {
    use crate::sheet::{Xf, format_with};
    let code = match kind {
        "date" if serial.fract() != 0.0 => "m/d/yyyy h:mm:ss AM/PM",
        "date" => "m/d/yyyy",
        _ if serial >= 1.0 => "[h]:mm:ss",
        _ => "h:mm:ss AM/PM",
    };
    let xf = Xf {
        code: Some(code.to_string()),
        ..Xf::default()
    };
    format_with(&xf, &CellValue::Number(serial), date1904)
}

/// What a bound's box shows: for a date or time rule a serial as the date or
/// time, any other formula behind `=`; for the other kinds the formula as it
/// is.
fn bound_box(dv: &DataValidation, f: &str, date1904: bool) -> String {
    if matches!(dv.kind.as_str(), "date" | "time") && !f.trim().is_empty() {
        return match f.trim().parse::<f64>() {
            Ok(n) => serial_text(&dv.kind, n, date1904),
            Err(_) => format!("={f}"),
        };
    }
    f.to_string()
}

/// What the box for a rule's first formula shows: an inline list as its
/// items, a list reference or name behind `=`, a date or time bound as text
/// ([`bound_box`]).
pub fn first_box(dv: &DataValidation, date1904: bool) -> String {
    if dv.kind == "list" {
        return match dv.list_values() {
            Some(items) => items.join(","),
            None => format!("={}", dv.formula1),
        };
    }
    bound_box(dv, &dv.formula1, date1904)
}

/// What the box for a rule's second bound shows.
pub fn second_box(dv: &DataValidation, date1904: bool) -> String {
    bound_box(dv, &dv.formula2, date1904)
}

/// One bound from its box: after `=` a formula; for a date or time, text read
/// as a typed entry (`1/1/2020`, `2020-01-01`, `9:00`) and kept as its serial;
/// otherwise the text as it is.
fn bound_formula(kind: &str, text: &str, ctx: &crate::entry::EntryCtx) -> Result<String, String> {
    let text = text.trim();
    if let Some(f) = text.strip_prefix('=') {
        return Ok(f.trim().to_string());
    }
    if !matches!(kind, "date" | "time") {
        return Ok(text.to_string());
    }
    let bad = || {
        format!(
            "Data validation: '{text}' is not a valid {}",
            if kind == "date" { "date" } else { "time" }
        )
    };
    match crate::entry::parse_entry(text, &crate::sheet::Xf::default(), ctx) {
        Ok(e) => match e.cell.value {
            CellValue::Number(n) if e.cell.formula.is_none() => Ok(format!("{n}")),
            _ => Err(bad()),
        },
        Err(_) => Err(bad()),
    }
}

/// The `(formula1, formula2)` of a rule from the dialog's boxes: a list's
/// text is its items (`Yes, No`) or, behind `=`, a reference or name; a date
/// or time bound typed as text becomes its serial; any other `=` is the
/// formula bar's and is dropped. The reason a box is missing or wrong
/// otherwise.
pub fn formulas_from_boxes(
    kind: &str,
    operator: &str,
    first: &str,
    second: &str,
    ctx: &crate::entry::EntryCtx,
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
        bound_formula(kind, first, ctx)?
    };
    if !takes_two(kind, operator) {
        return Ok((f1, String::new()));
    }
    let second = second.trim();
    if second.is_empty() {
        return Err("Data validation: enter a maximum".to_string());
    }
    Ok((f1, bound_formula(kind, second, ctx)?))
}

/// What a Data Validation dialog holds, whichever host draws it.
#[derive(Clone, Debug, Default)]
pub struct DialogBoxes {
    /// Index into [`KINDS`] and [`OPERATORS`].
    pub kind: usize,
    pub operator: usize,
    pub first: String,
    pub second: String,
    pub ignore_blank: bool,
    pub dropdown: bool,
    pub show_input: bool,
    pub prompt_title: String,
    pub prompt: String,
    pub show_error: bool,
    /// Index into [`ALERT_STYLES`].
    pub style: usize,
    pub error_title: String,
    pub error: String,
}

impl DialogBoxes {
    /// The boxes for rule `dv` seen from the cell the dialog opened on (`None`
    /// shows Excel's defaults).
    pub fn of(dv: Option<&DataValidation>, at: (u32, u32), date1904: bool) -> DialogBoxes {
        let blank = DataValidation {
            allow_blank: true,
            show_input: true,
            show_error: true,
            ..DataValidation::default()
        };
        let seen = dv.map(|d| as_seen_from(d, at.0, at.1));
        let dv = seen.as_ref().unwrap_or(&blank);
        DialogBoxes {
            kind: KINDS.iter().position(|k| k.0 == dv.kind).unwrap_or(0),
            operator: OPERATORS
                .iter()
                .position(|o| o.0 == dv.operator)
                .unwrap_or(0),
            first: first_box(dv, date1904),
            second: second_box(dv, date1904),
            ignore_blank: dv.allow_blank,
            dropdown: dv.show_dropdown,
            show_input: dv.show_input,
            prompt_title: dv.prompt_title.clone(),
            prompt: dv.prompt.clone().unwrap_or_default(),
            show_error: dv.show_error,
            style: ALERT_STYLES
                .iter()
                .position(|s| s.0 == dv.error_style)
                .unwrap_or(0),
            error_title: dv.error_title.clone(),
            error: dv.error.clone(),
        }
    }

    /// The rule OK applies (its ranges left empty, its formulas written for
    /// the cell `at` the dialog opened on), or why the boxes can't make one.
    /// `current` is the rule the dialog opened on: a bound whose box still
    /// shows what [`DialogBoxes::of`] put there keeps its formula, since the
    /// text can say less than the formula (a time of day, a serial past a day).
    pub fn rule(
        &self,
        ctx: &crate::entry::EntryCtx,
        current: Option<&DataValidation>,
        at: (u32, u32),
    ) -> Result<DataValidation, String> {
        let kind = KINDS[self.kind.min(KINDS.len() - 1)].0;
        let op = OPERATORS[self.operator.min(OPERATORS.len() - 1)].0;
        let mut dv = DataValidation {
            kind: kind.to_string(),
            allow_blank: self.ignore_blank,
            show_dropdown: self.dropdown,
            show_input: self.show_input,
            prompt_title: self.prompt_title.clone(),
            prompt: (!self.prompt.is_empty()).then(|| self.prompt.clone()),
            show_error: self.show_error,
            error_style: ALERT_STYLES[self.style.min(ALERT_STYLES.len() - 1)].0,
            error_title: self.error_title.clone(),
            error: self.error.clone(),
            ..DataValidation::default()
        };
        if kind.is_empty() {
            // Any value: only the messages remain.
            dv.allow_blank = true;
            return Ok(dv);
        }
        if takes_operator(kind) {
            dv.operator = op.to_string();
        }
        // A box still showing what it was opened with is not read again: its
        // text can say less than its formula (a time of day, a serial of 0
        // shown as 12/31/1899). Only a box the user changed is parsed.
        let (mut first, mut second) = (self.first.clone(), self.second.clone());
        if let Some(cur) = current.filter(|c| c.kind == kind) {
            let seen = as_seen_from(cur, at.0, at.1);
            if first == first_box(&seen, ctx.date1904) && !seen.formula1.is_empty() {
                first = format!("={}", seen.formula1);
            }
            if second == second_box(&seen, ctx.date1904) && !seen.formula2.is_empty() {
                second = format!("={}", seen.formula2);
            }
        }
        (dv.formula1, dv.formula2) = formulas_from_boxes(kind, op, &first, &second, ctx)?;
        Ok(dv)
    }
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

/// `ranges` with adjacent ones of the same width or height joined (a tiled
/// paste names thousands), then those another contains left out.
fn dedupe(ranges: Vec<Rect>) -> Vec<Rect> {
    let ranges = coalesce(ranges);
    // Sweep by first row: a range that starts later can only be inside one
    // that is still open (its last row not yet passed), so each is compared
    // with the open ones, not with every other.
    let mut order: Vec<usize> = (0..ranges.len()).collect();
    order.sort_by_key(|&i| {
        let (r1, c1, r2, c2) = ranges[i];
        (r1, std::cmp::Reverse(r2), c1, std::cmp::Reverse(c2), i)
    });
    let mut dropped = vec![false; ranges.len()];
    let mut open: Vec<usize> = Vec::new();
    for &i in &order {
        let r = ranges[i];
        open.retain(|&k| ranges[k].2 >= r.0);
        if open.iter().any(|&k| inside(&r, &ranges[k])) {
            dropped[i] = true;
        } else {
            open.push(i);
        }
    }
    ranges
        .into_iter()
        .zip(dropped)
        .filter(|(_, d)| !d)
        .map(|(r, _)| r)
        .collect()
}

/// Is `a` entirely within `b`?
fn inside(a: &Rect, b: &Rect) -> bool {
    a.0 >= b.0 && a.1 >= b.1 && a.2 <= b.2 && a.3 <= b.3
}

/// Join ranges that touch or overlap along one axis and span the same cells
/// along the other: stacked first, then side by side. Linear after a sort.
fn coalesce(mut ranges: Vec<Rect>) -> Vec<Rect> {
    if ranges.len() < 2 {
        return ranges;
    }
    // Same columns, rows touching: stack.
    ranges.sort_by_key(|&(r1, c1, r2, c2)| (c1, c2, r1, r2));
    let mut stacked: Vec<Rect> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match stacked.last_mut() {
            Some(l) if (l.1, l.3) == (r.1, r.3) && r.0 <= l.2.saturating_add(1) => {
                l.2 = l.2.max(r.2);
            }
            _ => stacked.push(r),
        }
    }
    // Same rows, columns touching: join.
    stacked.sort_by_key(|&(r1, c1, r2, c2)| (r1, r2, c1, c2));
    let mut out: Vec<Rect> = Vec::with_capacity(stacked.len());
    for r in stacked {
        match out.last_mut() {
            Some(l) if (l.0, l.2) == (r.0, r.2) && r.1 <= l.3.saturating_add(1) => {
                l.3 = l.3.max(r.3);
            }
            _ => out.push(r),
        }
    }
    // Back in reading order.
    out.sort_by_key(|&(r1, c1, r2, c2)| (r1, c1, r2, c2));
    out
}

/// `dv`'s formulas moved by (`dr`, `dc`): the meaning a relative reference
/// has from a rule's anchor, kept when the anchor moves. An inline list and a
/// formula that won't translate stay as they are.
fn shift_formulas(dv: &mut DataValidation, dr: i64, dc: i64) {
    if (dr, dc) == (0, 0) {
        return;
    }
    for f in [&mut dv.formula1, &mut dv.formula2] {
        if !f.is_empty() && !f.starts_with('"') {
            if let Some(t) = translate_formula(f, dr, dc) {
                *f = t;
            }
        }
    }
}

/// The corner of `ranges` that formulas written for them are relative to.
fn anchor_of(ranges: &[Rect]) -> (u32, u32) {
    ranges
        .iter()
        .fold((u32::MAX, u32::MAX), |(r, c), &(r1, c1, ..)| {
            (r.min(r1), c.min(c1))
        })
}

/// Move a rule's anchor to where its ranges' corner now is, its relative
/// formulas along with it, so each cell is checked as it was.
fn retarget(dv: &mut DataValidation, old: (u32, u32)) {
    let new = anchor_of(&dv.ranges);
    if old.0 == u32::MAX || new.0 == u32::MAX {
        return;
    }
    shift_formulas(
        dv,
        i64::from(new.0) - i64::from(old.0),
        i64::from(new.1) - i64::from(old.1),
    );
}

/// `dv` as seen from cell (`row`, `col`): its formulas written for that cell,
/// which is how a dialog shows them to someone standing on it.
pub fn as_seen_from(dv: &DataValidation, row: u32, col: u32) -> DataValidation {
    let mut out = dv.clone();
    let (ar, ac) = anchor(dv);
    if ar != u32::MAX {
        shift_formulas(
            &mut out,
            i64::from(row) - i64::from(ar),
            i64::from(col) - i64::from(ac),
        );
    }
    out
}

/// Does shifting `f` by (`dr`, `dc`) lose nothing: no reference falls off the
/// grid (`#REF!`), and shifting back gives the formula again?
fn lossless_shift(f: &str, dr: i64, dc: i64) -> bool {
    if f.is_empty() || f.starts_with('"') || (dr, dc) == (0, 0) {
        return true;
    }
    let Some(there) = translate_formula(f, dr, dc) else {
        return true;
    };
    if there.contains("#REF!") && !f.contains("#REF!") {
        return false;
    }
    match translate_formula(&there, -dr, -dc) {
        Some(back) => {
            back == f
                || matches!(
                    (crate::formula::parse(&back), crate::formula::parse(f)),
                    (Ok(a), Ok(b)) if a == b
                )
        }
        None => false,
    }
}

/// `dv` shifted by (`dr`, `dc`) when that loses nothing ([`lossless_shift`]).
fn shifted(dv: &DataValidation, dr: i64, dc: i64) -> Option<DataValidation> {
    if !lossless_shift(&dv.formula1, dr, dc) || !lossless_shift(&dv.formula2, dr, dc) {
        return None;
    }
    let mut out = dv.clone();
    shift_formulas(&mut out, dr, dc);
    Some(out)
}

/// `dv` as seen from cell (`row`, `col`), when that loses nothing.
fn seen_losslessly(dv: &DataValidation, row: u32, col: u32) -> Option<DataValidation> {
    let (ar, ac) = anchor(dv);
    if ar == u32::MAX {
        return Some(dv.clone());
    }
    shifted(
        dv,
        i64::from(row) - i64::from(ar),
        i64::from(col) - i64::from(ac),
    )
}

/// Take every rule off the cells of `rect`: ranges are split around it and a
/// rule left with none goes, its element named in `dv_removed` for the save.
/// A rule whose top-left corner moves because of it keeps what its relative
/// formulas mean.
pub fn clear_validation(sheet: &mut Sheet, rect: Rect) {
    let removed = &mut sheet.dv_removed;
    sheet.validations.retain_mut(|dv| {
        if !dv.ranges.iter().any(|&r| intersect(r, rect).is_some()) {
            return true;
        }
        let old = anchor(dv);
        dv.ranges = dv.ranges.iter().flat_map(|&r| subtract(r, rect)).collect();
        if dv.ranges.is_empty() {
            removed.extend(dv.ix);
            return false;
        }
        retarget(dv, old);
        true
    });
}

/// Give `rule`'s settings to `ranges`: onto an existing rule with the same
/// settings (one `sqref` list), else as a new rule. A rule that imposes
/// nothing ([`DataValidation::is_meaningful`]) adds nothing. The ranges must
/// already be free of other rules ([`clear_validation`]), and `rule`'s
/// formulas are written for the top-left corner of `ranges`: an existing rule
/// is the same one when its formulas say the same from there.
pub fn add_ranges(sheet: &mut Sheet, rule: &DataValidation, ranges: &[Rect]) {
    if ranges.is_empty() || !rule.is_meaningful() {
        return;
    }
    let corner = anchor_of(ranges);
    // The same rule when it says the same thing from the new cells, and when
    // joining them (which may move its corner) loses no reference.
    let same = |dv: &DataValidation| {
        let Some(mut probe) = seen_losslessly(dv, corner.0, corner.1) else {
            return false;
        };
        probe.ranges.clear();
        if !probe.same_settings(rule) {
            return false;
        }
        let joined = anchor_of(&[dv.ranges.as_slice(), ranges].concat());
        let old = anchor(dv);
        shifted(
            dv,
            i64::from(joined.0) - i64::from(old.0),
            i64::from(joined.1) - i64::from(old.1),
        )
        .is_some()
    };
    match sheet.validations.iter_mut().find(|dv| same(dv)) {
        Some(dv) => {
            let old = anchor(dv);
            dv.ranges.extend_from_slice(ranges);
            dv.ranges = dedupe(std::mem::take(&mut dv.ranges));
            retarget(dv, old);
        }
        None => {
            let mut dv = rule.clone();
            dv.ranges = dedupe(ranges.to_vec());
            // Written for `corner`; a deduped set may begin elsewhere.
            retarget(&mut dv, corner);
            dv.ix = None;
            dv.orig = None;
            sheet.validations.push(dv);
        }
    }
}

/// The Data Validation dialog's OK: `rule`'s settings on `range`, its
/// formulas written for the range's top-left cell (as the dialog shows them,
/// [`as_seen_from`]). With `apply_to_all`, every cell range whose rule has the
/// same settings as the one that cell holds now takes them too. "Any value"
/// without messages ([`DataValidation::is_meaningful`]) leaves the cells
/// with no rule.
pub fn set_validation(sheet: &mut Sheet, range: Rect, rule: &DataValidation, apply_to_all: bool) {
    let base_at = sheet
        .validations
        .iter()
        .position(|dv| dv.covers(range.0, range.1));
    let base = base_at.map(|i| sheet.validations[i].clone());
    let mut ranges = vec![range];
    if let (true, Some(b)) = (apply_to_all, &base) {
        for dv in sheet.validations.iter().filter(|dv| same_everywhere(dv, b)) {
            ranges.extend(dv.ranges.iter().copied());
        }
    }
    let ranges = dedupe(ranges);
    // The formulas were written for the selection's corner; the rule's anchor
    // is now the corner of everything it covers.
    let mut rule = rule.clone();
    let corner = anchor_of(&ranges);
    shift_formulas(
        &mut rule,
        i64::from(corner.0) - i64::from(range.0),
        i64::from(corner.1) - i64::from(range.1),
    );
    let rule = &rule;
    // The rule being rewritten whole keeps its element (and any attribute
    // this code doesn't know): when everything it covers is covered again.
    let keep = base
        .as_ref()
        .filter(|b| b.ranges.iter().all(|r| ranges.iter().any(|o| inside(r, o))))
        .and(base_at);
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
            sheet.validations.push(h);
        }
        None => add_ranges(sheet, rule, &ranges),
    }
}

/// Do two rules impose the same thing on every cell they cover: the same
/// settings, their formulas the same seen from the same cell?
fn same_everywhere(a: &DataValidation, b: &DataValidation) -> bool {
    let (br, bc) = anchor(b);
    if br == u32::MAX {
        return a.same_settings(b);
    }
    let Some(mut probe) = seen_losslessly(a, br, bc) else {
        return false;
    };
    probe.ranges.clear();
    probe.same_settings(b)
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
        for dv in sheet
            .validations
            .iter()
            .filter(|dv| same_everywhere(dv, &b))
        {
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
            let (pr, pc) = anchor_of(&ranges);
            shift_formulas(
                &mut piece,
                i64::from(pr) - i64::from(ar),
                i64::from(pc) - i64::from(ac),
            );
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
    let shift_rect = |&(r1, c1, r2, c2): &Rect, dr: i64, dc: i64| {
        let shift = |v: u32, d: i64| u32::try_from(i64::from(v) + d).ok();
        let moved = (
            shift(r1, dr)?,
            shift(c1, dc)?,
            shift(r2, dr)?,
            shift(c2, dc)?,
        );
        // What the grid has room for: a paste cut short at its edge doesn't
        // name cells past it.
        intersect(moved, (0, 0, MAX_ROWS - 1, MAX_COLS - 1))
    };
    let first = (
        i64::from(dst_origin.0) - i64::from(src_rect.0),
        i64::from(dst_origin.1) - i64::from(src_rect.1),
    );
    for rule in rules {
        // Each tile is the rule seen from its own cells. Tiles whose shift
        // from the SOURCE loses nothing are the same rule, so their ranges
        // are collected and joined once; one that would lose a reference
        // (off the top of the grid) is a rule of its own.
        let (sr, sc) = anchor_of(&rule.ranges);
        let mut joined: Vec<Rect> = Vec::new();
        let mut tiles_in: Vec<(i64, i64)> = Vec::new();
        let mut apart: Vec<(i64, i64)> = Vec::new();
        for ti in 0..tiles.0 {
            for tj in 0..tiles.1 {
                let (dr, dc) = (first.0 + i64::from(ti * h), first.1 + i64::from(tj * w));
                if lossless_shift(&rule.formula1, dr, dc) && lossless_shift(&rule.formula2, dr, dc)
                {
                    joined.extend(rule.ranges.iter().filter_map(|r| shift_rect(r, dr, dc)));
                    tiles_in.push((dr, dc));
                } else {
                    apart.push((dr, dc));
                }
            }
        }
        // The joined rule is anchored at the corner of its ranges.
        let corner = anchor_of(&joined);
        let (cdr, cdc) = (
            i64::from(corner.0) - i64::from(sr),
            i64::from(corner.1) - i64::from(sc),
        );
        match shifted(rule, cdr, cdc) {
            Some(base) if !joined.is_empty() => add_ranges(dst, &base, &joined),
            _ => apart.extend(tiles_in),
        }
        for (dr, dc) in apart {
            let mut moved = rule.clone();
            shift_formulas(&mut moved, dr, dc);
            let ranges: Vec<Rect> = rule
                .ranges
                .iter()
                .filter_map(|r| shift_rect(r, dr, dc))
                .collect();
            add_ranges(dst, &moved, &ranges);
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
        check_entry(wb, 0, r, c, &cell, None)
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
        assert!(check_entry(&mut wb, 0, 1, 1, &blank, None).is_some());
        wb.sheets[0].validations[0].allow_blank = true;
        assert!(check_entry(&mut wb, 0, 1, 1, &blank, None).is_none());
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
        assert_eq!(invalid_cells(&wb, 0, None), vec![(2, 1), (3, 1), (4, 1)]);
        // Circled whatever the rule's alert setting.
        wb.sheets[0].validations[0].show_error = false;
        assert_eq!(invalid_cells(&wb, 0, None).len(), 3);
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
        assert_eq!(dst.validations[0].ranges, vec![(3, 5, 4, 5)]);
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
        // Each tile's formula is its own, and they say the same thing from
        // their own cells: one rule over both, anchored at D4.
        assert_eq!(dst.validations.len(), 1);
        assert_eq!(dst.validations[0].formula1, "D4>C4");
        assert_eq!(dst.validations[0].ranges, vec![(3, 3, 4, 3)]);
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

    // ---- review r1 ----

    fn custom_rule() -> DataValidation {
        let mut dv = rule("custom", "", "B2>A2", "");
        dv.ranges = vec![(1, 1, 9, 1)]; // B2:B10
        dv
    }

    #[test]
    fn the_dialog_keeps_relative_formulas_when_the_selection_moves_the_anchor() {
        let mut s = Sheet::default();
        s.validations.push(custom_rule());
        // Select B5:B6, as the dialog shows the rule from B5, change the
        // message only, OK.
        let mut seen = as_seen_from(&s.validations[0], 4, 1);
        assert_eq!(seen.formula1, "B5>A5");
        seen.error = "new".into();
        set_validation(&mut s, (4, 1, 5, 1), &seen, false);
        let at = |s: &Sheet, r, c| validation_at(s, r, c).cloned().unwrap();
        // B5 is checked as B5>A5 (its rule is anchored at B5), the rest as before.
        let b5 = at(&s, 4, 1);
        assert_eq!(
            (b5.ranges.clone(), b5.formula1.as_str()),
            (vec![(4, 1, 5, 1)], "B5>A5")
        );
        let b2 = at(&s, 1, 1);
        assert_eq!(
            (b2.ranges.clone(), b2.formula1.as_str()),
            (vec![(1, 1, 3, 1), (6, 1, 9, 1)], "B2>A2")
        );

        // Apply to all from B5: the rule's cells now begin at B2, so the
        // formula is rewritten for B2.
        let mut s = Sheet::default();
        s.validations.push(custom_rule());
        let mut seen = as_seen_from(&s.validations[0], 4, 1);
        seen.error = "new".into();
        set_validation(&mut s, (4, 1, 5, 1), &seen, true);
        assert_eq!(s.validations.len(), 1);
        assert_eq!(s.validations[0].formula1, "B2>A2");
        assert_eq!(s.validations[0].ranges, vec![(1, 1, 9, 1)]);
    }

    #[test]
    fn a_custom_rule_reads_a_typed_formulas_result() {
        let mut wb = book();
        wb.sheets[0]
            .validations
            .push(rule("custom", "", "ISNUMBER(B2)", ""));
        assert!(check(&mut wb, 1, 1, "=5*2").is_none());
        assert!(check(&mut wb, 1, 1, "abc").is_some());
    }

    #[test]
    fn clearing_the_first_row_or_column_re_anchors_the_formulas() {
        let mut s = Sheet::default();
        s.validations.push(custom_rule());
        clear_validation(&mut s, (1, 1, 1, 1)); // B2
        assert_eq!(s.validations[0].ranges, vec![(2, 1, 9, 1)]);
        assert_eq!(s.validations[0].formula1, "B3>A3");
        // A column: C2:D3 without C2:C3 leaves D2:D3, anchored at D2.
        let mut s = Sheet::default();
        let mut dv = rule("custom", "", "C2>B2", "");
        dv.ranges = vec![(1, 2, 2, 3)];
        s.validations.push(dv);
        clear_validation(&mut s, (1, 2, 2, 2));
        assert_eq!(s.validations[0].ranges, vec![(1, 3, 2, 3)]);
        assert_eq!(s.validations[0].formula1, "D2>C2");
        // A split into pieces shares one rule, anchored at the corner left.
        let mut s = Sheet::default();
        s.validations.push(custom_rule());
        clear_validation(&mut s, (5, 1, 5, 1));
        assert_eq!(s.validations[0].formula1, "B2>A2");
    }

    #[test]
    fn a_paste_beside_an_equal_rule_only_joins_it_when_it_says_the_same() {
        let mut s = Sheet::default();
        let mut a = custom_rule();
        a.ranges = vec![(1, 1, 1, 1)]; // B2
        s.validations.push(a);
        // C3's rule says "left cell below": from C3 that is C3>B3, equal to
        // B2's seen from C3? B2>A2 seen from C3 is C3>B3: the same rule.
        let mut same = rule("custom", "", "C3>B3", "");
        same.ranges.clear();
        add_ranges(&mut s, &same, &[(2, 2, 2, 2)]);
        assert_eq!(s.validations.len(), 1);
        assert_eq!(s.validations[0].formula1, "B2>A2");
        // A different formula text for the same cell is a rule of its own.
        let mut other = rule("custom", "", "B3>A3", "");
        other.ranges.clear();
        add_ranges(&mut s, &other, &[(2, 2, 2, 2)]);
        assert_eq!(s.validations.len(), 2);
    }

    #[test]
    fn date_and_time_boxes_read_as_typed_entries_and_show_back_as_text() {
        let ctx = crate::entry::EntryCtx::default();
        for (text, serial) in [
            ("1/1/2020", "43831"),
            ("2020-01-01", "43831"),
            ("9:00", "0.375"),
        ] {
            let kind = if text.contains(':') { "time" } else { "date" };
            let (f1, f2) = formulas_from_boxes(kind, "greaterThan", text, "", &ctx).unwrap();
            assert_eq!((f1.as_str(), f2.as_str()), (serial, ""), "{text}");
        }
        assert!(formulas_from_boxes("date", "greaterThan", "next week", "", &ctx).is_err());
        // =DATE(2020,1,1) is a formula, and a serial typed as a number stays one.
        let f = formulas_from_boxes("date", "greaterThan", "=DATE(2020,1,1)", "", &ctx);
        assert_eq!(f, Ok(("DATE(2020,1,1)".to_string(), String::new())));
        assert_eq!(
            formulas_from_boxes("date", "greaterThan", "43831", "", &ctx)
                .unwrap()
                .0,
            "43831"
        );
        // Between: both bounds.
        let (a, b) =
            formulas_from_boxes("date", "between", "1/1/2020", "12/31/2020", &ctx).unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("43831", "44196"));

        // The reverse: serial bounds show as a date and a time.
        let mut dv = rule("date", "between", "43831", "44196");
        assert_eq!(first_box(&dv, false), "1/1/2020");
        assert_eq!(second_box(&dv, false), "12/31/2020");
        dv.kind = "time".into();
        dv.formula1 = "0.375".into();
        assert_eq!(first_box(&dv, false), "9:00:00 AM");
        // A date typed back is the same bound, and the rule checks with it.
        let mut wb = book();
        let mut dv = rule("date", "greaterThanOrEqual", "", "");
        dv.formula1 = formulas_from_boxes("date", "greaterThanOrEqual", "1/1/2020", "", &ctx)
            .unwrap()
            .0;
        wb.sheets[0].validations.push(dv);
        assert!(check(&mut wb, 1, 1, "2019-12-31").is_some());
        assert!(check(&mut wb, 1, 1, "2020-01-01").is_none());
    }

    // ---- review r2 ----

    #[test]
    fn a_formula_bound_comes_back_from_its_box_as_the_formula_it_was() {
        let ctx = crate::entry::EntryCtx::default();
        for f in ["TODAY()", "$A$1", "DATE(2020,1,1)"] {
            let dv = rule("date", "greaterThan", f, "");
            let boxes = DialogBoxes::of(Some(&dv), (1, 1), false);
            assert_eq!(boxes.first, format!("={f}"));
            assert_eq!(boxes.rule(&ctx, Some(&dv), (1, 1)).unwrap().formula1, f);
        }
    }

    #[test]
    fn a_paste_off_the_top_of_the_grid_does_not_merge_into_a_healthy_rule() {
        let mut s = Sheet::default();
        let mut dv = rule("custom", "", "B2>B1", "");
        dv.ranges = vec![(1, 1, 9, 1)]; // B2:B10
        s.validations.push(dv);
        let rules = copy_rules(&s, (1, 1, 1, 1)); // B2
        paste_rules(&mut s, &rules, (1, 1, 1, 1), (0, 1), (1, 1)); // at B1
        let b2 = validation_at(&s, 1, 1).unwrap();
        assert_eq!(b2.formula1, "B2>B1", "the healthy rule is untouched");
        assert!(!b2.formula1.contains("#REF!"));
        let b1 = validation_at(&s, 0, 1).unwrap();
        assert!(!std::ptr::eq(b1, b2), "B1 has its own rule");
    }

    #[test]
    fn the_dialog_edits_the_rule_it_was_opened_on_among_identical_ones() {
        let mut s = Sheet::default();
        for r in [(1, 1, 1, 1), (6, 3, 6, 3)] {
            // Two in-memory rules of the same text on B2 and D7.
            let mut dv = rule("list", "", "$A$1:$A$5", "");
            dv.ranges = vec![r];
            s.validations.push(dv);
        }
        // Edit D7's rule (as the dialog would) to something else.
        let mut edited = rule("list", "", "$A$1:$A$9", "");
        edited.ranges.clear();
        set_validation(&mut s, (6, 3, 6, 3), &edited, false);
        assert_eq!(validation_at(&s, 1, 1).unwrap().formula1, "$A$1:$A$5");
        assert_eq!(validation_at(&s, 6, 3).unwrap().formula1, "$A$1:$A$9");
    }

    #[test]
    fn ok_with_untouched_boxes_keeps_the_bounds_it_was_opened_with() {
        let ctx = crate::entry::EntryCtx::default();
        // A time between 0 and 1 and a date with a time of day.
        for (kind, f1, f2) in [("time", "0", "1"), ("date", "43831.5", "44196")] {
            let mut dv = rule(kind, "between", f1, f2);
            dv.error = "old".into();
            let mut boxes = DialogBoxes::of(Some(&dv), (1, 1), false);
            boxes.error = "new".into();
            let out = boxes.rule(&ctx, Some(&dv), (1, 1)).unwrap();
            assert_eq!(
                (out.formula1.as_str(), out.formula2.as_str()),
                (f1, f2),
                "{kind}"
            );
            assert_eq!(out.error, "new");
        }
        // The time-of-day shows, and times past a day show as hours.
        let dv = rule("date", "between", "43831.5", "44196");
        assert_eq!(first_box(&dv, false), "1/1/2020 12:00:00 PM");
        let dv = rule("time", "between", "0", "1.5");
        assert_eq!(second_box(&dv, false), "36:00:00");
        // A bound the user did change is read again.
        let dv = rule("time", "between", "0", "1");
        let mut boxes = DialogBoxes::of(Some(&dv), (1, 1), false);
        boxes.second = "9:00".into();
        assert_eq!(
            boxes.rule(&ctx, Some(&dv), (1, 1)).unwrap().formula2,
            "0.375"
        );
    }

    // ---- review r3 ----

    #[test]
    fn one_cell_pasted_over_twenty_thousand_rows_is_one_rule_and_one_range() {
        let mut src = Sheet::default();
        let mut dv = rule("list", "", "$F$1:$F$5", "");
        dv.ranges = vec![(1, 1, 1, 1)];
        src.validations.push(dv);
        let rules = copy_rules(&src, (1, 1, 1, 1));
        let mut dst = Sheet::default();
        let t = std::time::Instant::now();
        paste_rules(&mut dst, &rules, (1, 1, 1, 1), (1, 0), (20_000, 1)); // A2:A20001
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
        assert_eq!(dst.validations.len(), 1);
        assert_eq!(dst.validations[0].ranges, vec![(1, 0, 20_000, 0)]);
        // A 2x2 block tiled over a large area is one rectangle.
        let mut src = Sheet::default();
        let mut dv = rule("list", "", "$F$1:$F$5", "");
        dv.ranges = vec![(1, 1, 2, 2)];
        src.validations.push(dv);
        let rules = copy_rules(&src, (1, 1, 2, 2));
        let mut dst = Sheet::default();
        paste_rules(&mut dst, &rules, (1, 1, 2, 2), (0, 0), (5_000, 3));
        assert_eq!(dst.validations.len(), 1);
        assert_eq!(dst.validations[0].ranges, vec![(0, 0, 9_999, 5)]);
    }

    #[test]
    fn a_relative_formula_tiled_over_many_rows_is_one_rule_that_checks_each_cell() {
        let mut src = Sheet::default();
        let mut dv = rule("custom", "", "B2>A2", "");
        dv.ranges = vec![(1, 1, 1, 1)];
        src.validations.push(dv);
        let rules = copy_rules(&src, (1, 1, 1, 1));
        let mut wb = book();
        paste_rules(&mut wb.sheets[0], &rules, (1, 1, 1, 1), (4, 3), (1_000, 1)); // D5 down
        assert_eq!(wb.sheets[0].validations.len(), 1);
        assert_eq!(wb.sheets[0].validations[0].formula1, "D5>C5");
        wb.sheets[0].set_cell(504, 2, Cell::number(10.0)); // C505
        wb.sheets[0].validations[0].show_error = true;
        // D505 is checked as D505>C505.
        assert!(check(&mut wb, 504, 3, "11").is_none());
        assert!(check(&mut wb, 504, 3, "9").is_some());
    }

    #[test]
    fn a_bound_that_does_not_round_trip_in_text_keeps_its_exact_formula() {
        let ctx = crate::entry::EntryCtx::default();
        for (kind, f) in [("time", "0.354166666666667"), ("date", "43831.123456789")] {
            let mut dv = rule(kind, "greaterThan", f, "");
            dv.error = "old".into();
            let mut boxes = DialogBoxes::of(Some(&dv), (1, 1), false);
            boxes.error = "new".into();
            assert_eq!(
                boxes.rule(&ctx, Some(&dv), (1, 1)).unwrap().formula1,
                f,
                "{kind}"
            );
        }
    }

    #[test]
    fn a_date_bound_of_zero_can_be_okd_untouched() {
        let ctx = crate::entry::EntryCtx::default();
        let mut dv = rule("date", "greaterThan", "0", "");
        dv.error = "old".into();
        let mut boxes = DialogBoxes::of(Some(&dv), (1, 1), false);
        boxes.error = "new".into();
        assert_eq!(boxes.rule(&ctx, Some(&dv), (1, 1)).unwrap().formula1, "0");
    }

    #[test]
    fn apply_to_all_and_clear_all_leave_a_rule_alone_that_says_something_else_from_here() {
        let mut s = Sheet::default();
        let mut dv = rule("custom", "", "B2>B1", "");
        dv.ranges = vec![(1, 1, 9, 1)]; // B2:B10
        s.validations.push(dv);
        let rules = copy_rules(&s, (1, 1, 1, 1));
        paste_rules(&mut s, &rules, (1, 1, 1, 1), (0, 1), (1, 1)); // B2 -> B1
        let mut edited = rule("custom", "", "B1>B0", "");
        edited.ranges.clear();
        edited.error = "edited".into();
        set_validation(&mut s, (0, 1, 0, 1), &edited, true);
        assert_eq!(validation_at(&s, 1, 1).unwrap().formula1, "B2>B1");
        assert_eq!(validation_at(&s, 1, 1).unwrap().error, "10 to 90 only");
        clear_all_validation(&mut s, (0, 1, 0, 1), true);
        assert!(validation_at(&s, 0, 1).is_none());
        assert_eq!(validation_at(&s, 1, 1).unwrap().formula1, "B2>B1");
    }

    // ---- review r4 ----

    #[test]
    fn a_paste_over_the_top_edge_in_several_tiles_keeps_the_healthy_formulas() {
        let mut s = Sheet::default();
        let mut dv = rule("custom", "", "B2>B1", "");
        dv.ranges = vec![(1, 1, 9, 1)]; // B2:B10
        s.validations.push(dv);
        let rules = copy_rules(&s, (1, 1, 1, 1)); // B2
        paste_rules(&mut s, &rules, (1, 1, 1, 1), (0, 1), (3, 1)); // over B1:B3
        assert!(
            !s.validations
                .iter()
                .any(|d| d.formula1.contains("#REF!") && d.covers(1, 1))
        );
        assert_eq!(validation_at(&s, 1, 1).unwrap().formula1, "B2>B1");
        let b3 = validation_at(&s, 2, 1).unwrap();
        assert!(
            b3.covers(1, 1),
            "B2 and B3 share the rule that says B2>B1: {b3:?}"
        );
        assert_eq!(b3.formula1, "B2>B1");
    }

    #[test]
    fn a_touching_two_range_rule_keeps_its_element_under_apply_to_all() {
        let mut s = Sheet::default();
        let mut dv = rule("whole", "between", "10", "90");
        dv.ranges = vec![(1, 1, 4, 1), (5, 1, 9, 1)]; // B2:B5 B6:B10
        dv.ix = Some(2);
        dv.orig = Some(Box::new(dv.clone()));
        s.validations.push(dv);
        let mut edited = rule("whole", "between", "1", "5");
        edited.ranges.clear();
        set_validation(&mut s, (1, 1, 1, 1), &edited, true);
        assert_eq!(s.validations.len(), 1);
        assert_eq!(s.validations[0].ix, Some(2));
        assert!(s.dv_removed.is_empty());
    }

    #[test]
    fn an_untouched_inline_list_box_keeps_its_stored_text() {
        let ctx = crate::entry::EntryCtx::default();
        for stored in ["\"Yes, No\"", "\"a,,b\""] {
            let mut dv = rule("list", "", stored, "");
            dv.error = "old".into();
            let mut boxes = DialogBoxes::of(Some(&dv), (1, 1), false);
            boxes.error = "new".into();
            assert_eq!(
                boxes.rule(&ctx, Some(&dv), (1, 1)).unwrap().formula1,
                stored
            );
        }
    }

    #[test]
    fn a_sparse_paste_over_a_hundred_thousand_rows_is_quick_and_right() {
        let mut src = Sheet::default();
        let mut dv = rule("list", "", "$F$1:$F$5", "");
        dv.ranges = vec![(0, 0, 0, 0)]; // A1 only
        src.validations.push(dv);
        let rules = copy_rules(&src, (0, 0, 1, 0)); // A1:A2
        let mut dst = Sheet::default();
        let t = std::time::Instant::now();
        paste_rules(&mut dst, &rules, (0, 0, 1, 0), (0, 0), (50_000, 1));
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
        assert_eq!(dst.validations.len(), 1);
        let r = &dst.validations[0].ranges;
        assert_eq!(r.len(), 50_000);
        assert_eq!((r[0], r[49_999]), ((0, 0, 0, 0), (99_998, 0, 99_998, 0)));
    }

    #[test]
    fn the_dropdown_offers_what_the_check_accepts() {
        let mut wb = book();
        let s = &mut wb.sheets[0];
        for (r, v) in [(0, 1.0), (1, 2.0), (2, 3.0), (4, 5.0), (5, 6.0), (6, 7.0)] {
            s.set_cell(r, 0, Cell::number(v));
        }
        let mut dv = rule("list", "", "A1:A3", ""); // relative source
        dv.ranges = vec![(0, 1, 9, 1)]; // B1:B10
        s.validations.push(dv);
        // At B5 the source is A5:A7.
        let labels = |r| {
            list_choices(&wb, 0, r, 1, None)
                .map(|c| c.into_iter().map(|c| c.label).collect::<Vec<_>>())
        };
        assert_eq!(labels(4), Some(vec!["5".into(), "6".into(), "7".into()]));
        assert_eq!(labels(0), Some(vec!["1".into(), "2".into(), "3".into()]));
        assert!(list_choices(&wb, 0, 20, 1, None).is_none());
    }

    // ---- review r5 ----

    #[test]
    fn a_picked_choice_enters_the_exact_value_and_passes_the_check() {
        use crate::sheet::Xf;
        let mut wb = book();
        // Style 0 is General.
        wb.styles.intern(Xf::default());
        let mut styled = |code: &str| {
            wb.styles.intern(Xf {
                code: Some(code.to_string()),
                ..Xf::default()
            })
        };
        let (two, pct, dt) = (styled("0.00"), styled("0%"), styled("m/d/yyyy"));
        let s = &mut wb.sheets[0];
        let mut put = |r: u32, v: CellValue, style: u32| {
            s.set_cell(
                r,
                0,
                Cell {
                    value: v,
                    style,
                    ..Cell::default()
                },
            )
        };
        put(0, CellValue::Number(3.14127), two);
        put(1, CellValue::Number(0.125), pct);
        put(2, CellValue::Number(45000.75), dt);
        put(3, CellValue::Number(1.0 / 3.0), 0);
        put(4, CellValue::Text("007".into()), 0);
        put(5, CellValue::Text("1/2/2020".into()), 0);
        put(6, CellValue::Text("plain".into()), 0);
        let mut dv = rule("list", "", "$A$1:$A$7", "");
        dv.ranges = vec![(0, 3, 9, 3)]; // D1:D10
        wb.sheets[0].validations.push(dv);
        let choices = list_choices(&wb, 0, 0, 3, None).unwrap();
        assert_eq!(choices.len(), 7);
        assert_eq!(choices[0].label, "3.14");
        assert_eq!(choices[1].label, "13%");
        assert_eq!(choices[2].label, "3/15/2023");
        for (i, c) in choices.iter().enumerate() {
            // Picking puts the choice's own value; the check accepts it.
            let cell = pick_cell(&mut wb, 0, 9, 3, c, None).unwrap();
            assert_eq!(Some(cell.value.clone()), c.value);
            assert!(
                check_entry(&mut wb, 0, 9, 3, &cell, None).is_none(),
                "choice {i}: {:?}",
                c.label
            );
        }
        // A picked date shows as a date in a General cell.
        let cell = pick_cell(&mut wb, 0, 9, 3, &choices[2], None).unwrap();
        let xf = wb.styles.xf(cell.style);
        assert_eq!(xf.code.as_deref(), Some("m/d/yyyy"));
        // And a value near but not equal to an item still breaks the rule.
        assert!(check(&mut wb, 9, 3, "3.14").is_some());
    }

    // ---- review r6 ----

    /// A book whose D10 (the target) is formatted `code` and whose D1:D10
    /// carry an inline list.
    fn inline_book(items: &str, code: Option<&str>) -> Workbook {
        let mut wb = book();
        wb.styles.intern(crate::sheet::Xf::default()); // style 0: General
        let mut dv = rule("list", "", &format!("\"{items}\""), "");
        dv.ranges = vec![(0, 3, 9, 3)];
        wb.sheets[0].validations.push(dv);
        if let Some(code) = code {
            let style = wb.styles.intern(crate::sheet::Xf {
                code: Some(code.to_string()),
                ..Default::default()
            });
            wb.sheets[0].set_cell(
                9,
                3,
                Cell {
                    style,
                    ..Cell::default()
                },
            );
        }
        wb
    }

    fn pick_inline(wb: &mut Workbook, i: usize) -> Cell {
        let choices = list_choices(wb, 0, 9, 3, None).unwrap();
        pick_cell(wb, 0, 9, 3, &choices[i], None).unwrap()
    }

    #[test]
    fn an_inline_item_is_entered_the_way_typing_it_reads_under_the_cells_own_format() {
        // General: numbers, booleans, percents, dates and text as typed.
        for (items, pick, want) in [
            ("1,2,3", 0, CellValue::Number(1.0)),
            ("TRUE,FALSE", 0, CellValue::Bool(true)),
            ("10%,20%", 1, CellValue::Number(0.2)),
            ("1/1/2024,2/1/2024", 0, CellValue::Number(45292.0)),
            ("Yes,No", 0, CellValue::Text("Yes".into())),
        ] {
            let mut wb = inline_book(items, None);
            let cell = pick_inline(&mut wb, pick);
            assert_eq!(cell.value, want, "{items}");
            assert!(
                check_entry(&mut wb, 0, 9, 3, &cell, None).is_none(),
                "{items}"
            );
        }
        // A Text-formatted cell keeps `001` text, as typing does.
        let mut wb = inline_book("001,002,003", Some("@"));
        let cell = pick_inline(&mut wb, 0);
        assert_eq!(cell.value, CellValue::Text("001".into()));
        assert!(check_entry(&mut wb, 0, 9, 3, &cell, None).is_none());
        // A percent cell reads `10` as 10%, picked or typed, and both pass.
        let mut wb = inline_book("5,10,15", Some("0%"));
        let cell = pick_inline(&mut wb, 1);
        assert_eq!(cell.value, CellValue::Number(0.1));
        assert!(check_entry(&mut wb, 0, 9, 3, &cell, None).is_none());
        assert!(check(&mut wb, 9, 3, "10").is_none(), "typed 10 passes too");
        assert!(check(&mut wb, 9, 3, "11").is_some());
        // A picked percent shows as a percent in a General cell.
        let mut wb = inline_book("10%,20%", None);
        let cell = pick_inline(&mut wb, 0);
        assert!(crate::entry::is_percent(&wb.styles.xf(cell.style)));
    }

    #[test]
    fn an_inline_item_that_reads_as_an_error_is_not_offered() {
        let wb = inline_book("#N/A,OK", None);
        let labels: Vec<_> = list_choices(&wb, 0, 9, 3, None)
            .unwrap()
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert_eq!(labels, ["OK"]);
    }

    #[test]
    fn an_error_cell_in_the_source_is_not_a_choice() {
        let mut wb = book();
        let s = &mut wb.sheets[0];
        s.set_cell(0, 0, Cell::text("Yes"));
        s.set_cell(1, 0, Cell::formula("NA()"));
        s.set_cell(2, 0, Cell::text("No"));
        let mut dv = rule("list", "", "$A$1:$A$3", "");
        dv.ranges = vec![(0, 3, 9, 3)];
        s.validations.push(dv);
        let mut engine = crate::engine::Engine::new(&wb);
        engine.recalc_all(&mut wb);
        let labels: Vec<_> = list_choices(&wb, 0, 0, 3, None)
            .unwrap()
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert_eq!(labels, ["Yes", "No"]);
    }

    // ---- review r8 ----

    const CLOCK: f64 = 45000.0; // 2023-03-15

    /// For every inline item the dropdown offers, the stored result of a pick
    /// passes the entry check, and so does typing the label.
    #[test]
    fn every_offered_inline_item_passes_the_check_after_a_pick_and_when_typed() {
        let items = [
            "3/15", "-1/2", "'01", "@home", "=1+1", "1/2", "TRUE", "10%", "001", "#N/A", "1e3",
            "padded", "plain",
        ];
        // What is offered, per target format: everything but `#N/A`, which
        // typed into a General or percent cell is an error the check refuses
        // (under `@` it is text).
        let not_offered = |code: Option<&str>| -> Vec<&str> {
            if code == Some("@") {
                vec![]
            } else {
                vec!["#N/A"]
            }
        };
        for code in [None, Some("@"), Some("0%")] {
            let mut offered_labels = Vec::new();
            for item in items {
                let mut wb = inline_book(&format!("{item},zzz"), code);
                let choices = list_choices(&wb, 0, 9, 3, Some(CLOCK)).unwrap();
                for c in choices.iter().filter(|c| c.label != "zzz") {
                    offered_labels.push(c.label.clone());
                    let cell = pick_cell(&mut wb, 0, 9, 3, c, Some(CLOCK)).unwrap();
                    assert!(
                        check_entry(&mut wb, 0, 9, 3, &cell, Some(CLOCK)).is_none(),
                        "pick of {:?} under {code:?} gives {:?}",
                        c.label,
                        cell.value
                    );
                    // Typing the label passes too, wherever typing gives the
                    // value the pick stores. Where it can't (`=1+1` and `-1/2`
                    // type as formulas, `'01` as quote-prefixed `01`), the pick
                    // stores the label as text and typing is not comparable:
                    // Excel itself would refuse those typed.
                    let typed =
                        crate::entry::entry_cell(&mut wb, 0, 9, 3, &c.label, Some(CLOCK)).unwrap();
                    if typed.formula.is_none() && typed.value == cell.value {
                        assert!(
                            check_entry(&mut wb, 0, 9, 3, &typed, Some(CLOCK)).is_none(),
                            "typing {:?} under {code:?} gives {:?}",
                            c.label,
                            typed.value
                        );
                    }
                }
            }
            let want: Vec<&str> = items
                .iter()
                .copied()
                .filter(|i| !not_offered(code).contains(i))
                .collect();
            assert_eq!(offered_labels, want, "offered under {code:?}");
        }
    }
}
