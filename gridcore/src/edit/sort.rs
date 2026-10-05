//! Excel's Sort: levels on a value (A to Z, Z to A, a custom list), a cell
//! colour, a font colour or a conditional-formatting icon; case-sensitive
//! or not; top to bottom or left to right; and the checks that refuse a
//! sort (a cut spill, merged cells of different sizes) or ask first (a
//! selection inside a wider list).

use std::cmp::Ordering;

use super::{Area, array_rect, move_own_array_ref, subtotal_region};
use crate::sheet::{Cell, CellValue, Workbook};

/// Excel's built-in custom lists.
pub const BUILTIN_SORT_LISTS: [&[&str]; 4] = [
    &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"],
    &[
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ],
    &[
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ],
    &[
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ],
];

/// Excel's refusal for a range whose merged cells differ.
pub const SORT_MERGED: &str = "To do this, all the merged cells need to be the same size.";

/// Excel's Sort Warning, for a selection inside a wider list.
pub const SORT_WARNING: &str = "Excel found data next to your selection. Since you have not selected this data, it will not be sorted.";

/// The Sort dialog's options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SortOptions {
    /// Case sensitive: equal words order lowercase before uppercase.
    pub case_sensitive: bool,
    /// Sort left to right: columns move, by the values of a row.
    pub left_to_right: bool,
    /// My data has headers: the range's first row (column, left to right)
    /// stays where it is.
    pub header: bool,
}

/// What a level sorts on, and its order.
#[derive(Clone, Debug, PartialEq)]
pub enum SortOn {
    /// Values, ascending or not; with `list`, in that custom list's order
    /// (case-insensitive, unlisted values after the listed ones).
    Value {
        asc: bool,
        list: Option<Vec<String>>,
    },
    /// The cells showing this fill (`None`: No Fill) go on top or bottom.
    CellColor {
        rgb: Option<(u8, u8, u8)>,
        top: bool,
    },
    /// Likewise the font colour (`None`: automatic).
    FontColor {
        rgb: Option<(u8, u8, u8)>,
        top: bool,
    },
    /// Likewise the conditional-formatting icon.
    Icon { set: String, id: u32, top: bool },
}

/// One level: the absolute column (the row, left to right) it reads.
#[derive(Clone, Debug, PartialEq)]
pub struct SortLevel {
    pub key: u32,
    pub on: SortOn,
}

/// Why a sort moved nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortError {
    /// The range cuts a spilled or array formula's block.
    CutsSpill,
    /// The range's merged cells differ ([`SORT_MERGED`]).
    MergedSizes,
    /// Fewer than two rows (columns) to sort, or no level.
    Empty,
}

impl SortError {
    pub fn message(self) -> &'static str {
        match self {
            SortError::CutsSpill => super::SORT_CUTS_SPILL,
            SortError::MergedSizes => SORT_MERGED,
            SortError::Empty => "Nothing to sort.",
        }
    }
}

impl std::fmt::Display for SortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for SortError {}

/// A selection inside a wider list: Excel asks before sorting it (Expand
/// the selection, or Continue with the current selection). The list's
/// current region and whether its first row is a header, when the
/// selection is more than one cell and the region reaches past its
/// columns.
pub fn sort_warning(wb: &Workbook, sheet: usize, sel: Area) -> Option<(Area, bool)> {
    let (r1, c1, r2, c2) = sel;
    if (r1, c1) == (r2, c2) {
        return None;
    }
    let s = wb.sheets.get(sheet)?;
    let (region, header) = subtotal_region(s, r1, c1)?;
    (region.1 < c1 || region.3 > c2).then_some((region, header))
}

/// The range a single-cell sort acts on: the current region around `at`,
/// and whether its first row is a header (Excel's guess).
pub fn sort_region(wb: &Workbook, sheet: usize, at: (u32, u32)) -> Option<(Area, bool)> {
    subtotal_region(wb.sheets.get(sheet)?, at.0, at.1)
}

/// Sort `area` of `sheet` by `levels` (the first decides, the rest break
/// ties; equal units keep their order). Only the cells of the range move,
/// with their styles: whole rows of it, or whole columns left to right.
/// Hidden rows (columns) stay where they are, so a filtered list sorts only
/// the records it shows. Blanks sort last on a value level, whichever
/// direction. A range that cuts an array's block, or whose merged cells
/// differ, is refused; merges the same in every unit move with it.
/// Returns how many rows (columns) were sorted.
pub fn sort_range(
    wb: &mut Workbook,
    sheet: usize,
    area: Area,
    levels: &[SortLevel],
    opts: &SortOptions,
) -> Result<usize, SortError> {
    let ltr = opts.left_to_right;
    let s = wb.sheets.get(sheet).ok_or(SortError::Empty)?;
    let (used_r, used_c) = s.used_size();
    if levels.is_empty() || used_r == 0 {
        return Err(SortError::Empty);
    }
    let (r1, c1, mut r2, mut c2) = area;
    // `A:A` and `1:1` are the "whole column / row" idiom: what lies past the
    // used cells is empty.
    r2 = r2.min(used_r - 1);
    c2 = c2.min(used_c.saturating_sub(1));
    if r2 < r1 || c2 < c1 {
        return Err(SortError::Empty);
    }
    let (first, last) = if ltr { (c1, c2) } else { (r1, r2) };
    let first = first + u32::from(opts.header);
    let units: Vec<u32> = (first..=last)
        .filter(|&u| {
            if ltr {
                !s.col_hidden(u)
            } else {
                !s.row_hidden(u)
            }
        })
        .collect();
    if units.len() < 2 {
        return Err(SortError::Empty);
    }
    // The cell of unit `u` at position `p` across it.
    let at = |u: u32, p: u32| if ltr { (p, u) } else { (u, p) };
    let across = if ltr { r1..=r2 } else { c1..=c2 };
    let sorted_area = if ltr {
        (r1, first, r2, last)
    } else {
        (first, c1, last, c2)
    };
    if cuts_array(wb, sheet, sorted_area, ltr) {
        return Err(SortError::CutsSpill);
    }
    // Every sorted unit is merged the same way, so its merges stay valid
    // where they are whichever unit lands in each slot.
    check_merges(wb, sheet, sorted_area, &units, ltr)?;

    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by(|&a, &b| {
        for lv in levels {
            if !across.contains(&lv.key) {
                continue;
            }
            let o = level_cmp(
                wb,
                sheet,
                at(units[a], lv.key),
                at(units[b], lv.key),
                lv,
                opts,
            );
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
    let s = &mut wb.sheets[sheet];
    let take: Vec<Vec<Option<Cell>>> = units
        .iter()
        .map(|&u| {
            across
                .clone()
                .map(|p| {
                    let (r, c) = at(u, p);
                    s.cells.remove(&(r, c))
                })
                .collect()
        })
        .collect();
    let mut take: Vec<Option<Vec<Option<Cell>>>> = take.into_iter().map(Some).collect();
    for (slot, &from) in order.iter().enumerate() {
        let to = units[slot];
        let src = units[from];
        let Some(cells) = take[from].take() else {
            continue;
        };
        for (p, cell) in across.clone().zip(cells) {
            if let Some(mut cl) = cell {
                let (r, c) = at(to, p);
                if !ltr {
                    move_own_array_ref(&mut cl, (src, p), to);
                }
                s.set_cell(r, c, cl);
            }
        }
    }
    Ok(units.len())
}

/// Does `area` cut an array formula's block? Sorting rows, a block that
/// spans more than one row and meets them; left to right, more than one
/// column.
fn cuts_array(wb: &Workbook, sheet: usize, (r1, c1, r2, c2): Area, ltr: bool) -> bool {
    let Some(s) = wb.sheets.get(sheet) else {
        return false;
    };
    s.cells.iter().any(|(&(ar, ac), cell)| {
        array_rect(cell, (ar, ac)).is_some_and(|(h, w)| {
            let (br, bc) = (ar.saturating_add(h - 1), ac.saturating_add(w - 1));
            let meets = ar <= r2 && br >= r1 && ac <= c2 && bc >= c1;
            meets && if ltr { w > 1 } else { h > 1 }
        })
    })
}

/// Excel's rule for merged cells: when the range holds any, every unit
/// must be merged the same way (the same spans across it, each inside the
/// unit), and none may cross the range's edge. A mix of merged and
/// unmerged units is refused.
fn check_merges(
    wb: &Workbook,
    sheet: usize,
    (r1, c1, r2, c2): Area,
    units: &[u32],
    ltr: bool,
) -> Result<(), SortError> {
    let s = &wb.sheets[sheet];
    let meets: Vec<Area> = s
        .merges
        .iter()
        .copied()
        .filter(|&(a, b, c, d)| a <= r2 && c >= r1 && b <= c2 && d >= c1)
        .collect();
    if meets.is_empty() {
        return Ok(());
    }
    let mut spans: Vec<Vec<(u32, u32)>> = vec![Vec::new(); units.len()];
    for (a, b, c, d) in meets {
        if a < r1 || c > r2 || b < c1 || d > c2 {
            return Err(SortError::MergedSizes);
        }
        let (u_lo, u_hi, lo, hi) = if ltr { (b, d, a, c) } else { (a, c, b, d) };
        if u_lo != u_hi {
            return Err(SortError::MergedSizes);
        }
        if let Some(i) = units.iter().position(|&u| u == u_lo) {
            spans[i].push((lo, hi));
        }
    }
    for v in &mut spans {
        v.sort();
    }
    if spans.iter().any(|v| *v != spans[0]) {
        return Err(SortError::MergedSizes);
    }
    Ok(())
}

/// One level's order of two units' key cells.
fn level_cmp(
    wb: &Workbook,
    sheet: usize,
    a: (u32, u32),
    b: (u32, u32),
    lv: &SortLevel,
    opts: &SortOptions,
) -> Ordering {
    let s = &wb.sheets[sheet];
    let placed = |hit_a: bool, hit_b: bool, top: bool| {
        // Matching cells first (on top) or last; the rest keep their order.
        let o = hit_b.cmp(&hit_a);
        if top { o } else { o.reverse() }
    };
    match &lv.on {
        SortOn::Value { asc, list } => {
            let (ca, cb) = (s.cell(a.0, a.1), s.cell(b.0, b.1));
            let blank = |c: Option<&Cell>| c.is_none_or(|c| c.is_blank());
            match (blank(ca), blank(cb)) {
                (true, true) => return Ordering::Equal,
                (true, false) => return Ordering::Greater,
                (false, true) => return Ordering::Less,
                _ => {}
            }
            let (va, vb) = (&ca.unwrap().value, &cb.unwrap().value);
            let o = match list {
                Some(list) => {
                    let pos = |c: (u32, u32)| {
                        let t = crate::filter::shown_text(wb, sheet, c.0, c.1);
                        list.iter()
                            .position(|x| x.trim().eq_ignore_ascii_case(t.trim()))
                    };
                    match (pos(a), pos(b)) {
                        (Some(x), Some(y)) => x.cmp(&y),
                        (Some(_), None) => return Ordering::Less,
                        (None, Some(_)) => return Ordering::Greater,
                        (None, None) => value_cmp(va, vb, opts.case_sensitive),
                    }
                }
                None => value_cmp(va, vb, opts.case_sensitive),
            };
            if *asc { o } else { o.reverse() }
        }
        SortOn::CellColor { rgb, top } => placed(
            crate::cf::cell_fill(wb, sheet, a.0, a.1).is(*rgb),
            crate::cf::cell_fill(wb, sheet, b.0, b.1).is(*rgb),
            *top,
        ),
        SortOn::FontColor { rgb, top } => placed(
            crate::cf::cell_font_color(wb, sheet, a.0, a.1).is(*rgb),
            crate::cf::cell_font_color(wb, sheet, b.0, b.1).is(*rgb),
            *top,
        ),
        SortOn::Icon { set, id, top } => {
            let hit = |c: (u32, u32)| {
                crate::cf::cell_icon(wb, sheet, c.0, c.1)
                    .is_some_and(|(s, i)| s.eq_ignore_ascii_case(set) && i == *id)
            };
            placed(hit(a), hit(b), *top)
        }
    }
}

/// Two values in ascending order: numbers < text < booleans; text
/// case-insensitively, and with `case_sensitive` equal words then
/// lowercase before uppercase, letter by letter.
pub(crate) fn value_cmp(a: &CellValue, b: &CellValue, case_sensitive: bool) -> Ordering {
    let rank = |v: &CellValue| match v {
        CellValue::Number(_) => 0,
        CellValue::Text(_) => 1,
        CellValue::Bool(_) => 2,
        _ => 3,
    };
    match (a, b) {
        (CellValue::Number(x), CellValue::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (CellValue::Text(x), CellValue::Text(y)) => {
            let o = x.to_lowercase().cmp(&y.to_lowercase());
            if o != Ordering::Equal || !case_sensitive {
                return o;
            }
            for (p, q) in x.chars().zip(y.chars()) {
                if p == q {
                    continue;
                }
                return match (p.is_lowercase(), q.is_lowercase()) {
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    _ => p.cmp(&q),
                };
            }
            x.len().cmp(&y.len())
        }
        (CellValue::Bool(x), CellValue::Bool(y)) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)),
    }
}

#[cfg(test)]
mod tests;
