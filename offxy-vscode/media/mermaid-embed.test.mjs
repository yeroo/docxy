// Tests for Task 3 of the mermaid .docx image-embed plan (webview PNG
// rasterization + `images_json`/blob map building for
// `docx_save_with_mermaid_images`).
//
// Same harness convention as `mermaid-render.test.mjs`: load `webview.js`
// UNMODIFIED into a jsdom `vm` context with `window.__OFFXY_TEST__ = {}`
// already set (so the file's tail exposes its internal hooks instead of
// calling `boot()`, which would otherwise fetch a real wasm binary this test
// never supplies).
//
// The real browser rasterization path (`new Image()` decoding an SVG data
// URI, a real `<canvas>` 2D context, `canvas.toBlob('image/png')`) is stubbed
// here rather than exercised for real: jsdom has no canvas backend without
// the (not-a-dependency-of-this-project) `canvas` npm package. Per the task
// brief, that leaves the actual SVG-to-pixel rasterization
// browser-verified/deferred to the maintainer; what IS asserted here, against
// real (non-stubbed) code paths, is:
//   - `svgToPng` drives the Image-load -> canvas-size -> toBlob -> bytes
//     pipeline correctly (canvas sized to wPx*scale x hPx*scale; PNG bytes
//     read back through `Blob.arrayBuffer()` unchanged).
//   - `pxToEmu`'s EMU math.
//   - `concatBytes`'s flat-buffer concatenation.
//   - `buildMermaidImageSave`'s map-building: per-diagram EMU sizing from
//     each SVG's OWN natural size, omission of no-source/failed-render
//     diagrams, and — most load-bearing for the wasm ABI — that the
//     `images_json` descriptor's `pngOff`/`pngLen`/`svgOff`/`svgLen` for
//     every entry are an exact, non-overlapping slice of the concatenated
//     `pngBlob`/`svgBlob`.
//
//   node media/mermaid-embed.test.mjs        (wired as `npm run test:mermaid-embed`)

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const here = dirname(fileURLToPath(import.meta.url));

let JSDOM;
try {
  ({ JSDOM } = await import('jsdom'));
} catch {
  console.log(
    'test:mermaid-embed: jsdom not installed — skipping (needs a DOM for Image/canvas/document.createElement).'
  );
  process.exit(0);
}

const dom = new JSDOM(
  '<!doctype html><html><body><div id="doc"></div><div id="status"></div></body></html>',
  { pretendToBeVisual: true }
);
const { window } = dom;
const document = window.document;

// Node 18+'s global `Blob` has a real `arrayBuffer()` — use it for the
// `toBlob` stub below rather than relying on jsdom's own (version-dependent)
// Blob shim.
window.Blob = globalThis.Blob;

// ---- stub `mermaid`, shaped like the real v10 UMD global webview.js reads --
// One source resolves (used to exercise the "not yet cached" render path in
// `ensureMermaidRendered`/`buildMermaidImageSave`), one rejects (mimics a
// real parse failure — including v10's known stray-sandbox-div leak, so
// `removeMermaidStray` still has something to clean up).
const RENDER_SOURCE = 'flowchart TD\nC-->D';
const RENDER_SVG = '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100"></svg>';
const FAIL_SOURCE = 'garbage not a real diagram';
let renderCallCount = 0;
window.mermaid = {
  initialize() {},
  render(id, source) {
    renderCallCount++;
    if (source === FAIL_SOURCE) {
      const stray = document.createElement('div');
      stray.id = 'd' + id;
      document.body.appendChild(stray);
      return Promise.reject(new Error('mermaid parse error'));
    }
    return Promise.resolve({ svg: RENDER_SVG });
  },
};
window.acquireVsCodeApi = () => ({ postMessage: () => {} });
window.__OFFXY_TEST__ = {}; // present + truthy BEFORE webview.js runs

// ---- stub Image: fire onload asynchronously regardless of the data URI
// (jsdom has no real image decoder; the pixel content never matters here —
// only that svgToPng's await-the-load step resolves). ------------------------
class FakeImage {
  constructor() {
    this.onload = null;
    this.onerror = null;
    this._src = '';
  }
  set src(v) {
    this._src = v;
    setTimeout(() => {
      if (this.onload) this.onload();
    }, 0);
  }
  get src() {
    return this._src;
  }
}
window.Image = FakeImage;

// ---- stub canvas: getContext()/toBlob() (jsdom has no canvas backend
// without the `canvas` npm package, which isn't a dependency here). Records
// every canvas's committed width/height so the test can assert svgToPng sized
// it to wPx*scale x hPx*scale. toBlob always resolves with the SAME known PNG
// bytes — the point under test is the wiring (sizing, offsets), not pixel
// content. -------------------------------------------------------------------
const KNOWN_PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4]);
const createdCanvases = [];
window.HTMLCanvasElement.prototype.getContext = function () {
  return { drawImage() {} };
};
window.HTMLCanvasElement.prototype.toBlob = function (cb, type) {
  setTimeout(() => cb(new window.Blob([KNOWN_PNG], { type: type || 'image/png' })), 0);
};
const origCreateElement = document.createElement.bind(document);
document.createElement = function (tag) {
  const el = origCreateElement(tag);
  if (tag === 'canvas') createdCanvases.push(el);
  return el;
};

vm.createContext(window);
vm.runInContext(readFileSync(join(here, 'webview.js'), 'utf8'), window, { filename: 'webview.js' });

const hooks = window.__OFFXY_TEST__;
for (const name of ['svgToPng', 'pxToEmu', 'concatBytes', 'buildMermaidImageSave', 'ensureMermaidRendered']) {
  assert.equal(typeof hooks[name], 'function', `webview.js must expose ${name} via __OFFXY_TEST__`);
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

// ---- svgToPng: PNG signature + canvas sized to wPx*scale x hPx*scale -------
{
  const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="300" height="150" viewBox="0 0 300 150"></svg>';
  const png = await hooks.svgToPng(svg, 300, 150);
  assert.ok(png instanceof Uint8Array, 'svgToPng must resolve to a Uint8Array');
  assert.ok(png.length > 0, 'svgToPng must resolve to a non-empty buffer');
  assert.deepEqual(
    [...png.slice(0, 4)],
    [0x89, 0x50, 0x4e, 0x47],
    'svgToPng result must start with the PNG signature 89 50 4E 47'
  );

  const canvas = createdCanvases[createdCanvases.length - 1];
  assert.equal(canvas.width, 600, 'canvas width must be wPx * the ~2x crisp-render scale factor');
  assert.equal(canvas.height, 300, 'canvas height must be hPx * the ~2x crisp-render scale factor');
  console.log('mermaid embed: svgToPng resolves a PNG-signed buffer, canvas sized to ~2x natural px OK');
}

// ---- pxToEmu: standard 96px = 1in = 914400 EMU -----------------------------
{
  assert.equal(hooks.pxToEmu(96), 914400, '96px must convert to exactly one inch of EMU');
  assert.equal(hooks.pxToEmu(192), 1828800, '192px must convert to exactly two inches of EMU');
  assert.equal(hooks.pxToEmu(0), 0);
  console.log('mermaid embed: pxToEmu EMU math OK');
}

// ---- concatBytes: flat concatenation in order ------------------------------
{
  const out = hooks.concatBytes([new Uint8Array([1, 2]), new Uint8Array([]), new Uint8Array([3, 4, 5])], 5);
  assert.deepEqual([...out], [1, 2, 3, 4, 5]);
  console.log('mermaid embed: concatBytes OK');
}

// ---- buildMermaidImageSave: map-building + EMU sizing + descriptor offsets -
{
  hooks.setMetrics({ charW: 8, lineH: 18 });

  const CACHED_SOURCE = 'flowchart TD\nA-->B';
  const CACHED_SVG = '<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50" viewBox="0 0 100 50"></svg>';
  hooks.mmdCache.set(CACHED_SOURCE, CACHED_SVG); // pre-populate — no render() call needed for this one

  hooks.setLastView({
    lines: [],
    caret: { line: 0, col: 0 },
    mermaid: [
      { col: 0, row: 0, cols: 20, rows: 6, source: CACHED_SOURCE, geo: { nodes: [] } }, // already cached
      { col: 0, row: 10, cols: 20, rows: 6, source: RENDER_SOURCE, geo: { nodes: [] } }, // renders fresh
      { col: 0, row: 20, cols: 20, rows: 6, source: FAIL_SOURCE, geo: { nodes: [] } }, // render fails -> omitted
      { col: 0, row: 30, cols: 20, rows: 6, geo: { nodes: [] } }, // no `source` at all -> omitted
    ],
  });

  const before = renderCallCount;
  const built = await hooks.buildMermaidImageSave();
  await tick();

  assert.ok(built, 'buildMermaidImageSave must return a map when at least one diagram renders');
  // Two sources are NOT yet in mmdCache (RENDER_SOURCE and FAIL_SOURCE) — both
  // must call mermaid.render() once each; the already-cached source and the
  // no-`source` entry must not call it at all.
  assert.equal(renderCallCount, before + 2, 'both not-yet-cached sources (one ok, one failing) must call mermaid.render once each');

  const descriptor = JSON.parse(built.json);
  assert.equal(descriptor.length, 2, 'the failed-render and no-source diagrams must be OMITTED from the descriptor');
  assert.equal(descriptor[0].source, CACHED_SOURCE);
  assert.equal(descriptor[1].source, RENDER_SOURCE);

  // EMU sizing: each diagram's own natural SVG size (100x50 vs 200x100), NOT
  // a shared/global size.
  assert.equal(descriptor[0].wEmu, hooks.pxToEmu(100));
  assert.equal(descriptor[0].hEmu, hooks.pxToEmu(50));
  assert.equal(descriptor[1].wEmu, hooks.pxToEmu(200));
  assert.equal(descriptor[1].hEmu, hooks.pxToEmu(100));

  // Descriptor offsets must exactly (non-overlappingly) slice the
  // concatenated blobs — the load-bearing property for the wasm-side
  // `parse_mermaid_images` slicer.
  const pngExpectedLen = KNOWN_PNG.length; // the toBlob stub always returns this
  assert.equal(built.pngBlob.length, pngExpectedLen * 2);
  assert.equal(descriptor[0].pngOff, 0);
  assert.equal(descriptor[0].pngLen, pngExpectedLen);
  assert.equal(descriptor[1].pngOff, pngExpectedLen);
  assert.equal(descriptor[1].pngLen, pngExpectedLen);
  for (const d of descriptor) {
    const slice = built.pngBlob.slice(d.pngOff, d.pngOff + d.pngLen);
    assert.deepEqual([...slice], [...KNOWN_PNG], `pngOff/pngLen for ${d.source} must slice out its own PNG bytes`);
  }

  const svg0Bytes = new TextEncoder().encode(CACHED_SVG);
  const svg1Bytes = new TextEncoder().encode(RENDER_SVG);
  assert.equal(descriptor[0].svgOff, 0);
  assert.equal(descriptor[0].svgLen, svg0Bytes.length);
  assert.equal(descriptor[1].svgOff, svg0Bytes.length);
  assert.equal(descriptor[1].svgLen, svg1Bytes.length);
  assert.equal(built.svgBlob.length, svg0Bytes.length + svg1Bytes.length);
  assert.deepEqual([...built.svgBlob.slice(descriptor[0].svgOff, descriptor[0].svgOff + descriptor[0].svgLen)], [...svg0Bytes]);
  assert.deepEqual([...built.svgBlob.slice(descriptor[1].svgOff, descriptor[1].svgOff + descriptor[1].svgLen)], [...svg1Bytes]);

  console.log('mermaid embed: buildMermaidImageSave map-building + EMU sizing + descriptor offsets OK');
}

// ---- no mermaid diagrams at all / every diagram failed -> null (caller
// falls back to the plain, unchanged save() path) ---------------------------
{
  hooks.setLastView({ lines: [], caret: { line: 0, col: 0 }, mermaid: [] });
  assert.equal(await hooks.buildMermaidImageSave(), null, 'zero mermaid diagrams must yield null (plain-save fallback)');

  hooks.setLastView({
    lines: [],
    caret: { line: 0, col: 0 },
    mermaid: [{ col: 0, row: 0, cols: 20, rows: 6, source: FAIL_SOURCE, geo: { nodes: [] } }],
  });
  assert.equal(
    await hooks.buildMermaidImageSave(),
    null,
    'every diagram failing to render must yield null (plain-save fallback), same as zero diagrams'
  );
  console.log('mermaid embed: no-diagrams / all-failed both fall back to null (plain save) OK');
}

console.log('mermaid-embed: all assertions passed');
