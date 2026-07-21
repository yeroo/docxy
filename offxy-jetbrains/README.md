# Offxy for JetBrains IDEs

A **native** Word `.docx` editor for IntelliJ-platform IDEs — no webview, no
Microsoft Office, no per-platform native code. The document engine is the
dependency-free [docxy](https://github.com/yeroo/docxy) Rust core (parse →
render → edit → **lossless save**), compiled to WebAssembly and executed on
the JVM by [Chicory](https://chicory.dev). The same `docxwasm.wasm` artifact
also powers the [VS Code extension](../offxy-vscode) and shares its release
train.

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

## AI assistants (agent control surface)

Every open tab advertises as `docxy-jetbrains-<name>-<n>` on docxy's
[control surface](../docs/agent-control.md) — the same loopback protocol the
terminal apps and VS Code tabs use. Any `docxy --mcp` server sees IDE tabs
with zero configuration:

```sh
claude mcp add docxy -- docxy --mcp     # Claude Code
```

JetBrains AI Assistant / Junie: add `docxy --mcp` as an MCP server in its
settings. Agent edits repaint the tab live and are one Ctrl+Z away from
undone. (Read/edit verbs light up with the `docx_ctl` engine build from the
agent-access plan; `doc.path`/`doc.save`/`doc.reload`/`doc.open` work today.)

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
  reconciles.
- No `.xlsx` editor yet — designed for, planned as a second provider over
  the `gridwasm` viewport protocol.
