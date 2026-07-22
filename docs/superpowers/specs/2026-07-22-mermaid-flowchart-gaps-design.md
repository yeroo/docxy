# Mermaid flowchart gaps — design (Phase 1 of "support all architecture diagrams")

**Goal:** Close the flowchart constructs that real architecture docs use but the
current engine mishandles — `&` fan-out, `{{hexagon}}` shape, edges to a
subgraph id, and dotted/thick edge line-styles — so a document's `graph`/
`flowchart` blocks render faithfully in **both** Word `.docx` and the docxy VS
Code webview.

**Basis:** conversational request (2026-07-22). Driven by the Aliaksei VDI
architecture doc (`ELAB/Aliaksei/docs/superpowers/specs/2026-07-20-vdi-domain-model-design.md`),
whose `graph LR` context-map and `graph TB` topology exercise all four. Builds
directly on the merged flowchart-quality slice (PR #31).

**Relationship to Phase 2:** sequence-diagram rendering is a **separate**
sub-project (own spec → plan → PR) after this. This spec is flowchart-only.

## Background — the four gaps (all empirically confirmed)

Current behavior in `docxcore/src/mermaid.rs`:
1. **`&` fan-out** — `A -->|x| B & C & D` treats `&` as node text, so all
   targets collapse into ONE node whose label is the raw `&`-joined string. No
   fan-out edges are created. Pervasive in the context-map.
2. **`{{hexagon}}`** — `parse_node_token` sees the first `{`, strips one `{…}`
   layer as a **diamond**, leaving a stray brace in the label.
3. **Edge → subgraph id** — `X --> SharedVPC` where `SharedVPC` is a `subgraph`
   id creates a **phantom node** named `SharedVPC`, separate from the container.
4. **Line-styles** — `-.->` (dotted) and `==>` (thick) parse as edges but render
   as identical plain solid arrows; the style is discarded.

**Invariant preserved throughout:** the layout engine computes one
`DiagramGeometry`; the DrawingML emitter and the webview SVG both consume it, so
**Word == webview**. Every change below lands in the shared model/layout/geometry
so both renderers stay in agreement. The Mermaid source stays embedded in the
drawing `descr`, so md↔docx round-trip is unaffected.

## Component design (all in `docxcore/src/mermaid.rs`)

### 1. `&` fan-out — parser only
Mermaid lets a single statement fan a group of sources to a group of targets:
`A & B --> C & D` means the cross-product {A→C, A→D, B→C, B→D}; `A -->|x| B & C`
means {A→B, A→C} both labelled `x`. Today `split_edges` yields alternating
`Seg::Node` / `Seg::Arrow`, and a `Seg::Node` is treated as a single node.

Change: a `Seg::Node` token may be an **`&`-separated group**. In
`parse_statement`, when connecting `prev` group to the next group across an
arrow, iterate the cross-product of the two groups' node indices, pushing one
`Edge` per pair with the pending label. A group is parsed by splitting the node
token on top-level `&` (not inside `[...]`/`{...}`/`(...)` brackets — a label may
contain `&`), each piece parsed by the existing `parse_node_token`. `prev`
becomes the whole target group so a chain `A --> B & C --> D` fans correctly
(both B and C connect to D).

Edge cases: a single node (no `&`) is a one-element group (unchanged behavior);
`&` inside a bracketed label (`X[a & b]`) is NOT a separator (only split `&`
outside brackets); trailing/empty group members are ignored.

### 2. `{{hexagon}}` node shape
Add `NodeShape::Hexagon`. In `parse_node_token`, check the `{{ … }}` pair
**before** the single-`{ … }` (diamond) pair (longest-match first), mirroring how
`(( ))` is checked before `( )`. `prst()` → `"hexagon"` (a valid DrawingML preset).
The webview SVG maps `hexagon` to a `<polygon>` with the six standard hexagon
points across the node box. Geometry already carries `shape` as the `prst` tag,
so the webview picks it up.

### 3. Edge ↔ subgraph id
A `subgraph` may be referenced as an edge endpoint by its id. Register each
subgraph id in the node-index namespace as a **container reference** so an edge to
it resolves to the subgraph rather than minting a phantom node. Represent this by
adding, to `Edge`, optional `from_subgraph`/`to_subgraph: Option<usize>` (index
into `d.subgraphs`) set when an endpoint id matches a subgraph id; the normal
`from`/`to` node indices are unused for that end. At `emit_connector`/`edge_points`
time, an endpoint that is a subgraph uses the **subgraph box** rect (post-layout)
as the anchor source instead of a node rect (anchor on the box's nearest edge/
center by flow direction). Layout: an edge to a subgraph does not affect node
ranking (the subgraph is a container, not a rank participant) — treat it as a
purely visual connector between the box boundary and the node.

Scope limit (documented): only a **direct** edge to a subgraph id is supported;
we do not re-route edges to avoid crossing container boundaries, and an edge
between two subgraphs anchors box-to-box by direction. Good enough for the
topology diagram's `SharedVPC <--> EB` style; complex container routing is out of
scope.

### 4. Edge line-styles
Add `enum EdgeStyle { Solid, Dotted, Thick }` and a field `style: EdgeStyle` on
`Edge`. `split_edges` already scans arrow runs; classify the run: contains `.` →
`Dotted`; built from `=` (e.g. `==>`, `==`) → `Thick`; else `Solid`. Carry the
style into `EdgeGeom` (a `"style"` string in `to_json`). Render:
- DrawingML: `Dotted` → `<a:ln>` with `<a:prstDash val="dash"/>`; `Thick` →
  larger `w` (e.g. `19050` vs `12700`). Arrowhead unchanged.
- Webview SVG: `Dotted` → `stroke-dasharray`; `Thick` → larger `stroke-width`.

Both read the same `EdgeGeom.style`, so Word and webview match.

## Error handling
- All parsing stays total (never panics): an `&` group with a missing member, an
  edge to an unknown id, or an unrecognized arrow run degrade gracefully (skip
  the edge / default to `Solid` / one-element group).
- An edge endpoint that matches neither a node nor a subgraph id falls back to
  today's behavior (mint a node) — no regression for plain graphs.

## Testing
Unit tests in `mermaid.rs`, plus geometry assertions:
- fan-out: `A --> B & C` yields 2 edges to distinct nodes B, C (no phantom
  `&`-labelled node); `A & B --> C` yields 2 edges; `A & B --> C & D` yields 4;
  `X[a & b]` keeps `a & b` as one label (no split).
- hexagon: `A{{Bus}}` → `NodeShape::Hexagon`, `prst="hexagon"`, label `Bus` (no
  stray brace); checked before diamond so `A{d}` is still a diamond.
- edge↔subgraph: `subgraph S … end` + `X --> S` yields an edge whose target is
  the subgraph box (no phantom `S` node); the connector anchors on the box.
- line-styles: `A -.-> B` → `Dotted` (geometry `style:"dotted"`, DrawingML
  `prstDash`); `A ==> B` → `Thick` (larger `w`); `A --> B` → `Solid` (unchanged
  output, byte-identical to today for a plain solid edge).
- **Real-doc regression:** convert the committed `offxy-vscode/samples/mermaid/
  10-aliaksei-context-map.md` and assert the fan-out targets are now separate
  nodes (shape count reflects the real node set, not a giant `&` blob); convert
  `11-aliaksei-topology.md` and assert the `{{EventBridge}}` node is a hexagon
  and no phantom subgraph-id node exists.
- Webview: extend `mermaid-svg.test.mjs` — a hexagon renders `<polygon>` (6 pts),
  a dotted edge renders `stroke-dasharray`, a thick edge a larger `stroke-width`.
- Gates: `docxcore`/`docxwasm` tests + fmt/clippy; wasm rebuild; extension
  `typecheck`/`build`/`test:md-roundtrip`/`test:grid-layout`/`test:mermaid-svg`/
  `test:mcp-parity` (56/56). No version bump; no agent/ctl/MCP change.

## Out of scope
- Non-flowchart diagram types (`sequenceDiagram` etc.) — Phase 2.
- Container-aware edge routing / crossing avoidance around subgraph boxes.
- Named CSS colors; edge labels on fan-out beyond the shared label; Mermaid
  themes; `linkStyle` index targeting.
- Any change to the Word DrawingML for a plain solid edge / non-hexagon node
  (existing output stays byte-identical where behavior is unchanged).
