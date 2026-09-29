//! A typed view of one section's properties (`w:sectPr`), for the page Layout
//! commands (#649).
//!
//! Everything else in the crate keeps a sectPr as raw XML, so this is a
//! *partial* view: [`SectionSetup::parse`] reads the elements it owns (`w:type`,
//! `w:pgSz`, `w:pgMar`, `w:lnNumType`, `w:cols`) and [`SectionSetup::apply`]
//! writes back only the ones whose value changed, leaving every other child
//! (header references, `w:docGrid`, `w:pgNumType`, …) byte for byte. A new
//! child is placed by [`insert_ordered`] in `CT_SectPr` schema order, which
//! Word enforces.

/// `CT_SectPr` children in schema order (ECMA-376 §17.6.17).
const SECT_ORDER: &[&str] = &[
    "w:headerReference",
    "w:footerReference",
    "w:footnotePr",
    "w:endnotePr",
    "w:type",
    "w:pgSz",
    "w:pgMar",
    "w:paperSrc",
    "w:pgBorders",
    "w:lnNumType",
    "w:pgNumType",
    "w:cols",
    "w:formProt",
    "w:vAlign",
    "w:noEndnote",
    "w:titlePg",
    "w:textDirection",
    "w:bidi",
    "w:rtlGutter",
    "w:docGrid",
    "w:printerSettings",
    "w:sectPrChange",
];

/// Twips per inch.
pub const TWIPS_PER_INCH: i32 = 1440;

/// How a section starts relative to the one before it (`w:type`). The default,
/// when the element is absent, is a new page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SectionStart {
    #[default]
    NextPage,
    Continuous,
    EvenPage,
    OddPage,
    NextColumn,
}

impl SectionStart {
    pub fn val(self) -> &'static str {
        match self {
            Self::NextPage => "nextPage",
            Self::Continuous => "continuous",
            Self::EvenPage => "evenPage",
            Self::OddPage => "oddPage",
            Self::NextColumn => "nextColumn",
        }
    }
    fn from_val(v: &str) -> Self {
        match v {
            "continuous" => Self::Continuous,
            "evenPage" => Self::EvenPage,
            "oddPage" => Self::OddPage,
            "nextColumn" => Self::NextColumn,
            _ => Self::NextPage,
        }
    }
}

/// A named paper size: the Size menu's list, portrait dimensions in twips and
/// the printer paper code Word writes as `w:pgSz w:code` (DEVMODE `DMPAPER_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paper {
    Letter,
    Legal,
    Executive,
    A4,
    A5,
    B5Jis,
    Tabloid,
}

impl Paper {
    pub const ALL: [Paper; 7] = [
        Paper::Letter,
        Paper::Legal,
        Paper::Executive,
        Paper::A4,
        Paper::A5,
        Paper::B5Jis,
        Paper::Tabloid,
    ];
    /// Portrait width and height, in twips.
    pub fn size(self) -> (i32, i32) {
        match self {
            Self::Letter => (12240, 15840),
            Self::Legal => (12240, 20160),
            Self::Executive => (10440, 15120),
            Self::A4 => (11906, 16838),
            Self::A5 => (8391, 11906),
            Self::B5Jis => (10319, 14571),
            Self::Tabloid => (15840, 24480),
        }
    }
    pub fn code(self) -> i32 {
        match self {
            Self::Letter => 1,
            Self::Legal => 5,
            Self::Executive => 7,
            Self::A4 => 9,
            Self::A5 => 11,
            Self::B5Jis => 13,
            Self::Tabloid => 3,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Letter => "Letter",
            Self::Legal => "Legal",
            Self::Executive => "Executive",
            Self::A4 => "A4",
            Self::A5 => "A5",
            Self::B5Jis => "B5 (JIS)",
            Self::Tabloid => "Tabloid",
        }
    }
    /// The named size a page matches in either orientation (within 2 twips,
    /// which absorbs Word's rounding of metric sizes).
    pub fn matching(w: i32, h: i32) -> Option<Paper> {
        let (short, long) = (w.min(h), w.max(h));
        Self::ALL.into_iter().find(|p| {
            let (pw, ph) = p.size();
            (pw - short).abs() <= 2 && (ph - long).abs() <= 2
        })
    }
}

/// `w:pgSz`: physical page dimensions in twips, the orientation flag and the
/// printer paper code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSize {
    pub w: i32,
    pub h: i32,
    pub landscape: bool,
    pub code: Option<i32>,
}

impl Default for PageSize {
    fn default() -> Self {
        Self {
            w: 12240,
            h: 15840,
            landscape: false,
            code: None,
        }
    }
}

impl PageSize {
    /// Set a named paper size, kept in the current orientation, with its code.
    pub fn set_paper(&mut self, paper: Paper) {
        let (w, h) = paper.size();
        (self.w, self.h) = if self.landscape { (h, w) } else { (w, h) };
        self.code = Some(paper.code());
    }
    /// Set a custom size: the paper code no longer describes it, so it goes.
    pub fn set_custom(&mut self, w: i32, h: i32) {
        self.w = w;
        self.h = h;
        self.code = Paper::matching(w, h).map(Paper::code);
    }
}

/// `w:pgMar`, in twips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Margins {
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub left: i32,
    pub header: i32,
    pub footer: i32,
    pub gutter: i32,
}

impl Default for Margins {
    fn default() -> Self {
        Self {
            top: 1440,
            right: 1440,
            bottom: 1440,
            left: 1440,
            header: 720,
            footer: 720,
            gutter: 0,
        }
    }
}

/// One column of an unequal-width layout (`w:col`): its width and the space
/// after it, in twips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    pub w: i32,
    pub space: i32,
}

/// `w:cols`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Columns {
    pub num: i32,
    pub space: i32,
    /// A line between columns (`w:sep`).
    pub sep: bool,
    /// Explicit column widths; empty for equal columns.
    pub cols: Vec<Column>,
}

impl Default for Columns {
    fn default() -> Self {
        Self {
            num: 1,
            space: 720,
            sep: false,
            cols: Vec::new(),
        }
    }
}

impl Columns {
    /// `num` equal columns with the given gap, keeping the line between.
    pub fn equal(&self, num: i32, space: i32) -> Columns {
        Columns {
            num: num.max(1),
            space,
            sep: self.sep,
            cols: Vec::new(),
        }
    }
    /// The number of columns this layout draws.
    pub fn count(&self) -> i32 {
        if self.cols.is_empty() {
            self.num.max(1)
        } else {
            self.cols.len() as i32
        }
    }
    /// Whether the columns share one width.
    pub fn equal_width(&self) -> bool {
        self.cols.is_empty()
    }
    /// Word's Left (`narrow_first`) or Right preset for a text width `text_w`:
    /// two columns 720 twips apart, the narrow one `(W - 2s) / 3` wide (one of
    /// three equal columns) and the wide one taking the rest.
    pub fn two_unequal(&self, text_w: i32, narrow_first: bool) -> Columns {
        let s = 720;
        let narrow = (text_w - 2 * s) / 3;
        let wide = text_w - narrow - s;
        let (a, b) = if narrow_first {
            (narrow, wide)
        } else {
            (wide, narrow)
        };
        Columns {
            num: 2,
            space: s,
            sep: self.sep,
            cols: vec![Column { w: a, space: s }, Column { w: b, space: 0 }],
        }
    }
}

/// Where line numbering restarts (`w:lnNumType w:restart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LnRestart {
    #[default]
    NewPage,
    NewSection,
    Continuous,
}

impl LnRestart {
    pub fn val(self) -> &'static str {
        match self {
            Self::NewPage => "newPage",
            Self::NewSection => "newSection",
            Self::Continuous => "continuous",
        }
    }
}

/// `w:lnNumType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineNumbering {
    pub count_by: i32,
    pub start: Option<i32>,
    pub distance: Option<i32>,
    pub restart: LnRestart,
}

/// The section properties the Layout tab edits. Absent elements parse as
/// Word's defaults.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SectionSetup {
    pub start: SectionStart,
    pub page: PageSize,
    pub margins: Margins,
    pub columns: Columns,
    pub line_numbers: Option<LineNumbering>,
}

impl SectionSetup {
    pub fn parse(sect: &str) -> SectionSetup {
        let head = own_children(sect);
        let mut s = SectionSetup::default();
        if let Some(t) = start_tag(head, "w:type") {
            s.start = SectionStart::from_val(&attr(t, "w:val").unwrap_or_default());
        }
        if let Some(t) = start_tag(head, "w:pgSz") {
            let d = PageSize::default();
            s.page = PageSize {
                w: num(t, "w:w").unwrap_or(d.w),
                h: num(t, "w:h").unwrap_or(d.h),
                landscape: attr(t, "w:orient").as_deref() == Some("landscape"),
                code: num(t, "w:code"),
            };
        }
        if let Some(t) = start_tag(head, "w:pgMar") {
            let d = Margins::default();
            s.margins = Margins {
                top: num(t, "w:top").unwrap_or(d.top),
                right: num(t, "w:right").unwrap_or(d.right),
                bottom: num(t, "w:bottom").unwrap_or(d.bottom),
                left: num(t, "w:left").unwrap_or(d.left),
                header: num(t, "w:header").unwrap_or(d.header),
                footer: num(t, "w:footer").unwrap_or(d.footer),
                gutter: num(t, "w:gutter").unwrap_or(d.gutter),
            };
        }
        if let Some(t) = start_tag(head, "w:lnNumType") {
            s.line_numbers = Some(LineNumbering {
                count_by: num(t, "w:countBy").unwrap_or(1),
                start: num(t, "w:start"),
                distance: num(t, "w:distance"),
                restart: match attr(t, "w:restart").as_deref() {
                    Some("newSection") => LnRestart::NewSection,
                    Some("continuous") => LnRestart::Continuous,
                    _ => LnRestart::NewPage,
                },
            });
        }
        if let Some((a, b)) = find_element(head, "w:cols") {
            let el = &head[a..b];
            let t = start_tag(el, "w:cols").unwrap_or_default();
            let mut cols = Vec::new();
            let mut from = 0;
            while let Some((ca, cb)) = find_element(&el[from..], "w:col") {
                let ct = start_tag(&el[from + ca..from + cb], "w:col").unwrap_or_default();
                cols.push(Column {
                    w: num(ct, "w:w").unwrap_or(0),
                    space: num(ct, "w:space").unwrap_or(0),
                });
                from += cb;
            }
            let equal = attr(t, "w:equalWidth").is_none_or(|v| on(&v));
            s.columns = Columns {
                num: num(t, "w:num").unwrap_or(1).max(1),
                space: num(t, "w:space").unwrap_or(720),
                sep: attr(t, "w:sep").is_some_and(|v| on(&v)),
                cols: if equal { Vec::new() } else { cols },
            };
        }
        s
    }

    /// The page's text width: the page width less the side margins, and the
    /// gutter unless it sits at the top (settings `w:gutterAtTop`).
    pub fn text_width(&self, gutter_at_top: bool) -> i32 {
        let gutter = if gutter_at_top {
            0
        } else {
            self.margins.gutter
        };
        self.page.w - self.margins.left - self.margins.right - gutter
    }

    /// The page's text height: the page height less the top and bottom
    /// margins, and the gutter when it sits at the top.
    pub fn text_height(&self, gutter_at_top: bool) -> i32 {
        let gutter = if gutter_at_top {
            self.margins.gutter
        } else {
            0
        };
        self.page.h - self.margins.top.abs() - self.margins.bottom.abs() - gutter
    }

    /// Switch orientation: swap the page sides and rotate the margins. Choosing
    /// the current orientation changes nothing.
    ///
    /// The rotation is a stated decision, not checked against Word: Portrait
    /// to Landscape turns the page a quarter turn so that the old left margin
    /// becomes the top (new top = old left, new right = old top, new bottom =
    /// old right, new left = old bottom), and Landscape to Portrait undoes it.
    /// The gutter and header/footer distances stay.
    pub fn set_landscape(&mut self, landscape: bool) {
        if self.page.landscape == landscape {
            return;
        }
        self.page.landscape = landscape;
        if (self.page.w > self.page.h) != landscape {
            (self.page.w, self.page.h) = (self.page.h, self.page.w);
        }
        let m = self.margins;
        (
            self.margins.top,
            self.margins.right,
            self.margins.bottom,
            self.margins.left,
        ) = if landscape {
            (m.left, m.top, m.right, m.bottom)
        } else {
            (m.right, m.bottom, m.left, m.top)
        };
    }

    /// Write this setup into `sect`, touching only the elements whose value
    /// differs from what `sect` holds, so an untouched section stays byte for
    /// byte.
    pub fn apply(&self, sect: &str) -> String {
        let old = SectionSetup::parse(sect);
        let mut s = if sect.trim().is_empty() {
            "<w:sectPr></w:sectPr>".to_string()
        } else {
            sect.to_string()
        };
        if self.start != old.start {
            s = remove_element(&s, "w:type");
            if self.start != SectionStart::NextPage {
                let el = format!("<w:type w:val=\"{}\"/>", self.start.val());
                s = insert_ordered(&s, "w:type", &el);
            }
        }
        if self.page != old.page {
            let p = self.page;
            let orient = p.landscape.then_some("landscape".to_string());
            let code = p.code.map(|c| c.to_string());
            s = edit_attrs(
                &s,
                "w:pgSz",
                &[
                    ("w:w", Some(p.w.to_string())),
                    ("w:h", Some(p.h.to_string())),
                    ("w:orient", orient),
                    ("w:code", code),
                ],
            );
        }
        if self.margins != old.margins {
            let m = self.margins;
            s = edit_attrs(
                &s,
                "w:pgMar",
                &[
                    ("w:top", Some(m.top.to_string())),
                    ("w:right", Some(m.right.to_string())),
                    ("w:bottom", Some(m.bottom.to_string())),
                    ("w:left", Some(m.left.to_string())),
                    ("w:header", Some(m.header.to_string())),
                    ("w:footer", Some(m.footer.to_string())),
                    ("w:gutter", Some(m.gutter.to_string())),
                ],
            );
        }
        if self.line_numbers != old.line_numbers {
            s = remove_element(&s, "w:lnNumType");
            if let Some(ln) = self.line_numbers {
                let mut el = format!("<w:lnNumType w:countBy=\"{}\"", ln.count_by);
                if let Some(v) = ln.start {
                    el.push_str(&format!(" w:start=\"{v}\""));
                }
                if let Some(v) = ln.distance {
                    el.push_str(&format!(" w:distance=\"{v}\""));
                }
                el.push_str(&format!(" w:restart=\"{}\"/>", ln.restart.val()));
                s = insert_ordered(&s, "w:lnNumType", &el);
            }
        }
        if self.columns != old.columns {
            s = remove_element(&s, "w:cols");
            s = insert_ordered(&s, "w:cols", &cols_xml(&self.columns));
        }
        s
    }
}

fn cols_xml(c: &Columns) -> String {
    let sep = if c.sep { " w:sep=\"1\"" } else { "" };
    if c.cols.is_empty() {
        if c.num <= 1 {
            return format!("<w:cols w:space=\"{}\"{sep}/>", c.space);
        }
        return format!("<w:cols w:num=\"{}\" w:space=\"{}\"{sep}/>", c.num, c.space);
    }
    let mut s = format!(
        "<w:cols w:num=\"{}\" w:space=\"{}\"{sep} w:equalWidth=\"0\">",
        c.cols.len(),
        c.space
    );
    for col in &c.cols {
        s.push_str(&format!(
            "<w:col w:w=\"{}\" w:space=\"{}\"/>",
            col.w, col.space
        ));
    }
    s.push_str("</w:cols>");
    s
}

fn on(v: &str) -> bool {
    !matches!(v, "0" | "false" | "off")
}

/// The part of `sect` that holds its own children: a nested `w:sectPrChange`
/// carries a whole previous sectPr, whose elements must not be read or edited
/// as the current ones.
fn own_children(sect: &str) -> &str {
    match find_element(sect, "w:sectPrChange") {
        Some((a, _)) => &sect[..a],
        None => sect,
    }
}

/// The byte range of the first `<name …/>` or `<name …>…</name>` element in
/// `xml`, skipping longer names that share the prefix (`w:cols` for `w:col`).
pub(crate) fn find_element(xml: &str, name: &str) -> Option<(usize, usize)> {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(rel) = xml[from..].find(&open) {
        let start = from + rel;
        let after = start + open.len();
        if !xml[after..].starts_with([' ', '/', '>', '\t', '\n', '\r']) {
            from = after;
            continue;
        }
        let gt = after + xml[after..].find('>')?;
        let end = if xml[..gt].ends_with('/') {
            gt + 1
        } else {
            let close = format!("</{name}>");
            match xml[gt..].find(&close) {
                Some(c) => gt + c + close.len(),
                None => gt + 1,
            }
        };
        return Some((start, end));
    }
    None
}

/// Remove the first `name` element (see [`find_element`]).
pub(crate) fn remove_element(xml: &str, name: &str) -> String {
    match find_element(xml, name) {
        Some((a, b)) => format!("{}{}", &xml[..a], &xml[b..]),
        None => xml.to_string(),
    }
}

/// The start tag (without `<` and `>`) of the first `name` element.
fn start_tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let (a, _) = find_element(xml, name)?;
    let gt = a + xml[a..].find('>')?;
    Some(xml[a + 1..gt].trim_end_matches('/'))
}

/// An attribute's value in a start tag, either quote style.
fn attr(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=");
    let mut from = 0;
    while let Some(rel) = tag[from..].find(&pat) {
        let at = from + rel;
        let bounded = tag[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace);
        let rest = &tag[at + pat.len()..];
        if let (true, Some(q @ ('"' | '\''))) = (bounded, rest.chars().next()) {
            let body = &rest[1..];
            return body.find(q).map(|e| body[..e].to_string());
        }
        from = at + pat.len();
    }
    None
}

fn num(tag: &str, name: &str) -> Option<i32> {
    attr(tag, name)?.trim().parse().ok()
}

/// Set (`Some`) or remove (`None`) attributes on the first `name` element,
/// keeping its other attributes. Creates the element in schema order when it
/// is absent.
fn edit_attrs(sect: &str, name: &str, attrs: &[(&str, Option<String>)]) -> String {
    let Some((a, _)) = find_element(own_children(sect), name) else {
        let mut el = format!("<{name}");
        for (k, v) in attrs {
            if let Some(v) = v {
                el.push_str(&format!(" {k}=\"{v}\""));
            }
        }
        el.push_str("/>");
        return insert_ordered(sect, name, &el);
    };
    let gt = a + sect[a..].find('>').unwrap_or(0);
    let self_closing = sect[..gt].ends_with('/');
    let mut tag = sect[a + 1..if self_closing { gt - 1 } else { gt }]
        .trim_end()
        .to_string();
    for (k, v) in attrs {
        tag = remove_attr(&tag, k);
        if let Some(v) = v {
            tag.push_str(&format!(" {k}=\"{v}\""));
        }
    }
    let close = if self_closing { "/>" } else { ">" };
    format!("{}<{tag}{close}{}", &sect[..a], &sect[gt + 1..])
}

fn remove_attr(tag: &str, name: &str) -> String {
    let pat = format!(" {name}=");
    let Some(at) = tag.find(&pat) else {
        return tag.to_string();
    };
    let rest = &tag[at + pat.len()..];
    let Some(q @ ('"' | '\'')) = rest.chars().next() else {
        return tag.to_string();
    };
    let end = rest[1..].find(q).map_or(rest.len(), |e| e + 2);
    format!("{}{}", &tag[..at], &rest[end..])
}

/// Insert `child` (a `name` element) into `sect` at its `CT_SectPr` schema
/// position: before the first existing child that comes after it, else at the
/// end. Expands a self-closing `<w:sectPr/>`.
pub fn insert_ordered(sect: &str, name: &str, child: &str) -> String {
    let sect = if sect.trim().is_empty() {
        "<w:sectPr></w:sectPr>"
    } else {
        sect
    };
    let Some(gt) = sect.find('>') else {
        return sect.to_string();
    };
    if sect[..gt].ends_with('/') {
        return format!("{}>{child}</w:sectPr>", &sect[..gt - 1]);
    }
    let rank = SECT_ORDER.iter().position(|n| *n == name);
    let own = own_children(sect);
    let before = rank.and_then(|r| {
        SECT_ORDER[r + 1..]
            .iter()
            .filter_map(|later| find_element(&own[gt..], later).map(|(a, _)| gt + a))
            .min()
    });
    // With no later sibling, the child goes last, but still ahead of a
    // tracked `w:sectPrChange`, which the schema puts at the very end.
    let at = before.unwrap_or_else(|| {
        if own.len() < sect.len() {
            own.len()
        } else {
            sect.rfind("</w:sectPr>").unwrap_or(sect.len())
        }
    });
    format!("{}{child}{}", &sect[..at], &sect[at..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(sect: &str) -> Vec<&'static str> {
        let mut found: Vec<(usize, &'static str)> = SECT_ORDER
            .iter()
            .filter_map(|n| find_element(sect, n).map(|(a, _)| (a, *n)))
            .collect();
        found.sort();
        found.into_iter().map(|(_, n)| n).collect()
    }

    fn in_schema_order(sect: &str) -> bool {
        let names = order(sect);
        let ranks: Vec<usize> = names
            .iter()
            .map(|n| SECT_ORDER.iter().position(|m| m == n).unwrap())
            .collect();
        ranks.windows(2).all(|w| w[0] < w[1])
    }

    #[test]
    fn find_element_skips_longer_names() {
        let x = "<w:cols w:num=\"2\"><w:col w:w=\"100\"/></w:cols>";
        let (a, b) = find_element(x, "w:col").unwrap();
        assert_eq!(&x[a..b], "<w:col w:w=\"100\"/>");
        assert_eq!(remove_element(x, "w:col"), "<w:cols w:num=\"2\"></w:cols>");
    }

    #[test]
    fn round_trip_keeps_unknown_children_in_order() {
        let sect = "<w:sectPr w:rsidR=\"00A1\"><w:headerReference w:type=\"default\" r:id=\"rId7\"/>\
            <w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" \
            w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/>\
            <w:pgNumType w:start=\"3\"/><w:docGrid w:linePitch=\"360\"/></w:sectPr>";
        let s = SectionSetup::parse(sect);
        assert_eq!(s.apply(sect), sect, "unchanged setup rewrites nothing");
        let mut t = s.clone();
        t.margins.left = 720;
        let out = t.apply(sect);
        assert!(out.contains("w:left=\"720\""), "{out}");
        assert!(out.contains("<w:pgNumType w:start=\"3\"/>"));
        assert!(out.contains("r:id=\"rId7\""));
        assert!(out.starts_with("<w:sectPr w:rsidR=\"00A1\">"));
        assert_eq!(SectionSetup::parse(&out), t);
    }

    #[test]
    fn new_children_land_in_schema_order() {
        let sect = "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/>\
            <w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\"/>\
            <w:pgBorders><w:top w:val=\"single\"/></w:pgBorders>\
            <w:pgNumType w:fmt=\"lowerRoman\"/><w:docGrid w:linePitch=\"360\"/></w:sectPr>";
        let mut s = SectionSetup::parse(sect);
        s.columns = s.columns.equal(2, 720);
        s.line_numbers = Some(LineNumbering {
            count_by: 1,
            start: None,
            distance: None,
            restart: LnRestart::Continuous,
        });
        s.start = SectionStart::Continuous;
        let out = s.apply(sect);
        assert_eq!(
            order(&out),
            [
                "w:type",
                "w:pgSz",
                "w:pgMar",
                "w:pgBorders",
                "w:lnNumType",
                "w:pgNumType",
                "w:cols",
                "w:docGrid"
            ],
            "{out}"
        );
        assert!(in_schema_order(&out));
        assert_eq!(SectionSetup::parse(&out), s);
    }

    #[test]
    fn empty_and_self_closing_sections_take_children() {
        let mut s = SectionSetup::parse("<w:sectPr/>");
        assert_eq!(s, SectionSetup::default());
        s.page.set_paper(Paper::A4);
        s.margins.top = 720;
        let out = s.apply("<w:sectPr/>");
        assert!(in_schema_order(&out), "{out}");
        assert_eq!(SectionSetup::parse(&out), s);
        assert!(out.ends_with("</w:sectPr>"));
        let out = s.apply("");
        assert_eq!(SectionSetup::parse(&out), s);
    }

    #[test]
    fn orientation_rotates_margins() {
        let sect = "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\" w:code=\"1\"/>\
            <w:pgMar w:top=\"100\" w:right=\"200\" w:bottom=\"300\" w:left=\"400\" \
            w:header=\"50\" w:footer=\"60\" w:gutter=\"70\"/></w:sectPr>";
        let mut s = SectionSetup::parse(sect);
        s.set_landscape(true);
        assert_eq!((s.page.w, s.page.h), (15840, 12240));
        let m = s.margins;
        assert_eq!((m.top, m.right, m.bottom, m.left), (400, 100, 200, 300));
        assert_eq!((m.header, m.footer, m.gutter), (50, 60, 70));
        let out = s.apply(sect);
        assert!(out.contains("w:orient=\"landscape\""), "{out}");
        assert!(out.contains("w:code=\"1\""), "the paper is still Letter");
        let before = s.clone();
        s.set_landscape(true);
        assert_eq!(s, before, "choosing the current orientation is a no-op");
        s.set_landscape(false);
        assert_eq!(s, SectionSetup::parse(sect));
        assert!(!s.apply(&out).contains("w:orient"));
    }

    #[test]
    fn paper_code_follows_the_size() {
        let sect = "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\" w:code=\"1\"/></w:sectPr>";
        let mut s = SectionSetup::parse(sect);
        s.page.set_paper(Paper::A4);
        let out = s.apply(sect);
        assert!(out.contains("w:w=\"11906\""), "{out}");
        assert!(out.contains("w:code=\"9\""), "{out}");
        assert!(!out.contains("w:code=\"1\""));
        s.page.set_custom(10000, 14000);
        let out = s.apply(sect);
        assert!(
            !out.contains("w:code"),
            "a custom size drops the code: {out}"
        );
        // In landscape, a named size keeps the orientation.
        s.page.landscape = true;
        s.page.set_paper(Paper::Legal);
        assert_eq!((s.page.w, s.page.h), (20160, 12240));
    }

    #[test]
    fn left_and_right_columns_follow_word() {
        let c = Columns::default();
        // 6.5" of text width: 1.83" / 0.5" / 4.17".
        let left = c.two_unequal(9360, true);
        assert_eq!(left.cols[0].w, (9360 - 1440) / 3);
        assert_eq!(left.cols[0].w, 2640);
        assert_eq!(left.cols[1].w, 9360 - 2640 - 720);
        let right = c.two_unequal(9360, false);
        assert_eq!(right.cols[0].w, left.cols[1].w);
        assert_eq!(right.cols[1].w, left.cols[0].w);
    }

    #[test]
    fn columns_round_trip_sep_and_widths() {
        let sect = "<w:sectPr><w:pgMar w:top=\"1440\"/><w:cols w:num=\"2\" w:sep=\"1\" \
            w:space=\"720\" w:equalWidth=\"0\"><w:col w:w=\"3000\" w:space=\"720\"/>\
            <w:col w:w=\"5640\" w:space=\"0\"/></w:cols></w:sectPr>";
        let s = SectionSetup::parse(sect);
        assert!(s.columns.sep);
        assert_eq!(s.columns.cols.len(), 2);
        assert_eq!(s.columns.cols[1].w, 5640);
        let mut t = s.clone();
        t.columns = s.columns.equal(3, 720);
        let out = t.apply(sect);
        assert!(out.contains("w:sep=\"1\""), "{out}");
        assert!(!out.contains("<w:col "), "{out}");
        assert_eq!(SectionSetup::parse(&out).columns.count(), 3);
        // Back to explicit widths.
        let back = s.apply(&out);
        assert_eq!(SectionSetup::parse(&back), s);
    }

    #[test]
    fn line_numbering_round_trips() {
        let sect =
            "<w:sectPr><w:lnNumType w:countBy=\"5\" w:start=\"2\" w:distance=\"360\"/></w:sectPr>";
        let mut s = SectionSetup::parse(sect);
        let ln = s.line_numbers.unwrap();
        assert_eq!(
            (ln.count_by, ln.start, ln.distance),
            (5, Some(2), Some(360))
        );
        assert_eq!(ln.restart, LnRestart::NewPage);
        s.line_numbers = Some(LineNumbering {
            count_by: 1,
            restart: LnRestart::NewSection,
            ..ln
        });
        let out = s.apply(sect);
        assert_eq!(SectionSetup::parse(&out), s);
        assert!(out.contains("w:restart=\"newSection\""));
        s.line_numbers = None;
        assert!(!s.apply(&out).contains("lnNumType"));
    }

    #[test]
    fn a_tracked_previous_section_is_not_edited() {
        let sect = "<w:sectPr><w:pgMar w:top=\"1440\"/><w:sectPrChange w:id=\"1\">\
            <w:sectPr><w:pgSz w:w=\"1\" w:h=\"2\"/></w:sectPr></w:sectPrChange></w:sectPr>";
        let s = SectionSetup::parse(sect);
        assert_eq!(s.page, PageSize::default());
        let mut t = s.clone();
        t.page.set_paper(Paper::A4);
        let out = t.apply(sect);
        assert!(out.contains("<w:pgSz w:w=\"1\" w:h=\"2\"/>"), "{out}");
        let (a, _) = find_element(&out, "w:pgSz").unwrap();
        assert!(a < out.find("w:sectPrChange").unwrap(), "{out}");
    }
}
