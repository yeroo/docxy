// Regression tests for two bugs found reviewing Task 3 of the
// mermaid-live-render plan (real mermaid.js webview rendering), both in
// `paintMermaid()`/`paintMermaidSvgInto()`:
//
//   Bug 1 (DOM leak + failed-render re-render): `mermaid.render(id, source)`
//   (v10) creates its own temporary sandbox element — `<div id="d"+id>` —
//   appended straight to `document.body`, and on a REJECTED render (an
//   unsupported/garbage source) leaves that div attached permanently instead
//   of cleaning it up itself. A failed source was also never cached, so
//   every subsequent paint (every keystroke, even in an unrelated paragraph)
//   re-invoked `mermaid.render()` on the same broken source — leaking
//   another `document.body` child each time, unbounded.
//
//   Bug 2 (wide-diagram sizing): `paintMermaidSvgInto` sized the container
//   `<div>`'s CSS width/height, but the injected `<svg>` kept mermaid's own
//   literal `width="…" height="…"` px attributes (mermaid emits explicit px
//   because we init with `useMaxWidth:false`) — so a diagram wider than
//   `contentW` overflowed with both scrollbars instead of fitting to width.
//
// Both are exercised here via `window.__OFFXY_TEST__`, a hook block
// `webview.js` only installs when `window.__OFFXY_TEST__` already exists
// before the script runs (see that file's tail) — a real VS Code webview
// never sets it, so this is dead code there.
//
//   node media/mermaid-render.test.mjs        (wired as `npm run test:mermaid-render`)

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
    'test:mermaid-render: jsdom not installed — skipping (needs a real DOM to catch a ' +
      'document.body leak and to rescale an injected <svg> node).'
  );
  process.exit(0);
}

const dom = new JSDOM(
  '<!doctype html><html><body><div id="doc"></div><div id="status"></div></body></html>',
  { pretendToBeVisual: true }
);
const { window } = dom;
const document = window.document;

// ---- stub `mermaid`, shaped like the real v10 UMD global webview.js reads
// (`MERMAID.initialize(...)`, `MERMAID.render(id, source) -> Promise<{svg}>`).
// `FAIL_SOURCE`'s render mimics v10's real (buggy) behavior on rejection: it
// leaves its own temporary sandbox div attached to `document.body` and never
// removes it itself — exactly the leak `removeMermaidStray()` must clean up.
const FAIL_SOURCE = 'garbage not a real diagram';
const WIDE_SVG = '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="400" viewBox="0 0 1200 400"></svg>';
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
    return Promise.resolve({ svg: WIDE_SVG });
  },
};
window.acquireVsCodeApi = () => ({ postMessage: () => {} });
// Present (and truthy) BEFORE webview.js runs: its tail checks this to expose
// the test hooks and to skip boot() (which would otherwise fetch a real wasm
// binary this test never supplies).
window.__OFFXY_TEST__ = {};

vm.createContext(window);
vm.runInContext(readFileSync(join(here, 'webview.js'), 'utf8'), window, { filename: 'webview.js' });

const hooks = window.__OFFXY_TEST__;
assert.equal(typeof hooks.paintMermaid, 'function', 'webview.js must expose paintMermaid via __OFFXY_TEST__');
assert.equal(
  typeof hooks.paintMermaidSvgInto,
  'function',
  'webview.js must expose paintMermaidSvgInto via __OFFXY_TEST__'
);

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const strayDivs = () => [...document.body.querySelectorAll('div[id^="dmmd-"]')];

// ---- Bug 1: leak + failure-cache -------------------------------------------
hooks.setMetrics({ charW: 8, lineH: 18 });
hooks.setLastView({
  lines: [],
  caret: { line: 0, col: 0 },
  mermaid: [{ col: 0, row: 0, cols: 20, rows: 10, source: FAIL_SOURCE, geo: { nodes: [] } }],
});

hooks.paintMermaid();
await tick(); // let the rejected render's .catch() run

assert.equal(renderCallCount, 1, 'first paint of a failing source must call mermaid.render once');
assert.equal(
  strayDivs().length,
  0,
  'mermaid\'s leftover "d"+id sandbox div must be removed from document.body after a rejected render'
);

hooks.paintMermaid(); // second paint of the SAME failing source
await tick();

assert.equal(
  renderCallCount,
  1,
  'a second paint of an already-failed source must NOT call mermaid.render again (failure cache)'
);
assert.equal(strayDivs().length, 0, 'no stray div must appear on a re-paint of a known-failed source either');

console.log('mermaid render: DOM leak cleaned up + failed-render cache OK');

// ---- Bug 2: wide-diagram fit-to-width --------------------------------------
const wideEl = document.createElement('div');
document.body.appendChild(wideEl);
hooks.paintMermaidSvgInto(wideEl, WIDE_SVG, 600);

const injected = wideEl.querySelector('svg');
assert.ok(injected, 'paintMermaidSvgInto must inject the <svg> into the element');
const w = parseFloat(injected.getAttribute('width'));
const h = parseFloat(injected.getAttribute('height'));
assert.ok(w <= 600, `injected <svg> width must be rescaled to fit contentW=600, got ${w}`);
assert.ok(Math.abs(h - 200) < 1, `injected <svg> height must scale proportionally (~200), got ${h}`);

// A diagram that already fits keeps its natural size — the raw svg's own
// width/height attributes are left untouched (no forced upscale/rewrite).
const smallEl = document.createElement('div');
document.body.appendChild(smallEl);
const SMALL_SVG = '<svg xmlns="http://www.w3.org/2000/svg" width="300" height="150" viewBox="0 0 300 150"></svg>';
hooks.paintMermaidSvgInto(smallEl, SMALL_SVG, 600);
const smallInjected = smallEl.querySelector('svg');
assert.equal(smallInjected.getAttribute('width'), '300', 'a diagram narrower than contentW keeps its natural width');
assert.equal(smallInjected.getAttribute('height'), '150', 'a diagram narrower than contentW keeps its natural height');

console.log('mermaid render: wide-diagram fit-to-width (raw <svg> rescaled, not just its container) OK');
