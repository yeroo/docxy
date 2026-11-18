//! Text spilling over empty neighbours in the sheet view (#2240): which
//! sources of a row run on into a pane of the grid, and how far.
//!
//! The rules are `gridcore::overflow`'s, shared with the sheet PDF. Each pane
//! (the frozen columns, then the scrolled window) is its own window onto the
//! sheet, as Excel paints freeze panes: its sources are its own cells plus
//! the nearest non-blank cell on either side of it, the only ones whose text
//! can reach in. A run is computed in sheet columns, so a pane edge never
//! stops it, and the caller clips it to the pane it draws.

use gridcore::overflow::{self, Spill};
use gridcore::sheet::{MAX_COLS, Sheet, Styles, Xf};

/// A cell's inner padding on each side, in px, as the grid draws a cell:
/// 4 px on the left, 4 px and the 1 px gridline on the right.
pub(crate) const PAD: f32 = 4.5;

/// A cheap upper bound of a 12 px glyph's width, so a text that surely fits,
/// or a source too far off to reach a pane, is never shaped.
const GLYPH_MAX: f32 = 12.0 * 1.2;

/// One source's text running on over its empty neighbours.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpillRun {
    /// The source cell's column.
    pub src: u32,
    /// The run, in sheet columns, before any pane clips it.
    pub lo: u32,
    pub hi: u32,
    pub dir: Spill,
    /// The text the source shows.
    pub text: String,
}

/// What `pane_spills` reads besides the row and the pane.
pub(crate) struct SpillCtx<'a> {
    pub sheet: &'a Sheet,
    pub styles: &'a Styles,
    pub show_formulas: bool,
    pub d1904: bool,
    /// The column of this row's cell being edited: it blocks every run and
    /// spills nothing, so the editor covers the text while typing.
    pub edited: Option<u32>,
}

/// The runs of row `r` that reach into the pane of columns `a..=b`.
/// `col_w(c)` is column `c`'s drawn width and `text_w(c, xf, line)` the
/// drawn width of one line of column `c`'s text.
pub(crate) fn pane_spills(
    ctx: &SpillCtx,
    r: u32,
    (a, b): (u32, u32),
    col_w: impl Fn(u32) -> f32,
    text_w: impl Fn(u32, &Xf, &str) -> f32,
) -> Vec<SpillRun> {
    let sh = ctx.sheet;
    let mut sources: Vec<u32> = Vec::new();
    sources.extend(overflow::prev_blocker(sh, r, a));
    sources.extend(
        sh.cells
            .range((r, a)..=(r, b))
            .filter(|(_, cl)| !cl.is_blank())
            .map(|(&(_, c), _)| c),
    );
    sources.extend(overflow::next_blocker(sh, r, b));
    let free = |c: u32| Some(c) != ctx.edited && !overflow::blocks(sh, r, c);
    let mut runs = Vec::new();
    for c in sources {
        if Some(c) == ctx.edited {
            continue;
        }
        let Some(cell) = sh.cell(r, c) else {
            continue; // a merge's column: a merge never spills
        };
        let xf = ctx.styles.xf(cell.style);
        let shows_formula = ctx.show_formulas && cell.formula.is_some();
        let Some(dir) = overflow::spill_dir(sh, &xf, cell, r, c, shows_formula) else {
            continue;
        };
        // Off the pane, only a source running towards it can reach in.
        if (c < a && dir == Spill::Left) || (c > b && dir == Spill::Right) {
            continue;
        }
        let own = col_w(c);
        let text = crate::shown_cell_text(ctx.show_formulas, cell, &xf, ctx.d1904, own);
        // A line feed in an unwrapped cell: the widest line is what runs on
        // (gpui shapes one line at a time).
        let lines: Vec<&str> = text.split('\n').collect();
        let chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let room = own - 2.0 * PAD;
        let bound = chars as f32 * GLYPH_MAX;
        if bound <= room {
            continue;
        }
        let reach = if dir == Spill::Both {
            (bound - room) / 2.0
        } else {
            bound - room
        };
        if !reaches(c, (a, b), reach, &col_w) {
            continue;
        }
        let tw = lines
            .iter()
            .map(|l| text_w(c, &xf, l))
            .fold(0.0_f32, f32::max);
        let (lo, hi) = overflow::spill_span(
            c as usize,
            0,
            (MAX_COLS - 1) as usize,
            |i| col_w(i as u32) as f64,
            |i| free(i as u32),
            tw as f64,
            PAD as f64,
            dir,
        );
        let (lo, hi) = (lo as u32, hi as u32);
        if lo == hi || hi < a || lo > b {
            continue;
        }
        runs.push(SpillRun {
            src: c,
            lo,
            hi,
            dir,
            text,
        });
    }
    runs
}

/// Can a text reaching `reach` px past its cell `c` get into the pane
/// `a..=b`: the columns between them are narrower than that. Walks only as
/// far as `reach` covers.
fn reaches(c: u32, (a, b): (u32, u32), reach: f32, col_w: &impl Fn(u32) -> f32) -> bool {
    let between: Box<dyn Iterator<Item = u32>> = if c < a {
        Box::new(c + 1..a)
    } else if c > b {
        Box::new((b + 1..c).rev())
    } else {
        return true;
    };
    let mut gap = 0.0;
    for x in between {
        gap += col_w(x);
        if gap >= reach {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcore::sheet::{Align, Cell, CellValue, Workbook};

    /// 7 px a character, as `grid_cell_text` estimates.
    fn tw(_: u32, _: &Xf, line: &str) -> f32 {
        line.chars().count() as f32 * 7.0
    }

    /// Every column 64 px, the default width.
    fn w64(_: u32) -> f32 {
        64.0
    }

    /// 40 characters: 280 px, which needs A plus four more 64 px columns
    /// (room 64 - 9 = 55, then 119, 183, 247, 311).
    const LONG: &str = "This heading is much wider than its cell";

    fn book() -> Workbook {
        let mut wb = Workbook::default();
        // Style 0, the unstyled cells', is the default xf.
        wb.styles.intern(Xf::default());
        wb.sheets.push(Sheet::default());
        wb
    }

    fn styled(wb: &mut Workbook, xf: Xf) -> u32 {
        wb.styles.intern(xf)
    }

    fn runs_in(
        wb: &Workbook,
        r: u32,
        pane: (u32, u32),
        edited: Option<u32>,
    ) -> Vec<(u32, u32, u32)> {
        let ctx = SpillCtx {
            sheet: &wb.sheets[0],
            styles: &wb.styles,
            show_formulas: wb.sheets[0].show_formulas,
            d1904: false,
            edited,
        };
        pane_spills(&ctx, r, pane, w64, tw)
            .into_iter()
            .map(|s| (s.src, s.lo, s.hi))
            .collect()
    }

    #[test]
    fn a_long_text_runs_right_over_empty_cells() {
        let mut wb = book();
        wb.sheets[0].set_cell(0, 0, Cell::text(LONG));
        assert_eq!(runs_in(&wb, 0, (0, 20), None), vec![(0, 0, 4)]);
        // A text that fits draws in its cell.
        wb.sheets[0].set_cell(1, 0, Cell::text("short"));
        assert_eq!(runs_in(&wb, 1, (0, 20), None), vec![]);
    }

    #[test]
    fn a_run_stops_at_the_first_filled_cell() {
        let mut wb = book();
        wb.sheets[0].set_cell(0, 0, Cell::text(LONG));
        wb.sheets[0].set_cell(0, 3, Cell::number(1.0));
        assert_eq!(runs_in(&wb, 0, (0, 20), None), vec![(0, 0, 2)]);
        // B filled: nothing to run over.
        wb.sheets[0].set_cell(0, 1, Cell::text("x"));
        assert_eq!(runs_in(&wb, 0, (0, 20), None), vec![]);
    }

    #[test]
    fn right_runs_left_and_centre_runs_both_ways() {
        let mut wb = book();
        let right = styled(
            &mut wb,
            Xf {
                align: Align::Right,
                ..Xf::default()
            },
        );
        let center = styled(
            &mut wb,
            Xf {
                align: Align::Center,
                ..Xf::default()
            },
        );
        let sh = &mut wb.sheets[0];
        sh.set_cell(
            0,
            6,
            Cell {
                style: right,
                ..Cell::text(LONG)
            },
        );
        sh.set_cell(
            1,
            6,
            Cell {
                style: center,
                ..Cell::text(LONG)
            },
        );
        assert_eq!(runs_in(&wb, 0, (0, 20), None), vec![(6, 2, 6)]);
        // Excess 280 - 55 = 225: 112.5 a side, two columns each.
        assert_eq!(runs_in(&wb, 1, (0, 20), None), vec![(6, 4, 8)]);
    }

    #[test]
    fn numbers_wrapped_text_and_merges_never_spill() {
        let mut wb = book();
        let wrap = styled(
            &mut wb,
            Xf {
                wrap: true,
                ..Xf::default()
            },
        );
        let sh = &mut wb.sheets[0];
        sh.set_cell(0, 0, Cell::number(123456789012345.0));
        sh.set_cell(
            1,
            0,
            Cell {
                style: wrap,
                ..Cell::text(LONG)
            },
        );
        sh.set_cell(2, 0, Cell::text(LONG));
        sh.merges.push((2, 0, 2, 1));
        // A text beside a merge cannot run into it.
        sh.set_cell(3, 0, Cell::text(LONG));
        sh.merges.push((3, 1, 3, 2));
        for r in 0..4 {
            assert_eq!(runs_in(&wb, r, (0, 20), None), vec![], "row {r}");
        }
    }

    #[test]
    fn a_formula_with_an_empty_result_blocks_and_a_text_result_spills() {
        let mut wb = book();
        let sh = &mut wb.sheets[0];
        sh.set_cell(
            0,
            0,
            Cell {
                value: CellValue::Text(LONG.into()),
                formula: Some("B5&C5".into()),
                ..Cell::default()
            },
        );
        sh.set_cell(
            0,
            2,
            Cell {
                value: CellValue::Text(String::new()),
                formula: Some("\"\"".into()),
                ..Cell::default()
            },
        );
        assert_eq!(runs_in(&wb, 0, (0, 20), None), vec![(0, 0, 1)]);
    }

    #[test]
    fn a_shown_formula_runs_right() {
        let mut wb = book();
        let sh = &mut wb.sheets[0];
        sh.show_formulas = true;
        sh.set_cell(
            0,
            0,
            Cell {
                value: CellValue::Number(3.0),
                formula: Some("SUM(B10:B20)+SUM(C10:C20)+SUM(D10:D20)".into()),
                ..Cell::default()
            },
        );
        assert_eq!(runs_in(&wb, 0, (0, 20), None), vec![(0, 0, 4)]);
    }

    #[test]
    fn the_edited_cell_blocks_and_spills_nothing() {
        let mut wb = book();
        wb.sheets[0].set_cell(0, 0, Cell::text(LONG));
        // Typing into C1 cuts A1's run at B1.
        assert_eq!(runs_in(&wb, 0, (0, 20), Some(2)), vec![(0, 0, 1)]);
        // A1 itself being edited draws its editor, no run.
        assert_eq!(runs_in(&wb, 0, (0, 20), Some(0)), vec![]);
    }

    #[test]
    fn a_line_feed_measures_the_widest_line() {
        let mut wb = book();
        wb.sheets[0].set_cell(0, 0, Cell::text("a\nb"));
        wb.sheets[0].set_cell(1, 0, Cell::text(&format!("a\n{LONG}")));
        let ctx = SpillCtx {
            sheet: &wb.sheets[0],
            styles: &wb.styles,
            show_formulas: false,
            d1904: false,
            edited: None,
        };
        let no_lf = |_: u32, _: &Xf, line: &str| {
            assert!(!line.contains('\n'), "{line:?} measured whole");
            tw(0, &Xf::default(), line)
        };
        assert_eq!(pane_spills(&ctx, 0, (0, 20), w64, no_lf), vec![]);
        let runs = pane_spills(&ctx, 1, (0, 20), w64, no_lf);
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].lo, runs[0].hi), (0, 4));
    }

    #[test]
    fn a_source_scrolled_off_left_still_reaches_the_window() {
        let mut wb = book();
        wb.sheets[0].set_cell(0, 0, Cell::text(LONG));
        // The scrolled window starts at D: A1's run A..E reaches into it.
        assert_eq!(runs_in(&wb, 0, (3, 20), None), vec![(0, 0, 4)]);
        // Past E it does not.
        assert_eq!(runs_in(&wb, 0, (5, 20), None), vec![]);
        // A filled cell between the source and the window blocks it.
        wb.sheets[0].set_cell(0, 1, Cell::text("x"));
        assert_eq!(runs_in(&wb, 0, (3, 20), None), vec![]);
    }

    #[test]
    fn each_frozen_pane_is_a_window_onto_the_sheet() {
        let mut wb = book();
        let right = styled(
            &mut wb,
            Xf {
                align: Align::Right,
                ..Xf::default()
            },
        );
        let sh = &mut wb.sheets[0];
        sh.freeze = (0, 1);
        sh.set_cell(0, 0, Cell::text(LONG));
        sh.set_cell(
            1,
            3,
            Cell {
                style: right,
                ..Cell::text(LONG)
            },
        );
        // Frozen A: A1's run is drawn there, cut at the freeze line, and the
        // scrollable window from B carries the rest.
        assert_eq!(runs_in(&wb, 0, (0, 0), None), vec![(0, 0, 4)]);
        assert_eq!(runs_in(&wb, 0, (1, 20), None), vec![(0, 0, 4)]);
        // A right-aligned D2 runs left across the freeze line into A.
        assert_eq!(runs_in(&wb, 1, (0, 0), None), vec![(3, 0, 3)]);
        assert_eq!(runs_in(&wb, 1, (1, 20), None), vec![(3, 0, 3)]);
        // Scrolled to F, neither reaches the window.
        assert_eq!(runs_in(&wb, 0, (5, 20), None), vec![]);
        assert_eq!(runs_in(&wb, 1, (5, 20), None), vec![]);
    }

    #[test]
    fn a_far_source_is_not_shaped() {
        let mut wb = book();
        wb.sheets[0].set_cell(0, 0, Cell::text(LONG));
        let ctx = SpillCtx {
            sheet: &wb.sheets[0],
            styles: &wb.styles,
            show_formulas: false,
            d1904: false,
            edited: None,
        };
        let never = |_: u32, _: &Xf, _: &str| -> f32 { panic!("shaped") };
        assert_eq!(pane_spills(&ctx, 0, (5000, 5020), w64, never), vec![]);
    }
}
