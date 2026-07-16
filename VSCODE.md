# Docxy in VS Code — design

This document covers the **VS Code extension** side of the workspace: the
`docxwasm` bridge crate and the `docxy-vscode` extension that together open and
edit Word `.docx` files inside a VS Code editor tab. For the terminal app see the
[README](README.md); for the spreadsheet and project sides see
[SPREADSHEET.md](SPREADSHEET.md) and [PROJECT.md](PROJECT.md).

## The idea

`docxy` renders a `.docx` faithfully onto a **character grid** in the terminal —
not a pixel-perfect Word page, but a readable, editable view of the real
document. A VS Code editor tab is also a character grid. So the same render
engine that paints the terminal can paint an editor tab: the document reads like
text in your editor, at the editor's own font and size, honoring your color
theme — and *without a ribbon*, because VS Code already has the command palette,
the editor title bar, and keybindings.

The enabling fact: [`docxcore`](docxcore) — the whole DOCX engine (its own ZIP,
DEFLATE, XML, document model, renderer, editor, and **lossless** serializer) — is
pure `std` with no third-party crates. That compiles straight to
`wasm32-unknown-unknown` and runs inside a webview. There is no JavaScript docx
library involved; the same Rust code that backs the terminal app backs the
extension.

## Why this is worth doing

The existing `.docx` extensions fall into two camps, and both leave a gap:

- **Read-only viewers** (mammoth.js, docx-preview) render to HTML and can't edit.
- **HTML-editing** extensions open a docx, convert it to HTML, let you edit the
  HTML, then *re-serialize HTML back to docx* — which silently degrades styles,
  numbering, tables, and headers on every save.

`docxcore` edits the **real OOXML model** and preserves every unmodeled part of
the original package byte-for-faithful (bookmarks, fields, content controls,
section properties, …). A document opened, edited, and saved comes back
structurally intact. That lossless round-trip — the terminal app's headline
feature — is exactly what the crowded extension field lacks, and it carries into
the editor tab unchanged.

## Crates & pieces

| Piece | Language | Responsibility |
|-------|----------|----------------|
| [`docxcore`](docxcore) | Rust | the DOCX engine (unchanged; shared with `docxy`) |
| [`docxwasm`](docxwasm) | Rust `cdylib` | the hand-written WebAssembly ABI over `docxcore` |
| [`docxy-vscode/src/extension.ts`](docxy-vscode) | TypeScript | the extension host: a binary `CustomEditorProvider` |
| [`docxy-vscode/media/webview.js`](docxy-vscode) | JavaScript | the webview: wasm loader, grid painter, input handling |

## The wasm ABI

`docxwasm` deliberately uses **no `wasm-bindgen`** — matching the project's
from-scratch ethos and keeping the artifact tiny (~650 KB) and auditable. The
seam is a handful of C-ABI exports:

```
docx_alloc(len) -> ptr          docx_free(ptr, len)
docx_open(ptr, len) -> handle   docx_close(handle)
docx_render(handle) -> result
docx_cmd(handle, ptr, len) -> result
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

## Verification

- `cargo test -p docxwasm` exercises open → render → edit → **save → reopen** in
  plain Rust (the round-trip proves the edit survives lossless serialization).
- The wasm is exercised end-to-end in a JS runtime (open a real `.docx`, render,
  type, format, undo, save, reopen) and the webview client is smoke-tested under
  jsdom against the real `.wasm` — confirming the message protocol, the dirty/
  undo bookkeeping, and that `docx_save` returns valid ZIP (`PK`) `.docx` bytes.

## Status & next steps

Working today: faithful rendering (runs, headings, lists, tables, links),
editing (typing, navigation, selection, B/I/U, copy/cut/paste), a no-ribbon
formatting toolbar + command palette (headings, lists, alignment, font size),
**find** (VS Code's find widget over the rendered text) and **replace** (the
engine's replace-all), and native dirty / undo-redo / save / Save As / backup
with lossless round-trip.

Next, roughly in order:

1. **Images** — the terminal app overlays real pixels on placeholder boxes; the
   webview can render embedded media inline from the package's media parts.
2. **Markdown ↔ docx** — surface `docxcore`'s conversion as an editor action.
3. **Color / font pickers** over the `color` and `fontsize` bridge commands.
