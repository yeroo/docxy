# Changelog

## Unreleased

- **New: the bundled MCP server (`mcp/server.mjs`) is now registered with VS
  Code's MCP API** (`contributes.mcpServerDefinitionProviders` +
  `vscode.lm.registerMcpServerDefinitionProvider`), so GitHub Copilot's agent
  mode picks up the `docxy_*`/`xlsxy_*` tools automatically, with no user
  configuration. Claude Code users still register it manually — see the
  README's new "AI assistants" section for the one-liner and its
  versioned-extension-path caveat. Bumped `engines.vscode` (and the
  `@types/vscode` devDependency) from `^1.84.0` to `^1.101.0`: the MCP
  server-definition-provider API this uses stabilized (left proposed status)
  in VS Code 1.101 (May 2025), and is not present in 1.84.
- **Renamed:** the extension is now **offxy** (`yeroo.offxy`) — one extension
  for Word and (soon in this version) Excel. Uninstall `yeroo.docxy-vscode`
  and install the `offxy-*.vsix`. Command ids changed `docxy.*` → `offxy.*`
  (update any custom keybindings).
- Opening an empty (0-byte) `.docx` — e.g. a file just created in the
  explorer — now offers to create a new Word document in its place.
- Fixed the caret visually jumping to the document start when typing a
  space at a soft-wrap margin (the wrapped-away space made the caret's
  position unmappable); it now stays pinned at the wrap margin.
- **New: Excel `.xlsx` editing** (`offxy.gridEditor`), powered by a new
  `gridwasm` bridge over `gridcore`:
  - Virtualized grid with sticky headers and a formula bar, rendered at the
    editor's font/size and honoring the color theme.
  - Full recalculation on edit via `gridcore`'s dependency-graph engine, with
    Excel-faithful semantics: a formula the engine can't parse or evaluate
    keeps Excel's cached value untouched — Excel-faithful for what it
    computes, conservative for what it can't.
  - Cell editing (type-to-replace, `F2`, navigation, range selection),
    clipboard (copy/cut/paste as TSV through the OS clipboard, round-tripping
    raw cell content including formula source), and structural edits
    (insert/delete rows and columns, rewriting affected formulas).
  - Sheet tabs — switch, add, and rename worksheets.
  - Native dirty state, undo/redo, Save, Save As, and hot-exit backup, in
    lockstep with the wasm engine's own undo stack.
  - **Lossless save** — unmodeled parts (charts, pivots, conditional
    formatting…) are preserved byte-for-byte.
  - Opening an empty (0-byte) `.xlsx` offers to create a new workbook in
    its place, matching the `.docx` empty-file flow.

## 0.3.0

Initial release of the Docxy VS Code extension — open and edit Word `.docx` in
a VS Code editor tab.

- Faithful monospace-grid rendering (runs, headings, lists, tables, links,
  embedded images) at the editor's font/size, honoring the color theme.
- Editing: typing, navigation, selection, click/drag, copy/cut/paste.
- No-ribbon formatting toolbar + `Docxy: …` command palette (bold/italic/
  underline/strike, headings, bulleted & numbered lists, alignment, font size).
- Find (VS Code find widget) and Replace (`Docxy: Replace…`).
- Native dirty state, undo/redo, Save, Save As, and hot-exit backup.
- **Lossless save** — edits the real OOXML model; unmodeled parts are preserved.

Powered by a WebAssembly build of the dependency-free `docxcore` engine.
