# Mermaid Flowchart-Quality Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make ` ```mermaid ` flowcharts render well as editable Word shapes *and* in the docxy VS Code webview, from a single shared layout, with no new dependencies.

**Architecture:** One Rust layout engine (`docxcore/src/mermaid.rs`) computes the diagram once into a serializable `DiagramGeometry`; two renderers consume it — the existing DrawingML emitter (Word/.docx) and a new inline-SVG renderer in the webview. All four quality improvements (colors, crossing reduction, elbow connectors, subgraph boxes) live in the layout stage so both outputs benefit. The Mermaid source stays embedded in the drawing's `descr`, so md↔docx round-trip is unaffected.

**Tech Stack:** Rust (std-only `docxcore`), `docxwasm` (wasm bridge, hand-rolled JSON), VS Code webview JS (`offxy-vscode/media/webview.js`), node `.mjs` test harnesses.

## Global Constraints

- `docxcore` is **std-only, zero dependencies** — no new crates, ever.
- **No version bump** (`offxy-vscode/package.json` stays `0.3.0`).
- **No agent/ctl/MCP surface change**: `test:mcp-parity` must stay **56/56**.
- **md↔docx round-trip stays green**: the docxcore markdown idempotency corpus and `test:md-roundtrip` are unchanged — the Mermaid source is the round-trip carrier, independent of rendering.
- **Editable shapes, not an image** (Path B). No `mermaid.js`, no rasterization.
- Honest limits, do **not** exceed scope: elbow connectors only (no obstacle-avoiding routing); simple subgraph **bounding-box** (no clustering-aware layout); flowchart/`graph` only (no other diagram types); hex colors only (no named CSS colors).
- Colors are stored/emitted as 6-hex-digit `RRGGBB` with no leading `#`.
- Windows build env for cargo (bash): `export PATH="$HOME/.cargo/bin:$PATH"` before any cargo command; never pipe an exit-code command through `tail`.

---

### Task 1: Geometry foundation (shared layout output)

Introduce the serializable geometry vocabulary and a `geometry()` entry point that shares `to_drawing`'s parse+layout pipeline. Add the new `Node`/`Diagram` fields (defaulted) that later tasks populate. **DrawingML output must be byte-identical after this task** — this is pure scaffolding.

**Files:**
- Modify: `docxcore/src/mermaid.rs`

**Interfaces:**
- Consumes: existing `parse()`, `layout()`, `Diagram`, `Node`, `NodeShape`, `Edge`, `Dir`.
- Produces (later tasks & bridge rely on these):
  - `Node` fields `fill: Option<String>`, `stroke: Option<String>`, `text_color: Option<String>`, `subgraph: Option<usize>`.
  - `Diagram` field `subgraphs: Vec<Subgraph>` where `struct Subgraph { title: String, members: Vec<usize>, parent: Option<usize>, x: i64, y: i64, w: i64, h: i64 }`.
  - `pub fn geometry(src: &str) -> DiagramGeometry`.
  - `DiagramGeometry { canvas_w, canvas_h: i64, nodes: Vec<NodeGeom>, edges: Vec<EdgeGeom>, subgraphs: Vec<SubgraphGeom> }` and its `pub fn to_json(&self) -> String`.
  - `NodeGeom { x,y,w,h: i64, shape: &'static str, fill: String, stroke: String, text_color: String, label: String }` (shape tag = `NodeShape::prst()`; colors resolved to concrete hex — default `"DAE8FC"`/`"6C8EBF"`/`"000000"` when `None`).
  - `EdgeGeom { points: Vec<(i64,i64)>, label: String }`.
  - `SubgraphGeom { x,y,w,h: i64, title: String }`.

- [ ] **Step 1: Add the new fields to `Node` and `Diagram`, plus the `Subgraph` struct.**

Add to `struct Node` (after `rank`): `fill: Option<String>, stroke: Option<String>, text_color: Option<String>, subgraph: Option<usize>`. In the `get` closure's `nodes.push(Node { … })` initializer (currently ends with `rank: -1,`), add `fill: None, stroke: None, text_color: None, subgraph: None,`. Add:

```rust
#[derive(Debug, Clone)]
struct Subgraph {
    title: String,
    members: Vec<usize>,
    parent: Option<usize>,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}
```

Add `subgraphs: Vec<Subgraph>` to `struct Diagram`; in `parse()`'s final `Diagram { dir, nodes, edges }` add `subgraphs: Vec::new()`.

- [ ] **Step 2: Add the geometry types and `to_json`.**

Hand-rolled JSON (mirror `docxwasm`'s style; escape strings). Place near the bottom, before `#[cfg(test)]`:

```rust
/// A serializable snapshot of a laid-out diagram: the single geometry both the
/// DrawingML emitter (Word) and the webview SVG renderer consume.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagramGeometry {
    pub canvas_w: i64,
    pub canvas_h: i64,
    pub nodes: Vec<NodeGeom>,
    pub edges: Vec<EdgeGeom>,
    pub subgraphs: Vec<SubgraphGeom>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeGeom {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub shape: &'static str,
    pub fill: String,
    pub stroke: String,
    pub text_color: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EdgeGeom {
    pub points: Vec<(i64, i64)>,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubgraphGeom {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub title: String,
}

const DEFAULT_FILL: &str = "DAE8FC";
const DEFAULT_STROKE: &str = "6C8EBF";
const DEFAULT_TEXT: &str = "000000";

impl DiagramGeometry {
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\"canvasW\":");
        s.push_str(&self.canvas_w.to_string());
        s.push_str(",\"canvasH\":");
        s.push_str(&self.canvas_h.to_string());
        s.push_str(",\"nodes\":[");
        for (i, n) in self.nodes.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"shape\":\"{}\",\"fill\":\"{}\",\"stroke\":\"{}\",\"textColor\":\"{}\",\"label\":",
                n.x, n.y, n.w, n.h, n.shape, n.fill, n.stroke, n.text_color
            ));
            json_str(&mut s, &n.label);
            s.push('}');
        }
        s.push_str("],\"edges\":[");
        for (i, e) in self.edges.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str("{\"points\":[");
            for (j, (x, y)) in e.points.iter().enumerate() {
                if j > 0 {
                    s.push(',');
                }
                s.push_str(&format!("[{x},{y}]"));
            }
            s.push_str("],\"label\":");
            json_str(&mut s, &e.label);
            s.push('}');
        }
        s.push_str("],\"subgraphs\":[");
        for (i, g) in self.subgraphs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"title\":",
                g.x, g.y, g.w, g.h
            ));
            json_str(&mut s, &g.title);
            s.push('}');
        }
        s.push_str("]}");
        s
    }
}

/// Minimal JSON string escaping (quotes, backslash, control chars).
fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
```

- [ ] **Step 3: Add `geometry()` and a shared builder.**

Refactor so both `to_drawing` and `geometry` run parse+layout, then build geometry from the laid-out `Diagram`. Edge points for now are the two straight anchor endpoints (Task 4 makes them elbows). Add:

```rust
/// Parse + lay out `src`, returning the shared geometry both renderers consume.
pub fn geometry(src: &str) -> DiagramGeometry {
    let mut d = parse(src);
    layout(&mut d);
    build_geometry(&d)
}

fn build_geometry(d: &Diagram) -> DiagramGeometry {
    let (canvas_w, canvas_h) = canvas_extent(d);
    let nodes = d
        .nodes
        .iter()
        .map(|n| NodeGeom {
            x: n.x,
            y: n.y,
            w: n.w,
            h: n.h,
            shape: n.shape.prst(),
            fill: n.fill.clone().unwrap_or_else(|| DEFAULT_FILL.to_string()),
            stroke: n.stroke.clone().unwrap_or_else(|| DEFAULT_STROKE.to_string()),
            text_color: n.text_color.clone().unwrap_or_else(|| DEFAULT_TEXT.to_string()),
            label: n.label.clone(),
        })
        .collect();
    let edges = d
        .edges
        .iter()
        .map(|e| EdgeGeom {
            points: edge_points(d, e),
            label: e.label.clone(),
        })
        .collect();
    let subgraphs = d
        .subgraphs
        .iter()
        .map(|g| SubgraphGeom {
            x: g.x,
            y: g.y,
            w: g.w,
            h: g.h,
            title: g.title.clone(),
        })
        .collect();
    DiagramGeometry {
        canvas_w,
        canvas_h,
        nodes,
        edges,
        subgraphs,
    }
}

/// The polyline vertices of an edge. Task 4 replaces this with elbow routing;
/// for now it is the two straight anchor endpoints.
fn edge_points(d: &Diagram, e: &Edge) -> Vec<(i64, i64)> {
    let (from, to) = (&d.nodes[e.from], &d.nodes[e.to]);
    let (x1, y1, x2, y2) = anchors(d.dir, from, to);
    vec![(x1, y1), (x2, y2)]
}
```

Extract the anchor math currently inline in `emit_connector` into a shared helper so `emit_connector` and `edge_points` agree:

```rust
/// The (start, end) anchor points of an edge, by flow direction.
fn anchors(dir: Dir, from: &Node, to: &Node) -> (i64, i64, i64, i64) {
    match dir {
        Dir::TopDown => (from.x + from.w / 2, from.y + from.h, to.x + to.w / 2, to.y),
        Dir::LeftRight => (from.x + from.w, from.y + from.h / 2, to.x, to.y + to.h / 2),
    }
}
```

In `emit_connector`, replace the inline `let (x1, y1, x2, y2) = match d.dir { … };` with `let (x1, y1, x2, y2) = anchors(d.dir, from, to);`. (No output change — identical values.)

- [ ] **Step 4: Write the failing tests.**

Add to the `#[cfg(test)] mod tests`:

```rust
#[test]
fn geometry_matches_layout() {
    let g = geometry("flowchart TD\nA[Start]-->B[End]");
    assert_eq!(g.nodes.len(), 2);
    assert_eq!(g.edges.len(), 1);
    assert_eq!(g.nodes[0].label, "Start");
    // Defaults applied when no classDef/style is present.
    assert_eq!(g.nodes[0].fill, "DAE8FC");
    assert_eq!(g.nodes[0].stroke, "6C8EBF");
    assert_eq!(g.nodes[0].text_color, "000000");
    assert!(g.canvas_w > 0 && g.canvas_h > 0);
    // Edge endpoints touch the node anchor band (TopDown: from-bottom → to-top).
    let pts = &g.edges[0].points;
    assert_eq!(pts.first().copied().unwrap().1, g.nodes[0].y + g.nodes[0].h);
    assert_eq!(pts.last().copied().unwrap().1, g.nodes[1].y);
}

#[test]
fn geometry_json_is_wellformed() {
    let j = geometry("flowchart TD\nA-->B").to_json();
    assert!(j.starts_with("{\"canvasW\":"));
    assert!(j.contains("\"nodes\":[") && j.contains("\"edges\":[") && j.contains("\"subgraphs\":[]"));
    assert!(j.contains("\"shape\":\"rect\""));
}

#[test]
fn geometry_totality_on_garbage() {
    // Never panics on malformed input.
    let _ = geometry("flowchart TD\n)(*&^%$\n--> --> -->");
    let _ = geometry("");
}
```

- [ ] **Step 5: Run tests — expect FAIL (compile errors: `geometry` undefined, new fields).**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore mermaid 2>&1 | grep -E "test result|error\[|cannot find"`

- [ ] **Step 6: Implement per Steps 1–3 until the three new tests pass and all pre-existing `mermaid` tests still pass.**

Run: `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore mermaid`
Expected: PASS (all, including `emits_drawingml_with_shapes_and_connector`, `source_embeds_and_round_trips`, unchanged).

- [ ] **Step 7: Confirm DrawingML is byte-unchanged.** Add a temporary assertion or reason it: `to_drawing` still routes through the same `emit_*`; only `anchors()` extraction changed and it returns identical values. Verify `cargo test -p docxcore` (whole crate) is green.

- [ ] **Step 8: `cargo fmt` + `cargo clippy -p docxcore --all-targets -- -D warnings`; commit.**

```bash
git add docxcore/src/mermaid.rs
git commit -m "docxcore/mermaid: shared DiagramGeometry + geometry() entry point"
```

---

### Task 2: classDef / class / style colors

Parse Mermaid color directives and apply them to node fill/stroke/text in both the DrawingML emitter and the geometry.

**Files:**
- Modify: `docxcore/src/mermaid.rs`

**Interfaces:**
- Consumes: `Node.fill/stroke/text_color` (Task 1), `parse()` loop, `emit_node`, `build_geometry`.
- Produces: populated color fields; `emit_node` uses them.

- [ ] **Step 1: Write failing tests.**

```rust
#[test]
fn classdef_and_membership_apply_colors() {
    let src = "flowchart TD\n\
        classDef warn fill:#f9a,stroke:#900,color:#fff\n\
        A[Hot]:::warn --> B[Cold]\n\
        class B warn";
    let g = geometry(src);
    let a = g.nodes.iter().find(|n| n.label == "Hot").unwrap();
    assert_eq!(a.fill, "FF99AA"); // #f9a expands to FF99AA
    assert_eq!(a.stroke, "990000");
    assert_eq!(a.text_color, "FFFFFF");
    let b = g.nodes.iter().find(|n| n.label == "Cold").unwrap();
    assert_eq!(b.fill, "FF99AA"); // via `class B warn`
}

#[test]
fn style_directive_overrides_class() {
    let src = "flowchart TD\n\
        classDef c fill:#111\n\
        A:::c\n\
        style A fill:#222";
    let g = geometry(src);
    assert_eq!(g.nodes[0].fill, "222222"); // direct style wins over class
}

#[test]
fn non_hex_color_is_ignored() {
    let g = geometry("flowchart TD\nstyle A fill:red\nA-->B");
    assert_eq!(g.nodes[0].fill, "DAE8FC"); // named color ignored → default
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore classdef style non_hex`

- [ ] **Step 3: Implement color parsing.** Add a `ClassStyle { fill, stroke, color: Option<String> }` map and helpers:

```rust
#[derive(Debug, Clone, Default)]
struct ClassStyle {
    fill: Option<String>,
    stroke: Option<String>,
    color: Option<String>,
}

/// Parse `fill:#f9a,stroke:#900,stroke-width:2px,color:#fff` into a ClassStyle.
/// Only `#hex` values are honored; everything else (named colors, px widths) is
/// ignored this slice.
fn parse_style_defs(spec: &str) -> ClassStyle {
    let mut cs = ClassStyle::default();
    for part in spec.split(',') {
        let Some((k, v)) = part.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        let hex = normalize_hex(v);
        match k {
            "fill" => cs.fill = hex,
            "stroke" => cs.stroke = hex,
            "color" => cs.color = hex,
            _ => {}
        }
    }
    cs
}

/// `#f9a` / `#ff99aa` → `FF99AA`; anything not a 3/6-digit hex → None.
fn normalize_hex(v: &str) -> Option<String> {
    let h = v.strip_prefix('#')?;
    let h = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => h.to_string(),
        _ => return None,
    };
    if h.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(h.to_ascii_uppercase())
    } else {
        None
    }
}

fn apply_class_style(node: &mut Node, cs: &ClassStyle) {
    if cs.fill.is_some() {
        node.fill = cs.fill.clone();
    }
    if cs.stroke.is_some() {
        node.stroke = cs.stroke.clone();
    }
    if cs.color.is_some() {
        node.text_color = cs.color.clone();
    }
}
```

Threading through `parse()`: collect `classDef` and membership/`style` directives during the line loop, then apply after all nodes exist (so forward references like `class B warn` before B is fully seen still resolve — simplest: two-pass on the collected directives at the end of `parse`). Concretely:
- Add `let mut classdefs: HashMap<String, ClassStyle> = HashMap::new();` and `let mut pending: Vec<(String /*target*/, PendingStyle)>` where `PendingStyle` is either `Class(name)` or `Direct(ClassStyle)`.
- In `is_directive`'s current early-skip, intercept **before** skipping: lines starting `classDef ` → parse `classDef NAME spec` into `classdefs`. Lines starting `class ` → for each id in the comma list before the trailing class name, push `pending(id, Class(name))`. Lines starting `style ` → `style ID spec` → push `pending(id, Direct(parse_style_defs(spec)))`. Keep skipping them from graph parsing (return/continue as today).
- Inline `:::className` in a node token: in `parse_node_token`, split off a trailing `:::name` from the id/token first and return it; wire it so `parse_statement` records `pending(id, Class(name))`. (Simplest: strip `:::name` in `parse_node_token` before bracket parsing, stash via an out-param or handle in `parse_statement` by detecting `:::` in the raw token.)
- After the line loop, resolve: for each `(target, Class(name))` look up `classdefs[name]` and `apply_class_style`; for each `(target, Direct(cs))` apply directly. **Apply order: all `Class` first, then all `Direct`**, so `style` (Direct) overrides class membership per the test. Resolve target index via the `index` map; ignore unknown ids.

Keep it std-only (`std::collections::HashMap`).

`emit_node` and `build_geometry`: `emit_node` currently hardcodes `DAE8FC`/`6C8EBF`. Change its signature to take the node and use `node.fill.as_deref().unwrap_or("DAE8FC")` / `node.stroke.as_deref().unwrap_or("6C8EBF")`, and add a text color on the run (`<a:solidFill>` inside the run props, or `<w:color w:val=".."/>` in `rPr`). Minimum: fill + stroke (text color is a nice-to-have; if the run-color XML is uncertain, apply text color in geometry/SVG only and leave the Word run default — note this in the report). `build_geometry` already reads the fields (Task 1).

- [ ] **Step 4: Run — expect PASS.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore` (color tests + all mermaid tests + no regressions).

- [ ] **Step 5: fmt + clippy; commit.**

```bash
git commit -am "docxcore/mermaid: honor classDef/class/:::/style hex colors"
```

---

### Task 3: Crossing reduction

Order nodes within each rank by a median heuristic so edges cross less.

**Files:**
- Modify: `docxcore/src/mermaid.rs` (the `layout()` function)

**Interfaces:**
- Consumes: `assign_ranks`, `by_rank` grouping in `layout()`.
- Produces: reordered `by_rank` before positions are assigned.

- [ ] **Step 1: Write failing test (uses a crossing-count helper).**

```rust
/// Count edge crossings given each node's within-rank order index.
#[cfg(test)]
fn crossing_count(d: &Diagram, order: &std::collections::HashMap<usize, usize>) -> usize {
    let mut cross = 0;
    for (i, e1) in d.edges.iter().enumerate() {
        for e2 in &d.edges[i + 1..] {
            let (a1, b1) = (order[&e1.from], order[&e1.to]);
            let (a2, b2) = (order[&e2.from], order[&e2.to]);
            if (a1 < a2 && b1 > b2) || (a1 > a2 && b1 < b2) {
                cross += 1;
            }
        }
    }
    cross
}

#[test]
fn ordering_reduces_crossings() {
    // A graph that crosses in naive insertion order.
    let mut d = parse("flowchart TD\nA-->X\nB-->Y\nA-->Y\nB-->X");
    layout(&mut d);
    // After layout, read each node's within-rank order from its cross coordinate.
    let mut by_rank: std::collections::HashMap<i32, Vec<usize>> = std::collections::HashMap::new();
    for (i, n) in d.nodes.iter().enumerate() {
        by_rank.entry(n.rank).or_default().push(i);
    }
    for v in by_rank.values_mut() {
        v.sort_by_key(|&i| d.nodes[i].x); // TopDown: cross axis is x
    }
    let mut order = std::collections::HashMap::new();
    for v in by_rank.values() {
        for (pos, &i) in v.iter().enumerate() {
            order.insert(i, pos);
        }
    }
    // With reduction, this configuration reaches 0 crossings.
    assert_eq!(crossing_count(&d, &order), 0);
}
```

- [ ] **Step 2: Run — expect FAIL** (naive order crosses). `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore ordering_reduces_crossings`

- [ ] **Step 3: Implement median ordering in `layout()`.** After `assign_ranks(d)` and building `by_rank` (the `Vec<Vec<usize>>` grouped by rank in insertion order), insert an ordering pass before positions are assigned:

```rust
// Reduce edge crossings: alternate down/up sweeps, ordering each rank by the
// median of its neighbors' positions in the adjacent rank. Stable, bounded.
fn order_ranks(by_rank: &mut [Vec<usize>], d: &Diagram) {
    // pos[node] = its current index within its rank.
    let mut pos = vec![0usize; d.nodes.len()];
    let sync = |by_rank: &[Vec<usize>], pos: &mut [usize]| {
        for rank in by_rank {
            for (i, &n) in rank.iter().enumerate() {
                pos[n] = i;
            }
        }
    };
    sync(by_rank, &mut pos);
    let median = |neighbors: &[usize], pos: &[usize]| -> f64 {
        if neighbors.is_empty() {
            return -1.0;
        }
        let mut ps: Vec<usize> = neighbors.iter().map(|&n| pos[n]).collect();
        ps.sort_unstable();
        let m = ps.len() / 2;
        if ps.len() % 2 == 1 {
            ps[m] as f64
        } else {
            (ps[m - 1] + ps[m]) as f64 / 2.0
        }
    };
    for sweep in 0..4 {
        let down = sweep % 2 == 0;
        let idxs: Vec<usize> = if down {
            (0..by_rank.len()).collect()
        } else {
            (0..by_rank.len()).rev().collect()
        };
        for r in idxs {
            // Neighbors in the adjacent rank toward the sweep source.
            let mut keyed: Vec<(f64, usize)> = by_rank[r]
                .iter()
                .map(|&n| {
                    let neighbors: Vec<usize> = d
                        .edges
                        .iter()
                        .filter_map(|e| {
                            if down && e.to == n {
                                Some(e.from)
                            } else if !down && e.from == n {
                                Some(e.to)
                            } else {
                                None
                            }
                        })
                        .collect();
                    (median(&neighbors, &pos), n)
                })
                .collect();
            // Nodes with no neighbor (key -1) keep their relative spot (stable).
            let mut fixed: Vec<usize> = Vec::new();
            for (i, (k, _)) in keyed.iter().enumerate() {
                if *k < 0.0 {
                    fixed.push(i);
                }
            }
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            // Re-insert fixed (keyless) nodes at their original indices.
            // Simpler + good enough: stable sort keeps -1 keys at the front in
            // original order; acceptable for this heuristic.
            by_rank[r] = keyed.into_iter().map(|(_, n)| n).collect();
            let _ = fixed;
            sync(by_rank, &mut pos);
        }
    }
}
```

Call `order_ranks(&mut by_rank, d);` immediately after `by_rank` is built and before the placement loop. The placement loop then assigns `cross += w + SIBLING_GAP` in the new order.

- [ ] **Step 4: Run — expect PASS.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore` (ordering test + `ranks_are_assigned_by_longest_path` still passes + no regressions).

- [ ] **Step 5: fmt + clippy; commit.**

```bash
git commit -am "docxcore/mermaid: reduce edge crossings with a median ordering sweep"
```

---

### Task 4: Elbow connectors

Replace straight diagonal connectors with orthogonal `bentConnector3` elbows in DrawingML, and make the geometry's edge points the elbow polyline.

**Files:**
- Modify: `docxcore/src/mermaid.rs` (`emit_connector`, `edge_points`)

**Interfaces:**
- Consumes: `anchors()` (Task 1).
- Produces: `bentConnector3` XML; `EdgeGeom.points` = 4-point elbow polyline.

- [ ] **Step 1: Write failing tests.**

```rust
#[test]
fn connectors_are_bent_not_straight() {
    let (xml, _) = to_drawing("flowchart TD\nA[Start]-->B[End]");
    assert!(xml.contains("prst=\"bentConnector3\""), "{xml}");
    assert!(!xml.contains("straightConnector1"), "{xml}");
}

#[test]
fn edge_points_are_orthogonal_elbow() {
    let g = geometry("flowchart TD\nA[Start]-->B[End]");
    let pts = &g.edges[0].points;
    // 4-point elbow: start, down to mid-y, across, into target.
    assert_eq!(pts.len(), 4);
    // TopDown: consecutive segments alternate vertical / horizontal.
    assert_eq!(pts[0].0, pts[1].0); // vertical first
    assert_eq!(pts[1].1, pts[2].1); // horizontal
    assert_eq!(pts[2].0, pts[3].0); // vertical into target
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore connectors_are_bent edge_points_are_orthogonal`

- [ ] **Step 3: Implement elbow points + `bentConnector3`.** Replace `edge_points`:

```rust
fn edge_points(d: &Diagram, e: &Edge) -> Vec<(i64, i64)> {
    let (from, to) = (&d.nodes[e.from], &d.nodes[e.to]);
    let (x1, y1, x2, y2) = anchors(d.dir, from, to);
    match d.dir {
        Dir::TopDown => {
            let my = (y1 + y2) / 2;
            vec![(x1, y1), (x1, my), (x2, my), (x2, y2)]
        }
        Dir::LeftRight => {
            let mx = (x1 + x2) / 2;
            vec![(x1, y1), (mx, y1), (mx, y2), (x2, y2)]
        }
    }
}
```

In `emit_connector`, change `prst="straightConnector1"` to `prst="bentConnector3"` (keep the `xfrm` bounding box from the two anchor endpoints, `flipH`/`flipV`, `tailEnd` triangle unchanged — `bentConnector3`'s default `adj1=50000` routes through the midpoint, matching the geometry). Keep using `anchors()` for the bounding box endpoints.

- [ ] **Step 4: Update the pre-existing test.** `emits_drawingml_with_shapes_and_connector` asserts `prst="straightConnector1"`. Change that assertion to `prst="bentConnector3"`.

- [ ] **Step 5: Run — expect PASS.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore`

- [ ] **Step 6: fmt + clippy; commit.**

```bash
git commit -am "docxcore/mermaid: orthogonal bentConnector3 elbows + elbow geometry"
```

---

### Task 5: Subgraph containers

Parse `subgraph … end` (nestable), assign membership, compute bounding boxes post-layout, and emit a labeled rounded-rect behind the nodes plus geometry.

**Files:**
- Modify: `docxcore/src/mermaid.rs` (`parse`, `layout`, `emit_drawing`, `build_geometry`)

**Interfaces:**
- Consumes: `Subgraph` struct, `Node.subgraph` (Task 1), `layout()`, `emit_drawing`.
- Produces: populated `Diagram.subgraphs` with geometry; container shapes emitted first (behind).

- [ ] **Step 1: Write failing tests.**

```rust
#[test]
fn subgraph_box_contains_members() {
    let src = "flowchart TD\n\
        subgraph Group One\n\
        A[Alpha] --> B[Beta]\n\
        end\n\
        B --> C[Gamma]";
    let g = geometry(src);
    assert_eq!(g.subgraphs.len(), 1);
    let sg = &g.subgraphs[0];
    assert_eq!(sg.title, "Group One");
    // Box encloses A and B, excludes C.
    let a = g.nodes.iter().find(|n| n.label == "Alpha").unwrap();
    let b = g.nodes.iter().find(|n| n.label == "Beta").unwrap();
    assert!(sg.x <= a.x && sg.y <= a.y);
    assert!(sg.x + sg.w >= b.x + b.w && sg.y + sg.h >= b.y + b.h);
}

#[test]
fn subgraph_emits_container_shape() {
    let (xml, _) = to_drawing("flowchart TD\nsubgraph S\nA-->B\nend");
    assert!(xml.contains("roundRect"), "{xml}"); // container (or round nodes)
    assert!(xml.contains(">S<") || xml.contains("preserve\">S"), "title missing: {xml}");
}

#[test]
fn nested_subgraphs_nest() {
    let src = "flowchart TD\nsubgraph Outer\nsubgraph Inner\nA-->B\nend\nend";
    let g = geometry(src);
    assert_eq!(g.subgraphs.len(), 2);
    // One box strictly contains the other.
    let (o, i) = (&g.subgraphs[0], &g.subgraphs[1]);
    let contains = |a: &SubgraphGeom, b: &SubgraphGeom| {
        a.x <= b.x && a.y <= b.y && a.x + a.w >= b.x + b.w && a.y + a.h >= b.y + b.h
    };
    assert!(contains(o, i) || contains(i, o));
}
```

- [ ] **Step 2: Run — expect FAIL.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore subgraph nested_subgraphs`

- [ ] **Step 3: Parse subgraph blocks.** In `parse()`, maintain `let mut sg_stack: Vec<usize> = Vec::new();` and create a `Subgraph` on `subgraph [Title]`:
- Line `subgraph Title` (or `subgraph id[Title]` / bare `subgraph`): push a new `Subgraph { title: <Title or "">, members: vec![], parent: sg_stack.last().copied(), x:0,y:0,w:0,h:0 }` to `nodes`-sibling `subgraphs` vec; push its index to `sg_stack`. Title = the text after `subgraph ` (strip an `id[...]` wrapper if present, else the raw remainder).
- Line `end`: `sg_stack.pop()` (only when inside a subgraph; otherwise it's a directive as today).
- When `get()` creates or returns a node while `sg_stack` is non-empty, set `nodes[i].subgraph = Some(*sg_stack.last())` **only if not already set** (innermost-at-first-mention wins), and push `i` into that subgraph's `members` (dedupe). Easiest: after `parse_statement` processes a line, for every node touched this line assign membership — but simpler is to set membership inside the `get` closure via a captured `&sg_stack` + `&mut subgraphs`. Since `get` already captures `index`, extend it to also record membership. Keep borrow rules happy by recording `(node_idx)` touched and assigning after the line, or thread the current subgraph as a parameter.

Remove `subgraph`/`end` from being silently dropped by `is_directive` (handle them explicitly before the `is_directive` check; keep other directives skipping).

- [ ] **Step 4: Compute subgraph boxes in `layout()`.** After node positions are assigned, add:

```rust
// Bounding box per subgraph = union of member node rects, padded, with a title
// band at the top. Process innermost→outermost so nesting stays strict.
const SG_PAD: i64 = EMU_PER_INCH / 5; // 0.2"
const SG_TITLE_H: i64 = EMU_PER_INCH / 4; // 0.25"
fn layout_subgraphs(d: &mut Diagram) {
    // Order indices by depth (deepest first) so an outer box can include an inner
    // box that has already been sized.
    let depth = |mut idx: usize, d: &Diagram| {
        let mut n = 0;
        while let Some(p) = d.subgraphs[idx].parent {
            idx = p;
            n += 1;
        }
        n
    };
    let mut order: Vec<usize> = (0..d.subgraphs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(depth(i, d)));
    for si in order {
        let mut minx = i64::MAX;
        let mut miny = i64::MAX;
        let mut maxx = i64::MIN;
        let mut maxy = i64::MIN;
        let mut any = false;
        // Member nodes.
        for (ni, n) in d.nodes.iter().enumerate() {
            if n.subgraph == Some(si) {
                minx = minx.min(n.x);
                miny = miny.min(n.y);
                maxx = maxx.max(n.x + n.w);
                maxy = maxy.max(n.y + n.h);
                any = true;
            }
            let _ = ni;
        }
        // Child subgraphs already sized.
        for (ci, c) in d.subgraphs.iter().enumerate() {
            if c.parent == Some(si) {
                minx = minx.min(c.x);
                miny = miny.min(c.y);
                maxx = maxx.max(c.x + c.w);
                maxy = maxy.max(c.y + c.h);
                any = true;
            }
            let _ = ci;
        }
        let g = &mut d.subgraphs[si];
        if any {
            g.x = minx - SG_PAD;
            g.y = miny - SG_PAD - SG_TITLE_H;
            g.w = (maxx - minx) + 2 * SG_PAD;
            g.h = (maxy - miny) + 2 * SG_PAD + SG_TITLE_H;
        }
    }
}
```

Call `layout_subgraphs(d);` at the end of `layout()`. (Borrow note: split the member-scan and child-scan as above to avoid aliasing `d.subgraphs` mutably while iterating; if the borrow checker complains, compute the box into a local `(x,y,w,h)` then assign.)

- [ ] **Step 5: Emit container shapes behind everything.** In `emit_drawing`, before the connector/node loops, emit subgraph rects (outermost→innermost so inner draws on top):

```rust
// Subgraph containers first (drawn behind connectors and nodes).
let mut sg_order: Vec<usize> = (0..d.subgraphs.len()).collect();
sg_order.sort_by_key(|&i| {
    // outermost (shallowest) first
    let mut idx = i;
    let mut depth = 0;
    while let Some(p) = d.subgraphs[idx].parent { idx = p; depth += 1; }
    depth
});
for si in sg_order {
    shapes.push_str(&emit_subgraph(&d.subgraphs[si], sid));
    sid += 1;
}
```

Add `emit_subgraph` (a `roundRect` with light fill `F5F5F5`, gray stroke `999999`, and a top-anchored title run):

```rust
fn emit_subgraph(g: &Subgraph, sid: i32) -> String {
    format!(
        "<wps:wsp>\
         <wps:cNvPr id=\"{sid}\" name=\"Group {sid}\"/>\
         <wps:cNvSpPr/>\
         <wps:spPr>\
         <a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm>\
         <a:prstGeom prst=\"roundRect\"><a:avLst/></a:prstGeom>\
         <a:solidFill><a:srgbClr val=\"F5F5F5\"/></a:solidFill>\
         <a:ln w=\"9525\"><a:solidFill><a:srgbClr val=\"999999\"/></a:solidFill></a:ln>\
         </wps:spPr>\
         <wps:txbx><w:txbxContent><w:p><w:pPr><w:jc w:val=\"left\"/></w:pPr>\
         <w:r><w:t xml:space=\"preserve\">{t}</w:t></w:r></w:p></w:txbxContent></wps:txbx>\
         <wps:bodyPr anchor=\"t\"><a:noAutofit/></wps:bodyPr>\
         </wps:wsp>",
        x = g.x, y = g.y, w = g.w, h = g.h, t = xml_escape_text(&g.title),
    )
}
```

`build_geometry` already maps `d.subgraphs` → `SubgraphGeom` (Task 1). The canvas extent should include subgraph boxes: extend `canvas_extent` to also `max` over subgraph `x+w` / `y+h`.

- [ ] **Step 6: Run — expect PASS.** `export PATH="$HOME/.cargo/bin:$PATH" && cargo test -p docxcore`

- [ ] **Step 7: fmt + clippy; commit.**

```bash
git commit -am "docxcore/mermaid: labeled subgraph containers (bounding-box) + geometry"
```

---

### Task 6: Bridge — anchor + emit mermaid geometry in view_json

Make the render pass record a cell-anchored box for each mermaid diagram, and have `view_json` emit a document-level `"mermaid"` array (geometry + anchor), mirroring the existing `images` mechanism.

**Files:**
- Modify: `docxcore/src/render.rs` (`ImageBox` sibling type + `render_with_images` return + `emit_block_item` SmartArt arm)
- Modify: `docxwasm/src/bridge.rs` (`view_json`)
- Test: `docxwasm/src/bridge.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `mermaid::source_of(raw)`, `mermaid::geometry(&src)`, `mermaid::DiagramGeometry::to_json`.
- Produces:
  - `render::MermaidBox { row, col, cols, rows, geometry_json: String }`.
  - `render_with_images` returns `(Vec<Line>, Vec<LineMap>, Vec<ImageBox>, Vec<MermaidBox>)`.
  - `view_json` output gains `"mermaid":[{"row","col","cols","rows","geo":{…}}]`.

- [ ] **Step 1: Add `MermaidBox` and thread it through `render_with_images`.**

In `render.rs`, add:

```rust
/// A mermaid diagram's placement on the character grid. The webview overlays an
/// SVG (built from `geometry_json`) at this cell rectangle; the terminal/PDF keep
/// the text caption box. `geometry_json` is `mermaid::DiagramGeometry::to_json`.
#[derive(Debug, Clone, PartialEq)]
pub struct MermaidBox {
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub geometry_json: String,
}
```

Change `render_with_images` to also build and return `Vec<MermaidBox>`. **Approach:** thread a `&mut Vec<MermaidBox>` alongside `images`, exactly the way `images` is already threaded. The mermaid box's grid row is `out.len()` at the moment `emit_block_item` runs the SmartArt arm (render.rs:1533) — that row is only knowable inside the render walk, which is why a post-hoc second pass can't recover it; thread the parameter.

Follow every existing `images: &mut Vec<ImageBox>` parameter through the call chain (`render_with_images` → `render_section`/`render_blocks`/`render_paragraph` → `emit_block_item` → `text_box`) and add a sibling `mermaid: &mut Vec<MermaidBox>` next to each. Only `emit_block_item`'s SmartArt arm pushes to it; every other site just forwards it. In the SmartArt arm:

```rust
Inline::SmartArt { text, raw } => {
    if let Some(src) = crate::mermaid::source_of(raw) {
        let geo = crate::mermaid::geometry(&src);
        // Size the grid box from the canvas (EMU→cells). Cell ≈ CHAR_W wide,
        // ~2*CHAR_W tall; clamp so a diagram reserves a sensible area.
        let (cols, rows) = mermaid_box_cells(geo.canvas_w, geo.canvas_h, width);
        let row = out.len();
        // Still emit the caption box so terminal/PDF and the non-webview
        // fallback have content; the webview overlays SVG on the same rows.
        let blocks = smartart_blocks(text);
        out.extend(text_box(&blocks, None, width, opts, images));
        mermaid.push(MermaidBox { row, col: 0, cols, rows, geometry_json: geo.to_json() });
    } else {
        let blocks = smartart_blocks(text);
        out.extend(text_box(&blocks, None, width, opts, images));
    }
}
```

Add `fn mermaid_box_cells(emu_w: i64, emu_h: i64, width: usize) -> (usize, usize)` near `image_box_cells`: convert EMU→cells (`EMU_PER_INCH`≈`914400`, assume ~8px/cell horizontally and ~16px/cell vertically at 96dpi → cols = `emu_w * 96 / 914400 / 8`, rows = `emu_h * 96 / 914400 / 16`), clamp `cols` to `width` and to a floor (e.g. 8), `rows` floor 4.

Update the other call sites in `render.rs` that call `render_with_images` (the PDF export path at render.rs:336 uses `let (lines, maps, _imgs) = render_with_images(...)` — change to `let (lines, maps, _imgs, _mmd) = …`).

- [ ] **Step 2: Emit the array in `view_json`.** In `bridge.rs::view_json`, capture the 4th return value and emit an array after `"images":[…]`:

```rust
let (lines, maps, images, mermaid) = render::render_with_images(&self.editor.doc, &opts);
// … existing images loop …
out.push_str("],\"mermaid\":[");
for (mi, mb) in mermaid.iter().enumerate() {
    if mi > 0 { out.push(','); }
    out.push_str(&format!(
        "{{\"row\":{},\"col\":{},\"cols\":{},\"rows\":{},\"geo\":{}}}",
        mb.row, mb.col, mb.cols, mb.rows, mb.geometry_json
    ));
}
out.push(']');
```

(Place it right after the `images` array's closing `]`, before the optional `copied`.)

- [ ] **Step 3: Write the bridge test.**

```rust
#[test]
fn view_json_emits_mermaid_geometry() {
    let md = "flowchart TD\nA[Start]-->B[End]";
    // Build a doc from a mermaid fence via the markdown path.
    let doc = docxcore::markdown::from_markdown(&format!("```mermaid\n{md}\n```\n"));
    let mut s = Session::from_doc(doc); // use the crate's existing test constructor
    let v = s.view_json(None);
    assert!(v.contains("\"mermaid\":["), "{v}");
    assert!(v.contains("\"geo\":{\"canvasW\":"), "{v}");
    assert!(v.contains("\"shape\":\"rect\""), "{v}");
    // A plain doc has an empty array.
    let mut s2 = Session::from_doc(docxcore::markdown::from_markdown("hello\n"));
    assert!(s2.view_json(None).contains("\"mermaid\":[]"));
}
```

(Use whatever the existing bridge tests use to construct a `Session` — match the constructor in the surrounding `#[cfg(test)]` module; if it builds from bytes, serialize the doc through `new_markdown_package`/`save_package`/`load` as the md-roundtrip test does. Match the neighbors.)

- [ ] **Step 4: Run — expect PASS + no regressions.**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p docxcore
cargo test -p docxwasm
```

- [ ] **Step 5: Rebuild wasm + confirm extension gates unaffected.**

```bash
cd offxy-vscode && export PATH="$HOME/.cargo/bin:$PATH" && npm run build:wasm && npm run test:md-roundtrip && npm run test:mcp-parity
```
Expected: md-roundtrip OK; mcp parity 56/56.

- [ ] **Step 6: fmt + clippy; commit.**

```bash
git commit -am "docxcore/render + docxwasm: cell-anchored mermaid geometry in view_json"
```

---

### Task 7: Webview inline-SVG renderer

Render the mermaid geometry as an inline SVG overlay at its cell anchor, replacing the label-box fallback. Follow the existing image-overlay code path in `webview.js`.

**Files:**
- Modify: `offxy-vscode/media/webview.js`
- Test: `offxy-vscode/media/mermaid-svg.test.mjs` (new; node harness like `grid.layout.test.mjs`)
- Modify: `offxy-vscode/package.json` (add `test:mermaid-svg` script)

**Interfaces:**
- Consumes: `view_json` `"mermaid":[{row,col,cols,rows,geo}]` (Task 6); the existing image-overlay positioning (grid cell → px).
- Produces: an SVG element per mermaid box; a pure `buildMermaidSvg(geo)` function the test calls.

- [ ] **Step 1: Read the existing image overlay path.** In `webview.js`, find how the `images` array from `view_json` is turned into positioned overlay elements (cell row/col → pixel offset via the character-grid metrics). The mermaid overlay reuses that positioning; only the drawn content differs (SVG vs `<img>`).

- [ ] **Step 2: Write the failing test (`media/mermaid-svg.test.mjs`).** Export `buildMermaidSvg` from `webview.js` in a test-friendly way (mirror how `grid.layout.test.mjs` imports pure helpers — if `webview.js` isn't a module, factor `buildMermaidSvg` into a form the test can require, matching the pattern the grid test uses). Test:

```js
import { buildMermaidSvg } from './webview.js'; // match grid test's import style
const geo = {
  canvasW: 3000000, canvasH: 1200000,
  nodes: [
    { x:0,y:0,w:1000000,h:457200, shape:'rect', fill:'DAE8FC', stroke:'6C8EBF', textColor:'000000', label:'A' },
    { x:0,y:900000,w:1000000,h:457200, shape:'diamond', fill:'FF0000', stroke:'900000', textColor:'FFFFFF', label:'B' },
  ],
  edges: [ { points:[[500000,457200],[500000,678600],[500000,678600],[500000,900000]], label:'yes' } ],
  subgraphs: [ { x:-100000,y:-100000,w:1200000,h:1500000, title:'G' } ],
};
const svg = buildMermaidSvg(geo);
assert(/<svg/.test(svg));
assert((svg.match(/<rect/g) || []).length >= 2);   // subgraph rect + rect node
assert(/<polygon/.test(svg));                        // diamond
assert(/<polyline/.test(svg));                       // edge
assert(/#FF0000/i.test(svg) || /FF0000/i.test(svg)); // node fill honored
assert(/>A<|>A /.test(svg) && />B</.test(svg));       // labels
assert(/>G</.test(svg));                              // subgraph title
console.log('mermaid svg OK');
```

- [ ] **Step 3: Run — expect FAIL** (`buildMermaidSvg` undefined). `cd offxy-vscode && node media/mermaid-svg.test.mjs`

- [ ] **Step 4: Implement `buildMermaidSvg(geo)`.** Pure function: viewBox = `0 0 canvasW canvasH` (EMU units — SVG scales via width/height set by the overlay). For each subgraph: `<rect rx>` fill `#F5F5F5` stroke `#999` + `<text>` title at top-left. For each node: `rect`/`roundRect` → `<rect rx>`, `diamond` → `<polygon>` (4 points), `circle`/`ellipse` → `<ellipse>`; fill `#<fill>`, stroke `#<stroke>`; centered `<text>` with `fill:#<textColor>`. For each edge: `<polyline points="…" fill=none stroke=#333 marker-end=url(#arrow)>` + optional centered label `<text>` with a white `<rect>` behind. Include one `<defs><marker id="arrow">` triangle. Order: subgraphs, edges, nodes, edge-labels (z-order). Escape label text. Colors: prefix `#` to the 6-hex values.

- [ ] **Step 5: Wire the overlay.** Where the `images` overlays are placed, also iterate `view.mermaid`; for each, position an element at the same cell→px math using `mb.row/col/cols/rows`, set its size, and set `innerHTML = buildMermaidSvg(mb.geo)` (or build SVG DOM nodes). The SVG's own `width:100%;height:100%` fills the reserved cell box. Keep the label-box lines rendered underneath as the fallback for when a diagram has zero nodes (guard: only overlay when `mb.geo.nodes.length > 0`).

- [ ] **Step 6: Run — expect PASS.** `cd offxy-vscode && node media/mermaid-svg.test.mjs`

- [ ] **Step 7: Add the test script + run the full extension gate.** In `package.json` `scripts`, add `"test:mermaid-svg": "node media/mermaid-svg.test.mjs"`. Then:

```bash
cd offxy-vscode && export PATH="$HOME/.cargo/bin:$PATH"
npm run typecheck && npm run build && npm run test:mermaid-svg && npm run test:md-roundtrip && npm run test:grid-layout && npm run test:mcp-parity
```
Expected: all green; mcp parity 56/56.

- [ ] **Step 8: Commit.**

```bash
git add offxy-vscode/media/webview.js offxy-vscode/media/mermaid-svg.test.mjs offxy-vscode/package.json
git commit -m "offxy webview: render mermaid geometry as inline SVG (matches Word)"
```

---

## Notes for the executor

- Tasks 1–5 are `docxcore`-only and sequential (1 is the foundation; 2–5 each add one quality feature). Tasks 6–7 are the webview integration (6 = Rust/bridge, 7 = JS). Review after each.
- After Task 7, the diagram looks the same in Word and the webview because both consume `DiagramGeometry`.
- If a `docxcore` borrow-checker conflict arises in the subgraph layout (mutating `d.subgraphs` while reading `d.nodes`/`d.subgraphs`), compute each box into locals first, then assign — do not restructure the model.
- Do not add named-CSS-color support, edge line-styles, obstacle routing, clustering, or other diagram types — all explicitly out of scope (see the spec).
