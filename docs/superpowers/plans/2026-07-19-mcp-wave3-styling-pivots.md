# MCP Wave 3 Implementation Plan — formatting verbs + persistent pivots

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `doc.format` + `doc.set-style` (block-range formatting over an internal selection primitive) and `pivot.create` (real, persistent workbook pivots) — on every surface, 56 MCP tools.

**Architecture:** Layer-sliced as in Waves 1–2: docxy terminal → docxwasm → xlsxy terminal (with the persistence probe FIRST) → gridwasm → extension → MCP → docs+verification. The spec is THE contract; when plan and spec disagree, the spec governs.

**Tech Stack:** Rust (docxcore, docxy, gridcore, xlsxy, docxwasm, gridwasm), TypeScript (extension), Node ESM (server.mjs).

**Spec:** `docs/superpowers/specs/2026-07-19-mcp-wave3-styling-pivots-design.md` — required reading for every implementer.
**Branch:** `claude/mcp-wave3` (stacked on `claude/mcp-wave2`).

## Global Constraints

- No version bumps; no new dependencies; std-only/single-dependency rules hold.
- Existing tests unmodified; existing verbs' replies unchanged.
- Undo contract: `doc.format`/`doc.set-style` = ONE checkpoint each (default tab mapping, steps=1, no undoSteps field). `pivot.create`'s bucket is determined empirically (Task 3) and mapped per the Wave-1 playbook; its inverse removes the created sheet AND the pivot registration — both or neither.
- Set-to-value semantics for bold/italic/underline/strike — never toggle. Determinism tests mandatory.
- The pivot persistence probe (Task 3 Step 1) gates the verb's shape: proven → full verb; disproven → honest error (spec §Part B). No silent session-only pivots.
- Error strings: reuse the `cell.format` family verbatim where applicable (`patch needs at least one key`, unknown-key naming, `bad color '<v>'`); new strings fixed in the spec (`set-style needs 'style' or 'align'`, unknown-style listing the accepted set).
- **Windows agent shell quirks:** every cargo/npm command runs in bash with
  ```bash
  export PATH="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
  Never pipe exit-code-bearing commands through `tail`. Packaging: `npx --yes @vscode/vsce@latest package --no-dependencies`.

---

### Task 1: docxy formatting verbs (docxcore + terminal)

**Files:**
- Modify: `docxcore/src/agent.rs`, `docxy/src/control.rs`

**Interfaces:**
- Consumes: the selection mechanics inside `agent::replace_range` (anchor/caret construction — extract, don't duplicate); `Editor::{set_font, set_font_size, set_color, set_highlight, set_align, set_para_style}` and the RunProps fields behind bold/italic/underline/strike (read editor.rs + model.rs for exact names); Wave 2's `Package::ensure_styles` + `MARKDOWN_PARAGRAPH_STYLE_IDS`; `Editor::checkpoint` conventions.
- Produces (Task 2 mirrors; lock names + contracts):
  ```rust
  // docxcore/src/agent.rs
  pub struct RunPatch { /* bold/italic/underline/strike: Option<bool>, color: Option<(u8,u8,u8)>, highlight: Option<...>, font: Option<String>, size: Option<f32-or-what-set_font_size-takes> */ }
  impl RunPatch { pub fn parse(pairs: /* host-buildable shape, JSON-free — follow FormatPatch's precedent */) -> Result<RunPatch, String>; }
  pub fn format_range(ed: &mut Editor, start: usize, end: usize, patch: &RunPatch) -> Result<usize, String>;  // blocks touched; ONE checkpoint; SET semantics applied per run
  pub fn set_style_range(ed: &mut Editor, start: usize, end: usize, style: Option<&str>, align: Option<Align-ish>) -> Result<usize, String>; // ONE checkpoint
  ```
  Decompose via a private selection helper; if the Editor's setters already operate on selections, prefer selection+setter; where only toggles exist (bold/italic/underline/strike), apply values directly to the selected runs' props under one checkpoint. Adjust internals freely; keep names, reply counts, checkpoint counts.
- Control arms: `doc.format` / `doc.set-style` per the spec tables — `require_para`/bounds wording reused; style application calls `ensure_styles` for markdown-set ids (after validation, before mutation — Wave 2's ordering discipline); `Normal` clears without ensuring.

- [ ] **Step 1: TDD (core).** Determinism: bold:true over a mixed bold/plain selection → every run bold; bold:false clears all; repeat-application idempotent. One-checkpoint: format then single undo restores EXACT prior props (assert model equality). set_style Heading1 → paragraph style_id set + one undo reverts. Bounds/require_para errors touch nothing.
- [ ] **Step 2: TDD (control).** Dispatch tests: patch key coverage incl. color parsing reuse; `patch needs at least one key`; unknown key named; unknown style error lists the accepted set; `set-style needs 'style' or 'align'`; ensure_styles ran for Heading1 into a bare package (part assertion) and did NOT run for Normal/align-only; round-trip: format bold + set-style Heading1 → `doc.export {format:"markdown"}` shows `**` and `#`.
- [ ] **Step 3: RED → implement → GREEN** (`cargo test -p docxcore -p docxy`, existing tests unmodified). **Step 4:** Gates; commit — `"docxy: doc.format and doc.set-style over the block-selection primitive"`

---

### Task 2: docxwasm mirrors

**Files:**
- Modify: `docxwasm/src/bridge.rs`

- [ ] **Step 1: TDD.** Byte-shape parity with Task 1's arms (reply key sets, error strings); one-dispatch("undo") restoration per verb; ensure_styles-on-Session's-package test (bare package + set-style Heading1 → styles part present); round-trip via ctl doc.export; no undoSteps field on either reply (default mapping).
- [ ] **Step 2: Implement** reusing Task 1's agent functions + the host's Wave-2 ensure ordering. **Step 3: GREEN** + wasm32 + fmt/clippy. **Step 4: Commit** — `"docxwasm: doc.format and doc.set-style ctl mirrors"`

---

### Task 3: xlsxy persistent pivots (gridcore + terminal) — probe FIRST

**Files:**
- Modify: `xlsxy/src/control.rs`, `gridcore` (pivot-creation helper extraction per the >10-line rule)

**Interfaces:**
- Consumes: the TUI's pivot-creation flow (`open_pivot_editor`/`create_pivot_from`/`apply_pivot_edit`, xlsxy/src/main.rs ~1247-1533), `SheetPackage::add_pivot` (gridcore/src/xlsx.rs ~2056), `pivot::rewrite_pivot_definition` (~pivot.rs:583), `pivot::refresh_pivots`, Wave-1's `pivot_spec_from_names`/`Agg::from_verb_name` (arg parsing reuse), the TUI's placement behavior (new sheet).

- [ ] **Step 1: THE PERSISTENCE PROBE (before any verb code).** Gridcore-level test: build a pivot the way the TUI does → `save_xlsx` → `load_xlsx` → assert the pivot definition survives (in `workbook.pivots`) AND `refresh_pivots` recomputes on the reloaded workbook. Report the result HONESTLY. If it fails and the gap is small+obvious (e.g. one missing splice in save_xlsx for a part the model already builds), report the finding and STOP for controller adjudication — do NOT silently expand scope; if it passes, proceed.
- [ ] **Step 2: TDD (verb).** `pivot.create` dispatch tests: create → reply `{sheet,name}` → `pivot.list` includes it → output sheet holds computed values → source edit + `wb.recalc` refreshes → save/load → still present + refreshable. Name collision → generated `PivotN` unique; explicit duplicate name → error. Unknown header error parity with `sheet.pivot`. Undo semantics: determine empirically what the TUI's creation does to undo stacks (expected history-clear like sheet.add) — report the bucket + the exact inverse contract (remove created sheet AND pivot registration — find/build the removal path; if no pivot-removal core function exists, that's part of the extraction, both-or-neither).
- [ ] **Step 3: RED → implement → GREEN** (`cargo test -p gridcore -p xlsxy`). **Step 4:** Gates; commit — `"xlsxy: pivot.create — persistent workbook pivots"` (or, on a failed probe with controller-approved fallback: the honest-error variant + spec-amended commit).

---

### Task 4: gridwasm mirrors

**Files:**
- Modify: `gridwasm/src/bridge.rs`

- [ ] **Step 1: TDD.** Byte-parity with Task 3's arms; undo-integrity per Task 3's reported bucket (history-clear + inverse per the Wave-1 stash/inverse playbook — the inverse removes sheet AND registration; both-or-neither test: after inverse, `pivot.list` empty AND sheet gone); create→list→recalc-refresh in wasm; internal fields per convention.
- [ ] **Step 2: Implement. Step 3: GREEN** + wasm32 + fmt/clippy. **Step 4: Commit** — `"gridwasm: pivot.create ctl mirror"`

---

### Task 5: extension config + harness

**Files:**
- Modify: `offxy-vscode/src/extension.ts`

- [ ] **Step 1:** docxy sets: `doc.format`, `doc.set-style` (mutating, default steps=1). xlsxy sets: `pivot.create` (mutating, bucket per Task 4's contract).
- [ ] **Step 2: Harness** (scratchpad, extend Wave-2's): both docxy verbs over TCP with spec-shaped replies + error-string parity; format→export round-trip on a tab; pivot.create over TCP → reply + pivot.list + repaint; inverse routing recorded per bucket; internal-field hygiene. `ALL OK` exit 0.
- [ ] **Step 3:** typecheck/build/package/install. **Step 4: Commit** — `"offxy: tabs answer the wave-3 verb surface"`

---

### Task 6: MCP tools (56)

**Files:**
- Modify: `docxy/src/mcp.rs`, `xlsxy/src/mcp.rs`, `offxy-vscode/mcp/server.mjs` (+ ctlcore only if a schema helper is missing)

- [ ] **Step 1:** `docxy_format` (patch object schema — the 8 keys, typed, descriptions; required `["start","patch"]`), `docxy_set_style` (style enum-ish description listing the accepted ids + align values; required `["start"]` with the ≥1-of rule described), `xlsxy_pivot_create` (mirroring `xlsxy_pivot`'s schema + `name?`; required `["range","rows","values"]` — match `sheet.pivot`'s actual required set). Appended last; VERB_TABLEs + order/required/cardinality tests extended; server.mjs mirrors character-identical.
- [ ] **Step 2:** Parity harness: 56 tools, 0 diffs vs rebuilt binaries. Live smoke: `docxy_format` bold + `xlsxy_pivot_create` end-to-end (isolated APPDATA).
- [ ] **Step 3: Commit** — `"offxy + docxy + xlsxy: wave-3 formatting and pivot tools (56-tool surface)"`

---

### Task 7: docs + full verification

**Files:**
- Modify: `docs/agent-control.md`, `offxy-vscode/README.md`, `offxy-vscode/CHANGELOG.md`

- [ ] **Step 1: Docs.** Verb rows (patch key table for doc.format incl. the highlight-name set from the core enum; set-style accepted ids incl. Normal + align values; pivot.create args/reply/placement/refresh-via-recalc + the persistence statement matching what Task 3 proved); tabs section (both docxy verbs one Ctrl+Z; pivot.create's bucket + inverse behavior); tool lists → 56; CHANGELOG.
- [ ] **Step 2: Full gates** (7-crate tests, wasm32 ×2, typecheck/build/vsce/install) + harness + parity re-runs vs FINAL artifacts; exit codes reported.
- [ ] **Step 3: Manual e2e for Boris** (report): doc.format bold + set-style Heading1 in a live tab → renders, one Ctrl+Z each, Word shows the heading styled; pivot.create in a live tab → new sheet appears, source edit + recalc refreshes, Ctrl+Z removes sheet AND pivot.
- [ ] **Step 4: Commit** — `"offxy: document the wave-3 surface"`

## Self-Review Notes

- Spec coverage: Part A → Tasks 1-2 (+5/6/7); Part B incl. the probe → Tasks 3-4 (+5/6/7); the probe's stop-for-adjudication rule mirrors the spec's honest-error fallback; set-to-value determinism and both-or-neither inverse are pinned in Global Constraints and tested.
- Type consistency: `RunPatch`/`format_range`/`set_style_range` (Tasks 1-2); `pivot.create` reply `{sheet,name}` (Tasks 3-6); tool names `docxy_format`/`docxy_set_style`/`xlsxy_pivot_create` (Tasks 6-7).
- Empirical facts to determine and report: RunProps/setter exact APIs + highlight-name set (T1), TUI pivot-creation undo behavior + removal path existence (T3), pivot persistence (T3 probe — the wave's one gate).
