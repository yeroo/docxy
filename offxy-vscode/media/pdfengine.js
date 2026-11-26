// The PDF engine's JavaScript side (#2259): `pdfpages.wasm` (suite/pdfpages,
// PdfCraft's pdfcraft-render) behind its hand-written ABI (suite/pdfpages/src/
// abi.rs). Loaded by the PDF viewer's worker (pdf.js puts it into the worker's
// blob) and by the Node test (pdf.test.mjs).
//
// The module is built without the wasm-bindgen CLI, so it imports wasm-bindgen's
// placeholders (getrandom's browser backend, which only writing an encrypted file
// would call). Each import is a stub that throws: a call would be a bug, and the
// test checks no other import exists (ALLOWED_IMPORTS).
(function (root) {
  'use strict';

  /** The modules the engine's imports may come from. */
  const ALLOWED_IMPORTS = ['__wbindgen_placeholder__', '__wbindgen_externref_xform__'];

  /** Every function import of `module` as a stub that throws. */
  function stubImports(module) {
    const imports = {};
    for (const imp of WebAssembly.Module.imports(module)) {
      if (!ALLOWED_IMPORTS.includes(imp.module) || imp.kind !== 'function') {
        throw new Error(`the PDF engine imports ${imp.module}.${imp.name} (${imp.kind})`);
      }
      (imports[imp.module] ||= {})[imp.name] = () => {
        throw new Error(`PDF engine import ${imp.module}.${imp.name} called`);
      };
    }
    return imports;
  }

  /** One instance of the engine. A trap leaves it unusable: make another. */
  class Engine {
    constructor(exports) {
      this.x = exports;
    }

    static async create(module) {
      const instance = await WebAssembly.instantiate(module, stubImports(module));
      return new Engine(instance.exports);
    }

    /** Copy a length-prefixed answer out of linear memory and free it. */
    take(ptr) {
      const n = new DataView(this.x.memory.buffer).getUint32(ptr, true);
      const out = new Uint8Array(this.x.memory.buffer, ptr + 4, n).slice();
      this.x.pdf_free(ptr, 4 + n);
      return out;
    }

    /** Open `bytes` (a Uint8Array): a handle, 0 for none. */
    open(bytes) {
      if (!bytes.length) return 0;
      const ptr = this.x.pdf_alloc(bytes.length);
      new Uint8Array(this.x.memory.buffer, ptr, bytes.length).set(bytes);
      const handle = this.x.pdf_open(ptr, bytes.length);
      this.x.pdf_free(ptr, bytes.length);
      return handle;
    }

    /** `{ok, pages:[{w,h,label}], outline, title}` or `{ok:false, error, locked}`. */
    info(handle) {
      return JSON.parse(new TextDecoder().decode(this.take(this.x.pdf_info(handle))));
    }

    /** `{ok, text}` or `{ok:false, error}`. */
    text(handle, page) {
      return JSON.parse(new TextDecoder().decode(this.take(this.x.pdf_text(handle, page))));
    }

    /** `{width, height, rgba}` (straight RGBA) or `{error}`. */
    render(handle, page, scale) {
      const out = this.take(this.x.pdf_render(handle, page, scale));
      const view = new DataView(out.buffer, out.byteOffset, out.byteLength);
      const width = view.getUint32(0, true);
      const height = view.getUint32(4, true);
      if (width === 0 || height === 0) {
        return { error: new TextDecoder().decode(out.subarray(8)) || 'the page drew nothing' };
      }
      return { width, height, rgba: out.subarray(8) };
    }

    close(handle) {
      this.x.pdf_close(handle);
    }
  }

  const api = { ALLOWED_IMPORTS, stubImports, Engine };
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = api;
  } else {
    root.PdfEngine = api;
  }
})(typeof self !== 'undefined' ? self : this);
