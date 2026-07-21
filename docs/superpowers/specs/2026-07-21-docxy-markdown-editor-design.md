# docxy markdown WYSIWYG editor — design

**Goal:** Open a `.md` file in docxy's editor webview and edit it WYSIWYG, saving
back as **markdown** — never producing a `.docx` file. Backed by a faithful
markdown round-trip so saving doesn't mangle the source.

**Basis:** conversational request (2026-07-21). Reuses the existing docx webview
editor (`offxy-vscode/media/webview.js`, `docxwasm`) and the engine wrappers
`markdownToDocx`/`docxToMarkdown` (`offxy-vscode/src/engine.ts` →
`docx_from_markdown`/`docx_to_markdown`).

**Key transport fact (drove the design):** `markdownToDocx` serializes markdown
through `save_package` to **docx bytes** in memory, which the webview `docx_open`s.
So the editor's working model is docx-in-wasm; the on-disk file stays markdown.
Anything that must reach the webview has to survive that md→docx-bytes hop —
which is why task-list state is kept as **literal text**, not a model field
(a model checkbox would need Word/OOXML representation to survive the hop).

## Two phases

### Phase 1 — faithful markdown round-trip (`docxcore`, no extension work)

`from_markdown → to_markdown` must be stable and idempotent for the constructs
docxy supports, so an open→edit→save cycle doesn't rewrite unrelated content.
Three fixes in `docxcore/src/markdown.rs`:

1. **List-item continuation.** The list loop (`from_markdown`, ~line 483) only
   consumes lines that `list_item()` recognizes; an indented/soft-wrapped
   continuation line under an item currently falls out and becomes a separate
   paragraph (with a spurious blank line), breaking the list. Fix: within the
   list loop, a non-marker line that is indented under (or a lazy continuation
   of) the current item appends to that item's text instead of ending the list.

2. **Task lists round-trip as literal text.** `escape_inline` (~line 346)
   escapes bare `[` and `]`, so `- [ ]` → `- \[ \]`. Brackets are only special
   in link/image syntax (`[text](url)` / `![alt](src)` / `[ref][id]`), not on
   their own. Fix: stop escaping bare `[`/`]` (escape only where they would
   actually form a link — or rely on the fact that a lone `[ ]` never does).
   Result: `- [ ]` / `- [x]` round-trip byte-faithfully as literal list text.
   No model field, no OOXML — survives the md→docx-bytes→md transport unchanged.

3. **Escaping/normalization audit + committed round-trip corpus.** Audit
   `escape_inline` for other gratuitous escapes; add a committed test that
   asserts `md → from_markdown → to_markdown` is **idempotent** (a second pass
   equals the first) across a representative corpus: ATX headings, `**bold**` /
   `*italic*` / `` `code` `` / `~~strike~~`, links, nested bullet + ordered
   lists **with soft-wrapped continuations**, task lists (`- [ ]`/`- [x]`),
   pipe tables, fenced code blocks (with language), blockquotes, thematic
   breaks, and inline/display math. The corpus is the acceptance gate for
   "faithful."

   **Accepted, documented limitation:** hard-wrapped prose paragraphs are
   re-emitted one line per paragraph (CommonMark-canonical) — a first save
   unwraps them. This is inherent to model-based serialization (the model has
   no source line breaks) and is out of scope to preserve; it's a one-time
   normalization, then stable.

### Phase 2 — the `.md` WYSIWYG editor (extension)

- **New custom editor** `offxy.markdownEditor` for `*.md`, registered
  **opt-in** (`priority: "option"`): `.md` still opens as plain text by default;
  the user picks **Reopen Editor With → Docxy Markdown** (and a command
  `offxy.openMarkdownEditor`). Does not hijack normal markdown text editing.
- **Reuses the docx webview editor.** Load: read the `.md` text →
  `markdownToDocx` (in-memory docx bytes, no file) → webview `open`. Save:
  webview `docx_save` bytes → `docxToMarkdown` → write the `.md` file. Markdown
  is the on-disk format throughout; **no `.docx` is ever written.** This mirrors
  the existing docx `OffxyEditorProvider` load/save, swapping the two byte
  conversions in the provider's read/write for the markdown ones (the provider
  already has a `mintEmpty: markdownToDocx(ctx,'')` precedent).
- **Constrained editing surface (markdown mode).** The webview runs in a
  markdown-flavored mode where only markdown-representable formatting is
  offered — bold, italic, strike, headings, bullet/ordered lists, blockquote,
  inline code / code block, links, tables. Non-representable commands (font
  size, text color, arbitrary paragraph alignment) are **hidden/disabled** so
  the user can't create formatting that silently drops on save. The webview
  learns its mode from an init flag the provider passes.
- **Checkbox rendering (presentation only).** In markdown mode, the webview
  renders a list item whose text starts with `[ ] ` / `[x] ` / `[X] ` as a
  ☐ / ☑ glyph instead of the literal brackets. The underlying paragraph text is
  unchanged, so `docx_save → docxToMarkdown` still emits `- [ ] …` faithfully.
  (Optional: clicking the glyph toggles `[ ]`↔`[x]` by editing that text.)

## Error handling

- A `.md` that fails to parse still opens (worst case as one paragraph of its
  raw text) — `from_markdown` is total, never errors.
- Save writes the sibling `.md` via the provider's normal save pipeline (dirty
  flag, backup) — same as the docx editor, just markdown bytes.
- No new agent/ctl/MCP surface; the ctl bridge and MCP tools are untouched.

## Testing

- **Phase 1:** the committed idempotent round-trip corpus (unit tests in
  `docxcore`), plus targeted tests: a task-list survives round-trip byte-exact;
  a nested list with soft-wrapped continuations keeps its structure;
  `escape_inline` no longer escapes bare brackets but still escapes real
  markdown metacharacters (`*` `` ` `` `~` `|` `\`).
- **Phase 2:** extend the extension's webview/e2e checks — open a fixture `.md`
  through the markdown editor, edit, save, and assert the written `.md` matches
  the expected canonical markdown; a hand-authored `.md` with task lists +
  nested lists survives open→(no edit)→save with **no structural change** (only
  the documented one-time reflow). Confirm no `.docx` file is created. Full
  gates (fmt/clippy/tests, wasm build, typecheck/build/package/install).

## Out of scope

- Hard-wrap / source-line-break preservation.
- A real docx/OOXML checkbox model (task state stays literal text).
- Any agent/MCP change; the xlsx grid; making the `.md` editor the default (it's
  opt-in).
- Round-tripping markdown constructs docxy's engine doesn't already model.
