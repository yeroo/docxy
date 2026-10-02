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

use crate::page_bg::{OFFICE_NS, VML_NS, ensure_root_namespaces};
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

/// Remove every watermark from a header part's XML: a "Watermarks" gallery
/// `w:sdt` whole, and anywhere else the run whose `w:pict` holds a
/// `PowerPlusWaterMarkObject` shape (its paragraph too when nothing else is
/// left in it). Other content stays byte for byte; a header left with no
/// paragraph gets an empty one, as the schema requires.
pub fn strip_watermarks(xml: &str) -> String {
    if !xml.contains(SHAPE_ID) && !xml.contains("w:val=\"Watermarks\"") {
        return xml.to_string();
    }
    let sdts: Vec<(usize, usize)> = element_spans(xml, "w:sdt")
        .into_iter()
        .filter(|&(a, b)| is_watermark_sdt(&xml[a..b]))
        .collect();
    let mut out = cut(xml, sdts);
    // Watermark runs outside a gallery control (older Word, other producers).
    loop {
        let runs: Vec<(usize, usize)> = element_spans(&out, "w:p")
            .into_iter()
            .flat_map(|(pa, pb)| {
                element_spans(&out[pa..pb], "w:r")
                    .into_iter()
                    .map(move |(a, b)| (pa + a, pa + b))
            })
            .filter(|&(a, b)| out[a..b].contains(SHAPE_ID))
            .collect();
        let Some(&(a, b)) = runs.first() else {
            break;
        };
        // The paragraph the run sits in goes too when it keeps no run.
        let para = element_spans(&out, "w:p")
            .into_iter()
            .find(|&(pa, pb)| pa <= a && b <= pb);
        let next = cut(&out, vec![(a, b)]);
        out = match para {
            Some((pa, pb)) => {
                let rest = &next[pa..pb - (b - a)];
                if element_spans(rest, "w:r").is_empty() && !rest.contains("<w:hyperlink") {
                    cut(&next, vec![(pa, pb - (b - a))])
                } else {
                    next
                }
            }
            None => next,
        };
    }
    ensure_a_paragraph(&out)
}

/// A `w:hdr` with no block left gets an empty paragraph.
fn ensure_a_paragraph(xml: &str) -> String {
    let has_block = ["w:p", "w:tbl", "w:sdt", "w:customXml", "w:altChunk"]
        .iter()
        .any(|n| !element_spans(xml, n).is_empty());
    if has_block {
        return xml.to_string();
    }
    match xml.rfind("</w:hdr>") {
        Some(at) => format!("{}<w:p/>{}", &xml[..at], &xml[at..]),
        None => xml.to_string(),
    }
}

/// A header part's XML with `spec`'s watermark as its first block (after
/// removing any watermark it had), the VML prefixes declared on its root.
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
    let Some(gt) = hdr_start_end(&xml) else {
        return xml;
    };
    let block = watermark_xml(spec, n);
    if xml[..gt].ends_with('/') {
        return format!("{}>{block}</w:hdr>{}", &xml[..gt - 1], &xml[gt + 1..]);
    }
    // An empty placeholder paragraph a new or emptied header holds is the
    // watermark's paragraph now.
    let rest = &xml[gt + 1..];
    let rest = match rest.strip_prefix("<w:p/>") {
        Some(r) if r.trim_start().starts_with("</w:hdr>") => r,
        _ => rest,
    };
    format!("{}{block}{rest}", &xml[..gt + 1])
}

/// The index of the `>` that ends the `w:hdr` start tag.
fn hdr_start_end(xml: &str) -> Option<usize> {
    let mut parser = XmlParser::new(xml);
    loop {
        match parser.next() {
            Event::Start if parser.name() == "w:hdr" => {
                let end = parser.pos();
                // A self-closing root reports its End next; pos is past `/>`.
                return end.checked_sub(1);
            }
            Event::Eof => return None,
            _ => {}
        }
    }
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
    fn a_header_holding_only_a_watermark_keeps_a_paragraph_when_stripped() {
        let hdr = "<w:hdr xmlns:w=\"W\"><w:p/></w:hdr>";
        let with = insert_watermark(hdr, &TextWatermarkSpec::preset("URGENT", true), 3);
        assert!(
            !with.contains("<w:p/>"),
            "the placeholder becomes the mark: {with}"
        );
        assert_eq!(strip_watermarks(&with).matches("<w:p/>").count(), 1);
        // A self-closing root takes the block too.
        let empty = insert_watermark(
            "<w:hdr xmlns:w=\"W\"/>",
            &TextWatermarkSpec::preset("ASAP", false),
            4,
        );
        assert_eq!(text_watermarks(&empty).len(), 1, "{empty}");
        assert!(empty.ends_with("</w:hdr>"));
    }
}
