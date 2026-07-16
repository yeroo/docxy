# Docxy for VS Code

Open, read, and **edit** Microsoft Word `.docx` files right in a VS Code editor
tab — a faithful, monospace-grid view that uses the editor's own font, size, and
theme colors, so a Word document reads like text in your editor instead of an
embedded Word canvas. **No ribbon**: the keyboard and command palette drive
everything, exactly like editing code.

It's powered by [`docxwasm`](../docxwasm) — a WebAssembly build of the
dependency-free [`docxcore`](../docxcore) engine that also backs the `docxy`
terminal app. The *entire* pipeline — parse → render → edit → **lossless save** —
runs in the webview as one ~650 KB `.wasm`. There is no JavaScript docx library,
no server, and no external process.

## Why it's different

Most `.docx` extensions either only *view* (mammoth.js / docx-preview, read-only)
or *edit through HTML* (open → HTML → re-serialize), which quietly degrades
styles, numbering, tables, and headers on save. Docxy edits the **real OOXML
model** and preserves every unmodeled part of the original package byte-for-
faithful — so a document you open, edit, and save comes back structurally intact.
That lossless round-trip is `docxcore`'s core guarantee, carried straight into
the editor tab.

## Build

Prerequisites: a Rust toolchain with the wasm target, and Node ≥ 18.

```sh
rustup target add wasm32-unknown-unknown
cd docxy-vscode
npm install
npm run build          # builds the wasm, copies it into media/, bundles the extension
```

`npm run build` runs two steps you can also invoke separately:

- `npm run build:wasm` — `cargo build -p docxwasm --target wasm32-unknown-unknown
  --release` and copies the artifact to `media/docxwasm.wasm`.
- `npm run build:ext` — bundles `src/extension.ts` to `out/extension.js` with
  esbuild.

Then press <kbd>F5</kbd> in VS Code (Run ▸ Start Debugging) to launch an Extension
Development Host, and open any `.docx` file.

## What works

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
- **Native integration** — the VS Code **dirty indicator**, **undo/redo**
  (<kbd>Ctrl/Cmd</kbd>+<kbd>Z</kbd>/<kbd>Y</kbd>), **Save**, **Save As**, and
  **hot-exit backup** all work, driven through the standard `CustomEditor` edit
  events. Edits stay lockstep with the wasm engine's own undo stack.
- **Lossless save** — the original package is preserved; only the document part
  is re-serialized from the edited model.

## Architecture

| Layer | Where | Role |
|-------|-------|------|
| `docxcore` | Rust crate | the DOCX engine (parse/model/render/edit/save) |
| `docxwasm` | Rust `cdylib` → `.wasm` | a hand-written C-ABI seam (no wasm-bindgen): `docx_open/render/cmd/save/close` over length-prefixed buffers |
| `src/extension.ts` | extension host | binary `CustomEditorProvider`: opens files, relays undo/redo & save, writes bytes; owns dirty state |
| `media/webview.js` | webview | loads the wasm, paints the grid, captures keyboard/mouse, and serializes on save |

The webview owns the live editing session (low latency, no host round-trip per
keystroke); the host owns the file lifecycle. They talk over a small
`postMessage` protocol — the webview reports each mutating edit so VS Code can
light the dirty dot and route undo/redo back into the wasm editor.

See [../VSCODE.md](../VSCODE.md) for the full design.
