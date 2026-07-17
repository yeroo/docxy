// Host-side gridwasm loader — only `grid_new` is needed on the host (the
// empty-file create flow); the full engine runs in the webview.

import * as vscode from 'vscode';

interface Exports {
  memory: WebAssembly.Memory;
  grid_free(ptr: number, len: number): void;
  grid_new(): number;
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

/** Bytes of a fresh empty workbook. */
export async function newWorkbook(context: vscode.ExtensionContext): Promise<Uint8Array> {
  const ex = await load(context);
  const ptr = ex.grid_new();
  const m = new Uint8Array(ex.memory.buffer);
  const len = m[ptr] | (m[ptr + 1] << 8) | (m[ptr + 2] << 16) | (m[ptr + 3] << 24);
  const out = m.slice(ptr + 4, ptr + 4 + len);
  ex.grid_free(ptr, 4 + len);
  return out;
}
