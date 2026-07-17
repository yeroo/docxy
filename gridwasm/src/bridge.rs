//! The host-agnostic spreadsheet session: everything the wasm ABI exposes,
//! written as plain Rust so it can be unit-tested natively
//! (`cargo test -p gridwasm`). Mirrors `docxwasm::bridge` in shape.

use gridcore::edit::parse_input;
use gridcore::engine::Engine;
use gridcore::sheet::{
    Align, Cell, CellValue, DefinedName, MAX_COLS, MAX_ROWS, Sheet, cell_name, format_with,
};
use gridcore::xlsx::{SheetPackage, load_xlsx, save_xlsx};

use crate::json;

/// Bytes of a fresh empty workbook (the host's empty-file create flow).
pub fn new_workbook() -> Vec<u8> {
    save_xlsx(&gridcore::xlsx::new_xlsx())
}

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
    /// A sheet-add: its inverse is the real `SheetPackage::remove_sheet`
    /// (part + content-type override + workbook rel + workbook.xml entry),
    /// not a `WbSnapshot` restore. A `Structural` snapshot only rolls back
    /// `workbook.sheets`, leaving the minted worksheet part and its
    /// `<sheet>` element in `xl/workbook.xml` behind; that count mismatch
    /// makes `patch_sheet_names` bail on save (see
    /// `gridcore::xlsx::patch_sheet_names`), so an add→undo→save round trip
    /// would resurrect the "undone" sheet empty and silently drop any
    /// rename saved in between.
    SheetAdd {
        idx: usize,
        name: String,
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
                    let (r, c): (u32, u32) = (p[0].parse().unwrap_or(0), p[1].parse().unwrap_or(0));
                    if r >= MAX_ROWS || c >= MAX_COLS {
                        return None;
                    }
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
            "copy" => return Some(self.selection_tsv()),
            "cut" => {
                let tsv = self.selection_tsv();
                let (ar, ac) = self.anchor.unwrap_or(self.cur);
                let (r1, r2) = (self.cur.0.min(ar), self.cur.0.max(ar));
                let (c1, c2) = (self.cur.1.min(ac), self.cur.1.max(ac));
                // reuse the clear path as one undo group
                let cmd = format!("clear\t{r1}\t{c1}\t{r2}\t{c2}");
                self.dispatch(&cmd);
                return Some(tsv);
            }
            "paste" => {
                // rest = "<r>\t<c>\t<tsv...>" — split only twice, TSV keeps its tabs.
                let p: Vec<&str> = rest.splitn(3, '\t').collect();
                if p.len() == 3 {
                    let (r0, c0): (u32, u32) =
                        (p[0].parse().unwrap_or(0), p[1].parse().unwrap_or(0));
                    // Cap the paste so a hostile/huge clipboard can't lock the
                    // UI in per-cell recalcs (mirrors the TUI's external-paste
                    // guard in xlsxy::main::paste).
                    const MAX_PASTE_CELLS: usize = 100_000;
                    let mut changes = Vec::new();
                    let mut truncated = false;
                    // Excel-on-Windows clipboards are CRLF and end with a
                    // trailing newline; strip both so a naive split doesn't
                    // (a) leave a stray \r on the last field of every row and
                    // (b) manufacture a phantom empty row past the paste that
                    // would clear whatever was already there.
                    'outer: for (dr, line) in p[2].trim_end_matches('\n').split('\n').enumerate() {
                        for (dc, text) in line.trim_end_matches('\r').split('\t').enumerate() {
                            if changes.len() >= MAX_PASTE_CELLS {
                                truncated = true;
                                break 'outer;
                            }
                            let (r, c) = (r0 + dr as u32, c0 + dc as u32);
                            if r >= MAX_ROWS || c >= MAX_COLS {
                                continue;
                            }
                            let style = self.pkg.workbook.sheets[self.active]
                                .cell(r, c)
                                .map(|x| x.style)
                                .unwrap_or(0);
                            let mut cell = parse_input(text);
                            // A pasted `=…` that doesn't parse would freeze as
                            // an unsupported cell (never evaluates, renders
                            // blank); demote it to literal text instead, same
                            // as the TUI's external paste.
                            if let Some(f) = &cell.formula {
                                if Engine::validate(f).is_err() {
                                    cell = Cell {
                                        value: CellValue::Text(text.to_string()),
                                        style,
                                        ..Cell::default()
                                    };
                                }
                            }
                            cell.style = style;
                            changes.push((r, c, cell));
                        }
                    }
                    self.apply(changes);
                    if truncated {
                        self.err = Some(format!("Pasted (clipped to {MAX_PASTE_CELLS} cells)"));
                    }
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
                        // add_sheet operates on the whole SheetPackage (parts,
                        // not just Workbook), so its undo can't be a
                        // `Structural` before/after snapshot — that only
                        // rolls back `workbook.sheets`, leaving the minted
                        // worksheet part and its `<sheet>` entry in
                        // xl/workbook.xml behind (see the doc comment on
                        // `UndoAction::SheetAdd`). Nor can undo/redo simply
                        // be cleared here — that would desync the host's
                        // undo stack, since it registers one undo step per
                        // mutating command and an engine no-op undo would
                        // walk it back to a false-clean state. `remove_sheet`
                        // is the real inverse: it drops the part, the
                        // content-type override, the workbook rel, and the
                        // workbook.xml entry together.
                        let idx = self.pkg.add_sheet(name);
                        self.rebuild_engine();
                        self.undo.push(UndoAction::SheetAdd {
                            idx,
                            name: name.to_string(),
                        });
                        self.redo.clear();
                        self.active = idx;
                        self.cur = (0, 0);
                        self.anchor = None;
                        self.dirty = true;
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
            Some(UndoAction::SheetAdd { idx, name }) => {
                self.pkg.remove_sheet(idx);
                self.rebuild_engine();
                // Sheet count shrinks — keep the active index in bounds,
                // same clamp as the Cells case above.
                self.active = self.active.min(self.pkg.workbook.sheets.len() - 1);
                self.dirty = true;
                self.redo.push(UndoAction::SheetAdd { idx, name });
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
            Some(UndoAction::SheetAdd { idx: _, name }) => {
                let new_idx = self.pkg.add_sheet(&name);
                self.rebuild_engine();
                self.active = new_idx;
                self.dirty = true;
                self.undo.push(UndoAction::SheetAdd { idx: new_idx, name });
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

    /// Serialize the current workbook. Lossless: only modeled cell data is
    /// regenerated; every other part is preserved byte-for-byte.
    pub fn save(&mut self) -> Vec<u8> {
        let out = save_xlsx(&self.pkg);
        self.dirty = false;
        out
    }

    /// The selection as TSV of raw cell sources (formulas as `=...`), rows by
    /// `\n`, cells by `\t` — round-trips through `paste`.
    fn selection_tsv(&self) -> String {
        let (ar, ac) = self.anchor.unwrap_or(self.cur);
        let (r1, r2) = (self.cur.0.min(ar), self.cur.0.max(ar));
        let (c1, c2) = (self.cur.1.min(ac), self.cur.1.max(ac));
        let mut rows = Vec::new();
        for r in r1..=r2 {
            let mut cells = Vec::new();
            for c in c1..=c2 {
                cells.push(self.cell_src(r, c));
            }
            rows.push(cells.join("\t"));
        }
        rows.join("\n")
    }

    /// Render the current viewport to the JSON the webview consumes. `copied`,
    /// when set, carries text the host should place on the OS clipboard (from
    /// a copy or cut command).
    pub fn view_json(&mut self, copied: Option<&str>) -> String {
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
        if let Some(t) = copied {
            out.push_str(",\"copied\":");
            json::push_str(&mut out, t);
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
        let v = s.view_json(None);
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
        let v = s.view_json(None);
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
        let v = s.view_json(None);
        assert!(v.contains("12.5"), "SUM must update: {v}");
        assert!(v.contains("\"dirty\":true"), "{v}");
    }

    #[test]
    fn set_formula_validates_and_reports_errors() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t5\t0\t=SUM(");
        let v = s.view_json(None);
        assert!(
            v.contains("\"err\":"),
            "invalid formula must surface err: {v}"
        );
        // and the cell must not have been written
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"\""), "cell should stay empty: {v}");
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t1\t1\t10");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(v.contains("3.75"), "undo must restore SUM: {v}");
        s.dispatch("redo");
        let v = s.view_json(None);
        assert!(v.contains("12.5"), "redo must reapply: {v}");
    }

    #[test]
    fn clear_range_clears_as_one_undo_group() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("clear\t0\t0\t2\t1"); // wipe rows 0..=2
        let v = s.view_json(None);
        assert!(!v.contains("Apple"), "{v}");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(
            v.contains("Apple") && v.contains("3.75"),
            "one undo restores all: {v}"
        );
    }

    #[test]
    fn select_extends_selection_and_moves_cur() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t1\t0\t2\t1");
        let v = s.view_json(None);
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
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=SUM(B1:B4)\""),
            "refs must rewrite: {v}"
        );
        assert!(v.contains("3.75"), "total unchanged: {v}");
        s.dispatch("undo");
        s.dispatch("select\t3\t1");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=SUM(B1:B3)\""),
            "undo restores refs: {v}"
        );
    }

    #[test]
    fn delete_col_and_undo() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("delcol\t0\t1"); // drop the Item column; Price shifts to col 0
        let v = s.view_json(None);
        assert!(!v.contains("Apple"), "{v}");
        assert!(v.contains("3.75"), "sum column survives: {v}");
        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(v.contains("Apple"), "undo restores: {v}");
    }

    #[test]
    fn copy_returns_tsv_and_paste_round_trips() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t0\t0\t1\t1");
        let tsv = s.dispatch("copy").expect("copy returns tsv");
        assert_eq!(tsv, "Item\tPrice\nApple\t1.25");
        s.dispatch("paste\t5\t0\tItem\tPrice\nApple\t1.25");
        s.dispatch("select\t6\t1");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"1.25\""), "pasted number: {v}");
        s.dispatch("undo");
        s.dispatch("select\t6\t1");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"\""), "paste is one undo group: {v}");
    }

    #[test]
    fn copy_preserves_formulas_as_source() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t3\t1");
        let tsv = s.dispatch("copy").expect("copy");
        assert_eq!(tsv, "=SUM(B1:B3)");
    }

    #[test]
    fn cut_copies_then_clears() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("select\t1\t0");
        let tsv = s.dispatch("cut").expect("cut");
        assert_eq!(tsv, "Apple");
        let v = s.view_json(None);
        assert!(!v.contains("Apple"), "{v}");
    }

    #[test]
    fn save_round_trips_losslessly_and_clears_dirty() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t1\t1\t10");
        let out = s.save();
        let v = s.view_json(None);
        assert!(v.contains("\"dirty\":false"), "{v}");
        let mut s2 = Session::open(&out).expect("reopen");
        let v2 = s2.view_json(None);
        assert!(v2.contains("12.5"), "edit persisted through save: {v2}");
    }

    #[test]
    fn new_workbook_bytes_open() {
        let bytes = new_workbook();
        let mut s = Session::open(&bytes).expect("open fresh workbook");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\"]"), "{v}");
    }

    #[test]
    fn paste_strips_crlf_and_trailing_newline() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        // Pre-existing value at row index 7, col 0 — must survive the paste.
        // A naive `split('\n')` over a trailing-newline TSV yields a phantom
        // empty last line one row past the pasted data, which would clear it.
        s.dispatch("set\t7\t0\tsentinel");
        // B7's field is text ("x"), not numeric: parse_input trims whitespace
        // (which \r counts as) before parsing a number, so a numeric field
        // would still read back clean even without the fix and mask the bug.
        // A text field takes the untrimmed literal, so it's the only field
        // that actually proves the per-line \r is stripped.
        s.dispatch("paste\t5\t0\ta\t1\r\nb\tx\r\n");
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(v.contains("\"src\":\"a\""), "A6 must be clean 'a': {v}");
        s.dispatch("select\t6\t1");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"x\""),
            "B7 must be clean 'x', no \\r: {v}"
        );
        s.dispatch("select\t7\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"sentinel\""),
            "phantom empty last line must not clear row 7: {v}"
        );
    }

    #[test]
    fn paste_demotes_invalid_formula_to_text() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("paste\t6\t0\t=BOGUS((");
        let v = s.view_json(None);
        // An unparseable formula must not be stored as a formula (which
        // would render blank/frozen since it never evaluates) — it must be
        // demoted to literal text so the cell actually shows something.
        assert!(
            v.contains("\"r\":6,\"c\":0,\"t\":\"=BOGUS((\""),
            "invalid formula must demote to visible literal text: {v}"
        );
        s.dispatch("select\t6\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"=BOGUS((\""),
            "src should read back the literal text: {v}"
        );
    }

    #[test]
    fn sheet_add_rename_switch() {
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("sheet\tadd\tData");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\",\"Data\"]"), "{v}");
        assert!(
            v.contains("\"active\":1"),
            "add switches to the new sheet: {v}"
        );
        s.dispatch("sheet\trename\t1\tFacts");
        let v = s.view_json(None);
        assert!(v.contains("Facts"), "{v}");
        s.dispatch("sheet\tswitch\t0");
        let v = s.view_json(None);
        assert!(v.contains("\"active\":0") && v.contains("Apple"), "{v}");
    }

    #[test]
    fn sheet_add_is_undoable_in_lockstep() {
        // The host registers one undo step per mutating command; if sheet
        // add clears the engine's undo/redo stacks instead of pushing onto
        // them, the two stacks desync and VS Code's undo can walk the
        // document back past a point the engine can actually reach.
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("set\t5\t0\thello");
        s.dispatch("sheet\tadd\tData");
        let v = s.view_json(None);
        assert!(v.contains("\"sheets\":[\"Sheet1\",\"Data\"]"), "{v}");

        s.dispatch("undo");
        let v = s.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\"]"),
            "first undo removes the added sheet: {v}"
        );
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"hello\""),
            "the cell edit is a separate, still-undone step: {v}"
        );

        s.dispatch("undo");
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"\""),
            "second undo reverts the cell edit: {v}"
        );

        s.dispatch("redo");
        s.dispatch("select\t5\t0");
        let v = s.view_json(None);
        assert!(
            v.contains("\"src\":\"hello\""),
            "first redo reapplies the cell edit: {v}"
        );

        s.dispatch("redo");
        let v = s.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\",\"Data\"]"),
            "second redo re-adds the sheet: {v}"
        );
    }

    #[test]
    fn sheet_add_undo_survives_save() {
        // A `Structural` before/after snapshot only rolls back
        // `workbook.sheets` — it doesn't remove the minted worksheet part or
        // its <sheet> entry in xl/workbook.xml. That leftover entry outlives
        // the undo, so on save the model has 1 sheet but workbook.xml still
        // has 2 `<sheet>` elements; `patch_sheet_names` bails on the count
        // mismatch (see gridcore::xlsx::patch_sheet_names) and the saved
        // file still lists the "undone" sheet, which resurrects (empty) on
        // reopen. `SheetAdd`'s inverse (`remove_sheet`) must remove the part
        // and its workbook.xml entry for real, not just the model entry.
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("sheet\tadd\tData");
        s.dispatch("undo");
        let out = s.save();
        let mut reopened = Session::open(&out).expect("reopen");
        let v = reopened.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Sheet1\"]"),
            "the undone sheet must not resurrect on reopen: {v}"
        );
    }

    #[test]
    fn sheet_add_undo_then_rename_persists_through_save() {
        // Same root cause as `sheet_add_undo_survives_save`, but the fallout
        // is worse: with the model/workbook.xml <sheet>-count mismatch left
        // behind by an unremoved part, `patch_sheet_names` bails on *every*
        // name in the workbook, not just the orphaned one — so a rename
        // saved during the mismatched session was silently dropped too.
        let mut s = Session::open(&sample_xlsx()).expect("open");
        s.dispatch("sheet\tadd\tData");
        s.dispatch("undo");
        s.dispatch("sheet\trename\t0\tRenamed");
        let out = s.save();
        let mut reopened = Session::open(&out).expect("reopen");
        let v = reopened.view_json(None);
        assert!(
            v.contains("\"sheets\":[\"Renamed\"]"),
            "the rename must persist through save: {v}"
        );
    }
}
