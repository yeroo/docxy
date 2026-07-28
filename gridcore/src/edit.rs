//! Structural workbook edits: insert/delete rows & columns, sheet renames.
//!
//! These are the operations that are really *reference rewriting* in
//! disguise: every formula in the workbook (and every defined name) must
//! shift with the grid, exactly as Excel does — references into deleted
//! cells become `#REF!`, ranges stretch when rows are inserted inside them,
//! and `$` anchoring plays no role (the grid itself moved).
//!
//! Formulas that don't parse (the unsupported/preserved kind) are left
//! untouched: their text may go stale, but we never corrupt it. That is the
//! same stale-not-wrong contract the engine keeps everywhere else.

use std::collections::BTreeMap;

use crate::formula::{EditShift, ExcelError, adjust_formula_for_edit, rename_sheet_in_formula};
use crate::sheet::{Cell, CellValue, MAX_COLS, MAX_ROWS, Sheet, Workbook};

/// Interpret typed input as Excel would: formulas, numbers (incl. percent),
/// booleans, error constants, text.
pub fn parse_input(text: &str) -> Cell {
    if let Some(body) = text.strip_prefix('=') {
        if !body.is_empty() {
            return Cell::formula(body);
        }
    }
    if text.is_empty() {
        return Cell::default();
    }
    let t = text.trim();
    if let Ok(n) = t.parse::<f64>() {
        if n.is_finite() {
            return Cell::number(n);
        }
    }
    if let Some(pct) = t.strip_suffix('%') {
        if let Ok(n) = pct.trim().parse::<f64>() {
            let v = n / 100.0;
            if v.is_finite() {
                return Cell::number(v);
            }
        }
    }
    if t.eq_ignore_ascii_case("TRUE") {
        return Cell {
            value: CellValue::Bool(true),
            ..Cell::default()
        };
    }
    if t.eq_ignore_ascii_case("FALSE") {
        return Cell {
            value: CellValue::Bool(false),
            ..Cell::default()
        };
    }
    if ExcelError::from_code(t).is_some() {
        return Cell {
            value: CellValue::Error(t.to_ascii_uppercase()),
            ..Cell::default()
        };
    }
    Cell::text(text)
}

/// The text a cell would show in the formula bar (`=formula`, or the value
/// as it would be re-entered) — [`parse_input`]'s inverse, and the surface
/// Find/Replace operates on.
pub fn input_text_of(cell: &Cell) -> String {
    if let Some(f) = &cell.formula {
        format!("={f}")
    } else {
        match &cell.value {
            CellValue::Empty => String::new(),
            CellValue::Number(n) => crate::sheet::fmt_general(*n),
            CellValue::Text(s) => s.clone(),
            CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            CellValue::Error(e) => e.clone(),
        }
    }
}

/// The literal find/replace algorithm shared by the TUI's Find & Replace and
/// the `wb.replace-all` control verb: every cell whose *input text* (its
/// `=formula` source, or the value as it would be re-entered) contains
/// `find` gets `find` replaced with `with`, then reparsed via
/// [`parse_input`], preserving the cell's style. Returns the `(row, col,
/// new_cell)` changes for one sheet — pure; callers decide how to apply
/// them (one sheet under one undo group, or every sheet under one
/// structural snapshot).
pub fn replace_all_in_sheet(sheet: &Sheet, find: &str, with: &str) -> Vec<(u32, u32, Cell)> {
    sheet
        .cells
        .iter()
        .filter_map(|(&(r, c), cell)| {
            let text = input_text_of(cell);
            if text.contains(find) {
                let mut newcell = parse_input(&text.replace(find, with));
                newcell.style = cell.style;
                Some((r, c, newcell))
            } else {
                None
            }
        })
        .collect()
}

/// Insert `count` blank rows before 0-based row `at` on sheet `idx`.
pub fn insert_rows(wb: &mut Workbook, idx: usize, at: u32, count: u32) {
    structural_edit(
        wb,
        idx,
        EditShift {
            rows: true,
            at,
            delta: count as i64,
        },
    );
}

/// Delete `count` rows starting at 0-based row `at` on sheet `idx`.
pub fn delete_rows(wb: &mut Workbook, idx: usize, at: u32, count: u32) {
    structural_edit(
        wb,
        idx,
        EditShift {
            rows: true,
            at,
            delta: -(count as i64),
        },
    );
}

/// Insert `count` blank columns before 0-based column `at` on sheet `idx`.
pub fn insert_cols(wb: &mut Workbook, idx: usize, at: u32, count: u32) {
    structural_edit(
        wb,
        idx,
        EditShift {
            rows: false,
            at,
            delta: count as i64,
        },
    );
}

/// Delete `count` columns starting at 0-based column `at` on sheet `idx`.
pub fn delete_cols(wb: &mut Workbook, idx: usize, at: u32, count: u32) {
    structural_edit(
        wb,
        idx,
        EditShift {
            rows: false,
            at,
            delta: -(count as i64),
        },
    );
}

/// Remove duplicate rows in `sheet` over the row range `r1..=r2` (keeping the
/// first occurrence), comparing all used columns. When `has_header`, the first
/// row is treated as a header and never removed. Rows shift up to fill the gaps;
/// the freed tail rows are cleared. Returns how many rows were removed.
pub fn dedupe_rows(wb: &mut Workbook, sheet: usize, r1: u32, r2: u32, has_header: bool) -> usize {
    let Some(s) = wb.sheets.get_mut(sheet) else {
        return 0;
    };
    let (_, cols) = s.used_size();
    if cols == 0 {
        return 0;
    }
    let max_c = cols - 1;
    let start = if has_header { r1 + 1 } else { r1 };
    if r2 < start {
        return 0;
    }
    let total = (r2 - start + 1) as usize;
    let mut seen = std::collections::HashSet::new();
    let mut uniques: Vec<Vec<Option<crate::sheet::Cell>>> = Vec::new();
    for r in start..=r2 {
        let row: Vec<Option<crate::sheet::Cell>> = (0..=max_c).map(|c| s.cell(r, c).cloned()).collect();
        // Signature over the cells' values (formatting doesn't count for dedup).
        let key: Vec<String> = row
            .iter()
            .map(|c| c.as_ref().map(|cl| format!("{:?}", cl.value)).unwrap_or_default())
            .collect();
        if seen.insert(key) {
            uniques.push(row);
        }
    }
    let removed = total - uniques.len();
    if removed == 0 {
        return 0;
    }
    let kept = uniques.len() as u32;
    for (i, row) in uniques.into_iter().enumerate() {
        let dest = start + i as u32;
        for (c, cell) in row.into_iter().enumerate() {
            match cell {
                Some(cl) => s.set_cell(dest, c as u32, cl),
                None => {
                    s.cells.remove(&(dest, c as u32));
                }
            }
        }
    }
    for r in (start + kept)..=r2 {
        for c in 0..=max_c {
            s.cells.remove(&(r, c));
        }
    }
    removed
}

/// Parse a multi-level sort spec: comma-separated `COL [asc|desc]` terms, where
/// COL is a column letter (A, B, AA…) and the direction defaults to ascending.
/// e.g. "B asc, C desc" → `[(1, true), (2, false)]`. Returns `None` on any
/// malformed term (unknown direction word, trailing junk after the letters, or
/// an empty spec).
pub fn parse_sort_spec(s: &str) -> Option<Vec<(u32, bool)>> {
    let mut keys = Vec::new();
    for tok in s.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let mut parts = tok.split_whitespace();
        let col_s = parts.next()?;
        let (col, used) = crate::sheet::parse_col(col_s)?;
        if used != col_s.len() {
            return None;
        }
        let asc = match parts.next() {
            None => true,
            Some(d) if d.eq_ignore_ascii_case("asc") || d.eq_ignore_ascii_case("a") => true,
            Some(d) if d.eq_ignore_ascii_case("desc") || d.eq_ignore_ascii_case("d") => false,
            Some(_) => return None,
        };
        if parts.next().is_some() {
            return None; // more than two words in a term
        }
        keys.push((col, asc));
    }
    (!keys.is_empty()).then_some(keys)
}

/// Reorder rows `r1..=r2` of `sheet` by one or more sort keys, applied in order
/// (the first key is primary, later keys break ties). Each key is `(column,
/// ascending)`. Rows move as whole units — every column and its styles travel
/// together — so this preserves row integrity but does not re-base formula
/// references (it targets value tables, the common case). Blanks sort last in
/// every key column regardless of direction; cross-type order is number < text
/// < bool. The sort is stable, so rows equal on all keys keep their order.
/// Returns the number of rows reordered.
pub fn sort_rows(wb: &mut Workbook, sheet: usize, r1: u32, r2: u32, keys: &[(u32, bool)]) -> usize {
    use std::cmp::Ordering;
    let Some(s) = wb.sheets.get_mut(sheet) else {
        return 0;
    };
    if keys.is_empty() || r2 <= r1 {
        return 0;
    }
    let (_, cols) = s.used_size();
    if cols == 0 {
        return 0;
    }
    let max_c = cols - 1;
    let mut rows: Vec<Vec<Option<Cell>>> = (r1..=r2)
        .map(|r| (0..=max_c).map(|c| s.cell(r, c).cloned()).collect())
        .collect();
    let is_blank = |cell: &Option<Cell>| cell.as_ref().map_or(true, |c| c.is_blank());
    // Cross-type rank so values of different kinds order deterministically.
    let rank = |cell: &Option<Cell>| match cell.as_ref().map(|c| &c.value) {
        Some(CellValue::Number(_)) => 0,
        Some(CellValue::Text(_)) => 1,
        Some(CellValue::Bool(_)) => 2,
        _ => 3,
    };
    let value_cmp = |ka: &Option<Cell>, kb: &Option<Cell>| match (ka.as_ref().map(|c| &c.value), kb.as_ref().map(|c| &c.value)) {
        (Some(CellValue::Number(x)), Some(CellValue::Number(y))) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Some(CellValue::Text(x)), Some(CellValue::Text(y))) => x.to_lowercase().cmp(&y.to_lowercase()),
        (Some(CellValue::Bool(x)), Some(CellValue::Bool(y))) => x.cmp(y),
        _ => rank(ka).cmp(&rank(kb)),
    };
    rows.sort_by(|a, b| {
        for &(col, asc) in keys {
            let col = col as usize;
            if col > max_c as usize {
                continue;
            }
            let (ka, kb) = (&a[col], &b[col]);
            let (ba, bb) = (is_blank(ka), is_blank(kb));
            // Blanks always sort last, independent of the direction.
            if ba || bb {
                match ba.cmp(&bb) {
                    Ordering::Equal => continue,
                    o => return o,
                }
            }
            let ord = value_cmp(ka, kb);
            let ord = if asc { ord } else { ord.reverse() };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    for (i, row) in rows.into_iter().enumerate() {
        let r = r1 + i as u32;
        for (c, cell) in row.into_iter().enumerate() {
            match cell {
                Some(cl) => s.set_cell(r, c as u32, cl),
                None => {
                    s.cells.remove(&(r, c as u32));
                }
            }
        }
    }
    (r2 - r1 + 1) as usize
}

/// Auto-fill from a source range by dragging its fill handle. `to` is the far
/// corner the handle reached; the dominant axis (down or right) decides the
/// direction. A source line of ≥2 numbers extends as a linear series (step =
/// difference of the last two); otherwise the source cells are copied/cycled.
/// Formulas are copied verbatim (not yet re-based). Returns the count of filled
/// cells.
pub fn autofill(wb: &mut Workbook, sheet: usize, src: (u32, u32, u32, u32), to: (u32, u32)) -> usize {
    let (sr0, sc0, sr1, sc1) = src;
    let (tr, tc) = to;
    let dr = tr.saturating_sub(sr1);
    let dc = tc.saturating_sub(sc1);
    if dr == 0 && dc == 0 {
        return 0;
    }
    let Some(s) = wb.sheets.get_mut(sheet) else {
        return 0;
    };
    let mut filled = 0;
    if dr >= dc {
        // Fill DOWN: extend each column into rows sr1+1..=tr.
        let count = (tr - sr1) as usize;
        for c in sc0..=sc1 {
            let srcvals: Vec<Option<Cell>> = (sr0..=sr1).map(|r| s.cell(r, c).cloned()).collect();
            for (k, cell) in extend_series(&srcvals, count).into_iter().enumerate() {
                s.set_cell(sr1 + 1 + k as u32, c, cell);
                filled += 1;
            }
        }
    } else {
        // Fill RIGHT: extend each row into columns sc1+1..=tc.
        let count = (tc - sc1) as usize;
        for r in sr0..=sr1 {
            let srcvals: Vec<Option<Cell>> = (sc0..=sc1).map(|c| s.cell(r, c).cloned()).collect();
            for (k, cell) in extend_series(&srcvals, count).into_iter().enumerate() {
                s.set_cell(r, sc1 + 1 + k as u32, cell);
                filled += 1;
            }
        }
    }
    filled
}

/// Produce `count` cells continuing a source line: a numeric series when every
/// source cell is a number (≥2 of them), else the source pattern copied/cycled.
fn extend_series(src: &[Option<Cell>], count: usize) -> Vec<Cell> {
    let nums: Option<Vec<f64>> = src
        .iter()
        .map(|c| match c.as_ref().map(|x| &x.value) {
            Some(CellValue::Number(n)) => Some(*n),
            _ => None,
        })
        .collect();
    if let Some(nums) = nums {
        if nums.len() >= 2 {
            let step = nums[nums.len() - 1] - nums[nums.len() - 2];
            let last = nums[nums.len() - 1];
            let style = src.last().and_then(|c| c.as_ref()).map(|c| c.style).unwrap_or(0);
            return (0..count)
                .map(|k| {
                    let mut cell = Cell::number(last + step * (k as f64 + 1.0));
                    cell.style = style; // carry the source formatting
                    cell
                })
                .collect();
        }
    }
    // Copy / cycle the source cells (single value → repeat it).
    (0..count).map(|k| src[k % src.len()].clone().unwrap_or_default()).collect()
}

/// Insert subtotal rows into a region already grouped by `group_col`: at each
/// change in that column's value, add a `SUBTOTAL(9, …)` row over the numeric
/// `sum_cols` (inferred from the data when the slice is empty), then a grand
/// total over the whole region. Detail rows are given outline level 1 so they
/// collapse under their subtotal. The region must be sorted by `group_col`
/// first (Excel requires this too). Grand totals use `SUBTOTAL` precisely
/// because it skips the nested per-group subtotals. Returns the number of rows
/// added (groups + 1), or 0 when there's nothing to total.
pub fn subtotal(wb: &mut Workbook, sheet: usize, r1: u32, r2: u32, group_col: u32, sum_cols: &[u32], has_header: bool) -> usize {
    use crate::sheet::cell_name;
    let start = if has_header { r1 + 1 } else { r1 };
    if r2 < start {
        return 0;
    }
    // Snapshot the detail rows as full-width cell vectors.
    let (max_c, detail): (u32, Vec<Vec<Option<Cell>>>) = {
        let Some(s) = wb.sheets.get(sheet) else {
            return 0;
        };
        let (_, cols) = s.used_size();
        if cols == 0 {
            return 0;
        }
        let max_c = cols - 1;
        let detail = (start..=r2)
            .map(|r| (0..=max_c).map(|c| s.cell(r, c).cloned()).collect())
            .collect();
        (max_c, detail)
    };
    let gc = group_col as usize;
    let key_of = |row: &[Option<Cell>]| row.get(gc).and_then(|c| c.as_ref()).map(|c| format!("{:?}", c.value)).unwrap_or_default();
    let label_of = |row: &[Option<Cell>]| match row.get(gc).and_then(|c| c.as_ref()).map(|c| &c.value) {
        Some(CellValue::Text(t)) => t.clone(),
        Some(CellValue::Number(n)) => n.to_string(),
        Some(CellValue::Bool(b)) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        _ => String::new(),
    };
    // Runs of consecutive equal group values: (label, start_idx, end_idx).
    let mut runs: Vec<(String, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < detail.len() {
        let k = key_of(&detail[i]);
        let mut j = i;
        while j + 1 < detail.len() && key_of(&detail[j + 1]) == k {
            j += 1;
        }
        runs.push((label_of(&detail[i]), i, j));
        i = j + 1;
    }
    if runs.is_empty() {
        return 0;
    }
    // Which columns to total: the caller's list, or every numeric column but the
    // group column.
    let sum_set: Vec<u32> = if !sum_cols.is_empty() {
        sum_cols.to_vec()
    } else {
        (0..=max_c)
            .filter(|&c| c != group_col && detail.iter().any(|row| matches!(row.get(c as usize).and_then(|x| x.as_ref()).map(|x| &x.value), Some(CellValue::Number(_)))))
            .collect()
    };
    let added = runs.len() + 1;
    // Push the tail down to make room (adjusting formulas that reference it).
    insert_rows(wb, sheet, r2 + 1, added as u32);
    let Some(s) = wb.sheets.get_mut(sheet) else {
        return 0;
    };
    fn write_row(s: &mut Sheet, r: u32, row: &[Option<Cell>]) {
        for (c, cell) in row.iter().enumerate() {
            match cell {
                Some(cl) => s.set_cell(r, c as u32, cl.clone()),
                None => {
                    s.cells.remove(&(r, c as u32));
                }
            }
        }
    }
    let subtotal_row = |s: &mut Sheet, out: u32, label: &str, first: u32, last: u32| {
        for c in 0..=max_c {
            s.cells.remove(&(out, c));
        }
        s.set_row_outline(out, 0);
        s.set_cell(out, group_col, Cell::text(label));
        for &c in &sum_set {
            let rng = format!("{}:{}", cell_name(first, c), cell_name(last, c));
            s.set_cell(out, c, Cell::formula(&format!("SUBTOTAL(9,{rng})")));
        }
    };
    let mut out = start;
    for (label, si, ei) in &runs {
        let first = out;
        for idx in *si..=*ei {
            write_row(s, out, &detail[idx]);
            s.set_row_outline(out, 1);
            out += 1;
        }
        let text = if label.is_empty() { "Total".to_string() } else { format!("{label} Total") };
        subtotal_row(s, out, &text, first, out - 1);
        out += 1;
    }
    // Grand total over the whole region; SUBTOTAL skips the nested subtotals.
    subtotal_row(s, out, "Grand Total", start, out - 1);
    added
}

/// Split each text cell in column `col` over rows `r1..=r2` at `delim`, writing
/// the parts into `col`, `col+1`, … (overwriting adjacent cells, as Excel does).
/// Numeric-looking parts become numbers. Rows without the delimiter are left
/// alone. Returns how many rows were split.
pub fn text_to_columns(wb: &mut Workbook, sheet: usize, col: u32, r1: u32, r2: u32, delim: char) -> usize {
    let Some(s) = wb.sheets.get_mut(sheet) else {
        return 0;
    };
    let splits: Vec<(u32, Vec<String>)> = (r1..=r2)
        .filter_map(|r| {
            let cell = s.cell(r, col)?;
            if let crate::sheet::CellValue::Text(t) = &cell.value {
                let parts: Vec<String> = t.split(delim).map(|p| p.trim().to_string()).collect();
                if parts.len() > 1 {
                    return Some((r, parts));
                }
            }
            None
        })
        .collect();
    let n = splits.len();
    for (r, parts) in splits {
        for (i, part) in parts.into_iter().enumerate() {
            let c = col + i as u32;
            let cell = match part.parse::<f64>() {
                Ok(num) if !part.is_empty() => crate::sheet::Cell::number(num),
                _ => crate::sheet::Cell::text(&part),
            };
            s.set_cell(r, c, cell);
        }
    }
    n
}

/// Rename a sheet and rewrite every reference to it (formulas on all sheets
/// plus defined-name definitions), as Excel does.
pub fn rename_sheet(wb: &mut Workbook, idx: usize, new_name: &str) {
    let Some(old) = wb.sheets.get(idx).map(|s| s.name.clone()) else {
        return;
    };
    if old.eq_ignore_ascii_case(new_name) {
        wb.sheets[idx].name = new_name.to_string();
        return;
    }
    for sheet in &mut wb.sheets {
        for cell in sheet.cells.values_mut() {
            if cell.f_attrs.is_some() {
                continue; // preserved verbatim
            }
            if let Some(src) = &cell.formula {
                if let Some(updated) = rename_sheet_in_formula(src, &old, new_name) {
                    cell.formula = Some(updated);
                }
            }
        }
    }
    for dn in &mut wb.defined_names {
        if let Some(updated) = rename_sheet_in_formula(&dn.formula, &old, new_name) {
            dn.formula = updated;
        }
    }
    // Pivot sources name their sheet directly (not as a formula reference) —
    // `PivotSource::Range { sheet, .. }` must follow the rename too, or the
    // pivot silently orphans: `refresh_pivots` looks the name up via
    // `wb.sheet_index` and just skips when it no longer resolves, so nothing
    // ever surfaces the break.
    for piv in &mut wb.pivots {
        if let crate::pivot::PivotSource::Range { sheet, .. } = &mut piv.source {
            if sheet.eq_ignore_ascii_case(&old) {
                *sheet = new_name.to_string();
            }
        }
    }
    wb.sheets[idx].name = new_name.to_string();
}

/// The shared core: move the grid on the target sheet, then rewrite every
/// formula and defined name in the workbook.
fn structural_edit(wb: &mut Workbook, idx: usize, shift: EditShift) {
    let Some(target_name) = wb.sheets.get(idx).map(|s| s.name.clone()) else {
        return;
    };
    if shift.delta == 0 {
        return;
    }

    shift_grid(&mut wb.sheets[idx], &shift);

    for (s, sheet) in wb.sheets.iter_mut().enumerate() {
        let home_is_target = s == idx;
        for cell in sheet.cells.values_mut() {
            if cell.f_attrs.is_some() {
                continue; // preserved verbatim; stale is acceptable, corrupt is not
            }
            if let Some(src) = &cell.formula {
                if let Some(updated) =
                    adjust_formula_for_edit(src, home_is_target, &target_name, &shift)
                {
                    cell.formula = Some(updated);
                }
            }
        }
    }
    for dn in &mut wb.defined_names {
        // Defined names have no home sheet; only sheet-qualified refs shift.
        if let Some(updated) = adjust_formula_for_edit(&dn.formula, false, &target_name, &shift) {
            dn.formula = updated;
        }
    }

    // Table regions follow the grid. Row edits stretch/shift freely; column
    // edits move a table only when they fall entirely to its left — resizing
    // a table's column set would desync it from its tableColumns definition
    // (a later refinement), so intersecting column edits leave it in place.
    for t in &mut wb.tables {
        if t.sheet != idx {
            continue;
        }
        let (r1, c1, r2, c2) = t.range;
        if shift.rows {
            if let Some((lo, hi)) = span(r1, r2, &shift) {
                // Keep at least the header row alive.
                if hi >= lo {
                    t.range = (lo, c1, hi, c2);
                }
            }
        } else if shift.at <= c1 {
            let edge = if shift.delta < 0 {
                shift.at as i64 - shift.delta // first surviving column
            } else {
                shift.at as i64
            };
            if edge <= c1 as i64 {
                let d = shift.delta;
                let nc1 = (c1 as i64 + d).max(0) as u32;
                let nc2 = (c2 as i64 + d).max(0) as u32;
                if nc2 < MAX_COLS {
                    t.range = (r1, nc1, r2, nc2);
                }
            }
        }
    }
}

/// One coordinate through the shift; None = deleted.
fn point(v: u32, shift: &EditShift) -> Option<u32> {
    let v = v as i64;
    let at = shift.at as i64;
    if shift.delta >= 0 {
        let n = if v >= at { v + shift.delta } else { v };
        (n < if shift.rows { MAX_ROWS } else { MAX_COLS } as i64).then_some(n as u32)
    } else {
        let n = -shift.delta;
        if v < at {
            Some(v as u32)
        } else if v < at + n {
            None
        } else {
            Some((v - n) as u32)
        }
    }
}

/// A span through the shift (deletes clamp); None = span fully deleted.
fn span(a: u32, b: u32, shift: &EditShift) -> Option<(u32, u32)> {
    let at = shift.at;
    let lo = point(a.min(b), shift).unwrap_or(at);
    let hi = match point(a.max(b), shift) {
        Some(h) => h,
        None => at.checked_sub(1)?,
    };
    (lo <= hi).then_some((lo, hi))
}

fn shift_grid(sheet: &mut Sheet, shift: &EditShift) {
    // Cells.
    let cells = std::mem::take(&mut sheet.cells);
    sheet.cells = cells
        .into_iter()
        .filter_map(|((r, c), cell)| {
            let key = if shift.rows {
                point(r, shift).map(|nr| (nr, c))
            } else {
                point(c, shift).map(|nc| (r, nc))
            };
            key.map(|k| (k, cell))
        })
        .collect();

    // Row attributes move with their rows (only for row edits).
    if shift.rows {
        let attrs = std::mem::take(&mut sheet.row_attrs);
        sheet.row_attrs = attrs
            .into_iter()
            .filter_map(|(r, a)| point(r, shift).map(|nr| (nr, a)))
            .collect::<BTreeMap<_, _>>();
    } else {
        // Column definitions move with their columns (only for column edits).
        let defs = std::mem::take(&mut sheet.col_defs);
        sheet.col_defs = defs
            .into_iter()
            .filter_map(|mut d| {
                let (lo, hi) = span(d.min, d.max, shift)?;
                d.min = lo;
                d.max = hi;
                Some(d)
            })
            .collect();
    }

    // Merged regions stretch/clamp on the edited axis; fully-deleted ones go.
    let merges = std::mem::take(&mut sheet.merges);
    sheet.merges = merges
        .into_iter()
        .filter_map(|(r1, c1, r2, c2)| {
            if shift.rows {
                span(r1, r2, shift).map(|(a, b)| (a, c1, b, c2))
            } else {
                span(c1, c2, shift).map(|(a, b)| (r1, a, r2, b))
            }
        })
        // A 1×1 "merge" left over after clamping is meaningless.
        .filter(|&(r1, c1, r2, c2)| !(r1 == r2 && c1 == c2))
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::sheet::{Cell, CellValue, parse_cell_name};

    fn wb(cells: &[(&str, Cell)]) -> Workbook {
        let mut sheet = Sheet {
            name: "Sheet1".to_string(),
            ..Sheet::default()
        };
        for (name, cell) in cells {
            let (r, c) = parse_cell_name(name).unwrap();
            sheet.set_cell(r, c, cell.clone());
        }
        Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        }
    }

    #[test]
    fn sort_rows_multi_key_breaks_ties() {
        // Group asc, then Score desc within each group. Rows 2..=5 (0-based 1..=4).
        let mut w = wb(&[
            ("A1", Cell::text("Group")),
            ("B1", Cell::text("Score")),
            ("A2", Cell::text("B")),
            ("B2", Cell::number(10.0)),
            ("A3", Cell::text("A")),
            ("B3", Cell::number(5.0)),
            ("A4", Cell::text("B")),
            ("B4", Cell::number(20.0)),
            ("A5", Cell::text("A")),
            ("B5", Cell::number(8.0)),
        ]);
        let n = sort_rows(&mut w, 0, 1, 4, &[(0, true), (1, false)]);
        assert_eq!(n, 4);
        let s = &w.sheets[0];
        let col = |r: u32, c: u32| s.cell(r, c).map(|cl| cl.value.clone());
        // A/8, A/5, B/20, B/10
        assert_eq!(col(1, 0), Some(CellValue::Text("A".into())));
        assert_eq!(col(1, 1), Some(CellValue::Number(8.0)));
        assert_eq!(col(2, 0), Some(CellValue::Text("A".into())));
        assert_eq!(col(2, 1), Some(CellValue::Number(5.0)));
        assert_eq!(col(3, 0), Some(CellValue::Text("B".into())));
        assert_eq!(col(3, 1), Some(CellValue::Number(20.0)));
        assert_eq!(col(4, 1), Some(CellValue::Number(10.0)));
    }

    #[test]
    fn autofill_series_down_and_copy_right() {
        // A1=1, A2=2  → fill down to A5 should give the series 3,4,5.
        let mut w = wb(&[("A1", Cell::number(1.0)), ("A2", Cell::number(2.0))]);
        let n = autofill(&mut w, 0, (0, 0, 1, 0), (4, 0));
        assert_eq!(n, 3);
        let s = &w.sheets[0];
        let num = |r: u32| match s.cell(r, 0).map(|c| c.value.clone()) {
            Some(CellValue::Number(x)) => x,
            v => panic!("A{} not number: {v:?}", r + 1),
        };
        assert_eq!((num(2), num(3), num(4)), (3.0, 4.0, 5.0));

        // A single text cell copied to the right (B1..D1 = "x").
        let mut w2 = wb(&[("A1", Cell::text("x"))]);
        let n2 = autofill(&mut w2, 0, (0, 0, 0, 0), (0, 3));
        assert_eq!(n2, 3);
        let s2 = &w2.sheets[0];
        for c in 1..=3 {
            assert_eq!(s2.cell(0, c).map(|x| x.value.clone()), Some(CellValue::Text("x".into())));
        }
    }

    #[test]
    fn autofill_step_of_five_and_no_op() {
        // 0,5 → 10,15,20 (step 5).
        let mut w = wb(&[("A1", Cell::number(0.0)), ("A2", Cell::number(5.0))]);
        autofill(&mut w, 0, (0, 0, 1, 0), (4, 0));
        let s = &w.sheets[0];
        assert_eq!(s.cell(4, 0).map(|c| c.value.clone()), Some(CellValue::Number(20.0)));
        // Dragging back onto the source (no extension) fills nothing.
        assert_eq!(autofill(&mut w, 0, (0, 0, 1, 0), (1, 0)), 0);
    }

    #[test]
    fn subtotal_inserts_group_and_grand_totals() {
        // Region A1:B5 — header + two groups (A: 1,2 / B: 4), pre-sorted.
        let mut w = wb(&[
            ("A1", Cell::text("Grp")),
            ("B1", Cell::text("Amt")),
            ("A2", Cell::text("A")),
            ("B2", Cell::number(1.0)),
            ("A3", Cell::text("A")),
            ("B3", Cell::number(2.0)),
            ("A4", Cell::text("B")),
            ("B4", Cell::number(4.0)),
        ]);
        let added = subtotal(&mut w, 0, 0, 3, 0, &[], true);
        assert_eq!(added, 3); // 2 group subtotals + grand total
        let s = &w.sheets[0];
        let txt = |r: u32, c: u32| s.cell(r, c).map(|cl| cl.value.clone());
        // Layout: hdr, A/1, A/2, "A Total", B/4, "B Total", "Grand Total".
        assert_eq!(txt(3, 0), Some(CellValue::Text("A Total".into())));
        assert_eq!(txt(5, 0), Some(CellValue::Text("B Total".into())));
        assert_eq!(txt(6, 0), Some(CellValue::Text("Grand Total".into())));
        // Detail rows are grouped at outline level 1; totals stay at level 0.
        assert_eq!(s.row_outline(1), 1);
        assert_eq!(s.row_outline(2), 1);
        assert_eq!(s.row_outline(3), 0);
        assert_eq!(s.row_outline(4), 1);
        assert_eq!(s.row_outline(6), 0);
        // The subtotal formulas evaluate: A=3, B=4, grand=7 (SUBTOTAL skips nesting).
        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(value_at(&w, "B4"), CellValue::Number(3.0)); // A Total
        assert_eq!(value_at(&w, "B6"), CellValue::Number(4.0)); // B Total
        assert_eq!(value_at(&w, "B7"), CellValue::Number(7.0)); // Grand Total
    }

    #[test]
    fn sort_rows_puts_blanks_last_both_directions() {
        let mut w = wb(&[
            ("A1", Cell::number(3.0)),
            ("A3", Cell::number(1.0)), // A2 is blank
            ("A4", Cell::number(2.0)),
        ]);
        // Ascending: 1,2,3,blank
        sort_rows(&mut w, 0, 0, 3, &[(0, true)]);
        let s = &w.sheets[0];
        assert_eq!(s.cell(0, 0).map(|c| c.value.clone()), Some(CellValue::Number(1.0)));
        assert_eq!(s.cell(2, 0).map(|c| c.value.clone()), Some(CellValue::Number(3.0)));
        assert!(s.cell(3, 0).map_or(true, |c| c.is_blank()));
        // Descending: 3,2,1,blank (blank still last)
        sort_rows(&mut w, 0, 0, 3, &[(0, false)]);
        let s = &w.sheets[0];
        assert_eq!(s.cell(0, 0).map(|c| c.value.clone()), Some(CellValue::Number(3.0)));
        assert!(s.cell(3, 0).map_or(true, |c| c.is_blank()));
    }

    #[test]
    fn dedupe_rows_keeps_first_and_shifts_up() {
        // Header + rows: A, B, A(dup), C, B(dup) in cols A(name) & B(qty).
        let mut w = wb(&[
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("A2", Cell::text("A")),
            ("B2", Cell::number(1.0)),
            ("A3", Cell::text("B")),
            ("B3", Cell::number(2.0)),
            ("A4", Cell::text("A")),
            ("B4", Cell::number(1.0)), // dup of row 2
            ("A5", Cell::text("C")),
            ("B5", Cell::number(3.0)),
            ("A6", Cell::text("B")),
            ("B6", Cell::number(2.0)), // dup of row 3
        ]);
        let removed = dedupe_rows(&mut w, 0, 0, 5, true);
        assert_eq!(removed, 2);
        // Uniques A,B,C shifted to rows 2,3,4; rows 5,6 cleared.
        assert_eq!(value_at(&w, "A2"), CellValue::Text("A".into()));
        assert_eq!(value_at(&w, "A3"), CellValue::Text("B".into()));
        assert_eq!(value_at(&w, "A4"), CellValue::Text("C".into()));
        assert_eq!(value_at(&w, "A5"), CellValue::Empty);
        assert_eq!(value_at(&w, "A6"), CellValue::Empty);
    }

    #[test]
    fn text_to_columns_splits_and_types() {
        let mut w = wb(&[
            ("A1", Cell::text("Laptop,2,1199")),
            ("A2", Cell::text("Dock,1,179")),
            ("A3", Cell::text("NoDelimiter")),
        ]);
        let n = text_to_columns(&mut w, 0, 0, 0, 2, ',');
        assert_eq!(n, 2);
        assert_eq!(value_at(&w, "A1"), CellValue::Text("Laptop".into()));
        assert_eq!(value_at(&w, "B1"), CellValue::Number(2.0));
        assert_eq!(value_at(&w, "C1"), CellValue::Number(1199.0));
        assert_eq!(value_at(&w, "A2"), CellValue::Text("Dock".into()));
        assert_eq!(value_at(&w, "C2"), CellValue::Number(179.0));
        // The row without the delimiter is untouched.
        assert_eq!(value_at(&w, "A3"), CellValue::Text("NoDelimiter".into()));
    }

    fn formula_at(wb: &Workbook, name: &str) -> String {
        let (r, c) = parse_cell_name(name).unwrap();
        wb.sheets[0]
            .cell(r, c)
            .and_then(|cl| cl.formula.clone())
            .unwrap_or_default()
    }

    fn value_at(wb: &Workbook, name: &str) -> CellValue {
        let (r, c) = parse_cell_name(name).unwrap();
        wb.sheets[0]
            .cell(r, c)
            .map(|cl| cl.value.clone())
            .unwrap_or(CellValue::Empty)
    }

    #[test]
    fn insert_rows_shifts_cells_and_formulas() {
        let mut w = wb(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("B1", Cell::formula("SUM(A1:A3)")),
            ("B3", Cell::formula("A3*2")),
        ]);
        insert_rows(&mut w, 0, 1, 2); // two rows before row 2
        // Values moved.
        assert_eq!(value_at(&w, "A1"), CellValue::Number(1.0));
        assert_eq!(value_at(&w, "A2"), CellValue::Empty);
        assert_eq!(value_at(&w, "A4"), CellValue::Number(2.0));
        assert_eq!(value_at(&w, "A5"), CellValue::Number(3.0));
        // Formulas rewrote: the range stretched, the point ref followed.
        assert_eq!(formula_at(&w, "B1"), "SUM(A1:A5)");
        assert_eq!(formula_at(&w, "B5"), "A5*2");
        // And it still computes.
        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(value_at(&w, "B1"), CellValue::Number(6.0));
    }

    #[test]
    fn delete_rows_pins_refs_and_poisons_deleted() {
        let mut w = wb(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("A4", Cell::number(4.0)),
            ("B1", Cell::formula("SUM(A1:A4)")),
            ("B4", Cell::formula("A2+A4")),
        ]);
        delete_rows(&mut w, 0, 1, 1); // delete row 2 (the B4 formula moves up)
        assert_eq!(value_at(&w, "A2"), CellValue::Number(3.0));
        assert_eq!(value_at(&w, "A3"), CellValue::Number(4.0));
        // Range shrank; the ref into the deleted row is #REF!.
        assert_eq!(formula_at(&w, "B1"), "SUM(A1:A3)");
        assert_eq!(formula_at(&w, "B3"), "#REF!+A3");
        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(value_at(&w, "B1"), CellValue::Number(8.0));
        assert_eq!(value_at(&w, "B3"), CellValue::Error("#REF!".into()));
    }

    #[test]
    fn insert_cols_shifts_everything() {
        let mut w = wb(&[
            ("A1", Cell::number(5.0)),
            ("B1", Cell::number(6.0)),
            ("C1", Cell::formula("A1*B1")),
        ]);
        w.sheets[0].set_col_width(1, 20.0);
        insert_cols(&mut w, 0, 1, 1); // one column before B
        assert_eq!(value_at(&w, "C1"), CellValue::Number(6.0));
        assert_eq!(formula_at(&w, "D1"), "A1*C1");
        // The width definition moved with its column.
        assert_eq!(w.sheets[0].col_width(2), 20.0);
        assert_eq!(w.sheets[0].col_width(1), crate::sheet::DEFAULT_COL_WIDTH);
    }

    #[test]
    fn delete_cols_clamps_ranges_and_merges() {
        let mut w = wb(&[
            ("A1", Cell::number(1.0)),
            ("B1", Cell::number(2.0)),
            ("C1", Cell::number(3.0)),
            ("E1", Cell::formula("SUM(A1:C1)")),
        ]);
        w.sheets[0].merges.push((0, 0, 0, 2)); // A1:C1 merged
        delete_cols(&mut w, 0, 1, 1); // delete column B
        assert_eq!(formula_at(&w, "D1"), "SUM(A1:B1)");
        assert_eq!(w.sheets[0].merges, vec![(0, 0, 0, 1)]);
        // Whole-column refs shift too.
        w.sheets[0].set_cell(4, 5, Cell::formula("SUM(B:B)"));
        delete_cols(&mut w, 0, 0, 1); // delete column A
        let f = w.sheets[0].cell(4, 4).unwrap().formula.clone().unwrap();
        assert_eq!(f, "SUM(A:A)");
    }

    #[test]
    fn cross_sheet_refs_shift_only_for_target_sheet() {
        let mut data = Sheet {
            name: "Data".to_string(),
            ..Sheet::default()
        };
        data.set_cell(1, 0, Cell::number(7.0)); // Data!A2
        let mut calc = Sheet {
            name: "Calc".to_string(),
            ..Sheet::default()
        };
        calc.set_cell(0, 0, Cell::formula("Data!A2*2")); // Calc!A1
        calc.set_cell(1, 0, Cell::formula("A1+1")); // Calc!A2, local ref
        let mut w = Workbook {
            sheets: vec![data, calc],
            ..Workbook::default()
        };
        insert_rows(&mut w, 0, 0, 3); // rows on Data only
        let f0 = w.sheets[1].cell(0, 0).unwrap().formula.clone().unwrap();
        let f1 = w.sheets[1].cell(1, 0).unwrap().formula.clone().unwrap();
        assert_eq!(f0, "Data!A5*2"); // followed the shift on Data
        assert_eq!(f1, "A1+1"); // untouched: Calc didn't move
    }

    #[test]
    fn defined_names_shift_with_their_sheet() {
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        w.defined_names.push(crate::sheet::DefinedName {
            name: "Spot".to_string(),
            scope: None,
            formula: "Sheet1!$A$1".to_string(),
        });
        insert_rows(&mut w, 0, 0, 2);
        assert_eq!(w.defined_names[0].formula, "Sheet1!$A$3");
    }

    #[test]
    fn rename_sheet_rewrites_references() {
        let mut data = Sheet {
            name: "Data".to_string(),
            ..Sheet::default()
        };
        data.set_cell(0, 0, Cell::number(1.0));
        let mut calc = Sheet {
            name: "Calc".to_string(),
            ..Sheet::default()
        };
        calc.set_cell(0, 0, Cell::formula("Data!A1+SUM(Data!A1:A9)"));
        let mut w = Workbook {
            sheets: vec![data, calc],
            ..Workbook::default()
        };
        w.defined_names.push(crate::sheet::DefinedName {
            name: "D".to_string(),
            scope: None,
            formula: "Data!$A$1".to_string(),
        });
        rename_sheet(&mut w, 0, "Numbers Etc");
        assert_eq!(w.sheets[0].name, "Numbers Etc");
        let f = w.sheets[1].cell(0, 0).unwrap().formula.clone().unwrap();
        assert_eq!(f, "'Numbers Etc'!A1+SUM('Numbers Etc'!A1:A9)");
        assert_eq!(w.defined_names[0].formula, "'Numbers Etc'!$A$1");
        // Still evaluates.
        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(
            w.sheets[1].cell(0, 0).unwrap().value,
            CellValue::Number(2.0)
        );
    }

    #[test]
    fn input_text_of_is_parse_inputs_inverse() {
        assert_eq!(input_text_of(&Cell::formula("A1+1")), "=A1+1");
        assert_eq!(input_text_of(&Cell::number(30.0)), "30");
        assert_eq!(input_text_of(&Cell::text("hi")), "hi");
        assert_eq!(input_text_of(&Cell::default()), "");
        // Round-trips through parse_input for every non-formula kind.
        for text in ["42", "hello", "TRUE", "#DIV/0!"] {
            assert_eq!(input_text_of(&parse_input(text)), text);
        }
    }

    #[test]
    fn replace_all_in_sheet_matches_in_values_and_formulas_preserving_style() {
        let mut sheet = Sheet {
            name: "Sheet1".to_string(),
            ..Sheet::default()
        };
        sheet.set_cell(
            0,
            0,
            Cell {
                style: 7,
                ..Cell::text("foo bar")
            },
        );
        sheet.set_cell(1, 0, Cell::formula("foo+1")); // "foo" only inside the formula
        sheet.set_cell(2, 0, Cell::text("no match here"));
        let changes = replace_all_in_sheet(&sheet, "foo", "QUX");
        assert_eq!(changes.len(), 2);
        let at = |r: u32| changes.iter().find(|(cr, _, _)| *cr == r).unwrap();
        let (_, _, c0) = at(0);
        assert_eq!(c0.value, CellValue::Text("QUX bar".to_string()));
        assert_eq!(c0.style, 7); // style survives the replace
        let (_, _, c1) = at(1);
        assert_eq!(c1.formula.as_deref(), Some("QUX+1"));
    }

    #[test]
    fn replace_all_in_sheet_reports_no_changes_when_nothing_matches() {
        let mut sheet = Sheet {
            name: "Sheet1".to_string(),
            ..Sheet::default()
        };
        sheet.set_cell(0, 0, Cell::text("nothing to see"));
        assert!(replace_all_in_sheet(&sheet, "zzz", "y").is_empty());
    }
}
