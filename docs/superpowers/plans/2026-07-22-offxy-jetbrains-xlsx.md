# Offxy JetBrains xlsx Grid Editor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The second offxy-jetbrains editor: a native virtualized spreadsheet over `gridwasm.wasm` on Chicory — full editing (values/formulas + recalc, formatting, structural ops, sheets, TSV clipboard), engine-stack undo, lossless save, and the xlsxy agent ctl bridge.

**Architecture:** `GridEngine` (thin second client over a shared Chicory marshalling base) drives gridwasm's windowed viewport protocol; `GridPanel` is a lazy-model JBTable refreshed per `view` window; every mutation is one engine command = one `UndoableAction` driving engine `undo`/`redo` (no snapshots, no reconcile — the cell-transactional model has no native-edit interleaving). `XlsxFileType` claims the extension (the docx Word-launch lesson); `GridCtlBridge` reuses `CtlServer`/`Discovery` against xlsxy's ctl dir with `grid_ctl` passthrough.

**Tech Stack:** Kotlin/JVM 17 in the existing `offxy-jetbrains/` Gradle module (no new deps), Rust untouched (gridwasm already has everything: `view/select/set/clear/copy/cut/paste/fmt/decimals/autosum/undo/redo/structural/sheet` dispatch + the full `grid_ctl` verb surface + `grid_new`).

**Spec:** `docs/superpowers/specs/2026-07-22-offxy-jetbrains-xlsx-design.md`
**References:** `gridwasm/src/bridge.rs` (dispatch + view JSON, the protocol contract), `offxy-vscode/media/grid.js` (the windowed-grid behavior being paralleled), `docs/agent-control.md` (xlsxy verbs + VS Code tab semantics to mirror), the docx editor's files (patterns to reuse, not rebuild).

## Global Constraints

- **No Rust changes.** gridwasm/gridcore stay untouched; if a protocol gap appears, STOP and report (it likely means a misreading of `bridge.rs`).
- **No behavior change to the docx editor**; the `WasmBinding` extraction must keep all 37 existing tests green unchanged.
- Runtime deps: still **Chicory only**.
- Ctl wire fidelity: reuse `CtlServer` as-is; instance `xlsxy-jetbrains-<sanitized basename>-<pid>-<n>`; ctl dir `%APPDATA%\xlsxy\ctl` (tests: the `offxy.ctl.dir` property gains an app-name parameter — keep docxy tests passing).
- Gates: `./gradlew test --rerun` (build cache replays results otherwise) + `buildPlugin`; `cargo test -p gridwasm` only to confirm baseline (should be untouched).
- **JAVA_HOME:** `~/scoop/apps/temurin17-jdk/current`; cargo needs the wasm32 target already installed. Never pipe exit-code-bearing commands through `tail`.
- Fixtures: `assets/sample.xlsx` + picks from `corpus/xlsx` (538 real LO/AOO files — one formula-heavy, one large for the benchmark).

---

### Task 1: `WasmBinding` extraction + `GridEngine` + tests + benchmark

**Files:**
- Create: `engine/WasmBinding.kt`, `engine/GridEngine.kt`
- Modify: `engine/ChicoryEngine.kt` (extend the base; public surface unchanged)
- Create: `src/test/kotlin/dev/yeroo/offxy/engine/GridEngineTest.kt`, `engine/GridBenchmark.kt`
- Modify: `build.gradle.kts` (`buildWasm` grows a gridwasm build + resource copy, same staleness pattern)

**Interfaces:**
- Consumes: `gridwasm/src/lib.rs` ABI (`grid_alloc/free/open/close/cmd/save/new/ctl` — same length-prefixed idiom as docx).
- Produces:
  ```kotlin
  class GridEngine : AutoCloseable {            // one Chicory instance per workbook
    fun open(bytes: ByteArray): Boolean
    fun cmd(command: String): String            // tab-delimited → view JSON (or TSV for copy)
    fun save(): ByteArray
    fun ctl(requestJson: String): String
    companion object { fun newWorkbook(): ByteArray }   // grid_new
  }
  ```
  `view`/`select`/`set`… command strings are `bridge.rs`'s exact dispatch forms.

- [ ] **Step 1:** Extract the marshalling base from ChicoryEngine (module load by resource name, alloc/write/call/read/free); docx tests stay green untouched.
- [ ] **Step 2: Failing GridEngine tests** on real fixtures: open+`view` returns sheets/dims/cells JSON; window clips at edges; `set` recalcs dependents (fixture with `=SUM`); `copy` returns TSV; `paste` round-trips; `insrow` rewrites references; `undo` restores; `grid_new` bytes reopen; `ctl` `sheet.read` answers.
- [ ] **Step 3:** Implement; GREEN.
- [ ] **Step 4: Benchmark** (report, not assert): `view` window latency + `set`+recalc on the largest corpus workbook; p50/p95 printed. Expectation O(window); a surprise here is a STOP-and-report.
- [ ] **Step 5: Commit** — `"offxy-jetbrains: GridEngine — gridwasm on the shared Chicory binding"`

---

### Task 2: File type, provider, read-only grid rendering

**Files:**
- Create: `grid/XlsxFileType.kt`, `grid/XlsxEditorProvider.kt`, `grid/XlsxFileEditor.kt`, `grid/GridPanel.kt`, `grid/GridViewModel.kt` (parse the view JSON)
- Modify: `plugin.xml` (fileType + provider)

**Interfaces:**
- Consumes: Task 1's engine; view JSON fields (`sheets/active/dims/colw/cells/sel/cur/dirty`); `JBTable` + row-header table in a `JBScrollPane`.
- Produces: opening an `.xlsx` shows a virtualized grid — lazy model over the used extent (+ margin), viewport cache refreshed on scroll/resize (debounced `view` command), A/B/C column headers + numbered row header, widths from `colw` × char width, per-cell alignment/bold/italic/colors from the cache, theme-aware. **File type registered so Excel does NOT launch** (the docx lesson, test-pinned).

- [ ] **Step 1:** `GridViewModel` + unit tests against captured view JSON.
- [ ] **Step 2:** File type + provider + `GridPanel` read-only rendering; platform tests: xlsx maps to `Offxy Excel Workbook`; provider accepts; model exposes fixture values; scroll window refresh fetches new cells (drive the model directly).
- [ ] **Step 3:** `runIde` smoke on corpus workbooks (rendering, scrolling, themes).
- [ ] **Step 4: Commit** — `"offxy-jetbrains: virtualized xlsx grid — rendering over the viewport protocol"`

---

### Task 3: Editing — cells, formula bar, clipboard, undo, save

**Files:**
- Create: `grid/FormulaBar.kt`
- Modify: `grid/GridPanel.kt`, `grid/XlsxFileEditor.kt`

**Interfaces:**
- Consumes: `set/select/clear/copy/cut/paste` commands; `UndoManager` + `BasicUndoableAction` (engine-stack variant: undo → `cmd("undo")`, redo → `cmd("redo")`, repaint from returned view); `CopyPasteManager`.
- Produces: type-through and F2/double-click cell editing (Enter ↓, Tab →, Esc cancels); formula bar ⇄ active cell (`cur.ref`/`cur.src`, edits commit through the same path); click/drag/Shift+arrow selection mirrored via `select`; Ctrl+C/X/V as TSV through the OS clipboard; Delete clears. Every mutating command: one `UndoableAction` (engine stack), `PROP_MODIFIED` from the view's dirty flag. Save/Save All/close-save/external-reload/0-byte-create (`grid_new`) mirroring the docx editor's flows.

- [ ] **Step 1:** Selection/editing/commit + formula bar.
- [ ] **Step 2:** Clipboard + delete + undo actions + dirty/save/reload/create.
- [ ] **Step 3: Platform tests:** `set` via the editor path updates the model and fires modified; platform undo restores the old value AND the engine agrees; formula bar shows `=SUM(...)` for the active cell; TSV paste lands as one undo step; save bytes reopen in a fresh engine with the edit.
- [ ] **Step 4:** `runIde` manual: edit/undo/save a corpus workbook; reopen in terminal xlsxy for fidelity.
- [ ] **Step 5: Commit** — `"offxy-jetbrains: grid editing — cells, formula bar, clipboard, engine-stack undo"`

---

### Task 4: Sheets, structural ops, formatting toolbar, empty-file

**Files:**
- Create: `grid/SheetTabs.kt`, `grid/GridToolbar.kt`
- Modify: `grid/GridPanel.kt`, `grid/XlsxFileEditor.kt`, `plugin.xml` (actions if palette-exposed)

**Interfaces:**
- Consumes: `sheet\tswitch/add/rename`, `insrow/delrow/inscol/delcol`, `fmt\t<key>`, `decimals`, `autosum` commands (all existing).
- Produces: bottom sheet strip (click/+, double-click rename); context menu on headers/selection for insert/delete rows/columns; toolbar (labels like docx: B, I, align glyphs, `.0±`, `Σ`); all through the same one-command-one-undo path.

- [ ] **Step 1–2:** Implement; platform tests: sheet add+switch changes the model; `insrow` shifts values and undoes; `fmt bold` toggles the cached cell style.
- [ ] **Step 3: Commit** — `"offxy-jetbrains: sheets, structural edits, formatting toolbar"`

---

### Task 5: xlsxy agent ctl bridge

**Files:**
- Create: `grid/GridCtlBridge.kt`
- Modify: `ctl/Discovery.kt` (app-parameterized ctl dir; docxy default preserved), `grid/XlsxFileEditor.kt`

**Interfaces:**
- Consumes: `CtlServer` (unchanged); `GridEngine.ctl`; agent-control.md's xlsxy verb tables + VS Code tab semantics.
- Produces: per open workbook, instance `xlsxy-jetbrains-<name>-<pid>-<n>` in xlsxy's ctl dir; host verbs `wb.path` (composed with internal `wb.info`), `wb.save`, `wb.reload`, `wb.open` (new tab, `{path}` reply); `wb.info` rejected externally (`unknown verb` parity); everything else through `grid_ctl`; mutating verbs get the same engine-stack `UndoableAction` as UI edits.

- [ ] **Step 1:** Discovery parameterization (docxy tests untouched) + bridge routing with unit tests (fake engine).
- [ ] **Step 2: e2e platform test:** real TCP → `wb.path`, `sheet.read`, `cell.set` (view updates + modified + one platform undo reverses), dispose cleans discovery.
- [ ] **Step 3:** `runIde` manual: `xlsxy --mcp` lists the IDE workbook; live `xlsxy_set` repaints; Ctrl+Z undoes it.
- [ ] **Step 4: Commit** — `"offxy-jetbrains: workbooks advertise on the xlsxy control surface"`

---

### Task 6: Docs + verification

**Files:**
- Modify: `offxy-jetbrains/README.md` (+ xlsx section), `CHANGELOG.md`, `TESTPLAN.md` (grid sections), `docs/agent-control.md` (JetBrains tabs: xlsxy paragraph), root `README.md` (JetBrains section mentions both formats)

- [ ] **Step 1:** Full `./gradlew test --rerun buildPlugin` (docx + grid suites); zip contents include both wasm artifacts.
- [ ] **Step 2:** Docs; TESTPLAN gains grid manual sections (rendering/editing/recalc/sheets/agent).
- [ ] **Step 3:** Branch + PR (CI's existing `jetbrains` job covers the new tests); merge on green per Boris.
- [ ] **Step 4: Commit** — `"offxy-jetbrains: xlsx editor — docs and verification"`

## Self-Review Notes

- No Rust work anywhere: gridwasm's dispatch + ctl surface (post grid-overhaul + agent-access) already covers every UI need; the plan STOPs if that reading is wrong rather than improvising engine changes.
- Undo model deliberately differs from docx (engine stack, no snapshots) and the spec says why; the divergent ctl undo semantics (sheet.remove single-slot restore) mirror the documented VS Code tab behavior.
- The `WasmBinding` extraction is the only touch on docx-editor code and is gated on its 37 tests staying green.
- Fixtures come from the real corpus (538 workbooks) — formula-heavy and large picks, not synthetic toys.
