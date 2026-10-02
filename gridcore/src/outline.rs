//! Excel's outline: grouped rows and columns, their levels, and collapsing
//! them.
//!
//! The levels, `hidden` and `collapsed` flags live where the file keeps them,
//! in the `<row>` attributes and `<col>` definitions (see
//! [`Sheet::row_outline`] and [`Sheet::col_outline`]); a group is not stored,
//! it is a maximal run of rows (or columns) at or above a level. Its summary
//! row sits right after the run, or right before it when the sheet's
//! [`OutlineSettings`] say the summary is above (left of) the detail; the
//! summary carries `collapsed="1"` while the group's detail is hidden.
//!
//! These operations change one [`Sheet`] and never touch formulas (Auto
//! Outline only reads them), so the UIs wrap them in their structural
//! undo.

use std::collections::BTreeSet;
use std::fmt;

use crate::sheet::{MAX_COLS, MAX_ROWS, Sheet};

pub use crate::sheet::OutlineSettings;

/// Excel's deepest outline level: eight level buttons, detail at 1..=7.
pub const MAX_LEVEL: u8 = 7;

/// Which way an outline runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    Rows,
    Cols,
}

/// Why an outline command changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutlineError {
    /// Grouping would go deeper than [`MAX_LEVEL`].
    TooDeep,
    /// Ungroup or Clear Outline found nothing grouped.
    NotGrouped,
    /// Show/Hide Detail found no group at that row or column.
    NoGroup,
    /// Auto Outline found no summary formulas.
    NoSummaries,
}

impl fmt::Display for OutlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OutlineError::TooDeep => "Cannot group: an outline is limited to eight levels.",
            OutlineError::NotGrouped => "Cannot ungroup: there is no outline here.",
            OutlineError::NoGroup => "There is no group here to show or hide.",
            OutlineError::NoSummaries => "Cannot create an outline: no summary formulas found.",
        })
    }
}

impl std::error::Error for OutlineError {}

/// One group: the run `start..=end` whose indices all sit at `level` or
/// deeper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Group {
    pub level: u8,
    pub start: u32,
    pub end: u32,
    /// The summary row (column): after the run, or before it when summaries
    /// sit above (left). `None` when that would fall off the sheet.
    pub summary: Option<u32>,
    /// Whether the group is collapsed: its summary's `collapsed` flag, or,
    /// with no summary, whether its whole run is hidden.
    pub collapsed: bool,
}

impl Group {
    pub fn contains(&self, i: u32) -> bool {
        i >= self.start && i <= self.end
    }
}

/// The outline level of row or column `i`.
pub fn level(s: &Sheet, axis: Axis, i: u32) -> u8 {
    match axis {
        Axis::Rows => s.row_outline(i),
        Axis::Cols => s.col_outline(i),
    }
}

/// Set the outline level of row or column `i` (clamped to [`MAX_LEVEL`]).
pub fn set_level(s: &mut Sheet, axis: Axis, i: u32, lvl: u8) {
    let lvl = lvl.min(MAX_LEVEL);
    match axis {
        Axis::Rows => s.set_row_outline(i, lvl),
        Axis::Cols => s.set_col_outline(i, lvl),
    }
}

/// Whether row or column `i` is hidden.
pub fn hidden(s: &Sheet, axis: Axis, i: u32) -> bool {
    match axis {
        Axis::Rows => s.row_hidden(i),
        Axis::Cols => s.col_hidden(i),
    }
}

fn set_hidden(s: &mut Sheet, axis: Axis, i: u32, on: bool) {
    match axis {
        Axis::Rows => s.set_row_hidden(i, on),
        Axis::Cols => s.set_col_hidden(i, on),
    }
}

/// Whether row or column `i` carries the `collapsed` flag.
pub fn collapsed(s: &Sheet, axis: Axis, i: u32) -> bool {
    match axis {
        Axis::Rows => s.row_collapsed(i),
        Axis::Cols => s.col_collapsed(i),
    }
}

fn set_collapsed(s: &mut Sheet, axis: Axis, i: u32, on: bool) {
    if collapsed(s, axis, i) == on {
        return;
    }
    match axis {
        Axis::Rows => s.set_row_collapsed(i, on),
        Axis::Cols => s.set_col_collapsed(i, on),
    }
}

/// Unhide `i` unless an applied filter hid it: a filter's rows stay hidden
/// whatever the outline does.
fn show(s: &mut Sheet, axis: Axis, i: u32) {
    if axis == Axis::Rows && s.row_filtered(i) {
        return;
    }
    if hidden(s, axis, i) {
        set_hidden(s, axis, i, false);
    }
}

fn hide(s: &mut Sheet, axis: Axis, i: u32) {
    if !hidden(s, axis, i) {
        set_hidden(s, axis, i, true);
    }
}

/// The deepest level on that axis; 0 when it has no outline.
pub fn max_level(s: &Sheet, axis: Axis) -> u8 {
    match axis {
        Axis::Rows => s.max_row_outline(),
        Axis::Cols => s.max_col_outline(),
    }
}

/// Every row (column) at level 1 or deeper, with its level, in order.
fn leveled(s: &Sheet, axis: Axis) -> Vec<(u32, u8)> {
    match axis {
        Axis::Rows => s
            .row_attrs
            .keys()
            .map(|&r| (r, s.row_outline(r)))
            .filter(|&(_, l)| l > 0)
            .collect(),
        Axis::Cols => s
            .col_defs
            .iter()
            .flat_map(|d| d.min..=d.max)
            .map(|c| (c, s.col_outline(c)))
            .filter(|&(_, l)| l > 0)
            .collect(),
    }
}

/// Every row (column) carrying a `collapsed` flag.
fn flagged(s: &Sheet, axis: Axis) -> Vec<u32> {
    match axis {
        Axis::Rows => s
            .row_attrs
            .keys()
            .copied()
            .filter(|&r| s.row_collapsed(r))
            .collect(),
        Axis::Cols => s
            .col_defs
            .iter()
            .flat_map(|d| d.min..=d.max)
            .filter(|&c| s.col_collapsed(c))
            .collect(),
    }
}

fn summary_after(s: &Sheet, axis: Axis) -> bool {
    match axis {
        Axis::Rows => s.outline.summary_below,
        Axis::Cols => s.outline.summary_right,
    }
}

fn axis_len(axis: Axis) -> u32 {
    match axis {
        Axis::Rows => MAX_ROWS,
        Axis::Cols => MAX_COLS,
    }
}

/// The groups on an axis, outermost first: for each level, the maximal runs
/// of rows (columns) at that level or deeper.
pub fn groups(s: &Sheet, axis: Axis) -> Vec<Group> {
    let rows = leveled(s, axis);
    let after = summary_after(s, axis);
    let mut out = Vec::new();
    let max = rows.iter().map(|&(_, l)| l).max().unwrap_or(0);
    for lvl in 1..=max {
        let mut run: Option<(u32, u32)> = None;
        let close = |run: (u32, u32), out: &mut Vec<Group>| {
            let (start, end) = run;
            let summary = if after {
                (end + 1 < axis_len(axis)).then_some(end + 1)
            } else {
                start.checked_sub(1)
            };
            let collapsed = match summary {
                Some(i) => collapsed(s, axis, i),
                None => (start..=end).all(|i| hidden(s, axis, i)),
            };
            out.push(Group {
                level: lvl,
                start,
                end,
                summary,
                collapsed,
            });
        };
        for &(i, l) in &rows {
            if l < lvl {
                continue;
            }
            run = match run {
                Some((a, b)) if b + 1 == i => Some((a, i)),
                Some(r) => {
                    close(r, &mut out);
                    Some((i, i))
                }
                None => Some((i, i)),
            };
        }
        if let Some(r) = run {
            close(r, &mut out);
        }
    }
    out
}

/// Group rows (columns) `a..=b`: one level deeper each. Refuses, changing
/// nothing, when any would pass [`MAX_LEVEL`].
pub fn group(s: &mut Sheet, axis: Axis, a: u32, b: u32) -> Result<(), OutlineError> {
    let (a, b) = (a.min(b), a.max(b));
    if (a..=b).any(|i| level(s, axis, i) >= MAX_LEVEL) {
        return Err(OutlineError::TooDeep);
    }
    for i in a..=b {
        let l = level(s, axis, i);
        set_level(s, axis, i, l + 1);
    }
    Ok(())
}

/// Ungroup rows (columns) `a..=b`: one level shallower each, where grouped.
/// Hidden rows stay hidden, as in Excel; a `collapsed` flag left without a
/// group goes. Refuses when nothing in the range is grouped.
pub fn ungroup(s: &mut Sheet, axis: Axis, a: u32, b: u32) -> Result<(), OutlineError> {
    let (a, b) = (a.min(b), a.max(b));
    if (a..=b).all(|i| level(s, axis, i) == 0) {
        return Err(OutlineError::NotGrouped);
    }
    for i in a..=b {
        let l = level(s, axis, i);
        if l > 0 {
            set_level(s, axis, i, l - 1);
        }
    }
    drop_stale_flags(s, axis);
    Ok(())
}

/// Clear `collapsed` on rows (columns) that no longer summarize a group.
fn drop_stale_flags(s: &mut Sheet, axis: Axis) {
    let summaries: BTreeSet<u32> = groups(s, axis).iter().filter_map(|g| g.summary).collect();
    for i in flagged(s, axis) {
        if !summaries.contains(&i) {
            set_collapsed(s, axis, i, false);
        }
    }
}

/// Collapse one group: hide its run and flag its summary.
pub fn collapse_group(s: &mut Sheet, axis: Axis, g: &Group) {
    for i in g.start..=g.end {
        hide(s, axis, i);
    }
    if let Some(i) = g.summary {
        set_collapsed(s, axis, i, true);
    }
}

/// Expand one group: show its run, except the detail of groups inside it
/// that are themselves collapsed (Excel keeps those folded), and rows a
/// filter hid.
pub fn expand_group(s: &mut Sheet, axis: Axis, g: &Group) {
    let inner: Vec<Group> = groups(s, axis)
        .into_iter()
        .filter(|h| h.level > g.level && h.start >= g.start && h.end <= g.end && h.collapsed)
        .collect();
    for i in g.start..=g.end {
        if !inner.iter().any(|h| h.contains(i)) {
            show(s, axis, i);
        }
    }
    if let Some(i) = g.summary {
        set_collapsed(s, axis, i, false);
    }
}

/// Collapse a group if expanded, expand it if collapsed (the +/- button).
pub fn toggle_group(s: &mut Sheet, axis: Axis, g: &Group) {
    if g.collapsed {
        expand_group(s, axis, g);
    } else {
        collapse_group(s, axis, g);
    }
}

/// The level buttons: show levels below `n` (1..=8) and hide the rest.
/// Only grouped rows (columns) change; one hidden by hand outside any group,
/// or by a filter, stays as it is. The `collapsed` flags follow.
pub fn show_level(s: &mut Sheet, axis: Axis, n: u8) {
    let n = n.clamp(1, MAX_LEVEL + 1);
    for (i, l) in leveled(s, axis) {
        if l >= n {
            hide(s, axis, i);
        } else {
            show(s, axis, i);
        }
    }
    for g in groups(s, axis) {
        if let Some(i) = g.summary {
            set_collapsed(s, axis, i, g.level >= n);
        }
    }
}

/// The group Show/Hide Detail acts on at `i`: one whose summary is `i`,
/// else the innermost one containing `i`; `want_collapsed` picks folded
/// groups (Show Detail) or open ones (Hide Detail).
fn group_at(s: &Sheet, axis: Axis, i: u32, want_collapsed: bool) -> Option<Group> {
    let all = groups(s, axis);
    let by_summary: Vec<&Group> = all.iter().filter(|g| g.summary == Some(i)).collect();
    if !by_summary.is_empty() {
        // Show Detail opens the outermost folded group; Hide Detail folds the
        // innermost open one.
        let pick = if want_collapsed {
            by_summary
                .iter()
                .filter(|g| g.collapsed)
                .min_by_key(|g| g.level)
        } else {
            by_summary
                .iter()
                .filter(|g| !g.collapsed)
                .max_by_key(|g| g.level)
        };
        if let Some(g) = pick {
            return Some(**g);
        }
    }
    all.iter()
        .filter(|g| g.contains(i) && g.collapsed == want_collapsed)
        .max_by_key(|g| g.level)
        .copied()
}

/// Show Detail at row (column) `i`: expand the group it summarizes or sits
/// in.
pub fn show_detail(s: &mut Sheet, axis: Axis, i: u32) -> Result<(), OutlineError> {
    let g = group_at(s, axis, i, true).ok_or(OutlineError::NoGroup)?;
    expand_group(s, axis, &g);
    Ok(())
}

/// Hide Detail at row (column) `i`: collapse the group it summarizes or sits
/// in.
pub fn hide_detail(s: &mut Sheet, axis: Axis, i: u32) -> Result<(), OutlineError> {
    let g = group_at(s, axis, i, false).ok_or(OutlineError::NoGroup)?;
    collapse_group(s, axis, &g);
    Ok(())
}

/// Clear Outline: every row and column level to 0 and every `collapsed` flag
/// cleared. Hidden rows and columns stay hidden, as in Excel. Refuses when
/// the sheet has no outline.
pub fn clear_outline(s: &mut Sheet) -> Result<(), OutlineError> {
    if max_level(s, Axis::Rows) == 0 && max_level(s, Axis::Cols) == 0 {
        return Err(OutlineError::NotGrouped);
    }
    clear_axis(s, Axis::Rows);
    clear_axis(s, Axis::Cols);
    Ok(())
}

fn clear_axis(s: &mut Sheet, axis: Axis) {
    for (i, _) in leveled(s, axis) {
        set_level(s, axis, i, 0);
    }
    for i in flagged(s, axis) {
        set_collapsed(s, axis, i, false);
    }
}

/// Auto Outline: build the outline from summary formulas in `area` (r1, c1,
/// r2, c2), or the used range when `None`.
///
/// A formula at (r, c) referring to a same-sheet range in column c that ends
/// at r-1 (or starts at r+1) makes those rows detail of row r; one referring
/// to a range in row r ending at c-1 (or starting at c+1) does the same for
/// columns. A row's level is how many such ranges contain it, so a grand
/// total over the subtotals nests them. Only ranges of two or more cells
/// count: a total written `=B5+B9` makes no group. Each axis with summaries has its outline
/// replaced, and its summary setting follows the direction found.
pub fn auto_outline(s: &mut Sheet, area: Option<(u32, u32, u32, u32)>) -> Result<(), OutlineError> {
    let (r1, c1, r2, c2) = match area {
        Some(a) => a,
        None => {
            let (rows, cols) = s.used_size();
            if rows == 0 || cols == 0 {
                return Err(OutlineError::NoSummaries);
            }
            (0, 0, rows - 1, cols - 1)
        }
    };
    // (detail start, detail end, summary after the detail?) per axis.
    let mut row_ranges: BTreeSet<(u32, u32, bool)> = BTreeSet::new();
    let mut col_ranges: BTreeSet<(u32, u32, bool)> = BTreeSet::new();
    let me = s.name.to_ascii_lowercase();
    for (&(r, c), cell) in s.cells.range((r1, 0)..=(r2, u32::MAX)) {
        if c < c1 || c > c2 {
            continue;
        }
        let Some(src) = cell.formula.as_deref() else {
            continue;
        };
        let Ok(ast) = crate::formula::parse(src) else {
            continue;
        };
        let mut refs = Vec::new();
        crate::formula::collect_refs(&ast, &mut refs);
        for (sheet, a1, b1, a2, b2) in refs {
            if sheet.is_some_and(|n| n.to_ascii_lowercase() != me) {
                continue;
            }
            // A single cell is not a detail range: `=A1*2` below A1 is no
            // total.
            if b1 == c && b2 == c && a1 < a2 && !(a1 <= r && r <= a2) {
                if a2 + 1 == r {
                    row_ranges.insert((a1, a2, true));
                } else if a1 == r + 1 {
                    row_ranges.insert((a1, a2, false));
                }
            } else if a1 == r && a2 == r && b1 < b2 && !(b1 <= c && c <= b2) {
                if b2 + 1 == c {
                    col_ranges.insert((b1, b2, true));
                } else if b1 == c + 1 {
                    col_ranges.insert((b1, b2, false));
                }
            }
        }
    }
    if row_ranges.is_empty() && col_ranges.is_empty() {
        return Err(OutlineError::NoSummaries);
    }
    let depth = |ranges: &BTreeSet<(u32, u32, bool)>, i: u32| {
        ranges.iter().filter(|&&(a, b, _)| a <= i && i <= b).count()
    };
    let deepest = |ranges: &BTreeSet<(u32, u32, bool)>| {
        ranges
            .iter()
            .flat_map(|&(a, b, _)| [a, b])
            .map(|i| depth(ranges, i))
            .max()
            .unwrap_or(0)
    };
    // Depth peaks at a range end, so the ends are enough to check.
    if deepest(&row_ranges) > MAX_LEVEL as usize || deepest(&col_ranges) > MAX_LEVEL as usize {
        return Err(OutlineError::TooDeep);
    }
    for (axis, ranges) in [(Axis::Rows, &row_ranges), (Axis::Cols, &col_ranges)] {
        if ranges.is_empty() {
            continue;
        }
        clear_axis(s, axis);
        let covered: BTreeSet<u32> = ranges.iter().flat_map(|&(a, b, _)| a..=b).collect();
        for i in covered {
            set_level(s, axis, i, depth(ranges, i) as u8);
        }
        let after = ranges.iter().all(|&(_, _, after)| after);
        let before = ranges.iter().all(|&(_, _, after)| !after);
        if after || before {
            match axis {
                Axis::Rows => s.outline.summary_below = after,
                Axis::Cols => s.outline.summary_right = after,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Cell;

    fn rows(s: &Sheet, a: u32, b: u32) -> Vec<u8> {
        (a..=b).map(|r| s.row_outline(r)).collect()
    }

    fn hidden_rows(s: &Sheet, a: u32, b: u32) -> Vec<u32> {
        (a..=b).filter(|&r| s.row_hidden(r)).collect()
    }

    /// Rows 1..=3 and 5..=7 grouped inside an outer group 1..=8; summaries
    /// below at 4, 8 (inner) and 9 (outer).
    fn nested() -> Sheet {
        let mut s = Sheet::default();
        group(&mut s, Axis::Rows, 1, 8).unwrap();
        group(&mut s, Axis::Rows, 1, 3).unwrap();
        group(&mut s, Axis::Rows, 5, 7).unwrap();
        s
    }

    #[test]
    fn group_and_ungroup_nest_and_refuse() {
        let mut s = nested();
        assert_eq!(rows(&s, 0, 9), [0, 2, 2, 2, 1, 2, 2, 2, 1, 0]);
        assert_eq!(s.max_row_outline(), 2);
        // Ungroup lowers only what is grouped; nothing grouped refuses.
        ungroup(&mut s, Axis::Rows, 3, 4).unwrap();
        assert_eq!(rows(&s, 0, 9), [0, 2, 2, 1, 0, 2, 2, 2, 1, 0]);
        let before = s.row_attrs.clone();
        assert_eq!(
            ungroup(&mut s, Axis::Rows, 20, 30),
            Err(OutlineError::NotGrouped)
        );
        assert_eq!(s.row_attrs, before);
    }

    #[test]
    fn group_refuses_past_level_seven() {
        let mut s = Sheet::default();
        for _ in 0..7 {
            group(&mut s, Axis::Rows, 2, 4).unwrap();
        }
        assert_eq!(s.row_outline(3), 7);
        group(&mut s, Axis::Rows, 5, 6).unwrap();
        let before = s.row_attrs.clone();
        assert_eq!(group(&mut s, Axis::Rows, 4, 6), Err(OutlineError::TooDeep));
        assert_eq!(s.row_attrs, before, "a refusal changes nothing");
    }

    #[test]
    fn groups_derive_runs_and_summaries() {
        let s = nested();
        let g = groups(&s, Axis::Rows);
        let spans: Vec<(u8, u32, u32, Option<u32>)> = g
            .iter()
            .map(|g| (g.level, g.start, g.end, g.summary))
            .collect();
        assert_eq!(
            spans,
            [(1, 1, 8, Some(9)), (2, 1, 3, Some(4)), (2, 5, 7, Some(8))]
        );
        // Summary above: before the run.
        let mut s = nested();
        s.outline.summary_below = false;
        let g = groups(&s, Axis::Rows);
        assert_eq!(g[0].summary, Some(0));
        assert_eq!(g[2].summary, Some(4));
    }

    #[test]
    fn summary_above_a_group_at_row_one_has_no_summary() {
        let mut s = Sheet::default();
        s.outline.summary_below = false;
        group(&mut s, Axis::Rows, 0, 2).unwrap();
        let g = groups(&s, Axis::Rows)[0];
        assert_eq!(g.summary, None);
        collapse_group(&mut s, Axis::Rows, &g);
        assert_eq!(hidden_rows(&s, 0, 3), [0, 1, 2]);
        assert!(
            !(0..4).any(|r| s.row_collapsed(r)),
            "no summary row to flag"
        );
        // With no summary, the hidden run itself reads as collapsed.
        let g = groups(&s, Axis::Rows)[0];
        assert!(g.collapsed);
        expand_group(&mut s, Axis::Rows, &g);
        assert!(hidden_rows(&s, 0, 3).is_empty());
    }

    #[test]
    fn expanding_keeps_collapsed_inner_groups_folded() {
        let mut s = nested();
        let g = groups(&s, Axis::Rows);
        collapse_group(&mut s, Axis::Rows, &g[1]); // rows 1..=3
        assert!(s.row_collapsed(4));
        let g = groups(&s, Axis::Rows);
        collapse_group(&mut s, Axis::Rows, &g[0]); // everything 1..=8
        assert!(s.row_collapsed(9));
        assert_eq!(hidden_rows(&s, 0, 9), [1, 2, 3, 4, 5, 6, 7, 8]);
        let g = groups(&s, Axis::Rows);
        expand_group(&mut s, Axis::Rows, &g[0]);
        assert_eq!(hidden_rows(&s, 0, 9), [1, 2, 3], "the inner fold stays");
        assert!(!s.row_collapsed(9) && s.row_collapsed(4));
    }

    #[test]
    fn expanding_never_shows_filtered_rows() {
        let mut s = nested();
        s.set_row_filtered(2, true);
        let g = groups(&s, Axis::Rows);
        collapse_group(&mut s, Axis::Rows, &g[0]);
        let g = groups(&s, Axis::Rows);
        expand_group(&mut s, Axis::Rows, &g[0]);
        assert_eq!(hidden_rows(&s, 0, 9), [2]);
        show_level(&mut s, Axis::Rows, 8);
        assert_eq!(hidden_rows(&s, 0, 9), [2]);
        hide_detail(&mut s, Axis::Rows, 4).unwrap();
        show_detail(&mut s, Axis::Rows, 4).unwrap();
        assert_eq!(hidden_rows(&s, 0, 9), [2]);
    }

    #[test]
    fn show_level_hides_deeper_levels_and_sets_flags() {
        let mut s = nested();
        // A row hidden by hand outside any group is left alone.
        s.set_row_hidden(12, true);
        show_level(&mut s, Axis::Rows, 2);
        assert_eq!(hidden_rows(&s, 0, 12), [1, 2, 3, 5, 6, 7, 12]);
        assert!(s.row_collapsed(4) && s.row_collapsed(8) && !s.row_collapsed(9));
        show_level(&mut s, Axis::Rows, 1);
        assert_eq!(hidden_rows(&s, 0, 12), [1, 2, 3, 4, 5, 6, 7, 8, 12]);
        assert!(s.row_collapsed(9));
        show_level(&mut s, Axis::Rows, 3);
        assert_eq!(hidden_rows(&s, 0, 12), [12]);
        assert!(!(0..13).any(|r| s.row_collapsed(r)));
    }

    #[test]
    fn show_and_hide_detail_pick_the_group_at_the_summary_or_inside() {
        let mut s = nested();
        hide_detail(&mut s, Axis::Rows, 8).unwrap(); // the summary of 5..=7
        assert_eq!(hidden_rows(&s, 0, 9), [5, 6, 7]);
        hide_detail(&mut s, Axis::Rows, 2).unwrap(); // inside 1..=3
        assert_eq!(hidden_rows(&s, 0, 9), [1, 2, 3, 5, 6, 7]);
        show_detail(&mut s, Axis::Rows, 8).unwrap();
        assert_eq!(hidden_rows(&s, 0, 9), [1, 2, 3]);
        assert_eq!(
            hide_detail(&mut s, Axis::Rows, 20),
            Err(OutlineError::NoGroup)
        );
    }

    #[test]
    fn ungroup_keeps_hidden_rows_and_drops_stale_flags() {
        let mut s = Sheet::default();
        group(&mut s, Axis::Rows, 1, 3).unwrap();
        let g = groups(&s, Axis::Rows)[0];
        collapse_group(&mut s, Axis::Rows, &g);
        assert!(s.row_collapsed(4));
        ungroup(&mut s, Axis::Rows, 1, 3).unwrap();
        assert_eq!(hidden_rows(&s, 0, 4), [1, 2, 3], "Excel leaves them hidden");
        assert!(!s.row_collapsed(4), "no group left to be collapsed");
    }

    #[test]
    fn clear_outline_clears_levels_and_flags_on_both_axes() {
        let mut s = nested();
        group(&mut s, Axis::Cols, 2, 3).unwrap();
        show_level(&mut s, Axis::Rows, 1);
        show_level(&mut s, Axis::Cols, 1);
        clear_outline(&mut s).unwrap();
        assert_eq!(s.max_row_outline(), 0);
        assert_eq!(s.max_col_outline(), 0);
        assert!(!(0..10).any(|r| s.row_collapsed(r)));
        assert!(!s.col_collapsed(4));
        assert!(s.row_hidden(2) && s.col_hidden(2), "hidden stays hidden");
        assert_eq!(clear_outline(&mut s), Err(OutlineError::NotGrouped));
    }

    #[test]
    fn column_outline_splits_and_merges_col_defs() {
        let mut s = Sheet::default();
        s.col_defs.push(crate::sheet::ColDef {
            min: 0,
            max: 9,
            width: Some(12.0),
            attrs: " style=\"3\"".into(),
        });
        group(&mut s, Axis::Cols, 2, 4).unwrap();
        assert_eq!(
            (0..10).map(|c| s.col_outline(c)).collect::<Vec<_>>(),
            [0, 0, 1, 1, 1, 0, 0, 0, 0, 0]
        );
        // One definition per distinct run, every part keeping width and style.
        assert_eq!(s.col_defs.len(), 3, "{:?}", s.col_defs);
        assert!(s.col_defs.iter().all(|d| d.width == Some(12.0)));
        assert!(s.col_defs.iter().all(|d| d.attrs.contains("style=\"3\"")));
        let g = groups(&s, Axis::Cols)[0];
        assert_eq!(g.summary, Some(5));
        collapse_group(&mut s, Axis::Cols, &g);
        assert!((2..=4).all(|c| s.col_hidden(c)) && s.col_collapsed(5));
        ungroup(&mut s, Axis::Cols, 2, 4).unwrap();
        assert!(!s.col_collapsed(5));
    }

    #[test]
    fn collapsed_reads_true_and_false_spellings() {
        let mut s = Sheet::default();
        s.row_attrs.insert(1, " collapsed=\"true\"".into());
        s.row_attrs.insert(2, " collapsed=\"false\"".into());
        s.row_attrs.insert(3, " collapsed=\"0\"".into());
        s.row_attrs.insert(4, " collapsed=\"1\"".into());
        assert!(s.row_collapsed(1) && !s.row_collapsed(2));
        assert!(!s.row_collapsed(3) && s.row_collapsed(4));
    }

    fn put(s: &mut Sheet, cell: &str, v: &str) {
        let (r, c) = crate::sheet::parse_cell_name(cell).unwrap();
        let cell = match v.strip_prefix('=') {
            Some(f) => Cell::formula(f),
            None => Cell::number(v.parse().unwrap()),
        };
        s.set_cell(r, c, cell);
    }

    #[test]
    fn auto_outline_nests_subtotals_under_a_grand_total() {
        let mut s = Sheet {
            name: "Data".into(),
            ..Sheet::default()
        };
        for (cell, v) in [
            ("B2", "1"),
            ("B3", "2"),
            ("B4", "3"),
            ("B5", "=SUM(B2:B4)"),
            ("B6", "4"),
            ("B7", "5"),
            ("B8", "6"),
            ("B9", "=SUM(B6:B8)"),
            ("B10", "=SUM(B2:B9)"),
        ] {
            put(&mut s, cell, v);
        }
        auto_outline(&mut s, None).unwrap();
        assert_eq!(rows(&s, 0, 9), [0, 2, 2, 2, 1, 2, 2, 2, 1, 0]);
        assert!(s.outline.summary_below);
        assert_eq!(s.max_col_outline(), 0);
    }

    #[test]
    fn auto_outline_finds_column_totals_and_summaries_above() {
        let mut s = Sheet::default();
        for (cell, v) in [("B1", "1"), ("C1", "2"), ("D1", "=SUM(B1:C1)")] {
            put(&mut s, cell, v);
        }
        for (cell, v) in [("A3", "=SUM(A4:A5)"), ("A4", "1"), ("A5", "2")] {
            put(&mut s, cell, v);
        }
        auto_outline(&mut s, None).unwrap();
        assert_eq!(s.col_outline(1), 1);
        assert_eq!(s.col_outline(2), 1);
        assert_eq!(s.col_outline(3), 0);
        assert_eq!(rows(&s, 2, 4), [0, 1, 1]);
        assert!(!s.outline.summary_below, "the total sits above its detail");
        assert!(s.outline.summary_right);
    }

    #[test]
    fn auto_outline_without_summaries_changes_nothing() {
        let mut s = nested();
        put(&mut s, "A1", "5");
        put(&mut s, "A2", "=A1*2");
        let before = s.row_attrs.clone();
        assert_eq!(auto_outline(&mut s, None), Err(OutlineError::NoSummaries));
        assert_eq!(s.row_attrs, before);
        // Single-cell sums are not ranges: no group comes from them.
        put(&mut s, "A3", "=A1+A2");
        assert_eq!(auto_outline(&mut s, None), Err(OutlineError::NoSummaries));
    }

    #[test]
    fn auto_outline_replaces_the_existing_outline() {
        let mut s = Sheet::default();
        group(&mut s, Axis::Rows, 10, 12).unwrap();
        for (cell, v) in [("A1", "1"), ("A2", "2"), ("A3", "=SUM(A1:A2)")] {
            put(&mut s, cell, v);
        }
        auto_outline(&mut s, None).unwrap();
        assert_eq!(rows(&s, 0, 12), [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
}
