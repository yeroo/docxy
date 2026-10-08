//! PDF export ("print") — a from-scratch, dependency-free PDF writer.
//!
//! Phase 0 uses the **Courier** standard-14 base fonts (regular / bold /
//! oblique / bold-oblique). Because Courier is monospaced (every glyph is
//! 600/1000 em wide) we need no font embedding and no AFM width tables, and
//! line-breaking is exact. Proportional output (Helvetica/Times with AFM
//! metrics) is a later refinement. Output is deterministic, so it is golden-byte
//! testable.
//!
//! Covered now: paragraphs, runs (bold/italic/underline/strike/color),
//! headings, lists, hyperlink annotations, and Word's page layout: page and
//! column breaks (and `pageBreakBefore`, direct or from the style), sections
//! (start type and odd/even parity, page size and orientation, margins with
//! gutter and mirror margins, newspaper columns with separator rules, balanced
//! before a continuous break, vertical alignment including justified, page
//! borders with their line styles, line numbers), the page colour, headers/
//! footers (default/first/even, linked to the previous section) with
//! PAGE/NUMPAGES/SECTIONPAGES fields numbered per `w:pgNumType`, and the
//! headers' VML text watermarks. Tables are
//! flattened to text rows; real bordered tables and images in PDF come in a
//! later phase.

use std::collections::HashMap;
use std::rc::Rc;

use crate::field::{FieldEvent, field_events};
use crate::load::{Relationships, xml_attr_value};
use crate::model::*;
use crate::package::{HeaderVariant, Package, SectionParts, TextWatermark, section_header_parts};
use crate::sect::LnRestart;
use crate::styles::{PprFlag, StyleSheet};

#[derive(Debug, Clone)]
pub struct PdfOptions {
    /// Fallback page geometry for a section whose `w:sectPr` omits `w:pgSz` /
    /// `w:pgMar` (and for a document without any sectPr).
    pub page_width: f32,
    pub page_height: f32,
    pub margin: f32,
    pub base_font_size: f32,
    /// Resolved stylesheet for effective run formatting.
    pub styles: Rc<StyleSheet>,
    /// Parsed header/footer parts keyed by part name (`word/header1.xml`).
    pub header_footer: HashMap<String, Rc<Vec<Block>>>,
    /// The text watermarks of each header part, keyed by part name; drawn
    /// behind the body on every page that applies the header.
    pub watermarks: HashMap<String, Vec<TextWatermark>>,
    /// Main document relationships, resolving each section's header/footer
    /// references to part names.
    pub rels: Relationships,
    /// The final section's `w:sectPr` when the printed document carries no
    /// trailing [`Block::SectionProperties`] of its own.
    pub last_sect_pr: Option<String>,
    /// `w:evenAndOddHeaders` (settings.xml): even pages use the `even` variant.
    pub even_and_odd_headers: bool,
    /// `w:mirrorMargins` (settings.xml): even pages swap left/right margins.
    pub mirror_margins: bool,
    /// `w:gutterAtTop` (settings.xml): the gutter adds to the top margin.
    pub gutter_at_top: bool,
    /// The page colour (`w:background w:color` in document.xml).
    pub background: Option<(u8, u8, u8)>,
}

impl Default for PdfOptions {
    fn default() -> Self {
        // US Letter, 1-inch margins.
        PdfOptions {
            page_width: 612.0,
            page_height: 792.0,
            margin: 72.0,
            base_font_size: 11.0,
            styles: Rc::new(StyleSheet::default()),
            header_footer: HashMap::new(),
            watermarks: HashMap::new(),
            rels: Relationships::default(),
            last_sect_pr: None,
            even_and_odd_headers: false,
            mirror_margins: false,
            gutter_at_top: false,
            background: None,
        }
    }
}

impl PdfOptions {
    /// Everything besides the body that the page layout needs, read from a
    /// loaded package: every header/footer part, the document relationships,
    /// the settings flags and the page colour. A caller holding unsaved header
    /// or footer edits replaces that part's entry in `header_footer`.
    pub fn from_package(pkg: &Package, styles: Rc<StyleSheet>) -> PdfOptions {
        let rels = pkg.document_rels();
        let mut header_footer = HashMap::new();
        let mut watermarks = HashMap::new();
        for (_, target, external) in rels.iter() {
            if external {
                continue;
            }
            let Some(name) = crate::package::resolve_document_relationship_target(target) else {
                continue;
            };
            let Some(xml) = pkg.part(&name).and_then(crate::package::decode_xml_part) else {
                continue;
            };
            if !(xml.contains("<w:hdr") || xml.contains("<w:ftr")) {
                continue;
            }
            if xml.contains("<w:hdr") {
                let marks = crate::package::text_watermarks(&xml);
                if !marks.is_empty() {
                    watermarks.insert(name.clone(), marks);
                }
            }
            if let Some(blocks) = pkg.header_footer_blocks(&name) {
                header_footer.insert(name, Rc::new(blocks));
            }
        }
        let background = pkg
            .document_part()
            .and_then(crate::package::decode_xml_part)
            .and_then(|xml| {
                let el = start_tag(&xml, "w:background")?;
                parse_hex(&xml_attr_value(el, "w:color")?)
            });
        PdfOptions {
            styles,
            header_footer,
            watermarks,
            rels,
            last_sect_pr: Some(pkg.sect_pr().to_string()),
            even_and_odd_headers: pkg.has_even_odd(),
            mirror_margins: pkg.settings_flag("w:mirrorMargins").unwrap_or(false),
            gutter_at_top: pkg.settings_flag("w:gutterAtTop").unwrap_or(false),
            background,
            ..PdfOptions::default()
        }
    }
}

/// Render a document to PDF bytes.
pub fn to_pdf(doc: &Document, opts: &PdfOptions) -> Vec<u8> {
    let pages = Pager::run(doc, opts);
    write_pdf(&pages, opts)
}

// ---- section geometry ----

/// How a section begins relative to the previous one (`w:type`, read from the
/// section that starts, ECMA-376 §17.6.22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectStart {
    NextPage,
    Continuous,
    OddPage,
    EvenPage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageVAlign {
    Top,
    Center,
    Bottom,
    /// Justified: the paragraphs spread to fill the page.
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumFmt {
    Decimal,
    LowerRoman,
    UpperRoman,
    LowerLetter,
    UpperLetter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorderDisplay {
    AllPages,
    FirstPage,
    NotFirstPage,
}

#[derive(Debug, Clone, PartialEq)]
struct BorderSide {
    /// Line width in points (`w:sz` is in eighths of a point; in points for
    /// an art border).
    width: f32,
    /// Distance from the text (or page edge), in points.
    space: f32,
    color: (f32, f32, f32),
    style: BorderStyle,
}

/// How a border side is stroked (`w:val`, ECMA-376 §17.18.2).
#[derive(Debug, Clone, PartialEq)]
enum BorderStyle {
    /// One solid line: `single`, `thick`, and the styles drawn as it for now
    /// (`wave`, `doubleWave`, `dashDotStroked`, the 3-D styles).
    Single,
    /// Parallel solid lines, innermost first: whether each is thin (a third of
    /// the side's width), and the gap between them in widths of the thin line
    /// (or of the line, when none is thin).
    Lines { thin: Vec<bool>, gap: f32 },
    /// A dash array in multiples of the line width.
    Dashes(Vec<f32>),
    /// An art border (`apples`, `basicBlackDots`, …): a dashed band, since
    /// the artwork isn't drawn.
    Art,
}

impl BorderStyle {
    /// The style for `w:val`; `None` for no border.
    fn parse(val: &str) -> Option<BorderStyle> {
        let lines = |thin: &[bool], gap: f32| BorderStyle::Lines {
            thin: thin.to_vec(),
            gap,
        };
        Some(match val {
            "none" | "nil" => return None,
            "single" | "thick" | "wave" | "doubleWave" | "dashDotStroked" | "threeDEmboss"
            | "threeDEngrave" | "outset" | "inset" => BorderStyle::Single,
            // Gaps of one line width.
            "double" => lines(&[false, false], 1.0),
            "triple" => lines(&[false, false, false], 1.0),
            "dotted" => BorderStyle::Dashes(vec![1.0, 1.0]),
            "dashed" => BorderStyle::Dashes(vec![3.0, 2.0]),
            "dashSmallGap" => BorderStyle::Dashes(vec![3.0, 1.0]),
            "dotDash" => BorderStyle::Dashes(vec![3.0, 2.0, 1.0, 2.0]),
            "dotDotDash" => BorderStyle::Dashes(vec![3.0, 2.0, 1.0, 2.0, 1.0, 2.0]),
            // Our convention: the first-named line is the inner one (nearer the
            // text); the gap is 1, 2 or 3 thin widths.
            _ => {
                let (kind, gap) = if let Some(k) = val.strip_suffix("SmallGap") {
                    (k, 1.0)
                } else if let Some(k) = val.strip_suffix("MediumGap") {
                    (k, 2.0)
                } else if let Some(k) = val.strip_suffix("LargeGap") {
                    (k, 3.0)
                } else {
                    return Some(BorderStyle::Art);
                };
                match kind {
                    "thinThick" => lines(&[true, false], gap),
                    "thickThin" => lines(&[false, true], gap),
                    "thinThickThin" => lines(&[true, false, true], gap),
                    "thickThinThick" => lines(&[false, true, false], gap),
                    _ => BorderStyle::Art,
                }
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PageBorders {
    /// top, left, bottom, right
    sides: [Option<BorderSide>; 4],
    from_page: bool,
    display: BorderDisplay,
}

/// One section's page layout, in points.
#[derive(Debug, Clone, PartialEq)]
struct SectionLayout {
    w: f32,
    h: f32,
    top: f32,
    bottom: f32,
    left: f32,
    right: f32,
    header: f32,
    footer: f32,
    gutter: f32,
    /// Column widths, each with the space after it.
    cols: Vec<(f32, f32)>,
    col_sep: bool,
    valign: PageVAlign,
    title_pg: bool,
    num_fmt: NumFmt,
    num_start: Option<u32>,
    borders: PageBorders,
    start: SectStart,
    line_numbers: Option<LineNumbers>,
}

/// `w:lnNumType`, resolved to what the layout draws.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LineNumbers {
    count_by: u32,
    /// Word writes the dialog's "Start at" minus one ([MS-OI29500]; LibreOffice
    /// adds one on import too), so the first line is numbered `start + 1`.
    start: u32,
    /// From the number's right edge to the text column, in points.
    distance: f32,
    restart: LnRestart,
}

impl LineNumbers {
    fn from_setup(ln: crate::sect::LineNumbering) -> LineNumbers {
        LineNumbers {
            count_by: ln.count_by.max(1) as u32,
            start: ln.start.unwrap_or(0).max(0) as u32,
            // Absent or zero is Word's "Auto": a quarter inch.
            distance: twips(ln.distance.filter(|&d| d > 0).unwrap_or(360) as f32),
            restart: ln.restart,
        }
    }
}

/// The first start tag `<tag …>` in `xml` (not a longer tag name sharing the
/// prefix, so `w:col` skips `w:cols`), up to its closing `>`.
fn start_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    start_tags(xml, tag).next()
}

fn start_tags<'a>(xml: &'a str, tag: &str) -> impl Iterator<Item = &'a str> + use<'a> {
    crate::load::start_tags(xml, tag)
        .into_iter()
        .map(|(_, el)| el)
}

/// A numeric attribute (twips etc.) as f32; `inf`/`NaN` count as absent.
fn num_attr(el: &str, key: &str) -> Option<f32> {
    xml_attr_value(el, key)?
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
}

/// Word's limit on newspaper columns in a section.
const MAX_COLS: usize = 45;
/// Word's largest starting page number (`w:pgNumType w:start`).
const MAX_PAGE_START: f32 = 32767.0;

/// An on/off element in `xml` (`w:titlePg`, `w:pageBreakBefore`, …).
fn flag_on(xml: &str, elem: &str) -> bool {
    crate::package::settings_flag_of(xml, elem).unwrap_or(false)
}

fn twips(v: f32) -> f32 {
    v / 20.0
}

impl SectionLayout {
    fn parse(sect: &str, opts: &PdfOptions) -> SectionLayout {
        // A tracked `w:sectPrChange` holds the old values; lay out the current.
        let (current, _) =
            crate::load::split_property_change_container(sect, PropertyScope::Section);
        let sect = current.as_str();
        let pg_sz = start_tag(sect, "w:pgSz");
        let pg_mar = start_tag(sect, "w:pgMar");
        let size = |key: &str, fallback: f32| {
            pg_sz
                .and_then(|el| num_attr(el, key))
                .map(twips)
                .unwrap_or(fallback)
        };
        let mar = |key: &str, fallback: f32| {
            pg_mar
                .and_then(|el| num_attr(el, key))
                .map(|v| twips(v.abs()))
                .unwrap_or(fallback)
        };
        let w = size("w:w", opts.page_width);
        let h = size("w:h", opts.page_height);
        let (left, right) = (mar("w:left", opts.margin), mar("w:right", opts.margin));
        let gutter = mar("w:gutter", 0.0);

        // Columns: equal widths split the text width, or explicit `w:col`s.
        let content_w = w - left - right - if opts.gutter_at_top { 0.0 } else { gutter };
        let cols_el = start_tag(sect, "w:cols");
        let num = cols_el
            .and_then(|el| num_attr(el, "w:num"))
            .map_or(1, |n| n.clamp(1.0, MAX_COLS as f32) as usize);
        let space = cols_el
            .and_then(|el| num_attr(el, "w:space"))
            .map_or(36.0, twips);
        let equal = !matches!(
            cols_el
                .and_then(|el| xml_attr_value(el, "w:equalWidth"))
                .as_deref(),
            Some("0" | "false" | "off")
        );
        let explicit: Vec<(f32, f32)> = if equal {
            Vec::new()
        } else {
            let block = sect
                .find("<w:cols")
                .map(|s| &sect[s..])
                .map(|rest| rest.find("</w:cols>").map_or(rest, |e| &rest[..e]))
                .unwrap_or("");
            start_tags(block, "w:col")
                .filter_map(|el| {
                    Some((
                        twips(num_attr(el, "w:w")?),
                        num_attr(el, "w:space").map_or(0.0, twips),
                    ))
                })
                .take(MAX_COLS)
                .collect()
        };
        let cols = if !explicit.is_empty() {
            explicit
        } else {
            let n = num as f32;
            let col_w = ((content_w - (n - 1.0) * space) / n).max(1.0);
            (0..num).map(|_| (col_w, space)).collect()
        };
        let col_sep = cols_el.is_some_and(|el| {
            matches!(
                xml_attr_value(el, "w:sep").as_deref(),
                Some("1" | "true" | "on")
            )
        });

        let valign = match start_tag(sect, "w:vAlign")
            .and_then(|el| xml_attr_value(el, "w:val"))
            .as_deref()
        {
            Some("center") => PageVAlign::Center,
            Some("bottom") => PageVAlign::Bottom,
            Some("both") => PageVAlign::Both,
            _ => PageVAlign::Top,
        };
        let num_type = start_tag(sect, "w:pgNumType");
        let num_fmt = match num_type
            .and_then(|el| xml_attr_value(el, "w:fmt"))
            .as_deref()
        {
            Some("lowerRoman") => NumFmt::LowerRoman,
            Some("upperRoman") => NumFmt::UpperRoman,
            Some("lowerLetter") => NumFmt::LowerLetter,
            Some("upperLetter") => NumFmt::UpperLetter,
            _ => NumFmt::Decimal,
        };
        let num_start = num_type
            .and_then(|el| num_attr(el, "w:start"))
            .map(|n| n.clamp(0.0, MAX_PAGE_START) as u32);
        let start = match start_tag(sect, "w:type")
            .and_then(|el| xml_attr_value(el, "w:val"))
            .as_deref()
        {
            // A new column set on the same page; Word's `nextColumn` also stays
            // on the page here.
            Some("continuous" | "nextColumn") => SectStart::Continuous,
            Some("oddPage") => SectStart::OddPage,
            Some("evenPage") => SectStart::EvenPage,
            _ => SectStart::NextPage,
        };

        SectionLayout {
            w,
            h,
            top: mar("w:top", opts.margin),
            bottom: mar("w:bottom", opts.margin),
            left,
            right,
            header: mar("w:header", 36.0),
            footer: mar("w:footer", 36.0),
            gutter,
            cols,
            col_sep,
            valign,
            title_pg: flag_on(sect, "w:titlePg"),
            num_fmt,
            num_start,
            borders: parse_page_borders(sect),
            start,
            line_numbers: crate::sect::SectionSetup::parse(sect)
                .line_numbers
                .map(LineNumbers::from_setup),
        }
    }

    /// Left and right margins of a page (gutter included) — mirror margins swap
    /// them on even pages, where the gutter sits on the inside (right) edge.
    fn h_margins(&self, even: bool, opts: &PdfOptions) -> (f32, f32) {
        let mirrored = opts.mirror_margins && even;
        let (mut l, mut r) = if mirrored {
            (self.right, self.left)
        } else {
            (self.left, self.right)
        };
        if !opts.gutter_at_top {
            if mirrored {
                r += self.gutter;
            } else {
                l += self.gutter;
            }
        }
        (l, r)
    }

    fn top_margin(&self, opts: &PdfOptions) -> f32 {
        self.top + if opts.gutter_at_top { self.gutter } else { 0.0 }
    }
}

fn parse_page_borders(sect: &str) -> PageBorders {
    let mut out = PageBorders {
        sides: Default::default(),
        from_page: false,
        display: BorderDisplay::AllPages,
    };
    let Some(start) = sect.find("<w:pgBorders") else {
        return out;
    };
    let rest = &sect[start..];
    let block = rest.find("</w:pgBorders>").map_or(rest, |e| &rest[..e]);
    if let Some(el) = start_tag(block, "w:pgBorders") {
        out.from_page = xml_attr_value(el, "w:offsetFrom").as_deref() == Some("page");
        out.display = match xml_attr_value(el, "w:display").as_deref() {
            Some("firstPage") => BorderDisplay::FirstPage,
            Some("notFirstPage") => BorderDisplay::NotFirstPage,
            _ => BorderDisplay::AllPages,
        };
    }
    for (i, tag) in ["w:top", "w:left", "w:bottom", "w:right"]
        .into_iter()
        .enumerate()
    {
        let Some(el) = start_tag(block, tag) else {
            continue;
        };
        let Some(style) = xml_attr_value(el, "w:val")
            .as_deref()
            .and_then(BorderStyle::parse)
        else {
            continue;
        };
        let color = xml_attr_value(el, "w:color")
            .and_then(|c| parse_hex(&c))
            .map_or((0.0, 0.0, 0.0), rgb_f);
        let eighths = if style == BorderStyle::Art { 1.0 } else { 8.0 };
        out.sides[i] = Some(BorderSide {
            width: num_attr(el, "w:sz").map_or(0.5, |sz| (sz / eighths).max(0.25)),
            space: num_attr(el, "w:space").unwrap_or(0.0),
            color,
            style,
        });
    }
    out
}

fn format_page_number(n: u32, fmt: NumFmt) -> String {
    match fmt {
        NumFmt::Decimal => n.to_string(),
        NumFmt::LowerRoman => roman(n).to_lowercase(),
        NumFmt::UpperRoman => roman(n),
        NumFmt::LowerLetter => letters(n).to_lowercase(),
        NumFmt::UpperLetter => letters(n),
    }
}

fn roman(mut n: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const TABLE: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, digits) in TABLE {
        while n >= value {
            out.push_str(digits);
            n -= value;
        }
    }
    out
}

/// Word's letter numbering: A..Z, then AA..ZZ, AAA.. (the letter repeats).
fn letters(n: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let letter = char::from(b'A' + ((n - 1) % 26) as u8);
    std::iter::repeat_n(letter, ((n - 1) / 26 + 1) as usize).collect()
}

/// Split the body into sections: a paragraph with a `section_break` ends one;
/// the rest is the final section, described by the trailing sectPr.
fn split_sections<'d>(
    doc: &'d Document,
    opts: &'d PdfOptions,
) -> Vec<(std::ops::Range<usize>, &'d str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let end = doc.content_block_count();
    for (i, block) in doc.body[..end].iter().enumerate() {
        if let Block::Paragraph(p) = block
            && let Some(sect) = &p.props.section_break
        {
            out.push((start..i + 1, sect.as_str()));
            start = i + 1;
        }
    }
    let last = doc
        .trailing_section_properties()
        .map(|s| s.raw.as_str())
        .or(opts.last_sect_pr.as_deref())
        .unwrap_or("");
    out.push((start..end, last));
    out
}

// ---- layout ----

/// A page-dependent field, substituted when the page is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageField {
    Page,
    NumPages,
    SectionPages,
}

fn page_field_kind(instr: &str) -> Option<PageField> {
    match instr
        .split_whitespace()
        .next()?
        .to_ascii_uppercase()
        .as_str()
    {
        "PAGE" => Some(PageField::Page),
        "NUMPAGES" => Some(PageField::NumPages),
        "SECTIONPAGES" => Some(PageField::SectionPages),
        _ => None,
    }
}

#[derive(Clone)]
struct PCell {
    ch: char,
    font: u8, // 0=regular 1=bold 2=oblique 3=bold-oblique
    color: (f32, f32, f32),
    underline: bool,
    strike: bool,
    link: Option<Rc<str>>,
    /// Part of a page field's result; `true` on the result's first cell, which
    /// carries the substituted value (the rest of the result is dropped).
    field: Option<(PageField, bool)>,
}

/// A run of cells up to a break; `brk` is the break that ended it.
#[derive(Default)]
struct Seg {
    cells: Vec<PCell>,
    brk: Option<BreakKind>,
}

#[derive(Clone)]
struct Frag {
    x: f32,
    y: f32,
    text: String,
    size: f32,
    font: u8,
    color: (f32, f32, f32),
    underline: bool,
    strike: bool,
    /// A page field: its kind, and whether this frag holds the field's first
    /// cell (and so draws the value) or only the tail of its cached result.
    field: Option<(PageField, bool)>,
    /// The body section a field sits in (SECTIONPAGES); header and footer
    /// fields count the page's own section.
    sect: Option<usize>,
}

type Link = ((f32, f32, f32, f32), String);

/// A text watermark placed on a page: centred on `(cx, cy)`, turned `angle`
/// degrees counter-clockwise.
#[derive(Debug, Clone)]
struct WatermarkDraw {
    text: String,
    size: f32,
    cx: f32,
    cy: f32,
    angle: f32,
    color: (f32, f32, f32),
}

impl WatermarkDraw {
    /// Place `mark` in the margin box `(left, bottom, right, top)`, Word's
    /// default (`mso-position-*-relative:margin`, centred).
    fn place(mark: &TextWatermark, (left, bottom, right, top): (f32, f32, f32, f32)) -> Self {
        let chars = mark.text.chars().count().max(1) as f32;
        // "Auto" size stretches the text to the shape's width; without one, to
        // most of the text width. A set font size caps it.
        let width = mark.width_pt.unwrap_or(0.8 * (right - left));
        let mut size = width / (chars * 0.6);
        if let Some(pt) = mark.font_size_pt {
            size = size.min(pt);
        }
        WatermarkDraw {
            text: mark.text.clone(),
            size: size.max(1.0),
            cx: (left + right) / 2.0,
            cy: (bottom + top) / 2.0,
            // VML turns clockwise (y down); PDF angles run counter-clockwise.
            angle: -mark.rotation,
            color: rgb_f(mark.fill.unwrap_or((0xc0, 0xc0, 0xc0))),
        }
    }
}

#[derive(Clone)]
struct Rule {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    width: f32,
    color: (f32, f32, f32),
    /// A dash array in points; empty for a solid line.
    dash: Vec<f32>,
}

struct Page {
    w: f32,
    h: f32,
    /// The section owning the page: its geometry, headers and numbering.
    sect: usize,
    number: u32,
    /// The first page of its section (titlePg applies).
    first: bool,
    /// An odd/even filler page inserted before a section.
    blank: bool,
    left: f32,
    right: f32,
    /// The text area: where body lines start and where they overflow.
    body_top: f32,
    body_bottom: f32,
    frags: Vec<Frag>,
    links: Vec<Link>,
    hf_frags: Vec<Frag>,
    hf_links: Vec<Link>,
    /// Text watermarks from the page's header, drawn behind everything else.
    watermarks: Vec<WatermarkDraw>,
    rules: Vec<Rule>,
    /// Every body line, in layout order (vertical alignment, column balancing).
    lines: Vec<LineRec>,
    /// Lowest body baseline on the page (vertical alignment).
    min_base: f32,
    regions: usize,
    multi_col: bool,
    /// The owning section plus every section with a line on the page.
    members: Vec<usize>,
    /// Formatted PAGE, NUMPAGES and SECTIONPAGES values.
    values: [String; 3],
}

/// One body line on a page: where it sits, and the first of its frags and
/// links (the line owns them up to the next line's first, or the page's end).
#[derive(Debug, Clone, Copy)]
struct LineRec {
    y: f32,
    lh: f32,
    col: usize,
    /// The column set ([`Region::id`]) and paragraph it belongs to.
    region: u32,
    para: u32,
    frags: usize,
    links: usize,
    /// Paragraph spacing laid out after the line.
    gap_after: f32,
}

impl Page {
    /// The frags and links of line `i`.
    fn line_items(&self, i: usize) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let line = &self.lines[i];
        let next = self.lines.get(i + 1);
        (
            line.frags..next.map_or(self.frags.len(), |n| n.frags),
            line.links..next.map_or(self.links.len(), |n| n.links),
        )
    }

    /// Move line `i` (its frags and links) by `(dx, dy)`.
    fn shift_line(&mut self, i: usize, dx: f32, dy: f32) {
        let (frags, links) = self.line_items(i);
        for f in &mut self.frags[frags] {
            f.x += dx;
            f.y += dy;
        }
        for (rect, _) in &mut self.links[links] {
            rect.0 += dx;
            rect.2 += dx;
            rect.1 += dy;
            rect.3 += dy;
        }
        self.lines[i].y += dy;
    }
}

/// What a flow knows about the paragraph whose lines come next.
#[derive(Debug, Clone, Copy)]
struct ParaInfo {
    /// Its lines are line-numbered (`w:suppressLineNumbers` is off; tables
    /// are never numbered).
    numbered: bool,
}

/// Where laid-out lines go: the paginated body, or a header/footer band.
trait Flow {
    /// A paragraph (or a flattened table) starts.
    fn begin_paragraph(&mut self, _info: ParaInfo) {}
    /// Advance to the next line of height `lh`; returns the column's left x,
    /// the column's width and the line's baseline.
    fn next_line(&mut self, lh: f32) -> (f32, f32, f32);
    fn push(&mut self, frag: Frag, link: Option<Link>);
    /// Vertical space (paragraph spacing).
    fn gap(&mut self, dy: f32);
    fn hard_break(&mut self, kind: BreakKind);
    /// Nothing has been laid out on the current page yet.
    fn at_top(&self) -> bool;
}

fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn rgb_f((r, g, b): (u8, u8, u8)) -> (f32, f32, f32) {
    (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

fn run_color(p: &RunProps) -> (f32, f32, f32) {
    match p.color.as_deref().and_then(parse_hex) {
        Some(rgb) => rgb_f(rgb),
        None => (0.0, 0.0, 0.0),
    }
}

fn font_index(bold: bool, italic: bool) -> u8 {
    match (bold, italic) {
        (true, true) => 3,
        (true, false) => 1,
        (false, true) => 2,
        (false, false) => 0,
    }
}

fn heading_size(p: &Paragraph, base: f32) -> f32 {
    match p.props.heading_level {
        Some(1) => base * 1.8,
        Some(2) => base * 1.5,
        Some(3) => base * 1.3,
        Some(4) => base * 1.15,
        Some(_) => base * 1.05,
        None => base,
    }
}

/// `w:pageBreakBefore`: direct (kept verbatim in `raw_props`, on or off), else
/// from the paragraph style.
fn page_break_before(p: &Paragraph, styles: &StyleSheet) -> bool {
    styles.effective_ppr_flag(
        p.props.style_id.as_deref(),
        &p.props,
        PprFlag::PageBreakBefore,
    )
}

fn emit_blocks(flow: &mut dyn Flow, blocks: &[Block], opts: &PdfOptions) {
    for block in blocks {
        match block {
            Block::Paragraph(p) => emit_paragraph(flow, p, opts),
            Block::Table(t) => emit_table(flow, t, opts),
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
    }
}

fn emit_paragraph(flow: &mut dyn Flow, p: &Paragraph, opts: &PdfOptions) {
    if page_break_before(p, &opts.styles) && !flow.at_top() {
        flow.hard_break(BreakKind::Page);
    }
    flow.begin_paragraph(ParaInfo {
        numbered: !opts.styles.effective_ppr_flag(
            p.props.style_id.as_deref(),
            &p.props,
            PprFlag::SuppressLineNumbers,
        ),
    });
    let size = heading_size(p, opts.base_font_size);
    let mut segs = flatten_segments(p, p.props.heading_level.is_some(), &opts.styles);
    if p.props.num_id.is_some() {
        let ind = (p.props.ilvl.max(0) as usize) * 2;
        let mut bullet: Vec<PCell> = Vec::new();
        for _ in 0..ind {
            bullet.push(plain_cell(' '));
        }
        bullet.push(plain_cell('•'));
        bullet.push(plain_cell(' '));
        bullet.extend(std::mem::take(&mut segs[0].cells));
        segs[0].cells = bullet;
    }
    emit_segments(flow, segs, size, p.props.align);
}

fn emit_table(flow: &mut dyn Flow, t: &Table, opts: &PdfOptions) {
    // Word doesn't number table lines.
    flow.begin_paragraph(ParaInfo { numbered: false });
    // Phase 0: flatten each row to a text line (no borders).
    for row in &t.rows {
        let cols: Vec<String> = row
            .cells
            .iter()
            .map(|c| {
                c.blocks
                    .iter()
                    .map(|b| b.plain_text())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        let text = cols.join("    ");
        let seg = Seg {
            cells: text
                .chars()
                .filter_map(printed_char)
                .map(plain_cell)
                .collect(),
            brk: None,
        };
        emit_segments(flow, vec![seg], opts.base_font_size, Align::Left);
    }
}

fn emit_segments(flow: &mut dyn Flow, segs: Vec<Seg>, size: f32, align: Align) {
    let line_height = size * 1.35;
    let advance = 0.6 * size;
    for seg in segs {
        // Wrap one line at a time: the column (and its width) can change when a
        // line overflows to the next column or page.
        let mut rest: &[PCell] = &seg.cells;
        let mut first = true;
        loop {
            if !first && rest.iter().all(|c| c.ch == ' ') {
                break;
            }
            let (col_x, col_w, y) = flow.next_line(line_height);
            let cpl = ((col_w / advance).floor() as usize).max(1);
            let (line, used) = take_line(rest, cpl);
            rest = &rest[used..];
            first = false;
            place_line(flow, &line, col_x, col_w, y, size, align);
            if rest.is_empty() {
                break;
            }
        }
        if let Some(kind) = seg.brk {
            flow.hard_break(kind);
        }
    }
    // paragraph spacing
    flow.gap(size * 0.4);
}

fn place_line(
    flow: &mut dyn Flow,
    line: &[PCell],
    col_x: f32,
    col_w: f32,
    y: f32,
    size: f32,
    align: Align,
) {
    let advance = 0.6 * size;
    let line_w = line.len() as f32 * advance;
    let off = match align {
        Align::Center => (col_w - line_w).max(0.0) / 2.0,
        Align::Right => (col_w - line_w).max(0.0),
        _ => 0.0,
    };
    let x0 = col_x + off;
    let mut i = 0;
    while i < line.len() {
        let start = i;
        let c0 = line[start].clone();
        while i < line.len() && same_style(&line[i], &c0) {
            i += 1;
        }
        let cells = &line[start..i];
        let text: String = cells.iter().map(|c| c.ch).collect();
        let fx = x0 + start as f32 * advance;
        let span_w = (i - start) as f32 * advance;
        let link = c0.link.as_ref().map(|link| {
            (
                (fx, y - 2.0, fx + span_w, y + size * 0.85),
                link.to_string(),
            )
        });
        let field = c0
            .field
            .map(|(kind, _)| (kind, cells.iter().any(|c| c.field.is_some_and(|f| f.1))));
        flow.push(
            Frag {
                x: fx,
                y,
                text,
                size,
                font: c0.font,
                color: c0.color,
                underline: c0.underline,
                strike: c0.strike,
                field,
                sect: None,
            },
            link,
        );
    }
}

/// A header or footer band: lines stack down from `top` without pagination.
struct BandFlow {
    x: f32,
    width: f32,
    y: f32,
    low: f32,
    frags: Vec<Frag>,
    links: Vec<Link>,
}

impl BandFlow {
    fn new(x: f32, width: f32, top: f32) -> Self {
        BandFlow {
            x,
            width,
            y: top,
            low: top,
            frags: Vec::new(),
            links: Vec::new(),
        }
    }
}

impl Flow for BandFlow {
    fn next_line(&mut self, lh: f32) -> (f32, f32, f32) {
        self.y -= lh;
        self.low = self.low.min(self.y);
        (self.x, self.width, self.y)
    }
    fn push(&mut self, frag: Frag, link: Option<Link>) {
        self.frags.push(frag);
        self.links.extend(link);
    }
    fn gap(&mut self, dy: f32) {
        self.y -= dy;
    }
    fn hard_break(&mut self, _kind: BreakKind) {}
    fn at_top(&self) -> bool {
        self.frags.is_empty()
    }
}

/// A column set on one page, belonging to one section.
struct Region {
    /// Unique per column set per page, tying [`LineRec`]s to it.
    id: u32,
    sect: usize,
    top: f32,
    col: usize,
    /// Lowest y reached in any of its columns.
    low: f32,
    /// Each column's left x and width.
    xs: Vec<(f32, f32)>,
    /// A line has landed in it on the current page.
    placed: bool,
    /// A column break moved it on (its columns aren't balanced).
    column_break: bool,
}

/// The paginated body.
struct Pager<'a> {
    opts: &'a PdfOptions,
    sects: Vec<SectionLayout>,
    parts: Vec<SectionParts>,
    pages: Vec<Page>,
    next_number: u32,
    region: Region,
    next_region: u32,
    y: f32,
    /// The current paragraph's ordinal, and whether its lines are numbered.
    para: u32,
    numbered: bool,
    line_count: LineCount,
}

/// The line-number counter and where it last counted, for `w:restart`.
#[derive(Default)]
struct LineCount {
    count: u32,
    /// The page index and section of the last counted line.
    last: Option<(usize, usize)>,
}

impl<'a> Pager<'a> {
    fn run(doc: &Document, opts: &'a PdfOptions) -> Vec<Page> {
        let sections = split_sections(doc, opts);
        let raws: Vec<&str> = sections.iter().map(|(_, raw)| *raw).collect();
        let mut pager = Pager {
            opts,
            sects: raws
                .iter()
                .map(|raw| SectionLayout::parse(raw, opts))
                .collect(),
            parts: section_header_parts(&raws, &opts.rels),
            pages: Vec::new(),
            next_number: 1,
            region: Region {
                id: 0,
                sect: 0,
                top: 0.0,
                col: 0,
                low: 0.0,
                xs: Vec::new(),
                placed: false,
                column_break: false,
            },
            next_region: 0,
            y: 0.0,
            para: 0,
            numbered: false,
            line_count: LineCount::default(),
        };
        for (i, (range, _)) in sections.iter().enumerate() {
            pager.start_section(i);
            emit_blocks(&mut pager, &doc.body[range.clone()], opts);
        }
        pager.close_region();
        pager.finish()
    }

    fn start_section(&mut self, i: usize) {
        if i == 0 {
            self.new_page(0, true, false);
            return;
        }
        let s = &self.sects[i];
        match s.start {
            SectStart::Continuous => {
                let page = self.pages.last().expect("a page exists after section 0");
                if (page.w, page.h) != (s.w, s.h) {
                    // A different paper size can't share the page.
                    self.new_page(i, true, false);
                } else {
                    self.balance_region();
                    let top = self.region.low;
                    self.start_region(i, top);
                    // The page belongs to the section at its top, so a restart
                    // numbers the next page; titlePg likewise has no page of
                    // this section to apply to.
                    if let Some(n) = self.sects[i].num_start {
                        self.next_number = n;
                    }
                }
            }
            SectStart::NextPage => self.new_page(i, true, false),
            SectStart::OddPage | SectStart::EvenPage => {
                let n = s.num_start.unwrap_or(self.next_number);
                let want_odd = s.start == SectStart::OddPage;
                if (n % 2 == 1) != want_odd {
                    // The filler belongs to the previous section.
                    self.new_page(i - 1, false, true);
                }
                self.new_page(i, true, false);
            }
        }
    }

    fn new_page(&mut self, sect: usize, first: bool, blank: bool) {
        self.close_region();
        let opts = self.opts;
        let s = &self.sects[sect];
        let number = match s.num_start {
            Some(n) if first => n,
            _ => self.next_number,
        };
        self.next_number = number.saturating_add(1);
        let even = number.is_multiple_of(2);
        let (left, right) = s.h_margins(even, opts);
        let width = s.w - left - right;
        let variant = if first && s.title_pg {
            HeaderVariant::First
        } else if opts.even_and_odd_headers && even {
            HeaderVariant::Even
        } else {
            HeaderVariant::Default
        };
        let part_blocks = |applied: Option<&crate::package::AppliedPart>| {
            applied
                .and_then(|a| opts.header_footer.get(&a.part_name))
                .filter(|blocks| !blocks.is_empty())
        };
        let parts = self.parts.get(sect);
        let mut body_top = s.h - s.top_margin(opts);
        // A header's watermark applies even when the header shows no text.
        let margin_box = (left, s.bottom, s.w - right, body_top);
        let watermarks: Vec<WatermarkDraw> = parts
            .and_then(|p| p.headers[variant.index()].as_ref())
            .and_then(|a| opts.watermarks.get(&a.part_name))
            .into_iter()
            .flatten()
            .map(|mark| WatermarkDraw::place(mark, margin_box))
            .collect();
        let mut body_bottom = s.bottom;
        let mut hf_frags = Vec::new();
        let mut hf_links = Vec::new();
        if let Some(blocks) = part_blocks(parts.and_then(|p| p.headers[variant.index()].as_ref())) {
            let mut band = BandFlow::new(left, width, s.h - s.header);
            emit_blocks(&mut band, blocks, opts);
            // A tall header pushes the body down.
            body_top = body_top.min(band.low - opts.base_font_size * 0.4);
            hf_frags.append(&mut band.frags);
            hf_links.append(&mut band.links);
        }
        if let Some(blocks) = part_blocks(parts.and_then(|p| p.footers[variant.index()].as_ref())) {
            // Lay out from 0, then lift so the last baseline sits at the footer
            // distance from the bottom edge.
            let mut band = BandFlow::new(left, width, 0.0);
            emit_blocks(&mut band, blocks, opts);
            let dy = s.footer - band.low;
            for f in &mut band.frags {
                f.y += dy;
            }
            for (rect, _) in &mut band.links {
                rect.1 += dy;
                rect.3 += dy;
            }
            // A tall footer pushes the body's bottom up.
            body_bottom = body_bottom.max(dy);
            hf_frags.append(&mut band.frags);
            hf_links.append(&mut band.links);
        }
        self.pages.push(Page {
            w: s.w,
            h: s.h,
            sect,
            number,
            first,
            blank,
            left,
            right,
            body_top,
            body_bottom,
            frags: Vec::new(),
            links: Vec::new(),
            hf_frags,
            hf_links,
            watermarks,
            rules: Vec::new(),
            lines: Vec::new(),
            min_base: f32::INFINITY,
            regions: 0,
            multi_col: false,
            members: vec![sect],
            values: Default::default(),
        });
        self.start_region(sect, body_top);
    }

    fn start_region(&mut self, sect: usize, top: f32) {
        self.close_region();
        let s = &self.sects[sect];
        let page = self.pages.last_mut().expect("a page exists");
        let (left, _) = s.h_margins(page.number.is_multiple_of(2), self.opts);
        let mut x = left;
        let mut xs = Vec::with_capacity(s.cols.len());
        for &(w, space) in &s.cols {
            xs.push((x, w));
            x += w + space;
        }
        self.next_region += 1;
        self.region = Region {
            id: self.next_region,
            sect,
            top,
            col: 0,
            low: top,
            xs,
            placed: false,
            column_break: false,
        };
        self.y = top;
    }

    /// Draw the ending column set's separator rules.
    fn close_region(&mut self) {
        let xs = std::mem::take(&mut self.region.xs);
        let (top, low) = (self.region.top, self.region.low);
        let Some(s) = self.sects.get(self.region.sect) else {
            return;
        };
        if !s.col_sep || xs.len() < 2 || low >= top {
            return;
        }
        let Some(page) = self.pages.last_mut() else {
            return;
        };
        for pair in xs.windows(2) {
            let x = (pair[0].0 + pair[0].1 + pair[1].0) / 2.0;
            page.rules.push(Rule {
                x1: x,
                y1: top,
                x2: x,
                y2: low,
                width: 0.5,
                color: (0.0, 0.0, 0.0),
                dash: Vec::new(),
            });
        }
    }

    fn advance_column(&mut self) {
        if self.region.col + 1 < self.region.xs.len() {
            self.region.col += 1;
            self.y = self.region.top;
        } else {
            self.new_page(self.region.sect, false, false);
        }
    }

    /// Balance the ending column set's lines on this page before a continuous
    /// section starts below it, as Word does: re-pour them in order into its
    /// columns so the tallest column is as short as possible. Lines don't
    /// re-wrap, so only equal-width columns are balanced, and not after a
    /// column break (which placed the lines deliberately).
    fn balance_region(&mut self) {
        let region = &self.region;
        let Some(&(_, w0)) = region.xs.first() else {
            return;
        };
        if region.xs.len() < 2
            || region.column_break
            || region.xs.iter().any(|&(_, w)| (w - w0).abs() > 0.01)
        {
            return;
        }
        let Some(page) = self.pages.last_mut() else {
            return;
        };
        let first = page
            .lines
            .iter()
            .position(|l| l.region == region.id)
            .unwrap_or(page.lines.len());
        let lines: Vec<(f32, f32)> = page.lines[first..]
            .iter()
            .map(|l| (l.lh, l.gap_after))
            .collect();
        if lines.is_empty() {
            return;
        }
        let cols = region.xs.len();
        // A line fits a column while its baseline stays on the body, whatever
        // the gap after it.
        let room = region.top - page.body_bottom + 0.001;
        // Greedy pour at a column height `h` (lines and their gaps): each
        // line's column, or None when the lines need more columns than there
        // are.
        let pour = |h: f32| -> Option<Vec<usize>> {
            let (mut col, mut used) = (0, 0.0);
            let mut out = Vec::with_capacity(lines.len());
            for &(lh, gap) in &lines {
                if used > 0.0 && (used + lh + gap > h + 0.001 || used + lh > room) {
                    col += 1;
                    used = 0.0;
                }
                if col >= cols {
                    return None;
                }
                used += lh + gap;
                out.push(col);
            }
            Some(out)
        };
        // The pour is monotone in `h`: bisect for the smallest that fits.
        let (mut lo, mut hi) = (
            lines.iter().map(|&(lh, gap)| lh + gap).fold(0.0, f32::max),
            lines.iter().map(|&(lh, gap)| lh + gap).sum::<f32>(),
        );
        if pour(hi).is_none() {
            return;
        }
        for _ in 0..40 {
            let mid = (lo + hi) / 2.0;
            if pour(mid).is_some() {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let Some(assign) = pour(hi) else {
            return;
        };
        // Where each line goes; keep the layout unless it gets shorter.
        let mut cursor = vec![region.top; cols];
        let mut moves = Vec::with_capacity(assign.len());
        for (k, col) in assign.into_iter().enumerate() {
            let line = &page.lines[first + k];
            let y = cursor[col] - line.lh;
            moves.push((col, y));
            cursor[col] = y - line.gap_after;
        }
        let low = cursor.into_iter().fold(region.top, f32::min);
        if low <= region.low + 0.001 {
            return;
        }
        for (k, (col, y)) in moves.into_iter().enumerate() {
            let i = first + k;
            let line = page.lines[i];
            let dx = region.xs[col].0 - region.xs[line.col].0;
            page.shift_line(i, dx, y - line.y);
            page.lines[i].col = col;
        }
        page.min_base = page.lines.iter().map(|l| l.y).fold(f32::INFINITY, f32::min);
        self.region.low = low;
    }

    /// Count a body line just placed for line numbering (`w:lnNumType`): the
    /// number to print left of it, if any, and its distance from the text.
    fn count_line(&mut self) -> Option<(u32, f32)> {
        let sect = self.region.sect;
        let ln = self.sects[sect].line_numbers?;
        if !self.numbered {
            return None;
        }
        let page = self.pages.len() - 1;
        let restart = match (self.line_count.last, ln.restart) {
            (None, _) => true,
            (Some((p, _)), LnRestart::NewPage) => p != page,
            (Some((_, s)), LnRestart::NewSection) => s != sect,
            (Some(_), LnRestart::Continuous) => false,
        };
        if restart {
            self.line_count.count = ln.start;
        }
        self.line_count.count += 1;
        self.line_count.last = Some((page, sect));
        let n = self.line_count.count;
        n.is_multiple_of(ln.count_by).then_some((n, ln.distance))
    }

    fn mark_low(&mut self) {
        self.region.low = self.region.low.min(self.y);
    }

    /// Vertical alignment, page borders and field values, once every page exists.
    fn finish(mut self) -> Vec<Page> {
        let total = self.pages.len();
        // A section counts every page it has content on.
        let mut per_section: HashMap<usize, usize> = HashMap::new();
        for page in &self.pages {
            for &sect in &page.members {
                *per_section.entry(sect).or_default() += 1;
            }
        }
        for page in &mut self.pages {
            let s = &self.sects[page.sect];
            if !page.blank
                && page.regions == 1
                && !page.multi_col
                && page.min_base.is_finite()
                && s.valign != PageVAlign::Top
            {
                let free = (page.min_base - page.body_bottom).max(0.0);
                if s.valign == PageVAlign::Both {
                    justify_page(page, free);
                } else {
                    let shift = if s.valign == PageVAlign::Center {
                        free / 2.0
                    } else {
                        free
                    };
                    for f in &mut page.frags {
                        f.y -= shift;
                    }
                    for (rect, _) in &mut page.links {
                        rect.1 -= shift;
                        rect.3 -= shift;
                    }
                }
            }
            let show_borders = match s.borders.display {
                BorderDisplay::AllPages => true,
                BorderDisplay::FirstPage => page.first,
                BorderDisplay::NotFirstPage => !page.first,
            };
            if show_borders && s.borders.sides.iter().any(Option::is_some) {
                page.rules.extend(border_rules(
                    s, self.opts, page.w, page.h, page.left, page.right,
                ));
            }
            let section_pages = |sect: usize| per_section.get(&sect).copied().unwrap_or(1);
            page.values = [
                format_page_number(page.number, s.num_fmt),
                total.to_string(),
                section_pages(page.sect).to_string(),
            ];
            // A body SECTIONPAGES counts its own section's pages.
            for f in &mut page.frags {
                if let (Some((PageField::SectionPages, true)), Some(sect)) = (f.field, f.sect) {
                    f.text = section_pages(sect).to_string();
                    f.field = None;
                }
            }
        }
        self.pages
    }
}

/// `w:vAlign="both"`: spread the page's paragraphs evenly over its `free`
/// space, the first staying at the top and the last ending on the body's
/// bottom. A page with one paragraph stays top-aligned.
fn justify_page(page: &mut Page, free: f32) {
    let mut ordinals = Vec::with_capacity(page.lines.len());
    let mut n = 0usize;
    let mut last = None;
    for line in &page.lines {
        if last != Some(line.para) {
            last = Some(line.para);
            n += 1;
        }
        ordinals.push(n - 1);
    }
    if n < 2 {
        return;
    }
    let step = free / (n - 1) as f32;
    for (i, k) in ordinals.into_iter().enumerate() {
        page.shift_line(i, 0.0, -(k as f32) * step);
    }
}

/// The page border's four lines. Offsets are measured from the page edge
/// (`w:offsetFrom="page"`) or outward from the text margins.
fn border_rules(
    s: &SectionLayout,
    opts: &PdfOptions,
    w: f32,
    h: f32,
    left: f32,
    right: f32,
) -> Vec<Rule> {
    let b = &s.borders;
    let space = |i: usize| b.sides[i].as_ref().map_or(0.0, |side| side.space);
    let (top_y, left_x, bottom_y, right_x) = if b.from_page {
        (h - space(0), space(1), space(2), w - space(3))
    } else {
        (
            h - s.top_margin(opts) + space(0),
            left - space(1),
            s.bottom - space(2),
            w - right + space(3),
        )
    };
    let lines = [
        (left_x, top_y, right_x, top_y),
        (left_x, bottom_y, left_x, top_y),
        (left_x, bottom_y, right_x, bottom_y),
        (right_x, bottom_y, right_x, top_y),
    ];
    // Outward (away from the text) for each side.
    let outward: [(f32, f32); 4] = [(0.0, 1.0), (-1.0, 0.0), (0.0, -1.0), (1.0, 0.0)];
    let mut rules = Vec::new();
    for ((side, (x1, y1, x2, y2)), (ox, oy)) in b.sides.iter().zip(lines).zip(outward) {
        let Some(side) = side else {
            continue;
        };
        let w = side.width;
        // Each stroke's width and its distance outward from the frame line.
        let (strokes, dash): (Vec<(f32, f32)>, Vec<f32>) = match &side.style {
            BorderStyle::Single => (vec![(w, 0.0)], Vec::new()),
            BorderStyle::Dashes(pattern) => {
                (vec![(w, 0.0)], pattern.iter().map(|k| k * w).collect())
            }
            BorderStyle::Art => (vec![(w, 0.0)], vec![w, w]),
            BorderStyle::Lines { thin, gap } => {
                let thin_w = (w / 3.0).max(0.25);
                let widths: Vec<f32> = thin.iter().map(|&t| if t { thin_w } else { w }).collect();
                // Double and triple lines are spaced by their own width; the
                // thin-thick families by thin widths.
                let gap = if thin.iter().any(|&t| t) {
                    gap * thin_w
                } else {
                    gap * w
                };
                let mut d = 0.0;
                let mut out = Vec::new();
                for (k, &sw) in widths.iter().enumerate() {
                    if k > 0 {
                        d += widths[k - 1] / 2.0 + gap + sw / 2.0;
                    }
                    out.push((sw, d));
                }
                (out, Vec::new())
            }
        };
        for (width, d) in strokes {
            // Along the frame inflated by `d`, so the corners stay closed: a
            // horizontal side grows by `d` at both ends, a vertical one too.
            let (ax, ay) = (oy.abs(), ox.abs());
            rules.push(Rule {
                x1: x1 + ox * d - ax * d,
                y1: y1 + oy * d - ay * d,
                x2: x2 + ox * d + ax * d,
                y2: y2 + oy * d + ay * d,
                width,
                color: side.color,
                dash: dash.clone(),
            });
        }
    }
    rules
}

impl Flow for Pager<'_> {
    fn begin_paragraph(&mut self, info: ParaInfo) {
        self.para += 1;
        self.numbered = info.numbered;
    }
    fn next_line(&mut self, lh: f32) -> (f32, f32, f32) {
        self.y -= lh;
        // Move on until the line fits: a continuous column set that starts low
        // on the page can have no room in any of its columns. A line taller
        // than an empty page is placed anyway.
        loop {
            let bottom = self.pages.last().map_or(0.0, |p| p.body_bottom);
            if self.y >= bottom {
                break;
            }
            let new_page = self.region.col + 1 >= self.region.xs.len();
            self.advance_column();
            self.y -= lh;
            if new_page {
                break;
            }
        }
        self.mark_low();
        let (x, w) = self.region.xs[self.region.col];
        let number = self.count_line();
        let page = self.pages.last_mut().expect("a page exists");
        page.min_base = page.min_base.min(self.y);
        page.lines.push(LineRec {
            y: self.y,
            lh,
            col: self.region.col,
            region: self.region.id,
            para: self.para,
            frags: page.frags.len(),
            links: page.links.len(),
            gap_after: 0.0,
        });
        if let Some((n, distance)) = number {
            let text = n.to_string();
            let size = self.opts.base_font_size;
            page.frags.push(Frag {
                x: x - distance - text.len() as f32 * 0.6 * size,
                y: self.y,
                text,
                size,
                font: 0,
                color: (0.0, 0.0, 0.0),
                underline: false,
                strike: false,
                field: None,
                sect: None,
            });
        }
        // Count the column set, and its section, only once a line lands, so a
        // continuous section that overflows at once doesn't claim this page.
        if !self.region.placed {
            self.region.placed = true;
            page.regions += 1;
            page.multi_col |= self.region.xs.len() > 1;
            if !page.members.contains(&self.region.sect) {
                page.members.push(self.region.sect);
            }
        }
        (x, w, self.y)
    }
    fn push(&mut self, mut frag: Frag, link: Option<Link>) {
        if frag.field.is_some() {
            frag.sect = Some(self.region.sect);
        }
        let page = self.pages.last_mut().expect("a page exists");
        page.frags.push(frag);
        page.links.extend(link);
    }
    fn gap(&mut self, dy: f32) {
        self.y -= dy;
        self.mark_low();
        let region = self.region.id;
        if let Some(line) = self
            .pages
            .last_mut()
            .and_then(|p| p.lines.last_mut())
            .filter(|l| l.region == region)
        {
            line.gap_after += dy;
        }
    }
    fn hard_break(&mut self, kind: BreakKind) {
        match kind {
            BreakKind::Page => self.new_page(self.region.sect, false, false),
            BreakKind::Column => {
                self.region.column_break = true;
                self.advance_column();
            }
            BreakKind::Line | BreakKind::Clear(_) => {}
        }
    }
    fn at_top(&self) -> bool {
        self.pages
            .last()
            .is_none_or(|p| p.frags.is_empty() && self.y >= p.body_top)
    }
}

/// The character a PDF prints for `ch`, if any (#1101). A soft hyphen
/// prints only at a line break, which this layout never hyphenates at, so it
/// is left out; a non-breaking hyphen has no WinAnsi byte and prints as a
/// hyphen.
fn printed_char(ch: char) -> Option<char> {
    match ch {
        '\u{ad}' => None,
        '\u{2011}' => Some('-'),
        _ => Some(ch),
    }
}

fn plain_cell(ch: char) -> PCell {
    PCell {
        ch,
        font: 0,
        color: (0.0, 0.0, 0.0),
        underline: false,
        strike: false,
        link: None,
        field: None,
    }
}

fn same_style(a: &PCell, b: &PCell) -> bool {
    a.font == b.font
        && a.color == b.color
        && a.underline == b.underline
        && a.strike == b.strike
        && a.link.as_deref() == b.link.as_deref()
        && a.field.map(|f| f.0) == b.field.map(|f| f.0)
}

/// An open complex field while its paragraph is flattened.
struct OpenField {
    instr: String,
    separated: bool,
    kind: Option<PageField>,
    emitted: bool,
}

/// What a hyperlink shows, in order: [`Hyperlink::visible_pieces`] with its
/// tabs and breaks.
enum LinkPiece<'a> {
    Text(&'a str, RunProps),
    Tab,
    Break(BreakKind),
}

fn link_pieces<'a>(runs: &'a [Run], content: &'a [Inline], out: &mut Vec<LinkPiece<'a>>) {
    out.extend(
        runs.iter()
            .map(|r| LinkPiece::Text(r.text.as_str(), r.props.clone())),
    );
    for inline in content {
        match inline {
            Inline::Run(run) => out.push(LinkPiece::Text(run.text.as_str(), run.props.clone())),
            Inline::Hyperlink(link) => link_pieces(&link.runs, &link.content, out),
            Inline::Revision { content, .. } => link_pieces(&[], content, out),
            Inline::Field { raw, text } => out.push(LinkPiece::Text(
                text.as_str(),
                crate::load::field_result_props(raw),
            )),
            Inline::Tab(_) => out.push(LinkPiece::Tab),
            Inline::Break(kind, _) => out.push(LinkPiece::Break(*kind)),
            _ => {}
        }
    }
}

fn flatten_segments(p: &Paragraph, heading: bool, styles: &StyleSheet) -> Vec<Seg> {
    let pstyle = p.props.style_id.as_deref();
    let mut segs: Vec<Seg> = vec![Seg::default()];
    // Complex fields: only the outermost field's result is replaced.
    let mut fields: Vec<OpenField> = Vec::new();
    fn push(segs: &mut [Seg], mut cell: PCell) {
        let Some(ch) = printed_char(cell.ch) else {
            return;
        };
        cell.ch = ch;
        segs.last_mut().unwrap().cells.push(cell);
    }
    fn new_line(segs: &mut Vec<Seg>) {
        if !segs.last().map(|s| s.cells.is_empty()).unwrap_or(true) {
            segs.push(Seg::default());
        }
    }
    for item in &p.content {
        match item {
            Inline::Run(r) => {
                let eff = styles.effective_run(pstyle, r.props.style_id.as_deref(), &r.props);
                let font = font_index(eff.bold || heading, eff.italic);
                let color = run_color(&eff);
                let outer = fields
                    .first_mut()
                    .filter(|f| f.separated && f.kind.is_some());
                let mut mark = outer.map(|f| (f.kind.unwrap(), &mut f.emitted));
                // Only a printed character carries a page field's marker: a
                // soft hyphen first would take it along when left out.
                for ch in r.text.chars().filter_map(printed_char) {
                    let field = mark.as_mut().map(|(kind, emitted)| {
                        let first = !**emitted;
                        **emitted = true;
                        (*kind, first)
                    });
                    push(
                        &mut segs,
                        PCell {
                            ch,
                            font,
                            color,
                            underline: eff.underline,
                            strike: eff.strike,
                            link: None,
                            field,
                        },
                    );
                }
            }
            Inline::Hyperlink(h) => {
                let target = h
                    .target
                    .clone()
                    .or_else(|| h.anchor.as_ref().map(|a| format!("#{a}")))
                    .unwrap_or_default();
                let rc: Rc<str> = Rc::from(target.as_str());
                let mut pieces = Vec::new();
                link_pieces(&h.runs, &h.content, &mut pieces);
                for piece in pieces {
                    let (text, props) = match piece {
                        LinkPiece::Text(text, props) => (text, props),
                        // A tab or break inside the link (a TOC entry's tab
                        // before its page number) lays out as one outside.
                        LinkPiece::Tab => {
                            for _ in 0..4 {
                                push(
                                    &mut segs,
                                    PCell {
                                        link: Some(rc.clone()),
                                        ..plain_cell(' ')
                                    },
                                );
                            }
                            continue;
                        }
                        LinkPiece::Break(kind) => {
                            segs.last_mut().unwrap().brk = Some(kind);
                            segs.push(Seg::default());
                            continue;
                        }
                    };
                    let eff = styles.effective_run(pstyle, props.style_id.as_deref(), &props);
                    let (font, strike) = (font_index(eff.bold || heading, eff.italic), eff.strike);
                    for ch in text.chars() {
                        push(
                            &mut segs,
                            PCell {
                                ch,
                                font,
                                color: (0.0, 0.0, 0.55),
                                underline: true,
                                strike,
                                link: Some(rc.clone()),
                                field: None,
                            },
                        );
                    }
                }
            }
            Inline::Tab(_) => {
                for _ in 0..4 {
                    push(&mut segs, plain_cell(' '));
                }
            }
            Inline::Break(kind, _) => {
                segs.last_mut().unwrap().brk = Some(*kind);
                segs.push(Seg::default());
            }
            // SmartArt: the terminal can't draw the diagram, so lay its node text
            // out as plain lines ("SmartArt" caption first) in the PDF too.
            Inline::SmartArt { text, .. } => {
                for line in std::iter::once("SmartArt").chain(text.iter().map(|s| s.as_str())) {
                    new_line(&mut segs);
                    for ch in line.chars() {
                        push(&mut segs, plain_cell(ch));
                    }
                    segs.push(Seg::default());
                }
            }
            // A chart: lay its text bar/pie view out on its own lines.
            Inline::Chart { chart, .. } => {
                for line in crate::chart::render_chart(chart, 80) {
                    new_line(&mut segs);
                    for ch in line.chars() {
                        push(&mut segs, plain_cell(ch));
                    }
                    segs.push(Seg::default());
                }
            }
            // A field (a `w:fldSimple`, a complex field loaded as one unit, or a
            // `w:sym` symbol): PAGE/NUMPAGES/SECTIONPAGES get the page's value
            // when written; any other field keeps its cached result.
            Inline::Field { raw, text } => {
                let kind = crate::field::instr_of(raw)
                    .as_deref()
                    .and_then(page_field_kind);
                let printed: String = text.chars().filter_map(printed_char).collect();
                let text = if kind.is_some() && printed.is_empty() {
                    "#"
                } else {
                    printed.as_str()
                };
                // The result's own formatting (#642: a complex field's result
                // runs are inside the Field).
                let props = crate::load::field_result_props(raw);
                let eff = styles.effective_run(pstyle, props.style_id.as_deref(), &props);
                for (i, ch) in text.chars().enumerate() {
                    let mut cell = plain_cell(ch);
                    cell.font = font_index(eff.bold || heading, eff.italic);
                    cell.color = run_color(&eff);
                    cell.underline = eff.underline;
                    cell.strike = eff.strike;
                    cell.field = kind.map(|k| (k, i == 0));
                    push(&mut segs, cell);
                }
            }
            // A decoded equation flows inline as plain text.
            Inline::Equation { text, .. } => {
                for ch in text.chars() {
                    push(&mut segs, plain_cell(ch));
                }
            }
            // A tracked change: lay its inner text out inline.
            Inline::Revision { content, .. } => {
                for ch in content
                    .iter()
                    .flat_map(|i| i.text().chars().collect::<Vec<_>>())
                {
                    push(&mut segs, plain_cell(ch));
                }
            }
            // A footnote/endnote reference: the note number as inline text.
            Inline::FootnoteRef { id, .. } => {
                for ch in id.to_string().chars() {
                    push(&mut segs, plain_cell(ch));
                }
            }
            // A text box: lay its text out on its own lines.
            Inline::TextBox { blocks, .. } => {
                for line in blocks.iter().flat_map(|b| {
                    b.plain_text()
                        .lines()
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                }) {
                    new_line(&mut segs);
                    for ch in line.chars() {
                        push(&mut segs, plain_cell(ch));
                    }
                    segs.push(Seg::default());
                }
            }
            // Complex field markers (`fldChar`/`instrText`) live in raw runs.
            Inline::Raw(raw) => {
                for event in field_events(raw) {
                    match event {
                        FieldEvent::Begin => fields.push(OpenField {
                            instr: String::new(),
                            separated: false,
                            kind: None,
                            emitted: false,
                        }),
                        FieldEvent::Instr(text) => {
                            if let Some(f) = fields.last_mut()
                                && !f.separated
                            {
                                f.instr.push_str(&text);
                            }
                        }
                        FieldEvent::Separate => {
                            let depth = fields.len();
                            if let Some(f) = fields.last_mut() {
                                f.separated = true;
                                if depth == 1 {
                                    f.kind = page_field_kind(&f.instr);
                                }
                            }
                        }
                        FieldEvent::End => {
                            let depth = fields.len();
                            if let Some(mut f) = fields.pop() {
                                if depth == 1 && !f.separated {
                                    f.kind = page_field_kind(&f.instr);
                                }
                                // A page field with no cached result still shows
                                // its value.
                                if let Some(kind) = f.kind.filter(|_| depth == 1 && !f.emitted) {
                                    let mut cell = plain_cell('#');
                                    cell.field = Some((kind, true));
                                    push(&mut segs, cell);
                                }
                            }
                        }
                    }
                }
            }
            Inline::UnsupportedRevision { .. } => {}
        }
    }
    segs
}

/// Take one line of at most `width` cells, breaking at the last space that
/// fits (or hard at `width`); returns the line (trailing spaces trimmed) and
/// how many cells it consumed.
fn take_line(cells: &[PCell], width: usize) -> (Vec<PCell>, usize) {
    let width = width.max(1);
    let (mut line, used) = if cells.len() <= width {
        (cells.to_vec(), cells.len())
    } else if let Some(sp) = cells[..=width].iter().rposition(|c| c.ch == ' ') {
        (cells[..=sp].to_vec(), sp + 1)
    } else {
        (cells[..width].to_vec(), width)
    };
    while line.last().is_some_and(|c| c.ch == ' ') {
        line.pop();
    }
    (line, used)
}

// ---- PDF serialization ----

/// Encode a char to a single WinAnsi byte (best effort; unknowns -> '?').
fn winansi(ch: char) -> u8 {
    let u = ch as u32;
    match u {
        0x2022 => 0x95, // bullet
        0x2014 => 0x97, // em dash
        0x2013 => 0x96, // en dash
        0x2018 => 0x91,
        0x2019 => 0x92,
        0x201C => 0x93,
        0x201D => 0x94,
        _ if u <= 0xFF => u as u8,
        _ => b'?',
    }
}

/// Escape a string as a PDF literal string body (WinAnsi bytes).
fn pdf_string_body(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        let b = winansi(ch);
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(b);
            }
            _ => out.push(b),
        }
    }
    out
}

fn build_content(page: &Page, opts: &PdfOptions) -> Vec<u8> {
    let mut s: Vec<u8> = Vec::new();
    if let Some(rgb) = opts.background {
        let (r, g, b) = rgb_f(rgb);
        s.extend(
            format!(
                "{r:.3} {g:.3} {b:.3} rg 0 0 {:.2} {:.2} re f\n",
                page.w, page.h
            )
            .as_bytes(),
        );
    }
    for mark in &page.watermarks {
        let (sin, cos) = mark.angle.to_radians().sin_cos();
        let (r, g, b) = mark.color;
        let width = mark.text.chars().count() as f32 * 0.6 * mark.size;
        s.extend(
            format!(
                "q\n{r:.3} {g:.3} {b:.3} rg\n{cos:.4} {sin:.4} {:.4} {cos:.4} {:.2} {:.2} cm\n",
                -sin, mark.cx, mark.cy
            )
            .as_bytes(),
        );
        // Centred on the origin: half the width left, a third of the size down.
        s.extend(
            format!(
                "BT /F0 {:.2} Tf {:.2} {:.2} Td (",
                mark.size,
                -width / 2.0,
                -mark.size * 0.3
            )
            .as_bytes(),
        );
        s.extend(pdf_string_body(&mark.text));
        s.extend(b") Tj ET\nQ\n");
    }
    for rule in &page.rules {
        if !rule.dash.is_empty() {
            let dash: Vec<String> = rule.dash.iter().map(|d| format!("{d:.2}")).collect();
            s.extend(format!("[{}] 0 d\n", dash.join(" ")).as_bytes());
        }
        s.extend(
            format!(
                "{:.3} {:.3} {:.3} RG {:.2} w {:.2} {:.2} m {:.2} {:.2} l S\n",
                rule.color.0,
                rule.color.1,
                rule.color.2,
                rule.width,
                rule.x1,
                rule.y1,
                rule.x2,
                rule.y2
            )
            .as_bytes(),
        );
        if !rule.dash.is_empty() {
            s.extend(b"[] 0 d\n");
        }
    }
    for f in page.hf_frags.iter().chain(&page.frags) {
        let text: &str = match f.field {
            Some((_, false)) => continue,
            Some((kind, true)) => &page.values[kind as usize],
            None => &f.text,
        };
        let n_chars = text.chars().count() as f32;
        let advance = 0.6 * f.size;
        let width = n_chars * advance;
        // fill color for text
        s.extend(format!("{:.3} {:.3} {:.3} rg\n", f.color.0, f.color.1, f.color.2).as_bytes());
        s.extend(
            format!(
                "BT /F{} {:.2} Tf {:.2} {:.2} Td (",
                f.font, f.size, f.x, f.y
            )
            .as_bytes(),
        );
        s.extend(pdf_string_body(text));
        s.extend(b") Tj ET\n");
        if f.underline || f.strike {
            s.extend(
                format!(
                    "{:.3} {:.3} {:.3} RG 0.6 w\n",
                    f.color.0, f.color.1, f.color.2
                )
                .as_bytes(),
            );
            if f.underline {
                let uy = f.y - 1.5;
                s.extend(
                    format!("{:.2} {:.2} m {:.2} {:.2} l S\n", f.x, uy, f.x + width, uy).as_bytes(),
                );
            }
            if f.strike {
                let sy = f.y + f.size * 0.28;
                s.extend(
                    format!("{:.2} {:.2} m {:.2} {:.2} l S\n", f.x, sy, f.x + width, sy).as_bytes(),
                );
            }
        }
    }
    s
}

fn write_pdf(pages: &[Page], opts: &PdfOptions) -> Vec<u8> {
    // Object ids: 1=Catalog, 2=Pages, 3..6=Fonts, then per-page content/annot/page.
    let mut objs: Vec<Vec<u8>> = vec![Vec::new(), Vec::new()];
    const FONTS: [&str; 4] = [
        "Courier",
        "Courier-Bold",
        "Courier-Oblique",
        "Courier-BoldOblique",
    ];
    for name in FONTS {
        objs.push(
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /{name} /Encoding /WinAnsiEncoding >>"
            )
            .into_bytes(),
        );
    }

    let mut page_ids: Vec<usize> = Vec::new();
    for page in pages {
        let content = build_content(page, opts);
        objs.push(
            [
                format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
                content,
                b"\nendstream".to_vec(),
            ]
            .concat(),
        );
        let content_id = objs.len();

        let mut annot_ids: Vec<usize> = Vec::new();
        for (rect, uri) in page.hf_links.iter().chain(&page.links) {
            let mut obj = format!(
                "<< /Type /Annot /Subtype /Link /Rect [{:.2} {:.2} {:.2} {:.2}] /Border [0 0 0] /A << /S /URI /URI (",
                rect.0, rect.1, rect.2, rect.3
            )
            .into_bytes();
            obj.extend(pdf_string_body(uri));
            obj.extend(b") >> >>");
            objs.push(obj);
            annot_ids.push(objs.len());
        }

        let mut page_obj = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.2} {:.2}] /Resources << /Font << /F0 3 0 R /F1 4 0 R /F2 5 0 R /F3 6 0 R >> >> /Contents {content_id} 0 R",
            page.w, page.h
        );
        if !annot_ids.is_empty() {
            page_obj.push_str(" /Annots [");
            for id in &annot_ids {
                page_obj.push_str(&format!("{id} 0 R "));
            }
            page_obj.push(']');
        }
        page_obj.push_str(" >>");
        objs.push(page_obj.into_bytes());
        page_ids.push(objs.len());
    }

    objs[0] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
    let mut kids = String::new();
    for id in &page_ids {
        kids.push_str(&format!("{id} 0 R "));
    }
    objs[1] = format!(
        "<< /Type /Pages /Kids [{kids}] /Count {} >>",
        page_ids.len()
    )
    .into_bytes();

    // Serialize with a cross-reference table.
    let mut out: Vec<u8> = Vec::new();
    out.extend(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets: Vec<usize> = Vec::with_capacity(objs.len());
    for (i, obj) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(obj);
        out.extend(b"\nendobj\n");
    }
    let xref_pos = out.len();
    out.extend(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
    out.extend(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objs.len() + 1,
            xref_pos
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn run(text: &str, props: RunProps) -> Inline {
        Inline::Run(Run {
            text: text.to_string(),
            props,
        })
    }
    fn para(content: Vec<Inline>) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content,
        })
    }
    fn doc(blocks: Vec<Block>) -> Document {
        Document { body: blocks }
    }
    fn s(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn well_formed_pdf_envelope() {
        let d = doc(vec![para(vec![run("Hello PDF", RunProps::default())])]);
        let pdf = to_pdf(&d, &PdfOptions::default());
        assert!(pdf.starts_with(b"%PDF-1."));
        let text = s(&pdf);
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/Type /Pages"));
        assert!(text.contains("/Type /Page"));
        assert!(text.contains("BaseFont /Courier"));
        assert!(text.contains("xref"));
        assert!(text.trim_end().ends_with("%%EOF"));
    }

    #[test]
    fn deterministic_output() {
        let d = doc(vec![para(vec![run("repeatable", RunProps::default())])]);
        let a = to_pdf(&d, &PdfOptions::default());
        let b = to_pdf(&d, &PdfOptions::default());
        assert_eq!(a, b);
    }

    #[test]
    fn bold_run_uses_bold_font() {
        let bold = RunProps {
            bold: true,
            ..RunProps::default()
        };
        let d = doc(vec![para(vec![run("x", bold)])]);
        let text = s(&to_pdf(&d, &PdfOptions::default()));
        // bold = font index 1 = /F1
        assert!(text.contains("/F1"));
    }

    #[test]
    fn a_fields_bold_result_uses_the_bold_font_642() {
        let d = crate::load::parse_document_xml(
            "<w:document><w:body><w:p><w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText> PAGE </w:instrText></w:r><w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r></w:p></w:body></w:document>",
            &crate::load::Relationships::default(),
        );
        assert!(
            matches!(&d.body[0], Block::Paragraph(p) if matches!(p.content[0], Inline::Field { .. }))
        );
        let text = s(&to_pdf(&d, &PdfOptions::default()));
        let shown = text
            .find("(1) Tj")
            .or_else(|| text.find("(1)"))
            .expect("the result is drawn");
        let font = text[..shown].rfind("BT /F").expect("a font is set");
        assert_eq!(&text[font..font + 6], "BT /F1", "the result is bold");
    }

    #[test]
    fn paragraph_style_makes_pdf_bold() {
        let ss = crate::styles::parse_styles_xml(
            r#"<w:styles><w:style w:styleId="S"><w:rPr><w:b/></w:rPr></w:style></w:styles>"#,
        );
        let pr = ParProps {
            style_id: Some("S".to_string()),
            ..ParProps::default()
        };
        let d = doc(vec![Block::Paragraph(Paragraph {
            props: pr,
            content: vec![run("x", RunProps::default())],
        })]);
        let opts = PdfOptions {
            styles: std::rc::Rc::new(ss),
            ..PdfOptions::default()
        };
        let text = s(&to_pdf(&d, &opts));
        assert!(text.contains("/F1"), "style-derived bold font not used");
    }

    #[test]
    fn hyperlink_emits_uri_annotation() {
        let h = Inline::Hyperlink(Hyperlink {
            target: Some("https://example.org/".to_string()),
            anchor: None,
            rel_id: None,
            runs: vec![Run {
                text: "site".to_string(),
                props: RunProps::default(),
            }],
            ..Hyperlink::default()
        });
        let d = doc(vec![para(vec![h])]);
        let text = s(&to_pdf(&d, &PdfOptions::default()));
        assert!(text.contains("/Subtype /Link"));
        assert!(text.contains("/URI (https://example.org/)"));
    }

    #[test]
    fn many_paragraphs_paginate() {
        let blocks: Vec<Block> = (0..200)
            .map(|i| para(vec![run(&format!("line {i}"), RunProps::default())]))
            .collect();
        let d = doc(blocks);
        let text = s(&to_pdf(&d, &PdfOptions::default()));
        let pages = text.matches("/Type /Page\n").count() + text.matches("/Type /Page ").count();
        // crude: count /Contents references (one per page)
        let contents = text.matches("/Contents ").count();
        assert!(contents >= 2, "expected multiple pages, got {contents}");
        let _ = pages;
    }

    #[test]
    fn underline_draws_a_rule() {
        let u = RunProps {
            underline: true,
            ..RunProps::default()
        };
        let d = doc(vec![para(vec![run("under", u)])]);
        let text = s(&to_pdf(&d, &PdfOptions::default()));
        // a stroked line: "... l S"
        assert!(text.contains(" l S"));
    }

    #[test]
    fn bullet_is_winansi_encoded() {
        let mut p = Paragraph {
            props: ParProps::default(),
            content: vec![run("item", RunProps::default())],
        };
        p.props.num_id = Some(1);
        let d = doc(vec![Block::Paragraph(p)]);
        let pdf = to_pdf(&d, &PdfOptions::default());
        // 0x95 is the WinAnsi bullet byte; must appear in a content stream.
        assert!(pdf.windows(1).any(|w| w == [0x95]));
    }

    /// A non-breaking hyphen prints as a hyphen (WinAnsi has no U+2011), a
    /// soft hyphen not at all (#1101).
    #[test]
    fn hyphens_print_as_a_hyphen_and_nothing() {
        let d = doc(vec![text_para("e\u{2011}commerce co\u{ad}op")]);
        let texts: String = pages_of(&d, &PdfOptions::default())
            .iter()
            .flat_map(|p| p.texts.iter().map(|t| t.2.clone()))
            .collect();
        assert!(texts.contains("e-commerce coop"), "{texts:?}");
    }

    // ---- page layout (#637) ----

    /// One page of a written PDF: its MediaBox, its content stream, and each
    /// `Td` text draw as (x, y, text).
    struct PdfPage {
        media: String,
        content: String,
        texts: Vec<(f32, f32, String)>,
    }

    impl PdfPage {
        fn has(&self, text: &str) -> bool {
            self.texts.iter().any(|(_, _, t)| t.contains(text))
        }
        fn at(&self, text: &str) -> (f32, f32) {
            self.texts
                .iter()
                .find(|(_, _, t)| t.contains(text))
                .map(|(x, y, _)| (*x, *y))
                .unwrap_or_else(|| panic!("{text:?} not on page: {:?}", self.texts))
        }
        /// Texts drawn exactly as `text`.
        fn exact(&self, text: &str) -> bool {
            self.texts.iter().any(|(_, _, t)| t == text)
        }
    }

    fn parse_pages(pdf: &[u8]) -> Vec<PdfPage> {
        let text = s(pdf);
        let mut objs: HashMap<usize, &str> = HashMap::new();
        let mut from = 0;
        while let Some(off) = text[from..].find(" 0 obj\n") {
            let at = from + off;
            let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let id: usize = text[line_start..at].parse().unwrap();
            let body_start = at + " 0 obj\n".len();
            let body_end = body_start + text[body_start..].find("\nendobj\n").unwrap();
            objs.insert(id, &text[body_start..body_end]);
            from = body_end;
        }
        let mut ids: Vec<usize> = objs.keys().copied().collect();
        ids.sort();
        ids.into_iter()
            .filter(|id| objs[id].starts_with("<< /Type /Page /Parent"))
            .map(|id| {
                let body = objs[&id];
                let media = body.split("/MediaBox [").nth(1).unwrap();
                let media = media[..media.find(']').unwrap()].to_string();
                let cid = body.split("/Contents ").nth(1).unwrap();
                let cid: usize = cid[..cid.find(' ').unwrap()].parse().unwrap();
                let stream = objs[&cid];
                let content = stream
                    [stream.find("stream\n").unwrap() + 7..stream.rfind("\nendstream").unwrap()]
                    .to_string();
                let texts = content
                    .lines()
                    .filter(|l| l.starts_with("BT "))
                    .map(|l| {
                        let tok: Vec<&str> = l.split(' ').collect();
                        let t = &l[l.find("Td (").unwrap() + 4..l.rfind(") Tj").unwrap()];
                        (
                            tok[4].parse().unwrap(),
                            tok[5].parse().unwrap(),
                            t.to_string(),
                        )
                    })
                    .collect();
                PdfPage {
                    media,
                    content,
                    texts,
                }
            })
            .collect()
    }

    fn pages_of(d: &Document, opts: &PdfOptions) -> Vec<PdfPage> {
        parse_pages(&to_pdf(d, opts))
    }

    fn text_para(text: &str) -> Block {
        para(vec![run(text, RunProps::default())])
    }

    /// A paragraph ending a section described by `sect_pr`.
    fn sect_para(text: &str, sect_pr: &str) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps {
                section_break: Some(sect_pr.to_string()),
                ..ParProps::default()
            },
            content: vec![run(text, RunProps::default())],
        })
    }

    fn trailing(raw: &str) -> Block {
        Block::SectionProperties(SectionProperties {
            raw: raw.to_string(),
            property_change: None,
        })
    }

    fn page_break() -> Inline {
        Inline::Break(BreakKind::Page, RunProps::default())
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.02
    }

    const BLANK_SECT: &str = "<w:sectPr/>";
    const LH: f32 = 11.0 * 1.35;

    #[test]
    fn page_break_starts_a_new_page() {
        let d = doc(vec![para(vec![
            run("Before", RunProps::default()),
            page_break(),
            run("After", RunProps::default()),
        ])]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 2);
        assert!(pages[0].has("Before") && !pages[0].has("After"));
        assert!(pages[1].has("After"));

        // The Word shape: the break ends the first paragraph.
        let d = doc(vec![
            para(vec![run("Before", RunProps::default()), page_break()]),
            text_para("After"),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 2);
        assert!(pages[1].has("After"));

        let d = doc(vec![para(vec![
            run("one", RunProps::default()),
            page_break(),
            run("two", RunProps::default()),
            page_break(),
            run("three", RunProps::default()),
        ])]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 3);
        assert!(pages[2].has("three"));
    }

    #[test]
    fn each_next_page_section_starts_a_page() {
        let d = doc(vec![
            sect_para("s1", BLANK_SECT),
            sect_para("s2", r#"<w:sectPr><w:type w:val="nextPage"/></w:sectPr>"#),
            sect_para("s3", BLANK_SECT),
            sect_para("s4", r#"<w:sectPr><w:type w:val="nextPage"/></w:sectPr>"#),
            text_para("s5"),
            trailing(BLANK_SECT),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 5);
        for (i, page) in pages.iter().enumerate() {
            assert!(page.has(&format!("s{}", i + 1)));
        }
    }

    #[test]
    fn section_start_type_is_read_from_the_starting_section() {
        // The ending section is untyped; the next (trailing) one is continuous.
        let d = doc(vec![
            sect_para("one", BLANK_SECT),
            text_para("two"),
            trailing(r#"<w:sectPr><w:type w:val="continuous"/></w:sectPr>"#),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 1);
        let (one, two) = (pages[0].at("one"), pages[0].at("two"));
        assert!(two.1 < one.1, "the continuous section flows below");

        // A continuous type on the ending section says nothing about the next.
        let d = doc(vec![
            sect_para(
                "one",
                r#"<w:sectPr><w:type w:val="continuous"/></w:sectPr>"#,
            ),
            text_para("two"),
            trailing(BLANK_SECT),
        ]);
        assert_eq!(pages_of(&d, &PdfOptions::default()).len(), 2);
    }

    #[test]
    fn odd_and_even_page_sections_insert_a_blank_page_when_needed() {
        let d = doc(vec![
            sect_para("one", BLANK_SECT),
            text_para("two"),
            trailing(r#"<w:sectPr><w:type w:val="oddPage"/></w:sectPr>"#),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(
            pages.len(),
            3,
            "page 2 is even, so a blank page comes first"
        );
        assert!(pages[1].texts.is_empty());
        assert!(pages[2].has("two"));

        let d = doc(vec![
            sect_para("one", BLANK_SECT),
            text_para("two"),
            trailing(r#"<w:sectPr><w:type w:val="evenPage"/></w:sectPr>"#),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 2, "page 2 already is even");
        assert!(pages[1].has("two"));
    }

    #[test]
    fn page_break_before_starts_a_new_page_unless_at_the_top() {
        let pbb = |text: &str, raw: &str| {
            Block::Paragraph(Paragraph {
                props: ParProps {
                    raw_props: vec![raw.to_string()],
                    ..ParProps::default()
                },
                content: vec![run(text, RunProps::default())],
            })
        };
        let d = doc(vec![text_para("one"), pbb("two", "<w:pageBreakBefore/>")]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 2);
        assert!(pages[1].has("two"));

        let d = doc(vec![
            text_para("one"),
            pbb("two", r#"<w:pageBreakBefore w:val="0"/>"#),
        ]);
        assert_eq!(pages_of(&d, &PdfOptions::default()).len(), 1);

        let d = doc(vec![pbb("one", "<w:pageBreakBefore/>")]);
        assert_eq!(pages_of(&d, &PdfOptions::default()).len(), 1);
    }

    #[test]
    fn a_paragraph_styles_page_break_before_starts_a_new_page() {
        let ss = crate::styles::parse_styles_xml(
            r#"<w:styles><w:style w:type="paragraph" w:styleId="Heading1"><w:pPr><w:pageBreakBefore/></w:pPr></w:style></w:styles>"#,
        );
        let opts = PdfOptions {
            styles: Rc::new(ss),
            ..PdfOptions::default()
        };
        let heading = |text: &str, raw: &[&str]| {
            Block::Paragraph(Paragraph {
                props: ParProps {
                    style_id: Some("Heading1".to_string()),
                    raw_props: raw.iter().map(|r| r.to_string()).collect(),
                    ..ParProps::default()
                },
                content: vec![run(text, RunProps::default())],
            })
        };
        let d = doc(vec![text_para("one"), heading("Title", &[])]);
        let pages = pages_of(&d, &opts);
        assert_eq!(pages.len(), 2);
        assert!(pages[1].has("Title"));

        // A direct explicit off overrides the style.
        let d = doc(vec![
            text_para("one"),
            heading("Title", &[r#"<w:pageBreakBefore w:val="0"/>"#]),
        ]);
        assert_eq!(pages_of(&d, &opts).len(), 1);
    }

    #[test]
    fn media_box_follows_each_sections_page_size() {
        let d = doc(vec![
            sect_para(
                "portrait",
                r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>"#,
            ),
            text_para("landscape"),
            trailing(
                r#"<w:sectPr><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/></w:sectPr>"#,
            ),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].media, "0 0 612.00 792.00");
        assert_eq!(pages[1].media, "0 0 792.00 612.00");
        assert!(pages[1].has("landscape"));
    }

    #[test]
    fn margins_place_the_text() {
        let d = doc(vec![
            text_para("x"),
            trailing(
                r#"<w:sectPr><w:pgMar w:top="2880" w:left="2880" w:right="1440" w:bottom="1440"/></w:sectPr>"#,
            ),
        ]);
        let (x, y) = pages_of(&d, &PdfOptions::default())[0].at("x");
        assert!(close(x, 144.0), "x = {x}");
        assert!(close(y, 792.0 - 144.0 - LH), "y = {y}");

        // The gutter widens the left margin, or the top one with gutterAtTop.
        let gutter = r#"<w:sectPr><w:pgMar w:top="1440" w:left="1440" w:right="1440" w:bottom="1440" w:gutter="720"/></w:sectPr>"#;
        let d = doc(vec![text_para("x"), trailing(gutter)]);
        let (x, _) = pages_of(&d, &PdfOptions::default())[0].at("x");
        assert!(close(x, 108.0), "x = {x}");
        let at_top = PdfOptions {
            gutter_at_top: true,
            ..PdfOptions::default()
        };
        let (x, y) = pages_of(&d, &at_top)[0].at("x");
        assert!(close(x, 72.0) && close(y, 792.0 - 108.0 - LH), "({x}, {y})");
    }

    #[test]
    fn mirror_margins_swap_left_and_right_on_even_pages() {
        let d = doc(vec![
            para(vec![run("odd", RunProps::default()), page_break()]),
            text_para("even"),
            trailing(
                r#"<w:sectPr><w:pgMar w:top="1440" w:left="2880" w:right="720" w:bottom="1440" w:gutter="360"/></w:sectPr>"#,
            ),
        ]);
        let mirror = PdfOptions {
            mirror_margins: true,
            ..PdfOptions::default()
        };
        let pages = pages_of(&d, &mirror);
        // Odd: inside (left) margin 144 + gutter 18; even: outside margin 36.
        assert!(close(pages[0].at("odd").0, 162.0));
        assert!(close(pages[1].at("even").0, 36.0));
        // Without mirror margins both pages use the left margin.
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(close(pages[1].at("even").0, 162.0));
    }

    #[test]
    fn bottom_margin_decides_where_text_overflows() {
        let blocks = |sect: &str| {
            let mut b: Vec<Block> = (0..60).map(|i| text_para(&format!("l{i}"))).collect();
            b.push(trailing(sect));
            doc(b)
        };
        let normal = pages_of(
            &blocks(
                r#"<w:sectPr><w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440"/></w:sectPr>"#,
            ),
            &PdfOptions::default(),
        );
        let tall = pages_of(
            &blocks(
                r#"<w:sectPr><w:pgMar w:top="1440" w:bottom="7200" w:left="1440" w:right="1440"/></w:sectPr>"#,
            ),
            &PdfOptions::default(),
        );
        assert!(tall.len() > normal.len());
        for page in &tall {
            for (_, y, _) in &page.texts {
                assert!(*y >= 360.0, "text below the 5-inch bottom margin: {y}");
            }
        }
    }

    fn two_col_sect(extra: &str) -> String {
        format!(r#"<w:sectPr><w:cols w:num="2" w:space="720"{extra}/></w:sectPr>"#)
    }

    fn col_break() -> Inline {
        Inline::Break(BreakKind::Column, RunProps::default())
    }

    #[test]
    fn columns_fill_left_then_right() {
        let mut blocks: Vec<Block> = (0..60).map(|i| text_para(&format!("l{i}"))).collect();
        blocks.push(trailing(&two_col_sect("")));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        assert_eq!(pages.len(), 1, "two columns hold what one column can't");
        // Column width (468 - 36) / 2 = 216; the second column starts at 324.
        assert!(close(pages[0].at("l0").0, 72.0));
        assert!(close(pages[0].at("l59").0, 324.0));
        assert!(
            !pages[0].content.contains(" l S"),
            "no separator without w:sep"
        );
    }

    #[test]
    fn column_break_jumps_to_the_next_column_then_the_next_page() {
        let d = doc(vec![
            para(vec![
                run("A", RunProps::default()),
                col_break(),
                run("B", RunProps::default()),
                col_break(),
                run("C", RunProps::default()),
            ]),
            trailing(&two_col_sect("")),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 2);
        let (a, b) = (pages[0].at("A"), pages[0].at("B"));
        assert!(close(b.0, 324.0) && close(a.1, b.1), "{a:?} {b:?}");
        assert!(close(pages[1].at("C").0, 72.0));
    }

    #[test]
    fn column_separator_and_unequal_widths() {
        let ab = || {
            para(vec![
                run("A", RunProps::default()),
                col_break(),
                run("B", RunProps::default()),
            ])
        };
        let d = doc(vec![ab(), trailing(&two_col_sect(r#" w:sep="1""#))]);
        let pages = pages_of(&d, &PdfOptions::default());
        // The rule sits mid-gap: 72 + 216 + 18.
        assert!(
            pages[0].content.contains(" w 306.00 720.00 m 306.00 "),
            "{}",
            pages[0].content
        );

        let d = doc(vec![
            ab(),
            trailing(
                r#"<w:sectPr><w:cols w:num="2" w:equalWidth="0"><w:col w:w="2880" w:space="720"/><w:col w:w="5040"/></w:cols></w:sectPr>"#,
            ),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(close(pages[0].at("B").0, 72.0 + 144.0 + 36.0));
    }

    #[test]
    fn continuous_section_starts_its_columns_below_the_previous_text() {
        let d = doc(vec![
            sect_para("intro", BLANK_SECT),
            para(vec![
                run("left", RunProps::default()),
                col_break(),
                run("right", RunProps::default()),
            ]),
            trailing(
                r#"<w:sectPr><w:type w:val="continuous"/><w:cols w:num="2" w:space="720"/></w:sectPr>"#,
            ),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 1);
        let (intro, left, right) = (
            pages[0].at("intro"),
            pages[0].at("left"),
            pages[0].at("right"),
        );
        assert!(left.1 < intro.1 && close(left.1, right.1));
        assert!(close(right.0, 324.0));
    }

    #[test]
    fn vertical_alignment_centres_or_bottoms_the_body() {
        let d = |v: &str| {
            doc(vec![
                text_para("x"),
                trailing(&format!(r#"<w:sectPr><w:vAlign w:val="{v}"/></w:sectPr>"#)),
            ])
        };
        let first = 720.0 - LH;
        let (_, y) = pages_of(&d("center"), &PdfOptions::default())[0].at("x");
        assert!(close(y, first - (first - 72.0) / 2.0), "y = {y}");
        let (_, y) = pages_of(&d("bottom"), &PdfOptions::default())[0].at("x");
        assert!(close(y, 72.0), "y = {y}");
        let (_, y) = pages_of(&d("both"), &PdfOptions::default())[0].at("x");
        assert!(close(y, first), "one paragraph stays at the top");
    }

    /// Options with header/footer parts: `(rid, part file, blocks)`.
    fn with_parts(parts: &[(&str, &str, Vec<Block>)]) -> PdfOptions {
        let mut rels = String::from("<Relationships>");
        let mut header_footer = HashMap::new();
        for (rid, file, blocks) in parts {
            rels.push_str(&format!(r#"<Relationship Id="{rid}" Target="{file}"/>"#));
            header_footer.insert(format!("word/{file}"), Rc::new(blocks.clone()));
        }
        rels.push_str("</Relationships>");
        PdfOptions {
            rels: crate::load::parse_rels_xml(&rels),
            header_footer,
            ..PdfOptions::default()
        }
    }

    fn three_pages(sect: &str) -> Document {
        doc(vec![
            para(vec![
                run("p1", RunProps::default()),
                page_break(),
                run("p2", RunProps::default()),
                page_break(),
                run("p3", RunProps::default()),
            ]),
            trailing(sect),
        ])
    }

    #[test]
    fn default_header_and_footer_are_drawn_on_every_page() {
        let opts = with_parts(&[
            ("rH", "header1.xml", vec![text_para("HDR")]),
            ("rF", "footer1.xml", vec![text_para("FTR")]),
        ]);
        let d = three_pages(
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rH"/><w:footerReference w:type="default" r:id="rF"/><w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440" w:header="720" w:footer="720"/></w:sectPr>"#,
        );
        let pages = pages_of(&d, &opts);
        assert_eq!(pages.len(), 3);
        for page in &pages {
            let (x, y) = page.at("HDR");
            assert!(close(x, 72.0) && close(y, 792.0 - 36.0 - LH), "{y}");
            let (_, y) = page.at("FTR");
            assert!(
                close(y, 36.0),
                "footer baseline at the footer distance: {y}"
            );
        }
    }

    #[test]
    fn first_page_and_even_page_headers() {
        let opts = PdfOptions {
            even_and_odd_headers: true,
            ..with_parts(&[
                ("rD", "header1.xml", vec![text_para("DEF")]),
                ("rT", "header2.xml", vec![text_para("FIRST")]),
                ("rE", "header3.xml", vec![text_para("EVEN")]),
            ])
        };
        let d = three_pages(
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rD"/><w:headerReference w:type="first" r:id="rT"/><w:headerReference w:type="even" r:id="rE"/><w:titlePg/></w:sectPr>"#,
        );
        let pages = pages_of(&d, &opts);
        assert!(pages[0].has("FIRST") && !pages[0].has("DEF"));
        assert!(pages[1].has("EVEN"));
        assert!(pages[2].has("DEF"));

        // titlePg with no first header: the first page has none.
        let d = three_pages(
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rD"/><w:titlePg/></w:sectPr>"#,
        );
        let pages = pages_of(&d, &opts);
        assert!(!pages[0].has("DEF") && !pages[0].has("FIRST"));
        assert!(pages[2].has("DEF"));
    }

    /// Insert › Cover Page (#652): the cover takes no page number, since
    /// Different First Page hides the footer there, and the body's first page
    /// shows it.
    #[test]
    fn a_cover_page_shows_no_footer() {
        let opts = with_parts(&[("rF", "footer1.xml", vec![text_para("FTR")])]);
        let mut ed = crate::editor::Editor::new(doc(vec![
            text_para("Body"),
            trailing(r#"<w:sectPr><w:footerReference w:type="default" r:id="rF"/></w:sectPr>"#),
        ]));
        let pages = pages_of(&ed.doc, &opts);
        assert!(pages[0].has("Body") && pages[0].has("FTR"));
        ed.set_cover_page(0, &[]).unwrap();
        let pages = pages_of(&ed.doc, &opts);
        assert_eq!(pages.len(), 2);
        assert!(pages[0].has("Document title") && !pages[0].has("FTR"));
        assert!(pages[1].has("Body") && pages[1].has("FTR"));
    }

    #[test]
    fn a_section_without_a_header_reference_links_to_the_previous_one() {
        let opts = with_parts(&[("rH", "header1.xml", vec![text_para("HDR")])]);
        let d = doc(vec![
            sect_para(
                "one",
                r#"<w:sectPr><w:headerReference w:type="default" r:id="rH"/></w:sectPr>"#,
            ),
            text_para("two"),
            trailing(BLANK_SECT),
        ]);
        let pages = pages_of(&d, &opts);
        assert_eq!(pages.len(), 2);
        assert!(pages[1].has("HDR") && pages[1].has("two"));
    }

    #[test]
    fn a_tall_header_pushes_the_body_down() {
        let opts = with_parts(&[(
            "rH",
            "header1.xml",
            (0..6).map(|i| text_para(&format!("h{i}"))).collect(),
        )]);
        let d = doc(vec![
            text_para("body"),
            trailing(r#"<w:sectPr><w:headerReference w:type="default" r:id="rH"/></w:sectPr>"#),
        ]);
        let page = &pages_of(&d, &opts)[0];
        let (_, h5) = page.at("h5");
        let (_, body) = page.at("body");
        assert!(
            body < h5 - 11.0,
            "body {body} overlaps the header's last line {h5}"
        );
    }

    fn fld_simple(instr: &str, cached: &str) -> Inline {
        Inline::Field {
            raw: format!(
                r#"<w:fldSimple w:instr="{instr}"><w:r><w:t>{cached}</w:t></w:r></w:fldSimple>"#
            ),
            text: cached.to_string(),
        }
    }

    fn fld_char(kind: &str) -> Inline {
        Inline::Raw(format!(r#"<w:r><w:fldChar w:fldCharType="{kind}"/></w:r>"#))
    }

    fn instr(text: &str) -> Inline {
        Inline::Raw(format!(
            r#"<w:r><w:instrText xml:space="preserve">{text}</w:instrText></w:r>"#
        ))
    }

    #[test]
    fn simple_page_field_in_a_footer_counts_pages() {
        let opts = with_parts(&[(
            "rF",
            "footer1.xml",
            vec![para(vec![
                run("Page ", RunProps::default()),
                fld_simple(" PAGE ", "1"),
                run(" of ", RunProps::default()),
                fld_simple("NUMPAGES", "1"),
            ])],
        )]);
        let d =
            three_pages(r#"<w:sectPr><w:footerReference w:type="default" r:id="rF"/></w:sectPr>"#);
        let pages = pages_of(&d, &opts);
        assert_eq!(pages.len(), 3);
        for (i, page) in pages.iter().enumerate() {
            let values: Vec<&str> = page
                .texts
                .iter()
                .map(|(_, _, t)| t.as_str())
                .filter(|t| t.chars().all(|c| c.is_ascii_digit()))
                .collect();
            assert_eq!(values, vec![(i + 1).to_string().as_str(), "3"]);
        }
    }

    #[test]
    fn loaded_complex_page_field_in_a_footer_counts_pages_642() {
        // The loader collapses a complex field into one `Inline::Field`; its
        // page number must still be substituted per page, not print the cache.
        let footer = crate::load::parse_header_footer(
            r#"<w:ftr><w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE \* MERGEFORMAT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:ftr>"#,
            &crate::load::Relationships::default(),
        );
        assert!(matches!(
            &footer[0],
            Block::Paragraph(p) if matches!(p.content[1], Inline::Field { .. })
        ));
        let opts = with_parts(&[("rF", "footer1.xml", footer)]);
        let d =
            three_pages(r#"<w:sectPr><w:footerReference w:type="default" r:id="rF"/></w:sectPr>"#);
        let pages = pages_of(&d, &opts);
        assert_eq!(pages.len(), 3);
        for (i, page) in pages.iter().enumerate() {
            assert!(
                page.exact(&(i + 1).to_string()),
                "page {}: {:?}",
                i + 1,
                page.texts
            );
        }
    }

    #[test]
    fn a_toc_entrys_page_number_field_is_exported_inside_its_link_642() {
        // A `TOC \h` entry: the PAGEREF field (collapsed into one Field on
        // load) sits inside the entry's link.
        let body = crate::load::parse_document_xml(
            r#"<w:document><w:body><w:p><w:hyperlink w:anchor="_Toc1"><w:r><w:t>Intro</w:t></w:r><w:r><w:tab/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> PAGEREF _Toc1 \h </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>7</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:hyperlink></w:p></w:body></w:document>"#,
            &crate::load::Relationships::default(),
        );
        assert!(matches!(
            &body.body[0],
            Block::Paragraph(p) if matches!(&p.content[0], Inline::Hyperlink(h) if h.content.iter().any(|i| matches!(i, Inline::Field { .. })))
        ));
        let pages = pages_of(&body, &PdfOptions::default());
        assert!(pages[0].has("Intro"), "{:?}", pages[0].texts);
        assert!(
            pages[0].has("7"),
            "the page number is exported: {:?}",
            pages[0].texts
        );
    }

    /// A tab inside a link that is not plain runs (a TOC entry holding its
    /// PAGEREF field, or a smart tag, #1069) prints as a tab outside one does,
    /// inside the link, instead of vanishing.
    #[test]
    fn a_tab_inside_a_complex_link_is_exported_1069() {
        for inner in [
            r#"<w:r><w:t>Intro</w:t></w:r><w:r><w:tab/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> PAGEREF _Toc1 \h </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>7</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r>"#,
            r#"<w:smartTag w:element="place"><w:r><w:t>Intro</w:t></w:r></w:smartTag><w:r><w:tab/></w:r><w:r><w:t>7</w:t></w:r>"#,
        ] {
            let body = crate::load::parse_document_xml(
                &format!(
                    r#"<w:document><w:body><w:p><w:hyperlink w:anchor="_Toc1">{inner}</w:hyperlink></w:p></w:body></w:document>"#
                ),
                &crate::load::Relationships::default(),
            );
            let Block::Paragraph(p) = &body.body[0] else {
                panic!("{:?}", body.body);
            };
            let segs = flatten_segments(p, false, &StyleSheet::default());
            let cells: Vec<&PCell> = segs.iter().flat_map(|s| &s.cells).collect();
            let text: String = cells.iter().map(|c| c.ch).collect();
            assert_eq!(text, "Intro    7", "{inner}");
            assert!(cells.iter().all(|c| c.link.as_deref() == Some("#_Toc1")));
        }
    }

    #[test]
    fn complex_page_field_with_a_split_instruction() {
        let d = doc(vec![para(vec![
            run("p1", RunProps::default()),
            page_break(),
            run("at ", RunProps::default()),
            fld_char("begin"),
            instr(" PA"),
            instr("GE "),
            fld_char("separate"),
            run("9", RunProps::default()),
            run("9", RunProps::default()),
            fld_char("end"),
        ])]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 2);
        assert!(pages[1].exact("2"), "{:?}", pages[1].texts);
        assert!(!pages[1].has("9"), "the cached result is replaced");
    }

    /// A soft hyphen in a page field's result prints nothing, and does not
    /// take the page number's marker with it (#1101 r2).
    #[test]
    fn a_soft_hyphen_in_a_page_field_result_keeps_the_page_number() {
        let soft = |cached: &str| Inline::Field {
            raw: format!(
                r#"<w:fldSimple w:instr="PAGE"><w:r><w:softHyphen/><w:t>{cached}</w:t></w:r></w:fldSimple>"#
            ),
            text: format!("\u{ad}{cached}"),
        };
        let d = doc(vec![para(vec![
            run("p1", RunProps::default()),
            page_break(),
            run("at ", RunProps::default()),
            soft("9"),
        ])]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(pages[1].exact("2"), "{:?}", pages[1].texts);
        // Its whole result a soft hyphen: the page number still shows.
        let d = doc(vec![para(vec![
            run("p1", RunProps::default()),
            page_break(),
            run("at ", RunProps::default()),
            soft(""),
        ])]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(pages[1].exact("2"), "{:?}", pages[1].texts);
        // A loose complex field's result.
        let d = doc(vec![para(vec![
            run("p1", RunProps::default()),
            page_break(),
            run("at ", RunProps::default()),
            fld_char("begin"),
            instr(" PAGE "),
            fld_char("separate"),
            run("\u{ad}", RunProps::default()),
            fld_char("end"),
        ])]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(pages[1].exact("2"), "{:?}", pages[1].texts);
    }

    #[test]
    fn nested_fields_are_substituted_at_the_outermost_level_only() {
        // IF { PAGE } = 1 "a" "b": the inner PAGE is part of the IF's code.
        let d = doc(vec![para(vec![
            run("p1", RunProps::default()),
            page_break(),
            fld_char("begin"),
            instr(" IF "),
            fld_char("begin"),
            instr(" PAGE "),
            fld_char("separate"),
            run("7", RunProps::default()),
            fld_char("end"),
            instr(r#" = 1 "a" "b" "#),
            fld_char("separate"),
            run("b", RunProps::default()),
            fld_char("end"),
        ])]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(
            pages[1].has("7") && pages[1].has("b"),
            "{:?}",
            pages[1].texts
        );
        assert!(!pages[1].exact("2"));
    }

    #[test]
    fn page_numbers_restart_and_format_per_section() {
        let page_para = |label: &str| {
            para(vec![
                run(label, RunProps::default()),
                fld_simple("PAGE", "0"),
            ])
        };
        let d = doc(vec![
            page_para("a"),
            Block::Paragraph(Paragraph {
                props: ParProps {
                    section_break: Some(BLANK_SECT.to_string()),
                    ..ParProps::default()
                },
                content: vec![],
            }),
            page_para("b"),
            para(vec![page_break()]),
            page_para("c"),
            trailing(r#"<w:sectPr><w:pgNumType w:fmt="lowerRoman" w:start="5"/></w:sectPr>"#),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 3);
        let value = |page: &PdfPage| {
            page.texts
                .iter()
                .find(|(_, _, t)| !["a", "b", "c"].contains(&t.as_str()))
                .map(|(_, _, t)| t.clone())
                .unwrap()
        };
        assert_eq!(value(&pages[0]), "1");
        assert_eq!(value(&pages[1]), "v");
        assert_eq!(value(&pages[2]), "vi");
        assert_eq!(format_page_number(28, NumFmt::UpperLetter), "BB");
        assert_eq!(format_page_number(3, NumFmt::LowerLetter), "c");
        assert_eq!(format_page_number(14, NumFmt::UpperRoman), "XIV");
    }

    #[test]
    fn section_pages_counts_the_sections_pages() {
        let d = doc(vec![
            sect_para("one", BLANK_SECT),
            para(vec![
                fld_simple("SECTIONPAGES", "0"),
                page_break(),
                run("more", RunProps::default()),
            ]),
            trailing(BLANK_SECT),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 3);
        assert!(pages[1].exact("2"), "{:?}", pages[1].texts);
    }

    #[test]
    fn odd_even_choice_follows_the_page_number() {
        let opts = PdfOptions {
            even_and_odd_headers: true,
            ..with_parts(&[
                ("rD", "header1.xml", vec![text_para("DEF")]),
                ("rE", "header2.xml", vec![text_para("EVEN")]),
            ])
        };
        let d = doc(vec![
            text_para("x"),
            trailing(
                r#"<w:sectPr><w:headerReference w:type="default" r:id="rD"/><w:headerReference w:type="even" r:id="rE"/><w:pgNumType w:start="2"/></w:sectPr>"#,
            ),
        ]);
        let pages = pages_of(&d, &opts);
        assert!(pages[0].has("EVEN"), "page 1 is numbered 2");

        // oddPage compares the number the section would start on.
        let d = doc(vec![
            sect_para("one", BLANK_SECT),
            text_para("two"),
            trailing(r#"<w:sectPr><w:type w:val="oddPage"/><w:pgNumType w:start="3"/></w:sectPr>"#),
        ]);
        assert_eq!(pages_of(&d, &PdfOptions::default()).len(), 2);
    }

    #[test]
    fn page_borders_and_page_colour() {
        let two_pages = |sect: &str| {
            doc(vec![
                para(vec![
                    run("p1", RunProps::default()),
                    page_break(),
                    run("p2", RunProps::default()),
                ]),
                trailing(sect),
            ])
        };
        let d = two_pages(
            r#"<w:sectPr><w:pgBorders w:offsetFrom="page"><w:top w:val="single" w:sz="8" w:space="24" w:color="FF0000"/><w:left w:val="single" w:sz="8" w:space="24" w:color="FF0000"/><w:bottom w:val="single" w:sz="8" w:space="24" w:color="FF0000"/><w:right w:val="none" w:sz="8" w:space="24"/></w:pgBorders></w:sectPr>"#,
        );
        let opts = PdfOptions {
            background: Some((0, 0, 255)),
            ..PdfOptions::default()
        };
        let pages = pages_of(&d, &opts);
        for page in &pages {
            assert!(
                page.content
                    .starts_with("0.000 0.000 1.000 rg 0 0 612.00 792.00 re f\n"),
                "background first: {}",
                page.content
            );
            // The absent right side still bounds the others at the page edge.
            assert!(
                page.content
                    .contains("1.000 0.000 0.000 RG 1.00 w 24.00 768.00 m 612.00 768.00 l S")
            );
            assert!(
                page.content
                    .contains("RG 1.00 w 24.00 24.00 m 24.00 768.00 l S")
            );
            assert_eq!(
                page.content.matches("RG 1.00 w").count(),
                3,
                "no right side"
            );
        }

        // Offsets from the text, shown on the first page only.
        let d = two_pages(
            r#"<w:sectPr><w:pgBorders w:display="firstPage"><w:top w:val="single" w:sz="4" w:space="10"/></w:pgBorders></w:sectPr>"#,
        );
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(
            pages[0]
                .content
                .contains("0.50 w 72.00 730.00 m 540.00 730.00 l S"),
            "{}",
            pages[0].content
        );
        assert!(!pages[1].content.contains(" re f") && !pages[1].content.contains("0.50 w"));
    }

    #[test]
    fn a_document_without_section_properties_keeps_the_default_page() {
        let d = doc(vec![text_para("x")]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].media, "0 0 612.00 792.00");
        let (x, y) = pages[0].at("x");
        assert!(close(x, 72.0) && close(y, 720.0 - LH));
    }

    #[test]
    fn absurd_column_counts_and_page_starts_are_clamped() {
        for num in ["inf", "1e12", "NaN", "-3"] {
            let d = doc(vec![
                text_para("x"),
                trailing(&format!(r#"<w:sectPr><w:cols w:num="{num}"/></w:sectPr>"#)),
            ]);
            assert_eq!(pages_of(&d, &PdfOptions::default()).len(), 1, "w:num={num}");
        }
        let many: String = (0..100).map(|_| r#"<w:col w:w="100"/>"#).collect();
        let d = doc(vec![
            text_para("x"),
            trailing(&format!(
                r#"<w:sectPr><w:cols w:num="100" w:equalWidth="0">{many}</w:cols></w:sectPr>"#
            )),
        ]);
        assert_eq!(pages_of(&d, &PdfOptions::default()).len(), 1);
        let sect = SectionLayout::parse(
            r#"<w:sectPr><w:cols w:num="1e12"/></w:sectPr>"#,
            &PdfOptions::default(),
        );
        assert_eq!(sect.cols.len(), MAX_COLS);

        // A start past u32 range: numbering saturates instead of overflowing.
        let d = doc(vec![
            para(vec![
                fld_simple("PAGE", "0"),
                page_break(),
                fld_simple("PAGE", "0"),
            ]),
            trailing(r#"<w:sectPr><w:pgNumType w:start="4294967295"/></w:sectPr>"#),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(pages[0].exact("32767") && pages[1].exact("32768"));
        let d = doc(vec![
            text_para("x"),
            trailing(r#"<w:sectPr><w:pgNumType w:start="inf" w:fmt="upperLetter"/></w:sectPr>"#),
        ]);
        assert_eq!(pages_of(&d, &PdfOptions::default()).len(), 1);
    }

    #[test]
    fn a_continuous_column_set_with_no_room_left_moves_to_the_next_page() {
        // 33 lines fill the page to within one line of the bottom margin.
        let mut blocks: Vec<Block> = (0..32).map(|i| text_para(&format!("l{i}"))).collect();
        blocks.push(sect_para("l32", BLANK_SECT));
        blocks.push(para(vec![
            run("left", RunProps::default()),
            col_break(),
            run("right", RunProps::default()),
        ]));
        blocks.push(trailing(
            r#"<w:sectPr><w:type w:val="continuous"/><w:cols w:num="2" w:space="720" w:sep="1"/></w:sectPr>"#,
        ));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        assert_eq!(pages.len(), 2);
        assert!(pages[0].has("l32"));
        for (_, y, t) in &pages[0].texts {
            assert!(*y >= 72.0, "{t:?} below the bottom margin at {y}");
        }
        assert!(!pages[0].content.contains(" l S"), "no separator on page 1");
        let (left, right) = (pages[1].at("left"), pages[1].at("right"));
        assert!(close(left.0, 72.0) && close(right.0, 324.0) && close(left.1, right.1));
    }

    #[test]
    fn the_printed_documents_final_section_wins_and_last_sect_pr_is_the_fallback() {
        let opts = PdfOptions {
            last_sect_pr: Some(
                r#"<w:sectPr><w:pgSz w:w="15840" w:h="12240"/></w:sectPr>"#.to_string(),
            ),
            ..PdfOptions::default()
        };
        let d = doc(vec![text_para("x"), trailing(BLANK_SECT)]);
        assert_eq!(pages_of(&d, &opts)[0].media, "0 0 612.00 792.00");
        let d = doc(vec![text_para("x")]);
        assert_eq!(pages_of(&d, &opts)[0].media, "0 0 792.00 612.00");
    }

    #[test]
    fn a_continuous_section_restarts_numbering_on_its_next_page() {
        let mut blocks = vec![
            sect_para("intro", BLANK_SECT),
            para(vec![
                run("count=", RunProps::default()),
                fld_simple("SECTIONPAGES", "0"),
            ]),
        ];
        blocks.extend((0..40).map(|i| text_para(&format!("l{i}"))));
        blocks.push(para(vec![
            run("page=", RunProps::default()),
            fld_simple("PAGE", "0"),
        ]));
        blocks.push(trailing(
            r#"<w:sectPr><w:type w:val="continuous"/><w:pgNumType w:start="1"/></w:sectPr>"#,
        ));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        assert_eq!(pages.len(), 2);
        assert!(pages[0].has("intro") && pages[0].has("count="));
        // The start page belongs to the first section; the restart numbers page 2.
        assert!(
            pages[1].has("page=") && pages[1].exact("1"),
            "{:?}",
            pages[1].texts
        );
        // The section has content on both pages.
        assert!(pages[0].exact("2"), "{:?}", pages[0].texts);
    }

    #[test]
    fn a_tracked_section_change_lays_out_the_current_values() {
        let opts = PdfOptions {
            last_sect_pr: Some(
                r#"<w:sectPr><w:headerReference w:type="default" r:id="rD"/><w:headerReference w:type="first" r:id="rT"/><w:sectPrChange w:id="1" w:author="a"><w:sectPr><w:titlePg/><w:pgSz w:w="15840" w:h="12240"/></w:sectPr></w:sectPrChange></w:sectPr>"#
                    .to_string(),
            ),
            ..with_parts(&[
                ("rD", "header1.xml", vec![text_para("DEF")]),
                ("rT", "header2.xml", vec![text_para("FIRST")]),
            ])
        };
        let pages = pages_of(&doc(vec![text_para("x")]), &opts);
        assert_eq!(pages[0].media, "0 0 612.00 792.00");
        assert!(
            pages[0].has("DEF") && !pages[0].has("FIRST"),
            "titlePg was removed"
        );
    }

    #[test]
    fn a_continuous_section_that_overflows_at_once_owns_only_the_next_page() {
        let mut blocks: Vec<Block> = (0..32).map(|i| text_para(&format!("l{i}"))).collect();
        blocks.push(sect_para("l32", BLANK_SECT));
        blocks.push(para(vec![
            run("count=", RunProps::default()),
            fld_simple("SECTIONPAGES", "0"),
        ]));
        blocks.push(trailing(
            r#"<w:sectPr><w:type w:val="continuous"/></w:sectPr>"#,
        ));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        assert_eq!(pages.len(), 2);
        assert!(pages[1].has("count="));
        assert!(pages[1].exact("1"), "{:?}", pages[1].texts);
    }

    // ---- line numbers (#737) ----

    fn ln_sect(attrs: &str, extra: &str) -> String {
        format!(r#"<w:sectPr><w:lnNumType {attrs}/>{extra}</w:sectPr>"#)
    }

    /// Paragraph texts `prefix0..prefixN`.
    fn paras(prefix: &str, n: usize) -> Vec<Block> {
        (0..n).map(|i| text_para(&format!("{prefix}{i}"))).collect()
    }

    fn with_raw(text: &str, raw: &str) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps {
                raw_props: vec![raw.to_string()],
                ..ParProps::default()
            },
            content: vec![run(text, RunProps::default())],
        })
    }

    /// The line numbers drawn on a page, as (number, x, y), in drawing order.
    fn numbers(page: &PdfPage) -> Vec<(u32, f32, f32)> {
        page.texts
            .iter()
            .filter_map(|(x, y, t)| Some((t.parse().ok()?, *x, *y)))
            .collect()
    }

    #[test]
    fn line_numbers_count_by_start_and_restart() {
        let mut blocks = paras("t", 3);
        blocks.push(trailing(&ln_sect(r#"w:countBy="1""#, "")));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        let nums = numbers(&pages[0]);
        assert_eq!(
            nums.iter().map(|n| n.0).collect::<Vec<_>>(),
            [1, 2, 3],
            "{:?}",
            pages[0].texts
        );
        for (i, &(n, x, y)) in nums.iter().enumerate() {
            let (tx, ty) = pages[0].at(&format!("t{i}"));
            assert!(close(y, ty), "on the line's baseline");
            // Right edge a quarter inch (Auto) left of the text column.
            let right = x + n.to_string().len() as f32 * 6.6;
            assert!(close(right, tx - 18.0), "{right} vs {tx}");
        }

        // countBy 5, start 4 (Word's "Start at: 5"), a set distance.
        let mut blocks = paras("t", 12);
        blocks.push(trailing(&ln_sect(
            r#"w:countBy="5" w:start="4" w:distance="720""#,
            "",
        )));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        let nums = numbers(&pages[0]);
        // Lines are numbered 5..=16: 5 on t0, 10 on t5, 15 on t10.
        assert_eq!(nums.iter().map(|n| n.0).collect::<Vec<_>>(), [5, 10, 15]);
        assert!(close(nums[1].2, pages[0].at("t5").1));
        assert!(close(nums[0].1 + 6.6, 72.0 - 36.0));

        // newPage (the default) restarts on each page; continuous never does.
        let two_pages = |attrs: &str| {
            doc(vec![
                text_para("a"),
                with_raw("b", "<w:pageBreakBefore/>"),
                trailing(&ln_sect(attrs, "")),
            ])
        };
        let pages = pages_of(&two_pages(""), &PdfOptions::default());
        assert_eq!(numbers(&pages[1])[0].0, 1);
        let pages = pages_of(
            &two_pages(r#"w:restart="continuous""#),
            &PdfOptions::default(),
        );
        assert_eq!(numbers(&pages[1])[0].0, 2);
    }

    #[test]
    fn line_numbers_restart_new_section_continuous_break() {
        let sect = ln_sect(r#"w:restart="newSection""#, "");
        let cont = ln_sect(
            r#"w:restart="newSection""#,
            r#"<w:type w:val="continuous"/>"#,
        );
        let d = doc(vec![
            text_para("a0"),
            sect_para("a1", &sect),
            text_para("b0"),
            trailing(&cont),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(pages.len(), 1);
        let nums = numbers(&pages[0]);
        assert_eq!(nums.iter().map(|n| n.0).collect::<Vec<_>>(), [1, 2, 1]);
        assert!(close(nums[2].2, pages[0].at("b0").1));

        // A new page doesn't restart newSection numbering.
        let d = doc(vec![
            text_para("a0"),
            with_raw("a1", "<w:pageBreakBefore/>"),
            trailing(&sect),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert_eq!(numbers(&pages[1])[0].0, 2);
    }

    #[test]
    fn suppressed_paragraphs_and_tables_are_not_numbered_or_counted() {
        let ss = crate::styles::parse_styles_xml(
            r#"<w:styles><w:style w:type="paragraph" w:styleId="NoNum"><w:pPr><w:suppressLineNumbers/></w:pPr></w:style></w:styles>"#,
        );
        let styled = Block::Paragraph(Paragraph {
            props: ParProps {
                style_id: Some("NoNum".to_string()),
                ..ParProps::default()
            },
            content: vec![run("styled", RunProps::default())],
        });
        let table = Block::Table(Table {
            rows: vec![Row {
                cells: vec![Cell {
                    blocks: vec![text_para("cell")],
                    ..Cell::default()
                }],
                ..Row::default()
            }],
            ..Table::default()
        });
        let d = doc(vec![
            text_para("one"),
            with_raw("direct", "<w:suppressLineNumbers/>"),
            styled,
            table,
            text_para("two"),
            trailing(&ln_sect("", "")),
        ]);
        let opts = PdfOptions {
            styles: Rc::new(ss),
            ..PdfOptions::default()
        };
        let pages = pages_of(&d, &opts);
        let nums = numbers(&pages[0]);
        assert_eq!(nums.iter().map(|n| n.0).collect::<Vec<_>>(), [1, 2]);
        assert!(close(nums[1].2, pages[0].at("two").1));
    }

    #[test]
    fn no_line_numbers_without_ln_num_type_or_in_headers() {
        let mut blocks = paras("t", 3);
        blocks.push(trailing(BLANK_SECT));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        assert!(numbers(&pages[0]).is_empty());

        let opts = with_parts(&[("rH", "header1.xml", vec![text_para("HDR")])]);
        let d = doc(vec![
            text_para("body"),
            trailing(&ln_sect(
                "",
                r#"<w:headerReference w:type="default" r:id="rH"/>"#,
            )),
        ]);
        let pages = pages_of(&d, &opts);
        assert!(pages[0].has("HDR"));
        let nums = numbers(&pages[0]);
        assert_eq!(nums.len(), 1, "only the body line: {:?}", pages[0].texts);
        assert!(close(nums[0].2, pages[0].at("body").1));
    }

    #[test]
    fn line_numbers_per_column() {
        let d = doc(vec![
            para(vec![
                run("left", RunProps::default()),
                col_break(),
                run("right", RunProps::default()),
            ]),
            trailing(&ln_sect("", r#"<w:cols w:num="2" w:space="720"/>"#)),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        let nums = numbers(&pages[0]);
        assert_eq!(nums.iter().map(|n| n.0).collect::<Vec<_>>(), [1, 2]);
        let (rx, ry) = pages[0].at("right");
        assert!(close(rx, 324.0));
        assert!(close(nums[1].1 + 6.6, 324.0 - 18.0) && close(nums[1].2, ry));
    }

    // ---- vAlign both (#737) ----

    fn link_para(text: &str, url: &str) -> Block {
        para(vec![Inline::Hyperlink(Hyperlink {
            target: Some(url.to_string()),
            runs: vec![Run {
                text: text.to_string(),
                props: RunProps::default(),
            }],
            ..Hyperlink::default()
        })])
    }

    /// Every link annotation's rectangle in the PDF.
    fn link_rects(pdf: &[u8]) -> Vec<[f32; 4]> {
        s(pdf)
            .split("/Subtype /Link /Rect [")
            .skip(1)
            .map(|rest| {
                let nums: Vec<f32> = rest[..rest.find(']').unwrap()]
                    .split(' ')
                    .map(|n| n.parse().unwrap())
                    .collect();
                [nums[0], nums[1], nums[2], nums[3]]
            })
            .collect()
    }

    #[test]
    fn valign_both_spreads_paragraphs() {
        let long = "word ".repeat(20); // two lines at 70 characters a line
        let d = doc(vec![
            text_para("a"),
            text_para(long.trim_end()),
            link_para("c", "https://example.org/"),
            trailing(&ln_sect(
                r#"w:restart="newPage""#,
                r#"<w:vAlign w:val="both"/>"#,
            )),
        ]);
        let pdf = to_pdf(&d, &PdfOptions::default());
        let pages = parse_pages(&pdf);
        assert_eq!(pages.len(), 1);
        let page = &pages[0];
        let gap = 11.0 * 0.4;
        // Top-aligned layout: a, the two lines of b, then c.
        let a = 720.0 - LH;
        let b1 = a - gap - LH;
        let c = b1 - 2.0 * LH - gap;
        let free = c - 72.0;
        assert!(close(page.at("a").1, a), "the first paragraph stays");
        assert!(close(page.at("c").1, 72.0), "the last ends on the bottom");
        let b_lines: Vec<f32> = page
            .texts
            .iter()
            .filter(|(_, _, t)| t.starts_with("word"))
            .map(|(_, y, _)| *y)
            .collect();
        assert_eq!(b_lines.len(), 2);
        assert!(close(b_lines[0], b1 - free / 2.0), "{b_lines:?}");
        assert!(
            close(b_lines[0] - b_lines[1], LH),
            "a paragraph moves whole"
        );
        // The link and the line numbers move with their lines.
        let rect = link_rects(&pdf)[0];
        assert!(close(rect[1], 72.0 - 2.0), "{rect:?}");
        let nums = numbers(page);
        assert_eq!(nums.len(), 4);
        assert!(close(nums[1].2, b_lines[0]) && close(nums[3].2, 72.0));
    }

    #[test]
    fn valign_both_single_paragraph_or_columns_stay_at_the_top() {
        let long = "word ".repeat(20);
        let d = doc(vec![
            text_para(long.trim_end()),
            trailing(r#"<w:sectPr><w:vAlign w:val="both"/></w:sectPr>"#),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(close(pages[0].texts[0].1, 720.0 - LH));
        assert!(close(pages[0].texts[1].1, 720.0 - 2.0 * LH));

        // A multi-column page isn't justified (as for center and bottom).
        let d = doc(vec![
            text_para("a"),
            text_para("b"),
            trailing(
                &two_col_sect("").replace("</w:sectPr>", r#"<w:vAlign w:val="both"/></w:sectPr>"#),
            ),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(close(pages[0].at("b").1, 720.0 - 2.0 * LH - 4.4));
    }

    // ---- column balancing (#737) ----

    const CONTINUOUS: &str = r#"<w:sectPr><w:type w:val="continuous"/></w:sectPr>"#;

    /// `n` one-line paragraphs in a section `cols_sect`, then a continuous
    /// single-column section holding "next".
    fn balanced_doc(n: usize, cols_sect: &str) -> Document {
        let mut blocks = paras("l", n - 1);
        blocks.push(sect_para(&format!("l{}", n - 1), cols_sect));
        blocks.push(text_para("next"));
        blocks.push(trailing(CONTINUOUS));
        doc(blocks)
    }

    #[test]
    fn columns_balance_before_continuous_break() {
        let pages = pages_of(
            &balanced_doc(10, &two_col_sect(r#" w:sep="1""#)),
            &PdfOptions::default(),
        );
        assert_eq!(pages.len(), 1);
        let page = &pages[0];
        for i in 0..5 {
            let (left, right) = (page.at(&format!("l{i}")), page.at(&format!("l{}", i + 5)));
            assert!(
                close(left.0, 72.0) && close(right.0, 324.0),
                "{:?}",
                page.texts
            );
            assert!(close(left.1, right.1), "row {i}: {left:?} {right:?}");
        }
        // The next section starts just below the balanced columns, not below
        // ten lines.
        let step = LH + 4.4;
        let low = 720.0 - 5.0 * step;
        let next = page.at("next");
        assert!(close(next.1, low - LH), "{next:?}");
        assert!(close(next.0, 72.0));
        // The separator runs down to the balanced height.
        assert!(
            page.content
                .contains(&format!("306.00 720.00 m 306.00 {low:.2} l S")),
            "{}",
            page.content
        );
    }

    #[test]
    fn balancing_minimises_the_tallest_column_of_uneven_lines() {
        // A tall heading line and six body lines: a line-count split (4 | 3)
        // puts the heading and three lines in column one; by height the
        // heading shares its column with two.
        let heading = Block::Paragraph(Paragraph {
            props: ParProps {
                heading_level: Some(1),
                ..ParProps::default()
            },
            content: vec![run("H", RunProps::default())],
        });
        let mut blocks = vec![heading];
        blocks.extend(paras("l", 5));
        blocks.push(sect_para("l5", &two_col_sect("")));
        blocks.push(text_para("next"));
        blocks.push(trailing(CONTINUOUS));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        let page = &pages[0];
        let heading_h = 11.0 * 1.8 * 1.35 + 11.0 * 1.8 * 0.4;
        let step = LH + 4.4;
        // Heights: the heading 34.65, each body line 19.25. Heading + 2 lines
        // (73.15) | 4 lines (77.0) beats heading + 3 (92.4) | 3.
        assert!(close(page.at("l1").0, 72.0) && close(page.at("l2").0, 324.0));
        let low = 720.0 - (4.0 * step).max(heading_h + 2.0 * step);
        assert!(close(page.at("next").1, low - LH), "{:?}", page.texts);
    }

    #[test]
    fn columns_are_not_balanced_at_the_end_after_a_break_or_when_unequal() {
        // At the end of the document: everything stays in column one.
        let mut blocks = paras("l", 10);
        blocks.push(trailing(&two_col_sect("")));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        assert!(close(pages[0].at("l9").0, 72.0));

        // Unequal explicit widths would need re-wrapping.
        let unequal = r#"<w:sectPr><w:cols w:num="2" w:equalWidth="0"><w:col w:w="5000" w:space="720"/><w:col w:w="3000"/></w:cols></w:sectPr>"#;
        let pages = pages_of(&balanced_doc(10, unequal), &PdfOptions::default());
        assert!(close(pages[0].at("l9").0, 72.0), "{:?}", pages[0].texts);

        // A column break placed the lines deliberately.
        let d = doc(vec![
            text_para("a"),
            para(vec![run("b", RunProps::default()), col_break()]),
            sect_para("c", &two_col_sect("")),
            text_para("next"),
            trailing(CONTINUOUS),
        ]);
        let pages = pages_of(&d, &PdfOptions::default());
        assert!(close(pages[0].at("a").0, 72.0) && close(pages[0].at("b").0, 72.0));
        assert!(close(pages[0].at("c").0, 324.0));
    }

    // ---- border styles (#737) ----

    /// Page content for a page border with a `top` side (offset from the page
    /// edge by 24pt) and plain single left and right sides.
    fn border_page(top: &str) -> String {
        let sect = format!(
            r#"<w:sectPr><w:pgBorders w:offsetFrom="page"><w:top {top} w:space="24"/><w:left w:val="single" w:sz="8" w:space="24"/><w:right w:val="single" w:sz="8" w:space="24"/></w:pgBorders></w:sectPr>"#
        );
        let d = doc(vec![text_para("x"), trailing(&sect)]);
        pages_of(&d, &PdfOptions::default()).remove(0).content
    }

    #[test]
    fn double_top_border_stroke_endpoints() {
        let c = border_page(r#"w:val="double" w:sz="8""#);
        // The frame line, then one stroke width of gap (centres 2pt apart) on
        // the frame inflated outward, so it spans the inflated corners.
        assert!(c.contains("1.00 w 24.00 768.00 m 588.00 768.00 l S"), "{c}");
        assert!(c.contains("1.00 w 22.00 770.00 m 590.00 770.00 l S"), "{c}");
        assert_eq!(
            c.matches(" RG ").count(),
            4,
            "left and right are single: {c}"
        );
        assert!(!c.contains(" 0 d"), "solid rules set no dash: {c}");

        let c = border_page(r#"w:val="triple" w:sz="8""#);
        assert!(c.contains("1.00 w 20.00 772.00 m 592.00 772.00 l S"), "{c}");
    }

    #[test]
    fn thin_thick_borders_put_the_first_named_line_inside() {
        // sz 24 = 3pt, the thin line a third of it, a small gap one thin width.
        let c = border_page(r#"w:val="thinThickSmallGap" w:sz="24""#);
        assert!(c.contains("1.00 w 24.00 768.00 m 588.00 768.00 l S"), "{c}");
        assert!(c.contains("3.00 w 21.00 771.00 m 591.00 771.00 l S"), "{c}");

        let c = border_page(r#"w:val="thickThinLargeGap" w:sz="24""#);
        assert!(c.contains("3.00 w 24.00 768.00 m 588.00 768.00 l S"), "{c}");
        // 1.5 + 3 thin widths + 0.5 outward.
        assert!(c.contains("1.00 w 19.00 773.00 m 593.00 773.00 l S"), "{c}");

        let c = border_page(r#"w:val="thinThickThinMediumGap" w:sz="24""#);
        assert_eq!(c.matches("1.00 w").count(), 2 + 2, "{c}");
        // 0.5 + 2 thin widths + 1.5 outward.
        assert!(c.contains("3.00 w 20.00 772.00 m 592.00 772.00 l S"), "{c}");
    }

    #[test]
    fn dashed_page_borders_reset_the_dash_after_the_rule() {
        let c = border_page(r#"w:val="dotted" w:sz="16""#);
        assert!(
            c.contains("[2.00 2.00] 0 d\n0.000 0.000 0.000 RG 2.00 w 24.00 768.00 m 588.00 768.00 l S\n[] 0 d\n"),
            "{c}"
        );
        let c = border_page(r#"w:val="dashed" w:sz="8""#);
        assert!(c.contains("[3.00 2.00] 0 d\n"), "{c}");
        let c = border_page(r#"w:val="dotDotDash" w:sz="8""#);
        assert!(c.contains("[3.00 2.00 1.00 2.00 1.00 2.00] 0 d\n"), "{c}");
        // Only the dashed side sets a dash.
        assert_eq!(c.matches("[] 0 d").count(), 1);
    }

    #[test]
    fn art_border_uses_points_and_a_dash() {
        let c = border_page(r#"w:val="apples" w:sz="12" w:color="00FF00""#);
        assert!(
            c.contains("[12.00 12.00] 0 d\n0.000 1.000 0.000 RG 12.00 w 24.00 768.00 m 588.00 768.00 l S\n[] 0 d\n"),
            "{c}"
        );
        // A wave is drawn as a single line for now.
        let c = border_page(r#"w:val="wave" w:sz="8""#);
        assert!(c.contains("1.00 w 24.00 768.00 m 588.00 768.00 l S") && !c.contains(" 0 d"));
    }

    // ---- watermarks (#737) ----

    fn draft(width_pt: Option<f32>, font_size_pt: Option<f32>) -> TextWatermark {
        TextWatermark {
            text: "DRAFT".to_string(),
            rotation: 315.0,
            fill: None,
            width_pt,
            font_size_pt,
            font: None,
            opacity: None,
        }
    }

    /// Options with header parts and the given part's watermarks.
    fn with_watermark(
        parts: &[(&str, &str, Vec<Block>)],
        part: &str,
        mark: TextWatermark,
    ) -> PdfOptions {
        let mut opts = with_parts(parts);
        opts.watermarks.insert(format!("word/{part}"), vec![mark]);
        opts
    }

    #[test]
    fn text_watermark_drawn_behind_body() {
        let opts = with_watermark(
            &[("rH", "header1.xml", vec![text_para("HDR")])],
            "header1.xml",
            draft(Some(468.0), None),
        );
        let d = doc(vec![
            text_para("body"),
            trailing(
                r#"<w:sectPr><w:headerReference w:type="default" r:id="rH"/><w:pgBorders><w:top w:val="single" w:sz="8"/></w:pgBorders></w:sectPr>"#,
            ),
        ]);
        let page = pages_of(&d, &opts).remove(0);
        // Rotated 315 degrees clockwise in VML = 45 counter-clockwise, grey,
        // centred on the margin box (72..540 x 72..720), stretched to the
        // shape's 468pt: 156pt for five Courier characters.
        let expected = "q\n0.753 0.753 0.753 rg\n0.7071 0.7071 -0.7071 0.7071 306.00 396.00 cm\nBT /F0 156.00 Tf -234.00 -46.80 Td (DRAFT) Tj ET\nQ\n";
        assert!(page.content.starts_with(expected), "{}", page.content);
        // Behind the rules and the text.
        assert!(page.content.find("DRAFT").unwrap() < page.content.find(" l S").unwrap());
        assert!(page.content.find("DRAFT").unwrap() < page.content.find("(HDR)").unwrap());
        assert_eq!(page.content.matches("(DRAFT)").count(), 1);
    }

    #[test]
    fn watermark_size_falls_back_to_the_text_width_and_caps_at_a_set_size() {
        let place = |mark: &TextWatermark| WatermarkDraw::place(mark, (72.0, 72.0, 540.0, 720.0));
        // 80% of 468pt over five characters.
        assert!(close(place(&draft(None, None)).size, 0.8 * 468.0 / 3.0));
        assert!(close(place(&draft(Some(468.0), Some(36.0))).size, 36.0));
        assert!(close(place(&draft(Some(60.0), Some(36.0))).size, 20.0));
        let red = TextWatermark {
            fill: Some((255, 0, 0)),
            rotation: 0.0,
            ..draft(None, None)
        };
        let drawn = place(&red);
        assert_eq!(drawn.color, (1.0, 0.0, 0.0));
        assert!(close(drawn.angle, 0.0));
    }

    #[test]
    fn watermark_only_header_draws_watermark() {
        let opts = with_watermark(
            &[("rH", "header1.xml", vec![])],
            "header1.xml",
            draft(None, None),
        );
        let d = doc(vec![
            text_para("body"),
            trailing(r#"<w:sectPr><w:headerReference w:type="default" r:id="rH"/></w:sectPr>"#),
        ]);
        assert!(pages_of(&d, &opts)[0].content.contains("(DRAFT) Tj"));
    }

    #[test]
    fn watermark_follows_title_page_variant() {
        let opts = with_watermark(
            &[
                ("rH", "header1.xml", vec![text_para("HDR")]),
                ("rF", "header2.xml", vec![text_para("FIRST")]),
            ],
            "header1.xml",
            draft(None, None),
        );
        let d = three_pages(
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rH"/><w:headerReference w:type="first" r:id="rF"/><w:titlePg/></w:sectPr>"#,
        );
        let pages = pages_of(&d, &opts);
        assert_eq!(pages.len(), 3);
        assert!(pages[0].has("FIRST") && !pages[0].has("DRAFT"));
        assert!(pages[1].has("DRAFT") && pages[2].has("DRAFT"));

        // A section without a watermarked header draws none.
        let d = three_pages(BLANK_SECT);
        assert!(pages_of(&d, &opts).iter().all(|p| !p.has("DRAFT")));
    }

    #[test]
    fn balancing_a_full_page_keeps_every_line_above_the_bottom() {
        // Two full columns of one-line (1) and two-line (2) paragraphs and
        // headings (H): their trailing gaps differ (0, 4.4, 7.92), so the
        // shortest balanced height can exceed what a column holds; a pour at
        // it put a line's baseline 0.45pt into the bottom margin.
        let pattern = "12122H11122H22H2112H12222222112221112121112HH";
        let mut blocks: Vec<Block> = pattern
            .chars()
            .enumerate()
            .map(|(i, kind)| {
                let (text, heading_level) = match kind {
                    '1' => (format!("l{i}"), None),
                    '2' => (format!("l{i}{}", "x".repeat(36)), None),
                    _ => (format!("l{i}"), Some(1)),
                };
                Block::Paragraph(Paragraph {
                    props: ParProps {
                        heading_level,
                        ..ParProps::default()
                    },
                    content: vec![run(&text, RunProps::default())],
                })
            })
            .collect();
        if let Some(Block::Paragraph(last)) = blocks.last_mut() {
            last.props.section_break = Some(two_col_sect(""));
        }
        blocks.push(text_para("next"));
        blocks.push(trailing(CONTINUOUS));
        let pages = pages_of(&doc(blocks), &PdfOptions::default());
        let last = format!("l{}", pattern.len() - 1);
        assert!(
            pages[0].has("l0") && pages[0].has(&last),
            "one page of columns"
        );
        for (_, y, t) in &pages[0].texts {
            assert!(*y >= 72.0 - 0.01, "{t} at {y}");
        }
    }
}
