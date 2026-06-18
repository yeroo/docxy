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

/// `rId` -> (target, is_external) from a `.rels` part.
#[derive(Debug, Default, Clone)]
pub struct Relationships {
    map: HashMap<String, (String, bool)>,
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
        Some(b) => parse_rels_xml(std::str::from_utf8(&b).unwrap_or("")),
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

fn heading_level(style_id: &str) -> Option<u8> {
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
                        // Run held a drawing/field/etc. — preserve it verbatim.
                        para.content
                            .push(Inline::Raw(p.raw_slice(start, p.pos()).to_string()));
                    } else {
                        para.content.extend(tmp);
                    }
                }
                "w:hyperlink" => {
                    let h = parse_hyperlink(p, rels);
                    para.content.push(Inline::Hyperlink(h));
                }
                _ => {
                    // Unmodeled inline content (bookmarks, fields, sdt): preserve raw.
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
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

fn frame_int(p: &XmlParser, name: &str) -> Option<i32> {
    let v = p.attr(name);
    (!v.is_empty()).then(|| parse_int(v))
}

fn frame_str(p: &XmlParser, name: &str) -> Option<String> {
    let v = p.attr(name);
    (!v.is_empty()).then(|| decode_attr(v))
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
                    out.push(Inline::Tab);
                    p.skip_element();
                }
                "w:drawing" | "w:pict" | "w:object" | "w:fldChar" | "w:instrText" | "w:sym" => {
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
                    }
                    _ => {}
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

fn parse_hyperlink(p: &mut XmlParser, rels: &Relationships) -> Hyperlink {
    let rid = decode_attr(p.attr("r:id"));
    let anchor_attr = decode_attr(p.attr("w:anchor"));
    let target = if rid.is_empty() {
        None
    } else {
        rels.target(&rid).map(|t| t.to_string())
    };
    let rel_id = if rid.is_empty() { None } else { Some(rid) };
    let anchor = if anchor_attr.is_empty() {
        None
    } else {
        Some(anchor_attr)
    };

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
    Hyperlink {
        target,
        anchor,
        rel_id,
        runs,
    }
}

fn parse_table(p: &mut XmlParser, rels: &Relationships) -> Table {
    let mut table = Table::default();
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:tblGrid" => parse_tblgrid(p, &mut table.grid),
                "w:tr" => table.rows.push(parse_row(p, rels)),
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
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:tc" => row.cells.push(parse_cell(p, rels)),
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    row
}

fn parse_cell(p: &mut XmlParser, rels: &Relationships) -> Cell {
    let mut cell = Cell::default();
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:tcPr" => parse_tcpr(p, &mut cell),
                "w:p" => cell.blocks.push(Block::Paragraph(parse_paragraph(p, rels))),
                "w:tbl" => cell.blocks.push(Block::Table(parse_table(p, rels))),
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

fn parse_tcpr(p: &mut XmlParser, cell: &mut Cell) {
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
                    _ => {}
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
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

    fn first_para(d: &Document) -> &Paragraph {
        match &d.body[0] {
            Block::Paragraph(p) => p,
            _ => panic!("expected paragraph"),
        }
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
        assert!(matches!(c[1], Inline::Tab));
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
