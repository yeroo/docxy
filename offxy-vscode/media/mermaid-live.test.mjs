// Headless render-logic test for Task 3 of the mermaid-live-render plan: it
// exercises the SAME call our webview's `paintMermaid()` makes —
// `mermaid.render(id, source)` — using the local `mermaid` devDependency
// (the very package vendored into `media/mermaid.min.js` by
// `npm run build:mermaid`), and asserts it resolves to a usable `{svg}` for
// both diagram kinds the wasm side can hand us (a `mb.geo.kind` flowchart vs
// sequence box — see docxcore's `mermaid.rs`/`mermaid_seq.rs`).
//
// This validates the RENDER LOGIC only (does `mermaid.render()` produce a
// real svg for these two source shapes) — it does NOT touch the webview's
// actual CSP; that's the mermaid-live-render plan's Step 5 (headless-Edge
// against the real provider HTML shell), run and documented separately, not
// wired into `npm test` since it shells out to a system browser binary.
//
// `mermaid.render()` needs a real DOM (it calls `document.createElement`,
// measures text, and — via its bundled DOMPurify — expects a `window` to
// exist at MODULE-LOAD time, not just when render() runs) — so the jsdom
// globals below are installed BEFORE `mermaid` is imported (a plain
// `import mermaid from 'mermaid'` at the top of this file, evaluated before
// any other statement, would be too late).
//
//   node media/mermaid-live.test.mjs        (wired as `npm run test:mermaid-live`)

import assert from 'node:assert/strict';

let JSDOM;
try {
  ({ JSDOM } = await import('jsdom'));
} catch {
  console.log(
    'test:mermaid-live: jsdom not installed — skipping (mermaid.render() needs a DOM; ' +
      'see the mermaid-live-render plan Step 5 for the real-browser CSP check).'
  );
  process.exit(0);
}

const dom = new JSDOM('<!doctype html><html><body></body></html>', { pretendToBeVisual: true });
global.window = dom.window;
global.document = dom.window.document;
global.SVGElement = dom.window.SVGElement;
global.CSSStyleSheet = dom.window.CSSStyleSheet;
global.Node = dom.window.Node;
global.HTMLElement = dom.window.HTMLElement;
global.Element = dom.window.Element;
global.DOMParser = dom.window.DOMParser;
// Node 22+ ships its own read-only global `navigator` getter (Web-platform
// API parity) — redefine it (a plain assignment throws against a getter-only
// property) so mermaid sees jsdom's `navigator.userAgent`, matching what a
// real webview provides.
Object.defineProperty(global, 'navigator', { value: dom.window.navigator, configurable: true });
// jsdom does no layout at all, so it has no real `getBBox` (SVGElement text
// measurement) — a well-known jsdom limitation, not a mermaid bug; mermaid's
// own test suite stubs it the same way. The stub's numbers are arbitrary
// (never asserted on) — this only needs to let mermaid's layout pass finish
// without throwing so `render()` can produce a real svg to assert against.
global.SVGElement.prototype.getBBox = () => ({ x: 0, y: 0, width: 100, height: 20 });

const { default: mermaid } = await import('mermaid');

mermaid.initialize({ startOnLoad: false, securityLevel: 'loose' });

const flow = await mermaid.render('t-flow', 'flowchart TD\nA-->B');
assert.match(flow.svg, /<svg/, 'flowchart render did not return an <svg> string');
assert.match(
  flow.svg,
  /marker|flowchart/i,
  'flowchart render did not contain a flowchart-specific marker'
);
console.log('mermaid.render() flowchart OK');

const seq = await mermaid.render('t-seq', 'sequenceDiagram\nA->>B: hi');
assert.match(seq.svg, /<svg/, 'sequence render did not return an <svg> string');
assert.match(
  seq.svg,
  /sequenceDiagram|actor|messageLine/i,
  'sequence render did not contain a sequence-specific marker'
);
console.log('mermaid.render() sequenceDiagram OK');
