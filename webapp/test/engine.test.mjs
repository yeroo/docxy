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
      if (c.kind === 'dropdown') c.items.forEach(cmd);
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

// ---- ruler math (View > Ruler) -------------------------------------------------
//
// The numbers below are the suite's oracle (suite/docxy/src/main.rs): snap_twips
// (~24480), tw_px/px_tw (~4608), ruler_drag_result (~5604) and the page's own
// paraHtml effective indent (app.js), which the markers must match.

test('ruler snapTwips matches the suite 180-twip grid', () => {
  const R = E.ruler;
  for (const [v, want] of [[0, 0], [89, 0], [90, 180], [179, 180], [180, 180], [269, 180],
    [270, 360], [-89, 0], [-90, -180], [-179, -180], [-269, -180], [-270, -360], [720, 720]]) {
    assert.equal(R.snapTwips(v), want, `snapTwips(${v})`);
  }
  assert.equal(R.snapTwips(2147483647), 2147483647, 'clamped like the Rust i32 cast');
  assert.equal(R.snapTwips(-2147483648), -2147483648, 'clamped like the Rust i32 cast');
});

test('ruler twToPx/pxToTw round-trip at every zoom', () => {
  const R = E.ruler;
  assert.equal(R.twToPx(720, 1), 48, 'tw_px: t * zoom / 15');
  for (const zoom of [0.5, 1, 1.5, 3]) {
    for (const t of [0, 180, -360, 720, 1440]) {
      assert.equal(R.pxToTw(R.twToPx(t, zoom), zoom), t, `round trip ${t} @ ${zoom}`);
    }
  }
  // Rust f32::round is half away from zero; Math.round is not.
  assert.equal(R.pxToTw(24.5, 1), 368);
  assert.equal(R.pxToTw(-24.5, 1), -368);
});

test('ruler effIndent mirrors paraHtml exactly', () => {
  const R = E.ruler;
  const ind = (left, right, first) => ({ left, right, first });
  const list = { left: 360, first: -360, right: 0, listLeft: 360, list: true };
  assert.deepEqual(R.effIndent({ list: '•', level: 0, segs: [] }), list);
  assert.deepEqual(R.effIndent({ list: '•', level: 2, segs: [] }),
    { left: 1080, first: -360, right: 0, listLeft: 1080, list: true });
  // Explicit left suppresses the synthetic indent; the drag floor is then the
  // smallest non-zero grid value (only an exact 0 is re-synthesized).
  assert.deepEqual(R.effIndent({ list: '•', level: 1, ind: ind(720, 0, 0), segs: [] }),
    { left: 720, first: -360, right: 0, listLeft: 180, list: true });
  // Non-list paragraphs use the raw values.
  assert.deepEqual(R.effIndent({ ind: ind(1440, 1440, -720) }),
    { left: 1440, first: -720, right: 1440, listLeft: 0, list: false });
  assert.deepEqual(R.effIndent({}),
    { left: 0, first: 0, right: 0, listLeft: 0, list: false });
  assert.deepEqual(R.effIndent({ ind: ind(720, 0, -360) }),
    { left: 720, first: -360, right: 0, listLeft: 0, list: false });
});

test('ruler dragResult mirrors ruler_drag_result', () => {
  const R = E.ruler;
  const eff = (left, first, right, extra = {}) => ({ left, first, right, listLeft: 0, list: false, ...extra });
  // 720 twips of drag -> First(720), one firstline command.
  assert.deepEqual(R.dragResult('first', eff(0, 0, 0), 48, 1),
    { cmd: 'firstline\t720', marker: 720, guide: 720 });
  // The first-line marker's absolute position clamps at 0.
  assert.deepEqual(R.dragResult('first', eff(720, -720, 0), -48, 1),
    { cmd: 'firstline\t-720', marker: 0, guide: 0 });
  // A plain left drag snaps and writes first = max(first, -marker).
  assert.deepEqual(R.dragResult('left', eff(0, 0, 0), 48, 1),
    { cmd: 'setind\t720\t0', marker: 720, guide: 720 });
  // A list paragraph whose synthetic left applies clamps at it (level 0 and 2).
  assert.deepEqual(R.dragResult('left', eff(360, -360, 0, { listLeft: 360, list: true }), -48, 1),
    { cmd: 'setind\t360\t-360', marker: 360, guide: 360 });
  assert.deepEqual(R.dragResult('left', eff(1080, -360, 0, { listLeft: 1080, list: true }), -96, 1),
    { cmd: 'setind\t1080\t-360', marker: 1080, guide: 1080 });
  // A list with an explicit non-zero left floors at 180: free drags land on
  // the grid (level 2, explicit 720 dragged left -> 540; a short rightward
  // drag stays near 720 instead of jumping to the synthetic 1080).
  assert.deepEqual(R.dragResult('left', eff(720, -360, 0, { listLeft: 180, list: true }), -12, 1),
    { cmd: 'setind\t540\t-360', marker: 540, guide: 540 });
  assert.deepEqual(R.dragResult('left', eff(720, -360, 0, { listLeft: 180, list: true }), 12, 1),
    { cmd: 'setind\t900\t-360', marker: 900, guide: 900 });
  // A list first-line drag landing exactly on the left indent (first = 0)
  // nudges to the nearest non-zero grid value in the drag's direction —
  // paraHtml would otherwise re-render it at the synthetic -360, two grid
  // steps from the drop. The marker/guide report the nudged position.
  assert.deepEqual(R.dragResult('first', eff(360, 180, 0, { listLeft: 360, list: true }), -12, 1),
    { cmd: 'firstline\t-180', marker: 180, guide: 180 });
  assert.deepEqual(R.dragResult('first', eff(360, -180, 0, { listLeft: 360, list: true }), 12, 1),
    { cmd: 'firstline\t180', marker: 540, guide: 540 });
  // Right shrinks with a rightward drag and clamps at 0.
  assert.deepEqual(R.dragResult('right', eff(0, 0, 1440), -48, 1),
    { cmd: 'rightind\t2160', marker: 2160, guide: 2160 });
  assert.deepEqual(R.dragResult('right', eff(0, 0, 180), 48, 1),
    { cmd: 'rightind\t0', marker: 0, guide: 0 });
});

test('ruler geometry places markers and tab stops at the suite positions', () => {
  const R = E.ruler;
  // Page rect in viewport coords, unzoomed padding px, then the zoom.
  const g1 = R.geometry({ left: 100, right: 916, width: 816 }, 96, 96, 1,
    { left: 720, first: -360, right: 0 }, [{ pos: 720, a: 'l' }, { pos: 4680, a: 'c' }]);
  assert.equal(g1.contentX, 196, 'page left + margin * zoom');
  assert.equal(g1.contentRight, 820, 'page right - margin * zoom');
  assert.equal(g1.leftX, 196 + 48);
  assert.equal(g1.firstX, 196 + 48 - 24);
  assert.equal(g1.rightX, 820);
  assert.deepEqual(g1.tabs, [196 + 48, 196 + 4680 / 15]);
  // Zoom 2: the rect is already zoomed; the zoom factor converts the unzoomed padding.
  const g2 = R.geometry({ left: 100, right: 1732, width: 1632 }, 96, 96, 2,
    { left: 720, first: -360, right: 0 }, [{ pos: 720, a: 'l' }]);
  assert.equal(g2.contentX, 292);
  assert.equal(g2.contentRight, 1732 - 192);
  assert.equal(g2.leftX, 292 + 720 * 2 / 15);
  assert.equal(g2.firstX, 292 + (720 - 360) * 2 / 15);
  assert.deepEqual(g2.tabs, [292 + 720 * 2 / 15]);
  // A non-zero right indent pulls the right marker in from the content edge.
  const g3 = R.geometry({ left: 100, right: 916, width: 816 }, 96, 96, 1,
    { left: 0, first: 0, right: 360 }, null);
  assert.equal(g3.rightX, 820 - 360 / 15);
});
