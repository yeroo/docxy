//! Table styles (#648): the built-in set the Table Design gallery offers,
//! adding their definitions to `styles.xml` at save, and a small resolver
//! that works out each cell's fill and borders from a table style, the
//! table's `w:tblLook`, and the direct `tblBorders`/`tcBorders`/`shd`.
//!
//! The resolver covers fill and borders only; a style's run formatting (a
//! bold header row, white text) is not applied.

use crate::model::{Block, Document, Inline, Table, VMerge};
use crate::table::{GridMap, cell_props, table_props};
use crate::table_props::{BORDERS_ORDER, BorderLine, Edge, PropsXml, TblLook, border_of, shd_fill};
use crate::xml::{Event, XmlParser};

/// Word's default table style, the base of every built-in one.
pub const TABLE_NORMAL: &str = "TableNormal";

/// One built-in table style: its id, its display name and `w:style` XML.
#[derive(Debug, Clone)]
pub struct BuiltinStyle {
    pub id: &'static str,
    pub name: &'static str,
}

/// The gallery's built-in styles, in gallery order.
pub const BUILTIN: &[BuiltinStyle] = &[
    BuiltinStyle {
        id: "TableGrid",
        name: "Table Grid",
    },
    BuiltinStyle {
        id: "PlainTable1",
        name: "Plain Table 1",
    },
    BuiltinStyle {
        id: "PlainTable2",
        name: "Plain Table 2",
    },
    BuiltinStyle {
        id: "PlainTable3",
        name: "Plain Table 3",
    },
    BuiltinStyle {
        id: "PlainTable4",
        name: "Plain Table 4",
    },
    BuiltinStyle {
        id: "PlainTable5",
        name: "Plain Table 5",
    },
    BuiltinStyle {
        id: "GridTable1Light",
        name: "Grid Table 1 Light",
    },
    BuiltinStyle {
        id: "GridTable4-Accent1",
        name: "Grid Table 4 Accent 1",
    },
    BuiltinStyle {
        id: "GridTable5Dark-Accent1",
        name: "Grid Table 5 Dark Accent 1",
    },
    BuiltinStyle {
        id: "ListTable3-Accent1",
        name: "List Table 3 Accent 1",
    },
];

const ACCENT1: &str = "4472C4";
const ACCENT1_20: &str = "D9E2F3";
const ACCENT1_40: &str = "B4C6E7";
const ACCENT1_60: &str = "8EAADB";
const GRAY_25: &str = "BFBFBF";
const GRAY_50: &str = "7F7F7F";
const GRAY_05: &str = "F2F2F2";
const GRAY_40: &str = "999999";
const WHITE: &str = "FFFFFF";

/// A border element: `(edge tag, val, sz, color)`.
fn line(tag: &str, val: &str, sz: u32, color: &str) -> String {
    if val == "nil" {
        return format!("<w:{tag} w:val=\"nil\"/>");
    }
    format!("<w:{tag} w:val=\"{val}\" w:sz=\"{sz}\" w:space=\"0\" w:color=\"{color}\"/>")
}

fn group(name: &str, edges: &[(&str, &str, u32, &str)]) -> String {
    if edges.is_empty() {
        return String::new();
    }
    let mut s = format!("<w:{name}>");
    for (tag, val, sz, color) in edges {
        s.push_str(&line(tag, val, *sz, color));
    }
    s.push_str(&format!("</w:{name}>"));
    s
}

const ALL6: [&str; 6] = ["top", "left", "bottom", "right", "insideH", "insideV"];

fn all(
    val: &'static str,
    sz: u32,
    color: &'static str,
) -> Vec<(&'static str, &'static str, u32, &'static str)> {
    ALL6.iter().map(|t| (*t, val, sz, color)).collect()
}

fn shd(fill: &str) -> String {
    format!("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{fill}\"/>")
}

/// A conditional part: run properties and cell properties (`tcBorders` then
/// `shd`, in `CT_TcPr` order).
fn cond(kind: &str, rpr: &str, borders: &[(&str, &str, u32, &str)], fill: Option<&str>) -> String {
    let mut tcpr = group("tcBorders", borders);
    if let Some(f) = fill {
        tcpr.push_str(&shd(f));
    }
    let rpr = if rpr.is_empty() {
        String::new()
    } else {
        format!("<w:rPr>{rpr}</w:rPr>")
    };
    let tcpr = if tcpr.is_empty() {
        String::new()
    } else {
        format!("<w:tcPr>{tcpr}</w:tcPr>")
    };
    format!("<w:tblStylePr w:type=\"{kind}\">{rpr}<w:tblPr/>{tcpr}</w:tblStylePr>")
}

fn style(
    id: &str,
    name: &str,
    priority: u32,
    tbl_borders: &[(&str, &str, u32, &str)],
    whole_fill: Option<&str>,
    conds: &[String],
) -> String {
    let bands = if conds.is_empty() {
        ""
    } else {
        "<w:tblStyleRowBandSize w:val=\"1\"/><w:tblStyleColBandSize w:val=\"1\"/>"
    };
    let tcpr = whole_fill
        .map(|f| format!("<w:tcPr>{}</w:tcPr>", shd(f)))
        .unwrap_or_default();
    format!(
        "<w:style w:type=\"table\" w:styleId=\"{id}\"><w:name w:val=\"{name}\"/>\
         <w:basedOn w:val=\"{TABLE_NORMAL}\"/><w:uiPriority w:val=\"{priority}\"/>\
         <w:pPr><w:spacing w:after=\"0\" w:line=\"240\" w:lineRule=\"auto\"/></w:pPr>\
         <w:tblPr>{bands}{}</w:tblPr>{tcpr}{}</w:style>",
        group("tblBorders", tbl_borders),
        conds.concat()
    )
}

/// Word's `TableNormal`. `default` marks it the document's default table
/// style (only when the document has none).
pub fn table_normal_xml(default: bool) -> String {
    let d = if default { " w:default=\"1\"" } else { "" };
    format!(
        "<w:style w:type=\"table\"{d} w:styleId=\"{TABLE_NORMAL}\"><w:name w:val=\"Normal Table\"/>\
         <w:uiPriority w:val=\"99\"/><w:semiHidden/><w:unhideWhenUsed/><w:tblPr>\
         <w:tblInd w:w=\"0\" w:type=\"dxa\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/>\
         <w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/>\
         <w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style>"
    )
}

/// The `w:style` XML of a built-in table style, `None` for an unknown id.
pub fn builtin_style_xml(id: &str) -> Option<String> {
    let b = "<w:b/><w:bCs/>";
    let bw = "<w:b/><w:bCs/><w:color w:val=\"FFFFFF\"/>";
    let caps = "<w:b/><w:bCs/><w:caps/>";
    let it = "<w:i/><w:iCs/>";
    let band = |fill: &str| {
        vec![
            cond("band1Vert", "", &[], Some(fill)),
            cond("band1Horz", "", &[], Some(fill)),
        ]
    };
    let name = BUILTIN.iter().find(|s| s.id == id)?.name;
    Some(match id {
        "TableGrid" => style(id, name, 39, &all("single", 4, "auto"), None, &[]),
        "PlainTable1" => {
            let mut c = vec![
                cond("firstRow", b, &[], None),
                cond("lastRow", b, &[("top", "double", 4, GRAY_25)], None),
                cond("firstCol", b, &[], None),
                cond("lastCol", b, &[], None),
            ];
            c.extend(band(GRAY_05));
            style(id, name, 41, &all("single", 4, GRAY_25), None, &c)
        }
        "PlainTable2" => {
            let c = vec![
                cond("firstRow", b, &[("bottom", "single", 4, GRAY_50)], None),
                cond("lastRow", b, &[("top", "single", 4, GRAY_50)], None),
                cond("firstCol", b, &[], None),
                cond("lastCol", b, &[], None),
                cond(
                    "band1Vert",
                    "",
                    &[
                        ("left", "single", 4, GRAY_50),
                        ("right", "single", 4, GRAY_50),
                    ],
                    None,
                ),
                cond(
                    "band1Horz",
                    "",
                    &[
                        ("top", "single", 4, GRAY_50),
                        ("bottom", "single", 4, GRAY_50),
                    ],
                    None,
                ),
            ];
            style(
                id,
                name,
                42,
                &[
                    ("top", "single", 4, GRAY_50),
                    ("bottom", "single", 4, GRAY_50),
                ],
                None,
                &c,
            )
        }
        "PlainTable3" => {
            let mut c = vec![
                cond("firstRow", caps, &[("bottom", "single", 4, GRAY_50)], None),
                cond("lastRow", caps, &[], None),
                cond("firstCol", caps, &[("right", "single", 4, GRAY_50)], None),
                cond("lastCol", caps, &[], None),
            ];
            c.extend(band(GRAY_05));
            style(id, name, 43, &[], None, &c)
        }
        "PlainTable4" => {
            let mut c = vec![
                cond("firstRow", b, &[], None),
                cond("lastRow", b, &[], None),
                cond("firstCol", b, &[], None),
                cond("lastCol", b, &[], None),
            ];
            c.extend(band(GRAY_05));
            style(id, name, 44, &[], None, &c)
        }
        "PlainTable5" => {
            let mut c = vec![
                cond("firstRow", it, &[("bottom", "single", 4, GRAY_50)], None),
                cond("lastRow", it, &[("top", "single", 4, GRAY_50)], None),
                cond("firstCol", it, &[("right", "single", 4, GRAY_50)], None),
                cond("lastCol", it, &[("left", "single", 4, GRAY_50)], None),
            ];
            c.extend(band(GRAY_05));
            style(id, name, 45, &[], None, &c)
        }
        "GridTable1Light" => {
            let c = vec![
                cond("firstRow", b, &[("bottom", "single", 12, GRAY_40)], None),
                cond("lastRow", b, &[("top", "double", 2, GRAY_40)], None),
                cond("firstCol", b, &[], None),
                cond("lastCol", b, &[], None),
            ];
            style(id, name, 46, &all("single", 4, GRAY_25), None, &c)
        }
        "GridTable4-Accent1" => {
            let mut c = vec![
                cond(
                    "firstRow",
                    bw,
                    &[
                        ("top", "single", 4, ACCENT1),
                        ("left", "single", 4, ACCENT1),
                        ("bottom", "single", 4, ACCENT1),
                        ("right", "single", 4, ACCENT1),
                        ("insideH", "nil", 0, ""),
                        ("insideV", "nil", 0, ""),
                    ],
                    Some(ACCENT1),
                ),
                cond("lastRow", b, &[("top", "double", 4, ACCENT1)], None),
                cond("firstCol", b, &[], None),
                cond("lastCol", b, &[], None),
            ];
            c.extend(band(ACCENT1_20));
            style(id, name, 49, &all("single", 4, ACCENT1_60), None, &c)
        }
        "GridTable5Dark-Accent1" => {
            let mut c = vec![
                cond(
                    "firstRow",
                    bw,
                    &[
                        ("top", "nil", 0, ""),
                        ("left", "nil", 0, ""),
                        ("right", "nil", 0, ""),
                        ("insideV", "nil", 0, ""),
                    ],
                    Some(ACCENT1),
                ),
                cond("lastRow", bw, &[("top", "single", 4, WHITE)], Some(ACCENT1)),
                cond("firstCol", bw, &[], Some(ACCENT1)),
                cond("lastCol", bw, &[], Some(ACCENT1)),
            ];
            c.extend(band(ACCENT1_40));
            style(id, name, 50, &all("single", 4, WHITE), Some(ACCENT1_20), &c)
        }
        "ListTable3-Accent1" => {
            let c = vec![
                cond("firstRow", bw, &[], Some(ACCENT1)),
                cond("lastRow", b, &[("top", "double", 4, ACCENT1)], None),
                cond("firstCol", b, &[], None),
                cond("lastCol", b, &[], None),
                cond(
                    "band1Vert",
                    "",
                    &[
                        ("left", "single", 4, ACCENT1),
                        ("right", "single", 4, ACCENT1),
                    ],
                    None,
                ),
                cond(
                    "band1Horz",
                    "",
                    &[
                        ("top", "single", 4, ACCENT1),
                        ("bottom", "single", 4, ACCENT1),
                    ],
                    None,
                ),
            ];
            style(
                id,
                name,
                48,
                &[
                    ("top", "single", 4, ACCENT1),
                    ("left", "single", 4, ACCENT1),
                    ("bottom", "single", 4, ACCENT1),
                    ("right", "single", 4, ACCENT1),
                ],
                None,
                &c,
            )
        }
        _ => return None,
    })
}

// ---- styles.xml ----

/// The `w:style` element with `w:styleId="id"` in a styles part, verbatim.
pub fn find_style_xml<'a>(styles_xml: &'a str, id: &str) -> Option<&'a str> {
    let mut p = XmlParser::new(styles_xml);
    let mut depth = 0;
    loop {
        match p.next() {
            Event::Start => {
                depth += 1;
                if depth == 2 && p.name() == "w:style" && p.attr("w:styleId") == id {
                    let start = p.start_pos();
                    p.skip_element();
                    return Some(p.raw_slice(start, p.pos()));
                }
                if depth >= 2 {
                    p.skip_element();
                    depth -= 1;
                }
            }
            Event::End => depth -= 1,
            Event::Text => {}
            Event::Eof => return None,
        }
    }
}

/// The table style ids a document's tables reference (`w:tblStyle`), each
/// once, in document order.
pub fn referenced_table_styles(doc: &Document) -> Vec<String> {
    fn walk(blocks: &[Block], out: &mut Vec<String>) {
        for b in blocks {
            match b {
                Block::Table(t) => {
                    if let Some(id) = table_props(t).attr("w:tblStyle", "w:val") {
                        if !out.contains(&id) {
                            out.push(id);
                        }
                    }
                    for row in &t.rows {
                        for cell in &row.cells {
                            walk(&cell.blocks, out);
                        }
                    }
                }
                Block::Paragraph(p) => {
                    for inline in &p.content {
                        if let Inline::TextBox { blocks, .. } = inline {
                            walk(blocks, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.body, &mut out);
    out
}

/// `styles_xml` with a definition appended for each built-in id in `ids` it
/// lacks, plus `TableNormal` (their base) when missing. `None` when nothing
/// needs adding. Existing definitions are never touched.
pub fn with_table_styles(styles_xml: &str, ids: &[String]) -> Option<String> {
    let mut additions = String::new();
    let has = |xml: &str, id: &str| xml.contains(&format!("w:styleId=\"{id}\""));
    for id in ids {
        if has(styles_xml, id) || has(&additions, id) {
            continue;
        }
        if let Some(def) = builtin_style_xml(id) {
            additions.push_str(&def);
        }
    }
    if additions.is_empty() {
        return None;
    }
    // Only a well-formed part gets additions: its closing tag, or a
    // self-closing root that can be opened.
    let close = styles_xml.rfind("</w:styles>");
    let empty_root = close.is_none().then(|| {
        let start = styles_xml.find("<w:styles")?;
        let end = start + styles_xml[start..].find('>')?;
        styles_xml[..=end].ends_with("/>").then_some(end - 1)
    });
    let empty_root = empty_root.flatten();
    if close.is_none() && empty_root.is_none() {
        return None;
    }
    if !has(styles_xml, TABLE_NORMAL) {
        let has_default = styles_xml.contains("w:type=\"table\" w:default=\"1\"")
            || styles_xml.contains("w:default=\"1\" w:type=\"table\"");
        additions.insert_str(0, &table_normal_xml(!has_default));
    }
    Some(match (close, empty_root) {
        (Some(at), _) => format!("{}{additions}{}", &styles_xml[..at], &styles_xml[at..]),
        // `<w:styles .../>` → `<w:styles ...>additions</w:styles>`.
        (None, Some(slash)) => format!(
            "{}>{additions}</w:styles>{}",
            styles_xml[..slash].trim_end(),
            &styles_xml[slash + 2..]
        ),
        (None, None) => unreachable!("refused above"),
    })
}

/// The `w:tblStyle` ids an XML part's tables reference, each once.
pub fn table_style_ids_in_xml(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if p.name() == "w:tblStyle" => {
                let id = p.attr("w:val").to_string();
                if !id.is_empty() && !out.contains(&id) {
                    out.push(id);
                }
            }
            Event::Eof => return out,
            _ => {}
        }
    }
}

// ---- resolution ----

/// Fill and borders from one part of a table style.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StylePart {
    /// `Some(None)`: explicitly no fill.
    pub fill: Option<Option<String>>,
    pub borders: Vec<(Edge, BorderLine)>,
}

impl StylePart {
    fn edge(&self, e: Edge) -> Option<&BorderLine> {
        self.borders.iter().find(|(x, _)| *x == e).map(|(_, l)| l)
    }
    fn overlay(&mut self, other: &StylePart) {
        if other.fill.is_some() {
            self.fill = other.fill.clone();
        }
        for (e, l) in &other.borders {
            self.borders.retain(|(x, _)| x != e);
            self.borders.push((*e, l.clone()));
        }
    }
}

/// The parts of a table style the resolver uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableStyle {
    pub id: String,
    pub name: String,
    pub based_on: Option<String>,
    pub row_band: usize,
    pub col_band: usize,
    pub whole: StylePart,
    pub conds: Vec<(String, StylePart)>,
}

const EDGES: [Edge; 8] = [
    Edge::Top,
    Edge::Left,
    Edge::Bottom,
    Edge::Right,
    Edge::InsideH,
    Edge::InsideV,
    Edge::DiagDown,
    Edge::DiagUp,
];

fn read_part(tblpr: Option<&str>, tcpr: Option<&str>) -> StylePart {
    let mut part = StylePart::default();
    for (container, name, group) in [
        (tblpr, "w:tblPr", "w:tblBorders"),
        (tcpr, "w:tcPr", "w:tcBorders"),
    ] {
        let Some(c) = container else { continue };
        let props = PropsXml::parse(c, name, BORDERS_ORDER);
        let g = props.get(group);
        for e in EDGES {
            if let Some(l) = border_of(g, e) {
                part.borders.retain(|(x, _)| *x != e);
                part.borders.push((e, l));
            }
        }
        let (fill, present) = shd_fill(props.get("w:shd"));
        if present {
            part.fill = Some(fill);
        }
    }
    part
}

/// Parse a `w:style w:type="table"` element.
pub fn parse_table_style(xml: &str) -> TableStyle {
    let mut s = TableStyle {
        row_band: 1,
        col_band: 1,
        ..TableStyle::default()
    };
    let mut p = XmlParser::new(xml);
    // The style element.
    loop {
        match p.next() {
            Event::Start => break,
            Event::Eof => return s,
            _ => {}
        }
    }
    s.id = p.attr("w:styleId").to_string();
    let (mut tblpr, mut tcpr) = (None, None);
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name();
                let start = p.start_pos();
                let val = p.attr("w:val").to_string();
                let kind = p.attr("w:type").to_string();
                p.skip_element();
                let raw = p.raw_slice(start, p.pos());
                match name {
                    "w:name" => s.name = val,
                    "w:basedOn" => s.based_on = Some(val),
                    "w:tblPr" => {
                        tblpr = Some(raw);
                        let t = PropsXml::parse(raw, "w:tblPr", BORDERS_ORDER);
                        let n = |name| {
                            t.attr(name, "w:val")
                                .and_then(|v| v.parse::<usize>().ok())
                                .filter(|&n| n > 0)
                        };
                        s.row_band = n("w:tblStyleRowBandSize").unwrap_or(1);
                        s.col_band = n("w:tblStyleColBandSize").unwrap_or(1);
                    }
                    "w:tcPr" => tcpr = Some(raw),
                    "w:tblStylePr" => {
                        let c = PropsXml::parse(raw, "w:tblStylePr", BORDERS_ORDER);
                        s.conds
                            .push((kind, read_part(c.get("w:tblPr"), c.get("w:tcPr"))));
                    }
                    _ => {}
                }
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    s.whole = read_part(tblpr, tcpr);
    s
}

/// The table style `id`, from `styles_xml` (following `w:basedOn`) or the
/// built-in set.
pub fn lookup_style(styles_xml: Option<&str>, id: &str) -> Option<TableStyle> {
    fn go(styles_xml: Option<&str>, id: &str, depth: usize) -> Option<TableStyle> {
        let own = styles_xml
            .and_then(|x| find_style_xml(x, id))
            .map(parse_table_style)
            .or_else(|| builtin_style_xml(id).map(|x| parse_table_style(&x)))?;
        let Some(base_id) = own.based_on.clone().filter(|_| depth < 8) else {
            return Some(own);
        };
        let Some(mut base) = go(styles_xml, &base_id, depth + 1) else {
            return Some(own);
        };
        base.whole.overlay(&own.whole);
        for (kind, part) in &own.conds {
            match base.conds.iter_mut().find(|(k, _)| k == kind) {
                Some((_, b)) => b.overlay(part),
                None => base.conds.push((kind.clone(), part.clone())),
            }
        }
        base.id = own.id;
        base.name = own.name;
        base.row_band = own.row_band;
        base.col_band = own.col_band;
        Some(base)
    }
    go(styles_xml, id, 0)
}

/// One cell's resolved look.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellLook {
    /// `RRGGBB`, or `None` for no fill.
    pub fill: Option<String>,
    pub top: Option<BorderLine>,
    pub left: Option<BorderLine>,
    pub bottom: Option<BorderLine>,
    pub right: Option<BorderLine>,
    pub diag_down: Option<BorderLine>,
    pub diag_up: Option<BorderLine>,
}

impl CellLook {
    fn slot(&mut self, e: Edge) -> &mut Option<BorderLine> {
        match e {
            Edge::Top => &mut self.top,
            Edge::Left => &mut self.left,
            Edge::Bottom => &mut self.bottom,
            Edge::Right => &mut self.right,
            Edge::DiagDown => &mut self.diag_down,
            Edge::DiagUp => &mut self.diag_up,
            Edge::InsideH | Edge::InsideV => unreachable!("mapped to a side first"),
        }
    }
    /// A side, if a visible line is drawn there.
    pub fn visible(&self, e: Edge) -> Option<&BorderLine> {
        match e {
            Edge::Top => self.top.as_ref(),
            Edge::Left => self.left.as_ref(),
            Edge::Bottom => self.bottom.as_ref(),
            Edge::Right => self.right.as_ref(),
            Edge::DiagDown => self.diag_down.as_ref(),
            Edge::DiagUp => self.diag_up.as_ref(),
            _ => None,
        }
        .filter(|l| l.visible())
    }
}

/// A region a style part applies to: rows and grid columns, inclusive.
struct Region {
    rows: (usize, usize),
    cols: (usize, usize),
}

/// Apply `part` over `region` to a cell at rows `rr` and grid columns `cc`:
/// a side on the region's edge takes the part's outer border, one inside it
/// the part's inside border.
fn apply(
    look: &mut CellLook,
    part: &StylePart,
    region: &Region,
    rr: (usize, usize),
    cc: (usize, usize),
) {
    if let Some(f) = &part.fill {
        look.fill = f.clone();
    }
    let pick = |outer: bool, o: Edge, i: Edge| {
        if outer { part.edge(o) } else { part.edge(i) }
    };
    let sides = [
        (
            Edge::Top,
            pick(rr.0 <= region.rows.0, Edge::Top, Edge::InsideH),
        ),
        (
            Edge::Bottom,
            pick(rr.1 >= region.rows.1, Edge::Bottom, Edge::InsideH),
        ),
        (
            Edge::Left,
            pick(cc.0 <= region.cols.0, Edge::Left, Edge::InsideV),
        ),
        (
            Edge::Right,
            pick(cc.1 >= region.cols.1, Edge::Right, Edge::InsideV),
        ),
        (Edge::DiagDown, part.edge(Edge::DiagDown)),
        (Edge::DiagUp, part.edge(Edge::DiagUp)),
    ];
    for (side, l) in sides {
        if let Some(l) = l {
            *look.slot(side) = Some(l.clone());
        }
    }
}

/// Every cell's look, `[row][cell]`: the style's parts in Word's order of
/// precedence (whole table, column bands, row bands, first/last column,
/// first/last row, corners) as `w:tblLook` enables them, then the table's
/// direct `w:tblBorders`, then each cell's own `w:tcBorders`/`w:shd`.
pub fn resolve(table: &Table, style: Option<&TableStyle>) -> Vec<Vec<CellLook>> {
    let map = GridMap::of(table);
    let tp = table_props(table);
    let look = tp.get("w:tblLook").map(TblLook::parse).unwrap_or_default();
    let direct = read_part(table.raw_tblpr.as_deref(), None);
    let nrows = table.rows.len();
    let ncols = map.width(table);
    let whole = Region {
        rows: (0, nrows.saturating_sub(1)),
        cols: (0, ncols.saturating_sub(1)),
    };
    let cond =
        |kind: &str| style.and_then(|s| s.conds.iter().find(|(k, _)| k == kind).map(|(_, p)| p));
    let mut out = Vec::with_capacity(nrows);
    for (r, rm) in map.rows.iter().enumerate() {
        let mut row = Vec::with_capacity(rm.cells.len());
        for (ci, &(s, n)) in rm.cells.iter().enumerate() {
            let cell = &table.rows[r].cells[ci];
            // A vertically merged cell reaches down to its last row.
            let last = if cell.v_merge == VMerge::Restart {
                map.merge_end(table, r, ci)
            } else {
                r
            };
            let rr = (r, last);
            let cc = (s, s + n - 1);
            let mut cl = CellLook::default();
            if let Some(st) = style {
                apply(&mut cl, &st.whole, &whole, rr, cc);
                let first_row = usize::from(look.first_row);
                let last_row = usize::from(look.last_row);
                let first_col = usize::from(look.first_col);
                let last_col = usize::from(look.last_col);
                if look.banded_cols && s >= first_col && s + last_col < ncols {
                    let k = (s - first_col) / st.col_band.max(1);
                    let start = first_col + k * st.col_band.max(1);
                    let region = Region {
                        rows: whole.rows,
                        cols: (start, start + st.col_band.max(1) - 1),
                    };
                    let kind = if k % 2 == 0 { "band1Vert" } else { "band2Vert" };
                    if let Some(p) = cond(kind) {
                        apply(&mut cl, p, &region, rr, cc);
                    }
                }
                if look.banded_rows && r >= first_row && r + last_row < nrows {
                    let k = (r - first_row) / st.row_band.max(1);
                    let start = first_row + k * st.row_band.max(1);
                    let region = Region {
                        rows: (start, start + st.row_band.max(1) - 1),
                        cols: whole.cols,
                    };
                    let kind = if k % 2 == 0 { "band1Horz" } else { "band2Horz" };
                    if let Some(p) = cond(kind) {
                        apply(&mut cl, p, &region, rr, cc);
                    }
                }
                let is_first_col = look.first_col && s == 0;
                let is_last_col = look.last_col && s + n == ncols;
                let is_first_row = look.first_row && r == 0;
                let is_last_row = look.last_row && last + 1 == nrows;
                let parts = [
                    (
                        is_first_col,
                        "firstCol",
                        Region {
                            rows: whole.rows,
                            cols: (0, 0),
                        },
                    ),
                    (
                        is_last_col,
                        "lastCol",
                        Region {
                            rows: whole.rows,
                            cols: (ncols - 1, ncols - 1),
                        },
                    ),
                    (
                        is_first_row,
                        "firstRow",
                        Region {
                            rows: (0, 0),
                            cols: whole.cols,
                        },
                    ),
                    (
                        is_last_row,
                        "lastRow",
                        Region {
                            rows: (nrows - 1, nrows - 1),
                            cols: whole.cols,
                        },
                    ),
                    (
                        is_first_row && is_last_col,
                        "neCell",
                        Region {
                            rows: (0, 0),
                            cols: cc,
                        },
                    ),
                    (
                        is_first_row && is_first_col,
                        "nwCell",
                        Region {
                            rows: (0, 0),
                            cols: cc,
                        },
                    ),
                    (
                        is_last_row && is_last_col,
                        "seCell",
                        Region {
                            rows: (nrows - 1, nrows - 1),
                            cols: cc,
                        },
                    ),
                    (
                        is_last_row && is_first_col,
                        "swCell",
                        Region {
                            rows: (nrows - 1, nrows - 1),
                            cols: cc,
                        },
                    ),
                ];
                for (on, kind, region) in parts {
                    if on {
                        if let Some(p) = cond(kind) {
                            apply(&mut cl, p, &region, rr, cc);
                        }
                    }
                }
            }
            apply(&mut cl, &direct, &whole, rr, cc);
            let own = read_part(None, cell.raw_tcpr.as_deref());
            let own_region = Region { rows: rr, cols: cc };
            apply(&mut cl, &own, &own_region, rr, cc);
            // A direct fill of `auto` is an explicit "no fill".
            let (fill, present) = shd_fill(cell_props(cell).get("w:shd"));
            if present {
                cl.fill = fill;
            }
            row.push(cl);
        }
        out.push(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{AutoFit, edit_table_props, new_table};

    #[test]
    fn every_builtin_style_has_well_formed_xml() {
        for b in BUILTIN {
            let xml = builtin_style_xml(b.id).unwrap();
            let mut p = XmlParser::new(&xml);
            let mut depth = 0i32;
            loop {
                match p.next() {
                    Event::Start => depth += 1,
                    Event::End => depth -= 1,
                    Event::Eof => break,
                    Event::Text => {}
                }
            }
            assert_eq!(depth, 0, "{}", b.id);
            let s = parse_table_style(&xml);
            assert_eq!(s.id, b.id);
            assert_eq!(s.name, b.name);
            assert_eq!(s.based_on.as_deref(), Some(TABLE_NORMAL));
        }
        assert!(builtin_style_xml("NoSuchStyle").is_none());
    }

    const STYLES: &str = "<?xml version=\"1.0\"?><w:styles xmlns:w=\"x\">\
        <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>\
        </w:styles>";

    #[test]
    fn styles_are_added_once_with_their_base() {
        let ids = vec!["GridTable4-Accent1".to_string(), "Unknown".to_string()];
        let once = with_table_styles(STYLES, &ids).unwrap();
        assert!(once.contains("w:styleId=\"GridTable4-Accent1\""));
        assert!(once.contains("w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\""));
        assert!(
            once.contains("w:type=\"tblStylePr\"")
                || once.contains("<w:tblStylePr w:type=\"firstRow\">")
        );
        assert!(once.ends_with("</w:styles>"));
        assert_eq!(with_table_styles(&once, &ids), None, "idempotent");
        assert_eq!(with_table_styles(STYLES, &["Unknown".to_string()]), None);
        // A document's own definition is never replaced.
        let own = STYLES.replace(
            "</w:styles>",
            "<w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Mine\"/></w:style></w:styles>",
        );
        assert_eq!(with_table_styles(&own, &["TableGrid".to_string()]), None);
    }

    #[test]
    fn a_second_default_table_style_is_not_declared() {
        let styles = STYLES.replace(
            "</w:styles>",
            "<w:style w:type=\"table\" w:default=\"1\" w:styleId=\"a1\"><w:name w:val=\"Normal Table\"/></w:style></w:styles>",
        );
        let out = with_table_styles(&styles, &["TableGrid".to_string()]).unwrap();
        assert!(out.contains("<w:style w:type=\"table\" w:styleId=\"TableNormal\">"));
    }

    #[test]
    fn find_style_in_a_styles_part() {
        let xml = with_table_styles(STYLES, &["PlainTable1".to_string()]).unwrap();
        let s = find_style_xml(&xml, "PlainTable1").unwrap();
        assert!(s.starts_with("<w:style w:type=\"table\" w:styleId=\"PlainTable1\">"));
        assert!(s.ends_with("</w:style>"));
        assert!(find_style_xml(&xml, "Normal").is_some());
        assert!(find_style_xml(&xml, "Nope").is_none());
    }

    fn styled(rows: usize, cols: usize, id: &str, look: TblLook) -> Table {
        let mut t = new_table(rows, cols, 9000, AutoFit::Default);
        edit_table_props(&mut t, |p| {
            p.remove("w:tblBorders");
            p.set(&format!("<w:tblStyle w:val=\"{id}\"/>"));
            p.set(&look.to_xml());
        });
        t
    }

    #[test]
    fn header_row_and_banding_follow_tbl_look() {
        let t = styled(4, 2, "GridTable4-Accent1", TblLook::default());
        let st = lookup_style(None, "GridTable4-Accent1").unwrap();
        let l = resolve(&t, Some(&st));
        assert_eq!(l[0][0].fill.as_deref(), Some(ACCENT1), "header row");
        assert_eq!(l[1][0].fill.as_deref(), Some(ACCENT1_20), "first band");
        assert_eq!(l[2][0].fill, None, "second band");
        assert_eq!(l[3][1].fill.as_deref(), Some(ACCENT1_20));
        assert_eq!(l[1][0].top.as_ref().unwrap().color, ACCENT1_60);
        // The header's inside vertical border is nil.
        assert!(l[0][0].visible(Edge::Right).is_none());

        let look = TblLook {
            first_row: false,
            banded_rows: false,
            ..TblLook::default()
        };
        let t = styled(3, 2, "GridTable4-Accent1", look);
        let l = resolve(&t, Some(&st));
        assert!(l.iter().flatten().all(|c| c.fill.is_none()));
    }

    #[test]
    fn direct_formatting_beats_the_style() {
        let mut t = styled(2, 2, "TableGrid", TblLook::default());
        crate::table::edit_cell_props(&mut t.rows[0].cells[0], |p| {
            p.set("<w:tcBorders><w:top w:val=\"nil\"/><w:tl2br w:val=\"single\" w:sz=\"4\" w:color=\"FF0000\"/></w:tcBorders>");
            p.set("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"00FF00\"/>");
        });
        let st = lookup_style(None, "TableGrid").unwrap();
        let l = resolve(&t, Some(&st));
        assert!(l[0][0].visible(Edge::Top).is_none());
        assert!(l[0][0].visible(Edge::Left).is_some());
        assert_eq!(l[0][0].fill.as_deref(), Some("00FF00"));
        assert_eq!(l[0][0].diag_down.as_ref().unwrap().color, "FF0000");
        // No style: the table's own borders still draw.
        let t = new_table(1, 1, 9000, AutoFit::Default);
        let l = resolve(&t, None);
        assert!(l[0][0].visible(Edge::Top).is_some());
    }

    #[test]
    fn based_on_merges_the_parent() {
        let xml = format!(
            "<w:styles>{}<w:style w:type=\"table\" w:styleId=\"Mine\"><w:name w:val=\"Mine\"/>\
             <w:basedOn w:val=\"GridTable4-Accent1\"/><w:tblStylePr w:type=\"firstRow\">\
             <w:tcPr><w:shd w:val=\"clear\" w:fill=\"123456\"/></w:tcPr></w:tblStylePr></w:style></w:styles>",
            ""
        );
        let st = lookup_style(Some(&xml), "Mine").unwrap();
        let t = styled(2, 1, "Mine", TblLook::default());
        let l = resolve(&t, Some(&st));
        assert_eq!(l[0][0].fill.as_deref(), Some("123456"));
        assert_eq!(
            l[1][0].fill.as_deref(),
            Some(ACCENT1_20),
            "the parent's banding"
        );
    }

    #[test]
    fn referenced_ids_include_nested_tables() {
        let mut outer = styled(1, 1, "PlainTable1", TblLook::default());
        let inner = styled(1, 1, "TableGrid", TblLook::default());
        outer.rows[0].cells[0].blocks.push(Block::Table(inner));
        let doc = Document {
            body: vec![Block::Table(outer)],
        };
        assert_eq!(referenced_table_styles(&doc), ["PlainTable1", "TableGrid"]);
    }
}
