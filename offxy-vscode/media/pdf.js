// The PDF viewer webview (#2259): a PDF's pages drawn by PdfCraft's renderer in
// `pdfpages.wasm`, read-only. The engine runs in a worker (pdfworker.js behind
// pdfhost.js), so a slow or hostile page never freezes the editor: past its
// timeout, or when the engine traps, the worker is replaced and the page says
// why. Pages are stacked at their own size; only those near the viewport are
// drawn, at the zoom times the device pixel ratio, within the suite's caps
// (4096 px a side, 16 MP; the engine applies them).
(function () {
  'use strict';
  const vscode = acquireVsCodeApi();
  const cfg = window.__OFFXY_PDF__;
  const { PdfHost } = window.PdfHostApi;

  /** 100 % is the printed size: 96 CSS pixels an inch, 72 points. */
  const PX_PER_PT = 96 / 72;
  const ZOOMS = [0.25, 0.5, 0.67, 0.75, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3, 4];
  /** The gap around and between pages, in CSS pixels. */
  const GAP = 16;

  const pagesEl = document.getElementById('pages');
  const statusEl = document.getElementById('status');
  const zoomEl = document.getElementById('zoom');
  const pageBox = document.getElementById('page');
  const pageCount = document.getElementById('count');

  /** @type {{w:number,h:number,label:string,el:HTMLElement,canvas:HTMLCanvasElement|null,drawn:number|null,failed:string|null,near:boolean}[]} */
  let pages = [];
  let zoom = 1;
  let fitWidth = true;
  /** The zoom the page boxes were last laid out at. */
  let laidZoom = 1;
  let host = null;
  let busy = false;
  let current = 0;
  let observer = null;

  function status(text) {
    statusEl.textContent = text || '';
  }

  // ---- opening ---------------------------------------------------------------

  async function open(bytes) {
    status('Opening…');
    let module;
    let source;
    try {
      const [wasm, engine, worker] = await Promise.all([
        fetch(cfg.wasmUri).then((r) => r.arrayBuffer()),
        fetch(cfg.engineUri).then((r) => r.text()),
        fetch(cfg.workerUri).then((r) => r.text()),
      ]);
      module = await WebAssembly.compile(wasm);
      source = URL.createObjectURL(new Blob([engine, '\n', worker], { type: 'text/javascript' }));
    } catch (e) {
      failed(`the PDF engine could not be loaded: ${(e && e.message) || e}`);
      return;
    }
    host = new PdfHost({
      makeWorker: () => new Worker(source),
      module,
      bytes,
      onInfo,
    });
  }

  function failed(why, locked) {
    pagesEl.textContent = '';
    const box = document.createElement('div');
    box.className = 'failed-doc';
    const head = document.createElement('h2');
    head.textContent = "This PDF can't be opened";
    const reason = document.createElement('p');
    reason.textContent = locked
      ? 'It is protected by a password. Open it in the docxy suite, which asks for it.'
      : why;
    box.append(head, reason);
    pagesEl.append(box);
    status('');
  }

  function onInfo(info) {
    if (!info.ok) {
      failed(info.error, info.locked);
      return;
    }
    pages = info.pages.map((p, i) => {
      const el = document.createElement('div');
      el.className = 'page';
      el.dataset.page = String(i);
      el.setAttribute('aria-label', `Page ${p.label}`);
      pagesEl.append(el);
      return { w: p.w, h: p.h, label: p.label, el, canvas: null, drawn: null, failed: null, near: false };
    });
    pageCount.textContent = `of ${pages.length}`;
    pageBox.max = String(pages.length);
    observer = new IntersectionObserver(onNear, { root: pagesEl, rootMargin: '100% 0px' });
    for (const p of pages) observer.observe(p.el);
    layout();
    status('');
  }

  // ---- layout and zoom ------------------------------------------------------

  /** The zoom pages are drawn at: fit width's, or the chosen one. */
  function effectiveZoom() {
    if (!fitWidth || !pages.length) return zoom;
    const widest = Math.max(...pages.map((p) => p.w)) * PX_PER_PT;
    const room = Math.max(pagesEl.clientWidth - 2 * GAP, 50);
    return Math.max(0.1, Math.min(room / widest, 8));
  }

  function layout() {
    const z = effectiveZoom();
    laidZoom = z;
    zoomEl.textContent = `${Math.round(z * 100)}%`;
    for (const p of pages) {
      p.el.style.width = `${Math.round(p.w * PX_PER_PT * z)}px`;
      p.el.style.height = `${Math.round(p.h * PX_PER_PT * z)}px`;
    }
    schedule();
  }

  function setZoom(z, fit) {
    const anchor = current;
    fitWidth = fit;
    if (!fit) zoom = z;
    layout();
    goTo(anchor, false);
  }

  function zoomStep(dir) {
    const z = effectiveZoom();
    const next =
      dir > 0 ? ZOOMS.find((s) => s > z + 1e-3) ?? ZOOMS[ZOOMS.length - 1] : [...ZOOMS].reverse().find((s) => s < z - 1e-3) ?? ZOOMS[0];
    setZoom(next, false);
  }

  /** Device pixels a point at the zoom the boxes are laid out at. */
  function scaleOf() {
    return laidZoom * PX_PER_PT * (window.devicePixelRatio || 1);
  }

  // ---- drawing --------------------------------------------------------------

  function onNear(entries) {
    for (const e of entries) {
      const p = pages[Number(e.target.dataset.page)];
      p.near = e.isIntersecting;
      if (!p.near && p.canvas) {
        // A page scrolled far away gives its pixels back.
        p.canvas.remove();
        p.canvas = null;
        p.drawn = null;
      }
    }
    schedule();
  }

  /** Ask for the near page closest to the current one that is not drawn at this zoom. */
  function schedule() {
    if (busy || !host) return;
    const scale = scaleOf();
    let best = -1;
    for (let i = 0; i < pages.length; i++) {
      const p = pages[i];
      if (!p.near || p.failed || p.drawn === scale) continue;
      if (best < 0 || Math.abs(i - current) < Math.abs(best - current)) best = i;
    }
    if (best < 0) return;
    busy = true;
    const p = pages[best];
    host.request({ type: 'render', page: best, scale }).then((a) => {
      busy = false;
      if (a.error) {
        // Never asked again in this view, as the suite's view does.
        p.failed = a.error;
        showFailed(p);
      } else if (p.near) {
        paint(p, a, scale);
      }
      schedule();
    });
  }

  function paint(p, a, scale) {
    if (!p.canvas) {
      p.canvas = document.createElement('canvas');
      p.el.append(p.canvas);
    }
    p.canvas.width = a.width;
    p.canvas.height = a.height;
    const data = new ImageData(new Uint8ClampedArray(a.rgba.buffer, a.rgba.byteOffset, a.rgba.byteLength), a.width, a.height);
    p.canvas.getContext('2d').putImageData(data, 0, 0);
    p.drawn = scale;
  }

  function showFailed(p) {
    if (p.canvas) p.canvas.remove();
    p.canvas = null;
    const box = document.createElement('div');
    box.className = 'failed-page';
    box.textContent = `This page can't be drawn: ${p.failed}`;
    p.el.append(box);
  }

  // ---- navigation -----------------------------------------------------------

  function onScroll() {
    if (!pages.length) return;
    const mid = pagesEl.scrollTop + pagesEl.clientHeight / 3;
    let at = 0;
    for (let i = 0; i < pages.length; i++) {
      if (pages[i].el.offsetTop <= mid) at = i;
      else break;
    }
    if (at !== current) {
      current = at;
      pageBox.value = String(at + 1);
      schedule();
    }
  }

  function goTo(i, focus) {
    if (!pages.length) return;
    const n = Math.max(0, Math.min(pages.length - 1, i));
    pagesEl.scrollTop = pages[n].el.offsetTop - GAP;
    current = n;
    pageBox.value = String(n + 1);
    if (focus) pagesEl.focus();
  }

  pagesEl.addEventListener('scroll', onScroll, { passive: true });
  // Fit width follows the view's width: a window resize, and the scrollbar
  // that comes once the pages overflow.
  new ResizeObserver(() => {
    if (fitWidth && pages.length && Math.abs(effectiveZoom() - laidZoom) > 1e-4) layout();
  }).observe(pagesEl);
  document.getElementById('zoom-in').addEventListener('click', () => zoomStep(1));
  document.getElementById('zoom-out').addEventListener('click', () => zoomStep(-1));
  document.getElementById('fit-width').addEventListener('click', () => setZoom(zoom, true));
  pageBox.addEventListener('change', () => goTo(Number(pageBox.value) - 1, true));
  pageBox.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') goTo(Number(pageBox.value) - 1, true);
  });
  window.addEventListener('keydown', (e) => {
    const mod = e.ctrlKey || e.metaKey;
    if (mod && (e.key === '=' || e.key === '+')) {
      e.preventDefault();
      zoomStep(1);
    } else if (mod && e.key === '-') {
      e.preventDefault();
      zoomStep(-1);
    } else if (mod && e.key === '0') {
      e.preventDefault();
      setZoom(zoom, true);
    }
  });
  window.addEventListener('pagehide', () => host && host.dispose());

  // ---- host messages --------------------------------------------------------

  window.addEventListener('message', (event) => {
    const msg = event.data;
    if (msg && msg.type === 'open') {
      void open(msg.bytes instanceof Uint8Array ? msg.bytes : new Uint8Array(msg.bytes));
    }
  });
  vscode.postMessage({ type: 'ready' });
})();
