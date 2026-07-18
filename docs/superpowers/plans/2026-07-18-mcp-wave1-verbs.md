# MCP Wave 1 Verb Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose ~20 already-implemented capabilities as ctl verbs + MCP tools on every surface (terminal docxy/xlsxy, VS Code tabs, both MCP servers) with full adapted parity.

**Architecture:** Layer-sliced like the agent-access plan: terminal core+dispatch first (docxy then xlsxy), then the wasm ctl mirrors, then extension wiring, then MCP tools on both servers, then docs+verification. Every verb reuses an existing core function — the spec's admission rule.

**Tech Stack:** Rust (docxcore, docxy, gridcore, xlsxy, docxwasm, gridwasm), TypeScript (extension), Node ESM (server.mjs).

**Spec:** `docs/superpowers/specs/2026-07-18-mcp-wave1-verbs-design.md` — its two verb tables are THE contract: args, reply keys, semantics, error rules. Every task below implements rows of those tables; when this plan and the spec disagree, the spec governs. The research doc `docs/superpowers/research/2026-07-18-mcp-tool-opportunities.md` names each verb's core function and file location — implementers should read the relevant section before starting.

**Branch:** `claude/mcp-wave1` (stacked on `claude/mcp-new-file`).

## Global Constraints

- No version bumps (workspace 0.4.0, extension 0.3.0). No new dependencies anywhere; docxcore/wasm crates stay std-only/single-dependency.
- Existing tests pass unmodified; existing verbs' replies unchanged EXCEPT `doc.path` gains optional `protection`/`watermark` keys (added on all surfaces in the same task wave, harness-checked).
- Wire parity is sacred: a VS Code tab's reply for any verb is byte-shaped like the terminal's. Error wording reuses existing conventions (`unknown verb '<v>'`, bounds messages, ctlcore ambiguity strings, `already exists:`/`bad path:`/`create failed:` family for export-pdf).
- Every MUTATING verb on tabs must map to a TRUE inverse (wasm undo entry with correct `undoSteps`, or a host-orchestrated inverse op) — one undo-integrity test per mutating verb at the wasm layer.
- MCP tool parity: names/descriptions/schemas/order identical between Rust servers and server.mjs; cross-checked against the real binaries.
- **Windows agent shell quirks:** every cargo/npm command runs in bash with
  ```bash
  export PATH="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
  Never pipe exit-code-bearing commands through `tail`. Packaging via `npx --yes @vscode/vsce@latest package --no-dependencies`.

---

### Task 1: docxy read-only verbs (terminal)

**Files:**
- Modify: `docxcore/src/agent.rs`, `docxy/src/control.rs`

**Verbs (spec table rows):** `doc.export`, `doc.comments`, `doc.notes`, `doc.header`, `doc.footer`, `doc.metadata`, `doc.stats`, plus `doc.path`'s additive `protection`/`watermark` keys.

**Interfaces:**
- Consumes: `docxcore::markdown::to_markdown`, `Document::plain_text`, `comments::parse_comments`, `notes::parse_notes`, `load::parse_header_footer` + `Package::part`, `field::parse_core_props`, `Package::{protection, watermark}`. Read each function's actual signature first (the research doc cites locations); adapt marshalling to what they really return.
- Produces (Task 3 mirrors these): `docxcore::agent` gains pure functions where logic is core-worthy — REQUIRED: `pub fn stats(doc: &Document) -> (usize, usize, usize, usize)` (words, chars, paragraphs, blocks). The rest may be marshalled directly in control.rs when they're one-call wrappers, matching how existing read verbs are structured.

- [ ] **Step 1: TDD.** For each verb add a control.rs dispatch test in the existing fixture style (`doc_with`/`app_with`), asserting the spec's exact reply keys. Representative (write the analogous test for every verb in this task):

```rust
#[test]
fn export_returns_live_markdown() {
    let mut app = app_with(&["# Title", "body text"]);
    let r = dispatch(&mut app, "doc.export", &Json::obj(vec![("format", Json::Str("markdown".into()))])).unwrap();
    let text = r.get_str("text").unwrap();
    assert!(text.contains("Title") && r.get_str("format") == Some("markdown"));
}
#[test]
fn stats_counts_words_chars_paragraphs() {
    let mut app = app_with(&["one two", "three"]);
    let r = dispatch(&mut app, "doc.stats", &Json::obj(vec![])).unwrap();
    assert_eq!(r.get("words").and_then(Json::as_i64), Some(3));
    assert_eq!(r.get("paragraphs").and_then(Json::as_i64), Some(2));
}
#[test]
fn export_rejects_unknown_format() {
    let mut app = app_with(&["x"]);
    let err = dispatch(&mut app, "doc.export", &Json::obj(vec![("format", Json::Str("rtf".into()))])).unwrap_err();
    assert!(err.contains("unknown format"), "{err}");
}
```

Comments/notes/header/footer/metadata tests need fixtures carrying those parts — reuse or extend whatever fixture builders the comments/notes modules' own tests use (read their `#[cfg(test)]` mods first); if a fixture can't express a part, assert the empty-list shape (`{"comments":[]}`) against the plain fixture AND add the populated-case test at the core-module level instead. `doc.export {format:"text"}` and both `doc.header`/`doc.footer` empty cases must each have a test. `doc.path` additive keys: test that an unprotected fixture has NO `protection` key.

- [ ] **Step 2: RED** — `cargo test -p docxy control` fails on unknown verbs.
- [ ] **Step 3: Implement.** New `dispatch` arms marshalling each core call to the spec's reply shape. `doc.export` dispatches on `format`: `"markdown"` → `to_markdown`, `"text"` → `plain_text`, other → `Err("unknown format '<f>' (markdown|text)")`. `agent::stats`: words = `split_whitespace().count()` over plain text; chars = non-newline char count; paragraphs = paragraph-kind blocks; blocks = body len.
- [ ] **Step 4: GREEN** — `cargo test -p docxcore -p docxy`.
- [ ] **Step 5: Gates + commit** — fmt/clippy as always; `git add docxcore docxy && git commit -m "docxy: wave-1 read verbs (export, comments, notes, header/footer, metadata, stats)"`

---

### Task 2: docxy mutating verbs (terminal)

**Files:**
- Modify: `docxcore/src/agent.rs`, `docxy/src/control.rs`

**Verbs:** `doc.replace-all`, `doc.undo`, `doc.redo`, `doc.export-pdf`.

**Interfaces:**
- Consumes: `Editor::{replace_all, undo, redo}`, `export::to_pdf`, and the `already exists:`/`bad path:`/`create failed:` error family (copy the exact mapping from `ctlcore::client::new_file` — exclusive-create open included).
- Produces (Task 3 depends on this): `agent::replace_all(ed, query, text, case_sensitive) -> usize` AND its documented undo-step count. **Determine empirically what `Editor::replace_all` puts on the undo stack** (one checkpoint? one per match?) and write the answer into the function's doc comment + return it: `-> (usize, usize)` (replaced, undo_steps), following the `replace_range` precedent. `agent::undo/redo(ed) -> bool`.

- [ ] **Step 1: TDD.** Tests: replace-all replaces every occurrence and reports count; case-insensitive by default; undo restores prior text in exactly the reported step count (assert by looping `dispatch("undo")`-equivalents at the Editor level); `doc.undo` returns `{done:true}` after an edit and `{done:false}` on a fresh doc; `doc.export-pdf` writes a nonempty file to a temp path, refuses an existing path with `already exists:`, and the reply carries the absolutized path.
- [ ] **Step 2: RED. Step 3: Implement** (export-pdf reuses the exclusive-create pattern — `OpenOptions::create_new` — with the same error strings). **Step 4: GREEN** (`cargo test -p docxcore -p docxy`). **Step 5: commit** — `"docxy: wave-1 mutating verbs (replace-all, undo/redo, export-pdf)"`

---

### Task 3: docxwasm ctl mirrors

**Files:**
- Modify: `docxwasm/src/bridge.rs` (+ `json.rs` only if a new writer helper is needed)

**Verbs:** all Task 1+2 verbs EXCEPT `doc.export-pdf` gets a wasm variant returning bytes: verb `doc.export-pdf` in wasm takes `{}` (no path) and returns `{"pdfBase64": <base64 bytes>}` — an INTERNAL shape; the extension host (Task 7) writes the file and produces the terminal-shaped `{path}` reply on the wire. Base64 encoder: hand-rolled ~15-line std-only helper (docxwasm has no deps).

**Interfaces:**
- Consumes: Tasks 1–2's `docxcore::agent` functions; the established `undoSteps` internal-field pattern (see `ctl_replace_range` in this file).
- Produces (Task 7 depends on): every verb reachable via `Session::ctl`; mutating replies carry `undoSteps` (replace-all = the count Task 2's function reports; undo/redo = 0 — they are not themselves undoable edits, see Task 7's adaptation).

- [ ] **Step 1: TDD** in bridge.rs's test style: one test per verb asserting the spec reply keys; undo-integrity tests: `ctl doc.replace-all` then N× `dispatch("undo")` (N = reported undoSteps) restores the pre-state; `ctl doc.undo` after an interactive-style edit returns `{"done":true}` and the doc content reverts.
- [ ] **Step 2: RED. Step 3: Implement. Step 4: GREEN** + wasm32 release build. **Step 5: commit** — `"docxwasm: wave-1 ctl verbs"`

---

### Task 4: xlsxy read-only verbs (terminal)

**Files:**
- Modify: `xlsxy/src/control.rs` (+ small pure helpers in `gridcore` ONLY where the research flagged the logic as TUI-locked: lift `csv_to_pkg`-style import shaping and selection-stats math if not already core)

**Verbs:** `comment.list`, `wb.export-csv`, `sheet.pivot`, `formula.eval`, `sheet.stats`, `chart.list`, `pivot.list`.

**Interfaces:**
- Consumes: `SheetPackage::comments`, `sheet::sheet_to_csv`, `frame::pivot` + `Frame` construction from a sheet range (read how the TUI's pivot editor builds a `Frame` from cells — reuse that path), `engine::eval_formula_at`, `drawing::parse_chart` (charts enumerate from the package's drawing parts — find how the TUI's overlay locates them), `workbook.pivots`, `Agg` enum mapping from the spec's 11 agg strings.
- Produces (Task 6 mirrors): where logic needs >10 lines of shaping (the range→Frame builder, the agg-string mapping, stats math), put it in gridcore as a pure function so gridwasm can reuse it; name them in your report.

- [ ] **Step 1: TDD** — dispatch tests per verb against the existing workbook fixture (extend it with a comment + a second column of numbers for pivot/stats). `sheet.pivot` test: 2-col range (name, amount), `rows:["name"], values:[{col:"amount", agg:"sum"}]` → table includes the header row and correct sums; unknown header column errors naming it. `formula.eval` test: `=SUM(...)` returns value+formatted text AND a subsequent `cell.get` proves no mutation. `sheet.stats` numeric range returns all six keys.
- [ ] **Step 2: RED. Step 3: Implement. Step 4: GREEN** (`cargo test -p gridcore -p xlsxy`). **Step 5: commit** — `"xlsxy: wave-1 read verbs (comments, csv export, ad-hoc pivot, eval, stats, charts)"`

---

### Task 5: xlsxy mutating verbs (terminal)

**Files:**
- Modify: `xlsxy/src/control.rs` (+ gridcore pure helpers per the same rule as Task 4)

**Verbs:** `comment.add`, `comment.remove`, `range.set`, `sheet.import-csv`, `wb.replace-all`, `sheet.add`, `sheet.remove`, `sheet.rename`, `row.insert`, `row.delete`, `col.insert`, `col.delete`.

**Interfaces:**
- Consumes: `add_threaded_comment`/`remove_comment`, the TUI's `apply_on` batch pattern (ONE undo group + `recalc_from`), `Frame::from_csv` + the import shaping, the TUI `replace_all` algorithm, `add_sheet`/`remove_sheet`/`rename_sheet`, `edit::{insert_rows, delete_rows, insert_cols, delete_cols}` + the engine-rebuild glue (`Engine::new` + `recalc_all` — copy how the TUI does it).
- Produces (Task 6 depends on): per-verb undo semantics documented in your report — for each mutating verb state which mechanism reverts it (undo group / structural snapshot / not-on-stack e.g. comments) — Task 7 consumes this table for edit-event mapping.

- [ ] **Step 1: TDD.** Key tests beyond per-verb happy paths: `range.set` ATOMICITY (batch with one invalid formula → error names the offending cell, NO cell changed, undo stack unchanged); `range.set` one undo group (single TUI-level undo restores all cells); `sheet.import-csv` never overwrites (importing twice yields two sheets, distinct names); `sheet.remove` last-sheet error; `row.insert` shifts formulas (assert a `=A2` reference moved); `wb.replace-all` single undo group.
- [ ] **Step 2: RED. Step 3: Implement. Step 4: GREEN. Step 5: commit** — `"xlsxy: wave-1 mutating verbs (comments, range.set, csv import, replace-all, sheet/row/col ops)"`

---

### Task 6: gridwasm ctl mirrors

**Files:**
- Modify: `gridwasm/src/bridge.rs`

**Verbs:** all Task 4+5 verbs.

**Interfaces:**
- Consumes: the gridcore pure helpers Tasks 4–5 produced; gridwasm's existing undo-group machinery (`UndoGroup` with sheet index; the SheetAdd true-inverse precedent from the offxy branch).
- Produces (Task 7 depends on): mutating replies carry `undoSteps`; for operations gridwasm's undo stack cannot represent (verify per Task 5's undo-semantics table — likely comments, possibly structural ops), the reply instead carries internal `"inverse":{verb, args}` describing the host-orchestrated inverse op; the CtlServer strips both fields (Task 7).

- [ ] **Step 1: TDD** per verb + undo-integrity per mutating verb: apply via ctl → assert → revert via the declared mechanism (dispatch undo × undoSteps, or apply the declared inverse op) → assert restored. `sheet.pivot` read-only test asserts view_json unchanged after the call.
- [ ] **Step 2: RED. Step 3: Implement. Step 4: GREEN** + wasm32 build. **Step 5: commit** — `"gridwasm: wave-1 ctl verbs"`

---

### Task 7: extension wiring + tab adaptations

**Files:**
- Modify: `offxy-vscode/src/extension.ts`, `offxy-vscode/src/ctlserver.ts` (strip the new internal fields; no protocol change)

**Interfaces:**
- Consumes: Tasks 3+6 wasm verbs and their internal fields (`undoSteps`, `inverse`, `pdfBase64`); the established `onMutated(verb, undoSteps)` path.
- Produces: tabs answer every new verb byte-shaped like terminals.

- [ ] **Step 1: EDITORS config** — extend `wasmVerbs`/`mutatingVerbs` per the spec's mutating list. Read-only verbs: no repaint, no edit event.
- [ ] **Step 2: undo/redo adaptation** (spec section "Tab adaptations"): `doc.undo` with `{done:true}` → wasm undo already ran; provider fires a NEW edit event labeled "agent undo" whose `undo()` performs wasm `redo` and `redo()` performs wasm `undo` (inverse pairing; assert direction carefully — write the truth table in a comment). `{done:false}` → no event, no repaint.
- [ ] **Step 3: host-orchestrated inverses** — for verbs whose wasm reply carries `inverse`, the edit event's `undo()` sends that inverse ctl request into the webview (and `redo()` replays the original); CtlServer strips `inverse` from the wire like `undoSteps`.
- [ ] **Step 4: export-pdf host assist** — CtlServer routes `doc.export-pdf` (docxy tabs) as a host-assisted verb: wasm call (no path) → decode `pdfBase64` → exclusive-create write at the absolutized `path` arg (reuse the `already exists:`/`bad path:`/`create failed:` mapping from server.mjs's `doNew` — same wording) → wire reply `{path}`. Read Task 3's report for the internal shape.
- [ ] **Step 5: harness** (scratchpad) — extend the ctl harness: every new verb reachable over TCP on both apps with spec-shaped replies; internal fields never on the wire (exact key-set assertions on one mutating + one read verb per app); undo-lockstep spot-check: `doc.replace-all` then VS Code-level undo-equivalent (fake host records edit events; assert the event's undoSteps matches). `ALL OK` exit 0.
- [ ] **Step 6: typecheck/build/package/install; commit** — `"offxy: tabs answer the wave-1 verb surface"`

---

### Task 8: MCP tools on both servers

**Files:**
- Modify: `docxy/src/mcp.rs`, `xlsxy/src/mcp.rs`, `offxy-vscode/mcp/server.mjs`

**Interfaces:**
- Consumes: the spec's verb tables (tool per verb: `docxy_export`, `docxy_export_pdf`, `docxy_comments`, `docxy_notes`, `docxy_header`, `docxy_footer`, `docxy_metadata`, `docxy_stats`, `docxy_replace_all`, `docxy_undo`, `docxy_redo`; `xlsxy_comments`, `xlsxy_comment_add`, `xlsxy_comment_remove`, `xlsxy_range_set`, `xlsxy_export_csv`, `xlsxy_import_csv`, `xlsxy_pivot`, `xlsxy_replace_all`, `xlsxy_sheet_add`, `xlsxy_sheet_remove`, `xlsxy_sheet_rename`, `xlsxy_row_insert`, `xlsxy_row_delete`, `xlsxy_col_insert`, `xlsxy_col_delete`, `xlsxy_eval`, `xlsxy_stats`, `xlsxy_charts`, `xlsxy_pivots`).
- Produces: ~51-tool surface, parity-exact.

- [ ] **Step 1:** Rust tool defs + verb-map entries (write descriptions once, in Rust — JS copies them character-for-character). New tools appended after the existing ones, same relative order everywhere. Array/object args (`range_set`'s `rows`, `pivot`'s `values`) use inline JSON-schema `{"type":"array","items":…}` — check how `prop` composes and extend minimally if it only does scalar types.
- [ ] **Step 2:** server.mjs mirrors (defs + verb-map only — all new tools forward to verbs; none need bespoke JS logic).
- [ ] **Step 3:** parity cross-check harness vs both binaries — all tools, 0 differences; paste summary in report. Live smoke: one read + one mutating tool per app end-to-end.
- [ ] **Step 4: commit** — `"offxy + docxy + xlsxy: wave-1 MCP tools (~51-tool surface)"`

---

### Task 9: docs + full verification

**Files:**
- Modify: `docs/agent-control.md`, `offxy-vscode/README.md`, `offxy-vscode/CHANGELOG.md`

- [ ] **Step 1:** agent-control.md verb tables (args/results per spec) + MCP tool lists + a "live-buffer semantics" note for `doc.export`/`wb.export-csv`; README tool list; CHANGELOG entry.
- [ ] **Step 2:** full gates: fmt/clippy/tests all 7 crates; both wasm32 builds; typecheck/build/package/install; re-run Task 7 harness + Task 8 parity check against final artifacts; exit codes reported.
- [ ] **Step 3:** manual e2e checklist for Boris in the report (tab: agent replace-all then Ctrl+Z; export live buffer with unsaved edit; xlsxy comment appears; range.set block undo).
- [ ] **Step 4: commit** — `"offxy: document the wave-1 agent surface"`

## Self-Review Notes

- Spec coverage: every spec verb-table row appears in exactly one terminal task (1/2/4/5), its wasm mirror (3/6), extension (7), MCP (8), docs (9). `doc.path` additive keys: Task 1 (terminal) + Task 3 (wasm) + harness check (7).
- The spec's tables carry the exact args/reply keys so tasks reference rather than restate them — the spec file is required reading for every implementer (stated in each dispatch).
- Deliberate deviations from full-verbatim-code style: one-call marshalling arms follow the established control.rs pattern visible in the file being edited; complete code is reserved for genuinely novel logic (undo/redo event pairing, inverse-op plumbing, pdf host assist, atomicity). Empirical facts implementers must determine and report: `Editor::replace_all` undo granularity (Task 2), per-verb xlsxy undo mechanisms (Task 5), chart-part enumeration (Task 4).
