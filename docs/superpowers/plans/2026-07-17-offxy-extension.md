# Offxy VS Code Extension Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Merge docxy-vscode and a new full-editing `.xlsx` editor into one VS Code extension named **offxy**, powered by a new `gridwasm` WebAssembly bridge over `gridcore`.

**Architecture:** A new std-only `gridwasm` crate mirrors `docxwasm` (native-testable `Session` in `bridge.rs`, thin wasm ABI in `lib.rs`, length-prefixed result buffers). The webview drives it with tab-delimited commands and receives viewport JSON — never whole sheets. The extension renames to `offxy`, its provider generalizes into a registration table (`.docx` + `.xlsx` today, `.mpp` someday), and a new virtualized HTML-grid webview (`media/grid.js|css`) renders the spreadsheet with a formula bar and sheet tabs.

**Tech Stack:** Rust (gridcore, wasm32-unknown-unknown), TypeScript (esbuild bundle), plain-JS webview, @vscode/vsce packaging.

**Spec:** `docs/superpowers/specs/2026-07-17-offxy-extension-design.md`

## Global Constraints

- Version stays **0.3.0** everywhere until Boris asks for the 0.4.0 release — no bumps in this plan.
- `gridwasm` must depend only on `gridcore` (std-only; no other deps).
- All Rust must pass `cargo fmt --all --check` and `cargo clippy --all-targets -- -D warnings` (CI gates on main).
- The docx editor's behavior must not change (its webview.js/docxwasm are untouched except the one `window.__OFFXY__` global rename in Task 6).
- **Windows agent shell quirks (this machine):** plain `cargo` is an unspawnable 0-byte shim. Every cargo/npm command below assumes:
  ```bash
  export PATH="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
  Capture exit codes directly — never through `| tail` (it masks failures).
- Packaging uses `npx --yes @vscode/vsce@latest package --no-dependencies` (plain `npx vsce` resolves a broken 2015-era vsce).
- The wasm32 target is installed (`rustup target add wasm32-unknown-unknown` already done).

---

### Task 1: gridwasm crate — Session::open + viewport JSON

**Files:**
- Create: `gridwasm/Cargo.toml`
- Create: `gridwasm/src/lib.rs` (module decls only for now)
- Create: `gridwasm/src/json.rs` (copy of `docxwasm/src/json.rs`)
- Create: `gridwasm/src/bridge.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: `gridcore::xlsx::{load_xlsx, save_xlsx, new_xlsx, SheetPackage}`, `gridcore::engine::{Engine, Key}`, `gridcore::sheet::{Workbook, Sheet, Cell, CellValue, Xf, Align, Styles, col_name, cell_name, format_with}`.
- Produces: `gridwasm::bridge::Session` with `pub fn open(bytes: &[u8]) -> Option<Session>`, `pub fn view_json(&mut self) -> String`, `pub fn dispatch(&mut self, cmd: &str) -> Option<String>` (returns clipboard text for copy commands, `None` otherwise; every caller then calls `view_json`). Viewport JSON shape (consumed by Tasks 7–8's grid.js):
  ```json
  {"sheets":["Sheet1"],"active":0,
   "dims":{"rows":120,"cols":9},
   "colw":[{"c":0,"w":8.43}],
   "cells":[{"r":0,"c":0,"t":"1,234.50","a":"r","b":1,"i":1,"col":"#cc0000","bg":"#ffff00"}],
   "sel":{"r":0,"c":0,"r2":0,"c2":0},
   "cur":{"ref":"A1","src":"=SUM(B1:B3)"},
   "dirty":false,"err":"optional status/error line"}
  ```
  `a` is `"r"`/`"c"` (left omitted). `cells` lists only non-blank cells inside the requested window. `dims` is the active sheet's used extent.

- [ ] **Step 1: Crate scaffold**

`gridwasm/Cargo.toml`:

```toml
[package]
name = "gridwasm"
description = "WebAssembly bridge for gridcore: open, render (viewport grid), edit with Excel-compatible recalculation, and losslessly save .xlsx from a browser/webview host (e.g. the Offxy VS Code extension)."
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true

[lib]
crate-type = ["cdylib", "rlib"]

[lints]
workspace = true

[dependencies]
gridcore = { version = "0.1.0", path = "../gridcore" }
```

First check how `docxwasm/Cargo.toml` declares `[lib]` and mirror it exactly (if it has no `crate-type`, wasm export still works because `lib.rs` uses `#[no_mangle] extern "C"` — copy whatever docxwasm does; it is the proven configuration).

Add `"gridwasm"` to `members` in the root `Cargo.toml`.

`gridwasm/src/lib.rs` for now:

```rust
//! `gridwasm` — WebAssembly bridge for `gridcore` (the Offxy VS Code
//! extension's spreadsheet engine). ABI exports land in a later task; the
//! testable core is [`bridge::Session`].

pub mod bridge;
mod json;
```

Copy `docxwasm/src/json.rs` to `gridwasm/src/json.rs` verbatim (JSON string escaping helpers — read it first; it exposes `pub fn push_str(out: &mut String, s: &str)`).

- [ ] **Step 2: Write the failing tests**

In `gridwasm/src/bridge.rs`, write the Session skeleton signatures and a `#[cfg(test)] mod tests` FIRST, mirroring `docxwasm/src/bridge.rs`'s test style. Build test workbooks through the real save path:

```rust
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
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test -p gridwasm 2>&1 | grep -E 'error|test result'; echo "exit=$?"
```
Expected: compile errors (Session not defined).

- [ ] **Step 4: Implement Session::open, view/dispatch skeleton, view_json**

`gridwasm/src/bridge.rs` (complete implementation for this task):

```rust
//! The host-agnostic spreadsheet session: everything the wasm ABI exposes,
//! written as plain Rust so it can be unit-tested natively
//! (`cargo test -p gridwasm`). Mirrors `docxwasm::bridge` in shape.

use gridcore::engine::Engine;
use gridcore::sheet::{Align, Cell, CellValue, cell_name, col_name, format_with};
use gridcore::xlsx::{SheetPackage, load_xlsx, save_xlsx};

use crate::json;

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
        })
    }

    /// Apply one tab-delimited command. Returns `Some(text)` when the host
    /// should copy `text` to the OS clipboard. (Commands grow over Tasks 2–4;
    /// this task implements only `view`.)
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
            _ => {}
        }
        None
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
        // col_name is used by later tasks' status text; silence unused for now.
        let _ = col_name;
        out
    }
}
```

Adjust to reality as you build: field names above come from the actual gridcore API (`Sheet::{cell, set_cell, used_size, col_width, name}`, `Workbook::{sheets, styles, date1904}`, `Styles::xf`, `Xf::{align, bold, italic, color, fill}`, `format_with(&Xf, &CellValue, bool)`, `Cell::{style, formula, value, is_blank}`, `CellValue::{Empty, Number, Bool, Text, Error}`). If a name differs, fix the plan's code to match gridcore — do not change gridcore.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p gridwasm 2>&1 | grep -E 'test result|FAILED'; echo "exit=$?"
```
Expected: `3 passed`, exit 0. Also `cargo fmt --all && cargo clippy -p gridwasm --all-targets -- -D warnings`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock gridwasm
git commit -m "gridwasm: session core — open .xlsx, recalc, viewport JSON"
```

---

### Task 2: Cell editing — set/clear/select + recalc + undo/redo

**Files:**
- Modify: `gridcore/src/edit.rs` (add `parse_input`, moved from the TUI)
- Modify: `xlsxy/src/main.rs` (use `gridcore::edit::parse_input`, delete its local copy)
- Modify: `gridwasm/src/bridge.rs`

**Interfaces:**
- Consumes: Task 1's `Session`/`dispatch`. TUI patterns at `xlsxy/src/main.rs`: `apply` (line ~1039), `undo`/`redo` (~1100), `UndoGroup`/`UndoAction` (~662), `parse_input` (~3657).
- Produces: commands `select\t<r>\t<c>[\t<r2>\t<c2>]`, `set\t<r>\t<c>\t<text>`, `clear\t<r1>\t<c1>\t<r2>\t<c2>`, `undo`, `redo`, `clock\t<serial>`. `gridcore::edit::parse_input(text: &str) -> Cell` (pub, reused by TUI and gridwasm).

- [ ] **Step 1: Move `parse_input` into gridcore (DRY)**

Cut `fn parse_input(text: &str) -> Cell` from `xlsxy/src/main.rs:~3657` (read the whole function — number, percent, bool, error-literal, text inference) and add it as `pub fn parse_input(text: &str) -> Cell` in `gridcore/src/edit.rs` with its doc comment. In `xlsxy/src/main.rs`, import `gridcore::edit::parse_input` and delete the local copy. The TUI's `parse_input_kinds` test (~line 6022) stays where it is and keeps passing (it now tests the re-exported fn). Run:

```bash
cargo test -p gridcore -p xlsxy 2>&1 | grep -E 'test result|FAILED'; echo "exit=$?"
```
Expected: all pass.

- [ ] **Step 2: Write the failing tests**

Append to `gridwasm/src/bridge.rs` tests:

```rust
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
    assert!(v.contains("\"err\":"), "invalid formula must surface err: {v}");
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
    assert!(v.contains("Apple") && v.contains("3.75"), "one undo restores all: {v}");
}

#[test]
fn select_extends_selection_and_moves_cur() {
    let mut s = Session::open(&sample_xlsx()).expect("open");
    s.dispatch("select\t1\t0\t2\t1");
    let v = s.view_json();
    assert!(v.contains("\"sel\":{\"r\":1,\"c\":0,\"r2\":2,\"c2\":1}"), "{v}");
    assert!(v.contains("\"ref\":\"A2\""), "{v}");
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test -p gridwasm 2>&1 | grep -E 'test result|FAILED'; echo "exit=$?"
```
Expected: new tests FAIL (commands are no-ops).

- [ ] **Step 4: Implement**

Port the TUI's mechanism into `Session` (read `xlsxy/src/main.rs` around the given lines first). Add to `bridge.rs`:

```rust
use gridcore::edit::parse_input;
use gridcore::engine::Engine; // already imported
use gridcore::sheet::{DefinedName, Sheet};

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
    Structural { before: WbSnapshot, after: WbSnapshot },
}
```

Session gains `undo: Vec<UndoAction>, redo: Vec<UndoAction>` (init empty in `open`) and these methods (ported from the TUI's `apply`/`undo`/`redo` — same shape, no cursor/status handling):

```rust
/// Apply cell changes as one undo group, through the engine.
fn apply(&mut self, changes: Vec<(u32, u32, Cell)>) {
    if changes.is_empty() {
        return;
    }
    let sheet_idx = self.active;
    let mut group = UndoGroup { sheet: sheet_idx, changes: Vec::with_capacity(changes.len()) };
    for (r, c, cell) in changes {
        let before = self.pkg.workbook.sheets[sheet_idx].cell(r, c).cloned();
        self.engine.set_cell(&mut self.pkg.workbook, (sheet_idx, r, c), cell);
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
                self.engine.set_cell(&mut self.pkg.workbook, (group.sheet, r, c), cell);
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
// do_redo mirrors do_undo with after-states and the stacks swapped (see the
// TUI's `redo` at xlsxy/src/main.rs:~1120 — port it the same way).

fn restore(&mut self, snap: &WbSnapshot) {
    self.pkg.workbook.sheets = snap.sheets.clone();
    self.pkg.workbook.defined_names = snap.names.clone();
    self.rebuild_engine();
    self.dirty = true;
}

/// Formulas changed wholesale — reparse the graph and refresh values.
fn rebuild_engine(&mut self) {
    let clock = self.engine.clock;
    let mut engine = Engine::new(&self.pkg.workbook);
    engine.clock = clock;
    engine.recalc_all(&mut self.pkg.workbook);
    self.engine = engine;
}
```

(Check `Engine`'s public fields: the TUI sets `engine.clock` and `engine.seed`. If `seed` exists, carry it the same way as `clock`.)

New `dispatch` arms:

```rust
"clock" => {
    if let Ok(serial) = rest.parse::<f64>() {
        self.engine.clock = serial;
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
        let (r1, c1): (u32, u32) = (p[0].parse().unwrap_or(0), p[1].parse().unwrap_or(0));
        let (r2, c2): (u32, u32) = (p[2].parse().unwrap_or(0), p[3].parse().unwrap_or(0));
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
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p gridwasm -p gridcore -p xlsxy 2>&1 | grep -E 'test result|FAILED'; echo "exit=$?"
```
Expected: all pass. Then fmt + clippy as in Task 1.

- [ ] **Step 6: Commit**

```bash
git add gridcore/src/edit.rs xlsxy/src/main.rs gridwasm/src/bridge.rs
git commit -m "gridwasm: cell editing — set/clear/select with recalc and undo/redo"
```

---

### Task 3: Structural edits + sheet management

**Files:**
- Modify: `gridwasm/src/bridge.rs`

**Interfaces:**
- Consumes: `gridcore::edit::{insert_rows, delete_rows, insert_cols, delete_cols, rename_sheet}`, `SheetPackage::add_sheet(&mut self, name: &str) -> usize`, Task 2's `WbSnapshot`/`restore`/`rebuild_engine`.
- Produces: commands `insrow\t<at>\t<n>`, `delrow\t<at>\t<n>`, `inscol\t<at>\t<n>`, `delcol\t<at>\t<n>`, `sheet\tswitch\t<i>`, `sheet\tadd\t<name>`, `sheet\trename\t<i>\t<name>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn insert_row_rewrites_references_and_undoes() {
    let mut s = Session::open(&sample_xlsx()).expect("open");
    s.dispatch("insrow\t1\t1"); // push data rows down: SUM(B1:B3) -> SUM(B1:B4)
    s.dispatch("select\t4\t1");
    let v = s.view_json();
    assert!(v.contains("\"src\":\"=SUM(B1:B4)\""), "refs must rewrite: {v}");
    assert!(v.contains("3.75"), "total unchanged: {v}");
    s.dispatch("undo");
    s.dispatch("select\t3\t1");
    let v = s.view_json();
    assert!(v.contains("\"src\":\"=SUM(B1:B3)\""), "undo restores refs: {v}");
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
    assert!(v.contains("\"active\":1"), "add switches to the new sheet: {v}");
    s.dispatch("sheet\trename\t1\tFacts");
    let v = s.view_json();
    assert!(v.contains("Facts"), "{v}");
    s.dispatch("sheet\tswitch\t0");
    let v = s.view_json();
    assert!(v.contains("\"active\":0") && v.contains("Apple"), "{v}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

`cargo test -p gridwasm` — new tests FAIL.

- [ ] **Step 3: Implement**

Add a structural helper (port of the TUI's `structural`, `xlsxy/src/main.rs:~1065`):

```rust
/// Snapshot-run-snapshot for structural edits: the inverse isn't per-cell,
/// so undo restores the whole grid state.
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
```

Dispatch arms:

```rust
"insrow" | "delrow" | "inscol" | "delcol" => {
    let p: Vec<&str> = rest.split('\t').collect();
    if p.len() == 2 {
        let at: u32 = p[0].parse().unwrap_or(0);
        let n: u32 = p[1].parse().unwrap_or(1).max(1);
        let idx = self.active;
        match op {
            "insrow" => self.structural(|wb| gridcore::edit::insert_rows(wb, idx, at, n)),
            "delrow" => self.structural(|wb| gridcore::edit::delete_rows(wb, idx, at, n)),
            "inscol" => self.structural(|wb| gridcore::edit::insert_cols(wb, idx, at, n)),
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
```

Check how the TUI handles sheet add's interaction with undo (search `AddSheet` in `xlsxy/src/main.rs` — it clears the undo stacks at ~line 1183/1301; mirror whatever it does).

- [ ] **Step 4: Run tests to verify they pass**

`cargo test -p gridwasm` — all pass; fmt + clippy clean.

- [ ] **Step 5: Commit**

```bash
git add gridwasm/src/bridge.rs
git commit -m "gridwasm: structural edits (rows/cols) and sheet management"
```

---

### Task 4: Copy/paste TSV, lossless save, grid_new, wasm ABI + smoke

**Files:**
- Modify: `gridwasm/src/bridge.rs`
- Modify: `gridwasm/src/lib.rs` (full ABI)

**Interfaces:**
- Consumes: everything above; `docxwasm/src/lib.rs` as the ABI template (read it fully first — handle table, alloc/free, result-buffer encoding).
- Produces (for grid.js): wasm exports `grid_alloc(len)->ptr`, `grid_free(ptr,len)`, `grid_open(ptr,len)->handle`, `grid_close(handle)`, `grid_cmd(handle,ptr,len)->resultPtr` (result = viewport JSON; for `copy`/`cut` the JSON gains `"copied":"<tsv>"`), `grid_save(handle)->resultPtr` (raw .xlsx bytes), `grid_new()->resultPtr` (raw .xlsx bytes of a fresh workbook). All results are `[u32 le length][payload]`.
- Session additions: `pub fn save(&mut self) -> Vec<u8>`, commands `copy`, `cut`, `paste\t<r>\t<c>\t<tsv>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn copy_returns_tsv_and_paste_round_trips() {
    let mut s = Session::open(&sample_xlsx()).expect("open");
    s.dispatch("select\t0\t0\t1\t1");
    let tsv = s.dispatch("copy").expect("copy returns tsv");
    assert_eq!(tsv, "Item\tPrice\nApple\t1.25");
    s.dispatch("paste\t5\t0\tItem\tPrice\nApple\t1.25");
    s.dispatch("select\t6\t1");
    let v = s.view_json();
    assert!(v.contains("\"src\":\"1.25\""), "pasted number: {v}");
    s.dispatch("undo");
    s.dispatch("select\t6\t1");
    let v = s.view_json();
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
    let v = s.view_json();
    assert!(!v.contains("Apple"), "{v}");
}

#[test]
fn save_round_trips_losslessly_and_clears_dirty() {
    let mut s = Session::open(&sample_xlsx()).expect("open");
    s.dispatch("set\t1\t1\t10");
    let out = s.save();
    let v = s.view_json();
    assert!(v.contains("\"dirty\":false"), "{v}");
    let mut s2 = Session::open(&out).expect("reopen");
    let v2 = s2.view_json();
    assert!(v2.contains("12.5"), "edit persisted through save: {v2}");
}

#[test]
fn new_workbook_bytes_open() {
    let bytes = new_workbook();
    let mut s = Session::open(&bytes).expect("open fresh workbook");
    let v = s.view_json();
    assert!(v.contains("\"sheets\":[\"Sheet1\"]"), "{v}");
}
```

(`new_workbook` is the new pub fn below; import it in tests.)

- [ ] **Step 2: Run tests to verify they fail** — `cargo test -p gridwasm`.

- [ ] **Step 3: Implement Session parts**

```rust
/// Bytes of a fresh empty workbook (the host's empty-file create flow).
pub fn new_workbook() -> Vec<u8> {
    save_xlsx(&gridcore::xlsx::new_xlsx())
}

impl Session {
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
}
```

Dispatch arms (note `paste` must NOT split its TSV payload — take the remainder):

```rust
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
        let (r0, c0): (u32, u32) = (p[0].parse().unwrap_or(0), p[1].parse().unwrap_or(0));
        let mut changes = Vec::new();
        for (dr, line) in p[2].split('\n').enumerate() {
            for (dc, text) in line.split('\t').enumerate() {
                let (r, c) = (r0 + dr as u32, c0 + dc as u32);
                let style = self.pkg.workbook.sheets[self.active]
                    .cell(r, c)
                    .map(|x| x.style)
                    .unwrap_or(0);
                let mut cell = parse_input(text);
                cell.style = style;
                changes.push((r, c, cell));
            }
        }
        self.apply(changes);
    }
}
```

- [ ] **Step 4: Implement the wasm ABI in `lib.rs`**

Read `docxwasm/src/lib.rs` completely, then write `gridwasm/src/lib.rs` with the same structure and idioms (static handle table, `grid_alloc`/`grid_free` mirroring `docx_alloc`/`docx_free`, result-buffer helper `[u32 le len][payload]`), with these exports: `grid_alloc`, `grid_free`, `grid_open`, `grid_close`, `grid_cmd` (dispatch + `view_json`; when dispatch returned `Some(text)`, append it into the JSON by rebuilding: call `view_json`, then insert `"copied"` — simplest correct approach: give `Session::view_json` an `Option<&str>` copied parameter exactly like `docxwasm::Session::view_json(copied)` does, and update Task 1's calls to `view_json(None)`), `grid_save`, `grid_new`.

Update the earlier tests' `view_json()` calls to `view_json(None)` accordingly.

- [ ] **Step 5: Run tests, fmt, clippy**

```bash
cargo test -p gridwasm 2>&1 | grep -E 'test result|FAILED'; echo "exit=$?"
cargo fmt --all --check && cargo clippy -p gridwasm --all-targets -- -D warnings
```
Expected: all pass, clean.

- [ ] **Step 6: wasm32 build + Node smoke test**

```bash
cargo build -p gridwasm --target wasm32-unknown-unknown --release
```

Write a throwaway Node script (scratchpad, not committed) that instantiates `target/wasm32-unknown-unknown/release/gridwasm.wasm`, calls `grid_new()`, `grid_open`s the result, sends `view\t0\t0\t0\t40\t10`, `set\t0\t0\t=1+1`, and checks the JSON contains `"t":"2"`. Follow the marshalling pattern in this session's `wasm_open_test.mjs` (alloc → write bytes → call → read `[u32 len][payload]`). Expected: prints the JSON, exit 0.

- [ ] **Step 7: Commit**

```bash
git add gridwasm
git commit -m "gridwasm: clipboard TSV, lossless save, grid_new, wasm ABI"
```

---

### Task 5: Rename the extension to offxy

**Files:**
- Rename: `docxy-vscode/` → `offxy-vscode/` (via `git mv`)
- Modify: `offxy-vscode/package.json`
- Modify: `offxy-vscode/src/extension.ts`
- Modify: `.github/workflows/release.yml`
- Modify: `offxy-vscode/CHANGELOG.md`

**Interfaces:**
- Produces: extension id `yeroo.offxy`; viewType `offxy.docxEditor`; command ids `offxy.*` (same suffixes as the old `docxy.*`). Task 6+ work inside `offxy-vscode/`.

- [ ] **Step 1: Rename the directory**

```bash
git mv docxy-vscode offxy-vscode
```

- [ ] **Step 2: package.json identity + ids**

In `offxy-vscode/package.json`: `"name": "offxy"`, `"displayName": "Offxy — Word & Excel .docx/.xlsx editor"`, `"description": "Open, read, and edit Microsoft Word .docx and Excel .xlsx files right in an editor tab — powered by WebAssembly builds of the dependency-free docxcore and gridcore engines."`, add `"xlsx"`, `"excel"`, `"spreadsheet"` to `keywords`. Replace every `"docxy.` command/viewType id with `"offxy.` (customEditors viewType, commands, menus). Do NOT add the xlsx custom editor yet (Task 7 does).

- [ ] **Step 3: extension.ts ids**

In `offxy-vscode/src/extension.ts` replace: `viewType = 'docxy.docxEditor'` → `'offxy.docxEditor'`; every `'docxy.` command id in the `COMMANDS` array and `registerCommand` calls → `'offxy.`. User-visible strings that say "Docxy" can stay (the Word editor keeps its product name).

- [ ] **Step 4: release workflow paths**

In `.github/workflows/release.yml`, the `vsix` job: `working-directory: docxy-vscode` → `offxy-vscode`, artifact `path: docxy-vscode/*.vsix` → `offxy-vscode/*.vsix` (both occurrences — upload + release attach).

- [ ] **Step 5: Changelog entry**

Add under `## Unreleased` in `offxy-vscode/CHANGELOG.md`:

```markdown
- **Renamed:** the extension is now **offxy** (`yeroo.offxy`) — one extension
  for Word and (soon in this version) Excel. Uninstall `yeroo.docxy-vscode`
  and install the `offxy-*.vsix`. Command ids changed `docxy.*` → `offxy.*`
  (update any custom keybindings).
```

- [ ] **Step 6: Build, package, verify**

```bash
cd offxy-vscode && npm ci
npm run typecheck && npm run build
npx --yes @vscode/vsce@latest package --no-dependencies
code --install-extension "$(pwd -W)/offxy-0.3.0.vsix" --force
code --uninstall-extension yeroo.docxy-vscode
code --list-extensions | grep -i offxy
```
Expected: `yeroo.offxy`. Open `assets/sample.docx` in VS Code — the Word editor must work exactly as before.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "offxy: rename the extension (docxy-vscode -> offxy-vscode)"
```

---

### Task 6: Generalize the provider into a registration table

**Files:**
- Modify: `offxy-vscode/src/extension.ts`
- Modify: `offxy-vscode/media/webview.js` (one global rename)

**Interfaces:**
- Consumes: current `DocxDocument`/`DocxyEditorProvider` (read `extension.ts` fully first).
- Produces (Task 7/9 rely on these exact names):
  ```ts
  interface EditorSpec {
    viewType: string;              // 'offxy.docxEditor'
    label: string;                 // 'Word document' — used in messages
    script: string;                // media/ script file name
    style: string;                 // media/ css file name
    wasm: string;                  // media/ wasm file name
    emptyPrompt?: string;          // modal text for 0-byte files
    mintEmpty?: (context: vscode.ExtensionContext) => Promise<Uint8Array>;
  }
  class BinaryDocument implements vscode.CustomDocument { /* renamed DocxDocument, body unchanged */ }
  class OffxyEditorProvider implements vscode.CustomEditorProvider<BinaryDocument> {
    constructor(context: vscode.ExtensionContext, spec: EditorSpec)
  }
  ```
  The webview global becomes `window.__OFFXY__ = { wasmUri }`.

- [ ] **Step 1: Refactor**

Mechanical, no behavior change:
1. Rename `DocxDocument` → `BinaryDocument` (body unchanged).
2. `DocxyEditorProvider` → `OffxyEditorProvider`; delete the `private static readonly viewType` constant; the constructor takes `(context, spec: EditorSpec)` and registration uses `spec.viewType`.
3. `html()` builds URIs from `spec.script`/`spec.style`/`spec.wasm` and injects `window.__OFFXY__ = { wasmUri: "..." }`.
4. `openInWebview`'s empty-file branch: `if (bytes.length === 0 && this.spec.mintEmpty)` → modal with `this.spec.emptyPrompt` → `this.spec.mintEmpty(this.context)`; `seedNewDocument` uses `spec.mintEmpty` too (the `createNew` message path).
5. `register(context)` becomes a module-level function: defines `const EDITORS: EditorSpec[]` with ONE entry (docx: viewType `offxy.docxEditor`, label `Word document`, script `webview.js`, style `webview.css`, wasm `docxwasm.wasm`, emptyPrompt the existing modal text, mintEmpty `(ctx) => markdownToDocx(ctx, '')`), instantiates a provider per entry, registers each with the same webviewOptions as today. The docx-only commands (formatting `COMMANDS`, `offxy.replace`, markdown convert/export) bind to the docx provider instance (find it by viewType).
6. `activate` calls the new `register(context)`.

In `media/webview.js`: `window.__DOCXY__` → `window.__OFFXY__` (one line, in `boot()`).

- [ ] **Step 2: Verify no behavior change**

```bash
cd offxy-vscode && npm run typecheck && npm run build
npx --yes @vscode/vsce@latest package --no-dependencies
code --install-extension "$(pwd -W)/offxy-0.3.0.vsix" --force
```
Reload VS Code; open `assets/sample.docx` (renders + edits), create an empty `.docx` and confirm the Create flow still works.

- [ ] **Step 3: Commit**

```bash
git add offxy-vscode
git commit -m "offxy: generalize the editor provider into a registration table"
```

---

### Task 7: Grid webview — render, scroll, selection, navigation

**Files:**
- Create: `offxy-vscode/media/grid.js`
- Create: `offxy-vscode/media/grid.css`
- Modify: `offxy-vscode/src/extension.ts` (EDITORS += grid entry)
- Modify: `offxy-vscode/package.json` (customEditors += offxy.gridEditor; scripts build both wasms)
- Modify: `offxy-vscode/scripts/copy-wasm.mjs` (copy both artifacts)

**Interfaces:**
- Consumes: gridwasm ABI (Task 4), `EditorSpec` table (Task 6), host messages (`ready`/`open`/`do`/`getBytes`/`bytes`/`clipboard`/`readClipboard`/`clipboardText` — identical to the docx webview's protocol in `media/webview.js`; read it first).
- Produces: working read-only-ish grid (editing lands in Task 8): virtualized rendering, sticky headers, formula-bar display, click/drag/keyboard selection, sheet switching.

- [ ] **Step 1: Registration + packaging plumbing**

`package.json`:
- customEditors gains:
  ```json
  { "viewType": "offxy.gridEditor", "displayName": "Offxy Spreadsheet",
    "selector": [ { "filenamePattern": "*.xlsx" } ], "priority": "default" }
  ```
- `build:wasm` becomes:
  ```
  cargo build -p docxwasm -p gridwasm --target wasm32-unknown-unknown --release && node ./scripts/copy-wasm.mjs
  ```

`scripts/copy-wasm.mjs`: loop over `['docxwasm', 'gridwasm']`, copying `target/wasm32-unknown-unknown/release/<name>.wasm` → `media/<name>.wasm` (keep the exists-check + error message per artifact).

`extension.ts` EDITORS gains:
```ts
{
  viewType: 'offxy.gridEditor',
  label: 'Excel workbook',
  script: 'grid.js',
  style: 'grid.css',
  wasm: 'gridwasm.wasm',
  // emptyPrompt/mintEmpty land in Task 9
},
```

- [ ] **Step 2: Write `media/grid.css`**

```css
/* Offxy spreadsheet webview. Theme-driven: all colors from VS Code vars. */
* { box-sizing: border-box; margin: 0; padding: 0; }
html, body { height: 100%; overflow: hidden;
  font-family: var(--vscode-editor-font-family, monospace);
  font-size: var(--vscode-editor-font-size, 13px);
  color: var(--vscode-editor-foreground);
  background: var(--vscode-editor-background); }

#fbar { display: flex; align-items: center; gap: 6px; height: 28px;
  padding: 0 8px; border-bottom: 1px solid var(--vscode-editorWidget-border, #0003); }
#cellref { min-width: 64px; text-align: center; padding: 2px 6px;
  border: 1px solid var(--vscode-editorWidget-border, #0003);
  color: var(--vscode-descriptionForeground); }
#fsrc { flex: 1; padding: 2px 6px; font: inherit;
  color: var(--vscode-input-foreground); background: var(--vscode-input-background);
  border: 1px solid var(--vscode-input-border, transparent); outline: none; }

#gridwrap { position: absolute; top: 28px; bottom: 24px; left: 0; right: 0; overflow: auto; }
#spacer { position: absolute; top: 0; left: 0; width: 1px; height: 1px; }
#cells { position: absolute; top: 0; left: 0; }
.cell { position: absolute; overflow: hidden; white-space: pre;
  padding: 0 4px; line-height: 22px; height: 22px;
  border-right: 1px solid var(--vscode-editorWidget-border, #0002);
  border-bottom: 1px solid var(--vscode-editorWidget-border, #0002); }
.cell.num { text-align: right; }
.cell.ctr { text-align: center; }
.cell.b { font-weight: bold; }
.cell.i { font-style: italic; }

#colhdr, #rowhdr { position: absolute; z-index: 3; overflow: hidden;
  background: var(--vscode-editorWidget-background, var(--vscode-editor-background));
  color: var(--vscode-descriptionForeground); }
#colhdr { top: 28px; left: 44px; right: 0; height: 22px;
  border-bottom: 1px solid var(--vscode-editorWidget-border, #0003); }
#rowhdr { top: 50px; left: 0; bottom: 24px; width: 44px;
  border-right: 1px solid var(--vscode-editorWidget-border, #0003); }
#corner { position: absolute; top: 28px; left: 0; width: 44px; height: 22px; z-index: 4;
  background: var(--vscode-editorWidget-background, var(--vscode-editor-background));
  border-right: 1px solid var(--vscode-editorWidget-border, #0003);
  border-bottom: 1px solid var(--vscode-editorWidget-border, #0003); }
.hcell { position: absolute; text-align: center; line-height: 22px; height: 22px;
  border-right: 1px solid var(--vscode-editorWidget-border, #0002); }
.hcell.on { color: var(--vscode-editor-foreground); font-weight: bold; }

#selbox { position: absolute; pointer-events: none; z-index: 2;
  border: 2px solid var(--vscode-focusBorder, #007acc);
  background: color-mix(in srgb, var(--vscode-editor-selectionBackground, #264f78) 35%, transparent); }
#curbox { position: absolute; pointer-events: none; z-index: 2;
  border: 2px solid var(--vscode-focusBorder, #007acc); }

#tabs { position: absolute; bottom: 0; left: 0; right: 0; height: 24px;
  display: flex; align-items: stretch; gap: 2px; padding: 0 6px; overflow-x: auto;
  background: var(--vscode-editorWidget-background, var(--vscode-editor-background));
  border-top: 1px solid var(--vscode-editorWidget-border, #0003); }
#tabs button { font: inherit; border: none; cursor: pointer; padding: 0 10px;
  color: var(--vscode-descriptionForeground); background: transparent; }
#tabs button.on { color: var(--vscode-editor-foreground);
  border-bottom: 2px solid var(--vscode-focusBorder, #007acc); }

.empty-state { display: flex; flex-direction: column; align-items: center;
  gap: 12px; padding: 48px 24px; font-family: var(--vscode-font-family); }
.empty-state p { color: var(--vscode-descriptionForeground); }
.empty-state button { padding: 6px 14px; font: inherit; cursor: pointer;
  color: var(--vscode-button-foreground); background: var(--vscode-button-background);
  border: none; border-radius: 2px; }
```

Note: the grid area offsets (`#gridwrap` top 28 / bottom 24, headers at 44px/22px) are constants mirrored in grid.js (`HDR_W = 44`, `ROW_H = 22`, `FBAR_H = 28`).

- [ ] **Step 3: Write `media/grid.js`**

Complete file for this task (Task 8 extends it):

```js
// Offxy spreadsheet webview — drives the gridwasm engine (viewport protocol)
// and paints a virtualized HTML grid: sticky headers, formula bar, sheet tabs.
//
// The wasm ABI mirrors `gridwasm/src/lib.rs`:
//   grid_alloc(len)->ptr, grid_free(ptr,len)
//   grid_open(ptr,len)->handle, grid_close(handle)
//   grid_cmd(handle,ptr,len)->resultPtr   (viewport JSON)
//   grid_save(handle)->resultPtr          (xlsx bytes)
// A "result" buffer is [u32 little-endian length][payload bytes].

(function () {
  const vscode = acquireVsCodeApi();
  const $ = (id) => document.getElementById(id);

  const ROW_H = 22;     // must match grid.css .cell height
  const HDR_W = 44;     // row-number gutter width
  const COL_PX = 7.5;   // Excel column-width unit -> px (approximate MDW)
  const OVERSCAN = 5;   // extra rows/cols fetched around the visible window

  let ex = null;        // wasm exports
  let handle = 0;
  let view = null;      // last viewport JSON
  let colX = [0];       // prefix x of each col up to the fetched window's right edge
  let defW = 64;        // default column width in px

  const enc = new TextEncoder();
  const dec = new TextDecoder();

  // ---- wasm marshalling (same pattern as webview.js) -----------------------
  const mem = () => new Uint8Array(ex.memory.buffer);
  function writeBytes(u8) {
    const ptr = ex.grid_alloc(u8.length);
    mem().set(u8, ptr);
    return ptr;
  }
  function readResult(ptr) {
    const m = mem();
    const len = m[ptr] | (m[ptr + 1] << 8) | (m[ptr + 2] << 16) | (m[ptr + 3] << 24);
    const out = m.slice(ptr + 4, ptr + 4 + len);
    ex.grid_free(ptr, 4 + len);
    return out;
  }
  function cmd(str) {
    const u8 = enc.encode(str);
    const p = writeBytes(u8);
    const r = ex.grid_cmd(handle, p, u8.length);
    ex.grid_free(p, u8.length);
    view = JSON.parse(dec.decode(readResult(r)));
    paint();
    if (view.copied != null) {
      vscode.postMessage({ type: 'clipboard', text: view.copied });
    }
    return view;
  }

  // ---- geometry ------------------------------------------------------------
  function colWidthPx(c) {
    const e = (view.colw || []).find((x) => x.c === c);
    return e ? Math.max(24, Math.round(e.w * COL_PX)) : defW;
  }
  /** x position (px) of column c relative to the fetched window's left edge. */
  function rebuildColX(left, ncols) {
    colX = [0];
    for (let i = 0; i < ncols; i++) colX.push(colX[i] + colWidthPx(left + i));
  }
  function colAtX(x) {
    // x is relative to the sheet origin; walk from the fetched window.
    const { left } = win();
    let acc = left * defW; // approximation left of the window (uniform default)
    if (x < acc) return Math.max(0, Math.floor(x / defW));
    for (let i = 0; i < colX.length - 1; i++) {
      if (x < acc + colX[i + 1]) return left + i;
    }
    return left + colX.length - 1;
  }

  function win() {
    const wrap = $('gridwrap');
    const top = Math.max(0, Math.floor(wrap.scrollTop / ROW_H) - OVERSCAN);
    const left = Math.max(0, Math.floor(wrap.scrollLeft / defW) - OVERSCAN);
    const nrows = Math.ceil(wrap.clientHeight / ROW_H) + 2 * OVERSCAN;
    const ncols = Math.ceil(wrap.clientWidth / defW) + 2 * OVERSCAN;
    return { top, left, nrows, ncols };
  }

  let viewTimer = 0;
  function requestView() {
    const { top, left, nrows, ncols } = win();
    cmd(`view\t${view ? view.active : 0}\t${top}\t${left}\t${nrows}\t${ncols}`);
  }
  function onScroll() {
    clearTimeout(viewTimer);
    viewTimer = setTimeout(requestView, 50);
  }

  // ---- painting ------------------------------------------------------------
  function paint() {
    const { top, left, nrows, ncols } = win();
    rebuildColX(left, ncols);
    const originX = left * defW; // sheet-x of the fetched window's left edge
    const rows = Math.max(view.dims.rows + 50, top + nrows);
    const cols = Math.max(view.dims.cols + 10, left + ncols);
    $('spacer').style.height = rows * ROW_H + 'px';
    $('spacer').style.width = cols * defW + 'px';

    // cells
    const frag = document.createDocumentFragment();
    for (const cl of view.cells) {
      const el = document.createElement('div');
      el.className = 'cell';
      el.textContent = cl.t;
      if (cl.a === 'r') el.classList.add('num');
      if (cl.a === 'c') el.classList.add('ctr');
      if (cl.b) el.classList.add('b');
      if (cl.i) el.classList.add('i');
      if (cl.col) el.style.color = cl.col;
      if (cl.bg) el.style.background = cl.bg;
      el.style.top = cl.r * ROW_H + 'px';
      el.style.left = originX + colX[cl.c - left] + 'px';
      el.style.width = colWidthPx(cl.c) + 'px';
      frag.appendChild(el);
    }
    // selection + active cell boxes
    const sel = view.sel;
    const selEl = document.createElement('div');
    selEl.id = 'selbox';
    selEl.style.top = sel.r * ROW_H + 'px';
    selEl.style.left = originX + (colX[sel.c - left] ?? 0) + 'px';
    selEl.style.height = (sel.r2 - sel.r + 1) * ROW_H + 'px';
    let wsum = 0;
    for (let c = sel.c; c <= sel.c2; c++) wsum += colWidthPx(c);
    selEl.style.width = wsum + 'px';
    frag.appendChild(selEl);
    $('cells').replaceChildren(frag);

    paintHeaders(top, left, nrows, ncols, originX);
    paintTabs();
    $('cellref').textContent = view.cur.ref;
    if (document.activeElement !== $('fsrc')) $('fsrc').value = view.cur.src;
  }

  function paintHeaders(top, left, nrows, ncols, originX) {
    const wrap = $('gridwrap');
    const ch = document.createDocumentFragment();
    for (let i = 0; i < ncols; i++) {
      const c = left + i;
      const el = document.createElement('div');
      el.className = 'hcell';
      if (c >= view.sel.c && c <= view.sel.c2) el.classList.add('on');
      el.textContent = colName(c);
      el.style.left = originX + colX[i] - wrap.scrollLeft + 'px';
      el.style.width = colWidthPx(c) + 'px';
      el.dataset.col = c;
      ch.appendChild(el);
    }
    $('colhdr').replaceChildren(ch);
    const rh = document.createDocumentFragment();
    for (let i = 0; i < nrows; i++) {
      const r = top + i;
      const el = document.createElement('div');
      el.className = 'hcell';
      if (r >= view.sel.r && r <= view.sel.r2) el.classList.add('on');
      el.textContent = r + 1;
      el.style.top = r * ROW_H - wrap.scrollTop + 'px';
      el.style.width = HDR_W + 'px';
      el.dataset.row = r;
      rh.appendChild(el);
    }
    $('rowhdr').replaceChildren(rh);
  }

  function paintTabs() {
    const bar = document.createDocumentFragment();
    view.sheets.forEach((name, i) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.textContent = name;
      if (i === view.active) b.classList.add('on');
      b.addEventListener('click', () => cmd(`sheet\tswitch\t${i}`) && requestView());
      bar.appendChild(b);
    });
    $('tabs').replaceChildren(bar);
  }

  function colName(c) {
    let s = '';
    c += 1;
    while (c > 0) { c -= 1; s = String.fromCharCode(65 + (c % 26)) + s; c = Math.floor(c / 26); }
    return s;
  }

  // ---- selection + keyboard ------------------------------------------------
  function cellFromEvent(e) {
    const wrap = $('gridwrap');
    const rect = wrap.getBoundingClientRect();
    const x = e.clientX - rect.left + wrap.scrollLeft;
    const y = e.clientY - rect.top + wrap.scrollTop;
    return { r: Math.max(0, Math.floor(y / ROW_H)), c: colAtX(x) };
  }
  let dragging = false;
  function onMousedown(e) {
    if (!handle) return;
    const { r, c } = cellFromEvent(e);
    if (e.shiftKey) cmd(`select\t${view.cur ? refRow() : r}\t${refCol()}\t${r}\t${c}`);
    else cmd(`select\t${r}\t${c}`);
    dragging = true;
    e.preventDefault();
  }
  function refRow() { return view.sel.r === view.cur ? view.sel.r : rowOfRef(); }
  function rowOfRef() {
    // cur.ref like "B4" — parse row/col back out
    const m = view.cur.ref.match(/^([A-Z]+)(\d+)$/);
    return m ? parseInt(m[2], 10) - 1 : 0;
  }
  function refCol() {
    const m = view.cur.ref.match(/^([A-Z]+)(\d+)$/);
    if (!m) return 0;
    let c = 0;
    for (const ch of m[1]) c = c * 26 + (ch.charCodeAt(0) - 64);
    return c - 1;
  }
  function onMousemove(e) {
    if (!dragging) return;
    const { r, c } = cellFromEvent(e);
    cmd(`select\t${rowOfRef()}\t${refCol()}\t${r}\t${c}`);
  }
  function onMouseup() { dragging = false; }

  function move(dr, dc, extend) {
    const r0 = rowOfRef(), c0 = refCol();
    if (extend) {
      const s = view.sel;
      const er = Math.max(0, (s.r2 === r0 ? s.r : s.r2) + dr);
      const ec = Math.max(0, (s.c2 === c0 ? s.c : s.c2) + dc);
      cmd(`select\t${r0}\t${c0}\t${er}\t${ec}`);
    } else {
      cmd(`select\t${Math.max(0, r0 + dr)}\t${Math.max(0, c0 + dc)}`);
    }
    ensureVisible();
  }
  function ensureVisible() {
    const wrap = $('gridwrap');
    const r = rowOfRef(), c = refCol();
    const y = r * ROW_H, x = c * defW;
    if (y < wrap.scrollTop) wrap.scrollTop = y;
    if (y + ROW_H > wrap.scrollTop + wrap.clientHeight) wrap.scrollTop = y + ROW_H - wrap.clientHeight;
    if (x < wrap.scrollLeft) wrap.scrollLeft = x;
    if (x + defW > wrap.scrollLeft + wrap.clientWidth) wrap.scrollLeft = x + defW - wrap.clientWidth;
  }

  function onKeydown(e) {
    if (!handle) return;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && ['z', 'y', 's'].includes(e.key.toLowerCase())) return; // VS Code owns
    const ext = e.shiftKey;
    switch (e.key) {
      case 'ArrowUp': e.preventDefault(); return move(-1, 0, ext);
      case 'ArrowDown': e.preventDefault(); return move(1, 0, ext);
      case 'ArrowLeft': e.preventDefault(); return move(0, -1, ext);
      case 'ArrowRight': e.preventDefault(); return move(0, 1, ext);
      case 'PageUp': e.preventDefault(); return move(-20, 0, ext);
      case 'PageDown': e.preventDefault(); return move(20, 0, ext);
      case 'Home': e.preventDefault();
        return mod ? cmd('select\t0\t0') && ensureVisible() : move(0, -refCol(), ext);
      default: break;
    }
    if (mod && e.key.toLowerCase() === 'c') { e.preventDefault(); return void cmd('copy'); }
  }

  // ---- host messages -------------------------------------------------------
  window.addEventListener('message', (event) => {
    const msg = event.data;
    switch (msg.type) {
      case 'open': {
        const u8 = base64ToBytes(msg.data);
        if (handle) ex.grid_close(handle);
        const p = writeBytes(u8);
        handle = ex.grid_open(p, u8.length);
        ex.grid_free(p, u8.length);
        if (!handle) {
          document.body.textContent = 'Offxy could not read this .xlsx file.';
          return;
        }
        // Excel serial for NOW()/TODAY(): days since 1899-12-30, local time.
        const now = new Date();
        const serial = 25569 + (now.getTime() - now.getTimezoneOffset() * 60000) / 86400000;
        cmd(`clock\t${serial}`);
        requestView();
        $('gridwrap').focus();
        break;
      }
      case 'do':
        cmd(msg.op === 'redo' ? 'redo' : 'undo');
        requestView();
        break;
      case 'getBytes': {
        const bytes = readResult(ex.grid_save(handle));
        vscode.postMessage({ type: 'bytes', requestId: msg.requestId, data: bytesToBase64(bytes) });
        break;
      }
    }
  });

  // ---- base64 --------------------------------------------------------------
  function base64ToBytes(b64) {
    const bin = atob(b64);
    const u8 = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i);
    return u8;
  }
  function bytesToBase64(u8) {
    let bin = '';
    const CHUNK = 0x8000;
    for (let i = 0; i < u8.length; i += CHUNK) {
      bin += String.fromCharCode.apply(null, u8.subarray(i, i + CHUNK));
    }
    return btoa(bin);
  }

  // ---- boot ----------------------------------------------------------------
  document.body.innerHTML = `
    <div id="fbar"><span id="cellref">A1</span><input id="fsrc" spellcheck="false" /></div>
    <div id="corner"></div><div id="colhdr"></div><div id="rowhdr"></div>
    <div id="gridwrap" tabindex="0"><div id="spacer"></div><div id="cells"></div></div>
    <div id="tabs"></div>`;

  async function boot() {
    const resp = await fetch(window.__OFFXY__.wasmUri);
    const { instance } = await WebAssembly.instantiate(await resp.arrayBuffer(), {});
    ex = instance.exports;
    const wrap = $('gridwrap');
    wrap.addEventListener('scroll', onScroll);
    wrap.addEventListener('mousedown', onMousedown);
    window.addEventListener('mousemove', onMousemove);
    window.addEventListener('mouseup', onMouseup);
    wrap.addEventListener('keydown', onKeydown);
    window.addEventListener('resize', onScroll);
    vscode.postMessage({ type: 'ready' });
  }
  boot().catch((err) => {
    document.body.textContent = 'Offxy failed to start: ' + (err && err.message ? err.message : err);
  });
})();
```

Simplify `onMousedown`'s shift-click to use `rowOfRef()`/`refCol()` directly (the `refRow` indirection above is vestigial — use `rowOfRef`). Column x-positioning uses a uniform `defW` approximation left of the fetched window; inside the window real widths apply. This is v1-acceptable (custom-width sheets scrolled far right may show slight offset until the window catches up — the view re-requests on scroll and corrects itself).

- [ ] **Step 4: Build, package, e2e**

```bash
cd offxy-vscode && npm run build && npx --yes @vscode/vsce@latest package --no-dependencies
code --install-extension "$(pwd -W)/offxy-0.3.0.vsix" --force
```
Reload VS Code, open `assets/sample.xlsx`: grid renders with values and formulas' results, headers stick while scrolling, click/drag/arrows move the selection, the formula bar shows `=SUM(...)` sources, sheet tabs switch. Word editor still opens `assets/sample.docx`.

- [ ] **Step 5: Commit**

```bash
git add offxy-vscode
git commit -m "offxy: spreadsheet webview — virtualized grid, headers, selection"
```

---

### Task 8: Grid webview — editing, clipboard, structural UI

**Files:**
- Modify: `offxy-vscode/media/grid.js`

**Interfaces:**
- Consumes: Task 7's grid.js internals (`cmd`, `view`, `rowOfRef`, `refCol`, `move`, `requestView`); host messages `edit`, `clipboard`, `readClipboard`/`clipboardText` (same shapes as `media/webview.js`).
- Produces: full editing UX.

- [ ] **Step 1: In-cell + formula-bar editing**

Add to grid.js (new section before "host messages"):

```js
  // ---- editing -------------------------------------------------------------
  const MUTATING = /^(set|clear|paste|insrow|delrow|inscol|delcol|sheet\t(add|rename))/;
  function userCmd(str) {
    cmd(str);
    if (MUTATING.test(str)) vscode.postMessage({ type: 'edit' });
  }

  let editEl = null;
  function startEdit(initial) {
    if (editEl) return;
    const r = rowOfRef(), c = refCol();
    const wrap = $('gridwrap');
    editEl = document.createElement('input');
    editEl.id = 'celledit';
    editEl.value = initial != null ? initial : view.cur.src;
    editEl.style.position = 'absolute';
    editEl.style.top = r * ROW_H + 'px';
    editEl.style.left = c * defW + 'px';
    editEl.style.height = ROW_H + 'px';
    editEl.style.minWidth = defW + 'px';
    editEl.style.font = 'inherit';
    editEl.style.zIndex = 5;
    editEl.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') { e.preventDefault(); commitEdit(); move(1, 0, false); }
      else if (e.key === 'Tab') { e.preventDefault(); commitEdit(); move(0, 1, false); }
      else if (e.key === 'Escape') { e.preventDefault(); cancelEdit(); }
      e.stopPropagation();
    });
    $('cells').appendChild(editEl);
    editEl.focus();
    if (initial != null) editEl.setSelectionRange(initial.length, initial.length);
    else editEl.select();
  }
  function commitEdit() {
    if (!editEl) return;
    const text = editEl.value;
    cancelEdit();
    userCmd(`set\t${rowOfRef()}\t${refCol()}\t${text}`);
  }
  function cancelEdit() {
    if (editEl) { editEl.remove(); editEl = null; $('gridwrap').focus(); }
  }
```

Extend `onKeydown` (before the final `if (mod ...)` line):

```js
    if (e.key === 'F2') { e.preventDefault(); return startEdit(null); }
    if (e.key === 'Enter') { e.preventDefault(); return startEdit(null); }
    if (e.key === 'Delete' || e.key === 'Backspace') {
      e.preventDefault();
      const s = view.sel;
      return userCmd(`clear\t${s.r}\t${s.c}\t${s.r2}\t${s.c2}`);
    }
    if (!mod && e.key.length === 1 && !e.altKey) {
      e.preventDefault();
      return startEdit(e.key); // type-through starts a fresh edit
    }
```

Double-click opens the editor: add `wrap.addEventListener('dblclick', () => startEdit(null));` in `boot()`.

Formula bar (`#fsrc`) wiring in `boot()`:

```js
    $('fsrc').addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        userCmd(`set\t${rowOfRef()}\t${refCol()}\t${$('fsrc').value}`);
        $('gridwrap').focus();
      } else if (e.key === 'Escape') {
        e.preventDefault();
        $('fsrc').value = view.cur.src;
        $('gridwrap').focus();
      }
      e.stopPropagation();
    });
```

- [ ] **Step 2: Clipboard through the host**

Extend `onKeydown`'s mod-branch:

```js
    if (mod && e.key.toLowerCase() === 'x') { e.preventDefault(); return void userCmd('cut'); }
    if (mod && e.key.toLowerCase() === 'v') { e.preventDefault(); return void requestPaste(); }
    if (mod && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      return void cmd(`select\t0\t0\t${Math.max(0, view.dims.rows - 1)}\t${Math.max(0, view.dims.cols - 1)}`);
    }
```

Add the paste plumbing (same pattern as webview.js):

```js
  let pasteSeq = 0;
  const pastePending = new Map();
  function requestPaste() {
    const requestId = ++pasteSeq;
    pastePending.set(requestId, true);
    vscode.postMessage({ type: 'readClipboard', requestId });
  }
```

And in the message handler:

```js
      case 'clipboardText':
        if (pastePending.delete(msg.requestId) && msg.text) {
          userCmd(`paste\t${rowOfRef()}\t${refCol()}\t${msg.text}`);
        }
        break;
```

- [ ] **Step 3: Structural edits + sheet management UI**

Header context menus (right-click a row/col header): add in `boot()`:

```js
    $('colhdr').addEventListener('contextmenu', (e) => {
      const t = e.target.closest('.hcell');
      if (!t) return;
      e.preventDefault();
      headerMenu(e, 'col', parseInt(t.dataset.col, 10));
    });
    $('rowhdr').addEventListener('contextmenu', (e) => {
      const t = e.target.closest('.hcell');
      if (!t) return;
      e.preventDefault();
      headerMenu(e, 'row', parseInt(t.dataset.row, 10));
    });
    document.addEventListener('click', () => $('hmenu')?.remove());
```

```js
  function headerMenu(e, kind, at) {
    $('hmenu')?.remove();
    const m = document.createElement('div');
    m.id = 'hmenu';
    m.style.cssText = `position:fixed;left:${e.clientX}px;top:${e.clientY}px;z-index:10;` +
      'background:var(--vscode-menu-background,#252526);color:var(--vscode-menu-foreground,#ccc);' +
      'border:1px solid var(--vscode-editorWidget-border,#454545);padding:4px 0;';
    const items = kind === 'col'
      ? [[`Insert column`, `inscol\t${at}\t1`], [`Delete column`, `delcol\t${at}\t1`]]
      : [[`Insert row`, `insrow\t${at}\t1`], [`Delete row`, `delrow\t${at}\t1`]];
    for (const [label, op] of items) {
      const it = document.createElement('div');
      it.textContent = label;
      it.style.cssText = 'padding:2px 14px;cursor:pointer;';
      it.addEventListener('mouseenter', () => (it.style.background = 'var(--vscode-menu-selectionBackground,#04395e)'));
      it.addEventListener('mouseleave', () => (it.style.background = ''));
      it.addEventListener('click', () => { m.remove(); userCmd(op); requestView(); });
      m.appendChild(it);
    }
    document.body.appendChild(m);
  }
```

Sheet tabs: in `paintTabs()`, append a `+` button (`userCmd('sheet\tadd\tSheet' + (view.sheets.length + 1)); requestView();`) and give each tab a `dblclick` handler that swaps the button for an inline `<input>` prefilled with the name; Enter commits `userCmd('sheet\trename\t<i>\t<value>')`, Escape restores.

- [ ] **Step 4: Package + e2e editing pass**

Rebuild/reinstall as in Task 7. In VS Code with `assets/sample.xlsx`:
type into a cell (dirty dot lights), `=SUM(...)` recalculates dependents, Ctrl+Z/Ctrl+Y undo/redo through VS Code, Ctrl+S saves (reopen the file in the xlsxy TUI to confirm the edit + formatting survived), copy/paste a range, right-click header → insert row shifts formulas, sheet `+`/rename/switch work, invalid formula shows the error (surface `view.err` — set `$('cellref').title = view.err || ''` and flash the formula bar border red when present).

- [ ] **Step 5: Commit**

```bash
git add offxy-vscode/media/grid.js
git commit -m "offxy: spreadsheet editing — cells, clipboard, structural ops, sheets"
```

---

### Task 9: Empty-file create flow for .xlsx

**Files:**
- Create: `offxy-vscode/src/gridengine.ts`
- Modify: `offxy-vscode/src/extension.ts` (grid EDITORS entry gains emptyPrompt/mintEmpty)
- Modify: `offxy-vscode/media/grid.js` (empty state + createNew)

**Interfaces:**
- Consumes: `EditorSpec.mintEmpty` seam (Task 6), `grid_new` export (Task 4), the docx webview's `createNew` message shape.
- Produces: `gridengine.newWorkbook(context: vscode.ExtensionContext): Promise<Uint8Array>`.

- [ ] **Step 1: Host-side gridwasm loader**

`offxy-vscode/src/gridengine.ts` (mirror `engine.ts`'s loader):

```ts
// Host-side gridwasm loader — only `grid_new` is needed on the host (the
// empty-file create flow); the full engine runs in the webview.

import * as vscode from 'vscode';

interface Exports {
  memory: WebAssembly.Memory;
  grid_free(ptr: number, len: number): void;
  grid_new(): number;
}

let cached: Promise<Exports> | undefined;

async function load(context: vscode.ExtensionContext): Promise<Exports> {
  if (!cached) {
    cached = (async () => {
      const uri = vscode.Uri.joinPath(context.extensionUri, 'media', 'gridwasm.wasm');
      const bytes = await vscode.workspace.fs.readFile(uri);
      const module = await WebAssembly.compile(bytes as BufferSource);
      const instance = await WebAssembly.instantiate(module, {});
      return instance.exports as unknown as Exports;
    })();
  }
  return cached;
}

/** Bytes of a fresh empty workbook. */
export async function newWorkbook(context: vscode.ExtensionContext): Promise<Uint8Array> {
  const ex = await load(context);
  const ptr = ex.grid_new();
  const m = new Uint8Array(ex.memory.buffer);
  const len = m[ptr] | (m[ptr + 1] << 8) | (m[ptr + 2] << 16) | (m[ptr + 3] << 24);
  const out = m.slice(ptr + 4, ptr + 4 + len);
  ex.grid_free(ptr, 4 + len);
  return out;
}
```

- [ ] **Step 2: Wire the spec entry**

In `extension.ts`, the grid entry gains:

```ts
emptyPrompt: '“{name}” is empty — it isn’t an Excel workbook yet. Create a new workbook in its place?',
mintEmpty: (ctx) => newWorkbook(ctx),
```

(match however Task 6 parameterized the prompt — if it interpolates the file name in the provider, keep that mechanism).

- [ ] **Step 3: grid.js empty state**

In the `open` message handler, before `grid_open`: if `u8.length === 0`, render the same empty-state pattern as `media/webview.js`'s `showEmptyState()` (a `.empty-state` div with "This file is empty — it isn't an Excel workbook yet." and a **Create new workbook** button that posts `{ type: 'createNew' }` and disables itself). The host answers with a fresh `open` carrying real bytes.

- [ ] **Step 4: e2e**

Rebuild/reinstall. In the explorer create `empty.xlsx` (0 bytes), open it: modal offers Create; dismissing shows the in-tab button; either path lands in an editable one-sheet workbook; Ctrl+S persists it; delete the test file afterwards.

- [ ] **Step 5: Commit**

```bash
git add offxy-vscode
git commit -m "offxy: create-new-workbook flow for empty .xlsx files"
```

---

### Task 10: Docs, changelog, full verification

**Files:**
- Modify: `offxy-vscode/README.md`, `offxy-vscode/CHANGELOG.md`, `VSCODE.md`, `README.md` (root)

**Interfaces:** none new — closure task.

- [ ] **Step 1: Docs sweep**

- `offxy-vscode/README.md`: retitle to Offxy, add a Spreadsheet section (grid, formula bar, recalc via gridcore, structural edits, sheets, lossless save), keep the Word section, update install instructions to `offxy-*.vsix`.
- `offxy-vscode/CHANGELOG.md` Unreleased: add the spreadsheet feature bullet list.
- `VSCODE.md` + root `README.md`: update extension name/paths (`docxy-vscode` → `offxy-vscode`), mention `.xlsx` support.
- Search the repo for remaining `docxy-vscode` references: `grep -rn "docxy-vscode" --include="*.md" --include="*.yml" --include="*.json" .` — update stragglers (skip CHANGELOG history entries and this plan/spec).

- [ ] **Step 2: Full verification**

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test -p gridwasm -p gridcore -p xlsxy -p docxwasm
cd offxy-vscode && npm run typecheck && npm run build
npx --yes @vscode/vsce@latest package --no-dependencies
code --install-extension "$(pwd -W)/offxy-0.3.0.vsix" --force
```
Expected: everything green; final manual pass — open `assets/sample.docx` AND `assets/sample.xlsx`, edit + save both, empty-file flows for both formats.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "offxy: docs + changelog for the merged Word & Excel extension"
```

---

## Self-Review Notes

- Spec coverage: structure/naming (T1, T5), Session+protocol (T1–T4), grid webview (T7–T8), host integration incl. registration table + empty flow (T6, T9), testing (each task + T4 smoke + T10), packaging (T5, T7, T10). Release/0.4.0 is explicitly deferred to Boris.
- gridcore API names in code blocks were verified against the sources this session (`load_xlsx`/`save_xlsx`/`new_xlsx`, `Engine::{new, set_cell, recalc_all, validate, clock}`, `edit::{insert_rows, delete_rows, insert_cols, delete_cols, rename_sheet}`, `SheetPackage::add_sheet`, `Sheet::{cell, set_cell, used_size, col_width}`, `Styles::xf`, `format_with`, `cell_name`, TUI undo model at `xlsxy/src/main.rs:662–1096`). Where a helper's exact field spelling differs, the implementer adapts the plan's code to gridcore — never the reverse.
- Known accepted v1 limits (from spec): merged cells don't span; column x-positions left of the fetched window approximate with the default width; sheet-add undo doesn't remove the worksheet part (TUI parity).
