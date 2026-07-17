// Offxy spreadsheet webview — drives the gridwasm engine (viewport protocol)
// and paints a virtualized HTML grid: sticky headers, formula bar, sheet tabs.
//
// The wasm ABI mirrors `gridwasm/src/lib.rs`:
//   grid_alloc(len)->ptr, grid_free(ptr,len)
//   grid_open(ptr,len)->handle, grid_close(handle)
//   grid_cmd(handle,ptr,len)->resultPtr   (viewport JSON)
//   grid_save(handle)->resultPtr          (xlsx bytes)
// A "result" buffer is [u32 little-endian length][payload bytes].

(function () {
  const vscode = acquireVsCodeApi();
  const $ = (id) => document.getElementById(id);

  const ROW_H = 22;     // must match grid.css .cell height
  const HDR_W = 44;     // row-number gutter width
  const COL_PX = 7.5;   // Excel column-width unit -> px (approximate MDW)
  const OVERSCAN = 5;   // extra rows/cols fetched around the visible window

  let ex = null;        // wasm exports
  let handle = 0;
  let view = null;      // last viewport JSON
  let colX = [0];       // prefix x of each col up to the fetched window's right edge
  let defW = 64;        // default column width in px

  const enc = new TextEncoder();
  const dec = new TextDecoder();

  // ---- wasm marshalling (same pattern as webview.js) -----------------------
  const mem = () => new Uint8Array(ex.memory.buffer);
  function writeBytes(u8) {
    const ptr = ex.grid_alloc(u8.length);
    mem().set(u8, ptr);
    return ptr;
  }
  function readResult(ptr) {
    const m = mem();
    const len = m[ptr] | (m[ptr + 1] << 8) | (m[ptr + 2] << 16) | (m[ptr + 3] << 24);
    const out = m.slice(ptr + 4, ptr + 4 + len);
    ex.grid_free(ptr, 4 + len);
    return out;
  }
  function cmd(str) {
    const u8 = enc.encode(str);
    const p = writeBytes(u8);
    const r = ex.grid_cmd(handle, p, u8.length);
    ex.grid_free(p, u8.length);
    view = JSON.parse(dec.decode(readResult(r)));
    paint();
    if (view.copied != null) {
      vscode.postMessage({ type: 'clipboard', text: view.copied });
    }
    return view;
  }

  // ---- geometry ------------------------------------------------------------
  function colWidthPx(c) {
    const e = (view.colw || []).find((x) => x.c === c);
    return e ? Math.max(24, Math.round(e.w * COL_PX)) : defW;
  }
  /** x position (px) of column c relative to the fetched window's left edge. */
  function rebuildColX(left, ncols) {
    colX = [0];
    for (let i = 0; i < ncols; i++) colX.push(colX[i] + colWidthPx(left + i));
  }
  function colAtX(x) {
    // x is relative to the sheet origin; walk from the fetched window.
    const { left } = win();
    let acc = left * defW; // approximation left of the window (uniform default)
    if (x < acc) return Math.max(0, Math.floor(x / defW));
    for (let i = 0; i < colX.length - 1; i++) {
      if (x < acc + colX[i + 1]) return left + i;
    }
    // last valid fetched column (colX has ncols+1 entries); clamp so an empty
    // window (colX.length === 1, i.e. no columns fetched yet) can't go below `left`.
    return Math.max(left, left + colX.length - 2);
  }

  function win() {
    const wrap = $('gridwrap');
    const top = Math.max(0, Math.floor(wrap.scrollTop / ROW_H) - OVERSCAN);
    const left = Math.max(0, Math.floor(wrap.scrollLeft / defW) - OVERSCAN);
    const nrows = Math.ceil(wrap.clientHeight / ROW_H) + 2 * OVERSCAN;
    const ncols = Math.ceil(wrap.clientWidth / defW) + 2 * OVERSCAN;
    return { top, left, nrows, ncols };
  }

  let viewTimer = 0;
  function requestView() {
    const { top, left, nrows, ncols } = win();
    cmd(`view\t${view ? view.active : 0}\t${top}\t${left}\t${nrows}\t${ncols}`);
  }
  function onScroll() {
    // Reposition the sticky headers immediately so they track the pointer
    // during a scroll gesture; the debounced view re-fetch below corrects
    // cell content and exact column widths once scrolling settles.
    repositionHeaders();
    clearTimeout(viewTimer);
    viewTimer = setTimeout(requestView, 50);
  }

  // ---- painting ------------------------------------------------------------
  function paint() {
    const { top, left, nrows, ncols } = win();
    rebuildColX(left, ncols);
    const originX = left * defW; // sheet-x of the fetched window's left edge
    const rows = Math.max(view.dims.rows + 50, top + nrows);
    const cols = Math.max(view.dims.cols + 10, left + ncols);
    $('spacer').style.height = rows * ROW_H + 'px';
    $('spacer').style.width = cols * defW + 'px';

    // cells
    const frag = document.createDocumentFragment();
    for (const cl of view.cells) {
      const el = document.createElement('div');
      el.className = 'cell';
      el.textContent = cl.t;
      if (cl.a === 'r') el.classList.add('num');
      if (cl.a === 'c') el.classList.add('ctr');
      if (cl.b) el.classList.add('b');
      if (cl.i) el.classList.add('i');
      if (cl.col) el.style.color = cl.col;
      if (cl.bg) el.style.background = cl.bg;
      el.style.top = cl.r * ROW_H + 'px';
      el.style.left = originX + colX[cl.c - left] + 'px';
      el.style.width = colWidthPx(cl.c) + 'px';
      frag.appendChild(el);
    }
    // selection + active cell boxes
    const sel = view.sel;
    const selEl = document.createElement('div');
    selEl.id = 'selbox';
    selEl.style.top = sel.r * ROW_H + 'px';
    selEl.style.left = originX + (colX[sel.c - left] ?? 0) + 'px';
    selEl.style.height = (sel.r2 - sel.r + 1) * ROW_H + 'px';
    let wsum = 0;
    for (let c = sel.c; c <= sel.c2; c++) wsum += colWidthPx(c);
    selEl.style.width = wsum + 'px';
    frag.appendChild(selEl);
    // active-cell outline: a tighter box than the (possibly multi-cell) selEl.
    const curEl = document.createElement('div');
    curEl.id = 'curbox';
    const curR = rowOfRef(), curC = refCol();
    curEl.style.top = curR * ROW_H + 'px';
    curEl.style.left = originX + (colX[curC - left] ?? 0) + 'px';
    curEl.style.height = ROW_H + 'px';
    curEl.style.width = colWidthPx(curC) + 'px';
    frag.appendChild(curEl);
    $('cells').replaceChildren(frag);
    // A repaint can land mid-edit (e.g. any select/scroll that fires while the
    // in-cell editor is open). replaceChildren() above would otherwise
    // silently detach editEl, permanently tripping startEdit's `if (editEl)
    // return;` guard for the rest of the session — re-append it, same pattern
    // as the docx webview's placeCaret() in media/webview.js. Removal from the
    // DOM also blurs it, so restore focus too — otherwise it floats on screen
    // while #gridwrap (and its onKeydown) silently owns the keyboard.
    if (editEl) { $('cells').appendChild(editEl); editEl.focus(); }

    paintHeaders(top, left, nrows, ncols, originX);
    paintTabs();
    $('cellref').textContent = view.cur.ref;
    if (document.activeElement !== $('fsrc')) $('fsrc').value = view.cur.src;
    // one-shot error from the wasm side (e.g. an invalid formula on `set`):
    // surface it as a tooltip on the cell reference box and a brief red flash
    // on the formula bar's border.
    $('cellref').title = view.err || '';
    if (view.err) {
      $('fsrc').style.borderColor = '#f14c4c';
      setTimeout(() => { $('fsrc').style.borderColor = ''; }, 600);
    }
  }

  /** Repaint just the sticky headers against the *current* scroll position,
   *  reusing the last-fetched view/colX. Cheap (no wasm call), so it can run
   *  synchronously on every scroll/resize event instead of waiting for the
   *  debounced `requestView()` round trip. Column data beyond the last
   *  fetched window falls back to the uniform `defW` approximation until the
   *  next view fetch corrects it (see the module doc's v1 note). */
  function repositionHeaders() {
    if (!view) return;
    const { top, left, nrows, ncols } = win();
    const originX = left * defW;
    paintHeaders(top, left, nrows, ncols, originX);
  }

  function paintHeaders(top, left, nrows, ncols, originX) {
    const wrap = $('gridwrap');
    const ch = document.createDocumentFragment();
    for (let i = 0; i < ncols; i++) {
      const c = left + i;
      const el = document.createElement('div');
      el.className = 'hcell';
      if (c >= view.sel.c && c <= view.sel.c2) el.classList.add('on');
      el.textContent = colName(c);
      // colX may be shorter than ncols right after a resize/scroll, before the
      // next view fetch rebuilds it — fall back to defW so headers never end
      // up at NaN instead of just being briefly approximate.
      el.style.left = originX + (colX[i] ?? i * defW) - wrap.scrollLeft + 'px';
      el.style.width = colWidthPx(c) + 'px';
      el.dataset.col = c;
      ch.appendChild(el);
    }
    $('colhdr').replaceChildren(ch);
    const rh = document.createDocumentFragment();
    for (let i = 0; i < nrows; i++) {
      const r = top + i;
      const el = document.createElement('div');
      el.className = 'hcell';
      if (r >= view.sel.r && r <= view.sel.r2) el.classList.add('on');
      el.textContent = r + 1;
      el.style.top = r * ROW_H - wrap.scrollTop + 'px';
      el.style.width = HDR_W + 'px';
      el.dataset.row = r;
      rh.appendChild(el);
    }
    $('rowhdr').replaceChildren(rh);
  }

  function paintTabs() {
    const bar = document.createDocumentFragment();
    view.sheets.forEach((name, i) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.textContent = name;
      if (i === view.active) b.classList.add('on');
      b.addEventListener('click', () => cmd(`sheet\tswitch\t${i}`) && requestView());
      b.addEventListener('dblclick', (e) => {
        e.stopPropagation();
        startSheetRename(b, i, name);
      });
      bar.appendChild(b);
    });
    const add = document.createElement('button');
    add.type = 'button';
    add.textContent = '+';
    add.title = 'Add sheet';
    add.addEventListener('click', () => {
      userCmd('sheet\tadd\tSheet' + (view.sheets.length + 1));
      requestView();
    });
    bar.appendChild(add);
    $('tabs').replaceChildren(bar);
  }

  /** Swap a sheet tab's button for an inline rename `<input>`. Enter commits
   *  `sheet\trename`; Escape (or losing the value) just repaints the tabs,
   *  which restores the original button since nothing was committed. */
  function startSheetRename(button, i, name) {
    const input = document.createElement('input');
    input.type = 'text';
    input.value = name;
    input.style.font = 'inherit';
    input.style.width = Math.max(40, button.offsetWidth) + 'px';
    button.replaceWith(input);
    input.focus();
    input.select();
    // Enter/Escape settle the rename synchronously; `done` keeps the blur
    // that follows (removing a focused input from the DOM fires one) from
    // triggering a redundant second requestView().
    let done = false;
    const restore = () => { if (!done) { done = true; requestView(); } };
    input.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        done = true;
        userCmd(`sheet\trename\t${i}\t${input.value}`);
        requestView();
      } else if (e.key === 'Escape') {
        e.preventDefault();
        restore();
      }
      e.stopPropagation();
    });
    // Clicking away must not leave the stale rename input stranded in the
    // tab strip — restore the normal tab buttons.
    input.addEventListener('blur', restore);
  }

  function colName(c) {
    let s = '';
    c += 1;
    while (c > 0) { c -= 1; s = String.fromCharCode(65 + (c % 26)) + s; c = Math.floor(c / 26); }
    return s;
  }

  // ---- selection + keyboard ------------------------------------------------
  function cellFromEvent(e) {
    const wrap = $('gridwrap');
    const rect = wrap.getBoundingClientRect();
    const x = e.clientX - rect.left + wrap.scrollLeft;
    const y = e.clientY - rect.top + wrap.scrollTop;
    return { r: Math.max(0, Math.floor(y / ROW_H)), c: colAtX(x) };
  }
  let dragging = false;
  function onMousedown(e) {
    if (!handle) return;
    // Clicking another cell commits the in-progress edit first (Excel
    // semantics), then proceeds to select the clicked cell. editEl's own
    // mousedown listener stops propagation, so this handler only ever sees
    // clicks outside the editor. This also compensates for the
    // e.preventDefault() below: it suppresses the browser's native
    // focus-shift-on-mousedown, so editEl would otherwise stay natively
    // focused (and never blur) even though the user clicked elsewhere.
    if (editEl) { commitEdit(); $('gridwrap').focus(); }
    const { r, c } = cellFromEvent(e);
    if (e.shiftKey) cmd(`select\t${rowOfRef()}\t${refCol()}\t${r}\t${c}`);
    else cmd(`select\t${r}\t${c}`);
    dragging = true;
    e.preventDefault();
  }
  function rowOfRef() {
    // cur.ref like "B4" — parse row/col back out
    const m = view.cur.ref.match(/^([A-Z]+)(\d+)$/);
    return m ? parseInt(m[2], 10) - 1 : 0;
  }
  function refCol() {
    const m = view.cur.ref.match(/^([A-Z]+)(\d+)$/);
    if (!m) return 0;
    let c = 0;
    for (const ch of m[1]) c = c * 26 + (ch.charCodeAt(0) - 64);
    return c - 1;
  }
  function onMousemove(e) {
    if (!dragging) return;
    const { r, c } = cellFromEvent(e);
    cmd(`select\t${rowOfRef()}\t${refCol()}\t${r}\t${c}`);
  }
  function onMouseup() { dragging = false; }

  function move(dr, dc, extend) {
    const r0 = rowOfRef(), c0 = refCol();
    if (extend) {
      const s = view.sel;
      const er = Math.max(0, (s.r2 === r0 ? s.r : s.r2) + dr);
      const ec = Math.max(0, (s.c2 === c0 ? s.c : s.c2) + dc);
      cmd(`select\t${r0}\t${c0}\t${er}\t${ec}`);
    } else {
      cmd(`select\t${Math.max(0, r0 + dr)}\t${Math.max(0, c0 + dc)}`);
    }
    ensureVisible();
  }
  function ensureVisible() {
    const wrap = $('gridwrap');
    const r = rowOfRef(), c = refCol();
    const y = r * ROW_H, x = c * defW;
    if (y < wrap.scrollTop) wrap.scrollTop = y;
    if (y + ROW_H > wrap.scrollTop + wrap.clientHeight) wrap.scrollTop = y + ROW_H - wrap.clientHeight;
    if (x < wrap.scrollLeft) wrap.scrollLeft = x;
    if (x + defW > wrap.scrollLeft + wrap.clientWidth) wrap.scrollLeft = x + defW - wrap.clientWidth;
  }

  function onKeydown(e) {
    // Belt-and-braces alongside paint()'s focus restore above: while the
    // in-cell editor is open, the grid-level handler must do nothing — the
    // editor's own keydown listener owns the keyboard. Without this, a
    // repaint that lands mid-edit without going through a caret click (e.g.
    // the debounced requestView() after scroll settles, or a host 'do'
    // undo/redo) could still leave focus on #gridwrap in some edge case,
    // and Delete/Backspace here would `clear` the selected range instead of
    // editing the floating input.
    if (editEl) return;
    if (!handle) return;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && ['z', 'y', 's'].includes(e.key.toLowerCase())) return; // VS Code owns
    const ext = e.shiftKey;
    switch (e.key) {
      case 'ArrowUp': e.preventDefault(); return move(-1, 0, ext);
      case 'ArrowDown': e.preventDefault(); return move(1, 0, ext);
      case 'ArrowLeft': e.preventDefault(); return move(0, -1, ext);
      case 'ArrowRight': e.preventDefault(); return move(0, 1, ext);
      case 'PageUp': e.preventDefault(); return move(-20, 0, ext);
      case 'PageDown': e.preventDefault(); return move(20, 0, ext);
      case 'Home': e.preventDefault();
        return mod ? cmd('select\t0\t0') && ensureVisible() : move(0, -refCol(), ext);
      default: break;
    }
    if (e.key === 'F2') { e.preventDefault(); return startEdit(null); }
    if (e.key === 'Enter') { e.preventDefault(); return startEdit(null); }
    if (e.key === 'Delete' || e.key === 'Backspace') {
      e.preventDefault();
      const s = view.sel;
      return userCmd(`clear\t${s.r}\t${s.c}\t${s.r2}\t${s.c2}`);
    }
    if (!mod && e.key.length === 1 && !e.altKey) {
      e.preventDefault();
      return startEdit(e.key); // type-through starts a fresh edit
    }
    if (mod && e.key.toLowerCase() === 'c') { e.preventDefault(); return void cmd('copy'); }
    if (mod && e.key.toLowerCase() === 'x') { e.preventDefault(); return void userCmd('cut'); }
    if (mod && e.key.toLowerCase() === 'v') { e.preventDefault(); return void requestPaste(); }
    if (mod && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      return void cmd(`select\t0\t0\t${Math.max(0, view.dims.rows - 1)}\t${Math.max(0, view.dims.cols - 1)}`);
    }
  }

  // ---- editing ---------------------------------------------------------------
  const MUTATING = /^(set|clear|cut|paste|insrow|delrow|inscol|delcol|sheet\t(add|rename))/;
  /** Run a user-initiated command and, if it mutates the workbook, tell the
   *  host so VS Code lights the dirty dot and can drive undo/redo. */
  function userCmd(str) {
    cmd(str);
    if (MUTATING.test(str)) vscode.postMessage({ type: 'edit' });
  }

  let editEl = null;
  // True once commitEdit()/cancelEdit() has settled the current edit. Guards
  // re-entrancy the same way startSheetRename()'s `done` flag does: removing
  // a focused element from the DOM fires a synchronous `blur`, so every path
  // that removes editEl (Enter/Tab/Escape, a grid mousedown elsewhere, the
  // blur handler itself) needs this to avoid double-committing.
  let editDone = false;
  function startEdit(initial) {
    if (editEl) return;
    const r = rowOfRef(), c = refCol();
    editDone = false;
    editEl = document.createElement('input');
    editEl.id = 'celledit';
    editEl.value = initial != null ? initial : view.cur.src;
    editEl.style.top = r * ROW_H + 'px';
    editEl.style.left = c * defW + 'px';
    editEl.style.height = ROW_H + 'px';
    editEl.style.minWidth = defW + 'px';
    editEl.style.zIndex = 5;
    editEl.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') { e.preventDefault(); commitEdit(); $('gridwrap').focus(); move(1, 0, false); }
      else if (e.key === 'Tab') { e.preventDefault(); commitEdit(); $('gridwrap').focus(); move(0, 1, false); }
      else if (e.key === 'Escape') { e.preventDefault(); cancelEdit(); $('gridwrap').focus(); }
      e.stopPropagation();
    });
    // Clicking inside the input to move the caret must not bubble to the
    // grid's own mousedown handler (it would re-`select` the underlying cell
    // and repaint — editEl survives that now, but there's no reason to pay
    // for a wasm round trip just to place a text caret).
    editEl.addEventListener('mousedown', (e) => e.stopPropagation());
    // Excel commits an in-progress edit the moment the cell loses focus for
    // any real reason — clicking the formula bar, a sheet tab, a header
    // menu, anywhere. The one blur to ignore is the repaint detach/re-append
    // cycle: paint() removes editEl from the DOM (firing this same blur) and
    // then synchronously re-appends + refocuses it — `!editEl.isConnected`
    // catches exactly that window, before the re-append has run.
    editEl.addEventListener('blur', () => {
      if (editDone) return;
      if (!editEl.isConnected) return; // mid-repaint detach; refocus follows synchronously
      commitEdit(); // do NOT force focus back to #gridwrap — respect wherever it went
    });
    $('cells').appendChild(editEl);
    editEl.focus();
    if (initial != null) editEl.setSelectionRange(initial.length, initial.length);
    else editEl.select();
  }
  function commitEdit() {
    if (!editEl || editDone) return;
    editDone = true;
    const text = editEl.value;
    editEl.remove();
    editEl = null;
    userCmd(`set\t${rowOfRef()}\t${refCol()}\t${text}`);
  }
  function cancelEdit() {
    if (!editEl || editDone) return;
    editDone = true;
    editEl.remove();
    editEl = null;
  }

  // ---- clipboard through the host --------------------------------------------
  let pasteSeq = 0;
  const pastePending = new Map();
  function requestPaste() {
    const requestId = ++pasteSeq;
    pastePending.set(requestId, true);
    vscode.postMessage({ type: 'readClipboard', requestId });
  }

  // ---- host messages -------------------------------------------------------
  window.addEventListener('message', (event) => {
    const msg = event.data;
    switch (msg.type) {
      case 'open': {
        const u8 = base64ToBytes(msg.data);
        if (handle) ex.grid_close(handle);
        const p = writeBytes(u8);
        handle = ex.grid_open(p, u8.length);
        ex.grid_free(p, u8.length);
        if (!handle) {
          document.body.textContent = 'Offxy could not read this .xlsx file.';
          return;
        }
        // Excel serial for NOW()/TODAY(): days since 1899-12-30, local time.
        const now = new Date();
        const serial = 25569 + (now.getTime() - now.getTimezoneOffset() * 60000) / 86400000;
        cmd(`clock\t${serial}`);
        requestView();
        $('gridwrap').focus();
        break;
      }
      case 'do':
        cmd(msg.op === 'redo' ? 'redo' : 'undo');
        requestView();
        break;
      case 'getBytes': {
        const bytes = readResult(ex.grid_save(handle));
        vscode.postMessage({ type: 'bytes', requestId: msg.requestId, data: bytesToBase64(bytes) });
        break;
      }
      case 'clipboardText':
        if (pastePending.delete(msg.requestId) && msg.text) {
          userCmd(`paste\t${rowOfRef()}\t${refCol()}\t${msg.text}`);
        }
        break;
    }
  });

  // ---- base64 --------------------------------------------------------------
  function base64ToBytes(b64) {
    const bin = atob(b64);
    const u8 = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i);
    return u8;
  }
  function bytesToBase64(u8) {
    let bin = '';
    const CHUNK = 0x8000;
    for (let i = 0; i < u8.length; i += CHUNK) {
      bin += String.fromCharCode.apply(null, u8.subarray(i, i + CHUNK));
    }
    return btoa(bin);
  }

  // ---- boot ----------------------------------------------------------------
  document.body.innerHTML = `
    <div id="fbar"><span id="cellref">A1</span><input id="fsrc" spellcheck="false" /></div>
    <div id="corner"></div><div id="colhdr"></div><div id="rowhdr"></div>
    <div id="gridwrap" tabindex="0"><div id="spacer"></div><div id="cells"></div></div>
    <div id="tabs"></div>`;

  /** Right-click context menu for a row/col header: insert/delete at `at`. */
  function headerMenu(e, kind, at) {
    $('hmenu')?.remove();
    const m = document.createElement('div');
    m.id = 'hmenu';
    m.style.cssText = `position:fixed;left:${e.clientX}px;top:${e.clientY}px;z-index:10;` +
      'background:var(--vscode-menu-background,#252526);color:var(--vscode-menu-foreground,#ccc);' +
      'border:1px solid var(--vscode-editorWidget-border,#454545);padding:4px 0;';
    const items = kind === 'col'
      ? [[`Insert column`, `inscol\t${at}\t1`], [`Delete column`, `delcol\t${at}\t1`]]
      : [[`Insert row`, `insrow\t${at}\t1`], [`Delete row`, `delrow\t${at}\t1`]];
    for (const [label, op] of items) {
      const it = document.createElement('div');
      it.textContent = label;
      it.style.cssText = 'padding:2px 14px;cursor:pointer;';
      it.addEventListener('mouseenter', () => (it.style.background = 'var(--vscode-menu-selectionBackground,#04395e)'));
      it.addEventListener('mouseleave', () => (it.style.background = ''));
      it.addEventListener('click', () => { m.remove(); userCmd(op); requestView(); });
      m.appendChild(it);
    }
    document.body.appendChild(m);
  }

  async function boot() {
    const resp = await fetch(window.__OFFXY__.wasmUri);
    const { instance } = await WebAssembly.instantiate(await resp.arrayBuffer(), {});
    ex = instance.exports;
    const wrap = $('gridwrap');
    wrap.addEventListener('scroll', onScroll);
    wrap.addEventListener('mousedown', onMousedown);
    window.addEventListener('mousemove', onMousemove);
    window.addEventListener('mouseup', onMouseup);
    wrap.addEventListener('keydown', onKeydown);
    wrap.addEventListener('dblclick', () => startEdit(null));
    window.addEventListener('resize', onScroll);
    $('fsrc').addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        userCmd(`set\t${rowOfRef()}\t${refCol()}\t${$('fsrc').value}`);
        $('gridwrap').focus();
      } else if (e.key === 'Escape') {
        e.preventDefault();
        $('fsrc').value = view.cur.src;
        $('gridwrap').focus();
      }
      e.stopPropagation();
    });
    $('colhdr').addEventListener('contextmenu', (e) => {
      const t = e.target.closest('.hcell');
      if (!t) return;
      e.preventDefault();
      headerMenu(e, 'col', parseInt(t.dataset.col, 10));
    });
    $('rowhdr').addEventListener('contextmenu', (e) => {
      const t = e.target.closest('.hcell');
      if (!t) return;
      e.preventDefault();
      headerMenu(e, 'row', parseInt(t.dataset.row, 10));
    });
    document.addEventListener('click', () => $('hmenu')?.remove());
    vscode.postMessage({ type: 'ready' });
  }
  boot().catch((err) => {
    document.body.textContent = 'Offxy failed to start: ' + (err && err.message ? err.message : err);
  });
})();
