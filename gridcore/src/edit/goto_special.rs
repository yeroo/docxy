//! Home › Find & Select › Go To Special (#671): the cells of a kind, as the
//! rectangles of a multi-area selection.

use super::Area;
use super::areas::{RectIndex, entries_in};
use super::paste_special::cells_to_rects;
use crate::formula::{collect_refs, parse, translate_formula};
use crate::sheet::{Cell, CellValue, Sheet, Workbook};

/// Excel's message when Go To Special finds nothing.
pub const NO_CELLS: &str = "No cells were found.";

/// A result too fragmented to select (R17).
pub(crate) const TOO_MANY_AREAS: &str = "Too many areas to select.";

/// The most rectangles a Go To Special result may hold.
pub(crate) const MAX_AREAS: usize = 10_000;

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
pub(crate) fn special_scope(sheet: &Sheet, areas: &[Area]) -> Vec<Area> {
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

/// The existing cells of `scope`, each once, in sheet order (#707 r6:
/// through an index of the areas, not a walk per area).
fn cells_in<'a>(sheet: &'a Sheet, scope: &[Area]) -> Vec<((u32, u32), &'a Cell)> {
    entries_in(&sheet.cells, scope, &RectIndex::new(scope))
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
pub(crate) fn current_region(sheet: &Sheet, (r, c): (u32, u32)) -> Area {
    // Subtotal's own region, which grows rows to a fixpoint before it scans
    // columns (#707 r4 M3), diagonals included.
    super::subtotal_region(sheet, r, c).map_or((r, c, r, c), |(area, _)| area)
}

/// The union of `rects` as non-overlapping rectangles in sheet order,
/// without walking their cells (#707 r4 M2, r5 M1). Rectangles with the
/// same columns merge as row intervals first, and one inside a recent
/// larger one is dropped, which takes nested families (a running total's
/// `$A$2:A2` … `$A$2:A50001`) down to their largest. A sweep over the row
/// bands the rest cut then keeps each band's merged column runs, extending
/// a run that the band above had too. It costs the sum over bands of the
/// rectangles active in them, and stops past [`MAX_AREAS`] output
/// rectangles (the caller then refuses with [`TOO_MANY_AREAS`]).
pub(crate) fn union_rects(rects: &[Area]) -> Vec<Area> {
    use std::collections::{BTreeMap, HashMap};
    // Same columns: merge the row intervals.
    let mut by_cols: BTreeMap<(u32, u32), Vec<(u32, u32)>> = BTreeMap::new();
    for r in rects {
        by_cols.entry((r.1, r.3)).or_default().push((r.0, r.2));
    }
    let mut merged: Vec<Area> = Vec::new();
    for ((c0, c1), mut rows) in by_cols {
        rows.sort_unstable();
        let mut cur: Option<(u32, u32)> = None;
        for (a, b) in rows {
            match cur.as_mut() {
                Some(run) if u64::from(a) <= u64::from(run.1) + 1 => run.1 = run.1.max(b),
                _ => {
                    if let Some((x, y)) = cur.take() {
                        merged.push((x, c0, y, c1));
                    }
                    cur = Some((a, b));
                }
            }
        }
        if let Some((x, y)) = cur {
            merged.push((x, c0, y, c1));
        }
    }
    // Drop one inside a recent larger one (a bounded look-back: cheap, and
    // the sweep stays exact whatever it misses).
    let size = |r: &Area| u64::from(r.2 - r.0 + 1) * u64::from(r.3 - r.1 + 1);
    merged.sort_unstable_by_key(|r| std::cmp::Reverse(size(r)));
    let mut kept: Vec<Area> = Vec::new();
    for r in merged {
        let inside = kept
            .iter()
            .rev()
            .take(64)
            .any(|k| k.0 <= r.0 && k.1 <= r.1 && r.2 <= k.2 && r.3 <= k.3);
        if !inside {
            kept.push(r);
        }
    }
    // The sweep over row bands.
    let mut edges: Vec<u64> = kept
        .iter()
        .flat_map(|r| [u64::from(r.0), u64::from(r.2) + 1])
        .collect();
    edges.sort_unstable();
    edges.dedup();
    let mut starts: Vec<usize> = (0..kept.len()).collect();
    starts.sort_unstable_by_key(|&i| kept[i].0);
    let mut next_start = 0;
    let mut active: Vec<usize> = Vec::new();
    let mut out: Vec<Area> = Vec::new();
    // Runs open from the band above: (c0, c1) -> index into `out`.
    let mut open: HashMap<(u32, u32), usize> = HashMap::new();
    for w in edges.windows(2) {
        let (top, end) = (w[0], w[1]);
        active.retain(|&i| u64::from(kept[i].2) + 1 > top);
        while next_start < starts.len() && u64::from(kept[starts[next_start]].0) == top {
            active.push(starts[next_start]);
            next_start += 1;
        }
        let mut spans: Vec<(u32, u32)> = active.iter().map(|&i| (kept[i].1, kept[i].3)).collect();
        spans.sort_unstable();
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for (a, b) in spans {
            match runs.last_mut() {
                Some(run) if u64::from(a) <= u64::from(run.1) + 1 => run.1 = run.1.max(b),
                _ => runs.push((a, b)),
            }
        }
        let mut now: HashMap<(u32, u32), usize> = HashMap::new();
        for run in runs {
            let idx = match open.get(&run) {
                Some(&k) if u64::from(out[k].2) + 1 == top => {
                    out[k].2 = (end - 1) as u32;
                    k
                }
                _ => {
                    out.push((top as u32, run.0, (end - 1) as u32, run.1));
                    if out.len() > MAX_AREAS {
                        return out;
                    }
                    out.len() - 1
                }
            };
            now.insert(run, idx);
        }
        open = now;
    }
    out.sort_unstable();
    out
}

/// Intervals `[lo, hi]` along one line of cells (a column's rows, or a
/// row's columns), each naming a formula, found by the point they contain
/// and taken out as they are found: every interval is returned at most once
/// over all queries, and a query costs O((k + 1) log n) for k returned
/// (#707 r6 M1). A max-tree of `hi` over the intervals sorted by `lo`.
struct Stabber {
    lo: Vec<u32>,
    ids: Vec<usize>,
    /// The tree of `hi + 1` (0 once taken), leaves from `size`.
    max: Vec<u64>,
    size: usize,
}

impl Stabber {
    fn new(mut items: Vec<(u32, u32, usize)>) -> Stabber {
        items.sort_unstable();
        let size = items.len().next_power_of_two().max(1);
        let mut max = vec![0u64; 2 * size];
        for (k, &(_, hi, _)) in items.iter().enumerate() {
            max[size + k] = u64::from(hi) + 1;
        }
        for k in (1..size).rev() {
            max[k] = max[2 * k].max(max[2 * k + 1]);
        }
        Stabber {
            lo: items.iter().map(|e| e.0).collect(),
            ids: items.iter().map(|e| e.2).collect(),
            max,
            size,
        }
    }

    /// Take out every interval that contains `x`, adding its formula to `out`.
    fn take(&mut self, x: u32, out: &mut Vec<usize>) {
        let upto = self.lo.partition_point(|&lo| lo <= x);
        if upto > 0 {
            self.descend(1, 0, self.size, upto, u64::from(x) + 1, out);
        }
    }

    fn descend(
        &mut self,
        node: usize,
        from: usize,
        to: usize,
        upto: usize,
        need: u64,
        out: &mut Vec<usize>,
    ) {
        if from >= upto || self.max[node] < need {
            return;
        }
        if to - from == 1 {
            out.push(self.ids[from]);
            self.max[node] = 0;
        } else {
            let mid = (from + to) / 2;
            self.descend(2 * node, from, mid, upto, need, out);
            self.descend(2 * node + 1, mid, to, upto, need, out);
            self.max[node] = self.max[2 * node].max(self.max[2 * node + 1]);
        }
    }
}

/// Where each formula on a sheet reads, indexed by what it reads, so "which
/// formulas read cell (r, c)" is answered without scanning them all, and
/// each reference is examined once over a whole walk (#707 r5 M2, r6 M1):
/// single cells by cell, a range along its shorter side (per column for a
/// tall one, per row for a wide one), and a range huge both ways per block
/// of [`BAND`] columns it covers whole, its ragged edge columns per column.
struct ReadIndex {
    cells: Vec<(u32, u32)>,
    refs: Vec<Vec<Area>>,
    single: std::collections::HashMap<(u32, u32), Vec<usize>>,
    by_col: std::collections::HashMap<u32, Stabber>,
    by_row: std::collections::HashMap<u32, Stabber>,
    by_band: std::collections::HashMap<u32, Stabber>,
}

/// The columns a [`ReadIndex`] band spans.
const BAND: u32 = 256;

impl ReadIndex {
    fn new(wb: &Workbook, sheet: usize, s: &Sheet) -> ReadIndex {
        use std::collections::HashMap;
        let mut cells = Vec::new();
        let mut refs_all = Vec::new();
        let mut single: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        let mut cols: HashMap<u32, Vec<(u32, u32, usize)>> = HashMap::new();
        let mut rows: HashMap<u32, Vec<(u32, u32, usize)>> = HashMap::new();
        let mut bands: HashMap<u32, Vec<(u32, u32, usize)>> = HashMap::new();
        for (&rc, cell) in &s.cells {
            let Some(f) = cell.formula.as_deref() else {
                continue;
            };
            let refs = same_sheet_refs(wb, sheet, f);
            let i = cells.len();
            for &a in &refs {
                let (h, w) = (a.2 - a.0, a.3 - a.1);
                if h == 0 && w == 0 {
                    single.entry((a.0, a.1)).or_default().push(i);
                } else if w <= h && w < 256 {
                    for c in a.1..=a.3 {
                        cols.entry(c).or_default().push((a.0, a.2, i));
                    }
                } else if h < 256 {
                    for r in a.0..=a.2 {
                        rows.entry(r).or_default().push((a.1, a.3, i));
                    }
                } else {
                    // Whole bands, then the edge columns either side.
                    let first = a.1.div_ceil(BAND);
                    let last = (a.3 + 1) / BAND;
                    for band in first..last {
                        bands.entry(band).or_default().push((a.0, a.2, i));
                    }
                    let edges: Vec<u32> = if first >= last {
                        (a.1..=a.3).collect()
                    } else {
                        (a.1..first * BAND).chain(last * BAND..=a.3).collect()
                    };
                    for c in edges {
                        cols.entry(c).or_default().push((a.0, a.2, i));
                    }
                }
            }
            cells.push(rc);
            refs_all.push(refs);
        }
        ReadIndex {
            cells,
            refs: refs_all,
            single,
            by_col: cols
                .into_iter()
                .map(|(k, v)| (k, Stabber::new(v)))
                .collect(),
            by_row: rows
                .into_iter()
                .map(|(k, v)| (k, Stabber::new(v)))
                .collect(),
            by_band: bands
                .into_iter()
                .map(|(k, v)| (k, Stabber::new(v)))
                .collect(),
        }
    }

    /// Take out the references to cell (r, c), adding the formulas that make
    /// them to `out` (a formula may come more than once; the caller keeps
    /// the ones it has found).
    fn take_readers(&mut self, (r, c): (u32, u32), out: &mut Vec<usize>) {
        if let Some(v) = self.single.remove(&(r, c)) {
            out.extend(v);
        }
        if let Some(t) = self.by_col.get_mut(&c) {
            t.take(r, out);
        }
        if let Some(t) = self.by_row.get_mut(&r) {
            t.take(c, out);
        }
        if let Some(t) = self.by_band.get_mut(&(c / BAND)) {
            t.take(r, out);
        }
    }
}

/// Go To Special `kind` on `sheet` of `wb` for the selection `areas`, with
/// `active` the active cell: the cells found, as rectangles in sheet order
/// (the first holds the first cell found). Most kinds search
/// [`special_scope`] (the selection, or the used range when one cell is
/// selected). The exceptions: Precedents and Dependents start from the
/// selected cells themselves; Visible cells covers the selection as it is;
/// Conditional formats and Data validation take each rule's ranges within
/// the selection, or the whole sheet when one cell is selected. `note_cells` are the cells
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
    let scope_ix = RectIndex::new(scope);
    let in_scope = |r: u32, c: u32| scope_ix.holds(r, c);
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
            // cells of those ranges that hold formulas, each visited once,
            // found per column by range (#707 r5 M1).
            let mut frontier: Vec<(u32, u32)> =
                cells_in(s, areas).into_iter().map(|(rc, _)| rc).collect();
            // The formula cells not yet followed, by column.
            let mut unvisited: std::collections::BTreeMap<u32, std::collections::BTreeSet<u32>> =
                Default::default();
            if all {
                for (&(r, c), cell) in &s.cells {
                    if cell.formula.is_some() {
                        unvisited.entry(c).or_default().insert(r);
                    }
                }
                for &(r, c) in &frontier {
                    if let Some(rows) = unvisited.get_mut(&c) {
                        rows.remove(&r);
                    }
                }
            }
            let mut found: Vec<Area> = Vec::new();
            while let Some((r, c)) = frontier.pop() {
                let Some(f) = s.cell(r, c).and_then(|x| x.formula.as_deref()) else {
                    continue;
                };
                for rect in same_sheet_refs(wb, sheet, f) {
                    found.push(rect);
                    if !all {
                        continue;
                    }
                    let (r0, c0, r1, c1) = rect;
                    for (&col, rows) in unvisited.range_mut(c0..=c1) {
                        let hit: Vec<u32> = rows.range(r0..=r1).copied().collect();
                        for row in hit {
                            rows.remove(&row);
                            frontier.push((row, col));
                        }
                    }
                }
            }
            union_rects(&found)
        }
        GoSpecial::Dependents { all } => {
            // The formulas that read the selection, then (All levels) those
            // that read them, each found once through an index of what every
            // formula reads (#707 r5 M2).
            let mut ix = ReadIndex::new(wb, sheet, s);
            let mut found = vec![false; ix.cells.len()];
            let mut queue: Vec<usize> = Vec::new();
            let areas_ix = RectIndex::new(areas);
            for (i, refs) in ix.refs.iter().enumerate() {
                let hits = refs.iter().any(|&a| areas_ix.meets(a));
                if hits {
                    found[i] = true;
                    queue.push(i);
                }
            }
            if all {
                let mut readers = Vec::new();
                while let Some(i) = queue.pop() {
                    readers.clear();
                    let cell = ix.cells[i];
                    ix.take_readers(cell, &mut readers);
                    for &j in &readers {
                        if !found[j] {
                            found[j] = true;
                            queue.push(j);
                        }
                    }
                }
            }
            let cells: Vec<(u32, u32)> = ix
                .cells
                .iter()
                .zip(&found)
                .filter(|(_, f)| **f)
                .map(|(&rc, _)| rc)
                .collect();
            cells_to_rects(&cells)
        }
        GoSpecial::LastCell => used_rect(s)
            .map(|(_, _, r, c)| (r, c, r, c))
            .into_iter()
            .collect(),
        GoSpecial::VisibleCells => {
            // By rows and columns, never cells: cheap over any scope, so
            // the selection's own extent is kept (#707 r4 M2). The hidden
            // rows and columns are gathered once, and each area cut by the
            // runs of them it holds, not walked row by row (#707 r6).
            let hidden_rows = runs(s.row_attrs.keys().copied().filter(|&r| s.row_hidden(r)));
            let hidden_cols = runs((0..crate::sheet::MAX_COLS).filter(|&c| s.col_hidden(c)));
            let mut rects = Vec::new();
            for &a in scope {
                let row_runs = visible_runs(a.0, a.2, &hidden_rows);
                let col_runs = visible_runs(a.1, a.3, &hidden_cols);
                for &(r0, r1) in &row_runs {
                    for &(c0, c1) in &col_runs {
                        rects.push((r0, c0, r1, c1));
                    }
                    if rects.len() > MAX_AREAS {
                        return Err(TOO_MANY_AREAS);
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

/// Sorted numbers as runs of consecutive ones.
fn runs(sorted: impl Iterator<Item = u32>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for x in sorted {
        match out.last_mut() {
            Some(run) if u64::from(run.1) + 1 == u64::from(x) => run.1 = x,
            _ => out.push((x, x)),
        }
    }
    out
}

/// The runs of `lo..=hi` outside the sorted, disjoint `hidden` runs.
fn visible_runs(lo: u32, hi: u32, hidden: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut at = u64::from(lo);
    let first = hidden.partition_point(|h| h.1 < lo);
    for &(h0, h1) in &hidden[first..] {
        if h0 > hi {
            break;
        }
        if u64::from(h0) > at {
            out.push((at as u32, h0 - 1));
        }
        at = at.max(u64::from(h1) + 1);
    }
    if at <= u64::from(hi) {
        out.push((at as u32, hi));
    }
    out
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
    let scope_ix = RectIndex::new(&scope);
    let mut parts = Vec::new();
    let mut met = Vec::new();
    for ranges in rules {
        if same && !ranges.iter().any(|&a| inside(active.0, active.1, a)) {
            continue;
        }
        for &a in ranges {
            met.clear();
            scope_ix.meeting(a, &mut met);
            parts.extend(met.iter().filter_map(|&s| clip(a, s)));
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
