# MCP Wave 1 — the "free win" verb surface: design

**Goal:** Expose ~20 already-implemented core capabilities as ctl verbs + MCP tools on
every surface (terminal docxy/xlsxy, VS Code tabs via wasm, both MCP servers), with
full adapted parity.

**Basis:** `docs/superpowers/research/2026-07-18-mcp-tool-opportunities.md` (Tier 1).
**Admission rule:** a verb enters Wave 1 only if its core logic already exists in
docxcore/gridcore/the TUI apps. No new engine work. Anything needing new modeling or
save-path work (merges, freeze, CF, named-range writes, chart/pivot authoring,
tracked changes, formatting) is out of scope.

## Architecture

Same layering as agent-access: verb cores live in `docxcore::agent` (docxy) and the
wasm `Session::ctl` dispatchers reuse them; terminal `control.rs` and wasm ctl speak
the identical wire contract; the extension's `EDITORS` config grows the new
wasm/mutating verb sets; both MCP servers (Rust + bundled server.mjs) grow
schema-identical tools, cross-checked against the binaries as in Wave 0.
Text-returning designs (`export`, `export-csv`, `pivot`, `stats`) make every surface
byte-identical for free.

## docxy verbs

| Verb | Args | Result | Core source |
|---|---|---|---|
| `doc.export` | `{format:"markdown"\|"text"}` | `{format, text}` — the LIVE buffer | `markdown::to_markdown`, `Document::plain_text` |
| `doc.export-pdf` | `{path}` | `{path}` (absolutized; refuses overwrite like `*_new`) | `export::to_pdf` |
| `doc.comments` | `{}` | `{comments:[{id,author,initials,date,text,anchor}]}` | `comments::parse_comments` |
| `doc.notes` | `{}` | `{notes:[{id,kind:"footnote"\|"endnote",text}]}` | `notes::parse_notes` |
| `doc.header` / `doc.footer` | `{}` | `{blocks:[{index,kind,text}]}` (empty list if none) | `load::parse_header_footer` |
| `doc.metadata` | `{}` | present-if-set keys: `{title?,author?,subject?,keywords?,…}` | `field::parse_core_props` |
| `doc.stats` | `{}` | `{words,chars,paragraphs,blocks}` | 3-line count over `plain_text` (lift into `agent.rs`) |
| `doc.replace-all` | `{query,text,case_sensitive?}` | `{replaced}` | `Editor::replace_all` |
| `doc.undo` / `doc.redo` | `{}` | `{done:bool}` (false = nothing to undo/redo) | `Editor::undo/redo` |
| `doc.path` (additive) | — | gains `protection?` and `watermark?` string keys, present only when set — added on ALL surfaces in the same task | `Package::protection/watermark` |

## xlsxy verbs

| Verb | Args | Result | Core source |
|---|---|---|---|
| `comment.list` | `{}` | `{comments:[{sheet,ref,author,text,date?}]}` (threads flattened in reply order) | `SheetPackage::comments` |
| `comment.add` | `{ref,text,author?,sheet?}` | `{sheet,ref}` | `add_threaded_comment` |
| `comment.remove` | `{ref,sheet?}` | `{removed:bool}` | `remove_comment` |
| `range.set` | `{start,rows:[[string]],sheet?}` — `start` is an A1-style cell ref (top-left); each string is entered like `cell.set` text (empty string clears the cell) | `{set:N}` — ATOMIC: all formulas validated first; any invalid → error, nothing applied; one undo group, one incremental recalc | the TUI's `apply_on` pattern |
| `wb.export-csv` | `{sheet?}` | `{sheet,csv}` (display-formatted, RFC-4180) | `sheet_to_csv` |
| `sheet.import-csv` | `{text,name?}` | `{sheet,name,rows,cols}` — always creates a NEW sheet, never overwrites | `Frame::from_csv` + `csv_to_pkg` logic |
| `sheet.pivot` | `{range,rows:[col],cols?:[col],values:[{col,agg}],sheet?}` — first row of `range` = header names; `agg` ∈ sum/count/countNums/average/max/min/product/stdDev/stdDevP/var/varP | `{table:[[string]]}` computed grid incl. headers; READ-ONLY, no workbook mutation | `frame::pivot` |
| `wb.replace-all` | `{query,text}` | `{replaced}` — one undo group | the TUI's `replace_all` algorithm |
| `sheet.add` | `{name?}` | `{sheet,name}` | `SheetPackage::add_sheet` |
| `sheet.remove` | `{sheet}` | `{removed:true}` (error on last sheet, ctlcore-style wording) | `SheetPackage::remove_sheet` |
| `sheet.rename` | `{sheet,name}` | `{name}` (formula refs rewritten) | `edit::rename_sheet` |
| `row.insert`/`row.delete`/`col.insert`/`col.delete` | `{at,count?,sheet?}` | `{inserted\|deleted:N}` — engine rebuilt + recalc after | `edit::insert_rows` etc. |
| `formula.eval` | `{formula,ref?,sheet?}` | `{value,text}` — SIDE-EFFECT-FREE preview | `eval_formula_at` |
| `sheet.stats` | `{range,sheet?}` | `{sum,count,countNums,average,min,max}` | selection-stats logic |
| `chart.list` | `{}` | `{charts:[{kind,title?,categories,series:[{name?,values}]}]}` | `drawing::parse_chart` |
| `pivot.list` | `{}` | `{pivots:[{sheet,rows,cols,values}]}` (summary) | walk `workbook.pivots` |

## MCP tools

One tool per verb, named by the established convention (`docxy_export`,
`docxy_export_pdf`, `docxy_comments`, … `xlsxy_comment_add`, `xlsxy_range_set`,
`xlsxy_pivot`, …), every one carrying `target`; identical across the Rust servers and
server.mjs (names, descriptions, schemas, order — grouped after the existing tools,
same relative order on all surfaces). Tool count grows ~21 → ~51.

## Tab (VS Code) adaptations

- **`doc.undo`/`doc.redo`:** the wasm undo/redo runs, and the provider fires a NEW
  edit event (labeled "agent undo"/"agent redo") whose own undo/redo performs the
  inverse wasm op. VS Code's undo stack stays in lockstep; no focus or private API
  needed. `{done:false}` fires no event.
- **`doc.export-pdf`:** the wasm renders PDF bytes (docxcore's exporter is std-only);
  the extension host writes the file. Terminal writes directly. Same reply either way.
- **Mutating-verb bookkeeping:** every mutating verb's edit event must map to a TRUE
  inverse — either a wasm undo-stack entry (with the correct step count, via the
  established `undoSteps` internal field) or a host-orchestrated inverse operation
  (comment add ⇄ remove) when the core operation is not on the undo stack. Which
  mechanism each verb uses is verified per-verb during implementation and tested
  (undo-integrity test per mutating verb). Mutating sets: docxy `replace-all`,
  `undo`, `redo`; xlsxy `comment.add/remove`, `range.set`, `sheet.import-csv`,
  `wb.replace-all`, `sheet.add/remove/rename`, `row.*`, `col.*`.
- Repaint: all mutating verbs repaint; read-only verbs don't.

## Error handling

Existing conventions reused verbatim: bounds/`unknown verb` wording, ctlcore
resolution/ambiguity errors, `already exists:`/`bad path:`/`create failed:` for
`doc.export-pdf` (same family as `*_new`). New error cases: `range.set` atomic
validation error names the first offending cell; `sheet.remove` on the last sheet;
`sheet.import-csv` with unparseable text; `sheet.pivot` naming an unknown header
column. All error strings byte-identical between Rust and server.mjs where the JS
surface produces them.

## Testing

- Core: unit tests per new `agent.rs`/gridcore-adjacent function.
- Terminal: control.rs dispatch tests per verb (fixture style already in place).
- Wasm: `Session::ctl` tests per verb, including the undo-integrity tests for every
  mutating verb (apply → assert stack/inverse → undo → assert restored).
- Extension harness: parity spot-checks (new verbs reachable on a tab, byte-shaped
  like terminal replies; unknown-verb behavior preserved for non-verbs).
- MCP: schema cross-check of all ~51 tools against both real binaries (the Wave-0
  harness pattern); end-to-end `tools/call` smoke for one new read verb and one new
  mutating verb per app.
- Full gates: fmt/clippy/tests all crates, wasm builds, typecheck/build/package.

## Out of scope (explicit)

Markdown-formatted writes, the docxy range-selection/formatting tier, cell formatting,
persistent pivot creation, chart authoring, tracked changes, sort/filter, anything on
the xlsx byte-preservation gap list (merge/freeze/CF/named-range/hyperlink writes),
regex find, version bumps.
