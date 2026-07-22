// scripts/mmcompare/render.mjs — the Phase-2 gate for real-mermaid.js
// rendering in the docxy webview.
//
// Renders a Mermaid source TWO ways and drops both, plus a side-by-side
// composite, into an output directory:
//   1. "real"  — the actual mermaid.js (mermaid@10, loaded from this
//      package's node_modules) rendering the raw Mermaid source in a
//      headless browser.
//   2. "ours"  — docxcore's own layout engine. `cargo run --example dump_geo`
//      (docxcore/examples/dump_geo.rs) prints `mermaid::geometry_box(src)`'s
//      JSON (the same `DiagramGeometry`/`SequenceGeometry` shape the Word
//      exporter turns into DrawingML shapes); `buildMermaidSvg`, pulled
//      unmodified out of offxy-vscode/media/webview.js via a `vm` sandbox
//      (mirroring media/mermaid-svg.test.mjs), turns that JSON into an
//      inline SVG the same way the webview overlay does. That SVG's viewBox
//      is in EMU (914400 per inch — the DrawingML unit), so it's wrapped in
//      an explicit-pixel-sized container (EMU/914400*96) before rasterizing.
//   3. "cmp"   — both PNGs side by side, composed by embedding them as
//      `data:` URIs in one more HTML page and screenshotting THAT (no image
//      library needed).
//
// Every render goes through the same headless-browser screenshot: Edge
// (`--headless=new`) by default, falling back to a Playwright Chromium
// under %USERPROFILE%\AppData\Local\ms-playwright if Edge isn't installed.
//
// Usage: node render.mjs <file.mmd|file.md> [outdir]
//   .md inputs have their ```mermaid ...``` fence body extracted first.
//
// This is a dev tool, not shipped code: no export surface, no test file of
// its own (it IS the acceptance tool for later phases of the mermaid
// live-render plan).

import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import vm from 'node:vm';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '..', '..');

// ---- headless browser discovery --------------------------------------------
// Edge first (present on every dev machine in this shop); a Playwright
// Chromium (installed as a side effect of `npx playwright install` elsewhere
// in the repo) as a fallback so the harness still works on a box without
// Edge.
function findBrowser() {
  const edge = 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe';
  if (existsSync(edge)) return edge;

  const home = process.env.USERPROFILE || process.env.HOME || '';
  const pwDir = join(home, 'AppData', 'Local', 'ms-playwright');
  if (existsSync(pwDir)) {
    const candidates = readdirSync(pwDir)
      .filter((d) => d.startsWith('chromium-'))
      .sort()
      .reverse(); // highest version first
    for (const d of candidates) {
      const chrome = join(pwDir, d, 'chrome-win', 'chrome.exe');
      if (existsSync(chrome)) return chrome;
    }
  }
  throw new Error(
    'No headless browser found: expected Edge at ' +
      edge +
      ' or a Playwright Chromium under ' +
      pwDir
  );
}

// Screenshots one HTML file to one absolute PNG path via headless Chromium.
// Output paths MUST be absolute — Chromium's `--screenshot` rejects relative
// paths with "Access is denied". Each call gets its own `--user-data-dir` —
// concurrent headless runs sharing a profile fail to acquire its lock.
function screenshot(browserPath, htmlPathAbs, pngPathAbs, widthPx, heightPx) {
  const userDataDir = mkdtempSync(join(tmpdir(), 'mmcompare-profile-'));
  execFileSync(
    browserPath,
    [
      '--headless=new',
      '--disable-gpu',
      `--user-data-dir=${userDataDir}`,
      '--hide-scrollbars',
      `--window-size=${Math.round(widthPx)},${Math.round(heightPx)}`,
      '--virtual-time-budget=12000',
      `--screenshot=${pngPathAbs}`,
      pathToFileURL(htmlPathAbs).href,
    ],
    { stdio: 'pipe' }
  );
}

// ---- mermaid source extraction ---------------------------------------------
// Accepts a bare `.mmd` file (used as-is) or a `.md` file (the body of its
// first ```mermaid fence is extracted — samples under offxy-vscode/samples
// /mermaid are markdown write-ups with the diagram fenced inside).
function extractMermaidSource(filePath) {
  const raw = readFileSync(filePath, 'utf8');
  if (extname(filePath).toLowerCase() !== '.md') return raw;
  const m = raw.match(/```mermaid\s*\n([\s\S]*?)```/);
  if (!m) throw new Error(`no \`\`\`mermaid fence found in ${filePath}`);
  return m[1].replace(/\s+$/, '') + '\n';
}

// ---- "ours": docxcore geometry -> webview.js's buildMermaidSvg -------------
// Shells `cargo run --example dump_geo` (docxcore/examples/dump_geo.rs) to
// get the same `DiagramGeometry`/`SequenceGeometry` JSON the webview
// receives at runtime.
function getGeometry(mmdPathAbs) {
  const stdout = execFileSync(
    'cargo',
    ['run', '-q', '--example', 'dump_geo', '-p', 'docxcore', '--', mmdPathAbs],
    { cwd: repoRoot, encoding: 'utf8' }
  );
  return JSON.parse(stdout.trim());
}

// Loads offxy-vscode/media/webview.js, UNMODIFIED, in a `vm` sandbox just
// complete enough that its top-level statements don't throw (mirrors
// offxy-vscode/media/mermaid-svg.test.mjs), and returns its top-level
// `buildMermaidSvg` function.
function loadBuildMermaidSvg() {
  const webviewPath = join(repoRoot, 'offxy-vscode', 'media', 'webview.js');
  class El {
    constructor() {
      this.style = {};
    }
    addEventListener() {}
  }
  const document = {
    getElementById: () => new El(),
    createElement: () => new El(),
    createDocumentFragment: () => new El(),
    addEventListener: () => {},
  };
  const sandbox = {
    window: { addEventListener: () => {} },
    document,
    acquireVsCodeApi: () => ({ postMessage: () => {} }),
    TextEncoder,
    TextDecoder,
    console,
    setTimeout,
    clearTimeout,
  };
  vm.createContext(sandbox);
  vm.runInContext(readFileSync(webviewPath, 'utf8'), sandbox, { filename: 'webview.js' });
  if (typeof sandbox.buildMermaidSvg !== 'function') {
    throw new Error('webview.js did not expose buildMermaidSvg as a top-level function');
  }
  return sandbox.buildMermaidSvg;
}

const EMU_PER_INCH = 914400;
const PX_PER_INCH = 96;
function emuToPx(emu) {
  return Math.round((emu / EMU_PER_INCH) * PX_PER_INCH);
}

// ---- main -------------------------------------------------------------------
function main() {
  const [, , inputArg, outdirArg] = process.argv;
  if (!inputArg) {
    console.error('usage: node render.mjs <file.mmd|file.md> [outdir]');
    process.exit(1);
  }
  const inputPath = resolve(process.cwd(), inputArg);
  const outdir = resolve(process.cwd(), outdirArg || 'out');
  mkdirSync(outdir, { recursive: true });
  const name = basename(inputPath).replace(/\.(md|mmd)$/i, '');

  const browser = findBrowser();

  const mermaidSrc = extractMermaidSource(inputPath);
  // dump_geo.rs reads its argument as a raw file, with no fence-stripping of
  // its own, so the extracted body is written to a scratch .mmd before it's
  // handed to `cargo run`.
  const mmdTmpPath = join(outdir, `${name}.extracted.mmd`);
  writeFileSync(mmdTmpPath, mermaidSrc, 'utf8');

  // ---- ours: geometry JSON -> inline SVG -> screenshot ----
  const geo = getGeometry(mmdTmpPath);
  const buildMermaidSvg = loadBuildMermaidSvg();
  const oursSvg = buildMermaidSvg(geo);
  const canvasWpx = Math.max(emuToPx(geo.canvasW), 200);
  const canvasHpx = Math.max(emuToPx(geo.canvasH), 150);
  const oursHtmlPath = join(outdir, `ours-${name}.html`);
  writeFileSync(
    oursHtmlPath,
    `<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;padding:0;background:#ffffff;}
</style></head><body>
<div style="width:${canvasWpx}px;height:${canvasHpx}px;">${oursSvg}</div>
</body></html>`,
    'utf8'
  );
  const oursPng = join(outdir, `ours-${name}.png`);
  screenshot(browser, oursHtmlPath, oursPng, canvasWpx, canvasHpx);

  // ---- real: mermaid.js -> screenshot ----
  const mermaidJsPath = join(here, 'node_modules', 'mermaid', 'dist', 'mermaid.min.js');
  if (!existsSync(mermaidJsPath)) {
    throw new Error(
      `mermaid.min.js not found at ${mermaidJsPath} — run \`npm i\` in scripts/mmcompare first`
    );
  }
  // Real mermaid sizes its own SVG; the window just needs to be generously
  // larger than our geometry estimate so nothing clips (headless
  // `--screenshot` captures the window viewport, not a scrolled full page).
  const realWpx = Math.min(Math.max(canvasWpx * 1.4, 900), 3200);
  const realHpx = Math.min(Math.max(canvasHpx * 1.4, 700), 2400);
  const realHtmlPath = join(outdir, `real-${name}.html`);
  writeFileSync(
    realHtmlPath,
    `<!doctype html><html><head><meta charset="utf-8">
<script src="${pathToFileURL(mermaidJsPath).href}"></script>
<style>html,body{margin:0;padding:20px;background:#ffffff;}</style>
</head><body>
<pre class="mermaid">
${mermaidSrc}
</pre>
<script>
mermaid.initialize({
  startOnLoad: true,
  securityLevel: 'loose',
  flowchart: { useMaxWidth: false },
  sequence: { useMaxWidth: false },
});
</script>
</body></html>`,
    'utf8'
  );
  const realPng = join(outdir, `real-${name}.png`);
  screenshot(browser, realHtmlPath, realPng, realWpx, realHpx);

  // ---- cmp: both PNGs, side by side, composed via a data:-URI HTML page ----
  const realB64 = readFileSync(realPng).toString('base64');
  const oursB64 = readFileSync(oursPng).toString('base64');
  const cmpWpx = Math.round(realWpx + canvasWpx + 80);
  const cmpHpx = Math.round(Math.max(realHpx, canvasHpx) + 80);
  const cmpHtmlPath = join(outdir, `cmp-${name}.html`);
  writeFileSync(
    cmpHtmlPath,
    `<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;padding:20px;background:#ffffff;font-family:sans-serif;}
.row{display:flex;gap:20px;align-items:flex-start;}
.col{text-align:center;}
h3{margin:4px 0 8px;font-size:14px;font-weight:600;}
img{border:1px solid #cccccc;display:block;}
</style></head><body>
<div class="row">
  <div class="col"><h3>real mermaid.js</h3><img width="${Math.round(realWpx)}" height="${Math.round(realHpx)}" src="data:image/png;base64,${realB64}"></div>
  <div class="col"><h3>ours (docxcore geometry)</h3><img width="${canvasWpx}" height="${canvasHpx}" src="data:image/png;base64,${oursB64}"></div>
</div>
</body></html>`,
    'utf8'
  );
  const cmpPng = join(outdir, `cmp-${name}.png`);
  screenshot(browser, cmpHtmlPath, cmpPng, cmpWpx, cmpHpx);

  console.log(`browser:  ${browser}`);
  console.log(`real:     ${realPng}`);
  console.log(`ours:     ${oursPng}`);
  console.log(`cmp:      ${cmpPng}`);
}

main();
