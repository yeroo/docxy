// docxy editable HTML: the page. It draws the docxy desktop suite's window —
// title bar with the Quick Access Toolbar, the docx ribbon from the suite's
// snapshot, the File Backstage, a page-style document surface and the status
// bar — and edits the embedded .docx through the docxwasm engine.
//
// The document is real DOM rendered from docx_doc. It is contenteditable so
// the browser does caret movement, clicks, drags, IME and bidi, but every
// edit is cancelled in beforeinput and sent to the engine instead; the page
// then re-renders from the model. The DOM never becomes the source of truth.
(function () {
  'use strict';

  var E = globalThis.DocxyEngine;
  var S = {
    engine: null,
    ribbon: null,
    meta: null,
    model: null,
    state: {},
    dirty: false,
    tab: 'Home',
    backstage: false,
    bsPane: 'info',
    themePref: 'auto',
    keytips: 'off',
    showMarks: false,
    webLayout: false,
    ribbonMin: false,
    zoom: 1,
    composing: false,
    lastSelKey: '',
    fileHandle: null,
    media: {},
    status: 'loaded',
  };
  var el = {};

  function $(id) { return document.getElementById(id); }
  function textOf(id) { return $(id).textContent; }

  function h(tag, attrs, kids) {
    var n = document.createElement(tag);
    if (attrs) {
      Object.keys(attrs).forEach(function (k) {
        var v = attrs[k];
        if (v === undefined || v === null || v === false) return;
        if (k === 'class') n.className = v;
        else if (k === 'text') n.textContent = v;
        else if (k.slice(0, 2) === 'on') n.addEventListener(k.slice(2), v);
        else if (k === 'dataset') Object.keys(v).forEach(function (d) { n.dataset[d] = v[d]; });
        else n.setAttribute(k, v === true ? '' : v);
      });
    }
    (kids || []).forEach(function (c) {
      if (c === null || c === undefined || c === false) return;
      n.appendChild(typeof c === 'string' ? document.createTextNode(c) : c);
    });
    return n;
  }

  function esc(s) {
    return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }

  var tpl = document.createElement('template');
  function icon(name, size) {
    var svg = S.ribbon.icons[name];
    if (!svg) return h('span');
    tpl.innerHTML = svg;
    var n = tpl.content.firstElementChild.cloneNode(true);
    n.setAttribute('width', size);
    n.setAttribute('height', size);
    n.setAttribute('class', 'icon');
    n.setAttribute('aria-hidden', 'true');
    return n;
  }

  function toast(msg) {
    var t = h('div', { class: 'toast', role: 'status', text: msg });
    document.body.appendChild(t);
    setTimeout(function () { t.remove(); }, 2200);
  }

  // ---- theme (Auto follows prefers-color-scheme, like the suite's ThemePref) --

  var THEME_LABELS = { auto: '\u25D1 Auto', light: '\u2600 Light', dark: '\u263D Dark' };
  var TOKEN_VARS = {
    background: '--bg', foreground: '--fg', mutedForeground: '--dim', border: '--border',
    secondary: '--panel', sidebar: '--sidebar', tabActive: '--tab-active', selection: '--selection',
    titleBar: '--title-bar', titleBarBorder: '--title-bar-border', statusBar: '--status-bar',
    popover: '--popover', popoverForeground: '--popover-fg',
  };
  var darkQuery = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;

  function resolvedTheme() {
    if (S.themePref === 'auto') return darkQuery && darkQuery.matches ? 'dark' : 'light';
    return S.themePref;
  }

  function applyTheme() {
    var mode = resolvedTheme();
    var root = document.documentElement;
    root.dataset.theme = mode;
    var th = S.ribbon.theme;
    var tokens = th[mode];
    Object.keys(TOKEN_VARS).forEach(function (k) {
      if (tokens[k]) root.style.setProperty(TOKEN_VARS[k], tokens[k]);
    });
    root.style.setProperty('--brand', th.brand);
    root.style.setProperty('--on-brand', th.onBrand);
    root.style.setProperty('--canvas', th.canvas[mode]);
    root.style.setProperty('--ink', th.pageInk);
    if (el.themeBtn) el.themeBtn.textContent = THEME_LABELS[S.themePref];
  }

  function cycleTheme() {
    S.themePref = { auto: 'light', light: 'dark', dark: 'auto' }[S.themePref];
    try { localStorage.setItem('docxy.theme', S.themePref); } catch (e) { /* private mode */ }
    applyTheme();
  }

  // ---- chrome ---------------------------------------------------------------

  function bundleFileName() {
    var last = decodeURIComponent((location.pathname || '').split('/').pop() || '');
    if (/\.html?$/i.test(last)) return last;
    return E.metaGet(S.meta, 'sourceName') + '.html';
  }

  function sourceName() {
    return E.metaGet(S.meta, 'sourceName') || 'document.docx';
  }

  function buildChrome() {
    var app = $('app');
    app.textContent = '';
    el.chip = h('div', { class: 'chip', id: 'doc-chip' });
    el.themeBtn = h('button', { class: 'theme-btn', id: 'theme-btn', title: 'Theme', onclick: cycleTheme });
    var qat = h('div', { class: 'qat' }, S.ribbon.qat.map(function (q) {
      return h('button', {
        class: 'qat-btn', id: q.id, 'aria-label': q.label, dataset: { tip: q.tip },
        onmousedown: keepFocus,
        onclick: function () { run(q.action); },
      }, [icon(q.icon, 16)]);
    }));
    app.appendChild(h('div', { class: 'titlebar' }, [
      h('div', { class: 'wordmark', text: 'docxy' }), qat,
      h('div', { class: 'chips' }, [el.chip]), h('div', { class: 'spacer' }), el.themeBtn,
    ]));
    el.tabs = h('div', { class: 'ribbon-tabs', role: 'tablist' });
    el.ribbon = h('div', { class: 'ribbon', id: 'ribbon' });
    el.findbar = buildFindBar();
    el.surface = h('div', { class: 'surface', id: 'surface' });
    el.page = h('div', { class: 'page', id: 'page' });
    el.doc = h('div', {
      class: 'doc', id: 'doc', contenteditable: 'true', spellcheck: 'false',
      role: 'textbox', 'aria-multiline': 'true', 'aria-label': 'Document',
    });
    el.page.appendChild(el.doc);
    el.surface.appendChild(h('div', { class: 'pages' }, [el.page]));
    el.backstage = buildBackstage();
    el.status = h('span', { class: 'state', id: 'status-state' });
    el.stats = h('span', { id: 'status-stats' });
    el.zoomPct = h('button', { class: 'pct', title: 'Reset zoom', onclick: function () { setZoom(1); } });
    app.appendChild(el.tabs);
    app.appendChild(el.ribbon);
    app.appendChild(el.findbar);
    app.appendChild(el.surface);
    app.appendChild(el.backstage);
    app.appendChild(h('div', { class: 'statusbar' }, [
      el.status, h('span', { text: '\u00b7' }), el.stats, h('div', { class: 'spacer' }),
      h('span', { text: 'type \u00b7 Ctrl+B/I/U \u00b7 Ctrl+F find \u00b7 Ctrl+C/X/V \u00b7 Ctrl+Z/Y \u00b7 Ctrl+S' }),
      h('div', { class: 'zoom' }, [
        h('button', { id: 'zoom-out', text: '\u2212', onclick: function () { setZoom(S.zoom - 0.1); } }),
        el.zoomPct,
        h('button', { id: 'zoom-in', text: '+', onclick: function () { setZoom(S.zoom + 0.1); } }),
      ]),
    ]));
    wireDocument();
  }

  function setZoom(z) {
    S.zoom = Math.min(3, Math.max(0.5, Math.round(z * 10) / 10));
    el.page.style.zoom = S.zoom;
    el.zoomPct.textContent = Math.round(S.zoom * 100) + '%';
  }

  function updateChip() {
    el.chip.textContent = '\uD83D\uDCC4 ' + bundleFileName() + (S.dirty ? ' \u2022' : '');
    document.title = (S.dirty ? '\u2022 ' : '') + bundleFileName();
    el.status.textContent = S.dirty ? 'Unsaved changes' : (S.status === 'saved' ? 'Saved' : 'Saved \u00b7 editable HTML');
    el.status.classList.toggle('unsaved', S.dirty);
  }

  function markDirty() {
    if (!S.dirty) {
      S.dirty = true;
      updateChip();
    }
  }

  // ---- ribbon ---------------------------------------------------------------

  function keepFocus(e) { e.preventDefault(); }

  function contextualTabs() {
    return S.ribbon.contextual.filter(function (t) {
      return t.context === 'table' && S.state.inTable;
    });
  }

  function currentTabDef() {
    var all = S.ribbon.tabs.concat(contextualTabs());
    return all.filter(function (t) { return t.name === S.tab && t.kind !== 'backstage'; })[0];
  }

  function renderTabs() {
    // The Table tab is only valid while the caret is in a table (suite rule).
    if (!currentTabDef()) S.tab = 'Home';
    el.tabs.textContent = '';
    var add = function (t, contextual) {
      var isFile = t.kind === 'backstage';
      var active = !S.backstage && !isFile && t.name === S.tab;
      var b = h('button', {
        class: 'rtab' + (isFile ? ' file' : '') + (active ? ' active' : '') + (contextual ? ' contextual' : ''),
        role: 'tab', 'aria-selected': active ? 'true' : 'false', dataset: { tab: t.name },
        onmousedown: keepFocus,
        onclick: function () {
          if (isFile) openBackstage();
          else { S.tab = t.name; closeBackstage(); renderTabs(); renderRibbon(); }
        },
        text: t.name,
      });
      if (S.keytips === 'tabs' && t.keyTip) b.appendChild(h('span', { class: 'keytip', text: t.keyTip }));
      el.tabs.appendChild(b);
    };
    S.ribbon.tabs.forEach(function (t) { add(t, false); });
    contextualTabs().forEach(function (t) { add(t, true); });
  }

  function actEnabled(act) {
    return Object.prototype.hasOwnProperty.call(E.ACT_OPS, act);
  }

  function tipText(tip) {
    return { title: tip.title, body: tip.body, shortcut: tip.shortcut };
  }

  function cmdButton(cmd, variant) {
    var unsupported = !actEnabled(cmd.act);
    var reason = E.BROWSER_UNSUPPORTED[cmd.act];
    var kids = [icon(cmd.icon, variant === 'large' ? 32 : 16)];
    if (variant !== 'icon') kids.push(h('span', { class: 'label', text: cmd.label }));
    var b = h('button', {
      class: 'rb ' + variant + (unsupported ? ' unsupported' : ''),
      id: 'rb-' + cmd.id,
      'aria-label': cmd.label,
      'aria-disabled': unsupported ? 'true' : null,
      dataset: { act: cmd.act, cmd: cmd.id, keytip: cmd.keyTip || '' },
      onmousedown: keepFocus,
      onclick: function () { if (!unsupported) runAct(cmd.act, b); },
    }, kids);
    b._tip = unsupported
      ? { title: cmd.label, body: reason || 'Not available in the browser', shortcut: '' }
      : tipText(cmd.tip);
    if (S.keytips === 'commands' && cmd.keyTip) b.appendChild(h('span', { class: 'keytip', text: cmd.keyTip }));
    return b;
  }

  function comboBox(cmd, wide) {
    var b = h('button', {
      class: 'combo' + (wide ? ' wide' : ''), id: 'rb-' + cmd.id,
      'aria-label': cmd.label, dataset: { act: cmd.act, cmd: cmd.id, keytip: cmd.keyTip || '' },
      onmousedown: keepFocus,
      onclick: function () { runAct(cmd.act, b); },
    }, [h('span', { class: 'value' }), h('span', { class: 'caret', text: '\u25BE' })]);
    b._tip = tipText(cmd.tip);
    return b;
  }

  function control(c) {
    switch (c.kind) {
      case 'large': return cmdButton(c.cmd, 'large');
      case 'toggle': return cmdButton(c.cmd, 'icon');
      case 'column':
        return h('div', { class: 'col' }, c.cmds.map(function (x) { return cmdButton(x, 'small'); }));
      case 'split':
        return h('div', { class: 'col' }, [cmdButton(c.primary, 'large')].concat(
          c.menu.map(function (x) { return cmdButton(x, 'small'); })));
      case 'dropdown':
        return h('div', { class: 'col' }, [comboBox(c.cmd, true)]);
      case 'rows':
        return h('div', { class: 'rows' }, c.rows.map(function (row) {
          return h('div', { class: 'row' }, row.map(function (cell) {
            return cell.kind === 'combo' ? comboBox(cell.cmd, cell.wide) : cmdButton(cell.cmd, 'icon');
          }));
        }));
      case 'gallery':
        return h('div', { class: 'gallery', id: 'gallery-' + c.id, role: 'listbox' }, c.items.map(function (it) {
          var b = h('button', {
            class: 'gitem', dataset: { act: it.act }, role: 'option', 'aria-label': it.label,
            onmousedown: keepFocus, onclick: function () { runAct(it.act, b); },
          }, [h('span', { class: 'sample ' + it.preview, text: 'AaBbCc' }), h('span', { class: 'name', text: it.label })]);
          b._tip = { title: it.label, body: '', shortcut: '' };
          return b;
        }));
      case 'separator': return h('div', { class: 'sep' });
      default: return h('div');
    }
  }

  function renderRibbon() {
    el.ribbon.textContent = '';
    el.ribbon.classList.remove('icon-only');
    el.ribbon.hidden = S.ribbonMin || S.backstage;
    var tab = currentTabDef();
    if (!tab) return;
    tab.groups.forEach(function (g) {
      var group = h('div', { class: 'rgroup', dataset: { priority: String(g.priority), title: g.title } }, [
        h('div', { class: 'rgroup-body' }, g.items.map(control)),
        h('div', { class: 'rgroup-title', text: g.title }),
      ]);
      if (g.launcher) {
        var l = h('button', {
          class: 'launcher', 'aria-label': g.title + ' settings', dataset: { act: g.launcher },
          onmousedown: keepFocus, onclick: function () { runAct(g.launcher, l); }, text: '\u2198',
        });
        l._tip = { title: g.title, body: 'More ' + g.title.toLowerCase() + ' options', shortcut: '' };
        group.appendChild(l);
      }
      el.ribbon.appendChild(group);
    });
    refreshRibbonState();
    layoutRibbon();
  }

  // Responsive collapse, as the suite: drop labels first, then collapse the
  // lowest-priority groups into an overflow indicator.
  function layoutRibbon() {
    var r = el.ribbon;
    if (r.hidden) return;
    var over = r.querySelector('.overflow');
    if (over) over.remove();
    var groups = Array.prototype.slice.call(r.querySelectorAll('.rgroup'));
    groups.forEach(function (g) { g.hidden = false; });
    r.classList.remove('icon-only');
    var fits = function () { return r.scrollWidth <= r.clientWidth + 1; };
    if (fits()) return;
    r.classList.add('icon-only');
    var hidden = 0;
    var shown = groups.slice();
    while (!fits() && shown.length > 1) {
      var victim = shown.reduce(function (a, b) { return +b.dataset.priority < +a.dataset.priority ? b : a; });
      victim.hidden = true;
      shown.splice(shown.indexOf(victim), 1);
      hidden++;
    }
    if (hidden) {
      r.appendChild(h('div', { class: 'overflow', title: 'Widen the window to show every group' }, [
        h('div', { class: 'dots', text: '\u22EF' }), h('div', { class: 'more', text: hidden + ' more' }),
      ]));
    }
  }

  var CHECKED = {
    Bold: function (s) { return s.bold; },
    Italic: function (s) { return s.italic; },
    Underline: function (s) { return s.underline; },
    Strike: function (s) { return s.strike; },
    Super: function (s) { return s.superscript; },
    Sub: function (s) { return s.subscript; },
    AlignL: function (s) { return s.align === 'left'; },
    AlignC: function (s) { return s.align === 'center'; },
    AlignR: function (s) { return s.align === 'right'; },
    AlignJ: function (s) { return s.align === 'justify'; },
    Bullets: function (s) { return s.bullets; },
    Numbers: function (s) { return s.numbers; },
    ParaBorders: function (s) { return s.borderBottom; },
    ShowHide: function () { return S.showMarks; },
    PrintLayout: function () { return !S.webLayout; },
    // The Styles gallery's selected item follows the paragraph (suite rule).
    Normal: function (s) { return (s.style === null || s.style === 'Normal') && !s.noSpacing; },
    NoSpacing: function (s) { return s.noSpacing; },
    H1: function (s) { return s.style === 'Heading1'; },
    H2: function (s) { return s.style === 'Heading2'; },
    H3: function (s) { return s.style === 'Heading3'; },
    Title: function (s) { return s.style === 'Title'; },
    Subtitle: function (s) { return s.style === 'Subtitle'; },
  };

  function refreshRibbonState() {
    var s = S.state;
    Array.prototype.forEach.call(el.ribbon.querySelectorAll('[data-act]'), function (b) {
      var f = CHECKED[b.dataset.act];
      var on = !!(f && f(s));
      b.classList.toggle('checked', on);
      if (b.classList.contains('rb') || b.classList.contains('gitem')) {
        b.setAttribute('aria-pressed', on ? 'true' : 'false');
      }
    });
    var name = el.ribbon.querySelector('[data-act="FontName"] .value');
    if (name) name.textContent = s.font || 'Calibri';
    var size = el.ribbon.querySelector('[data-act="FontSize"] .value');
    if (size) size.textContent = s.size ? String(s.size / 2) : '11';
  }

  // ---- screen tips ----------------------------------------------------------

  var tipTimer = null;
  var tipEl = null;
  function hideTip() {
    clearTimeout(tipTimer);
    if (tipEl) { tipEl.remove(); tipEl = null; }
  }
  function wireScreenTips() {
    document.addEventListener('mouseover', function (e) {
      var t = e.target.closest && e.target.closest('button');
      if (!t || (!t._tip && !t.dataset.tip)) return;
      hideTip();
      tipTimer = setTimeout(function () {
        var tip = t._tip || { title: t.dataset.tip, body: '', shortcut: '' };
        tipEl = h('div', { class: 'screentip', role: 'tooltip', id: 'screentip' }, [
          h('div', { class: 'title', text: tip.title }),
          tip.body ? h('div', { class: 'body', text: tip.body }) : null,
          tip.shortcut ? h('div', { class: 'shortcut', text: tip.shortcut }) : null,
        ]);
        document.body.appendChild(tipEl);
        var r = t.getBoundingClientRect();
        var left = Math.min(r.left, window.innerWidth - tipEl.offsetWidth - 8);
        tipEl.style.left = Math.max(4, left) + 'px';
        tipEl.style.top = (r.bottom + 6) + 'px';
      }, 450);
    });
    document.addEventListener('mouseout', function (e) {
      var t = e.target.closest && e.target.closest('button');
      if (t && !t.contains(e.relatedTarget)) hideTip();
    });
  }

  // ---- key tips (Alt) --------------------------------------------------------

  function setKeytips(mode) {
    S.keytips = mode;
    renderTabs();
    renderRibbon();
  }

  function keytipKey(e) {
    var k = e.key.toUpperCase();
    if (S.keytips === 'tabs') {
      var t = S.ribbon.tabs.concat(contextualTabs()).filter(function (t) { return (t.keyTip || '').toUpperCase() === k; })[0];
      if (!t) { setKeytips('off'); return; }
      if (t.kind === 'backstage') { setKeytips('off'); openBackstage(); return; }
      S.tab = t.name;
      setKeytips('commands');
      return;
    }
    var b = Array.prototype.filter.call(el.ribbon.querySelectorAll('[data-keytip]'), function (b) {
      return b.dataset.keytip.toUpperCase() === k;
    })[0];
    setKeytips('off');
    if (b && !b.classList.contains('unsupported')) runAct(b.dataset.act, b);
  }

  // ---- running commands -------------------------------------------------------

  // Send one engine command: sync the DOM selection first, re-render when it
  // changed the document, and put the DOM selection where the engine says.
  function run(cmd) {
    syncSelection();
    var r = S.engine.exec(cmd);
    if (r.applied) {
      markDirty();
      renderDoc();
    }
    restoreSelection(r);
    refreshState();
    return r;
  }

  function runAct(act, anchor) {
    hidePopup();
    var op = E.ACT_OPS[act];
    if (op === undefined) return;
    if (typeof op === 'string') {
      run(op);
      el.doc.focus();
      return;
    }
    switch (op.ui) {
      case 'cut': clipboardCommand('cut'); break;
      case 'copy': clipboardCommand('copy'); break;
      case 'paste': pasteFromClipboard(); break;
      case 'find': toggleFind(true); break;
      case 'picker': openPicker(op.picker, anchor); break;
      case 'marks':
        S.showMarks = !S.showMarks;
        $('app').classList.toggle('marks', S.showMarks);
        refreshRibbonState();
        break;
      case 'layout':
        S.webLayout = !S.webLayout;
        el.surface.classList.toggle('web', S.webLayout);
        refreshRibbonState();
        break;
      case 'theme': cycleTheme(); break;
      case 'ribbon':
        S.ribbonMin = !S.ribbonMin;
        renderRibbon();
        break;
      case 'message': toast(op.text); break;
    }
  }

  function clipboardCommand(kind) {
    var r = run(kind);
    if (r.copied) {
      var done = function () { toast(kind === 'cut' ? 'Cut' : 'Copied'); };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(r.copied).then(done, function () {
          toast('Use Ctrl+' + (kind === 'cut' ? 'X' : 'C') + ' to reach the system clipboard');
        });
      } else {
        toast('Use Ctrl+' + (kind === 'cut' ? 'X' : 'C') + ' to reach the system clipboard');
      }
    }
    el.doc.focus();
  }

  function pasteFromClipboard() {
    if (navigator.clipboard && navigator.clipboard.readText) {
      navigator.clipboard.readText().then(function (t) {
        if (t) run('paste\t' + t);
        el.doc.focus();
      }, function () { toast('Use Ctrl+V to paste in the browser'); });
    } else {
      toast('Use Ctrl+V to paste in the browser');
    }
  }

  // ---- pickers (what the suite opens for these acts) ---------------------------

  var FONTS = ['Calibri', 'Aptos', 'Cambria', 'Arial', 'Times New Roman', 'Georgia', 'Verdana',
    'Segoe UI', 'Consolas', 'Courier New'];
  var SIZES = [8, 9, 10, 10.5, 11, 12, 14, 16, 18, 20, 24, 28, 36, 48, 72];
  var COLORS = ['000000', '404040', '7F7F7F', 'C00000', 'FF0000', 'FFC000', 'FFFF00', '92D050',
    '00B050', '00B0F0', '0070C0', '002060', '7030A0', 'ED7D31', '5B9BD5', '70AD47'];
  var HIGHLIGHTS = ['yellow', 'green', 'cyan', 'magenta', 'blue', 'red', 'darkBlue', 'darkCyan',
    'darkGreen', 'darkMagenta', 'darkRed', 'darkYellow', 'darkGray', 'lightGray', 'black'];
  var HIGHLIGHT_CSS = {
    yellow: '#ffff00', green: '#00ff00', cyan: '#00ffff', magenta: '#ff00ff', blue: '#0000ff',
    red: '#ff0000', darkBlue: '#000080', darkCyan: '#008080', darkGreen: '#008000',
    darkMagenta: '#800080', darkRed: '#800000', darkYellow: '#808000', darkGray: '#808080',
    lightGray: '#c0c0c0', black: '#000000', white: '#ffffff',
  };
  // Word's line-spacing presets (the suite's LINE_SPACINGS).
  var SPACINGS = [['1.0', 240], ['1.15', 276], ['1.5', 360], ['2.0', 480], ['2.5', 600], ['3.0', 720]];
  var SYMBOLS = '\u00A9\u00AE\u2122\u00A7\u00B6\u2020\u2021\u2022\u2026\u2013\u2014\u00B0\u00B1\u00D7\u00F7\u2260' +
    '\u2264\u2265\u221E\u20AC\u00A3\u00A5\u00A2\u03B1\u03B2\u03B3\u03B4\u03C0\u03A3\u03A9\u00B5\u2192' +
    '\u2190\u2191\u2193\u2713\u2717\u00BD\u00BC\u00BE';

  var popupEl = null;
  function hidePopup() {
    if (popupEl) { popupEl.remove(); popupEl = null; }
  }

  function openPicker(kind, anchor) {
    hidePopup();
    var item = function (label, cmd, checked, style) {
      var b = h('button', {
        class: 'item' + (checked ? ' checked' : ''), text: label, onmousedown: keepFocus,
        onclick: function () { hidePopup(); run(cmd); el.doc.focus(); },
      });
      if (style) b.setAttribute('style', style);
      return b;
    };
    var kids = [];
    var s = S.state;
    if (kind === 'font') {
      kids = FONTS.map(function (f) {
        return item(f, 'font\t' + f, s.font === f, 'font-family:"' + f + '"');
      });
    } else if (kind === 'size') {
      kids = SIZES.map(function (z) { return item(String(z), 'setsize\t' + Math.round(z * 2), s.size === z * 2); });
    } else if (kind === 'spacing') {
      kids = SPACINGS.map(function (p) {
        return item(p[0], 'linespacing\t' + p[1] + '\tauto', s.lineSpacing === p[1] / 240);
      });
    } else if (kind === 'color' || kind === 'highlight') {
      var list = kind === 'color' ? COLORS : HIGHLIGHTS;
      kids = [
        item(kind === 'color' ? 'Automatic' : 'No colour', kind === 'color' ? 'color\t' : 'highlight\t'),
        h('div', { class: 'swatches' }, list.map(function (c) {
          var b = h('button', {
            class: 'swatch', title: c, 'aria-label': c, onmousedown: keepFocus,
            onclick: function () { hidePopup(); run((kind === 'color' ? 'color\t' : 'highlight\t') + c); el.doc.focus(); },
          });
          b.style.background = kind === 'color' ? '#' + c : HIGHLIGHT_CSS[c];
          return b;
        })),
      ];
    } else if (kind === 'symbol') {
      kids = [h('div', { class: 'grid' }, Array.from(SYMBOLS).map(function (c) { return item(c, 'insert\t' + c); }))];
    }
    popupEl = h('div', { class: 'popup', id: 'popup-' + kind, role: 'menu' }, kids);
    document.body.appendChild(popupEl);
    var r = (anchor || el.ribbon).getBoundingClientRect();
    popupEl.style.left = Math.max(4, Math.min(r.left, window.innerWidth - popupEl.offsetWidth - 8)) + 'px';
    popupEl.style.top = (r.bottom + 2) + 'px';
  }

  // ---- find & replace ----------------------------------------------------------

  function buildFindBar() {
    var find = h('input', { id: 'find-input', type: 'text', placeholder: 'Find', 'aria-label': 'Find' });
    var repl = h('input', { id: 'replace-input', type: 'text', placeholder: 'Replace with', 'aria-label': 'Replace with' });
    var count = h('span', { class: 'count', id: 'find-count' });
    find.addEventListener('keydown', function (e) {
      if (e.key === 'Enter') { e.preventDefault(); findNext(find.value, count); }
      if (e.key === 'Escape') toggleFind(false);
    });
    return h('div', { class: 'findbar', id: 'findbar', hidden: true }, [
      find,
      h('button', { text: 'Find next', onclick: function () { findNext(find.value, count); } }),
      repl,
      h('button', {
        text: 'Replace all',
        onclick: function () {
          if (!find.value) return;
          var r = run('replace\t' + find.value + '\t' + repl.value);
          count.textContent = r.applied ? 'Replaced' : 'No matches';
        },
      }),
      count,
      h('div', { class: 'spacer' }),
      h('button', { text: '\u00d7', 'aria-label': 'Close find', onclick: function () { toggleFind(false); } }),
    ]);
  }

  function toggleFind(open) {
    el.findbar.hidden = !open;
    if (open) { var i = $('find-input'); i.focus(); i.select(); } else el.doc.focus();
  }

  // Paragraph texts in reading order, from the model.
  function paragraphTexts() {
    var out = [];
    var walk = function (blocks) {
      blocks.forEach(function (b) {
        if (b.t === 'p') {
          var text = '';
          var map = [];
          b.segs.forEach(function (s) {
            if (s.k !== 't') return;
            Array.from(s.x).forEach(function (ch, i) { text += ch; map.push(s.o + i); });
          });
          out.push({ p: b.p, chars: Array.from(text), map: map });
        } else if (b.t === 'tbl') {
          b.rows.forEach(function (row) { row.forEach(function (c) { walk(c.blocks); }); });
        }
      });
    };
    walk(S.model.blocks);
    return out;
  }

  function findNext(query, count) {
    if (!query) return;
    var q = Array.from(query.toLowerCase());
    var paras = paragraphTexts();
    var caret = S.lastCaret || { p: paras.length ? paras[0].p : '0', o: 0 };
    var start = paras.findIndex(function (x) { return x.p === caret.p; });
    if (start < 0) start = 0;
    for (var n = 0; n <= paras.length; n++) {
      var pi = (start + n) % paras.length;
      var para = paras[pi];
      var lower = para.chars.map(function (c) { return c.toLowerCase(); });
      for (var i = 0; i + q.length <= lower.length; i++) {
        var off = para.map[i];
        if (n === 0 && off < caret.o) continue;
        var hit = true;
        for (var j = 0; j < q.length; j++) if (lower[i + j] !== q[j]) { hit = false; break; }
        if (hit) {
          var end = para.map[i + q.length - 1] + 1;
          var r = S.engine.exec('select\t' + para.p + '\t' + off + '\t' + para.p + '\t' + end);
          restoreSelection(r);
          refreshState();
          count.textContent = '';
          return;
        }
      }
    }
    count.textContent = 'No matches';
  }

  // ---- the document surface -------------------------------------------------------

  var TW = 15; // twips per CSS px at 96 dpi (the suite uses the same factor)

  function pt(tw) { return (tw / 20) + 'pt'; }

  function segStyle(s) {
    var st = [];
    if (s.b) st.push('font-weight:700');
    if (s.i) st.push('font-style:italic');
    var deco = [];
    if (s.u || s.ins) deco.push('underline');
    if (s.s || s.del) deco.push('line-through');
    if (deco.length) st.push('text-decoration:' + deco.join(' '));
    if (s.sz) st.push('font-size:' + (s.sz / 2) + 'pt');
    if (s.color) st.push('color:#' + s.color);
    if (s.hl && HIGHLIGHT_CSS[s.hl]) st.push('background:' + HIGHLIGHT_CSS[s.hl]);
    if (s.font) st.push('font-family:"' + s.font.replace(/["\\]/g, '') + '",var(--doc-font)');
    if (s.va === 'sup') st.push('vertical-align:super;font-size:' + (s.sz ? s.sz / 2 * 0.65 + 'pt' : '0.65em'));
    if (s.va === 'sub') st.push('vertical-align:sub;font-size:' + (s.sz ? s.sz / 2 * 0.65 + 'pt' : '0.65em'));
    if (s.caps) st.push('text-transform:uppercase');
    if (s.smallCaps) st.push('font-variant:small-caps');
    if (s.code) st.push('font-family:Consolas,"Courier New",monospace');
    if (s.hidden) st.push('opacity:.45;text-decoration:underline dotted');
    return st.join(';');
  }

  function atom(s, cls, inner, extra) {
    return '<span class="atom ' + cls + '" contenteditable="false" data-o="' + s.o + '" data-w="' + s.w + '"' +
      (extra || '') + '>' + inner + '</span>';
  }

  function segHtml(s) {
    switch (s.k) {
      case 't': {
        var cls = [];
        if (s.href) cls.push('link');
        if (s.ins) cls.push('rev-ins');
        if (s.del) cls.push('rev-del');
        return '<span data-o="' + s.o + '" data-w="' + s.w + '"' +
          (cls.length ? ' class="' + cls.join(' ') + '"' : '') +
          (s.href ? ' title="' + esc(s.href) + '"' : '') +
          (s.rtl ? ' dir="rtl"' : '') +
          ' style="' + esc(segStyle(s)) + '">' + esc(s.x) + '</span>';
      }
      case 'tab': return atom(s, 'tab', '\t');
      case 'br': return '<br class="atom" data-o="' + s.o + '" data-w="' + s.w + '">';
      case 'pagebreak':
      case 'colbreak': return atom(s, 'pagebreak', '');
      case 'rev':
        return atom(s, s.rev === 'del' ? 'rev-del' : 'rev-ins', esc(s.x || ''),
          ' title="' + esc((s.rev === 'del' ? 'Deleted' : 'Inserted') + (s.author ? ' by ' + s.author : '')) + '"');
      case 'field': return atom(s, 'field', esc(s.x || ''));
      case 'eq': return atom(s, 'eq', '<i>' + esc(s.x || '') + '</i>');
      case 'note': return atom(s, 'note', esc(s.x || ''));
      case 'link': return atom(s, 'link', esc(s.x || ''));
      case 'comment': return atom(s, 'comment-mark', '', ' title="Comment"');
      case 'art':
      case 'chart':
      case 'box': return atom(s, 'boxed', esc(s.x || ''));
      case 'img': {
        var w = s.cx ? Math.round(s.cx / 9525) : 96;
        var ht = s.cy ? Math.round(s.cy / 9525) : 96;
        return '<img class="atom" contenteditable="false" alt="" data-o="' + s.o + '" data-w="' + s.w +
          '" data-rid="' + esc(s.rid) + '" width="' + w + '" height="' + ht + '">';
      }
      default: return '';
    }
  }

  function paraHtml(p) {
    var cls = [];
    if (p.head) cls.push('h' + Math.min(p.head, 6));
    if (p.style) cls.push('st-' + p.style.replace(/[^A-Za-z0-9_-]/g, ''));
    if (p.borderBottom) cls.push('border-bottom');
    if (p.borderTop) cls.push('border-top');
    var st = [];
    if (p.align) st.push('text-align:' + p.align);
    var left = p.ind ? p.ind.left : 0;
    if (p.list && !left) left = 360 + 360 * (p.level || 0);
    if (left) st.push('margin-left:' + pt(left));
    if (p.ind && p.ind.right) st.push('margin-right:' + pt(p.ind.right));
    var first = p.ind ? p.ind.first : 0;
    if (p.list && !first) first = -360;
    if (first) st.push('text-indent:' + pt(first));
    var sp = p.spacing;
    if (sp) {
      if (sp.before !== undefined) st.push('margin-top:' + pt(sp.before));
      if (sp.after !== undefined) st.push('margin-bottom:' + pt(sp.after));
      if (sp.line !== undefined) {
        if (!sp.rule || sp.rule === 'auto') st.push('line-height:' + (sp.line / 240 * 1.08).toFixed(3));
        else st.push('line-height:' + pt(sp.line));
      }
    }
    var body = p.segs.map(segHtml).join('');
    var visible = p.segs.some(function (s) { return s.k === 't' || s.k === 'img' || s.w > 0 || s.x; });
    var last = p.segs[p.segs.length - 1];
    // An empty paragraph (or one ending in a line break) needs a filler <br> to
    // have a line box the caret can sit on.
    if (!visible || (last && last.k === 'br')) body += '<br class="fill">';
    var label = p.list ? '<span class="list-label" contenteditable="false">' + esc(p.list) + '</span>' : '';
    return '<p data-p="' + p.p + '" data-len="' + p.len + '"' +
      (cls.length ? ' class="' + cls.join(' ') + '"' : '') +
      (p.rtl ? ' dir="rtl"' : '') +
      (st.length ? ' style="' + esc(st.join(';')) + '"' : '') + '>' + label + body + '</p>';
  }

  function tableHtml(t) {
    var cols = t.grid.length ? '<colgroup>' + t.grid.map(function (w) {
      return '<col style="width:' + Math.round(w / TW) + 'px">';
    }).join('') + '</colgroup>' : '';
    // Grid column of every cell, for vertical merges.
    var colOf = t.rows.map(function (row) {
      var c = 0;
      return row.map(function (cell) { var at = c; c += cell.span; return at; });
    });
    var rowsHtml = t.rows.map(function (row, ri) {
      return '<tr>' + row.map(function (cell, ci) {
        if (cell.vm === 'continue') return '';
        var rowspan = 1;
        if (cell.vm === 'restart') {
          for (var r = ri + 1; r < t.rows.length; r++) {
            var k = colOf[r].indexOf(colOf[ri][ci]);
            if (k < 0 || t.rows[r][k].vm !== 'continue') break;
            rowspan++;
          }
        }
        return '<td' + (cell.span > 1 ? ' colspan="' + cell.span + '"' : '') +
          (rowspan > 1 ? ' rowspan="' + rowspan + '"' : '') + '>' + blocksHtml(cell.blocks) + '</td>';
      }).join('') + '</tr>';
    }).join('');
    return '<table class="tbl">' + cols + '<tbody>' + rowsHtml + '</tbody></table>';
  }

  function blocksHtml(blocks) {
    return blocks.map(function (b) { return b.t === 'tbl' ? tableHtml(b) : paraHtml(b); }).join('');
  }

  function mediaUrl(rid) {
    if (S.media[rid] !== undefined) return S.media[rid];
    var bytes = S.engine.media(rid);
    var type = 'application/octet-stream';
    if (bytes[0] === 0x89 && bytes[1] === 0x50) type = 'image/png';
    else if (bytes[0] === 0xff && bytes[1] === 0xd8) type = 'image/jpeg';
    else if (bytes[0] === 0x47 && bytes[1] === 0x49) type = 'image/gif';
    else if (bytes[0] === 0x42 && bytes[1] === 0x4d) type = 'image/bmp';
    else if (bytes[0] === 0x3c) type = 'image/svg+xml';
    S.media[rid] = bytes.length ? URL.createObjectURL(new Blob([bytes], { type: type })) : '';
    return S.media[rid];
  }

  function renderDoc() {
    S.model = S.engine.doc();
    var pg = S.model.page;
    el.page.style.width = Math.round(pg.w / TW) + 'px';
    el.page.style.minHeight = Math.round(pg.h / TW) + 'px';
    el.page.style.padding = [pg.top, pg.right, pg.bottom, pg.left].map(function (v) {
      return Math.round(v / TW) + 'px';
    }).join(' ');
    el.doc.innerHTML = blocksHtml(S.model.blocks);
    Array.prototype.forEach.call(el.doc.querySelectorAll('img[data-rid]'), function (img) {
      var url = mediaUrl(img.dataset.rid);
      if (url) img.src = url;
    });
    updateStats();
  }

  function updateStats() {
    var words = 0;
    paragraphTexts().forEach(function (p) {
      words += p.chars.join('').split(/\s+/).filter(Boolean).length;
    });
    var contentH = (S.model.page.h - S.model.page.top - S.model.page.bottom) / TW;
    var pages = Math.max(1, Math.ceil(el.doc.scrollHeight / Math.max(1, contentH)));
    el.stats.textContent = pages + ' page' + (pages === 1 ? '' : 's') + ' \u00b7 ' + words + ' word' + (words === 1 ? '' : 's');
  }

  // ---- DOM selection <-> editor positions ----------------------------------------

  function paraOf(node) {
    var n = node && node.nodeType === 3 ? node.parentNode : node;
    return n && n.closest ? n.closest('p[data-p]') : null;
  }

  function segWithin(n, last) {
    if (n.nodeType !== 1) return null;
    if (n.dataset && n.dataset.o !== undefined) return n;
    var all = n.querySelectorAll('[data-o]');
    return all.length ? all[last ? all.length - 1 : 0] : null;
  }

  // The editor position of a DOM point.
  function modelPoint(node, offset) {
    var p = paraOf(node);
    if (!p || !el.doc.contains(p)) return boundaryPoint(node, offset);
    var path = p.dataset.p;
    if (node.nodeType === 3) {
      var seg = node.parentNode.closest('[data-o]');
      if (!seg || !p.contains(seg)) return { p: path, o: 0 };
      var o = +seg.dataset.o;
      if (seg.classList.contains('atom')) return { p: path, o: offset > 0 ? o + +seg.dataset.w : o };
      return { p: path, o: o + E.scalarOffset(node.data, offset) };
    }
    if (node !== p && node.dataset && node.dataset.o !== undefined) {
      return { p: path, o: offset === 0 ? +node.dataset.o : +node.dataset.o + +node.dataset.w };
    }
    var kids = node.childNodes;
    for (var i = offset; i < kids.length; i++) {
      var s = segWithin(kids[i], false);
      if (s) return { p: path, o: +s.dataset.o };
    }
    for (i = Math.min(offset, kids.length) - 1; i >= 0; i--) {
      var t = segWithin(kids[i], true);
      if (t) return { p: path, o: +t.dataset.o + +t.dataset.w };
    }
    return { p: path, o: 0 };
  }

  // A point outside any paragraph (between table cells, at the root): the
  // start of the next paragraph, or the end of the last.
  function boundaryPoint(node, offset) {
    var paras = el.doc.querySelectorAll('p[data-p]');
    if (!paras.length) return { p: '0', o: 0 };
    var r = document.createRange();
    try { r.setStart(node, offset); } catch (e) { return { p: paras[0].dataset.p, o: 0 }; }
    for (var i = 0; i < paras.length; i++) {
      if (r.comparePoint(paras[i], 0) >= 0) return { p: paras[i].dataset.p, o: 0 };
    }
    var last = paras[paras.length - 1];
    return { p: last.dataset.p, o: +last.dataset.len };
  }

  // The DOM point for an editor position.
  function domPoint(pt) {
    var p = el.doc.querySelector('p[data-p="' + pt.p + '"]');
    if (!p) return null;
    var segs = p.querySelectorAll('[data-o]');
    var i, s, o, w;
    for (i = 0; i < segs.length; i++) {
      s = segs[i];
      if (s.classList.contains('atom')) continue;
      o = +s.dataset.o;
      w = +s.dataset.w;
      if (o <= pt.o && pt.o <= o + w && s.firstChild) {
        return [s.firstChild, E.utf16Offset(s.firstChild.data, pt.o - o)];
      }
    }
    for (i = 0; i < segs.length; i++) {
      s = segs[i];
      if (+s.dataset.o >= pt.o) return [s.parentNode, Array.prototype.indexOf.call(s.parentNode.childNodes, s)];
    }
    var fill = p.querySelector('br.fill');
    var end = p.childNodes.length - (fill && fill === p.lastChild ? 1 : 0);
    return [p, Math.max(0, end)];
  }

  function selKey(a, f) { return a.p + ':' + a.o + '|' + f.p + ':' + f.o; }

  // Tell the engine where the DOM selection is (only when it moved).
  function syncSelection() {
    var sel = document.getSelection();
    if (!sel || !sel.rangeCount || !el.doc.contains(sel.anchorNode)) return null;
    var a = modelPoint(sel.anchorNode, sel.anchorOffset);
    var f = modelPoint(sel.focusNode, sel.focusOffset);
    var key = selKey(a, f);
    if (key !== S.lastSelKey) {
      S.engine.exec('select\t' + a.p + '\t' + a.o + '\t' + f.p + '\t' + f.o);
      S.lastSelKey = key;
      S.lastCaret = f;
    }
    return { a: a, f: f };
  }

  // Put the DOM selection where an exec result says the engine's is.
  function restoreSelection(r) {
    if (!r || !r.caret) return;
    var f = domPoint(r.caret);
    var a = r.anchor ? domPoint(r.anchor) : f;
    if (!f || !a) return;
    var sel = document.getSelection();
    try { sel.setBaseAndExtent(a[0], a[1], f[0], f[1]); } catch (e) { return; }
    S.lastSelKey = selKey(r.anchor || r.caret, r.caret);
    S.lastCaret = r.caret;
    var focusEl = f[0].nodeType === 3 ? f[0].parentNode : f[0];
    if (focusEl && focusEl.scrollIntoView && !isVisible(focusEl)) focusEl.scrollIntoView({ block: 'nearest' });
  }

  function isVisible(n) {
    var r = n.getBoundingClientRect();
    var s = el.surface.getBoundingClientRect();
    return r.top >= s.top && r.bottom <= s.bottom;
  }

  function refreshState() {
    S.state = S.engine.state();
    var hadTable = S.ribbon.contextual.some(function (t) { return t.name === S.tab; });
    var tabsChanged = !!S.state.inTable !== !!S.inTableShown;
    S.inTableShown = !!S.state.inTable;
    if (tabsChanged || (hadTable && !S.state.inTable)) {
      renderTabs();
      renderRibbon();
    } else {
      refreshRibbonState();
    }
  }

  // ---- input -------------------------------------------------------------------

  var INPUT_OPS = {
    insertParagraph: ['newline'],
    insertLineBreak: ['newline'],
    deleteContentBackward: ['backspace'],
    deleteContentForward: ['delete'],
    deleteWordBackward: ['move\twordleft\t1', 'backspace'],
    deleteWordForward: ['move\twordright\t1', 'delete'],
    deleteSoftLineBackward: ['move\thome\t1', 'backspace'],
    deleteHardLineBackward: ['move\thome\t1', 'backspace'],
    deleteSoftLineForward: ['move\tend\t1', 'delete'],
    deleteHardLineForward: ['move\tend\t1', 'delete'],
    historyUndo: ['undo'],
    historyRedo: ['redo'],
    formatBold: ['bold'],
    formatItalic: ['italic'],
    formatUnderline: ['underline'],
    formatStrikeThrough: ['strike'],
    formatSuperscript: ['vertalign\tsuper'],
    formatSubscript: ['vertalign\tsub'],
  };

  function runAll(cmds) {
    syncSelection();
    var last = null;
    cmds.forEach(function (c) {
      var r = S.engine.exec(c);
      if (r.applied) markDirty();
      last = r;
    });
    renderDoc();
    restoreSelection(last);
    refreshState();
  }

  function onBeforeInput(e) {
    // IME composition cannot be cancelled: let it draw, and take the result
    // from compositionend (see onCompositionEnd).
    if (e.isComposing || S.composing || e.inputType === 'insertCompositionText') return;
    e.preventDefault();
    var t = e.inputType;
    if (t === 'insertText') {
      if (e.data) run('insert\t' + e.data);
    } else if (t === 'insertFromPaste') {
      var text = e.dataTransfer && e.dataTransfer.getData('text/plain');
      if (text) run('paste\t' + text);
    } else if (INPUT_OPS[t]) {
      runAll(INPUT_OPS[t]);
    }
    // Everything else — drops, drags, spellcheck replacements — is cancelled.
  }

  function onCompositionStart() {
    S.compSel = syncSelection();
    S.composing = true;
  }

  function onCompositionEnd(e) {
    S.composing = false;
    var data = e.data || '';
    // Throw away what the composition drew (the model has not changed), put
    // the selection back where the composition started, and insert its text.
    renderDoc();
    var at = S.compSel;
    if (at) {
      S.lastSelKey = '';
      restoreSelection(S.engine.exec('select\t' + at.a.p + '\t' + at.a.o + '\t' + at.f.p + '\t' + at.f.o));
    }
    if (data) run('insert\t' + data);
  }

  function onKeyDown(e) {
    if (S.keytips !== 'off') {
      if (e.key === 'Escape') { e.preventDefault(); setKeytips('off'); return; }
      if (e.key.length === 1 && !e.ctrlKey && !e.metaKey) { e.preventDefault(); keytipKey(e); return; }
    }
    if (e.key === 'Alt') { S.altAlone = true; return; }
    S.altAlone = false;
    var mod = e.ctrlKey || e.metaKey;
    var k = e.key.toLowerCase();
    var inDoc = el.doc.contains(document.activeElement) || document.activeElement === el.doc;
    if (mod && k === 's') { e.preventDefault(); save(false); return; }
    if (mod && k === 'f') { e.preventDefault(); toggleFind(true); return; }
    if (mod && e.key === 'F1') { e.preventDefault(); runAct('AutoHideRibbon'); return; }
    if (e.key === 'Escape') {
      if (popupEl) { hidePopup(); return; }
      if (S.backstage) { closeBackstage(); return; }
    }
    if (!inDoc || S.composing) return;
    var cmd = null;
    if (mod && !e.shiftKey && k === 'z') cmd = 'undo';
    else if (mod && (k === 'y' || (e.shiftKey && k === 'z'))) cmd = 'redo';
    else if (mod && k === 'b') cmd = 'bold';
    else if (mod && k === 'i') cmd = 'italic';
    else if (mod && k === 'u') cmd = 'underline';
    else if (mod && k === 'a') cmd = 'selectall';
    else if (mod && k === 'm') cmd = e.shiftKey ? 'indent\t-720' : 'indent\t720';
    else if (!mod && e.key === 'Tab') cmd = 'tab';
    if (cmd) {
      e.preventDefault();
      run(cmd);
    }
  }

  function onKeyUp(e) {
    if (e.key === 'Alt' && S.altAlone) {
      S.altAlone = false;
      e.preventDefault();
      setKeytips(S.keytips === 'off' ? 'tabs' : 'off');
    }
  }

  var selPending = false;
  function onSelectionChange() {
    if (S.composing || selPending) return;
    selPending = true;
    requestAnimationFrame(function () {
      selPending = false;
      var sel = document.getSelection();
      if (!sel || !sel.rangeCount || !el.doc.contains(sel.anchorNode)) return;
      syncSelection();
      refreshState();
    });
  }

  function wireDocument() {
    var d = el.doc;
    d.addEventListener('beforeinput', onBeforeInput);
    d.addEventListener('compositionstart', onCompositionStart);
    d.addEventListener('compositionend', onCompositionEnd);
    d.addEventListener('copy', function (e) {
      e.preventDefault();
      syncSelection();
      var r = S.engine.exec('copy');
      if (r.copied) e.clipboardData.setData('text/plain', r.copied);
    });
    d.addEventListener('cut', function (e) {
      e.preventDefault();
      var r = run('cut');
      if (r.copied) e.clipboardData.setData('text/plain', r.copied);
    });
    d.addEventListener('paste', function (e) {
      e.preventDefault();
      var text = e.clipboardData && e.clipboardData.getData('text/plain');
      if (text) run('paste\t' + text);
    });
    d.addEventListener('dragstart', function (e) { e.preventDefault(); });
    d.addEventListener('drop', function (e) { e.preventDefault(); });
    d.addEventListener('click', function (e) {
      // Ctrl+click follows a link, as in Word.
      var link = e.target.closest && e.target.closest('.link[title]');
      if (link && (e.ctrlKey || e.metaKey)) window.open(link.title, '_blank', 'noopener');
    });
  }

  // ---- Backstage ------------------------------------------------------------------

  var RAIL_HELP = {
    new: 'A page saved from docxy holds one document. Use docxy to start a new one.',
    open: 'This page edits the document inside it. Open other files in docxy.',
    close: 'Close the browser tab to close this document.',
  };

  function buildBackstage() {
    var rail = h('div', { class: 'rail', role: 'navigation' }, S.ribbon.backstage.map(function (item) {
      var help = RAIL_HELP[item.action];
      var b = h('button', {
        class: 'rail-item' + (item.action === 'back' ? ' back' : ''), id: item.id,
        disabled: !!help, title: help || null, dataset: { action: item.action },
        text: item.label,
        onclick: function () { railAction(item.action); },
      });
      if (help) b._tip = { title: item.label.replace(/\u2026$/, ''), body: help, shortcut: '' };
      return b;
    }));
    el.bsPane = h('div', { class: 'bs-pane', id: 'bs-pane' });
    return h('div', { class: 'backstage', id: 'backstage', hidden: true }, [rail, el.bsPane]);
  }

  function railAction(action) {
    if (action === 'back') closeBackstage();
    else if (action === 'save') { closeBackstage(); save(false); }
    else if (action === 'saveAs' || action === 'export') { S.bsPane = 'saveAs'; renderBackstagePane(); }
  }

  function card(id, name, sub, onclick) {
    return h('button', { class: 'bs-card', id: id, onclick: onclick }, [
      h('div', { class: 'name', text: name }), h('div', { class: 'sub', text: sub }),
    ]);
  }

  function renderBackstagePane() {
    var p = el.bsPane;
    p.textContent = '';
    Array.prototype.forEach.call(el.backstage.querySelectorAll('.rail-item'), function (b) {
      b.classList.toggle('selected', b.dataset.action === S.bsPane);
    });
    if (S.bsPane === 'saveAs') {
      p.appendChild(h('h1', { text: 'Save As' }));
      p.appendChild(h('div', { class: 'actions' }, [
        card('saveas-html', 'Editable HTML (*.docx.html)', 'This page, with your edits, as ' + bundleFileName(),
          function () { closeBackstage(); save(true); }),
        card('saveas-docx', 'Word Document (*.docx)', 'Download ' + sourceName() + ' with your edits',
          function () { downloadDocx(); }),
      ]));
      return;
    }
    p.appendChild(h('h1', { text: 'Info' }));
    p.appendChild(h('div', { class: 'bs-info' }, [
      h('span', { class: 'muted', text: 'Document' }), h('span', { text: bundleFileName() }),
      h('span', { class: 'muted', text: 'Original' }), h('span', { text: sourceName() }),
      h('span', { class: 'muted', text: 'Exported' }),
      h('span', { id: 'bs-exported', text: (E.metaGet(S.meta, 'exportedAt') || '') + ' by docxy ' + (E.metaGet(S.meta, 'docxyVersion') || '') }),
      h('span', { class: 'muted', text: 'Saving' }),
      h('span', { text: typeof window.showSaveFilePicker === 'function'
        ? 'Save writes this file in place.'
        : 'Save downloads a new copy of this file.' }),
    ]));
    p.appendChild(h('div', { class: 'actions' }, [
      card('info-save', 'Save', 'Keep your edits in ' + bundleFileName(), function () { closeBackstage(); save(false); }),
      card('info-docx', 'Download ' + sourceName(), 'The Word document with your edits', function () { downloadDocx(); }),
    ]));
  }

  function openBackstage() {
    S.backstage = true;
    S.bsPane = 'info';
    el.backstage.hidden = false;
    el.surface.hidden = true;
    el.findbar.hidden = true;
    renderTabs();
    renderRibbon();
    renderBackstagePane();
  }

  function closeBackstage() {
    if (!S.backstage) return;
    S.backstage = false;
    el.backstage.hidden = true;
    el.surface.hidden = false;
    renderTabs();
    renderRibbon();
    el.doc.focus();
  }

  // ---- saving ---------------------------------------------------------------------

  function download(data, name, type) {
    var url = URL.createObjectURL(new Blob([data], { type: type }));
    var a = h('a', { href: url, download: name });
    a.style.display = 'none';
    document.body.appendChild(a);
    a.click();
    setTimeout(function () { a.remove(); URL.revokeObjectURL(url); }, 4000);
  }

  function downloadDocx() {
    download(S.engine.save(), sourceName(),
      'application/vnd.openxmlformats-officedocument.wordprocessingml.document');
  }

  // Save this page with the edits: the file is rebuilt around the new package
  // (DocxyEngine.rebuildFile == htmlbundle::rewrap). Chromium writes in place
  // through the File System Access API; other browsers download a copy.
  function save(saveAs) {
    var built = E.rebuildFile(textOf, S.engine.save());
    var name = bundleFileName();
    var done = function () {
      $('docxy-payload').textContent = built.payload;
      S.meta = built.meta;
      S.dirty = false;
      S.status = 'saved';
      updateChip();
      toast('Saved');
    };
    if (typeof window.showSaveFilePicker !== 'function') {
      download(built.html, name, 'text/html');
      done();
      return Promise.resolve();
    }
    var handle = saveAs ? null : S.fileHandle;
    var pick = handle ? Promise.resolve(handle) : window.showSaveFilePicker({
      suggestedName: name,
      types: [{ description: 'Editable Word document', accept: { 'text/html': ['.html'] } }],
    });
    return pick.then(function (h2) {
      S.fileHandle = h2;
      return h2.createWritable().then(function (w) {
        return w.write(built.html).then(function () { return w.close(); });
      });
    }).then(done, function (err) {
      if (err && err.name === 'AbortError') return;
      toast('Save failed: ' + (err && err.message ? err.message : err));
    });
  }

  // ---- boot -------------------------------------------------------------------------

  function fatal(title, detail) {
    var app = $('app');
    app.textContent = '';
    app.appendChild(h('div', { class: 'fatal', role: 'alert' }, [
      h('h1', { text: title }), h('div', { text: detail }),
    ]));
  }

  function boot() {
    try { S.themePref = localStorage.getItem('docxy.theme') || 'auto'; } catch (e) { S.themePref = 'auto'; }
    if (!THEME_LABELS[S.themePref]) S.themePref = 'auto';
    var payload;
    try {
      S.ribbon = JSON.parse(textOf('docxy-ribbon'));
      applyTheme();
      payload = E.readPayload(textOf('docxy-payload'));
    } catch (err) {
      fatal('This document cannot be opened', err.message);
      return;
    }
    S.meta = payload.meta;
    E.Engine.load(E.b64decode(textOf('docxy-engine'))).then(function (engine) {
      S.engine = engine;
      engine.open(payload.bytes);
      buildChrome();
      applyTheme();
      renderTabs();
      renderRibbon();
      renderDoc();
      setZoom(1);
      updateChip();
      refreshState();
      var start = S.engine.exec('select\t' + S.model.caret.p + '\t0\t' + S.model.caret.p + '\t0');
      el.doc.focus();
      restoreSelection(start);
      document.body.dataset.ready = 'true';
    }).catch(function (err) {
      fatal('This document cannot be opened', String(err && err.message ? err.message : err));
    });
    wireScreenTips();
    document.addEventListener('keydown', onKeyDown, true);
    document.addEventListener('keyup', onKeyUp, true);
    document.addEventListener('selectionchange', onSelectionChange);
    document.addEventListener('mousedown', function (e) {
      if (popupEl && !popupEl.contains(e.target)) hidePopup();
      hideTip();
    });
    window.addEventListener('resize', function () { if (el.ribbon) layoutRibbon(); });
    if (darkQuery && darkQuery.addEventListener) {
      darkQuery.addEventListener('change', function () { if (S.themePref === 'auto') applyTheme(); });
    }
    window.addEventListener('beforeunload', function (e) {
      if (S.dirty) {
        e.preventDefault();
        e.returnValue = '';
      }
    });
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot);
  else boot();
})();
