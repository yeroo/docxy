# Mermaid flowchart-quality rendering — design

**Goal:** Make ` ```mermaid ` flowcharts render well as **editable Word shapes**
*and* in the **docxy VS Code webview**, from a single shared layout, with **no new
dependencies**. First slice of a larger "fuller Mermaid" effort (Path B: keep
diagrams as native DrawingML shapes rather than rendering Mermaid to an image).

**Basis:** conversational request (2026-07-21). The user dislikes the current
crude conversion. Chosen direction: editable shapes (not an image), improve
flowchart quality first, and the webview preview must match Word.

## Background — what exists today

`docxcore/src/mermaid.rs` turns a ` ```mermaid ` fence into a **DrawingML shape
group** (`wpg:wgp` of `wps:wsp` boxes + `straightConnector1` arrows), riding the
`model::Inline::SmartArt { raw, text }` variant. The Mermaid **source is embedded
verbatim** in the drawing's `wp:docPr@descr` (`mermaid:` marker), so md↔docx
round-trips losslessly regardless of how shapes are drawn. Current limitations
this slice targets:

- Every node is hardcoded blue (`DAE8FC` fill / `6C8EBF` stroke); `classDef` /
  `class` / `style` directives are discarded.
- Within-rank node order is insertion order → edges cross needlessly.
- Connectors are straight diagonals that slice through boxes.
- `subgraph … end` is discarded → no visual grouping.
- In the **webview**, a SmartArt inline degrades to a plain box of node labels
  (`view_json` emits only the labels); the webview cannot draw DrawingML.

**Key invariant (frees us to change rendering freely):** the embedded source is
the round-trip carrier. Improving shape/geometry emission never affects md↔docx
fidelity. The existing markdown idempotency corpus stays green by construction.

## Architecture — one geometry, two renderers

The four quality improvements all live in the **layout stage**, so both outputs
benefit automatically:

```
mermaid source
      │  parse → rank → crossing-reduction → elbow routing
      │       → subgraph boxes → resolve colors
      ▼
   Diagram geometry  (the single source of truth)
   • nodes:     x,y,w,h, shape, fill, stroke, textColor, label
   • edges:     ordered elbow points [(x,y)…], optional label + its box
   • subgraphs: box x,y,w,h + title
   • canvas:    w,h
      │
      ├──► DrawingML emitter  → editable shapes in Word / .docx
      │        (colors, bentConnector3 elbows, subgraph rounded-rects)
      │
      └──► geometry (serialized) → docxwasm view_json → webview inline SVG
               (same boxes/colors/elbows/subgraphs → matches Word by construction)
```

No `mermaid.js`, no layout library — our own std-only Rust engine drives both
targets. The webview SVG renderer only needs to understand *our* geometry
vocabulary (5 node shapes, elbow polylines, subgraph rects, labels), not
arbitrary DrawingML.

## Components & data flow

### 1. `docxcore/src/mermaid.rs` — layout & geometry (the bulk of the work)

Extend the internal model and layout; add one serializable geometry type.

- **`Node`** gains `fill: Option<String>`, `stroke: Option<String>`,
  `text_color: Option<String>` (hex `RRGGBB`, no `#`), and `subgraph:
  Option<usize>` (index into the diagram's subgraph list). Defaults preserve
  today's blue when unset.
- **`Diagram`** gains `subgraphs: Vec<Subgraph>` where
  `Subgraph { title: String, members: Vec<usize>, parent: Option<usize>, x,y,w,h: i64 }`
  (geometry filled post-layout).
- **New `pub struct DiagramGeometry`** (serializable to JSON by hand, matching the
  crate's existing hand-rolled JSON style in `docxwasm`): canvas `w,h`; `nodes`
  (x,y,w,h, `shape` as a string tag, `fill`/`stroke`/`textColor` resolved to hex,
  `label`); `edges` (`points: Vec<(i64,i64)>`, `label`, label box x,y,w,h);
  `subgraphs` (x,y,w,h, title). Produced by a new
  `pub fn geometry(src: &str) -> DiagramGeometry` that runs the same
  parse+layout pipeline `to_drawing` uses.

**1a. Color parsing.** Stop discarding `classDef`/`class`/`style`. Parse:
- `classDef NAME fill:#f9f,stroke:#333,stroke-width:2px,color:#fff` → a
  `HashMap<String, ClassStyle>` where `ClassStyle { fill, stroke, color }` (hex,
  `#` stripped, 3-digit expanded to 6).
- membership: `class A,B NAME`, inline `A:::NAME` in a node token, and
  `style A fill:#f9f,stroke:#333,color:#fff` (direct per-node).
- Resolution order at emit: direct `style` > class > default blue. Unknown color
  keywords (named CSS colors) that aren't `#hex` are ignored this slice (hex
  only); document that.

**1b. Crossing reduction.** After `assign_ranks`, order nodes within each rank by
the **median heuristic**: a few alternating down/up sweeps setting each node's
cross-position key to the median of its neighbors' positions in the adjacent
rank, then sort each rank by that key (stable, ties keep insertion order). Bounded
iteration count (e.g. 4 sweeps). Replaces the current raw insertion order in
`layout()`.

**1c. Elbow connectors.** Replace `straightConnector1` with **`bentConnector3`**
(a 3-segment orthogonal connector). For TopDown, exit bottom-center of `from`,
enter top-center of `to`; the mid elbow is at the vertical midpoint. For
LeftRight, exit right-center → enter left-center, elbow at horizontal midpoint.
The geometry's `edges[].points` carries the 3–4 elbow vertices (also used by the
webview). DrawingML: emit `<a:prstGeom prst="bentConnector3">` with `xfrm`
bounding box + `flipH`/`flipV` as today; keep the `tailEnd` triangle. Honest
limit: elbows only, **not** obstacle-avoiding routing — a connector may still
clip a box in dense graphs.

**1d. Subgraph containers (simple version).** Parse `subgraph Title … end`
(nestable via a stack); assign enclosed nodes `subgraph = Some(idx)` (innermost
wins); record `parent`. After node layout, each subgraph's box = the bounding box
of its member nodes expanded by a padding margin, plus room at top for the title.
Emit **behind** the nodes: a `roundRect` with a light fill (e.g. `F5F5F5`), thin
gray stroke, and a top-left title text run. Nested subgraphs draw outer-first
(larger boxes first) so inner boxes layer on top. Honest limit: layout does
**not** force a subgraph's nodes to stay contiguous, so the box is tight when
nodes naturally group and loose otherwise; full clustering is a follow-up.

**1e. DrawingML emit order.** Subgraph rects (outermost→innermost) → connectors →
nodes → edge labels, so z-order reads correctly (containers behind, labels on
top). `to_drawing` keeps its `(drawing_xml, text_lines)` signature; `text_lines`
(node labels) is unchanged for the terminal/PDF fallback.

### 2. `docxwasm/src/bridge.rs` — expose geometry to the webview

`view_json` currently emits a SmartArt inline as its label text. Add, for a
SmartArt inline whose `raw` carries an embedded Mermaid source
(`mermaid::source_of(raw).is_some()`), a `"mermaid"` object on that inline
carrying `mermaid::geometry(&source)` serialized as JSON (canvas, nodes, edges,
subgraphs). Non-mermaid SmartArt (real Word `dgm:` diagrams) is untouched — keeps
the label box. Additive: existing consumers that read the label text still work.

### 3. `offxy-vscode/media/webview.js` — inline SVG renderer

When a SmartArt inline carries a `mermaid` geometry object, render an **inline
SVG** sized to the canvas (scaled to fit the content column) instead of the label
box:
- subgraph `<rect rx>` with title `<text>` (drawn first, behind);
- node shapes: `rect`/`roundRect`→`<rect rx>`, `diamond`→`<polygon>`,
  `circle`/`ellipse`→`<ellipse>`, filled/stroked from geometry colors, centered
  `<text>` label (with the geometry's `textColor`);
- edges: `<polyline>` through the elbow points with an arrowhead `<marker>`, plus
  an optional centered label `<text>`/`<rect>`.
EMU→px scale = canvas fit; a single scale factor keeps proportions identical to
Word. No external libs; plain SVG DOM built the way the webview builds other
nodes. Falls back to the label box if the geometry object is absent (older docs,
non-mermaid SmartArt).

## Error handling

- `geometry()` is total (mirrors `to_drawing`): a malformed diagram yields
  whatever nodes/edges parsed, never panics. An empty diagram → empty geometry →
  webview draws nothing (or the label box if geometry has zero nodes).
- Unknown/`#`-less colors are ignored (default applied), never an error.
- The webview treats a missing/zero-node `mermaid` object as "use label box."

## Testing

**`docxcore` (unit, in `mermaid.rs`):**
- color: `classDef`/`class`/`A:::x`/`style` parse to hex and resolve with the
  documented precedence; 3-digit hex expands; non-hex ignored → default.
- crossing reduction: a hand-built graph with a known crossing has fewer
  crossings after ordering than before (count via a small helper), and a simple
  chain is unchanged.
- elbows: `to_drawing` emits `bentConnector3` (not `straightConnector1`); the
  geometry's edge points are orthogonal (share x or y between consecutive points
  for TopDown/LeftRight).
- subgraph: `subgraph A … end` yields a subgraph whose box contains all member
  node rects; nested subgraphs nest (inner box ⊂ outer box).
- geometry totality: malformed source returns without panic.
- existing tests updated for the new connector preset / emit order.

**`docxwasm` (bridge test):** a `view_json` of a doc containing a mermaid inline
includes a `mermaid` object with ≥1 node and canvas dims; a non-mermaid SmartArt
does not.

**`offxy-vscode` (webview layout test, node .mjs like `grid.layout.test.mjs`):**
given a geometry fixture, the built SVG contains the expected element counts
(one shape per node, one polyline per edge, one rect+text per subgraph) and the
node fills match the geometry colors.

**Round-trip:** `test:md-roundtrip` and the docxcore markdown idempotency corpus
stay green unchanged (source-carried round-trip).

**Gates:** `cargo fmt`/`clippy -D warnings`/`cargo test` (docxcore + docxwasm),
wasm32 build, extension `typecheck`/`build`, `test:md-roundtrip`,
`test:grid-layout`, `test:mcp-parity` (56/56 — no tool-surface change).

## Out of scope

- Obstacle-avoiding edge routing (elbows only).
- Full subgraph clustering-aware layout (simple bounding box only).
- Non-flowchart diagram types (sequence/class/state/ER/gantt/pie/…) — later
  slices.
- Edge line-styles (dotted/thick), named CSS colors, font/size theming.
- Rendering Mermaid to an image (that is Path A, explicitly not chosen).
- Any agent/ctl/MCP surface change or version bump; the xlsx grid.
