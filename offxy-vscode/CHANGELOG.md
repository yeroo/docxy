# Changelog

## Unreleased

- **New: a WYSIWYG editor for Markdown files** (`offxy.markdownEditor`,
  opt-in via **Reopen Editor With → Docxy Markdown** — `.md` still opens as
  plain text by default). Reuses the Word editor's rendering/editing engine:
  a `.md` file loads through `markdownToDocx` and saves back through
  `docxToMarkdown`, so the on-disk file stays markdown throughout — no
  `.docx` is ever written. The toolbar (and its command-palette/keyboard
  equivalents) hides and disables formatting Markdown can't represent
  (underline, alignment, font size); task-list items (`- [ ]` / `- [x]`)
  render as ☐ / ☑ (display-only — the saved text is still literal
  `[ ]`/`[x]`). The first save re-emits the file as canonical markdown
  (hard-wrapped prose unwraps to one line per paragraph); after that,
  open/edit/save cycles are stable.
- **Fixed: the markdown round-trip (`docxcore`) is now faithful and
  idempotent** for the constructs docxy supports — task lists no longer get
  mangled by bracket-escaping (`- [ ]` used to round-trip as `- \[ \]`), and
  a soft-wrapped continuation line under a list item now stays part of that
  item instead of breaking the list into a separate paragraph. A committed
  corpus test asserts a second `from_markdown`/`to_markdown` pass is a no-op
  across headings, emphasis/code/strike, links, nested/ordered/task lists
  with continuations, tables, fenced code, blockquotes, thematic breaks, and
  math — the basis the new markdown editor's save-stability depends on.
- **New: the wave-3 agent styling + persistent-pivots surface** — the agent
  control surface (terminal apps, tabs, and both MCP servers) grows from 53
  to **56 tools**:
  - Two new docxy verbs apply direct formatting to existing content:
    `doc.format` (`docxy_format`) sets a patch — `bold`, `italic`,
    `underline`, `strike` (set-to-value, not toggle), `color`, `highlight`
    (`yellow`/`green`/`cyan`/`magenta`/`red`/`blue`/`lightGray`/`darkYellow`,
    or `"none"` to clear), `font`, `size` — onto every run in a block range,
    as one undo checkpoint; `doc.set-style` (`docxy_set_style`) applies a
    paragraph style (`Heading1`–`Heading6`, `Quote`, `SourceCode`, or
    `Normal` to clear) and/or an alignment (`left`/`center`/`right`/
    `justify`) to a block range, auto-ensuring the style definition so it
    actually renders in Word, also as one undo checkpoint. Both land as a
    single true wasm-undo-stack entry on tabs (<kbd>Ctrl+Z</kbd> undoes the
    whole call).
  - One new xlsxy verb, `pivot.create` (`xlsxy_pivot_create`), builds a REAL,
    persistent workbook pivot table — not the existing read-only, ad-hoc
    `sheet.pivot` — landing its computed output on a new sheet (reply:
    `{sheet, name}`). It's refreshed by `wb.recalc` like any other pivot
    (which now refreshes pivots as well as formulas) and survives
    `wb.save`/reload. Undo is a history-clear + host-orchestrated inverse
    (`sheet.remove` on the new sheet), matching `sheet.import-csv`; on tabs,
    one <kbd>Ctrl+Z</kbd> removes the sheet AND the pivot registration
    together (both-or-neither — `sheet.remove`'s cascade and its own
    restore path now carry pivot registrations along with the sheet either
    way), and redoing brings both back.
  - See [docs/agent-control.md](../docs/agent-control.md) for the full
    patch-key table, accepted style/align/highlight sets, and pivot
    placement/refresh/persistence details.
- **New: the wave-2 agent formatting surface** — the agent control surface
  (terminal apps, tabs, and both MCP servers) grows from 51 to **53 tools**:
  - `doc.insert`, `doc.replace-range`, and `doc.append` (docxy) gain an
    optional `markdown` flag: `text` is parsed as Markdown and the resulting
    formatted blocks — headings, bold/italic/strike, links, nested bullet
    and numbered lists, tables, blockquotes, fenced code, math, Mermaid
    fences — are spliced into the **existing** document at the same
    position the plain-text write would target. Referenced paragraph styles
    (`Heading1`–`Heading6`, `Quote`, `SourceCode`) are auto-ensured in
    `styles.xml` if the target document doesn't already define them, and
    referenced list numbering is auto-ensured the same way, so a heading or
    a bulleted list written into an old, styles-sparse `.docx` still renders
    correctly in Word. `markdown:false` (the default) is byte-identical to
    today's plain-text behavior; undo-step parity with plain text is
    preserved. An empty/whitespace-only markdown write errors
    `"empty markdown"` and touches nothing.
  - Two new xlsxy verbs: `cell.format` (`xlsxy_format`) applies a patch —
    `numFmt`, `bold`, `italic`, `fontColor`, `fillColor`, `align` — to every
    cell in a range as one undo group; `col.width` (`xlsxy_col_width`) sets
    a column's width (a fractional Excel column-width number), undoable via
    a prior-width inverse rather than a wasm undo-stack entry (like
    `comment.add`/`comment.remove`).
  - `cell.get`'s reply gains an additive, present-if-set `format` object
    echoing the cell's current style for the six `cell.format` keys above —
    scoped to `cell.get` only; `sheet.read`, `find`, and `cell.set` don't
    carry it. An unstyled cell has no `format` key.
  - See [docs/agent-control.md](../docs/agent-control.md) for the full
    construct table, patch-key table, and error family.
- **New: the wave-1 agent verb surface** — every tab (Word and Excel) and both
  terminal apps (`docxy`, `xlsxy`) gain ~30 new control-surface verbs exposed
  as MCP tools, growing the bundled server (and both terminal `--mcp`
  binaries) from 21 to **51 tools**: docxy adds `doc.export`,
  `doc.export-pdf`, `doc.comments`, `doc.notes`, `doc.header`/`doc.footer`,
  `doc.metadata`, `doc.stats`, `doc.replace-all`, `doc.undo`/`doc.redo`;
  xlsxy adds comment read/write, `range.set` (atomic block writes),
  `wb.export-csv`, `sheet.import-csv`, an ad-hoc read-only `sheet.pivot`,
  `wb.replace-all` (spans every sheet), `sheet.add`/`remove`/`rename`,
  `row.*`/`col.*` insert/delete, `formula.eval`, `sheet.stats`, and
  `chart.list`/`pivot.list`. See [docs/agent-control.md](../docs/agent-control.md)
  for the full verb tables and tool lists.
  - Every mutating verb lands on VS Code's own undo/redo stack as a labeled
    "Agent: `<verb>`" entry, in lockstep with the wasm engine's own undo
    stack — `doc.undo`/`doc.redo` fire a new inverse-wasm-op edit event
    rather than replaying the existing stack; comment add/remove and
    sheet-structural ops (`sheet.import-csv`, `sheet.remove`) drive a
    host-orchestrated inverse request instead of a wasm undo replay, since
    those changes live outside the cell-level undo model.
  - Agent `sheet.remove` is undoable — content, comments, and sheet-scoped
    defined names are restored — but the restored sheet re-appends at the
    **end** of the tab's sheet order, and the restore is backed by a
    **single-slot stash**: a second consecutive `sheet.remove` before
    undoing the first only leaves the second one recoverable, and undoing
    past it surfaces a warning instead of silently failing.
  - A tab's `comment.add` with no `author` defaults to `"agent"` (the
    terminal apps default to the OS username instead).
  - `doc.export-pdf` on a tab is rendered by the wasm engine but **written
    to disk by the extension host** (`docxcore`'s PDF exporter is std-only
    and can't run inside the wasm sandbox); the terminal apps write directly.
  - Added `"activationEvents": []` to `package.json` for `@vscode/vsce` 3.x
    packaging compatibility (3.x otherwise refuses to package a manifest
    with no explicit activation events; VS Code still derives the implicit
    `onCustomEditor:` activations from the `customEditors` contribution
    points, so this is a packaging-only change with no runtime effect).
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
- **New: the bundled MCP server gains `docxy_new`/`xlsxy_new`** — create a
  blank document/workbook and open it, from a blank template shipped under
  `mcp/templates/`.
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
