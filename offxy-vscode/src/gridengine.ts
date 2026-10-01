// Host-side gridwasm loader — only the empty-file create flow runs on the
// host (`grid_new`, typed through `grid_save_as`); the full engine runs in
// the webview.

import * as vscode from 'vscode';

interface Exports {
  memory: WebAssembly.Memory;
  grid_alloc(len: number): number;
  grid_free(ptr: number, len: number): void;
  grid_new(): number;
  grid_open(ptr: number, len: number): number;
  grid_save_as(handle: number, kind: number): number;
  grid_close(handle: number): void;
}

let cached: Promise<Exports> | undefined;

async function load(context: vscode.ExtensionContext): Promise<Exports> {
  if (!cached) {
    cached = (async () => {
      const uri = vscode.Uri.joinPath(context.extensionUri, 'media', 'gridwasm.wasm');
      const bytes = await vscode.workspace.fs.readFile(uri);
      const module = await WebAssembly.compile(bytes as BufferSource);
      const instance = await WebAssembly.instantiate(module, {});
      return instance.exports as unknown as Exports;
    })();
  }
  return cached;
}

/** Copy out, then free, a length-prefixed result buffer at `ptr`. */
function takeResult(ex: Exports, ptr: number): Uint8Array {
  const m = new Uint8Array(ex.memory.buffer);
  const len = (m[ptr] | (m[ptr + 1] << 8) | (m[ptr + 2] << 16) | (m[ptr + 3] << 24)) >>> 0;
  const out = m.slice(ptr + 4, ptr + 4 + len);
  ex.grid_free(ptr, 4 + len);
  return out;
}

/** Bytes of a fresh empty workbook, typed by `kind` (gridwasm's
 *  `grid_save_as` code, see `sheetKindCode`): an empty `.xltm` gets a macro
 *  template, not a workbook-typed package Excel refuses under that name.
 *  0 or 1 is a plain `.xlsx` workbook. */
export async function newWorkbook(
  context: vscode.ExtensionContext,
  kind = 0,
): Promise<Uint8Array> {
  const ex = await load(context);
  const fresh = takeResult(ex, ex.grid_new());
  if (kind <= 1) {
    return fresh;
  }
  const input = ex.grid_alloc(fresh.length);
  new Uint8Array(ex.memory.buffer).set(fresh, input);
  const handle = ex.grid_open(input, fresh.length);
  ex.grid_free(input, fresh.length);
  if (handle === 0) {
    throw new Error('grid_new produced a workbook grid_open could not read');
  }
  try {
    return takeResult(ex, ex.grid_save_as(handle, kind));
  } finally {
    ex.grid_close(handle);
  }
}
