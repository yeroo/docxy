//! The AutoFilter commands: turn a filter on or off, set a column's
//! criteria, reapply, clear, search and filter by the selected cell. Each
//! applies the whole filter once and reports `<n> of <m> records found`.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use super::{ColumnFilter, DateGroup, custom_pass, dates, is_blank_value};
use crate::cf::Shown;
use crate::sheet::{CellValue, DateParts, DefinedName, NumFmt, SheetAutoFilter, Workbook, Xf};

/// (r1, c1, r2, c2), 0-based.
pub type Area = (u32, u32, u32, u32);

/// The name that holds a sheet's filtered list.
pub(crate) const FILTER_DB: &str = "_xlnm._FilterDatabase";

/// What a filter command left: `shown` of the list's `total` records are
/// visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterOutcome {
    pub shown: usize,
    pub total: usize,
}

/// The status-bar text after a filter command: `<n> of <m> records found`.
pub fn status_text(o: &FilterOutcome) -> String {
    format!("{} of {} records found", o.shown, o.total)
}

/// Why a filter command did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterError {
    /// No list around the cell (or the range is empty).
    NoData,
    /// The cell is in a table, whose own filter this doesn't edit.
    InTable,
    /// The sheet has no filter to act on.
    NoFilter,
    /// The column is outside the filter's range.
    NotInFilter,
    /// The cell's colour is one we can't read (a theme or indexed colour).
    NoColor,
    /// The cell shows no conditional-formatting icon.
    NoIcon,
    /// Advanced Filter's copy-to range is on another sheet.
    OtherSheet,
    /// Advanced Filter's copy-to header names a field the list lacks.
    BadExtract,
}

impl FilterError {
    /// What a host shows.
    pub fn message(&self) -> &'static str {
        match self {
            FilterError::NoData => "There is no list here to filter.",
            FilterError::InTable => "This cell is in a table; its filter isn't changed here.",
            FilterError::NoFilter => "There is no filter on this sheet.",
            FilterError::NotInFilter => "That column is not in the filter.",
            FilterError::NoColor => "The cell's colour can't be read.",
            FilterError::NoIcon => "The cell shows no icon.",
            FilterError::OtherSheet => super::ADVANCED_OTHER_SHEET,
            FilterError::BadExtract => "The extract range has a missing or invalid field name.",
        }
    }
}

impl std::fmt::Display for FilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for FilterError {}

/// How Filter by Selected Cell reads the cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByCell {
    Value,
    CellColor,
    FontColor,
    Icon,
}

/// `today` (a serial in the 1900 system, as the engine's clock gives it) in
/// the workbook's date system.
pub(crate) fn wb_today(wb: &Workbook, today: f64) -> f64 {
    if wb.date1904 { today - 1462.0 } else { today }
}

/// Is a number in this format a date (or date and time)?
pub(crate) fn is_date_xf(xf: &Xf) -> bool {
    match xf.code.as_deref().and_then(crate::numfmt::parse_format) {
        Some(f) => f.is_date(),
        None => matches!(xf.numfmt, NumFmt::Date | NumFmt::DateTime),
    }
}

/// A cell's text as the grid shows it.
pub(crate) fn shown_text(wb: &Workbook, sheet: usize, r: u32, c: u32) -> String {
    let Some(cell) = wb.sheets.get(sheet).and_then(|s| s.cell(r, c)) else {
        return String::new();
    };
    crate::sheet::format_with(&wb.styles.xf(cell.style), &cell.value, wb.date1904)
}

/// The calendar date of a cell holding a date.
pub(crate) fn cell_date(wb: &Workbook, sheet: usize, r: u32, c: u32) -> Option<DateParts> {
    let cell = wb.sheets.get(sheet)?.cell(r, c)?;
    let CellValue::Number(n) = cell.value else {
        return None;
    };
    if !is_date_xf(&wb.styles.xf(cell.style)) {
        return None;
    }
    crate::sheet::serial_to_parts(n, wb.date1904)
}

fn cell_number(wb: &Workbook, sheet: usize, r: u32, c: u32) -> Option<f64> {
    match wb.sheets.get(sheet)?.cell(r, c)?.value {
        CellValue::Number(n) => Some(n),
        _ => None,
    }
}

/// `$A$1:$D$21` on `sheet`, as a defined name holds it.
pub(crate) fn name_formula(wb: &Workbook, sheet: usize, (r1, c1, r2, c2): Area) -> String {
    use crate::sheet::col_name;
    let name = wb.sheets.get(sheet).map_or("", |s| s.name.as_str());
    format!(
        "{}!${}${}:${}${}",
        crate::sheet::quote_sheet_name(name),
        col_name(c1),
        r1 + 1,
        col_name(c2),
        r2 + 1
    )
}

/// Set (or add) the sheet-local name `name` to `area` on `on`.
pub(crate) fn set_name(wb: &mut Workbook, sheet: usize, name: &str, on: usize, area: Area) {
    let formula = name_formula(wb, on, area);
    match wb
        .defined_names
        .iter_mut()
        .find(|d| d.scope == Some(sheet) && d.name.eq_ignore_ascii_case(name))
    {
        Some(d) => d.formula = formula,
        None => wb.defined_names.push(DefinedName {
            name: name.to_string(),
            scope: Some(sheet),
            formula,
        }),
    }
}

fn drop_name(wb: &mut Workbook, sheet: usize, name: &str) {
    wb.defined_names
        .retain(|d| !(d.scope == Some(sheet) && d.name.eq_ignore_ascii_case(name)));
}

/// The area a sheet-local name holds on that sheet, if it is a plain range.
pub(crate) fn name_area(wb: &Workbook, sheet: usize, name: &str) -> Option<Area> {
    let d = wb
        .defined_names
        .iter()
        .find(|d| d.scope == Some(sheet) && d.name.eq_ignore_ascii_case(name))?;
    let refs = d.formula.rsplit('!').next()?.replace('$', "");
    crate::sheet::parse_range_name(&refs)
}

fn in_table(wb: &Workbook, sheet: usize, (r1, c1, r2, c2): Area) -> bool {
    wb.tables.iter().any(|t| {
        let (a, b, c, d) = t.range;
        t.sheet == sheet && a <= r2 && r1 <= c && b <= c2 && c1 <= d
    })
}

/// Turn AutoFilter on over the current region around `at` (Excel's Filter
/// with one cell selected). Returns its range.
pub fn auto_filter_on(
    wb: &mut Workbook,
    sheet: usize,
    at: (u32, u32),
) -> Result<Area, FilterError> {
    let s = wb.sheets.get(sheet).ok_or(FilterError::NoData)?;
    if in_table(wb, sheet, (at.0, at.1, at.0, at.1)) {
        return Err(FilterError::InTable);
    }
    let (region, _) = crate::edit::subtotal_region(s, at.0, at.1).ok_or(FilterError::NoData)?;
    auto_filter_on_range(wb, sheet, region)
}

/// Turn AutoFilter on over `range` (header row first), replacing any filter
/// the sheet had.
pub fn auto_filter_on_range(
    wb: &mut Workbook,
    sheet: usize,
    range: Area,
) -> Result<Area, FilterError> {
    if wb.sheets.get(sheet).is_none() || range.0 > range.2 || range.1 > range.3 {
        return Err(FilterError::NoData);
    }
    if in_table(wb, sheet, range) {
        return Err(FilterError::InTable);
    }
    auto_filter_off(wb, sheet);
    wb.sheets[sheet].auto_filter = Some(SheetAutoFilter {
        range,
        ..SheetAutoFilter::default()
    });
    set_name(wb, sheet, FILTER_DB, sheet, range);
    Ok(range)
}

/// Turn AutoFilter off: every row of its range is shown again (Excel keeps no
/// record of why a row is hidden), and the filter and its name go. Whether
/// the sheet had one.
pub fn auto_filter_off(wb: &mut Workbook, sheet: usize) -> bool {
    let Some(af) = wb.sheets.get_mut(sheet).and_then(|s| s.auto_filter.take()) else {
        return false;
    };
    let s = &mut wb.sheets[sheet];
    for r in af.range.0 + 1..=af.range.2 {
        if s.row_hidden(r) || s.filtered_rows.contains(&r) {
            s.set_row_filtered(r, false);
        }
    }
    s.filter_mode = Some(false);
    drop_name(wb, sheet, FILTER_DB);
    true
}

/// Set (`Some`) or clear (`None`) the criteria of absolute column `col`, and
/// apply the filter.
pub fn set_criterion(
    wb: &mut Workbook,
    sheet: usize,
    col: u32,
    f: Option<ColumnFilter>,
    today: f64,
) -> Result<FilterOutcome, FilterError> {
    let af = wb
        .sheets
        .get_mut(sheet)
        .and_then(|s| s.auto_filter.as_mut())
        .ok_or(FilterError::NoFilter)?;
    if col < af.range.1 || col > af.range.3 {
        return Err(FilterError::NotInFilter);
    }
    af.criteria.retain(|(c, _)| *c != col);
    if let Some(f) = f {
        let at = af.criteria.partition_point(|(c, _)| *c < col);
        af.criteria.insert(at, (col, f));
    }
    Ok(run(wb, sheet, today))
}

/// Data › Reapply: work the criteria out again from the current data (Top
/// 10's cut-off, the average, the date periods) and apply them.
pub fn reapply(wb: &mut Workbook, sheet: usize, today: f64) -> Result<FilterOutcome, FilterError> {
    if wb
        .sheets
        .get(sheet)
        .and_then(|s| s.auto_filter.as_ref())
        .is_none()
    {
        return Err(FilterError::NoFilter);
    }
    Ok(run(wb, sheet, today))
}

/// Clear Filter From one column (the rest apply again), or Data › Clear
/// (`None`): every row of the list is shown and the filter buttons stay.
/// With no AutoFilter, Clear shows the rows an Advanced Filter hid in place
/// (the list `_FilterDatabase` names).
pub fn clear(
    wb: &mut Workbook,
    sheet: usize,
    col: Option<u32>,
    today: f64,
) -> Result<FilterOutcome, FilterError> {
    if let Some(col) = col {
        return set_criterion(wb, sheet, col, None, today);
    }
    let s = wb.sheets.get_mut(sheet).ok_or(FilterError::NoFilter)?;
    let area = match s.auto_filter.as_mut() {
        Some(af) => {
            af.criteria.clear();
            af.range
        }
        None => match name_area(wb, sheet, FILTER_DB) {
            Some(a) => a,
            None if !wb.sheets[sheet].filtered_rows.is_empty() => {
                let s = &mut wb.sheets[sheet];
                let rows: Vec<u32> = s.filtered_rows.iter().copied().collect();
                let total = rows.len();
                for r in rows {
                    s.set_row_filtered(r, false);
                }
                s.filter_mode = Some(false);
                return Ok(FilterOutcome {
                    shown: total,
                    total,
                });
            }
            None => return Err(FilterError::NoFilter),
        },
    };
    let s = &mut wb.sheets[sheet];
    for r in area.0 + 1..=area.2 {
        if s.row_hidden(r) || s.filtered_rows.contains(&r) {
            s.set_row_filtered(r, false);
        }
    }
    s.filter_mode = Some(false);
    let total = area.2.saturating_sub(area.0) as usize;
    Ok(FilterOutcome {
        shown: total,
        total,
    })
}

/// The filter drop-down's search: check the column's values whose displayed
/// text contains `pattern` (`*` and `?` wildcards, `~` escapes) — Select All
/// Search Results. With `add` ("Add current selection to filter"), they join
/// the column's checked values; a criterion other than a checklist is
/// replaced, as Excel does.
pub fn search(
    wb: &mut Workbook,
    sheet: usize,
    col: u32,
    pattern: &str,
    add: bool,
    today: f64,
) -> Result<FilterOutcome, FilterError> {
    let af = wb
        .sheets
        .get(sheet)
        .and_then(|s| s.auto_filter.as_ref())
        .ok_or(FilterError::NoFilter)?;
    let (r1, _, r2, _) = grown_range(wb, sheet, af.range);
    let pat = format!("*{pattern}*");
    let mut seen: HashSet<String> = HashSet::new();
    let mut hits: Vec<String> = Vec::new();
    for r in r1 + 1..=r2 {
        let t = shown_text(wb, sheet, r, col);
        if !t.is_empty()
            && crate::formula::wildcard_match(&pat, &t)
            && seen.insert(t.to_lowercase())
        {
            hits.push(t);
        }
    }
    let current = af.criteria.iter().find(|(c, _)| *c == col).map(|x| &x.1);
    let f = match (add, current) {
        (true, Some(ColumnFilter::Values { vals, blank, dates })) => {
            let mut vals = vals.clone();
            let have: HashSet<String> = vals.iter().map(|v| v.to_lowercase()).collect();
            vals.extend(
                hits.into_iter()
                    .filter(|h| !have.contains(&h.to_lowercase())),
            );
            ColumnFilter::Values {
                vals,
                blank: *blank,
                dates: dates.clone(),
            }
        }
        _ => ColumnFilter::values(hits),
    };
    set_criterion(wb, sheet, col, Some(f), today)
}

/// Right-click › Filter › Filter by Selected Cell's Value / Color / Font
/// Color / Icon: that column keeps the rows like the cell. Turns AutoFilter
/// on over the cell's list first when the sheet has none.
pub fn filter_by_cell(
    wb: &mut Workbook,
    sheet: usize,
    (r, c): (u32, u32),
    by: ByCell,
    today: f64,
) -> Result<FilterOutcome, FilterError> {
    let has = wb
        .sheets
        .get(sheet)
        .and_then(|s| s.auto_filter.as_ref())
        .is_some_and(|af| (af.range.0..=af.range.2).contains(&r));
    if !has {
        auto_filter_on(wb, sheet, (r, c))?;
    }
    let f = match by {
        ByCell::Value => {
            let blank = wb
                .sheets
                .get(sheet)
                .and_then(|s| s.cell(r, c))
                .is_none_or(|cl| is_blank_value(Some(&cl.value)));
            if blank {
                ColumnFilter::Values {
                    vals: Vec::new(),
                    blank: true,
                    dates: Vec::new(),
                }
            } else if let Some(d) = cell_date(wb, sheet, r, c) {
                ColumnFilter::Values {
                    vals: Vec::new(),
                    blank: false,
                    dates: vec![DateGroup {
                        year: d.year,
                        month: Some(d.month),
                        day: Some(d.day),
                    }],
                }
            } else {
                ColumnFilter::values(vec![shown_text(wb, sheet, r, c)])
            }
        }
        ByCell::CellColor | ByCell::FontColor => {
            let cell = by == ByCell::CellColor;
            let shown = if cell {
                crate::cf::cell_fill(wb, sheet, r, c)
            } else {
                crate::cf::cell_font_color(wb, sheet, r, c)
            };
            ColumnFilter::Color {
                cell,
                rgb: shown.rgb().ok_or(FilterError::NoColor)?,
                dxf_id: None,
            }
        }
        ByCell::Icon => {
            let (set, id) = crate::cf::cell_icon(wb, sheet, r, c).ok_or(FilterError::NoIcon)?;
            ColumnFilter::Icon { set, id }
        }
    };
    set_criterion(wb, sheet, c, Some(f), today)
}

/// `range` grown down over the rows typed directly below it: a non-blank
/// cell in its columns on the next row extends it (Excel takes such rows
/// into the list the next time the filter is applied).
fn grown_range(wb: &Workbook, sheet: usize, range: Area) -> Area {
    let (r1, c1, mut r2, c2) = range;
    let Some(s) = wb.sheets.get(sheet) else {
        return range;
    };
    while r2 + 1 < crate::sheet::MAX_ROWS
        && s.cells
            .range((r2 + 1, c1)..=(r2 + 1, c2))
            .any(|(_, c)| !c.is_blank())
    {
        r2 += 1;
    }
    (r1, c1, r2, c2)
}

/// A criterion ready to check cells against.
pub(crate) enum Test {
    Values {
        set: HashSet<String>,
        blank: bool,
        dates: Vec<DateGroup>,
    },
    Custom {
        and: bool,
        conds: Vec<(String, String)>,
    },
    AtLeast(f64),
    AtMost(f64),
    Above(f64),
    Below(f64),
    Window(f64, f64),
    Months(RangeInclusive<u32>),
    Color {
        cell: bool,
        rgb: Option<(u8, u8, u8)>,
    },
    Icon(String, u32),
    Pass,
    Fail,
}

/// The check criteria `f` makes, from what it holds (the cut-off, average
/// and window it was last applied with).
pub(crate) fn test_for(f: &ColumnFilter) -> Test {
    match f {
        ColumnFilter::Values { vals, blank, dates } => Test::Values {
            set: vals.iter().map(|v| v.trim().to_lowercase()).collect(),
            blank: *blank,
            dates: dates.clone(),
        },
        ColumnFilter::Custom { and, conds } => Test::Custom {
            and: *and,
            conds: conds.clone(),
        },
        ColumnFilter::Top10 {
            top, filter_val, ..
        } => match (top, filter_val) {
            (true, Some(v)) => Test::AtLeast(*v),
            (false, Some(v)) => Test::AtMost(*v),
            _ => Test::Fail,
        },
        ColumnFilter::Dynamic { kind, val, max_val } => match kind.as_str() {
            "null" => Test::Pass,
            "aboveAverage" => val.map_or(Test::Fail, Test::Above),
            "belowAverage" => val.map_or(Test::Fail, Test::Below),
            k => match (dates::period_months(k), val, max_val) {
                (Some(m), _, _) => Test::Months(m),
                (None, Some(a), Some(b)) => Test::Window(*a, *b),
                _ => Test::Fail,
            },
        },
        ColumnFilter::Color { cell, rgb, .. } => Test::Color {
            cell: *cell,
            rgb: *rgb,
        },
        ColumnFilter::Icon { set, id } => Test::Icon(set.clone(), *id),
        ColumnFilter::Raw(_) => Test::Pass,
    }
}

/// Whether the cell at (`r`, `c`) passes `t`.
pub(crate) fn passes(wb: &Workbook, sheet: usize, r: u32, c: u32, t: &Test) -> bool {
    let value = wb
        .sheets
        .get(sheet)
        .and_then(|s| s.cell(r, c))
        .map(|cl| &cl.value);
    match t {
        Test::Pass => true,
        Test::Fail => false,
        Test::Values { set, blank, dates } => {
            if is_blank_value(value) {
                return *blank;
            }
            if !dates.is_empty() {
                if let Some(d) = cell_date(wb, sheet, r, c) {
                    let hit = dates.iter().any(|g| {
                        g.year == d.year
                            && g.month.is_none_or(|m| m == d.month)
                            && g.day.is_none_or(|x| x == d.day)
                    });
                    if hit {
                        return true;
                    }
                }
            }
            set.contains(&shown_text(wb, sheet, r, c).trim().to_lowercase())
        }
        Test::Custom { and, conds } => {
            custom_pass(*and, conds, value, &|| shown_text(wb, sheet, r, c))
        }
        Test::AtLeast(v) => cell_number(wb, sheet, r, c).is_some_and(|n| n >= *v),
        Test::AtMost(v) => cell_number(wb, sheet, r, c).is_some_and(|n| n <= *v),
        Test::Above(v) => cell_number(wb, sheet, r, c).is_some_and(|n| n > *v),
        Test::Below(v) => cell_number(wb, sheet, r, c).is_some_and(|n| n < *v),
        Test::Window(a, b) => cell_number(wb, sheet, r, c).is_some_and(|n| n >= *a && n < *b),
        Test::Months(m) => cell_number(wb, sheet, r, c)
            .and_then(|n| crate::sheet::serial_to_parts(n, wb.date1904))
            .is_some_and(|d| m.contains(&d.month)),
        Test::Color { cell, rgb } => {
            let shown: Shown = if *cell {
                crate::cf::cell_fill(wb, sheet, r, c)
            } else {
                crate::cf::cell_font_color(wb, sheet, r, c)
            };
            shown.is(*rgb)
        }
        Test::Icon(set, id) => crate::cf::cell_icon(wb, sheet, r, c)
            .is_some_and(|(s, i)| s.eq_ignore_ascii_case(set) && i == *id),
    }
}

/// The numbers of column `c` in rows `rows`.
fn numbers(wb: &Workbook, sheet: usize, c: u32, rows: RangeInclusive<u32>) -> Vec<f64> {
    rows.filter_map(|r| cell_number(wb, sheet, r, c)).collect()
}

/// Top 10's cut-off over `nums`: the `k`-th largest (smallest) value, with
/// `k` the item count or, for a percent, `max(1, floor(count × pct / 100))`
/// of the numbers (LibreOffice's rule; Excel's is unmeasured). Every value
/// at or past it is shown, so ties at the cut-off all show.
pub(crate) fn top10_cutoff(mut nums: Vec<f64>, top: bool, percent: bool, val: f64) -> Option<f64> {
    if nums.is_empty() {
        return None;
    }
    nums.sort_by(|a, b| {
        let o = a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
        if top { o.reverse() } else { o }
    });
    let k = if percent {
        ((nums.len() as f64 * val / 100.0).floor() as usize).max(1)
    } else {
        (val.round().max(1.0)) as usize
    };
    Some(nums[k.min(nums.len()) - 1])
}

/// Work every criterion out again from the data and `today`: Top 10's
/// cut-off, the average, a period's window.
fn prepare(wb: &mut Workbook, sheet: usize, today: f64) {
    let Some(af) = wb.sheets[sheet].auto_filter.clone() else {
        return;
    };
    let rows = af.range.0 + 1..=af.range.2;
    let day = wb_today(wb, today);
    let mut criteria = af.criteria.clone();
    for (c, f) in criteria.iter_mut() {
        match f {
            ColumnFilter::Top10 {
                top,
                percent,
                val,
                filter_val,
            } => {
                *filter_val =
                    top10_cutoff(numbers(wb, sheet, *c, rows.clone()), *top, *percent, *val);
            }
            ColumnFilter::Dynamic { kind, val, max_val } => match kind.as_str() {
                "aboveAverage" | "belowAverage" => {
                    let nums = numbers(wb, sheet, *c, rows.clone());
                    *val = (!nums.is_empty()).then(|| nums.iter().sum::<f64>() / nums.len() as f64);
                    *max_val = None;
                }
                k => {
                    if let Some((a, b)) = dates::period_window(k, day, wb.date1904) {
                        *val = Some(a);
                        *max_val = Some(b);
                    }
                }
            },
            _ => {}
        }
    }
    if let Some(af) = wb.sheets[sheet].auto_filter.as_mut() {
        af.criteria = criteria;
    }
}

/// Apply the sheet's AutoFilter: grow its range over rows typed below it,
/// work its criteria out again, then show every record that passes them all
/// and hide the rest as filtered. A pass shows even a row hidden by hand
/// (Excel). A column we can't evaluate ([`ColumnFilter::is_opaque`]) leaves
/// a passing row as it is, so it neither shows what that column hid nor
/// hides anything itself.
fn run(wb: &mut Workbook, sheet: usize, today: f64) -> FilterOutcome {
    let Some(range) = wb.sheets[sheet].auto_filter.as_ref().map(|a| a.range) else {
        return FilterOutcome { shown: 0, total: 0 };
    };
    let grown = grown_range(wb, sheet, range);
    if grown != range {
        if let Some(af) = wb.sheets[sheet].auto_filter.as_mut() {
            af.range = grown;
        }
        set_name(wb, sheet, FILTER_DB, sheet, grown);
    }
    prepare(wb, sheet, today);
    let af = wb.sheets[sheet].auto_filter.clone().unwrap_or_default();
    let tests: Vec<(u32, Test)> = af.criteria.iter().map(|(c, f)| (*c, test_for(f))).collect();
    let opaque = af.criteria.iter().any(|(_, f)| f.is_opaque());
    let (r1, _, r2, _) = af.range;
    let pass: Vec<bool> = (r1 + 1..=r2)
        .map(|r| tests.iter().all(|(c, t)| passes(wb, sheet, r, *c, t)))
        .collect();
    let s = &mut wb.sheets[sheet];
    for (r, ok) in (r1 + 1..=r2).zip(&pass) {
        if !ok {
            s.set_row_filtered(r, true);
        } else if !opaque && (s.row_hidden(r) || s.filtered_rows.contains(&r)) {
            s.set_row_filtered(r, false);
        }
    }
    let shown = (r1 + 1..=r2).filter(|&r| !s.row_hidden(r)).count();
    let total = pass.len();
    s.filter_mode = Some(shown < total);
    FilterOutcome { shown, total }
}

#[cfg(test)]
mod tests;
