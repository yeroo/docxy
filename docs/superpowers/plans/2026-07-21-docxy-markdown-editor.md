# docxy Markdown WYSIWYG Editor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix docxy's markdown round-trip to be faithful, then add an opt-in `.md` WYSIWYG editor that opens/edits markdown in the docx webview and saves back as markdown (no `.docx` file).

**Architecture:** Phase 1 fixes `docxcore/src/markdown.rs` (list-item continuation merging; stop escaping bare `[`/`]` so task lists survive; a committed idempotent round-trip corpus). Phase 2 adds a `.md` custom editor in the extension that reuses the existing docx webview, converting md→docx-bytes on load and docx-bytes→md on save (`markdownToDocx`/`docxToMarkdown`), plus a webview markdown mode that constrains the toolbar and renders `[ ]`/`[x]` as checkboxes (presentation only).

**Tech Stack:** Rust (docxcore, docxwasm), TypeScript (`offxy-vscode/src`), plain JS webview (`offxy-vscode/media/webview.js`), VS Code custom-editor API.

**Spec:** `docs/superpowers/specs/2026-07-21-docxy-markdown-editor-design.md` — required reading.
**Branch:** `claude/md-editor` (off `main`).

## Global Constraints

- No version bumps (workspace 0.4.0 / extension 0.3.0); no new dependencies; docxcore stays std-only.
- Existing tests pass unmodified; no agent/ctl/MCP surface change (the ctl bridge, the 56 MCP tools, and `test:mcp-parity` are untouched).
- The `.md` editor never writes a `.docx` file; markdown is the on-disk format throughout. It is **opt-in** (`priority: "option"`) — `.md` still opens as plain text by default.
- Task-list state stays **literal text** (`[ ] `/`[x] ` in the paragraph), never a model/OOXML field — so it survives the md→docx-bytes→md transport unchanged.
- After Phase 1, the media wasm MUST be rebuilt (`cd offxy-vscode && npm run build`) so the editor's `markdownToDocx`/`docxToMarkdown` use the fixed converters.
- **Windows agent shell quirks:** every cargo command runs in bash with
  ```bash
  export PATH="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
  export RUSTC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustc.exe"
  export RUSTDOC="$USERPROFILE/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin/rustdoc.exe"
  ```
  Never pipe exit-code-bearing commands through `tail`. Packaging via `npx --yes @vscode/vsce@latest package --no-dependencies`.

---

### Task 1: task lists round-trip — stop escaping bare brackets

**Files:**
- Modify: `docxcore/src/markdown.rs` (`escape_inline`, ~line 346)

**Interfaces:**
- Consumes: nothing new.
- Produces: `escape_inline` no longer escapes `[`/`]`; `to_markdown` therefore emits `- [ ]` (not `- \[ \]`).

- [ ] **Step 1: Write the failing tests** in `docxcore/src/markdown.rs`'s test module:

```rust
#[test]
fn task_list_round_trips_as_literal_text() {
    let src = "- [ ] todo\n- [x] done\n";
    let out = to_markdown(&from_markdown(src));
    assert!(out.contains("- [ ] todo"), "unchecked task survives: {out:?}");
    assert!(out.contains("- [x] done"), "checked task survives: {out:?}");
    assert!(!out.contains("\\["), "brackets are not escaped: {out:?}");
}

#[test]
fn escape_inline_still_escapes_real_metacharacters_but_not_brackets() {
    // `*` `` ` `` `~` `|` `\` still escape; `[` `]` do not.
    let e = escape_inline("a*b`c~d|e[f]g\\h");
    assert!(e.contains("\\*") && e.contains("\\`") && e.contains("\\~") && e.contains("\\|") && e.contains("\\\\"));
    assert!(!e.contains("\\[") && !e.contains("\\]"), "brackets not escaped: {e:?}");
}
```

- [ ] **Step 2: Run to verify FAIL** — `cargo test -p docxcore markdown` → both fail (brackets currently escaped).
- [ ] **Step 3: Implement.** In `escape_inline`, drop `[` and `]` from the match:

```rust
fn escape_inline(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for c in text.chars() {
        // `[`/`]` are only special in link/image syntax, which `to_markdown`
        // emits through its own link path — not via this function. Escaping
        // bare brackets mangles task-list items (`- [ ]` -> `- \[ \]`) and other
        // literal bracket text, so they are left alone.
        if matches!(c, '\\' | '*' | '`' | '~' | '|') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}
```

- [ ] **Step 4: Run to verify PASS** — `cargo test -p docxcore` (all green, existing tests unmodified — confirm no link test regressed; links are emitted by the link path, not `escape_inline`).
- [ ] **Step 5: Gates + commit** — `cargo fmt --all && cargo clippy -p docxcore --all-targets -- -D warnings`; `git add docxcore && git commit -m "docxcore/markdown: don't escape bare brackets so task lists round-trip"`

---

### Task 2: list-item soft-wrapped continuation lines merge into the item

**Files:**
- Modify: `docxcore/src/markdown.rs` (the list loop in `from_markdown`, ~line 481)

**Interfaces:**
- Consumes: existing `list_item()`, `list_para()`, `starts_block()`.
- Produces: a list item's soft-wrapped/lazily-continued lines are folded into that item's text (no spurious separate paragraph / blank line).

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn list_item_continuation_lines_merge_into_the_item() {
    // The second line is an indented soft-wrap of the first item, not a new block.
    let src = "- first line\n  still the first item\n- second item\n";
    let doc = from_markdown(src);
    // Exactly two list paragraphs, and the first carries both lines' text.
    let paras: Vec<&docxcore::model::Paragraph> = doc.body.iter()
        .filter_map(|b| match b { docxcore::model::Block::Paragraph(p) => Some(p), _ => None })
        .filter(|p| p.props.num_id.is_some())
        .collect();
    assert_eq!(paras.len(), 2, "two list items, not three blocks");
    assert_eq!(paras[0].plain_text(), "first line still the first item");
    // And it round-trips without inserting a blank line inside the list.
    let out = to_markdown(&doc);
    assert!(!out.contains("- first line\n\n"), "no spurious blank inside the list: {out:?}");
}
```

- [ ] **Step 2: Run to verify FAIL** — `cargo test -p docxcore list_item_continuation` → fails (today the continuation splits into a 3rd block).
- [ ] **Step 3: Implement.** Replace the list-consuming loop (currently ~lines 482–493) with one that gathers continuations per item:

```rust
        // List (consecutive items; each item may span soft-wrapped/continued
        // lines that are not themselves new list markers or new blocks).
        if list_item(line).is_some() {
            while i < lines.len() {
                let Some((ilvl, ordered, first)) = list_item(lines[i]) else { break };
                i += 1;
                let mut text = first.to_string();
                // Fold in continuation lines belonging to THIS item.
                while i < lines.len() {
                    let l = lines[i];
                    let t = l.trim();
                    if t.is_empty()
                        || list_item(l).is_some()
                        || starts_block(l, lines.get(i + 1).copied())
                    {
                        break;
                    }
                    text.push(' ');
                    text.push_str(t);
                    i += 1;
                }
                body.push(list_para(ilvl, ordered, &text));
            }
            continue;
        }
```

- [ ] **Step 4: Run to verify PASS** — `cargo test -p docxcore` (all green).
- [ ] **Step 5: Gates + commit** — fmt/clippy; `git add docxcore && git commit -m "docxcore/markdown: fold soft-wrapped continuation lines into their list item"`

---

### Task 3: committed idempotent round-trip corpus (Phase-1 acceptance gate) + rebuild wasm

**Files:**
- Modify: `docxcore/src/markdown.rs` (test module)

**Interfaces:**
- Consumes: Tasks 1–2's fixes.
- Produces: the acceptance test proving `from_markdown → to_markdown` is idempotent across the corpus; the rebuilt media wasm for Phase 2.

- [ ] **Step 1: Write the corpus idempotency test:**

```rust
#[test]
fn markdown_round_trip_is_idempotent_over_the_corpus() {
    // Written in docxy's own canonical output style (one line per paragraph,
    // two-space list nesting) so the FIRST pass is already a fixed point.
    let corpus = "\
# Heading 1

## Heading 2

A paragraph with **bold**, *italic*, ~~strike~~, `code`, and a [link](https://x).

- bullet one
- bullet two continued text
  - nested bullet
- [ ] todo
- [x] done

1. first
2. second

> a quote

```text
code block
```

| a | b |
| --- | --- |
| 1 | 2 |

---
";
    let once = to_markdown(&from_markdown(corpus));
    let twice = to_markdown(&from_markdown(&once));
    assert_eq!(once, twice, "second pass must equal the first (idempotent)");
    // Spot-check the constructs survive the FIRST pass.
    for needle in ["# Heading 1", "**bold**", "~~strike~~", "`code`", "[link](https://x)",
                   "- [ ] todo", "- [x] done", "1. first", "> a quote", "| a | b |", "---"] {
        assert!(once.contains(needle), "corpus lost {needle:?}:\n{once}");
    }
}
```

(If a construct legitimately can't be a first-pass fixed point — e.g. the exact
code-fence info string — adjust the corpus to docxy's canonical emission and
note it in the report; the REQUIRED invariant is the `once == twice`
idempotency, which is what protects the editor's save.)

- [ ] **Step 2: Run** — `cargo test -p docxcore markdown_round_trip_is_idempotent` → PASS (green after Tasks 1–2). If it fails, the diff between `once` and `twice` names the remaining non-idempotent construct; fix it in `markdown.rs` (do not weaken the test).
- [ ] **Step 3: Rebuild the media wasm** so Phase 2 uses the fixed converters:

```bash
cd offxy-vscode && npm run build
```
Expected: builds `docxwasm.wasm`/`gridwasm.wasm` into `media/`, exit 0.

- [ ] **Step 4: Gates + commit** — `cargo fmt --all --check`; `cargo clippy -p docxcore --all-targets -- -D warnings`; `git add docxcore offxy-vscode/media/docxwasm.wasm offxy-vscode/media/gridwasm.wasm && git commit -m "docxcore/markdown: idempotent round-trip corpus; rebuild media wasm"` (include the wasm only if the repo tracks it — check `git status`; if `media/*.wasm` is gitignored, drop it from the add and note the rebuild in the report).

---

### Task 4: the `.md` custom editor — load/save through markdown, no docx file

**Files:**
- Modify: `offxy-vscode/src/extension.ts` (the `EditorSpec` interface + `EDITORS` + the provider read/save paths + registration), `offxy-vscode/package.json` (`contributes.customEditors`)

**Interfaces:**
- Consumes: `markdownToDocx(ctx, mdString)` and `docxToMarkdown(ctx, docxBytes)` from `./engine` (already imported at extension.ts:14).
- Produces: a `offxy.markdownEditor` custom editor for `*.md`, opt-in; markdown is read as text→docx-bytes for the webview and written back as docx-bytes→markdown text.

- [ ] **Step 1: Add byte-transform hooks to `EditorSpec`.** In the `EditorSpec` interface (extension.ts:28-56) add two optional transforms (default identity):

```ts
  /** Convert on-disk file bytes into the bytes the webview `docx_open`s.
   *  Default identity (the file already holds the editor's native bytes).
   *  The markdown editor uses this to turn `.md` text into in-memory docx
   *  bytes via `markdownToDocx` — no `.docx` file is ever written. */
  fromFileBytes?: (raw: Uint8Array, ctx: vscode.ExtensionContext) => Promise<Uint8Array>;
  /** Convert the webview's saved bytes (docx) back to the on-disk file bytes.
   *  Default identity. The markdown editor uses this to turn the edited docx
   *  model back into markdown text via `docxToMarkdown`. */
  toFileBytes?: (webviewBytes: Uint8Array, ctx: vscode.ExtensionContext) => Promise<Uint8Array>;
  /** When true, the webview runs in markdown mode (constrained toolbar +
   *  checkbox rendering); passed to the webview via `window.__OFFXY__`. */
  markdown?: boolean;
```

- [ ] **Step 2: Add the markdown editor spec** to `EDITORS` (after the docx entry). It reuses the docx webview/wasm and the docx `ctl` block (the tab's in-wasm model is docx), and sets the transforms + mode:

```ts
  {
    viewType: 'offxy.markdownEditor',
    label: 'Markdown document',
    script: 'webview.js',
    style: 'webview.css',
    wasm: 'docxwasm.wasm',
    markdown: true,
    emptyPrompt:
      '“{name}” is empty. Start a new Markdown document here?',
    // Empty md file → an empty docx model for the webview.
    mintEmpty: (ctx) => markdownToDocx(ctx, ''),
    // `.md` text on disk  <->  in-memory docx bytes for the webview.
    fromFileBytes: (raw, ctx) => markdownToDocx(ctx, new TextDecoder().decode(raw)),
    toFileBytes: async (bytes, ctx) => new TextEncoder().encode(await docxToMarkdown(ctx, bytes)),
    ctl: EDITORS[0].ctl, // same docxy control surface; agent edits save as .md
  },
```

(If `EDITORS[0].ctl`'s `Set`s must not be shared by reference, clone them: `{ ...EDITORS[0].ctl, wasmVerbs: new Set(EDITORS[0].ctl.wasmVerbs), mutatingVerbs: new Set(EDITORS[0].ctl.mutatingVerbs) }`. Prefer cloning to avoid cross-editor mutation.)

- [ ] **Step 3: Apply the transforms in the provider.** In `openCustomDocument` (extension.ts:555) where the file bytes are read (`vscode.workspace.fs.readFile(uri)`, ~line 559), pass them through `spec.fromFileBytes` when present:

```ts
    const raw = await vscode.workspace.fs.readFile(uri);
    const bytes = this.spec.fromFileBytes ? await this.spec.fromFileBytes(raw, this.context) : raw;
    // …use `bytes` where the raw bytes were used before (initialContent / webview open)…
```
And in the save path (the `writeFile` that persists the webview's `getBytes` result — around extension.ts:1048 in `saveCustomDocumentAs`/the shared save helper), apply `spec.toFileBytes`:

```ts
    const out = this.spec.toFileBytes ? await this.spec.toFileBytes(bytes, this.context) : bytes;
    await vscode.workspace.fs.writeFile(target, out);
```
Find EVERY place the webview bytes are written to disk (the normal save and save-as) and route them through `toFileBytes` — read the provider's save methods (`saveCustomDocument` ~1031, `saveCustomDocumentAs` ~1038, `backupCustomDocument` ~1059) and apply consistently. Empty-mint (`mintEmpty`) already returns docx bytes and is written as the file's initial content — for the markdown editor, a fresh empty file should be written as EMPTY markdown, so ensure the empty-create path writes `toFileBytes(mintEmpty())` (i.e. empty md), not the docx bytes. Verify the create flow writes `.md`, not docx.

- [ ] **Step 4: Register the editor** in `package.json` `contributes.customEditors` (after the docx entry), opt-in:

```json
{
  "viewType": "offxy.markdownEditor",
  "displayName": "Docxy Markdown",
  "selector": [{ "filenamePattern": "*.md" }],
  "priority": "option"
}
```

- [ ] **Step 5: Manual sanity via a script test.** Add a committed Node check `offxy-vscode/media/markdown-roundtrip.test.mjs` that loads the rebuilt `docxwasm.wasm` and asserts the editor's transport is faithful end-to-end at the wasm level (this is what the provider relies on):

```js
// md -> docx bytes -> md must preserve the corpus (mirrors fromFileBytes/toFileBytes).
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
const here = dirname(fileURLToPath(import.meta.url));
const ex = (await WebAssembly.instantiate(readFileSync(join(here, 'docxwasm.wasm')), {})).instance.exports;
const enc = new TextEncoder(), dec = new TextDecoder();
function call(fn, text) {
  const u8 = enc.encode(text); const p = ex.docx_alloc(u8.length); new Uint8Array(ex.memory.buffer).set(u8, p);
  const r = fn(p, u8.length); ex.docx_free(p, u8.length);
  const m = new Uint8Array(ex.memory.buffer); const len = m[r]|(m[r+1]<<8)|(m[r+2]<<16)|(m[r+3]<<24);
  const out = m.slice(r+4, r+4+len); ex.docx_free(r, 4+len); return out;
}
const md = '# Title\n\n- [ ] todo\n- [x] done\n\n1. one\n2. two continued\n';
const docxBytes = call(ex.docx_from_markdown, md);
// docx_to_markdown takes docx bytes, returns md text
const back = dec.decode(call(ex.docx_to_markdown, dec.decode(docxBytes) /* see ABI */));
assert.ok(back.includes('- [ ] todo') && back.includes('- [x] done'), 'task lists survive: ' + back);
console.log('markdown transport OK');
```
(Adjust to the real `docx_to_markdown` ABI — it takes docx **bytes** not text; read `docxwasm/src/lib.rs:206` and `media/webview.js`'s marshalling for the exact alloc/len/result convention, and mirror it. The point of the test: prove `md → docx_from_markdown → docx_to_markdown → md` keeps task lists and list structure through the exact transport the editor uses.) Wire it as `npm run test:md-roundtrip` in `package.json`.

- [ ] **Step 6: Gates + commit** — `cd offxy-vscode && npm run typecheck && npm run build && npm run test:md-roundtrip`; `git add offxy-vscode/src/extension.ts offxy-vscode/package.json offxy-vscode/media/markdown-roundtrip.test.mjs && git commit -m "offxy: opt-in .md WYSIWYG editor — load/save through markdown, no docx file"`

---

### Task 5: webview markdown mode — constrained toolbar + checkbox rendering

**Files:**
- Modify: `offxy-vscode/media/webview.js` (`buildToolbar`, `render`), `offxy-vscode/src/extension.ts` (`html()` — pass the `markdown` flag into `window.__OFFXY__`)

**Interfaces:**
- Consumes: `window.__OFFXY__.markdown` (added by the provider's `html()`).
- Produces: in markdown mode the toolbar hides non-markdown formatting; list items with a `[ ] `/`[x] ` prefix render as ☐/☑.

- [ ] **Step 1: Pass the mode flag.** In the provider's `html()` (extension.ts:1080), where `window.__OFFXY__ = {…}` is emitted, add `markdown: <spec.markdown ?? false>`:

```ts
    // inside the injected script object:
    // window.__OFFXY__ = { wasmUri: "…", markdown: ${this.spec.markdown ? 'true' : 'false'} };
```
Read the current `html()` to match its exact string-building style.

- [ ] **Step 2: Constrain the toolbar.** In `webview.js` `buildToolbar()` (~line 425), when `window.__OFFXY__.markdown` is true, omit the buttons markdown can't represent — underline, alignment (L/C/R), and font size (A−/A+) — keeping Bold, Italic, Strikethrough, H1/H2/Normal, bullet/numbered list. Concretely, filter the `buttons` array by a markdown-allowed set when the flag is set:

```js
    const MD = window.__OFFXY__ && window.__OFFXY__.markdown;
    // ops markdown can't express — dropped from the toolbar in markdown mode
    const MD_HIDDEN = new Set(['underline', 'align\tleft', 'align\tcenter', 'align\tright', 'fontsize\t-2', 'fontsize\t2']);
    const buttons = [ /* …existing list… */ ].filter((b) => !(MD && b[1] && MD_HIDDEN.has(b[1])));
```
(Also drop now-orphaned separators if two land adjacent — a small cleanup pass over the filtered array.)

- [ ] **Step 3: Checkbox rendering (presentation only).** In `render()` (~line 84), when building a list item's HTML and `window.__OFFXY__.markdown` is true, if the item's leading text is `[ ] `/`[x] `/`[X] `, replace that prefix with a `☐ `/`☑ ` glyph in the DISPLAYED text (do not alter the underlying model — the text the webview holds and returns via `docx_save` must stay `[ ] …` so the markdown save is faithful). Simplest: transform only the rendered text node, e.g. a display helper `mdCheckbox(text)` that returns `text.replace(/^\[ \] /, '☐ ').replace(/^\[[xX]\] /, '☑ ')`, applied where a list paragraph's text is written to the DOM. Verify the transform is display-only by re-reading the roundtrip test (save still yields `[ ]`).

- [ ] **Step 4: Verify** — `cd offxy-vscode && npm run typecheck && npm run build && npm run test:md-roundtrip` (still faithful — the checkbox is display-only). Manually reason: a `.md` with `- [ ] x` opens showing ☐ x; save writes `- [ ] x`.
- [ ] **Step 5: Commit** — `git add offxy-vscode/media/webview.js offxy-vscode/src/extension.ts && git commit -m "offxy: markdown-mode webview — constrained toolbar, checkbox rendering"`

---

### Task 6: docs + full verification

**Files:**
- Modify: `offxy-vscode/README.md`, `offxy-vscode/CHANGELOG.md`

- [ ] **Step 1: Docs.** README: a short "Edit Markdown files (WYSIWYG)" section — open a `.md` via **Reopen Editor With → Docxy Markdown**; it edits in the docx view and saves back as markdown (never a `.docx`); note the one-time reflow to canonical markdown on first save and that task lists / nested lists are preserved. CHANGELOG: an entry for the faithful markdown round-trip + the opt-in `.md` editor.
- [ ] **Step 2: Full gates** (report exit codes): `cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test -p docxcore -p docxy -p docxwasm`; wasm32 release build of docxwasm; `cd offxy-vscode && npm run typecheck && npm run build && npm run test:md-roundtrip && npm run test:mcp-parity` (still 56/56 — no MCP change) `&& npm run test:grid-layout` (grid untouched); vsce package (vsix includes `webview.js`/`docxwasm.wasm`); install.
- [ ] **Step 3: Manual e2e note for Boris** (in the report, not a doc): open a `.md` with headings + a task list + a nested list via "Reopen With → Docxy Markdown"; confirm it renders (checkboxes as ☐/☑), the toolbar omits underline/align/font-size; edit (bold a word, add a list item); save; reopen the `.md` as plain text and confirm the markdown is intact and faithful (only the documented one-time reflow), and that no `.docx` file was created next to it.
- [ ] **Step 4: Commit** — `git add offxy-vscode/README.md offxy-vscode/CHANGELOG.md && git commit -m "offxy: document the markdown editor + faithful round-trip"`

## Self-Review Notes

- Spec coverage: Phase-1 fixes → Tasks 1 (task-list escape), 2 (list continuation), 3 (idempotent corpus + wasm rebuild). Phase-2 → Task 4 (editor load/save transforms + opt-in registration, no docx file), Task 5 (constrained toolbar + checkbox render), Task 6 (docs + verification). The "no docx file", "opt-in", "literal-text task state", and "no MCP change" constraints are pinned in Global Constraints and re-checked in Tasks 4/6.
- Type consistency: `fromFileBytes`/`toFileBytes`/`markdown` on `EditorSpec` are defined in Task 4 and consumed in Tasks 4–5; `markdownToDocx(ctx, string): Promise<Uint8Array>` and `docxToMarkdown(ctx, Uint8Array): Promise<string>` are the existing engine signatures (verify against `engine.ts` before use).
- Empirical facts the implementer must verify and report: the exact `docx_to_markdown` wasm ABI (bytes-in) for the roundtrip test (Task 4 Step 5); whether `media/*.wasm` is git-tracked (Task 3 Step 4); every disk-write site in the provider's save path that must route through `toFileBytes` (Task 4 Step 3).
