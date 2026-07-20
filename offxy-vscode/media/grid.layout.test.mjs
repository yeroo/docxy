// Layout regression test for the grid webview: the painted cell layer must be
// aligned with the sticky header gutters, and screen-space clicks must map to
// the cell the HEADERS say is there. Guards the gutter-offset bug where the
// whole body layer (#cells, selection, editor, hit-testing) rendered one
// header-gutter up-left of the row/column headers, hiding row 1 / column A
// under the sticky bars.
//
// Runs grid.js in Node against the REAL gridwasm.wasm with a minimal DOM stub
// (elements record style/children; no rendering). Screen positions are
// computed from the same CSS constants grid.css pins — the test re-reads
// grid.css and fails loudly if those constants drift.
//
//   node media/grid.layout.test.mjs        (wired as `npm run test:grid-layout`)

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));

// ---- CSS layout constants (single source: grid.css) -------------------------
const css = readFileSync(join(here, 'grid.css'), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
function cssNum(selector, prop) {
  // Scan every rule whose selector list names `selector`; return the first
  // declaration of `prop` (handles combined rules like "#colhdr, #rowhdr").
  let found = false;
  for (const rule of css.matchAll(/([^{}]+)\{([^}]*)\}/g)) {
    if (!rule[1].split(',').some((s) => s.trim() === selector)) continue;
    found = true;
    const m = rule[2].match(new RegExp(`(?:^|[;\\s])${prop}:\\s*(-?\\d+)px`));
    if (m) return parseInt(m[1], 10);
  }
  assert.ok(found, `grid.css must have a ${selector} rule`);
  return 0;
}
const WRAP_TOP = cssNum('#gridwrap', 'top');       // 58: below the toolbar + formula bar
const COLHDR_LEFT = cssNum('#colhdr', 'left');     // 44: row-number gutter width
const COLHDR_TOP = cssNum('#colhdr', 'top');       // 58
const COLHDR_H = cssNum('#colhdr', 'height');      // 22: column-header band height
const ROWHDR_TOP = cssNum('#rowhdr', 'top');       // 80 = 58 + 22
const CELLS_TOP = cssNum('#cells', 'top');         // the fix under test
const CELLS_LEFT = cssNum('#cells', 'left');       // the fix under test
assert.equal(ROWHDR_TOP, COLHDR_TOP + COLHDR_H, 'rowhdr sits directly below colhdr');

// ---- minimal DOM stub -------------------------------------------------------
const byId = new Map();
class El {
  constructor(tag) {
    this.tagName = tag;
    this.style = {};
    this.children = [];
    this.listeners = new Map();
    this.dataset = {};
    this.className = '';
    // Real classList backed by `className` (a space-separated string), same
    // source of truth production code reads/writes directly in a few places
    // (e.g. `el.className = 'cell'`) — toggle(cls, force) matches the real
    // DOM signature so grid.js's toolbar pressed-state code (paint()) can
    // use it exactly as it would in a browser.
    const parts = () => (this.className || '').split(' ').filter(Boolean);
    this.classList = {
      add: (c) => { const p = parts(); if (!p.includes(c)) { p.push(c); this.className = p.join(' '); } },
      remove: (c) => { this.className = parts().filter((x) => x !== c).join(' '); },
      contains: (c) => parts().includes(c),
      toggle: (c, force) => {
        const has = parts().includes(c);
        const want = force === undefined ? !has : !!force;
        if (want) this.classList.add(c); else this.classList.remove(c);
      },
    };
    this._tc = '';
    this.scrollTop = 0;
    this.scrollLeft = 0;
    this.clientWidth = 800;
    this.clientHeight = 600;
    this.isConnected = true;
  }
  set id(v) { this._id = v; byId.set(v, this); }
  get id() { return this._id; }
  // DOM coerces textContent assignments to string — mirror that, or numeric
  // assignments (row labels: `el.textContent = r + 1`) break strict compares.
  set textContent(v) { this._tc = String(v); }
  get textContent() { return this._tc; }
  appendChild(c) { this.children.push(c); return c; }
  append(...cs) { for (const c of cs) this.appendChild(c); }
  replaceChildren(frag) { this.children = frag ? [...frag.children] : []; }
  remove() { this.isConnected = false; }
  addEventListener(t, fn) { this.listeners.set(t, fn); }
  fire(t, ev) { const fn = this.listeners.get(t); if (fn) fn(ev); }
  focus() {} select() {} setSelectionRange() {} click() {}
  closest() { return null; }
  getBoundingClientRect() {
    // gridwrap is the only element the code measures; it fills the page below
    // the formula bar (CSS: top:28, left:0).
    return { left: 0, top: WRAP_TOP };
  }
  set innerHTML(html) {
    this.children = [];
    for (const m of html.matchAll(/id="(\w+)"/g)) {
      const el = new El('div');
      el.id = m[1];
      this.appendChild(el);
    }
  }
}
const document = {
  body: new El('body'),
  activeElement: null,
  getElementById: (id) => byId.get(id) ?? null,
  createElement: (tag) => new El(tag),
  createDocumentFragment: () => new El('#fragment'),
  addEventListener: () => {},
};
const posted = [];
const winListeners = new Map();
const windowObj = {
  __OFFXY__: { wasmUri: 'gridwasm.wasm' },
  addEventListener: (t, fn) => winListeners.set(t, fn),
  dispatchMessage: (data) => winListeners.get('message')({ data }),
};
const sandbox = {
  window: windowObj,
  document,
  acquireVsCodeApi: () => ({ postMessage: (m) => posted.push(m) }),
  fetch: async () => ({ arrayBuffer: async () => readFileSync(join(here, 'gridwasm.wasm')).buffer }),
  WebAssembly, TextEncoder, TextDecoder, JSON, Math, Date, console,
  setTimeout, clearTimeout,
  atob: (b) => Buffer.from(b, 'base64').toString('binary'),
  btoa: (s) => Buffer.from(s, 'binary').toString('base64'),
};
vm.createContext(sandbox);
vm.runInContext(readFileSync(join(here, 'grid.js'), 'utf8'), sandbox, { filename: 'grid.js' });

// boot() is async (fetch + instantiate) — give it a tick.
await new Promise((r) => setTimeout(r, 50));
assert.ok(winListeners.has('message'), 'grid.js booted and listens for host messages');

// ---- open a real workbook and land a ctl edit -------------------------------
const blank = readFileSync(join(here, '..', 'mcp', 'templates', 'blank.xlsx'));
windowObj.dispatchMessage({ type: 'open', data: blank.toString('base64') });
windowObj.dispatchMessage({
  type: 'ctl', requestId: 1, repaint: true,
  payload: JSON.stringify({ verb: 'range.set', args: { start: 'A1', rows: [['name', 'amount'], ['alice', '10'], ['bob', '20']] } }),
});
const ctlReply = posted.find((m) => m.type === 'ctlResult' && m.requestId === 1);
assert.ok(ctlReply && JSON.parse(ctlReply.payload).ok === true, 'range.set landed');

const wrap = byId.get('gridwrap');
const cells = byId.get('cells');
const colhdr = byId.get('colhdr');
const rowhdr = byId.get('rowhdr');
const px = (v) => parseInt(v ?? '0', 10) || 0;

// Screen-space positions. Headers live in page space (#colhdr at left:44,
// #rowhdr at top:50); the cell layer lives inside #gridwrap (page top:28)
// offset by #cells' own top/left.
const cellScreenX = (el) => 0 + CELLS_LEFT + px(el.style.left) - wrap.scrollLeft;
const cellScreenY = (el) => WRAP_TOP + CELLS_TOP + px(el.style.top) - wrap.scrollTop;
const colLabelScreenX = (el) => COLHDR_LEFT + px(el.style.left);
const rowLabelScreenY = (el) => ROWHDR_TOP + px(el.style.top);

// ---- 1. header/cell alignment ----------------------------------------------
const cellA1 = cells.children.find((c) => c.textContent === 'name');
assert.ok(cellA1, 'A1 ("name") was painted');
const labelA = colhdr.children.find((c) => c.textContent === 'A');
const label1 = rowhdr.children.find((c) => c.textContent === '1');
assert.ok(labelA && label1, 'headers painted A and 1');
assert.equal(cellScreenX(cellA1), colLabelScreenX(labelA),
  'cell A1 must be horizontally aligned with the "A" column header');
assert.equal(cellScreenY(cellA1), rowLabelScreenY(label1),
  'cell A1 must be vertically aligned with the "1" row header');
// The first row/column must not be covered by the sticky header bars.
assert.ok(cellScreenY(cellA1) >= ROWHDR_TOP,
  'row 1 must start below the column-header band, not underneath it');
assert.ok(cellScreenX(cellA1) >= COLHDR_LEFT,
  'column A must start right of the row-number gutter, not underneath it');

// ---- 2. click round-trip: clicking where the HEADERS say B2 selects B2 -----
const cellB2 = cells.children.find((c) => c.textContent === '10');
assert.ok(cellB2, 'B2 ("10") was painted');
const clickX = cellScreenX(cellB2) + 5;
const clickY = cellScreenY(cellB2) + 5;
wrap.fire('mousedown', { clientX: clickX, clientY: clickY, shiftKey: false, preventDefault() {} });
assert.equal(byId.get('cellref').textContent, 'B2',
  `clicking at the painted B2 position (${clickX},${clickY}) must select B2`);

// ---- 2b. full-viewport gridlines: empty cells in the window are gridded -----
// The data occupies A1:B3. Every OTHER cell in the fetched window must still
// get a border-only grid tile, so the sheet looks gridded like Excel rather
// than only where data happens to be.
const gridlines = byId.get('gridlines');
const gridAtEmpty = gridlines.children.find((c) =>
  px(c.style.top) === 4 * 22 && px(c.style.left) === 0 && (c.className || '').includes('cell'));
assert.ok(gridAtEmpty, 'the A5 position inside the window must have a backdrop grid tile');
// The window is ~38 rows x ~23 cols (600/22 + overscan, 800/64 + overscan), so
// a fully-gridded window is hundreds of tiles. Guards against a regression
// back to "borders only where data is".
const gridTiles = gridlines.children.filter((c) => (c.className || '').includes('cell')).length;
assert.ok(gridTiles > 300,
  `window must be fully gridded (got ${gridTiles} backdrop tiles; expected the whole visible window)`);
// The backdrop is geometry-only: a selection-only repaint (same window) must
// NOT rebuild it. Re-select an existing cell and confirm the tile identity is
// preserved (replaceChildren was skipped).
const tileBefore = gridlines.children[0];
wrap.fire('mousedown', { clientX: clickX, clientY: clickY, shiftKey: false, preventDefault() {} });
assert.equal(gridlines.children[0], tileBefore,
  'a selection-only repaint (same window) must reuse the backdrop, not rebuild it');

// ---- 3. keyboard scroll-into-view clears the sticky headers -----------------
// Shrink the viewport so arrowing down forces scrolling, then check the active
// cell's screen rect is fully inside the uncovered band (below the column
// header, above the viewport bottom).
wrap.clientHeight = 6 * 22; // room for ~5 uncovered rows
for (let i = 0; i < 9; i++) {
  wrap.fire('keydown', {
    key: 'ArrowDown', ctrlKey: false, metaKey: false, shiftKey: false,
    preventDefault() {},
  });
}
const CELL_H = cssNum('.cell', 'height'); // 22: one grid row
assert.equal(byId.get('cellref').textContent, 'B11', 'arrowed down to B11');
const curTop = WRAP_TOP + CELLS_TOP + 10 * CELL_H - wrap.scrollTop; // row 11 => r=10
assert.ok(curTop >= WRAP_TOP + COLHDR_H,
  `active cell (top ${curTop}) must not be under the column-header band`);
assert.ok(curTop + CELL_H <= WRAP_TOP + wrap.clientHeight,
  `active cell (bottom ${curTop + CELL_H}) must be inside the viewport`);

// ---- 4. toolbar: exists, wired to the fmt/decimals/autosum verbs, and a --
//        button click doesn't disturb the grid selection --------------------
const toolbar = byId.get('toolbar');
assert.ok(toolbar, '#toolbar must exist');
const toolbarButtons = toolbar.children.filter((c) => c.tagName === 'button');
assert.ok(
  toolbarButtons.length > 8,
  `toolbar must have more than 8 buttons (got ${toolbarButtons.length})`,
);

const boldBtn = toolbarButtons.find((b) => b.title === 'Bold');
assert.ok(boldBtn, 'a Bold button must exist');
const cellrefBeforeToolbarClick = byId.get('cellref').textContent;
boldBtn.fire('click');
assert.equal(
  byId.get('cellref').textContent, cellrefBeforeToolbarClick,
  'a toolbar button click must not change the selection',
);
assert.ok(boldBtn.classList.contains('on'), 'Bold button must show pressed state right after toggling on');

// Confirm the underlying cell (not just the button's own class) actually
// flipped bold, via a follow-up ctl `cell.get` read — the same round trip an
// agent or the host would use.
let ctlSeq = 1000;
function ctlCellGet(ref) {
  const id = ++ctlSeq;
  windowObj.dispatchMessage({
    type: 'ctl', requestId: id, repaint: false,
    payload: JSON.stringify({ verb: 'cell.get', args: { ref } }),
  });
  return JSON.parse(posted.find((m) => m.type === 'ctlResult' && m.requestId === id).payload);
}
const activeRef = byId.get('cellref').textContent;
const afterToggleOn = ctlCellGet(activeRef);
assert.ok(
  afterToggleOn.ok && afterToggleOn.format && afterToggleOn.format.bold === true,
  `active cell ${activeRef} must be bold after the toolbar click: ${JSON.stringify(afterToggleOn)}`,
);

// Toggle back off (active-cell-state semantics: the active cell is now bold,
// so this click's target is "unbold") and confirm the pressed state clears.
boldBtn.fire('click');
assert.ok(!boldBtn.classList.contains('on'), 'Bold button must clear pressed state after toggling off');
const afterToggleOff = ctlCellGet(activeRef);
assert.ok(
  !(afterToggleOff.format && afterToggleOff.format.bold === true),
  `active cell ${activeRef} must not be bold after toggling back off: ${JSON.stringify(afterToggleOff)}`,
);

// ---- 5. the edits counter, not a verb-name regex, decides {type:'edit'} ---
//        (the AutoSum-desyncs-undo fix: a dispatched-but-no-op command must
//        NOT tell the host an edit happened) --------------------------------
const editMsgCount = () => posted.filter((m) => m.type === 'edit').length;

// `activeRef` (B11, from section 4) is isolated: nothing above it in column
// B (the data ends at row 3) and nothing to its left in row 11 — AutoSum
// there is a legitimate no-op.
assert.equal(activeRef, 'B11', 'sanity: still on the isolated B11 from section 4');
const autosumBtn = toolbarButtons.find((b) => b.title === 'AutoSum');
assert.ok(autosumBtn, 'an AutoSum button must exist');

const editsBeforeNoopAutosum = editMsgCount();
autosumBtn.fire('click');
assert.equal(
  editMsgCount(), editsBeforeNoopAutosum,
  'AutoSum with nothing to sum must NOT post {type:"edit"} — no real undo group was pushed',
);
assert.equal(byId.get('cellref').textContent, activeRef, 'AutoSum no-op must not move the selection');
const noopCell = ctlCellGet(activeRef);
assert.equal(noopCell.text, '', 'AutoSum no-op must not write anything to the active cell');

// Immediately after, a REAL edit (fmt) posts exactly one {type:'edit'} — the
// counter isn't stuck off after a no-op, and doesn't over-fire either.
boldBtn.fire('click');
assert.equal(
  editMsgCount(), editsBeforeNoopAutosum + 1,
  'a real fmt edit right after a no-op must post exactly one {type:"edit"}',
);
boldBtn.fire('click'); // toggle back off, so this cell's state matches how it started

// Copy never mutates -> never posts edit. Cut DOES (a real cell has content
// to clear) -> posts exactly one. Re-selects B2 (section 2's data cell) to
// exercise cut against a non-blank cell.
const copyBtn = toolbarButtons.find((b) => b.title === 'Copy');
const cutBtn = toolbarButtons.find((b) => b.title === 'Cut');
assert.ok(copyBtn && cutBtn, 'Cut/Copy buttons must exist');
const editsBeforeCopy = editMsgCount();
copyBtn.fire('click');
assert.equal(editMsgCount(), editsBeforeCopy, 'Copy must not post {type:"edit"}');

// Reselect via keyboard, not the remembered `clickX`/`clickY` screen
// coordinates from section 2 — the viewport has since scrolled (section 3),
// so the same screen position no longer maps to the same cell.
wrap.fire('keydown', { key: 'Home', ctrlKey: true, metaKey: false, shiftKey: false, preventDefault() {} });
wrap.fire('keydown', { key: 'ArrowDown', ctrlKey: false, metaKey: false, shiftKey: false, preventDefault() {} });
wrap.fire('keydown', { key: 'ArrowRight', ctrlKey: false, metaKey: false, shiftKey: false, preventDefault() {} });
assert.equal(byId.get('cellref').textContent, 'B2', 'reselected B2 (has content) for the cut check');
const editsBeforeCut = editMsgCount();
cutBtn.fire('click');
assert.equal(editMsgCount(), editsBeforeCut + 1, 'Cut over a non-blank cell must post exactly one {type:"edit"}');
const afterCut = ctlCellGet('B2');
assert.equal(afterCut.text, '', 'Cut must have cleared B2');

console.log('grid layout OK: headers/cells aligned; full-viewport gridlines; click round-trip exact; scroll clears the header band; toolbar wired without disturbing selection; edits counter (not a verb regex) gates {type:"edit"}');
