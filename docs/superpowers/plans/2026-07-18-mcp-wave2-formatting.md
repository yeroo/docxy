# MCP Wave 2 Implementation Plan — markdown writes + cell formatting

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `markdown: true` on docxy's three edit verbs (formatted writes via block-splicing), and `cell.format`/`col.width` + `cell.get` format read-back in xlsxy — on every surface, Wave-1 conventions unchanged.

**Architecture:** Layer-sliced like Wave 1: docxy terminal → docxwasm → xlsxy terminal → gridwasm → extension → MCP → docs+verification. The spec's tables are THE contract; when plan and spec disagree, the spec governs.

**Tech Stack:** Rust (docxcore, docxy, gridcore, xlsxy, docxwasm, gridwasm), TypeScript (extension), Node ESM (server.mjs).

**Spec:** `docs/superpowers/specs/2026-07-18-mcp-wave2-formatting-design.md` — required reading for every implementer.
**Branch:** `claude/mcp-wave2` (stacked on `claude/mcp-wave1`).

## Global Constraints

- No version bumps; no new dependencies; std-only/single-dependency rules hold.
- Existing behavior byte-identical when the new arg/keys are absent: `markdown` defaults false → plain-text path untouched; `cell.get` without styling → no `format` key; existing tests pass unmodified.
- **Undo-step parity is the load-bearing invariant:** markdown insert/append = 1 checkpoint; markdown replace-range = SAME step count as plain-text replace-range on the same range (2 non-empty / 1 empty), reported via the established internal `undoSteps`. Wave 1's tab mapping must need ZERO changes for docxy.
- Wire parity tab-vs-terminal; MCP parity across servers (extend the committed verb-map/table tests and the parity harness).
- Error strings byte-identical across surfaces where JS produces them. New errors (spec): `empty markdown`; `patch needs at least one key`; unknown patch key error naming the key; non-positive width error.
- **Windows agent shell quirks:** every cargo/npm command runs in bash with
  ```bash
  export PATH="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
  Never pipe exit-code-bearing commands through `tail`. Packaging: `npx --yes @vscode/vsce@latest package --no-dependencies`.

---

### Task 1: docxy markdown writes (docxcore + terminal)

**Files:**
- Modify: `docxcore/src/agent.rs`, `docxy/src/control.rs`

**Interfaces:**
- Consumes: `docxcore::markdown::from_markdown` (read its signature + what it returns — a `Document` whose body carries the parsed blocks; also read `new_markdown_package` in package.rs for what list content needs from the package: `Package::ensure_list(bullet)` at package.rs:322), the existing `agent::{insert, append, replace_range}` text functions and their checkpoint accounting (replace_range returns `(replaced, undo_steps)` — Wave-1 fact: 2 when the deleted range is non-empty, 1 when empty), `Editor`'s checkpoint mechanism (same crate — read how `replace_all` at editor.rs:~1193 calls `checkpoint`).
- Produces (Task 2 mirrors; exact signatures locked here):
  ```rust
  // docxcore/src/agent.rs — block-splice twins of the text functions.
  // Parse markdown → Vec<Block>; error "empty markdown" if no blocks result.
  pub fn parse_markdown_blocks(text: &str) -> Result<Vec<Block>, String>;
  pub fn insert_blocks(ed: &mut Editor, at: usize, blocks: Vec<Block>) -> Result<(), String>;   // 1 checkpoint
  pub fn append_blocks(ed: &mut Editor, blocks: Vec<Block>);                                     // 1 checkpoint
  pub fn replace_range_blocks(ed: &mut Editor, start: usize, end: usize, blocks: Vec<Block>) -> Result<(usize, usize), String>; // (replaced, undo_steps) — SAME counts as text variant
  ```
  If the Editor API makes a different decomposition cleaner (e.g. one splice helper the three wrap), adjust shapes but KEEP the names, the `(replaced, undo_steps)` contract, and the checkpoint counts — Task 2 and the spec depend on them.
- List handling: the CONTROL layer (which owns the `Package`) checks whether the parsed blocks reference numbering and calls `ensure_list` before splicing — find how `from_markdown`-produced list paragraphs mark themselves (read `markdown.rs`'s list emission: which numId they reference) and ensure both bullet(1)/decimal(2) definitions exist when referenced.

- [ ] **Step 1: TDD (core).** In agent.rs tests:

```rust
#[test]
fn markdown_insert_splices_formatted_blocks_with_one_checkpoint() {
    let mut ed = editor_with(&["existing"]);  // reuse the module's fixture style
    let blocks = parse_markdown_blocks("# Title\n\nbody with **bold**").unwrap();
    insert_blocks(&mut ed, 1, blocks).unwrap();
    assert_eq!(ed.doc().body.len(), 3);
    // heading landed as a styled paragraph, bold as a styled run — assert via
    // the model (paragraph style name / run props), not just text
    assert!(ed.undo());        // one undo removes the whole splice
    assert_eq!(ed.doc().body.len(), 1);
    assert!(!ed.undo() || ed.doc().plain_text() == "existing\n"); // nothing else on the stack from us
}

#[test]
fn markdown_replace_range_matches_text_variant_step_counts() {
    // non-empty range → 2 steps; empty paragraph → 1 step; mirror the exact
    // assertions of the existing replace_range step-count tests.
}

#[test]
fn empty_markdown_errors_and_touches_nothing() {
    assert_eq!(parse_markdown_blocks("   \n").unwrap_err(), "empty markdown");
}
```

- [ ] **Step 2: TDD (control).** Dispatch tests: `doc.insert {at, text, markdown:true}` with a table+list+link markdown → `doc.export {format:"markdown"}` round-trips the structure (contains the table row, the list item, the link); `markdown:false`/absent byte-matches the old plain-text behavior (pin one existing-style assertion); zero-block markdown → error `empty markdown`, doc unmodified, `!app.modified`; list markdown into a package WITHOUT numbering → numbering ensured (assert via `pkg.part_names()` or marker rendering — whichever the fixture can see).
- [ ] **Step 3: RED → implement → GREEN.** `cargo test -p docxcore -p docxy`; existing tests unmodified.
- [ ] **Step 4: The supported-constructs table.** Empirically splice each spec-listed construct into an EXISTING doc fixture and record: works / degraded (how) / errors. Put the table in your report — Task 7 turns it into docs. Silent degradation found = fix or error, per spec.
- [ ] **Step 5: Gates + commit** — fmt/clippy; `git add docxcore docxy && git commit -m "docxy: markdown-formatted writes on insert/replace-range/append"`

---

### Task 2: docxwasm mirror

**Files:**
- Modify: `docxwasm/src/bridge.rs`

- [ ] **Step 1: TDD.** `Session::ctl` honors `markdown:true` on the three verbs: formatted splice lands (assert via `doc.export` through ctl); `undoSteps` parity — markdown replace-range non-empty range reports 2, empty reports 1, insert/append report 1; flag-absent byte-parity with Wave-1 replies; `empty markdown` error; one `dispatch("undo")` per reported step restores.
- [ ] **Step 2: Implement** reusing Task 1's agent functions (the wasm Session owns a Package too — same `ensure_list` handling; read how Task 1's control layer did it and mirror). **Step 3: GREEN** + wasm32 build + fmt/clippy. **Step 4: Commit** — `"docxwasm: markdown-formatted ctl writes"`

---

### Task 3: xlsxy formatting (gridcore + terminal)

**Files:**
- Modify: `xlsxy/src/control.rs`, `gridcore/src/sheet.rs` (or best-fitting module) for the shared patch helper

**Interfaces:**
- Consumes: `Xf` (sheet.rs:626-637 — bold/italic/font-color/fill-color/align fields; read exact names/types), `Styles::intern` (sheet.rs:694), the TUI's `apply_format` (xlsxy/src/main.rs:2157 — the mutation path incl. undo grouping; VERIFY which undo mechanism format changes use — apply_on-style group expected, report it), `numfmt::parse_format` (numfmt.rs:86 — validate the numFmt code before applying), `Sheet::{col_width, set_col_width}` (sheet.rs:381,404), the TUI's width-change undo behavior (main.rs:5346 area — empirical fact to report for Task 4/5).
- Produces (Task 4 reuses — pure gridcore helper, named in your report):
  ```rust
  // gridcore — pure: parse a wire patch into an Xf-transform; error on unknown
  // keys (naming the key), empty patch, bad color, bad numFmt, bad align.
  pub struct FormatPatch { /* six optional fields, exact types per Xf */ }
  impl FormatPatch { pub fn parse(/* key-value pairs or a small adapter the hosts build */) -> Result<FormatPatch, String>; }
  pub fn apply_patch_to_xf(base: &Xf, patch: &FormatPatch) -> Xf;
  ```
  (Adapt the parse input shape to what both control.rs and gridwasm can build without a JSON type in gridcore — e.g. `&[(String, String)]` or a builder; keep it dependency-free.)
- `cell.get` read-back: additive present-if-set `format` object — keys only where the cell's Xf differs from default; build the reverse mapping (Xf → wire keys) next to the patch helper so both directions live together.

- [ ] **Step 1: TDD.** Control tests: `cell.format` bold+fill over a 2×2 range → `{formatted:4}`, `cell.get.format` echoes on each cell, ONE undo restores all and `cell.get` shows no `format`; patch with unknown key → error naming it; empty patch → `patch needs at least one key`; bad `numFmt` code → error, nothing applied; `col.width` set + read-back via whatever exposes it, non-positive width error; save→load round-trip test (gridcore level): format applied → `save_xlsx` → `load_xlsx` → Xf survives.
- [ ] **Step 2: RED → implement → GREEN** (`cargo test -p gridcore -p xlsxy`, existing tests unmodified). **Step 3: Gates + commit** — `"xlsxy: cell.format and col.width verbs with format read-back"`

---

### Task 4: gridwasm mirror

**Files:**
- Modify: `gridwasm/src/bridge.rs`

- [ ] **Step 1: TDD.** Byte-shape parity with Task 3's control arms (exact key sets on cell.format reply + cell.get.format); undo-integrity: cell.format = the bucket Task 3 reported (expected A, internal `undoSteps:1` — one dispatch("undo") restores); col.width per Task 3's empirical bucket (if not on the wasm stack, inverse op carrying prior width per the Wave-1 playbook — document); flag-absent cell.get parity.
- [ ] **Step 2: Implement** reusing the gridcore helpers. **Step 3: GREEN** + wasm32 + fmt/clippy. **Step 4: Commit** — `"gridwasm: cell.format and col.width ctl mirrors"`

---

### Task 5: extension config + harness

**Files:**
- Modify: `offxy-vscode/src/extension.ts` (xlsxy EDITORS sets only — docxy needs nothing: the flag rides existing verbs)

- [ ] **Step 1:** Add `cell.format`, `col.width` to xlsxy wasmVerbs + mutatingVerbs (bucket per Task 4's report).
- [ ] **Step 2: Harness** (scratchpad, extend Wave 1's): markdown insert over TCP on a docx tab → export round-trip shows formatting, undoSteps on the wire NEVER (internal-field checks on the changed verbs); cell.format over TCP → reply + cell.get.format byte-shaped like terminal; undo routing recorded per bucket; flag-absent calls unchanged vs Wave-1 recordings. `ALL OK` exit 0.
- [ ] **Step 3:** typecheck/build/package/install. **Step 4: Commit** — `"offxy: tabs honor markdown writes and cell formatting"`

---

### Task 6: MCP schemas + tools

**Files:**
- Modify: `docxy/src/mcp.rs`, `xlsxy/src/mcp.rs`, `offxy-vscode/mcp/server.mjs`

- [ ] **Step 1:** docxy: add optional `markdown` boolean prop (one shared description string: `"Parse text as Markdown (headings, bold, lists, tables, links) instead of plain text."`) to `docxy_insert`/`docxy_replace_range`/`docxy_append` — required arrays UNCHANGED. xlsxy: new tools `xlsxy_format` (→`cell.format`, required `["range","patch"]`, patch = object schema with the six optional typed properties + descriptions) and `xlsxy_col_width` (→`col.width`, required `["col","width"]`), appended last, both with `target`. Update the committed verb-map tables (`VERB_TABLE` + tests) and order/required tests → 53 tools.
- [ ] **Step 2:** server.mjs mirrors (defs + verb-map; character-identical descriptions).
- [ ] **Step 3:** Parity harness re-run: 53 tools, 0 def diffs, 0 verb-map diffs vs rebuilt release binaries; live smoke: `docxy_append {markdown:true}` + `xlsxy_format` end-to-end (isolated APPDATA). **Step 4: Commit** — `"offxy + docxy + xlsxy: markdown flag and formatting tools (53-tool surface)"`

---

### Task 7: docs + full verification

**Files:**
- Modify: `docs/agent-control.md`, `offxy-vscode/README.md`, `offxy-vscode/CHANGELOG.md`

- [ ] **Step 1: Docs.** agent-control.md: the `markdown` flag on the three verbs + Task 1's supported-constructs table (verbatim honesty — degraded/unsupported constructs listed); `cell.format`/`col.width` verb rows + patch key table + `cell.get.format` read-back note; tool lists → 53. README tools list; CHANGELOG entry.
- [ ] **Step 2: Full gates** (Wave-1 list: fmt/clippy/7-crate tests/wasm32 ×2/typecheck/build/vsce/install) + re-run Task 5 harness and Task 6 parity harness against FINAL artifacts. Exit codes reported.
- [ ] **Step 3: Manual e2e additions for Boris** (report): agent writes a markdown report (heading+table+list) into a live tab and it renders formatted with one-Ctrl+Z-per-step undo; `xlsxy_format` bolds a range in a live tab, Ctrl+Z unbolds.
- [ ] **Step 4: Commit** — `"offxy: document the wave-2 formatting surface"`

## Self-Review Notes

- Spec coverage: Part A → Tasks 1-2 (+6 schemas, 7 docs); Part B → Tasks 3-4 (+5 config, 6 tools, 7 docs); undo-parity invariant pinned in Global Constraints and tested at both docxy layers; error cases enumerated in Global Constraints and tested in Tasks 1/3.
- Type consistency: `parse_markdown_blocks`/`insert_blocks`/`append_blocks`/`replace_range_blocks` names used in Tasks 1-2; `FormatPatch`/`apply_patch_to_xf` in Tasks 3-4; tool names `xlsxy_format`/`xlsxy_col_width` in Tasks 6-7.
- Empirical facts implementers determine and report: list-numbering references in from_markdown output (T1), the supported-constructs table (T1), format/width undo buckets in the TUI (T3), Xf field exact names (T3).
