//! Dependency-graph recalculation over a [`Workbook`].
//!
//! The engine parses every formula once, extracts its reference rectangles,
//! and on each edit dirties only the transitive dependents — then evaluates
//! them in topological order (Kahn). Cells on a cycle get `#CYCLE!` instead
//! of hanging. Volatile formulas (`NOW`, `RAND`…) join every recalculation.
//!
//! **Graceful degradation:** a formula that fails to parse, carries preserved
//! `<f>` attributes (array/data-table), or evaluates through something we
//! don't model yet is marked *unsupported*: its cached value is kept, it is
//! never re-evaluated, and save writes it back byte-faithful. Dependents read
//! the cached value, so partial coverage yields stale-at-worst results,
//! never wrong-by-our-hand ones.

use std::cell::Cell as StdCell;
use std::collections::{HashMap, HashSet, VecDeque};

use crate::formula::{
    self, DynResult, Eval, ExcelError, Expr, Resolver, Value, collect_refs, is_volatile,
};
use crate::sheet::{Cell, CellValue, Sheet, Workbook};

/// (sheet index, row, col) — the engine's cell address.
pub type Key = (usize, u32, u32);

/// A reference rectangle a formula depends on: sheet + inclusive rect.
type Rect = (usize, u32, u32, u32, u32);

struct FormulaInfo {
    ast: Expr,
    volatile: bool,
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
                    // Array formulas (`t="array"`) are ours to evaluate — the
                    // dynamic-array engine recomputes their spill. Other
                    // preserved `<f>` attributes stay frozen.
                    let preserved = cell
                        .f_attrs
                        .as_deref()
                        .is_some_and(|a| !a.contains("t=\"array\""));
                    eng.index_formula(wb, (s, r, c), src, preserved);
                }
            }
        }
        eng
    }

    /// Is this cell's formula beyond the engine (kept on its cached value)?
    pub fn is_unsupported(&self, key: Key) -> bool {
        self.unsupported.contains(&key)
    }

    /// Parse and register one formula; preserved-`<f>` cells and parse
    /// failures are marked unsupported.
    fn index_formula(&mut self, wb: &Workbook, key: Key, src: &str, preserved: bool) {
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
                self.formulas.insert(
                    key,
                    FormulaInfo {
                        volatile: is_volatile(&ast),
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
            cell.f_attrs = None; // an edited formula is ours now
            self.index_formula(wb, key, &src, false);
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
        let mut dirty: HashSet<Key> = HashSet::new();
        // Sources whose dependents still need discovering. Edited cells seed
        // the walk whether or not they hold formulas.
        let mut frontier: VecDeque<Key> = changed.iter().copied().collect();

        for &k in changed {
            if self.formulas.contains_key(&k) {
                dirty.insert(k);
            }
        }
        for (&k, info) in &self.formulas {
            if info.volatile && dirty.insert(k) {
                frontier.push_back(k);
            }
        }
        // Blocked anchors retry every pass — a cleared blockage isn't
        // otherwise visible to the dependency walk.
        let blocked: Vec<Key> = self.spill_blocked.iter().copied().collect();
        for k in blocked {
            if self.formulas.contains_key(&k) && dirty.insert(k) {
                frontier.push_back(k);
            }
        }
        while let Some((s, r, c)) = frontier.pop_front() {
            for (&fk, info) in &self.formulas {
                if dirty.contains(&fk) {
                    continue;
                }
                let hit = info.deps.iter().any(|&(ds, r1, c1, r2, c2)| {
                    ds == s && r >= r1 && r <= r2 && c >= c1 && c <= c2
                });
                if hit {
                    dirty.insert(fk);
                    frontier.push_back(fk);
                }
            }
        }
        self.evaluate(wb, dirty, depth);
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
        let dirty_set: HashSet<Key> = dirty.iter().copied().collect();

        // in-degree of F = dirty formulas F depends on; edges G → dependents.
        let mut indeg: HashMap<Key, usize> = dirty.iter().map(|&k| (k, 0)).collect();
        let mut edges: HashMap<Key, Vec<Key>> = HashMap::new();
        for &f in &dirty {
            let info = &self.formulas[&f];
            for &g in &dirty_set {
                // Note g == f is NOT skipped: a formula whose own cell falls
                // inside its dependency rect is a self-reference, and must
                // land in the cycle remainder like any other circularity.
                let (gs, gr, gc) = g;
                let depends = info.deps.iter().any(|&(ds, r1, c1, r2, c2)| {
                    ds == gs && gr >= r1 && gr <= r2 && gc >= c1 && gc <= c2
                });
                if depends {
                    *indeg.get_mut(&f).unwrap() += 1;
                    edges.entry(g).or_default().push(f);
                }
            }
        }

        let mut queue: VecDeque<Key> = indeg
            .iter()
            .filter(|&(_, &d)| d == 0)
            .map(|(&k, _)| k)
            .collect();
        let mut done: HashSet<Key> = HashSet::new();
        let mut spilled: Vec<Key> = Vec::new();
        while let Some(k) = queue.pop_front() {
            done.insert(k);
            spilled.extend(self.eval_one(wb, k));
            if let Some(dependents) = edges.get(&k).cloned() {
                for d in dependents {
                    let e = indeg.get_mut(&d).unwrap();
                    *e -= 1;
                    if *e == 0 {
                        queue.push_back(d);
                    }
                }
            }
        }

        // Whatever never reached in-degree 0 sits on a cycle. With the
        // workbook's iterative-calculation opt-in, converge them the way
        // Excel does; otherwise flag the circularity honestly.
        let cycle: Vec<Key> = {
            let mut v: Vec<Key> = dirty
                .iter()
                .copied()
                .filter(|k| !done.contains(k))
                .collect();
            v.sort_unstable();
            v
        };
        if !cycle.is_empty() {
            match wb.iterate {
                Some((count, delta)) => {
                    for _ in 0..count.max(1) {
                        let mut max_change = 0.0f64;
                        for &k in &cycle {
                            let before = wb.sheets[k.0]
                                .cell(k.1, k.2)
                                .map(|c| c.value.clone())
                                .unwrap_or_default();
                            spilled.extend(self.eval_one(wb, k));
                            let after = wb.sheets[k.0]
                                .cell(k.1, k.2)
                                .map(|c| c.value.clone())
                                .unwrap_or_default();
                            if let (CellValue::Number(x), CellValue::Number(y)) = (&before, &after)
                            {
                                max_change = max_change.max((x - y).abs());
                            } else if before != after {
                                max_change = f64::MAX;
                            }
                        }
                        if max_change < delta {
                            break;
                        }
                    }
                }
                None => {
                    for &(s, r, c) in &cycle {
                        if let Some(cell) = wb
                            .sheets
                            .get_mut(s)
                            .and_then(|sh| sh.cells.get_mut(&(r, c)))
                        {
                            cell.value = CellValue::Error(ExcelError::Cycle.code().to_string());
                        }
                    }
                }
            }
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

    /// Evaluate one formula and store its result. Returns the keys of cells
    /// beyond the anchor whose stored values changed (spill writes/clears).
    fn eval_one(&mut self, wb: &mut Workbook, key: Key) -> Vec<Key> {
        let info = match self.formulas.get(&key) {
            Some(i) => i,
            None => return Vec::new(),
        };
        let resolver = WbResolver {
            wb,
            clock: self.clock,
            rand_state: StdCell::new(self.seed.unwrap_or(0)),
            has_rand: self.seed.is_some(),
        };
        let mut ev = Eval::new(&resolver, key.0, (key.1, key.2));
        let result = ev.eval_dynamic(&info.ast);
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
    fn cycles_get_cycle_error() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::formula("B1+1")),
            ("B1", Cell::formula("A1+1")),
            ("C1", Cell::number(5.0)),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Error("#CYCLE!".into()));
        assert_eq!(value_at(&wb, "B1"), CellValue::Error("#CYCLE!".into()));
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
        // Calculated column evaluates (no #CYCLE! from self-deps) and the
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
        // A1 = (A1+10)/2 → fixed point at 10. Without the opt-in: #CYCLE!.
        let mut wb = wb_one_sheet(&[("A1", Cell::formula("(A1+10)/2"))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A1"), CellValue::Error("#CYCLE!".into()));
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

    // ---- dynamic arrays / spilling ------------------------------------

    #[test]
    fn sequence_spills_and_resizes() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::formula("SEQUENCE(3)")),
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
            ("A1", Cell::formula("SEQUENCE(3)")),
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
        let mut wb = wb_one_sheet(&[("A1", Cell::formula("SEQUENCE(3)"))]);
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
            ("A1", Cell::formula("SEQUENCE(3)")),
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
            ("A1", Cell::formula("SEQUENCE(3,1,10,10)")),
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
            ("C1", Cell::formula("FILTER(A1:A4,A1:A4>9)")),
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
    fn plain_range_formula_spills() {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("C1", Cell::formula("A1:A2")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(1.0));
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(2.0));
        assert_eq!(wb.sheets[0].cell(0, 2).unwrap().spill, Some((2, 1)));
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
        let mut wb = wb_one_sheet(&[(format!("A{last}").as_str(), Cell::formula("SEQUENCE(2)"))]);
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
            ("C1", Cell::formula("MAP(A1:A3,LAMBDA(x,x*10))")),
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
            ("C1", Cell::formula("ABS(A1:A2)")),
            ("D1", Cell::formula("SUM(C1#)")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(4.0));
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(5.0));
        assert_eq!(value_at(&wb, "D1"), CellValue::Number(9.0));
    }
}
