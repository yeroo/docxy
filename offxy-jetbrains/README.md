# Offxy for JetBrains IDEs

**Native** Word `.docx` and Excel `.xlsx` editors for IntelliJ-platform IDEs
— no webview, no Microsoft Office, no per-platform native code. The engines
are the dependency-free [docxy](https://github.com/yeroo/docxy) Rust cores
(parse → render → edit → **lossless save**), compiled to WebAssembly and
executed on the JVM by [Chicory](https://chicory.dev). The same
`docxwasm.wasm`/`gridwasm.wasm` artifacts also power the
[VS Code extension](../offxy-vscode) and share its release train.

## How it works

The engine renders the document as a character grid (the same faithful
rendering the terminal `docxy` uses), and that grid *is* the text of a real
IntelliJ editor over a live, **editable** `Document`:

- **Typing is native.** Your keystrokes land in the Document at editor speed
  on any document size; a `DocumentListener` replays each change into the
  engine, whose authoritative render reconciles the view by minimal diff —
  a no-op when nothing re-wrapped.
- **Structure is guarded.** List markers, table borders, and image regions
  are read-only guarded blocks; the text between them is yours to edit.
- **The platform does the chrome.** Find (Ctrl+F), undo/redo (text edits
  undo natively; formatting undoes as one snapshot step), Save All, themes,
  editor font and zoom — all standard IDE behavior.
- **Formatting toolbar** (bold/italic/underline/strike, headings, lists,
  alignment, font size) plus Tools-menu actions: export to Markdown,
  replace-all. Project-view: convert a `.md` to `.docx`.
- **Lossless save.** The engine retains the whole OOXML package; saving
  regenerates only what you changed and preserves everything else
  byte-for-byte.

## The spreadsheet editor

`.xlsx` opens in a **virtualized native grid** over gridwasm's windowed
viewport protocol — the table asks the engine only for the visible window,
so scrolling huge sheets stays flat:

- **Full editing:** values and formulas (leading `=`, validated, live
  recalculation across the window), type-through or F2/double-click in
  place, a formula bar synced with the selection.
- **Structure:** insert/delete rows/columns (workbook-wide reference
  rewriting) from the context menu; sheet strip to switch/add/rename.
- **Formatting:** bold/italic/alignment/decimals/autosum toolbar; display
  honors the workbook's number formats and colors.
- **Clipboard:** rectangular TSV copy/cut/paste through the OS clipboard.
- **Undo:** every mutation is one engine transaction = one Ctrl+Z step
  (the engine's own undo stack drives it — including agent edits).
- **Lossless save**, same guarantee as the Word editor.

## AI assistants (agent control surface)

Every open tab advertises on the matching app's
[control surface](../docs/agent-control.md) — `docxy-jetbrains-…` for
documents, `xlsxy-jetbrains-…` for workbooks; the same loopback protocol the
terminal apps and VS Code tabs use. Any `docxy --mcp`/`xlsxy --mcp` server
sees IDE tabs with zero configuration:

```sh
claude mcp add docxy -- docxy --mcp     # Claude Code (Word)
claude mcp add xlsxy -- xlsxy --mcp     # Claude Code (Excel)
```

JetBrains AI Assistant / Junie: add `docxy --mcp` as an MCP server in its
settings. Agent reads and edits go through the same `docx_ctl` engine surface
the VS Code tabs use (outline/read/find/replace/insert/append/format/styles/
comments/metadata/…); edits repaint the tab live and land as one IDE undo
step each. `doc.undo`/`doc.redo` are rejected on JetBrains tabs (undo is
IDE-owned) and `doc.export-pdf` is a follow-up; see the "JetBrains tabs"
section of [agent-control.md](../docs/agent-control.md#jetbrains-tabs).

## Install

Grab `offxy-jetbrains-<version>.zip` from the
[releases](https://github.com/yeroo/docxy/releases) page, then in your IDE:
**Settings → Plugins → ⚙ → Install Plugin from Disk…** Requires any
IntelliJ-platform IDE 2024.2 or newer (IDEA, PyCharm, WebStorm, …).

## Build from source

```sh
cd offxy-jetbrains
./gradlew buildPlugin        # → build/distributions/offxy-jetbrains-*.zip
./gradlew runIde             # launch a sandbox IDE with the plugin
./gradlew test               # engine, editor, property, and ctl-bridge tests
```

Needs JDK 17+ and a Rust toolchain with the `wasm32-unknown-unknown` target
(the build invokes cargo when the wasm artifact is stale).

## Known limits (v1)

- Continuous flow (no page view/pagination) — the same fidelity target as
  the VS Code extension.
- WMF/EMF/SVG images show as labeled placeholder boxes (PNG/JPEG/GIF/BMP
  render inline).
- Double-width (CJK) column mapping can briefly drift until the engine
  reconciles (Word editor).
- Spreadsheet: merged cells render at their anchor without spanning; charts
  and pivot tables render as data; no frozen panes or column-drag resize
  yet; agent `sheet.remove` is not undoable from the IDE (unlike VS Code
  tabs' single-slot restore).
