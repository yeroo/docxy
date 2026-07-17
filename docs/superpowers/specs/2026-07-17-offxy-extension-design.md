# Offxy VS Code Extension — Design

**Date:** 2026-07-17
**Status:** Approved (design review with Boris, this session)

## Summary

Merge the existing `docxy-vscode` extension and a new Excel `.xlsx` editor into
one VS Code extension named **offxy** (`yeroo.offxy`). The Word editor ships
unchanged; the spreadsheet editor is new, powered by a WebAssembly build of the
dependency-free `gridcore` engine, with **full editing** in v1: cell values and
formulas with live recalculation, structural edits (insert/delete rows and
columns), sheet management, and lossless save. The architecture treats editors
as pluggable units so a future `.mpp` (yppxy/projcore) editor is one more
registration, not a restructuring.

## Decisions made during review

- **Merge, don't multiply:** rename `docxy-vscode` → `offxy`; the old name
  retires. It was never on the marketplace (release-asset distribution only),
  so migration is "uninstall old, install new" in the README.
- **Real HTML grid** for the spreadsheet view (sticky headers, formula bar,
  selection rectangle) — not a monospace text grid.
- **Full editing in v1**, including structural edits (gridcore's `edit.rs`
  already does workbook-wide reference rewriting).
- **Design for a third editor** (.mpp) but build none of it now.
- **Two wasm modules** (approach A), not one combined binary: `docxwasm` and
  the new `gridwasm` stay independent so release trains stay decoupled and
  each editor loads only its own engine.

## Structure & naming

```
gridwasm/                    new std-only crate (workspace member)
  src/lib.rs                 wasm ABI: grid_alloc/grid_free/grid_open/grid_close/
                             grid_cmd/grid_save/grid_new — length-prefixed
                             [u32 len][payload] result buffers, same idiom as docxwasm
  src/bridge.rs              Session (native-testable, no wasm imports)
  src/json.rs                minimal JSON string escaping (copied pattern; a
                             shared crate is not worth the coupling)

offxy-vscode/                renamed from docxy-vscode/
  package.json               name "offxy", displayName "Offxy — Word & Excel
                             .docx/.xlsx editor", publisher yeroo,
                             two customEditors:
                               offxy.docxEditor  → *.docx  (existing webview)
                               offxy.gridEditor  → *.xlsx  (new webview)
  src/extension.ts           provider generalization (see Host integration)
  src/engine.ts              unchanged (markdown ⇄ docx host-side wasm)
  media/webview.js|css       existing docx webview, unchanged
  media/docxwasm.wasm        unchanged
  media/grid.js|css          new spreadsheet webview
  media/gridwasm.wasm        new engine build
  scripts/copy-wasm.mjs      copies BOTH wasm artifacts
```

`gridwasm` uses the workspace version (joins the docxy/docxcore/docxwasm
release train). The crate depends only on `gridcore` (which is std-only), so
`cargo build -p gridwasm --target wasm32-unknown-unknown --release` works like
docxwasm's build.

Custom-editor view types change ids (`docxy.docxEditor` → `offxy.docxEditor`).
That is invisible to users (VS Code resolves editors by file selector), but the
command ids (`docxy.*`) also rename to `offxy.*`; keybindings users may have
bound are listed in the changelog.

## gridwasm Session & command protocol

A `Session` holds the loaded `SheetPackage` (whole package retained — save is
lossless: gridcore regenerates only modeled cell data and preserves every other
part byte-for-byte), the recalc engine state, the active sheet index, an undo
stack, and a dirty flag.

**Viewport protocol.** The webview never receives whole sheets. It requests
`view\t<sheet>\t<top>\t<left>\t<nrows>\t<ncols>` and receives JSON:

```json
{
  "sheets": ["Sheet1", "Data"], "active": 0,
  "dims": { "rows": 10500, "cols": 42 },          // used extent, for the scroll spacer
  "colw": [64, 120, ...],                          // px-ish widths for visible cols
  "cells": [ { "r": 3, "c": 1, "t": "1,234.50", "a": "r",
               "b": 1, "i": 1, "col": "Red" }, ... ],
  "sel": { "r": 3, "c": 1, "r2": 3, "c2": 1 },
  "cur": { "ref": "B4", "src": "=SUM(B1:B3)" },    // formula-bar content
  "dirty": true
}
```

Display text comes from `numfmt` (real number-format rendering); alignment is
`"r"` for numbers unless the format says otherwise. Only non-empty cells in the
window are listed.

**Commands** (tab-delimited, mirroring docxwasm's `dispatch`):

| Command | Effect |
|---|---|
| `set\t<r>\t<c>\t<text>` | Leading `=` → formula, else value (numbers/bools/text inferred like the TUI). Recalc runs; formulas the engine can't evaluate keep Excel's cached values. |
| `clear\t<r1>\t<c1>\t<r2>\t<c2>` | Clear range contents. |
| `insrow\t<at>\t<n>` / `delrow\t<at>\t<n>` | Structural edit via `edit.rs`, workbook-wide reference rewriting. Same for `inscol`/`delcol`. |
| `sheet\tswitch\t<i>` / `sheet\tadd\t<name>` / `sheet\trename\t<i>\t<name>` | Sheet management. |
| `select\t<r1>\t<c1>\t<r2>\t<c2>` | Set selection + active cell (drives `cur` in the view). |
| `copy\t<r1>\t<c1>\t<r2>\t<c2>` | Returns the range as TSV in the response (`copied` field) for the host clipboard. |
| `paste\t<r>\t<c>\t<tsv>` | Parse TSV, apply as one undo group, recalc. |
| `undo` / `redo` | See below. |

Every command response is the refreshed viewport JSON for the last-requested
window (the webview's window parameters are remembered in the session), so one
round-trip both applies and repaints.

**Undo.** Ported from the xlsxy TUI's model (`xlsxy/src/main.rs`): cell edits
push an `UndoGroup` of per-address before/after cell states; structural edits
(insert/delete rows/cols, where the inverse isn't per-cell) snapshot the whole
grid state before/after. One VS Code edit event fires per mutating command, so
Ctrl-Z/Ctrl-Y route through VS Code's stack into `undo`/`redo` exactly like the
docx editor.

**`grid_new()`** returns bytes of a fresh workbook via `gridcore::xlsx::new_xlsx()`
— used by the host's empty-file create flow.

## Grid webview (`media/grid.js`)

- **Layout:** formula bar (top, fixed): active-cell ref box + editable input.
  Grid scroll container (middle): a spacer div sized to the full used extent
  (so scrollbars are honest) with only the visible window rendered —
  absolutely-positioned cell layer plus sticky A/B/C header row and sticky
  row-number column. Sheet tab strip (bottom, fixed): click switches,
  `+` adds, double-click renames.
- **Rendering:** viewport re-requested on scroll (debounced ~50 ms) and after
  every command. Cells draw with VS Code theme variables (borders
  `--vscode-editorWidget-border`, selection `--vscode-editor-selectionBackground`,
  etc.); numbers right-aligned; bold/italic/color from the view JSON.
- **Editing:** typing or F2 or double-click opens an input overlay on the
  active cell (pre-filled with the raw formula/value for F2/double-click,
  empty-start for type-through). Enter commits + moves down, Tab commits +
  moves right, Esc cancels. The formula bar edits the same value; both are two
  faces of one editing state.
- **Selection & keys:** click sets the active cell; drag or Shift+arrows
  extend a rectangular range; Ctrl+C/X/V flow through the host clipboard
  (TSV), Delete clears the selection, Ctrl+Z/Y/S are left to VS Code (same
  convention as the docx webview).
- **CSP:** identical policy to the docx webview (nonce'd scripts +
  `wasm-unsafe-eval`, no remote origins).

## Host integration (`src/extension.ts`)

The current provider splits into a small shared core plus per-format
registrations:

- **Shared:** the document class (rename `DocxDocument` → `BinaryDocument`) —
  it is already format-agnostic (uri + bytes + request/fulfill round-trip),
  and the save/saveAs/revert/backup implementations move with it. One
  provider class parameterized by a registration entry.
- **Registration table** (the pluggable-editor seam for a future .mpp editor):
  viewType, file selector, webview script/style/wasm names, empty-file
  strategy, display name for messages.
- **Empty-file flow** ported to xlsx: 0-byte or missing-content `.xlsx` →
  modal "Create new workbook?" + in-tab Create button; minting calls the
  host-side `gridwasm` instance's `grid_new` (loaded lazily like `engine.ts`
  does for markdown conversion).
- **Webview messages:** the docx message protocol is unchanged; the grid
  webview adds `viewport` (scroll-driven view requests) but reuses the
  `ready`/`open`/`edit`/`bytes`/`clipboard`/`readClipboard`/`createNew` shapes.
- Markdown ⇄ docx commands (`offxy.convertMarkdown`, `offxy.exportMarkdown`)
  and all formatting commands keep working against the docx editor only.

## Testing

- **gridwasm native tests** (`cargo test -p gridwasm`), mirroring docxwasm's
  suite: open-and-render a real workbook built through `new_xlsx()`;
  `set` + recalc updates dependents; lossless save round-trip (reopen, cells
  and unmodeled parts intact); insert/delete rows rewrites references;
  undo/redo restores cell and structural state; viewport windows clip
  correctly at sheet edges; TSV copy/paste round-trip; `grid_new` output opens.
- **Node smoke script** against the built `.wasm` (the pattern proven in this
  session: instantiate, open `assets/sample.xlsx`, drive `view`/`set`/`save`).
- **Existing docxwasm tests** must stay green (no engine changes intended).
- **Manual e2e:** install the packaged vsix; open `assets/sample.xlsx`
  (formulas, formats), edit, save, reopen in the TUI to confirm fidelity;
  empty-file create flow for both formats.

## Packaging & release

- `npm run build:wasm` builds **both** crates for wasm32 and copies both
  artifacts; everything else in the build/package pipeline is unchanged (the
  CI vsix job runs `vsce package` which triggers the same scripts).
- Release train: ships as **0.4.0** (workspace + extension) when Boris says
  go; no version bump lands before that.
- Changelog + README: rename notice (uninstall `docxy-vscode`, install
  `offxy`), command-id rename table, new spreadsheet feature list.
- The repo directory renames via `git mv docxy-vscode offxy-vscode` to keep
  history.

## Out of scope (v1)

- .mpp/yppxy editor (designed-for, not built).
- Cell formatting edits (bold/color/number-format changes) from the grid UI —
  display honors existing formats; changing them is a follow-up.
- Charts, pivot refresh UI, conditional-formatting editing, comments UI,
  frozen panes. Merged cells don't span in v1 — the anchor cell's value
  renders in its own cell and the covered cells render empty.
- Marketplace publishing (distribution stays via GitHub release assets).
