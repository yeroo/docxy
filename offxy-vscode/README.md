# Offxy for VS Code

Open, read, and **edit** Microsoft Word `.docx` **and** Excel `.xlsx` files right
in a VS Code editor tab — one extension, two faithful editors that use the
editor's own font, size, and theme colors instead of embedding a Word/Excel
canvas. **No ribbon**: the keyboard and command palette drive everything,
exactly like editing code.

It's powered by [`docxwasm`](../docxwasm) and [`gridwasm`](../gridwasm) —
WebAssembly builds of the dependency-free [`docxcore`](../docxcore) and
[`gridcore`](../gridcore) engines that also back the `docxy` and `xlsxy`
terminal apps. For each format, the *entire* pipeline — parse → render → edit
→ **lossless save** — runs in the webview as a small `.wasm`. There is no
JavaScript docx/xlsx library, no server, and no external process.

## Why it's different

Most `.docx`/`.xlsx` extensions either only *view* (read-only preview) or
*edit through a lossy intermediate* (open → convert → re-serialize), which
quietly degrades formatting, formulas, and unmodeled parts on save. Offxy
edits the **real OOXML model** for both formats and preserves every
unmodeled part of the original package byte-for-faithful — so a document or
workbook you open, edit, and save comes back structurally intact. That
lossless round-trip is `docxcore`'s and `gridcore`'s core guarantee, carried
straight into the editor tab.

## Build

Prerequisites: a Rust toolchain with the wasm target, and Node ≥ 18.

```sh
rustup target add wasm32-unknown-unknown
cd offxy-vscode
npm install
npm run build          # builds both wasm bridges, copies them into media/, bundles the extension
```

`npm run build` runs two steps you can also invoke separately:

- `npm run build:wasm` — `cargo build -p docxwasm -p gridwasm --target
  wasm32-unknown-unknown --release` and copies the artifacts to
  `media/docxwasm.wasm` and `media/gridwasm.wasm`.
- `npm run build:ext` — bundles `src/extension.ts` to `out/extension.js` with
  esbuild.

Then press <kbd>F5</kbd> in VS Code (Run ▸ Start Debugging) to launch an Extension
Development Host, and open any `.docx` or `.xlsx` file.

## Word — what works

- **Faithful rendering** — paragraphs, runs (bold / italic / underline / strike /
  color), headings, lists, tables (with borders), hyperlinks, and **embedded
  images** (PNG/JPEG/GIF/BMP/SVG painted over their placeholder boxes; vector
  WMF/EMF fall back to a labeled box), laid out on a character grid at the
  editor's text size, honoring the active color theme.
- **Editing** — type, Enter/Backspace/Delete, arrow / word / document navigation,
  click to place the caret and drag to select, <kbd>Ctrl/Cmd</kbd>+
  <kbd>B</kbd>/<kbd>I</kbd>/<kbd>U</kbd> formatting, <kbd>Ctrl/Cmd</kbd>+
  <kbd>A</kbd> select-all, and copy/cut/paste mediated through the OS clipboard.
- **Formatting surface** — a slim, no-ribbon toolbar (bold / italic / underline /
  strike, Heading 1–2 / Normal, bulleted & numbered lists, alignment, font
  size), with every action also on the command palette (`Docxy: …`). Headings,
  lists, and alignment apply to the selected paragraphs.
- **Find & replace** — <kbd>Ctrl/Cmd</kbd>+<kbd>F</kbd> searches the rendered
  document with VS Code's own find widget; **Docxy: Replace…** runs a
  replace-all through the engine.
- **Markdown ⇄ Word** — right-click a `.md` file → **Convert Markdown to Word
  (.docx)** (opens the result in Docxy), and **Docxy: Export to Markdown (.md)**
  from an open document. Both run the wasm engine in the extension host — no
  open editor required for the conversion.
- **Native integration** — the VS Code **dirty indicator**, **undo/redo**
  (<kbd>Ctrl/Cmd</kbd>+<kbd>Z</kbd>/<kbd>Y</kbd>), **Save**, **Save As**, and
  **hot-exit backup** all work, driven through the standard `CustomEditor` edit
  events. Edits stay lockstep with the wasm engine's own undo stack.
- **Lossless save** — the original package is preserved; only the document part
  is re-serialized from the edited model.
- **Empty-file create flow** — opening a 0-byte `.docx` offers to create a new
  Word document in its place.

## Spreadsheet — what works

- **Virtualized grid** — a sticky-header, virtualized HTML grid renders only
  the visible viewport (rows/columns are fetched from the engine as you
  scroll), so large workbooks stay responsive, at the editor's font and size,
  honoring the active color theme.
- **Formula bar** — shows the selected cell's formula or literal, editable in
  place; `=` starts a formula, <kbd>Enter</kbd>/<kbd>Tab</kbd> commits and
  advances the selection.
- **Recalculation via `gridcore`** — the same dependency-graph recalc engine
  behind `xlsxy` runs in the webview: editing a cell recalculates every
  formula that depends on it, with Excel-faithful semantics. A formula the
  engine can't parse or evaluate keeps Excel's cached value untouched instead
  of guessing — Excel-faithful for what it computes, conservative for what it
  can't.
- **Cell editing** — type-to-replace, <kbd>F2</kbd> to edit in place,
  navigation (arrows, <kbd>Tab</kbd>, <kbd>Enter</kbd>), range selection by
  click-drag or <kbd>Shift</kbd>+move, and clear/delete.
- **Clipboard** — copy/cut/paste as TSV mediated through the OS clipboard,
  round-tripping each cell's raw content (including formula source) so
  ranges are interoperable with Excel and other spreadsheet apps. There is
  no fill handle, and paste re-enters formula text verbatim — it does not
  translate relative references the way Excel's paste does.
- **Structural edits** — insert/delete rows and columns, rewriting every
  affected formula in the workbook.
- **Sheets** — sheet tabs to switch, add, and rename worksheets.
- **Native integration** — the VS Code **dirty indicator**, **undo/redo**,
  **Save**, **Save As**, and **hot-exit backup**, driven through the standard
  `CustomEditor` edit events, in lockstep with the wasm engine's own undo
  stack.
- **Lossless save** — anything the engine doesn't model (charts, pivots,
  conditional formatting…) is preserved byte-for-byte; only touched parts are
  re-serialized.
- **Empty-file create flow** — opening a 0-byte `.xlsx` offers to create a new
  workbook in its place.

## Architecture

| Layer | Where | Role |
|-------|-------|------|
| `docxcore` / `gridcore` | Rust crates | the DOCX/XLSX engines (parse/model/render/edit/save) |
| `docxwasm` / `gridwasm` | Rust `cdylib` → `.wasm` | hand-written C-ABI seams (no wasm-bindgen) over each engine, over length-prefixed buffers |
| `src/extension.ts` | extension host | a registration table of binary `CustomEditorProvider`s: opens files, relays undo/redo & save, writes bytes; owns dirty state |
| `media/webview.js` | webview (Word) | loads `docxwasm`, paints the document grid, captures keyboard/mouse, and serializes on save |
| `media/grid.js` | webview (Excel) | loads `gridwasm`, paints the virtualized spreadsheet grid + formula bar + sheet tabs, and serializes on save |

The webview owns the live editing session (low latency, no host round-trip per
keystroke); the host owns the file lifecycle. They talk over a small
`postMessage` protocol — the webview reports each mutating edit so VS Code can
light the dirty dot and route undo/redo back into the wasm editor.

See [../VSCODE.md](../VSCODE.md) for the full design.

## AI assistants

Every open `.docx`/`.xlsx` tab exposes the same agent [control surface](../docs/agent-control.md)
as the `docxy`/`xlsxy` terminal apps — an AI assistant can read and edit the
**live, in-memory** document (including unsaved changes), with the edit
landing on VS Code's own undo stack and repainting the tab instantly, exactly
like driving a terminal pane. See ["VS Code tabs"](../docs/agent-control.md#vs-code-tabs)
for the two ways a tab's semantics differ from a terminal instance.

- **GitHub Copilot (agent mode)** — automatic. This extension registers
  `mcp/server.mjs`, its bundled dependency-free MCP bridge, as an MCP server
  provider on activation (`contributes.mcpServerDefinitionProviders` +
  `vscode.lm.registerMcpServerDefinitionProvider`, VS Code ≥ 1.101).
  Copilot's agent mode discovers the tools with no configuration.
- **Claude Code** — one-liner:

  ```sh
  claude mcp add offxy -- node <extension-path>/mcp/server.mjs
  ```

  `<extension-path>` is wherever VS Code installed this extension — typically
  `~/.vscode/extensions/yeroo.offxy-0.3.0` (Windows:
  `%USERPROFILE%\.vscode\extensions\yeroo.offxy-0.3.0`). **Caveat:** that path
  is *versioned* — it changes on every extension update, so the registration
  silently goes stale after an upgrade until you re-run `claude mcp add` with
  the new path. If you already have the `docxy`/`xlsxy` terminal binaries on
  `PATH`, `claude mcp add docxy -- docxy --mcp` (and the `xlsxy` equivalent)
  sidesteps the versioned-path problem entirely — see
  [docs/agent-control.md](../docs/agent-control.md#mcp-native-tools-in-claude-code).
- **Tools** — the bundled server (`serverInfo.name` `"offxy"`) exposes exactly
  the tool surface the terminal apps' own `docxy --mcp`/`xlsxy --mcp` do (53
  tools total): `docxy_list`, `docxy_new`, `docxy_status`, `docxy_outline`,
  `docxy_read`, `docxy_find`, `docxy_replace_range`, `docxy_insert`,
  `docxy_append`, `docxy_save`, `docxy_export`, `docxy_export_pdf`,
  `docxy_comments`, `docxy_notes`, `docxy_header`, `docxy_footer`,
  `docxy_metadata`, `docxy_stats`, `docxy_replace_all`, `docxy_undo`,
  `docxy_redo` (21) — `docxy_replace_range`/`docxy_insert`/`docxy_append` each
  take an optional `markdown` flag to splice formatted Markdown (headings,
  bold, lists, tables, links) into the document instead of plain text — and
  `xlsxy_list`, `xlsxy_new`, `xlsxy_status`, `xlsxy_sheets`, `xlsxy_read`,
  `xlsxy_get`, `xlsxy_set`, `xlsxy_clear`,
  `xlsxy_find`, `xlsxy_recalc`, `xlsxy_save`, `xlsxy_comments`,
  `xlsxy_comment_add`, `xlsxy_comment_remove`, `xlsxy_range_set`,
  `xlsxy_export_csv`, `xlsxy_import_csv`, `xlsxy_pivot`, `xlsxy_replace_all`,
  `xlsxy_sheet_add`, `xlsxy_sheet_remove`, `xlsxy_sheet_rename`,
  `xlsxy_row_insert`, `xlsxy_row_delete`, `xlsxy_col_insert`,
  `xlsxy_col_delete`, `xlsxy_eval`, `xlsxy_stats`, `xlsxy_charts`,
  `xlsxy_pivots`, `xlsxy_format`, `xlsxy_col_width` (32). It's
  a thin bridge — it opens no document itself, only forwards to whichever
  `docxy`/`xlsxy` instance (a VS Code tab or a terminal pane) is already
  running (the `_new` tools are the exception: they create the file on disk
  themselves, then hand off to an instance via the same open forwarding);
  pass `target` (a substring of the instance/pane id) to disambiguate
  when several are open, or call `docxy_list`/`xlsxy_list` to see what's
  running.

## Install

Grab the `offxy-*.vsix` from the [latest release](https://github.com/yeroo/docxy/releases/latest)
and install it:

```sh
code --install-extension offxy-0.3.0.vsix
```
