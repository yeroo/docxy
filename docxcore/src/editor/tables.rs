//! Table commands (#705): where the caret is in a table, cell-range
//! selection, Tab navigation and Insert Table.
//!
//! A caret path steps into a table as `table, row, cell` and continues in the
//! cell's blocks, so tables nest. Every command acts on the innermost table
//! holding the caret (or the selection), maps cells onto grid columns through
//! [`GridMap`] and records one undo step, and only after it has checked that it
//! can run: a refused command leaves no undo step behind.

use super::{
    Caret, EditKind, Editor, all_paragraph_paths, container_mut, para_text_len, resolve_para,
    split_content,
};
use crate::model::{Block, Inline, Paragraph, Table, VMerge};
use crate::sect::SectionSetup;
use crate::table::{AutoFit, DEFAULT_TEXT_WIDTH, GridMap, new_table, template_row};

/// A cell holding the caret: the path of its table block, and its row and
/// cell index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePos {
    pub table: Vec<usize>,
    pub row: usize,
    pub cell: usize,
}

/// A rectangle of grid cells in one table: rows `top..=bottom`, grid columns
/// `left..=right`, grown so no spanned or merged cell is cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRange {
    pub table: Vec<usize>,
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    pub right: usize,
}

impl CellRange {
    /// `(row, cell)` of every cell inside the range, in row-major order,
    /// including the `continue` parts of vertical merges.
    pub fn cells(&self, map: &GridMap) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for r in self.top..=self.bottom {
            let Some(rm) = map.rows.get(r) else { break };
            for (c, &(s, n)) in rm.cells.iter().enumerate() {
                if s <= self.right && s + n > self.left {
                    out.push((r, c));
                }
            }
        }
        out
    }

    /// Grow the range until it cuts no spanned or vertically merged cell.
    fn expand(&mut self, table: &Table, map: &GridMap) {
        loop {
            let before = self.clone();
            for (r, c) in self.cells(map) {
                let (s, n) = map.rows[r].cells[c];
                self.left = self.left.min(s);
                self.right = self.right.max(s + n - 1);
                if table.rows[r].cells[c].v_merge != VMerge::None {
                    let (or, oc) = map.owner(table, r, c);
                    self.top = self.top.min(or);
                    self.bottom = self.bottom.max(map.merge_end(table, or, oc));
                }
            }
            if *self == before {
                return;
            }
        }
    }
}

/// Where Tab or Shift+Tab takes the caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabTarget {
    /// Select this cell's content.
    Cell((usize, usize)),
    /// Past the last cell: add a row.
    AddRow,
    /// Nowhere (Shift+Tab in the first cell).
    Stay,
}

/// Every table a path passes through, outermost first.
pub(crate) fn table_steps(body: &[Block], path: &[usize]) -> Vec<TablePos> {
    let mut out = Vec::new();
    let mut blocks = body;
    let mut i = 0;
    while let Some(&b) = path.get(i) {
        match blocks.get(b) {
            Some(Block::Table(t)) if path.len() >= i + 3 => {
                let (row, cell) = (path[i + 1], path[i + 2]);
                out.push(TablePos {
                    table: path[..=i].to_vec(),
                    row,
                    cell,
                });
                let Some(c) = t.rows.get(row).and_then(|r| r.cells.get(cell)) else {
                    break;
                };
                blocks = &c.blocks;
                i += 3;
            }
            Some(Block::Paragraph(p)) if path.len() > i + 1 => match p.content.get(path[i + 1]) {
                Some(Inline::TextBox { blocks: inner, .. }) => {
                    blocks = inner;
                    i += 2;
                }
                _ => break,
            },
            _ => break,
        }
    }
    out
}

/// The block at `path` (a path to a block, not into a paragraph).
pub(crate) fn block_at<'a>(body: &'a [Block], path: &[usize]) -> Option<&'a Block> {
    let (&i, rest) = path.split_first()?;
    let b = body.get(i)?;
    if rest.is_empty() {
        return Some(b);
    }
    match b {
        Block::Table(t) if rest.len() >= 3 => {
            let cell = t.rows.get(rest[0])?.cells.get(rest[1])?;
            block_at(&cell.blocks, &rest[2..])
        }
        Block::Paragraph(p) => match p.content.get(rest[0])? {
            Inline::TextBox { blocks, .. } => block_at(blocks, &rest[1..]),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn table_at<'a>(body: &'a [Block], path: &[usize]) -> Option<&'a Table> {
    match block_at(body, path)? {
        Block::Table(t) => Some(t),
        _ => None,
    }
}

pub(crate) fn table_at_mut<'a>(body: &'a mut Vec<Block>, path: &[usize]) -> Option<&'a mut Table> {
    let (cont, i) = container_mut(body, path)?;
    match cont.get_mut(i)? {
        Block::Table(t) => Some(t),
        _ => None,
    }
}

/// The first and last paragraph paths inside `blocks` (at `prefix`).
fn paragraph_ends(blocks: &[Block], prefix: &[usize]) -> Option<(Vec<usize>, Vec<usize>)> {
    let paths = all_paragraph_paths(blocks);
    let full = |p: &Vec<usize>| {
        let mut v = prefix.to_vec();
        v.extend_from_slice(p);
        v
    };
    Some((full(paths.first()?), full(paths.last()?)))
}

/// The text width available at a caret: the innermost cell's width, else the
/// section's text width.
fn width_at(ed: &Editor) -> u32 {
    width_at_path(ed, &ed.caret.path)
}

/// [`width_at`] for a paragraph at `path`.
pub(crate) fn width_at_path(ed: &Editor, path: &[usize]) -> u32 {
    if let Some(pos) = table_steps(&ed.doc.body, path).last() {
        if let Some(t) = table_at(&ed.doc.body, &pos.table) {
            let map = GridMap::of(t);
            if let Some((s, n)) = map.span(pos.row, pos.cell) {
                let w: u32 = t.grid.iter().skip(s).take(n).sum();
                if w > 0 {
                    return w;
                }
            }
        }
    }
    let sections = ed.sections();
    let block = path.first().copied().unwrap_or(0);
    let raw = sections
        .get(ed.section_of_block(block))
        .map(String::as_str)
        .unwrap_or("");
    let w = SectionSetup::parse(raw).text_width(false);
    if w > 0 { w as u32 } else { DEFAULT_TEXT_WIDTH }
}

impl Editor {
    /// The innermost table cell holding the caret.
    pub fn table_at_caret(&self) -> Option<TablePos> {
        table_steps(&self.doc.body, &self.caret.path).pop()
    }

    /// Whether the caret is in a table cell.
    pub fn in_table(&self) -> bool {
        self.table_at_caret().is_some()
    }

    /// Run `f` on a copy of the outermost table holding the caret and, when
    /// it reports a change, put the copy back as one undo step. `Err` when
    /// the caret is not in a table.
    pub fn edit_table_at_caret(
        &mut self,
        f: impl FnOnce(&mut Table) -> bool,
    ) -> Result<bool, String> {
        let Some(pos) = table_steps(&self.doc.body, &self.caret.path)
            .into_iter()
            .next()
        else {
            return Err("Put the cursor in the label table first".into());
        };
        let Some(mut table) = table_at(&self.doc.body, &pos.table).cloned() else {
            return Err("Put the cursor in the label table first".into());
        };
        if !f(&mut table) {
            return Ok(false);
        }
        self.checkpoint(EditKind::Structural);
        if let Some(t) = table_at_mut(&mut self.doc.body, &pos.table) {
            *t = table;
        }
        // The caret's cell may have new content; keep it at the cell's start.
        if resolve_para(&self.doc.body, &self.caret.path).is_none() {
            let mut path = pos.table.clone();
            path.extend([pos.row, pos.cell, 0]);
            self.caret = Caret::at(path, 0);
        } else {
            self.caret.offset = 0;
        }
        self.anchor = None;
        self.doc.initialize_revision_targets();
        Ok(true)
    }

    /// The table a path names.
    pub fn table(&self, path: &[usize]) -> Option<&Table> {
        table_at(&self.doc.body, path)
    }

    /// The cell-range selection: set when the anchor and the caret sit in two
    /// different cells of the same table (the innermost one they share).
    pub fn cell_range(&self) -> Option<CellRange> {
        let anchor = self.anchor.as_ref()?;
        if *anchor == self.caret {
            return None;
        }
        let a_steps = table_steps(&self.doc.body, &anchor.path);
        let c_steps = table_steps(&self.doc.body, &self.caret.path);
        for (a, c) in a_steps.iter().zip(&c_steps) {
            if a.table != c.table {
                return None;
            }
            if (a.row, a.cell) != (c.row, c.cell) {
                return self.range_between(&c.table, (a.row, a.cell), (c.row, c.cell));
            }
        }
        None
    }

    /// The expanded rectangle spanning two cells of the table at `table`.
    pub fn range_between(
        &self,
        table: &[usize],
        a: (usize, usize),
        b: (usize, usize),
    ) -> Option<CellRange> {
        let t = self.table(table)?;
        let map = GridMap::of(t);
        let (sa, na) = map.span(a.0, a.1)?;
        let (sb, nb) = map.span(b.0, b.1)?;
        let mut r = CellRange {
            table: table.to_vec(),
            top: a.0.min(b.0),
            bottom: a.0.max(b.0),
            left: sa.min(sb),
            right: (sa + na).max(sb + nb) - 1,
        };
        r.expand(t, &map);
        Some(r)
    }

    /// The cells the table commands act on: the cell-range selection, else the
    /// caret's cell (with the rest of its vertical merge).
    pub fn table_selection(&self) -> Option<CellRange> {
        if let Some(r) = self.cell_range() {
            return Some(r);
        }
        let pos = self.table_at_caret()?;
        self.range_between(&pos.table, (pos.row, pos.cell), (pos.row, pos.cell))
    }

    /// The selection as whole paragraphs of the range's cells (see
    /// [`Editor::selection_spans`]). Empty paragraphs are included.
    pub(crate) fn cell_range_spans(&self, range: &CellRange) -> Vec<(Vec<usize>, usize, usize)> {
        let Some(t) = self.table(&range.table) else {
            return Vec::new();
        };
        let map = GridMap::of(t);
        let mut out = Vec::new();
        for (r, c) in range.cells(&map) {
            let mut prefix = range.table.clone();
            prefix.extend([r, c]);
            for p in all_paragraph_paths(&t.rows[r].cells[c].blocks) {
                let mut path = prefix.clone();
                path.extend(p);
                let len = resolve_para(&self.doc.body, &path)
                    .map(para_text_len)
                    .unwrap_or(0);
                out.push((path, 0, len));
            }
        }
        out
    }

    /// Word's Delete over selected cells: empty them (keeping each cell's first
    /// paragraph's formatting). One undo step.
    pub(crate) fn clear_cells(&mut self, range: &CellRange) -> bool {
        self.checkpoint(EditKind::Structural);
        self.anchor = None;
        let Some(t) = table_at_mut(&mut self.doc.body, &range.table) else {
            return false;
        };
        let map = GridMap::of(t);
        let cells = range.cells(&map);
        for &(r, c) in &cells {
            let cell = &mut t.rows[r].cells[c];
            let first = match cell.blocks.first() {
                Some(Block::Paragraph(p)) => Paragraph {
                    props: p.props.clone(),
                    content: Vec::new(),
                },
                _ => Paragraph::default(),
            };
            cell.blocks = vec![Block::Paragraph(first)];
        }
        if let Some(&(r, c)) = cells.first() {
            let mut path = range.table.clone();
            path.extend([r, c, 0]);
            self.caret = Caret::at(path, 0);
        }
        true
    }

    /// Select all the content of cell `(row, cell)` of the table at `table`
    /// (collapsed in an empty cell), as Tab does.
    pub fn select_cell_content(&mut self, table: &[usize], row: usize, cell: usize) -> bool {
        let Some(c) = self
            .table(table)
            .and_then(|t| t.rows.get(row))
            .and_then(|r| r.cells.get(cell))
        else {
            return false;
        };
        let mut prefix = table.to_vec();
        prefix.extend([row, cell]);
        let Some((first, last)) = paragraph_ends(&c.blocks, &prefix) else {
            return false;
        };
        let end = resolve_para(&self.doc.body, &last)
            .map(para_text_len)
            .unwrap_or(0);
        let start = Caret::at(first, 0);
        let stop = Caret::at(last, end);
        self.anchor = (start != stop).then_some(start);
        self.caret = stop;
        self.last = EditKind::None;
        true
    }

    /// The cells Tab visits, in order: every cell but the `continue` parts of
    /// vertical merges.
    fn tab_stops(t: &Table) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (r, row) in t.rows.iter().enumerate() {
            for (c, cell) in row.cells.iter().enumerate() {
                if cell.v_merge != VMerge::Continue {
                    out.push((r, c));
                }
            }
        }
        out
    }

    /// Where Tab (`back`: Shift+Tab) goes from the caret's cell. The single
    /// rule behind [`Editor::table_next_cell`], [`Editor::table_prev_cell`]
    /// and [`Editor::table_tab_adds_row`], so a host that asks first and then
    /// moves can never be told one thing and see another.
    fn tab_target(&self, back: bool) -> Option<(TablePos, TabTarget)> {
        let pos = self.table_at_caret()?;
        let t = self.table(&pos.table)?;
        let stops = Self::tab_stops(t);
        let here = GridMap::of(t).owner(t, pos.row, pos.cell);
        let target = match stops.iter().position(|&s| s == here) {
            Some(i) if back => i
                .checked_sub(1)
                .map_or(TabTarget::Stay, |i| TabTarget::Cell(stops[i])),
            Some(i) => stops
                .get(i + 1)
                .map_or(TabTarget::AddRow, |&s| TabTarget::Cell(s)),
            // Not a stop (an orphaned merge continuation): the nearest stop in
            // reading order that way, else nowhere. Only the last stop adds a
            // row.
            None if back => stops
                .iter()
                .rev()
                .find(|&&s| s < here)
                .map_or(TabTarget::Stay, |&s| TabTarget::Cell(s)),
            None => stops
                .iter()
                .find(|&&s| s > here)
                .map_or(TabTarget::Stay, |&s| TabTarget::Cell(s)),
        };
        Some((pos, target))
    }

    /// Whether Tab would add a row (the caret is in the table's last cell), so
    /// a host can check edit permission first.
    pub fn table_tab_adds_row(&self) -> bool {
        matches!(self.tab_target(false), Some((_, TabTarget::AddRow)))
    }

    /// Tab in a table: select the next cell's content; in the last cell, add a
    /// row like the last one (one undo step) and move into its first cell.
    /// `false` when the caret is not in a table.
    pub fn table_next_cell(&mut self) -> bool {
        let Some((pos, target)) = self.tab_target(false) else {
            return false;
        };
        match target {
            TabTarget::Cell((r, c)) => {
                self.select_cell_content(&pos.table, r, c);
            }
            TabTarget::Stay => {}
            TabTarget::AddRow => {
                self.checkpoint(EditKind::Structural);
                let Some(t) = table_at_mut(&mut self.doc.body, &pos.table) else {
                    return false;
                };
                let Some(last) = t.rows.last() else {
                    return false;
                };
                let row = template_row(last);
                let at = t.rows.len();
                t.insert_row(at, row);
                self.select_cell_content(&pos.table, at, 0);
                self.last = EditKind::Structural;
            }
        }
        true
    }

    /// Shift+Tab in a table: select the previous cell's content (nothing in
    /// the first cell). `false` when the caret is not in a table.
    pub fn table_prev_cell(&mut self) -> bool {
        let Some((pos, target)) = self.tab_target(true) else {
            return false;
        };
        if let TabTarget::Cell((r, c)) = target {
            self.select_cell_content(&pos.table, r, c);
        }
        true
    }

    /// Insert Table at the caret: a paragraph is split mid-text, the table goes
    /// before a paragraph whose start holds the caret and after one whose end
    /// does, nested when the caret is in a cell. A paragraph always follows
    /// the table. The caret lands in the first cell. One undo step.
    pub fn insert_table(&mut self, rows: usize, cols: usize, fit: AutoFit) -> Result<(), String> {
        if rows == 0 || cols == 0 {
            return Err("a table needs at least one row and one column".into());
        }
        let path = self.caret.path.clone();
        let Some(len) = resolve_para(&self.doc.body, &path).map(para_text_len) else {
            return Err("the caret is not in a paragraph".into());
        };
        let table = new_table(rows, cols, width_at(self), fit);
        let off = self.caret.offset.min(len);
        self.checkpoint(EditKind::Structural);
        self.anchor = None;
        let Some((cont, idx)) = container_mut(&mut self.doc.body, &path) else {
            return Err("the caret is not in a paragraph".into());
        };
        let at = if off == 0 {
            idx
        } else if off == len {
            idx + 1
        } else {
            let Some(Block::Paragraph(p)) = cont.get_mut(idx) else {
                return Err("the caret is not in a paragraph".into());
            };
            let right = split_content(&mut p.content, off);
            let props = p.props.clone();
            // The section break ends the section after the split, so it (and
            // its tracked change) moves with the paragraph's second half. A
            // tracked pPrChange stays on both halves, as every split does (#801).
            p.props.section_break = None;
            p.props.section_property_change = None;
            // So does a tracked change of the paragraph mark.
            crate::review::clear_mark_revisions(&mut p.props);
            cont.insert(
                idx + 1,
                Block::Paragraph(Paragraph {
                    props,
                    content: right,
                }),
            );
            idx + 1
        };
        cont.insert(at, Block::Table(table));
        if !matches!(cont.get(at + 1), Some(Block::Paragraph(_))) {
            cont.insert(at + 1, Block::Paragraph(Paragraph::default()));
        }
        let mut table_path = path;
        if let Some(last) = table_path.last_mut() {
            *last = at;
        }
        table_path.extend([0, 0, 0]);
        self.caret = Caret::at(table_path, 0);
        self.doc.initialize_revision_targets();
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::load::{Relationships, parse_document_xml};
    use crate::model::{Document, Run, RunProps, TableRowBoundary};
    use crate::serialize::document_to_xml;

    pub(crate) fn para(text: &str) -> Block {
        Block::Paragraph(Paragraph {
            content: if text.is_empty() {
                vec![]
            } else {
                vec![Inline::Run(Run {
                    text: text.into(),
                    props: RunProps::default(),
                })]
            },
            ..Paragraph::default()
        })
    }

    /// A `rows`×`cols` table whose cells read `r,c` (1-based), then a paragraph.
    pub(crate) fn grid_doc(rows: usize, cols: usize) -> Document {
        let mut t = new_table(rows, cols, 9000, AutoFit::Default);
        for (r, row) in t.rows.iter_mut().enumerate() {
            for (c, cell) in row.cells.iter_mut().enumerate() {
                cell.blocks = vec![para(&format!("{},{}", r + 1, c + 1))];
            }
        }
        Document {
            body: vec![Block::Table(t), para("after")],
        }
    }

    pub(crate) fn cell_text(ed: &Editor, table: &[usize], r: usize, c: usize) -> String {
        let t = ed.table(table).expect("table");
        t.rows[r].cells[c]
            .blocks
            .iter()
            .map(|b| b.plain_text())
            .collect::<Vec<_>>()
            .join("|")
    }

    fn reload(ed: &Editor) -> Document {
        parse_document_xml(&document_to_xml(&ed.doc), &Relationships::default())
    }

    #[test]
    fn issue_643_repro_tab_shift_tab_ctrl_tab() {
        let mut ed = Editor::new(Document {
            body: vec![
                Block::Table(new_table(2, 2, 9360, AutoFit::Default)),
                para(""),
            ],
        });
        assert_eq!(ed.caret.path, vec![0, 0, 0, 0]);
        ed.insert_char('a');
        ed.insert_char('\n');
        ed.insert_char('b');
        ed.insert_tab(); // Ctrl+Tab types a tab in the cell
        ed.insert_char('c');
        for ch in ['d', 'e', 'f', 'g'] {
            assert!(ed.table_next_cell());
            ed.insert_char(ch);
        }
        assert!(ed.table_prev_cell());
        ed.insert_char('h');

        let doc = reload(&ed);
        let ed = Editor::new(doc);
        let t = ed.table(&[0]).unwrap();
        assert_eq!(t.rows.len(), 3);
        assert_eq!(cell_text(&ed, &[0], 0, 0), "a|b\tc");
        assert_eq!(cell_text(&ed, &[0], 0, 1), "d");
        assert_eq!(cell_text(&ed, &[0], 1, 0), "e");
        assert_eq!(cell_text(&ed, &[0], 1, 1), "h");
        assert_eq!(cell_text(&ed, &[0], 2, 0), "g");
        assert_eq!(cell_text(&ed, &[0], 2, 1), "");
    }

    #[test]
    fn tab_selects_the_next_cells_content_and_shift_tab_stops_at_the_first() {
        let mut ed = Editor::new(grid_doc(2, 2));
        assert!(ed.table_next_cell());
        assert_eq!(ed.anchor, Some(Caret::at(vec![0, 0, 1, 0], 0)));
        assert_eq!(ed.caret, Caret::at(vec![0, 0, 1, 0], 3));
        assert!(ed.table_prev_cell());
        assert_eq!(ed.caret, Caret::at(vec![0, 0, 0, 0], 3));
        let before = ed.caret.clone();
        assert!(ed.table_prev_cell(), "still in a table");
        assert_eq!(ed.caret, before, "Shift+Tab in the first cell stays");
        // Moving is no edit.
        assert!(!ed.undo());
    }

    #[test]
    fn tab_outside_a_table_does_nothing() {
        let mut ed = Editor::new(Document {
            body: vec![para("x")],
        });
        assert!(!ed.table_next_cell());
        assert!(!ed.table_prev_cell());
    }

    #[test]
    fn tab_in_the_last_cell_adds_a_row_undone_in_one_step() {
        let mut ed = Editor::new(grid_doc(1, 2));
        ed.caret = Caret::at(vec![0, 0, 1, 0], 1);
        let original = ed.doc.clone();
        assert!(ed.table_next_cell());
        let t = ed.table(&[0]).unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1].cells.len(), 2);
        assert_eq!(ed.caret, Caret::at(vec![0, 1, 0, 0], 0));
        assert_eq!(ed.anchor, None);
        ed.insert_char('z');
        assert!(ed.undo());
        assert!(ed.undo());
        assert_eq!(ed.doc, original);
    }

    #[test]
    fn tab_skips_vertical_merge_continuations() {
        let mut ed = Editor::new(grid_doc(2, 2));
        {
            let Block::Table(t) = &mut ed.doc.body[0] else {
                panic!()
            };
            t.rows[0].cells[1].v_merge = VMerge::Restart;
            t.rows[1].cells[1].v_merge = VMerge::Continue;
        }
        ed.caret = Caret::at(vec![0, 1, 0, 0], 0);
        // (2,1) → the continuation (2,2) is skipped → a new row.
        assert!(ed.table_next_cell());
        assert_eq!(ed.table(&[0]).unwrap().rows.len(), 3);
        // A copied row is never vertically merged.
        assert_eq!(
            ed.table(&[0]).unwrap().rows[2].cells[1].v_merge,
            VMerge::None
        );
    }

    #[test]
    fn tab_from_an_orphaned_merge_continuation_agrees_with_tab_adds_row() {
        // Row 1's first cell claims to continue a merge with nothing above it.
        let mut ed = Editor::new(grid_doc(2, 2));
        {
            let Block::Table(t) = &mut ed.doc.body[0] else {
                panic!()
            };
            t.rows[0].cells[0].v_merge = VMerge::Continue;
        }
        ed.caret = Caret::at(vec![0, 0, 0, 0], 0);
        assert!(!ed.table_tab_adds_row());
        assert!(ed.table_next_cell());
        assert_eq!(ed.table(&[0]).unwrap().rows.len(), 2, "no row added");
        assert_eq!(ed.caret.path, vec![0, 0, 1, 0]);
        // Shift+Tab from it has nowhere to go.
        ed.caret = Caret::at(vec![0, 0, 0, 0], 0);
        assert!(ed.table_prev_cell());
        assert_eq!(ed.caret.path, vec![0, 0, 0, 0]);
        // From the last cell the two agree the other way.
        ed.caret = Caret::at(vec![0, 1, 1, 0], 0);
        assert!(ed.table_tab_adds_row());
        assert!(ed.table_next_cell());
        assert_eq!(ed.table(&[0]).unwrap().rows.len(), 3);
    }

    #[test]
    fn tab_navigates_the_innermost_table() {
        let mut outer = grid_doc(1, 2);
        let inner = grid_doc(1, 2);
        let Block::Table(t) = &mut outer.body[0] else {
            panic!()
        };
        t.rows[0].cells[0].blocks = vec![inner.body[0].clone(), para("")];
        let mut ed = Editor::new(outer);
        ed.caret = Caret::at(vec![0, 0, 0, 0, 0, 0, 0], 0);
        assert!(ed.table_next_cell());
        assert_eq!(ed.caret.path, vec![0, 0, 0, 0, 0, 1, 0]);
        assert!(ed.table_next_cell(), "last inner cell adds an inner row");
        assert_eq!(ed.table(&[0, 0, 0, 0]).unwrap().rows.len(), 2);
        assert_eq!(ed.table(&[0]).unwrap().rows.len(), 1);
    }

    #[test]
    fn a_new_row_keeps_content_control_boundaries_valid() {
        let mut t = new_table(2, 1, 9000, AutoFit::Default);
        t.row_boundaries = vec![
            TableRowBoundary::sdt_open(0, "<w:sdt><w:sdtPr/><w:sdtContent>"),
            TableRowBoundary::sdt_close(2, "</w:sdtContent></w:sdt>"),
        ];
        let mut ed = Editor::new(Document {
            body: vec![Block::Table(t), para("")],
        });
        ed.caret = Caret::at(vec![0, 1, 0, 0], 0);
        assert!(ed.table_next_cell());
        let t = ed.table(&[0]).unwrap();
        assert_eq!(t.rows.len(), 3);
        assert!(t.validate_row_boundaries().is_ok());
    }

    #[test]
    fn cell_range_selection_is_a_rectangle() {
        let mut ed = Editor::new(grid_doc(3, 3));
        // Column 2, rows 1..3.
        ed.anchor = Some(Caret::at(vec![0, 0, 1, 0], 0));
        ed.caret = Caret::at(vec![0, 2, 1, 0], 1);
        let r = ed.cell_range().unwrap();
        assert_eq!((r.top, r.bottom, r.left, r.right), (0, 2, 1, 1));
        let paths: Vec<Vec<usize>> = ed.selection_spans().into_iter().map(|s| s.0).collect();
        assert_eq!(
            paths,
            vec![vec![0, 0, 1, 0], vec![0, 1, 1, 0], vec![0, 2, 1, 0]]
        );
        ed.toggle_bold();
        let t = ed.table(&[0]).unwrap();
        let bold = |r: usize, c: usize| match &t.rows[r].cells[c].blocks[0] {
            Block::Paragraph(p) => match &p.content[0] {
                Inline::Run(run) => run.props.bold,
                _ => false,
            },
            _ => false,
        };
        assert!(bold(0, 1) && bold(1, 1) && bold(2, 1));
        assert!(!bold(0, 2) && !bold(1, 0) && !bold(2, 0));
    }

    #[test]
    fn delete_over_a_cell_range_clears_only_those_cells() {
        let mut ed = Editor::new(grid_doc(2, 3));
        ed.anchor = Some(Caret::at(vec![0, 0, 1, 0], 1));
        ed.caret = Caret::at(vec![0, 1, 2, 0], 0);
        assert!(ed.delete_selection());
        assert_eq!(cell_text(&ed, &[0], 0, 0), "1,1");
        assert_eq!(cell_text(&ed, &[0], 0, 1), "");
        assert_eq!(cell_text(&ed, &[0], 0, 2), "");
        assert_eq!(cell_text(&ed, &[0], 1, 0), "2,1");
        assert_eq!(cell_text(&ed, &[0], 1, 1), "");
        assert!(ed.undo());
        assert_eq!(cell_text(&ed, &[0], 0, 1), "1,2");
    }

    #[test]
    fn cell_range_grows_over_spans_and_merges() {
        let mut doc = grid_doc(3, 3);
        let Block::Table(t) = &mut doc.body[0] else {
            panic!()
        };
        // Row 1: cell (1,2) spans columns 2..3.
        t.rows[0].cells.remove(2);
        t.rows[0].cells[1].grid_span = 2;
        // Column 1 merged over rows 2..3.
        t.rows[1].cells[0].v_merge = VMerge::Restart;
        t.rows[2].cells[0].v_merge = VMerge::Continue;
        let mut ed = Editor::new(doc);
        ed.anchor = Some(Caret::at(vec![0, 0, 0, 0], 0));
        ed.caret = Caret::at(vec![0, 1, 1, 0], 0);
        let r = ed.cell_range().unwrap();
        assert_eq!((r.top, r.bottom, r.left, r.right), (0, 2, 0, 2));
    }

    #[test]
    fn same_cell_selection_is_not_a_cell_range() {
        let mut ed = Editor::new(grid_doc(1, 2));
        ed.anchor = Some(Caret::at(vec![0, 0, 0, 0], 0));
        ed.caret = Caret::at(vec![0, 0, 0, 0], 2);
        assert_eq!(ed.cell_range(), None);
        let s = ed.table_selection().unwrap();
        assert_eq!((s.top, s.bottom, s.left, s.right), (0, 0, 0, 0));
    }

    #[test]
    fn insert_table_at_the_caret() {
        // Mid-text: the paragraph splits around the table.
        let mut ed = Editor::new(Document {
            body: vec![para("abcd")],
        });
        ed.caret = Caret::top(0, 2);
        ed.insert_table(2, 3, AutoFit::Default).unwrap();
        assert!(matches!(ed.doc.body[1], Block::Table(_)));
        assert_eq!(ed.doc.body[0].plain_text(), "ab");
        assert_eq!(ed.doc.body[2].plain_text(), "cd");
        assert_eq!(ed.caret, Caret::at(vec![1, 0, 0, 0], 0));
        let t = ed.table(&[1]).unwrap();
        assert_eq!((t.rows.len(), t.rows[0].cells.len()), (2, 3));
        assert_eq!(t.grid, vec![3120; 3]);
        assert!(ed.undo());
        assert_eq!(ed.doc.body.len(), 1);
        assert_eq!(ed.doc.body[0].plain_text(), "abcd");

        // At the start: before the paragraph.
        let mut ed = Editor::new(Document {
            body: vec![para("x")],
        });
        ed.insert_table(1, 1, AutoFit::Default).unwrap();
        assert!(matches!(ed.doc.body[0], Block::Table(_)));
        assert_eq!(ed.doc.body[1].plain_text(), "x");

        // At the end of the last paragraph: after it, plus a paragraph.
        let mut ed = Editor::new(Document {
            body: vec![para("x")],
        });
        ed.caret = Caret::top(0, 1);
        ed.insert_table(1, 1, AutoFit::Default).unwrap();
        assert!(matches!(ed.doc.body[1], Block::Table(_)));
        assert!(matches!(ed.doc.body[2], Block::Paragraph(_)));
    }

    #[test]
    fn insert_table_nests_in_a_cell_with_the_cells_width() {
        let mut ed = Editor::new(grid_doc(1, 2));
        ed.caret = Caret::at(vec![0, 0, 1, 0], 3);
        ed.insert_table(1, 2, AutoFit::Default).unwrap();
        let outer = ed.table(&[0]).unwrap();
        assert!(matches!(outer.rows[0].cells[1].blocks[1], Block::Table(_)));
        assert_eq!(ed.caret.path, vec![0, 0, 1, 1, 0, 0, 0]);
        let inner = ed.table(&[0, 0, 1, 1]).unwrap();
        assert_eq!(inner.grid, vec![2250, 2250]);
    }

    #[test]
    fn insert_table_uses_the_sections_text_width() {
        let mut ed = Editor::new(Document {
            body: vec![
                para(""),
                Block::SectionProperties(crate::model::SectionProperties {
                    raw: "<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
                          <w:pgMar w:top=\"1440\" w:right=\"1000\" w:bottom=\"1440\" w:left=\"906\"/></w:sectPr>"
                        .into(),
                    property_change: None,
                }),
            ],
        });
        ed.insert_table(1, 2, AutoFit::Default).unwrap();
        assert_eq!(ed.table(&[0]).unwrap().grid, vec![5000, 5000]);
    }
}
