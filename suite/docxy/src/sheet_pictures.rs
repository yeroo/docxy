//! Where a sheet's floating pictures sit on the grid (#2228).
//!
//! The grid is not drawn at Excel's scale: a row is `row_height_px`, 1.4 px
//! per point with its gridline inside (21 px for 15 pt), and grows with its
//! text; a column's width is `col_px`, clamped to 28..=320 px. A picture
//! placed in Excel's pixels would drift off the cells it covers in Excel, so
//! it is placed in two steps instead:
//!
//! 1. [`anchor_span`]: the anchor becomes a cell-relative box, a [`CellPos`]
//!    (cell, fraction of that cell) at each edge, in Excel's own cell sizes.
//!    A `twoCellAnchor`'s offsets divide by their cell's size; a
//!    `oneCellAnchor`'s extent and an `absoluteAnchor`'s position and extent
//!    walk Excel's sizes from their start.
//! 2. [`offset_px`]: a [`CellPos`] becomes drawn pixels from a pane's first
//!    cell, over the sizes the grid draws.
//!
//! So a picture covers the same cells it covers in Excel, whatever the
//! grid's scale. Both steps are pure, over closures for the sizes, so the
//! tests pin them without a window.

use gridcore::sheet::{AnchorKind, DrawingAnchor, Sheet};

/// One axis of the sheet: rows or columns.
pub struct Axis<'a> {
    /// A cell's size as Excel lays it out, in EMU; 0 for a hidden cell.
    pub excel: &'a dyn Fn(u32) -> i64,
    /// A cell's size as the grid draws it, in px; 0 for a cell
    /// the grid does not draw (a hidden row; a hidden column is drawn).
    pub drawn: &'a dyn Fn(u32) -> f32,
    /// The first cell the grid never draws: nothing at or past it is placed.
    pub end: u32,
}

/// A point along an axis: a cell, and how far into it (0..=1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellPos {
    pub cell: u32,
    pub frac: f32,
}

/// A picture's cell-relative box: its top-left and bottom-right, each as
/// `(row, col)` positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub rows: (CellPos, CellPos),
    pub cols: (CellPos, CellPos),
}

/// The most cells one walk crosses. A crafted extent or position could
/// otherwise make every frame walk a million rows; a real picture ends far
/// sooner, and one that reaches this far is off screen where it is cut.
pub const MAX_WALK: u32 = 1 << 16;

/// EMU per point (a row's height) and per pixel (a column's width).
pub const EMU_PER_PT: i64 = 12700;
pub const EMU_PER_PX: i64 = 9525;

/// Excel's pixel width of a column whose file width is `w` (`<col width>`
/// or `defaultColWidth`: characters, the padding already in it), by
/// ECMA-376 18.3.1.13 with Calibri 11's 7 px maximum digit width:
/// `trunc(((256 w + trunc(128 / 7)) / 256) * 7)`, so 9.140625 is 64 px.
/// Unclamped, unlike the grid's `col_px`.
pub fn excel_col_px(w: f64) -> i64 {
    const MDW: f64 = 7.0;
    let px = ((256.0 * w + (128.0 / MDW).trunc()) / 256.0 * MDW).trunc();
    px.max(0.0) as i64
}

/// A column of file width `w` as Excel lays it out, in EMU; 0 when hidden.
pub fn excel_col_emu(w: f64, hidden: bool) -> i64 {
    if hidden {
        return 0;
    }
    excel_col_px(w) * EMU_PER_PX
}

/// Column `c` of `sh` as Excel lays it out, in EMU: its own `<col width>`,
/// else the sheet's default (`defaultColWidth`, else the width
/// `baseColWidth` gives), 0 when hidden. Not `Sheet::col_width`, which
/// falls back to 8.43 whatever the sheet says.
pub fn sheet_col_emu(sh: &Sheet, c: u32) -> i64 {
    let w = sh
        .col_own_width(c)
        .unwrap_or_else(|| sh.default_col_file_width());
    excel_col_emu(w, sh.col_hidden(c))
}

/// Row `r` of `sh` as Excel lays it out, in EMU ([`excel_row_emu`]).
pub fn sheet_row_emu(sh: &Sheet, r: u32) -> i64 {
    excel_row_emu(
        sh.row_height(r),
        sh.format.default_row_height,
        sh.row_hidden(r),
    )
}

/// A row's height in EMU: its own `ht`, else the sheet's default, else
/// 15 pt; 0 for a hidden row.
pub fn excel_row_emu(ht: Option<f64>, default_ht: Option<f64>, hidden: bool) -> i64 {
    if hidden {
        return 0;
    }
    let pt = ht.or(default_ht).unwrap_or(15.0).max(0.0);
    (pt * EMU_PER_PT as f64).round() as i64
}

/// `off` EMU into `cell`, as a fraction of it (a zero-size cell is 0).
fn frac_of(axis: &Axis, cell: u32, off: i64) -> CellPos {
    let size = (axis.excel)(cell);
    let frac = if size > 0 {
        (off as f64 / size as f64).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    CellPos { cell, frac }
}

/// The point `dist` EMU past `off` EMU into `cell`, walking Excel's sizes.
/// A hidden (zero-size) cell is stepped over. It stops at the axis's end, or
/// after [`MAX_WALK`] cells, at the start of the cell it reached.
fn walk(axis: &Axis, cell: u32, off: i64, dist: i64) -> CellPos {
    let mut left = off.max(0).saturating_add(dist.max(0));
    let mut c = cell;
    let last = cell.saturating_add(MAX_WALK).min(axis.end);
    while c < last {
        let size = (axis.excel)(c);
        if size > 0 && left < size {
            return CellPos {
                cell: c,
                frac: (left as f64 / size as f64) as f32,
            };
        }
        left -= size;
        c += 1;
    }
    CellPos { cell: c, frac: 0.0 }
}

/// Step 1: an anchor's cell-relative box. `from`/`to` are the drawing's
/// `(row, col)` cells (for an `absoluteAnchor`, unused: its `pos` places it).
pub fn anchor_span(
    anchor: &DrawingAnchor,
    from: (u32, u32),
    to: (u32, u32),
    rows: &Axis,
    cols: &Axis,
) -> Span {
    match anchor.kind {
        AnchorKind::TwoCell => Span {
            rows: (
                frac_of(rows, from.0, anchor.from_off.0),
                frac_of(rows, to.0, anchor.to_off.0),
            ),
            cols: (
                frac_of(cols, from.1, anchor.from_off.1),
                frac_of(cols, to.1, anchor.to_off.1),
            ),
        },
        AnchorKind::OneCell => {
            let (cx, cy) = anchor.ext.unwrap_or((0, 0));
            // The start offset is the cell's own, clamped to it, so the
            // extent walks on from where the start is drawn.
            let clamp = |axis: &Axis, cell: u32, off: i64| off.min((axis.excel)(cell)).max(0);
            let r_off = clamp(rows, from.0, anchor.from_off.0);
            let c_off = clamp(cols, from.1, anchor.from_off.1);
            Span {
                rows: (frac_of(rows, from.0, r_off), walk(rows, from.0, r_off, cy)),
                cols: (frac_of(cols, from.1, c_off), walk(cols, from.1, c_off, cx)),
            }
        }
        AnchorKind::Absolute => {
            let (x, y) = anchor.pos.unwrap_or((0, 0));
            let (cx, cy) = anchor.ext.unwrap_or((0, 0));
            let top = walk(rows, 0, 0, y);
            let left = walk(cols, 0, 0, x);
            Span {
                rows: (top, walk(rows, 0, 0, y.saturating_add(cy))),
                cols: (left, walk(cols, 0, 0, x.saturating_add(cx))),
            }
        }
    }
}

/// Step 2: how far `p` lies from the start of cell `origin`, in drawn px —
/// negative before it. It walks only the cells between the two,
/// at most [`MAX_WALK`].
pub fn offset_px(axis: &Axis, origin: u32, p: CellPos) -> f32 {
    let mut px = 0.0f32;
    if p.cell >= origin {
        let last = p.cell.min(origin.saturating_add(MAX_WALK));
        for c in origin..last {
            px += (axis.drawn)(c);
        }
    } else {
        let first = p.cell.max(origin.saturating_sub(MAX_WALK));
        for c in first..origin {
            px -= (axis.drawn)(c);
        }
    }
    let cell = if p.cell < axis.end {
        (axis.drawn)(p.cell)
    } else {
        0.0
    };
    px + p.frac * cell
}

/// A pane of the grid along one axis: the first cell it shows, how far that
/// cell is scrolled out of it (px), and the last cell it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pane {
    pub first: u32,
    pub shift: f32,
    pub last: u32,
}

/// A picture's piece in one pane along one axis: its start and length in
/// px from the pane's edge, or `None` when it misses the pane.
pub fn place(axis: &Axis, pane: Pane, span: (CellPos, CellPos)) -> Option<(f32, f32)> {
    let (a, b) = span;
    // Wholly before the pane or wholly after it: nothing to walk.
    if b.cell < pane.first || a.cell > pane.last {
        return None;
    }
    let start = offset_px(axis, pane.first, a) - pane.shift;
    let end = offset_px(axis, pane.first, b) - pane.shift;
    (end > start).then_some((start, end - start))
}

/// A picture's piece in one pane: its rectangle in px from the pane's
/// top-left, `(x, y, w, h)`.
pub fn piece(
    rows: &Axis,
    cols: &Axis,
    row_pane: Pane,
    col_pane: Pane,
    span: Span,
) -> Option<(f32, f32, f32, f32)> {
    let (y, h) = place(rows, row_pane, span.rows)?;
    let (x, w) = place(cols, col_pane, span.cols)?;
    Some((x, y, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 15 pt row as the grid draws it (`row_height_px`, gridline inside),
    /// and a 30 pt one.
    const R: f32 = 21.0;
    const TALL: f32 = 42.0;

    /// The sizes of a small sheet: rows of 15 pt drawn at 21 px, with row 3
    /// 30 pt (42 px) and row 5 hidden; columns of 8.43 units (64 Excel px)
    /// drawn at 65 px (file width 9.140625), with column 2 hidden (Excel 0, the grid's 28 px).
    fn excel_row(r: u32) -> i64 {
        match r {
            3 => excel_row_emu(Some(30.0), None, false),
            5 => excel_row_emu(None, None, true),
            _ => excel_row_emu(None, None, false),
        }
    }
    fn drawn_row(r: u32) -> f32 {
        match r {
            3 => TALL,
            5 => 0.0,
            _ => R,
        }
    }
    fn excel_col(c: u32) -> i64 {
        excel_col_emu(9.140625, c == 2)
    }
    fn drawn_col(c: u32) -> f32 {
        if c == 2 { 28.0 } else { 65.0 }
    }
    fn rows() -> Axis<'static> {
        Axis {
            excel: &excel_row,
            drawn: &drawn_row,
            end: 1000,
        }
    }
    fn cols() -> Axis<'static> {
        Axis {
            excel: &excel_col,
            drawn: &drawn_col,
            end: 256,
        }
    }
    const ROW_EMU: i64 = 15 * EMU_PER_PT;
    const COL_EMU: i64 = 64 * EMU_PER_PX;
    const TOP: Pane = Pane {
        first: 0,
        shift: 0.0,
        last: 999,
    };
    const LEFT: Pane = Pane {
        first: 0,
        shift: 0.0,
        last: 255,
    };

    fn two_cell(
        from: (u32, u32),
        from_off: (i64, i64),
        to: (u32, u32),
        to_off: (i64, i64),
    ) -> (DrawingAnchor, (u32, u32), (u32, u32)) {
        (
            DrawingAnchor {
                kind: AnchorKind::TwoCell,
                from_off,
                to_off,
                ..DrawingAnchor::default()
            },
            from,
            to,
        )
    }

    fn rect(
        a: &(DrawingAnchor, (u32, u32), (u32, u32)),
        rp: Pane,
        cp: Pane,
    ) -> Option<(f32, f32, f32, f32)> {
        let span = anchor_span(&a.0, a.1, a.2, &rows(), &cols());
        piece(&rows(), &cols(), rp, cp, span)
    }

    #[test]
    fn excel_sizes_are_estimated_as_excel_lays_them_out() {
        // Excel's own default file width, and a wider one, as Excel shows them.
        assert_eq!(excel_col_px(9.140625), 64);
        assert_eq!(excel_col_px(20.7109375), 145);
        assert_eq!(excel_col_px(14.43), 101);
        assert_eq!(excel_col_px(0.0), 0);
        assert_eq!(excel_col_emu(9.140625, false), COL_EMU);
        assert_eq!(excel_col_emu(9.140625, true), 0);
        assert_eq!(excel_row_emu(None, None, false), ROW_EMU);
        assert_eq!(excel_row_emu(None, Some(20.0), false), 20 * EMU_PER_PT);
        assert_eq!(
            excel_row_emu(Some(30.0), Some(20.0), false),
            30 * EMU_PER_PT
        );
        assert_eq!(excel_row_emu(Some(30.0), None, true), 0);
    }

    /// Offsets are fractions of their cells: half a 15 pt row and a quarter
    /// of a column land half and a quarter into the cells as drawn, over a
    /// custom-height row in between.
    #[test]
    fn a_two_cell_anchor_with_offsets_over_a_custom_height() {
        let a = two_cell(
            (1, 0),
            (ROW_EMU / 2, COL_EMU / 4),
            (4, 1),
            (ROW_EMU, COL_EMU / 2),
        );
        let (x, y, w, h) = rect(&a, TOP, LEFT).unwrap();
        assert_eq!(x, 65.0 / 4.0);
        assert_eq!(y, R + R / 2.0);
        assert_eq!(w, 65.0 - 65.0 / 4.0 + 32.5);
        // Rows 1 (half), 2, 3 (42 px) and all of row 4.
        assert_eq!(h, R / 2.0 + R + TALL + R);
    }

    /// An offset longer than its cell stops at the cell's end.
    #[test]
    fn offsets_larger_than_their_cell_are_clamped_to_it() {
        let a = two_cell((1, 0), (ROW_EMU * 5, COL_EMU * 3), (3, 2), (0, 0));
        let (x, y, w, h) = rect(&a, TOP, LEFT).unwrap();
        // It starts at the end of row 1 and of column 0.
        assert_eq!((x, y), (65.0, 2.0 * R));
        assert_eq!((w, h), (65.0, R));
        // A box that ends where it starts draws nothing.
        let flat = two_cell((1, 0), (ROW_EMU * 5, 0), (2, 1), (0, 0));
        assert_eq!(rect(&flat, TOP, LEFT), None);
    }

    /// A hidden row under a picture collapses (the grid does not draw it);
    /// a hidden column is spanned at the width the grid draws it.
    #[test]
    fn a_hidden_row_collapses_and_a_hidden_column_is_spanned_as_drawn() {
        let a = two_cell((4, 1), (0, 0), (7, 4), (0, 0));
        let (x, y, w, h) = rect(&a, TOP, LEFT).unwrap();
        assert_eq!(x, 65.0);
        assert_eq!(y, R * 3.0 + TALL);
        // Rows 4, 5 (hidden, 0 px), 6.
        assert_eq!(h, R + 0.0 + R);
        // Columns 1, 2 (hidden, 28 px), 3.
        assert_eq!(w, 65.0 + 28.0 + 65.0);
        // An offset into a hidden cell has nothing to be a fraction of.
        let a = two_cell((5, 2), (ROW_EMU, COL_EMU), (7, 3), (0, 0));
        let (x, y, _, _) = rect(&a, TOP, LEFT).unwrap();
        assert_eq!((x, y), (130.0, R * 4.0 + TALL));
    }

    /// A one-cell anchor's extent walks Excel's sizes: exactly N default
    /// rows tall ends on row from+N at its very start, whatever the grid's
    /// row height; the hidden column is stepped over, as Excel steps it.
    #[test]
    fn a_one_cell_anchor_covers_the_cells_its_extent_covers_in_excel() {
        let anchor = DrawingAnchor {
            kind: AnchorKind::OneCell,
            from_off: (0, COL_EMU / 2),
            ext: Some((COL_EMU * 2, ROW_EMU * 4)),
            ..DrawingAnchor::default()
        };
        let span = anchor_span(&anchor, (6, 1), (0, 0), &rows(), &cols());
        assert_eq!(
            span.rows.1,
            CellPos {
                cell: 10,
                frac: 0.0
            }
        );
        // From half into column 1, two Excel columns on: column 2 is hidden
        // (0 EMU), so it ends half into column 4.
        assert_eq!(span.cols.0, CellPos { cell: 1, frac: 0.5 });
        assert_eq!(span.cols.1, CellPos { cell: 4, frac: 0.5 });
        let (x, y, w, h) = piece(&rows(), &cols(), TOP, LEFT, span).unwrap();
        assert_eq!(y, R * 4.0 + TALL + 0.0);
        assert_eq!(h, R * 4.0);
        assert_eq!(x, 65.0 * 1.5);
        assert_eq!(w, 32.5 + 28.0 + 65.0 + 32.5);
    }

    /// An absolute anchor walks from A1 by its position, then by its extent.
    #[test]
    fn an_absolute_anchor_is_placed_from_the_sheets_top_left() {
        let anchor = DrawingAnchor {
            kind: AnchorKind::Absolute,
            pos: Some((COL_EMU, ROW_EMU + ROW_EMU / 2)),
            ext: Some((COL_EMU / 2, ROW_EMU)),
            ..DrawingAnchor::default()
        };
        let span = anchor_span(&anchor, (0, 0), (0, 0), &rows(), &cols());
        assert_eq!(span.rows.0, CellPos { cell: 1, frac: 0.5 });
        assert_eq!(span.rows.1, CellPos { cell: 2, frac: 0.5 });
        let (x, y, w, h) = piece(&rows(), &cols(), TOP, LEFT, span).unwrap();
        assert_eq!((x, y, w, h), (65.0, R * 1.5, 32.5, R));
    }

    /// Scrolled down and right, a picture is placed from the pane's first
    /// cell and how far that cell is scrolled out; one that started above
    /// the pane is drawn partly, at a negative y.
    #[test]
    fn a_scrolled_view_moves_the_picture_and_keeps_its_size() {
        let a = two_cell((1, 0), (0, 0), (8, 3), (0, 0));
        let unscrolled = rect(&a, TOP, LEFT).unwrap();
        let rp = Pane {
            first: 2,
            shift: 5.0,
            last: 40,
        };
        let cp = Pane {
            first: 1,
            shift: 0.0,
            last: 20,
        };
        let (x, y, w, h) = rect(&a, rp, cp).unwrap();
        assert_eq!(y, -R - 5.0);
        assert_eq!(x, -65.0);
        assert_eq!((w, h), (unscrolled.2, unscrolled.3));
        // Scrolled past it: no piece, and no walk over it either.
        let past = Pane {
            first: 9,
            shift: 0.0,
            last: 40,
        };
        assert_eq!(rect(&a, past, LEFT), None);
        let before = Pane {
            first: 0,
            shift: 0.0,
            last: 0,
        };
        assert_eq!(rect(&a, before, LEFT), None);
    }

    /// Frozen panes: a picture over the freeze line has a piece in each
    /// pane, each from that pane's own first cell. With rows 0..3 frozen and
    /// the rest scrolled to row 10, the frozen piece starts at the picture's
    /// top and the scrolled one far above its pane's edge.
    #[test]
    fn a_picture_over_a_freeze_line_has_a_piece_in_each_pane() {
        let a = two_cell((1, 0), (0, 0), (12, 2), (0, 0));
        let frozen = Pane {
            first: 0,
            shift: 0.0,
            last: 2,
        };
        let scrolled = Pane {
            first: 10,
            shift: 0.0,
            last: 40,
        };
        let (_, fy, _, fh) = rect(&a, frozen, LEFT).unwrap();
        assert_eq!(fy, R);
        let (_, sy, _, sh) = rect(&a, scrolled, LEFT).unwrap();
        // Rows 1..10 above the pane: 1, 2, 4, 6..9 at 21, 3 at 42, 5 hidden.
        assert_eq!(sy, -(R * 7.0 + TALL));
        assert_eq!(fh, sh, "one picture, one size");
        // The frozen columns' pane and the scrolled one split it the same way.
        let fcols = Pane {
            first: 0,
            shift: 0.0,
            last: 0,
        };
        let scols = Pane {
            first: 1,
            shift: 0.0,
            last: 20,
        };
        let (fx, ..) = rect(&a, frozen, fcols).unwrap();
        let (sx, ..) = rect(&a, frozen, scols).unwrap();
        assert_eq!((fx, sx), (0.0, -65.0));
    }

    /// A crafted extent stops at the axis's end, or after `MAX_WALK` cells.
    #[test]
    fn a_huge_extent_stops_at_the_last_drawn_cell() {
        let anchor = DrawingAnchor {
            kind: AnchorKind::OneCell,
            ext: Some((i64::MAX / 4, gridcore::sheet::MAX_ANCHOR_EMU)),
            ..DrawingAnchor::default()
        };
        let span = anchor_span(&anchor, (0, 0), (0, 0), &rows(), &cols());
        assert_eq!(
            span.rows.1,
            CellPos {
                cell: 1000,
                frac: 0.0
            }
        );
        assert_eq!(
            span.cols.1,
            CellPos {
                cell: 256,
                frac: 0.0
            }
        );
    }

    /// On an axis longer than `MAX_WALK`, a walk stops after `MAX_WALK` cells
    /// at the start of the cell it reached, and so does a pixel offset, either
    /// way: a crafted extent costs at most that many cells a frame.
    #[test]
    fn a_walk_stops_after_max_walk_cells() {
        let long = |c: u32| if c < 5 { 0 } else { ROW_EMU };
        let flat = |_: u32| R;
        let rows = Axis {
            excel: &long,
            drawn: &flat,
            end: 4 * MAX_WALK,
        };
        let cols = cols();
        let anchor = DrawingAnchor {
            kind: AnchorKind::OneCell,
            ext: Some((COL_EMU, gridcore::sheet::MAX_ANCHOR_EMU)),
            ..DrawingAnchor::default()
        };
        let span = anchor_span(&anchor, (5, 0), (0, 0), &rows, &cols);
        assert_eq!(
            span.rows.1,
            CellPos {
                cell: 5 + MAX_WALK,
                frac: 0.0
            }
        );
        let far = CellPos {
            cell: 3 * MAX_WALK,
            frac: 0.0,
        };
        assert_eq!(offset_px(&rows, 0, far), MAX_WALK as f32 * R);
        let near = CellPos { cell: 0, frac: 0.0 };
        assert_eq!(offset_px(&rows, 3 * MAX_WALK, near), -(MAX_WALK as f32 * R));
    }

    /// A sheet's columns take their own width, else the sheet's
    /// `defaultColWidth` (not 8.43), with the padding counted once: a
    /// one-cell picture exactly two default columns wide ends two columns on.
    #[test]
    fn a_sheets_columns_are_sized_from_its_own_widths_and_default() {
        let mut sh = Sheet::default();
        assert_eq!(sheet_col_emu(&sh, 0), 64 * EMU_PER_PX, "baseColWidth 8");
        sh.format.default_col_width = Some(14.43);
        sh.set_col_width(1, 20.7109375);
        sh.set_col_hidden(4, true);
        assert_eq!(sheet_col_emu(&sh, 0), 101 * EMU_PER_PX);
        assert_eq!(sheet_col_emu(&sh, 1), 145 * EMU_PER_PX);
        assert_eq!(sheet_col_emu(&sh, 4), 0);
        assert_eq!(sheet_row_emu(&sh, 0), ROW_EMU);
        let excel = |c: u32| sheet_col_emu(&sh, c);
        let drawn = |_: u32| 65.0;
        let cols = Axis {
            excel: &excel,
            drawn: &drawn,
            end: 256,
        };
        let anchor = DrawingAnchor {
            kind: AnchorKind::OneCell,
            ext: Some((2 * 101 * EMU_PER_PX, ROW_EMU)),
            ..DrawingAnchor::default()
        };
        let span = anchor_span(&anchor, (0, 5), (0, 0), &rows(), &cols);
        assert_eq!(span.cols.1, CellPos { cell: 7, frac: 0.0 });
    }
}
