//! Dependency-graph recalculation over a [`Workbook`].
//!
//! The engine parses every formula once, extracts its reference rectangles,
//! and on each edit dirties only the transitive dependents — then evaluates
//! them in topological order (Kahn). Cells on a circular reference are
//! handled as Excel does: without iterative calculation they are 0 and the
//! engine reports them ([`Engine::circular_refs`]); with it they iterate.
//! Volatile formulas (`NOW`, `RAND`…) join every recalculation.
//!
//! **Graceful degradation:** a formula that fails to parse, carries preserved
//! `<f>` attributes (array/data-table), or evaluates through something we
//! don't model yet is marked *unsupported*: its cached value is kept, it is
//! never re-evaluated, and save writes it back byte-faithful. Dependents read
//! the cached value, so partial coverage yields stale-at-worst results,
//! never wrong-by-our-hand ones.

use std::cell::Cell as StdCell;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::formula::{
    self, DynResult, Eval, ExcelError, Expr, Resolver, Value, always_recalc, collect_refs,
    contains_db_fn,
};
use crate::sheet::{Cell, CellValue, Sheet, Workbook, is_array_f};

/// (sheet index, row, col) — the engine's cell address.
pub type Key = (usize, u32, u32);

/// A reference rectangle a formula depends on: sheet + inclusive rect.
type Rect = (usize, u32, u32, u32, u32);

struct FormulaInfo {
    ast: Expr,
    /// Re-evaluated on every recalculation ([`formula::always_recalc`]).
    volatile: bool,
    /// Calls a D-function: evaluated after every other ready cell (see
    /// [`Engine::evaluate`]).
    db: bool,
    /// A legacy formula (loaded, not saved as a dynamic array): a multi-cell
    /// range result implicit-intersects to the cell's row/column rather than
    /// spilling. User-entered formulas are modern (spill).
    legacy: bool,
    /// Contains a spill reference (`A1#`) — its dep rects must be refreshed
    /// whenever spill extents may have changed.
    spillref: bool,
    /// Dependency rects with sheet names resolved to indices. References to
    /// unknown sheets simply have no edge (they evaluate to `#REF!`).
    deps: Vec<Rect>,
}

#[derive(Default)]
pub struct Engine {
    formulas: HashMap<Key, FormulaInfo>,
    /// Cells whose formulas we must not re-evaluate (see module docs).
    unsupported: HashSet<Key>,
    /// Dynamic-array anchors currently showing `#SPILL!` — retried on every
    /// recalculation so they recover the moment the blockage clears.
    spill_blocked: HashSet<Key>,
    /// Current moment as an Excel serial, supplied by the app (None = no
    /// clock → `TODAY`/`NOW` formulas stay on their cached values).
    pub clock: Option<f64>,
    /// PRNG state for `RAND`; None = no randomness source.
    pub seed: Option<u64>,
    /// Formula cells on a circular reference, sorted.
    circular: BTreeSet<Key>,
}

/// Spill chains (an anchor whose array feeds another anchor's spill cells)
/// resolve through repeated post-passes; this bounds pathological loops.
const MAX_SPILL_PASSES: u32 = 8;

impl Engine {
    /// Parse all formulas in the workbook and build the dependency graph.
    pub fn new(wb: &Workbook) -> Engine {
        let mut eng = Engine::default();
        for (s, sheet) in wb.sheets.iter().enumerate() {
            for (&(r, c), cell) in &sheet.cells {
                if let Some(src) = &cell.formula {
                    // Array formulas (`t="array"`, or `cm` after an edit
                    // dropped `f_attrs`) are ours to evaluate — the
                    // dynamic-array engine recomputes their spill. Other
                    // preserved `<f>` attributes stay frozen.
                    let is_array = cell.is_array_formula();
                    let preserved = cell.f_attrs.as_deref().is_some_and(|a| !is_array_f(a));
                    // A plain loaded formula is legacy (implicit intersection);
                    // a dynamic-array (`t="array"`) one spills.
                    eng.index_formula(wb, (s, r, c), src, preserved, !is_array);
                }
            }
        }
        eng.find_circles();
        eng
    }

    /// Find every circle in the whole formula graph without evaluating
    /// anything, so a workbook's circular references are known (and
    /// reported) as soon as it is opened, before any recalculation.
    fn find_circles(&mut self) {
        let mut all: Vec<Key> = self.formulas.keys().copied().collect();
        all.sort_unstable();
        let mut edges: HashMap<Key, Vec<Key>> = HashMap::new();
        for (f, srcs) in self.dependency_edges(&all) {
            for g in srcs {
                edges.entry(g).or_default().push(f);
            }
        }
        self.circular.clear();
        for comp in components_in_order(&all, &edges) {
            let circle =
                comp.len() > 1 || edges.get(&comp[0]).is_some_and(|ds| ds.contains(&comp[0]));
            if circle {
                self.circular.extend(comp);
            }
        }
    }

    /// The formula cells on a circular reference (a cycle of two or more
    /// cells, or a cell that reads itself), sorted by (sheet, row, col). Cells
    /// merely downstream of a circle are not included. Kept up to date across
    /// partial recalculations: a circle is dropped only when one of its cells
    /// is recalculated off it, or loses its formula.
    pub fn circular_refs(&self) -> Vec<Key> {
        self.circular.iter().copied().collect()
    }

    /// Is this cell's formula beyond the engine (kept on its cached value)?
    pub fn is_unsupported(&self, key: Key) -> bool {
        self.unsupported.contains(&key)
    }

    /// Parse and register one formula; preserved-`<f>` cells and parse
    /// failures are marked unsupported.
    fn index_formula(&mut self, wb: &Workbook, key: Key, src: &str, preserved: bool, legacy: bool) {
        if preserved {
            self.unsupported.insert(key);
            return;
        }
        match formula::parse(src) {
            Ok(ast) => {
                let mut deps = Vec::new();
                collect_deps(wb, key, &ast, &mut deps, 0);
                let mut spills = Vec::new();
                formula::collect_spillrefs(&ast, &mut spills);
                let (volatile, db) = recalc_flags(wb, key.0, &ast, 0);
                self.formulas.insert(
                    key,
                    FormulaInfo {
                        volatile,
                        db,
                        legacy,
                        spillref: !spills.is_empty(),
                        ast,
                        deps,
                    },
                );
            }
            Err(_) => {
                self.unsupported.insert(key);
            }
        }
    }

    /// Can this formula text be evaluated at all? Used by frontends to reject
    /// bad input at entry (as Excel does) instead of committing garbage.
    pub fn validate(src: &str) -> Result<(), String> {
        formula::parse(src).map(|_| ())
    }

    /// Apply one cell edit and recalculate everything affected.
    pub fn set_cell(&mut self, wb: &mut Workbook, key: Key, mut cell: Cell) {
        let (s, r, c) = key;
        // Drop stale bookkeeping for this address.
        self.formulas.remove(&key);
        self.circular.remove(&key);
        self.unsupported.remove(&key);
        self.spill_blocked.remove(&key);
        let mut changed = vec![key];
        if let Some(sheet) = wb.sheets.get_mut(s) {
            // Replacing a spill anchor orphans its spilled cells: clear them.
            if let Some(ext) = sheet.cell(r, c).and_then(|cl| cl.spill) {
                changed.extend(clear_spill(sheet, s, (r, c), ext, None));
            }
            // An edit landing inside another anchor's spill breaks that
            // spill: clear its cells and let the anchor recalc to #SPILL!.
            if let Some((anchor, ext)) = spill_owner(sheet, r, c) {
                changed.extend(clear_spill(sheet, s, anchor, ext, Some((r, c))));
                if let Some(a) = sheet.cells.get_mut(&anchor) {
                    a.spill = None;
                }
                changed.push((s, anchor.0, anchor.1));
            }
        }
        if let Some(src) = cell.formula.clone() {
            cell.f_attrs = None; // an edited formula is ours now — modern (spills)
            self.index_formula(wb, key, &src, false, false);
        }
        if s < wb.sheets.len() {
            wb.sheets[s].set_cell(r, c, cell);
        }
        self.recalc_from(wb, &changed);
    }

    /// Recalculate every formula in the workbook (headless `--recalc`, or
    /// after load when a full refresh is wanted).
    pub fn recalc_all(&mut self, wb: &mut Workbook) {
        let all: HashSet<Key> = self.formulas.keys().copied().collect();
        self.evaluate(wb, all, 0);
    }

    /// Dirty the transitive dependents of `changed` (plus volatiles) and
    /// re-evaluate them.
    pub fn recalc_from(&mut self, wb: &mut Workbook, changed: &[Key]) {
        self.recalc_from_depth(wb, changed, 0);
    }

    fn recalc_from_depth(&mut self, wb: &mut Workbook, changed: &[Key], depth: u32) {
        // Spill extents may have moved since indexing — refresh the dep
        // rects of every formula that reads one (`A1#`).
        let with_spillrefs: Vec<Key> = self
            .formulas
            .iter()
            .filter(|(_, i)| i.spillref)
            .map(|(&k, _)| k)
            .collect();
        for k in with_spillrefs {
            let mut deps = Vec::new();
            if let Some(info) = self.formulas.get(&k) {
                collect_deps(wb, k, &info.ast, &mut deps, 0);
            }
            if let Some(info) = self.formulas.get_mut(&k) {
                info.deps = deps;
            }
        }
        // Reverse dependency edges among formulas (source cell → the formulas
        // that read it), built once so the transitive walk follows edges
        // instead of rescanning every formula for each cell it touches —
        // O(edges) rather than O(dirty × formulas).
        let all: Vec<Key> = self.formulas.keys().copied().collect();
        let mut rev: HashMap<Key, Vec<Key>> = HashMap::new();
        for (f, srcs) in self.dependency_edges(&all) {
            for g in srcs {
                rev.entry(g).or_default().push(f);
            }
        }

        let mut dirty: HashSet<Key> = HashSet::new();
        let mut frontier: VecDeque<Key> = VecDeque::new();

        // Seed with the edited formulas, every volatile, and blocked anchors (a
        // cleared blockage isn't otherwise visible to the walk). These are all
        // formula cells, so their dependents are reached through `rev`.
        for &k in changed {
            if self.formulas.contains_key(&k) && dirty.insert(k) {
                frontier.push_back(k);
            }
        }
        for (&k, info) in &self.formulas {
            if info.volatile && dirty.insert(k) {
                frontier.push_back(k);
            }
        }
        for k in self.spill_blocked.iter().copied().collect::<Vec<_>>() {
            if self.formulas.contains_key(&k) && dirty.insert(k) {
                frontier.push_back(k);
            }
        }

        // An edited *data* cell (or a spill write in a recursive pass) isn't a
        // formula key, so `rev` can't reach its dependents. Find those first-
        // level dependents with a single scan over the (few) such seed cells;
        // everything reachable from them is a formula and expands via `rev`.
        let data_seeds: Vec<Key> = changed
            .iter()
            .copied()
            .filter(|k| !self.formulas.contains_key(k))
            .collect();
        if !data_seeds.is_empty() {
            for (&fk, info) in &self.formulas {
                if dirty.contains(&fk) {
                    continue;
                }
                let hit = data_seeds.iter().any(|&(s, r, c)| {
                    info.deps.iter().any(|&(ds, r1, c1, r2, c2)| {
                        ds == s && r >= r1 && r <= r2 && c >= c1 && c <= c2
                    })
                });
                if hit && dirty.insert(fk) {
                    frontier.push_back(fk);
                }
            }
        }

        // Transitive dependents via the precomputed reverse edges.
        while let Some(src) = frontier.pop_front() {
            if let Some(deps) = rev.get(&src) {
                for &fk in deps {
                    if dirty.insert(fk) {
                        frontier.push_back(fk);
                    }
                }
            }
        }
        self.evaluate(wb, dirty, depth);
    }

    /// For each formula in `scope`, the deduped list of formulas *also in
    /// `scope`* that it directly depends on (their cell lies inside one of its
    /// reference rectangles).
    ///
    /// The naive form is O(n²) — every formula tested against every other. This
    /// indexes the scope's cells per sheet, sorted by (row, col), so each
    /// dependency rectangle is answered by a binary search for its first row
    /// plus a walk over only the cells actually inside it. That turns a full
    /// recalc of an `n`-cell dependency chain from O(n²) into ~O(n log n).
    fn dependency_edges(&self, scope: &[Key]) -> Vec<(Key, Vec<Key>)> {
        let mut by_sheet: HashMap<usize, Vec<(u32, u32, Key)>> = HashMap::new();
        for &k in scope {
            by_sheet.entry(k.0).or_default().push((k.1, k.2, k));
        }
        for cells in by_sheet.values_mut() {
            cells.sort_unstable_by_key(|&(r, c, _)| (r, c));
        }
        let mut out: Vec<(Key, Vec<Key>)> = Vec::with_capacity(scope.len());
        let mut srcs: Vec<Key> = Vec::new();
        for &f in scope {
            let info = &self.formulas[&f];
            srcs.clear();
            for &(ds, r1, c1, r2, c2) in &info.deps {
                let Some(cells) = by_sheet.get(&ds) else {
                    continue;
                };
                // Cells are (row, col)-ordered, so the rect's rows are the
                // contiguous slice from the first row ≥ r1 up to the last ≤ r2.
                let start = cells.partition_point(|&(r, _, _)| r < r1);
                for &(r, c, g) in &cells[start..] {
                    if r > r2 {
                        break;
                    }
                    if c >= c1 && c <= c2 {
                        srcs.push(g);
                    }
                }
            }
            // One edge per (dependent, source) even if several rects overlap it.
            srcs.sort_unstable();
            srcs.dedup();
            out.push((f, srcs.clone()));
        }
        out
    }

    /// Kahn's algorithm over the dirty subgraph, then evaluation in order.
    fn evaluate(&mut self, wb: &mut Workbook, dirty: HashSet<Key>, depth: u32) {
        // Only supported formulas actually evaluate; unsupported ones keep
        // their cached values but still satisfy dependents.
        let dirty: Vec<Key> = dirty
            .into_iter()
            .filter(|k| self.formulas.contains_key(k) && !self.unsupported.contains(k))
            .collect();
        if dirty.is_empty() {
            return;
        }
        // in-degree of F = dirty formulas F depends on; edges G → dependents.
        // (A self-reference — F's own cell inside its rect — counts here, so it
        // never reaches in-degree 0 and lands in the cycle remainder below,
        // exactly like any other circularity.)
        let mut indeg: HashMap<Key, usize> = dirty.iter().map(|&k| (k, 0)).collect();
        let mut edges: HashMap<Key, Vec<Key>> = HashMap::new();
        for (f, srcs) in self.dependency_edges(&dirty) {
            *indeg.get_mut(&f).unwrap() = srcs.len();
            for g in srcs {
                edges.entry(g).or_default().push(f);
            }
        }

        // A D-function's computed criteria read helper cells it has no edge
        // to (it is always recalculated instead), so a ready D-function
        // waits until no other ready cell is left: helpers it reads are
        // evaluated first. A helper that itself reads a D-function has a real
        // edge and still waits for it. Two D-functions whose criteria read
        // each other's results stay in arbitrary order.
        let is_db = |k: &Key| self.formulas.get(k).is_some_and(|i| i.db);
        let mut queue: VecDeque<Key> = VecDeque::new();
        let mut deferred: VecDeque<Key> = VecDeque::new();
        let mut ready: Vec<Key> = indeg
            .iter()
            .filter(|&(_, &d)| d == 0)
            .map(|(&k, _)| k)
            .collect();
        ready.sort_unstable();
        for k in ready {
            if is_db(&k) {
                deferred.push_back(k);
            } else {
                queue.push_back(k);
            }
        }
        let mut done: HashSet<Key> = HashSet::new();
        let mut spilled: Vec<Key> = Vec::new();
        let mut db_done: Vec<Key> = Vec::new();
        while let Some(k) = queue.pop_front().or_else(|| deferred.pop_front()) {
            done.insert(k);
            if self.formulas.get(&k).is_some_and(|i| i.db) {
                db_done.push(k);
            }
            spilled.extend(self.eval_one(wb, k));
            if let Some(dependents) = edges.get(&k).cloned() {
                for d in dependents {
                    let e = indeg.get_mut(&d).unwrap();
                    *e -= 1;
                    if *e == 0 {
                        if self.formulas.get(&d).is_some_and(|i| i.db) {
                            deferred.push_back(d);
                        } else {
                            queue.push_back(d);
                        }
                    }
                }
            }
        }

        // Whatever never reached in-degree 0 sits on a circle or downstream
        // of one. Split it into strongly connected components and take them
        // in dependency order: a circle (two or more cells, or a cell that
        // reads itself) is 0 without iterative calculation, or iterates with
        // it; a cell merely downstream is evaluated normally from those
        // values.
        let rest: Vec<Key> = {
            let mut v: Vec<Key> = dirty
                .iter()
                .copied()
                .filter(|k| !done.contains(k))
                .collect();
            v.sort_unstable();
            v
        };
        let mut found: Vec<Key> = Vec::new();
        for comp in components_in_order(&rest, &edges) {
            let circle =
                comp.len() > 1 || edges.get(&comp[0]).is_some_and(|ds| ds.contains(&comp[0]));
            if !circle {
                spilled.extend(self.eval_one(wb, comp[0]));
                continue;
            }
            found.extend(comp.iter().copied());
            match wb.iterate {
                Some((count, delta)) => {
                    spilled.extend(self.iterate_circle(wb, &comp, count, delta));
                }
                None => {
                    for &(s, r, c) in &comp {
                        if let Some(sheet) = wb.sheets.get_mut(s) {
                            sheet.cells.entry((r, c)).or_default().value = CellValue::Number(0.0);
                        }
                    }
                }
            }
        }
        // Circles found now replace whatever was known about the cells just
        // recalculated; circles elsewhere stay as they were.
        for k in &dirty {
            self.circular.remove(k);
        }
        self.circular.extend(found);
        // A D-function evaluated above may read (through a computed
        // criterion, with no edge) a helper that sits on or below a circle,
        // which only got its value just now. Run those D-functions, and what
        // depends on them, once more. Bounded like the spill passes; a
        // circle member re-run this way is 0 again, or gets one more sweep
        // that the rollback rule keeps put once converged.
        if !rest.is_empty() && !db_done.is_empty() && depth < MAX_SPILL_PASSES {
            db_done.sort_unstable();
            self.recalc_from_depth(wb, &db_done, depth + 1);
        }
        // Spill writes change plain-value cells whose dependents the dirty
        // walk couldn't see (only the anchor is a formula). One more pass
        // over those cells picks them up; chains converge quickly.
        if !spilled.is_empty() && depth < MAX_SPILL_PASSES {
            spilled.sort_unstable();
            spilled.dedup();
            self.recalc_from_depth(wb, &spilled, depth + 1);
        }
    }

    /// Iterative calculation of one circle (Excel's File > Options >
    /// Formulas > Enable iterative calculation): sweep its cells up to
    /// `count` times. A sweep whose largest change is under `delta` means the
    /// circle had already converged, so that sweep is rolled back rather than
    /// applied: recalculating a converged circle leaves it where it was (and
    /// a file Excel saved verifies), instead of creeping one step further.
    fn iterate_circle(
        &mut self,
        wb: &mut Workbook,
        comp: &[Key],
        count: u32,
        delta: f64,
    ) -> Vec<Key> {
        let mut spilled = Vec::new();
        let value_of = |wb: &Workbook, k: Key| {
            wb.sheets[k.0]
                .cell(k.1, k.2)
                .map(|c| c.value.clone())
                .unwrap_or_default()
        };
        for _ in 0..count.max(1) {
            let before: Vec<CellValue> = comp.iter().map(|&k| value_of(wb, k)).collect();
            let mut max_change = 0.0f64;
            for (i, &k) in comp.iter().enumerate() {
                spilled.extend(self.eval_one(wb, k));
                match (&before[i], &value_of(wb, k)) {
                    (CellValue::Number(x), CellValue::Number(y)) => {
                        max_change = max_change.max((x - y).abs());
                    }
                    (a, b) if a != b => max_change = f64::MAX,
                    _ => {}
                }
            }
            if max_change < delta {
                for (&(s, r, c), v) in comp.iter().zip(before) {
                    if let Some(sheet) = wb.sheets.get_mut(s) {
                        sheet.cells.entry((r, c)).or_default().value = v;
                    }
                }
                break;
            }
        }
        spilled
    }

    /// Evaluate one formula and store its result. Returns the keys of cells
    /// beyond the anchor whose stored values changed (spill writes/clears).
    fn eval_one(&mut self, wb: &mut Workbook, key: Key) -> Vec<Key> {
        let info = match self.formulas.get(&key) {
            Some(i) => i,
            None => return Vec::new(),
        };
        // Legacy formulas implicit-intersect a range result; modern (user-entered
        // or `t="array"`) ones spill.
        let spill = !info.legacy;
        let resolver = WbResolver {
            wb,
            clock: self.clock,
            rand_state: StdCell::new(self.seed.unwrap_or(0)),
            has_rand: self.seed.is_some(),
        };
        let mut ev = Eval::new(&resolver, key.0, (key.1, key.2));
        let result = ev.eval_dynamic_as(&info.ast, spill);
        let unsupported = ev.unsupported;
        if self.seed.is_some() {
            self.seed = Some(resolver.rand_state.get());
        }
        if unsupported {
            // Something beyond the engine: freeze this cell on its cached
            // value from here on.
            self.unsupported.insert(key);
            return Vec::new();
        }
        let (s, r, c) = key;
        let Some(sheet) = wb.sheets.get_mut(s) else {
            return Vec::new();
        };
        let old = sheet.cell(r, c).and_then(|cl| cl.spill).unwrap_or((1, 1));
        let mut changed = Vec::new();
        match result {
            DynResult::Scalar(v) => {
                changed.extend(clear_spill(sheet, s, (r, c), old, None));
                let entry = sheet.cells.entry((r, c)).or_default();
                entry.value = value_to_cell(v);
                entry.spill = None;
                self.spill_blocked.remove(&key);
            }
            DynResult::Array(m) => {
                let (h, w) = (m.len() as u32, m[0].len() as u32);
                // Blocked when the array runs off the grid, or any target
                // cell (other than the anchor) holds content that isn't this
                // anchor's previous spill.
                let off_grid = r + h > crate::sheet::MAX_ROWS || c + w > crate::sheet::MAX_COLS;
                let blocked = off_grid
                    || (r..r + h).any(|rr| {
                        (c..c + w).any(|cc| {
                            if (rr, cc) == (r, c) {
                                return false;
                            }
                            let Some(cell) = sheet.cell(rr, cc) else {
                                return false;
                            };
                            let ours = rr < r + old.0 && cc < c + old.1 && cell.formula.is_none();
                            !cell.is_blank() && !ours
                        })
                    });
                if blocked {
                    changed.extend(clear_spill(sheet, s, (r, c), old, None));
                    let entry = sheet.cells.entry((r, c)).or_default();
                    entry.value = CellValue::Error(ExcelError::Spill.code().to_string());
                    entry.spill = None;
                    self.spill_blocked.insert(key);
                } else {
                    changed.extend(clear_spill(sheet, s, (r, c), old, Some((h, w))));
                    for (i, row) in m.into_iter().enumerate() {
                        for (j, v) in row.into_iter().enumerate() {
                            let (rr, cc) = (r + i as u32, c + j as u32);
                            let entry = sheet.cells.entry((rr, cc)).or_default();
                            entry.value = value_to_cell(v);
                            if (rr, cc) != (r, c) {
                                entry.formula = None;
                                entry.f_attrs = None;
                                entry.spill = None;
                                changed.push((s, rr, cc));
                            }
                        }
                    }
                    let entry = sheet.cells.entry((r, c)).or_default();
                    entry.spill = Some((h, w));
                    self.spill_blocked.remove(&key);
                }
            }
        }
        changed
    }
}

/// Clear the plain-value cells of a spill (keeping styles) outside the
/// surviving extent `keep` (None = clear all but the anchor). Returns the
/// cleared keys. Cells holding formulas are left alone.
fn clear_spill(
    sheet: &mut Sheet,
    s: usize,
    anchor: (u32, u32),
    old: (u32, u32),
    keep: Option<(u32, u32)>,
) -> Vec<Key> {
    let (r, c) = anchor;
    let (kh, kw) = keep.unwrap_or((1, 1));
    let mut out = Vec::new();
    for rr in r..r + old.0 {
        for cc in c..c + old.1 {
            if (rr, cc) == (r, c) || (rr < r + kh && cc < c + kw) {
                continue;
            }
            if sheet
                .cell(rr, cc)
                .is_some_and(|cl| cl.formula.is_none() && !cl.value.is_empty())
            {
                sheet.clear_cell(rr, cc);
                out.push((s, rr, cc));
            }
        }
    }
    out
}

/// The anchor whose spill contains (r, c), if any (excluding (r, c) itself
/// being the anchor).
fn spill_owner(sheet: &Sheet, r: u32, c: u32) -> Option<((u32, u32), (u32, u32))> {
    for (&(ar, ac), cell) in &sheet.cells {
        if ar > r {
            break;
        }
        if let Some((h, w)) = cell.spill {
            if (ar, ac) != (r, c) && r >= ar && r < ar + h && c >= ac && c < ac + w {
                return Some(((ar, ac), (h, w)));
            }
        }
    }
    None
}

/// Dependency rects of an AST, with sheet names resolved, defined names
/// expanded (depth-capped against name→name loops), and structured refs
/// resolved through the workbook's table definitions. `key` is the formula's
/// own cell — `[@Col]` depends on exactly that row, never the whole table
/// (a whole-table dep would make calculated columns self-referential).
fn collect_deps(wb: &Workbook, key: Key, ast: &Expr, out: &mut Vec<Rect>, depth: u32) {
    let (sheet, row, col) = key;
    let mut named = Vec::new();
    collect_refs(ast, &mut named);
    for (sheet_name, r1, c1, r2, c2) in named {
        let s = match sheet_name {
            None => Some(sheet),
            Some(name) => wb.sheet_index(&name),
        };
        if let Some(s) = s {
            out.push((s, r1, c1, r2, c2));
        }
    }
    let mut spillrefs = Vec::new();
    formula::collect_spillrefs(ast, &mut spillrefs);
    for (sheet_name, r, c) in spillrefs {
        let s = match sheet_name {
            None => Some(sheet),
            Some(name) => wb.sheet_index(&name),
        };
        if let Some(s) = s {
            // Widen to the anchor's current spill extent (at least itself).
            let (h, w) = wb
                .sheets
                .get(s)
                .and_then(|sh| sh.cell(r, c))
                .and_then(|cl| cl.spill)
                .unwrap_or((1, 1));
            out.push((s, r, c, r + h - 1, c + w - 1));
        }
    }
    let mut spans = Vec::new();
    formula::collect_ref3d(ast, &mut spans);
    for (first, last, r1, c1, r2, c2) in spans {
        if let (Some(a), Some(b)) = (wb.sheet_index(&first), wb.sheet_index(&last)) {
            for s in a.min(b)..=a.max(b) {
                out.push((s, r1, c1, r2, c2));
            }
        }
    }
    // Tables iterated by SUMX-family calls: the whole data region is read.
    let mut iterated = Vec::new();
    formula::collect_iterated_tables(ast, &mut iterated);
    for name in iterated {
        if let Some(t) = wb.table(&name) {
            out.push((t.sheet, t.range.0, t.range.1, t.range.2, t.range.3));
        }
    }
    let mut structured = Vec::new();
    formula::collect_structured(ast, &mut structured);
    for (tname, item, col1, col2) in structured {
        let t = match &tname {
            Some(n) => wb.table(n),
            None => wb.table_at(sheet, row, col),
        };
        if let Some(t) = t {
            let info = to_table_info(t);
            if let Some((r1, c1, r2, c2)) = info.resolve(item, &col1, &col2, row) {
                out.push((t.sheet, r1, c1, r2, c2));
            }
        }
    }
    if depth >= 8 {
        return;
    }
    let mut names = Vec::new();
    formula::collect_names(ast, &mut names);
    // Function-call names too: `f(3)` may be a defined-name LAMBDA whose
    // body references cells — those references are real dependencies.
    // (Builtin names simply miss the defined-name lookup.)
    formula::collect_called_names(ast, &mut names);
    for n in names {
        if let Some(def) = wb.defined_name(&n, sheet) {
            if let Ok(def_ast) = formula::parse(def) {
                collect_deps(wb, key, &def_ast, out, depth + 1);
            }
        }
    }
}

/// (always recalculate, calls a D-function) for a formula, looking through
/// the defined names it references or calls (`=Total` where Total is a DSUM
/// or NOW()), resolved as [`collect_deps`] resolves them and to the same
/// depth.
fn recalc_flags(wb: &Workbook, sheet: usize, ast: &Expr, depth: u32) -> (bool, bool) {
    let (mut volatile, mut db) = (always_recalc(ast), contains_db_fn(ast));
    if (volatile && db) || depth >= 8 {
        return (volatile, db);
    }
    let mut names = Vec::new();
    formula::collect_names(ast, &mut names);
    formula::collect_called_names(ast, &mut names);
    for n in names {
        let Some(def_ast) = wb
            .defined_name(&n, sheet)
            .and_then(|def| formula::parse(def).ok())
        else {
            continue;
        };
        let (v, d) = recalc_flags(wb, sheet, &def_ast, depth + 1);
        volatile |= v;
        db |= d;
    }
    (volatile, db)
}

fn to_table_info(t: &crate::sheet::Table) -> crate::formula::TableInfo {
    crate::formula::TableInfo {
        sheet: t.sheet,
        range: t.range,
        header_rows: t.header_rows,
        totals_rows: t.totals_rows,
        columns: t.columns.clone(),
    }
}

/// Engine result → stored cell value. A formula referencing an empty cell
/// yields 0 in Excel (`=Z99` shows 0), so Empty lands as Number(0).
fn value_to_cell(v: Value) -> CellValue {
    match v {
        Value::Empty => CellValue::Number(0.0),
        Value::Num(n) => CellValue::Number(n),
        Value::Str(s) => CellValue::Text(s),
        Value::Bool(b) => CellValue::Bool(b),
        Value::Err(e) => CellValue::Error(e.code().to_string()),
    }
}

/// Stored cell value → evaluation value.
pub fn cell_to_value(v: &CellValue) -> Value {
    match v {
        CellValue::Empty => Value::Empty,
        CellValue::Number(n) => Value::Num(*n),
        CellValue::Text(s) => Value::Str(s.clone()),
        CellValue::Bool(b) => Value::Bool(*b),
        CellValue::Error(e) => Value::Err(ExcelError::from_code(e).unwrap_or(ExcelError::Value)),
    }
}

/// The evaluator's view of a workbook mid-recalculation. Values already
/// updated earlier in topological order are naturally visible.
struct WbResolver<'a> {
    wb: &'a Workbook,
    clock: Option<f64>,
    rand_state: StdCell<u64>,
    has_rand: bool,
}

impl Resolver for WbResolver<'_> {
    fn value(&self, sheet: usize, row: u32, col: u32) -> Value {
        match self.wb.sheets.get(sheet).and_then(|s| s.cell(row, col)) {
            Some(cell) => cell_to_value(&cell.value),
            None => Value::Empty,
        }
    }

    fn sheet_index(&self, name: &str) -> Option<usize> {
        self.wb.sheet_index(name)
    }

    fn cells_in(
        &self,
        sheet: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
    ) -> Vec<((u32, u32), Value)> {
        let mut out = Vec::new();
        if let Some(s) = self.wb.sheets.get(sheet) {
            for (&(r, c), cell) in s.cells.range((r1, 0)..=(r2, u32::MAX)) {
                if c >= c1 && c <= c2 && !cell.value.is_empty() {
                    out.push(((r, c), cell_to_value(&cell.value)));
                }
            }
        }
        out
    }

    fn today(&self) -> Option<f64> {
        self.clock
    }

    fn used_size(&self, sheet: usize) -> (u32, u32) {
        self.wb
            .sheets
            .get(sheet)
            .map(|s| s.used_size())
            .unwrap_or((0, 0))
    }

    fn defined_name(&self, name: &str, current_sheet: usize) -> Option<String> {
        self.wb
            .defined_name(name, current_sheet)
            .map(str::to_string)
    }

    fn table(&self, name: &str) -> Option<formula::TableInfo> {
        self.wb.table(name).map(to_table_info)
    }

    fn table_at(&self, sheet: usize, row: u32, col: u32) -> Option<formula::TableInfo> {
        self.wb.table_at(sheet, row, col).map(to_table_info)
    }

    fn spill_extent(&self, sheet: usize, row: u32, col: u32) -> Option<(u32, u32)> {
        self.wb
            .sheets
            .get(sheet)
            .and_then(|s| s.cell(row, col))
            .and_then(|c| c.spill)
    }

    fn rand(&self) -> Option<f64> {
        if !self.has_rand {
            return None;
        }
        // xorshift64* — plenty for spreadsheet RAND.
        let mut x = self.rand_state.get().max(1);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rand_state.set(x);
        let r = (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64;
        Some(r / (1u64 << 53) as f64)
    }

    fn date1904(&self) -> bool {
        self.wb.date1904
    }

    fn cell_formula(&self, sheet: usize, row: u32, col: u32) -> Option<String> {
        self.wb
            .sheets
            .get(sheet)
            .and_then(|s| s.cell(row, col))
            .and_then(|c| c.formula.clone())
    }

    fn row_hidden(&self, sheet: usize, row: u32) -> bool {
        self.wb.sheets.get(sheet).is_some_and(|s| s.row_hidden(row))
    }

    fn row_filtered(&self, sheet: usize, row: u32) -> bool {
        self.wb
            .sheets
            .get(sheet)
            .is_some_and(|s| s.row_filtered(row))
    }

    fn num_format(&self, sheet: usize, row: u32, col: u32) -> Option<String> {
        let style = self.wb.sheets.get(sheet)?.cell(row, col)?.style;
        let xf = self.wb.styles.xfs.get(style as usize)?;
        xf.code.clone().or_else(|| numfmt_code(xf.numfmt))
    }
}

/// A format code for a classified format that carries no code of its own
/// (formatting authored in the editor), so `CELL("format")` sees what the grid
/// shows.
fn numfmt_code(nf: crate::sheet::NumFmt) -> Option<String> {
    use crate::sheet::NumFmt;
    let dec = |d: u8| {
        if d == 0 {
            String::new()
        } else {
            format!(".{}", "0".repeat(d as usize))
        }
    };
    Some(match nf {
        NumFmt::General => return None,
        NumFmt::Number {
            decimals,
            thousands,
        } => format!("{}0{}", if thousands { "#,##" } else { "" }, dec(decimals)),
        NumFmt::Percent { decimals } => format!("0{}%", dec(decimals)),
        NumFmt::Scientific => "0.00E+00".into(),
        NumFmt::Date => "m/d/yyyy".into(),
        NumFmt::Time => "h:mm:ss".into(),
        NumFmt::DateTime => "m/d/yyyy h:mm".into(),
        NumFmt::Text => "@".into(),
    })
}

/// The strongly connected components of the graph `edges` (precedent →
/// dependents) restricted to `nodes`, in dependency order: every component
/// comes after the ones it reads from. Tarjan's algorithm, iterative so a
/// long chain cannot overflow the stack; it emits components dependents
/// first, so the result is reversed.
fn components_in_order(nodes: &[Key], edges: &HashMap<Key, Vec<Key>>) -> Vec<Vec<Key>> {
    let in_set: HashSet<Key> = nodes.iter().copied().collect();
    let succ = |k: Key| -> Vec<Key> {
        edges
            .get(&k)
            .map(|ds| ds.iter().copied().filter(|d| in_set.contains(d)).collect())
            .unwrap_or_default()
    };
    let mut index: HashMap<Key, usize> = HashMap::new();
    let mut low: HashMap<Key, usize> = HashMap::new();
    let mut on_stack: HashSet<Key> = HashSet::new();
    let mut stack: Vec<Key> = Vec::new();
    let mut out: Vec<Vec<Key>> = Vec::new();
    let mut next = 0usize;
    for &root in nodes {
        if index.contains_key(&root) {
            continue;
        }
        // (node, its successors, how many of them were visited)
        let mut work: Vec<(Key, Vec<Key>, usize)> = Vec::new();
        index.insert(root, next);
        low.insert(root, next);
        next += 1;
        stack.push(root);
        on_stack.insert(root);
        work.push((root, succ(root), 0));
        while let Some((v, succs, i)) = work.last_mut() {
            let v = *v;
            if *i < succs.len() {
                let w = succs[*i];
                *i += 1;
                match index.entry(w) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(next);
                        low.insert(w, next);
                        next += 1;
                        stack.push(w);
                        on_stack.insert(w);
                        let ws = succ(w);
                        work.push((w, ws, 0));
                    }
                    std::collections::hash_map::Entry::Occupied(e) if on_stack.contains(&w) => {
                        let lw = *e.get();
                        let lv = low.get_mut(&v).unwrap();
                        *lv = (*lv).min(lw);
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {}
                }
                continue;
            }
            work.pop();
            if let Some((parent, _, _)) = work.last() {
                let lv = low[&v];
                let lp = low.get_mut(parent).unwrap();
                *lp = (*lp).min(lv);
            }
            if low[&v] == index[&v] {
                let mut comp = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack.remove(&w);
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                comp.sort_unstable();
                out.push(comp);
            }
        }
    }
    out.reverse();
    out
}

/// Evaluate a formula string in the context of cell (sheet, row, col) over the
/// workbook — ad-hoc, for things like conditional-format rule conditions. Uses no
/// clock/rand (CF conditions shouldn't be volatile). Returns the value, or
/// `#NAME?` if the formula doesn't parse.
pub fn eval_formula_at(wb: &Workbook, sheet: usize, row: u32, col: u32, src: &str) -> Value {
    match formula::parse(src) {
        Ok(ast) => {
            let resolver = WbResolver {
                wb,
                clock: None,
                rand_state: StdCell::new(0),
                has_rand: false,
            };
            let mut ev = Eval::new(&resolver, sheet, (row, col));
            ev.eval_formula(&ast)
        }
        Err(_) => Value::Err(ExcelError::Name),
    }
}

/// The (already recalculated) value of a cell as a [`Value`].
pub fn cell_value_at(wb: &Workbook, sheet: usize, row: u32, col: u32) -> Value {
    let resolver = WbResolver {
        wb,
        clock: None,
        rand_state: StdCell::new(0),
        has_rand: false,
    };
    resolver.value(sheet, row, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Sheet;

    fn wb_one_sheet(cells: &[(&str, Cell)]) -> Workbook {
        let mut sheet = Sheet {
            name: "Sheet1".to_string(),
            ..Sheet::default()
        };
        for (name, cell) in cells {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            sheet.set_cell(r, c, cell.clone());
        }
        Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        }
    }

    fn value_at(wb: &Workbook, name: &str) -> CellValue {
        let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
        wb.sheets[0]
            .cell(r, c)
            .map(|cl| cl.value.clone())
            .unwrap_or(CellValue::Empty)
    }

    fn set(engine: &mut Engine, wb: &mut Workbook, name: &str, cell: Cell) {
        let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
        engine.set_cell(wb, (0, r, c), cell);
    }

    /// A dynamic-array formula as Excel stores it: `t="array"` so it spills. (A
    /// plain loaded formula is legacy and implicit-intersects instead.)
    fn array_formula(src: &str) -> Cell {
        let mut c = Cell::formula(src);
        c.f_attrs = Some("t=\"array\"".to_string());
        c
    }

    #[test]
    fn large_chain_and_whole_column_recalc_correctly() {
        // Stresses the sorted-index dependency builder at scale: a long
        // running-sum chain (tiny per-formula rects) plus a whole-column SUM
        // (one huge rect), then a head edit that must ripple the entire chain
        // through the data-seed scan + reverse-edge walk.
        const N: u32 = 400;
        let mut sheet = Sheet {
            name: "Sheet1".to_string(),
            ..Sheet::default()
        };
        // A_i = 1; B1 = A1; B_i = B_{i-1} + A_i, so B_i == i.
        for i in 0..N {
            sheet.set_cell(i, 0, Cell::number(1.0));
            let f = if i == 0 {
                "A1".to_string()
            } else {
                format!("B{}+A{}", i, i + 1)
            };
            sheet.set_cell(i, 1, Cell::formula(&f));
        }
        sheet.set_cell(0, 3, Cell::formula("SUM(B:B)")); // whole-column rect
        let mut wb = Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        };
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "B400"), CellValue::Number(400.0));
        assert_eq!(
            value_at(&wb, "D1"),
            CellValue::Number((N * (N + 1) / 2) as f64)
        );

        // Editing the head data cell (+10) must propagate down the whole chain
        // and into the whole-column aggregate: every B_i rises by 10.
        set(&mut eng, &mut wb, "A1", Cell::number(11.0));
        assert_eq!(value_at(&wb, "B1"), CellValue::Number(11.0));
        assert_eq!(value_at(&wb, "B400"), CellValue::Number(410.0));
        assert_eq!(
            value_at(&wb, "D1"),
            CellValue::Number((N * (N + 1) / 2 + 10 * N) as f64)
        );
    }

    #[test]
    fn edit_propagates_through_chain() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::formula("A1*2")),
            ("A3", Cell::formula("A2*2")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(4.0));
        // Change the root: the whole chain updates.
        set(&mut eng, &mut wb, "A1", Cell::number(10.0));
        assert_eq!(value_at(&wb, "A2"), CellValue::Number(20.0));
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(40.0));
    }

    #[test]
    fn range_dependencies() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("B1", Cell::formula("SUM(A1:A10)")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "B1"), CellValue::Number(3.0));
        // Adding a value inside the range dirties the SUM.
        set(&mut eng, &mut wb, "A7", Cell::number(4.0));
        assert_eq!(value_at(&wb, "B1"), CellValue::Number(7.0));
    }

    #[test]
    fn circular_references_are_zero_and_reported() {
        // #660: without iterative calculation a circle is 0 (never
        // #CYCLE!), cells downstream of it evaluate normally, and the engine
        // names the circle's cells.
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::formula("B1+1")),
            ("B1", Cell::formula("A1+1")),
            ("C1", Cell::number(5.0)),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(0.0));
        assert_eq!(value_at(&wb, "B1"), CellValue::Number(0.0));
        assert_eq!(eng.circular_refs(), vec![(0, 0, 0), (0, 0, 1)]);

        // The issue's cells, typed one by one.
        let mut wb = wb_one_sheet(&[]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "E1", Cell::formula("E1+1"));
        assert_eq!(value_at(&wb, "E1"), CellValue::Number(0.0));
        set(&mut eng, &mut wb, "F1", Cell::formula("G1+1"));
        assert_eq!(value_at(&wb, "F1"), CellValue::Number(1.0));
        set(&mut eng, &mut wb, "G1", Cell::formula("F1*2"));
        assert_eq!(value_at(&wb, "F1"), CellValue::Number(0.0));
        assert_eq!(value_at(&wb, "G1"), CellValue::Number(0.0));
        set(&mut eng, &mut wb, "BD5", Cell::formula("SUM(BD:BD)"));
        assert_eq!(value_at(&wb, "BD5"), CellValue::Number(0.0));
        set(&mut eng, &mut wb, "H1", Cell::formula("E1+5"));
        assert_eq!(value_at(&wb, "H1"), CellValue::Number(5.0));
        let e1 = (0, 0, 4);
        let (f1, g1) = ((0, 0, 5), (0, 0, 6));
        let bd5 = (0, 4, 55);
        assert_eq!(eng.circular_refs(), vec![e1, f1, g1, bd5]);

        // An unrelated edit, and a volatile's recalculation, keep them.
        eng.seed = Some(1);
        set(&mut eng, &mut wb, "Z1", Cell::formula("RAND()"));
        set(&mut eng, &mut wb, "Z2", Cell::number(3.0));
        assert_eq!(eng.circular_refs(), vec![e1, f1, g1, bd5]);
        // Breaking a circle drops it; replacing a formula drops that cell.
        set(&mut eng, &mut wb, "G1", Cell::number(4.0));
        assert_eq!(value_at(&wb, "F1"), CellValue::Number(5.0));
        set(&mut eng, &mut wb, "E1", Cell::number(1.0));
        assert_eq!(value_at(&wb, "H1"), CellValue::Number(6.0));
        assert_eq!(eng.circular_refs(), vec![bd5]);
        set(&mut eng, &mut wb, "BD5", Cell::default());
        assert!(eng.circular_refs().is_empty());
    }

    #[test]
    fn loaded_circle_values_survive_a_full_recalc() {
        // #660: an Excel-saved circle (cached 0) verifies: recalc_all leaves
        // it at 0 and writes no error.
        let cached = |f: &str| Cell {
            value: CellValue::Number(0.0),
            ..Cell::formula(f)
        };
        let mut wb = wb_one_sheet(&[
            ("E1", cached("E1+1")),
            ("F1", cached("G1+1")),
            ("G1", cached("F1*2")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        for c in ["E1", "F1", "G1"] {
            assert_eq!(value_at(&wb, c), CellValue::Number(0.0), "{c}");
        }
    }

    #[test]
    fn converged_iteration_is_stable_under_recalc() {
        // #660: with iterate="1" iterateCount="100" iterateDelta="0.001",
        // Excel's cached D1 = D1/2+5 value survives a full recalculation.
        let cached = 9.999998807907104;
        let mut wb = wb_one_sheet(&[(
            "D1",
            Cell {
                value: CellValue::Number(cached),
                ..Cell::formula("D1/2+5")
            },
        )]);
        wb.iterate = Some((100, 0.001));
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "D1"), CellValue::Number(cached));
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "D1"), CellValue::Number(cached));
        assert_eq!(eng.circular_refs(), vec![(0, 0, 3)]);
        // A cell downstream of the circle reads its settled value once.
        set(&mut eng, &mut wb, "E1", Cell::formula("D1*2"));
        assert_eq!(value_at(&wb, "E1"), CellValue::Number(cached * 2.0));
    }

    #[test]
    fn unsupported_keeps_cached_value() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            (
                "B1",
                Cell {
                    value: CellValue::Number(42.0), // Excel's cached result
                    formula: Some("PIVOTBY(A1,4)".to_string()),
                    ..Cell::default()
                },
            ),
            ("C1", Cell::formula("B1*2")), // depends on the unsupported cell
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        // B1 keeps Excel's cached 42; C1 computes from the cache.
        assert_eq!(value_at(&wb, "B1"), CellValue::Number(42.0));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(84.0));
        let (r, c) = crate::sheet::parse_cell_name("B1").unwrap();
        assert!(eng.is_unsupported((0, r, c)));
    }

    #[test]
    fn empty_ref_result_becomes_zero() {
        let mut wb = wb_one_sheet(&[("A1", Cell::formula("Z99"))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(0.0));
    }

    #[test]
    fn cross_sheet_dependencies() {
        let mut s1 = Sheet {
            name: "Data".to_string(),
            ..Sheet::default()
        };
        s1.set_cell(0, 0, Cell::number(7.0));
        let mut s2 = Sheet {
            name: "Calc".to_string(),
            ..Sheet::default()
        };
        s2.set_cell(0, 0, Cell::formula("Data!A1*3"));
        let mut wb = Workbook {
            sheets: vec![s1, s2],
            ..Workbook::default()
        };
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(
            wb.sheets[1].cell(0, 0).unwrap().value,
            CellValue::Number(21.0)
        );
        // Editing Data!A1 recalcs Calc!A1.
        eng.set_cell(&mut wb, (0, 0, 0), Cell::number(10.0));
        assert_eq!(
            wb.sheets[1].cell(0, 0).unwrap().value,
            CellValue::Number(30.0)
        );
    }

    #[test]
    fn clearing_a_formula_updates_dependents() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(3.0)),
            ("A2", Cell::formula("A1+1")),
            ("A3", Cell::formula("A2+1")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(5.0));
        // Replace the middle formula with a literal.
        set(&mut eng, &mut wb, "A2", Cell::number(100.0));
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(101.0));
        // Clear it entirely: A3 = empty + 1 = 1.
        set(&mut eng, &mut wb, "A2", Cell::default());
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(1.0));
    }

    #[test]
    fn volatile_recalcs_on_any_edit() {
        let mut wb = wb_one_sheet(&[("A1", Cell::formula("TODAY()")), ("B1", Cell::number(1.0))]);
        let mut eng = Engine::new(&wb);
        eng.clock = Some(45_306.25);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(45_306.0));
        // Clock advances; an unrelated edit still refreshes TODAY().
        eng.clock = Some(45_400.5);
        set(&mut eng, &mut wb, "B1", Cell::number(2.0));
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(45_400.0));
    }

    #[test]
    fn no_clock_keeps_cached_today() {
        let mut wb = wb_one_sheet(&[(
            "A1",
            Cell {
                value: CellValue::Number(44_000.0),
                formula: Some("TODAY()".to_string()),
                ..Cell::default()
            },
        )]);
        let mut eng = Engine::new(&wb); // no clock
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(44_000.0));
    }

    #[test]
    fn defined_names_and_whole_columns_drive_recalc() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("B1", Cell::formula("SUM(Data)")),
            ("C1", Cell::formula("SUM(A:A)")),
        ]);
        wb.defined_names.push(crate::sheet::DefinedName {
            name: "Data".to_string(),
            scope: None,
            formula: "Sheet1!$A$1:$A$5".to_string(),
        });
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "B1"), CellValue::Number(3.0));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(3.0));
        // An edit inside the named range dirties the SUM through the name.
        set(&mut eng, &mut wb, "A4", Cell::number(10.0));
        assert_eq!(value_at(&wb, "B1"), CellValue::Number(13.0));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(13.0));
        // Deep in the column (outside the name) only the A:A sum changes.
        set(&mut eng, &mut wb, "A100", Cell::number(1.0));
        assert_eq!(value_at(&wb, "B1"), CellValue::Number(13.0));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(14.0));
    }

    #[test]
    fn structured_refs_calc_without_cycles_and_propagate() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::text("Item")),
            ("B1", Cell::text("Qty")),
            ("C1", Cell::text("Amount")),
            ("A2", Cell::text("pen")),
            ("B2", Cell::number(3.0)),
            ("C2", Cell::formula("[@Qty]*2")),
            ("A3", Cell::text("pad")),
            ("B3", Cell::number(4.0)),
            ("C3", Cell::formula("[@Qty]*2")),
            ("E1", Cell::formula("SUM(Sales[Amount])")),
        ]);
        wb.tables.push(crate::sheet::Table {
            name: "Sales".to_string(),
            sheet: 0,
            range: (0, 0, 2, 2),
            header_rows: 1,
            totals_rows: 0,
            columns: vec!["Item".into(), "Qty".into(), "Amount".into()],
            part: String::new(),
        });
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        // Calculated column evaluates (no circle from self-deps) and the
        // aggregation sees it in topological order.
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(6.0));
        assert_eq!(value_at(&wb, "C3"), CellValue::Number(8.0));
        assert_eq!(value_at(&wb, "E1"), CellValue::Number(14.0));
        // Editing a Qty propagates through the calculated column to the sum.
        set(&mut eng, &mut wb, "B2", Cell::number(10.0));
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(20.0));
        assert_eq!(value_at(&wb, "E1"), CellValue::Number(28.0));
    }

    #[test]
    fn three_d_spans_aggregate_and_propagate() {
        let mut wb = Workbook::default();
        for (i, name) in ["One", "Two", "Three", "Sum"].iter().enumerate() {
            let mut s = Sheet {
                name: name.to_string(),
                ..Sheet::default()
            };
            if i < 3 {
                s.set_cell(0, 0, Cell::number((i + 1) as f64 * 10.0));
            }
            wb.sheets.push(s);
        }
        wb.sheets[3].set_cell(0, 0, Cell::formula("SUM(One:Three!A1)"));
        wb.sheets[3].set_cell(1, 0, Cell::formula("COUNT(One:Three!A1:B2)"));
        wb.sheets[3].set_cell(2, 0, Cell::formula("AVERAGE(One:Three!A1)"));
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(
            wb.sheets[3].cell(0, 0).unwrap().value,
            CellValue::Number(60.0)
        );
        assert_eq!(
            wb.sheets[3].cell(1, 0).unwrap().value,
            CellValue::Number(3.0)
        );
        assert_eq!(
            wb.sheets[3].cell(2, 0).unwrap().value,
            CellValue::Number(20.0)
        );
        // Editing a middle sheet dirties the span's dependents.
        eng.set_cell(&mut wb, (1, 0, 0), Cell::number(100.0));
        assert_eq!(
            wb.sheets[3].cell(0, 0).unwrap().value,
            CellValue::Number(140.0)
        );
        // Scalar context rejects a 3D span.
        eng.set_cell(&mut wb, (3, 0, 1), Cell::formula("One:Three!A1*2"));
        assert_eq!(
            wb.sheets[3].cell(0, 1).unwrap().value,
            CellValue::Error("#VALUE!".into())
        );
    }

    #[test]
    fn iterative_calculation_converges() {
        // A1 = (A1+10)/2 → fixed point at 10. Without the opt-in: 0.
        let mut wb = wb_one_sheet(&[("A1", Cell::formula("(A1+10)/2"))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(0.0));
        // With iteration enabled it converges.
        let mut wb = wb_one_sheet(&[("A1", Cell::formula("(A1+10)/2"))]);
        wb.iterate = Some((100, 1e-9));
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        match value_at(&wb, "A1") {
            CellValue::Number(n) => assert!((n - 10.0).abs() < 1e-6, "{n}"),
            v => panic!("expected convergence, got {v:?}"),
        }
        // Mutual pair: A2 = B2+1, B2 = A2/2 → A2 = 2, B2 = 1.
        let mut wb = wb_one_sheet(&[("A2", Cell::formula("B2+1")), ("B2", Cell::formula("A2/2"))]);
        wb.iterate = Some((200, 1e-12));
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        match (value_at(&wb, "A2"), value_at(&wb, "B2")) {
            (CellValue::Number(a), CellValue::Number(b)) => {
                assert!((a - 2.0).abs() < 1e-6 && (b - 1.0).abs() < 1e-6, "{a} {b}");
            }
            v => panic!("expected numbers, got {v:?}"),
        }
    }

    #[test]
    fn rand_available_with_seed() {
        let mut wb = wb_one_sheet(&[("A1", Cell::formula("RAND()"))]);
        let mut eng = Engine::new(&wb);
        eng.seed = Some(12345);
        eng.recalc_all(&mut wb);
        match value_at(&wb, "A1") {
            CellValue::Number(n) => assert!((0.0..1.0).contains(&n)),
            v => panic!("RAND gave {v:?}"),
        }
    }

    #[test]
    fn rows_and_columns_of_randarray() {
        // #661: ROWS/COLUMNS of a computed (random) array.
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::formula("ROWS(RANDARRAY(3,2))")),
            ("A2", Cell::formula("COLUMNS(RANDARRAY(3,2))")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.seed = Some(7);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(3.0));
        assert_eq!(value_at(&wb, "A2"), CellValue::Number(2.0));
    }

    #[test]
    fn cell_format_and_isformula() {
        // #656: CELL("format") reads the cell's number format; ISFORMULA
        // whether it holds a formula.
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1234.0)),
            ("A2", Cell::number(0.5)),
            ("A3", Cell::number(45306.0)),
            ("A4", Cell::formula("A1*2")),
            ("B1", Cell::formula("CELL(\"format\",A1)")),
            ("B2", Cell::formula("CELL(\"format\",A2)")),
            ("B3", Cell::formula("CELL(\"format\",A3)")),
            ("B4", Cell::formula("CELL(\"format\",A5)")),
            ("C1", Cell::formula("ISFORMULA(A1)")),
            ("C4", Cell::formula("ISFORMULA(A4)")),
            ("C5", Cell::formula("ISFORMULA(5)")),
        ]);
        let xf = |code: &str| crate::sheet::Xf {
            code: Some(code.to_string()),
            ..Default::default()
        };
        if wb.styles.xfs.is_empty() {
            wb.styles.xfs.push(Default::default());
        }
        let s1 = wb.styles.intern(xf("$#,##0_);[Red]($#,##0)"));
        let s2 = wb.styles.intern(crate::sheet::Xf {
            numfmt: crate::sheet::NumFmt::Percent { decimals: 2 },
            ..Default::default()
        });
        let s3 = wb.styles.intern(xf("d-mmm-yy"));
        for (r, st) in [(0, s1), (1, s2), (2, s3)] {
            wb.sheets[0].cells.get_mut(&(r, 0)).unwrap().style = st;
        }
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        let t = |s: &str| CellValue::Text(s.into());
        assert_eq!(value_at(&wb, "B1"), t("C0-"));
        assert_eq!(value_at(&wb, "B2"), t("P2"));
        assert_eq!(value_at(&wb, "B3"), t("D1"));
        assert_eq!(value_at(&wb, "B4"), t("G"));
        assert_eq!(value_at(&wb, "C1"), CellValue::Bool(false));
        assert_eq!(value_at(&wb, "C4"), CellValue::Bool(true));
        assert_eq!(value_at(&wb, "C5"), CellValue::Error("#VALUE!".into()));
        // `_xlfn.ISFORMULA` from a file parses to the same function.
        let ast = formula::parse("_xlfn.ISFORMULA(A4)").unwrap();
        assert!(matches!(ast, Expr::Func(ref f, _) if f == "ISFORMULA"));
    }

    #[test]
    fn database_criteria_begin_with_and_compute() {
        // #677: the issue's table, all nine rows, plus the exact form.
        let t = |s: &str| Cell::text(s);
        let n = Cell::number;
        let f = Cell::formula;
        let mut cells = vec![
            ("F1", t("Rep")),
            ("F2", t("C")),
            ("P1", t("Rep")),
            ("P2", t("an")),
            ("Y1", t("Big")),
            ("Y2", f("C2>100")),
            ("AA2", f("AND(C2>=80,B2=\"East\")")),
            ("AB1", t("Over")),
            ("AB2", f("C2>$AD$1")),
            ("AD1", n(100.0)),
            ("AC1", t("Zone")),
            ("AC2", t("East")),
            ("H1", t("Rep")),
            ("H2", f("\"=an\"")),
            ("K1", f("DCOUNTA(A1:C8,\"Rep\",F1:F2)")),
            ("K2", f("DSUM(A1:C8,\"Amount\",F1:F2)")),
            ("K3", f("DCOUNTA(A1:C8,\"Rep\",P1:P2)")),
            ("K4", f("DCOUNTA(A1:C8,\"Rep\",Y1:Y2)")),
            ("K5", f("DSUM(A1:C8,\"Amount\",Y1:Y2)")),
            ("K6", f("DCOUNTA(A1:C8,\"Rep\",AA1:AA2)")),
            ("K7", f("DSUM(A1:C8,\"Amount\",AA1:AA2)")),
            ("K8", f("DCOUNTA(A1:C8,\"Rep\",AB1:AB2)")),
            ("K9", f("DCOUNTA(A1:C8,\"Rep\",AC1:AC2)")),
            ("K10", f("DCOUNTA(A1:C8,\"Rep\",H1:H2)")),
            // COUNTIF keeps exact text matching.
            ("K11", f("COUNTIF(A2:A8,\"C\")")),
        ];
        let reps = ["Rep", "Ann", "Bob", "Cara", "ann", "Dee", "Carl", "Eve"];
        let regions = ["Region", "East", "West", "East", "North", "West", "East"];
        let amounts = [120.0, 80.0, 200.0, 50.0, 300.0, 90.0, 60.0];
        let names: Vec<(String, Cell)> = reps
            .iter()
            .enumerate()
            .map(|(i, r)| (format!("A{}", i + 1), t(r)))
            .chain(
                regions
                    .iter()
                    .enumerate()
                    .map(|(i, r)| (format!("B{}", i + 1), t(r))),
            )
            .chain(std::iter::once(("C1".to_string(), t("Amount"))))
            .chain(
                amounts
                    .iter()
                    .enumerate()
                    .map(|(i, a)| (format!("C{}", i + 2), n(*a))),
            )
            .collect();
        for (k, c) in &names {
            cells.push((k.as_str(), c.clone()));
        }
        let mut wb = wb_one_sheet(&cells);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        let want = [2.0, 290.0, 2.0, 3.0, 620.0, 3.0, 410.0, 3.0, 0.0, 0.0, 0.0];
        for (i, w) in want.iter().enumerate() {
            let k = format!("K{}", i + 1);
            assert_eq!(value_at(&wb, &k), CellValue::Number(*w), "{k}");
        }
        // The computed criterion follows its absolute input.
        eng.set_cell(&mut wb, (0, 0, 29), n(250.0));
        assert_eq!(value_at(&wb, "K8"), CellValue::Number(1.0));
    }

    #[test]
    fn computed_criteria_that_reenter_themselves_terminate() {
        // #677: a computed criterion reading a D-function over its own
        // criteria range (directly, or through another criteria range) is a
        // circle; it matches nothing rather than recursing forever.
        let mut cells: Vec<(String, Cell)> = vec![
            ("A1".into(), Cell::text("N")),
            ("B1".into(), Cell::text("M")),
            ("C1".into(), Cell::text("V")),
        ];
        for r in 2..=8 {
            cells.push((format!("A{r}"), Cell::number(r as f64)));
            cells.push((format!("C{r}"), Cell::number(10.0 * r as f64)));
        }
        cells.push((
            "Y2".into(),
            Cell::formula("DCOUNT($A$1:$C$8,\"V\",$Y$1:$Y$2)>0"),
        ));
        cells.push(("K1".into(), Cell::formula("DCOUNT(A1:C8,\"V\",Y1:Y2)")));
        cells.push((
            "P2".into(),
            Cell::formula("DCOUNT($A$1:$C$8,\"V\",$Q$1:$Q$2)>0"),
        ));
        cells.push((
            "Q2".into(),
            Cell::formula("DCOUNT($A$1:$C$8,\"V\",$P$1:$P$2)>0"),
        ));
        cells.push(("K2".into(), Cell::formula("DCOUNT(A1:C8,\"V\",P1:P2)")));
        let refs: Vec<(&str, Cell)> = cells.iter().map(|(k, c)| (k.as_str(), c.clone())).collect();
        let mut wb = wb_one_sheet(&refs);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(0.0));
        assert_eq!(value_at(&wb, "K2"), CellValue::Number(0.0));
    }

    #[test]
    fn circles_are_known_before_any_recalc() {
        // #660: building the engine over an opened workbook finds its
        // circles without touching the cached values.
        let cached = |f: &str, v: f64| Cell {
            value: CellValue::Number(v),
            ..Cell::formula(f)
        };
        let wb = wb_one_sheet(&[
            ("E1", cached("E1+1", 3.0)),
            ("F1", cached("G1+1", 1.0)),
            ("G1", cached("F1*2", 2.0)),
            ("H1", cached("E1+5", 8.0)),
        ]);
        let eng = Engine::new(&wb);
        assert_eq!(eng.circular_refs(), vec![(0, 0, 4), (0, 0, 5), (0, 0, 6)]);
        assert_eq!(value_at(&wb, "E1"), CellValue::Number(3.0));
        assert!(
            Engine::new(&wb_one_sheet(&[("A1", Cell::formula("B1+1"))]))
                .circular_refs()
                .is_empty()
        );
    }

    #[test]
    fn computed_criteria_follow_cells_outside_the_database() {
        // #677: AA2 = E2>0 reads helper column E beside the database, one
        // row per record. D-functions are volatile, so editing any of E (or
        // a column the criterion is changed to read) updates the DSUM.
        let mut cells: Vec<(String, Cell)> = vec![
            ("A1".into(), Cell::text("Rep")),
            ("C1".into(), Cell::text("Amount")),
        ];
        for r in 2..=8 {
            cells.push((format!("A{r}"), Cell::text(&format!("R{r}"))));
            cells.push((format!("C{r}"), Cell::number(10.0 * r as f64)));
            cells.push((format!("E{r}"), Cell::number(0.0)));
        }
        cells.push(("E2".into(), Cell::number(1.0)));
        cells.push(("AA2".into(), Cell::formula("E2>0")));
        cells.push(("K1".into(), Cell::formula("DSUM(A1:C8,\"Amount\",AA1:AA2)")));
        let refs: Vec<(&str, Cell)> = cells.iter().map(|(k, c)| (k.as_str(), c.clone())).collect();
        let mut wb = wb_one_sheet(&refs);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(20.0));
        set(&mut eng, &mut wb, "E5", Cell::number(1.0));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(70.0));
        set(&mut eng, &mut wb, "E8", Cell::number(2.0));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(150.0));
        // The criteria formula changes to read another column: edits there
        // count too.
        for r in 2..=8 {
            set(&mut eng, &mut wb, &format!("F{r}"), Cell::number(0.0));
        }
        set(&mut eng, &mut wb, "AA2", Cell::formula("F2>0"));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(0.0));
        set(&mut eng, &mut wb, "F5", Cell::number(1.0));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(50.0));

        // Natural order: the DSUM first, then its criterion, then the edit.
        let mut wb = wb_one_sheet(&refs[..refs.len() - 2]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        set(
            &mut eng,
            &mut wb,
            "K1",
            Cell::formula("DSUM(A1:C8,\"Amount\",AA1:AA2)"),
        );
        set(&mut eng, &mut wb, "AA2", Cell::formula("E2>0"));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(20.0));
        set(&mut eng, &mut wb, "E5", Cell::number(1.0));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(70.0));
    }

    #[test]
    fn database_functions_wait_for_the_helpers_their_criteria_read() {
        // #677: helper formulas in E (E2:E8 = B*2) feed the computed
        // criterion AA2 = E2>100, with no edge from them to the D-functions.
        // Many DSUMs make an order that evaluated any of them before the
        // edited helper fail almost surely, whatever the HashMap order.
        let mut cells: Vec<(String, Cell)> = vec![
            ("A1".into(), Cell::text("Rep")),
            ("B1".into(), Cell::text("Units")),
            ("C1".into(), Cell::text("Amount")),
            ("AA2".into(), Cell::formula("E2>100")),
        ];
        for r in 2..=8 {
            cells.push((format!("A{r}"), Cell::text(&format!("R{r}"))));
            cells.push((format!("B{r}"), Cell::number(10.0)));
            cells.push((format!("C{r}"), Cell::number(r as f64)));
            cells.push((format!("E{r}"), Cell::formula(&format!("B{r}*2"))));
        }
        for k in 1..=30 {
            cells.push((
                format!("K{k}"),
                Cell::formula("DSUM(A1:C8,\"Amount\",AA1:AA2)"),
            ));
        }
        let refs: Vec<(&str, Cell)> = cells.iter().map(|(k, c)| (k.as_str(), c.clone())).collect();
        // recalc_all over helpers with no cached values.
        let mut wb = wb_one_sheet(&refs);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        for k in 1..=30 {
            assert_eq!(value_at(&wb, &format!("K{k}")), CellValue::Number(0.0));
        }
        // Edit helper inputs, one at a time.
        let mut want = 0.0;
        for r in [5u32, 2, 8, 3] {
            set(&mut eng, &mut wb, &format!("B{r}"), Cell::number(60.0));
            want += r as f64;
            for k in 1..=30 {
                assert_eq!(
                    value_at(&wb, &format!("K{k}")),
                    CellValue::Number(want),
                    "K{k} after B{r}"
                );
            }
        }
        let mut wb2 = wb.clone();
        let mut eng2 = Engine::new(&wb2);
        for r in 2..=8 {
            wb2.sheets[0].cells.get_mut(&(r - 1, 4)).unwrap().value = CellValue::Empty;
        }
        eng2.recalc_all(&mut wb2);
        assert_eq!(value_at(&wb2, "K30"), CellValue::Number(want));
    }

    /// The #677 helper layout: A1:C8 database (C = row number), E2:E8
    /// helpers, AA2 = E2>100 as a computed criterion, K1 = `k1`, plus
    /// `extra` cells.
    fn db_helper_book(k1: &str, e5: &str, extra: &[(&str, Cell)]) -> Workbook {
        let mut cells: Vec<(String, Cell)> = vec![
            ("A1".into(), Cell::text("Rep")),
            ("C1".into(), Cell::text("Amount")),
            ("AA2".into(), Cell::formula("E2>100")),
            ("K1".into(), Cell::formula(k1)),
        ];
        for r in 2..=8 {
            cells.push((format!("A{r}"), Cell::text(&format!("R{r}"))));
            cells.push((format!("C{r}"), Cell::number(r as f64)));
            cells.push((format!("E{r}"), Cell::number(0.0)));
        }
        cells.push(("E5".into(), Cell::formula(e5)));
        for (k, c) in extra {
            cells.push((k.to_string(), c.clone()));
        }
        let refs: Vec<(&str, Cell)> = cells.iter().map(|(k, c)| (k.as_str(), c.clone())).collect();
        wb_one_sheet(&refs)
    }

    #[test]
    fn database_functions_see_helpers_below_an_iterating_circle() {
        // #677/#660: E5 reads Z1, an iterating circle; the DSUM's computed
        // criterion reads E5. The circle settles after the DSUM first ran,
        // so the DSUM runs again.
        let dsum = "DSUM(A1:C8,\"Amount\",AA1:AA2)";
        let mut wb = db_helper_book(
            dsum,
            "Z1*2",
            &[("Z1", Cell::formula("Z1/2+B1")), ("B1", Cell::number(30.0))],
        );
        wb.iterate = Some((100, 0.001));
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        // Z1 → 60, E5 → 120 > 100: record 5 matches.
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(5.0));
        set(&mut eng, &mut wb, "B1", Cell::number(10.0));
        // Z1 → 20, E5 → 40: nothing matches.
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(0.0));
        set(&mut eng, &mut wb, "B1", Cell::number(40.0));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(5.0));
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(5.0));
    }

    #[test]
    fn database_functions_see_helpers_below_a_new_circle() {
        // #677/#660: without iteration a new circle is 0, so a helper below
        // it drops out of the criterion and the DSUM follows.
        let dsum = "DSUM(A1:C8,\"Amount\",AA1:AA2)";
        let mut wb = db_helper_book(dsum, "Z1*2+B1", &[("Z1", Cell::number(60.0))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(5.0));
        // Z1 becomes a circle: 0, so E5 = B1 = 0.
        set(&mut eng, &mut wb, "Z1", Cell::formula("Z1+1"));
        assert_eq!(value_at(&wb, "E5"), CellValue::Number(0.0));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(0.0));
        // An input below the circle moves E5 above 100 again.
        set(&mut eng, &mut wb, "B1", Cell::number(150.0));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(5.0));
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(5.0));
    }

    #[test]
    fn database_functions_behind_a_defined_name_recalculate() {
        // #677: K1 = Total, a defined name holding the DSUM. It is found
        // behind the name, so it always recalculates and waits for helpers.
        let mut wb = db_helper_book("Total", "B5*2", &[("B5", Cell::number(10.0))]);
        wb.defined_names.push(crate::sheet::DefinedName {
            name: "Total".into(),
            scope: None,
            formula: "DSUM(Sheet1!$A$1:$C$8,\"Amount\",Sheet1!$AA$1:$AA$2)".into(),
        });
        wb.defined_names.push(crate::sheet::DefinedName {
            name: "Stamp".into(),
            scope: None,
            formula: "NOW()".into(),
        });
        wb.sheets[0].set_cell(0, 20, Cell::formula("Stamp+1"));
        let mut eng = Engine::new(&wb);
        assert!(eng.formulas[&(0, 0, 10)].volatile && eng.formulas[&(0, 0, 10)].db);
        // NOW() behind a name is volatile too.
        assert!(eng.formulas[&(0, 0, 20)].volatile && !eng.formulas[&(0, 0, 20)].db);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(0.0));
        set(&mut eng, &mut wb, "B5", Cell::number(60.0));
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(5.0));
    }

    #[test]
    fn database_functions_below_their_inputs_are_not_circles() {
        // #677: a D-function under its criteria's inputs (H3 below H1, or a
        // total at C10 under an AVERAGE($C$2:$C$8) criterion) is no circle.
        let amounts = [120.0, 80.0, 200.0, 50.0, 300.0, 90.0, 60.0];
        let mut cells: Vec<(String, Cell)> = vec![
            ("A1".into(), Cell::text("Rep")),
            ("C1".into(), Cell::text("Amount")),
            ("F1".into(), Cell::text("Amount")),
            ("F2".into(), Cell::formula("\">\"&H1")),
            ("H1".into(), Cell::number(100.0)),
            ("H3".into(), Cell::formula("DSUM(A1:C8,\"Amount\",F1:F2)")),
            ("AB1".into(), Cell::text("Over")),
            ("AB2".into(), Cell::formula("C2>AVERAGE($C$2:$C$8)")),
            (
                "C10".into(),
                Cell::formula("DSUM(A1:C8,\"Amount\",AB1:AB2)"),
            ),
        ];
        for (i, a) in amounts.iter().enumerate() {
            cells.push((format!("A{}", i + 2), Cell::text(&format!("R{i}"))));
            cells.push((format!("C{}", i + 2), Cell::number(*a)));
        }
        let refs: Vec<(&str, Cell)> = cells.iter().map(|(k, c)| (k.as_str(), c.clone())).collect();
        let mut wb = wb_one_sheet(&refs);
        let mut eng = Engine::new(&wb);
        assert!(eng.circular_refs().is_empty());
        eng.recalc_all(&mut wb);
        assert!(eng.circular_refs().is_empty());
        assert_eq!(value_at(&wb, "H3"), CellValue::Number(620.0));
        assert_eq!(value_at(&wb, "C10"), CellValue::Number(500.0));
        set(&mut eng, &mut wb, "H1", Cell::number(250.0));
        assert_eq!(value_at(&wb, "H3"), CellValue::Number(300.0));
        assert!(eng.circular_refs().is_empty());
    }

    // ---- dynamic arrays / spilling ------------------------------------

    #[test]
    fn sequence_spills_and_resizes() {
        let mut wb = wb_one_sheet(&[
            ("A1", array_formula("SEQUENCE(3)")),
            ("C1", Cell::formula("SUM(A1#)")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(1.0));
        assert_eq!(value_at(&wb, "A2"), CellValue::Number(2.0));
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(3.0));
        assert_eq!(wb.sheets[0].cell(0, 0).unwrap().spill, Some((3, 1)));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(6.0));
        // Growing the spill updates both the grid and the A1# dependent.
        set(
            &mut eng,
            &mut wb,
            "A1",
            Cell::formula("SEQUENCE(4,1,10,10)"),
        );
        assert_eq!(value_at(&wb, "A4"), CellValue::Number(40.0));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(100.0));
        // Shrinking clears the cells that fell off the end.
        set(&mut eng, &mut wb, "A1", Cell::formula("SEQUENCE(2)"));
        assert_eq!(value_at(&wb, "A3"), CellValue::Empty);
        assert_eq!(value_at(&wb, "A4"), CellValue::Empty);
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(3.0));
    }

    #[test]
    fn blocked_spill_errors_and_recovers() {
        let mut wb = wb_one_sheet(&[
            ("A3", Cell::number(99.0)),
            ("A1", array_formula("SEQUENCE(3)")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Error("#SPILL!".into()));
        // The blocker keeps its value; nothing was overwritten.
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(99.0));
        assert_eq!(value_at(&wb, "A2"), CellValue::Empty);
        // Clearing the blockage lets the anchor spill on the next recalc.
        set(&mut eng, &mut wb, "A3", Cell::default());
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(1.0));
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(3.0));
    }

    #[test]
    fn typing_into_a_spill_breaks_it() {
        let mut wb = wb_one_sheet(&[("A1", array_formula("SEQUENCE(3)"))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A2"), CellValue::Number(2.0));
        // A value typed into a spilled cell wins; the anchor turns #SPILL!.
        set(&mut eng, &mut wb, "A2", Cell::number(7.0));
        assert_eq!(value_at(&wb, "A1"), CellValue::Error("#SPILL!".into()));
        assert_eq!(value_at(&wb, "A2"), CellValue::Number(7.0));
        assert_eq!(value_at(&wb, "A3"), CellValue::Empty);
        // Removing it heals the spill.
        set(&mut eng, &mut wb, "A2", Cell::default());
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(1.0));
        assert_eq!(value_at(&wb, "A2"), CellValue::Number(2.0));
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(3.0));
    }

    #[test]
    fn clearing_an_anchor_clears_its_spill() {
        let mut wb = wb_one_sheet(&[
            ("A1", array_formula("SEQUENCE(3)")),
            ("C1", Cell::formula("A2*10")), // direct dependent of a spill cell
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(20.0));
        set(&mut eng, &mut wb, "A1", Cell::default());
        assert_eq!(value_at(&wb, "A2"), CellValue::Empty);
        assert_eq!(value_at(&wb, "A3"), CellValue::Empty);
        // The dependent saw the cleared cell (empty*10 = 0).
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(0.0));
    }

    #[test]
    fn dependents_of_spilled_cells_update() {
        let mut wb = wb_one_sheet(&[
            ("A1", array_formula("SEQUENCE(3,1,10,10)")),
            ("C1", Cell::formula("A2+1")), // A2 is a spilled cell, not a formula
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(21.0));
        set(&mut eng, &mut wb, "A1", Cell::formula("SEQUENCE(3,1,5,5)"));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(11.0));
    }

    #[test]
    fn filter_spills_from_sheet_data() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(5.0)),
            ("A2", Cell::number(15.0)),
            ("A3", Cell::number(25.0)),
            ("A4", Cell::number(8.0)),
            ("C1", array_formula("FILTER(A1:A4,A1:A4>9)")),
            ("E1", Cell::formula("COUNT(C1#)")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(15.0));
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(25.0));
        assert_eq!(value_at(&wb, "E1"), CellValue::Number(2.0));
        // Data edit reshapes the filter result and its dependents.
        set(&mut eng, &mut wb, "A4", Cell::number(80.0));
        assert_eq!(value_at(&wb, "C3"), CellValue::Number(80.0));
        assert_eq!(value_at(&wb, "E1"), CellValue::Number(3.0));
    }

    #[test]
    fn user_entered_range_formula_spills() {
        // A user-entered (modern) range formula spills, like current Excel.
        let mut wb = wb_one_sheet(&[("A1", Cell::number(1.0)), ("A2", Cell::number(2.0))]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "C1", Cell::formula("A1:A2"));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(1.0));
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(2.0));
        assert_eq!(wb.sheets[0].cell(0, 2).unwrap().spill, Some((2, 1)));
    }

    #[test]
    fn loaded_legacy_range_formula_implicit_intersects() {
        // A plain formula loaded from a file (no `t="array"`) is legacy: a
        // range result reduces by implicit intersection to the formula's row,
        // matching pre-dynamic-array Excel — it does not spill.
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            // C2 references the whole column A1:A3; its own row (2) → A2.
            ("C2", Cell::formula("A1:A3")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(2.0));
        assert!(wb.sheets[0].cell(1, 2).unwrap().spill.is_none());
        // A legacy formula whose result is a *computed* array (not a bare range)
        // also reduces (top-left) rather than spilling — no #SPILL! cascade.
        let mut wb2 = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("C1", Cell::formula("A1:A3*10")),
            ("C2", Cell::text("blocker")), // would block a spill
        ]);
        let mut eng2 = Engine::new(&wb2);
        eng2.recalc_all(&mut wb2);
        assert_eq!(value_at(&wb2, "C1"), CellValue::Number(10.0)); // top-left, not #SPILL!
        assert!(wb2.sheets[0].cell(0, 2).unwrap().spill.is_none());
        // A row outside the range → #VALUE!.
        set(&mut eng, &mut wb, "A1", Cell::number(1.0)); // trigger a recalc
        let mut wb2 = wb_one_sheet(&[("A1", Cell::number(1.0)), ("E9", Cell::formula("A1:A3"))]);
        let mut eng2 = Engine::new(&wb2);
        eng2.recalc_all(&mut wb2);
        assert_eq!(value_at(&wb2, "E9"), CellValue::Error("#VALUE!".into()));
    }

    #[test]
    fn spill_ref_to_non_anchor_is_ref_error() {
        let mut wb = wb_one_sheet(&[("A1", Cell::number(3.0)), ("B1", Cell::formula("SUM(A1#)"))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "B1"), CellValue::Error("#REF!".into()));
    }

    #[test]
    fn spill_off_grid_is_blocked() {
        let last = crate::sheet::MAX_ROWS; // 1-based name of the last row
        let mut wb = wb_one_sheet(&[(format!("A{last}").as_str(), array_formula("SEQUENCE(2)"))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(
            value_at(&wb, &format!("A{last}")),
            CellValue::Error("#SPILL!".into())
        );
    }

    #[test]
    fn map_spills_and_named_lambda_tracks_deps() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("C1", array_formula("MAP(A1:A3,LAMBDA(x,x*10))")),
            ("E1", Cell::formula("SCALE(4)")), // named lambda: x * B1
            ("B1", Cell::number(100.0)),
        ]);
        wb.defined_names.push(crate::sheet::DefinedName {
            name: "SCALE".to_string(),
            scope: None,
            formula: "LAMBDA(x,x*Sheet1!$B$1)".to_string(),
        });
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        // MAP spilled.
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(20.0));
        assert_eq!(value_at(&wb, "C3"), CellValue::Number(30.0));
        // Named lambda computed through the workbook name.
        assert_eq!(value_at(&wb, "E1"), CellValue::Number(400.0));
        // Editing a cell referenced only inside the lambda body recalcs
        // the caller (dep tracking through called names).
        set(&mut eng, &mut wb, "B1", Cell::number(1000.0));
        assert_eq!(value_at(&wb, "E1"), CellValue::Number(4000.0));
        // Editing MAP's input reshapes its output.
        set(&mut eng, &mut wb, "A2", Cell::number(7.0));
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(70.0));
    }

    #[test]
    fn lifted_function_spills_in_workbook() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(-4.0)),
            ("A2", Cell::number(5.0)),
            ("C1", array_formula("ABS(A1:A2)")),
            ("D1", Cell::formula("SUM(C1#)")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(4.0));
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(5.0));
        assert_eq!(value_at(&wb, "D1"), CellValue::Number(9.0));
    }

    #[test]
    fn sumx_tracks_table_dependencies() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::text("Qty")),
            ("B1", Cell::text("Price")),
            ("A2", Cell::number(2.0)),
            ("B2", Cell::number(10.0)),
            ("A3", Cell::number(3.0)),
            ("B3", Cell::number(5.0)),
            ("D1", Cell::formula("SUMX(Sales,[@Qty]*[@Price])")),
        ]);
        wb.tables.push(crate::sheet::Table {
            name: "Sales".to_string(),
            sheet: 0,
            range: (0, 0, 2, 1),
            header_rows: 1,
            totals_rows: 0,
            columns: vec!["Qty".into(), "Price".into()],
            part: String::new(),
        });
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "D1"), CellValue::Number(35.0));
        // Editing a row inside the iterated table recalculates the SUMX.
        set(&mut eng, &mut wb, "B3", Cell::number(100.0));
        assert_eq!(value_at(&wb, "D1"), CellValue::Number(320.0));
    }
}
