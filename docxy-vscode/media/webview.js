// Docxy webview — runs the docxcore wasm engine and paints the document onto a
// monospace grid that matches the editor's own font and size, so a Word document
// reads like text in a VS Code tab. No ribbon: keyboard + command palette drive
// everything, exactly like editing code.
//
// The wasm ABI mirrors `docxwasm/src/lib.rs`:
//   docx_alloc(len)->ptr, docx_free(ptr,len)
//   docx_open(ptr,len)->handle, docx_close(handle)
//   docx_render(handle)->resultPtr
//   docx_cmd(handle,ptr,len)->resultPtr
//   docx_save(handle)->resultPtr
// A "result" buffer is [u32 little-endian length][payload bytes].

(function () {
  const vscode = acquireVsCodeApi();
  const docEl = document.getElementById('doc');
  const statusEl = document.getElementById('status');

  /** @type {WebAssembly.Exports} */
  let ex = null;
  let handle = 0;
  let lastView = { lines: [], caret: { line: 0, col: 0 }, selection: 0 };
  let metrics = { charW: 8, lineH: 18 };

  const enc = new TextEncoder();
  const dec = new TextDecoder();

  // ---- wasm marshalling ----------------------------------------------------
  const mem = () => new Uint8Array(ex.memory.buffer);

  function writeBytes(u8) {
    const ptr = ex.docx_alloc(u8.length);
    mem().set(u8, ptr); // fetch the view AFTER alloc (memory may have grown)
    return ptr;
  }
  function readResult(ptr) {
    const m = mem();
    const len = m[ptr] | (m[ptr + 1] << 8) | (m[ptr + 2] << 16) | (m[ptr + 3] << 24);
    const out = m.slice(ptr + 4, ptr + 4 + len);
    ex.docx_free(ptr, 4 + len);
    return out;
  }
  function callBytes(fn, u8) {
    const p = writeBytes(u8);
    const r = fn(handle, p, u8.length);
    ex.docx_free(p, u8.length);
    return readResult(r);
  }

  function openBytes(u8) {
    if (handle) ex.docx_close(handle);
    mediaCache.clear();
    if (u8.length === 0) {
      handle = 0;
      showEmptyState();
      return;
    }
    const p = writeBytes(u8);
    handle = ex.docx_open(p, u8.length);
    ex.docx_free(p, u8.length);
    if (!handle) {
      docEl.textContent = 'Docxy could not read this .docx file.';
      return;
    }
    render();
  }
  /** Empty file: offer to turn it into a real Word document right here. */
  function showEmptyState() {
    const box = document.createElement('div');
    box.className = 'empty-state';
    const note = document.createElement('p');
    note.textContent = 'This file is empty — it isn’t a Word document yet.';
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.textContent = 'Create new Word document';
    btn.addEventListener('click', () => {
      btn.disabled = true;
      vscode.postMessage({ type: 'createNew' });
    });
    box.append(note, btn);
    docEl.replaceChildren(box);
  }

  function render() {
    lastView = JSON.parse(dec.decode(readResult(ex.docx_render(handle))));
    paint();
  }
  /** Apply one command string; repaint; return the parsed view. */
  function cmd(str) {
    lastView = JSON.parse(dec.decode(callBytes(ex.docx_cmd, enc.encode(str))));
    paint();
    if (lastView.copied != null) {
      vscode.postMessage({ type: 'clipboard', text: lastView.copied });
    }
    return lastView;
  }
  function saveBytes() {
    return readResult(ex.docx_save(handle));
  }
  function mediaBytes(rid) {
    const u8 = enc.encode(rid);
    const p = writeBytes(u8);
    const r = ex.docx_media(handle, p, u8.length);
    ex.docx_free(p, u8.length);
    return readResult(r);
  }

  // ---- embedded images -----------------------------------------------------
  const PAD_L = 12; // must match #doc padding-left
  const PAD_T = 8; //  must match #doc padding-top
  const mediaCache = new Map(); // rid -> data URI (or null if undecodable)

  function sniffMime(b) {
    if (b.length < 4) return null;
    if (b[0] === 0x89 && b[1] === 0x50) return 'image/png';
    if (b[0] === 0xff && b[1] === 0xd8) return 'image/jpeg';
    if (b[0] === 0x47 && b[1] === 0x49 && b[2] === 0x46) return 'image/gif';
    if (b[0] === 0x42 && b[1] === 0x4d) return 'image/bmp';
    if (b[0] === 0x3c && b.length > 4) return 'image/svg+xml'; // '<'
    return null; // WMF/EMF and friends: no browser support → fallback box
  }
  function loadMedia(rid) {
    if (mediaCache.has(rid)) return mediaCache.get(rid);
    const bytes = mediaBytes(rid);
    const mime = bytes.length ? sniffMime(bytes) : null;
    const uri = mime ? `data:${mime};base64,${bytesToBase64(bytes)}` : null;
    mediaCache.set(rid, uri);
    return uri;
  }

  let imgEls = [];
  function paintImages() {
    for (const el of imgEls) el.remove();
    imgEls = [];
    for (const box of lastView.images || []) {
      const left = PAD_L + box.col * metrics.charW;
      const top = PAD_T + box.row * metrics.lineH;
      const w = box.w * metrics.charW;
      const h = box.h * metrics.lineH;
      const uri = box.rid ? loadMedia(box.rid) : null;
      let el;
      if (uri) {
        el = document.createElement('img');
        el.src = uri;
        el.className = 'docimg';
      } else {
        el = document.createElement('div');
        el.className = 'docimg fallback';
        el.textContent = box.label || 'image';
      }
      if (box.bordered) el.classList.add('bordered');
      el.style.left = left + 'px';
      el.style.top = top + 'px';
      el.style.width = w + 'px';
      el.style.height = h + 'px';
      docEl.appendChild(el);
      imgEls.push(el);
    }
  }

  // ---- painting ------------------------------------------------------------
  const ANSI = (name) => `var(--vscode-terminal-ansi${name})`;

  function paint() {
    const frag = document.createDocumentFragment();
    for (const line of lastView.lines) {
      const div = document.createElement('div');
      div.className = 'line';
      if (line.length === 0) {
        div.appendChild(document.createTextNode('​')); // keep empty lines tall
      }
      for (const sp of line) {
        const el = document.createElement('span');
        el.textContent = sp.t;
        if (sp.b) el.classList.add('b');
        if (sp.i) el.classList.add('i');
        if (sp.u) el.classList.add('u');
        if (sp.s) el.classList.add('st');
        if (sp.d) el.classList.add('dim');
        if (sp.h) el.classList.add('sel');
        if (sp.c) el.style.color = ANSI(sp.c);
        if (sp.lnk) {
          el.classList.add('link');
          el.dataset.href = sp.lnk;
        }
        div.appendChild(el);
      }
      frag.appendChild(div);
    }
    docEl.replaceChildren(frag);
    imgEls = []; // replaceChildren removed the old overlays
    placeCaret();
    paintImages();
    updateStatus();
  }

  let caretEl = null;
  function placeCaret() {
    if (!caretEl) {
      caretEl = document.createElement('div');
      caretEl.id = 'caret';
      docEl.appendChild(caretEl);
    } else {
      docEl.appendChild(caretEl); // keep it last
    }
    const c = lastView.caret || { line: 0, col: 0 };
    caretEl.style.transform = `translate(${c.col * metrics.charW}px, ${c.line * metrics.lineH}px)`;
    caretEl.style.height = metrics.lineH + 'px';
  }

  function updateStatus() {
    const c = lastView.caret || { line: 0, col: 0 };
    const n = lastView.lines.length;
    statusEl.textContent = `Ln ${c.line + 1}, Col ${c.col + 1}  ·  ${n} lines${
      lastView.dirty ? '  ·  ●' : ''
    }`;
  }

  // ---- metrics + width -----------------------------------------------------
  function measure() {
    const ruler = document.createElement('span');
    ruler.style.position = 'absolute';
    ruler.style.visibility = 'hidden';
    ruler.style.whiteSpace = 'pre';
    ruler.textContent = 'M'.repeat(100);
    docEl.appendChild(ruler);
    const charW = ruler.getBoundingClientRect().width / 100 || 8;
    const lineH = ruler.getBoundingClientRect().height || 18;
    docEl.removeChild(ruler);
    metrics = { charW, lineH };
  }

  let widthTimer = 0;
  function syncWidth() {
    if (!handle) return;
    const cols = Math.max(20, Math.floor(docEl.clientWidth / metrics.charW) - 1);
    cmd('width\t' + cols);
  }
  function onResize() {
    clearTimeout(widthTimer);
    widthTimer = setTimeout(() => {
      measure();
      syncWidth();
    }, 80);
  }

  // ---- input ---------------------------------------------------------------
  const MUTATING = new Set([
    'insert', 'newline', 'backspace', 'delete', 'bold', 'italic', 'underline',
    'strike', 'paste', 'cut', 'heading', 'list', 'align', 'indent', 'fontsize',
    'color', 'replace',
  ]);

  /** Run a user-initiated command and, if it mutates, tell the host so VS Code
   *  lights the dirty dot and can drive undo/redo. */
  function userCmd(str) {
    cmd(str);
    const op = str.split('\t', 1)[0];
    if (MUTATING.has(op)) {
      vscode.postMessage({ type: 'edit' });
    }
  }

  function onKeydown(e) {
    if (!handle) return;
    const mod = e.ctrlKey || e.metaKey;
    const sel = e.shiftKey ? '1' : '0';

    // Let VS Code own undo/redo/save so they route through its edit stack.
    if (mod && ['z', 'y', 's'].includes(e.key.toLowerCase())) return;

    if (mod) {
      switch (e.key.toLowerCase()) {
        case 'a': e.preventDefault(); return void cmd('selectall');
        case 'b': e.preventDefault(); return void userCmd('bold');
        case 'i': e.preventDefault(); return void userCmd('italic');
        case 'u': e.preventDefault(); return void userCmd('underline');
        case 'c': e.preventDefault(); return void cmd('copy');
        case 'x': e.preventDefault(); return void userCmd('cut');
        case 'v': e.preventDefault(); return void requestPaste();
        case 'arrowleft': e.preventDefault(); return void cmd('move\twordleft\t' + sel);
        case 'arrowright': e.preventDefault(); return void cmd('move\twordright\t' + sel);
        case 'home': e.preventDefault(); return void cmd('move\tdocstart\t' + sel);
        case 'end': e.preventDefault(); return void cmd('move\tdocend\t' + sel);
        default: return;
      }
    }

    switch (e.key) {
      case 'Enter': e.preventDefault(); return void userCmd('newline');
      case 'Backspace': e.preventDefault(); return void userCmd('backspace');
      case 'Delete': e.preventDefault(); return void userCmd('delete');
      case 'ArrowLeft': e.preventDefault(); return void cmd('move\tleft\t' + sel);
      case 'ArrowRight': e.preventDefault(); return void cmd('move\tright\t' + sel);
      case 'ArrowUp': e.preventDefault(); return void cmd('move\tup\t' + sel);
      case 'ArrowDown': e.preventDefault(); return void cmd('move\tdown\t' + sel);
      case 'Home': e.preventDefault(); return void cmd('move\thome\t' + sel);
      case 'End': e.preventDefault(); return void cmd('move\tend\t' + sel);
      case 'Tab': e.preventDefault(); return void userCmd('insert\t\t');
      default: break;
    }

    // Printable characters.
    if (e.key.length === 1 && !e.altKey) {
      e.preventDefault();
      userCmd('insert\t' + e.key);
    }
  }

  // Clipboard paste is mediated through the host (the webview's selection model
  // is custom, so we can't rely on the DOM paste event's target).
  let pasteSeq = 0;
  const pastePending = new Map();
  function requestPaste() {
    const requestId = ++pasteSeq;
    pastePending.set(requestId, true);
    vscode.postMessage({ type: 'readClipboard', requestId });
  }

  // Mouse: click to place the caret, drag to select.
  let dragging = false;
  function cellFromEvent(e) {
    const rect = docEl.getBoundingClientRect();
    const x = e.clientX - rect.left + docEl.scrollLeft;
    const y = e.clientY - rect.top + docEl.scrollTop;
    const line = Math.max(0, Math.floor(y / metrics.lineH));
    const col = Math.max(0, Math.round(x / metrics.charW));
    return { line, col };
  }
  function onMousedown(e) {
    if (!handle) return;
    const link = e.target.closest && e.target.closest('.link');
    if (link && (e.ctrlKey || e.metaKey)) {
      vscode.postMessage({ type: 'openLink', href: link.dataset.href });
      return;
    }
    docEl.focus();
    const { line, col } = cellFromEvent(e);
    cmd(`click\t${line}\t${col}\t0`);
    dragging = true;
  }
  function onMousemove(e) {
    if (!dragging) return;
    const { line, col } = cellFromEvent(e);
    cmd(`click\t${line}\t${col}\t1`); // extend selection
  }
  function onMouseup() {
    dragging = false;
  }

  // ---- host messages -------------------------------------------------------
  window.addEventListener('message', (event) => {
    const msg = event.data;
    switch (msg.type) {
      case 'open':
        openBytes(base64ToBytes(msg.data));
        measure();
        syncWidth();
        docEl.focus();
        break;
      case 'do': // VS Code-level undo/redo — do NOT re-notify the host.
        cmd(msg.op === 'redo' ? 'redo' : 'undo');
        break;
      case 'command':
        userCmd(msg.op);
        break;
      case 'getBytes':
        vscode.postMessage({
          type: 'bytes',
          requestId: msg.requestId,
          data: bytesToBase64(saveBytes()),
        });
        break;
      case 'clipboardText':
        if (pastePending.delete(msg.requestId) && msg.text) {
          userCmd('paste\t' + msg.text);
        }
        break;
    }
  });

  // ---- base64 (webview has no Buffer) --------------------------------------
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

  // ---- floating toolbar (no ribbon — just the essentials) ------------------
  function buildToolbar() {
    const bar = document.createElement('div');
    bar.id = 'toolbar';
    const SEP = '|';
    const buttons = [
      ['B', 'bold', 'Bold', 'tb-b'],
      ['I', 'italic', 'Italic', 'tb-i'],
      ['U', 'underline', 'Underline', 'tb-u'],
      ['S', 'strike', 'Strikethrough', 'tb-s'],
      [SEP],
      ['H1', 'heading\t1', 'Heading 1'],
      ['H2', 'heading\t2', 'Heading 2'],
      ['¶', 'heading\t0', 'Normal'],
      [SEP],
      ['•', 'list\tbullet', 'Bulleted list'],
      ['1.', 'list\tnumber', 'Numbered list'],
      [SEP],
      ['⯇', 'align\tleft', 'Align left'],
      ['≡', 'align\tcenter', 'Center'],
      ['⯈', 'align\tright', 'Align right'],
      [SEP],
      ['A−', 'fontsize\t-2', 'Smaller'],
      ['A+', 'fontsize\t2', 'Larger'],
    ];
    for (const [label, op, title, cls] of buttons) {
      if (label === SEP) {
        const s = document.createElement('span');
        s.className = 'tb-sep';
        bar.appendChild(s);
        continue;
      }
      const b = document.createElement('button');
      b.type = 'button';
      b.textContent = label;
      b.title = title;
      if (cls) b.classList.add(cls);
      // Keep the document's selection: don't let the button take focus.
      b.addEventListener('mousedown', (e) => e.preventDefault());
      b.addEventListener('click', () => {
        userCmd(op);
        docEl.focus();
      });
      bar.appendChild(b);
    }
    document.body.insertBefore(bar, document.body.firstChild);
  }

  // ---- boot ----------------------------------------------------------------
  async function boot() {
    const resp = await fetch(window.__DOCXY__.wasmUri);
    const { instance } = await WebAssembly.instantiate(await resp.arrayBuffer(), {});
    ex = instance.exports;
    buildToolbar();
    docEl.addEventListener('keydown', onKeydown);
    docEl.addEventListener('mousedown', onMousedown);
    window.addEventListener('mousemove', onMousemove);
    window.addEventListener('mouseup', onMouseup);
    window.addEventListener('resize', onResize);
    vscode.postMessage({ type: 'ready' });
  }
  boot().catch((err) => {
    docEl.textContent = 'Docxy failed to start: ' + (err && err.message ? err.message : err);
  });
})();
