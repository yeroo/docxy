//! The table pens (#763): Insert > Table > Draw Table and the Layout tab's
//! Draw group. Drawing a rectangle makes a one-cell table; drawing a line
//! across a table splits only the cells it crosses; erasing a line between
//! two cells merges them.
//!
//! Everything here is in twips from the table's left edge and in row
//! indices: the host measures the screen and decides which rows a vertical
//! line crosses and where a horizontal one falls. Each command is one undo
//! step, and a refusal changes nothing and records none.

use super::Editor;
use super::table_layout::{After, add_rows_below, merge_range, set_dxa_width};
use super::tables::{CellRange, width_at};
use crate::model::{Row, Table, VMerge};
use crate::table::{
    AutoFit, GridMap, grid_xs, new_table, normalize_vmerge, refine_grid, repair_grid, template_cell,
};
use crate::table_props;

/// The narrowest table Draw Table makes, in twips.
pub const DRAW_MIN_WIDTH: u32 = 360;
/// The lowest row height a drawn rectangle or a divided row gets, in twips;
/// a drawn rectangle no taller than this sets no height.
pub const DRAW_MIN_HEIGHT: u32 = 280;

/// A line drawn with the pen across a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawnLine {
    /// A vertical line at `x` twips from the table's left edge over rows
    /// `top..=bottom`.
    Vertical { x: u32, top: usize, bottom: usize },
    /// A horizontal line inside row `row` from `x0` to `x1` twips, `at`
    /// twips below the row's top (it divides a set row height there).
    Across {
        row: usize,
        x0: u32,
        x1: u32,
        at: u32,
    },
    /// A horizontal line on the boundary below row `row` (not the last), from
    /// `x0` to `x1` twips: it ends the vertical merges it crosses there.
    Under { row: usize, x0: u32, x1: u32 },
}

/// A line between cells, for the eraser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableEdge {
    /// The vertical edge at grid boundary `col` in row `row`.
    Vertical { row: usize, col: usize },
    /// The horizontal edge below row `row` at grid column `col`.
    Horizontal { row: usize, col: usize },
}

const OUTER: &str = "the table's outer border cannot be erased";
const NO_LINE: &str = "there is no line there";
const ALREADY: &str = "there is already a line there";

impl Editor {
    /// Draw Table's rectangle: a one-cell table `width` twips wide (between
    /// [`DRAW_MIN_WIDTH`] and the text width at the caret) at the caret, as
    /// Insert Table places one, with its row at least `height` twips high
    /// when that is above [`DRAW_MIN_HEIGHT`]. One undo step.
    pub fn draw_table(&mut self, width: u32, height: u32) -> Result<(), String> {
        let table = self.drawn_table(width, height);
        self.insert_built_table(table, false)
    }

    /// [`Editor::draw_table`] after the paragraph at `para`, never inside or
    /// before it, however short it is: a gesture's table goes after the
    /// paragraph it was drawn from, and a paragraph always follows it, so it
    /// never joins a table before or after.
    pub fn draw_table_after(
        &mut self,
        para: &[usize],
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if super::resolve_para(&self.doc.body, para).is_none() {
            return Err("there is no paragraph there".into());
        }
        self.caret = super::Caret::at(para.to_vec(), 0);
        let table = self.drawn_table(width, height);
        self.insert_built_table(table, true)
    }

    /// The one-cell table a rectangle `width`×`height` twips draws at the
    /// caret.
    fn drawn_table(&self, width: u32, height: u32) -> Table {
        let w = width.clamp(DRAW_MIN_WIDTH, width_at(self).max(DRAW_MIN_WIDTH));
        let mut table = new_table(1, 1, w, AutoFit::Fixed(Some(w)));
        if height > DRAW_MIN_HEIGHT {
            set_row_height(&mut table.rows[0], height, Some("atLeast"));
        }
        table
    }

    /// Draw a line across the table at `table` with the pen: the cells it
    /// crosses split, and nothing else changes shape. Refused, with no undo
    /// step, when the line crosses no cell.
    pub fn draw_table_line(&mut self, table: &[usize], line: DrawnLine) -> Result<(), String> {
        self.edit_table(table, |t| match line {
            DrawnLine::Vertical { x, top, bottom } => draw_vertical(t, x, top, bottom),
            DrawnLine::Across { row, x0, x1, at } => draw_across(t, row, x0, x1, at),
            DrawnLine::Under { row, x0, x1 } => draw_under(t, row, x0, x1),
        })
    }

    /// Erase the line `edge` of the table at `table`: the cells on either side
    /// merge, as Merge Cells merges them. Refused, with no undo step, on the
    /// table's outer border, where there is no line, and between cells that
    /// do not line up.
    pub fn erase_table_line(&mut self, table: &[usize], edge: TableEdge) -> Result<(), String> {
        self.edit_table(table, |t| match edge {
            TableEdge::Vertical { row, col } => erase_vertical(t, row, col),
            TableEdge::Horizontal { row, col } => erase_horizontal(t, row, col),
        })
    }
}

fn row_height(row: &Row) -> Option<(u32, Option<String>)> {
    table_props::row_height(&row.raw_props)
}

fn set_row_height(row: &mut Row, h: u32, rule: Option<&str>) {
    table_props::set_row_height(&mut row.raw_props, h, rule);
}

/// The cells of row `row` a horizontal line from `x0` to `x1` crosses: those
/// whose horizontal middle it covers.
fn crossed_cells(t: &Table, map: &GridMap, row: usize, x0: u32, x1: u32) -> Vec<usize> {
    let xs = grid_xs(t);
    let (a, b) = (x0.min(x1), x0.max(x1));
    map.rows[row]
        .cells
        .iter()
        .enumerate()
        .filter(|&(_, &(s, n))| {
            let mid = (xs[s] + xs[s + n]) / 2;
            mid >= a && mid <= b
        })
        .map(|(i, _)| i)
        .collect()
}

fn check_row(t: &Table, row: usize) -> Result<(), String> {
    if row < t.rows.len() {
        Ok(())
    } else {
        Err("draw the line inside the table".into())
    }
}

fn draw_vertical(t: &mut Table, x: u32, top: usize, bottom: usize) -> Result<After, String> {
    let (top, bottom) = (top.min(bottom), top.max(bottom));
    check_row(t, bottom)?;
    repair_grid(t);
    if x == 0 || x >= *grid_xs(t).last().unwrap_or(&0) {
        return Err(ALREADY.into());
    }
    refine_grid(t, &[x]);
    let xs = grid_xs(t);
    let col = xs.iter().position(|&v| v == x).ok_or(ALREADY)?;
    let map = GridMap::of(t);
    // A cell the line cuts that is merged over more rows is cut whole.
    let (mut top, mut bottom) = (top, bottom);
    loop {
        let (mut grown_top, mut grown_bottom) = (top, bottom);
        for r in top..=bottom {
            let Some(ci) = map.rows[r].cell_at(col) else {
                continue;
            };
            if map.rows[r].cells[ci].0 == col || t.rows[r].cells[ci].v_merge == VMerge::None {
                continue;
            }
            let (or, oc) = map.owner(t, r, ci);
            grown_top = grown_top.min(or);
            grown_bottom = grown_bottom.max(map.merge_end(t, or, oc));
        }
        if (grown_top, grown_bottom) == (top, bottom) {
            break;
        }
        (top, bottom) = (grown_top, grown_bottom);
    }
    let mut first = None;
    for r in top..=bottom {
        let Some(ci) = map.rows[r].cell_at(col) else {
            continue;
        };
        let (s, n) = map.rows[r].cells[ci];
        if s == col {
            continue;
        }
        let row = &mut t.rows[r];
        let mut right = template_cell(&row.cells[ci]);
        right.v_merge = row.cells[ci].v_merge;
        right.grid_span = (s + n - col) as u32;
        set_dxa_width(&mut right, xs[s + n] - xs[col]);
        let left = &mut row.cells[ci];
        left.grid_span = (col - s) as u32;
        set_dxa_width(left, xs[col] - xs[s]);
        row.cells.insert(ci + 1, right);
        first.get_or_insert((r, ci));
    }
    let (r, c) = first.ok_or(ALREADY)?;
    Ok(After::Cell(r, c))
}

fn draw_across(t: &mut Table, row: usize, x0: u32, x1: u32, at: u32) -> Result<After, String> {
    check_row(t, row)?;
    repair_grid(t);
    let map = GridMap::of(t);
    let hit = crossed_cells(t, &map, row, x0, x1);
    let Some(&first) = hit.first() else {
        return Err("the line crosses no cell".into());
    };
    let starts: Vec<usize> = hit.iter().map(|&ci| map.rows[row].cells[ci].0).collect();
    add_rows_below(t, row, 1, |s| starts.contains(&s));
    if let Some((h, rule)) = row_height(&t.rows[row]) {
        let top = at.max(DRAW_MIN_HEIGHT);
        let below = h.saturating_sub(at).max(DRAW_MIN_HEIGHT);
        set_row_height(&mut t.rows[row], top, rule.as_deref());
        set_row_height(&mut t.rows[row + 1], below, rule.as_deref());
    }
    Ok(After::Cell(row, first))
}

fn draw_under(t: &mut Table, row: usize, x0: u32, x1: u32) -> Result<After, String> {
    check_row(t, row + 1)?;
    repair_grid(t);
    let map = GridMap::of(t);
    let mut first = None;
    for ci in crossed_cells(t, &map, row + 1, x0, x1) {
        let cell = &mut t.rows[row + 1].cells[ci];
        if cell.v_merge == VMerge::Continue {
            cell.v_merge = VMerge::Restart;
            first.get_or_insert(ci);
        }
    }
    let c = first.ok_or(ALREADY)?;
    normalize_vmerge(t);
    Ok(After::Cell(row + 1, c))
}

/// The rows a cell's vertical merge covers, from any of its rows.
fn merge_rows(t: &Table, map: &GridMap, row: usize, cell: usize) -> (usize, usize) {
    let (or, oc) = map.owner(t, row, cell);
    (or, map.merge_end(t, or, oc))
}

fn erase_vertical(t: &mut Table, row: usize, col: usize) -> Result<After, String> {
    repair_grid(t);
    let map = GridMap::of(t);
    let rm = map.rows.get(row).ok_or(NO_LINE)?;
    let right = rm.cell_starting(col);
    let left = right.and_then(|r| r.checked_sub(1));
    let (Some(li), Some(ri)) = (left, right) else {
        // A cell running across the boundary hides no line; past the row's
        // first or last cell is the outside.
        let across = right.is_none() && rm.cell_at(col).is_some();
        return Err(if across { NO_LINE } else { OUTER }.into());
    };
    let rows = merge_rows(t, &map, row, li);
    if rows != merge_rows(t, &map, row, ri) {
        return Err("the cells on either side of the line are not the same height".into());
    }
    let (s, _) = rm.cells[li];
    let (rs, rn) = rm.cells[ri];
    let r = CellRange {
        table: Vec::new(),
        top: rows.0,
        bottom: rows.1,
        left: s,
        right: rs + rn - 1,
    };
    merge_range(t, &r)?;
    let c = GridMap::of(t).rows[r.top].cell_starting(s).unwrap_or(0);
    Ok(After::Cell(r.top, c))
}

fn erase_horizontal(t: &mut Table, row: usize, col: usize) -> Result<After, String> {
    repair_grid(t);
    let map = GridMap::of(t);
    if row + 1 >= map.rows.len() {
        return Err(OUTER.into());
    }
    let (Some(ui), Some(li)) = (map.rows[row].cell_at(col), map.rows[row + 1].cell_at(col)) else {
        return Err(OUTER.into());
    };
    let span = map.rows[row].cells[ui];
    if map.rows[row + 1].cells[li] != span {
        return Err("the cells on either side of the line are not the same width".into());
    }
    if t.rows[row + 1].cells[li].v_merge == VMerge::Continue {
        return Err(NO_LINE.into());
    }
    let r = CellRange {
        table: Vec::new(),
        top: merge_rows(t, &map, row, ui).0,
        bottom: merge_rows(t, &map, row + 1, li).1,
        left: span.0,
        right: span.0 + span.1 - 1,
    };
    merge_range(t, &r)?;
    drop_continued_rows(t, r.top + 1, r.bottom);
    normalize_vmerge(t);
    let c = GridMap::of(t).rows[r.top]
        .cell_starting(span.0)
        .unwrap_or(0);
    Ok(After::Cell(r.top, c))
}

/// Remove the rows in `from..=to` that only continue the row above (every
/// cell a vertical-merge continuation, laid on the grid as the row above),
/// adding a removed row's set height to the row above when both have one.
fn drop_continued_rows(t: &mut Table, from: usize, to: usize) {
    for r in (from.max(1)..=to.min(t.rows.len().saturating_sub(1))).rev() {
        let map = GridMap::of(t);
        let continued = map.rows[r] == map.rows[r - 1]
            && t.rows[r]
                .cells
                .iter()
                .all(|c| c.v_merge == VMerge::Continue);
        if !continued {
            continue;
        }
        if let (Some((above, rule)), Some((h, _))) =
            (row_height(&t.rows[r - 1]), row_height(&t.rows[r]))
        {
            set_row_height(&mut t.rows[r - 1], above + h, rule.as_deref());
        }
        t.remove_row(r);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tables::tests::{cell_text, grid_doc, para};
    use super::super::{Caret, Editor};
    use super::*;
    use crate::load::{Relationships, parse_document_xml};
    use crate::model::{Block, Document};
    use crate::serialize::document_to_xml;

    fn t(ed: &Editor) -> &Table {
        ed.table(&[0]).expect("table")
    }

    fn table_mut(ed: &mut Editor) -> &mut Table {
        let Block::Table(tb) = &mut ed.doc.body[0] else {
            panic!("no table")
        };
        tb
    }

    /// Each row's cells as (left x, right x, merge): the table's shape,
    /// whatever its grid columns.
    fn shape(t: &Table) -> Vec<Vec<(u32, u32, VMerge)>> {
        let xs = grid_xs(t);
        let map = GridMap::of(t);
        map.rows
            .iter()
            .zip(&t.rows)
            .map(|(rm, row)| {
                rm.cells
                    .iter()
                    .zip(&row.cells)
                    .map(|(&(s, n), c)| (xs[s], xs[s + n], c.v_merge))
                    .collect()
            })
            .collect()
    }

    fn heights(t: &Table) -> Vec<Option<u32>> {
        t.rows.iter().map(|r| row_height(r).map(|h| h.0)).collect()
    }

    fn reload(ed: &Editor) -> Document {
        parse_document_xml(&document_to_xml(&ed.doc), &Relationships::default())
    }

    /// The 2×2 `grid_doc` with 4500-twip columns.
    fn two_by_two() -> Editor {
        Editor::new(grid_doc(2, 2))
    }

    #[test]
    fn draw_table_makes_one_cell_of_the_drawn_size_at_the_caret() {
        let mut ed = Editor::new(Document {
            body: vec![para("hello world")],
        });
        ed.caret = Caret::at(vec![0], 5);
        ed.draw_table(3000, 1200).unwrap();
        // The paragraph splits; the table goes between its halves.
        assert!(matches!(ed.doc.body[1], Block::Table(_)));
        let Block::Table(tb) = &ed.doc.body[1] else {
            unreachable!()
        };
        assert_eq!(tb.grid, vec![3000]);
        assert_eq!(tb.rows.len(), 1);
        assert_eq!(tb.rows[0].cells.len(), 1);
        assert_eq!(
            row_height(&tb.rows[0]),
            Some((1200, Some("atLeast".into())))
        );
        assert_eq!(ed.caret.path, vec![1, 0, 0, 0]);
        assert!(ed.undo());
        assert!(!ed.undo(), "one undo step");
        assert_eq!(ed.doc.body.len(), 1);

        // Saved and loaded again, the drawn table keeps its size.
        let mut ed = Editor::new(Document {
            body: vec![para("")],
        });
        ed.draw_table(3000, 1200).unwrap();
        let back = reload(&ed);
        let Block::Table(bt) = &back.body[0] else {
            panic!("no table after reload")
        };
        assert_eq!(bt.grid, vec![3000]);
        assert_eq!(row_height(&bt.rows[0]).map(|h| h.0), Some(1200));
    }

    #[test]
    fn a_table_drawn_after_an_empty_paragraph_never_joins_the_table_before() {
        // [table][empty paragraph]: the drawn table goes after the paragraph,
        // and a paragraph follows it.
        let mut doc = grid_doc(1, 1);
        doc.body[1] = para("");
        let mut ed = Editor::new(doc);
        ed.draw_table_after(&[1], 3000, 0).unwrap();
        let kinds: Vec<&str> = ed
            .doc
            .body
            .iter()
            .map(|b| match b {
                Block::Table(_) => "table",
                Block::Paragraph(_) => "para",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["table", "para", "table", "para"]);
        assert_eq!(ed.caret.path, vec![2, 0, 0, 0]);
        // After a paragraph with text too, and never splitting it.
        let mut ed = Editor::new(Document {
            body: vec![para("hello"), para("world")],
        });
        ed.caret = Caret::at(vec![1], 2);
        ed.draw_table_after(&[0], 3000, 0).unwrap();
        assert!(matches!(ed.doc.body[1], Block::Table(_)));
        assert_eq!(ed.doc.body.len(), 3, "world follows; nothing split");
        assert!(ed.undo());
        assert!(!ed.undo(), "one undo step");
        assert!(ed.draw_table_after(&[7], 3000, 0).is_err());
        assert!(!ed.undo());
    }

    #[test]
    fn draw_table_clamps_its_size() {
        let mut ed = Editor::new(Document {
            body: vec![para("")],
        });
        ed.draw_table(100_000, 100).unwrap();
        let tb = t(&ed);
        assert_eq!(tb.grid, vec![crate::table::DEFAULT_TEXT_WIDTH]);
        assert_eq!(row_height(&tb.rows[0]), None, "a low drag sets no height");
        let mut ed = Editor::new(Document {
            body: vec![para("")],
        });
        ed.draw_table(10, 0).unwrap();
        assert_eq!(t(&ed).grid, vec![DRAW_MIN_WIDTH]);
    }

    #[test]
    fn draw_table_in_a_cell_nests_within_the_cell() {
        let mut ed = two_by_two();
        ed.caret = Caret::at(vec![0, 0, 1, 0], 3);
        ed.draw_table(9000, 600).unwrap();
        let inner = ed.table(&[0, 0, 1, 1]).expect("nested table");
        assert_eq!(inner.grid, vec![4500], "no wider than the cell");
        assert_eq!(ed.caret.path, vec![0, 0, 1, 1, 0, 0, 0]);
    }

    #[test]
    fn a_vertical_line_through_one_cell_splits_only_that_cell() {
        let mut ed = two_by_two();
        ed.draw_table_line(
            &[0],
            DrawnLine::Vertical {
                x: 2000,
                top: 0,
                bottom: 0,
            },
        )
        .unwrap();
        let tb = t(&ed);
        assert_eq!(tb.grid, vec![2000, 2500, 4500]);
        assert_eq!(
            shape(tb),
            vec![
                vec![
                    (0, 2000, VMerge::None),
                    (2000, 4500, VMerge::None),
                    (4500, 9000, VMerge::None)
                ],
                vec![(0, 4500, VMerge::None), (4500, 9000, VMerge::None)],
            ]
        );
        assert_eq!(tb.rows[1].cells[0].grid_span, 2);
        assert_eq!(cell_text(&ed, &[0], 0, 0), "1,1");
        assert_eq!(cell_text(&ed, &[0], 0, 1), "");
        assert_eq!(ed.caret.path, vec![0, 0, 0, 0]);
        assert!(ed.undo());
        assert!(!ed.undo(), "one undo step");
    }

    #[test]
    fn a_vertical_line_on_an_existing_line_is_refused() {
        let mut ed = two_by_two();
        let line = DrawnLine::Vertical {
            x: 4500,
            top: 0,
            bottom: 1,
        };
        assert_eq!(ed.draw_table_line(&[0], line), Err(ALREADY.into()));
        assert!(!ed.undo(), "no undo step");
        assert_eq!(t(&ed).grid, vec![4500, 4500]);
        let off = DrawnLine::Vertical {
            x: 9000,
            top: 0,
            bottom: 0,
        };
        assert!(ed.draw_table_line(&[0], off).is_err());
        let bad_row = DrawnLine::Vertical {
            x: 100,
            top: 0,
            bottom: 5,
        };
        assert!(ed.draw_table_line(&[0], bad_row).is_err());
        assert!(!ed.undo());
    }

    #[test]
    fn a_vertical_line_cuts_a_merged_cell_whole() {
        // Column 0 merged over both rows; the line is drawn in row 0 only.
        let mut ed = two_by_two();
        let tb = table_mut(&mut ed);
        tb.rows[0].cells[0].v_merge = VMerge::Restart;
        tb.rows[1].cells[0].v_merge = VMerge::Continue;
        ed.draw_table_line(
            &[0],
            DrawnLine::Vertical {
                x: 1500,
                top: 0,
                bottom: 0,
            },
        )
        .unwrap();
        let s = shape(t(&ed));
        assert_eq!(
            s[0][..2],
            [(0, 1500, VMerge::Restart), (1500, 4500, VMerge::Restart)]
        );
        assert_eq!(
            s[1][..2],
            [(0, 1500, VMerge::Continue), (1500, 4500, VMerge::Continue)]
        );
    }

    #[test]
    fn a_vertical_line_on_a_row_with_grid_before() {
        // Row 1 starts one grid column in: the line in that skipped space
        // crosses nothing there.
        let mut doc = grid_doc(2, 2);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        tb.rows[1].cells.remove(0);
        tb.rows[1].raw_props = vec!["<w:trPr><w:gridBefore w:val=\"1\"/></w:trPr>".into()];
        let mut ed = Editor::new(doc);
        ed.draw_table_line(
            &[0],
            DrawnLine::Vertical {
                x: 2000,
                top: 0,
                bottom: 1,
            },
        )
        .unwrap();
        let tb = t(&ed);
        assert_eq!(tb.rows[0].cells.len(), 3);
        assert_eq!(tb.rows[1].cells.len(), 1);
        assert_eq!(GridMap::of(tb).rows[1].before, 2);
    }

    #[test]
    fn a_line_across_one_cell_adds_a_row_and_merges_the_rest_down() {
        let mut ed = two_by_two();
        ed.draw_table_line(
            &[0],
            DrawnLine::Across {
                row: 0,
                x0: 500,
                x1: 4000,
                at: 300,
            },
        )
        .unwrap();
        let tb = t(&ed);
        assert_eq!(
            shape(tb),
            vec![
                vec![(0, 4500, VMerge::None), (4500, 9000, VMerge::Restart)],
                vec![(0, 4500, VMerge::None), (4500, 9000, VMerge::Continue)],
                vec![(0, 4500, VMerge::None), (4500, 9000, VMerge::None)],
            ]
        );
        assert_eq!(cell_text(&ed, &[0], 0, 0), "1,1");
        assert_eq!(cell_text(&ed, &[0], 1, 0), "");
        assert_eq!(heights(tb), vec![None; 3], "no set height, none added");
        assert!(ed.undo());
        assert!(!ed.undo());
        // A line covering no cell's middle crosses none.
        let short = DrawnLine::Across {
            row: 0,
            x0: 100,
            x1: 900,
            at: 300,
        };
        assert!(ed.draw_table_line(&[0], short).is_err());
        assert!(!ed.undo());
    }

    #[test]
    fn a_line_across_divides_the_rows_set_height() {
        let mut ed = Editor::new(Document {
            body: vec![para("")],
        });
        ed.draw_table(4000, 2000).unwrap();
        ed.draw_table_line(
            &[0],
            DrawnLine::Across {
                row: 0,
                x0: 0,
                x1: 4000,
                at: 800,
            },
        )
        .unwrap();
        let tb = t(&ed);
        assert_eq!(heights(tb), vec![Some(800), Some(1200)]);
        assert_eq!(
            row_height(&tb.rows[1]).and_then(|h| h.1).as_deref(),
            Some("atLeast"),
            "the rule is kept"
        );
        assert_eq!(shape(tb).len(), 2);
        // An exact row keeps its rule; a piece never goes below the minimum.
        let mut ed = two_by_two();
        set_row_height(&mut table_mut(&mut ed).rows[0], 1000, Some("exact"));
        ed.draw_table_line(
            &[0],
            DrawnLine::Across {
                row: 0,
                x0: 0,
                x1: 9000,
                at: 950,
            },
        )
        .unwrap();
        let tb = t(&ed);
        assert_eq!(heights(tb)[..2], [Some(950), Some(DRAW_MIN_HEIGHT)]);
        assert_eq!(
            row_height(&tb.rows[1]).and_then(|h| h.1).as_deref(),
            Some("exact")
        );
    }

    #[test]
    fn a_line_across_the_middle_row_of_a_merge_makes_two_merges() {
        // Column 0 merged over rows 0..=2; the line crosses it in row 1.
        let mut ed = Editor::new(grid_doc(3, 2));
        let tb = table_mut(&mut ed);
        tb.rows[0].cells[0].v_merge = VMerge::Restart;
        tb.rows[1].cells[0].v_merge = VMerge::Continue;
        tb.rows[2].cells[0].v_merge = VMerge::Continue;
        ed.draw_table_line(
            &[0],
            DrawnLine::Across {
                row: 1,
                x0: 0,
                x1: 4500,
                at: 100,
            },
        )
        .unwrap();
        let col0: Vec<VMerge> = shape(t(&ed)).iter().map(|r| r[0].2).collect();
        assert_eq!(
            col0,
            [
                VMerge::Restart,
                VMerge::Continue,
                VMerge::Restart,
                VMerge::Continue
            ],
            "rows 0–1 and the new row – row 2"
        );
        // Column 1 of row 1 merges down over the new row.
        let col1: Vec<VMerge> = shape(t(&ed)).iter().map(|r| r[1].2).collect();
        assert_eq!(
            col1,
            [
                VMerge::None,
                VMerge::Restart,
                VMerge::Continue,
                VMerge::None
            ]
        );
    }

    #[test]
    fn a_line_across_a_row_a_merge_passes_through() {
        // Column 1 merged over rows 0..=1; the line crosses column 0 in row 0:
        // the merge grows over the new row.
        let mut ed = two_by_two();
        let tb = table_mut(&mut ed);
        tb.rows[0].cells[1].v_merge = VMerge::Restart;
        tb.rows[1].cells[1].v_merge = VMerge::Continue;
        ed.draw_table_line(
            &[0],
            DrawnLine::Across {
                row: 0,
                x0: 0,
                x1: 4500,
                at: 100,
            },
        )
        .unwrap();
        let col1: Vec<VMerge> = shape(t(&ed)).iter().map(|r| r[1].2).collect();
        assert_eq!(col1, [VMerge::Restart, VMerge::Continue, VMerge::Continue]);
    }

    #[test]
    fn a_line_under_a_row_ends_a_merge_there() {
        let mut ed = two_by_two();
        let tb = table_mut(&mut ed);
        tb.rows[0].cells[0].v_merge = VMerge::Restart;
        tb.rows[1].cells[0].v_merge = VMerge::Continue;
        let under = DrawnLine::Under {
            row: 0,
            x0: 0,
            x1: 9000,
        };
        ed.draw_table_line(&[0], under).unwrap();
        let tb = t(&ed);
        assert!(
            tb.rows
                .iter()
                .all(|r| r.cells.iter().all(|c| c.v_merge == VMerge::None))
        );
        assert_eq!(ed.caret.path, vec![0, 1, 0, 0]);
        // The line is there now: drawing it again is refused.
        assert_eq!(ed.draw_table_line(&[0], under), Err(ALREADY.into()));
        assert!(ed.undo());
        assert!(!ed.undo());
        let last = DrawnLine::Under {
            row: 1,
            x0: 0,
            x1: 9000,
        };
        assert!(ed.draw_table_line(&[0], last).is_err());
    }

    #[test]
    fn erasing_a_vertical_line_merges_the_two_cells() {
        let mut ed = two_by_two();
        ed.erase_table_line(&[0], TableEdge::Vertical { row: 1, col: 1 })
            .unwrap();
        let tb = t(&ed);
        assert_eq!(tb.rows[1].cells.len(), 1);
        assert_eq!(tb.rows[1].cells[0].grid_span, 2);
        assert_eq!(tb.rows[0].cells.len(), 2, "the other row keeps its line");
        assert_eq!(cell_text(&ed, &[0], 1, 0), "2,1|2,2");
        assert_eq!(ed.caret.path, vec![0, 1, 0, 0]);
        assert!(ed.undo());
        assert!(!ed.undo());
    }

    #[test]
    fn erasing_a_horizontal_line_merges_the_two_cells_down() {
        let mut ed = Editor::new(grid_doc(3, 2));
        ed.erase_table_line(&[0], TableEdge::Horizontal { row: 0, col: 1 })
            .unwrap();
        let tb = t(&ed);
        assert_eq!(tb.rows.len(), 3, "column 0 still divides the rows");
        assert_eq!(tb.rows[0].cells[1].v_merge, VMerge::Restart);
        assert_eq!(tb.rows[1].cells[1].v_merge, VMerge::Continue);
        assert_eq!(cell_text(&ed, &[0], 0, 1), "1,2|2,2");
        assert_eq!(ed.caret.path, vec![0, 0, 1, 0]);
    }

    #[test]
    fn the_eraser_refuses_outer_missing_and_uneven_lines() {
        let mut doc = grid_doc(2, 2);
        let Block::Table(tb) = &mut doc.body[0] else {
            panic!()
        };
        tb.rows[0].cells[0].v_merge = VMerge::Restart;
        tb.rows[1].cells[0].v_merge = VMerge::Continue;
        let mut ed = Editor::new(doc);
        for (edge, why) in [
            (TableEdge::Vertical { row: 0, col: 0 }, OUTER),
            (TableEdge::Vertical { row: 0, col: 2 }, OUTER),
            (TableEdge::Horizontal { row: 1, col: 0 }, OUTER),
            (TableEdge::Horizontal { row: 0, col: 0 }, NO_LINE),
            (
                TableEdge::Vertical { row: 0, col: 1 },
                "the cells on either side of the line are not the same height",
            ),
        ] {
            assert_eq!(ed.erase_table_line(&[0], edge), Err(why.into()), "{edge:?}");
        }
        // Inside a cell spanning the boundary.
        let mut ed = two_by_two();
        ed.draw_table_line(
            &[0],
            DrawnLine::Vertical {
                x: 2000,
                top: 0,
                bottom: 0,
            },
        )
        .unwrap();
        assert_eq!(
            ed.erase_table_line(&[0], TableEdge::Vertical { row: 1, col: 1 }),
            Err(NO_LINE.into())
        );
        // Cells of different widths above and below.
        assert_eq!(
            ed.erase_table_line(&[0], TableEdge::Horizontal { row: 0, col: 0 }),
            Err("the cells on either side of the line are not the same width".into())
        );
        assert!(ed.undo());
        assert!(!ed.undo(), "no refusal left an undo step");
    }

    #[test]
    fn drawing_then_erasing_a_line_restores_the_table() {
        // Vertical.
        let mut ed = two_by_two();
        let before = shape(t(&ed));
        ed.draw_table_line(
            &[0],
            DrawnLine::Vertical {
                x: 2000,
                top: 0,
                bottom: 0,
            },
        )
        .unwrap();
        ed.erase_table_line(&[0], TableEdge::Vertical { row: 0, col: 1 })
            .unwrap();
        assert_eq!(shape(t(&ed)), before);
        assert_eq!(cell_text(&ed, &[0], 0, 0), "1,1");

        // Across, with a set height: the extra row goes and the height is whole.
        let mut ed = two_by_two();
        set_row_height(&mut table_mut(&mut ed).rows[0], 2000, None);
        let before = shape(t(&ed));
        ed.draw_table_line(
            &[0],
            DrawnLine::Across {
                row: 0,
                x0: 0,
                x1: 4500,
                at: 800,
            },
        )
        .unwrap();
        ed.erase_table_line(&[0], TableEdge::Horizontal { row: 0, col: 0 })
            .unwrap();
        let tb = t(&ed);
        assert_eq!(shape(tb), before);
        assert_eq!(heights(tb), vec![Some(2000), None]);
        assert_eq!(cell_text(&ed, &[0], 0, 0), "1,1");

        // Under: a merge, its line drawn, then erased.
        let mut ed = two_by_two();
        let tb = table_mut(&mut ed);
        tb.rows[0].cells[0].v_merge = VMerge::Restart;
        tb.rows[1].cells[0].v_merge = VMerge::Continue;
        let before = shape(t(&ed));
        ed.draw_table_line(
            &[0],
            DrawnLine::Under {
                row: 0,
                x0: 0,
                x1: 4500,
            },
        )
        .unwrap();
        ed.erase_table_line(&[0], TableEdge::Horizontal { row: 0, col: 0 })
            .unwrap();
        assert_eq!(shape(t(&ed)), before);
    }

    #[test]
    fn the_pens_work_on_a_nested_table() {
        let mut outer = grid_doc(1, 2);
        let inner = grid_doc(2, 2);
        let Block::Table(tb) = &mut outer.body[0] else {
            panic!()
        };
        tb.rows[0].cells[1].blocks = vec![inner.body[0].clone(), para("")];
        let mut ed = Editor::new(outer);
        let path = [0, 0, 1, 0];
        ed.erase_table_line(&path, TableEdge::Vertical { row: 0, col: 1 })
            .unwrap();
        assert_eq!(ed.table(&path).unwrap().rows[0].cells.len(), 1);
        assert_eq!(
            t(&ed).rows[0].cells.len(),
            2,
            "the outer table is untouched"
        );
        assert_eq!(ed.caret.path, vec![0, 0, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn the_pens_on_a_missing_or_zero_grid() {
        for grid in [vec![], vec![0, 0]] {
            let mut ed = two_by_two();
            table_mut(&mut ed).grid = grid.clone();
            let w = usable_width(t(&ed));
            ed.draw_table_line(
                &[0],
                DrawnLine::Vertical {
                    x: w / 4,
                    top: 0,
                    bottom: 1,
                },
            )
            .unwrap_or_else(|e| panic!("{grid:?}: {e}"));
            let tb = t(&ed);
            assert_eq!(tb.grid.len(), 3, "{grid:?}");
            assert!(tb.grid.iter().all(|&w| w > 0));
            let mut ed = two_by_two();
            table_mut(&mut ed).grid = grid.clone();
            ed.erase_table_line(&[0], TableEdge::Vertical { row: 0, col: 1 })
                .unwrap_or_else(|e| panic!("{grid:?}: {e}"));
        }
    }

    fn usable_width(t: &Table) -> u32 {
        crate::table::usable_grid(t).iter().sum()
    }
}
