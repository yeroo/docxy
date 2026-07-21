// Transport regression test for the `.md` custom editor's load/save path.
//
// The markdown editor's plumbing (extension.ts's `fromFileBytes`/`toFileBytes`
// on the `offxy.markdownEditor` spec) is exactly:
//   .md text --[markdownToDocx]--> docx bytes (webview `docx_open`s these)
//   webview `docx_save` bytes --[docxToMarkdown]--> .md text (written to disk)
// Both `markdownToDocx`/`docxToMarkdown` (src/engine.ts) are thin wrappers
// around the wasm exports `docx_from_markdown` / `docx_to_md`. This test calls
// those two wasm exports directly (the real ABI — no vscode host needed) to
// prove the exact round-trip the editor relies on is faithful: task lists
// (`- [ ]`/`- [x]`) and ordered lists (`1.`/`2.`) with a soft-wrapped
// continuation must survive `md -> docx bytes -> md` unmangled.
//
// ABI (docxwasm/src/lib.rs):
//   docx_alloc(len) -> ptr                    — allocate a buffer, write input there
//   docx_free(ptr, len)                       — free it (or a result buffer, len = 4+payload)
//   docx_from_markdown(ptr, len) -> resultPtr — markdown UTF-8 bytes in, docx bytes out
//   docx_to_md(ptr, len) -> resultPtr         — docx bytes in (NOT text), markdown UTF-8 bytes out
//   Every result is length-prefixed: a little-endian u32 byte count, then that
//   many payload bytes (see `ret_bytes` in lib.rs).
//
//   node media/markdown-roundtrip.test.mjs       (wired as `npm run test:md-roundtrip`)

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath = join(here, 'docxwasm.wasm');
const { instance } = await WebAssembly.instantiate(readFileSync(wasmPath), {});
const ex = instance.exports;

function mem() {
  return new Uint8Array(ex.memory.buffer);
}

/** Write bytes into wasm memory, returning the pointer (caller frees). */
function writeBytes(u8) {
  const ptr = ex.docx_alloc(u8.length);
  mem().set(u8, ptr); // fetch the view AFTER alloc: memory may have grown
  return ptr;
}

/** Read + free a length-prefixed result buffer (`[u32 len][payload]`). */
function readResult(ptr) {
  const m = mem();
  const len = m[ptr] | (m[ptr + 1] << 8) | (m[ptr + 2] << 16) | (m[ptr + 3] << 24);
  const out = m.slice(ptr + 4, ptr + 4 + len);
  ex.docx_free(ptr, 4 + len);
  return out;
}

const enc = new TextEncoder();
const dec = new TextDecoder();

/** markdown string -> docx bytes, via `docx_from_markdown` (mirrors engine.ts's markdownToDocx). */
function markdownToDocx(md) {
  const input = enc.encode(md);
  const p = writeBytes(input);
  const r = ex.docx_from_markdown(p, input.length);
  ex.docx_free(p, input.length);
  return readResult(r);
}

/** docx bytes -> markdown string, via `docx_to_md` (mirrors engine.ts's docxToMarkdown).
 *  Takes docx BYTES, not text — this is the ABI detail the brief flagged as
 *  needing verification against the real export (it is `docx_to_md`, not a
 *  `docx_to_markdown` export, and it consumes the binary docx buffer). */
function docxToMarkdown(docxBytes) {
  const p = writeBytes(docxBytes);
  const r = ex.docx_to_md(p, docxBytes.length);
  ex.docx_free(p, docxBytes.length);
  return dec.decode(readResult(r));
}

// The exact transport the editor uses: fromFileBytes (md -> docx bytes) then
// toFileBytes (docx bytes -> md), back to back, on one corpus.
const md = [
  '# Title',
  '',
  '- [ ] todo',
  '- [x] done',
  '',
  '1. one',
  '2. two continued',
  '   more of item two, soft-wrapped',
  '3. three',
  '',
  '- outer',
  '  - nested one',
  '  - nested two',
  '',
].join('\n');

const docxBytes = markdownToDocx(md);
assert.ok(docxBytes.length > 0, 'docx_from_markdown produced empty bytes');

const back = docxToMarkdown(docxBytes);

// Task lists: literal `- [ ]` / `- [x]` text, not escaped/mangled.
assert.ok(back.includes('- [ ] todo'), 'unchecked task list item did not survive:\n' + back);
assert.ok(back.includes('- [x] done'), 'checked task list item did not survive:\n' + back);

// Ordered list markers: `1.`/`2.`/`3.`, not renumbered/flattened to bullets.
assert.ok(/^1\.\s+one/m.test(back), 'ordered list item "1." did not survive:\n' + back);
assert.ok(/^2\.\s+two continued/m.test(back), 'ordered list item "2." did not survive:\n' + back);
assert.ok(/^3\.\s+three/m.test(back), 'ordered list item "3." did not survive:\n' + back);

// The soft-wrapped continuation line stays part of item 2's text, not split
// into a separate top-level paragraph (the Phase-1 list-continuation fix).
assert.ok(
  back.includes('more of item two, soft-wrapped'),
  'soft-wrapped list continuation text missing:\n' + back,
);
assert.ok(
  !/^more of item two/m.test(back),
  'soft-wrapped continuation fell out of its list item as a bare paragraph:\n' + back,
);

// Nested bullet structure survives (both nested items present).
assert.ok(back.includes('nested one') && back.includes('nested two'), 'nested list items missing:\n' + back);

// Heading survives.
assert.ok(/^#\s+Title/m.test(back), 'heading did not survive:\n' + back);

console.log('markdown transport OK (md -> docx bytes -> md, via docx_from_markdown/docx_to_md)');
