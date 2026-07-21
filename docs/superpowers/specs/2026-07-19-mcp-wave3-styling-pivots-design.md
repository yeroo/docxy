# MCP Wave 3 — docxy formatting verbs + persistent pivots: design

**Goal:** Agents format existing document content (`doc.format`, `doc.set-style`)
and create real, persistent pivot tables in workbooks (`pivot.create`).

**Basis:** Wave-3 tier of `docs/superpowers/research/2026-07-18-mcp-tool-opportunities.md`.
**Builds on:** Waves 1–2 (PRs #24/#25) — all parity, undo-bucket, internal-field,
`ensure_styles`, and error-family conventions carry forward unchanged.

**Admission rule:** existing core capabilities only, with ONE disclosed probe:
pivot save-persistence (§Part B) must be proven or the verb ships as an honest
error — no lying success.

## Part A — docxy formatting

### Internal selection primitive

An `agent`-layer helper builds a block-range selection the way `replace_range`
already does (anchor at `start`, caret at `end` + move_end), runs an operation,
and leaves the editor in a clamped, consistent state. It is NOT a wire verb.

### `doc.format {start, end?, patch}` → `{formatted:N}`

- `start`/`end` block indices (end default start), bounds-checked with the
  existing wording; endpoints must be paragraphs (reuse `require_para` rules
  where the operation demands it — tables per Wave-1 conventions).
- `patch` object, ≥1 key; keys and value forms:
  - `bold`, `italic`, `underline`, `strike` — boolean, **set-to-value semantics**
    (NOT toggle): the agent layer applies the value to every run in the
    selection directly; formatting an already-bold run with `bold:true` is a
    no-op on that run.
  - `color` — `"#RRGGBB"` (same parsing family as xlsxy's `fontColor`).
  - `highlight` — the highlight-name set the Editor's `set_highlight` accepts
    (enumerate the accepted names in docs from the actual core enum).
  - `font` — font-name string; `size` — points (number, fractional allowed —
    follow what `set_font_size` accepts).
- `{formatted:N}` = number of blocks in the range. ONE undo checkpoint per
  call. Errors mirror `cell.format`'s family verbatim where applicable:
  `patch needs at least one key`, unknown-key naming, `bad color '<v>'`.
- Empty-range no-op questions do not arise (a valid block range always has ≥1
  block); a patch that changes nothing still checkpoints (documented — undo
  parity with xlsxy's `cell.format`, which snapshots regardless).

### `doc.set-style {start, end?, style?, align?}` → `{styled:N}`

- ≥1 of `style`/`align` required (`set-style needs 'style' or 'align'`).
- `style`: a paragraph style id — the Wave-2 markdown set (`Heading1`–`Heading6`,
  `Quote`, `SourceCode`) plus `Normal` (clears to default). Applying a
  markdown-set style runs Wave 2's `ensure_styles` (strictly additive) so the
  result renders in Word. Unknown style id → error naming it and listing the
  accepted set.
- `align`: `left | center | right | justify` — whichever of these the core
  `set_align` supports; if `justify` is unsupported in the core enum, it is
  omitted from the accepted set and docs (no new engine work).
- ONE undo checkpoint per call.

### Cross-surface

Terminal control.rs + docxwasm ctl in lockstep; both verbs mutating,
repaint, default tab undo mapping (steps=1 — no undoSteps field needed). MCP
tools `docxy_format`, `docxy_set_style` with patch/enum schemas mirroring the
`xlsxy_format` idiom.

## Part B — xlsxy persistent pivots

### `pivot.create {range, rows, cols?, values, name?}` → `{sheet, name}`

- Arg shape identical to the ad-hoc `sheet.pivot` (first row of `range` =
  header names; same 11 agg strings; same unknown-header error), plus optional
  `name` (default: a generated `PivotN` unique among sheet names).
- Builds a REAL workbook pivot via the TUI's own creation machinery
  (`create_pivot_from`-adjacent core: `SheetPackage::add_pivot`,
  `rewrite_pivot_definition`) and lands its output on a NEW sheet, mirroring
  the TUI's placement. The pivot then participates in `pivot.list` and is
  refreshed by the existing `wb.recalc` path.
- Undo bucket: determined empirically against the TUI's pivot-creation flow
  and mapped per the Wave-1 playbook (expected bucket-C-like: history-clear +
  a host inverse that removes the created sheet AND the pivot registration —
  the inverse must remove both or neither; a sheet-only inverse that leaves a
  dangling pivot entry is a defect).
- **Persistence probe (the wave's one engine question):** a created pivot must
  survive `save_xlsx` → `load_xlsx` (definition present, refresh works after
  reload). If the write path proves incomplete, `pivot.create` ships returning
  an honest error (`pivot persistence not supported for this workbook` — exact
  wording fixed during implementation) and the docs say so; silent
  session-only pivots are NOT acceptable.

### Cross-surface

Terminal + gridwasm in lockstep; MCP tool `xlsxy_pivot_create`. Tool count
53 → 56.

## Error handling

Existing conventions verbatim; new strings enumerated above. Byte-identical
across surfaces where JS produces them (none expected — all forwards).

## Testing

- Established per-layer regime (core, control dispatch, wasm mirrors with
  undo-integrity, extension harness spot-checks, MCP parity + verb-map).
- Set-to-value determinism: bold:true on already-bold stays bold; bold:false
  clears; mixed-selection normalization asserted.
- Round-trip nets: `doc.format` bold + `doc.set-style` Heading1 recovered via
  `doc.export {format:"markdown"}`; style application into a style-less
  package ensures definitions (Wave-2 tests as the model).
- Pivot: create → `pivot.list` shows it → source-cell edit + `wb.recalc`
  refreshes the output sheet → save/load round-trip → refresh still works.
  Undo-integrity per the empirically-determined bucket on both surfaces.

## Out of scope (explicit)

Run-level style ids (`Code`), table-cell-scoped formatting, per-character
(sub-block) ranges — block-range granularity only this wave; pivot layout
options (filters/pages, calculated fields, "values on rows") beyond the
sheet.pivot arg shape; chart authoring; version bumps.
