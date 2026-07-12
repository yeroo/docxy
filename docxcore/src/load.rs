//! Load a `.docx` into the [`crate::model`] document tree.
//!
//! `load` opens the OPC ZIP and parses `word/document.xml` (resolving hyperlink
//! relationships from `word/_rels/document.xml.rels`). The XML-level functions
//! ([`parse_document_xml`], [`parse_rels_xml`]) are public so they can be unit
//! tested directly without constructing a ZIP.

use std::collections::HashMap;
use std::fmt;

use crate::model::*;
use crate::xml::{Event, XmlParser};
use crate::zip::ZipArchive;

#[derive(Debug, PartialEq, Eq)]
pub enum LoadError {
    /// Not a ZIP/OPC container.
    NotZip,
    /// A legacy OLE2 binary `.doc` (or encrypted) — not supported.
    Ole2,
    /// `word/document.xml` was missing.
    MissingDocument,
    /// `word/document.xml` was not valid UTF-8.
    NotUtf8,
    /// A ZIP entry could not be decompressed.
    CorruptPart,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            LoadError::NotZip => "not a valid .docx file (ZIP archive could not be read)",
            LoadError::Ole2 => {
                "OLE2 compound file: a legacy binary .doc or encrypted document (unsupported)"
            }
            LoadError::MissingDocument => "missing word/document.xml",
            LoadError::NotUtf8 => "word/document.xml is not valid UTF-8",
            LoadError::CorruptPart => "a ZIP entry could not be decompressed",
        };
        f.write_str(s)
    }
}

impl std::error::Error for LoadError {}

/// `rId` -> (target, is_external) from a `.rels` part, plus SmartArt node text
/// keyed by the diagram-data relationship id (`<dgm:relIds r:dm>`), resolved from
/// the external diagram parts at load time.
#[derive(Debug, Default, Clone)]
pub struct Relationships {
    map: HashMap<String, (String, bool)>,
    diagrams: HashMap<String, Vec<String>>,
    /// Decoded equation text keyed by the OLE object relationship id
    /// (`<o:OLEObject r:id>`), resolved from the embedded `Equation.3` objects.
    equations: HashMap<String, String>,
    /// Parsed charts keyed by the `<c:chart r:id>` relationship id.
    charts: HashMap<String, crate::chart::Chart>,
}

impl Relationships {
    pub fn target(&self, id: &str) -> Option<&str> {
        self.map.get(id).map(|(t, _)| t.as_str())
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Load a document from the raw bytes of a `.docx` file.
pub fn load(data: &[u8]) -> Result<Document, LoadError> {
    const OLE2: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    let zip = match ZipArchive::open(data) {
        Some(z) => z,
        None => {
            if data.len() >= 8 && data[..8] == OLE2 {
                return Err(LoadError::Ole2);
            }
            return Err(LoadError::NotZip);
        }
    };
    let doc_bytes = zip
        .read("word/document.xml")
        .ok_or(LoadError::MissingDocument)?;
    let doc_xml = std::str::from_utf8(&doc_bytes).map_err(|_| LoadError::NotUtf8)?;
    let rels = match zip.read("word/_rels/document.xml.rels") {
        Some(b) => {
            let xml = std::str::from_utf8(&b).unwrap_or("").to_string();
            let mut r = parse_rels_xml(&xml);
            r.diagrams = collect_diagram_texts(&xml, |n| zip.read(n));
            r.equations = collect_equation_texts(&xml, |n| zip.read(n));
            r.charts = collect_chart_data(&xml, |n| zip.read(n));
            r
        }
        None => Relationships::default(),
    };
    Ok(parse_document_xml(doc_xml, &rels))
}

// ---- small helpers ----

fn decode_attr(raw: &str) -> String {
    let mut s = String::new();
    XmlParser::append_decoded(raw, &mut s);
    s
}

fn parse_int(s: &str) -> i32 {
    let b = s.as_bytes();
    let mut v = 0i32;
    let mut neg = false;
    let mut i = 0;
    if !b.is_empty() && (b[0] == b'-' || b[0] == b'+') {
        neg = b[0] == b'-';
        i = 1;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        v = v.wrapping_mul(10).wrapping_add((b[i] - b'0') as i32);
        i += 1;
    }
    if neg { -v } else { v }
}

/// OOXML toggle property: present means on unless explicitly disabled.
fn toggle_on(val: &str) -> bool {
    !(val == "0" || val == "false" || val == "off" || val == "none")
}

fn map_align(jc: &str) -> Align {
    match jc {
        "center" => Align::Center,
        "right" | "end" => Align::Right,
        "both" | "distribute" => Align::Justify,
        _ => Align::Left,
    }
}

/// Map a paragraph style id to a heading level (`Heading2` → 2), if it is one.
pub fn heading_level(style_id: &str) -> Option<u8> {
    let t = style_id.replace([' ', '-', '_'], "").to_ascii_lowercase();
    let rest = t.strip_prefix("heading")?;
    let n: u8 = rest.parse().ok()?;
    (1..=9).contains(&n).then_some(n)
}

// ---- relationships ----

pub fn parse_rels_xml(xml: &str) -> Relationships {
    let mut rels = Relationships::default();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Relationship" {
                    let id = decode_attr(p.attr("Id"));
                    let target = decode_attr(p.attr("Target"));
                    let external = p.attr("TargetMode") == "External";
                    if !id.is_empty() {
                        rels.map.insert(id, (target, external));
                    }
                    p.skip_element();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    rels
}

/// Resolve every SmartArt diagram's node text, keyed by its `diagramData`
/// relationship id. The drawing in `document.xml` only references the diagram via
/// `<dgm:relIds r:dm="rIdN">`; the actual node text lives in the external
/// `word/diagrams/dataN.xml` part, which we read and flatten here.
pub(crate) fn collect_diagram_texts<F>(rels_xml: &str, read_part: F) -> HashMap<String, Vec<String>>
where
    F: Fn(&str) -> Option<Vec<u8>>,
{
    let mut out = HashMap::new();
    let mut p = XmlParser::new(rels_xml);
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Relationship" {
                    let ty = p.attr("Type");
                    if ty.ends_with("/diagramData") {
                        let id = decode_attr(p.attr("Id"));
                        let target = decode_attr(p.attr("Target"));
                        if !id.is_empty() && !target.is_empty() {
                            let part = resolve_word_part(&target);
                            if let Some(b) = read_part(&part) {
                                let texts =
                                    extract_diagram_text(std::str::from_utf8(&b).unwrap_or(""));
                                if !texts.is_empty() {
                                    out.insert(id, texts);
                                }
                            }
                        }
                    }
                    p.skip_element();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Attach resolved SmartArt diagram text to a `Relationships`, keyed by the
/// diagram-data relationship id. Public so the package loader can reuse it.
pub(crate) fn set_diagram_texts(rels: &mut Relationships, diagrams: HashMap<String, Vec<String>>) {
    rels.diagrams = diagrams;
}

/// Decode every embedded legacy equation (`Equation.3` OLE object) to Unicode
/// text, keyed by its relationship id, so `<o:OLEObject r:id>` runs can render as
/// inline text instead of the raster preview.
pub(crate) fn collect_equation_texts<F>(rels_xml: &str, read_part: F) -> HashMap<String, String>
where
    F: Fn(&str) -> Option<Vec<u8>>,
{
    let mut out = HashMap::new();
    let mut p = XmlParser::new(rels_xml);
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Relationship" {
                    if p.attr("Type").ends_with("/oleObject") {
                        let id = decode_attr(p.attr("Id"));
                        let target = decode_attr(p.attr("Target"));
                        if !id.is_empty() && !target.is_empty() {
                            let part = resolve_word_part(&target);
                            if let Some(b) = read_part(&part) {
                                if let Some(t) = crate::equation::decode(&b) {
                                    out.insert(id, t);
                                }
                            }
                        }
                    }
                    p.skip_element();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Attach decoded equation text to a `Relationships`, keyed by OLE relationship id.
pub(crate) fn set_equation_texts(rels: &mut Relationships, equations: HashMap<String, String>) {
    rels.equations = equations;
}

/// Parse every chart part referenced from the document rels, keyed by its
/// relationship id (the `<c:chart r:id>` in the drawing points at it).
pub(crate) fn collect_chart_data<F>(
    rels_xml: &str,
    read_part: F,
) -> HashMap<String, crate::chart::Chart>
where
    F: Fn(&str) -> Option<Vec<u8>>,
{
    let mut out = HashMap::new();
    let mut p = XmlParser::new(rels_xml);
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "Relationship" && p.attr("Type").ends_with("/chart") {
                    let id = decode_attr(p.attr("Id"));
                    let target = decode_attr(p.attr("Target"));
                    if !id.is_empty() && !target.is_empty() {
                        if let Some(b) = read_part(&resolve_word_part(&target)) {
                            if let Some(c) = std::str::from_utf8(&b)
                                .ok()
                                .and_then(crate::chart::parse_chart_xml)
                            {
                                out.insert(id, c);
                            }
                        }
                    }
                    p.skip_element();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Attach parsed charts to a `Relationships`, keyed by chart relationship id.
pub(crate) fn set_chart_data(
    rels: &mut Relationships,
    charts: HashMap<String, crate::chart::Chart>,
) {
    rels.charts = charts;
}

/// Resolve a relationship `Target` (relative to `word/`) to a package part name,
/// collapsing any leading `../` segments.
fn resolve_word_part(target: &str) -> String {
    let t = target.trim_start_matches('/');
    let mut base = vec!["word"];
    for seg in t.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                base.pop();
            }
            s => base.push(s),
        }
    }
    base.join("/")
}

/// Pull the ordered node text out of a diagram data part (`<a:t>` runs inside the
/// `<dgm:dataModel>` point list). Blank nodes (layout placeholders) are dropped.
fn extract_diagram_text(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if p.name() == "a:t" => {
                let t = read_text(&mut p);
                let t = t.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
            }
            Event::Start => {}
            Event::End => {}
            Event::Text => {}
            Event::Eof => break,
        }
    }
    out
}

/// Resolve a run holding a drawing/object: a decoded equation becomes inline
/// text, a SmartArt diagram becomes a `SmartArt` box, otherwise the run is
/// preserved verbatim as `Raw`.
fn drawing_inline(raw: String, rels: &Relationships) -> Inline {
    // A Mermaid diagram we generated: its source is embedded in the drawing's
    // descr. Recover it as a SmartArt box (node labels shown in the terminal,
    // raw preserved for save, source recoverable on Markdown export). Checked
    // before the text-box case because our node shapes also use txbxContent.
    if let Some(src) = crate::mermaid::source_of(&raw) {
        let text = crate::mermaid::labels(&src);
        return Inline::SmartArt { raw, text };
    }
    // A text box / shape with text: model its content so the caret can enter it.
    if raw.contains("txbxContent") {
        let blocks = parse_textbox_blocks(&raw);
        if !blocks.is_empty() {
            return Inline::TextBox { raw, blocks };
        }
    }
    // Legacy equation: the OLE object's r:id points at an `Equation.3` we decoded.
    if let Some(i) = raw.find("OLEObject") {
        if let Some(id) = raw_attr(&raw[i..], ":id=\"") {
            if let Some(text) = rels.equations.get(&id) {
                return Inline::Equation {
                    raw,
                    text: text.clone(),
                    latex: None,
                };
            }
        }
    }
    if let Some(dm) = raw_attr(&raw, ":dm=\"") {
        if let Some(text) = rels.diagrams.get(&dm) {
            return Inline::SmartArt {
                raw,
                text: text.clone(),
            };
        }
    }
    // A DrawingML chart: `<c:chart r:id="rIdN"/>` points at a parsed chart part.
    if raw.contains("c:chart") {
        if let Some(id) = raw_attr(&raw, ":id=\"") {
            if let Some(chart) = rels.charts.get(&id) {
                return Inline::Chart {
                    raw,
                    chart: chart.clone(),
                };
            }
        }
    }
    Inline::Raw(raw)
}

/// The quoted value of the first attribute whose text ends with `key` (e.g.
/// `:dm="`), used to read namespace-prefixed relationship ids out of raw XML.
fn raw_attr(s: &str, key: &str) -> Option<String> {
    let i = s.find(key)? + key.len();
    let rest = &s[i..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ---- document ----

pub fn parse_document_xml(xml: &str, rels: &Relationships) -> Document {
    let mut p = XmlParser::new(xml);
    let mut body = Vec::new();
    // Scan to <w:body>, then parse its children.
    loop {
        match p.next() {
            Event::Start if p.name() == "w:body" => {
                body = parse_blocks_until_end(&mut p, rels);
                break;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Document { body }
}

/// Parse a header (`word/headerN.xml`) or footer (`word/footerN.xml`) part into
/// its block content (the children of `<w:hdr>`/`<w:ftr>`).
pub fn parse_header_footer(xml: &str, rels: &Relationships) -> Vec<Block> {
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start if p.name() == "w:hdr" || p.name() == "w:ftr" => {
                return parse_blocks_until_end(&mut p, rels);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Vec::new()
}

/// Unwrap a block-level `<w:sdt>` (content control), appending the block content
/// of its `<w:sdtContent>` to `out`. `sdtPr`/`sdtEndPr` are ignored.
fn parse_sdt_block(p: &mut XmlParser, rels: &Relationships, out: &mut Vec<Block>) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:sdtContent" => out.extend(parse_blocks_until_end(p, rels)),
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

/// Parse a sequence of block-level children up to the enclosing End event.
fn parse_blocks_until_end(p: &mut XmlParser, rels: &Relationships) -> Vec<Block> {
    let mut blocks = Vec::new();
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:p" => blocks.push(Block::Paragraph(parse_paragraph(p, rels))),
                "w:tbl" => blocks.push(Block::Table(parse_table(p, rels))),
                // A block-level structured-document-tag (content control: cover
                // pages, TOC, …) — unwrap and parse its content so it's visible.
                // The control wrapper itself is not reconstructed on save.
                "w:sdt" => parse_sdt_block(p, rels, &mut blocks),
                // sectPr is preserved by the package layer; don't duplicate it.
                "w:sectPr" => p.skip_element(),
                // Block-level OMML math: a paragraph holding the text equation.
                "m:oMathPara" | "m:oMath" => {
                    let start = p.start_pos();
                    p.skip_element();
                    let raw = p.raw_slice(start, p.pos()).to_string();
                    let text = crate::omath::render_omath(&raw);
                    blocks.push(Block::Paragraph(Paragraph {
                        props: Default::default(),
                        content: vec![Inline::Equation {
                            raw,
                            text,
                            latex: None,
                        }],
                    }));
                }
                _ => {
                    // Unmodeled block content (content controls, etc.): preserve raw.
                    let start = p.start_pos();
                    p.skip_element();
                    blocks.push(Block::Raw(p.raw_slice(start, p.pos()).to_string()));
                }
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    blocks
}

fn parse_paragraph(p: &mut XmlParser, rels: &Relationships) -> Paragraph {
    let mut para = Paragraph::default();
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:pPr" => parse_ppr(p, &mut para.props),
                "w:r" => {
                    let start = p.start_pos();
                    let mut tmp = Vec::new();
                    if parse_run(p, &mut tmp) {
                        // Run held a drawing/field/etc. SmartArt becomes a text box;
                        // anything else is preserved verbatim.
                        let raw = p.raw_slice(start, p.pos()).to_string();
                        para.content.push(drawing_inline(raw, rels));
                    } else {
                        para.content.extend(tmp);
                    }
                }
                "w:hyperlink" => parse_hyperlink_into(p, rels, &mut para.content),
                // A simple field: keep its XML verbatim (lossless save) but surface
                // its cached result text so the value is visible.
                "w:fldSimple" => parse_fld_simple(p, rels, &mut para.content),
                // A smart tag wraps runs in (deprecated) metadata; unwrap to its
                // inner content so the text isn't lost.
                "w:smartTag" => parse_inlines_into(p, rels, &mut para.content),
                // Inline content control (content placeholder, etc.): unwrap it.
                "w:sdt" => parse_inline_sdt(p, rels, &mut para.content),
                // OMML math: render it to a text equation (lossless raw kept).
                "m:oMath" | "m:oMathPara" => {
                    let start = p.start_pos();
                    p.skip_element();
                    let raw = p.raw_slice(start, p.pos()).to_string();
                    let text = crate::omath::render_omath(&raw);
                    para.content.push(Inline::Equation {
                        raw,
                        text,
                        latex: None,
                    });
                }
                _ => {
                    // Unmodeled inline content (bookmarks, fields): preserve raw.
                    let start = p.start_pos();
                    p.skip_element();
                    para.content
                        .push(Inline::Raw(p.raw_slice(start, p.pos()).to_string()));
                }
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    if para.props.heading_level.is_none() {
        if let Some(s) = &para.props.style_id {
            para.props.heading_level = heading_level(s);
        }
    }
    para
}

/// Parse a `<w:fldSimple>` field. Its inner runs hold the field's last-computed
/// result; keep the whole element verbatim for a lossless save, but expose the
/// result text so the value renders (a date, page number, cross-reference, …).
fn parse_fld_simple(p: &mut XmlParser, rels: &Relationships, out: &mut Vec<Inline>) {
    // Capture the instruction (e.g. `= 2+2 \# "0.00"`) before advancing.
    let instr = decode_attr(p.attr("w:instr"));
    let start = p.start_pos();
    let mut inner = Vec::new();
    parse_inlines_into(p, rels, &mut inner);
    let raw = p.raw_slice(start, p.pos()).to_string();
    let cached: String = inner.iter().map(Inline::text).collect();
    // Recompute the value where we can (formula fields); otherwise show what Word
    // last cached. The original XML is kept verbatim for a lossless save.
    let text = crate::field::eval_field(&instr).unwrap_or(cached);
    out.push(Inline::Field { raw, text });
}

/// Unwrap an inline `<w:sdt>` (content control inside a paragraph), parsing the
/// inline content of its `<w:sdtContent>` into `out`.
fn parse_inline_sdt(p: &mut XmlParser, rels: &Relationships, out: &mut Vec<Inline>) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:sdtContent" => parse_inlines_into(p, rels, out),
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

/// Parse inline content (runs, hyperlinks, nested content controls) up to the
/// enclosing End, pushing into `out`.
fn parse_inlines_into(p: &mut XmlParser, rels: &Relationships, out: &mut Vec<Inline>) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:r" => {
                    let start = p.start_pos();
                    let mut tmp = Vec::new();
                    if parse_run(p, &mut tmp) {
                        let raw = p.raw_slice(start, p.pos()).to_string();
                        out.push(drawing_inline(raw, rels));
                    } else {
                        out.extend(tmp);
                    }
                }
                "w:hyperlink" => parse_hyperlink_into(p, rels, out),
                "w:fldSimple" => parse_fld_simple(p, rels, out),
                "w:smartTag" => parse_inlines_into(p, rels, out),
                "w:sdt" => parse_inline_sdt(p, rels, out),
                _ => {
                    let start = p.start_pos();
                    p.skip_element();
                    out.push(Inline::Raw(p.raw_slice(start, p.pos()).to_string()));
                }
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

/// Parse the block content of a text box (`<w:txbxContent>`) embedded in a
/// drawing/VML-shape's raw XML, so its text can be shown.
pub fn parse_textbox_blocks(raw: &str) -> Vec<Block> {
    let rels = Relationships::default();
    let mut p = XmlParser::new(raw);
    loop {
        match p.next() {
            Event::Start if p.name() == "w:txbxContent" => {
                return parse_blocks_until_end(&mut p, &rels);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Vec::new()
}

fn parse_ppr(p: &mut XmlParser, props: &mut ParProps) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:pStyle" => {
                    let v = decode_attr(p.attr("w:val"));
                    if !v.is_empty() {
                        props.style_id = Some(v);
                    }
                    p.skip_element();
                }
                "w:jc" => {
                    props.align = map_align(p.attr("w:val"));
                    p.skip_element();
                }
                "w:bidi" => {
                    props.rtl = toggle_on(p.attr("w:val"));
                    p.skip_element();
                }
                "w:numPr" => parse_numpr(p, props),
                "w:tabs" => parse_tab_stops(p, &mut props.tabs),
                "w:pBdr" => props.borders = crate::styles::parse_pbdr(p),
                "w:ind" => {
                    if let Some(v) = frame_int(p, "w:left").or_else(|| frame_int(p, "w:start")) {
                        props.indent = v;
                    }
                    // First-line indent: `w:firstLine` adds to the first line,
                    // `w:hanging` pulls it left (mutually exclusive in the schema).
                    if let Some(v) = frame_int(p, "w:firstLine") {
                        props.first_line = v;
                    } else if let Some(v) = frame_int(p, "w:hanging") {
                        props.first_line = -v;
                    }
                    p.skip_element();
                }
                "w:sectPr" => {
                    // A mid-document section break — preserve it verbatim.
                    let start = p.start_pos();
                    p.skip_element();
                    props.section_break = Some(p.raw_slice(start, p.pos()).to_string());
                }
                "w:framePr" => {
                    props.frame = Some(FramePr {
                        x: frame_int(p, "w:x"),
                        y: frame_int(p, "w:y"),
                        w: frame_int(p, "w:w"),
                        h: frame_int(p, "w:h"),
                        h_anchor: frame_str(p, "w:hAnchor"),
                        v_anchor: frame_str(p, "w:vAnchor"),
                        x_align: frame_str(p, "w:xAlign"),
                        y_align: frame_str(p, "w:yAlign"),
                    });
                    p.skip_element();
                }
                // Any pPr child we don't model (shading, spacing, keepNext,
                // outlineLvl, …) is preserved verbatim so save doesn't drop it.
                _ => capture_element(p, &mut props.raw_props),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

/// Capture the verbatim XML of the element the parser is positioned at (its
/// Start event), consuming it, and append it to `out`.
fn capture_element(p: &mut XmlParser, out: &mut Vec<String>) {
    let start = p.start_pos();
    p.skip_element();
    out.push(p.raw_slice(start, p.pos()).to_string());
}

fn frame_int(p: &XmlParser, name: &str) -> Option<i32> {
    let v = p.attr(name);
    (!v.is_empty()).then(|| parse_int(v))
}

fn frame_str(p: &XmlParser, name: &str) -> Option<String> {
    let v = p.attr(name);
    (!v.is_empty()).then(|| decode_attr(v))
}

fn parse_tab_stops(p: &mut XmlParser, tabs: &mut Vec<TabStop>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "w:tab" {
                    let val = p.attr("w:val");
                    if val != "clear" {
                        let align = match val {
                            "right" | "end" => TabAlign::Right,
                            "center" => TabAlign::Center,
                            _ => TabAlign::Left,
                        };
                        let leader = match p.attr("w:leader") {
                            "dot" | "middleDot" => TabLeader::Dot,
                            "hyphen" => TabLeader::Hyphen,
                            "underscore" => TabLeader::Underscore,
                            _ => TabLeader::None,
                        };
                        tabs.push(TabStop {
                            pos: parse_int(p.attr("w:pos")).max(0),
                            align,
                            leader,
                        });
                    }
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

fn parse_numpr(p: &mut XmlParser, props: &mut ParProps) {
    loop {
        match p.next() {
            Event::Start => {
                match p.name() {
                    "w:numId" => props.num_id = Some(parse_int(p.attr("w:val"))),
                    "w:ilvl" => props.ilvl = parse_int(p.attr("w:val")),
                    _ => {}
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

/// Parse a `w:r` run, pushing the resulting inline items into `out`. Returns
/// true if the run held significant content we don't model (a drawing, field,
/// embedded object, …) — in which case the caller preserves the whole run raw.
fn parse_run(p: &mut XmlParser, out: &mut Vec<Inline>) -> bool {
    let mut props = RunProps::default();
    let mut had_raw = false;
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:rPr" => parse_rpr(p, &mut props),
                "w:t" => {
                    let text = read_text(p);
                    out.push(Inline::Run(Run {
                        text,
                        props: props.clone(),
                    }));
                }
                "w:cr" => {
                    out.push(Inline::Break(BreakKind::Line));
                    p.skip_element();
                }
                "w:br" => {
                    let kind = match p.attr("w:type") {
                        "page" => BreakKind::Page,
                        "column" => BreakKind::Column,
                        _ => BreakKind::Line,
                    };
                    out.push(Inline::Break(kind));
                    p.skip_element();
                }
                "w:tab" => {
                    out.push(Inline::Tab(props.clone()));
                    p.skip_element();
                }
                "w:drawing"
                | "w:pict"
                | "w:object"
                | "w:fldChar"
                | "w:instrText"
                | "w:sym"
                | "w:commentReference"
                | "mc:AlternateContent" => {
                    had_raw = true;
                    p.skip_element();
                }
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    had_raw
}

fn parse_rpr(p: &mut XmlParser, props: &mut RunProps) {
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name();
                let val = p.attr("w:val");
                let start = p.start_pos();
                let mut modeled = true;
                match name {
                    "w:b" | "w:bCs" => props.bold = toggle_on(val),
                    "w:i" | "w:iCs" => props.italic = toggle_on(val),
                    "w:u" => props.underline = toggle_on(val),
                    "w:strike" | "w:dstrike" => props.strike = toggle_on(val),
                    "w:caps" => props.caps = toggle_on(val),
                    "w:smallCaps" => props.small_caps = toggle_on(val),
                    "w:vanish" | "w:webHidden" => props.vanish = toggle_on(val),
                    "w:vertAlign" => {
                        props.vert_align = match val {
                            "superscript" => VertAlign::Superscript,
                            "subscript" => VertAlign::Subscript,
                            _ => VertAlign::Baseline,
                        }
                    }
                    "w:color" => {
                        if !val.is_empty() && val != "auto" {
                            props.color = Some(val.to_ascii_uppercase());
                        }
                    }
                    "w:highlight" => {
                        if !val.is_empty() && val != "none" {
                            props.highlight = Some(val.to_string());
                        }
                    }
                    "w:sz" | "w:szCs" => {
                        let v = parse_int(val);
                        if v > 0 {
                            props.size_half_pts = Some(v as u32);
                        }
                    }
                    "w:rFonts" => {
                        let ascii = p.attr("w:ascii");
                        if !ascii.is_empty() {
                            props.font = Some(ascii.to_string());
                        }
                    }
                    "w:rStyle" if !val.is_empty() => {
                        props.style_id = Some(val.to_string());
                        // The "Code" character style is our inline-code marker.
                        if val.eq_ignore_ascii_case("Code") {
                            props.code = true;
                        }
                    }
                    // `w:rStyle` with an empty val, or anything else we don't
                    // model, is preserved verbatim (character spacing, kern,
                    // lang, shd, effect, …) so save doesn't drop it.
                    "w:rStyle" => {}
                    _ => modeled = false,
                }
                p.skip_element();
                if !modeled {
                    props
                        .raw_props
                        .push(p.raw_slice(start, p.pos()).to_string());
                }
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

/// Parse a `<w:hyperlink>`, pushing into `out`. An **external** link (resolvable
/// `r:id` target) is kept as a clickable [`Inline::Hyperlink`]; an internal
/// anchor (TOC entries, cross-references) is **unwrapped** so its inline content
/// — including tabs — renders normally (the link itself isn't actionable here).
fn parse_hyperlink_into(p: &mut XmlParser, rels: &Relationships, out: &mut Vec<Inline>) {
    let rid = decode_attr(p.attr("r:id"));
    let anchor_attr = decode_attr(p.attr("w:anchor"));
    let target = if rid.is_empty() {
        None
    } else {
        rels.target(&rid).map(|t| t.to_string())
    };
    if target.is_none() {
        parse_inlines_into(p, rels, out);
        return;
    }
    let rel_id = (!rid.is_empty()).then_some(rid);
    let anchor = (!anchor_attr.is_empty()).then_some(anchor_attr);

    let mut runs = Vec::new();
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:r" => {
                    let mut tmp = Vec::new();
                    parse_run(p, &mut tmp);
                    for it in tmp {
                        if let Inline::Run(r) = it {
                            runs.push(r);
                        }
                    }
                }
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    out.push(Inline::Hyperlink(Hyperlink {
        target,
        anchor,
        rel_id,
        runs,
    }));
}

fn parse_table(p: &mut XmlParser, rels: &Relationships) -> Table {
    let mut table = Table::default();
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:tblGrid" => parse_tblgrid(p, &mut table.grid),
                "w:tr" => table.rows.push(parse_row(p, rels)),
                // Whole table properties (borders/shading/width/style) preserved.
                "w:tblPr" => {
                    let start = p.start_pos();
                    p.skip_element();
                    table.raw_tblpr = Some(p.raw_slice(start, p.pos()).to_string());
                }
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    table
}

fn parse_tblgrid(p: &mut XmlParser, grid: &mut Vec<u32>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "w:gridCol" {
                    let w = parse_int(p.attr("w:w"));
                    grid.push(w.max(0) as u32);
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

fn parse_row(p: &mut XmlParser, rels: &Relationships) -> Row {
    let mut row = Row::default();
    parse_cells_into(p, rels, &mut row.cells, &mut row.raw_props);
    row
}

/// Collect the cells of a row (or a `<w:sdtContent>` within it), unwrapping
/// cell-level content controls (`<w:sdt>` wrapping a `<w:tc>`). Row-level
/// properties (`w:trPr`/`w:tblPrEx`) are captured verbatim into `raw`.
fn parse_cells_into(
    p: &mut XmlParser,
    rels: &Relationships,
    cells: &mut Vec<Cell>,
    raw: &mut Vec<String>,
) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:tc" => cells.push(parse_cell(p, rels)),
                "w:trPr" | "w:tblPrEx" => capture_element(p, raw),
                "w:sdt" => loop {
                    match p.next() {
                        Event::Start if p.name() == "w:sdtContent" => {
                            parse_cells_into(p, rels, cells, raw)
                        }
                        Event::Start => p.skip_element(),
                        Event::End | Event::Eof => break,
                        Event::Text => {}
                    }
                },
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

fn parse_cell(p: &mut XmlParser, rels: &Relationships) -> Cell {
    let mut cell = Cell::default();
    loop {
        match p.next() {
            Event::Start => match p.name() {
                // Parse gridSpan/vMerge for rendering. Keep the whole tcPr
                // verbatim (borders/shading/width/vAlign) ONLY when it carries
                // more than gridSpan/vMerge — so a cell the model can fully
                // describe still round-trips exactly (no spurious raw).
                "w:tcPr" => {
                    let start = p.start_pos();
                    let has_extra = parse_tcpr(p, &mut cell);
                    if has_extra {
                        cell.raw_tcpr = Some(p.raw_slice(start, p.pos()).to_string());
                    }
                }
                "w:p" => cell.blocks.push(Block::Paragraph(parse_paragraph(p, rels))),
                "w:tbl" => cell.blocks.push(Block::Table(parse_table(p, rels))),
                // Unwrap content controls nested in a cell (cover-page titles etc.).
                "w:sdt" => parse_sdt_block(p, rels, &mut cell.blocks),
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    // Every cell must hold at least one paragraph (OOXML requires it, and the
    // editor needs a paragraph to place the caret in an "empty" cell).
    if cell.blocks.is_empty() {
        cell.blocks.push(Block::Paragraph(Paragraph::default()));
    }
    cell
}

/// Parse `gridSpan`/`vMerge` into the model. Returns `true` if the `tcPr` holds
/// any other child (borders/shading/width/vAlign/…) — the signal to preserve the
/// whole `tcPr` verbatim for lossless save.
fn parse_tcpr(p: &mut XmlParser, cell: &mut Cell) -> bool {
    let mut has_extra = false;
    loop {
        match p.next() {
            Event::Start => {
                match p.name() {
                    "w:gridSpan" => {
                        let v = parse_int(p.attr("w:val"));
                        if v > 0 {
                            cell.grid_span = v as u32;
                        }
                    }
                    "w:vMerge" => {
                        cell.v_merge = if p.attr("w:val") == "restart" {
                            VMerge::Restart
                        } else {
                            VMerge::Continue
                        };
                    }
                    _ => has_extra = true,
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    has_extra
}

/// Reads character data up to the matching End (used for `w:t`).
fn read_text(p: &mut XmlParser) -> String {
    let mut s = String::new();
    loop {
        match p.next() {
            Event::Text => XmlParser::append_decoded(p.text(), &mut s),
            Event::Start => p.skip_element(),
            Event::End | Event::Eof => break,
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(xml: &str) -> Document {
        parse_document_xml(xml, &Relationships::default())
    }

    #[test]
    fn extracts_diagram_node_text() {
        // Node text from a diagram data part, with blank placeholder nodes dropped
        // and multi-run nodes kept as separate entries (matching `<a:t>` order).
        let xml = "<dgm:dataModel><dgm:ptLst>\
            <dgm:pt><dgm:t><a:p><a:r><a:t>Build</a:t></a:r></a:p></dgm:t></dgm:pt>\
            <dgm:pt><dgm:t><a:p><a:r><a:t>  </a:t></a:r></a:p></dgm:t></dgm:pt>\
            <dgm:pt><dgm:t><a:p><a:r><a:t>Ship</a:t></a:r></a:p></dgm:t></dgm:pt>\
            </dgm:ptLst></dgm:dataModel>";
        assert_eq!(
            extract_diagram_text(xml),
            vec!["Build".to_string(), "Ship".to_string()]
        );
    }

    #[test]
    fn resolves_diagram_part_name() {
        assert_eq!(
            resolve_word_part("diagrams/data1.xml"),
            "word/diagrams/data1.xml"
        );
        assert_eq!(
            resolve_word_part("../customXml/item1.xml"),
            "customXml/item1.xml"
        );
    }

    #[test]
    fn diagram_run_becomes_smartart() {
        let rels_xml = "<Relationships><Relationship Id=\"rId5\" \
            Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData\" \
            Target=\"diagrams/data1.xml\"/></Relationships>";
        let data = b"<a:t>Hello</a:t>".to_vec();
        let diagrams = collect_diagram_texts(rels_xml, |n| {
            (n == "word/diagrams/data1.xml").then(|| data.clone())
        });
        assert_eq!(diagrams.get("rId5"), Some(&vec!["Hello".to_string()]));

        let mut rels = parse_rels_xml(rels_xml);
        set_diagram_texts(&mut rels, diagrams);
        let xml = "<w:document><w:body><w:p><w:r><w:drawing>\
            <a:graphicData uri=\"x/diagram\"><dgm:relIds r:dm=\"rId5\"/></a:graphicData>\
            </w:drawing></w:r></w:p></w:body></w:document>";
        let d = parse_document_xml(xml, &rels);
        match &first_para(&d).content[0] {
            Inline::SmartArt { text, .. } => assert_eq!(text, &vec!["Hello".to_string()]),
            other => panic!("expected SmartArt, got {other:?}"),
        }
    }

    #[test]
    fn equation_object_run_becomes_inline_text() {
        let mut rels = Relationships::default();
        let mut eqs = HashMap::new();
        eqs.insert("rId7".to_string(), "x²+1".to_string());
        set_equation_texts(&mut rels, eqs);
        // A run holding the OLE equation object plus its image preview.
        let xml = "<w:document><w:body><w:p><w:r><w:object>\
            <o:OLEObject Type=\"Embed\" ProgID=\"Equation.3\" r:id=\"rId7\"/>\
            <v:shape><v:imagedata r:id=\"rId8\"/></v:shape>\
            </w:object></w:r></w:p></w:body></w:document>";
        let d = parse_document_xml(xml, &rels);
        match &first_para(&d).content[0] {
            Inline::Equation { text, .. } => assert_eq!(text, "x²+1"),
            other => panic!("expected Equation, got {other:?}"),
        }
    }

    fn first_para(d: &Document) -> &Paragraph {
        match &d.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn fld_simple_surfaces_result_and_stays_lossless() {
        let xml = "<w:document><w:body><w:p>\
                   <w:fldSimple w:instr=\" CREATEDATE \\@ &quot;M/d/yyyy&quot; \">\
                   <w:r><w:t>11/5/2007</w:t></w:r></w:fldSimple>\
                   </w:p></w:body></w:document>";
        let d = doc(xml);
        match &first_para(&d).content[0] {
            Inline::Field { text, raw } => {
                assert_eq!(text, "11/5/2007");
                assert!(raw.contains("CREATEDATE") && raw.contains("</w:fldSimple>"));
            }
            other => panic!("expected Field, got {other:?}"),
        }
        // the field's value is visible text
        assert_eq!(first_para(&d).plain_text(), "11/5/2007");
        // and it serializes back verbatim
        assert!(crate::serialize::document_to_xml(&d).contains("<w:fldSimple w:instr="));
    }

    #[test]
    fn formula_field_is_recomputed_from_cache() {
        // The cached result is a stale "0"; docxy recomputes the formula to 14.
        let xml = "<w:document><w:body><w:p>\
                   <w:fldSimple w:instr=\" = 2*(3+4) \"><w:r><w:t>0</w:t></w:r></w:fldSimple>\
                   </w:p></w:body></w:document>";
        let d = doc(xml);
        assert_eq!(first_para(&d).plain_text(), "14");
    }

    #[test]
    fn non_formula_simple_field_keeps_cached_value() {
        // A DATE field isn't recomputed (no clock context); the cache stands.
        let xml = "<w:document><w:body><w:p>\
                   <w:fldSimple w:instr=\" DATE \\@ &quot;M/d/yyyy&quot; \">\
                   <w:r><w:t>2/15/2008</w:t></w:r></w:fldSimple>\
                   </w:p></w:body></w:document>";
        let d = doc(xml);
        assert_eq!(first_para(&d).plain_text(), "2/15/2008");
    }

    #[test]
    fn smart_tag_runs_are_unwrapped() {
        // <w:smartTag> wrappers (deprecated MS metadata) must not hide their runs.
        let xml = "<w:document><w:body><w:p>\
                   <w:r><w:t xml:space=\"preserve\">The </w:t></w:r>\
                   <w:smartTag w:element=\"place\">\
                   <w:smartTag w:element=\"PlaceType\"><w:r><w:t>University</w:t></w:r></w:smartTag>\
                   <w:r><w:t xml:space=\"preserve\"> of </w:t></w:r>\
                   <w:smartTag w:element=\"PlaceName\"><w:r><w:t>Texas</w:t></w:r></w:smartTag>\
                   </w:smartTag>\
                   <w:r><w:t xml:space=\"preserve\"> System</w:t></w:r>\
                   </w:p></w:body></w:document>";
        let d = doc(xml);
        assert_eq!(
            first_para(&d).plain_text(),
            "The University of Texas System"
        );
    }

    #[test]
    fn comment_markers_round_trip() {
        // range start/end and the reference run must all survive load → save.
        let xml = "<w:document><w:body><w:p>\
                   <w:commentRangeStart w:id=\"1\"/><w:r><w:t>hi</w:t></w:r>\
                   <w:commentRangeEnd w:id=\"1\"/>\
                   <w:r><w:commentReference w:id=\"1\"/></w:r>\
                   </w:p></w:body></w:document>";
        let d = doc(xml);
        let out = crate::serialize::document_to_xml(&d);
        assert!(out.contains("commentRangeStart w:id=\"1\""));
        assert!(out.contains("commentReference w:id=\"1\""));
        assert!(out.contains("commentRangeEnd w:id=\"1\""));
        assert_eq!(first_para(&d).plain_text(), "hi");
    }

    #[test]
    fn complex_field_hides_code_and_shows_result() {
        // A fldChar field: begin / instruction / separate / result / end. The
        // instruction (PAGE) is hidden; only the cached result (7) renders.
        let xml = "<w:document><w:body><w:p>\
                   <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>\
                   <w:r><w:instrText> PAGE </w:instrText></w:r>\
                   <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>\
                   <w:r><w:t>7</w:t></w:r>\
                   <w:r><w:fldChar w:fldCharType=\"end\"/></w:r>\
                   </w:p></w:body></w:document>";
        let d = doc(xml);
        assert_eq!(first_para(&d).plain_text(), "7");
        // the field markers survive verbatim for a lossless save
        let out = crate::serialize::document_to_xml(&d);
        assert!(out.contains("PAGE") && out.contains("fldCharType=\"begin\""));
    }

    #[test]
    fn plain_paragraph_text() {
        let d = doc(
            "<w:document><w:body><w:p><w:r><w:t>Hello world</w:t></w:r></w:p></w:body></w:document>",
        );
        assert_eq!(d.body.len(), 1);
        assert_eq!(first_para(&d).plain_text(), "Hello world");
    }

    #[test]
    fn run_formatting_toggles() {
        let xml = "<w:document><w:body><w:p><w:r><w:rPr><w:b/><w:i/><w:strike/></w:rPr>\
                   <w:t>x</w:t></w:r></w:p></w:body></w:document>";
        let d = doc(xml);
        let para = first_para(&d);
        match &para.content[0] {
            Inline::Run(r) => {
                assert!(r.props.bold && r.props.italic && r.props.strike);
                assert!(!r.props.underline);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn explicit_off_toggle_disables() {
        let xml = "<w:document><w:body><w:p><w:r><w:rPr><w:b w:val=\"false\"/></w:rPr>\
                   <w:t>x</w:t></w:r></w:p></w:body></w:document>";
        let d = doc(xml);
        if let Inline::Run(r) = &first_para(&d).content[0] {
            assert!(!r.props.bold);
        } else {
            panic!();
        }
    }

    #[test]
    fn color_size_font_and_vertalign() {
        let xml = "<w:document><w:body><w:p><w:r><w:rPr>\
                   <w:color w:val=\"ff0000\"/><w:sz w:val=\"28\"/>\
                   <w:rFonts w:ascii=\"Calibri\"/><w:vertAlign w:val=\"superscript\"/>\
                   </w:rPr><w:t>x</w:t></w:r></w:p></w:body></w:document>";
        let d = doc(xml);
        if let Inline::Run(r) = &first_para(&d).content[0] {
            assert_eq!(r.props.color.as_deref(), Some("FF0000"));
            assert_eq!(r.props.size_half_pts, Some(28));
            assert_eq!(r.props.font.as_deref(), Some("Calibri"));
            assert_eq!(r.props.vert_align, VertAlign::Superscript);
        } else {
            panic!();
        }
    }

    #[test]
    fn color_auto_is_ignored() {
        let xml = "<w:document><w:body><w:p><w:r><w:rPr><w:color w:val=\"auto\"/></w:rPr>\
                   <w:t>x</w:t></w:r></w:p></w:body></w:document>";
        let d = doc(xml);
        if let Inline::Run(r) = &first_para(&d).content[0] {
            assert_eq!(r.props.color, None);
        } else {
            panic!();
        }
    }

    #[test]
    fn breaks_and_tabs_in_run() {
        let xml = "<w:document><w:body><w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t><w:br/></w:r></w:p></w:body></w:document>";
        let d = doc(xml);
        let c = &first_para(&d).content;
        assert!(matches!(c[0], Inline::Run(_)));
        assert!(matches!(c[1], Inline::Tab(_)));
        assert!(matches!(c[2], Inline::Run(_)));
        assert!(matches!(c[3], Inline::Break(BreakKind::Line)));
    }

    #[test]
    fn alignment_and_numbering() {
        let xml = "<w:document><w:body><w:p><w:pPr><w:jc w:val=\"center\"/>\
                   <w:numPr><w:ilvl w:val=\"2\"/><w:numId w:val=\"5\"/></w:numPr></w:pPr>\
                   <w:r><w:t>item</w:t></w:r></w:p></w:body></w:document>";
        let d = doc(xml);
        let p = first_para(&d);
        assert_eq!(p.props.align, Align::Center);
        assert_eq!(p.props.num_id, Some(5));
        assert_eq!(p.props.ilvl, 2);
    }

    #[test]
    fn heading_detected_from_style() {
        let xml = "<w:document><w:body><w:p><w:pPr><w:pStyle w:val=\"Heading2\"/></w:pPr>\
                   <w:r><w:t>Title</w:t></w:r></w:p></w:body></w:document>";
        let d = doc(xml);
        assert_eq!(first_para(&d).props.heading_level, Some(2));
        assert_eq!(first_para(&d).props.style_id.as_deref(), Some("Heading2"));
    }

    #[test]
    fn hyperlink_resolves_relationship() {
        let rels = parse_rels_xml(
            "<Relationships><Relationship Id=\"rId7\" Target=\"https://example.com/\" \
             TargetMode=\"External\"/></Relationships>",
        );
        assert_eq!(rels.len(), 1);
        let xml = "<w:document><w:body><w:p><w:hyperlink r:id=\"rId7\">\
                   <w:r><w:t>click</w:t></w:r></w:hyperlink></w:p></w:body></w:document>";
        let d = parse_document_xml(xml, &rels);
        if let Inline::Hyperlink(h) = &first_para(&d).content[0] {
            assert_eq!(h.target.as_deref(), Some("https://example.com/"));
            assert_eq!(h.runs.len(), 1);
            assert_eq!(h.runs[0].text, "click");
        } else {
            panic!("expected hyperlink");
        }
    }

    #[test]
    fn table_with_grid_span_and_vmerge() {
        let xml = "<w:document><w:body><w:tbl>\
                   <w:tblGrid><w:gridCol w:w=\"100\"/><w:gridCol w:w=\"200\"/></w:tblGrid>\
                   <w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr><w:p><w:r><w:t>wide</w:t></w:r></w:p></w:tc></w:tr>\
                   <w:tr><w:tc><w:tcPr><w:vMerge w:val=\"restart\"/></w:tcPr><w:p><w:r><w:t>top</w:t></w:r></w:p></w:tc>\
                   <w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>\
                   </w:tbl></w:body></w:document>";
        let d = doc(xml);
        let t = match &d.body[0] {
            Block::Table(t) => t,
            _ => panic!("expected table"),
        };
        assert_eq!(t.grid, vec![100, 200]);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells[0].grid_span, 2);
        assert_eq!(t.rows[0].cells[0].blocks.len(), 1);
        assert_eq!(t.rows[1].cells[0].v_merge, VMerge::Restart);
        assert_eq!(t.rows[1].cells[1].v_merge, VMerge::None);
    }

    #[test]
    fn break_types_are_captured() {
        let xml = "<w:document><w:body><w:p><w:r>\
                   <w:t>a</w:t><w:br w:type=\"page\"/><w:t>b</w:t><w:br/><w:cr/><w:br w:type=\"column\"/>\
                   </w:r></w:p></w:body></w:document>";
        let d = doc(xml);
        let c = &first_para(&d).content;
        assert!(matches!(c[1], Inline::Break(BreakKind::Page)));
        assert!(matches!(c[3], Inline::Break(BreakKind::Line))); // plain w:br
        assert!(matches!(c[4], Inline::Break(BreakKind::Line))); // w:cr
        assert!(matches!(c[5], Inline::Break(BreakKind::Column)));
    }

    #[test]
    fn empty_cell_gets_a_paragraph() {
        // A <w:tc> with no <w:p> should still yield a navigable (empty) paragraph.
        let xml = "<w:document><w:body><w:tbl>\
                   <w:tr><w:tc><w:tcPr/></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>\
                   </w:tbl></w:body></w:document>";
        let d = doc(xml);
        let t = match &d.body[0] {
            Block::Table(t) => t,
            _ => panic!("expected table"),
        };
        assert_eq!(t.rows[0].cells[0].blocks.len(), 1);
        assert!(matches!(t.rows[0].cells[0].blocks[0], Block::Paragraph(_)));
    }

    #[test]
    fn inline_sdt_is_unwrapped() {
        // A content control wrapping runs inside a paragraph must show its text.
        let xml = "<w:document><w:body><w:p>\
                   <w:r><w:t xml:space=\"preserve\">a </w:t></w:r>\
                   <w:sdt><w:sdtPr/><w:sdtContent><w:r><w:t>inner</w:t></w:r></w:sdtContent></w:sdt>\
                   <w:r><w:t xml:space=\"preserve\"> b</w:t></w:r></w:p></w:body></w:document>";
        assert_eq!(first_para(&doc(xml)).plain_text(), "a inner b");
    }

    #[test]
    fn textbox_blocks_parsed() {
        let raw = "<w:pict><v:shape><v:textbox><w:txbxContent>\
                   <w:p><w:r><w:t>boxed</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict>";
        let blocks = parse_textbox_blocks(raw);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].plain_text(), "boxed");
    }

    #[test]
    fn unknown_elements_are_skipped() {
        let xml = "<w:document><w:body><w:p><w:bookmarkStart w:id=\"0\" w:name=\"x\"/>\
                   <w:proofErr w:type=\"spellStart\"/><w:r><w:t>ok</w:t></w:r>\
                   <w:bookmarkEnd w:id=\"0\"/></w:p></w:body></w:document>";
        let d = doc(xml);
        assert_eq!(first_para(&d).plain_text(), "ok");
    }

    #[test]
    fn load_rejects_non_zip() {
        assert_eq!(load(b"not a zip").unwrap_err(), LoadError::NotZip);
    }

    #[test]
    fn load_detects_ole2() {
        let ole = [0xD0u8, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0x00, 0x00];
        assert_eq!(load(&ole).unwrap_err(), LoadError::Ole2);
    }
}
