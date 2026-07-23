# MCP Wave 2 — markdown writes + cell formatting: design

**Goal:** Agents author *formatted* content: markdown-formatted writes in docxy
(headings, bold, lists, tables, links through the live editor), and cell
formatting + column widths in xlsxy.

**Basis:** Wave-2 tier of `docs/superpowers/research/2026-07-18-mcp-tool-opportunities.md`.
**Builds on:** Wave 1 (`docs/superpowers/specs/2026-07-18-mcp-wave1-verbs-design.md`,
PR #24) — all of its parity, undo-bucket, and internal-field conventions carry
forward unchanged.

**Admission rule (same as Wave 1):** only capabilities the core already
implements. `from_markdown` and the `Xf` style round-trip exist; borders, row
heights, wrap, and font family/size do not round-trip and are OUT.

## Part A — docxy markdown-formatted writes

### Surface

`doc.insert`, `doc.replace-range`, `doc.append` (and MCP tools `docxy_insert`,
`docxy_replace_range`, `docxy_append`) gain one optional arg:

- `markdown` (boolean, default `false`) — when false, behavior is byte-identical
  to today (plain text via `Clip::from_text`; `\n` starts a new paragraph).
  When true, `text` is parsed by `docxcore::markdown::from_markdown` and the
  resulting **blocks** are spliced into the body at the same position the
  plain-text variant would target.

Replies are unchanged (`{}` / `{replaced}` shapes as today). Errors: existing
bounds/args errors; markdown that parses to ZERO blocks (empty/whitespace
input) → error `empty markdown` (nothing spliced, no undo entry, no dirty).

### Semantics

- Supported constructs = whatever `from_markdown` produces into `Document.body`:
  headings (level → paragraph style), bold/italic/strike/inline-code, links,
  bullet/ordered lists (nested), tables, blockquotes, horizontal rules, fenced
  code, `$…$`/`$$…$$` math, ` ```mermaid ` fences. The implementation must
  verify each lands correctly when spliced into an EXISTING document (not just
  a fresh markdown package) and produce a supported-constructs table for the
  docs; any construct that requires package parts the splice cannot provide is
  listed there as unsupported-with-error or degraded-with-note — silent
  degradation is not acceptable.
- Lists: if the target package lacks the numbering definitions `from_markdown`
  references, the splice ensures them via the existing `Package::ensure_list`
  machinery (both bullet and decimal, as needed by the parsed content).
- **Undo-step parity with the plain-text variants** (Wave 1's tab mapping must
  keep working unchanged): markdown `insert`/`append` = 1 checkpoint;
  markdown `replace-range` = same step count as plain-text replace-range on
  the same range (2 when the deleted range is non-empty, 1 when empty), and
  the wasm reply's internal `undoSteps` reports it identically. The
  `docxcore::agent` layer gains block-splice functions mirroring the existing
  text ones — exact signatures decided by the plan against the real Editor
  API, with the checkpoint accounting REQUIRED to match.

### Cross-surface

Terminal control.rs, docxwasm ctl, and both MCP servers all honor the flag in
the same commit wave; the three MCP tool schemas gain the optional `markdown`
property (additive — required arrays unchanged); the extension needs no
config change (the verbs' mutating classification is unchanged). Tab replies
remain byte-shaped identical to terminal.

## Part B — xlsxy cell formatting

### New verbs

| Verb | Args | Result | Notes |
|---|---|---|---|
| `cell.format` | `{range, patch, sheet?}` — `range` A1-style; `patch` object, at least one key required | `{formatted:N}` (cell count) | ONE undo group; applies over every cell in the range |
| `col.width` | `{col, width, sheet?}` — `col` letter or 0-based index (match existing arg conventions); `width` number (Excel column-width units) | `{col, width}` | undoable consistent with how the TUI's width change behaves (implementation verifies which undo bucket and reports it; the tab mapping uses the same bucket) |

`patch` keys (all optional, ≥1 required; unknown keys → error naming the key):

- `numFmt` — string format code (as `numfmt::parse_format` accepts)
- `bold`, `italic` — boolean
- `fontColor`, `fillColor` — `"#RRGGBB"` string (match the TUI pickers' accepted forms)
- `align` — `"left" | "center" | "right"`

Setting a key applies it to all cells in the range via the existing
`Styles::intern`/`apply_format` path; keys absent from the patch leave those
aspects of each cell's existing style untouched.

### Read-back (read-modify-write support)

`cell.get`'s reply gains an additive, present-if-set `format` object echoing
the same keys for the cell's current style (only keys whose value differs from
the default style are present; an unstyled cell has NO `format` key). This is
an additive reply change on ALL surfaces in the same wave (like Wave 1's
`protection`/`watermark`).

### MCP tools

`xlsxy_format` (→ `cell.format`) and `xlsxy_col_width` (→ `col.width`), both
with `target`, appended after the existing tools, same relative order
everywhere → 53 tools total. `patch` schema: object with the six optional
properties, correctly typed; required `["range","patch"]` / `["col","width"]`.

### Undo/tab mapping

Both new verbs are mutating, repaint, and expected bucket A (one wasm
undo-stack group, internal `undoSteps:1`); if the TUI's column-width mutation
turns out not to be on the undo stack, the implementation follows the Wave-1
three-bucket playbook (inverse op carrying the prior width) and documents the
choice.

## Error handling

Existing conventions verbatim. New cases: `empty markdown`; `cell.format`
with an empty/unknown-key patch (error names the offending key or says
`patch needs at least one key`); `col.width` non-positive width error.
Error strings byte-identical across surfaces where JS produces them.

## Testing

- Wave-1 regime per layer (core tests, control dispatch tests, wasm mirrors
  with undo-integrity per mutating path, extension harness spot-checks,
  MCP parity + verb-map pinning extended to the new/changed schemas).
- Markdown round-trip net: insert markdown (headings/list/table/link) into an
  existing doc → `doc.export {format:"markdown"}` recovers the structure;
  plain-text flag-off calls byte-match Wave-1 behavior.
- Undo parity net: markdown replace-range on empty vs non-empty ranges reports
  the same undoSteps as the plain-text variant (both layers).
- Format round-trip net: `cell.format` → `cell.get.format` echoes the patch;
  save → reload → format persists (styles genuinely round-trip through
  `save_xlsx`, unlike the byte-preservation gaps); undo restores the prior
  format and `cell.get.format` reflects it.

## Out of scope (explicit)

Borders, row heights, wrap text, font family/size (not modeled); the docxy
range-selection/`doc.set-style` tier (Wave 3); persistent pivot creation;
merge/freeze/CF/named-range writes (save-path gate); regex; version bumps.
