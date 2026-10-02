//! Writing Word's text watermark (Design > Watermark, #651).
//!
//! Word puts a watermark in a header part as a `w:sdt` from the "Watermarks"
//! building-block gallery, holding a header paragraph whose run draws a VML
//! WordArt shape named `PowerPlusWaterMarkObject…`. [`watermark_xml`] builds
//! that block; [`strip_watermarks`] and [`insert_watermark`] edit one header
//! part's XML. [`crate::package::Package::set_text_watermark`] applies them
//! to every header the document's sections show. The readers
//! ([`crate::package::text_watermarks`], `Package::watermarks`) are the
//! oracle the output satisfies.

use crate::page_bg::{OFFICE_NS, VML_NS, ensure_root_namespaces, root_start_end};
use crate::serialize::esc_attr;
use crate::xml::{Event, XmlParser};

const W10_NS: &str = "urn:schemas-microsoft-com:office:word";
const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// The marker every Word watermark shape's id starts with.
pub const SHAPE_ID: &str = "PowerPlusWaterMarkObject";

/// What Custom Watermark's Text watermark (and the gallery's presets) set.
#[derive(Debug, Clone, PartialEq)]
pub struct TextWatermarkSpec {
    pub text: String,
    pub font: String,
    /// Points; `None` is Auto (the text fits the page).
    pub size_pt: Option<f32>,
    /// RRGGBB.
    pub color: u32,
    pub semitransparent: bool,
    /// Diagonal (rotated 315°) rather than Horizontal.
    pub diagonal: bool,
}

impl TextWatermarkSpec {
    /// A gallery preset as Word writes it: Calibri, Auto size, silver,
    /// semitransparent.
    pub fn preset(text: &str, diagonal: bool) -> Self {
        Self {
            text: text.into(),
            font: "Calibri".into(),
            size_pt: None,
            color: 0xC0C0C0,
            semitransparent: true,
            diagonal,
        }
    }

    /// The shape's width and height in points. WordArt stretches the text to
    /// its box, so the box carries the text's proportions: about a third of
    /// the height per character in Word's own presets (DRAFT 412×247,
    /// CONFIDENTIAL 528×132). Auto fits a 6.5in text width; an explicit size
    /// is the line height.
    fn box_pt(&self) -> (f32, f32) {
        let chars = self.text.chars().count().max(1) as f32;
        let aspect = (chars / 3.0).max(1.0);
        match self.size_pt {
            Some(size) => (size * aspect, size),
            None => {
                let w = 468.0f32;
                (w, (w / aspect).min(w / 1.6))
            }
        }
    }
}

/// WordArt "plain text" (`o:spt="136"`), the shape type Word's watermarks use.
const SHAPETYPE: &str = "<v:shapetype id=\"_x0000_t136\" coordsize=\"21600,21600\" o:spt=\"136\" adj=\"10800\" path=\"m@7,l@8,m@5,21600l@6,21600e\">\
<v:formulas><v:f eqn=\"sum #0 0 10800\"/><v:f eqn=\"prod #0 2 1\"/><v:f eqn=\"sum 21600 0 @1\"/><v:f eqn=\"sum 0 0 @2\"/>\
<v:f eqn=\"sum 21600 0 @3\"/><v:f eqn=\"if @0 @3 0\"/><v:f eqn=\"if @0 21600 @1\"/><v:f eqn=\"if @0 0 @2\"/>\
<v:f eqn=\"if @0 @4 21600\"/><v:f eqn=\"mid @5 @6\"/><v:f eqn=\"mid @8 @5\"/><v:f eqn=\"mid @7 @8\"/>\
<v:f eqn=\"mid @6 @7\"/><v:f eqn=\"sum @6 0 @5\"/></v:formulas>\
<v:path textpathok=\"t\" o:connecttype=\"custom\" o:connectlocs=\"@9,0;@10,10800;@11,21600;@12,10800\" o:connectangles=\"270,180,90,0\"/>\
<v:textpath on=\"t\" fitshape=\"t\"/><v:handles><v:h position=\"#0,bottomRight\" xrange=\"6629,14971\"/></v:handles>\
<o:lock v:ext=\"edit\" text=\"t\" shapetype=\"t\"/></v:shapetype>";

fn pt(v: f32) -> String {
    let s = format!("{:.2}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    format!("{s}pt")
}

/// The watermark block Word writes into a header: the gallery `w:sdt`, its
/// Header paragraph and the VML shape. `n` numbers the shape (unique within
/// the document).
pub fn watermark_xml(spec: &TextWatermarkSpec, n: u32) -> String {
    let (w, h) = spec.box_pt();
    let rotation = if spec.diagonal { "rotation:315;" } else { "" };
    let font_size = spec.size_pt.map_or_else(|| "1pt".to_string(), pt);
    let mut text = String::new();
    esc_attr(&spec.text, &mut text);
    let mut font = String::new();
    esc_attr(&spec.font, &mut font);
    let fill = if spec.semitransparent {
        "<v:fill opacity=\".5\"/>"
    } else {
        ""
    };
    format!(
        "<w:sdt><w:sdtPr><w:id w:val=\"{id}\"/><w:docPartObj><w:docPartGallery w:val=\"Watermarks\"/>\
<w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtContent><w:p><w:pPr><w:pStyle w:val=\"Header\"/></w:pPr>\
<w:r><w:rPr><w:noProof/></w:rPr><w:pict>{SHAPETYPE}\
<v:shape id=\"{SHAPE_ID}{n}\" o:spid=\"_x0000_s{spid}\" type=\"#_x0000_t136\" \
style=\"position:absolute;margin-left:0;margin-top:0;width:{w};height:{h};{rotation}z-index:-251657216;\
mso-position-horizontal:center;mso-position-horizontal-relative:margin;mso-position-vertical:center;\
mso-position-vertical-relative:margin\" o:allowincell=\"f\" fillcolor=\"#{color:06X}\" stroked=\"f\">{fill}\
<v:textpath style=\"font-family:&quot;{font}&quot;;font-size:{font_size}\" string=\"{text}\"/>\
<w10:wrap anchorx=\"margin\" anchory=\"margin\"/></v:shape></w:pict></w:r></w:p></w:sdtContent></w:sdt>",
        id = -1_000_000_000i64 - i64::from(n),
        spid = 2049 + n,
        w = pt(w),
        h = pt(h),
        color = spec.color & 0xFF_FFFF,
    )
}

/// Outermost `name` elements in `xml`: their byte ranges, in order. Nested
/// elements of the same name stay inside their outer one's range.
fn element_spans(xml: &str, name: &str) -> Vec<(usize, usize)> {
    let mut parser = XmlParser::new(xml);
    let mut out = Vec::new();
    let mut open: Option<(usize, usize)> = None; // (start, depth)
    let mut depth = 0usize;
    loop {
        match parser.next() {
            Event::Start => {
                depth += 1;
                if open.is_none() && parser.name() == name {
                    open = Some((parser.start_pos(), depth));
                }
            }
            Event::End => {
                if let Some((start, d)) = open {
                    if d == depth && parser.name() == name {
                        out.push((start, parser.pos()));
                        open = None;
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => return out,
            Event::Text => {}
        }
    }
}

fn is_watermark_sdt(sdt: &str) -> bool {
    // The gallery is the sdt's own property, not a nested control's.
    let props = element_spans(sdt, "w:sdtPr")
        .first()
        .map_or("", |&(a, b)| &sdt[a..b]);
    props.contains("w:docPartGallery w:val=\"Watermarks\"")
}

/// Remove ranges (sorted, disjoint) from `xml`.
fn cut(xml: &str, mut spans: Vec<(usize, usize)>) -> String {
    spans.sort_unstable();
    let mut out = String::with_capacity(xml.len());
    let mut at = 0;
    for (a, b) in spans {
        if a >= at {
            out.push_str(&xml[at..a]);
            at = b;
        }
    }
    out.push_str(&xml[at..]);
    out
}

/// What makes a paragraph or a gallery control more than an emptied shell:
/// runs, tables, fields and links, equations, and the range markers
/// (bookmarks, comments, permissions, moves) that anchor elsewhere.
const CONTENT: [&str; 19] = [
    "w:r",
    "w:tbl",
    "w:hyperlink",
    "w:fldSimple",
    "w:sdt",
    "w:smartTag",
    "w:customXml",
    "m:oMath",
    "m:oMathPara",
    "w:bookmarkStart",
    "w:bookmarkEnd",
    "w:commentRangeStart",
    "w:commentRangeEnd",
    "w:permStart",
    "w:permEnd",
    "w:moveFromRangeStart",
    "w:moveToRangeStart",
    "w:moveFromRangeEnd",
    "w:moveToRangeEnd",
];

/// Whether a slice holds content of its own (see [`CONTENT`]).
fn has_content(xml: &str) -> bool {
    CONTENT.iter().any(|n| !element_spans(xml, n).is_empty())
}

/// Elements whose children are blocks, and which must keep one.
const CONTAINERS: [&str; 6] = [
    "w:hdr",
    "w:ftr",
    "w:tc",
    "w:sdtContent",
    "w:txbxContent",
    "w:body",
];
const BLOCKS: [&str; 5] = ["w:p", "w:tbl", "w:sdt", "w:customXml", "w:altChunk"];

/// Whether the element at `a..b` is the only block its innermost container
/// holds: removing it would leave a header, a cell or a control with none.
fn sole_block(xml: &str, a: usize, b: usize) -> bool {
    // The innermost container around a..b: open containers as a stack.
    let mut parser = XmlParser::new(xml);
    let mut open: Vec<Option<usize>> = Vec::new();
    let mut best: Option<(usize, usize)> = None;
    loop {
        match parser.next() {
            Event::Start => {
                let container = CONTAINERS.contains(&parser.name());
                open.push(container.then(|| parser.start_pos()));
            }
            Event::End => {
                if let Some(Some(s)) = open.pop() {
                    let e = parser.pos();
                    if s < a && b <= e && best.is_none_or(|(bs, be)| e - s < be - bs) {
                        best = Some((s, e));
                    }
                }
            }
            Event::Eof => break,
            Event::Text => {}
        }
    }
    let Some((s, e)) = best else {
        return false;
    };
    // Its direct block children.
    let mut parser = XmlParser::new(&xml[s..e]);
    let mut depth = 0usize;
    let mut blocks = 0usize;
    loop {
        match parser.next() {
            Event::Start => {
                depth += 1;
                if depth == 2 && BLOCKS.contains(&parser.name()) {
                    blocks += 1;
                }
            }
            Event::End => depth = depth.saturating_sub(1),
            Event::Eof => return blocks <= 1,
            Event::Text => {}
        }
    }
}

/// Remove every run that holds a watermark (by the readers' own test,
/// [`crate::package::holds_watermark`]: a text, picture or DrawingML one),
/// and the paragraph it sat in when that keeps no content and is not the
/// last block of its header, cell or control.
fn strip_watermark_runs(xml: &str) -> String {
    let mut out = xml.to_string();
    loop {
        let runs: Vec<(usize, usize)> = element_spans(&out, "w:p")
            .into_iter()
            .flat_map(|(pa, pb)| {
                element_spans(&out[pa..pb], "w:r")
                    .into_iter()
                    .map(move |(a, b)| (pa + a, pa + b))
            })
            .filter(|&(a, b)| crate::package::holds_watermark(&out[a..b]))
            .collect();
        let Some(&(a, b)) = runs.first() else {
            return out;
        };
        let para = element_spans(&out, "w:p")
            .into_iter()
            .find(|&(pa, pb)| pa <= a && b <= pb);
        let next = cut(&out, vec![(a, b)]);
        out = match para {
            Some((pa, pb)) => {
                let pb = pb - (b - a);
                if !has_content(&next[pa..pb]) && !sole_block(&next, pa, pb) {
                    cut(&next, vec![(pa, pb)])
                } else {
                    next
                }
            }
            None => next,
        };
    }
}

/// Remove every watermark from a header part's XML: a run that holds one
/// (its paragraph too when nothing else is left in it), and a "Watermarks"
/// gallery `w:sdt` whole when the watermark was all it held. A gallery
/// control someone typed into keeps what they typed. Other content stays
/// byte for byte. Nothing is left without the block the schema requires:
/// every removal first asks [`sole_block`], so a header, cell or control
/// keeps its last one.
pub fn strip_watermarks(xml: &str) -> String {
    if !crate::package::holds_watermark(xml) && !xml.contains("w:val=\"Watermarks\"") {
        return xml.to_string();
    }
    let mut empty_sdts: Vec<(usize, usize)> = element_spans(xml, "w:sdt")
        .into_iter()
        .filter(|&(a, b)| {
            let sdt = &xml[a..b];
            let content = element_spans(sdt, "w:sdtContent")
                .first()
                .map_or("", |&(ca, cb)| &sdt[ca..cb]);
            is_watermark_sdt(sdt) && !has_content(&strip_watermark_runs(content))
        })
        .collect();
    // From the end, so earlier spans stay put; a control that is the only
    // block of its container leaves an empty paragraph in its place.
    empty_sdts.sort_unstable();
    let mut out = xml.to_string();
    for &(a, b) in empty_sdts.iter().rev() {
        let keep = if sole_block(&out, a, b) { "<w:p/>" } else { "" };
        out.replace_range(a..b, keep);
    }
    strip_watermark_runs(&out)
}

/// A header part's XML with `spec`'s watermark as its first block (after
/// removing any watermark it had), the VML prefixes declared on its root.
/// The header's own paragraphs stay outside the gallery control, as in Word,
/// so what is typed in the header is never part of the watermark.
pub fn insert_watermark(xml: &str, spec: &TextWatermarkSpec, n: u32) -> String {
    let stripped = strip_watermarks(xml);
    let xml = ensure_root_namespaces(
        &stripped,
        "w:hdr",
        &[
            ("w", W_NS),
            ("r", R_NS),
            ("v", VML_NS),
            ("o", OFFICE_NS),
            ("w10", W10_NS),
        ],
    );
    let Some(gt) = root_start_end(&xml, "w:hdr") else {
        return xml;
    };
    let block = watermark_xml(spec, n);
    if xml[..gt].ends_with('/') {
        return format!("{}>{block}<w:p/></w:hdr>{}", &xml[..gt - 1], &xml[gt + 1..]);
    }
    format!("{}{block}{}", &xml[..gt + 1], &xml[gt + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::text_watermarks;

    const HDR: &str = "<?xml version=\"1.0\"?><w:hdr xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
        <w:p><w:pPr><w:pStyle w:val=\"Header\"/></w:pPr><w:r><w:t>Acme Corp</w:t></w:r></w:p></w:hdr>";

    #[test]
    fn preset_is_read_back_by_the_watermark_reader() {
        let spec = TextWatermarkSpec::preset("DRAFT", true);
        let out = insert_watermark(HDR, &spec, 1);
        let marks = text_watermarks(&out);
        assert_eq!(marks.len(), 1, "{out}");
        assert_eq!(marks[0].text, "DRAFT");
        assert_eq!(marks[0].rotation, 315.0);
        assert_eq!(marks[0].fill, Some((0xC0, 0xC0, 0xC0)));
        assert_eq!(marks[0].font_size_pt, None, "Auto writes font-size:1pt");
        assert_eq!(marks[0].font.as_deref(), Some("Calibri"));
        assert_eq!(marks[0].opacity, Some(0.5));
        assert!(out.contains("<v:fill opacity=\".5\"/>"));
        assert!(out.contains("id=\"PowerPlusWaterMarkObject1\""));
        assert!(out.contains("w:docPartGallery w:val=\"Watermarks\""));
        // The watermark is the first block; the header text stays.
        let first = out.find("<w:sdt>").unwrap();
        assert!(first < out.find("Acme Corp").unwrap());
        for ns in ["xmlns:v=", "xmlns:o=", "xmlns:w10=", "xmlns:r="] {
            assert!(out.contains(ns), "{ns} declared: {out}");
        }
    }

    #[test]
    fn custom_spec_writes_size_colour_layout_and_escapes_text() {
        let spec = TextWatermarkSpec {
            text: "R&D <only> \"x\"".into(),
            font: "Times New Roman".into(),
            size_pt: Some(54.0),
            color: 0xFF0000,
            semitransparent: false,
            diagonal: false,
        };
        let out = insert_watermark(HDR, &spec, 7);
        let marks = text_watermarks(&out);
        assert_eq!(marks.len(), 1, "{out}");
        assert_eq!(marks[0].text, "R&D <only> \"x\"");
        assert_eq!(marks[0].rotation, 0.0);
        assert_eq!(marks[0].fill, Some((0xFF, 0, 0)));
        assert_eq!(marks[0].font_size_pt, Some(54.0));
        assert_eq!(marks[0].font.as_deref(), Some("Times New Roman"));
        assert_eq!(marks[0].opacity, None, "opaque");
        assert!(!out.contains("opacity"));
        assert!(out.contains("font-family:&quot;Times New Roman&quot;"));
    }

    #[test]
    fn a_second_watermark_replaces_the_first() {
        let once = insert_watermark(HDR, &TextWatermarkSpec::preset("DRAFT", true), 1);
        let twice = insert_watermark(&once, &TextWatermarkSpec::preset("SAMPLE", false), 2);
        let marks = text_watermarks(&twice);
        assert_eq!(marks.len(), 1, "{twice}");
        assert_eq!(marks[0].text, "SAMPLE");
        assert_eq!(twice.matches("<v:shapetype").count(), 1);
        // Namespaces are declared once.
        assert_eq!(twice.matches("xmlns:v=").count(), 1);
    }

    #[test]
    fn strip_keeps_other_header_content() {
        let with = insert_watermark(HDR, &TextWatermarkSpec::preset("DRAFT", true), 1);
        let without = strip_watermarks(&with);
        assert!(text_watermarks(&without).is_empty());
        assert!(without.contains(
            "<w:p><w:pPr><w:pStyle w:val=\"Header\"/></w:pPr><w:r><w:t>Acme Corp</w:t></w:r></w:p>"
        ));
        assert!(!without.contains("<w:sdt"));
    }

    #[test]
    fn strip_removes_a_bare_watermark_run_and_its_empty_paragraph() {
        // A watermark run beside header text keeps the text; one alone takes
        // its paragraph with it; a non-watermark sdt stays.
        let hdr = "<w:hdr xmlns:w=\"W\" xmlns:v=\"V\">\
            <w:sdt><w:sdtPr><w:alias w:val=\"Title\"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>T</w:t></w:r></w:p></w:sdtContent></w:sdt>\
            <w:p><w:r><w:t>Left</w:t></w:r><w:r><w:pict><v:shape id=\"PowerPlusWaterMarkObject9\"><v:textpath string=\"A\"/></v:shape></w:pict></w:r></w:p>\
            <w:p><w:pPr/><w:r><w:pict><v:shape id=\"PowerPlusWaterMarkObject10\"><v:textpath string=\"B\"/></v:shape></w:pict></w:r></w:p></w:hdr>";
        let out = strip_watermarks(hdr);
        assert_eq!(
            out,
            "<w:hdr xmlns:w=\"W\" xmlns:v=\"V\">\
            <w:sdt><w:sdtPr><w:alias w:val=\"Title\"/></w:sdtPr><w:sdtContent><w:p><w:r><w:t>T</w:t></w:r></w:p></w:sdtContent></w:sdt>\
            <w:p><w:r><w:t>Left</w:t></w:r></w:p></w:hdr>"
        );
    }

    #[test]
    fn a_header_keeps_its_own_paragraph_outside_the_watermark() {
        let hdr = "<w:hdr xmlns:w=\"W\"><w:p/></w:hdr>";
        let with = insert_watermark(hdr, &TextWatermarkSpec::preset("URGENT", true), 3);
        assert!(with.ends_with("</w:sdt><w:p/></w:hdr>"), "{with}");
        assert_eq!(strip_watermarks(&with).matches("<w:p/>").count(), 1);
        // A self-closing root takes the block too.
        let empty = insert_watermark(
            "<w:hdr xmlns:w=\"W\"/>",
            &TextWatermarkSpec::preset("ASAP", false),
            4,
        );
        assert_eq!(text_watermarks(&empty).len(), 1, "{empty}");
        assert!(empty.ends_with("</w:sdt><w:p/></w:hdr>"), "{empty}");
    }

    /// Text typed into a Watermarks gallery control's paragraph is not the
    /// watermark: stripping keeps it, and the control around it.
    #[test]
    fn strip_keeps_text_typed_into_the_watermark_control() {
        let with = insert_watermark(HDR, &TextWatermarkSpec::preset("DRAFT", true), 1);
        let typed = with.replacen(
            "<w:rPr><w:noProof/></w:rPr><w:pict>",
            "<w:rPr><w:noProof/></w:rPr><w:t>Acme Corp</w:t></w:r><w:r><w:pict>",
            1,
        );
        assert_ne!(typed, with);
        let out = strip_watermarks(&typed);
        assert!(text_watermarks(&out).is_empty(), "{out}");
        assert!(out.contains("<w:t>Acme Corp</w:t>"), "{out}");
        assert!(out.contains("w:docPartGallery"), "the control stays: {out}");
        // A second watermark goes in beside it, never two.
        let again = insert_watermark(&typed, &TextWatermarkSpec::preset("SAMPLE", true), 2);
        assert_eq!(text_watermarks(&again).len(), 1);
        assert!(again.contains("<w:t>Acme Corp</w:t>"));
    }

    /// A gallery control holding Word's picture watermark (or a DrawingML
    /// one) is a watermark too: strip removes it, and a new watermark
    /// replaces it rather than joining it.
    #[test]
    fn picture_and_drawingml_watermarks_are_stripped_and_replaced() {
        let gallery = |run: &str| {
            format!(
                "<w:hdr xmlns:w=\"W\" xmlns:v=\"V\" xmlns:wp=\"WP\"><w:sdt><w:sdtPr><w:docPartObj>\
                 <w:docPartGallery w:val=\"Watermarks\"/></w:docPartObj></w:sdtPr><w:sdtContent>\
                 <w:p><w:r>{run}</w:r></w:p></w:sdtContent></w:sdt><w:p><w:r><w:t>Keep</w:t></w:r></w:p></w:hdr>"
            )
        };
        let vml = gallery(
            "<w:pict><v:shape id=\"WordPictureWatermark123\"><v:imagedata r:id=\"rImg\"/></v:shape></w:pict>",
        );
        let dml = gallery(
            "<w:drawing><wp:anchor><wp:docPr id=\"1\" name=\"Picture 1 Watermark\"/></wp:anchor></w:drawing>",
        );
        for hdr in [vml, dml] {
            assert!(crate::package::holds_watermark(&hdr));
            let out = strip_watermarks(&hdr);
            assert!(!crate::package::holds_watermark(&out), "{out}");
            assert!(!out.contains("<w:sdt>"), "the emptied control goes: {out}");
            assert!(out.contains("<w:t>Keep</w:t>"));
            let replaced = insert_watermark(&hdr, &TextWatermarkSpec::preset("DRAFT", true), 1);
            assert_eq!(replaced.matches("<w:sdt>").count(), 1, "{replaced}");
            assert_eq!(text_watermarks(&replaced).len(), 1);
            assert!(
                !replaced.contains("WordPictureWatermark")
                    && !replaced.contains("Picture 1 Watermark")
            );
        }
    }

    /// A watermark alone in a table cell's paragraph leaves the paragraph
    /// (a cell needs one); equations and bookmarks keep a paragraph too.
    #[test]
    fn an_emptied_paragraph_stays_when_its_container_needs_it_or_it_anchors_something() {
        let mark = "<w:r><w:pict><v:shape id=\"PowerPlusWaterMarkObject1\"><v:textpath string=\"X\"/></v:shape></w:pict></w:r>";
        let cell = format!(
            "<w:hdr xmlns:w=\"W\" xmlns:v=\"V\"><w:tbl><w:tr><w:tc><w:tcPr/><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr>{mark}</w:p></w:tc></w:tr></w:tbl><w:p/></w:hdr>"
        );
        let out = strip_watermarks(&cell);
        assert!(
            out.contains(
                "<w:tc><w:tcPr/><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr></w:p></w:tc>"
            ),
            "{out}"
        );
        for anchor in [
            "<m:oMath><m:r><m:t>x</m:t></m:r></m:oMath>",
            "<w:bookmarkStart w:id=\"0\" w:name=\"b\"/><w:bookmarkEnd w:id=\"0\"/>",
            "<w:commentRangeStart w:id=\"1\"/>",
            "<w:moveToRangeEnd w:id=\"2\"/>",
        ] {
            let hdr = format!(
                "<w:hdr xmlns:w=\"W\" xmlns:v=\"V\" xmlns:m=\"M\"><w:p>{anchor}{mark}</w:p><w:p/></w:hdr>"
            );
            let out = strip_watermarks(&hdr);
            assert!(out.contains(anchor), "{out}");
            assert!(!out.contains("PowerPlus"));
        }
        // Two paragraphs: the emptied one goes.
        let two = format!(
            "<w:hdr xmlns:w=\"W\" xmlns:v=\"V\"><w:p>{mark}</w:p><w:p><w:r><w:t>T</w:t></w:r></w:p></w:hdr>"
        );
        assert_eq!(
            strip_watermarks(&two),
            "<w:hdr xmlns:w=\"W\" xmlns:v=\"V\"><w:p><w:r><w:t>T</w:t></w:r></w:p></w:hdr>"
        );
    }
}
