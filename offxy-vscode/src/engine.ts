// Host-side wasm engine — the extension host (Node) instantiates the same
// docxwasm module the webview uses, for stateless format conversions that don't
// need an open editor (Markdown ⇄ docx). Loaded lazily and cached.

import * as vscode from 'vscode';

interface Exports {
  memory: WebAssembly.Memory;
  docx_alloc(len: number): number;
  docx_free(ptr: number, len: number): void;
  docx_from_markdown(ptr: number, len: number): number;
  docx_to_md(ptr: number, len: number): number;
}

let cached: Promise<Exports> | undefined;

async function load(context: vscode.ExtensionContext): Promise<Exports> {
  if (!cached) {
    cached = (async () => {
      const uri = vscode.Uri.joinPath(context.extensionUri, 'media', 'docxwasm.wasm');
      const bytes = await vscode.workspace.fs.readFile(uri);
      const module = await WebAssembly.compile(bytes as BufferSource);
      const instance = await WebAssembly.instantiate(module, {});
      return instance.exports as unknown as Exports;
    })();
  }
  return cached;
}

function mem(ex: Exports): Uint8Array {
  return new Uint8Array(ex.memory.buffer);
}

/** Write bytes into wasm memory, returning the pointer (caller frees). */
function writeBytes(ex: Exports, u8: Uint8Array): number {
  const ptr = ex.docx_alloc(u8.length);
  mem(ex).set(u8, ptr); // fetch view AFTER alloc (memory may have grown)
  return ptr;
}

/** Read + free a length-prefixed result buffer (`[u32 len][payload]`). */
function readResult(ex: Exports, ptr: number): Uint8Array {
  const m = mem(ex);
  const len = m[ptr] | (m[ptr + 1] << 8) | (m[ptr + 2] << 16) | (m[ptr + 3] << 24);
  const out = m.slice(ptr + 4, ptr + 4 + len);
  ex.docx_free(ptr, 4 + len);
  return out;
}

/** Convert Markdown source to `.docx` bytes. */
export async function markdownToDocx(
  context: vscode.ExtensionContext,
  markdown: string,
): Promise<Uint8Array> {
  const ex = await load(context);
  const input = new TextEncoder().encode(markdown);
  const p = writeBytes(ex, input);
  const r = ex.docx_from_markdown(p, input.length);
  ex.docx_free(p, input.length);
  return readResult(ex, r);
}

/** Convert `.docx` bytes to Markdown source. */
export async function docxToMarkdown(
  context: vscode.ExtensionContext,
  docx: Uint8Array,
): Promise<string> {
  const ex = await load(context);
  const p = writeBytes(ex, docx);
  const r = ex.docx_to_md(p, docx.length);
  ex.docx_free(p, docx.length);
  return new TextDecoder().decode(readResult(ex, r));
}
