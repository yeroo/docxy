// The PDF viewer's worker (#2259): one per view, so a slow page never blocks
// the webview and a trap (a panic, the memory cap, a stack overflow) costs this
// worker, which the view replaces (pdfhost.js). pdf.js builds it as a blob of
// pdfengine.js followed by this file; the compiled module comes in `init`.
//
//   {type:'init', module, bytes}       -> {type:'info', info}
//   {type:'render', id, page, scale}   -> {type:'done', id, width, height, rgba} | {type:'done', id, error}
//   {type:'text', id, page}            -> {type:'done', id, text} | {type:'done', id, error}
//
// Anything that throws (a trap is a `WebAssembly.RuntimeError`) answers
// {type:'trap', id, error}: the instance is unusable after it, so the host ends
// this worker and starts another.
'use strict';

let engine = null;
let handle = 0;

self.onmessage = (event) => {
  const msg = event.data;
  answer(msg).catch((e) => {
    self.postMessage({ type: 'trap', id: msg.id, error: String((e && e.message) || e) });
  });
};

async function answer(msg) {
  if (msg.type === 'init') {
    engine = await self.PdfEngine.Engine.create(msg.module);
    handle = engine.open(msg.bytes);
    const info = handle
      ? engine.info(handle)
      : { ok: false, error: 'the file is empty', locked: false };
    self.postMessage({ type: 'info', info });
    return;
  }
  if (msg.type === 'render') {
    const r = engine.render(handle, msg.page, msg.scale);
    if (r.error) {
      self.postMessage({ type: 'done', id: msg.id, error: r.error });
    } else {
      const rgba = r.rgba.slice();
      self.postMessage({ type: 'done', id: msg.id, width: r.width, height: r.height, rgba }, [
        rgba.buffer,
      ]);
    }
    return;
  }
  if (msg.type === 'text') {
    const t = engine.text(handle, msg.page);
    self.postMessage(
      t.ok ? { type: 'done', id: msg.id, text: t.text } : { type: 'done', id: msg.id, error: t.error },
    );
  }
}
