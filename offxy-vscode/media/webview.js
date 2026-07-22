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

// ---- mermaid geometry -> inline SVG (Task 7 of the mermaid-flowchart-quality
// plan) --------------------------------------------------------------------
// Draws the SAME `DiagramGeometry` (docxcore's `mermaid.rs`) that the Word
// exporter turns into DrawingML shapes, as an SVG overlay here — so a
// flowchart looks the same in both places. `geo`'s coordinates are EMU
// (English Metric Units, 914400 per inch — the OOXML DrawingML unit); the
// SVG's viewBox uses them directly as user units, and the caller's overlay
// element (sized in real pixels from the character-grid metrics) scales the
// whole picture down to screen size. Because the user-unit scale is EMU
// (values in the hundreds of thousands to millions), every absolute length
// drawn inside the SVG — stroke width, font size, arrowhead size — must ALSO
// be chosen in EMU, not small CSS-pixel defaults: a "2px" stroke or "14px"
// font would shrink to an invisible sub-pixel sliver once the viewBox is
// scaled down to a roughly one-inch-tall grid cell.
//
// `buildMermaidSvg` is pure (no DOM access) and declared at the TOP LEVEL of
// this script, outside the webview IIFE below — mirroring how
// `grid.layout.test.mjs` runs `grid.js` unmodified inside a Node `vm` sandbox
// to reach its behavior (this project has no module system for these webview
// scripts; they load as plain `<script src=...>` tags, see extension.ts). A
// top-level `function` declaration in non-strict script code becomes a
// property of the global object it runs against — `window.buildMermaidSvg` in
// a real browser tab, or a property of the sandbox object `vm.runInContext`
// ran the script against in `media/mermaid-svg.test.mjs` — so the test can
// call it directly without this file needing an export statement.
const MMD_STROKE = 12700; // 1pt in EMU: node border / edge line width
const MMD_FONT_NODE = 130000; // ~10.2pt in EMU: node label size
const MMD_FONT_TITLE = 115000; // subgraph title size
const MMD_FONT_EDGE = 110000; // edge label size
const MMD_ARROW = 140000; // arrowhead marker box side, in EMU (userSpaceOnUse)

// Sequence-diagram palette — mirrors docxcore's `mermaid_seq.rs` constants
// (`PART_FILL`/`PART_STROKE`, `FRAME_FILL`/`FRAME_STROKE`,
// `NOTE_FILL`/`NOTE_STROKE`, `LINE_STROKE`) exactly, so the webview overlay
// and the Word DrawingML shapes it mirrors read as the same picture.
const SEQ_PART_FILL = '#DAE8FC';
const SEQ_PART_STROKE = '#6C8EBF';
const SEQ_FRAME_FILL = '#F5F5F5';
const SEQ_FRAME_STROKE = '#999999';
const SEQ_NOTE_FILL = '#FFF6D5';
const SEQ_NOTE_STROKE = '#AAAA33';
const SEQ_LINE_STROKE = '#333333';

function escMermaidText(s) {
  return String(s == null ? '' : s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/** geo: `DiagramGeometry` JSON (canvasW/canvasH, nodes[], edges[], subgraphs[])
 *  -> a self-contained `<svg>...</svg>` string. Z-order (back to front, per the
 *  brief): subgraph bands + titles, then edges, then nodes + their labels,
 *  then edge labels (drawn last so they sit legibly on top of any edge line
 *  crossing under them). */
function buildMermaidSvg(geo) {
  if (geo.kind === 'sequence') return buildSequenceSvg(geo);
  const canvasW = geo.canvasW || 0;
  const canvasH = geo.canvasH || 0;
  const parts = [];
  parts.push(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${canvasW} ${canvasH}" ` +
      `width="100%" height="100%" preserveAspectRatio="xMidYMid meet">`
  );
  parts.push(
    '<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" ' +
      `markerWidth="${MMD_ARROW}" markerHeight="${MMD_ARROW}" markerUnits="userSpaceOnUse" ` +
      'orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 Z" fill="#333333"/></marker></defs>'
  );

  // Subgraph bands first — background, behind edges and nodes.
  for (const sg of geo.subgraphs || []) {
    parts.push(
      `<rect x="${sg.x}" y="${sg.y}" width="${sg.w}" height="${sg.h}" ` +
        `rx="${Math.min(sg.w, sg.h) * 0.02}" fill="#F5F5F5" stroke="#999999" stroke-width="${MMD_STROKE}"/>`
    );
    if (sg.title) {
      parts.push(
        `<text x="${sg.x + MMD_FONT_TITLE * 0.3}" y="${sg.y + MMD_FONT_TITLE * 1.3}" ` +
          `font-size="${MMD_FONT_TITLE}" fill="#333333">${escMermaidText(sg.title)}</text>`
      );
    }
  }

  // Edges next — under the nodes they connect. `style` (from the shared
  // geometry — docxcore's `mermaid.rs` `EdgeStyle`) mirrors the same dotted /
  // thick line the DrawingML `emit_connector` draws for Word, so the two
  // renderings agree; `solid` (the default when a geometry omits `style`)
  // keeps today's plain line untouched.
  for (const e of geo.edges || []) {
    const pts = (e.points || []).map((p) => `${p[0]},${p[1]}`).join(' ');
    const strokeWidth = e.style === 'thick' ? MMD_STROKE * 1.5 : MMD_STROKE;
    const dash = e.style === 'dotted' ? ' stroke-dasharray="8 6"' : '';
    parts.push(
      `<polyline points="${pts}" fill="none" stroke="#333333" ` +
        `stroke-width="${strokeWidth}"${dash} marker-end="url(#arrow)"/>`
    );
  }

  // Nodes on top of edges, each with a centered label.
  for (const n of geo.nodes || []) {
    const fill = '#' + (n.fill || 'FFFFFF');
    const stroke = '#' + (n.stroke || '000000');
    const textColor = '#' + (n.textColor || '000000');
    const cx = n.x + n.w / 2;
    const cy = n.y + n.h / 2;
    if (n.shape === 'diamond') {
      const pts = `${cx},${n.y} ${n.x + n.w},${cy} ${cx},${n.y + n.h} ${n.x},${cy}`;
      parts.push(`<polygon points="${pts}" fill="${fill}" stroke="${stroke}" stroke-width="${MMD_STROKE}"/>`);
    } else if (n.shape === 'hexagon') {
      const tip = n.w / 6;
      const pts =
        `${n.x + tip},${n.y} ${n.x + n.w - tip},${n.y} ${n.x + n.w},${cy} ` +
        `${n.x + n.w - tip},${n.y + n.h} ${n.x + tip},${n.y + n.h} ${n.x},${cy}`;
      parts.push(`<polygon points="${pts}" fill="${fill}" stroke="${stroke}" stroke-width="${MMD_STROKE}"/>`);
    } else if (n.shape === 'ellipse' || n.shape === 'circle') {
      parts.push(
        `<ellipse cx="${cx}" cy="${cy}" rx="${n.w / 2}" ry="${n.h / 2}" ` +
          `fill="${fill}" stroke="${stroke}" stroke-width="${MMD_STROKE}"/>`
      );
    } else {
      // rect / roundRect (any other/unknown shape tag falls back to a plain rect).
      const rx = n.shape === 'roundRect' ? Math.min(n.w, n.h) * 0.15 : 0;
      parts.push(
        `<rect x="${n.x}" y="${n.y}" width="${n.w}" height="${n.h}" rx="${rx}" ` +
          `fill="${fill}" stroke="${stroke}" stroke-width="${MMD_STROKE}"/>`
      );
    }
    parts.push(
      `<text x="${cx}" y="${cy}" text-anchor="middle" dominant-baseline="middle" ` +
        `font-size="${MMD_FONT_NODE}" fill="${textColor}">${escMermaidText(n.label)}</text>`
    );
  }

  // Edge labels last, on top of everything, with a white backing rect so an
  // edge line crossing under a label doesn't visually cut through the text.
  for (const e of geo.edges || []) {
    if (!e.label) continue;
    const pts = e.points || [];
    const mid = pts[Math.floor(pts.length / 2)] || pts[0] || [0, 0];
    const w = String(e.label).length * MMD_FONT_EDGE * 0.62 + MMD_FONT_EDGE;
    const h = MMD_FONT_EDGE * 1.6;
    parts.push(
      `<rect x="${mid[0] - w / 2}" y="${mid[1] - h / 2}" width="${w}" height="${h}" fill="#FFFFFF"/>`
    );
    parts.push(
      `<text x="${mid[0]}" y="${mid[1]}" text-anchor="middle" dominant-baseline="middle" ` +
        `font-size="${MMD_FONT_EDGE}" fill="#333333">${escMermaidText(e.label)}</text>`
    );
  }

  parts.push('</svg>');
  return parts.join('');
}

/** geo: `SequenceGeometry` JSON (`kind:"sequence"`, canvasW/canvasH,
 *  participants[], lifelines[], messages[], frames[], notes[]) -> a
 *  self-contained `<svg>...</svg>` string. Draws the SAME geometry
 *  docxcore's `mermaid_seq.rs` turns into DrawingML shapes for Word — same
 *  colors, same z-order (back to front, mirroring `to_drawing`): alt/else
 *  frame bands + titles + else-dividers, then notes, then participant
 *  header boxes, then lifelines, then messages + their labels. */
function buildSequenceSvg(geo) {
  const canvasW = geo.canvasW || 0;
  const canvasH = geo.canvasH || 0;
  const parts = [];
  parts.push(
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${canvasW} ${canvasH}" ` +
      `width="100%" height="100%" preserveAspectRatio="xMidYMid meet">`
  );
  parts.push(
    '<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" ' +
      `markerWidth="${MMD_ARROW}" markerHeight="${MMD_ARROW}" markerUnits="userSpaceOnUse" ` +
      'orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 Z" fill="#333333"/></marker></defs>'
  );

  // Frames first — the alt/else band sits behind everything else, with its
  // title top-left and (when present) a dashed divider + "[else] ..." label
  // at elseY.
  for (const f of geo.frames || []) {
    parts.push(
      `<rect x="${f.x}" y="${f.y}" width="${f.w}" height="${f.h}" ` +
        `rx="${Math.min(f.w, f.h) * 0.03}" fill="${SEQ_FRAME_FILL}" ` +
        `stroke="${SEQ_FRAME_STROKE}" stroke-width="${MMD_STROKE}"/>`
    );
    if (f.label) {
      parts.push(
        `<text x="${f.x + MMD_FONT_TITLE * 0.3}" y="${f.y + MMD_FONT_TITLE * 1.3}" ` +
          `font-size="${MMD_FONT_TITLE}" fill="#333333">${escMermaidText(f.label)}</text>`
      );
    }
    if (f.elseY != null) {
      parts.push(
        `<line x1="${f.x}" y1="${f.elseY}" x2="${f.x + f.w}" y2="${f.elseY}" ` +
          `stroke="${SEQ_FRAME_STROKE}" stroke-width="${MMD_STROKE}" stroke-dasharray="6 6"/>`
      );
      parts.push(
        `<text x="${f.x + MMD_FONT_TITLE * 0.3}" y="${f.elseY + MMD_FONT_TITLE * 1.3}" ` +
          `font-size="${MMD_FONT_TITLE}" fill="#333333">[else] ${escMermaidText(f.elseLabel)}</text>`
      );
    }
  }

  // Notes next — a distinctly-filled box, centered text.
  for (const n of geo.notes || []) {
    parts.push(
      `<rect x="${n.x}" y="${n.y}" width="${n.w}" height="${n.h}" ` +
        `fill="${SEQ_NOTE_FILL}" stroke="${SEQ_NOTE_STROKE}" stroke-width="${MMD_STROKE}"/>`
    );
    parts.push(
      `<text x="${n.x + n.w / 2}" y="${n.y + n.h / 2}" text-anchor="middle" ` +
        `dominant-baseline="middle" font-size="${MMD_FONT_NODE}" fill="#333333">` +
        `${escMermaidText(n.text)}</text>`
    );
  }

  // Participant header boxes on top of the frame/note bands.
  for (const p of geo.participants || []) {
    parts.push(
      `<rect x="${p.x}" y="${p.y}" width="${p.w}" height="${p.h}" ` +
        `fill="${SEQ_PART_FILL}" stroke="${SEQ_PART_STROKE}" stroke-width="${MMD_STROKE}"/>`
    );
    parts.push(
      `<text x="${p.x + p.w / 2}" y="${p.y + p.h / 2}" text-anchor="middle" ` +
        `dominant-baseline="middle" font-size="${MMD_FONT_NODE}" fill="#000000">` +
        `${escMermaidText(p.label)}</text>`
    );
  }

  // Lifelines drop from each participant box; dashed, like Word's.
  for (const l of geo.lifelines || []) {
    parts.push(
      `<line x1="${l.x}" y1="${l.y1}" x2="${l.x}" y2="${l.y2}" ` +
        `stroke="${SEQ_LINE_STROKE}" stroke-width="${MMD_STROKE}" stroke-dasharray="6 6"/>`
    );
  }

  // Messages last, on top: a straight arrow between lifelines, or — for a
  // self-message — a small rectangular loop out and back to the same
  // lifeline (mirrors `mermaid_seq.rs`'s `emit_message`). Its label sits
  // just above the line/loop.
  for (const m of geo.messages || []) {
    const dash = m.dashed ? ' stroke-dasharray="6 6"' : '';
    if (m.self) {
      const pts = `${m.x1},${m.y1} ${m.x2},${m.y1} ${m.x2},${m.y2} ${m.x1},${m.y2}`;
      parts.push(
        `<polyline points="${pts}" fill="none" stroke="${SEQ_LINE_STROKE}" ` +
          `stroke-width="${MMD_STROKE}"${dash} marker-end="url(#arrow)"/>`
      );
    } else {
      parts.push(
        `<line x1="${m.x1}" y1="${m.y1}" x2="${m.x2}" y2="${m.y2}" ` +
          `stroke="${SEQ_LINE_STROKE}" stroke-width="${MMD_STROKE}"${dash} marker-end="url(#arrow)"/>`
      );
    }
    if (m.text) {
      const cx = (m.x1 + m.x2) / 2;
      const cy = Math.min(m.y1, m.y2);
      parts.push(
        `<text x="${cx}" y="${cy - MMD_FONT_EDGE * 0.4}" text-anchor="middle" ` +
          `font-size="${MMD_FONT_EDGE}" fill="#333333">${escMermaidText(m.text)}</text>`
      );
    }
  }

  parts.push('</svg>');
  return parts.join('');
}

(function () {
  const vscode = acquireVsCodeApi();
  const docEl = document.getElementById('doc');
  const statusEl = document.getElementById('status');

  /** @type {WebAssembly.Exports} */
  let ex = null;
  let handle = 0;
  let lastView = { lines: [], caret: { line: 0, col: 0 }, selection: 0 };
  let metrics = { charW: 8, lineH: 18 };

  // `mermaid.min.js` is loaded as a plain global UMD `<script>` tag BEFORE
  // this one, ONLY for this editor (extension.ts's `hasMermaid` gate skips it
  // for grid.js) — so `mermaid` is a bare global, never an import. `typeof
  // mermaid` is safe even when the script never loaded: the grid editor, and
  // this file running unmodified inside mermaid-svg.test.mjs's `vm` sandbox
  // (see that test's header comment) both hit this line with no `mermaid`
  // global defined, and `typeof` never throws on an undeclared identifier.
  const MERMAID = typeof mermaid !== 'undefined' ? mermaid : null;
  if (MERMAID) {
    MERMAID.initialize({
      startOnLoad: false,
      securityLevel: 'loose',
      theme: 'default',
      flowchart: { useMaxWidth: false },
      sequence: { useMaxWidth: false },
    });
  }

  const enc = new TextEncoder();
  const dec = new TextDecoder();

  // The markdown editor (`.md` files) reuses this same webview; the provider
  // flags it via `window.__OFFXY__.markdown` so the UI can constrain itself to
  // what Markdown can actually represent (toolbar + op guard) and re-skin literal
  // task-list text for display (checkboxes) — see buildToolbar(), userCmd(), and
  // mdCheckbox() below.
  const MD_MODE = !!(window.__OFFXY__ && window.__OFFXY__.markdown);

  // Ops Markdown's own syntax has no way to represent, so a save silently drops
  // them. ONE source of truth, gating BOTH the toolbar (buildToolbar filters its
  // button list against this) and every path that can still invoke an op with
  // the toolbar hidden — keybindings (onKeydown) and the command-palette
  // `command` message (both route through userCmd(), the single choke point
  // guarded below). `align\tjustify` has no toolbar button at all (only a
  // command-palette entry, offxy.alignJustify) but is just as unrepresentable
  // in Markdown as left/center/right, so it's included here even though it was
  // never in the toolbar's button list.
  const MD_HIDDEN_OPS = new Set([
    'underline',
    'align\tleft', 'align\tcenter', 'align\tright', 'align\tjustify',
    'fontsize\t-2', 'fontsize\t2',
  ]);

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
    mmdCache.clear();
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

  let mmdEls = [];
  // Rendered-svg cache, keyed by the diagram's raw mermaid `source` text — a
  // paint that doesn't change a diagram's text (e.g. typing in an unrelated
  // paragraph) reuses the already-rendered svg instead of re-invoking
  // MERMAID.render() (an async DOM-touching call) on every keystroke.
  const mmdCache = new Map();
  // Bumped on every paintMermaid() call; a render Promise from a paint that's
  // since been superseded checks this before touching the DOM, so a burst of
  // edits can't let an old render land after a newer one already painted.
  let mmdVersion = 0;
  // MERMAID.render(id, ...) needs a fresh id each call (v10 briefly inserts a
  // measuring element under it) — a running counter, not the box index, so
  // two diagrams sharing an index across repaints (or the same box rendering
  // twice — cache miss then a later source edit) never collide.
  let mmdRenderSeq = 0;

  /** Parse a rendered mermaid `<svg ...>` string's natural pixel size from its
   *  width/height attributes, falling back to its viewBox — v10's `render()`
   *  always emits one or the other. Returns {w:0,h:0} if neither parses
   *  (defensive; not expected in practice), letting the caller fall back to
   *  the reserved cell width instead of collapsing to nothing. */
  function svgNaturalSize(svg) {
    const attr = (name) => {
      const m = new RegExp(`${name}="([\\d.]+)(?:px)?"`).exec(svg);
      return m ? parseFloat(m[1]) : null;
    };
    let w = attr('width');
    let h = attr('height');
    if (w == null || h == null) {
      const vb = /viewBox="[-\d.]+\s+[-\d.]+\s+([\d.]+)\s+([\d.]+)"/.exec(svg);
      if (vb) {
        if (w == null) w = parseFloat(vb[1]);
        if (h == null) h = parseFloat(vb[2]);
      }
    }
    return { w: w || 0, h: h || 0 };
  }

  /** Fill `el` with a REAL rendered-mermaid `svg` string, sized to the
   *  diagram's own natural aspect ratio and capped to `contentW` — real
   *  Mermaid output is laid out for its own content, not the `cols x rows`
   *  text-box cell the wasm side reserved for it, so (per the brief) this is
   *  deliberately NOT clamped into that cell box; a diagram taller than its
   *  cell is allowed to be tall, scrolling internally past a generous cap
   *  instead of being squashed or spilling unbounded over following lines. */
  function paintMermaidSvgInto(el, svg, contentW) {
    el.innerHTML = svg;
    const { w, h } = svgNaturalSize(svg);
    const displayW = w > 0 ? Math.min(w, contentW) : contentW;
    const scale = w > 0 ? displayW / w : 1;
    el.style.width = displayW + 'px';
    el.style.height = h > 0 ? h * scale + 'px' : '';
    el.style.maxHeight = '80vh';
    el.style.overflow = 'auto';
  }

  /** Fill `el` with today's geometry-SVG fallback, sized to the reserved
   *  `cols x rows` cell — used when real Mermaid is unavailable/erroring, has
   *  no `source` for this box, or while its render is still in flight. A
   *  diagram with zero laid-out nodes (the geometry builder found nothing to
   *  draw) leaves `el` empty, deferring entirely to the label-box text
   *  fallback already painted underneath by render_with_images's `text_box`. */
  function fallbackInto(el, mb) {
    el.style.maxHeight = '';
    el.style.overflow = '';
    if (mb.geo && mb.geo.nodes && mb.geo.nodes.length > 0) {
      el.innerHTML = buildMermaidSvg(mb.geo);
      el.style.width = mb.cols * metrics.charW + 'px';
      el.style.height = mb.rows * metrics.lineH + 'px';
    } else {
      el.innerHTML = '';
      el.style.width = '0px';
      el.style.height = '0px';
    }
  }

  /** Overlay each mermaid diagram over its reserved grid box (same col/row ->
   *  px math paintImages() uses: PAD_L/PAD_T + metrics). Prefers a REAL
   *  mermaid.js render of `mb.source` (Task 3 of the mermaid-live-render
   *  plan); falls back to `buildMermaidSvg(mb.geo)` — today's geometry-mirror
   *  SVG — whenever mermaid isn't available, the box has no `source`, or the
   *  real render rejects. Render is async (`MERMAID.render` returns a
   *  Promise), so a box's element is appended up front holding the fallback
   *  as an interim, then swapped in place for the real svg on resolve — see
   *  mmdVersion/mmdCache above for the staleness guard and the render cache. */
  function paintMermaid() {
    for (const el of mmdEls) el.remove();
    mmdEls = [];
    const myVersion = ++mmdVersion;
    for (const mb of lastView.mermaid || []) {
      const left = PAD_L + mb.col * metrics.charW;
      const top = PAD_T + mb.row * metrics.lineH;
      const contentW = Math.max(200, docEl.clientWidth - left - PAD_L);
      const el = document.createElement('div');
      el.className = 'docimg'; // reuses the image overlay's position:absolute
                                // + pointer-events:none rule — no new CSS needed.
      el.style.left = left + 'px';
      el.style.top = top + 'px';
      docEl.appendChild(el);
      mmdEls.push(el);

      if (!MERMAID || !mb.source) {
        fallbackInto(el, mb);
        continue;
      }
      const cached = mmdCache.get(mb.source);
      if (cached) {
        paintMermaidSvgInto(el, cached, contentW);
        continue;
      }
      fallbackInto(el, mb); // interim, while the real render is in flight
      const id = 'mmd-' + ++mmdRenderSeq;
      MERMAID.render(id, mb.source)
        .then(({ svg }) => {
          if (myVersion !== mmdVersion) return; // a newer paint superseded this one
          mmdCache.set(mb.source, svg);
          paintMermaidSvgInto(el, svg, contentW);
        })
        .catch(() => {
          // el already holds fallbackInto()'s interim result above — a
          // broken/unsupported diagram source just keeps showing that
          // (or nothing, if there was no geometry either), never blank/crash.
        });
    }
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

  // Markdown mode, DISPLAY ONLY: a task-list item's model/save text is the
  // literal `[ ] `/`[x] `/`[X] ` that Markdown's own task-list syntax uses (see
  // docxcore's markdown.rs — round-tripped verbatim, never escaped). This only
  // reskins that literal 4-character prefix into a checkbox glyph in the DOM;
  // it never calls `cmd`/`userCmd`, so the wasm model — and therefore
  // `docx_save`/`docx_to_md` — still sees `[ ] `/`[x] ` untouched.
  //
  // The grid is a strict character grid: caret placement (`col * charW`) and
  // click/drag hit-testing (`x / charW`) both work in MODEL columns. A naive
  // text substitution (`"[ ] "` → `"☐ "`) would shrink 4 model columns down to
  // 2 rendered characters, shifting every caret/click column after it on the
  // line by ~2 — wrong pointer feedback, even though the model/save stay
  // correct. `checkboxGlyph()` instead reports the glyph AND the leftover text
  // separately, and the caller renders the glyph in its own `width:4ch` inline
  // box (see `paint()` below) — pinning the glyph to exactly the 4 display
  // columns the 4 model characters `[ ] `/`[x] ` occupy, so every column after
  // it lines up with the model exactly as it did before the reskin.
  const MD_CHECKBOX_RE = /^\[( |[xX])\] /;
  function checkboxGlyph(text) {
    const m = MD_CHECKBOX_RE.exec(text);
    if (!m) return null;
    return { glyph: m[1] === ' ' ? '☐' : '☑', rest: text.slice(4) };
  }

  /** Style one rendered span element from its wasm view-JSON span object
   *  (bold/italic/underline/strike/dim/selected/color/link). Shared by the
   *  plain-span path and the checkbox glyph's two-piece split below, so a
   *  formatted task-list run (rare, but not impossible) keeps its formatting
   *  on both the glyph box and the remainder text. */
  function styleSpan(el, sp) {
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
  }

  function paint() {
    const frag = document.createDocumentFragment();
    for (const line of lastView.lines) {
      const div = document.createElement('div');
      div.className = 'line';
      const spans = line.sp;
      if (spans.length === 0) {
        div.appendChild(document.createTextNode('​')); // keep empty lines tall
      }
      // docxcore's render_paragraph always pushes the list marker (bullet glyph
      // or the equivalent blank-space prefix on a wrapped continuation line) as
      // its own span BEFORE the paragraph's own content spans, whenever the
      // paragraph carries a numId — so on a real list-item line (`line.list`,
      // from the wasm view's per-line flag) the checkbox-eligible text is always
      // spans[1], never spans[0] (the marker) or anything later in the line.
      // Scoping to that one index — and only when the line is actually a list
      // item — is what keeps a coincidental "[ ] " elsewhere in a paragraph (or
      // in a non-list paragraph entirely) from being misread as a task item.
      // This "spans[1] is the content" assumption holds for every UI-driven edit
      // (alignment ops are unreachable in markdown mode, see userCmd()'s guard),
      // but the agent control surface (`doc.format`/`doc.set-style` with an
      // align patch) can still reach the model directly and insert a lead span
      // ahead of the marker — in that case the index no longer lines up with
      // the checkbox text, and it just renders as the literal `[ ] ` string
      // instead of the glyph (never corrupting anything, only degrading the
      // cosmetic reskin back to plain text).
      const checkboxIdx = MD_MODE && line.list ? 1 : -1;
      spans.forEach((sp, si) => {
        const cb = si === checkboxIdx ? checkboxGlyph(sp.t) : null;
        if (cb) {
          // Fixed-width box: exactly 4 display columns for the 4 model columns
          // `[ ] `/`[x] ` occupied, regardless of the glyph's own font width.
          const marker = document.createElement('span');
          marker.textContent = cb.glyph;
          marker.style.display = 'inline-block';
          marker.style.width = '4ch';
          marker.style.textAlign = 'left';
          styleSpan(marker, sp);
          div.appendChild(marker);
          const rest = document.createElement('span');
          rest.textContent = cb.rest;
          styleSpan(rest, sp);
          div.appendChild(rest);
          return;
        }
        const el = document.createElement('span');
        el.textContent = sp.t;
        styleSpan(el, sp);
        div.appendChild(el);
      });
      frag.appendChild(div);
    }
    docEl.replaceChildren(frag);
    imgEls = []; // replaceChildren removed the old overlays
    mmdEls = [];
    placeCaret();
    paintImages();
    paintMermaid();
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
   *  lights the dirty dot and can drive undo/redo.
   *
   *  This is the ONE choke point every user-facing entry to a formatting op
   *  passes through — toolbar buttons (buildToolbar()'s click handler),
   *  keybindings (onKeydown, e.g. Ctrl+U), and the command-palette `command`
   *  host message (the `case 'command': userCmd(msg.op)` below) — so the
   *  Markdown-mode guard here covers all three with no per-surface duplication.
   *  In markdown mode, an op Markdown can't represent (MD_HIDDEN_OPS — the same
   *  set buildToolbar() filters the toolbar against) is silently dropped instead
   *  of reaching the wasm model, so it can never create formatting that would
   *  vanish unannounced on the next save. Gated on MD_MODE only, so plain .docx
   *  editing is entirely unaffected. */
  function userCmd(str) {
    if (MD_MODE && MD_HIDDEN_OPS.has(str)) return;
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
      case 'ctl': {
        // One agent control verb (docs/agent-control.md), routed through the
        // same docx_ctl marshalling callBytes() already uses for docx_cmd.
        // Always post a ctlResult, even on an unexpected throw (a wasm trap,
        // say) — the host's pending promise for this requestId has no other
        // way to settle, and silence here would hang the agent's TCP request.
        let raw;
        try {
          raw = dec.decode(callBytes(ex.docx_ctl, enc.encode(msg.payload)));
        } catch (err) {
          raw = JSON.stringify({
            ok: false,
            error: 'docx_ctl threw: ' + (err && err.message ? err.message : String(err)),
          });
        }
        vscode.postMessage({ type: 'ctlResult', requestId: msg.requestId, payload: raw });
        // The host already knows (from its mutating-verb set) whether this
        // call *could* have changed the document; only repaint if it also
        // actually succeeded.
        if (msg.repaint) {
          let ok = false;
          try { ok = JSON.parse(raw).ok === true; } catch { /* leave ok false */ }
          if (ok) render();
        }
        break;
      }
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
    let buttons = [
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
    // Markdown mode can't represent underline, alignment, or font size — the
    // model round-trips through Markdown text, which has no syntax for them.
    // Bold/italic/strike, headings, and list ops all survive the md<->docx
    // conversion, so those stay. (userCmd() below enforces the same set as a
    // no-op guard, so these ops are unreachable even via keybinding/palette —
    // hiding the button here is about decluttering, not the only defense.)
    if (MD_MODE) {
      buttons = buttons.filter((b) => !(b[1] && MD_HIDDEN_OPS.has(b[1])));
      // Drop separators left leading or doubled-up by the op filter (checked
      // against the last *kept* button, not the pre-filter array — two whole
      // groups, align and font size, are adjacent survivors here, so a
      // position-indexed check would miss the resulting double separator).
      buttons = buttons.reduce((acc, b) => {
        if (b[0] === SEP && (acc.length === 0 || acc[acc.length - 1][0] === SEP)) return acc;
        acc.push(b);
        return acc;
      }, []);
      if (buttons.length && buttons[buttons.length - 1][0] === SEP) buttons.pop();
    }
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
    const resp = await fetch(window.__OFFXY__.wasmUri);
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
