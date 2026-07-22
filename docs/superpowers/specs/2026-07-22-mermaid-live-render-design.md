# Mermaid live rendering + smart-block fidelity — design

**Goal:** Make ` ```mermaid ` diagrams look right. Two phases:
1. **Webview preview** renders each diagram with **real `mermaid.js`** (pixel-perfect), and doubles as the ground-truth reference.
2. **Word/docx "smart blocks"** (our editable DrawingML shapes) are brought up to **match Mermaid 1-to-1**, iteratively, verified by **rendered visual comparison** against real Mermaid.

**Basis:** conversational request (2026-07-22). Root problem, proven by a real-vs-ours render comparison: our hand-rolled std-only layout can't match Mermaid — flowcharts explode to a 32:1 strip (cycle rank-explosion), `<br/>` labels aren't wrapped, boxes aren't sized to text, and the webview crams the SVG into a character-grid box. The user chose real-Mermaid fidelity for the preview, then bring the editable shapes up to par by visual comparison.

**Supersedes for the webview:** the hand-rolled `buildMermaidSvg`/`buildSequenceSvg` become a fallback only. The Rust geometry engine (`mermaid.rs`/`mermaid_seq.rs`) is retained and improved for the Word shapes (Phase 2). The **Word==webview** invariant is intentionally relaxed: the webview uses `mermaid.js`; the Word shapes are matched to Mermaid visually instead.

## The visual-comparison harness (built first — the Phase-2 acceptance gate)

A committed dev tool `scripts/mmcompare/` that, given a `.mmd` source, renders BOTH:
- **real Mermaid** → SVG/PNG (bundled `mermaid.min.js` in a headless browser), and
- **ours** → the geometry-driven SVG (`buildMermaidSvg` fed by `mermaid::geometry_box`), which is a faithful proxy for the DrawingML shapes since both consume the same geometry,

into a side-by-side PNG. This is how "match 1-to-1" is judged. Inputs: the four Aliaksei diagrams (2 flowcharts + provisioning/disable sequences) plus a crafted corpus (colors, subgraphs, hexagon, self-message, alt/else, note). Rendering uses the local Playwright Chromium / Edge already present; a tiny Rust `examples/dump_geo.rs` prints `geometry_box(src)` JSON for the ours-SVG path.

## Phase 1 — real Mermaid.js in the webview

**Bundle.** Add `mermaid.min.js` (v10, ~3MB) to `offxy-vscode/media/`, referenced from the webview HTML with the existing script nonce. Validate the webview CSP allows it — Mermaid injects `<style>` and uses `d3`/`dompurify`; the `<meta http-equiv=Content-Security-Policy>` in the provider HTML may need `style-src` to allow Mermaid's injected styles (nonce or a scoped relaxation) and `img-src data:` for its foreignObject/label rendering. This CSP validation is the first implementation step and a real risk — fall back to the current SVG if Mermaid can't initialize.

**Source over the wire.** `view_json`'s `mermaid[]` entries already carry `{row,col,cols,rows,geo}`; add **`source`** — the raw Mermaid text (already available via `mermaid::source_of(raw)` in `render.rs`'s SmartArt arm; add a `source: String` field to `render::MermaidBox` and emit it). The geometry stays (Word still uses it and it's the SVG fallback).

**Render.** In the webview, for each mermaid box with a `source`, call `mermaid.render(id, source)` (async) → real SVG → place it as its own block at a **readable, naturally-sized** area (scaled to the content width, preserving aspect, allowed to be tall and scroll) — NOT crammed into the `cols×rows` character-grid box. This also fixes the "cramming" problem. `mermaid.render` is async, so the paint path gains an async overlay step (render, then insert; re-render on view change; cache by source to avoid re-rendering unchanged diagrams). On any Mermaid error, fall back to `buildMermaidSvg(geo)`.

**Testing (Phase 1):** a headless harness (not live VS Code) that loads `webview.js`'s mermaid path with a `mermaid`-stubbed-then-real environment and asserts a mermaid `<svg class="…flowchart|sequence…">` is produced for a source; CSP validated by loading the real provider HTML in headless Edge and confirming `mermaid.render` succeeds. Live in-editor e2e is deferred to the maintainer. Extension gates (`typecheck`/`build`/`package`) + `test:mcp-parity` 56/56 unaffected; bundle size increase noted.

## Phase 2 — bring the Word smart blocks up to Mermaid, by visual comparison

Iterative, each step gated by `scripts/mmcompare` (ours-SVG vs real-Mermaid, judged visually). Ordered by impact (from the comparison):
1. **Flowchart cycle rank-explosion (bug).** `assign_ranks` (longest-path) doesn't break cycles → ranks grow unbounded around a cycle (`A→B→C→A` → 18in). Break back-edges via DFS before ranking so ranks are bounded and contiguous; collapse empty ranks. This alone removes the 32:1 strip.
2. **`<br/>` multi-line labels + node auto-size.** Stop flattening `<br/>` to a space; wrap the label into lines and size the node box to the wrapped text (raise/remove the 3in cap; height per line count). Matches Mermaid's multi-line boxes.
3. **Node/participant box sizing.** Size boxes snugly to their (wrapped) text with sensible padding, instead of a fixed width — flowchart nodes and sequence participants both.
4. **Proportions/typography.** Recalibrate the font-to-geometry ratio and gaps so text reads at a Mermaid-like density (currently the layout is physically large and text looks tiny).
5. **Edge quality.** Improve edge routing/labels toward Mermaid (curved/relaxed orthogonal, label placement); dotted/thick already carried. Honest ceiling: without a dagre-class engine, dense-graph crossing/routing won't be identical — the gate is "close and readable," not byte-identical.
6. **Sequence polish.** Styled `alt` tab + `[condition]`, snug participant boxes, self-message loop shape, note styling — to match the real sequence render.

Each Phase-2 change keeps the shared-geometry contract (DrawingML + the fallback SVG both improve together), stays std-only/zero-dep, and is accepted when `mmcompare` shows the ours panel readably matching the Mermaid panel for the corpus.

## Error handling
- Webview: Mermaid init/render failure → fall back to `buildMermaidSvg(geo)` (today's behavior), never a blank.
- Source is always present for generated diagrams (the `descr` carrier); a diagram with no recoverable source keeps the geometry/fallback path.

## Out of scope
- Embedding the Mermaid-rendered SVG into the `.docx` as an image (a later, separate slice; needs a webview→save round-trip + OOXML SVG/PNG embedding, and can't work in the pure `docxy.exe` CLI).
- Diagram types Mermaid supports that our engine doesn't model (the webview renders them all via `mermaid.js`; the Word shapes cover the current flowchart+sequence scope).
- Changing the agent/ctl/MCP surface or the extension version.
