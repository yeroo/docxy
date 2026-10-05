//! Advanced Filter: a list filtered by a criteria range (the D-functions'
//! rules), in place or copied to another location, optionally unique.

use std::collections::HashSet;

use super::apply::{
    Area, FILTER_DB, FilterError, FilterOutcome, auto_filter_off, set_name, shown_text,
};
use crate::sheet::{Cell, Workbook};

/// Excel's refusal when the copy-to range is on another sheet.
pub const ADVANCED_OTHER_SHEET: &str = "You can only copy filtered data to the active sheet.";

/// An Advanced Filter's settings. `list` is on the sheet the filter runs
/// on, its first row the headers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AdvancedFilter {
    pub list: Area,
    /// The criteria range (headers, then rows of conditions) and its sheet;
    /// `None` keeps every record.
    pub criteria: Option<(usize, Area)>,
    /// Copy to another location: its sheet and range. One cell gets every
    /// column of the list; a row of headers gets only those columns, in that
    /// order. `None` filters in place.
    pub copy_to: Option<(usize, Area)>,
    /// Unique records only.
    pub unique: bool,
}

/// Run an Advanced Filter on `sheet` (the active sheet). In place, the
/// records that fail are hidden as filtered (an AutoFilter on the sheet is
/// turned off first, as in Excel); copied, the matching records are written
/// below the copy-to headers (values and styles), after the old extract rows
/// in those columns are cleared. Defines the sheet's `_FilterDatabase`,
/// `Criteria` (when a criteria range is given; otherwise the last one's
/// stays) and (copying) `Extract` names. Copying to another sheet is
/// refused ([`ADVANCED_OTHER_SHEET`]) with nothing changed.
pub fn advanced(
    wb: &mut Workbook,
    sheet: usize,
    a: &AdvancedFilter,
) -> Result<FilterOutcome, FilterError> {
    let (r1, c1, r2, c2) = a.list;
    if wb.sheets.get(sheet).is_none() || r1 > r2 || c1 > c2 {
        return Err(FilterError::NoData);
    }
    if a.copy_to.is_some_and(|(s, _)| s != sheet) {
        return Err(FilterError::OtherSheet);
    }
    // The list's columns the copy writes, in order.
    let cols: Vec<u32> = match a.copy_to {
        Some((_, (dr, dc, dr2, dc2))) if (dr, dc) != (dr2, dc2) => {
            let mut cols = Vec::new();
            for c in dc..=dc2 {
                let want = shown_text(wb, sheet, dr, c);
                let hit = (c1..=c2).find(|&lc| {
                    !want.trim().is_empty()
                        && shown_text(wb, sheet, r1, lc)
                            .trim()
                            .eq_ignore_ascii_case(want.trim())
                });
                cols.push(hit.ok_or(FilterError::BadExtract)?);
            }
            cols
        }
        _ => (c1..=c2).collect(),
    };
    let matched = match a.criteria {
        Some((cs, crit)) => crate::engine::criteria_matches(wb, sheet, a.list, cs, crit),
        None => vec![true; (r2 - r1) as usize],
    };
    let mut keep: Vec<u32> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (i, ok) in matched.iter().enumerate() {
        let r = r1 + 1 + i as u32;
        if !ok {
            continue;
        }
        if a.unique {
            let key: Vec<String> = cols
                .iter()
                .map(|&c| shown_text(wb, sheet, r, c).to_lowercase())
                .collect();
            if !seen.insert(key.join("\u{1f}")) {
                continue;
            }
        }
        keep.push(r);
    }
    let total = matched.len();
    set_name(wb, sheet, FILTER_DB, sheet, a.list);
    // A run without a criteria range leaves the last one's name alone.
    if let Some((cs, crit)) = a.criteria {
        set_name(wb, sheet, "_xlnm.Criteria", cs, crit);
    }
    let Some((_, (dr, dc, _, _))) = a.copy_to else {
        // In place.
        if wb.sheets[sheet].auto_filter.is_some() {
            auto_filter_off(wb, sheet);
            set_name(wb, sheet, FILTER_DB, sheet, a.list);
        }
        let keep: HashSet<u32> = keep.into_iter().collect();
        let s = &mut wb.sheets[sheet];
        for r in r1 + 1..=r2 {
            let show = keep.contains(&r);
            if !show || s.row_hidden(r) || s.filtered_rows.contains(&r) {
                s.set_row_filtered(r, !show);
            }
        }
        s.filter_mode = Some(keep.len() < total);
        return Ok(FilterOutcome {
            shown: keep.len(),
            total,
        });
    };
    // Copy: clear the old extract rows below the headers, then write.
    let width = cols.len() as u32;
    let rows: Vec<Vec<Option<Cell>>> = std::iter::once(r1)
        .chain(keep.iter().copied())
        .map(|r| {
            cols.iter()
                .map(|&c| {
                    // The value a formula shows, not the formula.
                    wb.sheets[sheet].cell(r, c).map(|cl| Cell {
                        value: cl.value.clone(),
                        style: cl.style,
                        ..Cell::default()
                    })
                })
                .collect()
        })
        .collect();
    let s = &mut wb.sheets[sheet];
    let bottom = s.used_size().0;
    let stale: Vec<(u32, u32)> = s
        .cells
        .keys()
        .filter(|&&(r, c)| r > dr && r < bottom.max(dr + 1) && c >= dc && c < dc + width)
        .copied()
        .collect();
    for at in stale {
        s.cells.remove(&at);
    }
    let header_given = a
        .copy_to
        .is_some_and(|(_, (r, c, r2, c2))| (r, c) != (r2, c2));
    for (i, row) in rows.into_iter().enumerate() {
        // A copy-to header row the user typed stays as it is.
        if i == 0 && header_given {
            continue;
        }
        for (j, cell) in row.into_iter().enumerate() {
            let at = (dr + i as u32, dc + j as u32);
            match cell {
                Some(c) => s.set_cell(at.0, at.1, c),
                None => {
                    s.cells.remove(&at);
                }
            }
        }
    }
    set_name(
        wb,
        sheet,
        "_xlnm.Extract",
        sheet,
        (dr, dc, dr, dc + width - 1),
    );
    Ok(FilterOutcome {
        shown: keep.len(),
        total,
    })
}
