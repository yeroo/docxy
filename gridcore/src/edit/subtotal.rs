//! Data ▸ Subtotal: total each run of equal values in a column, outline the
//! result, and take it out again (Remove All).
//!
//! Every total row goes in through [`insert_rows`], one group at a time from
//! the bottom up, so the rows below, every formula pointing into the region
//! (from this sheet or another), and the detail rows' own attributes move
//! the way a hand-inserted row moves them. [`remove_subtotals`] deletes the
//! same rows through [`delete_rows`], which makes it the exact inverse. As in
//! Excel, a `SUM` elsewhere over the whole region stretches to take in the
//! inserted rows, so it then counts the totals too.

use std::fmt;

use super::{delete_rows, insert_rows};
use crate::sheet::{Cell, CellValue, Sheet, Workbook, cell_name};

/// The functions of Excel's Subtotal dialog, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SubtotalFunc {
    #[default]
    Sum,
    Count,
    Average,
    Max,
    Min,
    Product,
    CountNums,
    StdDev,
    StdDevP,
    Var,
    VarP,
}

impl SubtotalFunc {
    pub const ALL: [SubtotalFunc; 11] = [
        SubtotalFunc::Sum,
        SubtotalFunc::Count,
        SubtotalFunc::Average,
        SubtotalFunc::Max,
        SubtotalFunc::Min,
        SubtotalFunc::Product,
        SubtotalFunc::CountNums,
        SubtotalFunc::StdDev,
        SubtotalFunc::StdDevP,
        SubtotalFunc::Var,
        SubtotalFunc::VarP,
    ];

    /// `SUBTOTAL`'s function number.
    pub fn code(self) -> u8 {
        match self {
            SubtotalFunc::Average => 1,
            SubtotalFunc::CountNums => 2,
            SubtotalFunc::Count => 3,
            SubtotalFunc::Max => 4,
            SubtotalFunc::Min => 5,
            SubtotalFunc::Product => 6,
            SubtotalFunc::StdDev => 7,
            SubtotalFunc::StdDevP => 8,
            SubtotalFunc::Sum => 9,
            SubtotalFunc::Var => 10,
            SubtotalFunc::VarP => 11,
        }
    }

    /// The name the dialog lists.
    pub fn name(self) -> &'static str {
        match self {
            SubtotalFunc::Sum => "Sum",
            SubtotalFunc::Count => "Count",
            SubtotalFunc::Average => "Average",
            SubtotalFunc::Max => "Max",
            SubtotalFunc::Min => "Min",
            SubtotalFunc::Product => "Product",
            SubtotalFunc::CountNums => "Count Numbers",
            SubtotalFunc::StdDev => "StdDev",
            SubtotalFunc::StdDevP => "StdDevp",
            SubtotalFunc::Var => "Var",
            SubtotalFunc::VarP => "Varp",
        }
    }

    /// The word Excel writes after the key, and after "Grand": "A Total",
    /// "Grand Total"; "A Count", "Grand Count".
    pub fn label(self) -> &'static str {
        match self {
            SubtotalFunc::Sum => "Total",
            SubtotalFunc::Count | SubtotalFunc::CountNums => "Count",
            other => other.name(),
        }
    }
}

/// What the Subtotal dialog asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtotalOptions {
    /// "At each change in": the column whose runs of equal values group.
    pub group_col: u32,
    /// "Use function".
    pub func: SubtotalFunc,
    /// "Add subtotal to": the columns that get a `SUBTOTAL` formula.
    pub add_to: Vec<u32>,
    /// "Replace current subtotals": Remove All first. Without it the new
    /// subtotals nest inside the ones already there.
    pub replace: bool,
    /// "Page break between groups".
    pub page_breaks: bool,
    /// "Summary below data"; false puts each total above its group.
    pub summary_below: bool,
    /// The region's first row is a header and is left alone.
    pub has_header: bool,
}

impl SubtotalOptions {
    /// Excel's defaults: Sum, replace, no page breaks, summary below.
    pub fn new(group_col: u32, add_to: Vec<u32>, has_header: bool) -> Self {
        SubtotalOptions {
            group_col,
            func: SubtotalFunc::Sum,
            add_to,
            replace: true,
            page_breaks: false,
            summary_below: true,
            has_header,
        }
    }
}

/// Why Subtotal changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtotalError {
    /// No column was chosen to add a subtotal to.
    NoColumns,
    /// The region holds no rows to total.
    Empty,
}

impl fmt::Display for SubtotalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SubtotalError::NoColumns => "Choose at least one column to add a subtotal to.",
            SubtotalError::Empty => "There is no data to subtotal here.",
        })
    }
}

impl std::error::Error for SubtotalError {}

/// Whether the row holds a `SUBTOTAL` formula (any case, `_xlfn.` or not):
/// what Remove All deletes and nesting treats as an existing total.
pub fn is_subtotal_row(s: &Sheet, row: u32) -> bool {
    s.cells.range((row, 0)..=(row, u32::MAX)).any(|(_, c)| {
        c.formula
            .as_deref()
            .is_some_and(|f| f.to_ascii_uppercase().contains("SUBTOTAL("))
    })
}

/// The block of non-blank rows around `row` that Subtotal and Remove All
/// work on, as (top, bottom, has_header): the first row is a header when its
/// cell in `col` is text. `None` when `row` is blank.
pub fn subtotal_region(s: &Sheet, row: u32, col: u32) -> Option<(u32, u32, bool)> {
    let used = |r: u32| {
        s.cells
            .range((r, 0)..=(r, u32::MAX))
            .any(|(_, c)| !c.is_blank())
    };
    if !used(row) {
        return None;
    }
    let mut top = row;
    while top > 0 && used(top - 1) {
        top -= 1;
    }
    let mut bottom = row;
    while used(bottom + 1) {
        bottom += 1;
    }
    let header = matches!(s.cell(top, col).map(|c| &c.value), Some(CellValue::Text(_)));
    Some((top, bottom, header))
}

/// The columns holding numbers in rows `r1..=r2` other than `group_col` and
/// the total rows: what the dialog offers checked by default.
pub fn numeric_columns(s: &Sheet, r1: u32, r2: u32, group_col: u32) -> Vec<u32> {
    let mut cols: Vec<u32> = s
        .cells
        .range((r1, 0)..=(r2, u32::MAX))
        .filter(|&(&(r, c), cell)| {
            c != group_col
                && matches!(cell.value, CellValue::Number(_))
                && cell.formula.is_none()
                && !is_subtotal_row(s, r)
        })
        .map(|(&(_, c), _)| c)
        .collect();
    cols.sort_unstable();
    cols.dedup();
    cols
}

/// Insert subtotals into rows `r1..=r2` (sorted by `opts.group_col` first,
/// as Excel requires). Each run of equal values gets a total row, and a grand
/// total goes after them all (before, with summaries above). The outline
/// follows Excel's: detail at level 2, group totals at 1, the grand total
/// at 0. Without `replace`, the new totals nest inside the existing ones.
/// Returns the number of rows inserted.
pub fn subtotal(
    wb: &mut Workbook,
    sheet: usize,
    r1: u32,
    r2: u32,
    opts: &SubtotalOptions,
) -> Result<usize, SubtotalError> {
    let add_to: Vec<u32> = opts
        .add_to
        .iter()
        .copied()
        .filter(|&c| c != opts.group_col)
        .collect();
    if add_to.is_empty() {
        return Err(SubtotalError::NoColumns);
    }
    let start = r1 + u32::from(opts.has_header);
    if wb.sheets.get(sheet).is_none() || r2 < start {
        return Err(SubtotalError::Empty);
    }
    let mut r2 = r2;
    if opts.replace {
        r2 -= remove_subtotals(wb, sheet, r1, r2) as u32;
        if r2 < start {
            return Err(SubtotalError::Empty);
        }
    }
    let below = opts.summary_below;
    let code = opts.func.code();
    let word = opts.func.label();

    // The runs to total: equal keys, never across an existing total row.
    let (groups, nested) = {
        let s = &wb.sheets[sheet];
        let key = |r: u32| {
            s.cell(r, opts.group_col)
                .map(|c| format!("{:?}", c.value))
                .unwrap_or_default()
        };
        let mut groups: Vec<(String, u32, u32)> = Vec::new();
        let mut nested = false;
        let mut open: Option<(u32, u32)> = None;
        for r in start..=r2 {
            if is_subtotal_row(s, r) {
                nested = true;
                if let Some((a, b)) = open.take() {
                    groups.push((label_of(s, a, opts.group_col), a, b));
                }
                continue;
            }
            open = match open {
                Some((a, b)) if key(a) == key(r) => Some((a, r.max(b))),
                Some((a, b)) => {
                    groups.push((label_of(s, a, opts.group_col), a, b));
                    Some((r, r))
                }
                None => Some((r, r)),
            };
        }
        if let Some((a, b)) = open {
            groups.push((label_of(s, a, opts.group_col), a, b));
        }
        (groups, nested)
    };
    if groups.is_empty() {
        return Err(SubtotalError::Empty);
    }
    let grand_level = wb.sheets[sheet].row_outline(start);

    // Where the new total rows are now, kept up to date as rows go in.
    let mut totals: Vec<u32> = Vec::with_capacity(groups.len());
    let insert = |wb: &mut Workbook, at: u32, totals: &mut Vec<u32>| {
        insert_rows(wb, sheet, at, 1);
        for t in totals.iter_mut().filter(|t| **t >= at) {
            *t += 1;
        }
    };
    for (label, first, last) in groups.iter().rev() {
        let s = &mut wb.sheets[sheet];
        // A fresh subtotal also opens the grand total's group, so its detail
        // goes two levels down; nested ones go one below the existing.
        let old = s.row_outline(*first);
        let (total_level, detail_level) = if nested {
            (old, old + 1)
        } else {
            (old + 1, old + 2)
        };
        for r in *first..=*last {
            s.set_row_outline(r, detail_level.min(crate::outline::MAX_LEVEL));
        }
        let at = if below { last + 1 } else { *first };
        insert(wb, at, &mut totals);
        let (f, l) = if below {
            (*first, *last)
        } else {
            (first + 1, last + 1)
        };
        let text = if label.is_empty() {
            word.to_string()
        } else {
            format!("{label} {word}")
        };
        write_total(
            &mut wb.sheets[sheet],
            at,
            opts.group_col,
            &text,
            (f, l),
            &add_to,
            code,
        );
        wb.sheets[sheet].set_row_outline(at, total_level);
        totals.push(at);
    }
    totals.sort_unstable();

    // The grand total: after (before) everything; when nesting, right
    // beside the grand total already there.
    let end = r2 + groups.len() as u32;
    let s = &wb.sheets[sheet];
    let (at, range, level) = match (below, nested) {
        (true, true) if is_subtotal_row(s, end) => (end, (start, end - 1), s.row_outline(end)),
        (false, true) if is_subtotal_row(s, start) => {
            (start + 1, (start + 2, end + 1), s.row_outline(start))
        }
        (true, _) => (end + 1, (start, end), grand_level),
        (false, _) => (start, (start + 1, end + 1), grand_level),
    };
    insert(wb, at, &mut totals);
    let grand = format!("Grand {word}");
    let s = &mut wb.sheets[sheet];
    write_total(s, at, opts.group_col, &grand, range, &add_to, code);
    s.set_row_outline(at, level);
    s.outline.summary_below = below;

    if opts.page_breaks {
        // A new page after each group's block, but not after the last.
        let breaks: Vec<u32> = if below {
            totals[..totals.len() - 1].iter().map(|t| t + 1).collect()
        } else {
            totals[1..].to_vec()
        };
        for id in breaks {
            crate::print::area::add_break(&mut s.row_breaks, id, true);
        }
    }
    Ok(groups.len() + 1)
}

/// The group key as the total's label shows it.
fn label_of(s: &Sheet, row: u32, col: u32) -> String {
    match s.cell(row, col).map(|c| &c.value) {
        Some(CellValue::Text(t)) => t.clone(),
        Some(CellValue::Number(n)) => n.to_string(),
        Some(CellValue::Bool(b)) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        _ => String::new(),
    }
}

/// Fill the (blank, just inserted) total row `at`: the label in the group
/// column and a `SUBTOTAL` over `first..=last` in each total column, styled
/// like the detail cell above it so number formats carry over.
fn write_total(
    s: &mut Sheet,
    at: u32,
    group_col: u32,
    label: &str,
    (first, last): (u32, u32),
    cols: &[u32],
    code: u8,
) {
    s.set_cell(at, group_col, Cell::text(label));
    for &c in cols {
        let mut cell = Cell::formula(&format!(
            "SUBTOTAL({code},{}:{})",
            cell_name(first, c),
            cell_name(last, c)
        ));
        cell.style = s.cell(last, c).map(|x| x.style).unwrap_or(0);
        s.set_cell(at, c, cell);
    }
}

/// Remove All: delete the rows in `r1..=r2` that hold a `SUBTOTAL` formula
/// (through the formula-adjusting row delete), then take the outline off the
/// rows left, show the ones it hid, and drop the manual page breaks inside.
/// Returns the number of rows deleted.
pub fn remove_subtotals(wb: &mut Workbook, sheet: usize, r1: u32, r2: u32) -> usize {
    let Some(s) = wb.sheets.get(sheet) else {
        return 0;
    };
    let rows: Vec<u32> = (r1..=r2).filter(|&r| is_subtotal_row(s, r)).collect();
    // Bottom-up, one delete per run of adjacent rows.
    let mut i = rows.len();
    while i > 0 {
        let last = rows[i - 1];
        let mut first = last;
        while i > 1 && rows[i - 2] + 1 == first {
            i -= 1;
            first = rows[i - 1];
        }
        i -= 1;
        delete_rows(wb, sheet, first, last - first + 1);
    }
    let n = rows.len();
    let s = &mut wb.sheets[sheet];
    let Some(end) = r2.checked_sub(n as u32).filter(|&e| e >= r1) else {
        return n;
    };
    for r in r1..=end {
        if s.row_outline(r) > 0 {
            if s.row_hidden(r) && !s.row_filtered(r) {
                s.set_row_hidden(r, false);
            }
            s.set_row_outline(r, 0);
        }
        if s.row_collapsed(r) {
            s.set_row_collapsed(r, false);
        }
    }
    // The row right after the region may be the summary of a group that is
    // gone now.
    if s.row_collapsed(end + 1) && s.row_outline(end) == 0 {
        s.set_row_collapsed(end + 1, false);
    }
    s.row_breaks
        .retain(|b| !(b.is_manual() && b.id > r1 && b.id <= end + 1));
    n
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

    /// Grp/Amt, three A rows and two B rows (A1:B6).
    fn sales() -> Workbook {
        Workbook {
            sheets: vec![sheet_with(
                "Sheet1",
                &[
                    ("A1", Cell::text("Grp")),
                    ("B1", Cell::text("Amt")),
                    ("A2", Cell::text("A")),
                    ("B2", Cell::number(1.0)),
                    ("A3", Cell::text("A")),
                    ("B3", Cell::number(2.0)),
                    ("A4", Cell::text("A")),
                    ("B4", Cell::number(3.0)),
                    ("A5", Cell::text("B")),
                    ("B5", Cell::number(4.0)),
                    ("A6", Cell::text("B")),
                    ("B6", Cell::number(5.0)),
                ],
            )],
            ..Workbook::default()
        }
    }

    fn opts() -> SubtotalOptions {
        SubtotalOptions::new(0, vec![1], true)
    }

    fn text(w: &Workbook, cell: &str) -> String {
        let (r, c) = parse_cell_name(cell).unwrap();
        match w.sheets[0].cell(r, c).map(|c| &c.value) {
            Some(CellValue::Text(t)) => t.clone(),
            Some(CellValue::Number(n)) => n.to_string(),
            _ => String::new(),
        }
    }

    fn formula(w: &Workbook, sheet: usize, cell: &str) -> String {
        let (r, c) = parse_cell_name(cell).unwrap();
        w.sheets[sheet]
            .cell(r, c)
            .and_then(|c| c.formula.clone())
            .unwrap_or_default()
    }

    fn num(w: &mut Workbook, cell: &str) -> f64 {
        let mut eng = Engine::new(w);
        eng.recalc_all(w);
        let (r, c) = parse_cell_name(cell).unwrap();
        match w.sheets[0].cell(r, c).map(|c| &c.value) {
            Some(CellValue::Number(n)) => *n,
            other => panic!("{cell}: {other:?}"),
        }
    }

    fn levels(w: &Workbook, a: u32, b: u32) -> Vec<u8> {
        (a..=b).map(|r| w.sheets[0].row_outline(r)).collect()
    }

    fn column_a(w: &Workbook, rows: u32) -> Vec<String> {
        (1..=rows).map(|r| text(w, &format!("A{r}"))).collect()
    }

    #[test]
    fn subtotal_inserts_group_and_grand_totals() {
        let mut w = sales();
        assert_eq!(subtotal(&mut w, 0, 0, 5, &opts()), Ok(3));
        assert_eq!(
            column_a(&w, 9),
            [
                "Grp",
                "A",
                "A",
                "A",
                "A Total",
                "B",
                "B",
                "B Total",
                "Grand Total"
            ]
        );
        assert_eq!(formula(&w, 0, "B5"), "SUBTOTAL(9,B2:B4)");
        assert_eq!(formula(&w, 0, "B8"), "SUBTOTAL(9,B6:B7)");
        assert_eq!(formula(&w, 0, "B9"), "SUBTOTAL(9,B2:B8)");
        // Excel's levels: detail 2, group totals 1, grand total 0.
        assert_eq!(levels(&w, 0, 8), [0, 2, 2, 2, 1, 2, 2, 1, 0]);
        assert!(w.sheets[0].outline.summary_below);
        assert_eq!(num(&mut w, "B5"), 6.0);
        assert_eq!(num(&mut w, "B8"), 9.0);
        assert_eq!(num(&mut w, "B9"), 15.0, "SUBTOTAL skips the nested totals");
    }

    #[test]
    fn each_function_writes_its_code_and_label() {
        for f in SubtotalFunc::ALL {
            let mut w = sales();
            let o = SubtotalOptions { func: f, ..opts() };
            subtotal(&mut w, 0, 0, 5, &o).unwrap();
            let word = f.label();
            assert_eq!(text(&w, "A5"), format!("A {word}"));
            assert_eq!(text(&w, "A9"), format!("Grand {word}"));
            assert_eq!(
                formula(&w, 0, "B5"),
                format!("SUBTOTAL({},B2:B4)", f.code())
            );
        }
        let mut w = sales();
        let o = SubtotalOptions {
            func: SubtotalFunc::Average,
            ..opts()
        };
        subtotal(&mut w, 0, 0, 5, &o).unwrap();
        assert_eq!(num(&mut w, "B5"), 2.0);
        assert_eq!(num(&mut w, "B9"), 3.0);
        assert_eq!(SubtotalFunc::CountNums.label(), "Count");
        assert_eq!(SubtotalFunc::StdDevP.label(), "StdDevp");
    }

    #[test]
    fn summary_above_puts_totals_over_their_groups() {
        let mut w = sales();
        let o = SubtotalOptions {
            summary_below: false,
            ..opts()
        };
        subtotal(&mut w, 0, 0, 5, &o).unwrap();
        assert_eq!(
            column_a(&w, 9),
            [
                "Grp",
                "Grand Total",
                "A Total",
                "A",
                "A",
                "A",
                "B Total",
                "B",
                "B"
            ]
        );
        assert_eq!(formula(&w, 0, "B2"), "SUBTOTAL(9,B3:B9)");
        assert_eq!(formula(&w, 0, "B3"), "SUBTOTAL(9,B4:B6)");
        assert_eq!(formula(&w, 0, "B7"), "SUBTOTAL(9,B8:B9)");
        assert_eq!(levels(&w, 0, 8), [0, 0, 1, 2, 2, 2, 1, 2, 2]);
        assert!(!w.sheets[0].outline.summary_below);
        assert_eq!(num(&mut w, "B2"), 15.0);
        // The outline reads it the same way: row 3 heads rows 4..=6.
        let g = crate::outline::groups(&w.sheets[0], crate::outline::Axis::Rows);
        assert!(
            g.iter()
                .any(|g| g.start == 3 && g.end == 5 && g.summary == Some(2))
        );
    }

    #[test]
    fn page_breaks_fall_between_groups() {
        let mut w = sales();
        let o = SubtotalOptions {
            page_breaks: true,
            ..opts()
        };
        subtotal(&mut w, 0, 0, 5, &o).unwrap();
        // After "A Total" (row 5), not after "B Total".
        assert_eq!(crate::print::area::manual_breaks(&w.sheets[0]).0, [5]);
        let mut w = sales();
        let o = SubtotalOptions {
            page_breaks: true,
            summary_below: false,
            ..opts()
        };
        subtotal(&mut w, 0, 0, 5, &o).unwrap();
        // Before "B Total" (row 7).
        assert_eq!(crate::print::area::manual_breaks(&w.sheets[0]).0, [6]);
        // Remove All takes them out again.
        remove_subtotals(&mut w, 0, 0, 8);
        assert!(crate::print::area::manual_breaks(&w.sheets[0]).0.is_empty());
    }

    #[test]
    fn formulas_and_row_attributes_follow_the_detail_and_come_back() {
        let mut w = sales();
        // A formula below the region and one on another sheet, both into the
        // detail, and a height on a detail row.
        w.sheets[0].set_cell(7, 0, Cell::formula("B3*10"));
        w.sheets
            .push(sheet_with("Other", &[("A1", Cell::formula("Sheet1!B5"))]));
        w.sheets[0].set_row_height(2, Some(30.0));
        subtotal(&mut w, 0, 0, 5, &opts()).unwrap();
        // B3 stays (inserted rows are all below it); B5 (first B) moved to B6.
        assert_eq!(formula(&w, 0, "A11"), "B3*10");
        assert_eq!(formula(&w, 1, "A1"), "Sheet1!B6");
        assert_eq!(w.sheets[0].row_height(2), Some(30.0));
        assert_eq!(num(&mut w, "A11"), 20.0);
        // Summaries above move every detail row; the references follow.
        let mut v = sales();
        v.sheets[0].set_cell(7, 0, Cell::formula("B3*10"));
        v.sheets[0].set_row_height(2, Some(30.0));
        let above = SubtotalOptions {
            summary_below: false,
            ..opts()
        };
        subtotal(&mut v, 0, 0, 5, &above).unwrap();
        assert_eq!(formula(&v, 0, "A11"), "B5*10");
        assert_eq!(v.sheets[0].row_height(4), Some(30.0));
        assert_eq!(num(&mut v, "A11"), 20.0);
        // Remove All is the inverse.
        assert_eq!(remove_subtotals(&mut w, 0, 0, 8), 3);
        assert_eq!(formula(&w, 0, "A8"), "B3*10");
        assert_eq!(formula(&w, 1, "A1"), "Sheet1!B5");
        assert_eq!(w.sheets[0].row_height(2), Some(30.0));
        assert_eq!(levels(&w, 0, 7), [0; 8]);
        assert_eq!(column_a(&w, 6), ["Grp", "A", "A", "A", "B", "B"]);
        assert_eq!(remove_subtotals(&mut v, 0, 0, 8), 3);
        assert_eq!(formula(&v, 0, "A8"), "B3*10");
        assert_eq!(v.sheets[0].row_height(2), Some(30.0));
    }

    #[test]
    fn remove_all_shows_rows_the_outline_hid_but_not_filtered_ones() {
        let mut w = sales();
        subtotal(&mut w, 0, 0, 5, &opts()).unwrap();
        crate::outline::show_level(&mut w.sheets[0], crate::outline::Axis::Rows, 2);
        w.sheets[0].set_row_filtered(6, true);
        remove_subtotals(&mut w, 0, 0, 8);
        let s = &w.sheets[0];
        let hidden: Vec<u32> = (0..8).filter(|&r| s.row_hidden(r)).collect();
        // Row 7 ("B", filtered) was row 6 before the deletes above it.
        assert_eq!(hidden, [5]);
        assert!(!(0..9).any(|r| s.row_collapsed(r)));
    }

    #[test]
    fn replace_takes_out_the_old_subtotals_first() {
        let mut w = sales();
        subtotal(&mut w, 0, 0, 5, &opts()).unwrap();
        let o = SubtotalOptions {
            func: SubtotalFunc::Count,
            ..opts()
        };
        assert_eq!(subtotal(&mut w, 0, 0, 8, &o), Ok(3));
        assert_eq!(
            column_a(&w, 9),
            [
                "Grp",
                "A",
                "A",
                "A",
                "A Count",
                "B",
                "B",
                "B Count",
                "Grand Count"
            ]
        );
        assert_eq!(levels(&w, 0, 8), [0, 2, 2, 2, 1, 2, 2, 1, 0]);
    }

    #[test]
    fn without_replace_new_subtotals_nest_inside_the_old() {
        // Region / Product: two regions, the first with two products.
        let mut w = Workbook {
            sheets: vec![sheet_with(
                "Sheet1",
                &[
                    ("A1", Cell::text("Region")),
                    ("B1", Cell::text("Product")),
                    ("C1", Cell::text("Amt")),
                    ("A2", Cell::text("E")),
                    ("B2", Cell::text("p")),
                    ("C2", Cell::number(1.0)),
                    ("A3", Cell::text("E")),
                    ("B3", Cell::text("q")),
                    ("C3", Cell::number(2.0)),
                    ("A4", Cell::text("W")),
                    ("B4", Cell::text("q")),
                    ("C4", Cell::number(4.0)),
                ],
            )],
            ..Workbook::default()
        };
        let by_region = SubtotalOptions::new(0, vec![2], true);
        subtotal(&mut w, 0, 0, 3, &by_region).unwrap();
        // Grp, E, E, E Total, W, W Total, Grand Total.
        let by_product = SubtotalOptions {
            group_col: 1,
            func: SubtotalFunc::Count,
            replace: false,
            ..SubtotalOptions::new(1, vec![2], true)
        };
        // "q" in E and "q" in W are separate groups: the total between them
        // splits the run.
        assert_eq!(subtotal(&mut w, 0, 0, 6, &by_product), Ok(4));
        let col =
            |c: &str| -> Vec<String> { (1..=11).map(|r| text(&w, &format!("{c}{r}"))).collect() };
        assert_eq!(
            col("A"),
            [
                "Region",
                "E",
                "",
                "E",
                "",
                "E Total",
                "W",
                "",
                "W Total",
                "",
                "Grand Total"
            ]
        );
        assert_eq!(
            col("B"),
            [
                "Product",
                "p",
                "p Count",
                "q",
                "q Count",
                "",
                "q",
                "q Count",
                "",
                "Grand Count",
                ""
            ]
        );
        // Detail 3, the new totals 2, the old totals 1, both grand rows 0.
        assert_eq!(levels(&w, 0, 10), [0, 3, 2, 3, 2, 1, 3, 2, 1, 0, 0]);
        // The old total stretched over the row inserted inside its range.
        assert_eq!(formula(&w, 0, "C6"), "SUBTOTAL(9,C2:C4)");
        assert_eq!(formula(&w, 0, "C10"), "SUBTOTAL(3,C2:C9)");
        assert_eq!(num(&mut w, "C6"), 3.0);
        assert_eq!(num(&mut w, "C10"), 3.0, "COUNTA skips the nested totals");
        assert_eq!(num(&mut w, "C11"), 7.0);
        // Remove All takes both layers out.
        assert_eq!(remove_subtotals(&mut w, 0, 0, 10), 7);
        assert_eq!(levels(&w, 0, 3), [0; 4]);
    }

    #[test]
    fn refusals_change_nothing() {
        let mut w = sales();
        let before = w.sheets[0].cells.clone();
        let none = SubtotalOptions::new(0, vec![], true);
        assert_eq!(
            subtotal(&mut w, 0, 0, 5, &none),
            Err(SubtotalError::NoColumns)
        );
        let own = SubtotalOptions::new(0, vec![0], true);
        assert_eq!(
            subtotal(&mut w, 0, 0, 5, &own),
            Err(SubtotalError::NoColumns)
        );
        assert_eq!(
            subtotal(&mut w, 0, 0, 0, &opts()),
            Err(SubtotalError::Empty)
        );
        assert_eq!(w.sheets[0].cells, before);
    }

    #[test]
    fn the_region_and_its_numeric_columns() {
        let w = sales();
        let s = &w.sheets[0];
        assert_eq!(subtotal_region(s, 3, 0), Some((0, 5, true)));
        assert_eq!(subtotal_region(s, 3, 1), Some((0, 5, true)));
        assert_eq!(subtotal_region(s, 9, 0), None);
        assert_eq!(numeric_columns(s, 1, 5, 0), [1]);
    }
}
