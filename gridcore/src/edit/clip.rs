//! The sheet clipboard's paste rules, as Excel has them: a copy tiles over a
//! paste area that is a whole number of copies of it, each tile's formulas
//! translated to where they land; a cut is a move, and the references to
//! the moved cells follow them ([`move_refs`]).

use super::{Area, FormulaSite, rewrite_workbook_formulas, walk_workbook_formulas};
use crate::formula::{
    CellMove, Expr, move_abs_ref_expr, move_block_expr, move_ref_expr, parse, qualify_bare_refs,
    qualify_local_names, to_string, translate_formula,
};
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
fn translated_block(
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
/// sheet, conditional-format rules, and defined names. A reference to a
/// moved cell (a range: both corners) follows it ([`move_ref_expr`]). The
/// moved cells' own formulas are left to the caller
/// ([`crate::formula::move_block_formula`]), and validation rules to
/// [`crate::validation::cut_rules`], called first: a rule's cells may lie
/// partly inside the cut, so its formula, written once for its anchor, is
/// rewritten part by part (#1050). A formula the move doesn't reach keeps
/// its text exactly, and a shared formula held verbatim is left alone, as an
/// insert or delete leaves it. Whether any formula was rewritten.
pub fn move_refs(wb: &mut Workbook, src: usize, mv: &CellMove) -> bool {
    move_refs_with(wb, src, mv, |_, _| true, false)
}

/// [`move_refs`] for a host whose cut leaves the conditional-format rules'
/// ranges where they are: a rule over cells of `mv.rect` keeps its
/// relative and mixed references, so (its formula being relative to its
/// range) it keeps checking what it checked, as the data form's Delete
/// leaves such rules; only its fully absolute references to moved cells
/// (`$F$1`) follow them ([`move_abs_ref_expr`]), as they name the cells
/// themselves. Every other rule is re-pointed whole. Whether any formula
/// was rewritten.
pub fn move_refs_rules_stay(wb: &mut Workbook, src: usize, mv: &CellMove) -> bool {
    walk_rules_stay(wb, src, mv, true)
}

/// Whether [`move_refs_rules_stay`] would rewrite any formula, leaving the
/// workbook as it is: a host skips the whole-workbook undo snapshot for a cut
/// that re-points nothing outside its own cells. It walks the formulas the
/// rewrite walks, with the same rewrite, so the two always agree; it takes
/// the workbook mutably only for that shared walk and writes nothing.
pub fn cut_moves_refs(wb: &mut Workbook, src: usize, mv: &CellMove) -> bool {
    walk_rules_stay(wb, src, mv, false)
}

/// [`move_refs_rules_stay`], or with `write` false its dry run.
fn walk_rules_stay(wb: &mut Workbook, src: usize, mv: &CellMove, write: bool) -> bool {
    let (r0, c0, r1, c1) = mv.rect;
    let meets = |a: &Area| a.0 <= r1 && r0 <= a.2 && a.1 <= c1 && c0 <= a.3;
    let names: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    walk_workbook_formulas(
        wb,
        |_, _| true,
        false,
        |e, site| {
            let left = site.sheet == Some(src) && site.rule.is_some_and(|r| r.iter().any(meets));
            if left {
                return move_abs_ref_expr(e, Some(&names[src]), mv);
            }
            moved_ref_expr(e, &names, src, mv, site)
        },
        write,
    )
}

/// Rewrite every formula in the workbook for the move `mv` of the cells in
/// `mv.rect` on sheet `src`, as [`move_refs`] does, but of the
/// conditional-format rules (and, with `validations`, the validation rules)
/// only those `rules` takes, given the rule's sheet and ranges: a move that
/// leaves some rules' ranges in place (the data form's Delete) leaves their
/// formulas too, so each keeps reading relative to its own range.
pub(super) fn move_refs_with(
    wb: &mut Workbook,
    src: usize,
    mv: &CellMove,
    rules: impl Fn(usize, &[Area]) -> bool,
    validations: bool,
) -> bool {
    let names: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    rewrite_workbook_formulas(wb, rules, validations, |e, site| {
        moved_ref_expr(e, &names, src, mv, site)
    })
}

/// A formula at `site` rewritten for the move `mv` off sheet `src`
/// ([`move_ref_expr`]); a moved cell's own formula is left to the caller
/// ([`moved_cell_formula`]). `names` are the sheets' names.
fn moved_ref_expr(
    e: &Expr,
    names: &[String],
    src: usize,
    mv: &CellMove,
    site: FormulaSite<'_>,
) -> Expr {
    let (r0, c0, r1, c1) = mv.rect;
    let (sheet, cell) = (site.sheet, site.cell);
    let moved = sheet == Some(src)
        && cell.is_some_and(|(r, c)| (r0..=r1).contains(&r) && (c0..=c1).contains(&c));
    if moved {
        return e.clone();
    }
    move_ref_expr(e, sheet.map(|s| names[s].as_str()), mv)
}

/// The formula `f` of a cell the cut `mv` moves from `from` on sheet `src` to
/// `to` on sheet `dst`, as Excel writes it there: references to cells of the
/// cut follow them and the rest keep reading what they read
/// ([`move_block_expr`]); moved to another sheet, the names it read in
/// `src`'s own scope are qualified with it (`Sheet2!Rate`,
/// [`qualify_local_names`]); and moved out of the table it sat in, its bare
/// structured references name that table (`Table1[@Qty]`,
/// [`qualify_bare_refs`]). Read on the workbook as it was before the cut.
/// `None` when it doesn't parse or nothing changes, so the text then stays
/// byte-for-byte (#1687).
pub fn moved_cell_formula(
    wb: &Workbook,
    (src, from): (usize, (u32, u32)),
    (dst, to): (usize, (u32, u32)),
    f: &str,
    mv: &CellMove,
) -> Option<String> {
    let ast = parse(f).ok()?;
    let mut out = move_block_expr(&ast, mv);
    if src != dst {
        if let Some(sheet) = wb.sheets.get(src) {
            let local = |n: &str| wb.local_name(src, n).map(str::to_string);
            out = qualify_local_names(&out, &sheet.name, &local);
        }
    }
    if let Some(t) = wb.table_at(src, from.0, from.1) {
        let stays = wb
            .table_at(dst, to.0, to.1)
            .is_some_and(|u| crate::names::name_eq(&u.name, &t.name));
        if !stays {
            out = qualify_bare_refs(&out, &t.name);
        }
    }
    (out != ast).then(|| to_string(&out))
}

#[cfg(test)]
mod tests;
