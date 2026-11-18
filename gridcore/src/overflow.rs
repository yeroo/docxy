//! Text overflow: which cells' text runs on over their empty neighbours, as
//! Excel draws an unwrapped text wider than its column (#2240).
//!
//! The rules, shared by the sheet PDF and the suite's sheet view:
//!
//! - **Who spills:** a text value (typed, or a formula's text result), or a
//!   formula the view shows as its formula text; never a number (too wide, it
//!   shows `#`s), a boolean or an error. A wrapped cell wraps instead, and a
//!   cell inside a merged area draws in its merge.
//! - **Which way:** General and Left run right, Right runs left, Center runs
//!   both ways with the text staying centred on its own cell.
//! - **What stops it:** a non-blank neighbour (any value, or any formula, so a
//!   formula whose result is `""` stops it too) and any cell of a merged
//!   area. A cell with only a style is empty: the text draws over its fill.
//!
//! [`spill_span`] works on slots, not columns, so the PDF's page slots (title
//! columns chained with the page's) and the view's sheet columns both fit.

use crate::sheet::{Align, Cell, CellValue, Sheet, Xf};

/// The way a cell's text runs on past its own edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spill {
    /// General and Left alignment: over the cells to the right.
    Right,
    /// Right alignment: over the cells to the left.
    Left,
    /// Center: both ways, staying centred on its own cell.
    Both,
}

/// Which way the shown text of `cell` (at `r`, `c`, styled `xf`) may spill,
/// or `None` when it never does: not text, wrapped, or inside a merge.
/// `shows_formula`: the view shows this cell's formula as text (#676), which
/// is raw text, left-aligned, so it runs right.
pub fn spill_dir(
    sheet: &Sheet,
    xf: &Xf,
    cell: &Cell,
    r: u32,
    c: u32,
    shows_formula: bool,
) -> Option<Spill> {
    if xf.wrap || sheet.merge_at(r, c).is_some() {
        return None;
    }
    if shows_formula {
        return Some(Spill::Right);
    }
    if !matches!(cell.value, CellValue::Text(_)) {
        return None;
    }
    Some(match xf.align {
        Align::General | Align::Left => Spill::Right,
        Align::Right => Spill::Left,
        Align::Center => Spill::Both,
    })
}

/// Can no spill cross the cell at (`r`, `c`): it is non-blank (a value or a
/// formula) or lies inside a merged area.
pub fn blocks(sheet: &Sheet, r: u32, c: u32) -> bool {
    sheet.cell(r, c).is_some_and(|cl| !cl.is_blank()) || sheet.merge_at(r, c).is_some()
}

/// The nearest column left of `c` in row `r` that [`blocks`], read from the
/// sparse cell map and the merges, so empty columns are never walked.
pub fn prev_blocker(sheet: &Sheet, r: u32, c: u32) -> Option<u32> {
    if c == 0 {
        return None;
    }
    let cell = sheet
        .cells
        .range((r, 0)..(r, c))
        .rev()
        .find(|(_, cl)| !cl.is_blank())
        .map(|(&(_, cc), _)| cc);
    let merge = sheet
        .merges
        .iter()
        .filter(|&&(r1, c1, r2, _)| r >= r1 && r <= r2 && c1 < c)
        .map(|&(_, _, _, c2)| c2.min(c - 1))
        .max();
    cell.max(merge)
}

/// The nearest column right of `c` in row `r` that [`blocks`]; see
/// [`prev_blocker`].
pub fn next_blocker(sheet: &Sheet, r: u32, c: u32) -> Option<u32> {
    let from = c.checked_add(1)?;
    let cell = sheet
        .cells
        .range((r, from)..=(r, u32::MAX))
        .find(|(_, cl)| !cl.is_blank())
        .map(|(&(_, cc), _)| cc);
    let merge = sheet
        .merges
        .iter()
        .filter(|&&(r1, _, r2, c2)| r >= r1 && r <= r2 && c2 > c)
        .map(|&(_, c1, _, _)| c1.max(from))
        .min();
    match (cell, merge) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The run of slots `(lo, hi)` the text of slot `src` covers, within slots
/// `min..=max`. `width(i)` is slot `i`'s width, `free(i)` says it is empty,
/// `tw` the text's width and `pad` the cell's inner padding on each side, all
/// in the caller's unit.
///
/// A text that fits is `(src, src)`. [`Spill::Right`] and [`Spill::Left`]
/// grow one slot at a time until the run's room (its width less the padding)
/// holds the text; [`Spill::Both`] grows each side until it holds half the
/// text's excess over the cell's own room, each side on its own, so a blocked
/// side cuts the text there without re-centring it. A run stops at a slot that
/// is not free and at `min`/`max`. Zero-width slots (hidden columns) are
/// crossed. The work is bounded by the text's width, not by `min..=max`.
#[allow(clippy::too_many_arguments)]
pub fn spill_span(
    src: usize,
    min: usize,
    max: usize,
    width: impl Fn(usize) -> f64,
    free: impl Fn(usize) -> bool,
    tw: f64,
    pad: f64,
    dir: Spill,
) -> (usize, usize) {
    let own = width(src);
    let room = own - 2.0 * pad;
    if tw <= room {
        return (src, src);
    }
    let grow_right = |need: f64| {
        let (mut hi, mut got) = (src, 0.0);
        while got < need && hi < max && free(hi + 1) {
            hi += 1;
            got += width(hi);
        }
        hi
    };
    let grow_left = |need: f64| {
        let (mut lo, mut got) = (src, 0.0);
        while got < need && lo > min && free(lo - 1) {
            lo -= 1;
            got += width(lo);
        }
        lo
    };
    match dir {
        Spill::Right => (src, grow_right(tw - room)),
        Spill::Left => (grow_left(tw - room), src),
        Spill::Both => {
            let half = (tw - room) / 2.0;
            (grow_left(half), grow_right(half))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Workbook;

    fn w10(_: usize) -> f64 {
        10.0
    }

    #[test]
    fn a_fitting_text_stays_in_its_cell() {
        assert_eq!(
            spill_span(2, 0, 9, w10, |_| true, 6.0, 2.0, Spill::Right),
            (2, 2)
        );
        assert_eq!(
            spill_span(2, 0, 9, w10, |_| true, 6.0, 2.0, Spill::Both),
            (2, 2)
        );
    }

    #[test]
    fn a_right_run_grows_until_the_text_fits() {
        // Room 6 in the cell; 25 needs 19 more: two more slots give 20.
        assert_eq!(
            spill_span(1, 0, 9, w10, |_| true, 25.0, 2.0, Spill::Right),
            (1, 3)
        );
        // Exactly enough: 16 = 6 + 10.
        assert_eq!(
            spill_span(1, 0, 9, w10, |_| true, 16.0, 2.0, Spill::Right),
            (1, 2)
        );
    }

    #[test]
    fn a_run_stops_at_a_filled_slot_and_at_the_bounds() {
        assert_eq!(
            spill_span(1, 0, 9, w10, |i| i != 3, 60.0, 2.0, Spill::Right),
            (1, 2)
        );
        assert_eq!(
            spill_span(1, 0, 4, w10, |_| true, 60.0, 2.0, Spill::Right),
            (1, 4)
        );
        assert_eq!(
            spill_span(1, 0, 9, w10, |i| i != 2, 60.0, 2.0, Spill::Right),
            (1, 1)
        );
    }

    #[test]
    fn a_left_run_grows_left() {
        assert_eq!(
            spill_span(5, 0, 9, w10, |_| true, 25.0, 2.0, Spill::Left),
            (3, 5)
        );
        assert_eq!(
            spill_span(5, 4, 9, w10, |_| true, 25.0, 2.0, Spill::Left),
            (4, 5)
        );
        assert_eq!(
            spill_span(5, 0, 9, w10, |i| i != 3, 60.0, 2.0, Spill::Left),
            (4, 5)
        );
    }

    #[test]
    fn a_centred_run_grows_each_side_by_half_the_excess() {
        // Excess 26 - 6 = 20: 10 a side, one slot each.
        assert_eq!(
            spill_span(5, 0, 9, w10, |_| true, 26.0, 2.0, Spill::Both),
            (4, 6)
        );
        // Excess 22: 11 a side, two slots each.
        assert_eq!(
            spill_span(5, 0, 9, w10, |_| true, 28.0, 2.0, Spill::Both),
            (3, 7)
        );
        // A blocked left side cuts there; the right still grows only its half.
        assert_eq!(
            spill_span(5, 0, 9, w10, |i| i != 4, 28.0, 2.0, Spill::Both),
            (5, 7)
        );
    }

    #[test]
    fn hidden_slots_are_crossed() {
        let w = |i: usize| if i == 2 || i == 3 { 0.0 } else { 10.0 };
        assert_eq!(
            spill_span(1, 0, 9, w, |_| true, 12.0, 2.0, Spill::Right),
            (1, 4)
        );
    }

    #[test]
    fn the_work_is_bounded_by_the_text() {
        let calls = std::cell::Cell::new(0);
        let w = |_: usize| {
            calls.set(calls.get() + 1);
            10.0
        };
        assert_eq!(
            spill_span(0, 0, usize::MAX - 1, w, |_| true, 26.0, 2.0, Spill::Right),
            (0, 2)
        );
        assert!(calls.get() <= 3, "{}", calls.get());
    }

    fn styled(wb: &mut Workbook, xf: Xf) -> u32 {
        wb.styles.intern(xf)
    }

    #[test]
    fn who_spills_and_which_way() {
        let s = Sheet::default();
        let general = Xf::default();
        let text = Cell::text("long");
        assert_eq!(
            spill_dir(&s, &general, &text, 0, 0, false),
            Some(Spill::Right)
        );
        for (align, dir) in [
            (Align::Left, Spill::Right),
            (Align::Right, Spill::Left),
            (Align::Center, Spill::Both),
        ] {
            let xf = Xf {
                align,
                ..Xf::default()
            };
            assert_eq!(spill_dir(&s, &xf, &text, 0, 0, false), Some(dir));
        }
        // A number, a boolean or an error never spills.
        for v in [
            CellValue::Number(1.0),
            CellValue::Bool(true),
            CellValue::Error("#N/A".into()),
        ] {
            let cell = Cell {
                value: v,
                ..Cell::default()
            };
            assert_eq!(spill_dir(&s, &general, &cell, 0, 0, false), None);
        }
        // A formula's text result spills; a formula shown as text runs right,
        // whatever its value and alignment.
        let f = Cell {
            value: CellValue::Text("result".into()),
            formula: Some("A2&A3".into()),
            ..Cell::default()
        };
        assert_eq!(spill_dir(&s, &general, &f, 0, 0, false), Some(Spill::Right));
        let n = Cell {
            value: CellValue::Number(3.0),
            formula: Some("1+2".into()),
            ..Cell::default()
        };
        let right = Xf {
            align: Align::Right,
            ..Xf::default()
        };
        assert_eq!(spill_dir(&s, &right, &n, 0, 0, true), Some(Spill::Right));
        // Wrapped text wraps.
        let wrap = Xf {
            wrap: true,
            ..Xf::default()
        };
        assert_eq!(spill_dir(&s, &wrap, &text, 0, 0, false), None);
        // A cell inside a merge draws in its merge.
        let mut m = Sheet::default();
        m.merges.push((0, 0, 0, 2));
        assert_eq!(spill_dir(&m, &general, &text, 0, 0, false), None);
        assert_eq!(
            spill_dir(&m, &general, &text, 0, 3, false),
            Some(Spill::Right)
        );
    }

    #[test]
    fn what_blocks_a_spill() {
        let mut wb = Workbook::default();
        // Style 0, the unstyled cells', is the default xf.
        wb.styles.intern(Xf::default());
        let fill = styled(
            &mut wb,
            Xf {
                fill: Some((255, 255, 0)),
                ..Xf::default()
            },
        );
        let mut s = Sheet::default();
        s.set_cell(0, 1, Cell::number(1.0));
        // A formula whose result is "" still blocks.
        s.set_cell(
            0,
            2,
            Cell {
                value: CellValue::Text(String::new()),
                formula: Some("\"\"".into()),
                ..Cell::default()
            },
        );
        // A styled blank does not.
        s.set_cell(
            0,
            3,
            Cell {
                style: fill,
                ..Cell::default()
            },
        );
        s.merges.push((0, 5, 1, 6));
        assert!(blocks(&s, 0, 1));
        assert!(blocks(&s, 0, 2));
        assert!(s.cell(0, 3).is_some() && !blocks(&s, 0, 3));
        assert!(!blocks(&s, 0, 4));
        assert!(blocks(&s, 0, 5) && blocks(&s, 1, 6));
        assert!(!blocks(&s, 2, 5));
    }

    #[test]
    fn the_nearest_blockers_skip_styled_blanks_and_see_merges() {
        let mut s = Sheet::default();
        s.set_cell(0, 2, Cell::text("a"));
        s.set_cell(
            0,
            5,
            Cell {
                style: 1,
                ..Cell::default()
            },
        );
        s.set_cell(0, 9, Cell::text("b"));
        s.set_cell(1, 4, Cell::text("other row"));
        assert_eq!(prev_blocker(&s, 0, 8), Some(2));
        assert_eq!(prev_blocker(&s, 0, 2), None);
        assert_eq!(prev_blocker(&s, 0, 0), None);
        assert_eq!(next_blocker(&s, 0, 2), Some(9));
        assert_eq!(next_blocker(&s, 0, 9), None);
        assert_eq!(next_blocker(&s, 0, u32::MAX), None);
        // A merge over rows 0..=1, columns 6..=7, is nearer either way.
        s.merges.push((0, 6, 1, 7));
        assert_eq!(prev_blocker(&s, 0, 8), Some(7));
        assert_eq!(next_blocker(&s, 0, 2), Some(6));
        assert_eq!(next_blocker(&s, 1, 0), Some(4));
        // From inside the merge, its own columns beside `c` are the nearest.
        assert_eq!(prev_blocker(&s, 1, 7), Some(6));
        assert_eq!(next_blocker(&s, 1, 6), Some(7));
        assert_eq!(prev_blocker(&s, 2, 8), None);
    }
}
