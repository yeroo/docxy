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
    /// Set the sides as typed. The paper code follows them: a named size's
    /// code when the sides match one (in either orientation), else none.
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
            s = remove_own(&s, "w:type");
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
            s = remove_own(&s, "w:lnNumType");
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
            s = remove_own(&s, "w:cols");
            s = insert_ordered(&s, "w:cols", &cols_xml(&self.columns));
        }
        s
    }
}

/// `w:pgNumType`: how a section's page numbers look and where they start
/// (the Page Number Format dialog). `None` fields are absent attributes:
/// decimal, continuing from the previous section, no chapter number.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageNumberFormat {
    /// `w:fmt` (`lowerRoman`, `upperLetter`, `numberInDash`, …).
    pub fmt: Option<String>,
    /// `w:start`: restart numbering at this value.
    pub start: Option<i32>,
    /// `w:chapStyle`: the heading level (1-9) whose number prefixes the page.
    pub chap_style: Option<i32>,
    /// `w:chapSep` (`hyphen`, `period`, `colon`, `emDash`, `enDash`).
    pub chap_sep: Option<String>,
}

impl PageNumberFormat {
    pub fn parse(sect: &str) -> PageNumberFormat {
        let Some(t) = start_tag(own_children(sect), "w:pgNumType") else {
            return PageNumberFormat::default();
        };
        PageNumberFormat {
            fmt: attr(t, "w:fmt").filter(|v| v != "decimal"),
            start: num(t, "w:start"),
            chap_style: num(t, "w:chapStyle"),
            chap_sep: attr(t, "w:chapSep"),
        }
    }

    /// Write this format into `sect`: unchanged when it already holds it, the
    /// element rewritten in schema order otherwise, and removed when every
    /// value is back at its default.
    pub fn apply(&self, sect: &str) -> String {
        if PageNumberFormat::parse(sect) == *self {
            return sect.to_string();
        }
        let s = remove_own(sect, "w:pgNumType");
        if *self == PageNumberFormat::default() {
            return s;
        }
        let mut el = "<w:pgNumType".to_string();
        if let Some(v) = &self.fmt {
            el.push_str(&format!(" w:fmt=\"{v}\""));
        }
        if let Some(v) = self.start {
            el.push_str(&format!(" w:start=\"{v}\""));
        }
        if let Some(v) = self.chap_style {
            el.push_str(&format!(" w:chapStyle=\"{v}\""));
        }
        if let Some(v) = &self.chap_sep {
            el.push_str(&format!(" w:chapSep=\"{v}\""));
        }
        el.push_str("/>");
        insert_ordered(&s, "w:pgNumType", &el)
    }
}

/// Which pages of a section show its page borders (`w:pgBorders/@w:display`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PgBorderDisplay {
    #[default]
    AllPages,
    FirstPage,
    NotFirstPage,
}

impl PgBorderDisplay {
    fn val(self) -> Option<&'static str> {
        match self {
            Self::AllPages => None,
            Self::FirstPage => Some("firstPage"),
            Self::NotFirstPage => Some("notFirstPage"),
        }
    }
}

/// What a page border's `w:space` is measured from (`w:offsetFrom`): the
/// text, Word's schema default, or the edge of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PgBorderOffset {
    #[default]
    Text,
    Page,
}

/// One side of a page border (`w:top`, `w:left`, `w:bottom`, `w:right`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorderSide {
    /// `w:val`: `single`, `double`, `dotted`, … (an Art border's name reads
    /// back here too).
    pub style: String,
    /// `w:sz`, in eighths of a point.
    pub sz: u32,
    /// `w:space`, in points.
    pub space: u32,
    /// `w:color` as RRGGBB; `None` is `auto`.
    pub color: Option<u32>,
    /// `w:shadow`: the Shadow setting.
    pub shadow: bool,
    /// `w:frame`: the 3-D setting.
    pub frame: bool,
}

/// A section's page borders (`w:pgBorders`, the Page Border tab of Borders
/// and Shading). `sides` is top, left, bottom, right: `CT_PageBorders` order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageBorders {
    pub sides: [Option<BorderSide>; 4],
    pub display: PgBorderDisplay,
    pub offset_from: PgBorderOffset,
    /// `w:zOrder="back"`: drawn behind the text.
    pub z_order_back: bool,
}

const PG_BORDER_SIDES: [&str; 4] = ["w:top", "w:left", "w:bottom", "w:right"];

impl PageBorders {
    /// The section's page borders; `None` when it has no `w:pgBorders` or the
    /// element names no side.
    pub fn parse(sect: &str) -> Option<PageBorders> {
        let own = own_children(sect);
        let (a, b) = find_element(own, "w:pgBorders")?;
        let el = &own[a..b];
        let tag = start_tag(el, "w:pgBorders")?;
        let inner = &el[tag.len() + 2..];
        let mut pb = PageBorders {
            display: match attr(tag, "w:display").as_deref() {
                Some("firstPage") => PgBorderDisplay::FirstPage,
                Some("notFirstPage") => PgBorderDisplay::NotFirstPage,
                _ => PgBorderDisplay::AllPages,
            },
            offset_from: match attr(tag, "w:offsetFrom").as_deref() {
                Some("page") => PgBorderOffset::Page,
                _ => PgBorderOffset::Text,
            },
            z_order_back: attr(tag, "w:zOrder").as_deref() == Some("back"),
            ..PageBorders::default()
        };
        for (slot, name) in pb.sides.iter_mut().zip(PG_BORDER_SIDES) {
            let Some(t) = start_tag(inner, name) else {
                continue;
            };
            let style = attr(t, "w:val").unwrap_or_default();
            if matches!(style.as_str(), "" | "nil" | "none") {
                continue;
            }
            *slot = Some(BorderSide {
                style,
                sz: num(t, "w:sz").unwrap_or(4).max(0) as u32,
                space: num(t, "w:space").unwrap_or(0).max(0) as u32,
                color: attr(t, "w:color").and_then(|c| u32::from_str_radix(&c, 16).ok()),
                shadow: attr(t, "w:shadow").is_some_and(|v| on(&v)),
                frame: attr(t, "w:frame").is_some_and(|v| on(&v)),
            });
        }
        pb.sides.iter().any(Option::is_some).then_some(pb)
    }

    /// Write `pb` into `sect` in schema order, or remove `w:pgBorders` with
    /// `None` (Setting: None) or when no side is set. Unchanged when the
    /// section already holds it.
    pub fn apply(pb: Option<&PageBorders>, sect: &str) -> String {
        let pb = pb.filter(|p| p.sides.iter().any(Option::is_some));
        if PageBorders::parse(sect).as_ref() == pb {
            return sect.to_string();
        }
        let s = remove_own(sect, "w:pgBorders");
        let Some(pb) = pb else {
            return s;
        };
        let mut el = "<w:pgBorders".to_string();
        if let Some(v) = pb.display.val() {
            el.push_str(&format!(" w:display=\"{v}\""));
        }
        if pb.offset_from == PgBorderOffset::Page {
            el.push_str(" w:offsetFrom=\"page\"");
        }
        if pb.z_order_back {
            el.push_str(" w:zOrder=\"back\"");
        }
        el.push('>');
        for (side, name) in pb.sides.iter().zip(PG_BORDER_SIDES) {
            let Some(side) = side else {
                continue;
            };
            let color = side
                .color
                .map_or_else(|| "auto".to_string(), |c| format!("{c:06X}"));
            el.push_str(&format!(
                "<{name} w:val=\"{}\" w:sz=\"{}\" w:space=\"{}\" w:color=\"{color}\"",
                side.style, side.sz, side.space
            ));
            if side.shadow {
                el.push_str(" w:shadow=\"1\"");
            }
            if side.frame {
                el.push_str(" w:frame=\"1\"");
            }
            el.push_str("/>");
        }
        el.push_str("</w:pgBorders>");
        insert_ordered(&s, "w:pgBorders", &el)
    }
}

/// The relationship id of the section's own header (`is_header`) or footer
/// reference of `variant` (`default`, `first`, `even`; a reference with no
/// `w:type` is the default one). `None` when the section links to the
/// previous one for that variant.
pub fn hf_reference(sect: &str, is_header: bool, variant: &str) -> Option<String> {
    hf_reference_span(sect, is_header, variant).map(|(_, _, rid)| rid)
}

fn hf_reference_span(sect: &str, is_header: bool, variant: &str) -> Option<(usize, usize, String)> {
    let name = if is_header {
        "w:headerReference"
    } else {
        "w:footerReference"
    };
    let own = own_children(sect);
    let mut from = 0;
    while let Some((a, b)) = find_element(&own[from..], name) {
        let (a, b) = (from + a, from + b);
        let tag = start_tag(&own[a..b], name).unwrap_or_default();
        if attr(tag, "w:type").as_deref().unwrap_or("default") == variant {
            return Some((a, b, attr(tag, "r:id").unwrap_or_default()));
        }
        from = b;
    }
    None
}

/// Point the section's header (`is_header`) or footer reference of `variant`
/// at relationship `rid`, or remove it with `None` (the section then links
/// to the previous one). Other references and children stay byte for byte;
/// a new reference goes in `CT_SectPr` order.
pub fn set_hf_reference(sect: &str, is_header: bool, variant: &str, rid: Option<&str>) -> String {
    let mut s = sect.to_string();
    while let Some((a, b, _)) = hf_reference_span(&s, is_header, variant) {
        s.replace_range(a..b, "");
    }
    let Some(rid) = rid else {
        return s;
    };
    let name = if is_header {
        "w:headerReference"
    } else {
        "w:footerReference"
    };
    let el = format!("<{name} w:type=\"{variant}\" r:id=\"{rid}\"/>");
    insert_ordered(&s, name, &el)
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

/// One attribute in a start tag: `S name S? = S? "value"`, either quote
/// style. The byte ranges of the whole attribute with the whitespace before
/// it, and of its value (without the quotes). Found from byte `from` on.
fn attr_span(tag: &str, name: &str, from: usize) -> Option<(usize, usize, usize, usize)> {
    let mut from = from;
    while let Some(rel) = tag[from..].find(name) {
        let at = from + rel;
        from = at + name.len();
        let ws = tag[..at].trim_end_matches(char::is_whitespace).len();
        if ws == at {
            continue; // not after whitespace: the tail of a longer name
        }
        let after = &tag[at + name.len()..];
        let Some(eq) = after.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = eq.trim_start();
        let Some(q @ ('"' | '\'')) = value.chars().next() else {
            continue;
        };
        let start = tag.len() - value.len() + 1;
        let end = start + tag[start..].find(q)?;
        return Some((ws, start, end, end + 1));
    }
    None
}

/// An attribute's value in a start tag, either quote style, with or without
/// whitespace around its `=`.
fn attr(tag: &str, name: &str) -> Option<String> {
    attr_span(tag, name, 0).map(|(_, a, b, _)| tag[a..b].to_string())
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

/// Remove an attribute from a start tag (without `<` and `>`), with the
/// whitespace before it: the same rule as [`attr`]. Every copy goes, so an
/// edit never leaves a duplicate.
fn remove_attr(tag: &str, name: &str) -> String {
    let mut out = tag.to_string();
    let mut from = 0;
    while let Some((ws, _, _, end)) = attr_span(&out, name, from) {
        out.replace_range(ws..end, "");
        from = ws;
    }
    out
}

/// Remove the first `name` element among the section's own children, never
/// one inside a tracked `w:sectPrChange`.
pub(crate) fn remove_own(sect: &str, name: &str) -> String {
    match find_element(own_children(sect), name) {
        Some((a, b)) => format!("{}{}", &sect[..a], &sect[b..]),
        None => sect.to_string(),
    }
}

/// Whether an on/off child (`w:titlePg`) is on among the section's own
/// children: present, and not `w:val="0"`/`"false"`/`"off"`.
pub fn has_flag(sect: &str, name: &str) -> bool {
    start_tag(own_children(sect), name).is_some_and(|t| attr(t, "w:val").is_none_or(|v| on(&v)))
}

/// Turn an empty on/off child (`w:titlePg`) on or off among the section's
/// own children, placed in schema order.
pub fn set_flag(sect: &str, name: &str, on: bool) -> String {
    let off = remove_own(sect, name);
    if on {
        insert_ordered(&off, name, &format!("<{name}/>"))
    } else {
        off
    }
}

/// Insert `child` (a `name` element) into `sect` at its `CT_SectPr` schema
/// position: before the first existing child that comes after it, else at the
/// end. Expands a self-closing `<w:sectPr/>`.
pub(crate) fn insert_ordered(sect: &str, name: &str, child: &str) -> String {
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
    fn attributes_after_any_whitespace_are_replaced_not_repeated() {
        let sect = "<w:sectPr><w:pgMar\n  w:top=\"1440\"\n\tw:right = '1440'\n  w:bottom=\"1440\"\n  w:left=\"1440\" w:gutter=\"0\"/></w:sectPr>";
        let mut s = SectionSetup::parse(sect);
        s.margins.top = 720;
        s.margins.right = 360;
        let out = s.apply(sect);
        for a in ["w:top", "w:right", "w:bottom", "w:left", "w:gutter"] {
            assert_eq!(out.matches(a).count(), 1, "{a} once: {out}");
        }
        assert_eq!(SectionSetup::parse(&out), s);
        assert!(out.starts_with("<w:sectPr><w:pgMar"), "{out}");
        assert!(
            out.contains("w:top=\"720\"") && out.contains("w:right=\"360\""),
            "{out}"
        );
    }

    #[test]
    fn spaces_around_the_equals_sign_are_read_and_kept() {
        let sect =
            "<w:sectPr><w:pgMar w:top=\"1440\" w:right = \"2000\" w:left =\t'1800'/></w:sectPr>";
        let s = SectionSetup::parse(sect);
        assert_eq!((s.margins.right, s.margins.left), (2000, 1800));
        let mut t = s.clone();
        t.margins.top = 720;
        let out = t.apply(sect);
        assert_eq!(SectionSetup::parse(&out).margins.right, 2000, "{out}");
        assert_eq!(SectionSetup::parse(&out).margins.left, 1800, "{out}");
        assert_eq!(out.matches("w:right").count(), 1, "{out}");
        assert_eq!(
            attr("w:x w:vals=\"1\" w:val = \"2\"", "w:val").as_deref(),
            Some("2")
        );
    }

    #[test]
    fn a_tracked_previous_section_is_not_edited() {
        let sect = "<w:sectPr><w:pgMar w:top=\"1440\"/><w:sectPrChange w:id=\"1\"><w:sectPr><w:pgSz w:w=\"1\" w:h=\"2\"/><w:type w:val=\"oddPage\"/><w:lnNumType w:countBy=\"2\"/><w:cols w:num=\"3\"/><w:titlePg/></w:sectPr></w:sectPrChange></w:sectPr>";
        let s = SectionSetup::parse(sect);
        assert_eq!(s, SectionSetup::default());
        let mut t = s.clone();
        t.page.set_paper(Paper::A4);
        let out = t.apply(sect);
        assert!(out.contains("<w:pgSz w:w=\"1\" w:h=\"2\"/>"), "{out}");
        let (a, _) = find_element(&out, "w:pgSz").unwrap();
        assert!(a < out.find("w:sectPrChange").unwrap(), "{out}");
        // Setting and then clearing the current ones leaves the tracked ones.
        let mut u = t.clone();
        u.start = SectionStart::Continuous;
        u.line_numbers = Some(LineNumbering {
            count_by: 1,
            start: None,
            distance: None,
            restart: LnRestart::Continuous,
        });
        u.columns = u.columns.equal(2, 720);
        let out = u.apply(&out);
        let mut v = u.clone();
        v.start = SectionStart::NextPage;
        v.line_numbers = None;
        v.columns = Columns::default();
        let out = v.apply(&out);
        let old = &out[out.find("<w:sectPrChange").unwrap()..];
        for kept in ["oddPage", "w:countBy=\"2\"", "w:num=\"3\"", "<w:titlePg/>"] {
            assert!(old.contains(kept), "{kept}: {out}");
        }
        assert_eq!(SectionSetup::parse(&out), v);
        assert!(
            !has_flag(&out, "w:titlePg"),
            "the tracked one is not current"
        );
        let off = set_flag(&out, "w:titlePg", false);
        assert_eq!(off, out, "the only titlePg is the tracked one");
        let on = set_flag(&off, "w:titlePg", true);
        assert!(on.find("<w:titlePg/>").unwrap() < on.find("<w:sectPrChange").unwrap());
        assert!(has_flag(&on, "w:titlePg"));
        assert!(!has_flag(
            "<w:sectPr><w:titlePg w:val=\"0\"/></w:sectPr>",
            "w:titlePg"
        ));
        assert_eq!(set_flag(&on, "w:titlePg", false), off);
    }

    #[test]
    fn hf_references_are_set_replaced_and_removed_per_kind_and_variant() {
        let sect = "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId1\"/>\
            <w:footerReference w:type=\"default\" r:id=\"rId2\"/><w:pgSz w:w=\"12240\" w:h=\"15840\"/>\
            <w:titlePg/></w:sectPr>";
        assert_eq!(hf_reference(sect, true, "default").as_deref(), Some("rId1"));
        assert_eq!(
            hf_reference(sect, false, "default").as_deref(),
            Some("rId2")
        );
        assert_eq!(hf_reference(sect, true, "first"), None);
        // A new header reference goes after the header references, before the footers.
        let first = set_hf_reference(sect, true, "first", Some("rId7"));
        assert!(
            first.contains(
                "r:id=\"rId1\"/><w:headerReference w:type=\"first\" r:id=\"rId7\"/><w:footerReference"
            ),
            "{first}"
        );
        // Replacing keeps one reference of that variant.
        let again = set_hf_reference(&first, true, "first", Some("rId8"));
        assert_eq!(again.matches("w:type=\"first\"").count(), 1);
        assert_eq!(hf_reference(&again, true, "first").as_deref(), Some("rId8"));
        // Removing touches only that kind and variant.
        let gone = set_hf_reference(&first, true, "first", None);
        assert_eq!(gone, sect);
        let no_footer = set_hf_reference(sect, false, "default", None);
        assert_eq!(
            hf_reference(&no_footer, true, "default").as_deref(),
            Some("rId1")
        );
        assert_eq!(hf_reference(&no_footer, false, "default"), None);
        // A footer reference into a sectPr with none goes first among the rest.
        let bare = "<w:sectPr><w:pgSz w:w=\"1\"/></w:sectPr>";
        assert_eq!(
            set_hf_reference(bare, false, "even", Some("rId3")),
            "<w:sectPr><w:footerReference w:type=\"even\" r:id=\"rId3\"/><w:pgSz w:w=\"1\"/></w:sectPr>"
        );
        // A reference without w:type is the default one.
        let untyped = "<w:sectPr><w:headerReference r:id=\"rId4\"/></w:sectPr>";
        assert_eq!(
            hf_reference(untyped, true, "default").as_deref(),
            Some("rId4")
        );
        assert_eq!(
            set_hf_reference(untyped, true, "default", None),
            "<w:sectPr></w:sectPr>"
        );
    }

    #[test]
    fn page_number_format_parses_and_applies_in_schema_order() {
        let sect =
            "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:cols w:space=\"720\"/></w:sectPr>";
        assert_eq!(PageNumberFormat::parse(sect), PageNumberFormat::default());
        let f = PageNumberFormat {
            fmt: Some("lowerRoman".into()),
            start: Some(5),
            ..Default::default()
        };
        let out = f.apply(sect);
        assert!(
            out.contains(
                "<w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgNumType w:fmt=\"lowerRoman\" w:start=\"5\"/><w:cols"
            ),
            "{out}"
        );
        assert_eq!(PageNumberFormat::parse(&out), f);
        // Unchanged stays byte for byte; all-default removes the element.
        assert_eq!(f.apply(&out), out);
        assert_eq!(PageNumberFormat::default().apply(&out), sect);
        // Chapter numbers round-trip; "decimal" reads as the default format.
        let ch = PageNumberFormat {
            chap_style: Some(1),
            chap_sep: Some("hyphen".into()),
            ..Default::default()
        };
        assert_eq!(PageNumberFormat::parse(&ch.apply(sect)), ch);
        let dec = "<w:sectPr><w:pgNumType w:fmt=\"decimal\"/></w:sectPr>";
        assert_eq!(PageNumberFormat::parse(dec), PageNumberFormat::default());
    }

    fn side(style: &str, sz: u32, space: u32) -> Option<BorderSide> {
        Some(BorderSide {
            style: style.into(),
            sz,
            space,
            color: None,
            shadow: false,
            frame: false,
        })
    }

    #[test]
    fn page_borders_write_in_schema_order_and_round_trip() {
        let sect = "<w:sectPr><w:headerReference w:type=\"default\" r:id=\"rId7\"/>\
            <w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\"/>\
            <w:pgNumType w:start=\"3\"/><w:cols w:space=\"720\"/><w:titlePg/></w:sectPr>";
        assert_eq!(PageBorders::parse(sect), None);
        // Box: all four sides, measured from the page edge, with a colour.
        let mut pb = PageBorders {
            sides: std::array::from_fn(|_| side("single", 4, 24)),
            offset_from: PgBorderOffset::Page,
            ..Default::default()
        };
        pb.sides[0].as_mut().unwrap().color = Some(0xFF0000);
        let out = PageBorders::apply(Some(&pb), sect);
        assert!(in_schema_order(&out), "{out}");
        assert_eq!(
            order(&out),
            [
                "w:headerReference",
                "w:pgSz",
                "w:pgMar",
                "w:pgBorders",
                "w:pgNumType",
                "w:cols",
                "w:titlePg"
            ]
        );
        assert!(
            out.contains(
                "<w:pgBorders w:offsetFrom=\"page\"><w:top w:val=\"single\" w:sz=\"4\" w:space=\"24\" w:color=\"FF0000\"/>\
                 <w:left w:val=\"single\" w:sz=\"4\" w:space=\"24\" w:color=\"auto\"/>"
            ),
            "{out}"
        );
        assert_eq!(PageBorders::parse(&out), Some(pb.clone()));
        // Unchanged rewrites nothing; None removes the element entirely.
        assert_eq!(PageBorders::apply(Some(&pb), &out), out);
        assert_eq!(PageBorders::apply(None, &out), sect);
        // No side set is the same as None.
        assert_eq!(
            PageBorders::apply(Some(&PageBorders::default()), &out),
            sect
        );
    }

    #[test]
    fn page_borders_settings_apply_to_and_custom_sides() {
        let sect = "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr>";
        // Shadow and 3-D are per-side flags; Apply to is w:display.
        let mut shadow = side("single", 12, 24).unwrap();
        shadow.shadow = true;
        let mut frame = side("double", 6, 4).unwrap();
        frame.frame = true;
        for (pb, needle) in [
            (
                PageBorders {
                    sides: std::array::from_fn(|_| Some(shadow.clone())),
                    display: PgBorderDisplay::FirstPage,
                    ..Default::default()
                },
                "<w:pgBorders w:display=\"firstPage\"><w:top w:val=\"single\" w:sz=\"12\" w:space=\"24\" w:color=\"auto\" w:shadow=\"1\"/>",
            ),
            (
                PageBorders {
                    sides: std::array::from_fn(|_| Some(frame.clone())),
                    display: PgBorderDisplay::NotFirstPage,
                    z_order_back: true,
                    ..Default::default()
                },
                "<w:pgBorders w:display=\"notFirstPage\" w:zOrder=\"back\"><w:top w:val=\"double\" w:sz=\"6\" w:space=\"4\" w:color=\"auto\" w:frame=\"1\"/>",
            ),
            (
                // Custom: only the bottom side.
                PageBorders {
                    sides: [None, None, side("dotted", 8, 1), None],
                    ..Default::default()
                },
                "<w:pgBorders><w:bottom w:val=\"dotted\" w:sz=\"8\" w:space=\"1\" w:color=\"auto\"/></w:pgBorders>",
            ),
        ] {
            let out = PageBorders::apply(Some(&pb), sect);
            assert!(out.contains(needle), "{out}");
            assert_eq!(PageBorders::parse(&out), Some(pb));
        }
    }

    #[test]
    fn page_borders_read_art_sides_and_skip_nil_ones() {
        let sect = "<w:sectPr><w:pgBorders w:offsetFrom=\"text\">\
            <w:top w:val=\"apples\" w:sz=\"20\" w:space=\"1\" w:color=\"auto\"/>\
            <w:left w:val=\"nil\"/></w:pgBorders>\
            <w:sectPrChange><w:sectPr><w:pgBorders><w:top w:val=\"single\"/></w:pgBorders></w:sectPr></w:sectPrChange></w:sectPr>";
        let pb = PageBorders::parse(sect).unwrap();
        assert_eq!(pb.sides[0].as_ref().unwrap().style, "apples");
        assert_eq!(pb.sides[1], None);
        assert_eq!(pb.offset_from, PgBorderOffset::Text);
        // Removing touches the section's own element, never the tracked one.
        let out = PageBorders::apply(None, sect);
        assert!(out.starts_with("<w:sectPr><w:sectPrChange>"), "{out}");
        assert!(out.contains("<w:top w:val=\"single\"/>"), "{out}");
    }
}
