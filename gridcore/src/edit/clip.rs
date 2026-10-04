//! The sheet clipboard's paste rules, as Excel has them: a copy tiles over a
//! paste area that is a whole number of copies of it, each tile's formulas
//! translated to where they land; a cut is a move, and the references to
//! the moved cells follow them ([`move_refs`]).

use super::rewrite_workbook_formulas;
use crate::formula::{CellMove, move_ref_expr, translate_formula};
use crate::sheet::{Cell, MAX_COLS, MAX_ROWS, Workbook};

/// The most cells a tiled paste writes; a bigger one is refused whole (the
/// cap xlsxy and gridwasm put on a text paste). A copy pasted once is not
/// capped: it writes no more than it copied.
pub const MAX_PASTE_CELLS: u64 = 100_000;

/// Excel's refusal of a paste area that is not a whole number of copies.
pub const PASTE_SHAPE: &str = "The information cannot be pasted because the Copy area and the \
                               paste area aren't the same size and shape.";

/// How many copies of a `copy` (rows, cols) block tile the selection `sel`
/// (r0, c0, r1, c1), down and across. Per axis: a selection one cell deep
/// takes one copy; one a whole number of copies deep takes that many; any
/// other is refused (`None`). A selection that spans the whole sheet on an
/// axis (a whole column or row) only reaches the sheet's `used` (rows, cols)
/// extent from where it starts, and at least one copy, rounded up to a whole
/// copy, so a copy pasted on column D fills D's used rows rather than a
/// million.
pub fn paste_tiles(
    copy: (u32, u32),
    sel: (u32, u32, u32, u32),
    used: (u32, u32),
) -> Option<(u32, u32)> {
    let (r0, c0, r1, c1) = sel;
    let axis = |len: u32, start: u32, end: u32, whole: bool, used: u32| {
        if len == 0 {
            return None;
        }
        let span = if whole {
            let reach = used.saturating_sub(start).max(len);
            reach.div_ceil(len) * len
        } else {
            end - start + 1
        };
        if span == 1 {
            Some(1)
        } else if span % len == 0 {
            Some(span / len)
        } else {
            None
        }
    };
    let whole_rows = r0 == 0 && r1 == MAX_ROWS - 1;
    let whole_cols = c0 == 0 && c1 == MAX_COLS - 1;
    Some((
        axis(copy.0, r0, r1, whole_rows, used.0)?,
        axis(copy.1, c0, c1, whole_cols, used.1)?,
    ))
}

/// The copied `cells` written at `at`, each formula translated from where it
/// was copied to where it lands: cell `(i, j)` came from sheet row
/// `src_rows[i]` and column `src_cols[j]` (a filtered copy skips rows and a
/// multi-area copy skips rows or columns, so each has its own offset). A
/// formula that lands where it came from, or that doesn't parse, keeps its
/// text as it was.
pub fn translated_block(
    cells: &[Vec<Cell>],
    src_rows: &[u32],
    src_cols: &[u32],
    at: (u32, u32),
) -> Vec<Vec<Cell>> {
    cells
        .iter()
        .zip(src_rows)
        .enumerate()
        .map(|(i, (row, &src_row))| {
            let dr = at.0 as i64 + i as i64 - src_row as i64;
            row.iter()
                .zip(src_cols)
                .enumerate()
                .map(|(j, (cell, &src_col))| {
                    let dc = at.1 as i64 + j as i64 - src_col as i64;
                    let mut cell = cell.clone();
                    if (dr, dc) != (0, 0) {
                        if let Some(t) = cell
                            .formula
                            .as_deref()
                            .and_then(|f| translate_formula(f, dr, dc))
                        {
                            cell.formula = Some(t);
                        }
                    }
                    cell
                })
                .collect()
        })
        .collect()
}

/// `tiles` (down, across) copies of the copied `cells` from `at`, each
/// translated to its own corner ([`translated_block`]), as one block; rows
/// and cells pushed off the grid's edge are dropped.
pub fn tiled_block(
    cells: &[Vec<Cell>],
    src_rows: &[u32],
    src_cols: &[u32],
    at: (u32, u32),
    tiles: (u32, u32),
) -> Vec<Vec<Cell>> {
    let (h, w) = (
        cells.len() as u32,
        cells.iter().map(Vec::len).max().unwrap_or(0) as u32,
    );
    let mut block: Vec<Vec<Cell>> = Vec::new();
    for ty in 0..tiles.0 {
        let r = at.0 as u64 + (ty * h) as u64;
        if r >= MAX_ROWS as u64 {
            break;
        }
        let mut band: Vec<Vec<Cell>> = vec![Vec::new(); h as usize];
        for tx in 0..tiles.1 {
            let c = at.1 as u64 + (tx * w) as u64;
            if c >= MAX_COLS as u64 {
                break;
            }
            let tile = translated_block(cells, src_rows, src_cols, (r as u32, c as u32));
            for (row, tile_row) in band.iter_mut().zip(tile) {
                // A short row is padded, so the next tile lands in its column.
                let mut tile_row = tile_row;
                tile_row.resize(w as usize, Cell::default());
                row.extend(tile_row);
            }
        }
        block.extend(band);
    }
    block.truncate(MAX_ROWS.saturating_sub(at.0) as usize);
    let room = MAX_COLS.saturating_sub(at.1) as usize;
    for row in &mut block {
        row.truncate(room);
    }
    block
}

/// Rewrite every formula in the workbook for the cut-and-paste `mv` of the
/// cells in `mv.rect` on sheet `src` to sheet `dst`: cell formulas on every
/// sheet, conditional-format and validation rules, and defined names. A
/// reference to a moved cell (a range: both corners) follows it
/// ([`move_ref_expr`]). The moved cells' own formulas are left to the
/// caller ([`crate::formula::move_block_formula`]); a formula the move
/// doesn't reach keeps its text exactly, and a shared formula held verbatim
/// is left alone, as an insert or delete leaves it.
pub fn move_refs(wb: &mut Workbook, src: usize, mv: &CellMove) {
    let names: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    let (r0, c0, r1, c1) = mv.rect;
    rewrite_workbook_formulas(wb, |e, (sheet, cell)| {
        let moved = sheet == Some(src)
            && cell.is_some_and(|(r, c)| (r0..=r1).contains(&r) && (c0..=c1).contains(&c));
        if moved {
            return e.clone();
        }
        move_ref_expr(e, sheet.map(|s| names[s].as_str()), mv)
    });
}

#[cfg(test)]
mod tests;
