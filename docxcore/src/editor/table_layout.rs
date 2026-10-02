//! The table Layout tab's commands (#647): Select, Rows & Columns, Delete,
//! Merge/Split, Cell Size, Alignment, Text Direction, Sort and Convert.
//!
//! Each command works on [`Editor::table_selection`] in the innermost table,
//! returns a refusal reason instead of changing anything when it cannot run,
//! and is otherwise one undo step.

use super::tables::{CellRange, table_at_mut};
use super::{Caret, EditKind, Editor, container_mut, resolve_para};
use crate::model::{Align, Block, Inline, Paragraph, Run, RunProps, Table, TableRowBoundaryKind};
use crate::model::{Row, VMerge};
use crate::table::{
    AutoFit, GridMap, edit_cell_props, edit_table_props, grid_xs, new_table, normalize_vmerge,
    refine_grid, repair_grid, set_row_skips, template_cell, template_row,
};
use crate::table_props::{VAlign, row_trpr, set_row_trpr, width_of};

/// Delete Cells... choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteShift {
    ShiftLeft,
    ShiftUp,
    EntireRow,
    EntireColumn,
}

/// Cell Size > AutoFit choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoFitKind {
    Contents,
    Window,
    Fixed,
}

/// Sort... key types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKind {
    #[default]
    Text,
    Number,
    Date,
}

/// One Sort... key: a grid column, how to read it, and the direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortKey {
    pub col: usize,
    pub kind: SortKind,
    pub descending: bool,
}

/// The Sort... dialog's choices.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SortSpec {
    /// The first row is a header and stays in place.
    pub header: bool,
    /// Up to three keys, most significant first.
    pub keys: Vec<SortKey>,
}

/// What separates cells in Convert to Text / Convert Text to Table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellSep {
    Tab,
    Paragraph,
    Char(char),
}

/// Where the caret goes after a table edit.
pub(super) enum After {
    /// Keep the caret and selection (the cells did not move).
    Stay,
    /// The start of cell `(row, cell)`.
    Cell(usize, usize),
}

impl Editor {
    /// Run `edit` on a copy of the table at `path`. A refusal changes nothing
    /// and records no undo step; an edit that changes nothing records none
    /// either.
    pub(super) fn edit_table(
        &mut self,
        path: &[usize],
        edit: impl FnOnce(&mut Table) -> Result<After, String>,
    ) -> Result<(), String> {
        let Some(original) = self.table(path) else {
            return Err("the caret is not in a table".into());
        };
        let mut t = original.clone();
        let after = edit(&mut t)?;
        if Some(&t) == self.table(path) {
            return Ok(());
        }
        self.checkpoint(EditKind::Structural);
        if let Some(slot) = table_at_mut(&mut self.doc.body, path) {
            *slot = t;
        }
        match after {
            After::Stay => {}
            After::Cell(r, c) => {
                self.anchor = None;
                self.caret = self.cell_start(path, r, c);
            }
        }
        self.clamp();
        self.last = EditKind::Structural;
        Ok(())
    }

    /// The caret position at the start of a cell's first paragraph.
    fn cell_start(&self, table: &[usize], row: usize, cell: usize) -> Caret {
        let mut p = table.to_vec();
        p.extend([row, cell]);
        let first = self
            .table(table)
            .and_then(|t| t.rows.get(row)?.cells.get(cell))
            .and_then(|c| super::all_paragraph_paths(&c.blocks).into_iter().next())
            .unwrap_or_else(|| vec![0]);
        p.extend(first);
        Caret::at(p, 0)
    }

    /// Select from the start of cell `a` to the end of cell `b`.
    fn select_cells(&mut self, table: &[usize], a: (usize, usize), b: (usize, usize)) {
        if a == b {
            self.select_cell_content(table, a.0, a.1);
            return;
        }
        let start = self.cell_start(table, a.0, a.1);
        let mut end = self.cell_start(table, b.0, b.1);
        if let Some(c) = self
            .table(table)
            .and_then(|t| t.rows.get(b.0)?.cells.get(b.1))
        {
            let paths = super::all_paragraph_paths(&c.blocks);
            if let Some(last) = paths.last() {
                let mut p = table.to_vec();
                p.extend([b.0, b.1]);
                p.extend(last.iter().copied());
                let len = resolve_para(&self.doc.body, &p)
                    .map(super::para_text_len)
                    .unwrap_or(0);
                end = Caret::at(p, len);
            }
        }
        self.anchor = Some(start);
        self.caret = end;
        self.last = EditKind::None;
    }

    /// The caret's row when the caret is in the range's own table; the
    /// range's top row when it is in a table nested inside it.
    fn caret_row_in(&self, r: &CellRange) -> usize {
        match self.table_at_caret() {
            Some(p) if p.table == r.table => p.row,
            _ => r.top,
        }
    }

    fn need_selection(&self) -> Result<CellRange, String> {
        self.table_selection()
            .ok_or_else(|| "the caret is not in a table".to_string())
    }

    // ---- Select ----

    /// Layout > Select > Cell: the caret's cell.
    pub fn select_cell(&mut self) -> Result<(), String> {
        let pos = self.table_at_caret().ok_or("the caret is not in a table")?;
        self.select_cell_content(&pos.table, pos.row, pos.cell);
        Ok(())
    }

    /// Layout > Select > Row: every row the selection touches.
    pub fn select_row(&mut self) -> Result<(), String> {
        let r = self.need_selection()?;
        let t = self.table(&r.table).ok_or("no table")?;
        let last = t.rows[r.bottom].cells.len().saturating_sub(1);
        self.select_cells(&r.table.clone(), (r.top, 0), (r.bottom, last));
        Ok(())
    }

    /// Layout > Select > Column: every column the selection touches.
    pub fn select_column(&mut self) -> Result<(), String> {
        let r = self.need_selection()?;
        let t = self.table(&r.table).ok_or("no table")?;
        let map = GridMap::of(t);
        let first = (0..t.rows.len())
            .find_map(|row| map.rows[row].cell_at(r.left).map(|c| (row, c)))
            .ok_or("no cell in that column")?;
        let last = (0..t.rows.len())
            .rev()
            .find_map(|row| map.rows[row].cell_at(r.right).map(|c| (row, c)))
            .ok_or("no cell in that column")?;
        self.select_cells(&r.table.clone(), first, last);
        Ok(())
    }

    /// Layout > Select > Table.
    pub fn select_table(&mut self) -> Result<(), String> {
        let table = self.need_selection()?.table;
        let t = self.table(&table).ok_or("no table")?;
        let last_row = t.rows.len() - 1;
        let last_cell = t.rows[last_row].cells.len().saturating_sub(1);
        self.select_cells(&table, (0, 0), (last_row, last_cell));
        Ok(())
    }

    // ---- Rows & Columns ----

    /// Insert Above/Below: one new row per selected row, formatted like the
    /// selection's first (above) or last (below) row.
    pub fn insert_rows(&mut self, above: bool) -> Result<(), String> {
        let r = self.need_selection()?;
        let n = r.bottom - r.top + 1;
        let (left, path) = (r.left, r.table.clone());
        self.edit_table(&path, |t| {
            let map = GridMap::of(t);
            let tmpl = if above { r.top } else { r.bottom };
            let at = if above { r.top } else { r.bottom + 1 };
            let mut row = template_row(&t.rows[tmpl]);
            // A new row inside a vertical merge continues it.
            if at > 0 && at < t.rows.len() {
                let (up, down) = (&map.rows[at - 1], &map.rows[at]);
                for (c, &(s, _)) in map.rows[tmpl].cells.iter().enumerate() {
                    let merged = up
                        .cell_starting(s)
                        .is_some_and(|i| t.rows[at - 1].cells[i].v_merge != VMerge::None)
                        && down
                            .cell_starting(s)
                            .is_some_and(|i| t.rows[at].cells[i].v_merge == VMerge::Continue);
                    if merged {
                        row.cells[c].v_merge = VMerge::Continue;
                    }
                }
            }
            for k in 0..n {
                t.insert_row(at + k, row.clone());
            }
            let c = GridMap::of(t).rows[at].cell_at(left).unwrap_or(0);
            Ok(After::Cell(at, c))
        })
    }

    /// Insert Left/Right: one new grid column per selected column, each as
    /// wide as the column it copies. A cell spanning the insertion point
    /// widens instead.
    pub fn insert_columns(&mut self, left: bool) -> Result<(), String> {
        let r = self.need_selection()?;
        let n = r.right - r.left + 1;
        let pos = if left { r.left } else { r.right + 1 };
        let caret_row = self.caret_row_in(&r);
        let path = r.table.clone();
        self.edit_table(&path, |t| {
            repair_grid(t);
            let map = GridMap::of(t);
            let widths: Vec<u32> = (r.left..=r.right).map(|c| t.grid[c]).collect();
            for (ri, rm) in map.rows.iter().enumerate() {
                let row = &mut t.rows[ri];
                if let Some(ci) = rm
                    .cells
                    .iter()
                    .position(|&(s, span)| s < pos && pos < s + span)
                {
                    row.cells[ci].grid_span += n as u32;
                    continue;
                }
                let at = rm
                    .cell_starting(pos)
                    .or((pos == rm.end()).then_some(rm.cells.len()));
                match at {
                    Some(at) if !rm.cells.is_empty() => {
                        let src = if left {
                            at.min(rm.cells.len() - 1)
                        } else {
                            at.saturating_sub(1)
                        };
                        for (k, w) in widths.iter().enumerate() {
                            let mut cell = template_cell(&row.cells[src]);
                            cell.grid_span = 1;
                            set_dxa_width(&mut cell, *w);
                            row.cells.insert(at + k, cell);
                        }
                    }
                    _ if pos < rm.before => set_row_skips(row, rm.before + n, rm.after),
                    _ => set_row_skips(row, rm.before, rm.after + n),
                }
            }
            t.grid.splice(pos..pos, widths);
            let c = GridMap::of(t).rows[caret_row].cell_at(pos).unwrap_or(0);
            Ok(After::Cell(caret_row, c))
        })
    }

    /// Delete > Rows: every row the selection touches (the whole table when
    /// that is all of them).
    pub fn delete_rows(&mut self) -> Result<(), String> {
        let r = self.need_selection()?;
        let rows = self.table(&r.table).map_or(0, |t| t.rows.len());
        if r.top == 0 && r.bottom + 1 >= rows {
            return self.delete_table();
        }
        let path = r.table.clone();
        self.edit_table(&path, |t| {
            for i in (r.top..=r.bottom).rev() {
                t.remove_row(i);
            }
            normalize_vmerge(t);
            Ok(After::Cell(r.top.min(t.rows.len() - 1), 0))
        })
    }

    /// Delete > Columns: every grid column the selection touches (the whole
    /// table when that is all of them). Spanning cells narrow.
    pub fn delete_columns(&mut self) -> Result<(), String> {
        let r = self.need_selection()?;
        let width = self.table(&r.table).map_or(0, |t| GridMap::of(t).width(t));
        if r.left == 0 && r.right + 1 >= width {
            return self.delete_table();
        }
        let path = r.table.clone();
        let caret_row = self.caret_row_in(&r);
        self.edit_table(&path, |t| {
            repair_grid(t);
            remove_grid_columns(t, r.left, r.right);
            let mut row = caret_row.min(t.rows.len().saturating_sub(1));
            if t.rows.is_empty() {
                return Err("nothing would be left of the table".into());
            }
            if t.rows[row].cells.is_empty() {
                row = 0;
            }
            let c = GridMap::of(t).rows[row]
                .cell_at(r.left.min(t.grid.len().saturating_sub(1)))
                .unwrap_or(0);
            Ok(After::Cell(row, c))
        })
    }

    /// Delete Cells...: shift the cells to the right of the selection left,
    /// shift the cells below it up, or delete whole rows or columns.
    pub fn delete_cells(&mut self, shift: DeleteShift) -> Result<(), String> {
        match shift {
            DeleteShift::EntireRow => return self.delete_rows(),
            DeleteShift::EntireColumn => return self.delete_columns(),
            _ => {}
        }
        let r = self.need_selection()?;
        let path = r.table.clone();
        self.edit_table(&path, |t| {
            let map = GridMap::of(t);
            match shift {
                DeleteShift::ShiftLeft => {
                    for ri in (r.top..=r.bottom).rev() {
                        let rm = &map.rows[ri];
                        let hit: Vec<usize> = rm
                            .cells
                            .iter()
                            .enumerate()
                            .filter(|&(_, &(s, n))| s <= r.right && s + n > r.left)
                            .map(|(i, _)| i)
                            .collect();
                        if hit.len() == rm.cells.len() {
                            t.remove_row(ri);
                            continue;
                        }
                        let removed: usize = hit.iter().map(|&i| rm.cells[i].1).sum();
                        let row = &mut t.rows[ri];
                        for &i in hit.iter().rev() {
                            row.cells.remove(i);
                        }
                        set_row_skips(row, rm.before, rm.after + removed);
                    }
                    if t.rows.is_empty() {
                        return Err("use Delete Table to delete every cell".into());
                    }
                    normalize_vmerge(t);
                    Ok(After::Cell(r.top.min(t.rows.len() - 1), 0))
                }
                DeleteShift::ShiftUp => {
                    let k = r.bottom - r.top + 1;
                    let mut columns = Vec::new();
                    for col in r.left..=r.right {
                        let mut cells = Vec::new();
                        for ri in r.top..t.rows.len() {
                            let Some(ci) = map.rows[ri].cell_starting(col) else {
                                return Err(
                                    "cells cannot shift up through merged or missing cells".into(),
                                );
                            };
                            let cell = &t.rows[ri].cells[ci];
                            if map.rows[ri].cells[ci].1 != 1 || cell.v_merge != VMerge::None {
                                return Err(
                                    "cells cannot shift up through merged or missing cells".into(),
                                );
                            }
                            cells.push((ri, ci));
                        }
                        columns.push(cells);
                    }
                    for cells in columns {
                        let contents: Vec<Vec<Block>> = cells
                            .iter()
                            .map(|&(ri, ci)| t.rows[ri].cells[ci].blocks.clone())
                            .collect();
                        for (i, &(ri, ci)) in cells.iter().enumerate() {
                            t.rows[ri].cells[ci].blocks = match contents.get(i + k) {
                                Some(b) => b.clone(),
                                None => template_cell(&t.rows[ri].cells[ci]).blocks,
                            };
                        }
                    }
                    let c = map.rows[r.top].cell_at(r.left).unwrap_or(0);
                    Ok(After::Cell(r.top, c))
                }
                _ => unreachable!("entire row/column returned above"),
            }
        })
    }

    /// Delete > Table: the innermost table at the caret. A paragraph takes
    /// its place when the container would be left without one.
    pub fn delete_table(&mut self) -> Result<(), String> {
        let path = self
            .table_selection()
            .map(|r| r.table)
            .ok_or("the caret is not in a table")?;
        self.checkpoint(EditKind::Structural);
        self.anchor = None;
        let Some((cont, idx)) = container_mut(&mut self.doc.body, &path) else {
            return Err("no table".into());
        };
        cont.remove(idx);
        if !matches!(cont.get(idx), Some(Block::Paragraph(_))) {
            cont.insert(idx, Block::Paragraph(Paragraph::default()));
        }
        // The table's path now names the paragraph in its place.
        self.caret = Caret::at(path, 0);
        self.last = EditKind::Structural;
        Ok(())
    }

    // ---- Merge ----

    /// Merge Cells: the selected rectangle becomes one cell (a horizontal
    /// span, and a vertical merge over several rows). The cells' contents
    /// follow one another as paragraphs; empty cells add nothing.
    pub fn merge_cells(&mut self) -> Result<(), String> {
        let r = self.need_selection()?;
        let path = r.table.clone();
        self.edit_table(&path, |t| {
            merge_range(t, &r)?;
            let c = GridMap::of(t).rows[r.top]
                .cell_starting(r.left)
                .unwrap_or(0);
            Ok(After::Cell(r.top, c))
        })
    }

    /// Split Cells...: each selected cell (or, with `merge_first`, the merged
    /// selection) becomes `cols`×`rows` cells. The grid gains columns where a
    /// cell is narrower than the split; a vertically merged cell of `m` rows
    /// splits into `rows` when `rows` divides `m`, and a single-row cell adds
    /// rows, merging the rest of its row across them.
    pub fn split_cells(
        &mut self,
        cols: usize,
        rows: usize,
        merge_first: bool,
    ) -> Result<(), String> {
        if cols == 0 || rows == 0 {
            return Err("split into at least one column and one row".into());
        }
        let r = self.need_selection()?;
        let path = r.table.clone();
        self.edit_table(&path, |t| {
            repair_grid(t);
            let map = GridMap::of(t);
            let cells = r.cells(&map);
            let targets: Vec<(usize, u32)> = if merge_first && cells.len() > 1 {
                merge_range(t, &r)?;
                vec![(r.top, grid_xs(t)[r.left])]
            } else {
                let xs = grid_xs(t);
                cells
                    .iter()
                    .filter(|&&(ri, ci)| t.rows[ri].cells[ci].v_merge != VMerge::Continue)
                    .map(|&(ri, ci)| (ri, xs[map.rows[ri].cells[ci].0]))
                    .collect()
            };
            for &(row, x) in targets.iter().rev() {
                split_one(t, row, x, cols, rows)?;
            }
            let (row, x) = targets[0];
            let xs = grid_xs(t);
            let col = xs.iter().position(|&v| v == x).unwrap_or(0);
            let c = GridMap::of(t).rows[row].cell_starting(col).unwrap_or(0);
            Ok(After::Cell(row, c))
        })
    }

    /// Split Table: the rows from the caret's row on become a new table after
    /// an empty paragraph, where the caret goes. On the first row, the
    /// paragraph goes before the table instead.
    pub fn split_table(&mut self) -> Result<(), String> {
        let r = self.need_selection()?;
        let t = self.table(&r.table).ok_or("no table")?.clone();
        let at = r.top;
        let mut first = t.clone();
        let mut second = t;
        if at > 0 {
            first.rows.truncate(at);
            second.rows.drain(..at);
            let boundaries = std::mem::take(&mut first.row_boundaries);
            second.row_boundaries.clear();
            for b in &boundaries {
                use std::cmp::Ordering::*;
                match b.at.cmp(&at) {
                    Less => first.row_boundaries.push(b.clone()),
                    Greater => second.row_boundaries.push(shifted(b, at)),
                    Equal if matches!(b.kind, TableRowBoundaryKind::SdtClose(_)) => {
                        first.row_boundaries.push(b.clone())
                    }
                    Equal => second.row_boundaries.push(shifted(b, at)),
                }
            }
            if first.validate_row_boundaries().is_err() || second.validate_row_boundaries().is_err()
            {
                return Err("a table cannot be split inside a content control".into());
            }
            normalize_vmerge(&mut first);
            normalize_vmerge(&mut second);
        }
        self.checkpoint(EditKind::Structural);
        self.anchor = None;
        let Some((cont, idx)) = container_mut(&mut self.doc.body, &r.table) else {
            return Err("no table".into());
        };
        let para_at = if at == 0 {
            cont.insert(idx, Block::Paragraph(Paragraph::default()));
            idx
        } else {
            cont[idx] = Block::Table(first);
            cont.insert(idx + 1, Block::Paragraph(Paragraph::default()));
            cont.insert(idx + 2, Block::Table(second));
            idx + 1
        };
        let mut p = r.table;
        if let Some(last) = p.last_mut() {
            *last = para_at;
        }
        self.caret = Caret::at(p, 0);
        self.last = EditKind::Structural;
        Ok(())
    }

    // ---- Cell Size ----

    /// AutoFit: the table's width rules. docxcore cannot measure text, so
    /// this writes the properties Word reads (Word re-lays the table out on
    /// open) and leaves the grid as it is.
    pub fn autofit(&mut self, kind: AutoFitKind) -> Result<(), String> {
        let r = self.need_selection()?;
        self.edit_table(&r.table, |t| {
            repair_grid(t);
            let total: u32 = t.grid.iter().sum();
            let xs = grid_xs(t);
            edit_table_props(t, |p| match kind {
                AutoFitKind::Contents => {
                    p.remove("w:tblLayout");
                    p.set("<w:tblW w:w=\"0\" w:type=\"auto\"/>");
                }
                AutoFitKind::Window => {
                    p.remove("w:tblLayout");
                    p.set("<w:tblW w:w=\"5000\" w:type=\"pct\"/>");
                }
                AutoFitKind::Fixed => {
                    p.set(&format!("<w:tblW w:w=\"{total}\" w:type=\"dxa\"/>"));
                    p.set("<w:tblLayout w:type=\"fixed\"/>");
                }
            });
            let map = GridMap::of(t);
            for (ri, rm) in map.rows.iter().enumerate() {
                for (ci, &(s, n)) in rm.cells.iter().enumerate() {
                    let w = xs[s + n] - xs[s];
                    let tcw = match kind {
                        AutoFitKind::Contents => "<w:tcW w:w=\"0\" w:type=\"auto\"/>".to_string(),
                        AutoFitKind::Window => format!(
                            "<w:tcW w:w=\"{}\" w:type=\"pct\"/>",
                            (w as u64 * 5000 / total.max(1) as u64)
                        ),
                        AutoFitKind::Fixed => format!("<w:tcW w:w=\"{w}\" w:type=\"dxa\"/>"),
                    };
                    edit_cell_props(&mut t.rows[ri].cells[ci], |p| p.set(&tcw));
                }
            }
            Ok(After::Stay)
        })
    }

    /// Distribute Rows: the selected rows (every row when only the caret is in
    /// the table) get one height. docxcore cannot measure rows, so they take
    /// the tallest explicit height among them (`w:trHeight`, at least).
    pub fn distribute_rows(&mut self) -> Result<(), String> {
        let r = self.need_selection()?;
        let all = self.cell_range().is_none();
        self.edit_table(&r.table, |t| {
            let rows = if all {
                0..=t.rows.len() - 1
            } else {
                r.top..=r.bottom
            };
            let h = rows
                .clone()
                .filter_map(|i| {
                    row_trpr(&t.rows[i].raw_props)
                        .attr("w:trHeight", "w:val")
                        .and_then(|v| v.parse::<u32>().ok())
                })
                .max()
                .ok_or("the rows have no set height to distribute")?;
            for i in rows {
                let mut p = row_trpr(&t.rows[i].raw_props);
                // A row's height rule (exact or at least) is kept.
                let rule = p
                    .attr("w:trHeight", "w:hRule")
                    .map(|r| format!(" w:hRule=\"{r}\""))
                    .unwrap_or_default();
                p.set(&format!("<w:trHeight w:val=\"{h}\"{rule}/>"));
                set_row_trpr(&mut t.rows[i].raw_props, &p);
            }
            Ok(After::Stay)
        })
    }

    /// Distribute Columns: the selected grid columns (every column when only
    /// the caret is in the table) share their total width equally.
    pub fn distribute_columns(&mut self) -> Result<(), String> {
        let r = self.need_selection()?;
        let all = self.cell_range().is_none();
        self.edit_table(&r.table, |t| {
            repair_grid(t);
            let (a, b) = if all {
                (0, t.grid.len() - 1)
            } else {
                (r.left, r.right)
            };
            if a >= b {
                return Ok(After::Stay);
            }
            let n = (b - a + 1) as u32;
            let total: u32 = t.grid[a..=b].iter().sum();
            for (k, w) in t.grid[a..=b].iter_mut().enumerate() {
                *w = total / n + u32::from((k as u32) < total % n);
            }
            let xs = grid_xs(t);
            let map = GridMap::of(t);
            for (ri, rm) in map.rows.iter().enumerate() {
                for (ci, &(s, span)) in rm.cells.iter().enumerate() {
                    set_dxa_width(&mut t.rows[ri].cells[ci], xs[s + span] - xs[s]);
                }
            }
            Ok(After::Stay)
        })
    }

    // ---- Alignment ----

    /// The nine alignment buttons: `w:vAlign` on each selected cell and the
    /// horizontal alignment of each paragraph directly in it.
    pub fn set_cell_alignment(&mut self, v: VAlign, h: Align) -> Result<(), String> {
        let r = self.need_selection()?;
        self.edit_table(&r.table, |t| {
            let map = GridMap::of(t);
            for (ri, ci) in r.cells(&map) {
                let cell = &mut t.rows[ri].cells[ci];
                edit_cell_props(cell, |p| {
                    p.set(&format!("<w:vAlign w:val=\"{}\"/>", v.val()))
                });
                for b in &mut cell.blocks {
                    if let Block::Paragraph(p) = b {
                        p.props.align = h;
                    }
                }
            }
            Ok(After::Stay)
        })
    }

    /// The caret cell's alignment: `(vertical, horizontal of its first
    /// paragraph)`.
    pub fn cell_alignment(&self) -> Option<(VAlign, Align)> {
        let pos = self.table_at_caret()?;
        let cell = self
            .table(&pos.table)?
            .rows
            .get(pos.row)?
            .cells
            .get(pos.cell)?;
        let v = crate::table::cell_props(cell)
            .attr("w:vAlign", "w:val")
            .map_or(VAlign::Top, |v| VAlign::parse(&v));
        let h = match cell.blocks.first() {
            Some(Block::Paragraph(p)) => p.props.align,
            _ => Align::Left,
        };
        Some((v, h))
    }

    /// Text Direction: horizontal → top-to-bottom (`tbRl`) → bottom-to-top
    /// (`btLr`) → horizontal, from the caret cell's direction, on every
    /// selected cell.
    pub fn cycle_text_direction(&mut self) -> Result<(), String> {
        let r = self.need_selection()?;
        // The direction to step from: the caret cell's when the caret is in
        // the table the command acts on, else the range's first cell's.
        let current = match self.table_at_caret() {
            Some(p) if p.table == r.table => self.cell_text_direction(),
            _ => self.table(&r.table).and_then(|t| {
                let map = GridMap::of(t);
                let (ri, ci) = *r.cells(&map).first()?;
                crate::table::cell_props(&t.rows[ri].cells[ci]).attr("w:textDirection", "w:val")
            }),
        };
        let next = match current.as_deref() {
            Some("tbRl") => Some("btLr"),
            Some("btLr") => None,
            _ => Some("tbRl"),
        };
        self.edit_table(&r.table, |t| {
            let map = GridMap::of(t);
            for (ri, ci) in r.cells(&map) {
                edit_cell_props(&mut t.rows[ri].cells[ci], |p| match next {
                    Some(v) => p.set(&format!("<w:textDirection w:val=\"{v}\"/>")),
                    None => {
                        p.remove("w:textDirection");
                    }
                });
            }
            Ok(After::Stay)
        })
    }

    /// The caret cell's `w:textDirection`, if any.
    pub fn cell_text_direction(&self) -> Option<String> {
        let pos = self.table_at_caret()?;
        let cell = self
            .table(&pos.table)?
            .rows
            .get(pos.row)?
            .cells
            .get(pos.cell)?;
        crate::table::cell_props(cell).attr("w:textDirection", "w:val")
    }

    // ---- Data ----

    /// Sort...: reorder the selected rows (every row when the selection is in
    /// one row) by up to three column keys, stably. A header row stays first.
    /// Refused over vertically merged cells and across a content control's
    /// row boundary.
    pub fn sort_table(&mut self, spec: &SortSpec) -> Result<(), String> {
        if spec.keys.is_empty() {
            return Err("choose a column to sort by".into());
        }
        let r = self.need_selection()?;
        let whole = r.top == r.bottom;
        self.edit_table(&r.table, |t| {
            let (mut start, end) = if whole {
                (0, t.rows.len() - 1)
            } else {
                (r.top, r.bottom)
            };
            if spec.header {
                start += 1;
            }
            if start >= end {
                return Ok(After::Stay);
            }
            if t.rows[start..=end]
                .iter()
                .any(|row| row.cells.iter().any(|c| c.v_merge != VMerge::None))
            {
                return Err("rows with vertically merged cells cannot be sorted".into());
            }
            if t.row_boundaries.iter().any(|b| b.at > start && b.at <= end) {
                return Err("the rows cross a content control and cannot be sorted".into());
            }
            let map = GridMap::of(t);
            let keys: Vec<Vec<SortValue>> = (start..=end)
                .map(|ri| {
                    spec.keys
                        .iter()
                        .map(|k| {
                            let text = map.rows[ri]
                                .cell_at(k.col)
                                .map(|ci| {
                                    t.rows[ri].cells[ci]
                                        .blocks
                                        .iter()
                                        .map(Block::plain_text)
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                })
                                .unwrap_or_default();
                            SortValue::read(text.trim(), k.kind)
                        })
                        .collect()
                })
                .collect();
            let mut order: Vec<usize> = (0..keys.len()).collect();
            order.sort_by(|&a, &b| {
                for (i, k) in spec.keys.iter().enumerate() {
                    let o = keys[a][i].cmp(&keys[b][i]);
                    let o = if k.descending { o.reverse() } else { o };
                    if o != std::cmp::Ordering::Equal {
                        return o;
                    }
                }
                std::cmp::Ordering::Equal
            });
            let rows: Vec<Row> = t.rows[start..=end].to_vec();
            for (i, &from) in order.iter().enumerate() {
                t.rows[start + i] = rows[from].clone();
            }
            Ok(After::Stay)
        })
    }

    /// Convert to Text...: the innermost table becomes paragraphs, cells
    /// separated by `sep` (each row one paragraph) or one paragraph per cell.
    /// Nested tables are converted too.
    pub fn table_to_text(&mut self, sep: CellSep) -> Result<(), String> {
        let path = self
            .table_selection()
            .map(|r| r.table)
            .ok_or("the caret is not in a table")?;
        let t = self.table(&path).ok_or("no table")?;
        let mut paras = table_paragraphs(t, sep);
        if paras.is_empty() {
            paras.push(Paragraph::default());
        }
        let n = paras.len();
        self.checkpoint(EditKind::Structural);
        self.anchor = None;
        let Some((cont, idx)) = container_mut(&mut self.doc.body, &path) else {
            return Err("no table".into());
        };
        cont.splice(idx..=idx, paras.into_iter().map(Block::Paragraph));
        let end = Caret::at(
            {
                let mut p = path.clone();
                if let Some(l) = p.last_mut() {
                    *l = idx + n - 1;
                }
                p
            },
            0,
        );
        self.anchor = Some(Caret::at(path, 0));
        self.caret = end;
        let len = resolve_para(&self.doc.body, &self.caret.path)
            .map(super::para_text_len)
            .unwrap_or(0);
        self.caret.offset = len;
        self.last = EditKind::Structural;
        Ok(())
    }

    /// Convert Text to Table...: the selected paragraphs become a table. With
    /// tabs or another character, each paragraph starts a row and its pieces
    /// fill it (`cols` defaults to the most pieces in a paragraph); with
    /// paragraph marks, each paragraph is a cell, `cols` (default 1) per row.
    pub fn text_to_table(&mut self, sep: CellSep, cols: Option<usize>) -> Result<(), String> {
        let (lo, hi) = self.selection_range().ok_or("select the text to convert")?;
        if lo.path.len() != hi.path.len()
            || lo.path[..lo.path.len() - 1] != hi.path[..hi.path.len() - 1]
        {
            return Err("select paragraphs in one place to convert".into());
        }
        let (li, hii) = (*lo.path.last().unwrap_or(&0), *hi.path.last().unwrap_or(&0));
        let hii = if hi.offset == 0 && hii > li {
            hii - 1
        } else {
            hii
        };
        let width = super::tables::width_at_path(self, &lo.path);
        let parent: Vec<usize> = lo.path[..lo.path.len() - 1].to_vec();
        let paras: Vec<Paragraph> = {
            let mut out = Vec::new();
            for i in li..=hii {
                let mut p = parent.clone();
                p.push(i);
                match super::tables::block_at(&self.doc.body, &p) {
                    Some(Block::Paragraph(para)) if para.props.section_break.is_none() => {
                        out.push(para.clone())
                    }
                    Some(Block::Paragraph(_)) => {
                        return Err("the selection holds a section break".into());
                    }
                    _ => return Err("select only paragraphs to convert".into()),
                }
            }
            out
        };
        // Each row's pieces.
        let rows_of_pieces: Vec<Vec<Paragraph>> = match sep {
            CellSep::Paragraph => {
                let n = cols.unwrap_or(1).max(1);
                paras.chunks(n).map(|c| c.to_vec()).collect()
            }
            CellSep::Tab | CellSep::Char(_) => {
                let split: Vec<Vec<Paragraph>> = paras
                    .iter()
                    .map(|p| {
                        let pieces = split_inlines(&p.content, sep);
                        let last = pieces.len().saturating_sub(1);
                        pieces
                            .into_iter()
                            .enumerate()
                            .map(|(i, content)| {
                                let mut props = p.props.clone();
                                // The paragraph's mark, and any tracked change
                                // of it, ends the last piece only.
                                if i < last {
                                    crate::review::clear_mark_revisions(&mut props);
                                }
                                Paragraph { props, content }
                            })
                            .collect()
                    })
                    .collect();
                let most = split.iter().map(Vec::len).max().unwrap_or(1);
                let n = cols.unwrap_or(most).max(1);
                split
                    .into_iter()
                    .flat_map(|pieces| pieces.chunks(n).map(|c| c.to_vec()).collect::<Vec<_>>())
                    .collect()
            }
        };
        let ncols = rows_of_pieces
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            .max(cols.unwrap_or(1));
        let mut table = new_table(rows_of_pieces.len(), ncols, width, AutoFit::Default);
        for (ri, pieces) in rows_of_pieces.into_iter().enumerate() {
            for (ci, piece) in pieces.into_iter().enumerate() {
                table.rows[ri].cells[ci].blocks = vec![Block::Paragraph(piece)];
            }
        }
        self.checkpoint(EditKind::Structural);
        self.anchor = None;
        let Some((cont, _)) = container_mut(&mut self.doc.body, &lo.path) else {
            return Err("no paragraphs".into());
        };
        cont.splice(li..=hii, [Block::Table(table)]);
        if !matches!(cont.get(li + 1), Some(Block::Paragraph(_))) {
            cont.insert(li + 1, Block::Paragraph(Paragraph::default()));
        }
        let mut p = parent;
        p.extend([li, 0, 0, 0]);
        self.caret = Caret::at(p, 0);
        self.last = EditKind::Structural;
        // A paragraph split into pieces copies its tracked changes onto each;
        // give every copy its own revision target (#801).
        self.doc.initialize_revision_targets();
        Ok(())
    }
}

fn shifted(b: &crate::model::TableRowBoundary, by: usize) -> crate::model::TableRowBoundary {
    let mut b = b.clone();
    b.at -= by;
    b
}

/// Remove grid columns `left..=right`: cells inside go, cells across them
/// narrow, `gridBefore`/`gridAfter` shrink, rows left without cells go.
fn remove_grid_columns(t: &mut Table, left: usize, right: usize) {
    let map = GridMap::of(t);
    let overlap = |s: usize, n: usize| {
        let a = s.max(left);
        let b = (s + n).min(right + 1);
        b.saturating_sub(a)
    };
    for (ri, rm) in map.rows.iter().enumerate().rev() {
        let row = &mut t.rows[ri];
        for (ci, &(s, n)) in rm.cells.iter().enumerate().rev() {
            let o = overlap(s, n);
            if o == n {
                row.cells.remove(ci);
            } else if o > 0 {
                row.cells[ci].grid_span -= o as u32;
            }
        }
        let before = rm.before - overlap(0, rm.before);
        let after = rm.after - overlap(rm.end(), rm.after);
        if before != rm.before || after != rm.after {
            set_row_skips(row, before, after);
        }
        if row.cells.is_empty() {
            t.remove_row(ri);
        }
    }
    let end = (right + 1).min(t.grid.len());
    if left < end {
        t.grid.drain(left..end);
    }
    normalize_vmerge(t);
}

/// Rewrite a cell's `w:tcW` to `width` twips when it is a fixed (`dxa`) one;
/// an auto or percentage width is left to the layout.
fn set_dxa_width(cell: &mut crate::model::Cell, width: u32) {
    edit_cell_props(cell, |p| {
        if width_of(p.get("w:tcW")).is_some_and(|(_, ty)| ty == "dxa") {
            p.set(&format!("<w:tcW w:w=\"{width}\" w:type=\"dxa\"/>"));
        }
    });
}

/// Merge the cells of `r` into one (see [`Editor::merge_cells`]).
fn merge_range(t: &mut Table, r: &CellRange) -> Result<(), String> {
    // The merged cell's width is summed from the grid.
    repair_grid(t);
    let map = GridMap::of(t);
    if r.cells(&map).len() < 2 {
        return Err("select more than one cell to merge".into());
    }
    let w = r.right - r.left + 1;
    let mut per_row = Vec::new();
    for ri in r.top..=r.bottom {
        let rm = &map.rows[ri];
        let hit: Vec<usize> = rm
            .cells
            .iter()
            .enumerate()
            .filter(|&(_, &(s, n))| s <= r.right && s + n > r.left)
            .map(|(i, _)| i)
            .collect();
        let covered: usize = hit.iter().map(|&i| rm.cells[i].1).sum();
        if hit.is_empty() || rm.cells[hit[0]].0 != r.left || covered != w {
            return Err("only a rectangle of cells can be merged".into());
        }
        per_row.push((ri, hit));
    }
    // The contents, in reading order; empty cells add nothing.
    let mut blocks = Vec::new();
    for (ri, hit) in &per_row {
        for &ci in hit {
            let cell = &t.rows[*ri].cells[ci];
            if cell.v_merge == VMerge::Continue || is_empty_cell(&cell.blocks) {
                continue;
            }
            blocks.extend(cell.blocks.iter().cloned());
        }
    }
    let first = per_row[0].1[0];
    if blocks.is_empty() {
        blocks = template_cell(&t.rows[r.top].cells[first]).blocks;
    }
    let width: u32 = t.grid.iter().skip(r.left).take(w).sum();
    let tall = r.bottom > r.top;
    for (k, (ri, hit)) in per_row.into_iter().enumerate() {
        let row = &mut t.rows[ri];
        let keep = hit[0];
        for &ci in hit[1..].iter().rev() {
            row.cells.remove(ci);
        }
        let cell = &mut row.cells[keep];
        cell.grid_span = w as u32;
        cell.v_merge = match (tall, k) {
            (false, _) => VMerge::None,
            (true, 0) => VMerge::Restart,
            (true, _) => VMerge::Continue,
        };
        cell.blocks = if k == 0 {
            std::mem::take(&mut blocks)
        } else {
            template_cell(cell).blocks
        };
        edit_cell_props(cell, |p| {
            p.remove("w:hMerge");
        });
        set_dxa_width(cell, width);
    }
    Ok(())
}

fn is_empty_cell(blocks: &[Block]) -> bool {
    // Only plain runs with no text count as nothing: anything else (a
    // picture, a field, a link, a note reference...) is content to keep.
    match blocks {
        [] => true,
        [Block::Paragraph(p)] => p
            .content
            .iter()
            .all(|i| matches!(i, Inline::Run(r) if r.text.is_empty())),
        _ => false,
    }
}

/// Split the cell of row `row` whose left edge is at `x` twips into
/// `cols`×`rows` cells.
fn split_one(t: &mut Table, row: usize, x: u32, cols: usize, rows: usize) -> Result<(), String> {
    let locate = |t: &Table| -> Result<(usize, usize, usize), String> {
        let xs = grid_xs(t);
        let col = xs.iter().position(|&v| v == x).ok_or("the cell moved")?;
        let map = GridMap::of(t);
        let ci = map.rows[row].cell_starting(col).ok_or("the cell moved")?;
        Ok((col, ci, map.rows[row].cells[ci].1))
    };
    let (col0, ci, n0) = locate(t)?;
    // The cell's right edge: after the column split, the new cells lie
    // between the two edges, however many grid columns that takes.
    let x_end = grid_xs(t)[col0 + n0];
    let m = GridMap::of(t).merge_end(t, row, ci) - row + 1;
    if rows > 1 && m > 1 && !m.is_multiple_of(rows) {
        return Err(format!(
            "{m} merged rows split only into a divisor of {m} rows"
        ));
    }
    // Columns: choose the grid boundaries the new cells start on.
    if cols > 1 {
        let (col, n) = (col0, n0);
        if n < cols {
            let xs = grid_xs(t);
            let (x0, x1) = (xs[col], xs[col + n]);
            let cuts: Vec<u32> = (1..cols)
                .map(|i| x0 + (x1 - x0) * i as u32 / cols as u32)
                .collect();
            refine_grid(t, &cuts);
        }
        let (col, _, n) = locate(t)?;
        let xs = grid_xs(t);
        let (x0, x1) = (xs[col], xs[col + n]);
        let mut bounds: Vec<usize> = (0..=cols)
            .map(|i| {
                let target = x0 + (x1 - x0) * i as u32 / cols as u32;
                // The grid boundary nearest the equal share.
                (col..=col + n)
                    .min_by_key(|&b| xs[b].abs_diff(target))
                    .unwrap_or(col)
            })
            .collect();
        bounds.dedup();
        for rr in row..row + m {
            let map = GridMap::of(t);
            let Some(ci) = map.rows[rr].cell_starting(col) else {
                continue;
            };
            let base = t.rows[rr].cells[ci].clone();
            let mut pieces = Vec::new();
            for (k, w) in bounds.windows(2).enumerate() {
                let mut cell = if k == 0 {
                    base.clone()
                } else {
                    let mut c = template_cell(&base);
                    c.v_merge = base.v_merge;
                    c
                };
                cell.grid_span = (w[1] - w[0]) as u32;
                set_dxa_width(&mut cell, xs[w[1]] - xs[w[0]]);
                pieces.push(cell);
            }
            t.rows[rr].cells.splice(ci..=ci, pieces);
        }
    }
    if rows <= 1 {
        return Ok(());
    }
    let xs = grid_xs(t);
    let edge = |x: u32| xs.iter().position(|&v| v == x).ok_or("the cell moved");
    let (a, b) = (edge(x)?, edge(x_end)?);
    let inside = |s: usize| s >= a && s < b;
    if m > 1 {
        // Regroup the merged rows into `rows` groups of m/rows.
        let g = m / rows;
        for (j, rr) in (row..row + m).enumerate() {
            let map = GridMap::of(t);
            for (ci, &(s, _)) in map.rows[rr].cells.iter().enumerate() {
                if inside(s) {
                    t.rows[rr].cells[ci].v_merge = match (g, j % g) {
                        (1, _) => VMerge::None,
                        (_, 0) => VMerge::Restart,
                        _ => VMerge::Continue,
                    };
                }
            }
        }
        return Ok(());
    }
    // A single-row cell: add rows below, merging the rest of the row down.
    let map = GridMap::of(t);
    let mut new_row = template_row(&t.rows[row]);
    for (ci, &(s, _)) in map.rows[row].cells.iter().enumerate() {
        if inside(s) {
            continue;
        }
        let cell = &mut t.rows[row].cells[ci];
        if cell.v_merge == VMerge::None {
            cell.v_merge = VMerge::Restart;
        }
        new_row.cells[ci].v_merge = VMerge::Continue;
    }
    for k in 1..rows {
        t.insert_row(row + k, new_row.clone());
    }
    normalize_vmerge(t);
    Ok(())
}

/// A sort key read from a cell: unparsed values sort before parsed ones.
#[derive(Debug, Clone, PartialEq)]
enum SortValue {
    Unparsed(String),
    Text(String),
    Number(f64),
    Date(i64),
}

impl Eq for SortValue {}

impl PartialOrd for SortValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SortValue {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use SortValue::*;
        use std::cmp::Ordering::*;
        match (self, other) {
            (Text(a), Text(b)) | (Unparsed(a), Unparsed(b)) => a.cmp(b),
            (Number(a), Number(b)) => a.partial_cmp(b).unwrap_or(Equal),
            (Date(a), Date(b)) => a.cmp(b),
            (Unparsed(_), _) => Less,
            (_, Unparsed(_)) => Greater,
            _ => Equal,
        }
    }
}

impl SortValue {
    fn read(text: &str, kind: SortKind) -> Self {
        let unparsed = || SortValue::Unparsed(text.to_lowercase());
        match kind {
            SortKind::Text => SortValue::Text(text.to_lowercase()),
            SortKind::Number => parse_number(text).map_or_else(unparsed, SortValue::Number),
            SortKind::Date => parse_date(text).map_or_else(unparsed, SortValue::Date),
        }
    }
}

/// A number with an optional sign, `,` thousands separators and `.` decimals;
/// a leading currency symbol and a trailing `%` are ignored.
pub(crate) fn parse_number(text: &str) -> Option<f64> {
    let mut t = text.trim();
    let neg = if let Some(rest) = t.strip_prefix('-') {
        t = rest;
        true
    } else {
        if let Some(rest) = t.strip_prefix('+') {
            t = rest;
        }
        false
    };
    let t = t.trim_start_matches(['$', '€', '£', '¥']).trim();
    let t = t.strip_suffix('%').unwrap_or(t).trim();
    if t.is_empty()
        || !t
            .chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
    {
        return None;
    }
    let v: f64 = t.replace(',', "").parse().ok()?;
    Some(if neg { -v } else { v })
}

/// A date as a day number: `yyyy-mm-dd`, `m/d/yyyy` or `d Month yyyy`.
pub(crate) fn parse_date(text: &str) -> Option<i64> {
    let t = text.trim();
    let (y, m, d) = if let Some((y, rest)) = t.split_once('-') {
        let (m, d) = rest.split_once('-')?;
        (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
    } else if let Some((m, rest)) = t.split_once('/') {
        let (d, y) = rest.split_once('/')?;
        (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
    } else {
        let mut it = t.split_whitespace();
        let d: i64 = it.next()?.parse().ok()?;
        let month = it.next()?.to_lowercase();
        let y: i64 = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        const MONTHS: [&str; 12] = [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ];
        let m = MONTHS
            .iter()
            .position(|p| month.starts_with(p))
            .map(|i| i as i64 + 1)?;
        (y, m, d)
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(y * 400 + m * 32 + d)
}

/// Split inline content at each separator (a tab inline, or a character in
/// run text), dropping the separators.
fn split_inlines(content: &[Inline], sep: CellSep) -> Vec<Vec<Inline>> {
    let mut out = vec![Vec::new()];
    for inline in content {
        match (inline, sep) {
            (Inline::Tab(_), CellSep::Tab) => out.push(Vec::new()),
            (Inline::Run(run), CellSep::Char(c)) if run.text.contains(c) => {
                for (k, part) in run.text.split(c).enumerate() {
                    if k > 0 {
                        out.push(Vec::new());
                    }
                    if !part.is_empty() {
                        out.last_mut().expect("one piece").push(Inline::Run(Run {
                            text: part.to_string(),
                            props: run.props.clone(),
                        }));
                    }
                }
            }
            _ => out.last_mut().expect("one piece").push(inline.clone()),
        }
    }
    out
}

/// A cell's blocks as paragraphs, nested tables converted too.
fn cell_paragraphs(blocks: &[Block], sep: CellSep) -> Vec<Paragraph> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Paragraph(p) => out.push(p.clone()),
            Block::Table(t) => out.extend(table_paragraphs(t, sep)),
            _ => {}
        }
    }
    if out.is_empty() {
        out.push(Paragraph::default());
    }
    out
}

/// A table as paragraphs (see [`Editor::table_to_text`]).
fn table_paragraphs(t: &Table, sep: CellSep) -> Vec<Paragraph> {
    let sep_inline = match sep {
        CellSep::Tab => Some(Inline::Tab(RunProps::default())),
        CellSep::Char(c) => Some(Inline::Run(Run {
            text: c.to_string(),
            props: RunProps::default(),
        })),
        CellSep::Paragraph => None,
    };
    let mut out = Vec::new();
    for row in &t.rows {
        let Some(sep_inline) = &sep_inline else {
            for cell in &row.cells {
                if cell.v_merge != VMerge::Continue {
                    out.extend(cell_paragraphs(&cell.blocks, sep));
                }
            }
            continue;
        };
        let mut cur: Option<Paragraph> = None;
        for (i, cell) in row.cells.iter().enumerate() {
            let paras = cell_paragraphs(&cell.blocks, sep);
            let mut it = paras.into_iter();
            let first = it.next().unwrap_or_default();
            match cur.as_mut() {
                Some(p) if i > 0 => {
                    p.content.push(sep_inline.clone());
                    p.content.extend(first.content);
                }
                _ => cur = Some(first),
            }
            for p in it {
                if let Some(done) = cur.replace(p) {
                    out.push(done);
                }
            }
        }
        if let Some(p) = cur {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tables::tests::{cell_text, grid_doc, para};
    use super::*;
    use crate::load::{Relationships, parse_document_xml};
    use crate::model::{Document, TableRowBoundary};
    use crate::serialize::document_to_xml;
    use crate::table::cell_props;

    fn at(r: usize, c: usize) -> Caret {
        Caret::at(vec![0, r, c, 0], 0)
    }

    fn select(ed: &mut Editor, a: (usize, usize), b: (usize, usize)) {
        ed.anchor = Some(at(a.0, a.1));
        ed.caret = at(b.0, b.1);
    }

    fn t(ed: &Editor) -> &Table {
        ed.table(&[0]).expect("table")
    }

    fn texts(ed: &Editor) -> Vec<Vec<String>> {
        let t = t(ed);
        (0..t.rows.len())
            .map(|r| {
                (0..t.rows[r].cells.len())
                    .map(|c| cell_text(ed, &[0], r, c))
                    .collect()
            })
            .collect()
    }

    fn roundtrip(ed: &Editor) -> Document {
        parse_document_xml(&document_to_xml(&ed.doc), &Relationships::default())
    }

    #[test]
    fn select_row_column_table_make_cell_ranges() {
        let mut ed = Editor::new(grid_doc(3, 3));
        ed.caret = at(1, 1);
        ed.select_row().unwrap();
        let r = ed.cell_range().unwrap();
        assert_eq!((r.top, r.bottom, r.left, r.right), (1, 1, 0, 2));
        ed.anchor = None;
        ed.caret = at(1, 1);
        ed.select_column().unwrap();
        let r = ed.cell_range().unwrap();
        assert_eq!((r.top, r.bottom, r.left, r.right), (0, 2, 1, 1));
        ed.select_table().unwrap();
        let r = ed.cell_range().unwrap();
        assert_eq!((r.top, r.bottom, r.left, r.right), (0, 2, 0, 2));
        ed.anchor = None;
        ed.caret = at(2, 2);
        ed.select_cell().unwrap();
        assert_eq!(ed.cell_range(), None);
        assert_eq!(ed.selection_spans(), vec![(vec![0, 2, 2, 0], 0, 3)]);
    }

    #[test]
    fn insert_rows_adds_one_per_selected_row_in_one_undo_step() {
        let mut ed = Editor::new(grid_doc(3, 2));
        select(&mut ed, (0, 0), (1, 1));
        ed.insert_rows(false).unwrap();
        let tx = texts(&ed);
        assert_eq!(tx.len(), 5);
        assert_eq!(tx[1], ["2,1", "2,2"]);
        assert_eq!(tx[2], ["", ""]);
        assert_eq!(tx[3], ["", ""]);
        assert_eq!(tx[4], ["3,1", "3,2"]);
        assert!(ed.undo());
        assert_eq!(texts(&ed).len(), 3);

        let mut ed = Editor::new(grid_doc(2, 2));
        ed.caret = at(1, 0);
        ed.insert_rows(true).unwrap();
        assert_eq!(texts(&ed)[1], ["", ""]);
        assert_eq!(ed.caret.path, vec![0, 1, 0, 0]);
    }

    #[test]
    fn insert_columns_is_span_aware_and_adds_grid_columns() {
        let mut doc = grid_doc(2, 3);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        // Row 1: one cell spanning columns 1..2.
        tb.rows[0].cells.remove(1);
        tb.rows[0].cells[0].grid_span = 2;
        let mut ed = Editor::new(doc);
        // Two columns selected (2,2)-(2,3) → two new columns to the right.
        select(&mut ed, (1, 1), (1, 2));
        ed.insert_columns(false).unwrap();
        let tb = t(&ed);
        assert_eq!(tb.grid.len(), 5);
        assert_eq!(tb.rows[1].cells.len(), 5);
        assert_eq!(tb.rows[0].cells.len(), 4);
        // Left of the spanning cell's middle: the spanning cell widens.
        let mut ed = Editor::new(ed.doc.clone());
        ed.caret = at(1, 1);
        ed.insert_columns(true).unwrap();
        let tb = t(&ed);
        assert_eq!(tb.grid.len(), 6);
        assert_eq!(tb.rows[0].cells[0].grid_span, 3);
        assert_eq!(GridMap::of(tb).width(tb), 6);
        assert!(GridMap::of(tb).rows.iter().all(|r| r.end() + r.after == 6));
    }

    #[test]
    fn insert_columns_respects_grid_before() {
        let mut doc = grid_doc(2, 2);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        tb.grid.push(1000);
        tb.rows[0].raw_props = vec!["<w:trPr><w:gridBefore w:val=\"1\"/></w:trPr>".into()];
        tb.rows[1].raw_props = vec!["<w:trPr><w:gridAfter w:val=\"1\"/></w:trPr>".into()];
        let mut ed = Editor::new(doc);
        // Row 2's first cell is grid column 0 → insert left of it.
        ed.caret = at(1, 0);
        ed.insert_columns(true).unwrap();
        let tb = t(&ed);
        let m = GridMap::of(tb);
        assert_eq!(m.rows[0].before, 2, "row 1 had column 0 skipped: it grows");
        assert_eq!(m.rows[1].cells[0], (0, 1));
        assert_eq!(tb.rows[1].cells.len(), 3);
    }

    #[test]
    fn delete_rows_columns_and_table() {
        let mut ed = Editor::new(grid_doc(3, 3));
        select(&mut ed, (0, 1), (1, 1));
        ed.delete_rows().unwrap();
        assert_eq!(texts(&ed), [["3,1", "3,2", "3,3"]]);
        ed.caret = at(0, 1);
        ed.delete_columns().unwrap();
        assert_eq!(texts(&ed), [["3,1", "3,3"]]);
        assert_eq!(t(&ed).grid.len(), 2);
        ed.caret = at(0, 0);
        ed.select_table().unwrap();
        ed.delete_rows().unwrap();
        assert!(matches!(ed.doc.body[0], Block::Paragraph(_)));
        assert!(resolve_para(&ed.doc.body, &ed.caret.path).is_some());
        assert!(ed.undo());
        assert!(matches!(ed.doc.body[0], Block::Table(_)));
    }

    #[test]
    fn delete_column_narrows_a_spanning_cell() {
        let mut doc = grid_doc(2, 3);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        tb.rows[0].cells.remove(1);
        tb.rows[0].cells[0].grid_span = 2;
        let mut ed = Editor::new(doc);
        ed.caret = at(1, 1);
        ed.delete_columns().unwrap();
        let tb = t(&ed);
        assert_eq!(tb.rows[0].cells[0].grid_span, 1);
        assert_eq!(tb.rows[1].cells.len(), 2);
    }

    #[test]
    fn delete_cells_four_ways() {
        let mut ed = Editor::new(grid_doc(3, 3));
        ed.caret = at(0, 1);
        ed.delete_cells(DeleteShift::ShiftLeft).unwrap();
        assert_eq!(texts(&ed)[0], ["1,1", "1,3"]);
        assert_eq!(GridMap::of(t(&ed)).rows[0].after, 1);

        let mut ed = Editor::new(grid_doc(3, 3));
        ed.caret = at(0, 1);
        ed.delete_cells(DeleteShift::ShiftUp).unwrap();
        let tx = texts(&ed);
        assert_eq!([&tx[0][1], &tx[1][1], &tx[2][1]], ["2,2", "3,2", ""]);
        assert_eq!(tx[0][0], "1,1");

        let mut ed = Editor::new(grid_doc(3, 3));
        ed.caret = at(1, 1);
        ed.delete_cells(DeleteShift::EntireRow).unwrap();
        assert_eq!(texts(&ed).len(), 2);
        ed.caret = at(1, 1);
        ed.delete_cells(DeleteShift::EntireColumn).unwrap();
        assert_eq!(texts(&ed)[0], ["1,1", "1,3"]);
    }

    #[test]
    fn merge_a_rectangle_and_refuse_without_an_undo_step() {
        let mut ed = Editor::new(grid_doc(3, 3));
        {
            let Block::Table(tb) = &mut ed.doc.body[0] else {
                panic!()
            };
            tb.rows[1].cells[1].blocks = vec![para("")];
        }
        select(&mut ed, (0, 0), (1, 1));
        ed.merge_cells().unwrap();
        let tb = t(&ed);
        assert_eq!(tb.rows[0].cells.len(), 2);
        assert_eq!(tb.rows[0].cells[0].grid_span, 2);
        assert_eq!(tb.rows[0].cells[0].v_merge, VMerge::Restart);
        assert_eq!(tb.rows[1].cells[0].v_merge, VMerge::Continue);
        assert_eq!(tb.rows[1].cells[0].grid_span, 2);
        assert_eq!(cell_text(&ed, &[0], 0, 0), "1,1|1,2|2,1");
        assert_eq!(cell_text(&ed, &[0], 1, 0), "");
        let back = roundtrip(&ed);
        let Block::Table(bt) = &back.body[0] else {
            panic!()
        };
        assert_eq!(bt.rows[1].cells[0].v_merge, VMerge::Continue);
        assert_eq!(bt.rows[0].cells[0].grid_span, 2);

        // One cell: refused, and nothing to undo but the merge.
        ed.anchor = None;
        ed.caret = at(2, 2);
        assert!(ed.merge_cells().is_err());
        assert!(ed.undo());
        assert!(!ed.undo(), "the refusal left no undo step");
    }

    #[test]
    fn merge_refuses_a_ragged_selection() {
        let mut doc = grid_doc(2, 3);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        tb.rows[1].cells.pop();
        tb.rows[1].raw_props = vec!["<w:trPr><w:gridAfter w:val=\"1\"/></w:trPr>".into()];
        let mut ed = Editor::new(doc);
        select(&mut ed, (0, 1), (1, 1));
        ed.caret = at(0, 2);
        ed.anchor = Some(at(1, 1));
        assert!(ed.merge_cells().is_err());
        assert!(!ed.undo());
    }

    #[test]
    fn split_cells_across_columns_and_rows() {
        // A single cell into 2 columns: the grid gains a column, other rows span it.
        let mut ed = Editor::new(grid_doc(2, 2));
        ed.caret = at(0, 0);
        ed.split_cells(2, 1, false).unwrap();
        let tb = t(&ed);
        assert_eq!(tb.grid.len(), 3);
        assert_eq!(tb.rows[0].cells.len(), 3);
        assert_eq!(tb.rows[1].cells[0].grid_span, 2);
        assert_eq!(cell_text(&ed, &[0], 0, 0), "1,1");
        assert_eq!(cell_text(&ed, &[0], 0, 1), "");

        // Into 2 rows: a new row, the rest of the row merged down.
        let mut ed = Editor::new(grid_doc(1, 2));
        ed.caret = at(0, 1);
        ed.split_cells(1, 2, false).unwrap();
        let tb = t(&ed);
        assert_eq!(tb.rows.len(), 2);
        assert_eq!(tb.rows[0].cells[0].v_merge, VMerge::Restart);
        assert_eq!(tb.rows[1].cells[0].v_merge, VMerge::Continue);
        assert_eq!(tb.rows[1].cells[1].v_merge, VMerge::None);

        // Merge first, then split 2×1: back to two cells with the merged text.
        let mut ed = Editor::new(grid_doc(1, 3));
        select(&mut ed, (0, 0), (0, 2));
        ed.split_cells(2, 1, true).unwrap();
        let tb = t(&ed);
        assert_eq!(tb.rows[0].cells.len(), 2);
        assert_eq!(cell_text(&ed, &[0], 0, 0), "1,1|1,2|1,3");
        let spans: Vec<u32> = tb.rows[0].cells.iter().map(|c| c.grid_span).collect();
        assert_eq!(spans.iter().sum::<u32>(), 3);
        assert!(ed.undo());
        assert_eq!(texts(&ed)[0].len(), 3, "merge-and-split is one undo step");
    }

    /// The visible cells of each row: the ones that are not the continuation
    /// of a vertical merge.
    fn visible(ed: &Editor) -> Vec<usize> {
        t(ed)
            .rows
            .iter()
            .map(|r| {
                r.cells
                    .iter()
                    .filter(|c| c.v_merge != VMerge::Continue)
                    .count()
            })
            .collect()
    }

    #[test]
    fn split_into_columns_and_rows_at_once() {
        // 1×1 → 2×2: four cells, none merged.
        let mut ed = Editor::new(grid_doc(1, 1));
        ed.split_cells(2, 2, false).unwrap();
        assert_eq!(visible(&ed), [2, 2]);
        assert!(
            t(&ed)
                .rows
                .iter()
                .flat_map(|r| &r.cells)
                .all(|c| c.v_merge == VMerge::None)
        );
        // The second cell of a 1×2 table → 3×2: the first cell spans both rows.
        let mut ed = Editor::new(grid_doc(1, 2));
        ed.caret = at(0, 1);
        ed.split_cells(3, 2, false).unwrap();
        let tb = t(&ed);
        assert_eq!(tb.rows[0].cells.len(), 4);
        assert_eq!(tb.rows[1].cells.len(), 4);
        assert_eq!(tb.rows[0].cells[0].v_merge, VMerge::Restart);
        assert_eq!(tb.rows[1].cells[0].v_merge, VMerge::Continue);
        assert!(
            tb.rows[1].cells[1..]
                .iter()
                .all(|c| c.v_merge == VMerge::None)
        );
        assert_eq!(visible(&ed), [4, 3]);
    }

    #[test]
    fn split_a_merged_cell_into_columns_and_rows() {
        // m = 2 → 2×2: each row gets two unmerged cells.
        let mut ed = Editor::new(grid_doc(2, 1));
        select(&mut ed, (0, 0), (1, 0));
        ed.merge_cells().unwrap();
        ed.anchor = None;
        ed.caret = at(0, 0);
        ed.split_cells(2, 2, false).unwrap();
        assert_eq!(visible(&ed), [2, 2]);
        // m = 4 → 2×2: two pairs of merged rows, two cells wide.
        let mut ed = Editor::new(grid_doc(4, 1));
        select(&mut ed, (0, 0), (3, 0));
        ed.merge_cells().unwrap();
        ed.anchor = None;
        ed.caret = at(0, 0);
        ed.split_cells(2, 2, false).unwrap();
        assert_eq!(visible(&ed), [2, 0, 2, 0]);
        let v: Vec<VMerge> = t(&ed).rows.iter().map(|r| r.cells[1].v_merge).collect();
        assert_eq!(
            v,
            [
                VMerge::Restart,
                VMerge::Continue,
                VMerge::Restart,
                VMerge::Continue
            ]
        );
    }

    #[test]
    fn merge_keeps_a_cell_whose_only_content_is_a_linked_picture() {
        let mut ed = Editor::new(grid_doc(1, 2));
        {
            let Block::Table(tb) = &mut ed.doc.body[0] else {
                panic!()
            };
            tb.rows[0].cells[1].blocks = vec![Block::Paragraph(Paragraph {
                content: vec![Inline::Hyperlink(crate::model::Hyperlink {
                    content: vec![Inline::Raw("<w:r><w:drawing/></w:r>".into())],
                    ..Default::default()
                })],
                ..Paragraph::default()
            })];
        }
        select(&mut ed, (0, 0), (0, 1));
        ed.merge_cells().unwrap();
        let cell = &t(&ed).rows[0].cells[0];
        assert_eq!(
            cell.blocks.len(),
            2,
            "the linked picture's paragraph is kept"
        );
    }

    /// A 2×3 table whose grid is missing, too short, or all zero widths, as
    /// some producers write it.
    fn odd_grid_docs() -> Vec<(&'static str, Document)> {
        [
            ("no grid", vec![]),
            ("short grid", vec![1000]),
            ("zero grid", vec![0, 0, 0]),
        ]
        .into_iter()
        .map(|(name, grid)| {
            let mut doc = grid_doc(2, 3);
            let Block::Table(tb) = &mut doc.body[0] else {
                panic!()
            };
            tb.grid = grid;
            (name, doc)
        })
        .collect()
    }

    #[test]
    fn split_cells_on_a_missing_short_or_zero_grid() {
        for (name, doc) in odd_grid_docs() {
            for (cols, rows) in [(2, 1), (1, 2), (2, 2)] {
                for cell in [0, 2] {
                    let mut ed = Editor::new(doc.clone());
                    ed.caret = at(0, cell);
                    ed.split_cells(cols, rows, false)
                        .unwrap_or_else(|e| panic!("{name} {cols}x{rows} cell {cell}: {e}"));
                    let tb = t(&ed);
                    assert_eq!(tb.rows.len(), 2 + rows - 1, "{name}");
                    assert_eq!(
                        tb.grid.len(),
                        GridMap::of(tb).width(tb),
                        "{name}: the grid covers every column"
                    );
                    assert!(tb.grid.iter().all(|&w| w > 0), "{name}");
                    assert_eq!(
                        visible(&ed)[0],
                        3 + cols - 1,
                        "{name} {cols}x{rows} cell {cell}"
                    );
                }
            }
        }
    }

    #[test]
    fn grid_commands_on_a_missing_short_or_zero_grid_do_not_panic() {
        type Cmd = fn(&mut Editor) -> Result<(), String>;
        let cmds: [(&str, Cmd); 11] = [
            ("insert left", |e| e.insert_columns(true)),
            ("insert right", |e| e.insert_columns(false)),
            ("delete columns", |e| e.delete_columns()),
            ("delete cells left", |e| {
                e.delete_cells(DeleteShift::ShiftLeft)
            }),
            ("merge", |e| e.merge_cells()),
            ("distribute columns", |e| e.distribute_columns()),
            ("autofit fixed", |e| e.autofit(AutoFitKind::Fixed)),
            ("autofit window", |e| e.autofit(AutoFitKind::Window)),
            ("borders", |e| {
                e.apply_borders(crate::editor::BorderCmd::All)
            }),
            ("sort", |e| {
                e.sort_table(&SortSpec {
                    header: false,
                    keys: vec![SortKey {
                        col: 2,
                        kind: SortKind::Text,
                        descending: true,
                    }],
                })
            }),
            ("select column", |e| e.select_column()),
        ];
        for (name, doc) in odd_grid_docs() {
            for (label, cmd) in cmds {
                let mut ed = Editor::new(doc.clone());
                select(&mut ed, (0, 1), (1, 2));
                cmd(&mut ed).unwrap_or_else(|e| panic!("{name}: {label}: {e}"));
                assert!(ed.table(&[0]).is_some(), "{name}: {label}");
            }
        }
    }

    #[test]
    fn merge_on_a_missing_short_or_zero_grid_gives_a_real_width() {
        for (name, mut doc) in odd_grid_docs() {
            let Block::Table(tb) = &mut doc.body[0] else {
                panic!()
            };
            for cell in &mut tb.rows[0].cells {
                crate::table::edit_cell_props(cell, |p| p.set("<w:tcW w:w=\"0\" w:type=\"dxa\"/>"));
            }
            let mut ed = Editor::new(doc);
            select(&mut ed, (0, 0), (0, 2));
            ed.merge_cells().unwrap_or_else(|e| panic!("{name}: {e}"));
            let tb = t(&ed);
            let total: u32 = tb.grid.iter().sum();
            let w = crate::table_props::width_of(
                crate::table::cell_props(&tb.rows[0].cells[0]).get("w:tcW"),
            )
            .unwrap()
            .0;
            assert!(w > 0 && w as u32 == total, "{name}: {w} of {total}");
        }
    }

    #[test]
    fn a_caret_in_a_nested_table_does_not_steer_a_command_on_the_outer_one() {
        // Outer 1×2; its second cell holds a 3×1 table. The selection runs
        // from the outer first cell to the nested table's last row.
        let mut outer = grid_doc(1, 2);
        let inner = grid_doc(3, 1);
        let Block::Table(tb) = &mut outer.body[0] else {
            panic!()
        };
        tb.rows[0].cells[1].blocks = vec![inner.body[0].clone(), para("")];
        let setup = || {
            let mut ed = Editor::new(outer.clone());
            ed.anchor = Some(at(0, 0));
            ed.caret = Caret::at(vec![0, 0, 1, 0, 2, 0, 0], 0);
            assert_eq!(ed.cell_range().unwrap().table, vec![0]);
            ed
        };
        let mut ed = setup();
        ed.insert_columns(false).unwrap();
        assert_eq!(t(&ed).grid.len(), 4, "two columns added to the outer table");
        assert_eq!(ed.table(&[0, 0, 1, 0]).unwrap().grid.len(), 1);
        let mut ed = setup();
        ed.insert_columns(true).unwrap();
        assert_eq!(t(&ed).grid.len(), 4);
        let mut ed = setup();
        ed.select_table().unwrap();
        assert_eq!(ed.cell_range().unwrap().table, vec![0]);
        // Text Direction steps from the outer range's first cell (tbRl →
        // btLr), not from the nested caret cell (none → tbRl).
        let mut ed = setup();
        {
            let Block::Table(tb) = &mut ed.doc.body[0] else {
                panic!()
            };
            crate::table::edit_cell_props(&mut tb.rows[0].cells[0], |p| {
                p.set("<w:textDirection w:val=\"tbRl\"/>")
            });
        }
        ed.cycle_text_direction().unwrap();
        let first = &t(&ed).rows[0].cells[0];
        assert_eq!(
            crate::table::cell_props(first)
                .attr("w:textDirection", "w:val")
                .as_deref(),
            Some("btLr")
        );
    }

    #[test]
    fn distribute_rows_keeps_each_rows_height_rule() {
        let mut ed = Editor::new(grid_doc(2, 1));
        {
            let Block::Table(tb) = &mut ed.doc.body[0] else {
                panic!()
            };
            tb.rows[0].raw_props =
                vec!["<w:trPr><w:trHeight w:val=\"400\" w:hRule=\"exact\"/></w:trPr>".into()];
            tb.rows[1].raw_props = vec!["<w:trPr><w:trHeight w:val=\"700\"/></w:trPr>".into()];
        }
        ed.distribute_rows().unwrap();
        let h = |r: usize| {
            row_trpr(&t(&ed).rows[r].raw_props)
                .get("w:trHeight")
                .unwrap()
                .to_string()
        };
        assert_eq!(h(0), "<w:trHeight w:val=\"700\" w:hRule=\"exact\"/>");
        assert_eq!(h(1), "<w:trHeight w:val=\"700\"/>");
    }

    #[test]
    fn split_a_merged_cell_into_a_divisor_of_its_rows() {
        let mut ed = Editor::new(grid_doc(4, 1));
        select(&mut ed, (0, 0), (3, 0));
        ed.merge_cells().unwrap();
        ed.anchor = None;
        ed.caret = at(0, 0);
        assert!(ed.split_cells(1, 3, false).is_err());
        ed.split_cells(1, 2, false).unwrap();
        let v: Vec<VMerge> = t(&ed).rows.iter().map(|r| r.cells[0].v_merge).collect();
        assert_eq!(
            v,
            [
                VMerge::Restart,
                VMerge::Continue,
                VMerge::Restart,
                VMerge::Continue
            ]
        );
    }

    /// Ported from the suite's old Table Tools test: inserting at a content
    /// control's first row joins that control; inserting after its last row
    /// stays outside; deleting keeps the remaining ownership.
    #[test]
    fn row_commands_keep_content_control_ownership() {
        let adjacent = || {
            let mut doc = grid_doc(2, 1);
            let Block::Table(tb) = &mut doc.body[0] else {
                panic!()
            };
            tb.row_boundaries = vec![
                TableRowBoundary::sdt_open(0, "<w:sdt><w:sdtContent>"),
                TableRowBoundary::sdt_close(1, "</w:sdtContent></w:sdt>"),
                TableRowBoundary::sdt_open(1, "<w:sdt><w:sdtContent>"),
                TableRowBoundary::sdt_close(2, "</w:sdtContent></w:sdt>"),
            ];
            Editor::new(doc)
        };
        for (above, row) in [(true, 1), (false, 0)] {
            let mut ed = adjacent();
            ed.caret = at(row, 0);
            ed.insert_rows(above).unwrap();
            assert_eq!(ed.caret.path[1], 1);
            assert_eq!(
                t(&ed).row_control_owners(),
                Ok(vec![vec![0], vec![2], vec![2]])
            );
        }
        let mut ed = adjacent();
        ed.caret = at(0, 0);
        ed.delete_rows().unwrap();
        assert_eq!(t(&ed).row_control_owners(), Ok(vec![vec![2]]));
        let back = roundtrip(&ed);
        let Block::Table(bt) = &back.body[0] else {
            panic!()
        };
        assert_eq!(bt.row_control_owners(), Ok(vec![vec![2]]));
    }

    #[test]
    fn split_table_moves_rows_to_a_new_table_after_a_paragraph() {
        let mut ed = Editor::new(grid_doc(3, 2));
        ed.caret = at(1, 0);
        ed.split_table().unwrap();
        assert!(matches!(ed.doc.body[0], Block::Table(_)));
        assert!(matches!(ed.doc.body[1], Block::Paragraph(_)));
        let Block::Table(second) = &ed.doc.body[2] else {
            panic!()
        };
        assert_eq!(t(&ed).rows.len(), 1);
        assert_eq!(second.rows.len(), 2);
        assert_eq!(ed.caret, Caret::at(vec![1], 0));

        let mut ed = Editor::new(grid_doc(2, 2));
        ed.caret = at(0, 0);
        ed.split_table().unwrap();
        assert!(matches!(ed.doc.body[0], Block::Paragraph(_)));
        assert!(matches!(ed.doc.body[1], Block::Table(_)));
    }

    #[test]
    fn split_table_partitions_content_control_boundaries() {
        let mut doc = grid_doc(4, 1);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        tb.row_boundaries = vec![
            TableRowBoundary::sdt_open(0, "<w:sdt><w:sdtContent>"),
            TableRowBoundary::sdt_close(2, "</w:sdtContent></w:sdt>"),
        ];
        let mut ed = Editor::new(doc.clone());
        ed.caret = at(2, 0);
        ed.split_table().unwrap();
        let Block::Table(a) = &ed.doc.body[0] else {
            panic!()
        };
        let Block::Table(b) = &ed.doc.body[2] else {
            panic!()
        };
        assert!(a.validate_row_boundaries().is_ok() && a.row_boundaries.len() == 2);
        assert!(b.row_boundaries.is_empty());
        // Inside the control: refused.
        let mut ed = Editor::new(doc);
        ed.caret = at(1, 0);
        assert!(ed.split_table().is_err());
        assert!(!ed.undo());
    }

    #[test]
    fn autofit_writes_the_width_properties() {
        let mut ed = Editor::new(grid_doc(1, 2));
        ed.autofit(AutoFitKind::Fixed).unwrap();
        let tb = t(&ed);
        let p = tb.raw_tblpr.as_deref().unwrap();
        assert!(p.contains("<w:tblW w:w=\"9000\" w:type=\"dxa\"/>"));
        assert!(p.contains("<w:tblLayout w:type=\"fixed\"/>"));
        assert!(
            tb.rows[0].cells[1]
                .raw_tcpr
                .as_deref()
                .unwrap()
                .contains("w:w=\"4500\" w:type=\"dxa\"")
        );
        ed.autofit(AutoFitKind::Window).unwrap();
        let tb = t(&ed);
        let p = tb.raw_tblpr.as_deref().unwrap();
        assert!(p.contains("w:w=\"5000\" w:type=\"pct\"") && !p.contains("tblLayout"));
        assert!(
            tb.rows[0].cells[0]
                .raw_tcpr
                .as_deref()
                .unwrap()
                .contains("w:w=\"2500\" w:type=\"pct\"")
        );
        ed.autofit(AutoFitKind::Contents).unwrap();
        let tb = t(&ed);
        assert!(
            tb.raw_tblpr
                .as_deref()
                .unwrap()
                .contains("<w:tblW w:w=\"0\" w:type=\"auto\"/>")
        );
        assert!(
            tb.rows[0].cells[0]
                .raw_tcpr
                .as_deref()
                .unwrap()
                .contains("<w:tcW w:w=\"0\" w:type=\"auto\"/>")
        );
        assert_eq!(tb.grid, vec![4500, 4500], "the grid is kept");
    }

    #[test]
    fn distribute_rows_and_columns() {
        let mut ed = Editor::new(grid_doc(2, 3));
        assert!(ed.distribute_rows().is_err(), "no heights to distribute");
        {
            let Block::Table(tb) = &mut ed.doc.body[0] else {
                panic!()
            };
            tb.rows[0].raw_props = vec!["<w:trPr><w:trHeight w:val=\"600\"/></w:trPr>".into()];
            tb.grid = vec![1000, 2000, 6000];
        }
        ed.distribute_rows().unwrap();
        assert_eq!(
            row_trpr(&t(&ed).rows[1].raw_props)
                .attr("w:trHeight", "w:val")
                .as_deref(),
            Some("600")
        );
        select(&mut ed, (0, 0), (0, 1));
        ed.distribute_columns().unwrap();
        assert_eq!(t(&ed).grid, vec![1500, 1500, 6000]);
        ed.anchor = None;
        ed.distribute_columns().unwrap();
        assert_eq!(t(&ed).grid, vec![3000, 3000, 3000]);
    }

    #[test]
    fn alignment_sets_valign_and_paragraph_jc_together() {
        let mut ed = Editor::new(grid_doc(2, 2));
        select(&mut ed, (0, 0), (0, 1));
        ed.set_cell_alignment(VAlign::Center, Align::Right).unwrap();
        for c in 0..2 {
            let cell = &t(&ed).rows[0].cells[c];
            assert_eq!(
                cell_props(cell).attr("w:vAlign", "w:val").as_deref(),
                Some("center")
            );
            let Block::Paragraph(p) = &cell.blocks[0] else {
                panic!()
            };
            assert_eq!(p.props.align, Align::Right);
        }
        assert!(t(&ed).rows[1].cells[0].raw_tcpr.is_none());
        let xml = document_to_xml(&ed.doc);
        assert!(xml.contains("<w:vAlign w:val=\"center\"/>"));
        assert!(xml.contains("<w:jc w:val=\"right\"/>"));
        ed.anchor = None;
        ed.caret = at(0, 0);
        assert_eq!(ed.cell_alignment(), Some((VAlign::Center, Align::Right)));
    }

    #[test]
    fn text_direction_cycles_through_the_vertical_directions() {
        let mut ed = Editor::new(grid_doc(1, 1));
        ed.cycle_text_direction().unwrap();
        assert_eq!(ed.cell_text_direction().as_deref(), Some("tbRl"));
        ed.cycle_text_direction().unwrap();
        assert_eq!(ed.cell_text_direction().as_deref(), Some("btLr"));
        ed.cycle_text_direction().unwrap();
        assert_eq!(ed.cell_text_direction(), None);
        assert!(t(&ed).rows[0].cells[0].raw_tcpr.is_none());
    }

    fn column_doc(values: &[&str]) -> Document {
        let mut doc = grid_doc(values.len(), 2);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        for (r, v) in values.iter().enumerate() {
            tb.rows[r].cells[0].blocks = vec![para(v)];
        }
        doc
    }

    fn column(ed: &Editor) -> Vec<String> {
        (0..t(ed).rows.len())
            .map(|r| cell_text(ed, &[0], r, 0))
            .collect()
    }

    fn by(kind: SortKind, descending: bool) -> SortSpec {
        SortSpec {
            header: false,
            keys: vec![SortKey {
                col: 0,
                kind,
                descending,
            }],
        }
    }

    #[test]
    fn sort_text_number_date_with_header_and_stability() {
        let mut ed = Editor::new(column_doc(&["Name", "pear", "Apple", "fig"]));
        let mut spec = by(SortKind::Text, false);
        spec.header = true;
        ed.sort_table(&spec).unwrap();
        assert_eq!(column(&ed), ["Name", "Apple", "fig", "pear"]);

        let mut ed = Editor::new(column_doc(&["10", "$2", "1,000", "n/a", "-3", "5%"]));
        ed.sort_table(&by(SortKind::Number, false)).unwrap();
        assert_eq!(column(&ed), ["n/a", "-3", "$2", "5%", "10", "1,000"]);
        ed.sort_table(&by(SortKind::Number, true)).unwrap();
        assert_eq!(column(&ed), ["1,000", "10", "5%", "$2", "-3", "n/a"]);

        let mut ed = Editor::new(column_doc(&[
            "2024-03-01",
            "1/15/2024",
            "2 February 2024",
            "soon",
        ]));
        ed.sort_table(&by(SortKind::Date, false)).unwrap();
        assert_eq!(
            column(&ed),
            ["soon", "1/15/2024", "2 February 2024", "2024-03-01"]
        );

        // Stable, and a second key breaks ties.
        let mut doc = column_doc(&["b", "a", "b", "a"]);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        for (r, v) in ["1", "2", "3", "4"].iter().enumerate() {
            tb.rows[r].cells[1].blocks = vec![para(v)];
        }
        let mut ed = Editor::new(doc.clone());
        ed.sort_table(&by(SortKind::Text, false)).unwrap();
        let second: Vec<String> = (0..4).map(|r| cell_text(&ed, &[0], r, 1)).collect();
        assert_eq!(second, ["2", "4", "1", "3"]);
        let mut ed = Editor::new(doc);
        let spec = SortSpec {
            header: false,
            keys: vec![
                SortKey {
                    col: 0,
                    kind: SortKind::Text,
                    descending: false,
                },
                SortKey {
                    col: 1,
                    kind: SortKind::Number,
                    descending: true,
                },
            ],
        };
        ed.sort_table(&spec).unwrap();
        let second: Vec<String> = (0..4).map(|r| cell_text(&ed, &[0], r, 1)).collect();
        assert_eq!(second, ["4", "2", "3", "1"]);
    }

    #[test]
    fn sort_refuses_merged_rows_and_content_control_boundaries() {
        let mut doc = column_doc(&["b", "a", "c"]);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        tb.rows[1].cells[1].v_merge = VMerge::Restart;
        tb.rows[2].cells[1].v_merge = VMerge::Continue;
        let mut ed = Editor::new(doc);
        assert!(ed.sort_table(&by(SortKind::Text, false)).is_err());
        assert!(!ed.undo());

        let mut doc = column_doc(&["b", "a", "c"]);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        tb.row_boundaries = vec![
            TableRowBoundary::sdt_open(1, "<w:sdt><w:sdtContent>"),
            TableRowBoundary::sdt_close(3, "</w:sdtContent></w:sdt>"),
        ];
        let mut ed = Editor::new(doc);
        assert!(ed.sort_table(&by(SortKind::Text, false)).is_err());
    }

    #[test]
    fn convert_table_to_text_and_back() {
        let mut ed = Editor::new(grid_doc(2, 2));
        ed.table_to_text(CellSep::Tab).unwrap();
        assert_eq!(ed.doc.body[0].plain_text(), "1,1\t1,2");
        assert_eq!(ed.doc.body[1].plain_text(), "2,1\t2,2");
        assert_eq!(ed.doc.body[2].plain_text(), "after");
        // The converted paragraphs are selected: convert them back.
        assert_eq!(ed.selection_spans().len(), 2);
        ed.text_to_table(CellSep::Tab, None).unwrap();
        assert_eq!(texts(&ed), [["1,1", "1,2"], ["2,1", "2,2"]]);
        assert!(ed.undo());
        assert!(ed.undo());
        assert!(matches!(ed.doc.body[0], Block::Table(_)));

        let mut ed = Editor::new(grid_doc(1, 2));
        ed.table_to_text(CellSep::Paragraph).unwrap();
        assert_eq!(ed.doc.body[0].plain_text(), "1,1");
        assert_eq!(ed.doc.body[1].plain_text(), "1,2");

        let mut ed = Editor::new(grid_doc(1, 2));
        ed.table_to_text(CellSep::Char(';')).unwrap();
        assert_eq!(ed.doc.body[0].plain_text(), "1,1;1,2");
        ed.text_to_table(CellSep::Char(';'), None).unwrap();
        assert_eq!(texts(&ed), [["1,1", "1,2"]]);
    }

    #[test]
    fn convert_to_text_flattens_nested_tables() {
        let mut outer = grid_doc(1, 2);
        let inner = grid_doc(1, 2);
        let Block::Table(tb) = &mut outer.body[0] else {
            panic!()
        };
        tb.rows[0].cells[1].blocks = vec![inner.body[0].clone()];
        let mut ed = Editor::new(outer);
        ed.table_to_text(CellSep::Tab).unwrap();
        assert_eq!(ed.doc.body[0].plain_text(), "1,1\t1,1\t1,2");
    }

    #[test]
    fn text_to_table_by_paragraphs_and_columns() {
        let mut ed = Editor::new(Document {
            body: vec![para("a"), para("b"), para("c")],
        });
        ed.anchor = Some(Caret::top(0, 0));
        ed.caret = Caret::top(2, 1);
        ed.text_to_table(CellSep::Paragraph, Some(2)).unwrap();
        assert_eq!(texts(&ed), [["a", "b"], ["c", ""]]);
        assert!(matches!(ed.doc.body[1], Block::Paragraph(_)));
        // Nothing selected: refused.
        let mut ed = Editor::new(Document {
            body: vec![para("a")],
        });
        assert!(ed.text_to_table(CellSep::Tab, None).is_err());
    }

    #[test]
    fn number_and_date_parsing() {
        assert_eq!(parse_number("1,234.5"), Some(1234.5));
        assert_eq!(parse_number("-$3"), Some(-3.0));
        assert_eq!(parse_number("12%"), Some(12.0));
        assert_eq!(parse_number("12 apples"), None);
        assert!(parse_date("2024-01-02") < parse_date("1/3/2024"));
        assert_eq!(parse_date("3 Mar 2024"), parse_date("2024-03-03"));
        assert_eq!(parse_date("13/1/2024"), None);
    }

    #[test]
    fn commands_work_in_nested_tables() {
        let mut outer = grid_doc(1, 2);
        let inner = grid_doc(2, 2);
        let Block::Table(tb) = &mut outer.body[0] else {
            panic!()
        };
        tb.rows[0].cells[0].blocks = vec![inner.body[0].clone(), para("")];
        let mut ed = Editor::new(outer);
        ed.caret = Caret::at(vec![0, 0, 0, 0, 1, 1, 0], 0);
        ed.insert_rows(false).unwrap();
        assert_eq!(ed.table(&[0, 0, 0, 0]).unwrap().rows.len(), 3);
        assert_eq!(t(&ed).rows.len(), 1, "the outer table is untouched");
        ed.delete_table().unwrap();
        assert!(matches!(
            t(&ed).rows[0].cells[0].blocks[0],
            Block::Paragraph(_)
        ));
    }
}
