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
    // Populated by a later task (subgraph containment); read once that task
    // groups nodes into `Diagram::subgraphs`.
    #[allow(dead_code)]
    subgraph: Option<usize>,
}

#[derive(Debug, Clone)]
struct Edge {
    from: usize,
    to: usize,
    label: String,
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
    title: String,
    // Populated by a later task (subgraph parsing/nesting); read once that
    // task assigns node membership and parent/child subgraph relationships.
    #[allow(dead_code)]
    members: Vec<usize>,
    #[allow(dead_code)]
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

const MARKER: &str = "mermaid:";

// ===========================================================================
// Parsing
// ===========================================================================

fn parse(src: &str) -> Diagram {
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut dir = Dir::TopDown;

    let mut get =
        |id: &str, label: Option<&str>, shape: Option<NodeShape>, nodes: &mut Vec<Node>| -> usize {
            if let Some(&i) = index.get(id) {
                // Upgrade a bare node when a later mention carries a label/shape.
                if let Some(l) = label {
                    if !l.is_empty() {
                        nodes[i].label = l.to_string();
                    }
                }
                if let Some(s) = shape {
                    nodes[i].shape = s;
                }
                return i;
            }
            let i = nodes.len();
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
                subgraph: None,
            });
            index.insert(id.to_string(), i);
            i
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
            get(&id, Some(&label), Some(NodeShape::Round), &mut nodes);
            continue;
        }
        // Skip obvious non-graph directives.
        if is_directive(line) {
            continue;
        }
        // Parse a (possibly chained) edge sequence, or a lone node declaration.
        parse_statement(line, &mut nodes, &mut edges, &mut get);
    }

    Diagram {
        dir,
        nodes,
        edges,
        subgraphs: Vec::new(),
    }
}

/// Parse one statement: `A[x] --> B(y) -->|lbl| C`, or a lone `A[x]`.
fn parse_statement(
    line: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    get: &mut impl FnMut(&str, Option<&str>, Option<NodeShape>, &mut Vec<Node>) -> usize,
) {
    // A sequence-style message carries its text after a colon: `A->>B: Hi`. When
    // the part before the colon holds an arrow, treat the tail as the edge label.
    let (graph_part, msg_label) = match line.split_once(':') {
        Some((h, t)) if has_arrow(h) => (h, Some(t.trim().to_string())),
        _ => (line, None),
    };
    let edges_before = edges.len();
    let line = graph_part;
    let segments = split_edges(line);
    // segments alternate: node, (edge-label, node), (edge-label, node), …
    let mut prev: Option<usize> = None;
    let mut pending_label = String::new();
    let mut first = true;
    for seg in segments {
        match seg {
            Seg::Node(tok) => {
                let (id, label, shape) = parse_node_token(&tok);
                if id.is_empty() {
                    continue;
                }
                let idx = get(&id, label.as_deref(), shape, nodes);
                if let Some(p) = prev {
                    edges.push(Edge {
                        from: p,
                        to: idx,
                        label: std::mem::take(&mut pending_label),
                    });
                }
                prev = Some(idx);
                first = false;
            }
            Seg::Arrow(label) => {
                pending_label = label;
                // A leading arrow with no left node: ignore.
                let _ = first;
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

enum Seg {
    Node(String),
    Arrow(String),
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
                out.push(Seg::Arrow(label.trim().to_string()));
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
/// / bare `id`. Returns `(id, label, shape)`.
fn parse_node_token(tok: &str) -> (String, Option<String>, Option<NodeShape>) {
    let tok = tok.trim();
    // Find the first opening bracket.
    let open = tok.find(['[', '(', '{']);
    let Some(open) = open else {
        return (sanitize_id(tok), None, None);
    };
    let id = sanitize_id(tok[..open].trim());
    let rest = &tok[open..];
    let (shape, label) = if let Some(l) = strip_pair(rest, "((", "))") {
        (NodeShape::Circle, l)
    } else if let Some(l) = strip_pair(rest, "([", "])") {
        (NodeShape::Stadium, l)
    } else if let Some(l) = strip_pair(rest, "{", "}") {
        (NodeShape::Diamond, l)
    } else if let Some(l) = strip_pair(rest, "(", ")") {
        (NodeShape::Round, l)
    } else if let Some(l) = strip_pair(rest, "[", "]") {
        (NodeShape::Rect, l)
    } else {
        (NodeShape::Rect, rest.to_string())
    };
    (id, Some(clean_label(&label)), Some(shape))
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
        || l.starts_with("subgraph")
        || l == "end"
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

    // Connectors first (drawn beneath nodes).
    for e in &d.edges {
        shapes.push_str(&emit_connector(d, e, sid));
        sid += 1;
    }
    // Node shapes.
    for node in &d.nodes {
        shapes.push_str(&emit_node(node, sid));
        sid += 1;
    }

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
        .max()
        .unwrap_or(MIN_NODE_W)
        .max(MIN_NODE_W);
    let h = d
        .nodes
        .iter()
        .map(|n| n.y + n.h)
        .max()
        .unwrap_or(NODE_H)
        .max(NODE_H);
    (w, h)
}

fn emit_node(node: &Node, sid: i32) -> String {
    format!(
        "<wps:wsp>\
         <wps:cNvPr id=\"{sid}\" name=\"Node {sid}\"/>\
         <wps:cNvSpPr/>\
         <wps:spPr>\
         <a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{w}\" cy=\"{h}\"/></a:xfrm>\
         <a:prstGeom prst=\"{prst}\"><a:avLst/></a:prstGeom>\
         <a:solidFill><a:srgbClr val=\"DAE8FC\"/></a:solidFill>\
         <a:ln w=\"9525\"><a:solidFill><a:srgbClr val=\"6C8EBF\"/></a:solidFill></a:ln>\
         </wps:spPr>\
         <wps:txbx><w:txbxContent><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr>\
         <w:r><w:t xml:space=\"preserve\">{label}</w:t></w:r></w:p></w:txbxContent></wps:txbx>\
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
    let (from, to) = (&d.nodes[e.from], &d.nodes[e.to]);
    // Anchor points based on flow direction.
    let (x1, y1, x2, y2) = anchors(d.dir, from, to);
    let ox = x1.min(x2);
    let oy = y1.min(y2);
    let cx = (x1 - x2).abs().max(1);
    let cy = (y1 - y2).abs().max(1);
    let flip_h = if x2 < x1 { " flipH=\"1\"" } else { "" };
    let flip_v = if y2 < y1 { " flipV=\"1\"" } else { "" };
    let label = if e.label.is_empty() {
        String::new()
    } else {
        emit_edge_label(&e.label, (x1 + x2) / 2, (y1 + y2) / 2, sid)
    };
    format!(
        "<wps:wsp>\
         <wps:cNvPr id=\"{sid}\" name=\"Edge {sid}\"/>\
         <wps:cNvCnPr/>\
         <wps:spPr>\
         <a:xfrm{flip_h}{flip_v}><a:off x=\"{ox}\" y=\"{oy}\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
         <a:prstGeom prst=\"straightConnector1\"><a:avLst/></a:prstGeom>\
         <a:ln w=\"12700\"><a:solidFill><a:srgbClr val=\"333333\"/></a:solidFill>\
         <a:tailEnd type=\"triangle\"/></a:ln>\
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
fn escape_source(src: &str) -> String {
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

fn xml_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn xml_escape_attr(s: &str) -> String {
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

/// The (start, end) anchor points of an edge, by flow direction.
fn anchors(dir: Dir, from: &Node, to: &Node) -> (i64, i64, i64, i64) {
    match dir {
        Dir::TopDown => (from.x + from.w / 2, from.y + from.h, to.x + to.w / 2, to.y),
        Dir::LeftRight => (from.x + from.w, from.y + from.h / 2, to.x, to.y + to.h / 2),
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
    fn ranks_are_assigned_by_longest_path() {
        let mut d = parse("graph TD\nA-->B\nB-->C\nA-->C");
        layout(&mut d);
        assert_eq!(d.nodes[0].rank, 0); // A
        assert_eq!(d.nodes[1].rank, 1); // B
        assert_eq!(d.nodes[2].rank, 2); // C (longest path A→B→C)
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
        assert!(
            xml.contains("prst=\"straightConnector1\""),
            "connector missing"
        );
        assert!(xml.contains("Start") && xml.contains("End"));
        assert_eq!(text, vec!["Start".to_string(), "End".to_string()]);
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
}
