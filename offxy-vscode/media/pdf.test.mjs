// The VS Code PDF viewer's engine and worker host (#2259), in Node:
//
// - pdfpages.wasm (suite/pdfpages, PdfCraft's renderer) imports only
//   wasm-bindgen's placeholders, and every one is a stub that throws, which
//   nothing in the corpus calls;
// - it reads and draws the PDF corpus, answers why for what it cannot, and
//   keeps to its memory cap: past it the instance traps, and a new one works;
// - pdfhost.js answers a request past its timeout, or whose worker trapped or
//   died, with why, and opens the file again in a new worker for the next one.
//
//   node media/pdf.test.mjs      (`npm run test:pdf`; needs the wasm built:
//   cargo build --manifest-path suite/Cargo.toml -p pdfpages --lib
//   --target wasm32-unknown-unknown --profile wasm)
//
// PDFPAGES_WASM names another module to test.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const { Engine, stubImports, ALLOWED_IMPORTS } = require('./pdfengine.js');
const { PdfHost, TOO_LONG, STOPPED } = require('./pdfhost.js');

const corpusDir = resolve(here, '../../corpus/pdf-view');
const wasmPath =
  process.env.PDFPAGES_WASM ||
  [resolve(here, '../../suite/target/wasm32-unknown-unknown/wasm/pdfpages.wasm'), join(here, 'pdfpages.wasm')].find(
    existsSync,
  );
assert.ok(wasmPath, 'pdfpages.wasm is not built (see the header of this file)');
const module = new WebAssembly.Module(readFileSync(wasmPath));
const corpus = (name) => new Uint8Array(readFileSync(join(corpusDir, `${name}.pdf`)));

let passed = 0;
async function test(name, f) {
  try {
    await f();
    passed += 1;
    console.log(`ok - ${name}`);
  } catch (e) {
    console.error(`not ok - ${name}`);
    throw e;
  }
}

/** A stub's call shows as this error: the test fails on any. */
const STUB_CALLED = /PDF engine import .* called/;

await test('the module imports only wasm-bindgen placeholders, and each is a throwing stub', () => {
  const imports = WebAssembly.Module.imports(module);
  assert.ok(imports.length > 0, 'getrandom brings placeholders');
  for (const imp of imports) {
    assert.ok(ALLOWED_IMPORTS.includes(imp.module), `unexpected import module ${imp.module}`);
    assert.equal(imp.kind, 'function', `${imp.module}.${imp.name}`);
    assert.match(imp.name, /^__wb(indgen|g)_/, `${imp.module}.${imp.name}`);
  }
  const stubs = stubImports(module);
  for (const imp of imports) {
    assert.throws(() => stubs[imp.module][imp.name](), STUB_CALLED);
  }
  const names = WebAssembly.Module.exports(module).map((e) => e.name);
  for (const want of ['memory', 'pdf_alloc', 'pdf_free', 'pdf_open', 'pdf_close', 'pdf_info', 'pdf_text', 'pdf_render']) {
    assert.ok(names.includes(want), `exports ${want}`);
  }
});

await test('text.pdf: its pages, its size and its text in reading order', async () => {
  const e = await Engine.create(module);
  const h = e.open(corpus('text'));
  const info = e.info(h);
  assert.equal(info.ok, true, JSON.stringify(info));
  assert.equal(info.pages.length, 2);
  assert.deepEqual([Math.round(info.pages[0].w), Math.round(info.pages[0].h)], [595, 842]);
  const t = e.text(h, 0);
  assert.equal(t.ok, true);
  const lines = [
    'Hello from the first page.',
    'Привет, мир! Съешь же ещё этих мягких французских булок,',
    'Καλημέρα κόσμε. Ξεσκεπάζω την ψυχοφθόρα βδελυγμία.',
  ];
  const at = lines.map((l) => t.text.indexOf(l));
  assert.ok(at.every((i) => i >= 0), t.text);
  assert.ok(at[0] < at[1] && at[1] < at[2], 'reading order');
  e.close(h);
});

await test('slides.pdf: three 960 x 540 pages, the outline, a page drawn', async () => {
  const e = await Engine.create(module);
  const h = e.open(corpus('slides'));
  const info = e.info(h);
  assert.equal(info.pages.length, 3);
  for (const p of info.pages) assert.deepEqual([Math.round(p.w), Math.round(p.h)], [960, 540]);
  assert.ok(info.outline.some((b) => b.title === 'Revenue by region' && b.page === 1));
  const r = e.render(h, 0, 0.5);
  assert.equal(r.error, undefined, r.error);
  assert.deepEqual([r.width, r.height], [480, 270]);
  assert.equal(r.rgba.length, 480 * 270 * 4);
  let ink = 0;
  for (let i = 0; i < r.rgba.length; i += 4) {
    assert.equal(r.rgba[i + 3], 255, 'opaque');
    if (r.rgba[i] < 200 || r.rgba[i + 1] < 200 || r.rgba[i + 2] < 200) ink += 1;
  }
  assert.ok(ink > 1000, `not blank (${ink} inked pixels)`);
  const past = e.render(h, 3, 1);
  assert.equal(past.error, 'the PDF has no page 4');
});

await test('every corpus file draws and reads page 1, or says why; no stub is called', async () => {
  const files = readdirSync(corpusDir).filter((f) => f.endsWith('.pdf'));
  assert.ok(files.length >= 20, files.join(' '));
  const times = [];
  const e = await Engine.create(module);
  for (const f of files) {
    const name = f.replace(/\.pdf$/, '');
    const h = e.open(corpus(name));
    const start = performance.now();
    const info = e.info(h);
    if (!info.ok) {
      assert.ok(info.error.length > 0, name);
      if (name.startsWith('enc-') && name !== 'enc-owner-only') assert.equal(info.locked, true, name);
      e.close(h);
      continue;
    }
    const r = e.render(h, 0, 1);
    if (r.error === undefined) assert.equal(r.rgba.length, r.width * r.height * 4, name);
    const t = e.text(h, 0);
    assert.ok(t.ok || t.error.length > 0, name);
    times.push(`${name} ${Math.round(performance.now() - start)} ms`);
    e.close(h);
  }
  console.log(`  page 1 drawn and read: ${times.join(', ')}`);
});

await test('garbage and a truncated file answer why', async () => {
  const e = await Engine.create(module);
  for (const bytes of [new TextEncoder().encode('no PDF at all'), corpus('text').subarray(0, 700)]) {
    const h = e.open(bytes);
    const info = e.info(h);
    if (!info.ok) {
      assert.ok(info.error.length > 0);
    } else {
      const r = e.render(h, 0, 1);
      assert.ok(r.error === undefined || r.error.length > 0);
    }
    e.close(h);
  }
  assert.equal(e.open(new Uint8Array(0)), 0, 'no bytes, no document');
});

await test('memory past the cap traps the instance, and a new one opens text.pdf', async () => {
  const e = await Engine.create(module);
  assert.throws(() => e.x.pdf_alloc(1.5 * 2 ** 30), WebAssembly.RuntimeError);
  assert.ok(e.x.memory.buffer.byteLength <= 2 ** 30, `${e.x.memory.buffer.byteLength}`);
  const fresh = await Engine.create(module);
  const h = fresh.open(corpus('text'));
  assert.equal(fresh.info(h).ok, true);
});

// ---- pdfhost.js with fake workers -------------------------------------------

/** Fake timers: `fire()` runs every armed one. */
function timers() {
  const armed = new Map();
  let next = 1;
  return {
    setTimer: (f) => {
      armed.set(next, f);
      return next++;
    },
    clearTimer: (id) => armed.delete(id),
    fire: () => {
      const all = [...armed.values()];
      armed.clear();
      for (const f of all) f();
    },
    count: () => armed.size,
  };
}

/** A worker that records what it is sent and answers when told. */
function fakeWorkers() {
  const made = [];
  const make = () => {
    const w = {
      sent: [],
      terminated: false,
      onmessage: null,
      onerror: null,
      postMessage(m) {
        this.sent.push(m);
      },
      terminate() {
        this.terminated = true;
      },
      say(data) {
        this.onmessage({ data });
      },
    };
    made.push(w);
    return w;
  };
  return { made, make };
}

const settle = () => new Promise((r) => setImmediate(r));

await test('the host asks one request at a time, each after the worker opened the file', async () => {
  const t = timers();
  const f = fakeWorkers();
  const infos = [];
  const host = new PdfHost({ makeWorker: f.make, module: 'M', bytes: 'B', onInfo: (i) => infos.push(i), ...t });
  assert.deepEqual(f.made[0].sent, [{ type: 'init', module: 'M', bytes: 'B' }]);
  const a = host.request({ type: 'render', page: 0, scale: 1 });
  const b = host.request({ type: 'text', page: 1 });
  assert.equal(f.made[0].sent.length, 1, 'nothing before the info');
  f.made[0].say({ type: 'info', info: { ok: true, pages: [] } });
  assert.deepEqual(infos, [{ ok: true, pages: [] }]);
  assert.deepEqual(f.made[0].sent[1], { type: 'render', page: 0, scale: 1, id: 1 });
  assert.equal(f.made[0].sent.length, 2, 'one at a time');
  f.made[0].say({ type: 'done', id: 1, width: 1, height: 1, rgba: [] });
  assert.equal((await a).width, 1);
  assert.deepEqual(f.made[0].sent[2], { type: 'text', page: 1, id: 2 });
  f.made[0].say({ type: 'done', id: 2, text: 'hi' });
  assert.equal((await b).text, 'hi');
  assert.equal(host.starts, 1);
});

await test('a request past its timeout answers why, and the next goes to a new worker', async () => {
  const t = timers();
  const f = fakeWorkers();
  const infos = [];
  const host = new PdfHost({ makeWorker: f.make, module: 'M', bytes: 'B', onInfo: (i) => infos.push(i), ...t });
  f.made[0].say({ type: 'info', info: { ok: true, pages: [] } });
  const slow = host.request({ type: 'render', page: 4, scale: 1 });
  const next = host.request({ type: 'render', page: 5, scale: 1 });
  t.fire();
  assert.deepEqual(await slow, { error: TOO_LONG, lost: true });
  assert.ok(f.made[0].terminated);
  assert.equal(f.made.length, 2, 'a new worker');
  assert.deepEqual(f.made[1].sent, [{ type: 'init', module: 'M', bytes: 'B' }], 'it opens the same bytes');
  f.made[1].say({ type: 'info', info: { ok: true, pages: [] } });
  assert.equal(infos.length, 1, 'the view hears the first info only');
  assert.equal(f.made[1].sent[1].page, 5);
  // A late answer from the old worker is ignored.
  f.made[0].say({ type: 'done', id: 1, width: 9 });
  f.made[1].say({ type: 'done', id: f.made[1].sent[1].id, width: 2, height: 2, rgba: [] });
  assert.equal((await next).width, 2);
});

await test('a trap or a dead worker answers why and starts another', async () => {
  const t = timers();
  const f = fakeWorkers();
  const host = new PdfHost({ makeWorker: f.make, module: 'M', bytes: 'B', onInfo: () => {}, ...t });
  f.made[0].say({ type: 'info', info: { ok: true, pages: [] } });
  const trapped = host.request({ type: 'render', page: 0, scale: 1 });
  f.made[0].say({ type: 'trap', id: 1, error: 'unreachable' });
  assert.deepEqual(await trapped, { error: `${STOPPED} (unreachable)`, lost: true });
  assert.equal(f.made.length, 2);
  f.made[1].say({ type: 'info', info: { ok: true, pages: [] } });
  const died = host.request({ type: 'render', page: 1, scale: 1 });
  f.made[1].onerror({ preventDefault() {} });
  assert.deepEqual(await died, { error: STOPPED, lost: true });
  assert.equal(f.made.length, 3);
  assert.equal(t.count(), 1, "the new worker's open is timed");
});

await test('a worker that dies opening the file is not started again', async () => {
  const t = timers();
  const f = fakeWorkers();
  const infos = [];
  const host = new PdfHost({ makeWorker: f.make, module: 'M', bytes: 'B', onInfo: (i) => infos.push(i), ...t });
  const waiting = host.request({ type: 'render', page: 0, scale: 1 });
  f.made[0].say({ type: 'trap', error: 'unreachable' });
  assert.deepEqual(infos, [{ ok: false, error: `${STOPPED} (unreachable)`, locked: false }]);
  assert.equal((await waiting).lost, true);
  assert.equal(f.made.length, 1, 'no second worker');
  assert.equal((await host.request({ type: 'text', page: 0 })).lost, true);
  // And one that spins opening it is ended at the timeout.
  const g = fakeWorkers();
  const spun = [];
  new PdfHost({ makeWorker: g.make, module: 'M', bytes: 'B', onInfo: (i) => spun.push(i), ...t });
  t.fire();
  await settle();
  assert.deepEqual(spun, [{ ok: false, error: TOO_LONG, locked: false }]);
  assert.ok(g.made[0].terminated);
});

console.log(`${passed} passed (${wasmPath})`);
