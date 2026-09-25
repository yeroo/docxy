// Unit tests for htmlbundle/web/engine.js — the DOM-free glue the page runs —
// loaded exactly as the page loads it (a classic script defining
// globalThis.DocxyEngine). Run: node --test test/
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const here = dirname(fileURLToPath(import.meta.url));
const web = join(here, '..', '..', 'htmlbundle', 'web');
vm.runInThisContext(readFileSync(join(web, 'engine.js'), 'utf8'), { filename: 'engine.js' });
const E = globalThis.DocxyEngine;
const snapshot = JSON.parse(readFileSync(join(web, 'ribbon-docx.json'), 'utf8'));
const fixture = (name) => readFileSync(join(here, 'fixtures', name));

// ---- the page's self-rebuild equals htmlbundle::rewrap ------------------------

// An element's raw text, as element.textContent gives it for <script>/<style>.
function textOf(html) {
  return (id) => {
    const m = html.match(new RegExp(`<(script|style)[^>]*\\sid="${id}"[^>]*>([\\s\\S]*?)</(script|style)>`));
    if (!m) throw new Error('no element #' + id);
    return m[2];
  };
}

test('rebuildFile reproduces htmlbundle::rewrap byte for byte', () => {
  const before = fixture('rewrap-before.html').toString('utf8');
  const payload = new Uint8Array(fixture('rewrap-payload.bin'));
  const after = fixture('rewrap-after.html').toString('utf8');
  const built = E.rebuildFile(textOf(before), payload);
  assert.equal(built.html, after);
  // And the rebuilt file reads back: the new package, the original's hash kept.
  const read = E.readPayload(textOf(built.html)('docxy-payload'));
  assert.deepEqual(Array.from(read.bytes), Array.from(payload));
  const orig = E.readPayload(textOf(before)('docxy-payload'));
  assert.equal(E.metaGet(read.meta, 'sourceSha256'), E.metaGet(orig.meta, 'sourceSha256'));
  assert.notEqual(E.metaGet(read.meta, 'payloadSha256'), E.metaGet(orig.meta, 'payloadSha256'));
});

test('rebuilding is stable: rebuild(rebuild(x)) == rebuild(x)', () => {
  const after = fixture('rewrap-after.html').toString('utf8');
  const payload = new Uint8Array(fixture('rewrap-payload.bin'));
  assert.equal(E.rebuildFile(textOf(after), payload).html, after);
});

test('a tampered payload is refused', () => {
  const before = fixture('rewrap-before.html').toString('utf8');
  const text = textOf(before)('docxy-payload');
  const [meta, b64] = text.trim().split('\n');
  const evil = '\n' + meta + '\n' + E.b64encode(new TextEncoder().encode('PK evil')) + '\n';
  assert.throws(() => E.readPayload(evil), /integrity check/);
  assert.equal(E.readPayload(text).bytes.length > 0, true);
  assert.ok(b64.length > 0);
});

test('metaJson escapes like htmlbundle::Meta::to_json', () => {
  const fields = [['k', 'a"b\\c\n<d>&e \u{1F600}\u0007']];
  assert.equal(E.metaJson(fields),
    '{"k":"a\\"b\\\\c\\n\\u003cd\\u003e\\u0026e\\u2028\u{1F600}\\u0007"}');
  assert.deepEqual(E.parseMeta(E.metaJson(fields)), fields);
});

test('fillTemplate is single-pass like htmlbundle::fill', () => {
  assert.equal(E.fillTemplate('a{{x}}b{{y}}c{{', { x: '{{y}}', y: 'Y' }), 'a{{y}}bYc{{');
  assert.equal(E.fillTemplate('{{nope}}', {}), '{{nope}}');
  assert.equal(E.fillTemplate('$1 {{x}}', { x: "$& $'" }), "$1 $& $'");
});

// ---- primitives ------------------------------------------------------------------

test('sha256 matches the FIPS vectors', () => {
  const enc = (s) => new TextEncoder().encode(s);
  assert.equal(E.sha256Hex(enc('')), 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
  assert.equal(E.sha256Hex(enc('abc')), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  assert.equal(E.sha256Hex(enc('abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq')),
    '248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1');
  assert.equal(E.sha256Hex(new Uint8Array(1_000_000).fill(0x61)),
    'cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0');
  for (const n of [55, 56, 63, 64, 65]) {
    assert.equal(E.sha256Hex(new Uint8Array(n).fill(0x61)).length, 64);
  }
});

test('base64 round-trips every byte', () => {
  const bytes = new Uint8Array(1000).map((_, i) => i & 255);
  assert.deepEqual(Array.from(E.b64decode(E.b64encode(bytes))), Array.from(bytes));
  assert.equal(E.b64encode(new TextEncoder().encode('foobar')), 'Zm9vYmFy');
});

test('UTF-16 and editor (code point) offsets convert both ways', () => {
  const s = 'a\u{1F600}b\u{1F468}‍\u{1F469}c';
  const points = Array.from(s);
  for (let n = 0; n <= points.length; n++) {
    const u = E.utf16Offset(s, n);
    assert.equal(u, points.slice(0, n).join('').length);
    assert.equal(E.scalarOffset(s, u), n);
  }
  // Inside a surrogate pair counts as before the pair.
  assert.equal(E.scalarOffset(s, 2), 1);
});

// ---- the ribbon: every suite command is handled or explained -----------------------

function snapshotActs() {
  const acts = new Set();
  const cmd = (c) => acts.add(c.act);
  const groups = (tab) => tab.groups.forEach((g) => {
    if (g.launcher) acts.add(g.launcher);
    g.items.forEach((c) => {
      if (c.cmd) cmd(c.cmd);
      (c.cmds || []).forEach(cmd);
      if (c.primary) cmd(c.primary);
      (c.menu || []).forEach(cmd);
      (c.rows || []).forEach((row) => row.forEach((cell) => cmd(cell.cmd)));
      if (c.kind === 'gallery') c.items.forEach((it) => acts.add(it.act));
    });
  });
  snapshot.tabs.filter((t) => t.kind === 'ribbon').forEach(groups);
  snapshot.contextual.forEach(groups);
  return acts;
}

test('every act in the suite ribbon is wired or listed as unsupported', () => {
  const acts = snapshotActs();
  assert.ok(acts.size > 40, `expected the full docx ribbon, got ${acts.size} acts`);
  const missing = [...acts].filter((a) => !(a in E.ACT_OPS) && !(a in E.BROWSER_UNSUPPORTED));
  assert.deepEqual(missing, [], 'acts with no browser behaviour and no reason');
  const both = [...acts].filter((a) => a in E.ACT_OPS && a in E.BROWSER_UNSUPPORTED);
  assert.deepEqual(both, []);
  // No stale entries for acts the suite no longer has.
  const stale = Object.keys(E.ACT_OPS).concat(Object.keys(E.BROWSER_UNSUPPORTED)).filter((a) => !acts.has(a));
  assert.deepEqual(stale, []);
});

test('every icon the ribbon and QAT use is in the snapshot', () => {
  const used = new Set(snapshot.qat.map((q) => q.icon));
  const walk = (v) => {
    if (Array.isArray(v)) v.forEach(walk);
    else if (v && typeof v === 'object') {
      if (typeof v.icon === 'string' && v.act) used.add(v.icon);
      Object.values(v).forEach(walk);
    }
  };
  walk(snapshot.tabs);
  walk(snapshot.contextual);
  const missing = [...used].filter((i) => !snapshot.icons[i] || !snapshot.icons[i].startsWith('<svg'));
  assert.deepEqual(missing, []);
});

test('engine commands are well-formed verbs', () => {
  const verbs = new Set(['bold', 'italic', 'underline', 'strike', 'vertalign', 'fontsize', 'align',
    'style', 'nospacing', 'hrule', 'selectall', 'case', 'list', 'indent', 'clearfmt', 'sort', 'borders']);
  for (const [act, op] of Object.entries(E.ACT_OPS)) {
    if (typeof op !== 'string') continue;
    assert.ok(verbs.has(op.split('\t')[0]), `${act}: unknown verb in ${JSON.stringify(op)}`);
  }
});
