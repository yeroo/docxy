//! Table geometry and look for the document view (#705): where each cell sits
//! on the grid, how wide it is, which rows a vertical merge covers, and its
//! fill, borders, diagonals and vertical alignment, resolved through
//! docxcore's table-style resolver. Pure, so it is tested without a window;
//! `table_el` in main.rs turns it into elements.

use docxcore::border_style::BorderStyle;
use docxcore::model::{Table, VMerge};
use docxcore::table::{GridMap, cell_props, usable_grid};
use docxcore::table_props::{BorderLine, VAlign};
use docxcore::table_styles::{CellLook, RunLook, TableStyle, resolve};

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
    /// The table style's run formatting for the cell's text (#765).
    pub run: RunLook,
}

/// One drawn stroke of a cell's side, in px at the drawn zoom (#765).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    /// From the cell's edge to the stroke.
    pub inset: f32,
    pub thick: f32,
    /// Where it stops short of the side's two ends (top or left first), so
    /// a compound frame's inner lines meet at the corners, not cross.
    pub start: f32,
    pub end: f32,
}

impl CellBox {
    /// The strokes of each side (top, bottom, left, right) at `zoom`. A
    /// side's k-th stroke from the edge ends at the k-th stroke of the side
    /// it meets, when that side has one, as the PDF export frames a page.
    pub fn strokes_at(&self, zoom: f32) -> [Vec<Stroke>; 4] {
        let at = |l: Option<Line>| l.map(|l| l.strokes_at(zoom)).unwrap_or_default();
        let sides = [at(self.top), at(self.bottom), at(self.left), at(self.right)];
        let meet = |side: &[(f32, f32)], k: usize| side.get(k).map_or(0.0, |&(inset, _)| inset);
        let mut out: [Vec<Stroke>; 4] = Default::default();
        for (which, own) in sides.iter().enumerate() {
            // A horizontal side meets the left and right ones; a vertical
            // side the top and bottom ones.
            let (a, b) = if which < 2 { (2, 3) } else { (0, 1) };
            out[which] = own
                .iter()
                .enumerate()
                .map(|(k, &(inset, thick))| Stroke {
                    inset,
                    thick,
                    start: meet(&sides[a], k),
                    end: meet(&sides[b], k),
                })
                .collect();
        }
        out
    }
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

/// The narrowest a fitted column is scaled to (#1895): the cell's 216-twip
/// margins and room for a few characters. A grid column already narrower
/// (an HTML spacer) has no floor: it scales with the others.
pub const MIN_FIT_COL_TW: u32 = 432;

/// The widest pct a table is drawn at (#1895): 10000%, in fiftieths. A
/// mail body is outside input; anything above is drawn at this.
const MAX_FIT_PCT: u64 = 5000 * 100;

/// The widest a fitted table is drawn (#1895), in twips (about 694"): far
/// past any view, and small enough that its columns' sums fit a `u32`.
const MAX_FIT_TW: u64 = 1_000_000;

/// The width a table asks for in a flow view (#1895), from its `w:tblW`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Wants {
    /// `pct`, in fiftieths of a percent (5000 = 100%), at most
    /// [`MAX_FIT_PCT`].
    Pct(u64),
    /// `auto`, `nil`, a missing `w:tblW`, or a pct of 0 or unreadable: at
    /// most the room there is.
    Auto,
    /// `dxa` (or no type): its own widths.
    Fixed,
}

fn wants(t: &Table) -> Wants {
    let p = docxcore::table::table_props(t);
    if p.get("w:tblW").is_none() {
        return Wants::Auto;
    }
    match p.attr("w:tblW", "w:type").as_deref().unwrap_or("dxa") {
        "pct" => {
            let w = p.attr("w:tblW", "w:w").unwrap_or_default();
            let w = w.trim();
            // The schema also allows "50%" (ST_MeasurementOrPercent).
            let fiftieths = match w.strip_suffix('%') {
                Some(n) => n
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v > 0.)
                    .map(|v| (v * 50.).round() as u64),
                None => w.parse::<u64>().ok(),
            };
            fiftieths
                .filter(|&f| f > 0)
                .map_or(Wants::Auto, |f| Wants::Pct(f.min(MAX_FIT_PCT)))
        }
        "auto" | "nil" => Wants::Auto,
        _ => Wants::Fixed,
    }
}

/// The grid widths `t` is drawn at (#1895). `fit` is the text width (twips)
/// a flow view gives it; `None` (Print Layout, headers and footers) draws
/// the grid as it is. A `pct` table takes its share of `fit`, an `auto` one
/// shrinks to it when wider, a `dxa` one keeps its grid. The columns scale
/// in proportion, summing exactly to the target (the remainder goes to the
/// last), then any that was at least [`MIN_FIT_COL_TW`] wide and fell below
/// it is lifted back to it, so a table that cannot shrink comes out wider
/// than `fit`. The target is at most [`MAX_FIT_TW`], so no sum overflows.
pub fn fitted_grid(t: &Table, fit: Option<u32>) -> Vec<u32> {
    let grid = usable_grid(t);
    let Some(fit) = fit else { return grid };
    let sum: u64 = grid.iter().map(|&w| u64::from(w)).sum();
    if sum == 0 {
        return grid;
    }
    let target = match wants(t) {
        Wants::Pct(f) => u64::from(fit) * f / 5000,
        Wants::Auto => sum.min(u64::from(fit)),
        Wants::Fixed => return grid,
    }
    .min(MAX_FIT_TW);
    if target == sum {
        return grid;
    }
    let mut out: Vec<u32> = grid
        .iter()
        .map(|&w| (u64::from(w) * target / sum) as u32)
        .collect();
    let given: u64 = out.iter().map(|&w| u64::from(w)).sum();
    if let Some(last) = out.last_mut() {
        *last += (target - given) as u32;
    }
    // A column at least the floor wide stops there; one already narrower
    // (an HTML spacer) scales with the rest, so it is never widened and the
    // table still meets its target.
    for (w, &g) in out.iter_mut().zip(&grid) {
        if g >= MIN_FIT_COL_TW {
            *w = (*w).max(MIN_FIT_COL_TW);
        }
    }
    out
}

/// Lay out `t` with its table style (if any), its grid fitted to `fit`
/// ([`fitted_grid`]).
pub fn layout(t: &Table, style: Option<&TableStyle>, fit: Option<u32>) -> Vec<RowBox> {
    let map = GridMap::of(t);
    // A missing, short or zero-width grid is filled out as the table
    // commands repair it, so the view and the edits agree.
    let widths = fitted_grid(t, fit);
    // Saturating: a document's grid may hold any u32 (#1895 r1).
    let span_w = |s: usize, n: usize| {
        widths
            .iter()
            .skip(s)
            .take(n)
            .fold(0u32, |a, &w| a.saturating_add(w))
    };
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
                        run: look.run.clone(),
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
/// Side by side, the cells are next to each other in a row. One above the
/// other, a cell faces the run of cells across the edge that exactly covers
/// its grid columns (one cell, or several narrower ones). A line wins over a
/// narrower one, else the left or upper cell's does; an edge neither cell
/// draws is left to the left or upper cell's gridline. When the wide cell's
/// line loses to one of the narrow cells', the narrow cells draw the edge,
/// each with the wide cell's line where that one wins. Cells across the edge
/// that only partly overlap (a staircase) each keep their own edges.
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
        let (above, below) = (&mut above[r - 1].cells, &mut below[0].cells);
        // The cells of `cells` that exactly cover `wide`'s grid columns.
        fn tiling(cells: &[CellBox], wide: &CellBox) -> Option<Vec<usize>> {
            let end = wide.start + wide.span;
            let ks: Vec<usize> = (0..cells.len())
                .filter(|&k| cells[k].start >= wide.start && cells[k].start + cells[k].span <= end)
                .collect();
            let mut at = wide.start;
            for &k in &ks {
                if cells[k].start != at {
                    return None;
                }
                at += cells[k].span;
            }
            (at == end && !ks.is_empty()).then_some(ks)
        }
        // One cell above over the cells below that cover it, then one cell
        // below under several above (one-to-one pairs are taken first).
        for ai in 0..above.len() {
            let Some(bs) = tiling(below, &above[ai]) else {
                continue;
            };
            // A merge's continued part has no top edge to share.
            if bs.iter().any(|&k| below[k].continues) {
                continue;
            }
            let wide = above[ai].bottom;
            if bs.iter().all(|&k| !later_wins(wide, below[k].top)) {
                for &k in &bs {
                    below[k].top = None;
                    below[k].neighbour_draws[TOP] = true;
                }
            } else {
                for &k in &bs {
                    if !later_wins(wide, below[k].top) {
                        below[k].top = wide;
                    }
                }
                above[ai].bottom = None;
                above[ai].neighbour_draws[BOTTOM] = true;
            }
        }
        for bi in 0..below.len() {
            if below[bi].continues {
                continue;
            }
            let Some(as_) = tiling(above, &below[bi]).filter(|v| v.len() > 1) else {
                continue;
            };
            let wide = below[bi].top;
            if as_.iter().all(|&k| later_wins(above[k].bottom, wide)) {
                for &k in &as_ {
                    above[k].bottom = None;
                    above[k].neighbour_draws[BOTTOM] = true;
                }
            } else {
                for &k in &as_ {
                    if later_wins(above[k].bottom, wide) {
                        above[k].bottom = wide;
                    }
                }
                below[bi].top = None;
                below[bi].neighbour_draws[TOP] = true;
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
        let rows = layout(&t, None, None);
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
        let heights: Vec<_> = layout(&t, None, None).iter().map(|r| r.height).collect();
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
            layout(&new_table(1, 1, 9000, AutoFit::Default), None, None)[0].height,
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
        let rows = layout(&t, None, None);
        assert!(rows.iter().all(RowBox::exact));
        // A merge from row 0 down over row 1: row 0 grows to fit; row 1,
        // which only continues it, stays exact.
        t.rows[0].cells[0].v_merge = VMerge::Restart;
        t.rows[1].cells[0].v_merge = VMerge::Continue;
        let rows = layout(&t, None, None);
        assert!(rows[0].cells[0].merges_down);
        assert!(!rows[0].exact(), "the merge's first row grows");
        assert!(rows[1].exact());
        assert!(rows[2].exact());
        // An at-least row or an unset height is never exact.
        t.rows[2].raw_props = vec!["<w:trPr><w:trHeight w:val=\"400\"/></w:trPr>".into()];
        t.rows[1].raw_props.clear();
        let rows = layout(&t, None, None);
        assert!(!rows[1].exact());
        assert!(!rows[2].exact());
    }

    #[test]
    fn a_missing_grid_falls_back_to_equal_columns() {
        let mut t = new_table(1, 3, 9000, AutoFit::Default);
        t.grid = vec![0, 0, 0];
        let rows = layout(&t, None, None);
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
        let rows = layout(&t, grid.as_ref(), None);
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
        let rows = layout(&t, Some(&st), None);
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
        let rows = layout(&t, lookup_style(None, "TableGrid").as_ref(), None);
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
        let rows = layout(&t, grid.as_ref(), None);
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
        let rows = layout(&t, lookup_style(None, "TableGrid").as_ref(), None);
        assert!(rows[0].cells[0].right.is_none());
        assert!(rows[0].cells[0].neighbour_draws[3]);
        assert_eq!(rows[0].cells[1].left.map(|l| l.color), Some(Some(0xFF0000)));
        // A gridline edge (no line either side) is the left cell's alone.
        let rows = layout(&new_table(1, 2, 9000, AutoFit::Default), None, None);
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
        let rows = layout(&t, grid.as_ref(), None);
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
        let rows = layout(&t, grid.as_ref(), None);
        assert!(rows[1].cells[0].top.is_none());
        assert!(!rows[1].cells[0].neighbour_draws[0]);
        assert!(rows[1].cells[0].bottom.is_some());
        assert!(rows[2].cells[0].top.is_none());
        assert!(rows[2].cells[0].neighbour_draws[0]);
    }

    #[test]
    fn a_wide_cell_shares_its_edge_with_the_cells_that_cover_it_765() {
        let grid = lookup_style(None, "TableGrid");
        // A two-column header over two cells, and two cells over a wide
        // last row.
        let mut t = new_table(3, 2, 9000, AutoFit::Default);
        t.rows[0].cells.remove(1);
        t.rows[0].cells[0].grid_span = 2;
        t.rows[2].cells.remove(1);
        t.rows[2].cells[0].grid_span = 2;
        let rows = layout(&t, grid.as_ref(), None);
        // The header draws the edge once; the cells under it leave it.
        assert!(rows[0].cells[0].bottom.is_some());
        for c in &rows[1].cells {
            assert!(c.top.is_none() && c.neighbour_draws[0]);
        }
        // The upper cells draw the edge over the wide last row.
        for c in &rows[1].cells {
            assert!(c.bottom.is_some() && !c.neighbour_draws[1]);
        }
        assert!(rows[2].cells[0].top.is_none() && rows[2].cells[0].neighbour_draws[0]);

        // A wider line under the header wins its part of the edge; the
        // other part takes the header's line.
        edit_cell_props(&mut t.rows[1].cells[1], |p| {
            p.set("<w:tcBorders><w:top w:val=\"double\" w:sz=\"12\" w:color=\"FF0000\"/></w:tcBorders>")
        });
        let rows = layout(&t, grid.as_ref(), None);
        assert!(rows[0].cells[0].bottom.is_none() && rows[0].cells[0].neighbour_draws[1]);
        assert_eq!(rows[1].cells[1].top.map(|l| l.color), Some(Some(0xFF0000)));
        let header = layout(&t, grid.as_ref(), None)[0].cells[0].left;
        assert_eq!(rows[1].cells[0].top, header, "the header's single line");
    }

    #[test]
    fn a_double_frames_inner_lines_meet_at_the_corners_765() {
        let mut t = new_table(1, 1, 9000, AutoFit::Default);
        edit_table_props(&mut t, |p| {
            p.set(
                "<w:tblBorders><w:top w:val=\"double\" w:sz=\"4\"/><w:left w:val=\"double\" w:sz=\"4\"/>                 <w:bottom w:val=\"single\" w:sz=\"4\"/><w:right w:val=\"double\" w:sz=\"4\"/></w:tblBorders>",
            )
        });
        let rows = layout(&t, None, None);
        let [top, bottom, left, right] = rows[0].cells[0].strokes_at(1.0);
        // The outer top line runs edge to edge; the inner one stops at the
        // left and right sides' inner lines.
        assert_eq!(
            top[0],
            Stroke {
                inset: 0.0,
                thick: 1.0,
                start: 0.0,
                end: 0.0
            }
        );
        assert_eq!(
            top[1],
            Stroke {
                inset: 2.0,
                thick: 1.0,
                start: 2.0,
                end: 2.0
            }
        );
        // The inner left line stops at the top's inner line; the bottom has
        // a single line, so it runs to the bottom edge.
        assert_eq!((left[1].start, left[1].end), (2.0, 0.0));
        assert_eq!((right[1].start, right[1].end), (2.0, 0.0));
        assert_eq!(bottom.len(), 1);
        assert_eq!((bottom[0].start, bottom[0].end), (0.0, 0.0));
    }

    #[test]
    fn a_style_header_is_bold_white_and_a_band_takes_its_fills_ink_765() {
        use docxcore::model::RunProps;
        let mut t = new_table(3, 2, 9000, AutoFit::Default);
        edit_table_props(&mut t, |p| {
            p.remove("w:tblBorders");
            p.set("<w:tblStyle w:val=\"GridTable5Dark-Accent1\"/>");
            p.set(&TblLook::default().to_xml());
        });
        let rows = layout(
            &t,
            lookup_style(None, "GridTable5Dark-Accent1").as_ref(),
            None,
        );
        // The header: the style's white, bold, whatever the ink.
        let run = rows[0].cells[1].run.under(&RunProps::default());
        assert!(run.bold);
        assert_eq!(run.color.as_deref(), Some("FFFFFF"));
        // A direct colour still wins there.
        let red = RunProps {
            color: Some("FF0000".into()),
            ..RunProps::default()
        };
        assert_eq!(
            rows[0].cells[1].run.under(&red).color.as_deref(),
            Some("FF0000")
        );
        // A band cell: no style colour, so its text takes the fill's ink.
        let band = &rows[1].cells[1];
        assert_eq!(band.run.under(&RunProps::default()).color, None);
        assert_eq!(
            crate::design_tab::cell_ink(band.fill).map(|i| i.fg),
            Some(0x202020)
        );
    }

    /// A one-row table with `grid` and the given `w:tblW` (none when `None`).
    fn sized(grid: &[u32], tblw: Option<&str>) -> Table {
        let mut t = new_table(1, grid.len(), 9360, AutoFit::Default);
        t.grid = grid.to_vec();
        edit_table_props(&mut t, |p| {
            p.remove("w:tblW");
            if let Some(w) = tblw {
                p.set(w);
            }
        });
        t
    }

    fn widths(t: &Table, fit: Option<u32>) -> Vec<u32> {
        layout(t, None, fit)[0]
            .cells
            .iter()
            .map(|c| c.width)
            .collect()
    }

    const PCT_100: &str = "<w:tblW w:w=\"5000\" w:type=\"pct\"/>";

    #[test]
    fn a_pct_table_takes_its_share_of_the_view_in_its_grids_proportions_1895() {
        let t = sized(&[4680, 4680], Some(PCT_100));
        assert_eq!(widths(&t, Some(6000)), [3000, 3000]);
        let t = sized(&[6000, 2000], Some(PCT_100));
        assert_eq!(widths(&t, Some(6000)), [4500, 1500]);
        // Rounding: the widths sum exactly to the target, the remainder last.
        let t = sized(&[3000, 3000, 3000], Some(PCT_100));
        assert_eq!(widths(&t, Some(4000)), [1333, 1333, 1334]);
        // Half the view.
        let t = sized(&[4680, 4680], Some("<w:tblW w:w=\"2500\" w:type=\"pct\"/>"));
        assert_eq!(widths(&t, Some(6000)), [1500, 1500]);
        // Narrower than the view: it grows.
        let t = sized(&[4680, 4680], Some(PCT_100));
        assert_eq!(widths(&t, Some(12000)), [6000, 6000]);
    }

    #[test]
    fn an_auto_table_shrinks_to_the_view_and_never_grows_1895() {
        for tblw in [Some("<w:tblW w:w=\"0\" w:type=\"auto\"/>"), None] {
            let t = sized(&[6000, 3000], tblw);
            assert_eq!(widths(&t, Some(6000)), [4000, 2000], "{tblw:?}");
            assert_eq!(widths(&t, Some(12000)), [6000, 3000], "{tblw:?}");
        }
        let t = sized(&[6000, 3000], Some("<w:tblW w:w=\"0\" w:type=\"nil\"/>"));
        assert_eq!(widths(&t, Some(6000)), [4000, 2000]);
    }

    #[test]
    fn a_dxa_table_keeps_its_grid_1895() {
        for tblw in [
            "<w:tblW w:w=\"12000\" w:type=\"dxa\"/>",
            "<w:tblW w:w=\"12000\"/>",
        ] {
            let t = sized(&[6000, 6000], Some(tblw));
            assert_eq!(widths(&t, Some(6000)), [6000, 6000], "{tblw}");
        }
    }

    #[test]
    fn without_a_fit_every_table_draws_at_its_grid_1895() {
        for tblw in [
            Some(PCT_100),
            Some("<w:tblW w:w=\"2500\" w:type=\"pct\"/>"),
            Some("<w:tblW w:w=\"0\" w:type=\"auto\"/>"),
            Some("<w:tblW w:w=\"9000\" w:type=\"dxa\"/>"),
            None,
        ] {
            let t = sized(&[6000, 3000], tblw);
            assert_eq!(widths(&t, None), [6000, 3000], "{tblw:?}");
            assert_eq!(fitted_grid(&t, None), usable_grid(&t), "{tblw:?}");
        }
    }

    #[test]
    fn a_column_stops_at_its_floor_and_a_spacer_is_never_widened_1895() {
        // Twenty columns in 2000 twips: each stops at the floor, so the
        // table comes out wider than the view (it scrolls).
        let t = sized(&[1000; 20], Some(PCT_100));
        let w = widths(&t, Some(2000));
        assert!(w.iter().all(|&c| c == MIN_FIT_COL_TW), "{w:?}");
        assert!(w.iter().sum::<u32>() > 2000);
        // A 300-twip spacer between two wide columns, shrunk: the spacer
        // stays at most its own width and the sum is the target.
        let t = sized(&[4000, 300, 4000], Some(PCT_100));
        let w = widths(&t, Some(6000));
        assert!(w[1] <= 300, "{w:?}");
        assert_eq!(w.iter().sum::<u32>(), 6000, "{w:?}");
    }

    #[test]
    fn pct_widths_read_as_the_schema_writes_them_1895() {
        // 0 or unreadable: auto (shrinks to the view, never grows).
        for w in ["0", "wide", ""] {
            let tblw = format!("<w:tblW w:w=\"{w}\" w:type=\"pct\"/>");
            let t = sized(&[6000, 3000], Some(&tblw));
            assert_eq!(widths(&t, Some(6000)), [4000, 2000], "{w}");
            assert_eq!(widths(&t, Some(12000)), [6000, 3000], "{w}");
        }
        let t = sized(&[4680, 4680], Some("<w:tblW w:w=\"50%\" w:type=\"pct\"/>"));
        assert_eq!(widths(&t, Some(6000)), [1500, 1500]);
        // Above 100% is kept: wider than the view.
        let t = sized(&[4680, 4680], Some("<w:tblW w:w=\"7500\" w:type=\"pct\"/>"));
        assert_eq!(widths(&t, Some(6000)), [4500, 4500]);
    }

    #[test]
    fn an_unbounded_pct_is_drawn_at_most_its_cap_1895() {
        // 4000000000 fiftieths and 1e30%: 10000% at most, and the target at
        // most MAX_FIT_TW, so nothing overflows (a debug build panics).
        for w in ["4000000000", "1e30%", "99999999999999999999%"] {
            let tblw = format!("<w:tblW w:w=\"{w}\" w:type=\"pct\"/>");
            let t = sized(&[4680, 4680], Some(&tblw));
            assert_eq!(widths(&t, Some(6000)), [300_000, 300_000], "{w}");
            let w = widths(&t, Some(u32::MAX));
            assert_eq!(w.iter().map(|&c| u64::from(c)).sum::<u64>(), MAX_FIT_TW);
        }
        // A grid of huge columns (their u32 sum stays below u32::MAX, which
        // docxcore's usable_grid needs) scaled to the cap.
        let t = sized(&[1_000_000_000, 1_000_000_000], Some(PCT_100));
        let w = widths(&t, Some(u32::MAX));
        assert_eq!(w.iter().map(|&c| u64::from(c)).sum::<u64>(), MAX_FIT_TW);
        assert_eq!(w, [500_000, 500_000]);
    }

    #[test]
    fn grid_before_scales_with_the_columns_1895() {
        let mut t = new_table(2, 2, 9000, AutoFit::Window);
        t.grid = vec![2000, 4000, 4000];
        t.rows[1].cells.remove(0);
        t.rows[0].cells[0].grid_span = 2;
        t.rows[1].raw_props = vec!["<w:trPr><w:gridBefore w:val=\"1\"/></w:trPr>".into()];
        let rows = layout(&t, None, Some(5000));
        assert_eq!(rows[0].before, 0);
        assert_eq!(
            rows[0].cells.iter().map(|c| c.width).collect::<Vec<_>>(),
            [3000, 2000]
        );
        assert_eq!(rows[1].before, 1000);
        assert_eq!(rows[1].cells[0].width, 2000);
    }
}
