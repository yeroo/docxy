//! A small editor over a raw table property container (`w:tblPr`, `w:trPr`,
//! `w:tcPr`, or a border group such as `w:tcBorders`).
//!
//! The model keeps table formatting as verbatim XML so unknown children
//! round-trip untouched. Table commands only need to read and replace a handful
//! of children, so this splits a container into its top-level child elements,
//! lets a caller get/set/remove one by name, and writes it back: a new child
//! goes where the schema puts it, every other child is kept as it was.

use crate::xml::{Event, XmlParser};

/// `CT_TcPr` child order.
pub const TCPR_ORDER: &[&str] = &[
    "w:cnfStyle",
    "w:tcW",
    "w:gridSpan",
    "w:hMerge",
    "w:vMerge",
    "w:tcBorders",
    "w:shd",
    "w:noWrap",
    "w:tcMar",
    "w:textDirection",
    "w:tcFitText",
    "w:vAlign",
    "w:hideMark",
    "w:headers",
    "w:cellIns",
    "w:cellDel",
    "w:cellMerge",
    "w:tcPrChange",
];

/// `CT_TblPr` child order.
pub const TBLPR_ORDER: &[&str] = &[
    "w:tblStyle",
    "w:tblpPr",
    "w:tblOverlap",
    "w:bidiVisual",
    "w:tblStyleRowBandSize",
    "w:tblStyleColBandSize",
    "w:tblW",
    "w:jc",
    "w:tblCellSpacing",
    "w:tblInd",
    "w:tblBorders",
    "w:shd",
    "w:tblLayout",
    "w:tblCellMar",
    "w:tblLook",
    "w:tblCaption",
    "w:tblDescription",
    "w:tblPrChange",
];

/// `CT_TrPr` is a choice group, so any order is valid; this is Word's.
pub const TRPR_ORDER: &[&str] = &[
    "w:cnfStyle",
    "w:divId",
    "w:gridBefore",
    "w:gridAfter",
    "w:wBefore",
    "w:wAfter",
    "w:cantSplit",
    "w:trHeight",
    "w:tblHeader",
    "w:tblCellSpacing",
    "w:jc",
    "w:hidden",
    "w:ins",
    "w:del",
    "w:trPrChange",
];

/// `CT_TcBorders` / `CT_TblBorders` child order. `w:start`/`w:end` are the
/// strict names of `w:left`/`w:right` and share their rank.
pub const BORDERS_ORDER: &[&str] = &[
    "w:top",
    "w:left",
    "w:bottom",
    "w:right",
    "w:insideH",
    "w:insideV",
    "w:tl2br",
    "w:tr2bl",
];

fn rank(order: &[&str], name: &str) -> Option<usize> {
    let name = match name {
        "w:start" => "w:left",
        "w:end" => "w:right",
        other => other,
    };
    order.iter().position(|n| *n == name)
}

/// A property container split into its child elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropsXml {
    /// The container's qualified name, e.g. `w:tcPr`.
    pub name: String,
    /// The container's attributes, verbatim (usually empty).
    attrs: String,
    /// Each top-level child element, verbatim.
    children: Vec<String>,
    order: &'static [&'static str],
}

impl PropsXml {
    /// An empty container.
    pub fn new(name: &str, order: &'static [&'static str]) -> Self {
        PropsXml {
            name: name.to_string(),
            attrs: String::new(),
            children: Vec::new(),
            order,
        }
    }

    /// Split `raw` (a whole container element). Anything that is not a single
    /// element yields an empty container named `name`.
    pub fn parse(raw: &str, name: &str, order: &'static [&'static str]) -> Self {
        let mut out = PropsXml::new(name, order);
        let mut p = XmlParser::new(raw);
        loop {
            match p.next() {
                Event::Start => break,
                Event::Text => continue,
                Event::End | Event::Eof => return out,
            }
        }
        out.name = p.name().to_string();
        let open_end = p.pos();
        let open = p.raw_slice(p.start_pos(), open_end);
        let inner = open
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_end_matches('/');
        out.attrs = inner
            .strip_prefix(out.name.as_str())
            .unwrap_or("")
            .trim_end()
            .to_string();
        loop {
            match p.next() {
                Event::Start => {
                    let start = p.start_pos();
                    p.skip_element();
                    out.children.push(p.raw_slice(start, p.pos()).to_string());
                }
                Event::Text => {}
                Event::End | Event::Eof => break,
            }
        }
        out
    }

    /// Parse an optional container, or start an empty one.
    pub fn parse_opt(raw: Option<&str>, name: &str, order: &'static [&'static str]) -> Self {
        match raw {
            Some(raw) => PropsXml::parse(raw, name, order),
            None => PropsXml::new(name, order),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// The qualified names of the children, in order.
    pub fn child_names(&self) -> Vec<String> {
        self.children.iter().map(|c| element_name(c)).collect()
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.children.iter().position(|c| element_name(c) == name)
    }

    /// The child element named `name`, verbatim.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.index_of(name).map(|i| self.children[i].as_str())
    }

    /// An attribute of the child named `name`.
    pub fn attr(&self, name: &str, attr: &str) -> Option<String> {
        self.get(name).and_then(|x| element_attr(x, attr))
    }

    /// Replace the child named like `xml`'s root, or insert it in schema order.
    pub fn set(&mut self, xml: &str) {
        let name = element_name(xml);
        if let Some(i) = self.index_of(&name) {
            self.children[i] = xml.to_string();
            return;
        }
        let at = match rank(self.order, &name) {
            Some(r) => self
                .children
                .iter()
                .position(|c| rank(self.order, &element_name(c)).is_some_and(|cr| cr > r))
                .unwrap_or(self.children.len()),
            None => self.children.len(),
        };
        self.children.insert(at, xml.to_string());
    }

    /// Remove the child named `name`. Returns whether one was there.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.children.len();
        self.children.retain(|c| element_name(c) != name);
        self.children.len() != before
    }

    /// Write the container back.
    pub fn to_xml(&self) -> String {
        let mut s = format!("<{}", self.name);
        if !self.attrs.is_empty() {
            s.push(' ');
            s.push_str(self.attrs.trim_start());
        }
        if self.children.is_empty() {
            s.push_str("/>");
            return s;
        }
        s.push('>');
        for c in &self.children {
            s.push_str(c);
        }
        s.push_str("</");
        s.push_str(&self.name);
        s.push('>');
        s
    }
}

/// The qualified name of the root element of `xml`.
pub fn element_name(xml: &str) -> String {
    let t = xml.trim_start();
    let t = t.strip_prefix('<').unwrap_or(t);
    t.chars()
        .take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/')
        .collect()
}

/// An attribute of the root element of `xml` (entities not decoded).
pub fn element_attr(xml: &str, attr: &str) -> Option<String> {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                return p
                    .attrs()
                    .iter()
                    .find(|a| a.name == attr)
                    .map(|a| a.value.to_string());
            }
            Event::Text => continue,
            Event::End | Event::Eof => return None,
        }
    }
}

// ---- typed views ----

/// A cell's vertical alignment (`w:vAlign`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

impl VAlign {
    pub fn val(self) -> &'static str {
        match self {
            VAlign::Top => "top",
            VAlign::Center => "center",
            VAlign::Bottom => "bottom",
        }
    }
    pub fn parse(v: &str) -> Self {
        match v {
            "center" => VAlign::Center,
            "bottom" => VAlign::Bottom,
            _ => VAlign::Top,
        }
    }
}

/// One side of a table/cell border group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    Top,
    Left,
    Bottom,
    Right,
    InsideH,
    InsideV,
    /// Diagonal from the top-left to the bottom-right corner (`w:tl2br`).
    DiagDown,
    /// Diagonal from the top-right to the bottom-left corner (`w:tr2bl`).
    DiagUp,
}

impl Edge {
    pub fn tag(self) -> &'static str {
        match self {
            Edge::Top => "w:top",
            Edge::Left => "w:left",
            Edge::Bottom => "w:bottom",
            Edge::Right => "w:right",
            Edge::InsideH => "w:insideH",
            Edge::InsideV => "w:insideV",
            Edge::DiagDown => "w:tl2br",
            Edge::DiagUp => "w:tr2bl",
        }
    }
    /// The strict-schema synonym (`w:start`/`w:end`), if any.
    fn alt_tag(self) -> Option<&'static str> {
        match self {
            Edge::Left => Some("w:start"),
            Edge::Right => Some("w:end"),
            _ => None,
        }
    }
}

/// A resolved border line: `None` style fields mean "no line".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorderLine {
    /// `w:val` (`single`, `double`, `nil`, `none`, …).
    pub val: String,
    /// Eighths of a point.
    pub sz: u32,
    /// `RRGGBB` or `auto`.
    pub color: String,
}

impl BorderLine {
    /// A visible line (anything but `nil`/`none`).
    pub fn visible(&self) -> bool {
        !matches!(self.val.as_str(), "nil" | "none" | "")
    }
    pub fn single() -> Self {
        BorderLine {
            val: "single".into(),
            sz: 4,
            color: "auto".into(),
        }
    }
    pub fn nil() -> Self {
        BorderLine {
            val: "nil".into(),
            sz: 0,
            color: "auto".into(),
        }
    }
    fn parse(xml: &str) -> Self {
        BorderLine {
            val: element_attr(xml, "w:val").unwrap_or_default(),
            sz: element_attr(xml, "w:sz")
                .and_then(|v| v.parse().ok())
                .unwrap_or(4),
            color: element_attr(xml, "w:color").unwrap_or_else(|| "auto".into()),
        }
    }
    /// The element for `edge`.
    pub fn to_xml(&self, edge: Edge) -> String {
        if self.visible() {
            format!(
                "<{} w:val=\"{}\" w:sz=\"{}\" w:space=\"0\" w:color=\"{}\"/>",
                edge.tag(),
                self.val,
                self.sz,
                self.color
            )
        } else {
            format!("<{} w:val=\"nil\"/>", edge.tag())
        }
    }
}

/// Read one edge of a border group (`w:tcBorders`/`w:tblBorders` element).
pub fn border_of(group: Option<&str>, edge: Edge) -> Option<BorderLine> {
    let g = PropsXml::parse(group?, "w:tcBorders", BORDERS_ORDER);
    g.get(edge.tag())
        .or_else(|| edge.alt_tag().and_then(|t| g.get(t)))
        .map(BorderLine::parse)
}

/// Set (`Some`) or remove (`None`) one edge in a border group container,
/// returning the new group (or `None` when it became empty).
pub fn with_border(
    group: Option<&str>,
    group_name: &str,
    edge: Edge,
    line: Option<&BorderLine>,
) -> Option<String> {
    let mut g = PropsXml::parse_opt(group, group_name, BORDERS_ORDER);
    // A strict-named synonym is replaced by the transitional name.
    if let Some(alt) = edge.alt_tag() {
        g.remove(alt);
    }
    match line {
        Some(l) => g.set(&l.to_xml(edge)),
        None => {
            g.remove(edge.tag());
        }
    }
    (!g.is_empty()).then(|| g.to_xml())
}

/// `w:tblLook`: which conditional parts of a table style apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TblLook {
    pub first_row: bool,
    pub last_row: bool,
    pub first_col: bool,
    pub last_col: bool,
    pub banded_rows: bool,
    pub banded_cols: bool,
}

impl Default for TblLook {
    /// Word's default for a new table: header row, first column, banded rows.
    fn default() -> Self {
        TblLook {
            first_row: true,
            last_row: false,
            first_col: true,
            last_col: false,
            banded_rows: true,
            banded_cols: false,
        }
    }
}

impl TblLook {
    /// The legacy `w:val` bit mask.
    pub fn bits(self) -> u32 {
        let mut v = 0;
        if self.first_row {
            v |= 0x0020;
        }
        if self.last_row {
            v |= 0x0040;
        }
        if self.first_col {
            v |= 0x0080;
        }
        if self.last_col {
            v |= 0x0100;
        }
        if !self.banded_rows {
            v |= 0x0200;
        }
        if !self.banded_cols {
            v |= 0x0400;
        }
        v
    }

    /// Parse a `w:tblLook` element. The attribute form wins over `w:val`.
    pub fn parse(xml: &str) -> Self {
        let bits = element_attr(xml, "w:val")
            .and_then(|v| u32::from_str_radix(&v, 16).ok())
            .unwrap_or(0x04A0);
        let flag = |attr: &str, bit: u32, inverted: bool| -> bool {
            match element_attr(xml, attr).as_deref() {
                Some("1") | Some("true") | Some("on") => !inverted,
                Some("0") | Some("false") | Some("off") => inverted,
                _ => (bits & bit != 0) != inverted,
            }
        };
        TblLook {
            first_row: flag("w:firstRow", 0x0020, false),
            last_row: flag("w:lastRow", 0x0040, false),
            first_col: flag("w:firstColumn", 0x0080, false),
            last_col: flag("w:lastColumn", 0x0100, false),
            banded_rows: flag("w:noHBand", 0x0200, true),
            banded_cols: flag("w:noVBand", 0x0400, true),
        }
    }

    /// The `w:tblLook` element with both the hex mask and the attributes.
    pub fn to_xml(self) -> String {
        let b = |v: bool| if v { "1" } else { "0" };
        format!(
            "<w:tblLook w:val=\"{:04X}\" w:firstRow=\"{}\" w:lastRow=\"{}\" \
             w:firstColumn=\"{}\" w:lastColumn=\"{}\" w:noHBand=\"{}\" w:noVBand=\"{}\"/>",
            self.bits(),
            b(self.first_row),
            b(self.last_row),
            b(self.first_col),
            b(self.last_col),
            b(!self.banded_rows),
            b(!self.banded_cols)
        )
    }
}

/// A cell fill (`w:shd w:fill`) as `RRGGBB`, `None` for no fill (`auto`) or no
/// `w:shd` at all. The second value says whether a `w:shd` was present.
pub fn shd_fill(shd: Option<&str>) -> (Option<String>, bool) {
    let Some(shd) = shd else {
        return (None, false);
    };
    let fill = element_attr(shd, "w:fill").filter(|f| !f.is_empty() && f != "auto");
    (fill, true)
}

/// A width element (`w:tcW`, `w:tblW`, …): `(w, type)`.
pub fn width_of(el: Option<&str>) -> Option<(i64, String)> {
    let el = el?;
    let w = element_attr(el, "w:w")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let t = element_attr(el, "w:type").unwrap_or_else(|| "dxa".into());
    Some((w, t))
}

/// A row's `w:gridBefore`/`w:gridAfter` from its raw `trPr`.
pub fn row_grid_skips(raw_props: &[String]) -> (usize, usize) {
    let Some(trpr) = raw_props.iter().find(|r| element_name(r) == "w:trPr") else {
        return (0, 0);
    };
    let t = PropsXml::parse(trpr, "w:trPr", TRPR_ORDER);
    let n = |name| {
        t.attr(name, "w:val")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
    };
    (n("w:gridBefore"), n("w:gridAfter"))
}

/// The row's `trPr`, parsed (empty when absent).
pub fn row_trpr(raw_props: &[String]) -> PropsXml {
    PropsXml::parse_opt(
        raw_props
            .iter()
            .find(|r| element_name(r) == "w:trPr")
            .map(String::as_str),
        "w:trPr",
        TRPR_ORDER,
    )
}

/// Store `trpr` back into a row's raw properties (removed when empty), keeping
/// `w:tblPrEx` and other entries where they were. `trPr` follows `tblPrEx`.
pub fn set_row_trpr(raw_props: &mut Vec<String>, trpr: &PropsXml) {
    let at = raw_props.iter().position(|r| element_name(r) == "w:trPr");
    match (at, trpr.is_empty()) {
        (Some(i), true) => {
            raw_props.remove(i);
        }
        (Some(i), false) => raw_props[i] = trpr.to_xml(),
        (None, true) => {}
        (None, false) => raw_props.push(trpr.to_xml()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_inserts_in_schema_order_and_keeps_unknown_children() {
        let mut t = PropsXml::parse(
            "<w:tcPr><w:tcW w:w=\"100\" w:type=\"dxa\"/><w:foo x=\"1\"><w:bar/></w:foo><w:vAlign w:val=\"center\"/></w:tcPr>",
            "w:tcPr",
            TCPR_ORDER,
        );
        t.set("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"FF0000\"/>");
        t.set("<w:gridSpan w:val=\"2\"/>");
        assert_eq!(
            t.child_names(),
            // A new child goes before the first known child ranked after it;
            // an unknown child keeps its place.
            ["w:tcW", "w:foo", "w:gridSpan", "w:shd", "w:vAlign"]
        );
        assert!(t.to_xml().contains("<w:foo x=\"1\"><w:bar/></w:foo>"));
        t.set("<w:vAlign w:val=\"bottom\"/>");
        assert_eq!(t.attr("w:vAlign", "w:val").as_deref(), Some("bottom"));
        assert!(t.remove("w:tcW"));
        assert!(!t.remove("w:tcW"));
        assert!(t.to_xml().starts_with("<w:tcPr><w:foo"));
    }

    #[test]
    fn a_change_record_stays_last() {
        let mut t = PropsXml::parse(
            "<w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblPrChange w:id=\"1\"><w:tblPr/></w:tblPrChange></w:tblPr>",
            "w:tblPr",
            TBLPR_ORDER,
        );
        t.set(&TblLook::default().to_xml());
        t.set("<w:tblStyle w:val=\"TableGrid\"/>");
        assert_eq!(
            t.child_names(),
            ["w:tblStyle", "w:tblW", "w:tblLook", "w:tblPrChange"]
        );
    }

    #[test]
    fn empty_and_self_closing_containers() {
        let t = PropsXml::parse("<w:tcPr/>", "w:tcPr", TCPR_ORDER);
        assert!(t.is_empty());
        assert_eq!(t.to_xml(), "<w:tcPr/>");
        let mut t = PropsXml::new("w:tcPr", TCPR_ORDER);
        t.set("<w:vAlign w:val=\"center\"/>");
        assert_eq!(t.to_xml(), "<w:tcPr><w:vAlign w:val=\"center\"/></w:tcPr>");
    }

    #[test]
    fn tbl_look_bits_and_attributes() {
        let look = TblLook::default();
        assert_eq!(look.bits(), 0x04A0);
        let xml = look.to_xml();
        assert!(xml.contains("w:val=\"04A0\""));
        assert!(xml.contains("w:noVBand=\"1\""));
        assert_eq!(TblLook::parse(&xml), look);
        // Only the legacy mask.
        let l = TblLook::parse("<w:tblLook w:val=\"0600\"/>");
        assert!(!l.first_row && !l.banded_rows && !l.banded_cols);
        // Attributes win over the mask.
        let l = TblLook::parse("<w:tblLook w:val=\"0000\" w:firstRow=\"1\" w:noHBand=\"1\"/>");
        assert!(l.first_row && !l.banded_rows && l.banded_cols);
        let all = TblLook {
            first_row: true,
            last_row: true,
            first_col: true,
            last_col: true,
            banded_rows: false,
            banded_cols: false,
        };
        assert_eq!(
            all.bits(),
            0x0020 | 0x0040 | 0x0080 | 0x0100 | 0x0200 | 0x0400
        );
    }

    #[test]
    fn borders_read_strict_synonyms_and_write_transitional_names() {
        let g =
            "<w:tcBorders><w:start w:val=\"single\" w:sz=\"8\" w:color=\"FF0000\"/></w:tcBorders>";
        let left = border_of(Some(g), Edge::Left).unwrap();
        assert_eq!((left.val.as_str(), left.sz), ("single", 8));
        let g2 = with_border(Some(g), "w:tcBorders", Edge::Left, Some(&BorderLine::nil())).unwrap();
        assert_eq!(g2, "<w:tcBorders><w:left w:val=\"nil\"/></w:tcBorders>");
        let g3 = with_border(
            Some(&g2),
            "w:tcBorders",
            Edge::DiagDown,
            Some(&BorderLine::single()),
        )
        .unwrap();
        assert!(g3.ends_with(
            "<w:tl2br w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/></w:tcBorders>"
        ));
        assert_eq!(
            with_border(Some(&g2), "w:tcBorders", Edge::Left, None),
            None
        );
    }

    #[test]
    fn grid_skips_from_trpr() {
        let raw = vec![
            "<w:tblPrEx><w:tblW w:w=\"0\"/></w:tblPrEx>".to_string(),
            "<w:trPr><w:gridBefore w:val=\"1\"/><w:gridAfter w:val=\"2\"/></w:trPr>".to_string(),
        ];
        assert_eq!(row_grid_skips(&raw), (1, 2));
        assert_eq!(row_grid_skips(&[]), (0, 0));
    }
}
