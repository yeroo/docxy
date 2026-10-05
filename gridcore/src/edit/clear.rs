//! Home › Clear, as Excel has it (#671): each item removes exactly its part
//! of the cells in the selected areas, and no cell moves.

use super::{Rect, rects_overlap as overlaps};
use crate::sheet::{Cell, Sheet};

/// Excel's refusal of a clear that would split a merged cell.
pub const MERGED_PART: &str = "Cannot change part of a merged cell.";

/// A Home › Clear menu item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClearWhat {
    /// Contents, formats, notes and hyperlinks.
    All,
    /// Formats only (style 0); merges inside go too.
    Formats,
    /// Contents only (Delete).
    Contents,
    /// Notes and threaded comments.
    Comments,
    /// The links, keeping each cell's style (the hyperlink font).
    Hyperlinks,
    /// The links and their formatting.
    RemoveHyperlinks,
}

impl ClearWhat {
    pub const ALL: [ClearWhat; 6] = [
        ClearWhat::All,
        ClearWhat::Formats,
        ClearWhat::Contents,
        ClearWhat::Comments,
        ClearWhat::Hyperlinks,
        ClearWhat::RemoveHyperlinks,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ClearWhat::All => "Clear All",
            ClearWhat::Formats => "Clear Formats",
            ClearWhat::Contents => "Clear Contents",
            ClearWhat::Comments => "Clear Comments and Notes",
            ClearWhat::Hyperlinks => "Clear Hyperlinks",
            ClearWhat::RemoveHyperlinks => "Remove Hyperlinks",
        }
    }

    pub fn from_label(s: &str) -> Option<ClearWhat> {
        let s = s.trim();
        ClearWhat::ALL.into_iter().find(|w| {
            w.label().eq_ignore_ascii_case(s)
                || w.label()
                    .strip_prefix("Clear ")
                    .is_some_and(|l| l.eq_ignore_ascii_case(s))
        })
    }

    fn unlinks(self) -> bool {
        matches!(
            self,
            ClearWhat::All | ClearWhat::Hyperlinks | ClearWhat::RemoveHyperlinks
        )
    }
}

/// What a Home › Clear changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClearPlan {
    /// Cell writes (a default cell clears one).
    pub cells: Vec<(u32, u32, Cell)>,
    /// Merges to undo.
    pub unmerge: Vec<Rect>,
    /// Cells whose hyperlink goes, and the loaded `ref`s that go with them.
    pub unlink: Vec<(u32, u32)>,
    pub unlinked_refs: Vec<Rect>,
    /// Cells whose notes go (the host removes them from its package).
    pub notes: Vec<(u32, u32)>,
}

impl ClearPlan {
    /// Whether it changes anything.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
            && self.unmerge.is_empty()
            && self.unlink.is_empty()
            && self.notes.is_empty()
    }
}

fn inside(r: u32, c: u32, a: Rect) -> bool {
    (a.0..=a.2).contains(&r) && (a.1..=a.3).contains(&c)
}

/// The changes Home › Clear `what` makes over `areas` of `sheet`.
/// `note_cells` are the cells of the sheet that carry a note or a comment
/// (the package holds them). A range hyperlink touched anywhere goes as a
/// whole. Clear All and Clear Formats undo the merges inside the areas and
/// refuse ([`MERGED_PART`]) one that only partly is.
pub fn clear_plan(
    sheet: &Sheet,
    areas: &[Rect],
    what: ClearWhat,
    note_cells: &[(u32, u32)],
) -> Result<ClearPlan, &'static str> {
    let mut plan = ClearPlan::default();
    let in_areas = |r: u32, c: u32| areas.iter().any(|&a| inside(r, c, a));
    if matches!(what, ClearWhat::All | ClearWhat::Formats) {
        for &m in &sheet.merges {
            if !areas.iter().any(|&a| overlaps(a, m)) {
                continue;
            }
            if !areas
                .iter()
                .any(|&a| a.0 <= m.0 && a.1 <= m.1 && m.2 <= a.2 && m.3 <= a.3)
            {
                return Err(MERGED_PART);
            }
            plan.unmerge.push(m);
        }
    }
    if what.unlinks() {
        let mut gone = std::collections::BTreeSet::new();
        for &(r, c) in sheet.hyperlinks.keys() {
            if !in_areas(r, c) {
                continue;
            }
            gone.insert((r, c));
            // The loaded link this cell belongs to goes whole.
            if let Some((_, &rect)) = sheet
                .hyperlink_refs
                .iter()
                .find(|(_, rect)| inside(r, c, **rect))
            {
                if !plan.unlinked_refs.contains(&rect) {
                    plan.unlinked_refs.push(rect);
                }
            }
        }
        for &rect in &plan.unlinked_refs {
            for &(r, c) in sheet.hyperlinks.keys() {
                if inside(r, c, rect) {
                    gone.insert((r, c));
                }
            }
        }
        plan.unlink = gone.into_iter().collect();
    }
    if matches!(what, ClearWhat::All | ClearWhat::Comments) {
        plan.notes = note_cells
            .iter()
            .copied()
            .filter(|&(r, c)| in_areas(r, c))
            .collect();
    }
    let mut seen = std::collections::BTreeSet::new();
    for &(r0, c0, r1, c1) in areas {
        for (&(r, c), cell) in sheet.cells.range((r0, c0)..=(r1, c1)) {
            if !(c0..=c1).contains(&c) || !seen.insert((r, c)) {
                continue;
            }
            let next = match what {
                ClearWhat::All => Cell::default(),
                ClearWhat::Contents => Cell {
                    style: cell.style,
                    ..Cell::default()
                },
                ClearWhat::Formats => Cell {
                    style: 0,
                    ..cell.clone()
                },
                ClearWhat::RemoveHyperlinks if plan.unlink.contains(&(r, c)) => Cell {
                    style: 0,
                    ..cell.clone()
                },
                _ => continue,
            };
            if next != *cell {
                plan.cells.push((r, c, next));
            }
        }
    }
    // A Remove Hyperlinks reaching cells of a range link outside the areas
    // resets their style too, so the whole link reads as gone.
    if what == ClearWhat::RemoveHyperlinks {
        for &(r, c) in &plan.unlink {
            if seen.contains(&(r, c)) {
                continue;
            }
            if let Some(cell) = sheet.cell(r, c) {
                if cell.style != 0 {
                    plan.cells.push((
                        r,
                        c,
                        Cell {
                            style: 0,
                            ..cell.clone()
                        },
                    ));
                }
            }
        }
    }
    Ok(plan)
}

/// The sheet-level part of a clear: merges undone and links removed (the
/// cells are the host's to write, through its engine).
pub fn apply_clear_sheet(sheet: &mut Sheet, plan: &ClearPlan) {
    sheet.merges.retain(|m| !plan.unmerge.contains(m));
    for rc in &plan.unlink {
        sheet.hyperlinks.remove(rc);
    }
    for &rect in &plan.unlinked_refs {
        sheet.hyperlink_refs.remove(&(rect.0, rect.1));
        if !sheet.hyperlinks_removed.contains(&rect) {
            sheet.hyperlinks_removed.push(rect);
        }
    }
}

/// Home › Clear `what` over `areas` of `wb`'s sheet `sheet`, cells and all.
/// Notes are the host's (it removes [`ClearPlan::notes`] from its package).
pub fn apply_clear(
    wb: &mut crate::sheet::Workbook,
    sheet: usize,
    areas: &[Rect],
    what: ClearWhat,
    note_cells: &[(u32, u32)],
) -> Result<ClearPlan, &'static str> {
    let s = wb.sheets.get_mut(sheet).ok_or("No such sheet")?;
    let plan = clear_plan(s, areas, what, note_cells)?;
    for (r, c, cell) in plan.cells.clone() {
        s.set_cell(r, c, cell);
    }
    apply_clear_sheet(s, &plan);
    Ok(plan)
}

#[cfg(test)]
#[path = "clear/tests.rs"]
mod tests;
