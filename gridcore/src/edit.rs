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

mod clip;
pub use clip::{
    MAX_PASTE_CELLS, PASTE_SHAPE, move_refs, paste_tiles, tiled_block, translated_block,
};
mod subtotal;
pub use subtotal::{
    Area, SubtotalError, SubtotalFunc, SubtotalOptions, is_subtotal_row, numeric_columns,
    remove_subtotals, sheets_differ, subtotal, subtotal_columns, subtotal_region,
};

use crate::entry::EntryCtx;
use crate::formula::{
    EditShift, ExcelError, Expr, adjust_for_edit, adjust_formula_for_edit, parse,
    rename_sheet_in_expr, rename_sheet_in_formula, to_string, translate, translate_formula,
};
use crate::sheet::{
    Cell, CellValue, MAX_COLS, MAX_ROWS, Sheet, Styles, Workbook, cell_name, f_ref, is_array_f,
    own_array_ref, ref_starts_at, with_ref,
};

/// Read text as a bare value: formulas, plain numbers (incl. percent),
/// booleans, error constants, text. Deliberately narrower than typed entry
/// ([`crate::entry::entry_cell`]), which also recognises currency, dates and
/// the like and gives them a number format: this reading has no format to
/// give, so a date would show as its bare serial, and it keeps those shapes
/// as text. (Paste reads by the entry rules: [`crate::entry::paste_cell`].)
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
    if let Some(n) = entry_number(t) {
        return match n {
            Some(n) => Cell::number(n),
            None => Cell::text(text),
        };
    }
    if let Some(pct) = t.strip_suffix('%') {
        match entry_number(pct.trim()) {
            Some(Some(n)) => return Cell::number(n / 100.0),
            Some(None) => return Cell::text(text),
            None => {}
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

/// A typed number the way Excel keeps it: `None` when `t` is not a number at
/// all, `Some(None)` when it is one Excel cannot store (beyond
/// 9.99999999999999E+307, or non-zero below 2.2250738585072E-308) and keeps
/// as text, else the value with digits past the fifteenth significant one
/// zeroed.
fn entry_number(t: &str) -> Option<Option<f64>> {
    // `f64::from_str` also takes "inf" and "NaN"; a typed number has digits.
    if t.parse::<f64>().is_err() || !t.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: f64 = crate::formula::truncate_15(t).parse().ok()?;
    let nonzero_digits = t
        .split(['e', 'E'])
        .next()
        .is_some_and(|m| m.bytes().any(|b| (b'1'..=b'9').contains(&b)));
    let a = n.abs();
    if !n.is_finite() || a > 9.99999999999999e307 || (nonzero_digits && a < 2.2250738585072e-308) {
        return Some(None);
    }
    Some(Some(n))
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
/// the `wb.replace-all` control verb: every cell whose own *input text* (its
/// `=formula` source, or the value as it would be re-entered — without the
/// `'` a quote prefix adds, which is never matched) contains `find` gets
/// `find` replaced with `with`, then re-read as a typed entry into that cell
/// ([`crate::entry::replaced_entry`], [`crate::entry::reenter_cell`]): a
/// Text cell keeps text, a quote-prefixed cell stays text, a recognised shape
/// in a General cell takes its format (interned into `styles`). A percent
/// cell's number constant is not divided again (its input text is the plain
/// value); anything else in a percent cell reads as typed there. A result over the cell limit
/// leaves that cell as it was. Returns the `(row, col, new_cell)` changes for
/// one sheet; callers decide how to apply them (one sheet under one undo
/// group, or every sheet under one structural snapshot).
pub fn replace_all_in_sheet(
    sheet: &Sheet,
    styles: &mut Styles,
    ctx: &EntryCtx,
    find: &str,
    with: &str,
) -> Vec<(u32, u32, Cell)> {
    let mut out = Vec::new();
    for (&(r, c), cell) in &sheet.cells {
        // Match in the cell's own text, not the `'` re-entry adds; put that
        // back on the result.
        let text = input_text_of(cell);
        if !text.contains(find) {
            continue;
        }
        let xf = styles.xf(cell.style);
        let new_text = crate::entry::replaced_entry(cell, &xf, text.replace(find, with));
        if let Ok(new) = crate::entry::reenter_cell(cell, styles, ctx, &text, &new_text) {
            out.push((r, c, new));
        }
    }
    out
}

/// Excel's Fill Down / Fill Right (Ctrl+D / Ctrl+R) over the selection
/// `(r1, c1, r2, c2)`: a range copies its first row down (or first column
/// right); a selection one row high (for Fill Down) or one column wide (for
/// Fill Right) — a single cell included — pulls from the row above (or the
/// column to the left).
/// Relative references move with the copy and the source's style comes
/// along; its file metadata does not ([`copy_meta`]). Pure: returns the
/// `(row, col, cell)` changes.
pub fn fill_changes(
    sheet: &Sheet,
    (r1, c1, r2, c2): (u32, u32, u32, u32),
    down: bool,
) -> Vec<(u32, u32, Cell)> {
    let mut changes = Vec::new();
    let mut copy_from = |sr: u32, sc: u32, tr: u32, tc: u32| {
        let mut cell = sheet.cell(sr, sc).cloned().unwrap_or_default();
        copy_meta(&mut cell);
        if let Some(f) = &cell.formula {
            if let Some(t) = translate_formula(f, tr as i64 - sr as i64, tc as i64 - sc as i64) {
                cell.formula = Some(t);
            }
        }
        changes.push((tr, tc, cell));
    };
    if down && r1 == r2 {
        if r1 > 0 {
            for c in c1..=c2 {
                copy_from(r1 - 1, c, r1, c);
            }
        }
    } else if !down && c1 == c2 {
        if c1 > 0 {
            for r in r1..=r2 {
                copy_from(r, c1 - 1, r, c1);
            }
        }
    } else if down {
        for c in c1..=c2 {
            for r in r1 + 1..=r2 {
                copy_from(r1, c, r, c);
            }
        }
    } else {
        for r in r1..=r2 {
            for c in c1 + 1..=c2 {
                copy_from(r, c1, r, c);
            }
        }
    }
    changes
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
        let row: Vec<Option<crate::sheet::Cell>> =
            (0..=max_c).map(|c| s.cell(r, c).cloned()).collect();
        // Signature over the cells' values (formatting doesn't count for dedup).
        let key: Vec<String> = row
            .iter()
            .map(|c| {
                c.as_ref()
                    .map(|cl| format!("{:?}", cl.value))
                    .unwrap_or_default()
            })
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
///
/// `r2` is clamped to the last used row: `A1:A1048576` is the ordinary "the
/// whole column" idiom, and materialising a million rows × every used column
/// would exhaust memory long before it sorted anything. Rows past the used
/// region are empty, so they sort last either way.
///
/// Rows that cut a spilled array ([`sort_cuts_spill`]) are not sorted: as in
/// Excel, part of an array can't be moved. An array within one row moves
/// with it, its own `ref` too.
pub fn sort_rows(wb: &mut Workbook, sheet: usize, r1: u32, r2: u32, keys: &[(u32, bool)]) -> usize {
    use std::cmp::Ordering;
    if sort_cuts_spill(wb, sheet, r1, r2) {
        return 0;
    }
    let Some(s) = wb.sheets.get_mut(sheet) else {
        return 0;
    };
    let Some((r2, max_c)) = sort_span(s, r1, r2) else {
        return 0;
    };
    if keys.is_empty() {
        return 0;
    }
    // Each row with the row it came from.
    let mut rows: Vec<(u32, Vec<Option<Cell>>)> = (r1..=r2)
        .map(|r| (r, (0..=max_c).map(|c| s.cell(r, c).cloned()).collect()))
        .collect();
    let is_blank = |cell: &Option<Cell>| cell.as_ref().is_none_or(|c| c.is_blank());
    // Cross-type rank so values of different kinds order deterministically.
    let rank = |cell: &Option<Cell>| match cell.as_ref().map(|c| &c.value) {
        Some(CellValue::Number(_)) => 0,
        Some(CellValue::Text(_)) => 1,
        Some(CellValue::Bool(_)) => 2,
        _ => 3,
    };
    let value_cmp = |ka: &Option<Cell>, kb: &Option<Cell>| match (
        ka.as_ref().map(|c| &c.value),
        kb.as_ref().map(|c| &c.value),
    ) {
        (Some(CellValue::Number(x)), Some(CellValue::Number(y))) => {
            x.partial_cmp(y).unwrap_or(Ordering::Equal)
        }
        (Some(CellValue::Text(x)), Some(CellValue::Text(y))) => {
            x.to_lowercase().cmp(&y.to_lowercase())
        }
        (Some(CellValue::Bool(x)), Some(CellValue::Bool(y))) => x.cmp(y),
        _ => rank(ka).cmp(&rank(kb)),
    };
    rows.sort_by(|(_, a), (_, b)| {
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
    for (i, (from, row)) in rows.into_iter().enumerate() {
        let r = r1 + i as u32;
        for (c, cell) in row.into_iter().enumerate() {
            match cell {
                Some(mut cl) => {
                    move_own_array_ref(&mut cl, (from, c as u32), r);
                    s.set_cell(r, c as u32, cl)
                }
                None => {
                    s.cells.remove(&(r, c as u32));
                }
            }
        }
    }
    (r2 - r1 + 1) as usize
}

/// What hosts tell the user when [`sort_cuts_spill`] refuses a sort.
pub const SORT_CUTS_SPILL: &str = "Can't sort: the rows cut a spilled array";

/// Would sorting rows `r1..=r2` of `sheet` ([`sort_rows`], which moves every
/// column of a row) cut an array: one, live or frozen, whose block spans
/// two rows or more and meets those rows? Its block is its extent, or for a
/// legacy CSE array whose result no longer spills ([`array_rect`]) the
/// `ref` it saves with. Hosts ask before they sort, to say why nothing
/// moved.
pub fn sort_cuts_spill(wb: &Workbook, sheet: usize, r1: u32, r2: u32) -> bool {
    let Some(s) = wb.sheets.get(sheet) else {
        return false;
    };
    let Some((r2, _)) = sort_span(s, r1, r2) else {
        return false;
    };
    s.cells.iter().any(|(&(ar, ac), cell)| {
        array_rect(cell, (ar, ac))
            .is_some_and(|(h, _)| h > 1 && ar <= r2 && ar.saturating_add(h - 1) >= r1)
    })
}

/// The `(height, width)` of the array block anchored at `at`: its spill
/// extent, or else, for a legacy CSE array (no `cm`, not a dynamic array),
/// the `ref` it owns (one that starts at `at`). A CSE formula evaluated to
/// one value (`SUM` over its block) has no extent, but save keeps that
/// `ref`, and Excel fills the block from it.
fn array_rect(cell: &Cell, at: (u32, u32)) -> Option<(u32, u32)> {
    if cell.spill.is_some() {
        return cell.spill;
    }
    if cell.is_dynamic() {
        return None;
    }
    let fa = cell.f_attrs.as_deref().filter(|fa| is_array_f(fa))?;
    if !ref_starts_at(fa, &cell_name(at.0, at.1)) {
        return None;
    }
    let (r1, c1, r2, c2) = crate::sheet::array_block(cell)?;
    Some((r2 - r1 + 1, c2 - c1 + 1))
}

/// The rows [`sort_rows`] sorts of `r1..=r2` and the last column it moves:
/// `r2` clamped to the last used row, `None` when that leaves under two rows.
fn sort_span(s: &Sheet, r1: u32, r2: u32) -> Option<(u32, u32)> {
    let (used_rows, cols) = s.used_size();
    if cols == 0 || used_rows == 0 {
        return None;
    }
    let r2 = r2.min(used_rows - 1);
    (r2 > r1).then_some((r2, cols - 1))
}

/// An array anchor moved from `from` to row `to` takes its block along: its
/// `ref`, when the anchor owns it (starts there), is rewritten to its block
/// ([`array_rect`]) at the new row. Left behind, it would name the old rows:
/// the cached block of an anchor the engine can't evaluate would no longer
/// count as its own, and a CSE block would save as its anchor alone.
fn move_own_array_ref(cell: &mut Cell, (from, col): (u32, u32), to: u32) {
    let Some((h, w)) = array_rect(cell, (from, col)) else {
        return;
    };
    let Some(fa) = cell.f_attrs.as_deref().filter(|fa| is_array_f(fa)) else {
        return;
    };
    if from == to || !ref_starts_at(fa, &cell_name(from, col)) {
        return;
    }
    let mut block = cell_name(to, col);
    if (h, w) != (1, 1) {
        block = format!("{block}:{}", cell_name(to + h - 1, col + w - 1));
    }
    cell.f_attrs = Some(with_ref(fa, &block));
}

/// Auto-fill from a source range by dragging its fill handle. `to` is the far
/// corner the handle reached; the dominant axis (down or right) decides the
/// direction. A source line of ≥2 numbers extends as a linear series (step =
/// difference of the last two); otherwise the source cells are copied/cycled.
/// Copied formulas are re-based like Excel's: relative references shift by the
/// copy's row/column distance, absolute (`$`) ones stay put. Returns the count
/// of filled cells.
pub fn autofill(
    wb: &mut Workbook,
    sheet: usize,
    src: (u32, u32, u32, u32),
    to: (u32, u32),
) -> usize {
    let (sr0, sc0, sr1, sc1) = src;
    let (tr, tc) = to;
    // A denormalized source has no cells to read, and the pattern walk below
    // divides by their count.
    if sr0 > sr1 || sc0 > sc1 {
        return 0;
    }
    let dr = tr.saturating_sub(sr1);
    let dc = tc.saturating_sub(sc1);
    if dr == 0 && dc == 0 {
        return 0;
    }
    let Some(s) = wb.sheets.get_mut(sheet) else {
        return 0;
    };
    // A source cell spilled by an anchor that is filled with it is copied
    // blank (keeping its style): the anchor's copy refills it, where a copied
    // value would block that copy's spill.
    let mut spilled = std::collections::HashSet::new();
    for (&(r, c), cell) in s.cells.range((sr0, 0)..=(sr1, u32::MAX)) {
        let (Some((h, w)), true) = (cell.spill, (sc0..=sc1).contains(&c)) else {
            continue;
        };
        if cell.formula.is_some() {
            for rr in r..(r + h).min(sr1 + 1) {
                for cc in c..(c + w).min(sc1 + 1) {
                    spilled.insert((rr, cc));
                }
            }
            spilled.remove(&(r, c));
        }
    }
    let source = |s: &Sheet, r: u32, c: u32| {
        s.cell(r, c).map(|cell| {
            if cell.formula.is_none() && spilled.contains(&(r, c)) {
                Cell {
                    style: cell.style,
                    ..Cell::default()
                }
            } else {
                cell.clone()
            }
        })
    };
    let mut filled = 0;
    if dr >= dc {
        // Fill DOWN: extend each column into rows sr1+1..=tr.
        let count = (tr - sr1) as usize;
        for c in sc0..=sc1 {
            let srcvals: Vec<Option<Cell>> = (sr0..=sr1).map(|r| source(s, r, c)).collect();
            let len = srcvals.len();
            for (k, mut cell) in extend_series(&srcvals, count).into_iter().enumerate() {
                let dst = sr1 + 1 + k as u32;
                // A copied cell came from src[k % len]; shift its formula by the
                // distance it travelled.
                rebase(
                    &mut cell,
                    i64::from(dst) - i64::from(sr0 + (k % len) as u32),
                    0,
                );
                s.set_cell(dst, c, cell);
                filled += 1;
            }
        }
    } else {
        // Fill RIGHT: extend each row into columns sc1+1..=tc.
        let count = (tc - sc1) as usize;
        for r in sr0..=sr1 {
            let srcvals: Vec<Option<Cell>> = (sc0..=sc1).map(|c| source(s, r, c)).collect();
            let len = srcvals.len();
            for (k, mut cell) in extend_series(&srcvals, count).into_iter().enumerate() {
                let dst = sc1 + 1 + k as u32;
                rebase(
                    &mut cell,
                    0,
                    i64::from(dst) - i64::from(sc0 + (k % len) as u32),
                );
                s.set_cell(r, dst, cell);
                filled += 1;
            }
        }
    }
    filled
}

/// Shift a filled cell's formula by (`dr`, `dc`).
///
/// A copy never inherits the source's `<f>` attributes: `t="array" ref="A1:A3"`
/// or a shared group's `si` names cells this copy does not own, and writing the
/// same `ref`/`si` out from several cells is what makes Excel offer to repair
/// the file. Dropped, the copy is an ordinary formula — which is also what
/// makes it safe to shift.
///
/// Nor does it inherit the source's `<c>` metadata: `vm` describes the
/// source's value, and a `cm` names the source's dynamic-array entry. A copy
/// is typed there, as a paste or Fill Down is (#724, `Engine::set_cell`): a
/// modern formula, spilling once the engine evaluates it, whatever the source
/// was — a loaded legacy formula or CSE anchor included. What the engine
/// learned about a typed dynamic array carries over (`dynamic`); a copy of a
/// loaded one becomes one again when the engine evaluates its array result.
fn rebase(cell: &mut Cell, dr: i64, dc: i64) {
    let dynamic = cell.meta.as_ref().is_some_and(|m| m.dynamic);
    cell.meta = None;
    // The source's spill extent isn't the copy's: the engine would take the
    // cells under it for the copy's own when it evaluates it.
    cell.spill = None;
    if cell.f_attrs.take().is_some() && cell.formula.as_deref() == Some("") {
        // A shared-group follower whose master wouldn't parse carries no text of
        // its own; without the group marker there is no formula left to write.
        cell.formula = None;
    }
    if cell.formula.is_some() {
        cell.meta = Some(Box::new(crate::sheet::CellMeta {
            modern: true,
            dynamic,
            ..Default::default()
        }));
    }
    if (dr, dc) == (0, 0) {
        return;
    }
    if let Some(f) = &cell.formula {
        if let Some(shifted) = crate::formula::translate_formula(f, dr, dc) {
            cell.formula = Some(shifted);
        }
    }
}

/// The `<c>` metadata a Fill Down/Right copy of `cell` keeps: none of the
/// file's (`cm`, `vm`, `vm_body`, `ph` describe the source cell and its loaded
/// value), only what the engine learned about a formula typed here (`modern`,
/// `dynamic`).
fn copy_meta(cell: &mut Cell) {
    cell.meta = cell.meta.take().filter(|m| m.modern || m.dynamic).map(|m| {
        Box::new(crate::sheet::CellMeta {
            modern: m.modern,
            dynamic: m.dynamic,
            ..Default::default()
        })
    });
}

/// Produce `count` cells continuing a source line: a numeric series when every
/// source cell is a number (≥2 of them), else the source pattern copied/cycled.
fn extend_series(src: &[Option<Cell>], count: usize) -> Vec<Cell> {
    // Formulas carry a cached numeric result; extending them as a linear series
    // would silently replace the formulas with numbers, so copy them instead.
    if src
        .iter()
        .flatten()
        .any(|c| c.formula.is_some() || c.f_attrs.is_some())
    {
        return (0..count)
            .map(|k| src[k % src.len()].clone().unwrap_or_default())
            .collect();
    }
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
            let style = src
                .last()
                .and_then(|c| c.as_ref())
                .map(|c| c.style)
                .unwrap_or(0);
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
    (0..count)
        .map(|k| src[k % src.len()].clone().unwrap_or_default())
        .collect()
}

/// Excel's refusal when Text to Columns is given more than one column.
pub const TTC_ONE_COLUMN: &str = "Microsoft Excel can convert only one column at a time. \
The range can be many rows tall but no more than one column wide. Try again by selecting \
cells in one column only.";

/// Excel's question before Text to Columns overwrites cells that hold data.
pub const TTC_REPLACE: &str = "Do you want to replace the contents of the destination cells?";

/// What Text to Columns converts and where the result goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TtcSource {
    pub sheet: usize,
    pub col: u32,
    pub r1: u32,
    pub r2: u32,
    /// The Destination cell for the first row's first field.
    pub dest: (u32, u32),
}

impl TtcSource {
    /// The selection `(r1, c1, r2, c2)` on `sheet`, converted in place. More
    /// than one column is refused with [`TTC_ONE_COLUMN`].
    pub fn new(sheet: usize, (r1, c1, r2, c2): (u32, u32, u32, u32)) -> Result<Self, &'static str> {
        if c1 != c2 {
            return Err(TTC_ONE_COLUMN);
        }
        Ok(TtcSource {
            sheet,
            col: c1,
            r1: r1.min(r2),
            r2: r1.max(r2),
            dest: (r1.min(r2), c1),
        })
    }
}

/// Each non-empty source cell's row and its fields, and where they land:
/// (row, field index, destination column) for every field not skipped.
#[allow(clippy::type_complexity)]
fn ttc_fields(
    wb: &Workbook,
    src: &TtcSource,
    opts: &crate::textio::TextParse,
) -> Vec<(u32, Vec<(String, usize, u32)>)> {
    let Some(s) = wb.sheets.get(src.sheet) else {
        return Vec::new();
    };
    let used_rows = s.used_size().0;
    if used_rows == 0 || src.r1 >= used_rows {
        return Vec::new();
    }
    // `r2` is clamped to the last used row, so the `A1:A1048576` whole-column
    // idiom walks the sheet rather than a million empty rows (see `sort_rows`).
    let r2 = src.r2.min(used_rows - 1);
    let mut out = Vec::new();
    for r in src.r1..=r2 {
        let Some(cell) = s.cell(r, src.col) else {
            continue;
        };
        if cell.formula.is_none() && matches!(cell.value, CellValue::Empty) {
            continue;
        }
        // The text the wizard shows for the cell: numbers and dates as
        // displayed, formulas as their results.
        let text = crate::sheet::format_with(&wb.styles.xf(cell.style), &cell.value, wb.date1904);
        let dest_row = src.dest.0 + (r - src.r1);
        let mut placed = Vec::new();
        let mut c = src.dest.1;
        for (i, field) in crate::textio::split_value(&text, opts)
            .into_iter()
            .enumerate()
        {
            if opts.column(i) == crate::textio::ColFormat::Skip {
                continue;
            }
            placed.push((field, i, c));
            c += 1;
        }
        out.push((dest_row, placed));
    }
    out
}

/// Would Text to Columns overwrite a cell holding data? The source column's
/// own cells do not count: converting them in place is the point.
pub fn ttc_would_overwrite(
    wb: &Workbook,
    src: &TtcSource,
    opts: &crate::textio::TextParse,
) -> bool {
    let Some(s) = wb.sheets.get(src.sheet) else {
        return false;
    };
    let in_source = |r: u32, c: u32| c == src.col && (src.r1..=src.r2).contains(&r);
    ttc_fields(wb, src, opts).iter().any(|(r, fields)| {
        fields.iter().any(|&(_, _, c)| {
            !in_source(*r, c)
                && s.cell(*r, c)
                    .is_some_and(|cell| cell.formula.is_some() || cell.value != CellValue::Empty)
        })
    })
}

/// Text to Columns: split each non-empty cell of the source column under
/// `opts` (delimiters and qualifier, or fixed width) and write the fields
/// from the destination cell rightwards, each converted under its column's
/// format (General, Text, Date, or skipped) and the Advanced separators.
/// A cell that does not split still has its first field converted. Cells
/// are overwritten as Excel does once the user has agreed (see
/// [`ttc_would_overwrite`]). Returns how many rows were converted.
pub fn text_to_columns(
    wb: &mut Workbook,
    src: &TtcSource,
    opts: &crate::textio::TextParse,
    today: Option<f64>,
) -> usize {
    let rows = ttc_fields(wb, src, opts);
    let ctx = EntryCtx {
        date1904: wb.date1904,
        today,
        fixed_decimal: None,
    };
    let auto = crate::textio::AutoConvert::default();
    let Workbook { sheets, styles, .. } = wb;
    let Some(s) = sheets.get_mut(src.sheet) else {
        return 0;
    };
    for (r, fields) in &rows {
        for (field, i, c) in fields {
            if let Some(conv) =
                crate::textio::convert_field(field, opts.column(*i), opts, &auto, &ctx)
            {
                crate::textio::put(s, styles, *r, *c, conv);
            }
        }
    }
    rows.len()
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
            let Some(src) = &cell.formula else {
                continue;
            };
            let updated = match cell.f_attrs.as_deref() {
                Some(a) if !is_array_f(a) => continue, // preserved verbatim
                // An array formula is ours, but its loaded text is only
                // reprinted when the rename really touches it.
                Some(_) => rewrite_if_changed(src, |e| rename_sheet_in_expr(e, &old, new_name)),
                None => rename_sheet_in_formula(src, &old, new_name),
            };
            if let Some(updated) = updated {
                cell.formula = Some(updated);
            }
        }
    }
    for dn in &mut wb.defined_names {
        if let Some(updated) = crate::formula::rewrite_defined_name(
            &dn.formula,
            |e| crate::formula::rename_sheet_in_expr(e, &old, new_name),
            Some((&old, new_name)),
        ) {
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
    // Conditional formatting and data validation formulas can name a sheet
    // too (a list on another sheet). Only one the rename touches is
    // reprinted, so the loaded spelling stays otherwise.
    for sheet in &mut wb.sheets {
        for_each_rule_formula(sheet, |src| {
            if let Some(updated) =
                rewrite_if_changed(src, |e| rename_sheet_in_expr(e, &old, new_name))
            {
                *src = updated;
            }
        });
    }
    // A chart's refs name their sheet the same way, and a save writes them back
    // out as `<c:f>` — left behind, they'd point at a sheet that no longer
    // exists and Excel would drop the chart's data.
    for sheet in &mut wb.sheets {
        for dw in &mut sheet.drawings {
            if let crate::sheet::DrawingKind::Chart(cd) = &mut dw.kind {
                rename_sheet_in_chart(cd, &old, new_name);
            }
        }
    }
    wb.sheets[idx].name = new_name.to_string();
}

/// Before the sheets named in `removed` leave the workbook, turn every cell
/// formula's reference to one of them into `#REF!`
/// ([`crate::formula::remove_sheet_refs_in_expr`]). The formulas
/// [`rename_sheet`] rewrites are the ones covered (shared groups were
/// expanded at load); a formula that names none of them keeps its text
/// exactly.
///
/// A rewritten cell's cached value is its new formula evaluated
/// ([`crate::engine::eval_formula_at`]), so `IFERROR(#REF!,0)` caches 0
/// rather than `#REF!`. An array formula's whole block (its `spill`) takes
/// the anchor's value, which is right once the removed range has collapsed
/// to the scalar `#REF!`. Only the rewritten cells are evaluated: a cell that
/// reads one keeps its cached value, and a volatile formula keeps its own,
/// until the file is next calculated.
pub fn remove_sheet_refs(wb: &mut Workbook, removed: &[String]) {
    if removed.is_empty() {
        return;
    }
    let mut rewritten = Vec::new();
    for (s, sheet) in wb.sheets.iter_mut().enumerate() {
        for (&(r, c), cell) in sheet.cells.iter_mut() {
            let Some(src) = &cell.formula else {
                continue;
            };
            if cell.f_attrs.as_deref().is_some_and(|a| !is_array_f(a)) {
                continue; // preserved verbatim
            }
            let rewrite = |e: &Expr| crate::formula::remove_sheet_refs_in_expr(e, removed);
            if let Some(updated) = rewrite_if_changed(src, rewrite) {
                cell.formula = Some(updated);
                rewritten.push((s, r, c));
            }
        }
    }
    // Evaluated once every rewrite is in, against the rewritten workbook.
    let values: Vec<CellValue> = rewritten
        .iter()
        .map(|&(s, r, c)| {
            let src = wb.sheets[s].cells[&(r, c)].formula.as_deref().unwrap_or("");
            crate::engine::value_to_cell(crate::engine::eval_formula_at(wb, s, r, c, src))
        })
        .collect();
    for ((s, r, c), value) in rewritten.into_iter().zip(values) {
        let sheet = &mut wb.sheets[s];
        let Some(cell) = sheet.cells.get_mut(&(r, c)) else {
            continue;
        };
        let block = cell.spill.filter(|_| cell.is_array_formula());
        cell.value = value.clone();
        let Some((rows, cols)) = block else {
            continue;
        };
        for dr in 0..rows {
            for dc in 0..cols {
                if (dr, dc) == (0, 0) {
                    continue;
                }
                if let Some(follower) = sheet.cells.get_mut(&(r + dr, c + dc)) {
                    follower.value = value.clone();
                }
            }
        }
    }
}

/// Point every ref a chart holds at `new_name` where it named `old`. Public so
/// the UI can do the same for charts it authored, which live outside the
/// workbook until they are saved.
pub fn rename_sheet_in_chart(cd: &mut crate::sheet::ChartData, old: &str, new_name: &str) {
    fn retarget(src: &mut crate::sheet::ChartSource, old: &str, new_name: &str) -> bool {
        let hit = src.sheet.eq_ignore_ascii_case(old);
        if hit {
            src.sheet = new_name.to_string();
        }
        hit
    }
    let mut changed = false;
    for s in cd.source.iter_mut().chain(cd.categories_ref.iter_mut()) {
        changed |= retarget(s, old, new_name);
    }
    for ser in &mut cd.series {
        if let Some(s) = ser.values_ref.as_mut() {
            changed |= retarget(s, old, new_name);
        }
        // A scatter's or bubble's points are the only numbers such a series
        // has, and the panel's `rebuild_source` folds them FIRST — leaving them
        // behind would seed the box with the old name and then skip the
        // correctly-renamed slots after it for the sheet mismatch, so DATA
        // RANGE would name a sheet the workbook no longer has.
        for s in &mut ser.point_refs {
            changed |= retarget(s, old, new_name);
        }
        // The name ref is kept verbatim as its `<c:f>` text, so it has to go
        // back through the same spelling rules (quoting included). A single cell
        // stays a single cell rather than becoming `$B$1:$B$1`.
        if let Some(mut p) = ser
            .name_ref
            .as_deref()
            .and_then(crate::sheet::ChartSource::parse_f_ref)
        {
            if retarget(&mut p, old, new_name) {
                let (r1, c1, r2, c2) = p.range;
                ser.name_ref = Some(if (r1, c1) == (r2, c2) {
                    p.header_ref(c1)
                } else {
                    p.to_ref()
                });
                changed = true;
            }
        }
    }
    // Only a chart whose refs actually moved is regenerated on save; the rest
    // round-trip verbatim, formatting and all.
    //
    // `edited` is a REQUEST to regenerate, not a promise: `save_xlsx` also asks
    // `chart_is_writable`, and a scatter, bubble, stacked or combo chart fails
    // it, so its part is copied byte for byte and the `<c:f>` on disk keeps the
    // old sheet name until something makes the chart writable. That is the same
    // trade-off `chart_kind_is_writable`'s own comment records for every other
    // slot on such a chart — a stale ref beats a destroyed chart — and it is
    // why re-basing them here is still worth doing: the panel, `rebuild_source`
    // and the next re-derivation all read the model, not the part.
    cd.edited |= changed;
}

/// Move every ref a chart holds on `target` through a row/column insert or
/// delete. Public alongside [`rename_sheet_in_chart`] and for the same reason:
/// a chart the UI authored lives outside the workbook until it is saved, and has
/// to be shifted by the same rules.
///
/// A range whose rows (or columns) are wholly deleted loses its ref rather than
/// keeping a dangling one — the cached values still draw the card, and a chart
/// that plots nothing beats one plotting a stranger's numbers.
pub fn shift_chart_refs(
    cd: &mut crate::sheet::ChartData,
    target: &str,
    home: bool,
    shift: &EditShift,
) -> bool {
    // A ref with no sheet name means the chart's OWN sheet — which is the edited
    // one only when the drawing itself lives there. `home` says so: a chart
    // sitting on "Report" whose refs read `$B$2:$B$10` plots Report's cells, and
    // deleting rows on "Data" must leave it alone.
    let mine = |s: &crate::sheet::ChartSource| -> bool {
        (home && s.sheet.is_empty()) || s.sheet.eq_ignore_ascii_case(target)
    };
    /// `Some(src)` shifted in place, `None` = the ref's cells are all gone.
    fn moved(
        src: &crate::sheet::ChartSource,
        shift: &EditShift,
    ) -> Option<crate::sheet::ChartSource> {
        let (r1, c1, r2, c2) = src.range;
        let (r1, c1, r2, c2) = if shift.rows {
            let (a, b) = span(r1, r2, shift)?;
            (a, c1, b, c2)
        } else {
            let (a, b) = span(c1, c2, shift)?;
            (r1, a, r2, b)
        };
        let mut out = src.clone();
        out.range = (r1, c1, r2, c2);
        // The label column rides along with the box it names.
        if !shift.rows {
            out.cat_col = point(src.cat_col, shift).unwrap_or(c1);
        }
        Some(out)
    }
    // Shift one slot; `true` if it came out different (gone included).
    let shift_slot = |slot: &mut Option<crate::sheet::ChartSource>| -> bool {
        let Some(s) = slot.as_ref().filter(|s| mine(s)) else {
            return false;
        };
        let next = moved(s, shift);
        let hit = next.as_ref() != Some(s);
        *slot = next;
        hit
    };
    // Which sheet a ref-less series reads: `chart_space_xml` derives its cells
    // from the chart's box, so that box decides whether its column is an index
    // into the edited grid. (Shifting leaves the sheet name alone, so reading it
    // after `shift_slot` is the same answer.)
    let source_mine = cd.source.as_ref().is_some_and(mine);
    let mut changed = shift_slot(&mut cd.source);
    changed |= shift_slot(&mut cd.categories_ref);
    for ser in &mut cd.series {
        let had_values = ser.values_ref.is_some();
        changed |= shift_slot(&mut ser.values_ref);
        if had_values && ser.values_ref.is_none() {
            // `chart_space_xml` derives a ref-less series' cells from the
            // chart's box and this column, so leaving `col` behind would put
            // back exactly the dangling ref the drop above is for.
            ser.col = None;
        }
        // A scatter's/bubble's points follow the grid like any other ref, and
        // are dropped rather than left dangling when their cells are wholly
        // deleted — the same rule `shift_slot` applies to `values_ref`, and for
        // the same reason: these ARE the series' numbers, and `rebuild_source`
        // folds them first, so a stale one drags the panel's box back onto the
        // pre-edit rectangle.
        ser.point_refs.retain_mut(|s| {
            if !mine(s) {
                return true;
            }
            match moved(s, shift) {
                Some(next) => {
                    changed |= next != *s;
                    *s = next;
                    true
                }
                None => {
                    changed = true;
                    false
                }
            }
        });
        // The column a series plots is an index into the grid like any other —
        // but only into the grid it actually reads. Shifting it for a column
        // inserted on some OTHER sheet would leave `col` contradicting
        // `values_ref`, and would mark a chart nobody touched for regeneration.
        let col_mine = match ser.values_ref.as_ref() {
            Some(v) => mine(v),
            None => source_mine,
        };
        if !shift.rows && col_mine {
            if let Some(c) = ser.col {
                let next = point(c, shift);
                changed |= next != Some(c);
                ser.col = next;
            }
        }
        // `name_ref` is kept as its `<c:f>` text, so it round-trips through the
        // same spelling rules `rename_sheet_in_chart` uses.
        if let Some(p) = ser
            .name_ref
            .as_deref()
            .and_then(crate::sheet::ChartSource::parse_f_ref)
            .filter(|p| mine(p))
        {
            let next = moved(&p, shift);
            if next.as_ref() != Some(&p) {
                changed = true;
                ser.name_ref = next.map(|p| {
                    let (r1, c1, r2, c2) = p.range;
                    if (r1, c1) == (r2, c2) {
                        p.header_ref(c1)
                    } else {
                        p.to_ref()
                    }
                });
            }
        }
    }
    // Only a chart whose refs actually moved is regenerated on save — and only
    // if `chart_is_writable` also says yes; see `rename_sheet_in_chart`.
    cd.edited |= changed;
    changed
}

/// `src` rewritten by `f`, or `None` when it doesn't parse or `f` changes
/// nothing: the text then stays byte-for-byte as loaded (reprinting drops
/// spellings such as `_xlfn.` prefixes).
fn rewrite_if_changed(src: &str, f: impl FnOnce(&Expr) -> Expr) -> Option<String> {
    let ast = parse(src).ok()?;
    let out = f(&ast);
    (out != ast).then(|| to_string(&out))
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

    // An array ref that doesn't start at its own cell (one moved without
    // set_cell, say by a sort) names another block. Re-anchor it before the
    // shift: a clamped shift could otherwise make it look like this cell's.
    for (&(r, c), cell) in wb.sheets[idx].cells.iter_mut() {
        own_array_ref(cell, r, c);
    }
    shift_grid(&mut wb.sheets[idx], &shift);

    for (s, sheet) in wb.sheets.iter_mut().enumerate() {
        let home_is_target = s == idx;
        for (&(r, c), cell) in sheet.cells.iter_mut() {
            if cell.f_attrs.as_deref().is_some_and(|a| !is_array_f(a)) {
                continue; // preserved verbatim; stale is acceptable, corrupt is not
            }
            // An array formula is one the engine evaluates, so its text is
            // ours to rewrite; the block its `ref` names moves with the grid.
            let array = cell.f_attrs.is_some();
            if let Some(fa) = cell.f_attrs.as_mut() {
                let moved = f_ref(fa).and_then(|rf| {
                    adjust_formula_for_edit(rf, home_is_target, &target_name, &shift)
                });
                if let Some(m) = moved {
                    *fa = with_ref(fa, &m);
                }
            }
            // The pass above re-anchored every stale ref on the target sheet,
            // so a block there still starts at its anchor. What's left is a
            // stale ref on another sheet (its unqualified ref never shifts):
            // it covers its anchor alone.
            own_array_ref(cell, r, c);
            let Some(src) = &cell.formula else {
                continue;
            };
            let adjust = |e: &Expr| adjust_for_edit(e, home_is_target, &target_name, &shift);
            // An array formula's loaded text is only reprinted when the edit
            // really moves one of its references.
            let updated = if array {
                rewrite_if_changed(src, adjust)
            } else {
                adjust_formula_for_edit(src, home_is_target, &target_name, &shift)
            };
            if let Some(updated) = updated {
                cell.formula = Some(updated);
            }
        }
    }
    for dn in &mut wb.defined_names {
        // Defined names have no home sheet; only sheet-qualified refs shift.
        if let Some(updated) = crate::formula::rewrite_defined_name(
            &dn.formula,
            |e| crate::formula::adjust_for_edit(e, false, &target_name, &shift),
            None,
        ) {
            dn.formula = updated;
        }
    }

    // Conditional formatting and data validation follow the cells they cover,
    // and their formulas move like cell formulas.
    for (s, sheet) in wb.sheets.iter_mut().enumerate() {
        shift_rules(sheet, s == idx, &target_name, &shift);
    }

    // Page breaks (manual and automatic) stay with the row (column) that
    // starts their page.
    let sheet = &mut wb.sheets[idx];
    let breaks = if shift.rows {
        &mut sheet.row_breaks
    } else {
        &mut sheet.col_breaks
    };
    breaks.retain_mut(|b| match point(b.id, &shift) {
        Some(id) => {
            b.id = id;
            true
        }
        None => false,
    });

    // The sheet's autoFilter moves like the area its `_xlnm._FilterDatabase`
    // name holds, so the two keep naming the same cells. Each filter column
    // stays on its data column, or goes with it.
    // An insert that pushes its far edge off the sheet turns the name into
    // `#REF!`, so the filter goes too rather than being clamped.
    if let Some(af) = &mut sheet.auto_filter {
        let (r1, c1, r2, c2) = af.range;
        let far = if shift.rows { r2 } else { c2 };
        let moved = if shift.delta > 0 && point(far, &shift).is_none() {
            None
        } else if shift.rows {
            span(r1, r2, &shift).map(|(lo, hi)| (lo, c1, hi, c2))
        } else {
            span(c1, c2, &shift).map(|(lo, hi)| (r1, lo, r2, hi))
        };
        match moved {
            Some(range) => {
                af.range = range;
                if !shift.rows {
                    for c in &mut af.columns {
                        *c = c.and_then(|v| point(v, &shift));
                    }
                }
            }
            None => sheet.auto_filter = None,
        }
    }

    // Chart refs follow the grid too. They are WRITTEN back out as `<c:f>` now,
    // so a stale one doesn't just mis-draw our card: Excel re-reads it and plots
    // whatever moved into those cells. A delete is the worse half — the ref can
    // end up naming cells that hold something else entirely.
    for (s, sheet) in wb.sheets.iter_mut().enumerate() {
        let home_is_target = s == idx;
        for dw in &mut sheet.drawings {
            if let crate::sheet::DrawingKind::Chart(cd) = &mut dw.kind {
                shift_chart_refs(cd, &target_name, home_is_target, &shift);
            }
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
    // A converted table keeps the geometry it was converted with; the edits
    // its cells went through since are replayed at save on the references
    // to it (see `RemovedTable::edits`).
    for rt in &mut wb.removed_tables {
        if rt.table.sheet == idx {
            rt.edits.push(shift);
        }
    }
}

/// Every conditional-formatting and data-validation formula on `sheet`.
fn for_each_rule_formula(sheet: &mut Sheet, mut f: impl FnMut(&mut String)) {
    for cf in &mut sheet.cond_formats {
        for rule in &mut cf.rules {
            rule.formulas_mut().into_iter().for_each(&mut f);
        }
    }
    for dv in &mut sheet.validations {
        f(&mut dv.formula1);
        f(&mut dv.formula2);
    }
}

/// Move a rule's ranges (`sqref`) through the edit, as merges move, and
/// return how far its formulas have to be translated first. The formulas
/// are relative to the ranges' top-left, (min r1, min c1) over them all (the
/// anchor [`crate::cf`] evaluates them at). When a delete moves that corner
/// to another cell, by trimming the range that held it or by taking a whole
/// range that held its row or its column, the cell that becomes the new
/// anchor read the formulas translated by the distance it sat from the old
/// one: that (rows, cols) offset comes back. `None` when the edit deleted
/// every range of a rule that had some.
fn shift_rule_ranges(
    ranges: &mut Vec<(u32, u32, u32, u32)>,
    shift: &EditShift,
) -> Option<(i64, i64)> {
    if ranges.is_empty() {
        return Some((0, 0));
    }
    let anchor = |rs: &[(u32, u32, u32, u32)]| {
        rs.iter()
            .fold((u32::MAX, u32::MAX), |(r, c), &(r1, c1, _, _)| {
                (r.min(r1), c.min(c1))
            })
    };
    let before = anchor(ranges);
    let moved: Vec<_> = ranges
        .iter()
        .filter_map(|&(r1, c1, r2, c2)| {
            if shift.rows {
                span(r1, r2, shift).map(|(lo, hi)| (lo, c1, hi, c2))
            } else {
                span(c1, c2, shift).map(|(lo, hi)| (r1, lo, r2, hi))
            }
        })
        .collect();
    if moved.is_empty() {
        return None;
    }
    *ranges = moved;
    // An insert drops no range and moves the corner with its cell.
    if shift.delta >= 0 {
        return Some((0, 0));
    }
    let after = anchor(ranges);
    // The new anchor's position before the edit: on the edited axis, past
    // the deleted band when it sits at or after it; the other axis wasn't
    // edited.
    let pre = |v: u32| {
        if v < shift.at {
            v as i64
        } else {
            v as i64 - shift.delta
        }
    };
    let (pre_r, pre_c) = if shift.rows {
        (pre(after.0), after.1 as i64)
    } else {
        (after.0 as i64, pre(after.1))
    };
    Some((pre_r - before.0 as i64, pre_c - before.1 as i64))
}

/// A rule formula through the edit: translated by `by` to the rule's new
/// anchor (see [`shift_rule_ranges`]), then adjusted like a cell formula.
/// Unchanged or unparseable text stays as it is.
fn shift_rule_formula(
    src: &mut String,
    by: (i64, i64),
    home_is_target: bool,
    target: &str,
    shift: &EditShift,
) {
    let moved = rewrite_if_changed(src, |e| {
        let e = if by == (0, 0) {
            e.clone()
        } else {
            translate(e, by.0, by.1)
        };
        adjust_for_edit(&e, home_is_target, target, shift)
    });
    if let Some(moved) = moved {
        *src = moved;
    }
}

/// Move one sheet's conditional formatting and data validation for an edit
/// on sheet `target`. On the target sheet their ranges move, and a rule that
/// loses every range goes (its element named in `cf_removed` / `dv_removed`
/// for the save); on any sheet their formulas' refs move.
fn shift_rules(sheet: &mut Sheet, home_is_target: bool, target: &str, shift: &EditShift) {
    let removed = &mut sheet.cf_removed;
    sheet.cond_formats.retain_mut(|cf| {
        let by = if home_is_target {
            match shift_rule_ranges(&mut cf.ranges, shift) {
                Some(by) => by,
                None => {
                    removed.extend(cf.ix);
                    return false;
                }
            }
        } else {
            (0, 0)
        };
        for rule in &mut cf.rules {
            for f in rule.formulas_mut() {
                shift_rule_formula(f, by, home_is_target, target, shift);
            }
        }
        true
    });
    let removed = &mut sheet.dv_removed;
    sheet.validations.retain_mut(|dv| {
        let by = if home_is_target {
            match shift_rule_ranges(&mut dv.ranges, shift) {
                Some(by) => by,
                None => {
                    removed.extend(dv.ix);
                    return false;
                }
            }
        } else {
            (0, 0)
        };
        for f in [&mut dv.formula1, &mut dv.formula2] {
            shift_rule_formula(f, by, home_is_target, target, shift);
        }
        true
    });
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
    // `point` says `None` for two different things. On a DELETE the coordinate
    // is gone, and the span clamps to the edit point. On an INSERT it was pushed
    // off the end of the sheet — it clamps to the last row/column instead, since
    // collapsing to `at - 1` would silently truncate a sheet-wide
    // `<col min="1" max="16384">`, or a chart ref reading a whole column, down
    // to the few cells before the insert.
    let last = (if shift.rows { MAX_ROWS } else { MAX_COLS }) - 1;
    let lo = match point(a.min(b), shift) {
        Some(l) => l,
        None if shift.delta > 0 => last,
        None => at,
    };
    let hi = match point(a.max(b), shift) {
        Some(h) => h,
        None if shift.delta > 0 => last,
        None => at.checked_sub(1)?,
    };
    (lo <= hi).then_some((lo, hi))
}

fn shift_grid(sheet: &mut Sheet, shift: &EditShift) {
    // Cells.
    let cells = std::mem::take(&mut sheet.cells);
    sheet.cells = cells
        .into_iter()
        .filter_map(|((r, c), mut cell)| {
            let key = if shift.rows {
                point(r, shift).map(|nr| (nr, c))
            } else {
                point(c, shift).map(|nc| (r, nc))
            };
            // A spill's extent stretches/clamps on the edited axis like a
            // merge: the engine takes it as the block it owns, so a stale one
            // would clear user data moved into it or block its own values.
            if let (Some(_), Some((h, w))) = (key, cell.spill) {
                cell.spill = if shift.rows {
                    span(r, r + h.saturating_sub(1), shift).map(|(lo, hi)| (hi - lo + 1, w))
                } else {
                    span(c, c + w.saturating_sub(1), shift).map(|(lo, hi)| (h, hi - lo + 1))
                };
            }
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
        let filtered = std::mem::take(&mut sheet.filtered_rows);
        sheet.filtered_rows = filtered
            .into_iter()
            .filter_map(|r| point(r, shift))
            .collect();
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

// ---------------------------------------------------------------------------
// Tables: rename, resize, convert to range
// ---------------------------------------------------------------------------

/// The name a new table column takes, as Excel's Format as Table and Resize
/// Table give it: its header cell's text (a number's digits), else
/// `Column<n>`; then made unique against `taken`, case-insensitively as table
/// column names compare, by appending 2, 3, ….
pub fn table_column_name(header: Option<&CellValue>, n: u32, taken: &[String]) -> String {
    let base = match header {
        Some(CellValue::Text(t)) if !t.trim().is_empty() => t.clone(),
        Some(CellValue::Number(v)) => v.to_string(),
        _ => format!("Column{n}"),
    };
    let (mut name, mut k) = (base.clone(), 1);
    while taken.iter().any(|x| x.eq_ignore_ascii_case(&name)) {
        k += 1;
        name = format!("{base}{k}");
    }
    name
}

fn rects_overlap(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

/// Why a table can't cover `rect` on `sheet`, or `None` when it can: Excel
/// refuses a table over another table, over a PivotTable, or over part of a
/// multi-cell array formula. `ignore` is the table being resized, which may
/// of course overlap itself.
pub fn table_range_conflict(
    wb: &Workbook,
    sheet: usize,
    rect: (u32, u32, u32, u32),
    ignore: Option<usize>,
) -> Option<String> {
    let other = wb
        .tables
        .iter()
        .enumerate()
        .find(|&(i, t)| Some(i) != ignore && t.sheet == sheet && rects_overlap(t.range, rect));
    if let Some((_, t)) = other {
        return Some(format!("The range overlaps table {}", t.name));
    }
    if let Some(p) = wb
        .pivots
        .iter()
        .find(|p| p.sheet == sheet && rects_overlap(p.location, rect))
    {
        return Some(format!("The range overlaps PivotTable {}", p.name));
    }
    let sh = wb.sheets.get(sheet)?;
    for (&(r, c), cell) in &sh.cells {
        let Some((h, w)) = cell.spill.filter(|_| cell.is_array_formula()) else {
            continue;
        };
        if h * w > 1 && rects_overlap((r, c, r + h - 1, c + w - 1), rect) {
            return Some(format!(
                "The range contains part of the array formula at {}",
                cell_name(r, c)
            ));
        }
    }
    None
}

/// Where a formula sits, for the table rewrites: its sheet (a defined name
/// has none) and its cell (a conditional-format or validation rule has none).
type FormulaSite = (Option<usize>, Option<(u32, u32)>);

/// Rewrite every formula a table rename or conversion can reach: cell
/// formulas (array formulas included), defined names, and conditional-format
/// and data-validation rules. A formula `f` leaves unchanged keeps its text
/// exactly; one held verbatim (a shared or data-table formula) is left alone.
fn rewrite_workbook_formulas(wb: &mut Workbook, f: impl Fn(&Expr, FormulaSite) -> Expr) {
    for (s, sheet) in wb.sheets.iter_mut().enumerate() {
        for (&(r, c), cell) in sheet.cells.iter_mut() {
            let Some(src) = &cell.formula else {
                continue;
            };
            if cell.f_attrs.as_deref().is_some_and(|a| !is_array_f(a)) {
                continue;
            }
            if let Some(updated) = rewrite_if_changed(src, |e| f(e, (Some(s), Some((r, c))))) {
                cell.formula = Some(updated);
            }
        }
        for_each_rule_formula(sheet, |src| {
            if let Some(updated) = rewrite_if_changed(src, |e| f(e, (Some(s), None))) {
                *src = updated;
            }
        });
    }
    for dn in &mut wb.defined_names {
        if let Some(updated) =
            crate::formula::rewrite_defined_name(&dn.formula, |e| f(e, (None, None)), None)
        {
            dn.formula = updated;
        }
    }
}

fn table_index(wb: &Workbook, name: &str) -> Result<usize, String> {
    wb.tables
        .iter()
        .position(|t| t.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("There is no table named {name}"))
}

/// Excel's Table Name: give table `old` the name `new` and rewrite every
/// formula that names it — structured references, bare table names, defined
/// names, rules — and the PivotTables built on it. The name must follow
/// Excel's rules ([`crate::names::check_name`]) and be unique among tables
/// and defined names, case-insensitively; another case of the table's own
/// name is allowed. The table part takes the name when the file is saved.
pub fn rename_table(wb: &mut Workbook, old: &str, new: &str) -> Result<(), String> {
    let idx = table_index(wb, old)?;
    crate::names::check_name(new)?;
    let clash = wb
        .tables
        .iter()
        .enumerate()
        .any(|(i, t)| i != idx && t.name.eq_ignore_ascii_case(new));
    if clash {
        return Err(format!("A table named {new} already exists"));
    }
    if wb
        .defined_names
        .iter()
        .any(|d| d.name.eq_ignore_ascii_case(new))
    {
        return Err(format!("{new} is already a defined name"));
    }
    let cur = wb.tables[idx].name.clone();
    if cur == new {
        return Ok(());
    }
    let map = [(cur.clone(), new.to_string())];
    rewrite_workbook_formulas(wb, |e, _| crate::formula::rename_tables_in_expr(e, &map));
    for piv in &mut wb.pivots {
        if let crate::pivot::PivotSource::Table(n) = &mut piv.source {
            if n.eq_ignore_ascii_case(&cur) {
                *n = new.to_string();
            }
        }
    }
    wb.tables[idx].name = new.to_string();
    Ok(())
}

/// Excel's Resize Table: move table `name` onto `rect` (r1, c1, r2, c2,
/// 0-based). The header row stays where it is, the new range overlaps the
/// old one, keeps at least one data row ("A table needs at least one data
/// row"), and covers no other table, PivotTable or array formula
/// ([`table_range_conflict`]). A table with a totals row keeps its bottom row
/// ("Turn off the Total Row first"). Columns still covered keep their names;
/// a new column is named after its header cell, or `Column<n>`, made unique
/// ignoring case (`Qty` beside a `Qty` becomes `Qty2`, [`table_column_name`]),
/// and that name is written into the header cell. A dropped column's references go `#REF!` at
/// evaluation; their text is left alone. Formulas aren't rewritten: the
/// columns they name are still found by name.
pub fn resize_table(
    wb: &mut Workbook,
    name: &str,
    rect: (u32, u32, u32, u32),
) -> Result<(), String> {
    let idx = table_index(wb, name)?;
    let t = &wb.tables[idx];
    let (r1, c1, r2, c2) = rect;
    if r1 > r2 || c1 > c2 || r2 >= MAX_ROWS || c2 >= MAX_COLS {
        return Err("That isn't a valid range".into());
    }
    if t.header_rows > 0 && r1 != t.range.0 {
        return Err(format!("The header row must stay in row {}", t.range.0 + 1));
    }
    if !rects_overlap(t.range, rect) {
        return Err("The new range must overlap the table".into());
    }
    if r2 - r1 < t.header_rows + t.totals_rows {
        return Err("A table needs at least one data row".into());
    }
    // The totals row is the table's last row: a new bottom would leave its
    // cells behind as data (a SUBTOTAL over itself) and make a data row the
    // totals row. Moving it belongs with the Total Row command.
    if t.totals_rows > 0 && r2 != t.range.2 {
        return Err("Turn off the Total Row first".into());
    }
    if let Some(why) = table_range_conflict(wb, t.sheet, rect, Some(idx)) {
        return Err(why);
    }
    let (sheet, header_rows) = (t.sheet, t.header_rows);
    let (_, oc1, _, oc2) = t.range;
    let old_columns = t.columns.clone();
    let kept = |c: u32| {
        (oc1..=oc2)
            .contains(&c)
            .then(|| old_columns.get((c - oc1) as usize).cloned())
            .flatten()
    };
    let mut taken: Vec<String> = (c1..=c2).filter_map(kept).collect();
    let mut columns = Vec::new();
    for c in c1..=c2 {
        if let Some(n) = kept(c) {
            columns.push(n);
            continue;
        }
        let header = (header_rows > 0)
            .then(|| wb.sheets[sheet].cell(r1, c).map(|cl| cl.value.clone()))
            .flatten();
        let nm = table_column_name(header.as_ref(), c - c1 + 1, &taken);
        taken.push(nm.clone());
        if header_rows > 0 && !matches!(&header, Some(CellValue::Text(t)) if *t == nm) {
            let sh = &mut wb.sheets[sheet];
            let mut cell = sh.cell(r1, c).cloned().unwrap_or_default();
            cell.value = CellValue::Text(nm.clone());
            cell.formula = None;
            sh.set_cell(r1, c, cell);
        }
        columns.push(nm);
    }
    let t = &mut wb.tables[idx];
    t.range = rect;
    t.columns = columns;
    Ok(())
}

/// Excel's Convert to Range: table `name` stops being a table. Every
/// structured reference to it — qualified anywhere, unqualified inside it —
/// becomes the cells it covers ([`crate::formula::table_refs_to_cells_in_expr`]),
/// and its cell values and formats stay as they are. The table part leaves
/// the file at the next save (it is kept in [`Workbook::removed_tables`]
/// until then, so an undo can bring the table back).
///
/// Refused while a PivotTable is built on the table, or while a SUMX-family
/// formula iterates it: neither has a range form that means the same.
pub fn convert_table_to_range(wb: &mut Workbook, name: &str) -> Result<(), String> {
    let idx = table_index(wb, name)?;
    let t = wb.tables[idx].clone();
    let pivot = wb.pivots.iter().find(|p| {
        matches!(&p.source, crate::pivot::PivotSource::Table(n) if n.eq_ignore_ascii_case(&t.name))
    });
    if let Some(p) = pivot {
        return Err(format!("PivotTable {} uses this table", p.name));
    }
    // Every place the rewrite below reaches (cells, rules, names).
    let iterates = |src: &str| {
        parse(src).is_ok_and(|ast| {
            let mut iterated = Vec::new();
            crate::formula::collect_iterated_tables(&ast, &mut iterated);
            iterated.iter().any(|n| n.eq_ignore_ascii_case(&t.name))
        })
    };
    for sh in &wb.sheets {
        for (&(r, c), cell) in &sh.cells {
            if cell.formula.as_deref().is_some_and(iterates) {
                return Err(format!(
                    "The formula in {}!{} iterates this table",
                    sh.name,
                    cell_name(r, c)
                ));
            }
        }
        let cf = sh.cond_formats.iter().flat_map(|cf| &cf.rules);
        if cf.flat_map(|rule| rule.formulas()).any(|f| iterates(f)) {
            return Err(format!(
                "A conditional format on {} iterates this table",
                sh.name
            ));
        }
        let dv = sh.validations.iter();
        if dv
            .flat_map(|v| [&v.formula1, &v.formula2])
            .any(|f| iterates(f))
        {
            return Err(format!(
                "A data validation rule on {} iterates this table",
                sh.name
            ));
        }
    }
    if let Some(dn) = wb.defined_names.iter().find(|d| iterates(&d.formula)) {
        return Err(format!("The name {} iterates this table", dn.name));
    }
    let info = t.info();
    let sheet_name = wb.sheets[t.sheet].name.clone();
    let target = crate::formula::TableToRange {
        name: &t.name,
        sheet_name: &sheet_name,
        info: &info,
    };
    rewrite_workbook_formulas(wb, |e, (s, cell)| {
        let host = crate::formula::FormulaHost {
            same_sheet: s == Some(t.sheet),
            row: cell.map(|(r, _)| r),
            inside: match (s, cell) {
                (Some(s), Some((r, c))) => t.contains(s, r, c),
                _ => false,
            },
        };
        crate::formula::table_refs_to_cells_in_expr(e, &target, host)
    });
    wb.tables.remove(idx);
    wb.removed_tables.push(crate::sheet::RemovedTable {
        table: t,
        edits: Vec::new(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::sheet::{Cell, CellValue, Xf, parse_cell_name};

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
    fn a_whole_column_sort_stops_at_the_last_used_row() {
        // `A1:A1048576` is the ordinary "sort this column" idiom, and the range
        // field passes it straight through. Materialising a million rows ×
        // every used column would exhaust memory long before it sorted
        // anything; the rows past the used region are empty either way.
        let mut w = wb(&[
            ("A1", Cell::text("Pear")),
            ("A2", Cell::text("Apple")),
            ("A3", Cell::text("Fig")),
        ]);
        let n = sort_rows(&mut w, 0, 0, crate::sheet::MAX_ROWS - 1, &[(0, true)]);
        assert_eq!(n, 3, "the used rows, not a million");
        let s = &w.sheets[0];
        let col = |r: u32| s.cell(r, 0).map(|c| c.value.clone());
        assert_eq!(col(0), Some(CellValue::Text("Apple".into())));
        assert_eq!(col(1), Some(CellValue::Text("Fig".into())));
        assert_eq!(col(2), Some(CellValue::Text("Pear".into())));
        // An empty sheet has nothing to clamp against, and says so.
        assert_eq!(sort_rows(&mut wb(&[]), 0, 0, 100, &[(0, true)]), 0);
    }

    fn comma() -> crate::textio::TextParse {
        crate::textio::TextParse {
            kind: crate::textio::SplitKind::Delimited {
                delims: crate::textio::Delimiters::only(','),
                consecutive: false,
            },
            ..crate::textio::TextParse::default()
        }
    }

    /// Column A, rows `r1..=r2`, converted in place.
    fn col_a(r1: u32, r2: u32) -> TtcSource {
        TtcSource::new(0, (r1, 0, r2, 0)).unwrap()
    }

    #[test]
    fn a_whole_column_text_to_columns_stops_at_the_last_used_row() {
        let mut w = wb(&[("A1", Cell::text("a,b")), ("A2", Cell::text("c,d"))]);
        let n = text_to_columns(
            &mut w,
            &col_a(0, crate::sheet::MAX_ROWS - 1),
            &comma(),
            None,
        );
        assert_eq!(n, 2);
        let s = &w.sheets[0];
        assert_eq!(
            s.cell(0, 1).map(|c| c.value.clone()),
            Some(CellValue::Text("b".into()))
        );
        assert_eq!(
            text_to_columns(&mut wb(&[]), &col_a(0, 100), &comma(), None),
            0
        );
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
            assert_eq!(
                s2.cell(0, c).map(|x| x.value.clone()),
                Some(CellValue::Text("x".into()))
            );
        }
    }

    #[test]
    fn autofill_step_of_five_and_no_op() {
        // 0,5 → 10,15,20 (step 5).
        let mut w = wb(&[("A1", Cell::number(0.0)), ("A2", Cell::number(5.0))]);
        autofill(&mut w, 0, (0, 0, 1, 0), (4, 0));
        let s = &w.sheets[0];
        assert_eq!(
            s.cell(4, 0).map(|c| c.value.clone()),
            Some(CellValue::Number(20.0))
        );
        // Dragging back onto the source (no extension) fills nothing.
        assert_eq!(autofill(&mut w, 0, (0, 0, 1, 0), (1, 0)), 0);
    }

    #[test]
    fn autofill_rebases_relative_refs_but_not_absolute() {
        // D1 = B1*C1 over three rows of data; drag D1's handle down to D3.
        let mut w = wb(&[
            ("B1", Cell::number(2.0)),
            ("C1", Cell::number(3.0)),
            ("B2", Cell::number(4.0)),
            ("C2", Cell::number(5.0)),
            ("B3", Cell::number(6.0)),
            ("C3", Cell::number(7.0)),
            ("A1", Cell::number(10.0)), // the fixed rate $A$1
        ]);
        w.sheets[0].set_cell(
            0,
            3,
            Cell {
                formula: Some("B1*C1*$A$1".into()),
                ..Default::default()
            },
        );
        assert_eq!(autofill(&mut w, 0, (0, 3, 0, 3), (2, 3)), 2);
        let f = |r: u32| w.sheets[0].cell(r, 3).and_then(|c| c.formula.clone());
        assert_eq!(f(1).as_deref(), Some("B2*C2*$A$1"));
        assert_eq!(f(2).as_deref(), Some("B3*C3*$A$1"));

        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(value_at(&w, "D2"), CellValue::Number(200.0));
        assert_eq!(value_at(&w, "D3"), CellValue::Number(420.0));
    }

    #[test]
    fn autofill_right_rebases_columns_not_rows() {
        // B1 = B2+B3, dragged RIGHT to D1: the refs must walk columns.
        let mut w = wb(&[("B2", Cell::number(1.0)), ("B3", Cell::number(2.0))]);
        w.sheets[0].set_cell(
            0,
            1,
            Cell {
                formula: Some("B2+B3".into()),
                ..Default::default()
            },
        );
        assert_eq!(autofill(&mut w, 0, (0, 1, 0, 1), (0, 3)), 2);
        let f = |c: u32| w.sheets[0].cell(0, c).and_then(|x| x.formula.clone());
        assert_eq!(f(2).as_deref(), Some("C2+C3"));
        assert_eq!(f(3).as_deref(), Some("D2+D3"));
    }

    #[test]
    fn autofill_cycles_a_non_numeric_pattern() {
        // "x","y" filled down five rows repeats the pair, rather than trying to
        // read a series out of text.
        let mut w = wb(&[("A1", Cell::text("x")), ("A2", Cell::text("y"))]);
        assert_eq!(autofill(&mut w, 0, (0, 0, 1, 0), (6, 0)), 5);
        let t = |r: u32| match w.sheets[0].cell(r, 0).map(|c| c.value.clone()) {
            Some(CellValue::Text(s)) => s,
            v => panic!("A{} not text: {v:?}", r + 1),
        };
        assert_eq!(
            (t(2), t(3), t(4), t(5), t(6)),
            ("x".into(), "y".into(), "x".into(), "y".into(), "x".into())
        );
    }

    #[test]
    fn autofill_drops_the_group_marker_from_a_copied_formula() {
        // `f_attrs` names cells the SOURCE owns — a shared group's `si`, an array
        // formula's `ref`. Copied along, several cells would claim the same
        // group and Excel would offer to repair the file; and while the marker
        // stayed on, the copy went unshifted and quietly recomputed the source's
        // own formula. The copy is a plain formula of its own instead.
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        w.sheets[0].set_cell(
            0,
            1,
            Cell {
                formula: Some("A1*2".into()),
                f_attrs: Some(" t=\"shared\" si=\"0\"".into()),
                ..Default::default()
            },
        );
        assert_eq!(autofill(&mut w, 0, (0, 1, 0, 1), (2, 1)), 2);
        let cell = |r: u32| w.sheets[0].cell(r, 1).cloned().unwrap();
        assert_eq!(
            cell(1).formula.as_deref(),
            Some("A2*2"),
            "the copy shifts like any other formula"
        );
        assert_eq!(cell(2).formula.as_deref(), Some("A3*2"));
        assert!(cell(1).f_attrs.is_none() && cell(2).f_attrs.is_none());
        // The source keeps its own group intact.
        assert_eq!(cell(0).formula.as_deref(), Some("A1*2"));
        assert!(cell(0).f_attrs.is_some());
    }

    /// #785 r1: a copy of a spilling anchor doesn't bring the source's spill
    /// extent. Evaluated after the suite's engine rebuild, a constant where
    /// the copy would spill blocks it rather than being taken as its own.
    #[test]
    fn autofill_copy_of_a_spilling_anchor_never_takes_cells_under_its_spill() {
        let mut w = wb(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("C2", Cell::number(99.0)),
        ]);
        let mut eng = crate::engine::Engine::new(&w);
        eng.set_cell(&mut w, (0, 0, 1), Cell::formula("A1:A3*2"));
        assert_eq!(w.sheets[0].cell(0, 1).unwrap().spill, Some((3, 1)));
        assert_eq!(autofill(&mut w, 0, (0, 1, 0, 1), (0, 2)), 1);
        assert_eq!(w.sheets[0].cell(0, 2).unwrap().spill, None);
        let mut eng = crate::engine::Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(
            w.sheets[0].cell(1, 2).unwrap().value,
            CellValue::Number(99.0)
        );
        assert_eq!(
            w.sheets[0].cell(0, 2).unwrap().value,
            CellValue::Error("#SPILL!".into())
        );
    }

    fn values(w: &Workbook, names: &[&str]) -> Vec<CellValue> {
        names
            .iter()
            .map(|n| {
                let (r, c) = parse_cell_name(n).unwrap();
                w.sheets[0]
                    .cell(r, c)
                    .map(|c| c.value.clone())
                    .unwrap_or_default()
            })
            .collect()
    }

    fn nums(ns: &[f64]) -> Vec<CellValue> {
        ns.iter().map(|&n| CellValue::Number(n)).collect()
    }

    /// #785 r2: a whole spill block filled down or right copies its spilled
    /// values blank — each copy of the anchor spills there itself, with no
    /// stale constants in its way. A typed dynamic array and a loaded CSE
    /// block alike.
    #[test]
    fn autofill_of_a_whole_spill_block_spills_each_copy() {
        let column: Vec<(String, Cell)> = (1..=6)
            .map(|r| (format!("A{r}"), Cell::number(f64::from(r))))
            .collect();
        let column: Vec<(&str, Cell)> = column
            .iter()
            .map(|(n, c)| (n.as_str(), c.clone()))
            .collect();
        // A loaded CSE block, as the loader gives it: its `<f>` attributes, the
        // spill its ref records, and its values stored over the block.
        let cse = Cell {
            value: CellValue::Number(2.0),
            formula: Some("A1:A3*2".into()),
            f_attrs: Some(" t=\"array\" ref=\"D1:D3\"".into()),
            spill: Some((3, 1)),
            ..Cell::default()
        };
        for loaded in [false, true] {
            let mut w = wb(&column);
            if loaded {
                w.sheets[0].set_cell(0, 3, cse.clone());
                w.sheets[0].set_cell(1, 3, Cell::number(4.0));
                w.sheets[0].set_cell(2, 3, Cell::number(6.0));
                Engine::new(&w).recalc_all(&mut w);
            } else {
                let mut eng = Engine::new(&w);
                eng.set_cell(&mut w, (0, 0, 3), Cell::formula("A1:A3*2"));
            }
            assert_eq!(w.sheets[0].cell(0, 3).unwrap().spill, Some((3, 1)));
            let d1 = w.sheets[0].cell(0, 3).unwrap();
            assert_eq!(d1.f_attrs.is_some(), loaded);
            assert_eq!(autofill(&mut w, 0, (0, 3, 2, 3), (5, 3)), 3);
            let mut eng = Engine::new(&w);
            eng.recalc_all(&mut w);
            let d = ["D1", "D2", "D3", "D4", "D5", "D6"];
            assert_eq!(values(&w, &d), nums(&[2.0, 4.0, 6.0, 8.0, 10.0, 12.0]));
            assert_eq!(w.sheets[0].cell(3, 3).unwrap().spill, Some((3, 1)));
            assert_eq!(w.sheets[0].cell(0, 3).unwrap().f_attrs.is_some(), loaded);
        }

        // Right: SEQUENCE(1,3) in A5 spills A5:C5; filled to D5:F5.
        let mut w = wb(&[]);
        let mut eng = Engine::new(&w);
        eng.set_cell(&mut w, (0, 4, 0), Cell::formula("SEQUENCE(1,3)"));
        assert_eq!(autofill(&mut w, 0, (4, 0, 4, 2), (4, 5)), 3);
        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        let row = ["A5", "B5", "C5", "D5", "E5", "F5"];
        assert_eq!(values(&w, &row), nums(&[1.0, 2.0, 3.0, 1.0, 2.0, 3.0]));
        assert_eq!(w.sheets[0].cell(4, 3).unwrap().spill, Some((1, 3)));
    }

    #[test]
    fn autofill_of_an_unparseable_shared_follower_leaves_no_empty_formula() {
        // A follower whose master didn't parse carries the marker and no text;
        // dropping the marker must drop the empty `<f>` with it.
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        w.sheets[0].set_cell(
            0,
            1,
            Cell {
                formula: Some(String::new()),
                f_attrs: Some(" t=\"shared\" si=\"3\"".into()),
                ..Default::default()
            },
        );
        assert_eq!(autofill(&mut w, 0, (0, 1, 0, 1), (1, 1)), 1);
        // Nothing left to write: a blank copy isn't stored at all.
        let copy = w.sheets[0].cell(1, 1).cloned().unwrap_or_default();
        assert!(copy.formula.is_none() && copy.f_attrs.is_none());
    }

    #[test]
    fn autofill_refuses_a_denormalized_source() {
        // A backwards range has no cells to read, and the pattern walk divides
        // by their count — this used to panic rather than decline.
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        assert_eq!(autofill(&mut w, 0, (3, 0, 1, 0), (9, 0)), 0);
        assert_eq!(autofill(&mut w, 0, (0, 3, 0, 1), (0, 9)), 0);
    }

    #[test]
    fn autofill_copies_formulas_instead_of_extending_their_values() {
        // Two formula cells whose RESULTS look like a series (1, 2) must still
        // fill as copied formulas, not as the numbers 3, 4.
        let mut w = wb(&[("A1", Cell::number(1.0)), ("A2", Cell::number(2.0))]);
        w.sheets[0].set_cell(
            0,
            1,
            Cell {
                formula: Some("A1".into()),
                ..Default::default()
            },
        );
        w.sheets[0].set_cell(
            1,
            1,
            Cell {
                formula: Some("A2".into()),
                ..Default::default()
            },
        );
        autofill(&mut w, 0, (0, 1, 1, 1), (3, 1));
        let f = |r: u32| w.sheets[0].cell(r, 1).and_then(|c| c.formula.clone());
        assert_eq!(f(2).as_deref(), Some("A3"));
        assert_eq!(f(3).as_deref(), Some("A4"));
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
        assert_eq!(
            s.cell(0, 0).map(|c| c.value.clone()),
            Some(CellValue::Number(1.0))
        );
        assert_eq!(
            s.cell(2, 0).map(|c| c.value.clone()),
            Some(CellValue::Number(3.0))
        );
        assert!(s.cell(3, 0).is_none_or(|c| c.is_blank()));
        // Descending: 3,2,1,blank (blank still last)
        sort_rows(&mut w, 0, 0, 3, &[(0, false)]);
        let s = &w.sheets[0];
        assert_eq!(
            s.cell(0, 0).map(|c| c.value.clone()),
            Some(CellValue::Number(3.0))
        );
        assert!(s.cell(3, 0).is_none_or(|c| c.is_blank()));
    }

    /// A1:A4 = 3, 1, 4, 2 and a spilling `formula` typed at E1, evaluated.
    fn data_with_spill_at_e1(formula: &str) -> (Workbook, crate::engine::Engine) {
        let mut w = wb(&[
            ("A1", Cell::number(3.0)),
            ("A2", Cell::number(1.0)),
            ("A3", Cell::number(4.0)),
            ("A4", Cell::number(2.0)),
        ]);
        let mut eng = crate::engine::Engine::new(&w);
        eng.set_cell(&mut w, (0, 0, 4), Cell::formula(formula));
        (w, eng)
    }

    #[test]
    fn sort_refuses_rows_that_cut_a_spill() {
        // #840 (r4-pre-structural-spill): rows that meet a spill of two rows
        // or more don't sort, whether they hold its anchor or only some of
        // its cells; rows clear of it do.
        let (mut w, _) = data_with_spill_at_e1("SEQUENCE(3)");
        assert_eq!(w.sheets[0].cell(0, 4).unwrap().spill, Some((3, 1)));
        let before = w.sheets[0].cells.clone();
        for (r1, r2) in [(0, 3), (2, 3), (1, 1_048_575)] {
            assert!(sort_cuts_spill(&w, 0, r1, r2), "{r1}..={r2}");
            assert_eq!(sort_rows(&mut w, 0, r1, r2, &[(0, true)]), 0);
            assert_eq!(w.sheets[0].cells, before, "{r1}..={r2}");
        }
        w.sheets[0].set_cell(4, 0, Cell::number(9.0));
        w.sheets[0].set_cell(5, 0, Cell::number(8.0));
        assert!(!sort_cuts_spill(&w, 0, 3, 5));
        assert_eq!(sort_rows(&mut w, 0, 3, 5, &[(0, true)]), 3);
        assert_eq!(value_at(&w, "A4"), CellValue::Number(2.0));
        assert_eq!(value_at(&w, "A6"), CellValue::Number(9.0));
    }

    #[test]
    fn sort_moves_a_one_row_spill_with_its_row() {
        // #840: a spill within one row moves with it and spills there.
        let (mut w, _) = data_with_spill_at_e1("SEQUENCE(1,3)");
        assert_eq!(w.sheets[0].cell(0, 4).unwrap().spill, Some((1, 3)));
        assert!(!sort_cuts_spill(&w, 0, 0, 3));
        assert_eq!(sort_rows(&mut w, 0, 0, 3, &[(0, true)]), 4);
        // A1 = 3 sorts third.
        let mut eng = crate::engine::Engine::new(&w);
        eng.recalc_all(&mut w);
        let row = |w: &Workbook, r: u32| -> Vec<CellValue> {
            (4..7)
                .map(|c| {
                    w.sheets[0]
                        .cell(r, c)
                        .map_or(CellValue::Empty, |cl| cl.value.clone())
                })
                .collect()
        };
        let n = |v: f64| CellValue::Number(v);
        assert_eq!(row(&w, 2), vec![n(1.0), n(2.0), n(3.0)]);
        assert_eq!(row(&w, 0), vec![CellValue::Empty; 3]);
        assert_eq!(w.sheets[0].cell(2, 4).unwrap().spill, Some((1, 3)));
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
            ("A4", Cell::text(" 7 ")),
        ]);
        // Every non-empty cell is converted, split or not.
        let n = text_to_columns(&mut w, &col_a(0, 3), &comma(), None);
        assert_eq!(n, 4);
        assert_eq!(value_at(&w, "A1"), CellValue::Text("Laptop".into()));
        assert_eq!(value_at(&w, "B1"), CellValue::Number(2.0));
        assert_eq!(value_at(&w, "C1"), CellValue::Number(1199.0));
        assert_eq!(value_at(&w, "A2"), CellValue::Text("Dock".into()));
        assert_eq!(value_at(&w, "C2"), CellValue::Number(179.0));
        assert_eq!(value_at(&w, "A3"), CellValue::Text("NoDelimiter".into()));
        // A text that reads as a number becomes one.
        assert_eq!(value_at(&w, "A4"), CellValue::Number(7.0));
    }

    /// #692: a qualifier keeps a delimiter inside a field, and a Text column
    /// keeps leading zeros.
    #[test]
    fn text_to_columns_honours_the_qualifier_and_column_formats() {
        use crate::textio::ColFormat;
        let mut w = wb(&[("A1", Cell::text("Pen,4,\"Blue, fine\",0012"))]);
        let opts = crate::textio::TextParse {
            columns: vec![
                ColFormat::General,
                ColFormat::General,
                ColFormat::General,
                ColFormat::Text,
            ],
            ..comma()
        };
        text_to_columns(&mut w, &col_a(0, 0), &opts, None);
        assert_eq!(value_at(&w, "A1"), CellValue::Text("Pen".into()));
        assert_eq!(value_at(&w, "B1"), CellValue::Number(4.0));
        assert_eq!(value_at(&w, "C1"), CellValue::Text("Blue, fine".into()));
        assert_eq!(value_at(&w, "D1"), CellValue::Text("0012".into()));
        let d1 = w.sheets[0].cell(0, 3).unwrap().style;
        assert_eq!(w.styles.xf(d1).code.as_deref(), Some("@"));
        // General turns 0012 into 12.
        let mut w = wb(&[("A1", Cell::text("Pen,4,\"Blue, fine\",0012"))]);
        text_to_columns(&mut w, &col_a(0, 0), &comma(), None);
        assert_eq!(value_at(&w, "D1"), CellValue::Number(12.0));
    }

    /// #692: empty fields are kept unless consecutive delimiters are one.
    #[test]
    fn text_to_columns_keeps_or_collapses_empty_fields() {
        let mut w = wb(&[("A1", Cell::text("Ink,,Red,7"))]);
        text_to_columns(&mut w, &col_a(0, 0), &comma(), None);
        assert_eq!(value_at(&w, "B1"), CellValue::Empty);
        assert_eq!(value_at(&w, "C1"), CellValue::Text("Red".into()));
        assert_eq!(value_at(&w, "D1"), CellValue::Number(7.0));
        let one = crate::textio::TextParse {
            kind: crate::textio::SplitKind::Delimited {
                delims: crate::textio::Delimiters::only(','),
                consecutive: true,
            },
            ..comma()
        };
        let mut w = wb(&[("A1", Cell::text("Ink,,Red,7"))]);
        text_to_columns(&mut w, &col_a(0, 0), &one, None);
        assert_eq!(value_at(&w, "B1"), CellValue::Text("Red".into()));
        assert_eq!(value_at(&w, "C1"), CellValue::Number(7.0));
    }

    /// #692: Tab, Semicolon, Comma, Space and Other at once, and fixed width.
    #[test]
    fn text_to_columns_splits_on_several_delimiters_or_fixed_width() {
        let all = crate::textio::TextParse {
            kind: crate::textio::SplitKind::Delimited {
                delims: crate::textio::Delimiters {
                    tab: true,
                    semicolon: true,
                    comma: true,
                    space: true,
                    other: Some('|'),
                },
                consecutive: false,
            },
            ..comma()
        };
        let mut w = wb(&[("A1", Cell::text("a\tb;c,d e|f"))]);
        text_to_columns(&mut w, &col_a(0, 0), &all, None);
        let got: Vec<CellValue> = ["A1", "B1", "C1", "D1", "E1", "F1"]
            .iter()
            .map(|n| value_at(&w, n))
            .collect();
        let want: Vec<CellValue> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|t| CellValue::Text(t.to_string()))
            .collect();
        assert_eq!(got, want);
        let fixed = crate::textio::TextParse {
            kind: crate::textio::SplitKind::Fixed { breaks: vec![3, 5] },
            ..comma()
        };
        let mut w = wb(&[("A1", Cell::text("ABC12xyz"))]);
        text_to_columns(&mut w, &col_a(0, 0), &fixed, None);
        assert_eq!(value_at(&w, "A1"), CellValue::Text("ABC".into()));
        assert_eq!(value_at(&w, "B1"), CellValue::Number(12.0));
        assert_eq!(value_at(&w, "C1"), CellValue::Text("xyz".into()));
    }

    /// #692: Date columns in a given order, skipped columns, and the
    /// Advanced separators with a trailing minus.
    #[test]
    fn text_to_columns_dates_skips_and_advanced_numbers() {
        use crate::textio::{ColFormat, DateOrder};
        let mut w = wb(&[("A1", Cell::text("03/04/2024;junk;1.234,5-"))]);
        let opts = crate::textio::TextParse {
            kind: crate::textio::SplitKind::Delimited {
                delims: crate::textio::Delimiters::only(';'),
                consecutive: false,
            },
            columns: vec![ColFormat::Date(DateOrder::Dmy), ColFormat::Skip],
            decimal: ',',
            thousands: '.',
            trailing_minus: true,
            ..comma()
        };
        text_to_columns(&mut w, &col_a(0, 0), &opts, None);
        let apr3 = crate::sheet::parts_to_serial(2024, 4, 3, 0, false);
        assert_eq!(value_at(&w, "A1"), CellValue::Number(apr3));
        // The skipped field takes no column.
        assert_eq!(value_at(&w, "B1"), CellValue::Number(-1234.5));
        assert_eq!(value_at(&w, "C1"), CellValue::Empty);
    }

    /// #692: data in the destination (other than the source column) asks
    /// first; more than one column is refused.
    #[test]
    fn text_to_columns_detects_overwrites_and_refuses_two_columns() {
        let w = wb(&[
            ("A1", Cell::text("a,b")),
            ("A2", Cell::text("c")),
            ("B1", Cell::text("x")),
        ]);
        assert!(ttc_would_overwrite(&w, &col_a(0, 1), &comma()));
        // B2 is empty and A2 is the source itself.
        assert!(!ttc_would_overwrite(&w, &col_a(1, 1), &comma()));
        // Skipping the second field leaves B1 alone.
        let skip = crate::textio::TextParse {
            columns: vec![
                crate::textio::ColFormat::General,
                crate::textio::ColFormat::Skip,
            ],
            ..comma()
        };
        assert!(!ttc_would_overwrite(&w, &col_a(0, 1), &skip));
        // A destination elsewhere counts its own cells.
        let mut to_b = col_a(0, 0);
        to_b.dest = (0, 1);
        assert!(ttc_would_overwrite(&w, &to_b, &comma()));
        assert_eq!(TtcSource::new(0, (0, 0, 3, 1)), Err(TTC_ONE_COLUMN));
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
    fn rename_sheet_follows_a_charts_refs() {
        use crate::sheet::{ChartData, ChartSeries, ChartSource, Drawing, DrawingKind};
        let src = |r: (u32, u32, u32, u32)| ChartSource {
            sheet: "Data".into(),
            range: r,
            cat_col: 0,
        };
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        w.sheets[0].name = "Data".into();
        w.sheets[0].drawings.push(Drawing {
            anchor_ix: 0,
            from: (0, 0),
            to: (5, 5),
            kind: DrawingKind::Chart(ChartData {
                source: Some(src((0, 0, 3, 2))),
                categories_ref: Some(src((1, 0, 3, 0))),
                series: vec![ChartSeries {
                    name: "Qty".into(),
                    values_ref: Some(src((1, 1, 3, 1))),
                    name_ref: Some("Data!$B$1".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        });
        rename_sheet(&mut w, 0, "Numbers Etc");
        let DrawingKind::Chart(cd) = &w.sheets[0].drawings[0].kind else {
            panic!("still a chart")
        };
        assert_eq!(
            cd.source.as_ref().map(|s| s.sheet.as_str()),
            Some("Numbers Etc")
        );
        assert_eq!(
            cd.categories_ref.as_ref().map(|s| s.sheet.as_str()),
            Some("Numbers Etc")
        );
        assert_eq!(
            cd.series[0].values_ref.as_ref().map(|s| s.sheet.as_str()),
            Some("Numbers Etc")
        );
        // The name ref is text, and comes back quoted — a single cell still.
        assert_eq!(cd.series[0].name_ref.as_deref(), Some("'Numbers Etc'!$B$1"));
        // Its refs moved, so the part has to be regenerated on save.
        assert!(cd.edited);
    }

    /// A chart on "Data" plotting `A1:C4`, categories in A, one series in B.
    fn chart_wb() -> Workbook {
        use crate::sheet::{ChartData, ChartSeries, ChartSource, Drawing, DrawingKind};
        let src = |r: (u32, u32, u32, u32)| ChartSource {
            sheet: "Data".into(),
            range: r,
            cat_col: 0,
        };
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        w.sheets[0].name = "Data".into();
        w.sheets[0].drawings.push(Drawing {
            anchor_ix: 0,
            from: (0, 0),
            to: (5, 5),
            kind: DrawingKind::Chart(ChartData {
                source: Some(src((0, 0, 3, 2))),
                categories_ref: Some(src((1, 0, 3, 0))),
                series: vec![ChartSeries {
                    name: "Qty".into(),
                    col: Some(1),
                    values_ref: Some(src((1, 1, 3, 1))),
                    name_ref: Some("Data!$B$1".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        });
        w
    }

    fn chart_of(w: &Workbook) -> &crate::sheet::ChartData {
        match &w.sheets[0].drawings[0].kind {
            crate::sheet::DrawingKind::Chart(cd) => cd,
            other => panic!("expected a chart, got {other:?}"),
        }
    }

    #[test]
    fn a_row_insert_moves_a_charts_refs_with_the_grid() {
        // Chart refs are WRITTEN back out as `<c:f>` now, so a stale one isn't
        // just a mis-drawn card: Excel re-reads it and plots whatever moved into
        // those cells.
        let mut w = chart_wb();
        insert_rows(&mut w, 0, 1, 2); // two rows above the data body
        let cd = chart_of(&w);
        assert_eq!(cd.source.as_ref().map(|s| s.range), Some((0, 0, 5, 2)));
        assert_eq!(
            cd.categories_ref.as_ref().map(|s| s.range),
            Some((3, 0, 5, 0))
        );
        assert_eq!(
            cd.series[0].values_ref.as_ref().map(|s| s.range),
            Some((3, 1, 5, 1))
        );
        // The header row didn't move, so neither did the name ref.
        assert_eq!(cd.series[0].name_ref.as_deref(), Some("Data!$B$1"));
        assert!(cd.edited);
    }

    #[test]
    fn a_column_insert_moves_a_charts_columns_and_its_name_ref() {
        let mut w = chart_wb();
        insert_cols(&mut w, 0, 0, 1); // one column to the left of everything
        let cd = chart_of(&w);
        assert_eq!(cd.source.as_ref().map(|s| s.range), Some((0, 1, 3, 3)));
        assert_eq!(cd.source.as_ref().map(|s| s.cat_col), Some(1));
        assert_eq!(
            cd.series[0].values_ref.as_ref().map(|s| s.range),
            Some((1, 2, 3, 2))
        );
        assert_eq!(cd.series[0].col, Some(2));
        assert_eq!(cd.series[0].name_ref.as_deref(), Some("Data!$C$1"));
    }

    #[test]
    fn deleting_a_charts_only_column_drops_the_ref_instead_of_dangling() {
        // A ref that survives a full delete names cells that now hold something
        // else entirely — worse than a chart that plots nothing.
        let mut w = chart_wb();
        delete_cols(&mut w, 0, 1, 1); // column B, the series' own column
        let cd = chart_of(&w);
        assert_eq!(cd.series[0].values_ref, None);
        assert_eq!(cd.series[0].name_ref, None);
        assert_eq!(cd.series[0].col, None);
        // The box shrank rather than vanishing: A and C are still in it.
        assert_eq!(cd.source.as_ref().map(|s| s.range), Some((0, 0, 3, 1)));
        assert!(cd.edited);
    }

    #[test]
    fn deleting_a_charts_rows_drops_the_column_the_writer_would_derive_from() {
        // A ROW delete can empty a series' range too, and `col` isn't touched
        // by a row shift — but `chart_space_xml` derives a ref-less series'
        // cells from the chart's box and that column, putting the dangling ref
        // straight back. The drop has to take `col` with it.
        let mut w = chart_wb();
        let rows = match chart_of(&w).series[0].values_ref {
            Some(ref v) => (v.range.0, v.range.2),
            None => panic!("the fixture's series should start with a ref"),
        };
        delete_rows(&mut w, 0, rows.0, rows.1 - rows.0 + 1);
        let cd = chart_of(&w);
        assert_eq!(cd.series[0].values_ref, None);
        assert_eq!(cd.series[0].col, None, "or the writer re-derives the ref");
    }

    /// A scatter on "Data": no `<c:val>` at all, its numbers in `<c:xVal>`
    /// (A2:A4) and `<c:yVal>` (B2:B4), named from B1.
    fn scatter_wb() -> Workbook {
        use crate::sheet::{ChartData, ChartSeries, ChartSource, Drawing, DrawingKind};
        let src = |r: (u32, u32, u32, u32)| ChartSource {
            sheet: "Data".into(),
            range: r,
            cat_col: 0,
        };
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        w.sheets[0].name = "Data".into();
        w.sheets[0].drawings.push(Drawing {
            anchor_ix: 0,
            from: (0, 0),
            to: (5, 5),
            kind: DrawingKind::Chart(ChartData {
                kind: "scatter".into(),
                source: Some(src((0, 0, 3, 1))),
                series: vec![ChartSeries {
                    name: "Qty".into(),
                    point_refs: vec![src((1, 0, 3, 0)), src((1, 1, 3, 1))],
                    name_ref: Some("Data!$B$1".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        });
        w
    }

    #[test]
    fn a_rename_follows_a_scatters_point_refs() {
        // `point_refs` are the only ref a scatter's series has, and the panel's
        // `rebuild_source` folds them FIRST. Left on the old name they seed the
        // box with a sheet the workbook hasn't got, and every correctly-renamed
        // slot after them is skipped for the mismatch.
        let mut w = scatter_wb();
        rename_sheet(&mut w, 0, "Numbers");
        let cd = chart_of(&w);
        assert!(
            cd.series[0].point_refs.iter().all(|p| p.sheet == "Numbers"),
            "got {:?}",
            cd.series[0].point_refs
        );
        assert_eq!(
            cd.source.as_ref().map(|s| s.sheet.as_str()),
            Some("Numbers")
        );
        assert!(cd.edited);
    }

    #[test]
    fn a_row_insert_moves_a_scatters_point_refs() {
        let mut w = scatter_wb();
        insert_rows(&mut w, 0, 0, 2); // two rows above the data body
        let cd = chart_of(&w);
        assert_eq!(
            cd.series[0]
                .point_refs
                .iter()
                .map(|p| p.range)
                .collect::<Vec<_>>(),
            vec![(3, 0, 5, 0), (3, 1, 5, 1)],
        );
        assert_eq!(cd.source.as_ref().map(|s| s.range), Some((2, 0, 5, 1)));
        assert!(cd.edited);
    }

    #[test]
    fn deleting_a_scatters_x_cells_drops_that_point_ref_instead_of_dangling() {
        // Same rule as `values_ref`: a wholly-deleted range loses its ref rather
        // than plotting whatever moved into those cells.
        let mut w = scatter_wb();
        delete_cols(&mut w, 0, 0, 1); // column A, the X values
        let cd = chart_of(&w);
        assert_eq!(
            cd.series[0]
                .point_refs
                .iter()
                .map(|p| p.range)
                .collect::<Vec<_>>(),
            vec![(1, 0, 3, 0)],
            "the X ref should be gone and Y should have moved left",
        );
        assert!(cd.edited);
    }

    #[test]
    fn a_scatter_on_another_sheet_keeps_its_point_refs() {
        let mut w = scatter_wb();
        w.sheets.push(crate::sheet::Sheet {
            name: "Other".into(),
            ..Default::default()
        });
        insert_rows(&mut w, 1, 0, 5); // edit the OTHER sheet
        let cd = chart_of(&w);
        assert_eq!(
            cd.series[0]
                .point_refs
                .iter()
                .map(|p| p.range)
                .collect::<Vec<_>>(),
            vec![(1, 0, 3, 0), (1, 1, 3, 1)],
        );
        assert!(!cd.edited);
    }

    #[test]
    fn a_chart_on_another_sheet_is_left_alone() {
        let mut w = chart_wb();
        w.sheets.push(crate::sheet::Sheet {
            name: "Other".into(),
            ..Default::default()
        });
        insert_rows(&mut w, 1, 0, 5); // edit the OTHER sheet
        let cd = chart_of(&w);
        assert_eq!(cd.source.as_ref().map(|s| s.range), Some((0, 0, 3, 2)));
        assert!(!cd.edited);
    }

    #[test]
    fn a_column_insert_elsewhere_leaves_a_charts_plotted_column_alone() {
        // `ser.col` is an index into the grid the series READS. Shifting it for
        // a column inserted on some other sheet leaves it contradicting
        // `values_ref` — and `chart_space_xml` derives a ref-less series' cells
        // from it — while also marking a chart nobody touched as edited.
        let mut w = chart_wb();
        w.sheets.push(crate::sheet::Sheet {
            name: "Other".into(),
            ..Default::default()
        });
        insert_cols(&mut w, 1, 0, 3); // three columns on the OTHER sheet
        let cd = chart_of(&w);
        assert_eq!(cd.series[0].col, Some(1));
        assert_eq!(
            cd.series[0].values_ref.as_ref().map(|s| s.range),
            Some((1, 1, 3, 1))
        );
        assert!(!cd.edited);
    }

    #[test]
    fn an_insert_pushing_a_span_off_the_sheet_clamps_to_the_last_column() {
        // `point` says None both for "deleted" and for "pushed past the last
        // column". Reading the second as the first collapses the span to just
        // before the edit — and Excel writes `<col min="1" max="16384"/>` for a
        // sheet-wide width, so every column past the insert would revert.
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        w.sheets[0].col_defs.push(crate::sheet::ColDef {
            min: 0,
            max: crate::sheet::MAX_COLS - 1,
            width: Some(20.0),
            attrs: String::new(),
            default_width: false,
        });
        insert_cols(&mut w, 0, 3, 1);
        let d = &w.sheets[0].col_defs[0];
        assert_eq!((d.min, d.max), (0, crate::sheet::MAX_COLS - 1));
    }

    #[test]
    fn an_unqualified_ref_belongs_to_the_charts_own_sheet() {
        use crate::sheet::{ChartData, ChartSource, Drawing, DrawingKind};
        // A `<c:f>` with no `!` means the sheet the CHART sits on. Put such a
        // chart on "Report" and edit "Data": nothing about Report moved, so
        // Report's chart must not move either — and must not be marked edited,
        // which would regenerate a part the user never touched.
        let mut w = chart_wb();
        w.sheets.push(crate::sheet::Sheet {
            name: "Report".into(),
            drawings: vec![Drawing {
                anchor_ix: 0,
                from: (0, 0),
                to: (5, 5),
                kind: DrawingKind::Chart(ChartData {
                    source: Some(ChartSource {
                        sheet: String::new(),
                        range: (1, 1, 9, 1),
                        cat_col: 0,
                    }),
                    ..Default::default()
                }),
            }],
            ..Default::default()
        });
        delete_rows(&mut w, 0, 1, 5); // five rows off "Data"
        let far = match &w.sheets[1].drawings[0].kind {
            DrawingKind::Chart(cd) => cd,
            other => panic!("expected a chart, got {other:?}"),
        };
        assert_eq!(far.source.as_ref().map(|s| s.range), Some((1, 1, 9, 1)));
        assert!(!far.edited);
        // The chart that IS on "Data" still follows the grid.
        assert!(chart_of(&w).edited);
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
    fn a_sheet_auto_filter_moves_and_its_columns_follow_their_data() {
        let mut w = wb(&[("A1", Cell::number(1.0))]);
        // B2:D9, filtering B and D.
        w.sheets[0].auto_filter = Some(crate::sheet::SheetAutoFilter {
            range: (1, 1, 8, 3),
            columns: vec![Some(1), Some(3)],
        });
        let af = |w: &Workbook| {
            w.sheets[0]
                .auto_filter
                .clone()
                .map(|a| (a.range, a.columns))
        };
        insert_rows(&mut w, 0, 0, 2);
        assert_eq!(af(&w), Some(((3, 1, 10, 3), vec![Some(1), Some(3)])));
        insert_cols(&mut w, 0, 2, 1); // inside, between the filtered columns
        assert_eq!(af(&w), Some(((3, 1, 10, 4), vec![Some(1), Some(4)])));
        delete_cols(&mut w, 0, 1, 1); // the first filtered column
        assert_eq!(af(&w), Some(((3, 1, 10, 3), vec![None, Some(3)])));
        insert_rows(&mut w, 0, 20, 5); // below: nothing moves
        assert_eq!(af(&w), Some(((3, 1, 10, 3), vec![None, Some(3)])));
        delete_rows(&mut w, 0, 3, 8); // every row it had
        assert_eq!(af(&w), None);
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

    /// A formula cell carrying preserved `<f>` attributes `fa`.
    fn with_f_attrs(src: &str, fa: &str) -> Cell {
        let mut c = Cell::formula(src);
        c.f_attrs = Some(fa.to_string());
        c
    }

    #[test]
    fn renaming_a_sheet_rewrites_a_cse_array_that_reads_it() {
        // An array formula is one the engine evaluates, so its text is ours.
        let mut calc = Sheet {
            name: "Calc".to_string(),
            ..Sheet::default()
        };
        calc.set_cell(
            0,
            3,
            with_f_attrs("Data!A1:A3*2", " t=\"array\" ref=\"D1:D3\""),
        );
        let mut data = Sheet {
            name: "Data".to_string(),
            ..Sheet::default()
        };
        for r in 0..3 {
            data.set_cell(r, 0, Cell::number(f64::from(r + 1)));
        }
        let mut w = Workbook {
            sheets: vec![calc, data],
            ..Workbook::default()
        };
        rename_sheet(&mut w, 1, "Numbers");
        let d1 = w.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.formula.as_deref(), Some("Numbers!A1:A3*2"));
        assert_eq!(d1.f_attrs.as_deref(), Some(" t=\"array\" ref=\"D1:D3\""));
        let mut eng = Engine::new(&w);
        eng.recalc_all(&mut w);
        assert_eq!(
            w.sheets[0].cell(2, 3).unwrap().value,
            CellValue::Number(6.0)
        );
    }

    #[test]
    fn a_stale_array_ref_covers_its_own_anchor_after_a_column_delete() {
        // A clone at E1 still naming its source's block D1:D3 (one that never
        // went through set_cell). On the edited sheet the pass before the
        // shift re-anchors it to E1, which then moves to D1 (without that
        // pass, deleting column D would leave #REF!).
        let stale = || with_f_attrs("B1*2", " t=\"array\" ref=\"D1:D3\"");
        let mut w = wb(&[("E1", stale())]);
        delete_cols(&mut w, 0, 3, 1);
        let d1 = w.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.f_attrs.as_deref(), Some(" t=\"array\" ref=\"D1\""));
        assert_eq!(d1.formula.as_deref(), Some("B1*2"));
        // On another sheet nothing moves; the check after the shift re-anchors it.
        let mut other = Sheet {
            name: "Other".to_string(),
            ..Sheet::default()
        };
        other.set_cell(0, 4, stale());
        w.sheets.push(other);
        delete_cols(&mut w, 0, 0, 1);
        let e1 = w.sheets[1].cell(0, 4).unwrap();
        assert_eq!(e1.f_attrs.as_deref(), Some(" t=\"array\" ref=\"E1\""));
    }

    #[test]
    fn a_stale_array_ref_clamped_by_a_row_delete_covers_its_own_anchor() {
        // D2 holds a stale D1:D3 (moved without set_cell, say by a sort).
        // Deleting row 1 clamps it to D1:D2 as D2 moves to D1: shifted, it
        // would look like D1's own block.
        let mut w = wb(&[("D2", with_f_attrs("A2*2", " t=\"array\" ref=\"D1:D3\""))]);
        delete_rows(&mut w, 0, 0, 1);
        let d1 = w.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.f_attrs.as_deref(), Some(" t=\"array\" ref=\"D1\""));
        // A real block keeps its size through the same delete.
        let mut w = wb(&[("D2", with_f_attrs("A2*2", " t=\"array\" ref=\"D2:D4\""))]);
        delete_rows(&mut w, 0, 0, 1);
        let d1 = w.sheets[0].cell(0, 3).unwrap();
        assert_eq!(d1.f_attrs.as_deref(), Some(" t=\"array\" ref=\"D1:D3\""));
    }

    #[test]
    fn structural_edit_still_skips_a_shared_formula_marker() {
        // A shared group's or a data table's `<f>` stays verbatim, through an
        // insert and a rename alike.
        let shared = " t=\"shared\" ref=\"B2:B3\" si=\"0\"";
        let table = " t=\"dataTable\" ref=\"C2:C3\" dt2D=\"0\" dtr=\"0\" r1=\"A1\"";
        let mut w = wb(&[
            ("A1", Cell::number(1.0)),
            ("B2", with_f_attrs("Sheet1!A1*2", shared)),
            ("C2", with_f_attrs("A1*3", table)),
        ]);
        insert_rows(&mut w, 0, 0, 1);
        rename_sheet(&mut w, 0, "Renamed");
        let b = w.sheets[0].cell(2, 1).unwrap();
        assert_eq!(b.formula.as_deref(), Some("Sheet1!A1*2"));
        assert_eq!(b.f_attrs.as_deref(), Some(shared));
        let c = w.sheets[0].cell(2, 2).unwrap();
        assert_eq!(c.formula.as_deref(), Some("A1*3"));
        assert_eq!(c.f_attrs.as_deref(), Some(table));
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
        let mut styles = Styles::default();
        let changes = replace_all_in_sheet(&sheet, &mut styles, &EntryCtx::default(), "foo", "QUX");
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
        let mut styles = Styles::default();
        assert!(
            replace_all_in_sheet(&sheet, &mut styles, &EntryCtx::default(), "zzz", "y").is_empty()
        );
    }

    #[test]
    fn parse_input_keeps_shapes_that_would_need_a_format() {
        // A bare value reading: a date or currency without its number
        // format would show as a bare number, so those stay text here.
        for text in ["1/15/2024", "$5", "1,234", "-L1", "9:30 PM"] {
            assert_eq!(parse_input(text), Cell::text(text), "{text}");
        }
        assert_eq!(parse_input("50%").value, CellValue::Number(0.5));
    }

    #[test]
    fn replace_all_keeps_a_quote_prefixed_cell_text() {
        let mut styles = Styles::default();
        let quoted = styles.intern(Xf {
            quote_prefix: true,
            ..Xf::default()
        });
        let mut sheet = Sheet::default();
        sheet.set_cell(
            0,
            0,
            Cell {
                style: quoted,
                ..Cell::text("007")
            },
        );
        let changes = replace_all_in_sheet(&sheet, &mut styles, &EntryCtx::default(), "0", "1");
        let (_, _, c) = &changes[0];
        assert_eq!(c.value, CellValue::Text("117".into()));
        assert_eq!(c.style, quoted);
    }

    #[test]
    fn replace_all_never_matches_the_reentry_apostrophe() {
        let mut styles = Styles::default();
        styles.intern(Xf::default()); // style 0: General
        let quoted = styles.intern(Xf {
            quote_prefix: true,
            ..Xf::default()
        });
        let mut sheet = Sheet::default();
        sheet.set_cell(
            0,
            0,
            Cell {
                style: quoted,
                ..Cell::text("007")
            },
        );
        sheet.set_cell(1, 0, Cell::text("'abc"));
        let ctx = EntryCtx::default();
        // The prefix's `'` is not in the text: nothing to replace in 007.
        let changes = replace_all_in_sheet(&sheet, &mut styles, &ctx, "'", "");
        assert_eq!(changes.len(), 1);
        let (r, _, c) = &changes[0];
        assert_eq!(*r, 1);
        assert_eq!(c.value, CellValue::Text("abc".into()));
        // A loaded 'abc's own apostrophe is its text.
        let changes = replace_all_in_sheet(&sheet, &mut styles, &ctx, "'", "x");
        assert_eq!(changes[0].2.value, CellValue::Text("xabc".into()));
        assert!(replace_all_in_sheet(&sheet, &mut styles, &ctx, "''", "q").is_empty());
        assert!(
            !styles.xf(changes[0].2.style).quote_prefix,
            "xabc gains no prefix"
        );
    }

    #[test]
    fn replace_all_decides_the_apostrophe_from_the_result() {
        let mut styles = Styles::default();
        styles.intern(Xf::default()); // style 0: General
        let quoted = styles.intern(Xf {
            quote_prefix: true,
            ..Xf::default()
        });
        let mut sheet = Sheet::default();
        sheet.set_cell(0, 0, Cell::text("'5"));
        sheet.set_cell(1, 0, Cell::text("x'abc"));
        sheet.set_cell(
            2,
            0,
            Cell {
                style: quoted,
                ..Cell::text("zz")
            },
        );
        let ctx = EntryCtx::default();
        let at = |ch: &[(u32, u32, Cell)], r: u32| ch.iter().find(|c| c.0 == r).unwrap().2.clone();
        // A loaded '5 with its ' removed is the number 5, no prefix.
        let ch = replace_all_in_sheet(&sheet, &mut styles, &ctx, "'", "");
        let five = at(&ch, 0);
        assert_eq!(five.value, CellValue::Number(5.0));
        assert!(!styles.xf(five.style).quote_prefix);
        // x'abc with x removed keeps its own ' (as text 'abc).
        let ch = replace_all_in_sheet(&sheet, &mut styles, &ctx, "x", "");
        assert_eq!(at(&ch, 1).value, CellValue::Text("'abc".into()));
        // Replacing all of a quote-prefixed text clears the cell.
        let ch = replace_all_in_sheet(&sheet, &mut styles, &ctx, "zz", "");
        assert_eq!(at(&ch, 2).value, CellValue::Empty);
    }

    #[test]
    fn replace_all_does_not_divide_a_percent_cell_again() {
        let mut styles = Styles::default();
        let pct = styles.intern(Xf {
            numfmt: crate::sheet::NumFmt::Percent { decimals: 0 },
            code: Some("0%".into()),
            ..Xf::default()
        });
        let mut sheet = Sheet::default();
        sheet.set_cell(
            0,
            0,
            Cell {
                style: pct,
                ..Cell::number(5.0)
            },
        );
        let changes = replace_all_in_sheet(&sheet, &mut styles, &EntryCtx::default(), "5", "6");
        assert_eq!(changes[0].2.value, CellValue::Number(6.0));
        assert_eq!(changes[0].2.style, pct);
    }

    #[test]
    fn replace_all_formats_a_recognised_entry_in_a_general_cell() {
        let mut styles = Styles::default();
        let mut sheet = Sheet::default();
        sheet.set_cell(0, 0, Cell::text("1.234"));
        let changes = replace_all_in_sheet(&sheet, &mut styles, &EntryCtx::default(), ".", ",");
        let (_, _, c) = &changes[0];
        assert_eq!(c.value, CellValue::Number(1234.0));
        assert_eq!(styles.xf(c.style).code.as_deref(), Some("#,##0"));
    }

    #[test]
    fn fill_changes_drops_file_index_meta_keeps_modern_dynamic() {
        // #777: Ctrl+D/Ctrl+R copies don't inherit the source's `<c>`
        // metadata (as autofill's rebase doesn't); what the engine learned
        // about a typed formula carries over.
        use crate::sheet::CellMeta;
        let mut sheet = Sheet::default();
        let loaded = CellMeta {
            cm: Some("1".into()),
            vm: Some(("2".into(), CellValue::Number(1.0))),
            vm_body: Some("#VALUE!".into()),
            ph: true,
            ..CellMeta::default()
        };
        sheet.set_cell(
            0,
            0,
            Cell {
                meta: Some(Box::new(loaded)),
                f_attrs: Some("t=\"array\" ref=\"A1:A3\"".into()),
                ..Cell::formula("SEQUENCE(3)")
            },
        );
        let typed = CellMeta {
            modern: true,
            dynamic: true,
            ..CellMeta::default()
        };
        sheet.set_cell(
            0,
            1,
            Cell {
                meta: Some(Box::new(typed.clone())),
                ..Cell::formula("SEQUENCE(2)")
            },
        );
        // Fill Down over A1:B3: A2:A3 copy the loaded cell, B2:B3 the typed one.
        let down = fill_changes(&sheet, (0, 0, 2, 1), true);
        assert_eq!(down.len(), 4);
        for (r, c, cell) in &down {
            let want = if *c == 0 { None } else { Some(&typed) };
            assert_eq!(cell.meta.as_deref(), want, "{r},{c}");
        }
        // Fill Right over A1:C1: B1:C1 copy the loaded cell.
        let right = fill_changes(&sheet, (0, 0, 0, 2), false);
        assert_eq!(right.len(), 2);
        assert!(right.iter().all(|(_, _, cell)| cell.meta.is_none()));
    }

    #[test]
    fn fill_changes_copies_down_and_right_translating_refs() {
        let mut sheet = Sheet::default();
        sheet.set_cell(
            0,
            1,
            Cell {
                style: 3,
                ..Cell::formula("A1*2")
            },
        );
        sheet.set_cell(0, 3, Cell::text("x"));
        let down = fill_changes(&sheet, (0, 1, 3, 1), true);
        assert_eq!(down.len(), 3);
        assert_eq!(down[2].0, 3);
        assert_eq!(down[2].2.formula.as_deref(), Some("A4*2"));
        assert_eq!(down[2].2.style, 3);
        let right = fill_changes(&sheet, (0, 3, 0, 5), false);
        assert_eq!(right.len(), 2);
        assert!(
            right
                .iter()
                .all(|(_, _, c)| c.value == CellValue::Text("x".into()))
        );
        // A single cell pulls from above; nothing above row 0.
        let one = fill_changes(&sheet, (1, 1, 1, 1), true);
        assert_eq!(one[0].2.formula.as_deref(), Some("A2*2"));
        assert!(fill_changes(&sheet, (0, 0, 0, 0), true).is_empty());
        // One row, several columns, Ctrl+D: each pulls from the row above.
        let row = fill_changes(&sheet, (1, 1, 1, 3), true);
        assert_eq!(row.len(), 3);
        assert_eq!(row[0].2.formula.as_deref(), Some("A2*2"));
        assert_eq!(row[2].2.value, CellValue::Text("x".into()));
        // One column, several rows, Ctrl+R: each pulls from the column left.
        let col = fill_changes(&sheet, (0, 2, 1, 2), false);
        assert_eq!(col.len(), 2);
        assert_eq!(col[0].2.formula.as_deref(), Some("B1*2"));
    }

    #[test]
    fn typed_numbers_keep_fifteen_digits_and_excel_limits() {
        // #655: Excel truncates a typed number past 15 significant digits and
        // keeps out-of-range numbers as text.
        let num = |t: &str| match parse_input(t).value {
            CellValue::Number(n) => n,
            v => panic!("{t} → {v:?}"),
        };
        assert_eq!(num("1234567890123456789"), 1234567890123450000.0);
        assert_eq!(num("1234567890123456"), 1234567890123450.0);
        assert_eq!(num("12345678901234567"), 12345678901234500.0);
        assert_eq!(num("-1234567890123456789"), -1234567890123450000.0);
        assert_eq!(num("0.1234567890123456789"), 0.123456789012345);
        assert_eq!(num("0.000123456789012345678"), 0.000123456789012345);
        assert_eq!(num("1.23456789012345678E+20"), 1.23456789012345e20);
        assert_eq!(num("12345678901234567%"), 123456789012345.0);
        assert_eq!(num("9.99999999999999E+307"), 9.99999999999999e307);
        assert_eq!(num("0"), 0.0);
        assert_eq!(num("0E+5"), 0.0);
        assert_eq!(num("  42 "), 42.0);
        for t in ["1E+308", "1E-400", "-1E+308", "1E-310"] {
            assert_eq!(parse_input(t).value, CellValue::Text(t.into()), "{t}");
        }
        // Not numbers at all: unchanged.
        assert_eq!(parse_input("inf").value, CellValue::Text("inf".into()));
        assert_eq!(parse_input("NaN").value, CellValue::Text("NaN".into()));
    }

    #[test]
    fn fill_copy_keeps_dynamic_mark() {
        // A copy of a typed dynamic array is one too; the source's file
        // indices (`cm`, `vm`) stay behind.
        let mut wb = Workbook::default();
        wb.sheets.push(Sheet::default());
        let mut cell = Cell::formula("SEQUENCE(1)");
        cell.meta = Some(Box::new(crate::sheet::CellMeta {
            cm: Some("1".into()),
            vm: Some(("2".into(), CellValue::Number(1.0))),
            modern: true,
            dynamic: true,
            ..Default::default()
        }));
        wb.sheets[0].set_cell(0, 0, cell);
        autofill(&mut wb, 0, (0, 0, 0, 0), (0, 1));
        let copy = wb.sheets[0].cell(0, 1).unwrap();
        assert_eq!(
            copy.meta.as_deref(),
            Some(&crate::sheet::CellMeta {
                modern: true,
                dynamic: true,
                ..Default::default()
            })
        );
        // A copy of a loaded formula is typed there (#785): modern, and
        // nothing else of the source's.
        let mut plain = Cell::formula("A1");
        plain.meta = Some(Box::new(crate::sheet::CellMeta {
            cm: Some("1".into()),
            ..Default::default()
        }));
        wb.sheets[0].set_cell(1, 0, plain);
        autofill(&mut wb, 0, (1, 0, 1, 0), (1, 1));
        assert_eq!(
            wb.sheets[0].cell(1, 1).unwrap().meta.as_deref(),
            Some(&crate::sheet::CellMeta {
                modern: true,
                ..Default::default()
            })
        );
    }

    /// Sheet1 with A{top}:A{top+2} = 1..3, a CSE array `A..:A..*2` anchored in
    /// column D on the same rows, and user data (99) just below the block —
    /// recalculated, so the anchor carries its real spill extent.
    fn cse_column_block(top: u32) -> Workbook {
        let (a, b) = (top + 1, top + 3);
        let mut w = wb(&[
            (&format!("A{a}"), Cell::number(1.0)),
            (&format!("A{}", a + 1), Cell::number(2.0)),
            (&format!("A{b}"), Cell::number(3.0)),
            (
                &format!("D{a}"),
                with_f_attrs(
                    &format!("A{a}:A{b}*2"),
                    &format!(" t=\"array\" ref=\"D{a}:D{b}\""),
                ),
            ),
            (&format!("D{}", b + 1), Cell::number(99.0)),
        ]);
        Engine::new(&w).recalc_all(&mut w);
        assert_eq!(w.sheets[0].cell(top, 3).unwrap().spill, Some((3, 1)));
        w
    }

    #[test]
    fn deleting_a_row_inside_a_spill_shrinks_it_and_keeps_user_data() {
        let mut w = cse_column_block(0);
        delete_rows(&mut w, 0, 1, 1);
        // Before any recalc: the stored extent already matches the block.
        assert_eq!(w.sheets[0].cell(0, 3).unwrap().spill, Some((2, 1)));
        Engine::new(&w).recalc_all(&mut w);
        assert_eq!(value_at(&w, "D1"), CellValue::Number(2.0));
        assert_eq!(value_at(&w, "D2"), CellValue::Number(6.0));
        // The user's 99 moved up into the old extent; the rebuild leaves it.
        assert_eq!(value_at(&w, "D3"), CellValue::Number(99.0));
    }

    #[test]
    fn inserting_a_row_inside_a_spill_grows_it_without_spill_error() {
        let mut w = cse_column_block(0);
        insert_rows(&mut w, 0, 1, 1);
        assert_eq!(w.sheets[0].cell(0, 3).unwrap().spill, Some((4, 1)));
        Engine::new(&w).recalc_all(&mut w);
        // Its own moved-down values are still its own: no #SPILL!.
        let d: Vec<_> = ["D1", "D2", "D3", "D4"]
            .iter()
            .map(|n| value_at(&w, n))
            .collect();
        assert_eq!(
            d,
            [2.0, 0.0, 4.0, 6.0].map(CellValue::Number).to_vec(),
            "{d:?}"
        );
        assert_eq!(w.sheets[0].cell(0, 3).unwrap().spill, Some((4, 1)));
        assert_eq!(value_at(&w, "D5"), CellValue::Number(99.0));
    }

    #[test]
    fn deleting_a_column_inside_a_dynamic_array_shrinks_its_spill() {
        let mut typed = Cell::formula("A1:C1*2");
        typed.meta = Some(Box::new(crate::sheet::CellMeta {
            modern: true,
            ..Default::default()
        }));
        let mut w = wb(&[
            ("A1", Cell::number(1.0)),
            ("B1", Cell::number(2.0)),
            ("C1", Cell::number(3.0)),
            ("A3", typed),
            ("D3", Cell::number(99.0)),
        ]);
        Engine::new(&w).recalc_all(&mut w);
        assert_eq!(w.sheets[0].cell(2, 0).unwrap().spill, Some((1, 3)));
        delete_cols(&mut w, 0, 1, 1);
        assert_eq!(w.sheets[0].cell(2, 0).unwrap().spill, Some((1, 2)));
        Engine::new(&w).recalc_all(&mut w);
        assert_eq!(value_at(&w, "A3"), CellValue::Number(2.0));
        assert_eq!(value_at(&w, "B3"), CellValue::Number(6.0));
        assert_eq!(value_at(&w, "C3"), CellValue::Number(99.0));
    }

    #[test]
    fn an_edit_outside_a_spill_keeps_its_extent() {
        // Block D2:D4 (anchor at row 1).
        let spill_at = |w: &Workbook, r: u32, c: u32| w.sheets[0].cell(r, c).unwrap().spill;
        let mut w = cse_column_block(1);
        delete_rows(&mut w, 0, 0, 1); // above: the anchor moves to D1
        assert_eq!(spill_at(&w, 0, 3), Some((3, 1)));
        let mut w = cse_column_block(1);
        insert_rows(&mut w, 0, 4, 1); // just past its last row
        assert_eq!(spill_at(&w, 1, 3), Some((3, 1)));
        let mut w = cse_column_block(1);
        insert_cols(&mut w, 0, 0, 1); // left: the anchor moves to E2
        assert_eq!(spill_at(&w, 1, 4), Some((3, 1)));
        // A row edit on another sheet never touches this one.
        let mut w = cse_column_block(1);
        w.sheets.push(Sheet {
            name: "Sheet2".to_string(),
            ..Sheet::default()
        });
        delete_rows(&mut w, 1, 2, 1);
        assert_eq!(spill_at(&w, 1, 3), Some((3, 1)));
    }

    #[test]
    fn a_structural_edit_resizes_a_2d_spill_on_its_own_axis_only() {
        let block = || {
            let mut cells = Vec::new();
            for r in 1..=3 {
                cells.push((format!("A{r}"), Cell::number(f64::from(r))));
                cells.push((format!("B{r}"), Cell::number(f64::from(r * 10))));
            }
            cells.push((
                "D1".to_string(),
                with_f_attrs("A1:B3*2", " t=\"array\" ref=\"D1:E3\""),
            ));
            let cells: Vec<(&str, Cell)> =
                cells.iter().map(|(n, c)| (n.as_str(), c.clone())).collect();
            let mut w = wb(&cells);
            Engine::new(&w).recalc_all(&mut w);
            assert_eq!(w.sheets[0].cell(0, 3).unwrap().spill, Some((3, 2)));
            w
        };
        let mut w = block();
        delete_rows(&mut w, 0, 1, 1); // a row inside: h shrinks, w stays
        assert_eq!(w.sheets[0].cell(0, 3).unwrap().spill, Some((2, 2)));
        let mut w = block();
        delete_cols(&mut w, 0, 4, 1); // column E: w shrinks, h stays
        assert_eq!(w.sheets[0].cell(0, 3).unwrap().spill, Some((3, 1)));
        let mut w = block();
        insert_cols(&mut w, 0, 4, 1); // between D and E: w grows, h stays
        assert_eq!(w.sheets[0].cell(0, 3).unwrap().spill, Some((3, 3)));
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;
    use crate::engine::Engine;
    use crate::sheet::{Cell, DefinedName, Table, parse_cell_name};

    /// Sheet1 holds `Sales` over A1:C4 (Item, Qty, Dbl; Dbl = [@Qty]*2); a
    /// second sheet, `My Data`, holds nothing yet.
    fn sales() -> Workbook {
        let mut s1 = Sheet {
            name: "Sheet1".into(),
            ..Sheet::default()
        };
        s1.set_cell(0, 0, Cell::text("Item"));
        s1.set_cell(0, 1, Cell::text("Qty"));
        s1.set_cell(0, 2, Cell::text("Dbl"));
        for (r, (item, qty)) in [("Pen", 3.0), ("Ink", 4.0), ("Pad", 5.0)]
            .into_iter()
            .enumerate()
        {
            let r = r as u32 + 1;
            s1.set_cell(r, 0, Cell::text(item));
            s1.set_cell(r, 1, Cell::number(qty));
            s1.set_cell(r, 2, Cell::formula("[@Qty]*2"));
        }
        let s2 = Sheet {
            name: "My Data".into(),
            ..Sheet::default()
        };
        let mut wb = Workbook {
            sheets: vec![s1, s2],
            ..Workbook::default()
        };
        wb.tables.push(Table {
            name: "Sales".into(),
            sheet: 0,
            range: (0, 0, 3, 2),
            header_rows: 1,
            totals_rows: 0,
            columns: vec!["Item".into(), "Qty".into(), "Dbl".into()],
            part: "xl/tables/table1.xml".into(),
        });
        wb
    }

    fn pivot(
        name: &str,
        sheet: usize,
        location: (u32, u32, u32, u32),
        source: &str,
    ) -> crate::pivot::Pivot {
        crate::pivot::Pivot {
            name: name.into(),
            sheet,
            location,
            source: crate::pivot::PivotSource::Table(source.into()),
            fields: Vec::new(),
            row_fields: Vec::new(),
            col_fields: Vec::new(),
            data_fields: Vec::new(),
            field_items: Vec::new(),
            hidden: Vec::new(),
            page: Vec::new(),
            items_order: Vec::new(),
            calc_formulas: Vec::new(),
            grand_rows: true,
            grand_cols: true,
            subtotals: false,
            data_on_rows: false,
            unsupported: false,
            edited: false,
            part: String::new(),
            cache_part: String::new(),
        }
    }

    fn put(wb: &mut Workbook, sheet: usize, at: &str, src: &str) {
        let (r, c) = parse_cell_name(at).unwrap();
        wb.sheets[sheet].set_cell(r, c, Cell::formula(src));
    }

    fn formula(wb: &Workbook, sheet: usize, at: &str) -> String {
        let (r, c) = parse_cell_name(at).unwrap();
        wb.sheets[sheet]
            .cell(r, c)
            .unwrap()
            .formula
            .clone()
            .unwrap()
    }

    fn value(wb: &mut Workbook, sheet: usize, at: &str) -> CellValue {
        Engine::new(wb).recalc_all(wb);
        let (r, c) = parse_cell_name(at).unwrap();
        wb.sheets[sheet].cell(r, c).unwrap().value.clone()
    }

    fn other_table(name: &str, sheet: usize, range: (u32, u32, u32, u32)) -> Table {
        Table {
            name: name.into(),
            sheet,
            range,
            header_rows: 1,
            totals_rows: 0,
            columns: (range.1..=range.3).map(|c| format!("C{c}")).collect(),
            part: format!("xl/tables/{name}.xml"),
        }
    }

    #[test]
    fn rename_table_rewrites_every_formula_that_names_it() {
        let mut wb = sales();
        put(&mut wb, 0, "E1", "SUM(Sales[Qty])");
        put(&mut wb, 1, "A1", "ROWS(sales)+SUM(Sales[[#All],[Qty]])");
        put(&mut wb, 0, "E2", "SUMX(Sales,[@Qty])");
        wb.defined_names.push(DefinedName {
            name: "All".into(),
            scope: None,
            formula: "Sales[#All]".into(),
        });
        assert_eq!(value(&mut wb, 0, "E1"), CellValue::Number(12.0));
        rename_table(&mut wb, "Sales", "Revenue").unwrap();
        assert_eq!(wb.tables[0].name, "Revenue");
        assert_eq!(formula(&wb, 0, "E1"), "SUM(Revenue[Qty])");
        assert_eq!(
            formula(&wb, 1, "A1"),
            "ROWS(Revenue)+SUM(Revenue[[#All],[Qty]])"
        );
        assert_eq!(formula(&wb, 0, "E2"), "SUMX(Revenue,[@Qty])");
        // Unqualified references name no table.
        assert_eq!(formula(&wb, 0, "C2"), "[@Qty]*2");
        assert_eq!(wb.defined_names[0].formula, "Revenue[#All]");
        assert_eq!(value(&mut wb, 0, "E1"), CellValue::Number(12.0));
        assert_eq!(value(&mut wb, 0, "E2"), CellValue::Number(12.0));
        assert_eq!(value(&mut wb, 0, "C3"), CellValue::Number(8.0));
    }

    #[test]
    fn rename_table_follows_excel_name_rules() {
        let mut wb = sales();
        wb.tables.push(other_table("Other", 1, (0, 0, 1, 0)));
        wb.defined_names.push(DefinedName {
            name: "Rate".into(),
            scope: Some(1),
            formula: "0.2".into(),
        });
        for bad in ["", "1x", "A1", "R1C1", "r", "Two words", "other", "RATE"] {
            assert!(rename_table(&mut wb, "Sales", bad).is_err(), "{bad:?}");
        }
        assert_eq!(wb.tables[0].name, "Sales");
        assert!(rename_table(&mut wb, "Nope", "X").is_err());
        // Another case of its own name is a rename.
        put(&mut wb, 0, "E1", "SUM(Sales[Qty])");
        rename_table(&mut wb, "Sales", "SALES").unwrap();
        assert_eq!(wb.tables[0].name, "SALES");
        assert_eq!(formula(&wb, 0, "E1"), "SUM(SALES[Qty])");
    }

    #[test]
    fn rename_table_reaches_rules_and_pivots() {
        let mut wb = sales();
        wb.sheets[0].validations.push(crate::sheet::DataValidation {
            formula1: "Sales[Item]".into(),
            ..Default::default()
        });
        wb.pivots.push(pivot("P", 0, (9, 9, 9, 9), "SALES"));
        rename_table(&mut wb, "Sales", "Revenue").unwrap();
        assert_eq!(wb.sheets[0].validations[0].formula1, "Revenue[Item]");
        assert_eq!(
            wb.pivots[0].source,
            crate::pivot::PivotSource::Table("Revenue".into())
        );
    }

    #[test]
    fn convert_to_range_writes_cell_references() {
        let mut wb = sales();
        put(&mut wb, 0, "E1", "SUM(Sales[Qty])");
        put(&mut wb, 0, "E2", "ROWS(Sales[#All])+ROWS(Sales[#Headers])");
        put(&mut wb, 0, "E3", "SUM(Sales[[Qty]:[Dbl]])");
        put(&mut wb, 1, "A1", "SUM(Sales[Qty])");
        put(&mut wb, 1, "A2", "IFERROR(SUM(Sales[Nope]),-1)");
        put(&mut wb, 1, "A3", "ROWS(Sales)");
        wb.defined_names.push(DefinedName {
            name: "Items".into(),
            scope: None,
            formula: "Sales[Item]".into(),
        });
        let before = value(&mut wb, 0, "E3");
        convert_table_to_range(&mut wb, "sales").unwrap();
        assert!(wb.tables.is_empty());
        assert_eq!(wb.removed_tables.len(), 1);
        assert_eq!(formula(&wb, 0, "E1"), "SUM($B$2:$B$4)");
        assert_eq!(formula(&wb, 0, "E2"), "ROWS($A$1:$C$4)+ROWS($A$1:$C$1)");
        assert_eq!(formula(&wb, 0, "E3"), "SUM($B$2:$C$4)");
        // `[@Qty]` inside the table: absolute column, its own row.
        assert_eq!(formula(&wb, 0, "C2"), "$B2*2");
        assert_eq!(formula(&wb, 0, "C4"), "$B4*2");
        assert_eq!(formula(&wb, 1, "A1"), "SUM(Sheet1!$B$2:$B$4)");
        assert_eq!(formula(&wb, 1, "A2"), "IFERROR(SUM(#REF!),-1)");
        assert_eq!(formula(&wb, 1, "A3"), "ROWS(Sheet1!$A$2:$C$4)");
        assert_eq!(wb.defined_names[0].formula, "Sheet1!$A$2:$A$4");
        assert_eq!(value(&mut wb, 0, "E1"), CellValue::Number(12.0));
        assert_eq!(value(&mut wb, 0, "E3"), before);
        assert_eq!(value(&mut wb, 0, "C3"), CellValue::Number(8.0));
        assert_eq!(value(&mut wb, 1, "A3"), CellValue::Number(3.0));
    }

    #[test]
    fn convert_to_range_quotes_the_sheet_name() {
        let mut wb = sales();
        wb.sheets[0].name = "Q1 Data".into();
        put(&mut wb, 1, "A1", "SUM(Sales[Qty])");
        convert_table_to_range(&mut wb, "Sales").unwrap();
        assert_eq!(formula(&wb, 1, "A1"), "SUM('Q1 Data'!$B$2:$B$4)");
    }

    #[test]
    fn convert_to_range_refuses_what_has_no_range_form() {
        let mut wb = sales();
        put(&mut wb, 0, "E1", "SUMX(Sales,[@Qty])");
        let err = convert_table_to_range(&mut wb, "Sales").unwrap_err();
        assert!(err.contains("iterates"), "{err}");
        assert_eq!(wb.tables.len(), 1);

        let mut wb = sales();
        wb.pivots
            .push(pivot("PivotTable1", 1, (9, 9, 9, 9), "Sales"));
        let err = convert_table_to_range(&mut wb, "Sales").unwrap_err();
        assert_eq!(err, "PivotTable PivotTable1 uses this table");
        assert_eq!(wb.tables.len(), 1);
        assert_eq!(formula(&wb, 0, "C2"), "[@Qty]*2");

        // Names and rules are reached by the rewrite too.
        let mut wb = sales();
        wb.defined_names.push(DefinedName {
            name: "Total".into(),
            scope: None,
            formula: "SUMX(Sales,[@Qty])".into(),
        });
        let err = convert_table_to_range(&mut wb, "Sales").unwrap_err();
        assert_eq!(err, "The name Total iterates this table");
        let mut wb = sales();
        wb.sheets[1].validations.push(crate::sheet::DataValidation {
            formula1: "SUMX(Sales,[@Qty])>3".into(),
            ..Default::default()
        });
        let err = convert_table_to_range(&mut wb, "Sales").unwrap_err();
        assert_eq!(err, "A data validation rule on My Data iterates this table");
        assert_eq!(wb.tables.len(), 1);
    }

    #[test]
    fn resize_table_refuses_to_move_a_totals_row() {
        let mut wb = sales();
        wb.tables[0].totals_rows = 1;
        for r2 in [2, 5] {
            let err = resize_table(&mut wb, "Sales", (0, 0, r2, 2)).unwrap_err();
            assert_eq!(err, "Turn off the Total Row first");
        }
        // Columns may still change: the totals row stays the last row.
        resize_table(&mut wb, "Sales", (0, 0, 3, 3)).unwrap();
        assert_eq!(wb.tables[0].range, (0, 0, 3, 3));
    }

    #[test]
    fn new_column_names_are_unique_ignoring_case() {
        let taken = vec!["Qty".to_string(), "Column2".to_string()];
        let text = CellValue::Text("qty".into());
        assert_eq!(table_column_name(Some(&text), 1, &taken), "qty2");
        assert_eq!(table_column_name(None, 2, &taken), "Column22");
        assert_eq!(
            table_column_name(Some(&CellValue::Number(2024.0)), 3, &taken),
            "2024"
        );
    }

    #[test]
    fn resize_table_keeps_names_and_names_new_columns() {
        let mut wb = sales();
        // E1 holds a header that clashes; D1 is blank.
        wb.sheets[0].set_cell(0, 4, Cell::text("Qty"));
        resize_table(&mut wb, "Sales", (0, 0, 5, 4)).unwrap();
        let t = &wb.tables[0];
        assert_eq!(t.range, (0, 0, 5, 4));
        assert_eq!(t.columns, vec!["Item", "Qty", "Dbl", "Column4", "Qty2"]);
        let header = |c| wb.sheets[0].cell(0, c).map(|cl| cl.value.clone());
        assert_eq!(header(3), Some(CellValue::Text("Column4".into())));
        assert_eq!(header(4), Some(CellValue::Text("Qty2".into())));
        // Shrinking drops a column.
        resize_table(&mut wb, "Sales", (0, 1, 2, 2)).unwrap();
        assert_eq!(wb.tables[0].columns, vec!["Qty", "Dbl"]);
    }

    #[test]
    fn resize_table_refuses_what_excel_refuses() {
        let mut wb = sales();
        let err = |wb: &mut Workbook, r| resize_table(wb, "Sales", r).unwrap_err();
        assert!(err(&mut wb, (9, 3, 11, 4)).contains("header row"));
        assert!(err(&mut wb, (1, 0, 3, 2)).contains("header row"));
        assert!(err(&mut wb, (0, 4, 3, 5)).contains("overlap"));
        assert!(err(&mut wb, (0, 0, 0, 2)).contains("data row"));
        wb.tables.push(other_table("Next", 0, (0, 4, 2, 4)));
        assert_eq!(err(&mut wb, (0, 0, 3, 4)), "The range overlaps table Next");
        assert_eq!(wb.tables[0].range, (0, 0, 3, 2));
    }

    #[test]
    fn table_range_conflicts() {
        let mut wb = sales();
        assert_eq!(
            table_range_conflict(&wb, 0, (2, 2, 5, 5), None).as_deref(),
            Some("The range overlaps table Sales")
        );
        assert_eq!(table_range_conflict(&wb, 0, (2, 2, 5, 5), Some(0)), None);
        assert_eq!(table_range_conflict(&wb, 1, (0, 0, 5, 5), None), None);
        wb.pivots.push(pivot("P", 1, (3, 3, 6, 4), "Elsewhere"));
        assert_eq!(
            table_range_conflict(&wb, 1, (0, 0, 3, 3), None).as_deref(),
            Some("The range overlaps PivotTable P")
        );
        // A two-cell CSE block at G1:G2.
        let mut arr = Cell::formula("A1:A2");
        arr.f_attrs = Some("t=\"array\" ref=\"G1:G2\"".into());
        arr.spill = Some((2, 1));
        wb.sheets[1].set_cell(0, 6, arr);
        assert_eq!(
            table_range_conflict(&wb, 1, (1, 5, 4, 6), None).as_deref(),
            Some("The range contains part of the array formula at G1")
        );
        assert_eq!(table_range_conflict(&wb, 1, (0, 7, 4, 8), None), None);
    }
}
