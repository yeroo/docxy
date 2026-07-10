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

use crate::formula::{self, Eval, ExcelError, Expr, Resolver, Value, collect_refs, is_volatile};
use crate::sheet::{Cell, CellValue, Workbook};

/// (sheet index, row, col) — the engine's cell address.
pub type Key = (usize, u32, u32);

/// A reference rectangle a formula depends on: sheet + inclusive rect.
type Rect = (usize, u32, u32, u32, u32);

struct FormulaInfo {
    ast: Expr,
    volatile: bool,
    /// Dependency rects with sheet names resolved to indices. References to
    /// unknown sheets simply have no edge (they evaluate to `#REF!`).
    deps: Vec<Rect>,
}

#[derive(Default)]
pub struct Engine {
    formulas: HashMap<Key, FormulaInfo>,
    /// Cells whose formulas we must not re-evaluate (see module docs).
    unsupported: HashSet<Key>,
    /// Current moment as an Excel serial, supplied by the app (None = no
    /// clock → `TODAY`/`NOW` formulas stay on their cached values).
    pub clock: Option<f64>,
    /// PRNG state for `RAND`; None = no randomness source.
    pub seed: Option<u64>,
}

impl Engine {
    /// Parse all formulas in the workbook and build the dependency graph.
    pub fn new(wb: &Workbook) -> Engine {
        let mut eng = Engine::default();
        for (s, sheet) in wb.sheets.iter().enumerate() {
            for (&(r, c), cell) in &sheet.cells {
                if let Some(src) = &cell.formula {
                    eng.index_formula(wb, (s, r, c), src, cell.f_attrs.is_some());
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
                collect_deps(wb, key.0, &ast, &mut deps, 0);
                self.formulas.insert(
                    key,
                    FormulaInfo {
                        volatile: is_volatile(&ast),
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
        if let Some(src) = cell.formula.clone() {
            cell.f_attrs = None; // an edited formula is ours now
            self.index_formula(wb, key, &src, false);
        }
        if s < wb.sheets.len() {
            wb.sheets[s].set_cell(r, c, cell);
        }
        self.recalc_from(wb, &[key]);
    }

    /// Recalculate every formula in the workbook (headless `--recalc`, or
    /// after load when a full refresh is wanted).
    pub fn recalc_all(&mut self, wb: &mut Workbook) {
        let all: HashSet<Key> = self.formulas.keys().copied().collect();
        self.evaluate(wb, all);
    }

    /// Dirty the transitive dependents of `changed` (plus volatiles) and
    /// re-evaluate them.
    pub fn recalc_from(&mut self, wb: &mut Workbook, changed: &[Key]) {
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
        self.evaluate(wb, dirty);
    }

    /// Kahn's algorithm over the dirty subgraph, then evaluation in order.
    fn evaluate(&mut self, wb: &mut Workbook, dirty: HashSet<Key>) {
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
                if g == f {
                    continue;
                }
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
        while let Some(k) = queue.pop_front() {
            done.insert(k);
            self.eval_one(wb, k);
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

        // Whatever never reached in-degree 0 sits on a cycle.
        for &k in &dirty {
            if !done.contains(&k) {
                let (s, r, c) = k;
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

    fn eval_one(&mut self, wb: &mut Workbook, key: Key) {
        let info = match self.formulas.get(&key) {
            Some(i) => i,
            None => return,
        };
        let resolver = WbResolver {
            wb,
            clock: self.clock,
            rand_state: StdCell::new(self.seed.unwrap_or(0)),
            has_rand: self.seed.is_some(),
        };
        let mut ev = Eval::new(&resolver, key.0, (key.1, key.2));
        let value = ev.eval(&info.ast);
        let unsupported = ev.unsupported;
        if self.seed.is_some() {
            self.seed = Some(resolver.rand_state.get());
        }
        if unsupported {
            // Something beyond the engine: freeze this cell on its cached
            // value from here on.
            self.unsupported.insert(key);
            return;
        }
        let (s, r, c) = key;
        if let Some(sheet) = wb.sheets.get_mut(s) {
            let entry = sheet.cells.entry((r, c)).or_default();
            entry.value = value_to_cell(value);
        }
    }
}

/// Dependency rects of an AST, with sheet names resolved and defined names
/// expanded through the workbook (depth-capped against name→name loops).
fn collect_deps(wb: &Workbook, sheet: usize, ast: &Expr, out: &mut Vec<Rect>, depth: u32) {
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
    if depth >= 8 {
        return;
    }
    let mut names = Vec::new();
    formula::collect_names(ast, &mut names);
    for n in names {
        if let Some(def) = wb.defined_name(&n, sheet) {
            if let Ok(def_ast) = formula::parse(def) {
                collect_deps(wb, sheet, &def_ast, out, depth + 1);
            }
        }
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
                    formula: Some("SEQUENCE(A1,4)".to_string()),
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
}
