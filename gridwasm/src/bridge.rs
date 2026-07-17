//! The host-agnostic spreadsheet session: everything the wasm ABI exposes,
//! written as plain Rust so it can be unit-tested natively
//! (`cargo test -p gridwasm`). Mirrors `docxwasm::bridge` in shape.

use gridcore::edit::parse_input;
use gridcore::engine::Engine;
use gridcore::sheet::{Align, Cell, CellValue, DefinedName, Sheet, cell_name, format_with};
use gridcore::xlsx::{SheetPackage, load_xlsx};

use crate::json;

/// One undoable action: cell states before/after, per address.
struct UndoGroup {
    sheet: usize,
    changes: Vec<(u32, u32, Option<Cell>, Option<Cell>)>,
}

/// Sheets + defined names — snapshotted around structural edits whose inverse
/// is not expressible as per-cell changes (Task 3 uses this).
#[derive(Clone)]
struct WbSnapshot {
    sheets: Vec<Sheet>,
    names: Vec<DefinedName>,
}

enum UndoAction {
    Cells(UndoGroup),
    Structural {
        before: WbSnapshot,
        after: WbSnapshot,
    },
}

/// A live editing session over one `.xlsx`.
pub struct Session {
    /// Whole package retained — save regenerates only modeled cell data and
    /// preserves every other part byte-for-byte.
    pkg: SheetPackage,
    engine: Engine,
    /// Active sheet index (what the webview is looking at).
    active: usize,
    /// Active cell (row, col) and the optional selection anchor it extends from.
    cur: (u32, u32),
    anchor: Option<(u32, u32)>,
    /// Last requested viewport: (top, left, nrows, ncols).
    window: (u32, u32, u32, u32),
    dirty: bool,
    /// One-shot status/error line for the next view (formula errors etc.).
    err: Option<String>,
    undo: Vec<UndoAction>,
    redo: Vec<UndoAction>,
}

impl Session {
    pub fn open(bytes: &[u8]) -> Option<Session> {
        let mut pkg = load_xlsx(bytes).ok()?;
        let mut engine = Engine::new(&pkg.workbook);
        engine.recalc_all(&mut pkg.workbook);
        Some(Session {
            pkg,
            engine,
            active: 0,
            cur: (0, 0),
            anchor: None,
            window: (0, 0, 60, 20),
            dirty: false,
            err: None,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    /// Apply one tab-delimited command. Returns `Some(text)` when the host
    /// should copy `text` to the OS clipboard. (Commands grow over Tasks 2–4.)
    pub fn dispatch(&mut self, cmd: &str) -> Option<String> {
        let mut it = cmd.splitn(2, '\t');
        let op = it.next().unwrap_or("");
        let rest = it.next().unwrap_or("");
        match op {
            "view" => {
                let p: Vec<&str> = rest.split('\t').collect();
                if p.len() == 5 {
                    let sheet: usize = p[0].parse().unwrap_or(0);
                    if sheet < self.pkg.workbook.sheets.len() {
                        self.active = sheet;
                    }
                    self.window = (
                        p[1].parse().unwrap_or(0),
                        p[2].parse().unwrap_or(0),
                        p[3].parse().unwrap_or(60).max(1),
                        p[4].parse().unwrap_or(20).max(1),
                    );
                }
            }
            "clock" => {
                if let Ok(serial) = rest.parse::<f64>() {
                    self.engine.clock = Some(serial);
                }
            }
            "select" => {
                let p: Vec<&str> = rest.split('\t').collect();
                if p.len() >= 2 {
                    let r = p[0].parse().unwrap_or(0);
                    let c = p[1].parse().unwrap_or(0);
                    self.cur = (r, c);
                    self.anchor = if p.len() == 4 {
                        Some((p[2].parse().unwrap_or(r), p[3].parse().unwrap_or(c)))
                    } else {
                        None
                    };
                }
            }
            "set" => {
                let p: Vec<&str> = rest.splitn(3, '\t').collect();
                if p.len() == 3 {
                    let (r, c) = (p[0].parse().unwrap_or(0), p[1].parse().unwrap_or(0));
                    let text = p[2];
                    if let Some(body) = text.strip_prefix('=') {
                        if !body.is_empty() {
                            if let Err(e) = Engine::validate(body) {
                                self.err = Some(format!("formula error: {e}"));
                                return None;
                            }
                        }
                    }
                    let style = self.pkg.workbook.sheets[self.active]
                        .cell(r, c)
                        .map(|x| x.style)
                        .unwrap_or(0);
                    let mut cell = parse_input(text);
                    cell.style = style;
                    self.apply(vec![(r, c, cell)]);
                }
            }
            "clear" => {
                let p: Vec<&str> = rest.split('\t').collect();
                if p.len() == 4 {
                    let (r1, c1): (u32, u32) =
                        (p[0].parse().unwrap_or(0), p[1].parse().unwrap_or(0));
                    let (r2, c2): (u32, u32) =
                        (p[2].parse().unwrap_or(0), p[3].parse().unwrap_or(0));
                    let mut changes = Vec::new();
                    for r in r1..=r2 {
                        for c in c1..=c2 {
                            if let Some(cell) = self.pkg.workbook.sheets[self.active].cell(r, c) {
                                if !cell.is_blank() {
                                    let style = cell.style;
                                    let mut blank = Cell::default();
                                    blank.style = style;
                                    changes.push((r, c, blank));
                                }
                            }
                        }
                    }
                    self.apply(changes);
                }
            }
            "undo" => self.do_undo(),
            "redo" => self.do_redo(),
            "insrow" | "delrow" | "inscol" | "delcol" => {
                let p: Vec<&str> = rest.split('\t').collect();
                if p.len() == 2 {
                    let at: u32 = p[0].parse().unwrap_or(0);
                    let n: u32 = p[1].parse().unwrap_or(1).max(1);
                    let idx = self.active;
                    match op {
                        "insrow" => {
                            self.structural(|wb| gridcore::edit::insert_rows(wb, idx, at, n))
                        }
                        "delrow" => {
                            self.structural(|wb| gridcore::edit::delete_rows(wb, idx, at, n))
                        }
                        "inscol" => {
                            self.structural(|wb| gridcore::edit::insert_cols(wb, idx, at, n))
                        }
                        _ => self.structural(|wb| gridcore::edit::delete_cols(wb, idx, at, n)),
                    }
                }
            }
            "sheet" => {
                let p: Vec<&str> = rest.splitn(2, '\t').collect();
                match (p.first().copied(), p.get(1).copied()) {
                    (Some("switch"), Some(i)) => {
                        let i: usize = i.parse().unwrap_or(0);
                        if i < self.pkg.workbook.sheets.len() {
                            self.active = i;
                            self.cur = (0, 0);
                            self.anchor = None;
                        }
                    }
                    (Some("add"), Some(name)) if !name.is_empty() => {
                        // add_sheet also creates the worksheet part; snapshot-undo won't
                        // remove the part (accepted, same as the TUI's semantics).
                        let idx = self.pkg.add_sheet(name);
                        self.rebuild_engine();
                        self.active = idx;
                        self.cur = (0, 0);
                        self.anchor = None;
                        self.dirty = true;
                        self.undo.clear();
                        self.redo.clear();
                    }
                    (Some("rename"), Some(rest2)) => {
                        if let Some((i, name)) = rest2.split_once('\t') {
                            let i: usize = i.parse().unwrap_or(usize::MAX);
                            if i < self.pkg.workbook.sheets.len() && !name.is_empty() {
                                let n = name.to_string();
                                self.structural(|wb| gridcore::edit::rename_sheet(wb, i, &n));
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }

    /// Apply cell changes as one undo group, through the engine.
    fn apply(&mut self, changes: Vec<(u32, u32, Cell)>) {
        if changes.is_empty() {
            return;
        }
        let sheet_idx = self.active;
        let mut group = UndoGroup {
            sheet: sheet_idx,
            changes: Vec::with_capacity(changes.len()),
        };
        for (r, c, cell) in changes {
            let before = self.pkg.workbook.sheets[sheet_idx].cell(r, c).cloned();
            self.engine
                .set_cell(&mut self.pkg.workbook, (sheet_idx, r, c), cell);
            let after = self.pkg.workbook.sheets[sheet_idx].cell(r, c).cloned();
            group.changes.push((r, c, before, after));
        }
        self.undo.push(UndoAction::Cells(group));
        self.redo.clear();
        self.dirty = true;
    }

    fn do_undo(&mut self) {
        match self.undo.pop() {
            Some(UndoAction::Cells(group)) => {
                self.active = group.sheet.min(self.pkg.workbook.sheets.len() - 1);
                for &(r, c, ref before, _) in group.changes.iter().rev() {
                    let cell = before.clone().unwrap_or_default();
                    self.engine
                        .set_cell(&mut self.pkg.workbook, (group.sheet, r, c), cell);
                }
                self.redo.push(UndoAction::Cells(group));
                self.dirty = true;
            }
            Some(UndoAction::Structural { before, after }) => {
                self.restore(&before);
                self.redo.push(UndoAction::Structural { before, after });
            }
            None => {}
        }
    }

    fn do_redo(&mut self) {
        match self.redo.pop() {
            Some(UndoAction::Cells(group)) => {
                self.active = group.sheet.min(self.pkg.workbook.sheets.len() - 1);
                for &(r, c, _, ref after) in group.changes.iter() {
                    let cell = after.clone().unwrap_or_default();
                    self.engine
                        .set_cell(&mut self.pkg.workbook, (group.sheet, r, c), cell);
                }
                self.undo.push(UndoAction::Cells(group));
                self.dirty = true;
            }
            Some(UndoAction::Structural { before, after }) => {
                self.restore(&after);
                self.undo.push(UndoAction::Structural { before, after });
            }
            None => {}
        }
    }

    /// Snapshot-run-snapshot for structural edits (row/col ops, renames):
    /// the inverse isn't per-cell, so undo restores the whole grid state.
    fn structural(&mut self, op: impl FnOnce(&mut gridcore::sheet::Workbook)) {
        let before = WbSnapshot {
            sheets: self.pkg.workbook.sheets.clone(),
            names: self.pkg.workbook.defined_names.clone(),
        };
        op(&mut self.pkg.workbook);
        self.rebuild_engine();
        let after = WbSnapshot {
            sheets: self.pkg.workbook.sheets.clone(),
            names: self.pkg.workbook.defined_names.clone(),
        };
        self.undo.push(UndoAction::Structural { before, after });
        self.redo.clear();
        self.dirty = true;
    }

    fn restore(&mut self, snap: &WbSnapshot) {
        self.pkg.workbook.sheets = snap.sheets.clone();
        self.pkg.workbook.defined_names = snap.names.clone();
        self.rebuild_engine();
        self.dirty = true;
    }

    /// Formulas changed wholesale — reparse the graph and refresh values.
    fn rebuild_engine(&mut self) {
        let clock = self.engine.clock;
        let seed = self.engine.seed;
        let mut engine = Engine::new(&self.pkg.workbook);
        engine.clock = clock;
        engine.seed = seed;
        engine.recalc_all(&mut self.pkg.workbook);
        self.engine = engine;
    }

    /// The raw editable source of a cell: `=FORMULA` or the raw value text.
    fn cell_src(&self, row: u32, col: u32) -> String {
        let Some(cell) = self.pkg.workbook.sheets[self.active].cell(row, col) else {
            return String::new();
        };
        if let Some(f) = &cell.formula {
            return format!("={f}");
        }
        match &cell.value {
            CellValue::Empty => String::new(),
            CellValue::Number(n) => {
                // Shortest round-trip text (Rust's f64 Display is shortest).
                format!("{n}")
            }
            CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            CellValue::Text(s) => s.clone(),
            CellValue::Error(e) => e.clone(),
        }
    }

    /// Render the current viewport to the JSON the webview consumes.
    pub fn view_json(&mut self) -> String {
        let wb = &self.pkg.workbook;
        let sh = &wb.sheets[self.active];
        let (top, left, nrows, ncols) = self.window;
        let (used_r, used_c) = sh.used_size();

        let mut out = String::with_capacity(4096);
        out.push_str("{\"sheets\":[");
        for (i, s) in wb.sheets.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            json::push_str(&mut out, &s.name);
        }
        out.push_str("],\"active\":");
        out.push_str(&self.active.to_string());
        out.push_str(",\"dims\":{\"rows\":");
        out.push_str(&used_r.to_string());
        out.push_str(",\"cols\":");
        out.push_str(&used_c.to_string());
        out.push_str("},\"colw\":[");
        for (i, c) in (left..left.saturating_add(ncols)).enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"c\":");
            out.push_str(&c.to_string());
            out.push_str(",\"w\":");
            out.push_str(&format!("{:.2}", sh.col_width(c)));
            out.push('}');
        }
        out.push_str("],\"cells\":[");
        let mut first = true;
        for r in top..top.saturating_add(nrows) {
            for c in left..left.saturating_add(ncols) {
                let Some(cell) = sh.cell(r, c) else { continue };
                if cell.is_blank() {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                let xf = wb.styles.xf(cell.style);
                let text = format_with(&xf, &cell.value, wb.date1904);
                out.push_str("{\"r\":");
                out.push_str(&r.to_string());
                out.push_str(",\"c\":");
                out.push_str(&c.to_string());
                out.push_str(",\"t\":");
                json::push_str(&mut out, &text);
                let align = match xf.align {
                    Align::Right => Some("r"),
                    Align::Center => Some("c"),
                    Align::Left => None,
                    Align::General => match cell.value {
                        CellValue::Number(_) | CellValue::Bool(_) => Some("r"),
                        _ => None,
                    },
                };
                if let Some(a) = align {
                    out.push_str(",\"a\":\"");
                    out.push_str(a);
                    out.push('"');
                }
                if xf.bold {
                    out.push_str(",\"b\":1");
                }
                if xf.italic {
                    out.push_str(",\"i\":1");
                }
                if let Some((r8, g8, b8)) = xf.color {
                    out.push_str(&format!(",\"col\":\"#{r8:02x}{g8:02x}{b8:02x}\""));
                }
                if let Some((r8, g8, b8)) = xf.fill {
                    out.push_str(&format!(",\"bg\":\"#{r8:02x}{g8:02x}{b8:02x}\""));
                }
                out.push('}');
            }
        }
        out.push_str("],\"sel\":{");
        let (ar, ac) = self.anchor.unwrap_or(self.cur);
        let (r1, r2) = (self.cur.0.min(ar), self.cur.0.max(ar));
        let (c1, c2) = (self.cur.1.min(ac), self.cur.1.max(ac));
        out.push_str(&format!("\"r\":{r1},\"c\":{c1},\"r2\":{r2},\"c2\":{c2}"));
        out.push_str("},\"cur\":{\"ref\":");
        json::push_str(&mut out, &cell_name(self.cur.0, self.cur.1));
        out.push_str(",\"src\":");
        let src = self.cell_src(self.cur.0, self.cur.1);
        json::push_str(&mut out, &src);
        out.push_str("},\"dirty\":");
        out.push_str(if self.dirty { "true" } else { "false" });
        if let Some(e) = self.err.take() {
            out.push_str(",\"err\":");
            json::push_str(&mut out, &e);
        }
        out.push('}');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridcore::sheet::Cell;
    use gridcore::xlsx::{new_xlsx, save_xlsx};

    /// A real .xlsx (one sheet, a few cells incl. a formula) built through the
    /// package layer, so tests exercise the same load path the webview uses.
    fn sample_xlsx() -> Vec<u8> {
        let mut pkg = new_xlsx();
        let sh = &mut pkg.workbook.sheets[0];
        sh.set_cell(0, 0, Cell::text("Item"));
        sh.set_cell(0, 1, Cell::text("Price"));
        sh.set_cell(1, 0, Cell::text("Apple"));
        sh.set_cell(1, 1, Cell::number(1.25));
        sh.set_cell(2, 0, Cell::text("Pear"));
        sh.set_cell(2, 1, Cell::number(2.5));
        sh.set_cell(3, 1, Cell::formula("SUM(B1:B3)"));
        save_xlsx(&pkg)
    }

    #[test]
    fn opens_and_renders_viewport() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        let v = s.view_json();
        assert!(v.contains("\"sheets\":[\"Sheet1\"]"), "{v}");
        assert!(v.contains("Apple"), "{v}");
        assert!(v.contains("3.75"), "formula not recalculated: {v}");
        assert!(v.contains("\"dirty\":false"), "{v}");
        assert!(v.contains("\"cur\":{\"ref\":\"A1\""), "{v}");
    }

    #[test]
    fn viewport_clips_to_window() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("view\t0\t0\t0\t2\t1"); // rows 0..2, col 0 only
        let v = s.view_json();
        assert!(v.contains("Item") && v.contains("Apple"), "{v}");
        assert!(!v.contains("Price"), "col 1 must be clipped: {v}");
        assert!(!v.contains("Pear"), "row 2 must be clipped: {v}");
        // dims still reports the full used extent, not the window
        assert!(v.contains("\"dims\":{\"rows\":4,\"cols\":2}"), "{v}");
    }

    #[test]
    fn open_rejects_garbage() {
        assert!(Session::open(b"not an xlsx").is_none());
    }

    #[test]
    fn set_recalculates_dependents_and_marks_dirty() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t1\t1\t10"); // B2: 1.25 -> 10
        let v = s.view_json();
        assert!(v.contains("12.5"), "SUM must update: {v}");
        assert!(v.contains("\"dirty\":true"), "{v}");
    }

    #[test]
    fn set_formula_validates_and_reports_errors() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t5\t0\t=SUM(");
        let v = s.view_json();
        assert!(
            v.contains("\"err\":"),
            "invalid formula must surface err: {v}"
        );
        // and the cell must not have been written
        s.dispatch("select\t5\t0");
        let v = s.view_json();
        assert!(v.contains("\"src\":\"\""), "cell should stay empty: {v}");
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t1\t1\t10");
        s.dispatch("undo");
        let v = s.view_json();
        assert!(v.contains("3.75"), "undo must restore SUM: {v}");
        s.dispatch("redo");
        let v = s.view_json();
        assert!(v.contains("12.5"), "redo must reapply: {v}");
    }

    #[test]
    fn clear_range_clears_as_one_undo_group() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("clear\t0\t0\t2\t1"); // wipe rows 0..=2
        let v = s.view_json();
        assert!(!v.contains("Apple"), "{v}");
        s.dispatch("undo");
        let v = s.view_json();
        assert!(
            v.contains("Apple") && v.contains("3.75"),
            "one undo restores all: {v}"
        );
    }

    #[test]
    fn select_extends_selection_and_moves_cur() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t1\t0\t2\t1");
        let v = s.view_json();
        assert!(
            v.contains("\"sel\":{\"r\":1,\"c\":0,\"r2\":2,\"c2\":1}"),
            "{v}"
        );
        assert!(v.contains("\"ref\":\"A2\""), "{v}");
    }

    #[test]
    fn insert_row_rewrites_references_and_undoes() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("insrow\t1\t1"); // push data rows down: SUM(B1:B3) -> SUM(B1:B4)
        s.dispatch("select\t4\t1");
        let v = s.view_json();
        assert!(
            v.contains("\"src\":\"=SUM(B1:B4)\""),
            "refs must rewrite: {v}"
        );
        assert!(v.contains("3.75"), "total unchanged: {v}");
        s.dispatch("undo");
        s.dispatch("select\t3\t1");
        let v = s.view_json();
        assert!(
            v.contains("\"src\":\"=SUM(B1:B3)\""),
            "undo restores refs: {v}"
        );
    }

    #[test]
    fn delete_col_and_undo() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("delcol\t0\t1"); // drop the Item column; Price shifts to col 0
        let v = s.view_json();
        assert!(!v.contains("Apple"), "{v}");
        assert!(v.contains("3.75"), "sum column survives: {v}");
        s.dispatch("undo");
        let v = s.view_json();
        assert!(v.contains("Apple"), "undo restores: {v}");
    }

    #[test]
    fn sheet_add_rename_switch() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("sheet\tadd\tData");
        let v = s.view_json();
        assert!(v.contains("\"sheets\":[\"Sheet1\",\"Data\"]"), "{v}");
        assert!(
            v.contains("\"active\":1"),
            "add switches to the new sheet: {v}"
        );
        s.dispatch("sheet\trename\t1\tFacts");
        let v = s.view_json();
        assert!(v.contains("Facts"), "{v}");
        s.dispatch("sheet\tswitch\t0");
        let v = s.view_json();
        assert!(v.contains("\"active\":0") && v.contains("Apple"), "{v}");
    }
}
