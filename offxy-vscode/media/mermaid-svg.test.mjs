// Unit test for `buildMermaidSvg`, the pure geometry -> inline-SVG renderer the
// docxy webview overlays on top of a mermaid diagram's reserved grid box (Task
// 7 of the mermaid-flowchart-quality plan). It draws the SAME `DiagramGeometry`
// (docxcore's `mermaid.rs`) the Word exporter turns into DrawingML shapes, so
// the two renderings match.
//
// `webview.js` has no module system (it loads as a plain `<script src=...>`
// tag — see extension.ts) and `buildMermaidSvg` is declared at its top level,
// outside the webview's own IIFE. Mirroring how `grid.layout.test.mjs` runs
// `grid.js` unmodified inside a Node `vm` sandbox to reach its behavior, this
// test runs `webview.js` the same way: a top-level `function` declaration in
// non-strict script code becomes a property of the global object the script
// runs against (`window.buildMermaidSvg` under a real `<script>` tag; a
// property of the sandbox object here), so the function is callable directly
// with no export statement needed. The sandbox only needs to be complete
// enough that webview.js's top-level statements (acquireVsCodeApi(),
// document.getElementById, window.addEventListener) don't throw — booting the
// real wasm engine is irrelevant to this pure-function test, and boot() itself
// is async and already wrapped in a `.catch()`, so anything it touches that
// the stub omits (fetch, WebAssembly, window.__OFFXY__) just rejects quietly.
//
//   node media/mermaid-svg.test.mjs        (wired as `npm run test:mermaid-svg`)

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));

// ---- minimal DOM/host stub --------------------------------------------------
class El {
  constructor() {
    this.style = {};
  }
  addEventListener() {}
}
const document = {
  getElementById: () => new El(),
  createElement: () => new El(),
  createDocumentFragment: () => new El(),
  addEventListener: () => {},
};
const sandbox = {
  window: { addEventListener: () => {} },
  document,
  acquireVsCodeApi: () => ({ postMessage: () => {} }),
  TextEncoder,
  TextDecoder,
  console,
  setTimeout,
  clearTimeout,
};
vm.createContext(sandbox);
vm.runInContext(readFileSync(join(here, 'webview.js'), 'utf8'), sandbox, { filename: 'webview.js' });

const { buildMermaidSvg } = sandbox;
assert.equal(
  typeof buildMermaidSvg,
  'function',
  'webview.js must expose buildMermaidSvg as a top-level function',
);

// ---- fixture: matches the `DiagramGeometry` JSON shape docxwasm's view_json
// emits (docxcore's mermaid.rs DiagramGeometry::to_json) — a rect node, a
// diamond node, one multi-point labeled edge, one subgraph. ------------------
const geo = {
  canvasW: 3000000,
  canvasH: 1200000,
  nodes: [
    { x: 0, y: 0, w: 1000000, h: 457200, shape: 'rect', fill: 'DAE8FC', stroke: '6C8EBF', textColor: '000000', label: 'A' },
    { x: 0, y: 900000, w: 1000000, h: 457200, shape: 'diamond', fill: 'FF0000', stroke: '900000', textColor: 'FFFFFF', label: 'B' },
    { x: 0, y: 1800000, w: 1200000, h: 457200, shape: 'hexagon', fill: '00FF00', stroke: '009000', textColor: '000000', label: 'C' },
  ],
  edges: [
    { points: [[500000, 457200], [500000, 678600], [500000, 678600], [500000, 900000]], label: 'yes', style: 'solid' },
    { points: [[1000000, 457200], [1000000, 678600], [1000000, 678600], [1000000, 900000]], label: '', style: 'dotted' },
    { points: [[1200000, 457200], [1200000, 678600], [1200000, 678600], [1200000, 900000]], label: '', style: 'thick' },
  ],
  subgraphs: [{ x: -100000, y: -100000, w: 1200000, h: 1500000, title: 'G' }],
};

const svg = buildMermaidSvg(geo);
assert.ok(/<svg/.test(svg), 'must produce an <svg> root');
assert.ok((svg.match(/<rect/g) || []).length >= 2, 'must draw at least the subgraph rect + the rect node');
assert.ok(/<polygon/.test(svg), 'diamond node must draw as a <polygon>');
assert.ok(/<polyline/.test(svg), 'edge must draw as a <polyline>');
assert.ok(/#FF0000/i.test(svg), 'node fill color must be honored');
assert.ok(/>A<|>A /.test(svg) && />B</.test(svg), 'node labels must be drawn');
assert.ok(/>G</.test(svg), 'subgraph title must be drawn');

// ---- hexagon node: a second, distinct <polygon> with 6 points (vs the
// diamond's 4), filled/stroked/labeled like every other shape. --------------
const polygons = [...svg.matchAll(/<polygon points="([^"]+)"/g)].map((m) => m[1]);
assert.equal(polygons.length, 2, 'diamond + hexagon must each draw a <polygon>');
const pointCounts = polygons.map((p) => p.trim().split(/\s+/).length).sort((a, b) => a - b);
assert.deepEqual(pointCounts, [4, 6], 'diamond is a 4-point polygon, hexagon a 6-point polygon');
assert.ok(/#00FF00/i.test(svg), 'hexagon fill color must be honored');
assert.ok(/>C</.test(svg), 'hexagon node label must be drawn');

// ---- edge line-styles: dotted gets a dasharray, thick a wider stroke, solid
// is untouched (Task 3 of the flowchart-gaps plan). ------------------------
const edgeLines = [...svg.matchAll(/<polyline[^>]*\/>/g)].map((m) => m[0]);
assert.equal(edgeLines.length, 3, 'must draw one <polyline> per edge');
const [solidLine, dottedLine, thickLine] = edgeLines;
assert.ok(!/stroke-dasharray/.test(solidLine), 'solid edge must not have a dasharray');
assert.ok(/stroke-dasharray="8 6"/.test(dottedLine), 'dotted edge must have an 8 6 dasharray');
assert.ok(!/stroke-dasharray/.test(thickLine), 'thick edge must not have a dasharray');
const solidWidth = Number(solidLine.match(/stroke-width="([\d.]+)"/)[1]);
const thickWidth = Number(thickLine.match(/stroke-width="([\d.]+)"/)[1]);
assert.ok(thickWidth > solidWidth, 'thick edge must have a larger stroke-width than solid');

console.log('mermaid svg OK');

// ---- sequence diagram: `buildMermaidSvg` dispatches on `geo.kind` (Task 5 of
// the mermaid-sequence plan) to `buildSequenceSvg`, a separate renderer for
// the `SequenceGeometry` JSON shape docxcore's `mermaid_seq.rs` emits
// (participants, lifelines, messages, alt/else frames, notes) — the sequence
// analog of the flowchart geometry exercised above. Fixture matches the
// brief: 2 participants/lifelines, 1 message, 1 frame with an else-divider,
// 1 note whose text ('n & <ote>') proves escaping survives the sequence path
// too. -----------------------------------------------------------------------
const seqGeo = {
  kind: 'sequence',
  canvasW: 3000000,
  canvasH: 2000000,
  participants: [
    { x: 0, y: 0, w: 900000, h: 400000, label: 'A' },
    { x: 1500000, y: 0, w: 900000, h: 400000, label: 'B' },
  ],
  lifelines: [
    { x: 450000, y1: 400000, y2: 1900000 },
    { x: 1950000, y1: 400000, y2: 1900000 },
  ],
  messages: [
    { x1: 450000, y1: 700000, x2: 1950000, y2: 700000, text: 'm1', dashed: false, self: false },
  ],
  frames: [
    { x: 200000, y: 550000, w: 2000000, h: 900000, label: 'c', elseLabel: 'd', elseY: 1000000 },
  ],
  notes: [{ x: 300000, y: 1500000, w: 1800000, h: 300000, text: 'n & <ote>' }],
};

const seqSvg = buildMermaidSvg(seqGeo);
assert.ok(/<svg/.test(seqSvg), 'sequence must produce an <svg> root');
assert.ok(
  new RegExp(`viewBox="0 0 ${seqGeo.canvasW} ${seqGeo.canvasH}"`).test(seqSvg),
  'viewBox must use canvasW/canvasH'
);

const seqRects = [...seqSvg.matchAll(/<rect[^>]*\/>/g)].map((m) => m[0]);
const partRects = seqRects.filter((r) => /#DAE8FC/i.test(r));
assert.equal(partRects.length, 2, 'must draw one participant <rect> per participant');
const frameRects = seqRects.filter((r) => /#F5F5F5/i.test(r));
assert.equal(frameRects.length, 1, 'must draw one frame <rect>');
const noteRects = seqRects.filter((r) => /#FFF6D5/i.test(r));
assert.equal(noteRects.length, 1, 'must draw one note <rect>');

const seqLines = [...seqSvg.matchAll(/<line[^>]*\/>/g)].map((m) => m[0]);
const lifelineLines = seqLines.filter((l) => /stroke-dasharray/.test(l));
assert.ok(lifelineLines.length >= 2, 'must draw a dashed <line> per lifeline');

assert.ok(/marker-end="url\(#arrow\)"/.test(seqSvg), 'a message must carry the arrow marker');
assert.ok(/<line[^>]*marker-end="url\(#arrow\)"[^>]*\/>|<polyline[^>]*marker-end="url\(#arrow\)"[^>]*\/>/.test(seqSvg), 'the message must draw as a line/polyline with the arrow marker');

// the [else] divider: a distinct dashed line (frame stroke, not a lifeline)
// at elseY, plus its own label text.
assert.ok(/y1="1000000"[^>]*y2="1000000"|y="1000000"/.test(seqSvg), 'an else-divider must sit at elseY');
assert.ok(/\[else\]/.test(seqSvg) && /d</.test(seqSvg), 'the elseLabel must be drawn, prefixed [else]');

assert.ok(/>A</.test(seqSvg) && />B</.test(seqSvg), 'participant labels must be drawn');
assert.ok(/>m1</.test(seqSvg), 'message label must be drawn');
assert.ok(/>c</.test(seqSvg), 'frame title must be drawn');

// escaping: the note text 'n & <ote>' must appear escaped, never as a raw
// '<ote>' tag-looking fragment.
assert.ok(!/<ote>/.test(seqSvg), 'note text must be escaped, not emitted as raw markup');
assert.ok(/n &amp; &lt;ote&gt;/.test(seqSvg), 'note text must be escaped with &amp;/&lt;/&gt;');

console.log('mermaid sequence svg OK');
