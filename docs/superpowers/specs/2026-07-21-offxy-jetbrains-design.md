# Offxy JetBrains Plugin (native docx editor) — Design

**Date:** 2026-07-21
**Status:** Approved (design review with Boris, this session)

## Summary

A **native** JetBrains IDE plugin named **Offxy** that edits Word `.docx` files
in an IntelliJ editor tab — no JCEF, no webview. The plugin runs the *same*
`docxwasm.wasm` artifact the VS Code extension ships, executed on the JVM by
**Chicory** (a pure-Java WebAssembly runtime, no native code), and paints the
engine's styled-line view with a custom Swing component. Docx ships in v1;
the xlsx grid editor is designed-for (a second provider registration over
`gridwasm`'s viewport protocol) but not built. Every open tab advertises on the
ctlcore agent-control protocol, so Claude Code and Junie can read and edit live
documents exactly as they do terminal docxy panes and VS Code tabs.

## Decisions made during review

- **Native, not JCEF** (Boris). This is cheap because the webview was never a
  rich-text editor: `docxwasm::Session` renders the document with the TUI's
  grid engine — styled monospace lines, caret `(line, col)`, engine-side
  selection, image boxes in grid cells. `webview.js` is 462 lines of painting
  and key forwarding. The native editor is a grid painter, not a word
  processor.
- **Docx first, xlsxy later** (Boris). Marketplace survey: xlsx has several
  viewer plugins; docx editing has only Syncfusion's commercial JCEF-based
  plugin. Docx is the gap and our strongest asset.
- **Swing custom component, not Compose/Jewel.** A monospace grid wants exact
  `FontMetrics` cell math and per-span painting; Compose adds dependency
  weight and a platform-version floor for no gain here.
- **Chicory, not JNI/Panama cdylibs.** `docxwasm.wasm` has zero imports and a
  manual alloc + length-prefixed-result ABI — Chicory's ideal case. One
  artifact shared with the VS Code extension, no per-platform Rust builds.
  The binding hides behind a Kotlin interface so a Panama `cdylib` can
  replace it if benchmarks demand (see Performance gate).
- **Same namespaces for agents:** tabs advertise as
  `docxy-jetbrains-<basename>-<n>` in docxy's ctl dir; existing `docxy --mcp`
  sessions see them with zero reconfiguration. No new MCP server — JetBrains
  AI Assistant/Junie register `docxy --mcp` in their MCP settings (README).
- **Defaults accepted:** module `offxy-jetbrains/` in this repo (own Gradle
  build; the repo stays a cargo workspace), Kotlin + IntelliJ Platform Gradle
  Plugin 2.x, min platform **2024.2**, distribution via GitHub release assets
  first, Marketplace as a follow-up.

## Structure

```
offxy-jetbrains/
  build.gradle.kts             IntelliJ Platform Gradle Plugin 2.x, Kotlin
  settings.gradle.kts          standalone Gradle build inside the repo
  src/main/resources/META-INF/plugin.xml
  src/main/resources/docxwasm.wasm    copied by a Gradle task (see Packaging)
  src/main/kotlin/dev/yeroo/offxy/
    engine/DocxEngine.kt       interface: open/close/render/cmd/save/media/
                               fromMarkdown/toMarkdown (+ ctl when it lands)
    engine/ChicoryEngine.kt    Chicory instantiation + ABI marshalling
    editor/DocxEditorProvider.kt  FileEditorProvider for *.docx (accept by
                               extension; the registration seam for the future
                               grid editor mirrors extension.ts's EDITORS table)
    editor/DocxFileEditor.kt   FileEditor: lifecycle, isModified, save, undo
    editor/DocPanel.kt         the custom-painted grid component
    editor/DocxToolbar.kt      ActionToolbar (same buttons as the webview)
    actions/                   convertMarkdown / exportMarkdown / replace
    ctl/CtlServer.kt           loopback TCP + discovery files (ctlcore wire)
```

## Engine binding (Chicory)

Marshalling is a line-for-line port of `webview.js`'s wasm section: write input
at `docx_alloc(len)`, call the export, read the `[u32 le len][payload]` result
buffer, `docx_free(ptr, 4 + len)`. Exports used: `docx_open/close/render/cmd/
save/media`, stateless `docx_from_markdown` / `docx_to_md`.

- **One Chicory `Instance` per open document.** Isolates linear-memory growth
  per document and makes lifetime trivial (close tab → drop instance). The
  in-module handle registry is still used (handle from `docx_open`), just with
  one live handle per instance.
- **EDT confinement.** A Chicory instance is not thread-safe; all engine calls
  run on the EDT. Paint and input already live there; per-keystroke
  `docx_cmd` + render is the same work the webview does synchronously today.
  Ctl requests hop onto the EDT (see Agent bridge).
- Use Chicory's runtime-AOT backend where available, interpreter as fallback.

**Performance gate:** an early plan task benchmarks per-keystroke
`cmd("insert…")` + view parse on the largest corpus documents. Target ≤16 ms,
acceptable ≤50 ms; a miss triggers the Panama-FFI fallback behind
`DocxEngine` — a seam change, not a redesign.

## Editor panel (DocPanel)

- **Metrics & font:** the IDE's global editor font via
  `EditorColorsManager`/`FontPreferences`; `FontMetrics` gives exact
  `charW`/`lineH` — no DOM-style measuring. On editor-font or LAF change
  (message-bus listeners): remeasure, resync width, repaint.
- **Painting:** spans drawn at `col × charW` so the grid stays aligned
  (terminal-emulator style, never advance-by-string-width); bold/italic by
  font derivation; underline/strike as rules; `dim` via alpha; `h`
  (engine-side selection) paints the scheme's selection background; span
  colors map ANSI names → the scheme's console/terminal palette (the same
  theme-awareness `--vscode-terminal-ansi*` gave the webview). Background is
  the editor background color. Only the clip's line range paints.
- **Scrolling & caret:** the panel sits in a `JBScrollPane`; preferred height
  = `lines × lineH`. Caret is a filled rect with a blink timer,
  `scrollRectToVisible` on every caret move.
- **Width sync:** viewport width → `width\t<cols>` command (min 20 cols),
  debounced on resize, mirroring `syncWidth()`.
- **Images:** rid → `docx_media` bytes, decoded with `ImageIO` (PNG/JPEG/
  GIF/BMP; SVG and WMF/EMF get the labeled fallback box, matching the
  webview), cached per rid, drawn as overlays in `paintComponent` at their
  grid rects.
- **Input:** the `onKeydown` table ports directly — Ctrl+B/I/U, arrows/word/
  home/end/doc moves with Shift extension, Enter/Backspace/Delete/Tab,
  printable chars via `keyTyped` → `insert`. Click/drag → `click\t…` with the
  cell from pixel math. Ctrl+click on a link span → `BrowserUtil.browse`.
  Ctrl+Z/Y/S are *not* handled here — they belong to the IDE (below).
- **Toolbar & status:** an `ActionToolbar` with the webview's button set
  (bold/italic/underline/strike, H1/H2/¶, lists, alignment, font size); the
  same actions register in plugin.xml for Find Action/keymap users. A status
  line shows `Ln, Col · lines · dirty`, as the webview does.
- **Empty file:** a 0-byte `.docx` shows "Create new Word document" in-tab;
  Create mints bytes via `docx_from_markdown("")` and writes them to the file
  (the `mintEmpty` flow from extension.ts).
- **Find (small engine addition):** the webview got Ctrl+F free from VS Code's
  DOM find widget; native gets a find bar (`SearchTextField`) driving a new
  `find\t<query>` dispatch op in `docxwasm::Session` (move caret to the next
  match and select it — the TUI's find logic, natively tested). This is the
  only Rust change in the project and benefits the VS Code editor later.

## Undo, dirty state, save

- **Undo lockstep, IDE-native:** each mutating command (the webview's
  `MUTATING` set) registers a `BasicUndoableAction` (non-document
  `DocumentReference` for the `VirtualFile`) with the project `UndoManager`;
  its undo/redo callbacks dispatch the engine's `undo`/`redo`. One command =
  one undo step, same contract as the VS Code host — Ctrl+Z/Ctrl+Shift+Z hit
  the platform Undo action and route back into the engine.
- **Dirty flag:** the view JSON's `dirty` drives `FileEditor.isModified` (with
  `PROP_MODIFIED` property-change events for the tab asterisk).
- **Save:** `docx_save` bytes written to the `VirtualFile` in a
  `WriteAction`. Wired to Ctrl+S / Save All via a `FileDocumentManager`
  save-hook listener while our editor is selected, plus save-confirmation on
  tab/project close for modified editors. External changes on disk (VFS
  change event while open) prompt reload, which reopens bytes in a fresh
  engine session (the `revert` path).
- **Markdown ⇄ docx:** project-view action on `.md` → sibling `.docx` via
  `docx_from_markdown`, opened in the editor; export action → sibling `.md`
  via `docx_to_md`. Replace-all: two input boxes → `replace\t…` (parity with
  `offxy.replace`).

## Agent ctl bridge

Per open docx tab, `CtlServer` implements ctlcore's wire protocol in Kotlin:
loopback TCP on port 0, one JSON object per line, token
(`SecureRandom`-derived) checked on every request, one reply line per request.
Discovery file `{"instance","port","token","pid"}` named
`docxy-jetbrains-<sanitized basename>-<seq>` in docxy's ctl dir
(`%APPDATA%\docxy\ctl` / `$XDG_CONFIG_HOME/docxy/ctl`); deleted on tab close
and plugin unload; recreated on an advertise-refresh tick if a terminal
docxy's stale-sweep removed it while alive.

- **Doc verbs** (`doc.outline/read/find/replace-range/insert/append`) are
  serviced through the `docx_ctl` wasm entry point that Layer 1 of the
  agent-access plan (`2026-07-17-offxy-agent-access`) adds to docxwasm. The
  JetBrains plugin **depends on that artifact**; if it ships first, doc verbs
  answer `ok:false, error:"not yet implemented"` (ctl-conformant) and light up
  with a wasm refresh. Requests hop to the EDT (`invokeLater` + future,
  per-request timeout), one in flight per document.
- **Host verbs** answered in Kotlin, mirroring the VS Code bridge: `doc.save`
  (WriteAction save so the dirty state clears), `doc.reload` (fresh open),
  `doc.open` (`FileEditorManager.openFile`), `doc.path` (path + dirty + block
  count).
- Mutating ctl verbs register one `UndoableAction` each — agent edits are
  user-undoable, same as keyboard edits.
- **Security posture:** identical to PR #19's — loopback only, per-instance
  random token, discovery files in the user's config dir.

## Packaging & release

- Gradle task `copyWasm` copies `docxwasm.wasm` from
  `../target/wasm32-unknown-unknown/release/` into resources, invoking
  `cargo build -p docxwasm --target wasm32-unknown-unknown --release` if the
  artifact is missing/stale — same artifact, same release train as
  offxy-vscode.
- Runtime dependencies: **Chicory only** (runtime + AOT modules), packaged in
  the plugin zip. Kotlin stdlib comes from the platform.
- `plugin.xml`: since-build 242 (2024.2), no until-build; plugin id
  `dev.yeroo.offxy`, name "Offxy".
- CI: a `gradle buildPlugin` job in the existing workflow, zip attached to
  GitHub releases alongside the vsix. `./gradlew runIde` for manual e2e.
- Marketplace publishing deferred (same stance as the VS Code extension).

## Testing

- **Engine tests (Kotlin, real wasm on Chicory):** open corpus fixtures and
  parse the view JSON; `insert` marks dirty and the text appears; save
  round-trip reopens with the edit; `media` returns bytes for an image doc;
  markdown both ways; undo/redo restores text. These double as the ABI
  parity check against `cargo test -p docxwasm`'s expectations.
- **Benchmark:** the performance-gate measurement on the largest corpus docs,
  reported (not asserted) in CI output.
- **Platform tests (`BasePlatformTestCase`):** provider accepts `.docx`;
  opening a fixture yields a panel with rendered lines; a mutating command
  fires the modified property; platform undo reverses it.
- **Ctl integration:** start `CtlServer` against a real engine session,
  connect over TCP with the ctlcore framing, drive read → edit → undo → save
  verbs (doc verbs once `docx_ctl` lands).
- **Manual e2e:** `runIde`; open corpus documents (styles, tables, lists,
  images), edit/undo/save, reopen in the TUI to confirm fidelity; theme
  switch light/dark; Claude Code editing a live tab through `docxy --mcp`;
  terminal pane + IDE tab disambiguated by `target`.

## Out of scope (v1)

- The xlsx grid editor (designed-for: a second provider registration painting
  `gridwasm`'s viewport JSON natively — the seam is the provider/spec table,
  as in extension.ts).
- Compose/Jewel UI; JCEF in any form.
- Marketplace publishing.
- WMF/EMF/SVG rasterization on the JVM (fallback box, parity with webview).
- Page view/pagination — continuous flow, same fidelity target as VS Code.
- IME composition polish (basic `keyTyped` input only) and RTL/complex-script
  rendering beyond what the grid renderer provides.
- JetBrains remote development / Gateway thin-client support.
