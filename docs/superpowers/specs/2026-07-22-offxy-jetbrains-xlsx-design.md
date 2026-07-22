# Offxy JetBrains Plugin — xlsx grid editor — Design

**Date:** 2026-07-22
**Status:** Approved (design review with Boris, this session)

## Summary

The second editor registration in **offxy-jetbrains** (the seam the docx
design reserved): a **native spreadsheet editor** for `.xlsx`, powered by the
same `gridwasm.wasm` artifact the VS Code extension ships, executed on the JVM
by Chicory. Unlike the docx editor there is **no editable-Document trick** —
a spreadsheet is cell-transactional (edit → commit → recalc), and gridwasm's
protocol reflects that: a **windowed viewport** (`view` → cells/widths/
selection JSON) built for exactly the virtualized grid component we render.
Full editing in v1: values and formulas with live recalculation, formatting
(bold/italic/align/number formats via the engine's `fmt` commands), structural
edits, sheet management, TSV clipboard, lossless save. Every open workbook
advertises on xlsxy's agent control surface via `grid_ctl`.

## Decisions made during review

- **Native virtualized table, not an editable text Document.** The grid UI is
  a windowed view over the engine — the same shape as the VS Code webview
  grid — because the editing model is transactional per cell. All the docx
  editor's replay/reconcile machinery is deliberately absent.
- **Undo is engine-stack-driven** — simpler than docx: there is no native
  free-typing to interleave with, every mutation is an engine command, so one
  mutating command registers one `UndoableAction` whose undo/redo dispatch
  the engine's own `undo`/`redo`. No snapshots. (Structural verbs that clear
  the engine's history — `sheet.remove`, CSV import via ctl — mirror the
  VS Code tab semantics and are documented divergences.)
- **Scope inherits VS Code grid v1 + the overhaul's additions**: full editing
  including insert/delete rows/columns (workbook-wide reference rewriting),
  sheet switch/add/rename, formatting toggles (`fmt`: bold/italic/align/
  font+fill color/numfmt), `decimals`, `autosum`, TSV copy/cut/paste. Merged
  cells render at their anchor without spanning; charts/pivots render as
  data; no conditional-formatting or frozen-pane UI.
- **Shared binding layer:** the Chicory marshalling (alloc → call →
  length-prefixed result → free) is factored out of `ChicoryEngine` into a
  common base; `GridEngine` is a thin second client over
  `grid_open/close/cmd/save/new/ctl`. One instance per open workbook,
  EDT-confined, same as docx.
- **File type registered up front** (`Offxy Excel Workbook`, extension
  `xlsx`) — the docx lesson: without it the IDE's Native type launches Excel
  before any editor provider runs.
- **Agent identity:** `xlsxy-jetbrains-<basename>-<pid>-<n>` in xlsxy's ctl
  dir; host verbs (`wb.path/save/reload/open`) in Kotlin, everything else
  through `grid_ctl` (already shipped with the full wave-verb surface).
  `wb.undo`/`wb.redo`-style driving is allowed here (unlike docx tabs)
  because the engine stack IS the tab's undo source; internal composition
  verbs (`wb.info`) are rejected externally for parity.

## Structure (additions to offxy-jetbrains)

```
src/main/kotlin/dev/yeroo/offxy/
  engine/WasmBinding.kt        extracted shared marshalling (Chicory instance,
                               alloc/free, length-prefixed results)
  engine/DocxEngine.kt         unchanged interface; ChicoryEngine now extends
                               the binding base
  engine/GridEngine.kt         open/view/cmd/save/media-less; newWorkbook();
                               ctl(requestJson)
  grid/XlsxFileType.kt         claims *.xlsx (binary)
  grid/XlsxEditorProvider.kt   second FileEditorProvider registration
  grid/XlsxFileEditor.kt       FileEditor shell: engine, dirty, save, reload,
                               empty-file (grid_new), ctl server lifecycle
  grid/GridPanel.kt            the virtualized table: viewport cache, lazy
                               model, headers, renderers, cell editor
  grid/FormulaBar.kt           ref box + editable formula field (cur.ref/src)
  grid/SheetTabs.kt            bottom strip: switch/add/rename (+ context ops)
  grid/GridToolbar.kt          B/I/align/decimals/autosum/number-format
  grid/GridCtlBridge.kt        xlsxy ctl dir advertising + verb routing
```

## The grid surface

- **Viewport protocol.** The panel asks the engine for the visible window
  (`view\t<sheet>\t<top>\t<left>\t<nrows>\t<ncols>`, debounced on scroll and
  resize) and caches the returned cells. The table model spans the used
  extent plus editing margin (honest scrollbars); `getValueAt` reads the
  cache; a miss triggers a window refresh. Column widths from `colw` (engine
  character units → px via font metrics); real row/column headers (A, B, C…
  sticky by construction in a JBTable + row-header table).
- **Rendering.** Display text comes pre-formatted from the engine (`numfmt`
  applied); alignment/bold/italic/colors per cell from the view JSON; theme
  colors from the scheme (selection, grid lines, header background).
- **Editing.** Type-through (replaces), F2/double-click (edit in place,
  pre-filled with the raw formula/value); Enter commits + moves down, Tab
  commits + moves right, Esc cancels. Commit = `set\t<r>\t<c>\t<text>`
  (leading `=` → formula, validated + recalculated; the whole dependent
  window repaints from the command's returned view). The formula bar and the
  in-cell editor are two faces of one editing state; selection sync via
  `select` keeps `cur.ref`/`cur.src` correct.
- **Selection & clipboard.** Click/drag/Shift+arrows extend the rectangular
  selection (mirrored to the engine via `select`); Ctrl+C/X (`copy`/`cut` →
  TSV to the OS clipboard), Ctrl+V (`paste` TSV, one undo group), Delete →
  `clear`.
- **Formatting & tools.** Toolbar buttons dispatch `fmt\t<key>` toggles
  (bold/italic/align/colors/numfmt presets), `decimals\t±1`, `autosum` —
  labels-in-toolbar like the docx bar, icons deferred with it.
- **Sheets.** Bottom tab strip: click switches (`sheet\tswitch\t<i>`), `+`
  adds, double-click renames; context menu: insert/delete rows/columns at
  the selection, remove sheet.
- **Undo/dirty/save.** One mutating command = one `UndoableAction` driving
  engine `undo`/`redo` (the returned view repaints). Dirty from the view
  JSON → tab asterisk; save = `grid_save` bytes in a `WriteAction` (lossless
  — gridcore preserves unmodeled parts); Save All / close-save / external
  reload / 0-byte create (`grid_new`) all mirror the docx editor's flows.

## Agent ctl bridge

`GridCtlBridge` reuses `CtlServer`/`Discovery` verbatim, pointed at xlsxy's
ctl dir. Host verbs answered in Kotlin (`wb.path` composed with `wb.info`
internally; `wb.save`/`wb.reload`/`wb.open` with the same semantics as the
VS Code tabs, including open-as-new-tab); every other verb passes through
`grid_ctl` (sheet.read, cell/range ops, comments, pivots, csv, stats —
already implemented engine-side). Mutating verbs register the same
engine-stack `UndoableAction` as UI edits, so agent edits are one Ctrl+Z
away. Divergences documented in agent-control.md's JetBrains section
(sheet-remove restore order and single-slot semantics mirror the VS Code tab
notes).

## Testing

- **GridEngine tests** (real wasm on Chicory): open `assets/sample.xlsx` and
  corpus workbooks; window clipping at sheet edges; `set` + recalc updates
  dependents in the returned view; TSV copy/paste round-trip; structural
  insert/delete rewrites references; undo/redo restores cell and structural
  state; `grid_new` bytes reopen; benchmark: `view` + `set`+recalc latency
  on the largest corpus workbook (expectation: O(window), no gate drama —
  report p50/p95 anyway).
- **Platform tests:** xlsx file type claims the extension (the Word-launch
  regression class); provider renders values into the model; commit → view
  updates + modified fires; platform undo reverses a `set`; formula bar
  reflects `cur`; sheet switch repaints; TSV via clipboard.
- **Ctl bridge e2e:** real TCP against a live workbook editor —
  `wb.path`/`sheet.read`/`cell.set` round-trip, undoable agent edit,
  discovery lifecycle in the build-local ctl dir.
- **Manual (TESTPLAN.md § additions):** corpus workbooks with formats/
  formulas; edit + TUI round-trip fidelity via terminal xlsxy; `xlsxy --mcp`
  listing the IDE workbook; theme switch; large-sheet scrolling feel.

## Out of scope (v1)

- Charts, pivot-table UI (pivot verbs remain ctl-only), conditional
  formatting UI, frozen panes, cell merge editing (anchor-render only).
- Column-width dragging (ctl `col.width` exists; UI resize is a follow-up).
- Real toolbar icons and Marketplace publishing — the shared release-polish
  pass after both editors are in.
- yppxy (.mpp) — still designed-for via the same seam, still not built.
