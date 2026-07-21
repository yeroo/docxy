# Mermaid Flowchart Gaps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close four flowchart constructs real architecture docs use but the engine mishandles — `&` fan-out, `{{hexagon}}`, edges to a subgraph id, and dotted/thick edge styles — so `graph`/`flowchart` blocks render faithfully in Word `.docx` AND the docxy webview.

**Architecture:** All layout/parse changes land in the shared `docxcore/src/mermaid.rs` model so the DrawingML emitter (Word) and the webview SVG (fed by `view_json`'s geometry) stay in agreement (Word == webview). Hexagon and line-styles also touch the webview SVG renderer. The Mermaid source stays embedded in the drawing `descr`, so md↔docx round-trip is unaffected.

**Tech Stack:** Rust std-only `docxcore`; `docxwasm` geometry JSON; `offxy-vscode/media/webview.js` (`buildMermaidSvg`) + `media/mermaid-svg.test.mjs`.

## Global Constraints

- `docxcore` std-only, ZERO external dependencies.
- No version bump (`offxy-vscode/package.json` stays 0.3.0). No agent/ctl/MCP change: `test:mcp-parity` stays **56/56**.
- md↔docx round-trip stays green (source is the carrier).
- **Shared-geometry invariant:** any new visual attribute must be carried in the geometry so Word and the webview render it identically.
- **No regression for existing graphs:** a plain solid edge, a non-hexagon node, and a graph with no `&`/subgraph-edges must produce byte-identical DrawingML to today.
- Scope limits (NOT defects): edge↔subgraph is direct-anchor only (no container-aware routing); flowchart/`graph` only (sequence diagrams are Phase 2); hex colors only.
- Windows cargo env (bash): `export PATH="$HOME/.cargo/bin:$PATH"` before any cargo command; never pipe an exit-code command through `tail`.

## Current structures (context for all tasks)

`docxcore/src/mermaid.rs`:
- `struct Edge { from: usize, to: usize, label: String }` — `from`/`to` index `Diagram.nodes`.
- `struct Subgraph { title, members, parent, x,y,w,h }` — **no id field today**.
- `enum NodeShape { Rect, Round, Stadium, Diamond, Circle }`, `NodeShape::prst()` → DrawingML preset.
- `parse_statement(...)` splits a line via `split_edges` into `Seg::Node(String)`/`Seg::Arrow(label:String)`, then connects `prev` (a single `Option<usize>`) to each node.
- `split_edges` classifies arrow runs (`--`,`-->`,`-.->`,`==>`, …) but only extracts the `|label|`.
- `parse_node_token(tok) -> (id, label, shape, class_name)` — checks bracket pairs `(( ))`,`([ ])`,`{ }`,`( )`,`[ ]`.
- `emit_connector(d, e, sid)` / `edge_points(d, e)` index `d.nodes[e.from]`/`[e.to]` and call `anchors(dir, from, to)`.
- `assign_ranks`/`order_ranks` iterate `d.edges` using `e.from`/`e.to` as node indices.
- `build_geometry` → `EdgeGeom { points, label }`; `DiagramGeometry::to_json` emits edges/nodes/subgraphs; the webview `buildMermaidSvg(geo)` draws nodes by `shape`, edges as `<polyline>`.

---

### Task 1: `&` fan-out (parser)

Expand `A & B --> C & D` into the cross-product of edges, `&` inside brackets protected.

**Files:** Modify `docxcore/src/mermaid.rs`

**Interfaces:**
- Consumes: `split_edges`, `parse_node_token`, the `get` closure, `Seg`.
- Produces: `fn split_ampersand(tok: &str) -> Vec<String>`; `parse_statement` now tracks a `prev_group: Vec<usize>`.

- [ ] **Step 1: Write failing tests.**

```rust
#[test]
fn ampersand_fan_out_targets() {
    let d = parse("flowchart TD\nA -->|x| B & C & D");
    // 3 edges A→B, A→C, A→D, all labelled x.
    assert_eq!(d.edges.len(), 3);
    let labels: Vec<&str> = d.edges.iter().map(|e| e.label.as_str()).collect();
    assert!(labels.iter().all(|l| *l == "x"));
    // No phantom node whose label contains '&'.
    assert!(d.nodes.iter().all(|n| !n.label.contains('&')));
    assert_eq!(d.nodes.len(), 4); // A,B,C,D
}

#[test]
fn ampersand_fan_out_sources_and_product() {
    let d = parse("flowchart TD\nA & B --> C");
    assert_eq!(d.edges.len(), 2); // A→C, B→C
    let d2 = parse("flowchart TD\nA & B --> C & D");
    assert_eq!(d2.edges.len(), 4); // cross-product
    let d3 = parse("flowchart TD\nA --> B & C --> D");
    // A→B, A→C, then B→D, C→D
    assert_eq!(d3.edges.len(), 4);
}

#[test]
fn ampersand_in_label_is_not_a_separator() {
    let d = parse("flowchart TD\nX[a & b] --> Y");
    assert_eq!(d.nodes.len(), 2);
    assert!(d.nodes.iter().any(|n| n.label == "a & b"));
    assert_eq!(d.edges.len(), 1);
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore ampersand`

- [ ] **Step 3: Add `split_ampersand` and rework `parse_statement`.**

```rust
/// Split a node segment on top-level `&` (Mermaid group operator), keeping `&`
/// inside a `[...]`/`(...)`/`{...}` label intact. Empty pieces are dropped.
fn split_ampersand(tok: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut depth = 0i32;
    for c in tok.chars() {
        match c {
            '[' | '(' | '{' => {
                depth += 1;
                buf.push(c);
            }
            ']' | ')' | '}' => {
                depth -= 1;
                buf.push(c);
            }
            '&' if depth == 0 => out.push(std::mem::take(&mut buf)),
            _ => buf.push(c),
        }
    }
    out.push(buf);
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}
```

In `parse_statement`, replace the `prev: Option<usize>` node-handling loop body. For a `Seg::Node(tok)`: build a `group: Vec<usize>` by iterating `split_ampersand(&tok)`, each piece through `parse_node_token` + `get` (pushing `PendingStyle::Class` exactly as today per member). If the group is non-empty: when `prev_group` is non-empty, push one `Edge { from: p, to: c, label: pending_label.clone() }` for every `(p, c)` in `prev_group × group`, then `pending_label.clear()`; set `prev_group = group`. `Seg::Arrow(label)` sets `pending_label = label` as today. Remove the old `first`/`prev` scalars. The trailing sequence-message-label application (`edges[edges_before..]`) stays unchanged.

- [ ] **Step 4: Run — expect PASS + no regressions.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore` (ampersand tests + all existing mermaid tests, e.g. `parses_flowchart_nodes_and_edges`, `edge_labels_parsed`).

- [ ] **Step 5: Real-doc check.** Convert the committed sample and assert fan-out expanded:

```rust
#[test]
fn context_map_fan_out_expands() {
    // The Aliaksei context map fan-outs must become many edges, not phantom nodes.
    let src = "graph LR\n  IA[Identity]\n  IA -->|claims| DL & POOL & OE & CAT";
    let d = parse(src);
    assert_eq!(d.edges.len(), 4);
    assert!(d.nodes.iter().all(|n| !n.label.contains('&')));
}
```

- [ ] **Step 6: fmt + clippy; commit.**

```bash
git commit -am "docxcore/mermaid: expand & fan-out into cross-product edges"
```

---

### Task 2: `{{hexagon}}` node shape

**Files:** Modify `docxcore/src/mermaid.rs`, `offxy-vscode/media/webview.js`, `offxy-vscode/media/mermaid-svg.test.mjs`

**Interfaces:**
- Consumes: `NodeShape`, `parse_node_token`'s `strip_pair` chain, `buildMermaidSvg`.
- Produces: `NodeShape::Hexagon`, `prst()="hexagon"`; SVG polygon for `shape:"hexagon"`.

- [ ] **Step 1: Write failing Rust tests.**

```rust
#[test]
fn double_brace_is_hexagon() {
    let (_, _, shape, _) = parse_node_token("EB{{EventBridge bus}}");
    assert_eq!(shape, Some(NodeShape::Hexagon));
    let (_, label, _, _) = parse_node_token("EB{{EventBridge bus}}");
    assert_eq!(label.as_deref(), Some("EventBridge bus")); // no stray brace
}

#[test]
fn single_brace_still_diamond() {
    let (_, _, shape, _) = parse_node_token("D{Choice}");
    assert_eq!(shape, Some(NodeShape::Diamond));
}

#[test]
fn hexagon_prst_and_geometry() {
    let g = geometry("flowchart TD\nA{{Bus}}");
    assert_eq!(g.nodes[0].shape, "hexagon");
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore hexagon double_brace single_brace`

- [ ] **Step 3: Implement the shape.** Add `Hexagon` to `enum NodeShape`; `prst()` arm `NodeShape::Hexagon => "hexagon"`. In `parse_node_token`, add a `strip_pair(rest, "{{", "}}")` branch **before** the `strip_pair(rest, "{", "}")` (diamond) branch so `{{ }}` matches first:

```rust
let (shape, label) = if let Some(l) = strip_pair(rest, "((", "))") {
    (NodeShape::Circle, l)
} else if let Some(l) = strip_pair(rest, "([", "])") {
    (NodeShape::Stadium, l)
} else if let Some(l) = strip_pair(rest, "{{", "}}") {
    (NodeShape::Hexagon, l)
} else if let Some(l) = strip_pair(rest, "{", "}") {
    (NodeShape::Diamond, l)
} else if ...
```

(`NodeShape` derives `Eq`; ensure any exhaustive `match` on it elsewhere gets the new arm — search the file.)

- [ ] **Step 4: Run — expect PASS.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore`

- [ ] **Step 5: Webview SVG hexagon + failing JS test.** In `mermaid-svg.test.mjs`, add a node `{shape:'hexagon', ...}` to the fixture and assert the SVG contains a `<polygon>` with 6 points for it (distinct from the diamond's 4-point polygon — assert two `<polygon>` when both present, or check point count). In `buildMermaidSvg` (`webview.js`), add a `hexagon` case: a `<polygon>` with the 6 standard hexagon vertices over the node box `{x,y,w,h}` (e.g. inset the left/right tips by `w/6`): points `(x+w/6,y) (x+5w/6,y) (x+w,y+h/2) (x+5w/6,y+h) (x+w/6,y+h) (x,y+h/2)`.

- [ ] **Step 6: Run JS test.** `cd offxy-vscode && node media/mermaid-svg.test.mjs`

- [ ] **Step 7: fmt/clippy + typecheck/build; commit.**

```bash
git commit -am "docxcore/mermaid + webview: {{hexagon}} node shape"
```

---

### Task 3: Edge line-styles (dotted / thick)

**Files:** Modify `docxcore/src/mermaid.rs`, `offxy-vscode/media/webview.js`, `offxy-vscode/media/mermaid-svg.test.mjs`

**Interfaces:**
- Consumes: `split_edges` (arrow-run classification), `Edge`, `emit_connector`, `build_geometry`/`EdgeGeom`/`to_json`, `buildMermaidSvg`.
- Produces: `enum EdgeStyle { Solid, Dotted, Thick }`; `Edge.style`; `EdgeGeom.style`; geometry JSON `"style"`.

- [ ] **Step 1: Write failing tests.**

```rust
#[test]
fn edge_styles_classified() {
    assert_eq!(parse("flowchart TD\nA --> B").edges[0].style, EdgeStyle::Solid);
    assert_eq!(parse("flowchart TD\nA -.-> B").edges[0].style, EdgeStyle::Dotted);
    assert_eq!(parse("flowchart TD\nA ==> B").edges[0].style, EdgeStyle::Thick);
}

#[test]
fn dotted_edge_drawingml_and_geometry() {
    let (xml, _) = to_drawing("flowchart TD\nA -.-> B");
    assert!(xml.contains("prstDash"), "{xml}");
    let g = geometry("flowchart TD\nA -.-> B");
    assert!(g.to_json().contains("\"style\":\"dotted\""));
}

#[test]
fn solid_edge_unchanged() {
    let (xml, _) = to_drawing("flowchart TD\nA --> B");
    assert!(!xml.contains("prstDash")); // solid emits no dash
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore edge_styles dotted_edge solid_edge`

- [ ] **Step 3: Implement.**
- Add `#[derive(Debug, Clone, Copy, PartialEq, Eq)] enum EdgeStyle { Solid, Dotted, Thick }`; add `style: EdgeStyle` to `Edge`.
- `enum Seg` → `Arrow(String, EdgeStyle)`. In `split_edges`, after building the arrow `run` string, classify: `if run.contains('.') { Dotted } else if run.contains('=') { Thick } else { Solid }` and push `Seg::Arrow(label, style)`.
- `parse_statement`: carry `cur_style: EdgeStyle` alongside `pending_label` (set both on `Seg::Arrow`); include `style: cur_style` in every `Edge` pushed (fan-out product included). Default `Solid` before any arrow.
- `build_geometry`: `EdgeGeom` gains `style: EdgeStyle`; map it. `to_json` edges emit `"style":"solid"|"dotted"|"thick"` (add a helper `EdgeStyle::tag()`).
- `emit_connector`: change the `<a:ln w="12700">…` for the connector — `Dotted` adds `<a:prstDash val="dash"/>` inside `<a:ln>`; `Thick` uses `w="19050"`. `Solid` stays exactly `w="12700"` with no `prstDash` (byte-identical to today). Keep the `<a:tailEnd>` triangle.

- [ ] **Step 4: Run — expect PASS + no regressions.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore`

- [ ] **Step 5: Webview + failing JS test.** In `mermaid-svg.test.mjs`, add a dotted and a thick edge to the fixture; assert the dotted edge's `<polyline>` has `stroke-dasharray` and the thick edge a larger `stroke-width`. In `buildMermaidSvg`, read `edge.style` (from geometry JSON) and set `stroke-dasharray="8 6"` for `dotted` and a larger `stroke-width` for `thick`; `solid` unchanged.

- [ ] **Step 6: Run JS test; then rebuild wasm + gates.** `cd offxy-vscode && node media/mermaid-svg.test.mjs && export PATH="$HOME/.cargo/bin:$PATH" && npm run build:wasm && npm run test:md-roundtrip && npm run test:mcp-parity`

- [ ] **Step 7: fmt/clippy + typecheck; commit.**

```bash
git commit -am "docxcore/mermaid + webview: dotted/thick edge line-styles"
```

---

### Task 4: Edge ↔ subgraph id

Let an edge reference a subgraph id (e.g. `SharedVPC <--> EB`); anchor on the container box instead of minting a phantom node.

**Files:** Modify `docxcore/src/mermaid.rs`

**Interfaces:**
- Consumes: the `subgraph` parse branch, `parse_statement` group builder (Task 1), `assign_ranks`/`order_ranks`, `anchors`/`edge_points`/`emit_connector`, `build_geometry`.
- Produces: `Subgraph.id: String`; a subgraph-id→index map; `Edge.from_subgraph`/`to_subgraph: Option<usize>`.

- [ ] **Step 1: Write failing tests.**

```rust
#[test]
fn edge_to_subgraph_id_no_phantom() {
    let src = "flowchart TB\nsubgraph S[Shared]\n  A\nend\nB --> S";
    let d = parse(src);
    // No phantom node named S; the edge targets subgraph 0.
    assert!(d.nodes.iter().all(|n| n.label != "S" && n.label != "Shared"
        || n.subgraph.is_some())); // only member nodes, none named S
    assert_eq!(d.edges.len(), 1);
    assert_eq!(d.edges[0].to_subgraph, Some(0));
}

#[test]
fn edge_to_subgraph_excluded_from_ranking_and_anchors_box() {
    let src = "flowchart TB\nsubgraph S[Shared]\n  A\nend\nB --> S";
    let g = geometry(src);
    // Geometry still produces an edge with real points (anchored on the box),
    // and layout didn't panic on a non-node endpoint.
    assert_eq!(g.edges.len(), 1);
    assert!(g.edges[0].points.len() >= 2);
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore edge_to_subgraph`

- [ ] **Step 3: Capture subgraph ids.** Add `id: String` to `struct Subgraph`. When parsing a `subgraph` line, extract the id: the token before `[` (`subgraph SharedVPC[Shared VPC]` → id `SharedVPC`); for a bare `subgraph Title` with no `[...]`, id = the title text (Mermaid uses the title as id then). Update the `subgraphs.push(Subgraph { … })` initializer (and any other construction site, e.g. the fix-wave tests) to set `id`. Maintain `let mut subgraph_ids: HashMap<String, usize>` in `parse`, inserting on each `subgraph` open.

- [ ] **Step 4: Resolve endpoints as node-or-subgraph.** Add `from_subgraph: Option<usize>` and `to_subgraph: Option<usize>` to `Edge` (default `None`; update all `Edge { … }` constructions incl. Task 1/3 sites). In `parse_statement`'s group builder, when a member's parsed `id` matches `subgraph_ids`, DON'T `get()` a node — instead record the endpoint as `Endpoint::Subgraph(sg_idx)`. Represent a group element as an enum `enum Endpoint { Node(usize), Subgraph(usize) }` and build `group: Vec<Endpoint>`. When pushing an `Edge` for `(p, c)`: set `from`/`from_subgraph` from `p` (Node→`from=idx, from_subgraph=None`; Subgraph→`from=usize::MAX placeholder, from_subgraph=Some(idx)`) and likewise `to`. Use a named `const NO_NODE: usize = usize::MAX;` for the placeholder.

Note: subgraph-id resolution requires the subgraph to be declared before the edge (the common case). Forward references (edge before its subgraph) fall back to a node — acceptable, documented.

- [ ] **Step 5: Exclude subgraph-endpoint edges from ranking.** In `assign_ranks` and `order_ranks` (and the `crossing_count` test helper), `continue`/skip any edge where `from_subgraph.is_some() || to_subgraph.is_some()` — they are visual connectors, not rank constraints, and `from`/`to` may be the `NO_NODE` placeholder. Guard every `d.nodes[e.from]`/`d.nodes[e.to]` access.

- [ ] **Step 6: Anchor on the box.** Add a helper returning an endpoint's rect `(x,y,w,h)`: a node's rect, or a subgraph's box (post-layout `g.x/g.y/g.w/g.h`). In `edge_points`/`emit_connector`/`anchors`, when an endpoint is a subgraph use the box rect for the anchor (bottom/top/left/right-center by `Dir`, same formulae as nodes). Since `edge_points` is the single source of truth, the webview SVG follows automatically (no webview change).

- [ ] **Step 7: Run — expect PASS + no regressions.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore` (whole crate; watch for panics on the placeholder index — every node-index access on an edge must be guarded).

- [ ] **Step 8: Real-doc check + gates.** Convert `offxy-vscode/samples/mermaid/11-aliaksei-topology.md` (`export PATH="$HOME/.cargo/bin:$PATH" && cargo build --release -p docxy && target/release/docxy.exe offxy-vscode/samples/mermaid/11-aliaksei-topology.md --docx /tmp/topo.docx`) and confirm via `unzip -p /tmp/topo.docx word/document.xml` there is no phantom `SharedVPC`/`WorkloadVPC` node text separate from the container titles. Then `cd offxy-vscode && export PATH="$HOME/.cargo/bin:$PATH" && npm run build:wasm && npm run test:md-roundtrip && npm run test:grid-layout && npm run test:mermaid-svg && npm run test:mcp-parity`.

- [ ] **Step 9: fmt/clippy; commit.**

```bash
git commit -am "docxcore/mermaid: edges to a subgraph id anchor on the container box"
```

---

## Notes for the executor

- Tasks are largely independent; recommended order 1→2→3→4 (Task 4 is the largest and touches `Edge`'s shape, so doing it last avoids re-touching fan-out/style edge construction).
- After each task that changes `Edge`'s fields (1 adds nothing to `Edge`; 3 adds `style`; 4 adds `from_subgraph`/`to_subgraph`), update EVERY `Edge { … }` construction site (including tests) in the same commit so the crate compiles.
- Do NOT add container-aware routing, sequence diagrams, named CSS colors, or `linkStyle` — all out of scope (Phase 2 / future).
- The `.docx` conversions in Task 1/4 checks are throwaway (gitignored/tmp) — do not commit them.
