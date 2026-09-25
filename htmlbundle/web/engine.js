// docxy editable HTML: the engine glue. DOM-free on purpose, so node tests
// load this very file (vm.runInThisContext) and exercise what the page runs.
//
// It owns: the docxwasm ABI (alloc/free, length-prefixed results), base64,
// SHA-256, the bundle metadata line, and rebuildFile(), which recreates the
// whole .docx.html around a new payload. rebuildFile must equal Rust's
// htmlbundle::rewrap byte for byte; fillTemplate and metaJson mirror
// htmlbundle's fill() and Meta::to_json() for that reason.
(function (root) {
  'use strict';

  var enc = new TextEncoder();
  var dec = new TextDecoder();

  // ---- base64 -------------------------------------------------------------

  function b64decode(text) {
    var clean = text.replace(/\s+/g, '');
    if (typeof atob === 'function') {
      var bin = atob(clean);
      var out = new Uint8Array(bin.length);
      for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      return out;
    }
    return new Uint8Array(Buffer.from(clean, 'base64'));
  }

  function b64encode(bytes) {
    if (typeof btoa === 'function') {
      var parts = [];
      for (var i = 0; i < bytes.length; i += 0x8000) {
        parts.push(String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000)));
      }
      return btoa(parts.join(''));
    }
    return Buffer.from(bytes).toString('base64');
  }

  // ---- SHA-256 (FIPS 180-4) ----------------------------------------------

  var K = new Uint32Array([
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
  ]);

  function sha256Hex(bytes) {
    var h = new Uint32Array([
      0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ]);
    var len = bytes.length;
    var total = Math.ceil((len + 9) / 64) * 64;
    var msg = new Uint8Array(total);
    msg.set(bytes);
    msg[len] = 0x80;
    var view = new DataView(msg.buffer);
    view.setUint32(total - 8, Math.floor(len / 0x20000000), false);
    view.setUint32(total - 4, (len * 8) >>> 0, false);
    var w = new Uint32Array(64);
    for (var off = 0; off < total; off += 64) {
      for (var i = 0; i < 16; i++) w[i] = view.getUint32(off + i * 4, false);
      for (i = 16; i < 64; i++) {
        var x = w[i - 15], y = w[i - 2];
        var s0 = ((x >>> 7) | (x << 25)) ^ ((x >>> 18) | (x << 14)) ^ (x >>> 3);
        var s1 = ((y >>> 17) | (y << 15)) ^ ((y >>> 19) | (y << 13)) ^ (y >>> 10);
        w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
      }
      var a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], hh = h[7];
      for (i = 0; i < 64; i++) {
        var S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
        var ch = (e & f) ^ (~e & g);
        var t1 = (hh + S1 + ch + K[i] + w[i]) | 0;
        var S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
        var maj = (a & b) ^ (a & c) ^ (b & c);
        var t2 = (S0 + maj) | 0;
        hh = g; g = f; f = e; e = (d + t1) | 0; d = c; c = b; b = a; a = (t1 + t2) | 0;
      }
      h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e; h[5] += f; h[6] += g; h[7] += hh;
    }
    var hex = '';
    for (i = 0; i < 8; i++) hex += ('00000000' + h[i].toString(16)).slice(-8);
    return hex;
  }

  // ---- bundle metadata and the self-rebuild -------------------------------

  function hex4(n) {
    return '\\u' + ('0000' + n.toString(16)).slice(-4);
  }

  // A JSON string literal, escaped exactly as htmlbundle's json_string.
  function jsonString(s) {
    var out = '"';
    for (var c of s) {
      var n = c.codePointAt(0);
      if (c === '"') out += '\\"';
      else if (c === '\\') out += '\\\\';
      else if (c === '\n') out += '\\n';
      else if (c === '\r') out += '\\r';
      else if (c === '\t') out += '\\t';
      else if (c === '<' || c === '>' || c === '&' || n === 0x2028 || n === 0x2029) out += hex4(n);
      else if (n < 0x20) out += hex4(n);
      else out += c;
    }
    return out + '"';
  }

  // Meta is an ordered list of [key, value] string pairs.
  function metaJson(fields) {
    return '{' + fields.map(function (kv) {
      return jsonString(kv[0]) + ':' + jsonString(kv[1]);
    }).join(',') + '}';
  }

  function parseMeta(line) {
    var obj = JSON.parse(line);
    return Object.keys(obj).map(function (k) {
      if (typeof obj[k] !== 'string') throw new Error('bundle metadata: ' + k + ' is not a string');
      return [k, obj[k]];
    });
  }

  function metaGet(fields, key) {
    for (var i = 0; i < fields.length; i++) if (fields[i][0] === key) return fields[i][1];
    return undefined;
  }

  function metaSet(fields, key, value) {
    for (var i = 0; i < fields.length; i++) {
      if (fields[i][0] === key) { fields[i][1] = value; return; }
    }
    fields.push([key, value]);
  }

  // The payload element's text: a newline, the meta line, a newline, the
  // base64 package, a newline (htmlbundle's payload_block_text).
  function payloadText(fields, bytes) {
    return '\n' + metaJson(fields) + '\n' + b64encode(bytes) + '\n';
  }

  // Split a payload element's text into its metadata and package bytes, and
  // verify payloadSha256.
  function readPayload(text) {
    var t = text.trim();
    var nl = t.indexOf('\n');
    if (nl < 0) throw new Error('damaged docxy HTML payload');
    var meta = parseMeta(t.slice(0, nl));
    var bytes = b64decode(t.slice(nl + 1));
    var actual = sha256Hex(bytes);
    if (metaGet(meta, 'payloadSha256') !== actual) {
      throw new Error('the embedded document fails its integrity check (payloadSha256 ' +
        metaGet(meta, 'payloadSha256') + ', actual ' + actual + '); the file was altered outside docxy');
    }
    return { meta: meta, bytes: bytes };
  }

  // Single-pass {{name}} substitution, as htmlbundle's fill().
  function fillTemplate(template, slots) {
    var out = '';
    var rest = template;
    for (;;) {
      var i = rest.indexOf('{{');
      if (i < 0) break;
      out += rest.slice(0, i);
      var after = rest.slice(i + 2);
      var j = after.indexOf('}}');
      var name = j >= 0 ? after.slice(0, j) : null;
      if (name !== null && Object.prototype.hasOwnProperty.call(slots, name)) {
        out += slots[name];
        rest = after.slice(j + 2);
      } else {
        out += '{{';
        rest = after;
      }
    }
    return out + rest;
  }

  // The ids of the elements whose raw text fills each data slot.
  var SLOT_IDS = {
    shell: 'docxy-shell',
    css: 'docxy-css',
    ribbon: 'docxy-ribbon',
    engine: 'docxy-engine',
    engine_js: 'docxy-engine-js',
    app_js: 'docxy-app-js',
  };

  // Rebuild the whole file around `bytes`. `textOf(id)` returns an element's
  // raw text (element.textContent on the page). The metadata is the current
  // payload's with payloadSha256 recomputed, so sourceSha256 never changes.
  function rebuildFile(textOf, bytes) {
    var shellText = textOf(SLOT_IDS.shell);
    var template = JSON.parse(shellText);
    var meta = readPayload(textOf('docxy-payload')).meta;
    metaSet(meta, 'payloadSha256', sha256Hex(bytes));
    var slots = {};
    Object.keys(SLOT_IDS).forEach(function (k) { slots[k] = textOf(SLOT_IDS[k]); });
    slots.payload = payloadText(meta, bytes);
    return { html: fillTemplate(template, slots), payload: slots.payload, meta: meta };
  }

  // ---- the wasm engine ----------------------------------------------------

  function Engine(instance) {
    this.ex = instance.exports;
    this.handle = 0;
  }

  Engine.load = function (wasmBytes) {
    return WebAssembly.instantiate(wasmBytes, {}).then(function (r) {
      return new Engine(r.instance);
    });
  };

  Engine.prototype.mem = function () {
    return new Uint8Array(this.ex.memory.buffer);
  };

  // Copy bytes into wasm memory; returns [ptr, len]. The caller frees.
  Engine.prototype.put = function (bytes) {
    var ptr = this.ex.docx_alloc(bytes.length);
    this.mem().set(bytes, ptr);
    return [ptr, bytes.length];
  };

  // Read and free a length-prefixed result buffer.
  Engine.prototype.take = function (ptr) {
    var m = this.mem();
    var len = (m[ptr] | (m[ptr + 1] << 8) | (m[ptr + 2] << 16) | (m[ptr + 3] << 24)) >>> 0;
    var out = m.slice(ptr + 4, ptr + 4 + len);
    this.ex.docx_free(ptr, 4 + len);
    return out;
  };

  Engine.prototype.withInput = function (bytes, f) {
    var a = this.put(bytes);
    try {
      return f(a[0], a[1]);
    } finally {
      this.ex.docx_free(a[0], a[1]);
    }
  };

  Engine.prototype.open = function (bytes) {
    var ex = this.ex;
    this.handle = this.withInput(bytes, function (p, n) { return ex.docx_open(p, n); });
    if (!this.handle) throw new Error('the embedded document could not be opened');
    return this.handle;
  };

  Engine.prototype.json = function (ptr) {
    return JSON.parse(dec.decode(this.take(ptr)));
  };

  // The rich document model (docx_doc).
  Engine.prototype.doc = function () {
    return this.json(this.ex.docx_doc(this.handle));
  };

  // Formatting at the caret (docx_state).
  Engine.prototype.state = function () {
    return this.json(this.ex.docx_state(this.handle));
  };

  // Apply one tab-delimited command (docx_exec).
  Engine.prototype.exec = function (cmd) {
    var ex = this.ex, h = this.handle, self = this;
    return this.withInput(enc.encode(cmd), function (p, n) {
      return self.json(ex.docx_exec(h, p, n));
    });
  };

  // The .docx bytes, losslessly re-serialized.
  Engine.prototype.save = function () {
    return this.take(this.ex.docx_save(this.handle));
  };

  Engine.prototype.media = function (rid) {
    var ex = this.ex, h = this.handle, self = this;
    return this.withInput(enc.encode(rid), function (p, n) {
      return self.take(ex.docx_media(h, p, n));
    });
  };

  // ---- editor offsets vs. the browser's UTF-16 ------------------------------

  // Code points in the first `utf16` code units of `s`.
  function scalarOffset(s, utf16) {
    var n = 0;
    for (var i = 0; i < utf16 && i < s.length; i++) {
      var c = s.charCodeAt(i);
      if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) {
        var d = s.charCodeAt(i + 1);
        if (d >= 0xdc00 && d <= 0xdfff) {
          if (i + 1 >= utf16) break; // inside a pair: count it as before
          i++;
        }
      }
      n++;
    }
    return n;
  }

  // UTF-16 index of the `scalars`-th code point of `s`.
  function utf16Offset(s, scalars) {
    var i = 0;
    for (var n = 0; n < scalars && i < s.length; n++) {
      var c = s.charCodeAt(i);
      i += (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) ? 2 : 1;
    }
    return i;
  }

  // ---- what each ribbon act does in the browser -----------------------------
  //
  // Keyed by the suite's Act name (the snapshot's "act"). A string is an engine
  // command (docx_exec); {ui: …} is handled by the page itself (pickers,
  // clipboard, view toggles), mirroring what the suite does for that act.
  // Every act in the snapshot must be here or in BROWSER_UNSUPPORTED; a node
  // test enforces that, so a new suite command cannot silently do nothing.
  var ACT_OPS = {
    Bold: 'bold',
    Italic: 'italic',
    Underline: 'underline',
    Strike: 'strike',
    Super: 'vertalign\tsuper',
    Sub: 'vertalign\tsub',
    Grow: 'fontsize\t2',
    Shrink: 'fontsize\t-2',
    AlignL: 'align\tleft',
    AlignC: 'align\tcenter',
    AlignR: 'align\tright',
    AlignJ: 'align\tjustify',
    Normal: 'style\t',
    NoSpacing: 'nospacing',
    H1: 'style\tHeading1',
    H2: 'style\tHeading2',
    H3: 'style\tHeading3',
    Title: 'style\tTitle',
    Subtitle: 'style\tSubtitle',
    HRule: 'hrule',
    SelectAll: 'selectall',
    Case: 'case',
    Bullets: 'list\tbullet',
    Numbers: 'list\tnumber',
    IndentInc: 'indent\t720',
    IndentDec: 'indent\t-720',
    ClearFmt: 'clearfmt',
    Sort: 'sort',
    ParaBorders: 'borders',
    Cut: { ui: 'cut' },
    Copy: { ui: 'copy' },
    Paste: { ui: 'paste' },
    Find: { ui: 'find' },
    FontName: { ui: 'picker', picker: 'font' },
    FontSize: { ui: 'picker', picker: 'size' },
    FontColor: { ui: 'picker', picker: 'color' },
    Highlight: { ui: 'picker', picker: 'highlight' },
    LineSpacing: { ui: 'picker', picker: 'spacing' },
    InsertSymbol: { ui: 'picker', picker: 'symbol' },
    ShowHide: { ui: 'marks' },
    PrintLayout: { ui: 'layout' },
    DarkMode: { ui: 'theme' },
    AutoHideRibbon: { ui: 'ribbon' },
    // The suite's launchers are placeholders too; say the same thing.
    LaunchFont: { ui: 'message', text: 'Font — advanced dialog coming soon' },
    LaunchParagraph: { ui: 'message', text: 'Paragraph — advanced dialog coming soon' },
  };

  // Acts the page draws dimmed, with the reason as their tooltip.
  var NOT_YET = 'Not available in the browser yet — open the file in docxy';
  var BROWSER_UNSUPPORTED = {
    InsertTable: NOT_YET,
    PageBreak: NOT_YET,
    RowAbove: NOT_YET,
    RowBelow: NOT_YET,
    ColLeft: NOT_YET,
    ColRight: NOT_YET,
    DelRow: NOT_YET,
    DelCol: NOT_YET,
    DelTable: NOT_YET,
    EditHeader: NOT_YET,
    EditFooter: NOT_YET,
    PageNumber: NOT_YET,
    InsertField: NOT_YET,
    InsertEquation: NOT_YET,
    Columns: NOT_YET,
    Hyphenation: NOT_YET,
    NewComment: NOT_YET,
    ToggleComments: NOT_YET,
    ToggleNotes: NOT_YET,
    ToggleNav: NOT_YET,
    ToggleRuler: NOT_YET,
  };

  root.DocxyEngine = {
    ACT_OPS: ACT_OPS,
    BROWSER_UNSUPPORTED: BROWSER_UNSUPPORTED,
    Engine: Engine,
    b64decode: b64decode,
    b64encode: b64encode,
    sha256Hex: sha256Hex,
    jsonString: jsonString,
    metaJson: metaJson,
    parseMeta: parseMeta,
    metaGet: metaGet,
    metaSet: metaSet,
    payloadText: payloadText,
    readPayload: readPayload,
    fillTemplate: fillTemplate,
    rebuildFile: rebuildFile,
    scalarOffset: scalarOffset,
    utf16Offset: utf16Offset,
    SLOT_IDS: SLOT_IDS,
  };
})(typeof globalThis !== 'undefined' ? globalThis : this);
