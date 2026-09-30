//! Table geometry and look for the document view (#705): where each cell sits
//! on the grid, how wide it is, which rows a vertical merge covers, and its
//! fill, borders, diagonals and vertical alignment, resolved through
//! docxcore's table-style resolver. Pure, so it is tested without a window;
//! `table_el` in main.rs turns it into elements.

use docxcore::model::{Table, VMerge};
use docxcore::table::{DEFAULT_TEXT_WIDTH, GridMap, cell_props};
use docxcore::table_props::{BorderLine, VAlign};
use docxcore::table_styles::{CellLook, TableStyle, resolve};

/// A drawn border line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line {
    /// `0xRRGGBB`, `None` for automatic (the text colour).
    pub color: Option<u32>,
    /// Thickness in px at 100% zoom.
    pub width: f32,
}

impl Line {
    fn of(l: &BorderLine) -> Option<Line> {
        if !l.visible() {
            return None;
        }
        // `w:sz` is eighths of a point; a point is 4/3 px.
        let mut width = (l.sz as f32 / 8.0 * 4.0 / 3.0).max(1.0);
        if l.val == "double" {
            width = width.max(3.0);
        }
        Some(Line {
            color: crate::hex_rgb(&l.color),
            width,
        })
    }
}

/// One cell's box.
#[derive(Debug, Clone, PartialEq)]
pub struct CellBox {
    /// Index in the row's `cells`.
    pub cell: usize,
    /// First grid column and span.
    pub start: usize,
    pub span: usize,
    /// Width in twips.
    pub width: u32,
    /// A `continue` part of a vertical merge: drawn empty, no top edge.
    pub continues: bool,
    pub fill: Option<u32>,
    pub top: Option<Line>,
    pub left: Option<Line>,
    pub bottom: Option<Line>,
    pub right: Option<Line>,
    pub diag_down: Option<Line>,
    pub diag_up: Option<Line>,
    pub valign: VAlign,
}

/// One row: the width skipped before its first cell (`w:gridBefore`), then
/// its cells.
#[derive(Debug, Clone, PartialEq)]
pub struct RowBox {
    pub before: u32,
    pub cells: Vec<CellBox>,
}

/// The grid widths to draw with: the table grid, filled out when it is
/// missing or has zero-width columns.
pub fn grid_widths(t: &Table, map: &GridMap) -> Vec<u32> {
    let n = map.width(t).max(1);
    let known: Vec<u32> = t.grid.iter().copied().filter(|&w| w > 0).collect();
    let fallback = if known.is_empty() {
        DEFAULT_TEXT_WIDTH / n as u32
    } else {
        known.iter().sum::<u32>() / known.len() as u32
    };
    (0..n)
        .map(|i| {
            t.grid
                .get(i)
                .copied()
                .filter(|&w| w > 0)
                .unwrap_or(fallback)
        })
        .collect()
}

/// Lay out `t` with its table style (if any).
pub fn layout(t: &Table, style: Option<&TableStyle>) -> Vec<RowBox> {
    let map = GridMap::of(t);
    let widths = grid_widths(t, &map);
    let span_w = |s: usize, n: usize| widths.iter().skip(s).take(n).sum::<u32>();
    let looks = resolve(t, style);
    map.rows
        .iter()
        .enumerate()
        .map(|(r, rm)| RowBox {
            before: span_w(0, rm.before),
            cells: rm
                .cells
                .iter()
                .enumerate()
                .map(|(ci, &(s, n))| {
                    let cell = &t.rows[r].cells[ci];
                    let look: &CellLook = &looks[r][ci];
                    let continues = cell.v_merge == VMerge::Continue;
                    let continued_below = map
                        .rows
                        .get(r + 1)
                        .and_then(|m| m.cell_starting(s))
                        .is_some_and(|b| t.rows[r + 1].cells[b].v_merge == VMerge::Continue);
                    let line = |l: &Option<BorderLine>| l.as_ref().and_then(Line::of);
                    CellBox {
                        cell: ci,
                        start: s,
                        span: n,
                        width: span_w(s, n),
                        continues,
                        fill: look.fill.as_deref().and_then(crate::hex_rgb),
                        top: if continues { None } else { line(&look.top) },
                        left: line(&look.left),
                        bottom: if continued_below {
                            None
                        } else {
                            line(&look.bottom)
                        },
                        right: line(&look.right),
                        diag_down: line(&look.diag_down),
                        diag_up: line(&look.diag_up),
                        valign: cell_props(cell)
                            .attr("w:vAlign", "w:val")
                            .map_or(VAlign::Top, |v| VAlign::parse(&v)),
                    }
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::table::{AutoFit, edit_cell_props, edit_table_props, new_table};
    use docxcore::table_props::TblLook;
    use docxcore::table_styles::lookup_style;

    #[test]
    fn widths_follow_the_grid_spans_and_grid_before() {
        let mut t = new_table(2, 3, 9000, AutoFit::Default);
        t.grid = vec![1000, 2000, 3000];
        t.rows[0].cells.remove(1);
        t.rows[0].cells[0].grid_span = 2;
        t.rows[1].cells.remove(0);
        t.rows[1].raw_props = vec!["<w:trPr><w:gridBefore w:val=\"1\"/></w:trPr>".into()];
        let rows = layout(&t, None);
        assert_eq!(rows[0].before, 0);
        assert_eq!(
            rows[0].cells.iter().map(|c| c.width).collect::<Vec<_>>(),
            [3000, 3000]
        );
        assert_eq!(rows[1].before, 1000);
        assert_eq!(rows[1].cells[0].start, 1);
    }

    #[test]
    fn a_missing_grid_falls_back_to_equal_columns() {
        let mut t = new_table(1, 3, 9000, AutoFit::Default);
        t.grid = vec![0, 0, 0];
        let rows = layout(&t, None);
        assert!(
            rows[0]
                .cells
                .iter()
                .all(|c| c.width == DEFAULT_TEXT_WIDTH / 3)
        );
    }

    #[test]
    fn a_vertical_merge_has_no_edge_between_its_rows() {
        let mut t = new_table(2, 1, 9000, AutoFit::Default);
        t.rows[0].cells[0].v_merge = VMerge::Restart;
        t.rows[1].cells[0].v_merge = VMerge::Continue;
        let grid = lookup_style(None, "TableGrid");
        let rows = layout(&t, grid.as_ref());
        assert!(rows[0].cells[0].top.is_some());
        assert!(rows[0].cells[0].bottom.is_none());
        assert!(rows[1].cells[0].top.is_none());
        assert!(rows[1].cells[0].bottom.is_some());
        assert!(rows[1].cells[0].continues);
    }

    #[test]
    fn fill_borders_and_valign_from_the_cell_over_the_style() {
        let mut t = new_table(3, 2, 9000, AutoFit::Default);
        edit_table_props(&mut t, |p| {
            p.remove("w:tblBorders");
            p.set("<w:tblStyle w:val=\"GridTable4-Accent1\"/>");
            p.set(&TblLook::default().to_xml());
        });
        edit_cell_props(&mut t.rows[1].cells[1], |p| {
            p.set("<w:tcBorders><w:right w:val=\"single\" w:sz=\"16\" w:color=\"FF0000\"/><w:tr2bl w:val=\"single\" w:sz=\"4\" w:color=\"auto\"/></w:tcBorders>");
            p.set("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"00FF00\"/>");
            p.set("<w:vAlign w:val=\"bottom\"/>");
        });
        let st = lookup_style(None, "GridTable4-Accent1").unwrap();
        let rows = layout(&t, Some(&st));
        assert_eq!(
            rows[0].cells[0].fill,
            Some(0x4472C4),
            "header row from the style"
        );
        assert_eq!(rows[1].cells[0].fill, Some(0xD9E2F3), "banded row");
        let c = &rows[1].cells[1];
        assert_eq!(c.fill, Some(0x00FF00));
        assert_eq!(
            c.right,
            Some(Line {
                color: Some(0xFF0000),
                width: 16.0 / 6.0
            })
        );
        assert_eq!(c.diag_up.map(|l| l.color), Some(None));
        assert_eq!(c.valign, VAlign::Bottom);
        assert_eq!(rows[2].cells[0].fill, None, "second band has no fill");
    }

    #[test]
    fn nil_borders_draw_nothing() {
        let mut t = new_table(1, 1, 9000, AutoFit::Default);
        edit_cell_props(&mut t.rows[0].cells[0], |p| {
            p.set("<w:tcBorders><w:top w:val=\"nil\"/></w:tcBorders>")
        });
        let rows = layout(&t, lookup_style(None, "TableGrid").as_ref());
        assert!(rows[0].cells[0].top.is_none());
        assert!(rows[0].cells[0].left.is_some());
    }
}
