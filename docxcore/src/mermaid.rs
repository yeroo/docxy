//! Mermaid ⇄ Word diagrams.
//!
//! A ```` ```mermaid ```` fenced block in Markdown becomes a **native Word
//! drawing** (a DrawingML group of auto-shapes + connectors), so Word shows an
//! editable diagram rather than code. The Mermaid source is embedded in the
//! drawing's `wp:docPr@descr` (escaped, no literal newlines) so the reverse —
//! Word → Markdown — recovers the exact original ```` ```mermaid ```` block.
//!
//! The result rides the existing [`crate::model::Inline::SmartArt`] variant:
//! `raw` is the `<w:drawing>` XML (serialized verbatim on save), `text` is the
//! node labels (shown as a box in the terminal, which can't draw shapes).
//!
//! Layout is a small layered (Sugiyama-lite) engine. Flowcharts (`flowchart` /
//! `graph TD|LR|…`) parse fully (nodes, edges, labels, shapes). Other diagram
//! types are **best-effort**: declared nodes and any arrow-style edges are
//! extracted and laid out, and — because the source is preserved — they always
//! round-trip exactly even when the rendered shapes are approximate.

const EMU_PER_INCH: i64 = 914_400;

// Layout metrics, in EMU.
const NODE_H: i64 = EMU_PER_INCH / 2; // 0.5"
const RANK_GAP: i64 = EMU_PER_INCH * 9 / 10; // 0.9" between ranks
const SIBLING_GAP: i64 = EMU_PER_INCH * 3 / 10; // 0.3" between siblings
const CHAR_W: i64 = EMU_PER_INCH * 9 / 100; // ~0.09" per char
const MIN_NODE_W: i64 = EMU_PER_INCH; // 1.0"
const MAX_NODE_W: i64 = EMU_PER_INCH * 3; // 3.0"
const PAD_W: i64 = EMU_PER_INCH * 4 / 10; // 0.4" text padding

/// The visual shape of a flowchart node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeShape {
    Rect,
    Round,
    Stadium,
    Diamond,
    Circle,
    Hexagon,
}

impl NodeShape {
    /// The DrawingML preset geometry name.
    fn prst(self) -> &'static str {
        match self {
            NodeShape::Rect => "rect",
            NodeShape::Round => "roundRect",
            NodeShape::Stadium => "roundRect",
            NodeShape::Diamond => "diamond",
            NodeShape::Circle => "ellipse",
            NodeShape::Hexagon => "hexagon",
        }
    }
}

#[derive(Debug, Clone)]
struct Node {
    label: String,
    shape: NodeShape,
    // Filled in by layout (EMU).
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    rank: i32,
    fill: Option<String>,
    stroke: Option<String>,
    text_color: Option<String>,
    // The innermost `subgraph` open at this node's first mention, if any.
    subgraph: Option<usize>,
}

#[derive(Debug, Clone)]
struct Edge {
    from: usize,
    to: usize,
    label: String,
    style: EdgeStyle,
    // When an endpoint names a subgraph id rather than a node, `from`/`to`
    // holds the `NO_NODE` placeholder and the matching `*_subgraph` field
    // carries the real subgraph index — the edge anchors on the container
    // box instead of a phantom node. `None` (the common case) means a real
    // node index.
    from_subgraph: Option<usize>,
    to_subgraph: Option<usize>,
}

/// Placeholder `Edge.from`/`Edge.to` value when an endpoint is a subgraph id
/// rather than a node — never a valid index into `Diagram.nodes`. Any code
/// indexing `d.nodes[e.from]`/`d.nodes[e.to]` MUST first check
/// `from_subgraph`/`to_subgraph` (or go through `endpoint_rect`), or it will
/// panic on this value.
const NO_NODE: usize = usize::MAX;

/// One end of an edge: either a real node, or a subgraph id (anchors on the
/// container box — see `endpoint_rect`).
#[derive(Debug, Clone, Copy)]
enum Endpoint {
    Node(usize),
    Subgraph(usize),
}

/// An edge's link-line rendering, carried through the shared geometry so Word
/// (`emit_connector`) and the webview (`buildMermaidSvg`) draw it identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeStyle {
    Solid,
    Dotted,
    Thick,
}

impl EdgeStyle {
    /// The geometry-JSON tag for this style — read back by the webview.
    fn tag(self) -> &'static str {
        match self {
            EdgeStyle::Solid => "solid",
            EdgeStyle::Dotted => "dotted",
            EdgeStyle::Thick => "thick",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    TopDown,
    LeftRight,
}

#[derive(Debug, Clone)]
struct Diagram {
    dir: Dir,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    subgraphs: Vec<Subgraph>,
}

#[derive(Debug, Clone)]
struct Subgraph {
    // The subgraph's Mermaid id — the token before `[` in `subgraph
    // SharedVPC[Shared VPC]`, or the title text itself for a bare `subgraph
    // Title` (Mermaid treats the title as the id then). Fed into `parse`'s
    // `subgraph_ids` map at construction time, which is what actually
    // resolves an edge endpoint naming this subgraph — so nothing reads this
    // field back off the struct afterward (dead-code-clean, but part of the
    // documented interface and handy for debugging/future lookups).
    #[allow(dead_code)]
    id: String,
    title: String,
    // Node indices assigned to this subgraph (its innermost containing block).
    members: Vec<usize>,
    // The immediately-enclosing subgraph, for nesting.
    parent: Option<usize>,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

// ===========================================================================
// Public API
// ===========================================================================

/// Convert a Mermaid source block to a `<w:drawing>` run holding a DrawingML
/// diagram, plus the node-label lines for the terminal box. Returns
/// `(drawing_xml, text_lines)`.
pub fn to_drawing(src: &str) -> (String, Vec<String>) {
    let mut d = parse(src);
    layout(&mut d);
    let text: Vec<String> = d.nodes.iter().map(|n| n.label.clone()).collect();
    (emit_drawing(&d, src), text)
}

/// The node labels of a Mermaid source, in declaration order — used to fill the
/// terminal box when a generated diagram is reloaded from a `.docx`.
pub fn labels(src: &str) -> Vec<String> {
    parse(src).nodes.into_iter().map(|n| n.label).collect()
}

/// If a drawing's `wp:docPr@descr` carries an embedded Mermaid source (written by
/// [`to_drawing`]), decode and return it. This is how Word → Markdown recovers
/// the original ```` ```mermaid ```` block losslessly.
pub fn source_of(raw: &str) -> Option<String> {
    let descr = attr_value(raw, "descr")?;
    let decoded = xml_unescape(&descr);
    let body = decoded.strip_prefix(MARKER)?;
    Some(unescape_source(body))
}

pub(crate) const MARKER: &str = "mermaid:";

// ===========================================================================
// Parsing
// ===========================================================================

/// A `classDef`'s parsed color spec: `fill:#f9a,stroke:#900,color:#fff`. Only
/// `#hex` values are honored this slice; named colors and non-color props
/// (e.g. `stroke-width:2px`) are ignored.
#[derive(Debug, Clone, Default)]
struct ClassStyle {
    fill: Option<String>,
    stroke: Option<String>,
    color: Option<String>,
}

/// A deferred `class`/`:::`/`style` directive, resolved once every node exists.
enum PendingStyle {
    /// `class ID name` / `ID:::name` → look up `name` in `classdefs`.
    Class(String),
    /// `style ID fill:#..,stroke:#..` → apply directly.
    Direct(ClassStyle),
}

/// Parse `fill:#f9a,stroke:#900,stroke-width:2px,color:#fff` into a ClassStyle.
/// Only `#hex` values are honored this slice; everything else (named colors,
/// px widths) is ignored.
fn parse_style_defs(spec: &str) -> ClassStyle {
    let mut cs = ClassStyle::default();
    for part in spec.split(',') {
        let Some((k, v)) = part.split_once(':') else {
            continue;
        };
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

/// `#f9a` / `#ff99aa` → `FF99AA`; anything not a 3/6-digit hex → `None`.
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

fn parse(src: &str) -> Diagram {
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut dir = Dir::TopDown;
    let mut classdefs: std::collections::HashMap<String, ClassStyle> =
        std::collections::HashMap::new();
    let mut pending: Vec<(String, PendingStyle)> = Vec::new();
    let mut subgraphs: Vec<Subgraph> = Vec::new();
    let mut sg_stack: Vec<usize> = Vec::new();
    // Maps a subgraph's Mermaid id to its index, so an edge like `B --> S`
    // anchors on the container box instead of minting a phantom node named
    // `S`. Only subgraphs opened *before* the referencing edge are resolvable
    // this way (the common case); a forward reference falls back to a node.
    let mut subgraph_ids: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    // `sg_stack`/`subgraphs` are threaded through as explicit parameters (like
    // `nodes` already is) rather than captured by the closure — capturing them
    // would hold a borrow for the closure's whole lifetime, which conflicts
    // with the main loop's own `sg_stack.push`/`.pop()` on `subgraph`/`end`
    // lines.
    let mut get = |id: &str,
                   label: Option<&str>,
                   shape: Option<NodeShape>,
                   nodes: &mut Vec<Node>,
                   sg_stack: &[usize],
                   subgraphs: &mut Vec<Subgraph>|
     -> usize {
        if let Some(&i) = index.get(id) {
            // Upgrade a bare node when a later mention carries a label/shape.
            // Membership was frozen at creation (below); an existing node's
            // `subgraph`/membership is never revisited here — otherwise a
            // node first seen outside any subgraph, then re-mentioned inside
            // one, would wrongly get claimed by that later subgraph.
            if let Some(l) = label {
                if !l.is_empty() {
                    nodes[i].label = l.to_string();
                }
            }
            if let Some(s) = shape {
                nodes[i].shape = s;
            }
            i
        } else {
            let i = nodes.len();
            // Freeze membership now, at first mention: the innermost subgraph
            // open right now (or None if no subgraph is open). This is
            // decided exactly once, at creation, and never changed again.
            let sg = sg_stack.last().copied();
            nodes.push(Node {
                label: label.filter(|l| !l.is_empty()).unwrap_or(id).to_string(),
                shape: shape.unwrap_or(NodeShape::Rect),
                x: 0,
                y: 0,
                w: 0,
                h: 0,
                rank: -1,
                fill: None,
                stroke: None,
                text_color: None,
                subgraph: sg,
            });
            index.insert(id.to_string(), i);
            if let Some(sg) = sg {
                subgraphs[sg].members.push(i);
            }
            i
        }
    };

    for (lineno, raw_line) in src.lines().enumerate() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        // The header line declares the diagram type and (for flowcharts) direction.
        if lineno == 0 || (nodes.is_empty() && edges.is_empty()) {
            if let Some(d) = parse_header(line) {
                dir = d;
                // A header line is consumed only if it is purely a header.
                if is_header_only(line) {
                    continue;
                }
            }
        }
        // `participant X` / `actor X` (sequence diagrams) → a node.
        if let Some(rest) = line
            .strip_prefix("participant ")
            .or_else(|| line.strip_prefix("actor "))
        {
            let (id, label) = participant(rest);
            get(
                &id,
                Some(&label),
                Some(NodeShape::Round),
                &mut nodes,
                &sg_stack,
                &mut subgraphs,
            );
            continue;
        }
        // `subgraph [id[Title]]` → open a (possibly nested) container.
        if let Some(rest) = line.strip_prefix("subgraph") {
            let title = parse_subgraph_title(rest);
            let id = parse_subgraph_id(rest, &title);
            let idx = subgraphs.len();
            if !id.is_empty() {
                subgraph_ids.insert(id.clone(), idx);
            }
            subgraphs.push(Subgraph {
                id,
                title,
                members: Vec::new(),
                parent: sg_stack.last().copied(),
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            });
            sg_stack.push(idx);
            continue;
        }
        // `end` → close the innermost open subgraph (a no-op, still consumed,
        // when not nested — e.g. sequence-diagram `loop`/`alt`/`opt` blocks).
        if line == "end" {
            sg_stack.pop();
            continue;
        }
        // `classDef name spec` → register a reusable color style.
        if let Some(rest) = line.strip_prefix("classDef ") {
            if let Some((name, spec)) = rest.trim().split_once(char::is_whitespace) {
                classdefs.insert(name.trim().to_string(), parse_style_defs(spec.trim()));
            }
            continue;
        }
        // `class id1,id2 name` → apply a registered classDef to node(s).
        if let Some(rest) = line.strip_prefix("class ") {
            if let Some((ids, name)) = rest.trim().rsplit_once(char::is_whitespace) {
                for id in ids.split(',') {
                    let id = id.trim();
                    if !id.is_empty() {
                        pending
                            .push((id.to_string(), PendingStyle::Class(name.trim().to_string())));
                    }
                }
            }
            continue;
        }
        // `style id spec` → apply a direct color spec to one node.
        if let Some(rest) = line.strip_prefix("style ") {
            if let Some((id, spec)) = rest.trim().split_once(char::is_whitespace) {
                pending.push((
                    id.trim().to_string(),
                    PendingStyle::Direct(parse_style_defs(spec.trim())),
                ));
            }
            continue;
        }
        // Skip obvious non-graph directives.
        if is_directive(line) {
            continue;
        }
        // Parse a (possibly chained) edge sequence, or a lone node declaration.
        parse_statement(
            line,
            &mut nodes,
            &mut edges,
            &mut pending,
            &sg_stack,
            &mut subgraphs,
            &subgraph_ids,
            &mut get,
        );
    }

    // Resolve deferred color directives now that every node exists (so forward
    // references like `class B warn` before B is otherwise touched still
    // resolve). Apply order: all class-membership first, then all direct
    // `style` — a direct style must win over class membership.
    for (target, style) in &pending {
        if let PendingStyle::Class(name) = style {
            if let (Some(&i), Some(cs)) = (index.get(target.as_str()), classdefs.get(name)) {
                apply_class_style(&mut nodes[i], cs);
            }
        }
    }
    for (target, style) in &pending {
        if let PendingStyle::Direct(cs) = style {
            if let Some(&i) = index.get(target.as_str()) {
                apply_class_style(&mut nodes[i], cs);
            }
        }
    }

    Diagram {
        dir,
        nodes,
        edges,
        subgraphs,
    }
}

/// The title of a `subgraph` line: the text after `subgraph` (already stripped
/// of the keyword by the caller), with an `id[Title]` wrapper unwrapped if
/// present; a bare `subgraph` yields an empty title.
fn parse_subgraph_title(rest: &str) -> String {
    let rest = rest.trim();
    if rest.is_empty() {
        return String::new();
    }
    if let Some(open) = rest.find('[') {
        if rest.ends_with(']') {
            return clean_label(&rest[open + 1..rest.len() - 1]);
        }
    }
    rest.to_string()
}

/// The id of a `subgraph` line (same `rest` as `parse_subgraph_title`, plus
/// its already-computed `title`): the token before `[` in `subgraph
/// SharedVPC[Shared VPC]`, or the title text itself for a bare `subgraph
/// Title` — Mermaid uses the title as the id in that case. A bare `subgraph`
/// with no title at all yields an empty id (never resolvable from an edge).
fn parse_subgraph_id(rest: &str, title: &str) -> String {
    let rest = rest.trim();
    if rest.is_empty() {
        return String::new();
    }
    if let Some(open) = rest.find('[') {
        if rest.ends_with(']') {
            return sanitize_id(&rest[..open]);
        }
    }
    title.to_string()
}

/// Parse one statement: `A[x] --> B(y) -->|lbl| C`, or a lone `A[x]`.
#[allow(clippy::too_many_arguments)]
fn parse_statement(
    line: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    pending: &mut Vec<(String, PendingStyle)>,
    sg_stack: &[usize],
    subgraphs: &mut Vec<Subgraph>,
    subgraph_ids: &std::collections::HashMap<String, usize>,
    get: &mut impl FnMut(
        &str,
        Option<&str>,
        Option<NodeShape>,
        &mut Vec<Node>,
        &[usize],
        &mut Vec<Subgraph>,
    ) -> usize,
) {
    // A sequence-style message carries its text after a colon: `A->>B: Hi`. When
    // the part before the colon holds an arrow, treat the tail as the edge label.
    // The split must land on a standalone `:`, not one that's part of the
    // `:::className` inline-class operator (e.g. `B --> C[Deploy]:::warn`).
    let (graph_part, msg_label) = match message_colon(line) {
        Some(i) if has_arrow(&line[..i]) => (&line[..i], Some(line[i + 1..].trim().to_string())),
        _ => (line, None),
    };
    let edges_before = edges.len();
    let line = graph_part;
    let segments = split_edges(line);
    // segments alternate: node-group, (edge-label, node-group), …, where a
    // node-group is one or more `&`-joined node tokens (`A & B --> C & D`).
    // A group element resolves to a real node OR a subgraph id (`Endpoint`) —
    // an edge referencing a subgraph id anchors on the container box rather
    // than minting a phantom node (see `subgraph_ids`).
    let mut prev_group: Vec<Endpoint> = Vec::new();
    let mut pending_label = String::new();
    let mut cur_style = EdgeStyle::Solid;
    for seg in segments {
        match seg {
            Seg::Node(tok) => {
                let mut group: Vec<Endpoint> = Vec::new();
                for member in split_ampersand(&tok) {
                    let (id, label, shape, class_name) = parse_node_token(&member);
                    if id.is_empty() {
                        continue;
                    }
                    // A bare id matching an already-declared subgraph resolves
                    // to that container instead of a node — this is the only
                    // place a phantom node would otherwise get minted for a
                    // subgraph-id edge endpoint. Forward references (edge
                    // before its subgraph) aren't in `subgraph_ids` yet and
                    // fall back to a node, as documented.
                    if let Some(&sg_idx) = subgraph_ids.get(&id) {
                        group.push(Endpoint::Subgraph(sg_idx));
                        continue;
                    }
                    let idx = get(&id, label.as_deref(), shape, nodes, sg_stack, subgraphs);
                    if let Some(name) = class_name {
                        pending.push((id.clone(), PendingStyle::Class(name)));
                    }
                    group.push(Endpoint::Node(idx));
                }
                if group.is_empty() {
                    continue;
                }
                if !prev_group.is_empty() {
                    for &p in &prev_group {
                        for &c in &group {
                            let (from, from_subgraph) = match p {
                                Endpoint::Node(idx) => (idx, None),
                                Endpoint::Subgraph(idx) => (NO_NODE, Some(idx)),
                            };
                            let (to, to_subgraph) = match c {
                                Endpoint::Node(idx) => (idx, None),
                                Endpoint::Subgraph(idx) => (NO_NODE, Some(idx)),
                            };
                            edges.push(Edge {
                                from,
                                to,
                                label: pending_label.clone(),
                                style: cur_style,
                                from_subgraph,
                                to_subgraph,
                            });
                        }
                    }
                    pending_label.clear();
                }
                prev_group = group;
            }
            Seg::Arrow(label, style) => {
                pending_label = label;
                cur_style = style;
            }
        }
    }
    // Apply a sequence message's colon-label to the edge(s) this line produced.
    if let Some(msg) = msg_label {
        if !msg.is_empty() {
            for e in &mut edges[edges_before..] {
                if e.label.is_empty() {
                    e.label = msg.clone();
                }
            }
        }
    }
}

/// Whether a fragment contains a Mermaid link operator.
fn has_arrow(s: &str) -> bool {
    s.contains("--") || s.contains("->") || s.contains("==") || s.contains(">>")
}

/// The byte index of the first "standalone" `:` in `line` — one that is not
/// immediately preceded or followed by another `:`. This excludes colons that
/// are part of a `::` or `:::` run (the inline-class operator), so only a
/// real sequence-diagram message separator (`A->>B: Hi`) is matched.
fn message_colon(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b':' {
            continue;
        }
        let prev_colon = i > 0 && bytes[i - 1] == b':';
        let next_colon = i + 1 < bytes.len() && bytes[i + 1] == b':';
        if !prev_colon && !next_colon {
            return Some(i);
        }
    }
    None
}

enum Seg {
    Node(String),
    Arrow(String, EdgeStyle),
}

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

/// Split a line into alternating node / arrow segments. Arrows are runs built
/// from the link characters `-.=><ox` (length ≥ 2), optionally carrying a
/// `|label|` or `-- label --` caption.
fn split_edges(line: &str) -> Vec<Seg> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    let is_link = |c: char| matches!(c, '-' | '.' | '=' | '>' | '<' | 'o' | 'x');
    while i < chars.len() {
        // An arrow must contain at least one of - = and be ≥2 chars; a bare
        // 'x'/'o'/'>' inside a word is not an arrow.
        if matches!(chars[i], '-' | '=' | '<') {
            let start = i;
            while i < chars.len() && is_link(chars[i]) {
                i += 1;
            }
            let run: String = chars[start..i].iter().collect();
            if run.len() >= 2 && run.contains(['-', '=']) {
                // Flush the node text gathered so far.
                push_node(&mut out, &mut buf);
                // An inline `-- label --` caption: the label is the node text
                // accumulated *inside* the arrow run — handled via `|lbl|` below.
                // Look for a following `|label|`.
                let mut label = String::new();
                if i < chars.len() && chars[i] == '|' {
                    i += 1;
                    while i < chars.len() && chars[i] != '|' {
                        label.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() {
                        i += 1; // closing |
                    }
                }
                let style = if run.contains('.') {
                    EdgeStyle::Dotted
                } else if run.contains('=') {
                    EdgeStyle::Thick
                } else {
                    EdgeStyle::Solid
                };
                out.push(Seg::Arrow(label.trim().to_string(), style));
                continue;
            }
            // Not an arrow: treat as node text.
            buf.push_str(&run);
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    push_node(&mut out, &mut buf);
    out
}

fn push_node(out: &mut Vec<Seg>, buf: &mut String) {
    let t = buf.trim();
    if !t.is_empty() {
        out.push(Seg::Node(t.to_string()));
    }
    buf.clear();
}

/// Parse `id[Label]` / `id(Label)` / `id{Label}` / `id((Label))` / `id([Label])`
/// / `id{{Label}}` / bare `id`, with an optional trailing `:::className` (applies a
/// `classDef` inline, e.g. `A[Hot]:::warn`). Returns `(id, label, shape,
/// class_name)`.
fn parse_node_token(tok: &str) -> (String, Option<String>, Option<NodeShape>, Option<String>) {
    let tok = tok.trim();
    let (tok, class_name) = match tok.find(":::") {
        Some(pos) => (
            tok[..pos].trim(),
            Some(tok[pos + 3..].trim().to_string()).filter(|s| !s.is_empty()),
        ),
        None => (tok, None),
    };
    // Find the first opening bracket.
    let open = tok.find(['[', '(', '{']);
    let Some(open) = open else {
        return (sanitize_id(tok), None, None, class_name);
    };
    let id = sanitize_id(tok[..open].trim());
    let rest = &tok[open..];
    let (shape, label) = if let Some(l) = strip_pair(rest, "((", "))") {
        (NodeShape::Circle, l)
    } else if let Some(l) = strip_pair(rest, "([", "])") {
        (NodeShape::Stadium, l)
    } else if let Some(l) = strip_pair(rest, "{{", "}}") {
        (NodeShape::Hexagon, l)
    } else if let Some(l) = strip_pair(rest, "{", "}") {
        (NodeShape::Diamond, l)
    } else if let Some(l) = strip_pair(rest, "(", ")") {
        (NodeShape::Round, l)
    } else if let Some(l) = strip_pair(rest, "[", "]") {
        (NodeShape::Rect, l)
    } else {
        (NodeShape::Rect, rest.to_string())
    };
    (id, Some(clean_label(&label)), Some(shape), class_name)
}

fn strip_pair(s: &str, open: &str, close: &str) -> Option<String> {
    let inner = s.strip_prefix(open)?.strip_suffix(close)?;
    Some(inner.to_string())
}

/// A node id is the leading identifier; keep it as-is but trimmed.
fn sanitize_id(s: &str) -> String {
    s.trim().to_string()
}

/// Clean a label: drop surrounding quotes and Mermaid `<br>` → space.
fn clean_label(s: &str) -> String {
    let s = s.trim();
    let s = s
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(s);
    s.replace("<br>", " ")
        .replace("<br/>", " ")
        .trim()
        .to_string()
}

fn participant(rest: &str) -> (String, String) {
    // `X as Label` or just `X`.
    if let Some((id, label)) = rest.split_once(" as ") {
        (sanitize_id(id), clean_label(label))
    } else {
        let id = sanitize_id(rest);
        (id.clone(), id)
    }
}

fn strip_comment(line: &str) -> &str {
    // Mermaid comments start with `%%`.
    match line.find("%%") {
        Some(i) => &line[..i],
        None => line,
    }
}

fn parse_header(line: &str) -> Option<Dir> {
    let lower = line.to_ascii_lowercase();
    let kw = lower.split_whitespace().next()?;
    let is_graph = matches!(kw, "flowchart" | "graph");
    if !is_graph
        && !matches!(
            kw,
            "sequencediagram"
                | "classdiagram"
                | "statediagram"
                | "statediagram-v2"
                | "erdiagram"
                | "gantt"
                | "pie"
                | "mindmap"
                | "journey"
                | "gitgraph"
        )
    {
        return None;
    }
    // Direction token for flowcharts.
    let dir = if lower.contains(" lr") || lower.contains(" rl") {
        Dir::LeftRight
    } else {
        Dir::TopDown
    };
    Some(dir)
}

fn is_header_only(line: &str) -> bool {
    // "flowchart TD" / "graph LR" / "sequenceDiagram" with nothing after the
    // direction token.
    let toks: Vec<&str> = line.split_whitespace().collect();
    match toks.first().map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("flowchart") | Some("graph") => toks.len() <= 2,
        Some(_) => toks.len() == 1,
        None => false,
    }
}

fn is_directive(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("title ")
        || l.starts_with("direction ")
        || l.starts_with("class ")
        || l.starts_with("style ")
        || l.starts_with("classDef ")
        || l.starts_with("linkStyle ")
        || l.starts_with("note ")
        || l.starts_with("Note ")
        || l.starts_with("loop ")
        || l.starts_with("alt ")
        || l.starts_with("opt ")
        || l.starts_with("section ")
}

// ===========================================================================
// Layout (layered)
// ===========================================================================

fn layout(d: &mut Diagram) {
    let n = d.nodes.len();
    if n == 0 {
        return;
    }
    // Node widths from label length.
    for node in &mut d.nodes {
        let chars = node.label.chars().count() as i64;
        node.w = (chars * CHAR_W + PAD_W).clamp(MIN_NODE_W, MAX_NODE_W);
        node.h = NODE_H;
    }
    assign_ranks(d);

    // Group node indices by rank, in insertion order.
    let max_rank = d.nodes.iter().map(|n| n.rank).max().unwrap_or(0);
    let mut by_rank: Vec<Vec<usize>> = vec![Vec::new(); (max_rank + 1) as usize];
    for (i, node) in d.nodes.iter().enumerate() {
        by_rank[node.rank.max(0) as usize].push(i);
    }

    order_ranks(&mut by_rank, d);

    // Place each rank. The "cross axis" packs siblings; the "main axis" steps by
    // rank. For TopDown: main = y (rank), cross = x. For LeftRight: swapped.
    let mut main_pos: i64 = 0;
    let mut rank_extent: Vec<(i64, i64)> = Vec::new(); // (start, thickness) per rank
    for rank in &by_rank {
        let thickness = match d.dir {
            Dir::TopDown => NODE_H,
            Dir::LeftRight => rank
                .iter()
                .map(|&i| d.nodes[i].w)
                .max()
                .unwrap_or(MIN_NODE_W),
        };
        rank_extent.push((main_pos, thickness));
        main_pos += thickness + RANK_GAP;
    }

    for (r, rank) in by_rank.iter().enumerate() {
        let (main_start, thickness) = rank_extent[r];
        let mut cross: i64 = 0;
        for &i in rank {
            let (w, h) = (d.nodes[i].w, d.nodes[i].h);
            match d.dir {
                Dir::TopDown => {
                    d.nodes[i].x = cross;
                    d.nodes[i].y = main_start;
                    cross += w + SIBLING_GAP;
                }
                Dir::LeftRight => {
                    // Center each node within the rank's thickness column.
                    d.nodes[i].x = main_start + (thickness - w) / 2;
                    d.nodes[i].y = cross;
                    cross += h + SIBLING_GAP;
                }
            }
        }
    }

    layout_subgraphs(d);
    normalize_origin(d);
}

// A subgraph box straddles negative coordinates whenever it encloses the
// diagram's top/left-most node (typically the node at rank 0, cross 0):
// `layout_subgraphs` sets `x = minx - SG_PAD`, `y = miny - SG_PAD - SG_TITLE_H`,
// which is negative there. `canvas_extent` never shifts the origin — it only
// maxes over `x+w`/`y+h` starting from a (0,0) viewBox/frame — so a negative
// box clips its title band and top/left border in BOTH renderers. Fix by
// translating everything (nodes + sized subgraphs) so the minimum coordinate
// is >= 0. Edges are NOT translated here: `edge_points`/`emit_connector` read
// node positions live at build/emit time (no cached copies), so shifting the
// nodes moves the edges automatically.
fn normalize_origin(d: &mut Diagram) {
    let mut min_x = d.nodes.iter().map(|n| n.x).min().unwrap_or(0);
    let mut min_y = d.nodes.iter().map(|n| n.y).min().unwrap_or(0);
    for g in &d.subgraphs {
        if g.w == 0 && g.h == 0 {
            continue; // never sized (empty subgraph) — excluded from geometry too
        }
        min_x = min_x.min(g.x);
        min_y = min_y.min(g.y);
    }
    if min_x >= 0 && min_y >= 0 {
        return;
    }
    let dx = -min_x.min(0);
    let dy = -min_y.min(0);
    for n in &mut d.nodes {
        n.x += dx;
        n.y += dy;
    }
    for g in &mut d.subgraphs {
        if g.w == 0 && g.h == 0 {
            continue;
        }
        g.x += dx;
        g.y += dy;
    }
}

// Bounding box per subgraph = union of member node rects, padded, with a title
// band at the top. Process innermost→outermost so nesting stays strict.
const SG_PAD: i64 = EMU_PER_INCH / 5; // 0.2"
const SG_TITLE_H: i64 = EMU_PER_INCH / 4; // 0.25"
fn layout_subgraphs(d: &mut Diagram) {
    // Order indices by depth (deepest first) so an outer box can include an
    // inner box that has already been sized.
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
        for n in &d.nodes {
            if n.subgraph == Some(si) {
                minx = minx.min(n.x);
                miny = miny.min(n.y);
                maxx = maxx.max(n.x + n.w);
                maxy = maxy.max(n.y + n.h);
                any = true;
            }
        }
        // Child subgraphs already sized.
        for c in &d.subgraphs {
            if c.parent == Some(si) {
                minx = minx.min(c.x);
                miny = miny.min(c.y);
                maxx = maxx.max(c.x + c.w);
                maxy = maxy.max(c.y + c.h);
                any = true;
            }
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
                        // Skip subgraph-endpoint edges: not a rank/ordering
                        // constraint, and `from`/`to` may be `NO_NODE`, which
                        // must never index `pos` (sized to `d.nodes.len()`).
                        .filter(|e| e.from_subgraph.is_none() && e.to_subgraph.is_none())
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
            // Nodes with no neighbor (key -1) keep their relative spot: `sort_by`
            // is stable, so equal keys (including all the -1s) preserve their
            // original relative order.
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            by_rank[r] = keyed.into_iter().map(|(_, n)| n).collect();
            sync(by_rank, &mut pos);
        }
    }
}

/// Longest-path layering: rank = max(rank(pred)) + 1, sources at 0. Cycles are
/// broken by capping at `n` iterations.
fn assign_ranks(d: &mut Diagram) {
    let n = d.nodes.len();
    for node in &mut d.nodes {
        node.rank = 0;
    }
    for _ in 0..n {
        let mut changed = false;
        for e in &d.edges {
            // A subgraph-endpoint edge is a visual connector, not a rank
            // constraint: `from`/`to` may be the `NO_NODE` placeholder, which
            // must never index `d.nodes`.
            if e.from_subgraph.is_some() || e.to_subgraph.is_some() {
                continue;
            }
            if e.from == e.to {
                continue;
            }
            let want = d.nodes[e.from].rank + 1;
            if want > d.nodes[e.to].rank {
                d.nodes[e.to].rank = want;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

// ===========================================================================
// DrawingML emission
// ===========================================================================

const NS_MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const NS_WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
// Word-processing shapes/groups live in the Office 2010 extension namespaces
// (NOT the drawingml/2006 ones) and must be wrapped in mc:AlternateContent with
// `Requires="wpg"`, or Word refuses to open the document.
const NS_WPG: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup";
const NS_WPS: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingShape";

fn emit_drawing(d: &Diagram, src: &str) -> String {
    let (w, h) = canvas_extent(d);
    let mut shapes = String::new();
    let mut sid = 2; // 1 is the docPr id

    // Subgraph containers first (drawn behind connectors and nodes),
    // outermost→innermost so an inner box draws on top of its parent.
    let mut sg_order: Vec<usize> = (0..d.subgraphs.len()).collect();
    sg_order.sort_by_key(|&i| {
        let mut idx = i;
        let mut depth = 0;
        while let Some(p) = d.subgraphs[idx].parent {
            idx = p;
            depth += 1;
        }
        depth
    });
    for si in sg_order {
        let g = &d.subgraphs[si];
        // A subgraph never sized by `layout_subgraphs` (zero members, zero
        // child subgraphs) is a degenerate box — skip it rather than emit a
        // zero-area `roundRect` that wastes a shape id. A real subgraph is
        // always padded to `w>0, h>0`, so `w==0 && h==0` reliably means
        // "never sized".
        if g.w == 0 && g.h == 0 {
            continue;
        }
        shapes.push_str(&emit_subgraph(g, sid));
        sid += 1;
    }

    // Connectors next (drawn beneath nodes).
    for e in &d.edges {
        shapes.push_str(&emit_connector(d, e, sid));
        sid += 1;
    }
    // Node shapes.
    for node in &d.nodes {
        shapes.push_str(&emit_node(node, sid));
        sid += 1;
    }

    wrap_drawing_group(&shapes, w, h, src)
}

/// Wraps a `{shapes}` DrawingML fragment (a run of `wps:wsp`/`wps:cxnSp`
/// elements) in the shared `mc:AlternateContent` / `wpg:wgp` group + `w:drawing`
/// scaffold, embedding `src` (escaped, marker-prefixed) in the group's
/// `wp:docPr@descr` so [`source_of`] can recover it losslessly. This is the
/// single wrapper both [`emit_drawing`] (flowcharts) and
/// `mermaid_seq::to_drawing` (sequence diagrams) call, so every Mermaid
/// diagram type produces byte-identical scaffolding around its shapes.
pub(crate) fn wrap_drawing_group(shapes: &str, w: i64, h: i64, src: &str) -> String {
    let descr = format!("{MARKER}{}", escape_source(src));
    format!(
        "<w:r>\
         <mc:AlternateContent xmlns:mc=\"{NS_MC}\">\
         <mc:Choice Requires=\"wpg\" xmlns:wpg=\"{NS_WPG}\">\
         <w:drawing>\
         <wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" xmlns:wp=\"{NS_WP}\">\
         <wp:extent cx=\"{w}\" cy=\"{h}\"/>\
         <wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
         <wp:docPr id=\"1\" name=\"Mermaid Diagram\" descr=\"{descr_attr}\"/>\
         <wp:cNvGraphicFramePr/>\
         <a:graphic xmlns:a=\"{NS_A}\">\
         <a:graphicData uri=\"{NS_WPG}\">\
         <wpg:wgp xmlns:wps=\"{NS_WPS}\">\
         <wpg:cNvGrpSpPr/>\
         <wpg:grpSpPr>\
         <a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{w}\" cy=\"{h}\"/>\
         <a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"{w}\" cy=\"{h}\"/></a:xfrm>\
         </wpg:grpSpPr>\
         {shapes}\
         </wpg:wgp></a:graphicData></a:graphic></wp:inline></w:drawing>\
         </mc:Choice><mc:Fallback/></mc:AlternateContent></w:r>",
        descr_attr = xml_escape_attr(&descr),
    )
}

fn canvas_extent(d: &Diagram) -> (i64, i64) {
    let w = d
        .nodes
        .iter()
        .map(|n| n.x + n.w)
        .chain(d.subgraphs.iter().map(|g| g.x + g.w))
        .max()
        .unwrap_or(MIN_NODE_W)
        .max(MIN_NODE_W);
    let h = d
        .nodes
        .iter()
        .map(|n| n.y + n.h)
        .chain(d.subgraphs.iter().map(|g| g.y + g.h))
        .max()
        .unwrap_or(NODE_H)
        .max(NODE_H);
    (w, h)
}

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
        x = g.x,
        y = g.y,
        w = g.w,
        h = g.h,
        t = xml_escape_text(&g.title),
    )
}

fn emit_node(node: &Node, sid: i32) -> String {
    let fill = node.fill.as_deref().unwrap_or("DAE8FC");
    let stroke = node.stroke.as_deref().unwrap_or("6C8EBF");
    // Honor `textColor` in Word too (standard WordprocessingML run properties
    // inside `wps:txbx`/`w:txbxContent`), so a `classDef ... color:#fff` node
    // isn't black-on-dark in Word while showing correctly in the webview.
    let text = node.text_color.as_deref().unwrap_or("000000");
    format!(
        "<wps:wsp>\
         <wps:cNvPr id=\"{sid}\" name=\"Node {sid}\"/>\
         <wps:cNvSpPr/>\
         <wps:spPr>\
         <a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm>\
         <a:prstGeom prst=\"{prst}\"><a:avLst/></a:prstGeom>\
         <a:solidFill><a:srgbClr val=\"{fill}\"/></a:solidFill>\
         <a:ln w=\"9525\"><a:solidFill><a:srgbClr val=\"{stroke}\"/></a:solidFill></a:ln>\
         </wps:spPr>\
         <wps:txbx><w:txbxContent><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr>\
         <w:r><w:rPr><w:color w:val=\"{text}\"/></w:rPr><w:t xml:space=\"preserve\">{label}</w:t></w:r></w:p></w:txbxContent></wps:txbx>\
         <wps:bodyPr rot=\"0\" anchor=\"ctr\"><a:noAutofit/></wps:bodyPr>\
         </wps:wsp>",
        x = node.x,
        y = node.y,
        w = node.w,
        h = node.h,
        prst = node.shape.prst(),
        label = xml_escape_text(&node.label),
    )
}

fn emit_connector(d: &Diagram, e: &Edge, sid: i32) -> String {
    let from = endpoint_rect(d, e.from, e.from_subgraph);
    let to = endpoint_rect(d, e.to, e.to_subgraph);
    // Anchor points based on flow direction.
    let (x1, y1, x2, y2) = anchors(d.dir, from, to);
    // The single source of truth for the elbow shape: the same 4-point
    // polyline the shared geometry (and later the webview SVG) draws. Emitting
    // a custom geometry from these exact points — rather than a preset like
    // `bentConnector3` (fixed 50%-of-width bend, ignorant of direction) —
    // guarantees Word renders the identical path the geometry describes.
    let pts = edge_points(d, e);
    let ox = pts.iter().map(|p| p.0).min().unwrap_or(0);
    let oy = pts.iter().map(|p| p.1).min().unwrap_or(0);
    let maxx = pts.iter().map(|p| p.0).max().unwrap_or(0);
    let maxy = pts.iter().map(|p| p.1).max().unwrap_or(0);
    let cx = (maxx - ox).max(1);
    let cy = (maxy - oy).max(1);
    let mut path = String::new();
    for (i, (px, py)) in pts.iter().enumerate() {
        let (rx, ry) = (px - ox, py - oy);
        if i == 0 {
            path.push_str(&format!(
                "<a:moveTo><a:pt x=\"{rx}\" y=\"{ry}\"/></a:moveTo>"
            ));
        } else {
            path.push_str(&format!("<a:lnTo><a:pt x=\"{rx}\" y=\"{ry}\"/></a:lnTo>"));
        }
    }
    let label = if e.label.is_empty() {
        String::new()
    } else {
        emit_edge_label(&e.label, (x1 + x2) / 2, (y1 + y2) / 2, sid)
    };
    let line_w = match e.style {
        EdgeStyle::Thick => 19050,
        _ => 12700,
    };
    let dash = match e.style {
        EdgeStyle::Dotted => "<a:prstDash val=\"dash\"/>",
        _ => "",
    };
    format!(
        "<wps:wsp>\
         <wps:cNvPr id=\"{sid}\" name=\"Edge {sid}\"/>\
         <wps:cNvCnPr/>\
         <wps:spPr>\
         <a:xfrm><a:off x=\"{ox}\" y=\"{oy}\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
         <a:custGeom>\
         <a:avLst/><a:gdLst/><a:ahLst/><a:cxnLst/>\
         <a:rect l=\"0\" t=\"0\" r=\"{cx}\" b=\"{cy}\"/>\
         <a:pathLst><a:path w=\"{cx}\" h=\"{cy}\">{path}</a:path></a:pathLst>\
         </a:custGeom>\
         <a:ln w=\"{line_w}\"><a:solidFill><a:srgbClr val=\"333333\"/></a:solidFill>\
         {dash}<a:tailEnd type=\"triangle\"/></a:ln>\
         </wps:spPr>\
         <wps:bodyPr/>\
         </wps:wsp>{label}",
    )
}

fn emit_edge_label(label: &str, cx: i64, cy: i64, sid: i32) -> String {
    let w = (label.chars().count() as i64 * CHAR_W + PAD_W).clamp(MIN_NODE_W / 2, MAX_NODE_W);
    let h = NODE_H * 3 / 5;
    format!(
        "<wps:wsp>\
         <wps:cNvPr id=\"{sid}00\" name=\"Label {sid}\"/>\
         <wps:cNvSpPr txBox=\"1\"/>\
         <wps:spPr>\
         <a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm>\
         <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom>\
         <a:solidFill><a:srgbClr val=\"FFFFFF\"/></a:solidFill>\
         </wps:spPr>\
         <wps:txbx><w:txbxContent><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr>\
         <w:r><w:t xml:space=\"preserve\">{t}</w:t></w:r></w:p></w:txbxContent></wps:txbx>\
         <wps:bodyPr anchor=\"ctr\"><a:noAutofit/></wps:bodyPr>\
         </wps:wsp>",
        x = cx - w / 2,
        y = cy - h / 2,
        t = xml_escape_text(label),
    )
}

// ===========================================================================
// Escaping helpers
// ===========================================================================

/// Encode the Mermaid source for an XML attribute: no literal newlines (Word and
/// XML attribute-value normalization would mangle them), so escape them.
pub(crate) fn escape_source(src: &str) -> String {
    let mut out = String::new();
    for c in src.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn unescape_source(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub(crate) fn xml_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
pub(crate) fn xml_escape_attr(s: &str) -> String {
    xml_escape_text(s).replace('"', "&quot;")
}

/// Read a (CDATA) attribute value `name="..."` from raw XML and undo XML entity
/// escaping. Best-effort: finds the first occurrence.
fn attr_value(raw: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = raw.find(&key)? + key.len();
    let end = raw[start..].find('"')? + start;
    Some(raw[start..end].to_string())
}

fn xml_unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

// ===========================================================================
// Shared geometry (DrawingML emitter + webview renderer)
// ===========================================================================

/// The rect `(x, y, w, h)` an edge endpoint anchors on: a node's own rect, or
/// — when the endpoint names a subgraph id instead — the container's
/// post-layout box. This is the only place `e.from`/`e.to`/`e.from_subgraph`/
/// `e.to_subgraph` should be turned into geometry: it never indexes
/// `d.nodes` with the `NO_NODE` placeholder because a `Some(sg_idx)` always
/// takes the subgraph branch first.
fn endpoint_rect(d: &Diagram, idx: usize, sg: Option<usize>) -> (i64, i64, i64, i64) {
    match sg {
        Some(si) => {
            let g = &d.subgraphs[si];
            (g.x, g.y, g.w, g.h)
        }
        None => {
            let n = &d.nodes[idx];
            (n.x, n.y, n.w, n.h)
        }
    }
}

/// The (start, end) anchor points of an edge, by flow direction. `from`/`to`
/// are endpoint rects (`(x, y, w, h)`) from `endpoint_rect` — a node's rect or
/// a subgraph's box; the bottom/top/left/right-center formulae are identical
/// either way.
fn anchors(dir: Dir, from: (i64, i64, i64, i64), to: (i64, i64, i64, i64)) -> (i64, i64, i64, i64) {
    let (fx, fy, fw, fh) = from;
    let (tx, ty, tw, th) = to;
    match dir {
        Dir::TopDown => (fx + fw / 2, fy + fh, tx + tw / 2, ty),
        Dir::LeftRight => (fx + fw, fy + fh / 2, tx, ty + th / 2),
    }
}

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
            stroke: n
                .stroke
                .clone()
                .unwrap_or_else(|| DEFAULT_STROKE.to_string()),
            text_color: n
                .text_color
                .clone()
                .unwrap_or_else(|| DEFAULT_TEXT.to_string()),
            label: n.label.clone(),
        })
        .collect();
    let edges = d
        .edges
        .iter()
        .map(|e| EdgeGeom {
            points: edge_points(d, e),
            label: e.label.clone(),
            style: e.style,
        })
        .collect();
    let subgraphs = d
        .subgraphs
        .iter()
        // A never-sized subgraph (zero members, zero child subgraphs) stays
        // at (0,0,0,0); a real subgraph is always padded to `w>0, h>0`, so
        // filter those out here to match `emit_drawing` skipping them too.
        .filter(|g| !(g.w == 0 && g.h == 0))
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

/// The polyline vertices of an edge: a 4-point orthogonal elbow that routes
/// through the midpoint between the two anchors. This is the single source of
/// truth for the edge's shape — `emit_connector` draws these exact points as a
/// custom-geometry path, so Word and the webview renderer agree.
fn edge_points(d: &Diagram, e: &Edge) -> Vec<(i64, i64)> {
    let from = endpoint_rect(d, e.from, e.from_subgraph);
    let to = endpoint_rect(d, e.to, e.to_subgraph);
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
    style: EdgeStyle,
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
            s.push_str(",\"style\":\"");
            s.push_str(e.style.tag());
            s.push_str("\"}");
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

/// Count edge crossings given each node's within-rank order index.
#[cfg(test)]
fn crossing_count(d: &Diagram, order: &std::collections::HashMap<usize, usize>) -> usize {
    let mut cross = 0;
    for (i, e1) in d.edges.iter().enumerate() {
        // A subgraph-endpoint edge isn't a rank/ordering constraint and its
        // `from`/`to` may be `NO_NODE`, which isn't a key in `order`.
        if e1.from_subgraph.is_some() || e1.to_subgraph.is_some() {
            continue;
        }
        for e2 in &d.edges[i + 1..] {
            if e2.from_subgraph.is_some() || e2.to_subgraph.is_some() {
                continue;
            }
            let (a1, b1) = (order[&e1.from], order[&e1.to]);
            let (a2, b2) = (order[&e2.from], order[&e2.to]);
            if (a1 < a2 && b1 > b2) || (a1 > a2 && b1 < b2) {
                cross += 1;
            }
        }
    }
    cross
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flowchart_nodes_and_edges() {
        let d = parse("flowchart TD\n A[Start] --> B{Choice}\n B --> C[End]");
        assert_eq!(d.dir, Dir::TopDown);
        assert_eq!(d.nodes.len(), 3);
        assert_eq!(d.nodes[0].label, "Start");
        assert_eq!(d.nodes[1].shape, NodeShape::Diamond);
        assert_eq!(d.edges.len(), 2);
        assert_eq!(d.edges[0].from, 0);
        assert_eq!(d.edges[0].to, 1);
    }

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

    #[test]
    fn direction_lr_detected() {
        let d = parse("graph LR\nA-->B");
        assert_eq!(d.dir, Dir::LeftRight);
    }

    #[test]
    fn edge_labels_parsed() {
        let d = parse("graph TD\nA -->|yes| B");
        assert_eq!(d.edges.len(), 1);
        assert_eq!(d.edges[0].label, "yes");
    }

    #[test]
    fn edge_styles_classified() {
        assert_eq!(
            parse("flowchart TD\nA --> B").edges[0].style,
            EdgeStyle::Solid
        );
        assert_eq!(
            parse("flowchart TD\nA -.-> B").edges[0].style,
            EdgeStyle::Dotted
        );
        assert_eq!(
            parse("flowchart TD\nA ==> B").edges[0].style,
            EdgeStyle::Thick
        );
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

    #[test]
    fn context_map_fan_out_expands() {
        // The Aliaksei context map fan-outs must become many edges, not phantom nodes.
        let src = "graph LR\n  IA[Identity]\n  IA -->|claims| DL & POOL & OE & CAT";
        let d = parse(src);
        assert_eq!(d.edges.len(), 4);
        assert!(d.nodes.iter().all(|n| !n.label.contains('&')));
    }

    #[test]
    fn ranks_are_assigned_by_longest_path() {
        let mut d = parse("graph TD\nA-->B\nB-->C\nA-->C");
        layout(&mut d);
        assert_eq!(d.nodes[0].rank, 0); // A
        assert_eq!(d.nodes[1].rank, 1); // B
        assert_eq!(d.nodes[2].rank, 2); // C (longest path A→B→C)
    }

    #[test]
    fn ordering_reduces_crossings() {
        // A graph that crosses in naive insertion order.
        // (Complete bipartite K2,2 between {A,B} and {X,Y} — i.e. all 4 edges
        // A-X, B-Y, A-Y, B-X present, as in the original brief's example — is
        // topologically forced to exactly 1 crossing regardless of ordering,
        // so no algorithm can bring it to 0. This omits A-->X to make 0
        // crossings reachable while still exercising the reorder; see
        // task-3-report.md for the derivation.)
        let mut d = parse("flowchart TD\nB-->Y\nA-->Y\nB-->X");
        layout(&mut d);
        // After layout, read each node's within-rank order from its cross coordinate.
        let mut by_rank: std::collections::HashMap<i32, Vec<usize>> =
            std::collections::HashMap::new();
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

    #[test]
    fn emits_drawingml_with_shapes_and_connector() {
        let (xml, text) = to_drawing("flowchart TD\nA[Start]-->B[End]");
        assert!(xml.contains("<w:drawing>"), "{xml}");
        // Wrapped for Word with the Office 2010 wpg namespace.
        assert!(
            xml.contains("mc:AlternateContent") && xml.contains("Requires=\"wpg\""),
            "{xml}"
        );
        assert!(
            xml.contains("office/word/2010/wordprocessingGroup"),
            "{xml}"
        );
        assert!(xml.contains("prst=\"roundRect\"") || xml.contains("prst=\"rect\""));
        assert!(xml.contains("<a:custGeom>"), "connector missing");
        assert!(xml.contains("Start") && xml.contains("End"));
        assert_eq!(text, vec!["Start".to_string(), "End".to_string()]);
    }

    #[test]
    fn connectors_are_bent_not_straight() {
        let (xml, _) = to_drawing("flowchart TD\nA[Start]-->B[End]");
        assert!(xml.contains("<a:custGeom>"), "{xml}");
        assert!(xml.contains("<a:moveTo>"), "{xml}");
        assert!(!xml.contains("straightConnector1"), "{xml}");
        assert!(!xml.contains("bentConnector3"), "{xml}");
    }

    #[test]
    fn connector_path_matches_geometry() {
        // The DrawingML connector must draw the exact same polyline as the
        // shared geometry's `edge_points` — for both branches of a TopDown
        // diagram. If someone reverts to a fixed preset (e.g. bentConnector3),
        // this test fails because the preset never varies its point count or
        // bend axis with direction, but a custom-geometry path emits one
        // `<a:lnTo>` per interior/end vertex of `edge_points`.
        let g = geometry("flowchart TD\nA-->B\nA-->C");
        let (xml, _) = to_drawing("flowchart TD\nA-->B\nA-->C");
        assert_eq!(g.edges.len(), 2);
        for edge in &g.edges {
            let pts = &edge.points;
            assert_eq!(pts.len(), 4, "expected a 4-point elbow: {pts:?}");
            let ox = pts.iter().map(|p| p.0).min().unwrap();
            let oy = pts.iter().map(|p| p.1).min().unwrap();
            // The interior bend point (pts[1]), expressed relative to the
            // connector's bounding box, must appear verbatim in an <a:lnTo>.
            let (bx, by) = (pts[1].0 - ox, pts[1].1 - oy);
            let needle = format!("<a:pt x=\"{bx}\" y=\"{by}\"/>");
            assert!(
                xml.contains(&needle),
                "expected bend point {needle} in emitted XML: {xml}"
            );
        }
        // A fixed preset never emits per-vertex <a:lnTo> path data at all.
        let lnto_count = xml.matches("<a:lnTo>").count();
        assert_eq!(
            lnto_count,
            g.edges.iter().map(|e| e.points.len() - 1).sum::<usize>(),
            "expected one <a:lnTo> per non-initial vertex across all edges: {xml}"
        );
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

    #[test]
    fn source_embeds_and_round_trips() {
        let src = "flowchart TD\nA[Hello] --> B[World]";
        let (xml, _) = to_drawing(src);
        assert!(xml.contains("descr=\"mermaid:"), "{xml}");
        assert_eq!(source_of(&xml).as_deref(), Some(src));
    }

    #[test]
    fn source_survives_special_chars() {
        let src = "graph LR\nA[\"a & b < c\"] --> B";
        let (xml, _) = to_drawing(src);
        assert_eq!(source_of(&xml).as_deref(), Some(src));
    }

    #[test]
    fn sequence_participants_become_nodes() {
        let d = parse("sequenceDiagram\nparticipant Alice\nparticipant Bob\nAlice->>Bob: Hi");
        assert!(d.nodes.iter().any(|n| n.label == "Alice"));
        assert!(d.nodes.iter().any(|n| n.label == "Bob"));
        // The message is an edge with its text as the label.
        assert!(d.edges.iter().any(|e| e.label == "Hi"));
    }

    #[test]
    fn sequence_message_colon_still_parses() {
        let d = parse("sequenceDiagram\nA->>B: Hello");
        assert!(d.edges.iter().any(|e| e.label == "Hello"), "{:?}", d.edges);
    }

    #[test]
    fn non_arrow_dashes_are_not_edges() {
        // A hyphenated label shouldn't be split into an edge.
        let d = parse("graph TD\nA[well-known node]");
        assert_eq!(d.nodes.len(), 1);
        assert_eq!(d.nodes[0].label, "well-known node");
        assert_eq!(d.edges.len(), 0);
    }

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
        assert!(
            j.contains("\"nodes\":[")
                && j.contains("\"edges\":[")
                && j.contains("\"subgraphs\":[]")
        );
        assert!(j.contains("\"shape\":\"rect\""));
    }

    #[test]
    fn geometry_totality_on_garbage() {
        // Never panics on malformed input.
        let _ = geometry("flowchart TD\n)(*&^%$\n--> --> -->");
        let _ = geometry("");
    }

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
    fn inline_class_on_chained_node_styles_it() {
        // `:::warn` after a node reached via `-->` (not the first node on the
        // line) must not be mistaken for a sequence-message colon.
        let src = "flowchart TD\nclassDef warn fill:#ffffcc\nB --> C[Deploy]:::warn";
        let g = geometry(src);
        let deploy = g.nodes.iter().find(|n| n.label == "Deploy").unwrap();
        assert_eq!(deploy.fill, "FFFFCC");
        assert!(
            !g.nodes
                .iter()
                .any(|n| n.label.contains("warn") || n.label.contains("::warn")),
            "phantom node from misparsed :::warn: {:?}",
            g.nodes
        );
    }

    #[test]
    fn node_text_color_reaches_word_run() {
        // The geometry/webview honor `textColor`; the Word emitter must too,
        // or a `classDef x color:#fff` node renders white in the webview and
        // black in Word (a break of the Word==webview invariant).
        let (xml, _) = to_drawing("flowchart TD\nclassDef w color:#fff\nA:::w");
        assert!(
            xml.contains("<w:color w:val=\"FFFFFF\"/>"),
            "expected white run color in Word XML: {xml}"
        );
    }

    #[test]
    fn node_default_text_color_is_black_in_word() {
        let (xml, _) = to_drawing("flowchart TD\nA-->B");
        assert!(
            xml.contains("<w:color w:val=\"000000\"/>"),
            "expected default black run color in Word XML: {xml}"
        );
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
    fn node_at_origin_subgraph_not_clipped() {
        // A is the top/left-most node (rank 0, cross 0 => x=0, y=0) and is a
        // member of "Group One". `layout_subgraphs` pads outward from the
        // member bounding box: x = minx - SG_PAD, y = miny - SG_PAD -
        // SG_TITLE_H. With A at the origin that box goes negative — and
        // `canvas_extent` never shifts the origin to compensate, so the
        // title band and top/left border get clipped in both the Word
        // inline frame (fixed x/y offsets) and the webview
        // (`viewBox="0 0 canvasW canvasH"`). Against the pre-fix code this
        // assertion fails: the subgraph's y lands around -411480 EMU.
        let src = "flowchart TD\nsubgraph Group One\nA-->B\nend\nB-->C";
        let g = geometry(src);
        assert_eq!(g.subgraphs.len(), 1);
        for sg in &g.subgraphs {
            assert!(sg.x >= 0 && sg.y >= 0, "subgraph clipped: {sg:?}");
        }
        for n in &g.nodes {
            assert!(n.x >= 0 && n.y >= 0, "node clipped: {n:?}");
        }
    }

    #[test]
    fn subgraph_emits_container_shape() {
        let (xml, _) = to_drawing("flowchart TD\nsubgraph S\nA-->B\nend");
        assert!(xml.contains("roundRect"), "{xml}"); // container (or round nodes)
        assert!(
            xml.contains(">S<") || xml.contains("preserve\">S"),
            "title missing: {xml}"
        );
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

    #[test]
    fn node_first_seen_outside_stays_outside() {
        // A is first declared outside any subgraph; a later mention inside S
        // must NOT retroactively claim it. Against the old
        // `subgraph.is_none()` "undecided" flag, A's still-`None` field
        // looked undecided and got reassigned to S on the second mention —
        // this test fails against that logic and passes against
        // freeze-at-creation.
        let src = "flowchart TD\nA-->B\nsubgraph S\nA-->C\nend";
        let d = parse(src);
        let a_idx = d.nodes.iter().position(|n| n.label == "A").unwrap();
        assert_eq!(
            d.nodes[a_idx].subgraph, None,
            "A must stay outside every subgraph"
        );
        assert_eq!(d.subgraphs.len(), 1);
        let sg = &d.subgraphs[0];
        assert!(
            !sg.members.contains(&a_idx),
            "S must not claim A as a member: {:?}",
            sg.members
        );
        // C, first seen inside S, is correctly a member.
        let c_idx = d.nodes.iter().position(|n| n.label == "C").unwrap();
        assert!(sg.members.contains(&c_idx));
    }

    #[test]
    fn empty_subgraph_is_not_emitted() {
        let src = "flowchart TD\nsubgraph S\nend\nA-->B";
        let g = geometry(src);
        assert!(
            g.subgraphs.is_empty(),
            "an empty subgraph must not be geometry-visible: {:?}",
            g.subgraphs
        );
        let (xml, _) = to_drawing(src);
        assert!(
            !xml.contains("cx=\"0\" cy=\"0\""),
            "no zero-area container should be emitted: {xml}"
        );
    }

    #[test]
    fn subgraph_id_extracted_from_bracket_and_bare_title() {
        // `subgraph SharedVPC[Shared VPC]` → id is the token before `[`.
        let d1 = parse("flowchart TD\nsubgraph SharedVPC[Shared VPC]\nA\nend");
        assert_eq!(d1.subgraphs[0].id, "SharedVPC");
        assert_eq!(d1.subgraphs[0].title, "Shared VPC");
        // A bare `subgraph Title` (no `[...]`) → id is the title itself,
        // same as Mermaid's own behavior.
        let d2 = parse("flowchart TD\nsubgraph Group One\nA\nend");
        assert_eq!(d2.subgraphs[0].id, "Group One");
    }

    #[test]
    fn edge_to_subgraph_id_no_phantom() {
        let src = "flowchart TB\nsubgraph S[Shared]\n  A\nend\nB --> S";
        let d = parse(src);
        // No phantom node named S; the edge targets subgraph 0.
        assert!(
            d.nodes
                .iter()
                .all(|n| n.label != "S" && n.label != "Shared" || n.subgraph.is_some())
        ); // only member nodes, none named S
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
}
