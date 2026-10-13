//! Table geometry and look for the document view (#705): where each cell sits
//! on the grid, how wide it is, which rows a vertical merge covers, and its
//! fill, borders, diagonals and vertical alignment, resolved through
//! docxcore's table-style resolver. Pure, so it is tested without a window;
//! `table_el` in main.rs turns it into elements.

use docxcore::border_style::BorderStyle;
use docxcore::model::{Table, VMerge};
use docxcore::table::{GridMap, cell_props, usable_grid};
use docxcore::table_props::{BorderLine, VAlign};
use docxcore::table_styles::{CellLook, TableStyle, resolve};

/// A drawn border line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Line {
    /// `0xRRGGBB`, `None` for automatic (the text colour).
    pub color: Option<u32>,
    /// The band its strokes cover, in px at 100% zoom.
    pub width: f32,
    /// Its solid strokes (#765), innermost first: each one's width and the
    /// distance of its centre line from the first one's, in px at 100% zoom.
    /// A double or triple line has two or three, a thin-thick one two of
    /// unequal widths; every other style one, drawn solid (dashes, dots and
    /// art are not drawn).
    strokes: [(f32, f32); 3],
    count: usize,
}

impl Line {
    fn of(l: &BorderLine) -> Option<Line> {
        if !l.visible() {
            return None;
        }
        let style = BorderStyle::parse(&l.val)?;
        // `w:sz` is eighths of a point; a point is 4/3 px.
        let w = l.sz as f32 / 8.0 * 4.0 / 3.0;
        let mut strokes = [(0.0, 0.0); 3];
        let all = style.strokes(w);
        let count = all.len().min(3);
        strokes[..count].copy_from_slice(&all[..count]);
        Some(Line {
            color: crate::hex_rgb(&l.color),
            width: style.total(w),
            strokes,
            count,
        })
    }

    /// The strokes at `zoom`, outermost first: each one's distance in px
    /// from the cell's edge, and its width. A stroke and a gap are each at
    /// least a pixel, so a hairline double still shows two lines.
    pub fn strokes_at(&self, zoom: f32) -> Vec<(f32, f32)> {
        let src = &self.strokes[..self.count];
        let mut inner_first = Vec::with_capacity(src.len());
        let mut at = 0.0;
        for (k, &(w, d)) in src.iter().enumerate() {
            if k > 0 {
                let (pw, pd) = src[k - 1];
                at += ((d - pd - pw / 2.0 - w / 2.0) * zoom).max(1.0);
            }
            let width = (w * zoom).max(1.0);
            inner_first.push((at, width));
            at += width;
        }
        // The innermost stroke is the one furthest from the edge.
        inner_first
            .into_iter()
            .rev()
            .map(|(from_inner, width)| (at - from_inner - width, width))
            .collect()
    }

    /// The width a single-stroke drawing (a diagonal) uses: its widest
    /// stroke, in px at 100% zoom.
    pub fn stroke_width(&self) -> f32 {
        self.strokes[..self.count]
            .iter()
            .map(|&(w, _)| w)
            .fold(0.0, f32::max)
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
    /// The first part of a vertical merge: its content runs on down over
    /// the merged rows.
    pub merges_down: bool,
    pub fill: Option<u32>,
    pub top: Option<Line>,
    pub left: Option<Line>,
    pub bottom: Option<Line>,
    pub right: Option<Line>,
    pub diag_down: Option<Line>,
    pub diag_up: Option<Line>,
    pub valign: VAlign,
    /// Top, bottom, left, right: whether the cell beside this one draws the
    /// edge they share (#765), so this side draws nothing, not even a
    /// gridline.
    pub neighbour_draws: [bool; 4],
}

/// One row: the width skipped before its first cell (`w:gridBefore`), then
/// its cells.
#[derive(Debug, Clone, PartialEq)]
pub struct RowBox {
    pub before: u32,
    pub cells: Vec<CellBox>,
    /// `w:trHeight` in twips, and whether it is exact (else at least): a
    /// drawn table's row keeps the height drawn (#763).
    pub height: Option<(u32, bool)>,
}

/// A row's `w:trHeight`: twips, and whether its rule is exact. An auto rule
/// or a zero height sets none.
fn row_height(row: &docxcore::model::Row) -> Option<(u32, bool)> {
    match docxcore::table_props::row_height(&row.raw_props)? {
        (_, Some(rule)) if rule == "auto" => None,
        (0, _) => None,
        (h, rule) => Some((h, rule.as_deref() == Some("exact"))),
    }
}

impl RowBox {
    /// Whether the row is drawn at exactly its set height, its cells cut off
    /// at its edges. A row starting a vertical merge grows to fit instead,
    /// as every row did before the view drew heights: the merged cell's
    /// content is drawn in this row alone, and one row's exact height would
    /// cut it off or spill it over the rows around.
    pub fn exact(&self) -> bool {
        matches!(self.height, Some((_, true))) && !self.cells.iter().any(|c| c.merges_down)
    }
}

/// Lay out `t` with its table style (if any).
pub fn layout(t: &Table, style: Option<&TableStyle>) -> Vec<RowBox> {
    let map = GridMap::of(t);
    // A missing, short or zero-width grid is filled out as the table
    // commands repair it, so the view and the edits agree.
    let widths = usable_grid(t);
    let span_w = |s: usize, n: usize| widths.iter().skip(s).take(n).sum::<u32>();
    let looks = resolve(t, style);
    let mut rows: Vec<RowBox> = map
        .rows
        .iter()
        .enumerate()
        .map(|(r, rm)| RowBox {
            before: span_w(0, rm.before),
            height: row_height(&t.rows[r]),
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
                        merges_down: !continues && continued_below,
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
                        neighbour_draws: [false; 4],
                    }
                })
                .collect(),
        })
        .collect();
    share_edges(&mut rows);
    rows
}

/// Give each edge two cells share one owner (#765): each cell draws its sides
/// inside its own box, so both drawing it would double every inside line.
/// Side by side, the cells are next to each other in a row; one above the
/// other, the cell above covers the same grid columns. The wider line wins,
/// else the left or upper cell's; an edge neither cell draws is left to the
/// left or upper cell's gridline. Cells above that only partly overlap
/// (ragged rows) each keep their own edges.
fn share_edges(rows: &mut [RowBox]) {
    const TOP: usize = 0;
    const BOTTOM: usize = 1;
    const LEFT: usize = 2;
    const RIGHT: usize = 3;
    // Whether `later` takes the edge from `earlier`: only a wider line.
    let later_wins = |earlier: Option<Line>, later: Option<Line>| match (earlier, later) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(e), Some(l)) => l.width > e.width,
    };
    for row in rows.iter_mut() {
        for k in 1..row.cells.len() {
            let (before, after) = row.cells.split_at_mut(k);
            let (a, b) = (&mut before[k - 1], &mut after[0]);
            if a.start + a.span != b.start {
                continue;
            }
            if later_wins(a.right, b.left) {
                a.right = None;
                a.neighbour_draws[RIGHT] = true;
            } else {
                b.left = None;
                b.neighbour_draws[LEFT] = true;
            }
        }
    }
    for r in 1..rows.len() {
        let (above, below) = rows.split_at_mut(r);
        for b in &mut below[0].cells {
            // A merge's continued part has no top edge to share.
            if b.continues {
                continue;
            }
            let Some(a) = above[r - 1]
                .cells
                .iter_mut()
                .find(|a| a.start == b.start && a.span == b.span)
            else {
                continue;
            };
            if later_wins(a.bottom, b.top) {
                a.bottom = None;
                a.neighbour_draws[BOTTOM] = true;
            } else {
                b.top = None;
                b.neighbour_draws[TOP] = true;
            }
        }
    }
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
    fn a_rows_set_height_is_kept_with_its_rule() {
        let mut t = new_table(4, 1, 9000, AutoFit::Default);
        for (row, xml) in t.rows.iter_mut().zip([
            "<w:trHeight w:val=\"1200\" w:hRule=\"atLeast\"/>",
            "<w:trHeight w:val=\"600\" w:hRule=\"exact\"/>",
            "<w:trHeight w:val=\"600\" w:hRule=\"auto\"/>",
            "<w:trHeight w:val=\"700\"/>",
        ]) {
            row.raw_props = vec![format!("<w:trPr>{xml}</w:trPr>")];
        }
        let heights: Vec<_> = layout(&t, None).iter().map(|r| r.height).collect();
        assert_eq!(
            heights,
            [
                Some((1200, false)),
                Some((600, true)),
                None,
                Some((700, false))
            ]
        );
        assert_eq!(
            layout(&new_table(1, 1, 9000, AutoFit::Default), None)[0].height,
            None
        );
    }

    #[test]
    fn an_exact_row_is_exact_unless_a_merge_starts_in_it() {
        let exact = "<w:trPr><w:trHeight w:val=\"400\" w:hRule=\"exact\"/></w:trPr>";
        let mut t = new_table(3, 2, 9000, AutoFit::Default);
        for row in &mut t.rows {
            row.raw_props = vec![exact.into()];
        }
        let rows = layout(&t, None);
        assert!(rows.iter().all(RowBox::exact));
        // A merge from row 0 down over row 1: row 0 grows to fit; row 1,
        // which only continues it, stays exact.
        t.rows[0].cells[0].v_merge = VMerge::Restart;
        t.rows[1].cells[0].v_merge = VMerge::Continue;
        let rows = layout(&t, None);
        assert!(rows[0].cells[0].merges_down);
        assert!(!rows[0].exact(), "the merge's first row grows");
        assert!(rows[1].exact());
        assert!(rows[2].exact());
        // An at-least row or an unset height is never exact.
        t.rows[2].raw_props = vec!["<w:trPr><w:trHeight w:val=\"400\"/></w:trPr>".into()];
        t.rows[1].raw_props.clear();
        let rows = layout(&t, None);
        assert!(!rows[1].exact());
        assert!(!rows[2].exact());
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
                .all(|c| c.width == docxcore::table::DEFAULT_TEXT_WIDTH / 3)
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
        let right = c.right.expect("a right edge");
        assert_eq!(right.color, Some(0xFF0000));
        assert_eq!(right.width, 16.0 / 6.0);
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

    fn line(val: &str, sz: u32) -> Line {
        Line::of(&BorderLine {
            val: val.into(),
            sz,
            color: "auto".into(),
        })
        .expect("a visible line")
    }

    #[test]
    fn compound_borders_draw_their_strokes_apart_765() {
        // Single: one stroke, a hairline at least a pixel.
        assert_eq!(line("single", 4).strokes_at(1.0), [(0.0, 1.0)]);
        assert_eq!(line("single", 12).strokes_at(1.0), [(0.0, 2.0)]);
        // A half-point double at 100%: two 1px lines a pixel apart.
        assert_eq!(line("double", 4).strokes_at(1.0), [(0.0, 1.0), (2.0, 1.0)]);
        // 1.5pt (2px) at 200%: 4px lines, 4px gaps.
        assert_eq!(
            line("triple", 12).strokes_at(2.0),
            [(0.0, 4.0), (8.0, 4.0), (16.0, 4.0)]
        );
        // Thin-thick, the thin line inner: outermost (thick) first.
        let tt = line("thinThickSmallGap", 18);
        assert_eq!(tt.strokes_at(1.0), [(0.0, 3.0), (4.0, 1.0)]);
        // The band is the style's whole width, as the PDF export draws it.
        assert_eq!(line("double", 12).width, 6.0);
        assert_eq!(line("double", 12).stroke_width(), 2.0);
        assert_eq!(tt.stroke_width(), 3.0);
    }

    #[test]
    fn an_inside_edge_is_drawn_by_one_cell_765() {
        let grid = lookup_style(None, "TableGrid");
        let t = new_table(2, 2, 9000, AutoFit::Default);
        let rows = layout(&t, grid.as_ref());
        // Across: the left cell draws the edge, the right one leaves it.
        assert!(rows[0].cells[0].right.is_some());
        assert!(rows[0].cells[1].left.is_none());
        assert!(rows[0].cells[1].neighbour_draws[2]);
        assert!(!rows[0].cells[0].neighbour_draws[3]);
        // Down: the upper cell draws it.
        assert!(rows[0].cells[1].bottom.is_some());
        assert!(rows[1].cells[1].top.is_none());
        assert!(rows[1].cells[1].neighbour_draws[0]);
        // The outer edges stay.
        assert!(rows[0].cells[0].left.is_some() && rows[0].cells[1].right.is_some());
        assert!(rows[0].cells[0].top.is_some() && rows[1].cells[0].bottom.is_some());
    }

    #[test]
    fn the_wider_line_wins_a_shared_edge_765() {
        let mut t = new_table(1, 2, 9000, AutoFit::Default);
        edit_cell_props(&mut t.rows[0].cells[1], |p| {
            p.set("<w:tcBorders><w:left w:val=\"double\" w:sz=\"4\" w:color=\"FF0000\"/></w:tcBorders>")
        });
        let rows = layout(&t, lookup_style(None, "TableGrid").as_ref());
        assert!(rows[0].cells[0].right.is_none());
        assert!(rows[0].cells[0].neighbour_draws[3]);
        assert_eq!(rows[0].cells[1].left.map(|l| l.color), Some(Some(0xFF0000)));
        // A gridline edge (no line either side) is the left cell's alone.
        let rows = layout(&new_table(1, 2, 9000, AutoFit::Default), None);
        assert!(!rows[0].cells[0].neighbour_draws[3]);
        assert!(rows[0].cells[1].neighbour_draws[2]);
    }

    #[test]
    fn ragged_rows_and_merges_share_only_matching_edges_765() {
        let grid = lookup_style(None, "TableGrid");
        let mut t = new_table(3, 2, 9000, AutoFit::Default);
        // Row 1 starts a grid column in: its one cell spans column 1 only.
        t.rows[1].cells.remove(0);
        t.rows[1].raw_props = vec!["<w:trPr><w:gridBefore w:val=\"1\"/></w:trPr>".into()];
        // Row 2's first cell spans both columns.
        t.rows[2].cells.remove(1);
        t.rows[2].cells[0].grid_span = 2;
        let rows = layout(&t, grid.as_ref());
        // Row 1's cell has no left neighbour, and sits under row 0's second.
        assert!(rows[1].cells[0].left.is_some());
        assert!(rows[1].cells[0].top.is_none());
        assert!(rows[0].cells[1].bottom.is_some());
        // Row 0's first cell has nothing matching below it: its bottom stays.
        assert!(rows[0].cells[0].bottom.is_some());
        // Row 2's wide cell matches nothing above: both edges stay.
        assert!(rows[1].cells[0].bottom.is_some());
        assert!(rows[2].cells[0].top.is_some());

        // A vertical merge: its continued part shares no top edge, and the
        // row under the merge takes its top from the merge's last part.
        let mut t = new_table(3, 1, 9000, AutoFit::Default);
        t.rows[0].cells[0].v_merge = VMerge::Restart;
        t.rows[1].cells[0].v_merge = VMerge::Continue;
        let rows = layout(&t, grid.as_ref());
        assert!(rows[1].cells[0].top.is_none());
        assert!(!rows[1].cells[0].neighbour_draws[0]);
        assert!(rows[1].cells[0].bottom.is_some());
        assert!(rows[2].cells[0].top.is_none());
        assert!(rows[2].cells[0].neighbour_draws[0]);
    }
}
