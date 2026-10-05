//! Home › Find & Select › Go To Special (#671): the cells of a kind, as the
//! rectangles of a multi-area selection.

use super::Area;
use super::paste_special::cells_to_rects;
use crate::formula::{collect_refs, parse, translate_formula};
use crate::sheet::{Cell, CellValue, Sheet, Workbook};

/// Excel's message when Go To Special finds nothing.
pub const NO_CELLS: &str = "No cells were found.";

/// A result too fragmented to select (R17).
pub const TOO_MANY_AREAS: &str = "Too many areas to select.";

/// The most rectangles a Go To Special result may hold.
pub const MAX_AREAS: usize = 10_000;

/// Which value types Constants and Formulas pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Types {
    pub numbers: bool,
    pub text: bool,
    pub logicals: bool,
    pub errors: bool,
}

impl Types {
    pub const ALL: Types = Types {
        numbers: true,
        text: true,
        logicals: true,
        errors: true,
    };

    fn takes(&self, v: &CellValue) -> bool {
        match v {
            CellValue::Number(_) => self.numbers,
            CellValue::Text(_) => self.text,
            CellValue::Bool(_) => self.logicals,
            CellValue::Error(_) => self.errors,
            CellValue::Empty => false,
        }
    }
}

impl Default for Types {
    fn default() -> Self {
        Types::ALL
    }
}

/// A Go To Special choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GoSpecial {
    Notes,
    Constants(Types),
    Formulas(Types),
    Blanks,
    CurrentRegion,
    CurrentArray,
    RowDifferences,
    ColumnDifferences,
    /// `all`: every level, else direct only.
    Precedents {
        all: bool,
    },
    Dependents {
        all: bool,
    },
    LastCell,
    VisibleCells,
    /// `same`: only the cells with the active cell's rules, else every one.
    ConditionalFormats {
        same: bool,
    },
    DataValidation {
        same: bool,
    },
}

impl GoSpecial {
    /// The kind a short name names (the harness's): `notes`, `constants`,
    /// `formulas`, `blanks`, `region`, `array`, `row-differences`,
    /// `column-differences`, `precedents`, `dependents`, `last-cell`,
    /// `visible`, `conditional-formats`, `validation`. The levels and the
    /// All/Same choice are separate.
    pub fn from_name(name: &str, types: Types, all: bool, same: bool) -> Option<GoSpecial> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "notes" | "comments" => GoSpecial::Notes,
            "constants" => GoSpecial::Constants(types),
            "formulas" => GoSpecial::Formulas(types),
            "blanks" => GoSpecial::Blanks,
            "region" | "current-region" => GoSpecial::CurrentRegion,
            "array" | "current-array" => GoSpecial::CurrentArray,
            "row-differences" => GoSpecial::RowDifferences,
            "column-differences" => GoSpecial::ColumnDifferences,
            "precedents" => GoSpecial::Precedents { all },
            "dependents" => GoSpecial::Dependents { all },
            "last-cell" => GoSpecial::LastCell,
            "visible" | "visible-cells" => GoSpecial::VisibleCells,
            "conditional-formats" => GoSpecial::ConditionalFormats { same },
            "validation" | "data-validation" => GoSpecial::DataValidation { same },
            _ => return None,
        })
    }
}

/// The cells Go To Special searches: the selection when it is more than one
/// cell, otherwise the used range.
pub fn special_scope(sheet: &Sheet, areas: &[Area]) -> Vec<Area> {
    let single = areas.len() == 1 && {
        let a = areas[0];
        a.0 == a.2 && a.1 == a.3
    };
    if !single && !areas.is_empty() {
        return areas.to_vec();
    }
    used_rect(sheet).into_iter().collect()
}

fn used_rect(sheet: &Sheet) -> Option<Area> {
    let (rows, cols) = sheet.used_size();
    (rows > 0 && cols > 0).then(|| (0, 0, rows - 1, cols - 1))
}

fn inside(r: u32, c: u32, a: Area) -> bool {
    (a.0..=a.2).contains(&r) && (a.1..=a.3).contains(&c)
}

fn clip(a: Area, b: Area) -> Option<Area> {
    let r = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (r.0 <= r.2 && r.1 <= r.3).then_some(r)
}

/// The existing cells of `scope`, each once.
fn cells_in<'a>(sheet: &'a Sheet, scope: &'a [Area]) -> Vec<((u32, u32), &'a Cell)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for &(r0, c0, r1, c1) in scope {
        for (&(r, c), cell) in sheet.cells.range((r0, c0)..=(r1, c1)) {
            if (c0..=c1).contains(&c) && seen.insert((r, c)) {
                out.push(((r, c), cell));
            }
        }
    }
    out
}

/// The references a formula on `sheet` makes to cells of that same sheet,
/// as rectangles.
fn same_sheet_refs(wb: &Workbook, sheet: usize, f: &str) -> Vec<Area> {
    let Ok(e) = parse(f) else {
        return Vec::new();
    };
    let mut refs = Vec::new();
    collect_refs(&e, &mut refs);
    let name = &wb.sheets[sheet].name;
    refs.into_iter()
        .filter(|(q, ..)| q.as_deref().is_none_or(|q| q.eq_ignore_ascii_case(name)))
        .map(|(_, r0, c0, r1, c1)| (r0, c0, r1, c1))
        .collect()
}

/// The rectangle of the current region around `(r, c)`: grown while any
/// cell touching it, diagonals included, holds something.
pub fn current_region(sheet: &Sheet, (r, c): (u32, u32)) -> Area {
    // Subtotal's own region, which grows rows to a fixpoint before it scans
    // columns (#707 r4 M3), diagonals included.
    super::subtotal_region(sheet, r, c).map_or((r, c, r, c), |(area, _)| area)
}

/// The union of `rects` as non-overlapping rectangles, in sheet order,
/// without walking their cells: over the grid their edges cut the sheet
/// into, which has at most (2n)² pieces for n rectangles (#707 r4 M2).
pub(crate) fn union_rects(rects: &[Area]) -> Vec<Area> {
    let mut rows: Vec<u64> = rects
        .iter()
        .flat_map(|r| [u64::from(r.0), u64::from(r.2) + 1])
        .collect();
    let mut cols: Vec<u64> = rects
        .iter()
        .flat_map(|r| [u64::from(r.1), u64::from(r.3) + 1])
        .collect();
    rows.sort_unstable();
    rows.dedup();
    cols.sort_unstable();
    cols.dedup();
    let at = |v: &[u64], x: u64| v.binary_search(&x).unwrap_or(0) as u32;
    let mut pieces = std::collections::BTreeSet::new();
    for r in rects {
        for ri in at(&rows, u64::from(r.0))..at(&rows, u64::from(r.2) + 1) {
            for ci in at(&cols, u64::from(r.1))..at(&cols, u64::from(r.3) + 1) {
                pieces.insert((ri, ci));
            }
        }
    }
    let pieces: Vec<(u32, u32)> = pieces.into_iter().collect();
    let mut out: Vec<Area> = cells_to_rects(&pieces)
        .into_iter()
        .map(|(r0, c0, r1, c1)| {
            (
                rows[r0 as usize] as u32,
                cols[c0 as usize] as u32,
                (rows[r1 as usize + 1] - 1) as u32,
                (cols[c1 as usize + 1] - 1) as u32,
            )
        })
        .collect();
    out.sort_unstable();
    out
}

/// Go To Special `kind` on `sheet` of `wb` for the selection `areas`, with
/// `active` the active cell. Most kinds search [`special_scope`] (the
/// selection, or the used range when one cell is selected); Precedents and
/// Dependents start from the selected cells themselves: the cells found, as rectangles in sheet
/// order (the first holds the first cell found). `note_cells` are the cells
/// the package holds notes or comments on. [`NO_CELLS`] when nothing is
/// found, [`TOO_MANY_AREAS`] past [`MAX_AREAS`] rectangles.
pub fn go_to_special(
    wb: &Workbook,
    sheet: usize,
    areas: &[Area],
    active: (u32, u32),
    kind: GoSpecial,
    note_cells: &[(u32, u32)],
) -> Result<Vec<Area>, &'static str> {
    let s = wb.sheets.get(sheet).ok_or(NO_CELLS)?;
    let scope_v = special_scope(s, areas);
    let scope = scope_v.as_slice();
    // Every cell-by-cell walk stays inside the used range: a whole-sheet
    // selection must not walk 17 billion cells (#707 r3 M3).
    let used = used_rect(s);
    let clip_used = |a: Area| used.and_then(|u| clip(a, u));
    let scope_used: Vec<Area> = scope.iter().filter_map(|&a| clip_used(a)).collect();
    let in_scope = |r: u32, c: u32| scope.iter().any(|&a| inside(r, c, a));
    let rects: Vec<Area> = match kind {
        GoSpecial::Notes => cells_to_rects(
            &note_cells
                .iter()
                .copied()
                .filter(|&(r, c)| in_scope(r, c))
                .collect::<Vec<_>>(),
        ),
        GoSpecial::Constants(t) => cells_to_rects(
            &cells_in(s, scope)
                .into_iter()
                .filter(|(_, cell)| cell.formula.is_none() && t.takes(&cell.value))
                .map(|(rc, _)| rc)
                .collect::<Vec<_>>(),
        ),
        GoSpecial::Formulas(t) => cells_to_rects(
            &cells_in(s, scope)
                .into_iter()
                .filter(|(_, cell)| cell.formula.is_some() && t.takes(&cell.value))
                .map(|(rc, _)| rc)
                .collect::<Vec<_>>(),
        ),
        GoSpecial::Blanks => {
            let Some(used) = used_rect(s) else {
                return Err(NO_CELLS);
            };
            let mut cells = Vec::new();
            for a in scope.iter().filter_map(|&a| clip(a, used)) {
                for r in a.0..=a.2 {
                    for c in a.1..=a.3 {
                        let blank = s.cell(r, c).is_none_or(Cell::is_blank);
                        if blank {
                            cells.push((r, c));
                        }
                    }
                }
            }
            cells_to_rects(&cells)
        }
        GoSpecial::CurrentRegion => vec![current_region(s, active)],
        GoSpecial::CurrentArray => s
            .cells
            .iter()
            .find_map(|(&(r, c), cell)| {
                let (h, w) = cell.spill?;
                let rect = (r, c, r + h - 1, c + w - 1);
                inside(active.0, active.1, rect).then_some(rect)
            })
            .into_iter()
            .collect(),
        GoSpecial::RowDifferences | GoSpecial::ColumnDifferences => {
            let rows = kind == GoSpecial::RowDifferences;
            // A cell's content as seen from the comparison cell: a formula
            // translated there, so a filled-down formula reads the same.
            let sig = |r: u32, c: u32, to: (u32, u32)| -> (Option<String>, CellValue) {
                let cell = s.cell(r, c);
                match cell.and_then(|x| x.formula.as_deref()) {
                    Some(f) => (
                        Some(
                            translate_formula(
                                f,
                                i64::from(to.0) - i64::from(r),
                                i64::from(to.1) - i64::from(c),
                            )
                            .unwrap_or_else(|| f.to_string()),
                        ),
                        CellValue::Empty,
                    ),
                    None => (None, cell.map(|x| x.value.clone()).unwrap_or_default()),
                }
            };
            let mut cells = Vec::new();
            for &a in &scope_used {
                for r in a.0..=a.2 {
                    for c in a.1..=a.3 {
                        let cmp = if rows { (r, active.1) } else { (active.0, c) };
                        if (r, c) == cmp {
                            continue;
                        }
                        if sig(r, c, cmp) != sig(cmp.0, cmp.1, cmp) {
                            cells.push((r, c));
                        }
                    }
                }
            }
            cells_to_rects(&cells)
        }
        GoSpecial::Precedents { all } => {
            // From the formulas of the selection's cells (the active cell
            // when one cell is selected): each range they read, whole, as a
            // rectangle (#707 r4 M2). All levels follows on through the
            // cells of those ranges that hold formulas; an empty cell has
            // none, so only the used range is looked at for them.
            let mut frontier: Vec<(u32, u32)> =
                cells_in(s, areas).into_iter().map(|(rc, _)| rc).collect();
            let mut found: Vec<Area> = Vec::new();
            let mut visited = std::collections::BTreeSet::new();
            while let Some((r, c)) = frontier.pop() {
                if !visited.insert((r, c)) {
                    continue;
                }
                let Some(f) = s.cell(r, c).and_then(|x| x.formula.as_deref()) else {
                    continue;
                };
                for rect in same_sheet_refs(wb, sheet, f) {
                    found.push(rect);
                    if !all {
                        continue;
                    }
                    if let Some((r0, c0, r1, c1)) = clip_used(rect) {
                        for (&(rr, cc), cell) in s.cells.range((r0, c0)..=(r1, c1)) {
                            if (c0..=c1).contains(&cc) && cell.formula.is_some() {
                                frontier.push((rr, cc));
                            }
                        }
                    }
                }
            }
            union_rects(&found)
        }
        GoSpecial::Dependents { all } => {
            let formulas: Vec<((u32, u32), Vec<Area>)> = s
                .cells
                .iter()
                .filter_map(|(&rc, cell)| {
                    cell.formula
                        .as_deref()
                        .map(|f| (rc, same_sheet_refs(wb, sheet, f)))
                })
                .collect();
            let mut targets: Vec<Area> = areas.to_vec();
            let mut found = std::collections::BTreeSet::new();
            loop {
                let mut next = Vec::new();
                for (rc, refs) in &formulas {
                    if found.contains(rc) {
                        continue;
                    }
                    let hits = refs.iter().any(|&a| {
                        targets
                            .iter()
                            .any(|&t| a.0 <= t.2 && t.0 <= a.2 && a.1 <= t.3 && t.1 <= a.3)
                    });
                    if hits {
                        found.insert(*rc);
                        next.push((rc.0, rc.1, rc.0, rc.1));
                    }
                }
                if !all || next.is_empty() {
                    break;
                }
                targets = next;
            }
            cells_to_rects(&found.into_iter().collect::<Vec<_>>())
        }
        GoSpecial::LastCell => used_rect(s)
            .map(|(_, _, r, c)| (r, c, r, c))
            .into_iter()
            .collect(),
        GoSpecial::VisibleCells => {
            // By rows and columns, never cells: cheap over any scope, so
            // the selection's own extent is kept (#707 r4 M2).
            let mut rects = Vec::new();
            for &a in scope {
                let mut row_runs: Vec<(u32, u32)> = Vec::new();
                for r in a.0..=a.2 {
                    if s.row_hidden(r) {
                        continue;
                    }
                    match row_runs.last_mut() {
                        Some(run) if run.1 + 1 == r => run.1 = r,
                        _ => row_runs.push((r, r)),
                    }
                }
                let mut col_runs: Vec<(u32, u32)> = Vec::new();
                for c in a.1..=a.3 {
                    if s.col_hidden(c) {
                        continue;
                    }
                    match col_runs.last_mut() {
                        Some(run) if run.1 + 1 == c => run.1 = c,
                        _ => col_runs.push((c, c)),
                    }
                }
                for &(r0, r1) in &row_runs {
                    for &(c0, c1) in &col_runs {
                        rects.push((r0, c0, r1, c1));
                    }
                }
            }
            rects.sort_unstable();
            rects
        }
        GoSpecial::ConditionalFormats { same } => rule_ranges(
            s.cond_formats.iter().map(|cf| &cf.ranges),
            same,
            active,
            areas,
        ),
        GoSpecial::DataValidation { same } => rule_ranges(
            s.validations.iter().map(|dv| &dv.ranges),
            same,
            active,
            areas,
        ),
    };
    if rects.is_empty() {
        return Err(NO_CELLS);
    }
    if rects.len() > MAX_AREAS {
        return Err(TOO_MANY_AREAS);
    }
    Ok(rects)
}

/// The ranges of every rule (or, `same`, of the rules covering `active`),
/// within the selection `areas` when it is more than one cell (the whole
/// sheet otherwise), as rectangles: a rule's ranges are never walked cell
/// by cell, so a rule over whole columns is as cheap as one cell (#707 r4
/// M2).
fn rule_ranges<'a>(
    rules: impl Iterator<Item = &'a Vec<Area>>,
    same: bool,
    active: (u32, u32),
    areas: &[Area],
) -> Vec<Area> {
    let whole = (0, 0, crate::sheet::MAX_ROWS - 1, crate::sheet::MAX_COLS - 1);
    let single = areas.len() == 1 && areas[0].0 == areas[0].2 && areas[0].1 == areas[0].3;
    let scope: Vec<Area> = if single || areas.is_empty() {
        vec![whole]
    } else {
        areas.to_vec()
    };
    let mut parts = Vec::new();
    for ranges in rules {
        if same && !ranges.iter().any(|&a| inside(active.0, active.1, a)) {
            continue;
        }
        for &a in ranges {
            parts.extend(scope.iter().filter_map(|&s| clip(a, s)));
        }
    }
    union_rects(&parts)
}

/// Where Go To's Reference box (or a chosen name) goes: a cell or range,
/// optionally `Sheet!`-qualified (`$` signs allowed), or a defined name
/// (scoped to `active` first) whose definition is one. `(sheet, rect)`;
/// `None` when the text names nothing there.
pub fn resolve_reference(wb: &Workbook, active: usize, text: &str) -> Option<(usize, Area)> {
    let text = text.trim().trim_start_matches('=');
    if text.is_empty() {
        return None;
    }
    let named = wb
        .defined_names
        .iter()
        .filter(|d| d.name.eq_ignore_ascii_case(text))
        .min_by_key(|d| d.scope != Some(active))
        .map(|d| d.formula.as_str());
    let target = named.unwrap_or(text);
    let (sheet, cells) = match target.rsplit_once('!') {
        Some((name, cells)) => {
            let name = name.trim().trim_matches('\'');
            (wb.sheet_index(&name.replace("''", "'"))?, cells)
        }
        None => (active, target),
    };
    let cells = cells.replace('$', "");
    let rect = crate::sheet::parse_range_name(&cells)
        .or_else(|| crate::sheet::parse_cell_name(&cells).map(|(r, c)| (r, c, r, c)))?;
    (sheet < wb.sheets.len()).then_some((sheet, rect))
}

#[cfg(test)]
#[path = "goto_special/tests.rs"]
mod tests;
