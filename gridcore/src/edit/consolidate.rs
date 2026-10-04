//! Data ▸ Consolidate: combine several source ranges (on any sheet of the
//! workbook) into one, by position or by matching labels, with one of the
//! Subtotal functions; optionally as formulas linked to the sources.
//!
//! The static result is computed here, with the reference semantics of the
//! worksheet function the links would use (`SUM`, `COUNTA`, `COUNT`, …), so
//! linking and then recalculating gives the same numbers. The output is
//! written over the destination, as Excel's dialog does: no row is inserted.

use std::collections::HashMap;
use std::fmt;

use super::subtotal::{Area, SubtotalFunc};
use crate::sheet::{
    Cell, CellValue, MAX_COLS, MAX_ROWS, Sheet, Workbook, cell_name, parse_range_name,
    quote_sheet_name,
};

/// Consolidate offers the Subtotal dialog's functions, in its order.
pub type ConsolidateFunc = SubtotalFunc;

/// The worksheet function a linked consolidation totals its detail with.
pub fn consolidate_fn_name(f: ConsolidateFunc) -> &'static str {
    match f {
        SubtotalFunc::Sum => "SUM",
        SubtotalFunc::Count => "COUNTA",
        SubtotalFunc::Average => "AVERAGE",
        SubtotalFunc::Max => "MAX",
        SubtotalFunc::Min => "MIN",
        SubtotalFunc::Product => "PRODUCT",
        SubtotalFunc::CountNums => "COUNT",
        SubtotalFunc::StdDev => "STDEV",
        SubtotalFunc::StdDevP => "STDEVP",
        SubtotalFunc::Var => "VAR",
        SubtotalFunc::VarP => "VARP",
    }
}

/// The `ST_DataConsolidateFunction` token `<dataConsolidate function>` uses.
pub fn consolidate_token(f: ConsolidateFunc) -> &'static str {
    match f {
        SubtotalFunc::Sum => "sum",
        SubtotalFunc::Count => "count",
        SubtotalFunc::Average => "average",
        SubtotalFunc::Max => "max",
        SubtotalFunc::Min => "min",
        SubtotalFunc::Product => "product",
        SubtotalFunc::CountNums => "countNums",
        SubtotalFunc::StdDev => "stdDev",
        SubtotalFunc::StdDevP => "stdDevp",
        SubtotalFunc::Var => "var",
        SubtotalFunc::VarP => "varp",
    }
}

/// A function by its dialog name (`Count Numbers`) or its file token
/// (`countNums`), ignoring case.
pub fn parse_consolidate_func(s: &str) -> Option<ConsolidateFunc> {
    let s = s.trim();
    SubtotalFunc::ALL
        .into_iter()
        .find(|f| f.name().eq_ignore_ascii_case(s) || consolidate_token(*f).eq_ignore_ascii_case(s))
}

/// The dialog's settings, kept on the destination sheet
/// (`<dataConsolidate>`) so the next Consolidate there starts from them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsolidateSettings {
    pub func: ConsolidateFunc,
    /// "All references", as the list shows them.
    pub refs: Vec<String>,
    /// "Use labels in: Top row".
    pub top_row: bool,
    /// "Use labels in: Left column".
    pub left_col: bool,
    /// "Create links to source data".
    pub links: bool,
}

/// What the Consolidate dialog asks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsolidateOptions {
    pub func: ConsolidateFunc,
    /// The source references, as typed: `Sheet!A1:D4`, `'My sheet'!$A$1:$D$4`,
    /// or a bare range on the destination sheet.
    pub refs: Vec<String>,
    pub top_row: bool,
    pub left_col: bool,
    pub links: bool,
    /// The workbook's name, which a linked consolidation writes on each
    /// detail row (the host knows it: the file's stem).
    pub book_name: String,
}

/// A source reference: a sheet of this workbook and an area on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsolidateRef {
    /// The sheet's name as the workbook spells it.
    pub sheet: String,
    pub area: Area,
}

/// Why Consolidate changed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsolidateError {
    /// The reference list is empty.
    NoRefs,
    /// A reference does not parse or names no sheet of this workbook.
    BadRef(String),
    /// Links were asked for with a source on the destination sheet.
    LinksOnDestSheet,
    /// A source overlaps where the output goes.
    OverlapsDest,
    /// The sources hold nothing to consolidate.
    Empty,
    /// The output would run past the sheet's last row or column.
    OffSheet,
}

impl fmt::Display for ConsolidateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConsolidateError::NoRefs => {
                f.write_str("Add at least one source reference to consolidate.")
            }
            ConsolidateError::BadRef(r) => write!(f, "Reference isn't valid: {r}"),
            ConsolidateError::LinksOnDestSheet => f.write_str(
                "Cannot link to source data on the destination sheet. Clear Create links to source data, or put the output on another sheet.",
            ),
            ConsolidateError::OverlapsDest => {
                f.write_str("Source references overlap destination area.")
            }
            ConsolidateError::Empty => f.write_str("There is no data to consolidate."),
            ConsolidateError::OffSheet => {
                f.write_str("The consolidated data doesn't fit on the sheet from here.")
            }
        }
    }
}

impl std::error::Error for ConsolidateError {}

/// Split `Sheet!ref` at its last `!` outside quotes, unquoting the sheet
/// part (`'It''s'` → `It's`). `None` for a bare reference.
fn split_sheet(text: &str) -> Result<(Option<String>, &str), ()> {
    let Some(bang) = text.rfind('!') else {
        return Ok((None, text));
    };
    let (sheet, rest) = (text[..bang].trim(), &text[bang + 1..]);
    let name = match sheet.strip_prefix('\'') {
        Some(q) => {
            let inner = q.strip_suffix('\'').ok_or(())?;
            if inner.replace("''", "").contains('\'') {
                return Err(());
            }
            inner.replace("''", "'")
        }
        None => sheet.to_string(),
    };
    // Another workbook (`[Book.xlsx]Sheet`) is not supported.
    if name.is_empty() || name.contains(['[', ']']) {
        return Err(());
    }
    Ok((Some(name), rest))
}

/// A kept reference's sheet (unquoted; `None` for a bare one) and its
/// range text, without the workbook to check them against: what the file
/// writes as `<dataRef sheet ref>`.
pub(crate) fn split_ref_text(text: &str) -> Option<(Option<String>, &str)> {
    let t = text.trim();
    let t = t.strip_prefix('=').unwrap_or(t).trim();
    split_sheet(t)
        .ok()
        .map(|(sheet, range)| (sheet, range.trim()))
}

/// Read a source reference. A bare range is on sheet `dest`; `$` anchors
/// and a leading `=` are accepted; the sheet is matched ignoring case.
pub fn parse_consolidate_ref(
    wb: &Workbook,
    dest: usize,
    text: &str,
) -> Result<ConsolidateRef, ConsolidateError> {
    let bad = || ConsolidateError::BadRef(text.trim().to_string());
    let t = text.trim();
    let t = t.strip_prefix('=').unwrap_or(t).trim();
    let (sheet, range) = split_sheet(t).map_err(|_| bad())?;
    let area = parse_range_name(range.trim()).ok_or_else(bad)?;
    let ix = match sheet {
        Some(name) => wb.sheet_index(&name).ok_or_else(bad)?,
        None => dest,
    };
    let sheet = wb.sheets.get(ix).ok_or_else(bad)?.name.clone();
    Ok(ConsolidateRef { sheet, area })
}

/// `Sheet!$A$1:$D$4` (`Sheet!$A$1` for one cell): how the list shows a
/// reference and the settings keep it.
pub fn format_consolidate_ref(r: &ConsolidateRef) -> String {
    format!("{}!{}", quote_sheet_name(&r.sheet), abs_area(r.area))
}

fn abs_cell(row: u32, col: u32) -> String {
    format!("${}${}", crate::sheet::col_name(col), row + 1)
}

fn abs_area((r1, c1, r2, c2): Area) -> String {
    if (r1, c1) == (r2, c2) {
        abs_cell(r1, c1)
    } else {
        format!("{}:{}", abs_cell(r1, c1), abs_cell(r2, c2))
    }
}

/// A category: a label (its key folded for case) or a position.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Label(String),
    Pos(u32),
}

/// The text a label matches by: a number reads as General (`1` and `1.0`
/// alike), text folded for case. `None` for a blank label cell, which makes
/// no category.
fn label_key(c: Option<&Cell>) -> Option<String> {
    match &c?.value {
        CellValue::Empty => None,
        CellValue::Text(t) if t.trim().is_empty() => None,
        CellValue::Text(t) => Some(t.to_lowercase()),
        CellValue::Number(n) => Some(n.to_string()),
        CellValue::Bool(b) => Some(if *b { "true" } else { "false" }.into()),
        CellValue::Error(e) => Some(e.to_lowercase()),
    }
}

/// The output categories of one axis, in first-met order, with the label
/// cell each was first met in.
#[derive(Default)]
struct Axis {
    keys: Vec<Key>,
    index: HashMap<Key, usize>,
    labels: Vec<Option<Cell>>,
}

impl Axis {
    fn add(&mut self, key: Key, label: Option<&Cell>) -> usize {
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        let i = self.keys.len();
        self.index.insert(key.clone(), i);
        self.keys.push(key);
        self.labels.push(label.map(|c| Cell {
            value: c.value.clone(),
            style: c.style,
            ..Cell::default()
        }));
        i
    }

    fn len(&self) -> u32 {
        self.keys.len() as u32
    }
}

/// One contributing source cell.
#[derive(Clone, Copy)]
struct Hit {
    src: usize,
    sheet: usize,
    row: u32,
    col: u32,
}

/// Combine `vals` (the non-blank contributing values) as the worksheet
/// function over references would: blanks absent, text and booleans
/// counted only by `COUNTA`, the first error propagating except to the
/// counts. `None` when nothing contributes.
fn aggregate(func: ConsolidateFunc, vals: &[&CellValue]) -> Option<CellValue> {
    if vals.is_empty() {
        return None;
    }
    let div0 = || CellValue::Error("#DIV/0!".into());
    match func {
        SubtotalFunc::Count => return Some(CellValue::Number(vals.len() as f64)),
        SubtotalFunc::CountNums => {
            let n = vals
                .iter()
                .filter(|v| matches!(v, CellValue::Number(_)))
                .count();
            return Some(CellValue::Number(n as f64));
        }
        _ => {}
    }
    if let Some(e) = vals.iter().find(|v| matches!(v, CellValue::Error(_))) {
        return Some((*e).clone());
    }
    let nums: Vec<f64> = vals
        .iter()
        .filter_map(|v| match v {
            CellValue::Number(n) => Some(*n),
            _ => None,
        })
        .collect();
    let n = nums.len() as f64;
    let sum: f64 = nums.iter().sum();
    let var = |pop: bool| {
        let dof = if pop { n } else { n - 1.0 };
        if dof < 1.0 {
            return None;
        }
        let mean = sum / n;
        Some(nums.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / dof)
    };
    let num = CellValue::Number;
    Some(match func {
        SubtotalFunc::Sum => num(sum),
        SubtotalFunc::Average if nums.is_empty() => div0(),
        SubtotalFunc::Average => num(sum / n),
        SubtotalFunc::Max => num(nums.iter().copied().reduce(f64::max).unwrap_or(0.0)),
        SubtotalFunc::Min => num(nums.iter().copied().reduce(f64::min).unwrap_or(0.0)),
        SubtotalFunc::Product if nums.is_empty() => num(0.0),
        SubtotalFunc::Product => num(nums.iter().product()),
        SubtotalFunc::StdDev => var(false).map_or_else(div0, |v| num(v.sqrt())),
        SubtotalFunc::StdDevP => var(true).map_or_else(div0, |v| num(v.sqrt())),
        SubtotalFunc::Var => var(false).map_or_else(div0, num),
        SubtotalFunc::VarP => var(true).map_or_else(div0, num),
        SubtotalFunc::Count | SubtotalFunc::CountNums => unreachable!("counted above"),
    })
}

fn overlaps((a1, b1, a2, b2): Area, (c1, d1, c2, d2): Area) -> bool {
    a1 <= c2 && c1 <= a2 && b1 <= d2 && d1 <= b2
}

/// Consolidate `opts.refs` into sheet `dest` at (`row`, `col`), and keep
/// the settings on that sheet. Refuses, changing nothing, on an empty or
/// bad reference list, links to a source on the destination sheet, a
/// source overlapping the output, or sources with nothing in them. Returns
/// the output area.
///
/// With links, each output row becomes one hidden detail row per
/// contributing source row (formulas such as `=East!$B$2`, written only for
/// source cells that hold something, so a count or an average agrees with
/// the static result; a source cell filled in later is not picked up), then
/// a summary row totalling them, grouped in an outline below its detail.
pub fn consolidate(
    wb: &mut Workbook,
    dest: usize,
    (row, col): (u32, u32),
    opts: &ConsolidateOptions,
) -> Result<Area, ConsolidateError> {
    if wb.sheets.get(dest).is_none() {
        return Err(ConsolidateError::BadRef(String::new()));
    }
    if opts.refs.is_empty() {
        return Err(ConsolidateError::NoRefs);
    }
    let refs = opts
        .refs
        .iter()
        .map(|t| parse_consolidate_ref(wb, dest, t))
        .collect::<Result<Vec<_>, _>>()?;
    let dest_name = &wb.sheets[dest].name;
    if opts.links && refs.iter().any(|r| r.sheet == *dest_name) {
        return Err(ConsolidateError::LinksOnDestSheet);
    }

    // The categories of each axis, and each output cell's contributors.
    let (top, left) = (u32::from(opts.top_row), u32::from(opts.left_col));
    let mut rows = Axis::default();
    let mut cols = Axis::default();
    let mut hits: HashMap<(usize, usize), Vec<Hit>> = HashMap::new();
    let mut any = false;
    for (src, r) in refs.iter().enumerate() {
        let si = wb
            .sheet_index(&r.sheet)
            .expect("parsed against this workbook");
        let s = &wb.sheets[si];
        let (r1, c1, r2, c2) = r.area;
        let (d1, e1) = (r1 + top, c1 + left);
        // Row and column categories of this source: by label or position.
        let mut row_of: Vec<Option<usize>> = Vec::new();
        for rr in d1..=r2 {
            row_of.push(if opts.left_col {
                label_key(s.cell(rr, c1)).map(|k| rows.add(Key::Label(k), s.cell(rr, c1)))
            } else {
                Some(rows.add(Key::Pos(rr - d1), None))
            });
        }
        let mut col_of: Vec<Option<usize>> = Vec::new();
        for cc in e1..=c2 {
            col_of.push(if opts.top_row {
                label_key(s.cell(r1, cc)).map(|k| cols.add(Key::Label(k), s.cell(r1, cc)))
            } else {
                Some(cols.add(Key::Pos(cc - e1), None))
            });
        }
        any |= rows.len() > 0 && cols.len() > 0 && (opts.top_row || opts.left_col);
        if col_of.is_empty() {
            continue;
        }
        for (i, ro) in row_of.iter().enumerate() {
            let Some(ro) = *ro else { continue };
            let rr = d1 + i as u32;
            for (&(_, cc), cell) in s.cells.range((rr, e1)..=(rr, c2)) {
                if cell.value.is_empty() {
                    continue;
                }
                let Some(co) = col_of[(cc - e1) as usize] else {
                    continue;
                };
                any = true;
                hits.entry((ro, co)).or_default().push(Hit {
                    src,
                    sheet: si,
                    row: rr,
                    col: cc,
                });
            }
        }
    }
    if !any || rows.len() == 0 || cols.len() == 0 {
        return Err(ConsolidateError::Empty);
    }

    // Lay the output out: the static grid, or detail and summary rows.
    let data_col = col + left + u32::from(opts.links);
    let first_row = row + top;
    let mut plan: Vec<(u32, u32, Cell)> = Vec::new();
    let mut outline: Vec<(u32, u8, bool, bool)> = Vec::new(); // row, level, hidden, collapsed
    if opts.top_row {
        for (j, l) in cols.labels.iter().enumerate() {
            if let Some(l) = l {
                plan.push((row, data_col + j as u32, l.clone()));
            }
        }
    }
    let src_cell = |h: &Hit| wb.sheets[h.sheet].cell(h.row, h.col).expect("a hit");
    let mut cur = first_row;
    for i in 0..rows.keys.len() {
        if opts.links {
            // The contributing source rows, in list order, each as many
            // detail rows as it has cells under one output column.
            let mut groups: Vec<(usize, u32)> = Vec::new();
            for j in 0..cols.keys.len() {
                for h in hits.get(&(i, j)).into_iter().flatten() {
                    if !groups.contains(&(h.src, h.row)) {
                        groups.push((h.src, h.row));
                    }
                }
            }
            groups.sort_by_key(|&(src, r)| (src, r));
            let detail_first = cur;
            for &(src, srow) in &groups {
                let mut per_col: Vec<Vec<&Hit>> = vec![Vec::new(); cols.keys.len()];
                for (j, list) in per_col.iter_mut().enumerate() {
                    list.extend(
                        hits.get(&(i, j))
                            .into_iter()
                            .flatten()
                            .filter(|h| h.src == src && h.row == srow),
                    );
                }
                let depth = per_col.iter().map(Vec::len).max().unwrap_or(0);
                for t in 0..depth {
                    plan.push((cur, data_col - 1, Cell::text(&opts.book_name)));
                    for (j, list) in per_col.iter().enumerate() {
                        if let Some(h) = list.get(t) {
                            let f = format!(
                                "{}!{}",
                                quote_sheet_name(&wb.sheets[h.sheet].name),
                                abs_cell(h.row, h.col)
                            );
                            let mut c = Cell::formula(&f);
                            c.style = src_cell(h).style;
                            plan.push((cur, data_col + j as u32, c));
                        }
                    }
                    outline.push((cur, 1, true, false));
                    cur += 1;
                }
            }
            if opts.left_col {
                if let Some(l) = &rows.labels[i] {
                    plan.push((cur, col, l.clone()));
                }
            }
            for j in 0..cols.keys.len() {
                let c = data_col + j as u32;
                let Some(first) = hits.get(&(i, j)).and_then(|v| v.first()) else {
                    continue;
                };
                let f = format!(
                    "{}({}:{})",
                    consolidate_fn_name(opts.func),
                    cell_name(detail_first, c),
                    cell_name(cur - 1, c)
                );
                let mut cell = Cell::formula(&f);
                cell.style = src_cell(first).style;
                plan.push((cur, c, cell));
            }
            outline.push((cur, 0, false, cur > detail_first));
            cur += 1;
        } else {
            if opts.left_col {
                if let Some(l) = &rows.labels[i] {
                    plan.push((cur, col, l.clone()));
                }
            }
            for j in 0..cols.keys.len() {
                let Some(list) = hits.get(&(i, j)) else {
                    continue;
                };
                let vals: Vec<&CellValue> = list.iter().map(|h| &src_cell(h).value).collect();
                if let Some(v) = aggregate(opts.func, &vals) {
                    let cell = Cell {
                        value: v,
                        style: src_cell(&list[0]).style,
                        ..Cell::default()
                    };
                    plan.push((cur, data_col + j as u32, cell));
                }
            }
            cur += 1;
        }
    }
    let last_col = u64::from(data_col) + u64::from(cols.len()) - 1;
    if u64::from(cur) > u64::from(MAX_ROWS) || last_col >= u64::from(MAX_COLS) {
        return Err(ConsolidateError::OffSheet);
    }
    let out: Area = (row, col, cur - 1, last_col as u32);
    if refs
        .iter()
        .any(|r| r.sheet == *dest_name && overlaps(r.area, out))
    {
        return Err(ConsolidateError::OverlapsDest);
    }

    // Write: clear the area (contents and row grouping), then the plan.
    let settings = ConsolidateSettings {
        func: opts.func,
        refs: refs.iter().map(format_consolidate_ref).collect(),
        top_row: opts.top_row,
        left_col: opts.left_col,
        links: opts.links,
    };
    let s: &mut Sheet = &mut wb.sheets[dest];
    let (r1, c1, r2, c2) = out;
    let stale: Vec<(u32, u32)> = s
        .cells
        .range((r1, 0)..=(r2, u32::MAX))
        .map(|(&k, _)| k)
        .filter(|&(_, c)| (c1..=c2).contains(&c))
        .collect();
    for (r, c) in stale {
        s.clear_cell(r, c);
    }
    for r in r1..=r2 {
        s.set_row_outline(r, 0);
        s.set_row_hidden(r, false);
        s.set_row_collapsed(r, false);
    }
    for (r, c, cell) in plan {
        s.set_cell(r, c, cell);
    }
    for (r, level, hidden, collapsed) in outline {
        s.set_row_outline(r, level);
        s.set_row_hidden(r, hidden);
        s.set_row_collapsed(r, collapsed);
    }
    if opts.links {
        s.outline.summary_below = true;
    }
    s.consolidate = Some(settings);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::sheet::parse_cell_name;

    fn sheet_with(name: &str, cells: &[(&str, Cell)]) -> Sheet {
        let mut sheet = Sheet {
            name: name.to_string(),
            ..Sheet::default()
        };
        for (n, cell) in cells {
            let (r, c) = parse_cell_name(n).unwrap();
            sheet.set_cell(r, c, cell.clone());
        }
        sheet
    }

    fn t(s: &str) -> Cell {
        Cell::text(s)
    }

    fn n(x: f64) -> Cell {
        Cell::number(x)
    }

    /// DAT-CASE-034: East has A, B, C over Jan/Feb; West has C, A, D over
    /// Feb, `jan`: a different order, a folded case, a label of its own.
    fn east_west() -> Workbook {
        Workbook {
            sheets: vec![
                sheet_with(
                    "East",
                    &[
                        ("B1", t("Jan")),
                        ("C1", t("Feb")),
                        ("A2", t("A")),
                        ("B2", n(1.0)),
                        ("C2", n(2.0)),
                        ("A3", t("B")),
                        ("B3", n(3.0)),
                        ("C3", n(4.0)),
                        ("A4", t("C")),
                        ("B4", n(5.0)),
                        ("C4", n(6.0)),
                    ],
                ),
                sheet_with(
                    "West",
                    &[
                        ("B1", t("Feb")),
                        ("C1", t("jan")),
                        ("A2", t("c")),
                        ("B2", n(10.0)),
                        ("C2", n(20.0)),
                        ("A3", t("A")),
                        ("B3", n(30.0)),
                        ("C3", n(40.0)),
                        ("A4", t("D")),
                        ("B4", n(50.0)),
                        ("C4", n(60.0)),
                    ],
                ),
                sheet_with("Summary", &[]),
            ],
            ..Workbook::default()
        }
    }

    fn opts(refs: &[&str], top: bool, left: bool) -> ConsolidateOptions {
        ConsolidateOptions {
            func: SubtotalFunc::Sum,
            refs: refs.iter().map(|s| s.to_string()).collect(),
            top_row: top,
            left_col: left,
            links: false,
            book_name: "Book1".into(),
        }
    }

    fn val(w: &Workbook, sheet: usize, cell: &str) -> CellValue {
        let (r, c) = parse_cell_name(cell).unwrap();
        w.sheets[sheet]
            .cell(r, c)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    fn formula(w: &Workbook, sheet: usize, cell: &str) -> String {
        let (r, c) = parse_cell_name(cell).unwrap();
        w.sheets[sheet]
            .cell(r, c)
            .and_then(|c| c.formula.clone())
            .unwrap_or_default()
    }

    fn num(x: f64) -> CellValue {
        CellValue::Number(x)
    }

    fn txt(s: &str) -> CellValue {
        CellValue::Text(s.into())
    }

    fn recalc(w: &mut Workbook) {
        let mut eng = Engine::new(w);
        eng.recalc_all(w);
    }

    #[test]
    fn by_position_aligns_corners_and_grows_to_the_largest() {
        let mut w = Workbook {
            sheets: vec![
                sheet_with(
                    "S1",
                    &[
                        ("C3", n(1.0)),
                        ("D3", n(2.0)),
                        ("C4", n(3.0)),
                        ("D4", n(4.0)),
                    ],
                ),
                sheet_with(
                    "S2",
                    &[
                        ("A1", n(10.0)),
                        ("B1", n(20.0)),
                        ("C1", n(30.0)),
                        ("A2", n(40.0)),
                        ("A3", n(50.0)),
                    ],
                ),
                sheet_with("Out", &[]),
            ],
            ..Workbook::default()
        };
        let o = opts(&["S1!C3:D4", "S2!A1:C3"], false, false);
        assert_eq!(consolidate(&mut w, 2, (1, 1), &o), Ok((1, 1, 3, 3)));
        assert_eq!(val(&w, 2, "B2"), num(11.0));
        assert_eq!(val(&w, 2, "C2"), num(22.0));
        assert_eq!(val(&w, 2, "D2"), num(30.0));
        assert_eq!(val(&w, 2, "B3"), num(43.0));
        assert_eq!(val(&w, 2, "C3"), num(4.0));
        assert_eq!(val(&w, 2, "B4"), num(50.0));
        assert_eq!(val(&w, 2, "C4"), CellValue::Empty, "nothing contributes");
    }

    #[test]
    fn by_category_matches_labels_case_insensitively_in_any_order() {
        let mut w = east_west();
        let o = opts(&["East!A1:C4", "West!A1:C4"], true, true);
        assert_eq!(consolidate(&mut w, 2, (0, 0), &o), Ok((0, 0, 4, 2)));
        // Categories in first-met order, the first meeting's spelling.
        assert_eq!(val(&w, 2, "A1"), CellValue::Empty, "the corner is blank");
        assert_eq!(val(&w, 2, "B1"), txt("Jan"));
        assert_eq!(val(&w, 2, "C1"), txt("Feb"));
        let col_a: Vec<CellValue> = (2..=5).map(|r| val(&w, 2, &format!("A{r}"))).collect();
        assert_eq!(col_a, [txt("A"), txt("B"), txt("C"), txt("D")]);
        // A: East 1/2 + West jan 40, Feb 30.
        assert_eq!(val(&w, 2, "B2"), num(41.0));
        assert_eq!(val(&w, 2, "C2"), num(32.0));
        assert_eq!(val(&w, 2, "B3"), num(3.0), "B is East's alone");
        assert_eq!(val(&w, 2, "B4"), num(25.0), "c folds into C");
        assert_eq!(val(&w, 2, "C4"), num(16.0));
        assert_eq!(val(&w, 2, "B5"), num(60.0), "D is West's alone");
        assert_eq!(val(&w, 2, "C5"), num(50.0));
    }

    #[test]
    fn whole_label_only_mar_is_not_march() {
        let mut w = Workbook {
            sheets: vec![
                sheet_with("S1", &[("A1", t("Mar")), ("B1", n(1.0))]),
                sheet_with(
                    "S2",
                    &[
                        ("A1", t("March")),
                        ("B1", n(2.0)),
                        ("A2", t("MAR")),
                        ("B2", n(4.0)),
                    ],
                ),
                sheet_with("Out", &[]),
            ],
            ..Workbook::default()
        };
        let o = opts(&["S1!A1:B1", "S2!A1:B2"], false, true);
        assert_eq!(consolidate(&mut w, 2, (0, 0), &o), Ok((0, 0, 1, 1)));
        assert_eq!(val(&w, 2, "A1"), txt("Mar"));
        assert_eq!(val(&w, 2, "B1"), num(5.0));
        assert_eq!(val(&w, 2, "A2"), txt("March"));
        assert_eq!(val(&w, 2, "B2"), num(2.0));
    }

    #[test]
    fn number_labels_match_by_value() {
        let mut w = Workbook {
            sheets: vec![
                sheet_with("S1", &[("A1", n(2024.0)), ("B1", n(1.0))]),
                sheet_with("S2", &[("A1", t("2024")), ("B1", n(2.0))]),
                sheet_with("Out", &[]),
            ],
            ..Workbook::default()
        };
        let o = opts(&["S1!A1:B1", "S2!A1:B1"], false, true);
        assert_eq!(consolidate(&mut w, 2, (0, 0), &o), Ok((0, 0, 0, 1)));
        assert_eq!(
            val(&w, 2, "A1"),
            num(2024.0),
            "the first label keeps its number"
        );
        assert_eq!(val(&w, 2, "B1"), num(3.0));
    }

    #[test]
    fn top_row_only_keeps_rows_by_position() {
        let mut w = east_west();
        let o = opts(&["East!B1:C3", "West!B1:C2"], true, false);
        assert_eq!(consolidate(&mut w, 2, (0, 0), &o), Ok((0, 0, 2, 1)));
        assert_eq!(val(&w, 2, "A1"), txt("Jan"));
        assert_eq!(val(&w, 2, "B1"), txt("Feb"));
        // Row 1 of each: East 1/2, West Feb 10 jan 20.
        assert_eq!(val(&w, 2, "A2"), num(21.0));
        assert_eq!(val(&w, 2, "B2"), num(12.0));
        // Row 2: East's alone.
        assert_eq!(val(&w, 2, "A3"), num(3.0));
        assert_eq!(val(&w, 2, "B3"), num(4.0));
    }

    /// One column of mixed kinds: 2, 4, "x", TRUE, blank.
    fn mixed(extra: Option<Cell>) -> Workbook {
        let mut cells = vec![
            ("A1", n(2.0)),
            ("A2", n(4.0)),
            ("A3", t("x")),
            (
                "A4",
                Cell {
                    value: CellValue::Bool(true),
                    ..Cell::default()
                },
            ),
        ];
        if let Some(c) = extra {
            cells.push(("A6", c));
        }
        Workbook {
            sheets: vec![sheet_with("S", &cells), sheet_with("Out", &[])],
            ..Workbook::default()
        }
    }

    fn one(w: &mut Workbook, func: SubtotalFunc) -> CellValue {
        // Six one-cell sources, by position: all land in one output cell.
        let refs: Vec<String> = (1..=6).map(|r| format!("S!A{r}")).collect();
        let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
        let mut o = opts(&refs, false, false);
        o.func = func;
        consolidate(w, 1, (0, 0), &o).unwrap();
        val(w, 1, "A1")
    }

    #[test]
    fn value_kinds_follow_the_worksheet_functions() {
        use SubtotalFunc::*;
        let cases = [
            (Sum, num(6.0)),
            (Count, num(4.0)),
            (Average, num(3.0)),
            (Max, num(4.0)),
            (Min, num(2.0)),
            (Product, num(8.0)),
            (CountNums, num(2.0)),
            (StdDev, num(2f64.sqrt())),
            (StdDevP, num(1.0)),
            (Var, num(2.0)),
            (VarP, num(1.0)),
        ];
        for (f, want) in cases {
            assert_eq!(one(&mut mixed(None), f), want, "{f:?}");
        }
        // An error propagates, except to the counts.
        let err = CellValue::Error("#N/A".into());
        let e = Cell {
            value: err.clone(),
            ..Cell::default()
        };
        for f in [Sum, Average, Max, Min, Product, StdDev, StdDevP, Var, VarP] {
            assert_eq!(one(&mut mixed(Some(e.clone())), f), err, "{f:?}");
        }
        assert_eq!(one(&mut mixed(Some(e.clone())), Count), num(5.0));
        assert_eq!(one(&mut mixed(Some(e)), CountNums), num(2.0));
        // Too few values: the formula's error.
        let mut w = Workbook {
            sheets: vec![sheet_with("S", &[("A1", n(3.0))]), sheet_with("Out", &[])],
            ..Workbook::default()
        };
        let mut o = opts(&["S!A1"], false, false);
        o.func = StdDev;
        consolidate(&mut w, 1, (0, 0), &o).unwrap();
        assert_eq!(val(&w, 1, "A1"), CellValue::Error("#DIV/0!".into()));
        o.func = StdDevP;
        consolidate(&mut w, 1, (0, 0), &o).unwrap();
        assert_eq!(val(&w, 1, "A1"), num(0.0));
        // Only text: Average has no number to divide.
        let mut w = Workbook {
            sheets: vec![sheet_with("S", &[("A1", t("x"))]), sheet_with("Out", &[])],
            ..Workbook::default()
        };
        o.func = Average;
        consolidate(&mut w, 1, (0, 0), &o).unwrap();
        assert_eq!(val(&w, 1, "A1"), CellValue::Error("#DIV/0!".into()));
    }

    #[test]
    fn static_output_does_not_follow_the_source() {
        let mut w = east_west();
        let o = opts(&["East!A1:C4", "West!A1:C4"], true, true);
        consolidate(&mut w, 2, (0, 0), &o).unwrap();
        assert_eq!(formula(&w, 2, "B2"), "", "a plain value");
        w.sheets[0].set_cell(1, 1, n(100.0));
        recalc(&mut w);
        assert_eq!(val(&w, 2, "B2"), num(41.0));
    }

    #[test]
    fn links_write_detail_formulas_hidden_in_an_outline_and_recalc() {
        let mut w = east_west();
        let mut o = opts(&["East!A1:C4", "West!A1:C4"], true, true);
        o.links = true;
        o.book_name = "Sales".into();
        let area = consolidate(&mut w, 2, (0, 0), &o).unwrap();
        // Labels row, then A: 2 details + summary, B: 1 + 1, C: 2 + 1,
        // D: 1 + 1 → rows 1..=11; label, book, Jan, Feb columns.
        assert_eq!(area, (0, 0, 10, 3));
        assert_eq!(val(&w, 2, "C1"), txt("Jan"));
        assert_eq!(val(&w, 2, "D1"), txt("Feb"));
        assert_eq!(val(&w, 2, "B2"), txt("Sales"));
        assert_eq!(
            val(&w, 2, "A2"),
            CellValue::Empty,
            "detail rows carry no label"
        );
        assert_eq!(formula(&w, 2, "C2"), "East!$B$2");
        assert_eq!(formula(&w, 2, "D2"), "East!$C$2");
        assert_eq!(formula(&w, 2, "C3"), "West!$C$3", "West's jan column");
        assert_eq!(formula(&w, 2, "D3"), "West!$B$3");
        assert_eq!(val(&w, 2, "A4"), txt("A"));
        assert_eq!(val(&w, 2, "B4"), CellValue::Empty);
        assert_eq!(formula(&w, 2, "C4"), "SUM(C2:C3)");
        assert_eq!(formula(&w, 2, "D4"), "SUM(D2:D3)");
        assert_eq!(formula(&w, 2, "C6"), "SUM(C5:C5)");
        let s = &w.sheets[2];
        let levels: Vec<u8> = (0..11).map(|r| s.row_outline(r)).collect();
        assert_eq!(levels, [0, 1, 1, 0, 1, 0, 1, 1, 0, 1, 0]);
        let hidden: Vec<bool> = (0..11).map(|r| s.row_hidden(r)).collect();
        assert_eq!(hidden, levels.iter().map(|&l| l == 1).collect::<Vec<_>>());
        assert!(s.row_collapsed(3) && s.row_collapsed(10) && !s.row_collapsed(0));
        assert!(s.outline.summary_below);

        // Recalculated, the summaries are the static result.
        recalc(&mut w);
        let mut st = east_west();
        consolidate(
            &mut st,
            2,
            (0, 0),
            &opts(&["East!A1:C4", "West!A1:C4"], true, true),
        )
        .unwrap();
        let summaries = [("4", "2"), ("6", "3"), ("9", "4"), ("11", "5")];
        for (linked, plain) in summaries {
            for (lc, pc) in [("C", "B"), ("D", "C")] {
                assert_eq!(
                    val(&w, 2, &format!("{lc}{linked}")),
                    val(&st, 2, &format!("{pc}{plain}")),
                    "{lc}{linked}"
                );
            }
        }
        // And they follow the source.
        w.sheets[0].set_cell(1, 1, n(101.0));
        recalc(&mut w);
        assert_eq!(val(&w, 2, "C4"), num(141.0));
    }

    #[test]
    fn links_count_the_way_the_static_result_does() {
        // A blank and a text cell: no detail formula for the blank, so
        // COUNTA and AVERAGE agree with the static result.
        let base = || Workbook {
            sheets: vec![
                sheet_with("S1", &[("A1", n(2.0)), ("B1", t("x"))]),
                sheet_with("S2", &[("A1", n(4.0))]),
                sheet_with("Out", &[]),
            ],
            ..Workbook::default()
        };
        for func in SubtotalFunc::ALL {
            let mut o = opts(&["S1!A1:B1", "S2!A1:B1"], false, false);
            o.func = func;
            let mut st = base();
            consolidate(&mut st, 2, (0, 0), &o).unwrap();
            o.links = true;
            let mut w = base();
            consolidate(&mut w, 2, (0, 0), &o).unwrap();
            assert_eq!(formula(&w, 2, "C2"), "", "S2!B1 is blank: no link");
            recalc(&mut w);
            assert_eq!(val(&w, 2, "B3"), val(&st, 2, "A1"), "{func:?} A");
            assert_eq!(val(&w, 2, "C3"), val(&st, 2, "B1"), "{func:?} B");
        }
    }

    /// Every sheet, for refusal tests.
    fn snapshot(w: &Workbook) -> Vec<(String, Option<ConsolidateSettings>)> {
        w.sheets
            .iter()
            .map(|s| {
                (
                    format!("{:?}{:?}", s.cells, s.row_attrs),
                    s.consolidate.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn links_with_a_source_on_the_destination_sheet_are_refused_and_nothing_changes() {
        let mut w = east_west();
        let before = snapshot(&w);
        let mut o = opts(&["East!A1:C4", "Summary!A1:C4"], true, true);
        o.links = true;
        assert_eq!(
            consolidate(&mut w, 2, (10, 10), &o),
            Err(ConsolidateError::LinksOnDestSheet)
        );
        o.refs = vec!["East!A1:C4".into(), "A1:C4".into()];
        assert_eq!(
            consolidate(&mut w, 2, (10, 10), &o),
            Err(ConsolidateError::LinksOnDestSheet),
            "a bare range is on the destination"
        );
        assert_eq!(snapshot(&w), before);
    }

    #[test]
    fn a_source_overlapping_the_output_is_refused() {
        let mut w = east_west();
        let before = snapshot(&w);
        let o = opts(&["East!A1:C4", "West!A1:C4"], true, true);
        assert_eq!(
            consolidate(&mut w, 0, (2, 2), &o),
            Err(ConsolidateError::OverlapsDest)
        );
        assert_eq!(snapshot(&w), before);
        // Beside the sources it goes.
        assert!(consolidate(&mut w, 0, (0, 4), &o).is_ok());
    }

    #[test]
    fn bad_and_unknown_references_are_refused() {
        let mut w = east_west();
        let before = snapshot(&w);
        let o = |refs: &[&str]| opts(refs, false, false);
        assert_eq!(
            consolidate(&mut w, 2, (0, 0), &o(&[])),
            Err(ConsolidateError::NoRefs)
        );
        for bad in [
            "Nowhere!A1:B2",
            "East!A1:",
            "East!ZZZZ1",
            "[Book2.xlsx]East!A1",
            "'East!A1",
        ] {
            assert_eq!(
                consolidate(&mut w, 2, (0, 0), &o(&["East!A1:C4", bad])),
                Err(ConsolidateError::BadRef(bad.into())),
                "{bad}"
            );
        }
        assert_eq!(
            consolidate(&mut w, 2, (0, 0), &o(&["East!Z100:Z200"])),
            Err(ConsolidateError::Empty)
        );
        assert_eq!(snapshot(&w), before);
    }

    #[test]
    fn parse_and_format_references_round_trip() {
        let mut w = east_west();
        w.sheets.push(sheet_with("My sheet", &[]));
        w.sheets.push(sheet_with("It's", &[]));
        let p = |s: &str| format_consolidate_ref(&parse_consolidate_ref(&w, 2, s).unwrap());
        assert_eq!(p("East!A1:D4"), "East!$A$1:$D$4");
        assert_eq!(p("=east!$a$1:$d$4"), "East!$A$1:$D$4");
        assert_eq!(p("'My sheet'!$B$2:C3"), "'My sheet'!$B$2:$C$3");
        assert_eq!(p("'It''s'!B2"), "'It''s'!$B$2");
        assert_eq!(
            p("D4:A1"),
            "Summary!$A$1:$D$4",
            "a bare range: the destination"
        );
        assert_eq!(p(&p("'My sheet'!B2:C3")), "'My sheet'!$B$2:$C$3");
    }

    #[test]
    fn settings_are_remembered_on_the_destination() {
        let mut w = east_west();
        let mut o = opts(&["east!a1:c4", "West!A1:C4"], true, false);
        o.func = SubtotalFunc::Average;
        consolidate(&mut w, 2, (0, 0), &o).unwrap();
        assert_eq!(
            w.sheets[2].consolidate,
            Some(ConsolidateSettings {
                func: SubtotalFunc::Average,
                refs: vec!["East!$A$1:$C$4".into(), "West!$A$1:$C$4".into()],
                top_row: true,
                left_col: false,
                links: false,
            })
        );
        assert_eq!(w.sheets[0].consolidate, None);
    }

    #[test]
    fn a_rerun_clears_the_old_output_and_its_grouping() {
        let mut w = east_west();
        let mut o = opts(&["East!A1:C4", "West!A1:C4"], true, true);
        o.links = true;
        consolidate(&mut w, 2, (0, 0), &o).unwrap();
        o.links = false;
        assert_eq!(consolidate(&mut w, 2, (0, 0), &o), Ok((0, 0, 4, 2)));
        let s = &w.sheets[2];
        assert_eq!(formula(&w, 2, "C4"), "");
        assert!((0..5).all(|r| s.row_outline(r) == 0 && !s.row_hidden(r)));
        assert_eq!(val(&w, 2, "B2"), num(41.0));
    }

    #[test]
    fn a_settings_only_change_is_an_undoable_difference() {
        // Same output cells, a different reference list: the apps record
        // an undo step (and a dirty workbook) only when sheets_differ.
        let mut w = east_west();
        let o = opts(&["East!A1:C4", "West!A1:C4"], true, true);
        consolidate(&mut w, 2, (0, 0), &o).unwrap();
        let before = w.sheets.clone();
        let o = opts(&["East!A1:C4", "West!A1:C4", "East!Z90:Z99"], true, true);
        consolidate(&mut w, 2, (0, 0), &o).unwrap();
        assert_eq!(before[2].cells, w.sheets[2].cells);
        assert!(super::super::sheets_differ(&before, &w.sheets));
    }

    #[test]
    fn func_names_and_tokens_parse() {
        for f in SubtotalFunc::ALL {
            assert_eq!(parse_consolidate_func(f.name()), Some(f));
            assert_eq!(parse_consolidate_func(consolidate_token(f)), Some(f));
        }
        assert_eq!(
            parse_consolidate_func("count numbers"),
            Some(SubtotalFunc::CountNums)
        );
        assert_eq!(
            parse_consolidate_func("STDDEVP"),
            Some(SubtotalFunc::StdDevP)
        );
        assert_eq!(parse_consolidate_func("median"), None);
    }
}
