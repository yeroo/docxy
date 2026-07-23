# Mermaid Live Render — Phase 1 (webview mermaid.js) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render each ` ```mermaid ` diagram in the docxy webview with **real `mermaid.js`** (pixel-perfect), at a readable natural size, and build the real-vs-ours visual-comparison harness that gates Phase 2.

**Architecture:** Bundle `mermaid.min.js` in the extension; `view_json` emits the raw Mermaid **source** per diagram; the webview calls `mermaid.render(source)` and overlays the real SVG (fallback to today's `buildMermaidSvg(geo)` on any error). The Rust geometry engine is untouched this phase (it stays for Word + fallback; Phase 2 improves it).

**Tech Stack:** VS Code webview (`offxy-vscode`), `docxcore`/`docxwasm` (source passthrough), bundled `mermaid.min.js` v10, node + headless Edge/Chromium for the harness.

## Global Constraints

- No extension version bump (`package.json` stays 0.3.0). No agent/ctl/MCP change: `test:mcp-parity` stays **56/56**.
- Rust `docxcore` stays std-only, zero-dep. The Word DrawingML output is UNCHANGED this phase (only an additive `source` field on the view_json mermaid entry).
- Webview must **fall back** to `buildMermaidSvg(geo)` if `mermaid.js` fails to init/render — never a blank or a crash.
- Windows: bash tool for node/git; `export PATH="$HOME/.cargo/bin:$PATH"` before cargo; never pipe an exit-code command through `tail`.
- Live in-editor VS Code e2e is DEFERRED to the maintainer; validation is via headless-browser harness + unit tests only. Subagents must NOT drive the live desktop / open VS Code.

## Current seams (context)

- `offxy-vscode/src/extension.ts:1160-1168` builds the webview CSP: `img-src ${cspSource} data:`, `style-src ${cspSource}`, `script-src 'nonce-${nonce}' 'wasm-unsafe-eval'`. Scripts loaded at :1183-1184 with `nonce`; `asWebviewUri` helper at :1155.
- `offxy-vscode/media/webview.js:431 paintMermaid()` iterates `lastView.mermaid`, guards `mb.geo.nodes.length`, sets `el.innerHTML = buildMermaidSvg(mb.geo)`; overlay positioned by cell rect; `mmdEls` cleared/repainted at :583-586.
- `view_json` `mermaid[]` entries are `{row,col,cols,rows,geo}` built in `docxwasm/src/bridge.rs`; the box + geometry come from `render::MermaidBox` (`docxcore/src/render.rs`, SmartArt arm ~:1588 where `crate::mermaid::source_of(raw)` already yields the source).
- `docxcore/src/mermaid.rs::source_of(raw)` recovers the raw source from the drawing `descr`.

---

### Task 1: Visual-comparison harness (the Phase-2 gate)

A committed dev tool that renders real Mermaid and ours side by side for a `.mmd`.

**Files:** Create `docxcore/examples/dump_geo.rs`; Create `scripts/mmcompare/README.md`, `scripts/mmcompare/render.mjs`, `scripts/mmcompare/package.json`; Modify root `.gitignore` (ignore `scripts/mmcompare/out/`, `scripts/mmcompare/node_modules/`).

**Interfaces:**
- Produces: `cargo run --example dump_geo -- <file.mmd>` prints `mermaid::geometry_box(src)` JSON to stdout (canvas dims to stderr).
- Produces: `node scripts/mmcompare/render.mjs <file.mmd> [outdir]` writes `real-<name>.png`, `ours-<name>.png`, and `cmp-<name>.png` (side-by-side) using local `mermaid.min.js` + headless Edge/Chromium and `webview.js`'s `buildMermaidSvg`.

- [ ] **Step 1: Add `dump_geo.rs`.**

```rust
// Dev tool: print the mermaid geometry JSON for a source file (Phase-2 visual harness).
use std::io::Read;
fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_geo <file>");
    let mut src = String::new();
    std::fs::File::open(&path).expect("open").read_to_string(&mut src).expect("read");
    let (w, h, json) = docxcore::mermaid::geometry_box(&src);
    eprintln!("canvas {w} x {h} EMU ({:.1} x {:.1} in)", w as f64 / 914400.0, h as f64 / 914400.0);
    println!("{json}");
}
```
Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo build --example dump_geo -p docxcore` → compiles.

- [ ] **Step 2: `scripts/mmcompare/render.mjs`.** A node script that: (1) shells `cargo run -q --example dump_geo -- <mmd>` to get ours-geometry JSON; (2) loads `offxy-vscode/media/webview.js` in a `vm` sandbox (mirror `media/mermaid-svg.test.mjs`'s stub) to get `buildMermaidSvg`, produces the ours-SVG, wraps in HTML; (3) writes an HTML that renders the raw source with local `node_modules/mermaid/dist/mermaid.min.js` (`mermaid.initialize({startOnLoad:true, securityLevel:'loose', flowchart:{useMaxWidth:false}, sequence:{useMaxWidth:false}})`); (4) screenshots both with headless Edge (`--headless=new --disable-gpu --user-data-dir=<unique> --window-size=W,H --virtual-time-budget=12000 --screenshot=<ABSOLUTE path>`), and composes a side-by-side `cmp-<name>.png`. Edge path: `C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe` (fall back to a Playwright Chromium under `$HOME/AppData/Local/ms-playwright/chromium-*/chrome-win/chrome.exe` if Edge is absent). **Screenshot output paths MUST be absolute** (relative paths fail with access-denied). Compose the side-by-side by writing an HTML that embeds both PNGs as `data:` URIs and screenshotting that.

- [ ] **Step 3: `scripts/mmcompare/package.json`** with `mermaid@10` as a devDependency and a `"compare": "node render.mjs"` script; `README.md` documenting `npm i && node render.mjs path/to.mmd`.

- [ ] **Step 4: `.gitignore`** — add `scripts/mmcompare/out/` and `scripts/mmcompare/node_modules/`.

- [ ] **Step 5: Smoke test.** `cd scripts/mmcompare && npm i --no-audit --no-fund && node render.mjs ../../offxy-vscode/samples/mermaid/12-aliaksei-provisioning-seq.md out` produces `out/cmp-12-*.png` (non-empty). (The `.md` fence body is extracted — accept a `.md` or `.mmd`; strip the ```` ```mermaid ```` fence.)

- [ ] **Step 6: Commit** (do NOT commit `out/` or `node_modules/`).

```bash
git add docxcore/examples/dump_geo.rs scripts/mmcompare/render.mjs scripts/mmcompare/package.json scripts/mmcompare/README.md .gitignore
git commit -m "tools: mermaid real-vs-ours visual comparison harness"
```

---

### Task 2: Emit the raw Mermaid source in view_json

**Files:** Modify `docxcore/src/render.rs`, `docxwasm/src/bridge.rs`; Test in `docxwasm/src/bridge.rs`.

**Interfaces:**
- `render::MermaidBox` gains `pub source: String`.
- `view_json` `mermaid[]` entries gain `"source": "<raw mermaid text>"` (JSON-escaped).

- [ ] **Step 1: Write the failing bridge test.**

```rust
#[test]
fn view_json_mermaid_carries_source() {
    let doc = docxcore::markdown::from_markdown("```mermaid\nflowchart TD\nA[Start]-->B[End]\n```\n");
    let mut s = /* same Session constructor the neighboring view_json_* tests use */;
    let v = s.view_json(None);
    assert!(v.contains("\"mermaid\":["));
    assert!(v.contains("\"source\":\"flowchart TD"), "{v}");
    // sequence source too
    let doc2 = docxcore::markdown::from_markdown("```mermaid\nsequenceDiagram\nA->>B: hi\n```\n");
    let mut s2 = /* ... */;
    assert!(s2.view_json(None).contains("\"source\":\"sequenceDiagram"));
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxwasm view_json_mermaid_carries_source`

- [ ] **Step 3: Implement.** In `render.rs` add `source: String` to `MermaidBox`; in the SmartArt arm, the `source` from `mermaid::source_of(raw)` (already computed as `src`) is stored on the box (clone). In `bridge.rs::view_json`, add `,"source":<json-escaped mb.source>` to each mermaid entry (reuse the existing JSON string escaper the file uses for other strings). Update the paginate-remap `MermaidBox { … }` construction (render.rs ~:2959) to carry `source: mb.source.clone()`.

- [ ] **Step 4: Run — expect PASS + no regressions.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore && cargo test -p docxwasm && cargo fmt && cargo clippy -p docxcore -p docxwasm --all-targets -- -D warnings`

- [ ] **Step 5: Rebuild wasm + gates.** `cd offxy-vscode && export PATH="$HOME/.cargo/bin:$PATH" && npm run build:wasm && npm run test:md-roundtrip && npm run test:mcp-parity` (56/56).

- [ ] **Step 6: Commit.**

```bash
git commit -am "docxcore/docxwasm: emit raw mermaid source in view_json (for live render)"
```

---

### Task 3: Bundle mermaid.js, fix CSP, render in the webview

**Files:** Modify `offxy-vscode/src/extension.ts` (CSP + bundle script tag), `offxy-vscode/media/webview.js` (`paintMermaid` async render), `offxy-vscode/package.json`/build (copy mermaid into `media/`); Create `offxy-vscode/media/mermaid.min.js` (vendored) + a `media/vendor-mermaid.mjs` copy step OR a committed vendored file; Test: `offxy-vscode/media/mermaid-live.test.mjs`.

**Interfaces:**
- Consumes: `view.mermaid[].source` (Task 2), bundled `mermaid` global.
- Produces: `paintMermaid()` renders real Mermaid; a `renderMermaid(source) -> Promise<svgString>` helper; fallback to `buildMermaidSvg`.

- [ ] **Step 1: Vendor `mermaid.min.js`.** Add `mermaid@10` to `offxy-vscode` devDependencies and a build step that copies `node_modules/mermaid/dist/mermaid.min.js` → `media/mermaid.min.js` (extend `scripts/copy-wasm.mjs` or add `scripts/copy-mermaid.mjs`, wired into `npm run build`). Confirm the file lands in `media/` and is included by `vsce package` (it packages `media/**`). Note the ~3MB size in the report.

- [ ] **Step 2: CSP + script tag (extension.ts).** In the CSP string: change `style-src ${webview.cspSource}` to `style-src ${webview.cspSource} 'unsafe-inline'` (Mermaid injects `<style>` at render; without this they are blocked and the diagram is unstyled/broken) — document why in a comment. Add the mermaid script BEFORE `webview.js`: compute `const mermaidUri = asWebviewUri('mermaid.min.js')` and emit `<script nonce="${nonce}" src="${mermaidUri}"></script>` just before the `webview.js` tag (:1184). `mermaid.min.js` is a UMD bundle → it defines `window.mermaid`. (If the UMD build needs `'unsafe-eval'`, prefer the ESM build loaded as a module, or add `'unsafe-eval'` to `script-src` ONLY if required — validate in Step 5 and keep the CSP as tight as works.)

- [ ] **Step 3: Webview render (webview.js).** Add near the top: `const MERMAID = (typeof mermaid !== 'undefined') ? mermaid : null; if (MERMAID) MERMAID.initialize({ startOnLoad:false, securityLevel:'loose', theme:'default', flowchart:{useMaxWidth:false}, sequence:{useMaxWidth:false} });`. Rewrite `paintMermaid()`:
  - For each `mb` in `lastView.mermaid`: if `MERMAID && mb.source`, call `MERMAID.render('mmd-'+i, mb.source)` (async; v10 returns `{svg}`); on resolve, set the overlay element's `innerHTML = svg` and size the element to the diagram's natural size (parse the returned svg's width/height or `viewBox`; display at min(naturalWidth, contentWidth) preserving aspect, allow vertical scroll — NOT clamped to the `cols×rows` cell box). On reject, fall back to `el.innerHTML = buildMermaidSvg(mb.geo)`.
  - If `!MERMAID` or no source: today's `buildMermaidSvg(mb.geo)` path (guard `mb.geo.nodes?.length`).
  - Because render is async, guard against races: capture the current `lastView` token/version and drop stale renders; cache rendered svg by `mb.source` so unchanged diagrams don't re-render each paint.
  - Keep `mmdEls` cleanup on repaint.

- [ ] **Step 4: Write the headless render test (`media/mermaid-live.test.mjs`).** A node test using the local `mermaid` package (devDep) that asserts `await mermaid.render('t','flowchart TD\nA-->B')` returns an object whose `svg` contains `<svg` and a flowchart marker, and that `sequenceDiagram\nA->>B: hi` yields a sequence svg — i.e. the render call our webview makes works for both kinds. (This validates the render logic, not the live CSP.) Add `"test:mermaid-live": "node media/mermaid-live.test.mjs"` to `package.json` scripts.

- [ ] **Step 5: CSP validation (headless).** Load the REAL provider HTML shell in headless Edge with the actual CSP string and the bundled `mermaid.min.js` + a stub that calls `mermaid.render` on a sample, and confirm (via a screenshot or a console-error check) that Mermaid initializes and renders WITHOUT CSP violations. If CSP blocks it, adjust `style-src`/`script-src` minimally (Step 2) and re-validate. Document the final CSP in the report. (This is a headless-browser check, not live VS Code.)

- [ ] **Step 6: Full gates.**
```bash
cd offxy-vscode && export PATH="$HOME/.cargo/bin:$PATH"
npm run typecheck && npm run build && npm run test:mermaid-live && npm run test:mermaid-svg && npm run test:md-roundtrip && npm run test:grid-layout && npm run test:mcp-parity && npm run package
```
All green; mcp 56/56; `vsce package` includes `media/mermaid.min.js`.

- [ ] **Step 7: Commit.**

```bash
git add offxy-vscode/src/extension.ts offxy-vscode/media/webview.js offxy-vscode/media/mermaid-live.test.mjs offxy-vscode/package.json offxy-vscode/scripts/*.mjs
git commit -m "offxy webview: render diagrams with real mermaid.js (CSP + bundle + async overlay), fallback to geometry SVG"
```

---

## Notes for the executor

- Task 1 (harness) has no unit test in the TDD sense — its gate is Step-5's smoke run producing a non-empty `cmp` PNG; the reviewer confirms the script exists and the smoke output was produced.
- Do NOT vendor-commit a 3MB `mermaid.min.js` into git if a build-time copy works and `vsce package` picks it up from `media/` — but it MUST be present in the packaged `.vsix`. If the copy step is fragile, committing `media/mermaid.min.js` is acceptable (note it in the report).
- Live in-editor visual confirmation is the maintainer's; subagents validate via headless render + unit tests only.
- Phase 2 (matching the Word smart blocks to Mermaid via `scripts/mmcompare`) is a SEPARATE plan — do not start it here.
