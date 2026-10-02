//! Dependency-graph recalculation over a [`Workbook`].
//!
//! The engine parses every formula once, extracts its reference rectangles,
//! and on each edit dirties only the transitive dependents — then evaluates
//! them in topological order (Kahn). Cells on a circular reference are
//! handled as Excel does: without iterative calculation they are 0 and the
//! engine reports them ([`Engine::circular_refs`]); with it they iterate.
//! Volatile formulas (`NOW`, `RAND`…) join every recalculation.
//!
//! Array formulas (`t="array"`) are evaluated: a dynamic array spills, and a
//! legacy Ctrl+Shift+Enter block fills its fixed `ref` ([`Engine::fill_cse`]).
//!
//! **Graceful degradation:** a formula that fails to parse, carries other
//! preserved `<f>` attributes (data tables, unparseable shared groups), or
//! evaluates through something we don't model yet is marked *unsupported*:
//! its cached value is kept, it is never re-evaluated, and save writes it
//! back byte-faithful. Dependents read
//! the cached value, so partial coverage yields stale-at-worst results,
//! never wrong-by-our-hand ones.

use std::cell::Cell as StdCell;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::formula::{
    self, DynResult, Eval, ExcelError, Expr, Resolver, Value, always_recalc, collect_refs,
    contains_db_fn,
};
use crate::sheet::{
    Cell, CellMeta, CellValue, Sheet, Workbook, array_block, f_ref, is_array_f, own_array_ref,
};

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
    /// spilling. Array formulas and formulas typed here
    /// ([`crate::sheet::CellMeta::modern`]: new text through
    /// [`Engine::set_cell`], or a copy of one) are modern (spill).
    legacy: bool,
    /// Contains a spill reference (`A1#`) — its dep rects must be refreshed
    /// whenever spill extents may have changed.
    spillref: bool,
    /// A legacy Ctrl+Shift+Enter array block (`t="array"` with a `ref` that
    /// starts at this cell, no `cm`, not typed here): (rows, cols) of the
    /// fixed block its result fills. `None` for every other formula.
    cse: Option<(u32, u32)>,
    /// Dependency rects with sheet names resolved to indices. References to
    /// unknown sheets simply have no edge (they evaluate to `#REF!`).
    deps: Vec<Rect>,
}

#[derive(Default)]
pub struct Engine {
    formulas: HashMap<Key, FormulaInfo>,
    /// Cells whose formulas we must not re-evaluate (see module docs).
    unsupported: HashSet<Key>,
    /// Formulas evaluated without meeting anything unsupported, so known not
    /// to be frozen ([`Engine::is_frozen`] answers them without evaluating).
    supported: HashSet<Key>,
    /// Anchors whose array result cannot be written — dynamic arrays showing
    /// `#SPILL!`, blocked legacy CSE blocks — retried on every recalculation
    /// so they recover the moment the blockage clears.
    spill_blocked: HashSet<Key>,
    /// Current moment as an Excel serial, supplied by the app (None = no
    /// clock → `TODAY`/`NOW` formulas stay on their cached values).
    pub clock: Option<f64>,
    /// PRNG state for `RAND`; None = no randomness source.
    pub seed: Option<u64>,
    /// Formula cells on a circular reference, sorted.
    circular: BTreeSet<Key>,
    /// The post-circle D-function rerun already ran in this top-level
    /// recalculation (see [`Engine::evaluate`]); it runs at most once.
    db_rerun_done: bool,
    /// Per sheet, for the current top-level recalculation only: each row →
    /// the anchors whose spill covered it at some point in the pass
    /// ([`Engine::foreign_spills`]). Built on first use, extended by every
    /// spill the pass writes, never pruned: a superset, re-read before use.
    pass_anchors: HashMap<usize, RowCover>,
}

/// One sheet's spill anchors by the rows their extents cover
/// ([`Engine::pass_anchors`]).
#[derive(Default)]
struct RowCover {
    /// Row → the anchors whose extent covers (or covered) that row.
    rows: HashMap<u32, Vec<(u32, u32)>>,
    /// Anchor → the rows from its own already in `rows`, so noting a spill
    /// again adds only rows beyond them.
    noted: HashMap<(u32, u32), u32>,
}

impl RowCover {
    /// Put the anchor at `(r, c)` in every row of an `h`-row extent from it
    /// that it isn't in yet.
    fn note(&mut self, (r, c): (u32, u32), h: u32) {
        let done = self.noted.entry((r, c)).or_insert(0);
        for rr in r + *done..r + h {
            self.rows.entry(rr).or_default().push((r, c));
        }
        *done = (*done).max(h);
    }
}

/// Spill chains (an anchor whose array feeds another anchor's spill cells)
/// resolve through repeated post-passes; this bounds pathological loops.
const MAX_SPILL_PASSES: u32 = 8;

/// Excel's message for an edit refused because it would change part of a
/// legacy CSE array ([`Engine::refuses`]).
pub const PART_OF_ARRAY: &str = "You can't change part of an array.";

impl Engine {
    /// Parse all formulas in the workbook and build the dependency graph.
    pub fn new(wb: &Workbook) -> Engine {
        let mut eng = Engine::default();
        for (s, sheet) in wb.sheets.iter().enumerate() {
            for (&(r, c), cell) in &sheet.cells {
                eng.index_cell(wb, (s, r, c), cell);
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

    /// Is this cell's formula kept on its cached value — beyond the engine,
    /// whether or not it has been evaluated yet (xlsxy opens a workbook on its
    /// cached values)? A formula not yet known to be is evaluated, without
    /// storing anything, to find out. Hosts ask this of a spill anchor before
    /// treating its cells as spill output that a re-spill re-creates: a frozen
    /// anchor never re-spills.
    pub fn is_frozen(&self, wb: &Workbook, key: Key) -> bool {
        if self.unsupported.contains(&key) {
            return true;
        }
        if self.supported.contains(&key) {
            return false;
        }
        self.evaluates_unsupported(wb, key).unwrap_or(false)
    }

    /// [`Engine::is_frozen`] as the cell's inputs stand now: a formula of
    /// ours not known to be supported is evaluated afresh, whatever an
    /// earlier evaluation met (an
    /// input that made it unsupported, `INDIRECT` asked for R1C1 say, may
    /// have changed since). One kept on its cached value for what it is (a
    /// preserved `<f>`, a parse failure) stays so.
    fn is_frozen_now(&self, wb: &Workbook, key: Key) -> bool {
        // Kept current: evaluation marks it, and an edit to the cell clears
        // it. Only `unsupported` is sticky.
        if self.supported.contains(&key) {
            return false;
        }
        self.evaluates_unsupported(wb, key)
            .unwrap_or_else(|| self.unsupported.contains(&key))
    }

    /// Does evaluating this cell's formula meet something beyond the engine?
    /// Nothing is stored. `None` for a cell with no formula of ours.
    fn evaluates_unsupported(&self, wb: &Workbook, key: Key) -> Option<bool> {
        let info = self.formulas.get(&key)?;
        let resolver = WbResolver {
            wb,
            clock: self.clock,
            rand_state: StdCell::new(self.seed.unwrap_or(0)),
            has_rand: self.seed.is_some(),
        };
        #[cfg(test)]
        tests::FROZEN_EVALS.with(|n| n.set(n.get() + 1));
        let mut ev = Eval::new(&resolver, key.0, (key.1, key.2));
        let _ = ev.eval_dynamic_shaped(&info.ast, !info.legacy);
        Some(ev.unsupported)
    }

    /// Register a cell's formula (if any) as it stands, the way a freshly
    /// built engine sees it.
    fn index_cell(&mut self, wb: &Workbook, key: Key, cell: &Cell) {
        if let Some(src) = &cell.formula {
            // Array formulas (`t="array"`, or a dynamic array after an edit
            // dropped `f_attrs`) are ours to evaluate: a dynamic array's spill
            // is recomputed, a legacy CSE block ([`cse_block`]) is refilled.
            // Other preserved `<f>` attributes stay frozen.
            let is_array = cell.is_array_formula();
            let preserved = cell.f_attrs.as_deref().is_some_and(|a| !is_array_f(a));
            // A plain loaded formula is legacy (implicit intersection); an
            // array one, or one typed here (`modern`), is evaluated as an
            // array: it spills, or fills its fixed block when it is a CSE one.
            let legacy = !(is_array || cell.is_modern());
            let cse = cse_block(cell, key.1, key.2);
            self.index_formula(wb, key, src, preserved, legacy, cse);
        }
    }

    /// Parse and register one formula; preserved-`<f>` cells and parse
    /// failures are marked unsupported.
    fn index_formula(
        &mut self,
        wb: &Workbook,
        key: Key,
        src: &str,
        preserved: bool,
        legacy: bool,
        cse: Option<(u32, u32)>,
    ) {
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
                        cse,
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
    ///
    /// A formula whose text differs from the one in the cell is typed: it is
    /// ours now, modern (it spills), and its preserved `<f>` attributes go. The
    /// same text again (re-committing the editor unchanged builds a fresh
    /// cell; a pasted clone) keeps what the formula was — a loaded
    /// legacy formula stays legacy, a CSE array keeps its `<f>` attributes, a
    /// dynamic array stays one. A restyle is not an edit: it goes through
    /// [`Engine::set_styles`], which leaves a spill whole. Nor is the same
    /// text again on a formula the engine can't evaluate ([`Engine::is_frozen`]):
    /// it would never recompute the cached value, or re-spill the cached
    /// block, that a re-entry clears, so only its style is taken.
    ///
    /// Any plain value or blank written into a non-anchor cell of an
    /// evaluated legacy CSE block's `ref` is refused, whatever the cell holds,
    /// as in Excel ([`PART_OF_ARRAY`]): nothing changes, style included, and
    /// this returns false. A write over a formula already in the block is not
    /// refused (it frees the block). True when the edit was applied.
    pub fn set_cell(&mut self, wb: &mut Workbook, key: Key, cell: Cell) -> bool {
        self.type_cell(wb, key, cell, true, true)
    }

    /// [`Engine::set_cell`]; `recommit` takes the same text on a frozen
    /// formula as a restyle. [`Engine::paste_block`] types a pasted anchor
    /// without it: the copied block replaces this cell's, whatever its text.
    /// `refuse` is [`Engine::put_cell`]'s: off for a cell of a group already
    /// checked whole ([`Engine::set_cells_prechecked`]).
    fn type_cell(
        &mut self,
        wb: &mut Workbook,
        key: Key,
        mut cell: Cell,
        recommit: bool,
        refuse: bool,
    ) -> bool {
        let (s, r, c) = key;
        // A pasted anchor's extent is its source's, not this cell's:
        // [`Engine::put_cell`] drops it, and evaluation works out the spill
        // afresh.
        let prev = wb.sheets.get(s).and_then(|sh| sh.cell(r, c));
        let same = prev.filter(|p| cell.formula.is_some() && p.formula == cell.formula);
        if recommit && same.is_some() && self.is_frozen_now(wb, key) {
            self.set_styles(wb, s, &[(r, c, cell.style)]);
            return true;
        }
        match same {
            // What kind of formula it is comes from the cell's own previous
            // formula alone — not from the incoming cell, which may be a fresh
            // one (Enter) or a clone pasted from another address, or from
            // another workbook, whose `ref`/`si`/`cm`/`vm` are not this cell's.
            Some(p) => {
                cell.f_attrs = p.f_attrs.clone();
                own_array_ref(&mut cell, r, c);
                let pm = p.meta.as_deref().cloned().unwrap_or_default();
                let kind_of = |m: &CellMeta| {
                    let file = (m.cm.clone(), m.vm.clone(), m.vm_body.clone());
                    (file, m.modern, m.dynamic)
                };
                if cell.meta.as_deref().map(kind_of).unwrap_or_default() != kind_of(&pm) {
                    let m = cell.meta.get_or_insert_default();
                    ((m.cm, m.vm, m.vm_body), m.modern, m.dynamic) = kind_of(&pm);
                }
            }
            None if cell.formula.is_some() => {
                cell.f_attrs = None;
                let m = cell.meta.get_or_insert_default();
                m.modern = true;
                forget_file_meta(m);
                // One that could return an array is a dynamic array from the
                // start, even while its result is one value (FILTER with no
                // match yet): it saves with a `cm`, or it would reopen as a
                // legacy formula. Evaluation marks the ones this misses.
                let may_array = cell
                    .formula
                    .as_deref()
                    .and_then(|f| formula::parse(f).ok())
                    .is_some_and(|ast| formula::may_return_array(&ast));
                let m = cell.meta.get_or_insert_default();
                m.dynamic |= may_array;
            }
            None => {}
        }
        self.put_cell(wb, key, cell, refuse)
    }

    /// Apply a group of edits to sheet `s` (a paste, a fill, a replace-all)
    /// as [`Engine::set_cell`] types each cell, in order, except that the blanks landing
    /// in a frozen anchor's block go last. Such a blank is a no-op while the
    /// block is whole (`put_cell`), but content the group puts into the same
    /// block breaks it, and then the blank clears its cell like any other:
    /// applied last, it does so whatever order the group came in.
    ///
    /// The group is refused whole when it would change part of an evaluated
    /// legacy CSE block ([`Engine::refuses`]), as Excel refuses it: nothing
    /// is applied and this returns false. A group that also replaces such a
    /// block's anchor is not refused; the anchor goes first, so the rest of
    /// the group lands in cells the block no longer owns.
    pub fn set_cells(
        &mut self,
        wb: &mut Workbook,
        s: usize,
        changes: Vec<(u32, u32, Cell)>,
    ) -> bool {
        if self.refuses(wb, s, &changes) {
            return false;
        }
        self.set_cells_prechecked(wb, s, changes);
        true
    }

    /// [`Engine::set_cells`] for a group the caller has already asked
    /// [`Engine::refuses`] (or [`Engine::refuses_paste`]) about, whole: a
    /// host that writes one group in parts (a cut's clears, then its paste)
    /// decides once, before the first part. Nothing is decided again cell by
    /// cell, against a sheet the group's earlier cells have recalculated.
    pub fn set_cells_prechecked(&mut self, wb: &mut Workbook, s: usize, changes: CellEdits) {
        let changes = self.cse_anchors_first(wb, s, changes);
        let (now, later) = self.split_frozen_blanks(wb, s, changes);
        for (r, c, cell) in now.into_iter().chain(later) {
            self.type_cell(wb, (s, r, c), cell, true, false);
        }
    }

    /// `changes` with those landing on an evaluated legacy CSE anchor moved
    /// to the front, each part in order. Replaced first, the block lets go
    /// of its cells; a value written into the block before it would be
    /// refilled by the block and then cleared with it.
    fn cse_anchors_first(&self, wb: &Workbook, s: usize, changes: CellEdits) -> CellEdits {
        let blocks = self.cse_blocks(wb, s);
        if blocks.is_empty() {
            return changes;
        }
        let (mut first, rest): (Vec<_>, Vec<_>) = changes.into_iter().partition(|(r, c, _)| {
            blocks.iter().any(|b| (b.0, b.1) == (*r, *c)) && !self.is_frozen(wb, (s, *r, *c))
        });
        first.extend(rest);
        first
    }

    /// Does `key` hold the anchor of a legacy CSE block ([`cse_block`]) the
    /// engine evaluates (not [`Engine::is_frozen`])?
    fn is_live_cse(&self, wb: &Workbook, key: Key) -> bool {
        let (s, r, c) = key;
        wb.sheets
            .get(s)
            .and_then(|sh| sh.cell(r, c))
            .is_some_and(|cl| cse_block(cl, r, c).is_some())
            && !self.is_frozen(wb, key)
    }

    /// The legacy CSE blocks ([`cse_block`]) on sheet `s` whose formulas the
    /// engine indexes, as `(anchor row, anchor col, rows, cols)`: found once
    /// per group, from the formulas rather than every cell, so a sheet with
    /// none pays nothing per edited cell. One the engine can't evaluate is
    /// among them; callers ask [`Engine::is_frozen`] of a block an edit
    /// touches.
    fn cse_blocks(&self, wb: &Workbook, s: usize) -> Vec<(u32, u32, u32, u32)> {
        let Some(sheet) = wb.sheets.get(s) else {
            return Vec::new();
        };
        self.formulas
            .keys()
            .filter(|k| k.0 == s)
            .filter_map(|&(_, r, c)| {
                let (h, w) = cse_block(sheet.cell(r, c)?, r, c)?;
                Some((r, c, h, w))
            })
            .collect()
    }

    /// Would applying `changes` to sheet `s` as one group ([`Engine::set_cells`])
    /// change part of an evaluated legacy CSE block? Excel refuses that
    /// ([`PART_OF_ARRAY`]): any plain value or blank written into a cell of
    /// the block's `ref` other than its anchor, whatever the cell holds now,
    /// unless the group also replaces the anchor (with a plain cell, or a
    /// formula whose text differs from the anchor's; the same text again
    /// keeps it a block). A formula written into the block is not refused,
    /// nor is a write over a formula already there: the formula blocks the
    /// block, and replacing it frees it. A frozen block
    /// ([`Engine::is_frozen`]) keeps its own rules. Hosts that write one
    /// group in parts ask this, or [`Engine::refuses_paste`], once for the
    /// whole of it before the first part.
    pub fn refuses(&self, wb: &Workbook, s: usize, changes: &[(u32, u32, Cell)]) -> bool {
        let changes: Vec<(u32, u32, &Cell)> =
            changes.iter().map(|(r, c, cl)| (*r, *c, cl)).collect();
        self.refused(wb, s, &changes, &HashSet::new())
    }

    /// [`Engine::refuses`] for a paste of `block` at `at`
    /// ([`Engine::paste_block`]), written as one group with `also` (a cut's
    /// clears on the same sheet). A pasted cell on a block's anchor replaces
    /// it as a typed one does, and so does an array block `paste_block`
    /// restores in place there with its own `ref` ([`paste_anchors`]); a
    /// copied formula with the anchor's own text, typed there, keeps the
    /// block.
    pub fn refuses_paste(
        &self,
        wb: &Workbook,
        s: usize,
        at: (u32, u32),
        block: &[Vec<Cell>],
        also: &[(u32, u32, Cell)],
    ) -> bool {
        let (br, bc) = at;
        let mut changes: Vec<(u32, u32, &Cell)> =
            also.iter().map(|(r, c, cl)| (*r, *c, cl)).collect();
        for (dr, row) in block.iter().enumerate() {
            for (dc, cell) in row.iter().enumerate() {
                changes.push((br + dr as u32, bc + dc as u32, cell));
            }
        }
        let restored: HashSet<(u32, u32)> = paste_anchors(block, at)
            .into_iter()
            .filter(|a| a.3)
            .map(|a| (a.0, a.1))
            .collect();
        self.refused(wb, s, &changes, &restored)
    }

    /// Would a fill that writes every cell of `dest` on sheet `s` (a drag
    /// fill's destination: `autofill` writes the cells itself) change part
    /// of an evaluated legacy CSE block? Any non-anchor cell of a block's
    /// `ref` inside `dest` counts, unless `dest` holds the block's whole
    /// `ref`: `autofill` writes straight into the sheet and never clears an
    /// old block's cells outside `dest`, so only a fill over all of it
    /// replaces it. The fill's source is not written, so an anchor there is
    /// not replaced.
    pub fn refuses_area(&self, wb: &Workbook, s: usize, dest: (u32, u32, u32, u32)) -> bool {
        let (r1, c1, r2, c2) = dest;
        let inside = |r: u32, c: u32| (r1..=r2).contains(&r) && (c1..=c2).contains(&c);
        self.cse_blocks(wb, s).into_iter().any(|(ar, ac, h, w)| {
            let (er, ec) = (ar + h - 1, ac + w - 1);
            // Some cell of the block but its anchor lies in the area.
            let takes_part = (r1.max(ar)..=r2.min(er))
                .any(|r| (c1.max(ac)..=c2.min(ec)).any(|c| (r, c) != (ar, ac)));
            let whole = inside(ar, ac) && inside(er, ec);
            takes_part && !whole && !self.is_frozen(wb, (s, ar, ac))
        })
    }

    /// [`Engine::refuses`]; an anchor in `restored` is replaced by the paste
    /// whatever lands there.
    fn refused(
        &self,
        wb: &Workbook,
        s: usize,
        changes: &[(u32, u32, &Cell)],
        restored: &HashSet<(u32, u32)>,
    ) -> bool {
        let blocks = self.cse_blocks(wb, s);
        if blocks.is_empty() {
            return false;
        }
        let sheet = &wb.sheets[s];
        let at: HashMap<(u32, u32), &Cell> =
            changes.iter().map(|&(r, c, cl)| ((r, c), cl)).collect();
        let frees = |a: (u32, u32)| {
            restored.contains(&a)
                || at.get(&a).is_some_and(|new| {
                    new.formula.is_none()
                        || sheet
                            .cell(a.0, a.1)
                            .is_none_or(|held| held.formula != new.formula)
                })
        };
        let mut frozen: HashMap<(u32, u32), bool> = HashMap::new();
        changes.iter().any(|&(r, c, cell)| {
            if cell.formula.is_some() || sheet.cell(r, c).is_some_and(|h| h.formula.is_some()) {
                return false;
            }
            let Some(a) = block_over(&blocks, r, c) else {
                return false;
            };
            !frees(a)
                && !*frozen
                    .entry(a)
                    .or_insert_with(|| self.is_frozen(wb, (s, a.0, a.1)))
        })
    }

    /// Restyle cells on sheet `sheet`: `(row, col, style)` sets only each
    /// cell's style index. Value, formula, spill and metadata are untouched,
    /// so restyling a spilled cell leaves the spill whole (an edit through
    /// [`Engine::set_cell`] would break it). A blank cell restyled to the
    /// default style is dropped, as [`Sheet::set_cell`] does. One recalculation
    /// follows, so formulas that read a style (`CELL("format")`) see it.
    pub fn set_styles(&mut self, wb: &mut Workbook, sheet: usize, styles: &[(u32, u32, u32)]) {
        let Some(sh) = wb.sheets.get_mut(sheet) else {
            return;
        };
        for &(r, c, style) in styles {
            match sh.cells.get_mut(&(r, c)) {
                Some(cell) => {
                    cell.style = style;
                    if cell.is_blank() && style == 0 {
                        sh.cells.remove(&(r, c));
                    }
                }
                None if style != 0 => {
                    sh.cells.insert(
                        (r, c),
                        Cell {
                            style,
                            ..Cell::default()
                        },
                    );
                }
                None => {}
            }
        }
        let keys: Vec<Key> = styles.iter().map(|&(r, c, _)| (sheet, r, c)).collect();
        self.recalc_from(wb, &keys);
    }

    /// Put one cell back exactly as it was: its `<f>` attributes and metadata
    /// stay, and its formula is indexed as [`Engine::new`] would. Restoring a
    /// snapshot is not typing. An undo or redo of a group goes through
    /// [`Engine::restore_cells`], which puts each cell back with this.
    ///
    /// A snapshot is never refused as part of a CSE block
    /// ([`Engine::set_cell`]): it puts back a state the sheet was in. A plain
    /// value restored into an evaluated block that stays is refilled by it.
    pub fn restore_cell(&mut self, wb: &mut Workbook, key: Key, cell: Cell) {
        self.put_cell(wb, key, cell, false);
    }

    /// Put a group of cells on sheet `s` back exactly as they were (an undo or
    /// redo of one edit), whatever order they come in. The cells must be one
    /// snapshot, all taken at the same moment: then a value inside a spill
    /// anchor's extent is that anchor's spilled value. Those values are
    /// written as styled blanks first and the anchors last, so each anchor's
    /// recalc refills its spill; restored one by one, a spilled value landing
    /// after its anchor would block it (`#SPILL!`). An anchor the engine can't
    /// evaluate keeps its snapshot values instead. A cell landing on an
    /// evaluated legacy CSE anchor goes first, so the block lets go of its
    /// cells before the snapshot's values for them land (an undo hands its
    /// group over in reverse, members before their anchor). Never refused
    /// ([`Engine::restore_cell`]).
    pub fn restore_cells(&mut self, wb: &mut Workbook, s: usize, cells: &[(u32, u32, Cell)]) {
        let anchors: Vec<(u32, u32, &Cell)> = cells
            .iter()
            .filter(|(_, _, cl)| cl.formula.is_some() && cl.spill.is_some())
            .map(|(r, c, cl)| (*r, *c, cl))
            .collect();
        let mut members = Vec::new();
        let mut plain = Vec::new();
        for &(r, c, ref cell) in cells {
            if cell.formula.is_some() && cell.spill.is_some() {
                continue;
            }
            if cell.formula.is_none() && in_extent(&anchors, r, c) {
                members.push((r, c, cell));
                plain.push((r, c, cell.blank_like()));
            } else {
                plain.push((r, c, cell.clone()));
            }
        }
        // As in [`Engine::set_cells`]: a replaced CSE anchor first, and
        // blanks landing in a frozen block after the rest, so the redo of a
        // group that put a blank and a value into one frozen block ends as
        // the group did.
        let plain = self.cse_anchors_first(wb, s, plain);
        let (now, later) = self.split_frozen_blanks(wb, s, plain);
        for (r, c, cell) in now.into_iter().chain(later) {
            self.restore_cell(wb, (s, r, c), cell);
        }
        let frozen: Vec<_> = anchors
            .iter()
            .filter_map(|&(r, c, cell)| {
                self.restore_cell(wb, (s, r, c), cell.clone());
                self.is_unsupported((s, r, c)).then_some((r, c, cell))
            })
            .collect();
        self.refill_frozen(wb, s, &frozen, &members);
    }

    /// Write a copied `block` onto sheet `s` from `(br, bc)` (a paste), one
    /// cell at a time through the engine. Rows may differ in length.
    ///
    /// A pasted cell is typed there ([`Engine::set_cells`]), except that the
    /// block's spilling arrays come out spilling, as in Excel. A pasted value
    /// keeps its value metadata (`vm`: a picture in a cell, a rich error), as
    /// Excel copies it with the cell; save writes it while the value is the
    /// one it was loaded with. A pasted formula takes this cell's own
    /// metadata, as a typed one does. An *anchor* is
    /// a formula cell whose spill lies wholly inside the block, wherever it
    /// lands, or (#785) a legacy array block (`t="array"`) pasted back at its
    /// own address with its whole `ref` in the block. Its spilled values in the
    /// block are written as styled blanks (as constants they would block its
    /// spill), and the anchors go last so their recalc refills them.
    ///
    /// The in-place array block is restored as it was ([`Engine::restore_cell`]),
    /// keeping its `<f>` attributes: a cut has already cleared its source, so
    /// `set_cell` would see a blank target and type it. It claims no spill
    /// beyond its `ref`, so it never takes over content outside the paste
    /// (pasted at the same address on another sheet it stays an array there,
    /// as in Excel). Its file metadata goes, as a typed formula's does: the
    /// `cm` may index another workbook's metadata, and save resolves a fresh
    /// one. Every other anchor is typed as through `set_cell`, even over the
    /// same frozen formula (which `set_cell` only restyles): the copied block
    /// replaces the one there. An anchor the engine can't evaluate keeps its
    /// copied values instead: they are put back as its spill.
    ///
    /// A paste that would change part of an evaluated legacy CSE block is
    /// refused whole ([`Engine::refuses_paste`]): nothing is written and this
    /// returns false. One that replaces such a block's anchor replaces the
    /// block, the anchor first: a plain cell there, a formula with other
    /// text, or the block restored in place ([`Engine::refuses_paste`]).
    pub fn paste_block(
        &mut self,
        wb: &mut Workbook,
        s: usize,
        (br, bc): (u32, u32),
        block: &[Vec<Cell>],
    ) -> bool {
        if self.refuses_paste(wb, s, (br, bc), block, &[]) {
            return false;
        }
        self.paste_block_prechecked(wb, s, (br, bc), block);
        true
    }

    /// [`Engine::paste_block`] for a paste the caller has already asked
    /// [`Engine::refuses_paste`] about, with the rest of its group (a cut's
    /// clears, written first through [`Engine::set_cells_prechecked`]).
    /// Nothing is decided again against the sheet those clears left.
    pub fn paste_block_prechecked(
        &mut self,
        wb: &mut Workbook,
        s: usize,
        (br, bc): (u32, u32),
        block: &[Vec<Cell>],
    ) {
        let anchors = paste_anchors(block, (br, bc));
        let extents: Vec<(u32, u32, &Cell)> = anchors
            .iter()
            .map(|(r, c, cell, _)| (*r, *c, cell))
            .collect();
        let mut members = Vec::new();
        let mut plain = Vec::new();
        for (dr, row) in block.iter().enumerate() {
            for (dc, cell) in row.iter().enumerate() {
                let (r, c) = (br + dr as u32, bc + dc as u32);
                if extents.iter().any(|&(ar, ac, _)| (ar, ac) == (r, c)) {
                    continue;
                }
                if cell.formula.is_none() && in_extent(&extents, r, c) {
                    members.push((r, c, cell));
                    plain.push((r, c, cell.blank_like()));
                } else {
                    plain.push((r, c, cell.clone()));
                }
            }
        }
        // A pasted anchor landing on an evaluated CSE anchor replaces it
        // before the rest is written, so the block lets go of its cells
        // ([`Engine::set_cells_prechecked`] does the same for a plain cell
        // there).
        let early: Vec<bool> = anchors
            .iter()
            .map(|(r, c, _, _)| self.is_live_cse(wb, (s, *r, *c)))
            .collect();
        let order = (0..anchors.len())
            .filter(|&i| early[i])
            .chain([usize::MAX])
            .chain((0..anchors.len()).filter(|&i| !early[i]));
        let mut frozen = Vec::new();
        for i in order {
            let Some((r, c, cell, restore)) = anchors.get(i) else {
                self.set_cells_prechecked(wb, s, std::mem::take(&mut plain));
                continue;
            };
            let key = (s, *r, *c);
            if *restore {
                self.restore_cell(wb, key, cell.clone());
            } else {
                self.type_cell(wb, key, cell.clone(), false, false);
            }
            if self.is_unsupported(key) {
                frozen.push((*r, *c, cell));
            }
        }
        self.refill_frozen(wb, s, &frozen, &members);
    }

    /// Put the copied values of `frozen` anchors (ones the engine can't
    /// evaluate, so their recalc refills nothing) back as their spill:
    /// written into the sheet as an evaluated spill is (not through
    /// `set_cell`, which would break it), then recalculated for their
    /// dependents. `members` are the values written as blanks.
    fn refill_frozen(
        &mut self,
        wb: &mut Workbook,
        s: usize,
        frozen: &[(u32, u32, &Cell)],
        members: &[(u32, u32, &Cell)],
    ) {
        let Some(sheet) = wb.sheets.get_mut(s) else {
            return;
        };
        for &(r, c, cell) in frozen {
            if let Some(a) = sheet.cells.get_mut(&(r, c)) {
                a.spill = cell.spill;
            }
        }
        let mut refilled = Vec::new();
        for &(r, c, cell) in members {
            if in_extent(frozen, r, c) {
                sheet.set_cell(r, c, cell.clone());
                refilled.push((s, r, c));
            }
        }
        if !refilled.is_empty() {
            self.recalc_from(wb, &refilled);
        }
    }

    /// A cell's spill extent is the engine's own state, derived by
    /// [`Engine::eval_one`] — never input. An incoming one (a pasted or filled
    /// clone of an anchor, an undo/redo snapshot) names the source's spill, and
    /// left in place it would make the cells under it count as this anchor's
    /// own, overwriting (or, on a scalar result, clearing) what they hold. So
    /// it is dropped and the anchor re-spills on its own recalc. Callers that
    /// submit an anchor together with its spilled values blank those first
    /// ([`Engine::paste_block`], [`Engine::restore_cells`], and the hosts'
    /// undo snapshots through [`crate::sheet::snapshot_cells`]), or they would
    /// block it; a frozen anchor's extent and values are put back by
    /// `refill_frozen`.
    ///
    /// A blank landing in a frozen anchor's block keeps the block whole;
    /// content landing there drops the anchor's extent, the other cached
    /// cells staying as plain values.
    ///
    /// With `refuse`, a plain value or blank written into a non-anchor cell
    /// of an evaluated legacy CSE block's `ref` is refused by the group rule
    /// ([`Engine::refuses`]), as Excel refuses to change part of an array:
    /// nothing changes and this returns false. A cell typed alone
    /// ([`Engine::set_cell`]) refuses; a group checked whole
    /// ([`Engine::set_cells_prechecked`], [`Engine::paste_block_prechecked`])
    /// does not, nor does a restored snapshot.
    fn put_cell(&mut self, wb: &mut Workbook, key: Key, mut cell: Cell, refuse: bool) -> bool {
        cell.spill = None;
        let (s, r, c) = key;
        let SpillOwner {
            mut owner,
            frozen: owner_frozen,
            frozen_ref_covers,
        } = self.spill_owner_of(wb, key);
        if frozen_ref_covers == Some(false) {
            owner = None;
        }
        // Keep what `spill_owner_of` learnt of a live owner (it may have
        // evaluated it to find out), so the cells of a group landing in its
        // spill, and the refusal below, ask [`Engine::is_frozen`] once, not
        // once per cell.
        if let Some(((ar, ac), _)) = owner {
            let k = (s, ar, ac);
            if !owner_frozen && self.formulas.contains_key(&k) && !self.unsupported.contains(&k) {
                self.supported.insert(k);
            }
        }
        // A legacy CSE block the engine evaluates owns its whole `ref`, as in
        // Excel ("You can't change part of an array"): a cell written alone
        // (not part of a group checked whole) is decided by the group rule,
        // [`Engine::refuses`]. A frozen block keeps the rules below.
        if refuse
            && cell.formula.is_none()
            && self.refused(wb, s, &[(r, c, &cell)], &HashSet::new())
        {
            return false;
        }
        // Deleting a cell of a frozen anchor's block (or putting back the
        // value it holds, as undo/redo of that does) is a no-op but for the
        // style, as deleting a spilled cell is in Excel: the anchor never
        // re-spills, so its block must stay whole — cached values, extent
        // and the `ref` the writer gives it.
        let in_frozen_block = frozen_ref_covers == Some(true);
        if in_frozen_block && cell.formula.is_none() {
            if let Some(sheet) = wb.sheets.get_mut(s) {
                let held = sheet.cell(r, c);
                let same = held.is_none_or(|h| h.formula.is_none())
                    && (cell.value.is_empty() || held.is_some_and(|h| h.value == cell.value));
                if same {
                    match sheet.cells.get_mut(&(r, c)) {
                        Some(h) => h.style = cell.style,
                        None => sheet.set_cell(r, c, cell),
                    }
                    return true;
                }
            }
        }
        // Drop stale bookkeeping for this address.
        self.formulas.remove(&key);
        self.circular.remove(&key);
        self.unsupported.remove(&key);
        self.supported.remove(&key);
        self.spill_blocked.remove(&key);
        let mut changed = vec![key];
        if let Some(sheet) = wb.sheets.get_mut(s) {
            // Replacing a spill anchor orphans its spilled cells: clear them.
            if let Some(ext) = sheet.cell(r, c).and_then(|cl| cl.spill) {
                changed.extend(clear_spill(sheet, s, (r, c), ext, None));
            }
            // An edit landing inside another anchor's spill breaks that
            // spill: clear its cells and recalc the anchor (a dynamic array
            // shows #SPILL!; a CSE block refills or, blocked, keeps its own
            // value — see `fill_cse`). A frozen anchor never re-spills, so
            // content typed into its block leaves its other cached cells as
            // they are (plain values now that it has no extent).
            if let Some((anchor, ext)) = owner {
                if !owner_frozen {
                    changed.extend(clear_spill(sheet, s, anchor, ext, None));
                }
                if let Some(a) = sheet.cells.get_mut(&anchor) {
                    a.spill = None;
                }
                changed.push((s, anchor.0, anchor.1));
            }
        }
        self.index_cell(wb, key, &cell);
        if s < wb.sheets.len() {
            wb.sheets[s].set_cell(r, c, cell);
        }
        self.recalc_from(wb, &changed);
        true
    }

    /// A group of edits to sheet `s` split in two, each in order: the rest,
    /// and the blanks landing in a frozen array block as the sheet stands
    /// ([`Engine::blanks_in_frozen_blocks`]), to be put after the rest. Such
    /// a blank is a no-op while its block is whole, so it must come after
    /// any content the group puts into the block ([`Engine::set_cells`]); a
    /// host that writes a group in parts (a cut's clears, then its paste)
    /// puts the second half after the last part.
    pub fn split_frozen_blanks(
        &self,
        wb: &Workbook,
        s: usize,
        changes: Vec<(u32, u32, Cell)>,
    ) -> (CellEdits, CellEdits) {
        let blanks: Vec<(u32, u32)> = changes
            .iter()
            .filter(|(_, _, cell)| cell.is_blank())
            .map(|&(r, c, _)| (r, c))
            .collect();
        let later = self.blanks_in_frozen_blocks(wb, s, &blanks);
        let (later, now) = changes
            .into_iter()
            .partition(|(r, c, _)| later.contains(&(*r, *c)));
        (now, later)
    }

    /// Which of `blanks` (cells a group is about to blank on sheet `s`) land
    /// in a frozen array anchor's block as the sheet stands: the cells, but
    /// the anchor, that both its extent and its own `ref` (one that starts
    /// at it) cover, where a blank is a no-op ([`Engine::spill_owner_of`]'s
    /// `frozen_ref_covers`). Found once per group, not per blank; only an
    /// anchor whose block takes one of them is asked [`Engine::is_frozen`]
    /// (which may evaluate it).
    fn blanks_in_frozen_blocks(
        &self,
        wb: &Workbook,
        s: usize,
        blanks: &[(u32, u32)],
    ) -> HashSet<(u32, u32)> {
        let mut out = HashSet::new();
        let Some(sheet) = wb.sheets.get(s).filter(|_| !blanks.is_empty()) else {
            return out;
        };
        for (&(r, c), cell) in &sheet.cells {
            let Some((h, w)) = cell.spill else {
                continue;
            };
            let Some(fa) = cell.f_attrs.as_deref().filter(|fa| is_array_f(fa)) else {
                continue;
            };
            if !crate::sheet::ref_starts_at(fa, &crate::sheet::cell_name(r, c)) {
                continue;
            }
            let Some((_, _, r2, c2)) = array_block(cell) else {
                continue;
            };
            let (r2, c2) = (r2.min(r + h - 1), c2.min(c + w - 1));
            let inside: Vec<(u32, u32)> = blanks
                .iter()
                .copied()
                .filter(|&(br, bc)| {
                    (br, bc) != (r, c) && (r..=r2).contains(&br) && (c..=c2).contains(&bc)
                })
                .collect();
            if !inside.is_empty() && self.is_frozen(wb, (s, r, c)) {
                out.extend(inside);
            }
        }
        out
    }

    /// The spill anchor whose extent holds `key` (other than `key` itself),
    /// and what [`Engine::put_cell`] makes of it.
    fn spill_owner_of(&self, wb: &Workbook, key: Key) -> SpillOwner {
        let (s, r, c) = key;
        let owner = wb.sheets.get(s).and_then(|sh| spill_owner(sh, r, c));
        let frozen = owner.is_some_and(|((ar, ac), _)| self.is_frozen(wb, (s, ar, ac)));
        // A frozen anchor's extent is never corrected: row/column edits
        // shift its array `ref` but not its extent, and a sort moves a
        // one-row anchor (extent and all) but leaves a `ref` it does not own
        // as is. Its block is what a `ref` that starts at the anchor covers;
        // elsewhere the cell is not part of it and the edit leaves the
        // anchor alone.
        let frozen_ref_covers = owner.filter(|_| frozen).and_then(|(anchor, _)| {
            let a = wb.sheets[s].cell(anchor.0, anchor.1)?;
            let fa = a.f_attrs.as_deref().filter(|fa| is_array_f(fa))?;
            let own = crate::sheet::ref_starts_at(fa, &crate::sheet::cell_name(anchor.0, anchor.1));
            Some(own && crate::sheet::ref_covers(fa, r, c))
        });
        SpillOwner {
            owner,
            frozen,
            frozen_ref_covers,
        }
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
        // level dependents via the seed index in `formulas_reading`;
        // everything reachable from them is a formula and expands via `rev`.
        let data_seeds: Vec<Key> = changed
            .iter()
            .copied()
            .filter(|k| !self.formulas.contains_key(k))
            .collect();
        if !data_seeds.is_empty() {
            for fk in self.formulas_reading(&data_seeds, &dirty) {
                if dirty.insert(fk) {
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

    /// The formulas not in `skip` with a dependency rectangle covering one of
    /// `seeds` (plain-value cells, which `rev` cannot reach). Seeds are indexed
    /// per sheet, sorted by (row, col), so each rectangle costs a binary search
    /// for its first row plus a walk over the seeds in its row band — not a
    /// test against every seed.
    fn formulas_reading(&self, seeds: &[Key], skip: &HashSet<Key>) -> Vec<Key> {
        let mut by_sheet: HashMap<usize, Vec<(u32, u32)>> = HashMap::new();
        for &(s, r, c) in seeds {
            by_sheet.entry(s).or_default().push((r, c));
        }
        if by_sheet.is_empty() {
            return Vec::new();
        }
        for cells in by_sheet.values_mut() {
            cells.sort_unstable();
            cells.dedup();
        }
        let mut out = Vec::new();
        'formulas: for (&fk, info) in &self.formulas {
            if skip.contains(&fk) {
                continue;
            }
            for &(ds, r1, c1, r2, c2) in &info.deps {
                let Some(cells) = by_sheet.get(&ds) else {
                    continue;
                };
                // Cells are (row, col)-ordered, so the rect's rows are the
                // contiguous slice from the first row ≥ r1 up to the last ≤ r2.
                let start = cells.partition_point(|&(r, _)| r < r1);
                for &(r, c) in &cells[start..] {
                    if r > r2 {
                        break;
                    }
                    if c >= c1 && c <= c2 {
                        out.push(fk);
                        continue 'formulas;
                    }
                }
            }
        }
        out
    }

    /// Kahn's algorithm over the dirty subgraph, then evaluation in order.
    fn evaluate(&mut self, wb: &mut Workbook, dirty: HashSet<Key>, depth: u32) {
        // Depth 0 is a top-level recalculation (an edit, recalc_all); nested
        // passes (spills, the D-function rerun) have depth ≥ 1.
        if depth == 0 {
            self.db_rerun_done = false;
            // Hosts change the sheets between engine calls, so an index of
            // their spills never outlives one top-level pass.
            self.pass_anchors.clear();
        }
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
                if self.formulas.get(&comp[0]).is_some_and(|i| i.db) {
                    db_done.push(comp[0]);
                }
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
        // or that the circle phase evaluated after it. Evaluate those
        // D-functions once more, in the order they ran; only when one's value
        // changed do its dependents run again (a circle among them gets one
        // more sweep, which the rollback rule keeps put once converged). This
        // happens at most once per top-level recalculation, and never re-seeds
        // the volatile cells, so an unrelated circle gets no extra sweep.
        // Circle members are not rerun: evaluating one alone would bypass
        // the circle rules.
        if !rest.is_empty() && !db_done.is_empty() && !self.db_rerun_done {
            self.db_rerun_done = true;
            let value_of = |wb: &Workbook, k: Key| {
                wb.sheets[k.0]
                    .cell(k.1, k.2)
                    .map(|c| c.value.clone())
                    .unwrap_or_default()
            };
            let mut changed: Vec<Key> = Vec::new();
            for &k in &db_done {
                let before = value_of(wb, k);
                spilled.extend(self.eval_one(wb, k));
                if value_of(wb, k) != before {
                    changed.push(k);
                }
            }
            if !changed.is_empty() {
                let dependents = self.dependents_of(&changed);
                if !dependents.is_empty() {
                    self.evaluate(wb, dependents, depth + 1);
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
        if depth == 0 {
            self.pass_anchors.clear();
        }
    }

    /// The formulas that depend on `seeds`, directly or transitively (the
    /// seeds themselves excluded unless one depends on another).
    fn dependents_of(&self, seeds: &[Key]) -> HashSet<Key> {
        let all: Vec<Key> = self.formulas.keys().copied().collect();
        let mut rev: HashMap<Key, Vec<Key>> = HashMap::new();
        for (f, srcs) in self.dependency_edges(&all) {
            for g in srcs {
                rev.entry(g).or_default().push(f);
            }
        }
        let mut out: HashSet<Key> = HashSet::new();
        let mut frontier: VecDeque<Key> = seeds.iter().copied().collect();
        while let Some(src) = frontier.pop_front() {
            for &f in rev.get(&src).map(Vec::as_slice).unwrap_or_default() {
                if out.insert(f) {
                    frontier.push_back(f);
                }
            }
        }
        out
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
        // or `t="array"`) ones spill, except a CSE block, which fills its `ref`.
        let spill = !info.legacy;
        let cse = info.cse;
        let resolver = WbResolver {
            wb,
            clock: self.clock,
            rand_state: StdCell::new(self.seed.unwrap_or(0)),
            has_rand: self.seed.is_some(),
        };
        let mut ev = Eval::new(&resolver, key.0, (key.1, key.2));
        let (result, shaped) = ev.eval_dynamic_shaped(&info.ast, spill);
        let unsupported = ev.unsupported;
        if self.seed.is_some() {
            self.seed = Some(resolver.rand_state.get());
        }
        if unsupported {
            // Something beyond the engine: freeze this cell on its cached
            // value from here on.
            self.unsupported.insert(key);
            self.supported.remove(&key);
            return Vec::new();
        }
        self.supported.insert(key);
        let (s, r, c) = key;
        let Some(sheet) = wb.sheets.get_mut(s) else {
            return Vec::new();
        };
        let old = sheet.cell(r, c).and_then(|cl| cl.spill).unwrap_or((1, 1));
        if let Some((h, w)) = cse {
            return self.fill_cse(sheet, key, (h, w), old, result);
        }
        // A modern formula of ours (not a loaded `t="array"` one, which keeps
        // its `<f>` attributes) that produced an array is a dynamic array from
        // now on, whatever it evaluates to later: it saves with a `cm`.
        if shaped && spill {
            if let Some(cell) = sheet.cells.get_mut(&(r, c)) {
                if cell.f_attrs.is_none() && !cell.is_dynamic() {
                    cell.meta.get_or_insert_default().dynamic = true;
                }
            }
        }
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
                            let v = value_to_cell(v);
                            let entry = sheet.cells.entry((rr, cc)).or_default();
                            if (rr, cc) != (r, c) {
                                // Outside the previous extent the cell newly
                                // belongs to this spill (report it, as on
                                // growth); inside, only a real content change
                                // is reported.
                                let fresh = rr >= r + old.0 || cc >= c + old.1;
                                let differs = fresh
                                    || entry.value != v
                                    || entry.formula.is_some()
                                    || entry.f_attrs.is_some()
                                    || entry.spill.is_some();
                                entry.formula = None;
                                entry.f_attrs = None;
                                entry.spill = None;
                                if differs {
                                    changed.push((s, rr, cc));
                                }
                            }
                            entry.value = v;
                        }
                    }
                    let entry = sheet.cells.entry((r, c)).or_default();
                    entry.spill = Some((h, w));
                    self.note_spill(key, h);
                    self.spill_blocked.remove(&key);
                }
            }
        }
        changed
    }

    /// Store a legacy CSE array's result over its fixed block ([`cse_at`]).
    ///
    /// The block owns every plain value in its `ref`, as in Excel, which
    /// refuses to change part of an array: a value typed or pasted into a
    /// block cell never lands ([`Engine::refuses`], [`Engine::refuses_paste`]
    /// refuse it). One that
    /// reaches the `ref` another way is refilled by the block: loaded there,
    /// shifted into a grown `ref` by an insert, restored by an undo, or left
    /// by a blocking formula that is replaced. A formula in a block cell, or a cell inside
    /// another anchor's spill, blocks it: the anchor then shows its own value
    /// alone (never `#SPILL!`), leaves the other anchor's cells alone, and
    /// refills once the block is clear.
    fn fill_cse(
        &mut self,
        sheet: &mut Sheet,
        key: Key,
        (h, w): (u32, u32),
        old: (u32, u32),
        result: DynResult,
    ) -> Vec<Key> {
        let (s, r, c) = key;
        let m = match result {
            DynResult::Scalar(v) => vec![vec![v]],
            DynResult::Array(m) => m,
        };
        let off_grid = r + h > crate::sheet::MAX_ROWS || c + w > crate::sheet::MAX_COLS;
        let by_formula = off_grid
            || (r..r + h).any(|rr| {
                (c..c + w).any(|cc| {
                    (rr, cc) != (r, c) && sheet.cell(rr, cc).is_some_and(|cl| cl.formula.is_some())
                })
            });
        // Other anchors whose spill overlaps this block or its old extent:
        // their spilled values are theirs, never this block's. They matter
        // to whether the block is blocked, unless a formula already decided
        // that, and to clearing an old extent beyond the anchor. A spill
        // writes every cell it covers, so when no cell but the anchor exists
        // there (a one-cell block, or a block over empty cells) there is
        // nothing to find.
        let (bh, bw) = (h.max(old.0), w.max(old.1));
        let wanted = !by_formula || old != (1, 1);
        let occupied = wanted
            && (r..r + bh)
                .any(|rr| (c..c + bw).any(|cc| (rr, cc) != (r, c) && sheet.cell(rr, cc).is_some()));
        let foreign = if occupied {
            self.foreign_spills(sheet, key, (bh, bw))
        } else {
            Vec::new()
        };
        let theirs = |rr: u32, cc: u32| {
            foreign
                .iter()
                .any(|&(ar, ac, sh, sw)| rr >= ar && rr < ar + sh && cc >= ac && cc < ac + sw)
        };
        let blocked = by_formula
            || (r..r + h).any(|rr| (c..c + w).any(|cc| (rr, cc) != (r, c) && theirs(rr, cc)));
        let mut changed = Vec::new();
        if blocked {
            // Clear what this block wrote before (its old extent), but never
            // another anchor's spilled cells.
            for rr in r..r + old.0 {
                for cc in c..c + old.1 {
                    if (rr, cc) == (r, c) || theirs(rr, cc) {
                        continue;
                    }
                    if sheet
                        .cell(rr, cc)
                        .is_some_and(|cl| cl.formula.is_none() && !cl.value.is_empty())
                    {
                        sheet.clear_cell(rr, cc);
                        changed.push((s, rr, cc));
                    }
                }
            }
            let entry = sheet.cells.entry((r, c)).or_default();
            entry.value = value_to_cell(cse_at(&m, 0, 0));
            entry.spill = None;
            self.spill_blocked.insert(key);
            return changed;
        }
        for rr in r..r + h {
            for cc in c..c + w {
                let v = value_to_cell(cse_at(&m, (rr - r) as usize, (cc - c) as usize));
                let entry = sheet.cells.entry((rr, cc)).or_default();
                if (rr, cc) != (r, c) && entry.value != v {
                    changed.push((s, rr, cc));
                }
                entry.value = v;
            }
        }
        // A one-cell block has no extent beyond its anchor (as a scalar).
        sheet.cells.entry((r, c)).or_default().spill = ((h, w) != (1, 1)).then_some((h, w));
        if (h, w) != (1, 1) {
            self.note_spill(key, h);
        }
        self.spill_blocked.remove(&key);
        changed
    }

    /// The anchors other than `key`'s whose spill now overlaps the `(bh, bw)`
    /// cells from `key`, as `(row, col, rows, cols)`: [`Engine::fill_cse`]
    /// leaves their cells alone. Looked up in the sheet's row index for this
    /// pass ([`Engine::pass_anchors`]), built here on first use, so a pass
    /// over many blocks walks the sheet once rather than the rows above
    /// each block. That one walk is O(cells) per pass even for a single
    /// block near the top of a big sheet, which used to walk only the rows
    /// above it; the load cap on array refs (`xlsx::cap_array_refs`) keeps
    /// the index's size, the sum of the extents' heights, within the sheet's.
    fn foreign_spills(
        &mut self,
        sheet: &Sheet,
        key: Key,
        (bh, bw): (u32, u32),
    ) -> Vec<(u32, u32, u32, u32)> {
        let (s, r, c) = key;
        let cover = self.pass_anchors.entry(s).or_insert_with(|| {
            let mut cover = RowCover::default();
            for (&at, cl) in &sheet.cells {
                if let Some((sh, _)) = cl.spill {
                    cover.note(at, sh);
                }
            }
            cover
        });
        let mut near: Vec<(u32, u32)> = (r..r + bh)
            .filter_map(|rr| cover.rows.get(&rr))
            .flatten()
            .copied()
            .collect();
        near.sort_unstable();
        near.dedup();
        // The index may hold extents since replaced: each anchor's spill is
        // read as it is now.
        near.into_iter()
            .filter_map(|(ar, ac)| {
                let (sh, sw) = sheet.cell(ar, ac)?.spill?;
                Some((ar, ac, sh, sw))
            })
            .filter(|&(ar, ac, sh, sw)| {
                (ar, ac) != (r, c) && ar < r + bh && ar + sh > r && ac < c + bw && ac + sw > c
            })
            .collect()
    }

    /// Record in this pass's index (when the sheet has one) that the anchor
    /// at `key` now spills over `h` rows from its own.
    fn note_spill(&mut self, (s, r, c): Key, h: u32) {
        if let Some(cover) = self.pass_anchors.get_mut(&s) {
            cover.note((r, c), h);
        }
    }
}

/// The fixed block of a legacy Ctrl+Shift+Enter array at `(row, col)`: a
/// `t="array"` formula with no `cm` (not a dynamic array) and not typed here,
/// whose `ref` starts at the cell. None for anything else — a dynamic array,
/// or an array `<f>` with no `ref` (it spills as before).
fn cse_block(cell: &Cell, row: u32, col: u32) -> Option<(u32, u32)> {
    let fa = cell.f_attrs.as_deref().filter(|a| is_array_f(a))?;
    if cell.is_dynamic() || cell.is_modern() {
        return None;
    }
    let (r1, c1, r2, c2) = crate::sheet::parse_range_name(f_ref(fa)?)?;
    ((r1, c1) == (row, col) && r2 >= r1 && c2 >= c1).then_some((r2 - r1 + 1, c2 - c1 + 1))
}

/// The anchors [`Engine::paste_block`] writes last for a paste of `block`
/// at `(br, bc)`: `(row, col, cell, restored)`. One is a formula cell whose
/// spill lies wholly inside the block, typed there; or (#785) a legacy array
/// block pasted back at its own address with its whole `ref` in the block,
/// restored there (`restored`), its spill cut to that `ref` and its file
/// metadata gone.
fn paste_anchors(block: &[Vec<Cell>], (br, bc): (u32, u32)) -> Vec<(u32, u32, Cell, bool)> {
    let in_block = |r: u32, c: u32| {
        r >= br
            && c >= bc
            && block
                .get((r - br) as usize)
                .is_some_and(|row| ((c - bc) as usize) < row.len())
    };
    let inside = |r: u32, c: u32, (h, w): (u32, u32)| {
        (r..r + h).all(|rr| (c..c + w).all(|cc| in_block(rr, cc)))
    };
    let mut anchors = Vec::new();
    for (dr, row) in block.iter().enumerate() {
        for (dc, cell) in row.iter().enumerate() {
            let (r, c) = (br + dr as u32, bc + dc as u32);
            if cell.formula.is_none() {
                continue;
            }
            let in_place = array_block(cell).filter(|&(r1, c1, r2, c2)| {
                (r1, c1) == (r, c) && inside(r, c, (r2 - r1 + 1, c2 - c1 + 1))
            });
            if let Some((r1, c1, r2, c2)) = in_place {
                let mut cell = cell.clone();
                cell.spill = cell
                    .spill
                    .map(|(h, w)| (h.min(r2 - r1 + 1), w.min(c2 - c1 + 1)));
                if let Some(m) = cell.meta.as_deref_mut() {
                    forget_file_meta(m);
                }
                anchors.push((r, c, cell, true));
            } else if cell.spill.is_some_and(|ext| inside(r, c, ext)) {
                anchors.push((r, c, cell.clone(), false));
            }
        }
    }
    anchors
}

/// The non-anchor cell `(r, c)` of one of `blocks` (`(anchor row, anchor
/// col, rows, cols)`, [`Engine::cse_blocks`]): that block's anchor.
fn block_over(blocks: &[(u32, u32, u32, u32)], r: u32, c: u32) -> Option<(u32, u32)> {
    blocks.iter().find_map(|&(ar, ac, h, w)| {
        let covers = (ar, ac) != (r, c) && r >= ar && r < ar + h && c >= ac && c < ac + w;
        covers.then_some((ar, ac))
    })
}

/// Excel's fill of a CSE block from an array result, at block offset
/// `(i, j)`: a 1-row (1-column) result repeats down (across), a scalar over
/// the whole block, and cells past a larger dimension are `#N/A`.
fn cse_at(m: &[Vec<Value>], i: usize, j: usize) -> Value {
    let i = if m.len() == 1 { 0 } else { i };
    let row = m.get(i);
    let j = if row.is_some_and(|r| r.len() == 1) {
        0
    } else {
        j
    };
    row.and_then(|r| r.get(j))
        .cloned()
        .unwrap_or(Value::Err(ExcelError::NA))
}

/// Edits to one sheet: `(row, col, the cell put there)`, in order.
type CellEdits = Vec<(u32, u32, Cell)>;

/// The spill anchor over a cell ([`Engine::spill_owner_of`]).
struct SpillOwner {
    /// The anchor and its extent.
    owner: Option<((u32, u32), (u32, u32))>,
    /// The anchor is one the engine can't evaluate ([`Engine::is_frozen`]).
    frozen: bool,
    /// For a frozen array anchor, whether its block holds the cell: `Some(false)`
    /// when its stored `ref` is not its own or leaves the cell out.
    frozen_ref_covers: Option<bool>,
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

/// Is (r, c) inside the spill of one of `anchors` (other than an anchor
/// itself)?
fn in_extent(anchors: &[(u32, u32, &Cell)], r: u32, c: u32) -> bool {
    anchors.iter().any(|&(ar, ac, a)| {
        (ar, ac) != (r, c)
            && a.spill
                .is_some_and(|(h, w)| (ar..ar + h).contains(&r) && (ac..ac + w).contains(&c))
    })
}

/// Drop the metadata a cell brought from a file (`cm`, `vm`): its indices
/// name entries in the metadata part of the workbook it came from, which a
/// formula typed or pasted here need not share. A `cm` marked a dynamic
/// array, so the cell stays one, and save resolves a `cm` in this package.
fn forget_file_meta(m: &mut CellMeta) {
    if m.cm.take().is_some() {
        m.dynamic = true;
    }
    m.vm = None;
    m.vm_body = None;
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
pub(crate) fn value_to_cell(v: Value) -> CellValue {
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

    thread_local! {
        /// How many formulas [`Engine::is_frozen`] has evaluated on this
        /// thread.
        pub(super) static FROZEN_EVALS: StdCell<usize> = const { StdCell::new(0) };
    }

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
    fn set_cell_still_drops_a_shared_formula_marker() {
        // Only an array marker survives set_cell; a shared group's or a data
        // table's names cells this formula no longer owns.
        let mut wb = wb_one_sheet(&[("A1", Cell::number(2.0))]);
        let mut eng = Engine::new(&wb);
        for (name, fa) in [
            ("B1", " t=\"shared\" ref=\"B1:B3\" si=\"0\""),
            (
                "C1",
                " t=\"dataTable\" ref=\"C1:C2\" dt2D=\"0\" dtr=\"0\" r1=\"A1\"",
            ),
        ] {
            let mut cell = Cell::formula("A1*2");
            cell.f_attrs = Some(fa.to_string());
            set(&mut eng, &mut wb, name, cell);
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            assert!(wb.sheets[0].cell(r, c).unwrap().f_attrs.is_none(), "{name}");
            assert_eq!(value_at(&wb, name), CellValue::Number(4.0), "{name}");
        }
        // A typed formula is a fresh Cell: nothing to keep.
        set(&mut eng, &mut wb, "D1", Cell::formula("A1*3"));
        assert!(wb.sheets[0].cell(0, 3).unwrap().f_attrs.is_none());
        // An incoming array marker is dropped too (typing, #724); the same
        // formula re-submitted keeps the one the cell already had.
        let mut cell = Cell::formula("A1*4");
        cell.f_attrs = Some(" t=\"array\" ref=\"E1\"".to_string());
        set(&mut eng, &mut wb, "E1", cell);
        assert!(wb.sheets[0].cell(0, 4).unwrap().f_attrs.is_none());
        let mut cse = Cell::formula("A1*5");
        cse.f_attrs = Some(" t=\"array\" ref=\"F1:F2\"".to_string());
        wb.sheets[0].set_cell(0, 5, cse);
        let mut restyled = wb.sheets[0].cell(0, 5).cloned().unwrap();
        restyled.style = 1;
        restyled.f_attrs = None;
        set(&mut eng, &mut wb, "F1", restyled);
        assert_eq!(
            wb.sheets[0].cell(0, 5).unwrap().f_attrs.as_deref(),
            Some(" t=\"array\" ref=\"F1:F2\"")
        );
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

    /// #603: Excel's quoted 3D spelling, `'First:Last'!`, evaluates as the
    /// span, also over names that read as cells (`'Q1:Q3'!`).
    #[test]
    fn quoted_three_d_spans_evaluate() {
        let mut wb = Workbook::default();
        for (i, name) in ["Jan 2024", "Feb 2024", "Mar 2024", "Q1", "Q2", "Q3", "Sum"]
            .iter()
            .enumerate()
        {
            let mut s = Sheet {
                name: name.to_string(),
                ..Sheet::default()
            };
            if i < 6 {
                s.set_cell(0, 0, Cell::number((i + 1) as f64));
                s.set_cell(1, 0, Cell::number(100.0));
            }
            wb.sheets.push(s);
        }
        wb.sheets[6].set_cell(0, 0, Cell::formula("SUM('Jan 2024:Mar 2024'!A1)"));
        wb.sheets[6].set_cell(1, 0, Cell::formula("SUM('Q1:Q3'!A1:A2)"));
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(
            wb.sheets[6].cell(0, 0).unwrap().value,
            CellValue::Number(6.0)
        );
        assert_eq!(
            wb.sheets[6].cell(1, 0).unwrap().value,
            CellValue::Number(315.0)
        );
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
    fn a_circle_reading_a_database_function_advances_once_per_recalc() {
        // #677/#660: Z1 = Z1+K1 with one iteration per recalculation and K1 a
        // DSUM (5). An unrelated edit recalculates K1 (it always does) and so
        // Z1 once: Z1 advances by exactly 5. The D-function rerun must not
        // add sweeps (it used to re-arm itself at every nested pass).
        let mut wb = db_helper_book(
            "DSUM(A1:C8,\"Amount\",AA1:AA2)",
            "B5*2",
            &[("B5", Cell::number(60.0)), ("Z1", Cell::formula("Z1+K1"))],
        );
        wb.iterate = Some((1, 0.001));
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(5.0));
        let z = |wb: &Workbook| match value_at(wb, "Z1") {
            CellValue::Number(z) => z,
            v => panic!("{v:?}"),
        };
        let z0 = z(&wb);
        set(&mut eng, &mut wb, "Y9", Cell::number(1.0));
        assert_eq!(z(&wb) - z0, 5.0);
        set(&mut eng, &mut wb, "Y9", Cell::number(2.0));
        assert_eq!(z(&wb) - z0, 10.0);
    }

    #[test]
    fn database_functions_downstream_of_a_circle_see_every_helper() {
        // #677/#660: the whole helper column E sits below the iterating
        // circle Z1, and so does the DSUM (through AA2 = E2>100). The circle
        // phase may run the DSUM before E3..E8; the rerun fixes it.
        let mut cells: Vec<(String, Cell)> = vec![
            ("A1".into(), Cell::text("Rep")),
            ("C1".into(), Cell::text("Amount")),
            ("AA2".into(), Cell::formula("E2>100")),
            ("Z1".into(), Cell::formula("Z1/2+B1")),
            ("B1".into(), Cell::number(10.0)),
            (
                "K21".into(),
                Cell::formula("DSUM(A1:C8,\"Amount\",AA1:AA2)"),
            ),
        ];
        for r in 2..=8 {
            cells.push((format!("A{r}"), Cell::text(&format!("R{r}"))));
            cells.push((format!("C{r}"), Cell::number(r as f64)));
            cells.push((format!("E{r}"), Cell::formula(&format!("C{r}*Z1"))));
        }
        let refs: Vec<(&str, Cell)> = cells.iter().map(|(k, c)| (k.as_str(), c.clone())).collect();
        let mut wb = wb_one_sheet(&refs);
        wb.iterate = Some((100, 0.001));
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        // Z1 → 20: E6..E8 (120, 140, 160) pass.
        assert_eq!(value_at(&wb, "K21"), CellValue::Number(21.0));
        set(&mut eng, &mut wb, "B1", Cell::number(30.0));
        // Z1 → 60: E2..E8 pass.
        assert_eq!(value_at(&wb, "K21"), CellValue::Number(35.0));
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "K21"), CellValue::Number(35.0));
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

    /// #785 r2: a value typed into a spill away from column A and row 1 clears
    /// the rest of the spill too (the kept extent was the typed cell's
    /// address), and clearing it heals the spill.
    #[test]
    fn typing_into_a_spill_off_the_origin_clears_the_rest_of_it() {
        let mut wb = wb_one_sheet(&[("D1", array_formula("SEQUENCE(3)"))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "D2"), CellValue::Number(2.0));
        set(&mut eng, &mut wb, "D3", Cell::number(7.0));
        assert_eq!(value_at(&wb, "D1"), CellValue::Error("#SPILL!".into()));
        assert_eq!(value_at(&wb, "D2"), CellValue::Empty);
        assert_eq!(value_at(&wb, "D3"), CellValue::Number(7.0));
        set(&mut eng, &mut wb, "D3", Cell::default());
        for (name, v) in [("D1", 1.0), ("D2", 2.0), ("D3", 3.0)] {
            assert_eq!(value_at(&wb, name), CellValue::Number(v), "{name}");
        }
    }

    fn spill_of(wb: &Workbook, name: &str) -> Option<(u32, u32)> {
        let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
        wb.sheets[0].cell(r, c).and_then(|cl| cl.spill)
    }

    fn style_of(wb: &Workbook, name: &str) -> u32 {
        let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
        wb.sheets[0].cell(r, c).map_or(0, |cl| cl.style)
    }

    #[test]
    fn restyling_a_whole_spill_keeps_it() {
        // #784: formatting a spill block (or one member) changes styles only.
        let mut wb = wb_one_sheet(&[]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "D1", Cell::formula("SEQUENCE(3)"));
        assert_eq!(spill_of(&wb, "D1"), Some((3, 1)));
        eng.set_styles(&mut wb, 0, &[(0, 3, 1), (1, 3, 1), (2, 3, 1)]);
        assert_eq!(spill_of(&wb, "D1"), Some((3, 1)));
        for (i, name) in ["D1", "D2", "D3"].into_iter().enumerate() {
            assert_eq!(value_at(&wb, name), CellValue::Number(i as f64 + 1.0));
            assert_eq!(style_of(&wb, name), 1, "{name}");
        }
        // A lone member.
        eng.set_styles(&mut wb, 0, &[(1, 3, 2)]);
        assert_eq!(spill_of(&wb, "D1"), Some((3, 1)));
        assert_eq!(value_at(&wb, "D2"), CellValue::Number(2.0));
        assert_eq!(style_of(&wb, "D2"), 2);
        assert_eq!(style_of(&wb, "D3"), 1);
    }

    #[test]
    fn restyling_a_legacy_array_keeps_it() {
        let mut cse = Cell::formula("A1:A3*2");
        cse.f_attrs = Some(" t=\"array\" ref=\"D1:D3\"".to_string());
        cse.meta = Some(Box::new(CellMeta {
            cm: Some("1".to_string()),
            ..CellMeta::default()
        }));
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("D1", cse),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(spill_of(&wb, "D1"), Some((3, 1)));
        let before = wb.sheets[0].cell(0, 3).cloned().unwrap();
        eng.set_styles(&mut wb, 0, &[(0, 3, 1), (1, 3, 1), (2, 3, 1)]);
        assert_eq!(spill_of(&wb, "D1"), Some((3, 1)));
        for (i, name) in ["D1", "D2", "D3"].into_iter().enumerate() {
            assert_eq!(
                value_at(&wb, name),
                CellValue::Number(2.0 * (i as f64 + 1.0))
            );
            assert_eq!(style_of(&wb, name), 1, "{name}");
        }
        let after = wb.sheets[0].cell(0, 3).unwrap();
        assert_eq!(after.f_attrs, before.f_attrs);
        assert_eq!(after.meta, before.meta);
    }

    #[test]
    fn restyle_recalcs_cell_format_dependents() {
        // A style is read by CELL("format"): restyling must recalc its
        // readers (this part held before #784 too) and keep the spill.
        let mut wb = wb_one_sheet(&[("C1", Cell::formula("CELL(\"format\",D2)"))]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "D1", Cell::formula("SEQUENCE(3)"));
        assert_eq!(value_at(&wb, "C1"), CellValue::Text("G".into()));
        let mut xf = wb.styles.xf(0);
        xf.set_code(Some("0.00".to_string()));
        let idx = wb.styles.intern(xf);
        eng.set_styles(&mut wb, 0, &[(1, 3, idx)]);
        assert_eq!(value_at(&wb, "C1"), CellValue::Text("F2".into()));
        assert_eq!(spill_of(&wb, "D1"), Some((3, 1)));
        assert_eq!(value_at(&wb, "D2"), CellValue::Number(2.0));
    }

    #[test]
    fn resubmitting_a_spilled_value_still_breaks_the_spill() {
        // Guard: an entry into a spill breaks it even when it equals the
        // spilled value (only a restyle, through set_styles, does not).
        let mut wb = wb_one_sheet(&[]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "D1", Cell::formula("SEQUENCE(3)"));
        set(&mut eng, &mut wb, "D2", Cell::number(2.0));
        assert_eq!(value_at(&wb, "D1"), CellValue::Error("#SPILL!".into()));
        assert_eq!(spill_of(&wb, "D1"), None);
    }

    #[test]
    fn set_styles_leaves_no_blank_cells() {
        let mut wb = wb_one_sheet(&[("A1", Cell::number(5.0))]);
        let mut eng = Engine::new(&wb);
        // Default style on an absent cell creates nothing.
        eng.set_styles(&mut wb, 0, &[(0, 1, 0)]);
        assert!(wb.sheets[0].cell(0, 1).is_none());
        // Styling a blank cell creates it; restoring the default drops it.
        eng.set_styles(&mut wb, 0, &[(0, 1, 3)]);
        assert_eq!(style_of(&wb, "B1"), 3);
        eng.set_styles(&mut wb, 0, &[(0, 1, 0)]);
        assert!(wb.sheets[0].cell(0, 1).is_none());
        // A cell with content keeps it at the default style.
        eng.set_styles(&mut wb, 0, &[(0, 0, 3), (0, 0, 0)]);
        assert_eq!(value_at(&wb, "A1"), CellValue::Number(5.0));
        // An out-of-range sheet is ignored.
        eng.set_styles(&mut wb, 9, &[(0, 0, 3)]);
        assert_eq!(style_of(&wb, "A1"), 0);
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
    fn formulas_reading_finds_rect_readers() {
        // #878: the data-seed lookup finds exactly the formulas whose dep
        // rect covers a seed, with rect bounds treated as inclusive.
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("A4", Cell::number(4.0)),
            ("A5", Cell::number(5.0)),
            ("C1", Cell::formula("SUM(A1:A3)")),
            ("D1", Cell::formula("A5*2")),
            ("E1", Cell::formula("SUM(B1:B9)")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        let none = HashSet::new();
        assert_eq!(eng.formulas_reading(&[(0, 1, 0)], &none), vec![(0, 0, 2)]); // A2 -> C1
        assert_eq!(eng.formulas_reading(&[(0, 4, 0)], &none), vec![(0, 0, 3)]); // A5 -> D1
        assert!(eng.formulas_reading(&[(0, 3, 0)], &none).is_empty()); // A4 -> none
        let mut both = eng.formulas_reading(&[(0, 1, 0), (0, 4, 0)], &none);
        both.sort_unstable();
        assert_eq!(both, vec![(0, 0, 2), (0, 0, 3)]); // C1 and D1
        // Rect boundaries are inclusive: A3 is inside A1:A3, A4 is not.
        assert_eq!(eng.formulas_reading(&[(0, 2, 0)], &none), vec![(0, 0, 2)]);
        assert!(eng.formulas_reading(&[(0, 3, 0)], &none).is_empty());
    }

    #[test]
    fn formulas_reading_skips_and_respects_sheet() {
        // #878: seeds only match dep rects on their own sheet, and a formula
        // named in `skip` is not returned.
        let mut wb = wb_one_sheet(&[("C1", Cell::formula("SUM(Sheet2!A1:A3)"))]);
        let mut sheet2 = Sheet {
            name: "Sheet2".to_string(),
            ..Sheet::default()
        };
        for r in 0..3 {
            sheet2.set_cell(r, 0, Cell::number(f64::from(r + 1)));
        }
        wb.sheets.push(sheet2);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        // Sheet2's A1 (sheet 1, row 0, col 0) hits the cross-sheet reader…
        assert_eq!(
            eng.formulas_reading(&[(1, 0, 0)], &HashSet::new()),
            vec![(0, 0, 2)]
        );
        // …the same row/col on sheet 0 does not.
        assert!(
            eng.formulas_reading(&[(0, 0, 0)], &HashSet::new())
                .is_empty()
        );
        // skip holds the formula's key: nothing.
        assert!(
            eng.formulas_reading(&[(1, 0, 0)], &HashSet::from([(0, 0, 2)]))
                .is_empty()
        );
    }

    #[test]
    fn formulas_reading_tall_rect_ignores_other_columns() {
        // #878: a tall rect (A1:A1000) walks only the seeds in its row band;
        // seeds in other columns don't match, one inside the column does.
        let mut wb = wb_one_sheet(&[("C1", Cell::formula("SUM(A1:A1000)"))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        let mut b_seeds: Vec<Key> = (0..100).map(|r| (0, r, 1)).collect(); // B1..B100
        assert!(eng.formulas_reading(&b_seeds, &HashSet::new()).is_empty());
        b_seeds.push((0, 699, 0)); // A700
        assert_eq!(
            eng.formulas_reading(&b_seeds, &HashSet::new()),
            vec![(0, 0, 2)]
        );
    }

    #[test]
    fn respill_with_unchanged_values_reports_no_cells() {
        // #878: re-evaluating an unchanged dynamic array must not report its
        // spill cells — thousands of them would seed the recalc's data-cell
        // scan for nothing.
        let mut wb = wb_one_sheet(&[("A1", array_formula("SEQUENCE(50)"))]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(eng.eval_one(&mut wb, (0, 0, 0)), Vec::new());
    }

    #[test]
    fn respill_reports_only_changed_cells() {
        // #878: only the spilled cell whose value actually changed (A3) is
        // reported, not the whole spill.
        let mut wb = wb_one_sheet(&[
            ("A1", array_formula("IF(SEQUENCE(5)=3,B1,SEQUENCE(5))")),
            ("B1", Cell::number(3.0)),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(3.0));
        // Edit B1's stored value directly: only A3's element depends on it.
        wb.sheets[0].cells.get_mut(&(0, 1)).unwrap().value = CellValue::Number(100.0);
        assert_eq!(eng.eval_one(&mut wb, (0, 0, 0)), vec![(0, 2, 0)]);
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(100.0));
    }

    #[test]
    fn dependent_of_respilled_cell_still_updates() {
        // #878 side effect: a changed spilled value still seeds its
        // dependents (C1 reads the spilled cell A3, which is a plain value).
        let mut wb = wb_one_sheet(&[
            ("A1", array_formula("SEQUENCE(3,1,B1,1)")),
            ("B1", Cell::number(10.0)),
            ("C1", Cell::formula("A3*2")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(24.0));
        set(&mut eng, &mut wb, "B1", Cell::number(20.0));
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(22.0));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(44.0));
    }

    #[test]
    fn spill_growth_updates_rows_of_spillref() {
        // #878 side effect: growing and shrinking a spill still refreshes a
        // ROWS(A1#) reader, and a shrunk spill still clears its old cells.
        let mut wb = wb_one_sheet(&[
            ("A1", array_formula("SEQUENCE(B1)")),
            ("B1", Cell::number(2.0)),
            ("C1", Cell::formula("ROWS(A1#)")),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(2.0));
        set(&mut eng, &mut wb, "B1", Cell::number(4.0));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(4.0));
        assert_eq!(value_at(&wb, "A4"), CellValue::Number(4.0));
        set(&mut eng, &mut wb, "B1", Cell::number(2.0));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(2.0));
        assert_eq!(value_at(&wb, "A3"), CellValue::Empty);
        assert_eq!(value_at(&wb, "A4"), CellValue::Empty);
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

    #[test]
    fn typed_dynamic_survives_engine_rebuild() {
        // #724 AC4: a typed formula that spilled is marked a dynamic array, so
        // a rebuilt engine (xlsxy's rebuild_engine) still spills it.
        let mut wb = wb_one_sheet(&[]);
        let mut eng = Engine::new(&wb);
        eng.set_cell(&mut wb, (0, 0, 0), Cell::formula("SEQUENCE(3)"));
        let a1 = wb.sheets[0].cell(0, 0).unwrap();
        assert!(a1.is_modern() && a1.is_dynamic() && !a1.has_cm());
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(wb.sheets[0].cell(0, 0).unwrap().spill, Some((3, 1)));
        assert_eq!(value_at(&wb, "A3"), CellValue::Number(3.0));
    }

    #[test]
    fn typed_no_match_filter_is_dynamic_and_respills_after_rebuild() {
        // A typed FILTER that matches nothing yet (#CALC!) is ours: after a
        // rebuild it still spills once rows match. It could return an array,
        // so it is a dynamic array from the start (#777).
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
        ]);
        let mut eng = Engine::new(&wb);
        eng.set_cell(&mut wb, (0, 0, 2), Cell::formula("FILTER(A1:A3,A1:A3>5)"));
        let c1 = wb.sheets[0].cell(0, 2).unwrap();
        assert_eq!(c1.value, CellValue::Error("#CALC!".into()));
        assert!(c1.is_modern() && c1.is_dynamic());
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        for (r, v) in [6.0, 7.0, 8.0].into_iter().enumerate() {
            eng.set_cell(&mut wb, (0, r as u32, 0), Cell::number(v));
        }
        assert_eq!(wb.sheets[0].cell(0, 2).unwrap().spill, Some((3, 1)));
        assert_eq!(value_at(&wb, "C3"), CellValue::Number(8.0));
        assert!(wb.sheets[0].cell(0, 2).unwrap().is_dynamic());
    }

    #[test]
    fn typed_formulas_over_single_cells_are_not_dynamic() {
        // #724 r1: an elementwise op over single cells that arrive as 1x1
        // ranges (a table's this-row ref, OFFSET, INDIRECT) is a scalar
        // formula, not a dynamic array; a real array stays one.
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::text("Qty")),
            ("B1", Cell::text("Price")),
            ("C1", Cell::text("Amount")),
            ("A2", Cell::number(3.0)),
            ("B2", Cell::number(2.5)),
        ]);
        wb.tables.push(crate::sheet::Table {
            name: "Sales".to_string(),
            sheet: 0,
            range: (0, 0, 1, 2),
            header_rows: 1,
            totals_rows: 0,
            columns: vec!["Qty".into(), "Price".into(), "Amount".into()],
            part: String::new(),
        });
        let mut eng = Engine::new(&wb);
        let scalar = [
            ("C2", "[@Qty]*[@Price]", CellValue::Number(7.5)),
            ("E1", "OFFSET(A2,0,1)+1", CellValue::Number(3.5)),
            ("E2", "INDIRECT(\"A2\")*2", CellValue::Number(6.0)),
            ("E3", "-OFFSET(A2,0,0)", CellValue::Number(-3.0)),
            ("E4", "OFFSET(A2,0,0)>1", CellValue::Bool(true)),
            ("E5", "OFFSET(A2,0,0)&\"x\"", CellValue::Text("3x".into())),
            ("E6", "ABS(OFFSET(A2,0,0))", CellValue::Number(3.0)),
            ("E7", "IF(OFFSET(A2,0,0)>1,1,2)", CellValue::Number(1.0)),
            (
                "E8",
                "OFFSET(A2,0,0)*OFFSET(A2,0,1)",
                CellValue::Number(7.5),
            ),
        ];
        let shaped = [
            ("G1", "SEQUENCE(1)*2", CellValue::Number(2.0)),
            ("G2", "{1}+0", CellValue::Number(1.0)),
            ("G3", "-SEQUENCE(1)", CellValue::Number(-1.0)),
        ];
        for (name, src, _) in scalar.iter().chain(shaped.iter()) {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            eng.set_cell(&mut wb, (0, r, c), Cell::formula(src));
        }
        for (name, src, want) in &scalar {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            assert_eq!(&value_at(&wb, name), want, "{src}");
            assert!(
                !wb.sheets[0].cell(r, c).unwrap().is_dynamic(),
                "{src} marked dynamic"
            );
        }
        for (name, src, want) in &shaped {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            assert_eq!(&value_at(&wb, name), want, "{src}");
            assert!(
                wb.sheets[0].cell(r, c).unwrap().is_dynamic(),
                "{src} not dynamic"
            );
        }
    }

    // ---- #825: spilling arrays pasted, undone and redone ------------------

    /// A1:A3 = 1, 2, 3 and `anchor` at D1, evaluated as a fresh engine sees
    /// it: `=A1:A3*2` spills 2, 4, 6 down D1:D3.
    fn spilling_book(anchor: Cell) -> (Workbook, Engine) {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("D1", anchor),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        (wb, eng)
    }

    /// A legacy CSE block, `{=A1:A3*2}` over D1:D3.
    fn cse_block() -> Cell {
        let mut c = Cell::formula("A1:A3*2");
        c.f_attrs = Some(" t=\"array\" ref=\"D1:D3\"".to_string());
        c
    }

    /// The cells of `r1..=r2` x `c1..=c2` on sheet 0, as a copy takes them.
    fn copy_block(wb: &Workbook, (r1, c1): (u32, u32), (r2, c2): (u32, u32)) -> Vec<Vec<Cell>> {
        (r1..=r2)
            .map(|r| {
                (c1..=c2)
                    .map(|c| wb.sheets[0].cell(r, c).cloned().unwrap_or_default())
                    .collect()
            })
            .collect()
    }

    fn cell_at(wb: &Workbook, name: &str) -> Cell {
        let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
        wb.sheets[0].cell(r, c).cloned().unwrap_or_default()
    }

    /// `anchor` spills (3, 1) with 2, 4, 6 down from it.
    fn assert_spills(wb: &Workbook, anchor: &str) {
        let (r, c) = crate::sheet::parse_cell_name(anchor).unwrap();
        assert_eq!(cell_at(wb, anchor).spill, Some((3, 1)), "{anchor}");
        for (i, want) in [2.0, 4.0, 6.0].into_iter().enumerate() {
            let name = crate::sheet::cell_name(r + i as u32, c);
            assert_eq!(value_at(wb, &name), CellValue::Number(want), "{name}");
        }
    }

    /// Clear D1:D3 as a cut's source clear does.
    fn cut_d1_d3(eng: &mut Engine, wb: &mut Workbook) {
        for r in 0..3 {
            eng.set_cell(wb, (0, r, 3), Cell::default());
        }
    }

    /// A1:A3 = 1, 2, 3 and the evaluated CSE block D1:D3 = 2, 4, 6.
    fn live_cse_wb() -> (Workbook, Engine) {
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("D1", cse_block()),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_spills(&wb, "D1");
        (wb, eng)
    }

    fn col_d_and_d4(wb: &Workbook) -> Vec<CellValue> {
        ["D1", "D2", "D3", "D4"]
            .iter()
            .map(|n| value_at(wb, n))
            .collect()
    }

    #[test]
    fn a_paste_reaching_into_a_cse_block_is_refused_whole() {
        // r7 M2: Excel refuses the whole paste, not just the cells inside the
        // block. A 3-row column pasted at D2 leaves D4 alone too.
        let (mut wb, mut eng) = live_cse_wb();
        let before = wb.sheets[0].cells.clone();
        let column: Vec<Vec<Cell>> = (7..10).map(|n| vec![Cell::number(n as f64)]).collect();
        assert!(eng.refuses_paste(&wb, 0, (1, 3), &column, &[]));
        assert!(!eng.paste_block(&mut wb, 0, (1, 3), &column));
        assert_eq!(wb.sheets[0].cells, before);
        // A formula and a value: the value would change the block, so the
        // formula is not written either.
        let mixed = vec![vec![Cell::number(5.0)], vec![Cell::formula("7")]];
        assert!(!eng.paste_block(&mut wb, 0, (1, 3), &mixed));
        assert_eq!(wb.sheets[0].cells, before);
        // Its own values pasted back over it, without its anchor, are still
        // a write into part of it (r8): refused, as in Excel.
        let same = vec![vec![Cell::number(4.0)], vec![Cell::number(6.0)]];
        assert!(!eng.paste_block(&mut wb, 0, (1, 3), &same));
        assert_eq!(wb.sheets[0].cells, before);
    }

    #[test]
    fn a_group_over_part_of_a_cse_block_is_refused_whole() {
        // r7 M2: a fill, a range entry or a replace-all through `set_cells`.
        let (mut wb, mut eng) = live_cse_wb();
        let before = wb.sheets[0].cells.clone();
        let group = vec![(3, 3, Cell::number(8.0)), (2, 3, Cell::number(9.0))];
        assert!(eng.refuses(&wb, 0, &group));
        assert!(!eng.set_cells(&mut wb, 0, group));
        assert_eq!(wb.sheets[0].cells, before);
        // Clearing part of it is refused too, but not clearing all of it.
        let clear = |rows: std::ops::Range<u32>| -> CellEdits {
            rows.map(|r| (r, 3, Cell::default())).collect()
        };
        assert!(!eng.set_cells(&mut wb, 0, clear(1..4)));
        assert_eq!(wb.sheets[0].cells, before);
        // A drag fill writes its area itself: asked by area.
        assert!(eng.refuses_area(&wb, 0, (1, 2, 4, 3)));
        assert!(!eng.refuses_area(&wb, 0, (0, 2, 4, 3)));
        assert!(!eng.refuses_area(&wb, 0, (3, 3, 5, 3)));
        assert!(eng.set_cells(&mut wb, 0, clear(0..4)));
        assert_eq!(col_d_and_d4(&wb), vec![CellValue::Empty; 4]);
    }

    #[test]
    fn a_group_that_replaces_the_cse_anchor_is_not_refused() {
        // r7 M2: with the anchor in the group, the block goes; the anchor is
        // written first whatever order the group came in, so the values
        // below it land.
        let n = |v: f64| CellValue::Number(v);
        let (mut wb, mut eng) = live_cse_wb();
        let group = vec![
            (2, 3, Cell::number(9.0)),
            (1, 3, Cell::number(8.0)),
            (0, 3, Cell::number(1.0)),
        ];
        assert!(!eng.refuses(&wb, 0, &group));
        assert!(eng.set_cells(&mut wb, 0, group));
        assert_eq!(
            col_d_and_d4(&wb),
            vec![n(1.0), n(8.0), n(9.0), CellValue::Empty]
        );
        assert_eq!(cell_at(&wb, "D1").spill, None);

        // The same text again on the anchor keeps it a block: not a
        // replacement, so the values are refused with it.
        let (mut wb, mut eng) = live_cse_wb();
        let group = vec![(0, 3, cse_block()), (1, 3, Cell::number(8.0))];
        assert!(eng.refuses(&wb, 0, &group));

        // A paste over the anchor replaces the block: column D1:D3 = 5, 6, =A1.
        let block = vec![
            vec![Cell::number(5.0)],
            vec![Cell::number(6.0)],
            vec![Cell::formula("A1")],
        ];
        assert!(eng.paste_block(&mut wb, 0, (0, 3), &block));
        assert_eq!(
            col_d_and_d4(&wb),
            vec![n(5.0), n(6.0), n(1.0), CellValue::Empty]
        );
    }

    #[test]
    fn a_pasted_same_text_anchor_keeps_the_block_so_the_rest_is_refused() {
        // r8 M1: D1:D2 of a live block copied ([its formula; 4]) and pasted
        // over D1:D2 of a block with the same formula text. Typed there, the
        // same text keeps the block, so D2 is a write into part of it: the
        // paste is refused whole. On another sheet, and on the same sheet
        // after the block's inputs changed.
        let (mut wb, _) = live_cse_wb();
        let clip = copy_block(&wb, (0, 3), (1, 3));
        let mut sheet2 = Sheet {
            name: "Sheet2".to_string(),
            ..Sheet::default()
        };
        for r in 0..3 {
            sheet2.set_cell(r, 0, Cell::number(f64::from(r + 5)));
        }
        sheet2.set_cell(0, 3, cse_block());
        wb.sheets.push(sheet2);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        let before = wb.sheets[1].cells.clone();
        assert!(eng.refuses_paste(&wb, 1, (0, 3), &clip, &[]));
        assert!(!eng.paste_block(&mut wb, 1, (0, 3), &clip));
        assert_eq!(wb.sheets[1].cells, before);

        set(&mut eng, &mut wb, "A1", Cell::number(10.0));
        let before = wb.sheets[0].cells.clone();
        assert!(!eng.paste_block(&mut wb, 0, (0, 3), &clip));
        assert_eq!(wb.sheets[0].cells, before);
        assert_eq!(value_at(&wb, "D1"), CellValue::Number(20.0));
    }

    #[test]
    fn a_group_writing_a_held_value_into_a_cse_block_is_refused_whole() {
        // r8 (second Immaterial): {=A1:A3*1} over D1:D3 = 1, 2, 3, and the
        // group [A2 = 5, D2 = 2]. D2 already holds 2, but writing it is still
        // a write into part of the array: refused whole, A2 untouched.
        let mut c = Cell::formula("A1:A3*1");
        c.f_attrs = Some(" t=\"array\" ref=\"D1:D3\"".to_string());
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("D1", c),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        let before = wb.sheets[0].cells.clone();
        let group = vec![(1, 0, Cell::number(5.0)), (1, 3, Cell::number(2.0))];
        assert!(!eng.set_cells(&mut wb, 0, group));
        assert_eq!(wb.sheets[0].cells, before);
    }

    #[test]
    fn a_fill_whose_source_holds_the_anchor_is_refused() {
        // r8 M2: D1 = 5 and a block anchored at D2 over D2:D4; D1:D2 filled
        // down to D6 writes D3:D6 only. The anchor lies in the source, which
        // the fill never rewrites, so it is not replaced: refused.
        let mut d2 = Cell::formula("A1*2");
        d2.f_attrs = Some(" t=\"array\" ref=\"D2:D4\"".to_string());
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("D1", Cell::number(5.0)),
            ("D2", d2),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(value_at(&wb, "D4"), CellValue::Number(2.0));
        assert!(eng.refuses_area(&wb, 0, (2, 3, 5, 3)));
        // Filled from D2 itself, over the whole block: still part of it.
        assert!(eng.refuses_area(&wb, 0, (2, 3, 3, 3)));
        // Below the block (D5:D6): nothing of it is written.
        assert!(!eng.refuses_area(&wb, 0, (4, 3, 5, 3)));
        // A destination that takes the whole block replaces it.
        assert!(!eng.refuses_area(&wb, 0, (1, 3, 5, 3)));
    }

    #[test]
    fn a_fill_over_the_anchor_but_not_the_whole_block_is_refused() {
        // r9: block D3:D8, fill destination D3:D6. `autofill` would replace
        // the anchor but leave D7:D8 of the old block behind: refused. All
        // of D3:D8 in the destination is not.
        let mut d3 = Cell::formula("A1*2");
        d3.f_attrs = Some(" t=\"array\" ref=\"D3:D8\"".to_string());
        let mut wb = wb_one_sheet(&[("A1", Cell::number(1.0)), ("D3", d3)]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert!(eng.refuses_area(&wb, 0, (2, 3, 5, 3)));
        assert!(!eng.refuses_area(&wb, 0, (2, 3, 7, 3)));
    }

    #[test]
    fn undoing_a_cse_paste_in_reverse_order_puts_every_cell_back() {
        // r7 M3: Sheet2 D1:D3 = 1, 2, 3; a CSE block pasted in place from
        // Sheet1; the undo hands its snapshot over in reverse (members before
        // the anchor), which must restore all three, never refuse them.
        let n = |v: f64| CellValue::Number(v);
        let (mut wb, _) = live_cse_wb();
        let mut sheet2 = Sheet {
            name: "Sheet2".to_string(),
            ..Sheet::default()
        };
        for r in 0..3 {
            sheet2.set_cell(r, 3, Cell::number(f64::from(r + 1)));
        }
        wb.sheets.push(sheet2);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        let before: CellEdits = (0..3)
            .map(|r| (r, 3, wb.sheets[1].cell(r, 3).cloned().unwrap()))
            .collect();
        let block = copy_block(&wb, (0, 3), (2, 3));
        assert!(eng.paste_block(&mut wb, 1, (0, 3), &block));
        let on2 = |wb: &Workbook| -> Vec<CellValue> {
            (0..3)
                .map(|r| {
                    wb.sheets[1]
                        .cell(r, 3)
                        .map(|c| c.value.clone())
                        .unwrap_or_default()
                })
                .collect()
        };
        assert_eq!(wb.sheets[1].cell(0, 3).unwrap().spill, Some((3, 1)));
        let undo: CellEdits = before.into_iter().rev().collect();
        eng.restore_cells(&mut wb, 1, &undo);
        assert_eq!(on2(&wb), vec![n(1.0), n(2.0), n(3.0)]);
        assert_eq!(wb.sheets[1].cell(0, 3).unwrap().formula, None);
    }

    #[test]
    fn a_snapshot_restored_into_a_cse_block_is_not_refused() {
        // r7 M3: restore_cell never refuses: the snapshot's cell lands (its
        // style shows it), and the block, which stays, refills its value.
        let (mut wb, mut eng) = live_cse_wb();
        let snap = Cell {
            style: 3,
            ..Cell::number(99.0)
        };
        eng.restore_cell(&mut wb, (0, 1, 3), snap.clone());
        assert_spills(&wb, "D1");
        assert_eq!(cell_at(&wb, "D2").style, 3);
        // Typed, the same cell is refused whole, style included.
        let (mut wb, mut eng) = live_cse_wb();
        assert!(!eng.set_cell(&mut wb, (0, 1, 3), snap));
        assert_eq!(cell_at(&wb, "D2").style, 0);
        assert!(!eng.set_cell(&mut wb, (0, 1, 3), Cell::number(99.0)));
        // The value it holds already is refused too (r8: no value exemption).
        assert!(!eng.set_cell(&mut wb, (0, 1, 3), Cell::number(4.0)));
    }

    #[test]
    fn pasting_a_spilling_cse_block_in_place_keeps_it_spilling() {
        // #825 AC1: copy or cut D1:D3, paste at D1. One cell at a time through
        // set_cell, the 4 written into D2 blocks D1 (#SPILL!).
        for cut in [false, true] {
            let (mut wb, mut eng) = spilling_book(cse_block());
            assert_spills(&wb, "D1");
            let block = copy_block(&wb, (0, 3), (2, 3));
            if cut {
                cut_d1_d3(&mut eng, &mut wb);
            } else {
                // r9: restored in place, the block replaces its own anchor,
                // so the paste over the live block is not refused.
                assert!(!eng.refuses_paste(&wb, 0, (0, 3), &block, &[]));
            }
            assert!(eng.paste_block(&mut wb, 0, (0, 3), &block));
            assert_spills(&wb, "D1");
            let d1 = cell_at(&wb, "D1");
            assert_eq!(
                d1.f_attrs.as_deref(),
                Some(" t=\"array\" ref=\"D1:D3\""),
                "cut {cut}"
            );
            assert!(cell_at(&wb, "D2").formula.is_none());
        }
    }

    #[test]
    fn pasting_a_typed_dynamic_array_block_keeps_it_spilling() {
        // #825 AC2: a typed dynamic array (no `f_attrs`) copied D1:D3 and
        // pasted back in place (set_cell's same-formula arm), cut and pasted
        // back (its typed arm), and pasted at F1.
        for (cut, at) in [(false, "D1"), (true, "D1"), (false, "F1")] {
            let (mut wb, mut eng) = spilling_book(Cell::default());
            set(&mut eng, &mut wb, "D1", Cell::formula("A1:A3*2"));
            assert_spills(&wb, "D1");
            let block = copy_block(&wb, (0, 3), (2, 3));
            if cut {
                cut_d1_d3(&mut eng, &mut wb);
            }
            let (r, c) = crate::sheet::parse_cell_name(at).unwrap();
            eng.paste_block(&mut wb, 0, (r, c), &block);
            assert_spills(&wb, at);
            let anchor = cell_at(&wb, at);
            assert!(
                anchor.f_attrs.is_none() && anchor.is_dynamic(),
                "{at} cut {cut}"
            );
        }
    }

    #[test]
    fn a_loaded_dynamic_array_pasted_elsewhere_spills() {
        // #825 AC2: Excel's dynamic array (`t="array"` and a `cm`) copied with
        // its spill and pasted at F1 is typed there and spills, its `cm` gone
        // (save resolves one) and still a dynamic array.
        let mut da = cse_block();
        da.meta = Some(Box::new(CellMeta {
            cm: Some("1".into()),
            ..CellMeta::default()
        }));
        let (mut wb, mut eng) = spilling_book(da);
        assert_spills(&wb, "D1");
        let block = copy_block(&wb, (0, 3), (2, 3));
        eng.paste_block(&mut wb, 0, (0, 5), &block);
        assert_spills(&wb, "F1");
        let f1 = cell_at(&wb, "F1");
        let m = f1.meta.as_deref().unwrap();
        assert!(f1.f_attrs.is_none() && m.cm.is_none() && m.dynamic && m.modern);
    }

    #[test]
    fn a_pasted_anchor_does_not_claim_cells_below_its_target() {
        // #825 AC10: a spilling anchor copied alone keeps its source's
        // `spill`; set_cell must not let it take over the 99 below F1.
        let (mut wb, mut eng) = spilling_book(Cell::default());
        set(&mut eng, &mut wb, "D1", Cell::formula("A1:A3*2"));
        set(&mut eng, &mut wb, "F2", Cell::number(99.0));
        let anchor = cell_at(&wb, "D1");
        assert_eq!(anchor.spill, Some((3, 1)));
        set(&mut eng, &mut wb, "F1", anchor);
        assert_eq!(value_at(&wb, "F2"), CellValue::Number(99.0));
        assert_eq!(value_at(&wb, "F1"), CellValue::Error("#SPILL!".into()));
        assert_eq!(cell_at(&wb, "F1").spill, None);
    }

    /// Before/after snapshots of the cells a paste of `block` at `at` writes.
    type Snapshot = Vec<(u32, u32, Cell)>;

    /// Paste `block` at `at` the way xlsxy and gridwasm record it: every
    /// `before` taken ahead of the paste and every `after` once it is done.
    fn recorded_paste(
        eng: &mut Engine,
        wb: &mut Workbook,
        at: (u32, u32),
        block: &[Vec<Cell>],
    ) -> (Snapshot, Snapshot) {
        let keys: Vec<(u32, u32)> = block
            .iter()
            .enumerate()
            .flat_map(|(dr, row)| {
                (0..row.len()).map(move |dc| (at.0 + dr as u32, at.1 + dc as u32))
            })
            .collect();
        let snap = |wb: &Workbook| -> Snapshot {
            keys.iter()
                .map(|&(r, c)| (r, c, wb.sheets[0].cell(r, c).cloned().unwrap_or_default()))
                .collect()
        };
        let before = snap(wb);
        eng.paste_block(wb, 0, at, block);
        (before, snap(wb))
    }

    fn assert_snapshot(wb: &Workbook, snap: &Snapshot) {
        for (r, c, cell) in snap {
            let now = wb.sheets[0].cell(*r, *c).cloned().unwrap_or_default();
            assert_eq!(now, *cell, "({r}, {c})");
        }
    }

    #[test]
    fn restore_cells_restores_a_spill_anchor_after_its_members() {
        // #825 AC3: undo restores the befores in reverse (anchor first, then
        // its spilled 4 and 6, which would block it one by one); redo the
        // afters in order. Both leave D1 spilling, exactly as recorded.
        // The CSE block keeps its `<f>` attributes both ways.
        for cse in [true, false] {
            let (mut wb, mut eng) = if cse {
                spilling_book(cse_block())
            } else {
                let (mut wb, mut eng) = spilling_book(Cell::default());
                set(&mut eng, &mut wb, "D1", Cell::formula("A1:A3*2"));
                (wb, eng)
            };
            let f_attrs = cse.then_some(" t=\"array\" ref=\"D1:D3\"");
            assert_eq!(cell_at(&wb, "D1").f_attrs.as_deref(), f_attrs);
            let block = copy_block(&wb, (0, 3), (2, 3));
            let (before, after) = recorded_paste(&mut eng, &mut wb, (0, 3), &block);
            let undo: Snapshot = before.iter().rev().cloned().collect();
            eng.restore_cells(&mut wb, 0, &undo);
            assert_spills(&wb, "D1");
            assert_snapshot(&wb, &before);
            assert_eq!(cell_at(&wb, "D1").f_attrs.as_deref(), f_attrs, "undo");
            eng.restore_cells(&mut wb, 0, &after);
            assert_spills(&wb, "D1");
            assert_snapshot(&wb, &after);
            assert_eq!(cell_at(&wb, "D1").f_attrs.as_deref(), f_attrs, "redo");
        }
    }

    #[test]
    fn undoing_a_paste_over_a_live_spill_restores_the_spill() {
        // #825 AC3: constants pasted over D1:D3 replace the dynamic array;
        // undo brings back the anchor and its spill, redo the constants.
        let (mut wb, mut eng) = spilling_book(Cell::default());
        set(&mut eng, &mut wb, "D1", Cell::formula("A1:A3*2"));
        let block: Vec<Vec<Cell>> = (7..10).map(|n| vec![Cell::number(n as f64)]).collect();
        let (before, after) = recorded_paste(&mut eng, &mut wb, (0, 3), &block);
        assert_eq!(value_at(&wb, "D2"), CellValue::Number(8.0));
        let undo: Snapshot = before.iter().rev().cloned().collect();
        eng.restore_cells(&mut wb, 0, &undo);
        assert_spills(&wb, "D1");
        assert_snapshot(&wb, &before);
        eng.restore_cells(&mut wb, 0, &after);
        assert_snapshot(&wb, &after);
        assert!(cell_at(&wb, "D1").formula.is_none());
    }

    #[test]
    fn a_same_formula_pasted_over_a_cell_keeps_no_foreign_vm() {
        // #825 r1 p4: a clone of the same formula from another workbook lands
        // on this cell's own formula (set_cell's same-formula arm). Its `vm`
        // indexes the other workbook's metadata: this cell's own (none) wins.
        let (mut wb, mut eng) = spilling_book(Cell::default());
        set(&mut eng, &mut wb, "D1", Cell::formula("A1:A3*2"));
        let mut clone = cell_at(&wb, "D1");
        let m = clone.meta.get_or_insert_default();
        m.vm = Some(("9".into(), CellValue::Number(2.0)));
        m.vm_body = Some("2".into());
        set(&mut eng, &mut wb, "D1", clone);
        let m = cell_at(&wb, "D1").meta.unwrap();
        assert_eq!((m.vm, m.vm_body), (None, None));
        assert!(m.modern && m.dynamic);
        assert_spills(&wb, "D1");
    }

    #[test]
    fn a_frozen_anchor_pasted_in_place_keeps_its_copied_spill() {
        // An anchor the engine can't evaluate keeps its cached values: the
        // paste puts them back as its spill rather than blanks.
        let mut anchor = Cell::formula("NOSUCHFN(A1:A3)");
        anchor.f_attrs = Some(" t=\"array\" ref=\"D1:D3\"".to_string());
        anchor.value = CellValue::Number(2.0);
        anchor.spill = Some((3, 1));
        let mut wb = wb_one_sheet(&[
            ("D1", anchor),
            ("D2", Cell::number(4.0)),
            ("D3", Cell::number(6.0)),
        ]);
        let mut eng = Engine::new(&wb);
        let block = copy_block(&wb, (0, 3), (2, 3));
        eng.paste_block(&mut wb, 0, (0, 3), &block);
        assert!(eng.is_unsupported((0, 0, 3)));
        assert_eq!(cell_at(&wb, "D1").spill, Some((3, 1)));
        assert_eq!(value_at(&wb, "D2"), CellValue::Number(4.0));
        assert_eq!(value_at(&wb, "D3"), CellValue::Number(6.0));
    }

    #[test]
    fn pasted_anchor_does_not_keep_source_spill_extent() {
        // #777: a pasted clone of a spill anchor carries the source's extent;
        // the engine must not take the target's neighbours for its own spill.
        let mut wb = wb_one_sheet(&[("B1", Cell::number(1.0))]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "C1", Cell::formula("SEQUENCE(3)"));
        set(&mut eng, &mut wb, "G2", Cell::text("keep"));
        let c1 = wb.sheets[0].cell(0, 2).unwrap().clone();
        assert_eq!(c1.spill, Some((3, 1)));
        set(&mut eng, &mut wb, "G1", c1);
        assert_eq!(value_at(&wb, "G1"), CellValue::Error("#SPILL!".into()));
        assert_eq!(value_at(&wb, "G2"), CellValue::Text("keep".into()));
        assert_eq!(value_at(&wb, "G3"), CellValue::Empty);
        assert_eq!(wb.sheets[0].cell(0, 6).unwrap().spill, None);

        // Pasted where it evaluates to a scalar, it leaves the cells under the
        // source's extent alone.
        set(
            &mut eng,
            &mut wb,
            "D1",
            Cell::formula("IF(B1=1,SEQUENCE(3),0)"),
        );
        set(&mut eng, &mut wb, "K2", Cell::text("x"));
        set(&mut eng, &mut wb, "K3", Cell::text("y"));
        let d1 = wb.sheets[0].cell(0, 3).unwrap().clone();
        assert_eq!(d1.spill, Some((3, 1)));
        let pasted = Cell {
            formula: Some("IF(H1=1,SEQUENCE(3),0)".into()),
            ..d1
        };
        set(&mut eng, &mut wb, "K1", pasted);
        assert_eq!(value_at(&wb, "K1"), CellValue::Number(0.0));
        assert_eq!(value_at(&wb, "K2"), CellValue::Text("x".into()));
        assert_eq!(value_at(&wb, "K3"), CellValue::Text("y".into()));

        // The #725 repro: D1 SEQUENCE(3), F2 = 99, paste D1 at F1.
        let mut wb = wb_one_sheet(&[]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "D1", Cell::formula("SEQUENCE(3)"));
        set(&mut eng, &mut wb, "F2", Cell::number(99.0));
        let d1 = wb.sheets[0].cell(0, 3).unwrap().clone();
        set(&mut eng, &mut wb, "F1", d1);
        assert_eq!(value_at(&wb, "F1"), CellValue::Error("#SPILL!".into()));
        assert_eq!(value_at(&wb, "F2"), CellValue::Number(99.0));
        // Clearing the blocker lets it spill.
        set(&mut eng, &mut wb, "F2", Cell::default());
        assert_eq!(value_at(&wb, "F3"), CellValue::Number(3.0));
    }

    #[test]
    fn restoring_an_anchor_snapshot_does_not_overwrite_neighbours() {
        // An undo/redo snapshot of an anchor carries the extent it had; put
        // back where its spill area is now occupied, it is #SPILL!.
        let mut wb = wb_one_sheet(&[]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "C1", Cell::formula("SEQUENCE(3)"));
        let snap = wb.sheets[0].cell(0, 2).unwrap().clone();
        set(&mut eng, &mut wb, "C1", Cell::default());
        set(&mut eng, &mut wb, "C2", Cell::text("keep"));
        eng.restore_cell(&mut wb, (0, 0, 2), snap.clone());
        assert_eq!(value_at(&wb, "C1"), CellValue::Error("#SPILL!".into()));
        assert_eq!(value_at(&wb, "C2"), CellValue::Text("keep".into()));
        assert_eq!(value_at(&wb, "C3"), CellValue::Empty);
        // Over empty cells it spills again.
        set(&mut eng, &mut wb, "C2", Cell::default());
        eng.restore_cell(&mut wb, (0, 0, 2), snap);
        assert_eq!(value_at(&wb, "C3"), CellValue::Number(3.0));
        assert_eq!(wb.sheets[0].cell(0, 2).unwrap().spill, Some((3, 1)));
    }

    #[test]
    fn blanking_a_spill_child_keeps_the_spill() {
        // An edit inside another anchor's spill clears all of that spill
        // before the anchor recalcs: blanking its last cell (what a pasted
        // spill block does with its children) must not leave a stale value
        // above it to block the anchor.
        let mut wb = wb_one_sheet(&[]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "C1", Cell::formula("SEQUENCE(3)"));
        set(&mut eng, &mut wb, "C3", Cell::default());
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(1.0));
        assert_eq!(value_at(&wb, "C2"), CellValue::Number(2.0));
        assert_eq!(value_at(&wb, "C3"), CellValue::Number(3.0));
        // A value typed into it still blocks the anchor, and nothing stale
        // is left once it goes.
        set(&mut eng, &mut wb, "C3", Cell::number(9.0));
        assert_eq!(value_at(&wb, "C1"), CellValue::Error("#SPILL!".into()));
        assert_eq!(value_at(&wb, "C2"), CellValue::Empty);
        set(&mut eng, &mut wb, "C3", Cell::default());
        assert_eq!(value_at(&wb, "C3"), CellValue::Number(3.0));
    }

    #[test]
    fn typed_maybe_array_formulas_are_dynamic() {
        // #777: a typed formula that could return an array is a dynamic array
        // even when its result is one value; scalar ones are not.
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("A2", Cell::number(2.0)),
            ("A3", Cell::number(3.0)),
            ("B1", Cell::text("A2")),
        ]);
        let mut eng = Engine::new(&wb);
        let cases = [
            (
                "D1",
                "FILTER(A1:A3,A1:A3>5)",
                true,
                CellValue::Error("#CALC!".into()),
            ),
            ("D2", "INDIRECT(B1)", true, CellValue::Number(2.0)),
            (
                "D3",
                "IFERROR(FILTER(A1:A3,A1:A3>5),\"\")",
                true,
                CellValue::Text(String::new()),
            ),
            ("D4", "A1+1", false, CellValue::Number(2.0)),
            ("D5", "SUM(A1:A3)", false, CellValue::Number(6.0)),
            (
                "D6",
                "ROWS(FILTER(A1:A3,A1:A3>1))",
                false,
                CellValue::Number(2.0),
            ),
            ("D7", "@INDIRECT(B1)", false, CellValue::Number(2.0)),
        ];
        for (name, src, _, _) in &cases {
            set(&mut eng, &mut wb, name, Cell::formula(src));
        }
        for (name, src, dynamic, want) in &cases {
            let (r, c) = crate::sheet::parse_cell_name(name).unwrap();
            let cell = wb.sheets[0].cell(r, c).unwrap();
            assert_eq!(&cell.value, want, "{src}");
            assert!(cell.is_modern(), "{src}");
            assert_eq!(cell.is_dynamic(), *dynamic, "{src}");
        }
    }

    #[test]
    fn is_frozen_knows_an_unevaluated_unsupported_formula() {
        // xlsxy opens on cached values: nothing has been evaluated yet.
        let wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("B1", array_formula("PIVOTBY(A1,4)")),
            ("C1", array_formula("SEQUENCE(3)")),
        ]);
        let eng = Engine::new(&wb);
        assert!(!eng.is_unsupported((0, 0, 1)));
        assert!(eng.is_frozen(&wb, (0, 0, 1)));
        assert!(!eng.is_frozen(&wb, (0, 0, 2)));
        assert!(!eng.is_frozen(&wb, (0, 0, 0)));
        // Evaluated, it is known.
        let mut wb = wb;
        let mut eng = eng;
        eng.recalc_all(&mut wb);
        assert!(eng.is_unsupported((0, 0, 1)) && eng.is_frozen(&wb, (0, 0, 1)));
    }

    /// A1 = 1 and a frozen array anchor E1 `PIVOTBY(A1,4)` (`ref="E1:E3"`)
    /// whose cached block E1:E3 is 7/8/9, as xlsxy opens it (on cached
    /// values, nothing evaluated yet).
    fn frozen_block_wb() -> Workbook {
        let mut e1 = array_formula("PIVOTBY(A1,4)");
        e1.f_attrs = Some("t=\"array\" ref=\"E1:E3\"".to_string());
        e1.value = CellValue::Number(7.0);
        e1.spill = Some((3, 1));
        wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("E1", e1),
            ("E2", Cell::number(8.0)),
            ("E3", Cell::number(9.0)),
        ])
    }

    fn col_e(wb: &Workbook) -> Vec<CellValue> {
        ["E1", "E2", "E3"].iter().map(|n| value_at(wb, n)).collect()
    }

    #[test]
    fn recommitting_a_frozen_formula_keeps_its_cached_values() {
        // #840 (r3-pre-frozen-anchor-restyle): the same text again on a
        // formula the engine can't evaluate is no edit. The editor's cell
        // comes with no value, and a frozen formula never recomputes one:
        // the anchor, its block and its extent stay; only the style is taken.
        let n = |v: f64| CellValue::Number(v);
        for evaluated in [false, true] {
            let mut wb = frozen_block_wb();
            let mut eng = Engine::new(&wb);
            if evaluated {
                eng.recalc_all(&mut wb);
            }
            let before = cell_at(&wb, "E1");
            let entered = Cell {
                style: 3,
                ..Cell::formula("PIVOTBY(A1,4)")
            };
            set(&mut eng, &mut wb, "E1", entered);
            assert_eq!(col_e(&wb), vec![n(7.0), n(8.0), n(9.0)], "{evaluated}");
            let e1 = cell_at(&wb, "E1");
            assert_eq!(e1.spill, Some((3, 1)), "{evaluated}");
            assert_eq!(e1.style, 3, "{evaluated}");
            assert_eq!(Cell { style: 0, ..e1 }, before, "{evaluated}");
            assert!(eng.is_frozen(&wb, (0, 0, 4)));
        }
        // A frozen scalar keeps its cached value too.
        let mut b1 = Cell::formula("PIVOTBY(A1,4)");
        b1.value = n(5.0);
        let mut wb = wb_one_sheet(&[("A1", Cell::number(1.0)), ("B1", b1)]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "B1", Cell::formula("PIVOTBY(A1,4)"));
        assert_eq!(value_at(&wb, "B1"), n(5.0));
    }

    #[test]
    fn a_new_formula_on_a_frozen_anchor_still_clears_its_block() {
        // #840 guard: other text is typed, as before; a live anchor
        // re-entered unchanged re-spills.
        let mut wb = frozen_block_wb();
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "E1", Cell::formula("PIVOTBY(A1,5)"));
        assert_eq!(value_at(&wb, "E2"), CellValue::Empty);
        assert_eq!(value_at(&wb, "E3"), CellValue::Empty);
        assert_eq!(cell_at(&wb, "E1").spill, None);

        let mut wb = wb_one_sheet(&[]);
        let mut eng = Engine::new(&wb);
        set(&mut eng, &mut wb, "C1", Cell::formula("SEQUENCE(3)"));
        set(&mut eng, &mut wb, "C1", Cell::formula("SEQUENCE(3)"));
        assert_eq!(value_at(&wb, "C3"), CellValue::Number(3.0));
        assert_eq!(cell_at(&wb, "C1").spill, Some((3, 1)));
    }

    #[test]
    fn a_blank_and_a_value_in_one_frozen_block_are_order_independent() {
        // #840 (r4-m1): a blank landing in a frozen block is a no-op only
        // while the block is whole; with a value put into the same block by
        // the same group, it clears its cell, in either order. Paste [blank;
        // 5] at E2, and the same group reversed.
        let n = |v: f64| CellValue::Number(v);
        let want = vec![n(7.0), CellValue::Empty, n(5.0)];
        let mut wb = frozen_block_wb();
        let mut eng = Engine::new(&wb);
        eng.paste_block(
            &mut wb,
            0,
            (1, 4),
            &[vec![Cell::default()], vec![Cell::number(5.0)]],
        );
        assert_eq!(col_e(&wb), want, "paste");
        assert_eq!(cell_at(&wb, "E1").spill, None, "paste");
        for order in [[1u32, 2], [2, 1]] {
            let mut wb = frozen_block_wb();
            let mut eng = Engine::new(&wb);
            let changes = order
                .iter()
                .map(|&r| {
                    let cell = if r == 1 {
                        Cell::default()
                    } else {
                        Cell::number(5.0)
                    };
                    (r, 4, cell)
                })
                .collect();
            eng.set_cells(&mut wb, 0, changes);
            assert_eq!(col_e(&wb), want, "{order:?}");
            assert_eq!(cell_at(&wb, "E1").spill, None, "{order:?}");
        }
        // Blanks alone, or with a value put back as it was, keep the block.
        let cached = vec![n(7.0), n(8.0), n(9.0)];
        for third in [Cell::default(), Cell::number(9.0)] {
            let mut wb = frozen_block_wb();
            let mut eng = Engine::new(&wb);
            eng.set_cells(&mut wb, 0, vec![(1, 4, Cell::default()), (2, 4, third)]);
            assert_eq!(col_e(&wb), cached);
            assert_eq!(cell_at(&wb, "E1").spill, Some((3, 1)));
        }
    }

    #[test]
    fn a_blank_group_over_a_live_spill_evaluates_its_anchor_once() {
        // #840 r1 M1: deleting the cells of a loaded live spill (an array
        // `ref` the engine evaluates) goes as it did cell by cell, without
        // evaluating its anchor once per blank: `set_cells` finds the frozen
        // blocks once, and `put_cell` asks `is_frozen` of an anchor the
        // engine has evaluated since (it re-spills after each blank) without
        // evaluating it again.
        let load = || {
            let mut e1 = array_formula("SEQUENCE(50)");
            e1.f_attrs = Some("t=\"array\" ref=\"E1:E50\"".to_string());
            e1.value = CellValue::Number(1.0);
            e1.spill = Some((50, 1));
            let mut wb = wb_one_sheet(&[("E1", e1)]);
            for r in 1..50u32 {
                wb.sheets[0].set_cell(r, 4, Cell::number(f64::from(r + 1)));
            }
            let eng = Engine::new(&wb);
            (wb, eng)
        };
        let blanks = || {
            (1..50u32)
                .map(|r| (r, 4, Cell::default()))
                .collect::<Vec<_>>()
        };
        let (mut one_by_one, mut eng) = load();
        for (r, c, cell) in blanks() {
            eng.set_cell(&mut one_by_one, (0, r, c), cell);
        }
        let (mut wb, mut eng) = load();
        FROZEN_EVALS.with(|n| n.set(0));
        eng.set_cells(&mut wb, 0, blanks());
        // One for the frozen blocks, one where the first blank meets the
        // still unevaluated anchor (`put_cell`).
        let evals = FROZEN_EVALS.with(StdCell::get);
        assert!(evals <= 2, "{evals}");
        assert_eq!(wb.sheets[0].cells, one_by_one.sheets[0].cells);
    }

    #[test]
    fn a_frozen_anchor_pasted_over_the_same_formula_replaces_its_block() {
        // #840 r1 m2: a pasted anchor is typed, not taken as a re-entry of
        // the frozen formula already there: the copied block (G1:G3) replaces
        // the taller one at E1:E4.
        let n = |v: f64| CellValue::Number(v);
        let frozen = |r: &str, value: f64, h: u32| {
            let mut a = array_formula("PIVOTBY($A$1,4)");
            a.f_attrs = Some(format!("t=\"array\" ref=\"{r}\""));
            a.value = n(value);
            a.spill = Some((h, 1));
            a
        };
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("E1", frozen("E1:E4", 7.0, 4)),
            ("E2", Cell::number(8.0)),
            ("E3", Cell::number(9.0)),
            ("E4", Cell::number(10.0)),
            ("G1", frozen("G1:G3", 1.0, 3)),
            ("G2", Cell::number(2.0)),
            ("G3", Cell::number(3.0)),
        ]);
        let mut eng = Engine::new(&wb);
        let block = copy_block(&wb, (0, 6), (2, 6));
        eng.paste_block(&mut wb, 0, (0, 4), &block);
        let col: Vec<CellValue> = ["E1", "E2", "E3", "E4"]
            .iter()
            .map(|c| value_at(&wb, c))
            .collect();
        assert_eq!(col, vec![n(1.0), n(2.0), n(3.0), CellValue::Empty]);
        assert_eq!(cell_at(&wb, "E1").spill, Some((3, 1)));
    }

    #[test]
    fn a_blank_group_away_from_array_anchors_evaluates_none() {
        // #840 r2 M1: only an anchor whose block a blank lands in is asked
        // whether it is frozen. Loaded arrays (a one-cell CSE, a 3-row CSE, a
        // dynamic array) elsewhere on the sheet are not evaluated by a Delete
        // in A10:A12.
        let array = |src: &str, r: &str, h: u32| {
            let mut a = array_formula(src);
            a.f_attrs = Some(format!("t=\"array\" ref=\"{r}\""));
            a.spill = Some((h, 1));
            a
        };
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(1.0)),
            ("D5", array("SUM(A1:A3)", "D5", 1)),
            ("H1", array("A1:A3*2", "H1:H3", 3)),
            ("J1", array("SEQUENCE(2)", "J1:J2", 2)),
            ("A10", Cell::number(1.0)),
            ("A11", Cell::number(2.0)),
            ("A12", Cell::number(3.0)),
        ]);
        let mut eng = Engine::new(&wb);
        FROZEN_EVALS.with(|n| n.set(0));
        let blanks = (9..12u32).map(|r| (r, 0, Cell::default())).collect();
        eng.set_cells(&mut wb, 0, blanks);
        assert_eq!(FROZEN_EVALS.with(StdCell::get), 0);
        assert_eq!(value_at(&wb, "A11"), CellValue::Empty);
        // #840 r3 m1: the same text again on an evaluated, supported
        // formula is not evaluated to see whether it is frozen.
        set(&mut eng, &mut wb, "B1", Cell::formula("SEQUENCE(2)"));
        FROZEN_EVALS.with(|n| n.set(0));
        set(&mut eng, &mut wb, "B1", Cell::formula("SEQUENCE(2)"));
        assert_eq!(FROZEN_EVALS.with(StdCell::get), 0);
        assert_eq!(value_at(&wb, "B2"), CellValue::Number(2.0));
    }

    #[test]
    fn restoring_a_mixed_group_into_a_frozen_block_ends_as_the_group_did() {
        // #840 r2 M2: a redo puts back the group's after-snapshot as its
        // undo group lists it, the edited keys and then the frozen anchor
        // they lie in: E2 blank, E3 5, E1 (no extent now). Over the whole
        // frozen block the blank would be a no-op ahead of the 5; it goes
        // last, as in `set_cells`.
        let n = |v: f64| CellValue::Number(v);
        let mut wb = frozen_block_wb();
        let mut eng = Engine::new(&wb);
        eng.set_cells(
            &mut wb,
            0,
            vec![(1, 4, Cell::default()), (2, 4, Cell::number(5.0))],
        );
        let want = vec![n(7.0), CellValue::Empty, n(5.0)];
        assert_eq!(col_e(&wb), want);
        let after: Vec<(u32, u32, Cell)> = (0..3u32)
            .map(|r| (r, 4, wb.sheets[0].cell(r, 4).cloned().unwrap_or_default()))
            .collect();
        let after = [after[1].clone(), after[2].clone(), after[0].clone()];
        let mut wb = frozen_block_wb();
        let mut eng = Engine::new(&wb);
        eng.restore_cells(&mut wb, 0, &after);
        assert_eq!(col_e(&wb), want);
        assert_eq!(cell_at(&wb, "E1").spill, None);
    }

    #[test]
    fn retyping_a_formula_frozen_by_its_input_recomputes_it() {
        // #840 r2 m1: INDIRECT asked for R1C1 is beyond the engine, so C1
        // keeps its cached value. Once B2 asks for A1 style, the same text
        // typed again is evaluated: only a formula still frozen is kept.
        let mut c1 = Cell::formula("INDIRECT(B1,B2)");
        c1.value = CellValue::Number(42.0);
        let mut wb = wb_one_sheet(&[
            ("A1", Cell::number(5.0)),
            ("B1", Cell::text("A1")),
            (
                "B2",
                Cell {
                    value: CellValue::Bool(false),
                    ..Cell::default()
                },
            ),
            ("C1", c1),
        ]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert!(eng.is_unsupported((0, 0, 2)));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(42.0));
        // Still frozen: retyped, it keeps its cached value.
        set(&mut eng, &mut wb, "C1", Cell::formula("INDIRECT(B1,B2)"));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(42.0));
        set(
            &mut eng,
            &mut wb,
            "B2",
            Cell {
                value: CellValue::Bool(true),
                ..Cell::default()
            },
        );
        set(&mut eng, &mut wb, "C1", Cell::formula("INDIRECT(B1,B2)"));
        assert_eq!(value_at(&wb, "C1"), CellValue::Number(5.0));
    }

    #[test]
    fn fill_cse_sees_a_spill_begun_earlier_in_the_pass() {
        // #846 AC5 (r6-m1): the pass's index of spills is built by the first
        // block that needs it (X at A1, over a loaded A2), before the dynamic
        // array at C2 spills down C2:C4. The block at B3, filled after it,
        // must still find that spill over C3 and leave it alone.
        let mut x = Cell::formula("5");
        x.f_attrs = Some(" t=\"array\" ref=\"A1:A2\"".to_string());
        x.spill = Some((2, 1));
        let mut d = Cell::formula("SEQUENCE(3)");
        d.f_attrs = Some(" t=\"array\" ref=\"C2\"".to_string());
        d.meta = Some(Box::new(CellMeta {
            cm: Some("1".into()),
            ..CellMeta::default()
        }));
        let mut y = Cell::formula("7");
        y.f_attrs = Some(" t=\"array\" ref=\"B3:C3\"".to_string());
        let mut wb = wb_one_sheet(&[("A1", x), ("A2", Cell::number(5.0)), ("C2", d), ("B3", y)]);
        let mut eng = Engine::new(&wb);
        eng.recalc_all(&mut wb);
        assert_eq!(cell_at(&wb, "C2").spill, Some((3, 1)));
        assert_eq!(
            ["C2", "C3", "C4"].map(|n| value_at(&wb, n)),
            [1.0, 2.0, 3.0].map(CellValue::Number)
        );
        assert_eq!(value_at(&wb, "B3"), CellValue::Number(7.0));
        assert_eq!(cell_at(&wb, "B3").spill, None);
    }

    #[test]
    fn noting_a_spill_again_does_not_grow_the_pass_index() {
        // #846 r1 m2: two anchors over the same rows that re-spill turn by
        // turn (a circle's iterations, the spill post-passes) leave each row
        // listing each anchor once.
        let mut wb = wb_one_sheet(&[]);
        for (c, h) in [(0, 3), (1, 3)] {
            let mut a = Cell::formula("1");
            a.spill = Some((h, 1));
            wb.sheets[0].cells.insert((0, c), a);
        }
        let mut eng = Engine::default();
        eng.foreign_spills(&wb.sheets[0], (0, 0, 5), (1, 1));
        for _ in 0..50 {
            eng.note_spill((0, 0, 0), 3);
            eng.note_spill((0, 0, 1), 3);
        }
        eng.note_spill((0, 0, 1), 4);
        let rows = &eng.pass_anchors[&0].rows;
        assert_eq!(rows[&0], vec![(0, 0), (0, 1)]);
        assert_eq!(rows[&2], vec![(0, 0), (0, 1)]);
        assert_eq!(rows[&3], vec![(0, 1)]);
    }

    /// #846 AC6: `rows` one-row CSE blocks `{=E:G*2}` over A:C, their
    /// sources in E:G, ten filler columns, and one tall CSE block down J
    /// (an extent over every row), all with the cached values a fill
    /// writes, as `--recalc` loads them.
    fn many_cse_blocks(rows: u32) -> Workbook {
        let mut sheet = Sheet {
            name: "Sheet1".to_string(),
            ..Sheet::default()
        };
        let name = crate::sheet::cell_name;
        for r in 0..rows {
            let n = r + 1;
            let mut a = Cell::formula(&format!("E{n}:G{n}*2"));
            a.f_attrs = Some(format!(" t=\"array\" ref=\"A{n}:C{n}\""));
            a.spill = Some((1, 3));
            for j in 0..3 {
                let v = f64::from(r + j);
                sheet.cells.insert((r, 4 + j), Cell::number(v));
                let mut out = if j == 0 { a.clone() } else { Cell::default() };
                out.value = CellValue::Number(2.0 * v);
                sheet.cells.insert((r, j), out);
            }
            for j in 10..20 {
                sheet.cells.insert((r, j), Cell::number(f64::from(j)));
            }
            sheet.cells.insert((r, 9), Cell::number(f64::from(r)));
        }
        let tall = sheet.cells.get_mut(&(0, 9)).unwrap();
        *tall = Cell {
            value: CellValue::Number(0.0),
            ..Cell::formula(&format!("E1:E{rows}"))
        };
        tall.f_attrs = Some(format!(" t=\"array\" ref=\"J1:{}\"", name(rows - 1, 9)));
        tall.spill = Some((rows, 1));
        Workbook {
            sheets: vec![sheet],
            ..Workbook::default()
        }
    }

    #[test]
    fn recalc_of_many_cse_blocks_is_not_quadratic() {
        // #846 AC6 (r6-m1): finding the other anchors over a block used to
        // walk every cell above it, so this recalc was O(blocks × cells).
        let mut wb = many_cse_blocks(8000);
        let mut eng = Engine::new(&wb);
        let start = std::time::Instant::now();
        eng.recalc_all(&mut wb);
        let took = start.elapsed();
        eprintln!("recalc_all of 8000 CSE blocks: {took:?}");
        assert_eq!(value_at(&wb, "C8000"), CellValue::Number(2.0 * 8001.0));
        assert_eq!(cell_at(&wb, "A8000").spill, Some((1, 3)));
        assert_eq!(cell_at(&wb, "J1").spill, Some((8000, 1)));
        assert_eq!(value_at(&wb, "J8000"), CellValue::Number(7999.0));
        assert!(took < std::time::Duration::from_secs(3), "{took:?}");
    }
}
