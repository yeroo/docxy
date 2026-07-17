# Offxy in VS Code — design

This document covers the **VS Code extension** side of the workspace: the
`docxwasm` and `gridwasm` bridge crates and the `offxy-vscode` extension that
together open and edit Word `.docx` **and** Excel `.xlsx` files inside a VS
Code editor tab. For the terminal apps see the [README](README.md); for the
spreadsheet and project sides see [SPREADSHEET.md](SPREADSHEET.md) and
[PROJECT.md](PROJECT.md).

`offxy-vscode` started as `docxy-vscode` (Word only) and was renamed to
**offxy** when Excel `.xlsx` editing was added as a second custom editor in
the same extension; the two editors are independent (separate wasm bridge,
separate webview) and share only the extension host's registration table.

## The idea

`docxy` and `xlsxy` render a `.docx`/`.xlsx` faithfully onto a **character
grid** in the terminal — not a pixel-perfect Word/Excel page, but a readable,
editable view of the real document or workbook. A VS Code editor tab is also a
character grid. So the same render engines that paint the terminal can paint
an editor tab: the document/workbook reads like text in your editor, at the
editor's own font and size, honoring your color theme — and *without a
ribbon*, because VS Code already has the command palette, the editor title
bar, and keybindings.

The enabling fact: [`docxcore`](docxcore) and [`gridcore`](gridcore) — the
whole DOCX and XLSX engines (their own ZIP, DEFLATE, XML, document/workbook
model, renderer, editor, and **lossless** serializer) — are pure `std` with no
third-party crates. That compiles straight to `wasm32-unknown-unknown` and
runs inside a webview. There is no JavaScript docx/xlsx library involved; the
same Rust code that backs the terminal apps backs the extension.

## Why this is worth doing

The existing `.docx`/`.xlsx` extensions fall into two camps, and both leave a
gap:

- **Read-only viewers** render to HTML/canvas and can't edit.
- **HTML/JS-editing** extensions open the file, convert it to an intermediate
  form, let you edit that, then *re-serialize it back* — which silently
  degrades styles, numbering, tables, headers, or formulas on every save.

`docxcore` and `gridcore` edit the **real OOXML model** and preserve every
unmodeled part of the original package byte-for-faithful (bookmarks, fields,
content controls, section properties, charts, pivots, conditional
formatting, …). A document or workbook opened, edited, and saved comes back
structurally intact. That lossless round-trip — the terminal apps' headline
feature — is exactly what the crowded extension field lacks, and it carries
into the editor tab unchanged, for both formats.

## Crates & pieces

| Piece | Language | Responsibility |
|-------|----------|----------------|
| [`docxcore`](docxcore) / [`gridcore`](gridcore) | Rust | the DOCX/XLSX engines (unchanged; shared with `docxy`/`xlsxy`) |
| [`docxwasm`](docxwasm) / [`gridwasm`](gridwasm) | Rust `cdylib` | hand-written WebAssembly ABIs over `docxcore`/`gridcore` |
| [`offxy-vscode/src/extension.ts`](offxy-vscode) | TypeScript | the extension host: a registration table of binary `CustomEditorProvider`s (`offxy.docxEditor`, `offxy.gridEditor`) |
| [`offxy-vscode/media/webview.js`](offxy-vscode) | JavaScript | the Word webview: `docxwasm` loader, grid painter, input handling |
| [`offxy-vscode/media/grid.js`](offxy-vscode) | JavaScript | the Excel webview: `gridwasm` loader, virtualized spreadsheet grid, formula bar, sheet tabs |

## The wasm ABI

`docxwasm` and `gridwasm` deliberately use **no `wasm-bindgen`** — matching
the project's from-scratch ethos and keeping each artifact small and
auditable. `docxwasm`'s seam is a handful of C-ABI exports:

```
docx_alloc(len) -> ptr          docx_free(ptr, len)
docx_open(ptr, len) -> handle   docx_close(handle)
docx_render(handle) -> result
docx_cmd(handle, ptr, len) -> result
docx_media(handle, ptr, len) -> result   // raw bytes of the media for an rId
docx_save(handle) -> result
```

- **Memory** is shared by pointer. The host allocates a buffer with `docx_alloc`,
  writes input into wasm memory, and frees it after the call.
- Every result-returning export returns a pointer to a **length-prefixed buffer**
  (`[u32 little-endian length][payload]`), which the host reads and then frees —
  no 64-bit return values or BigInt on the JS side.
- A document is an opaque `u32` **handle** from `docx_open`; sessions live in a
  thread-local registry (wasm is single-threaded).

The interesting logic lives in `docxwasm::bridge` as plain, natively-testable
Rust (`cargo test -p docxwasm`); `lib.rs` is just marshalling. The **render**
channel serializes the engine's styled lines to compact JSON
(`{lines:[[{t,b,i,u,s,d,h,c,lnk}...]], caret, selection, dirty, width}`); colors
map to VS Code terminal ANSI theme variables. The **command** channel is a
trivial tab-delimited string (`insert\tX`, `move\tleft\t1`, `click\t3\t5\t0`,
`bold`, `undo`, …), so no JSON *parser* is needed on the Rust side.

The view is a **continuous flow** (`page_view = false`): no pagination, headers,
or footers — the fidelity target is "text in an editor tab," not a Word page.

`gridwasm`'s seam is smaller — it's viewport-based rather than full-render,
since a spreadsheet grid is virtualized:

```
grid_alloc(len) -> ptr   grid_free(ptr, len)
grid_open(ptr, len) -> handle   grid_close(handle)
grid_cmd(handle, ptr, len) -> result
grid_save(handle) -> result
grid_new() -> result      // bytes of a fresh empty workbook (used host-side too)
```

Memory sharing and the length-prefixed result buffer follow the same
convention as `docxwasm`. The single tab-delimited `grid_cmd` channel carries
both queries and mutations — `view\t…` (fetch a viewport window), `select`,
`set`, `clear`, `cut`, `paste`, `undo`/`redo`, `insrow`/`delrow`/`inscol`/
`delcol`, and `sheet\t(switch|add|rename)\t…` — dispatched in
`gridwasm::bridge` (`cargo test -p gridwasm`), with the underlying edits and
recalculation delegated straight to `gridcore::Engine`.

## Host ↔ webview split

The webview owns the **live editing session** (the wasm engine), so keystrokes
are local — no host round-trip per character. The extension host owns the **file
lifecycle**. They talk over a small `postMessage` protocol:

- On open, the host reads the file and sends the bytes; the webview instantiates
  the wasm, opens the document, and paints.
- Each **mutating** edit makes the webview post `{type:'edit'}`; the host fires
  `onDidChangeCustomDocument`, which lights VS Code's **dirty** dot and registers
  an undo/redo entry whose callbacks post `{type:'do', op:'undo'|'redo'}` back to
  the webview. Because it's exactly one VS Code edit per webview edit, VS Code's
  undo stack stays in lockstep with the wasm editor's own — and host-driven undo
  must *not* echo another `edit` (verified).
- On **save** / **backup**, the host requests bytes; the webview calls
  `docx_save` and replies. The host writes them with `workspace.fs`. Because the
  provider is the binary `CustomEditorProvider` (not the text variant), Save,
  Save As, revert, and hot-exit all route through it.
- **Clipboard** is mediated through the host (`vscode.env.clipboard`), since the
  webview's selection model is its own, not the DOM's.

The `offxy.gridEditor` follows the same split (webview owns the `gridcore`
session, host owns the file lifecycle) over the analogous protocol; its
clipboard payload is **TSV**, so ranges round-trip with Excel and other
spreadsheet apps through the same OS-clipboard mediation.

## Verification

- `cargo test -p docxwasm` exercises open → render → edit → **save → reopen** in
  plain Rust (the round-trip proves the edit survives lossless serialization).
  `cargo test -p gridwasm` does the same for the spreadsheet bridge (open →
  view → edit/structural-edit/sheet-op → **save → reopen**), on top of
  `cargo test -p gridcore` for the underlying engine.
- The wasm is exercised end-to-end in a JS runtime (open a real `.docx`, render,
  type, format, undo, save, reopen) and the webview client is smoke-tested under
  jsdom against the real `.wasm` — confirming the message protocol, the dirty/
  undo bookkeeping, and that `docx_save` returns valid ZIP (`PK`) `.docx` bytes.

## Status & next steps

### Word (`offxy.docxEditor`)

Working today: faithful rendering (runs, headings, lists, tables, links, and
**embedded images**), editing (typing, navigation, selection, B/I/U,
copy/cut/paste), a no-ribbon formatting toolbar + command palette (headings,
lists, alignment, font size), **find** (VS Code's find widget over the rendered
text) and **replace** (the engine's replace-all), and native dirty / undo-redo /
save / Save As / backup with lossless round-trip. Opening an empty (0-byte)
`.docx` offers to create a new document in its place.

Images ride the same overlay idea the terminal app uses: `render_with_images`
returns image **boxes** (grid position + size + relationship id) in the JSON
view, and a `docx_media(rid)` ABI export returns the raw media bytes. The webview
fetches each rid once, sniffs the format, and paints a data-URI `<img>` over the
placeholder box (raster + SVG); vector WMF/EMF, which browsers can't decode, fall
back to a labeled box — exactly the terminal app's fallback.

**Markdown ⇄ docx** conversion runs the *same wasm in the extension host*
(Node instantiates `docxwasm` too), via stateless exports `docx_from_markdown`
and `docx_to_md` — so **Convert Markdown to Word** (right-click a `.md`) and
**Export to Markdown** (from an open document) work without an open editor or a
webview round-trip. This is the one place the engine runs host-side rather than
in the webview.

Word next steps, roughly in order:

1. **Color / font pickers** over the `color` and `fontsize` bridge commands.
2. **Table structure editing** (add/delete rows and columns).

### Excel (`offxy.gridEditor`)

Working today: a virtualized grid with sticky headers and a formula bar,
recalculation on edit via `gridcore`'s dependency graph, cell editing
(type-to-replace, `F2`, navigation, range selection), TSV clipboard (copy/
cut/paste, relative-reference translation on paste/fill), structural edits
(insert/delete rows and columns, rewriting affected formulas), sheet tabs
(switch/add/rename), and native dirty / undo-redo / save / Save As / backup
with lossless round-trip. Opening an empty (0-byte) `.xlsx` offers to create
a new workbook in its place.

Excel next steps, roughly in order:

1. **Sheet delete** (currently switch/add/rename only, matching the TUI's
   undo-model parity for sheet-add).
2. Formula-bar autocomplete / function help.
3. Formatting surface (number formats, fills, borders) beyond what the
   source workbook already carries.
