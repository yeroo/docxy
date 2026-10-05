//! Home › Clear, as Excel has it (#671): each item removes exactly its part
//! of the cells in the selected areas, and no cell moves.

use super::Area;
use super::areas::{RectIndex, entries_in};
use crate::sheet::{Cell, Sheet};

/// Excel's refusal of a clear that would split a merged cell.
pub(crate) const MERGED_PART: &str = "Cannot change part of a merged cell.";

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
    pub unmerge: Vec<Area>,
    /// Cells whose hyperlink goes, and the loaded `ref`s that go with them.
    pub unlink: Vec<(u32, u32)>,
    pub unlinked_refs: Vec<Area>,
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

/// The changes Home › Clear `what` makes over `areas` of `sheet`.
/// `note_cells` are the cells of the sheet that carry a note or a comment
/// (the package holds them). A range hyperlink touched anywhere goes as a
/// whole. Clear All and Clear Formats undo the merges inside the areas and
/// refuse ([`MERGED_PART`]) one that only partly is.
pub fn clear_plan(
    sheet: &Sheet,
    areas: &[Area],
    what: ClearWhat,
    note_cells: &[(u32, u32)],
) -> Result<ClearPlan, &'static str> {
    let mut plan = ClearPlan::default();
    // Cells, links, notes and merges are looked up in the areas through an
    // index, not tested against each area (#707 r6).
    let ix = RectIndex::new(areas);
    if matches!(what, ClearWhat::All | ClearWhat::Formats) {
        for &m in &sheet.merges {
            if !ix.meets(m) {
                continue;
            }
            if !ix.any(m, |a| a.0 <= m.0 && a.1 <= m.1 && m.2 <= a.2 && m.3 <= a.3) {
                return Err(MERGED_PART);
            }
            plan.unmerge.push(m);
        }
    }
    // The cells whose link goes, as a set for the lookups below.
    let mut gone = std::collections::BTreeSet::new();
    if what.unlinks() {
        // The loaded range links by the cells they spread over (a one-cell
        // link is found by its own key), built once: no per-link scan of
        // every link (#707 r6 M2).
        let mut spread: std::collections::HashMap<(u32, u32), Area> = Default::default();
        for &rect in sheet.hyperlink_refs.values() {
            if (rect.0, rect.1) == (rect.2, rect.3) {
                continue;
            }
            for (&(r, c), _) in sheet.hyperlinks.range((rect.0, rect.1)..=(rect.2, rect.3)) {
                if (rect.1..=rect.3).contains(&c) {
                    spread.insert((r, c), rect);
                }
            }
        }
        let mut refs = std::collections::BTreeSet::new();
        for ((r, c), _) in entries_in(&sheet.hyperlinks, areas, &ix) {
            gone.insert((r, c));
            // The loaded link this cell belongs to goes whole.
            if let Some(&rect) = spread.get(&(r, c)) {
                refs.insert(rect);
            } else if let Some(&rect) = sheet.hyperlink_refs.get(&(r, c)) {
                refs.insert(rect);
            }
        }
        for &rect in &refs {
            for (&(r, c), _) in sheet.hyperlinks.range((rect.0, rect.1)..=(rect.2, rect.3)) {
                if (rect.1..=rect.3).contains(&c) {
                    gone.insert((r, c));
                }
            }
        }
        plan.unlinked_refs = refs.into_iter().collect();
        plan.unlink = gone.iter().copied().collect();
    }
    if matches!(what, ClearWhat::All | ClearWhat::Comments) {
        plan.notes = note_cells
            .iter()
            .copied()
            .filter(|&(r, c)| ix.holds(r, c))
            .collect();
    }
    let mut seen = std::collections::HashSet::new();
    for ((r, c), cell) in entries_in(&sheet.cells, areas, &ix) {
        seen.insert((r, c));
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
            ClearWhat::RemoveHyperlinks if gone.contains(&(r, c)) => Cell {
                style: 0,
                ..cell.clone()
            },
            _ => continue,
        };
        if next != *cell {
            plan.cells.push((r, c, next));
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
    let unmerge: std::collections::HashSet<Area> = plan.unmerge.iter().copied().collect();
    sheet.merges.retain(|m| !unmerge.contains(m));
    for rc in &plan.unlink {
        sheet.hyperlinks.remove(rc);
    }
    let mut removed: std::collections::HashSet<Area> =
        sheet.hyperlinks_removed.iter().copied().collect();
    for &rect in &plan.unlinked_refs {
        sheet.hyperlink_refs.remove(&(rect.0, rect.1));
        if removed.insert(rect) {
            sheet.hyperlinks_removed.push(rect);
        }
    }
}

/// Home › Clear `what` over `areas` of `wb`'s sheet `sheet`, cells and all.
/// Notes are the host's (it removes [`ClearPlan::notes`] from its package).
#[cfg(test)]
pub fn apply_clear(
    wb: &mut crate::sheet::Workbook,
    sheet: usize,
    areas: &[Area],
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
