//! Serialize the [`crate::model`] document tree back to `word/document.xml`.
//!
//! This is a *semantic* serializer: it re-emits the structure and properties we
//! model (paragraphs, runs + rPr, tables, lists, hyperlinks). It is designed so
//! that `parse_document_xml(document_to_xml(&doc)) == doc` for everything we
//! model — see the round-trip tests. Body content we do not model (e.g.
//! `sectPr`, bookmarks) is preserved separately by the package layer, not here.

use crate::model::*;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

/// Serialize a document to the bytes of `word/document.xml`.
pub fn document_to_xml(doc: &Document) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    // The `m:` namespace is declared so equations (`<m:oMath>`) authored from
    // Markdown serialize as valid Office Math.
    s.push_str(&format!(
        "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:m=\"{M_NS}\"><w:body>"
    ));
    for block in &doc.body {
        write_block(&mut s, block);
    }
    s.push_str("</w:body></w:document>");
    s
}

/// Serialize just the block content (no document wrapper), for splicing back into
/// a preserved header/footer part (`<w:hdr>…</w:hdr>` / `<w:ftr>…</w:ftr>`).
pub fn blocks_to_xml(blocks: &[Block]) -> String {
    let mut s = String::new();
    for block in blocks {
        write_block(&mut s, block);
    }
    s
}

fn esc_text(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn esc_attr(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

fn write_block(s: &mut String, block: &Block) {
    match block {
        Block::Paragraph(p) => write_paragraph(s, p),
        Block::Table(t) => write_table(s, t),
        Block::Raw(raw) => s.push_str(raw),
    }
}

fn write_paragraph(s: &mut String, p: &Paragraph) {
    s.push_str("<w:p>");
    write_ppr(s, &p.props);
    for item in &p.content {
        write_inline(s, item);
    }
    s.push_str("</w:p>");
}

fn write_ppr(s: &mut String, props: &ParProps) {
    // Effective paragraph style: explicit style, else a synthesized heading style.
    let style = props
        .style_id
        .clone()
        .or_else(|| props.heading_level.map(|l| format!("Heading{l}")));
    let has_any = style.is_some()
        || props.num_id.is_some()
        || props.align != Align::Left
        || props.rtl
        || props.frame.is_some()
        || props.section_break.is_some()
        || props.borders.top.is_some()
        || props.borders.bottom.is_some()
        || props.indent != 0
        || !props.tabs.is_empty()
        || !props.raw_props.is_empty();
    if !has_any {
        return;
    }
    s.push_str("<w:pPr>");
    if let Some(st) = &style {
        s.push_str("<w:pStyle w:val=\"");
        esc_attr(st, s);
        s.push_str("\"/>");
    }
    // framePr must precede numPr/jc in the schema order.
    if let Some(f) = &props.frame {
        s.push_str("<w:framePr");
        if let Some(v) = f.w {
            s.push_str(&format!(" w:w=\"{v}\""));
        }
        if let Some(v) = f.h {
            s.push_str(&format!(" w:h=\"{v}\""));
        }
        if let Some(a) = &f.h_anchor {
            s.push_str(" w:hAnchor=\"");
            esc_attr(a, s);
            s.push('"');
        }
        if let Some(a) = &f.v_anchor {
            s.push_str(" w:vAnchor=\"");
            esc_attr(a, s);
            s.push('"');
        }
        if let Some(a) = &f.x_align {
            s.push_str(" w:xAlign=\"");
            esc_attr(a, s);
            s.push('"');
        }
        if let Some(a) = &f.y_align {
            s.push_str(" w:yAlign=\"");
            esc_attr(a, s);
            s.push('"');
        }
        if let Some(v) = f.x {
            s.push_str(&format!(" w:x=\"{v}\""));
        }
        if let Some(v) = f.y {
            s.push_str(&format!(" w:y=\"{v}\""));
        }
        s.push_str("/>");
    }
    if let Some(num) = props.num_id {
        s.push_str(&format!(
            "<w:numPr><w:ilvl w:val=\"{}\"/><w:numId w:val=\"{}\"/></w:numPr>",
            props.ilvl, num
        ));
    }
    // pBdr precedes tabs in the schema order.
    if props.borders.top.is_some() || props.borders.bottom.is_some() {
        s.push_str("<w:pBdr>");
        for (tag, side) in [
            ("w:top", props.borders.top),
            ("w:bottom", props.borders.bottom),
        ] {
            if let Some(k) = side {
                s.push_str(&format!(
                    "<{tag} w:val=\"{}\" w:sz=\"6\" w:space=\"1\" w:color=\"auto\"/>",
                    k.to_val()
                ));
            }
        }
        s.push_str("</w:pBdr>");
    }
    if !props.tabs.is_empty() {
        s.push_str("<w:tabs>");
        for t in &props.tabs {
            let val = match t.align {
                TabAlign::Center => "center",
                TabAlign::Right => "right",
                TabAlign::Left => "left",
            };
            s.push_str(&format!("<w:tab w:val=\"{val}\""));
            let leader = match t.leader {
                TabLeader::Dot => Some("dot"),
                TabLeader::Hyphen => Some("hyphen"),
                TabLeader::Underscore => Some("underscore"),
                TabLeader::None => None,
            };
            if let Some(l) = leader {
                s.push_str(&format!(" w:leader=\"{l}\""));
            }
            s.push_str(&format!(" w:pos=\"{}\"/>", t.pos));
        }
        s.push_str("</w:tabs>");
    }
    // w:ind precedes w:jc in the schema order.
    if props.indent != 0 || props.first_line != 0 {
        s.push_str("<w:ind");
        if props.indent != 0 {
            s.push_str(&format!(" w:left=\"{}\"", props.indent));
        }
        // firstLine and hanging are mutually exclusive; both are non-negative.
        match props.first_line.cmp(&0) {
            std::cmp::Ordering::Greater => {
                s.push_str(&format!(" w:firstLine=\"{}\"", props.first_line))
            }
            std::cmp::Ordering::Less => {
                s.push_str(&format!(" w:hanging=\"{}\"", -props.first_line))
            }
            std::cmp::Ordering::Equal => {}
        }
        s.push_str("/>");
    }
    match props.align {
        Align::Left => {}
        Align::Center => s.push_str("<w:jc w:val=\"center\"/>"),
        Align::Right => s.push_str("<w:jc w:val=\"right\"/>"),
        Align::Justify => s.push_str("<w:jc w:val=\"both\"/>"),
    }
    if props.rtl {
        s.push_str("<w:bidi/>");
    }
    // Preserved unmodeled pPr children (the paragraph-mark `w:rPr`, `outlineLvl`,
    // shading, spacing, …). Emitted here, near the end of pPr — where the most
    // structurally-sensitive of them, the paragraph-mark `w:rPr`, belongs (just
    // before sectPr) so Word accepts the ordering.
    for raw in &props.raw_props {
        s.push_str(raw);
    }
    // A section break (`<w:sectPr>`) is the last pPr child.
    if let Some(sect) = &props.section_break {
        s.push_str(sect);
    }
    s.push_str("</w:pPr>");
}

fn write_inline(s: &mut String, item: &Inline) {
    match item {
        Inline::Run(r) => write_run(s, r),
        Inline::Tab(props) => {
            s.push_str("<w:r>");
            write_rpr(s, props);
            s.push_str("<w:tab/></w:r>");
        }
        Inline::Break(kind) => match kind {
            BreakKind::Line => s.push_str("<w:r><w:br/></w:r>"),
            BreakKind::Page => s.push_str("<w:r><w:br w:type=\"page\"/></w:r>"),
            BreakKind::Column => s.push_str("<w:r><w:br w:type=\"column\"/></w:r>"),
        },
        Inline::Hyperlink(h) => {
            s.push_str("<w:hyperlink");
            if let Some(id) = &h.rel_id {
                s.push_str(" r:id=\"");
                esc_attr(id, s);
                s.push('"');
            }
            if let Some(a) = &h.anchor {
                s.push_str(" w:anchor=\"");
                esc_attr(a, s);
                s.push('"');
            }
            s.push('>');
            for r in &h.runs {
                write_run(s, r);
            }
            s.push_str("</w:hyperlink>");
        }
        Inline::SmartArt { raw, .. } => s.push_str(raw),
        Inline::Chart { raw, .. } => s.push_str(raw),
        Inline::Equation { raw, .. } => s.push_str(raw),
        Inline::Field { raw, .. } => s.push_str(raw),
        // Tracked change: re-emit the original <w:ins>/<w:del> verbatim (the
        // display `content` is not serialized).
        Inline::Revision { raw, .. } => s.push_str(raw),
        // Footnote/endnote reference: re-emit the original reference run verbatim.
        Inline::FootnoteRef { raw, .. } => s.push_str(raw),
        Inline::TextBox { raw, blocks } => {
            // Splice the (possibly edited) content back into the shape's
            // `txbxContent`, preserving the surrounding VML/drawing markup.
            const OPEN: &str = "<w:txbxContent>";
            match (raw.find(OPEN), raw.find("</w:txbxContent>")) {
                (Some(a), Some(b)) if a + OPEN.len() <= b => {
                    s.push_str(&raw[..a + OPEN.len()]);
                    s.push_str(&blocks_to_xml(blocks));
                    s.push_str(&raw[b..]);
                }
                _ => s.push_str(raw),
            }
        }
        Inline::Raw(raw) => s.push_str(raw),
    }
}

fn write_run(s: &mut String, r: &Run) {
    s.push_str("<w:r>");
    write_rpr(s, &r.props);
    s.push_str("<w:t xml:space=\"preserve\">");
    esc_text(&r.text, s);
    s.push_str("</w:t></w:r>");
}

fn write_rpr(s: &mut String, p: &RunProps) {
    let has_any = p.bold
        || p.italic
        || p.underline
        || p.strike
        || p.code
        || p.caps
        || p.small_caps
        || p.vanish
        || p.vert_align != VertAlign::Baseline
        || p.color.is_some()
        || p.highlight.is_some()
        || p.size_half_pts.is_some()
        || p.font.is_some()
        || p.style_id.is_some()
        || !p.raw_props.is_empty();
    if !has_any {
        return;
    }
    s.push_str("<w:rPr>");
    // Inline code carries the "Code" character style unless a more specific
    // character style is already set (which then implies the code styling).
    let rstyle = p
        .style_id
        .as_deref()
        .or(if p.code { Some("Code") } else { None });
    if let Some(st) = rstyle {
        s.push_str("<w:rStyle w:val=\"");
        esc_attr(st, s);
        s.push_str("\"/>");
    }
    if let Some(f) = &p.font {
        s.push_str("<w:rFonts w:ascii=\"");
        esc_attr(f, s);
        s.push_str("\"/>");
    }
    if p.bold {
        s.push_str("<w:b/>");
    }
    if p.italic {
        s.push_str("<w:i/>");
    }
    if p.caps {
        s.push_str("<w:caps/>");
    }
    if p.small_caps {
        s.push_str("<w:smallCaps/>");
    }
    if p.strike {
        s.push_str("<w:strike/>");
    }
    if p.vanish {
        s.push_str("<w:vanish/>");
    }
    if let Some(c) = &p.color {
        s.push_str("<w:color w:val=\"");
        esc_attr(c, s);
        s.push_str("\"/>");
    }
    if let Some(sz) = p.size_half_pts {
        s.push_str(&format!("<w:sz w:val=\"{sz}\"/>"));
    }
    if let Some(h) = &p.highlight {
        s.push_str("<w:highlight w:val=\"");
        esc_attr(h, s);
        s.push_str("\"/>");
    }
    if p.underline {
        s.push_str("<w:u w:val=\"single\"/>");
    }
    match p.vert_align {
        VertAlign::Baseline => {}
        VertAlign::Superscript => s.push_str("<w:vertAlign w:val=\"superscript\"/>"),
        VertAlign::Subscript => s.push_str("<w:vertAlign w:val=\"subscript\"/>"),
    }
    // Preserved unmodeled rPr children (character spacing, kern, lang, shd, …).
    for raw in &p.raw_props {
        s.push_str(raw);
    }
    s.push_str("</w:rPr>");
}

fn write_table(s: &mut String, t: &Table) {
    s.push_str("<w:tbl>");
    // tblPr is the first tbl child; preserved verbatim when present.
    if let Some(raw) = &t.raw_tblpr {
        s.push_str(raw);
    }
    if !t.grid.is_empty() {
        s.push_str("<w:tblGrid>");
        for w in &t.grid {
            s.push_str(&format!("<w:gridCol w:w=\"{w}\"/>"));
        }
        s.push_str("</w:tblGrid>");
    }
    for row in &t.rows {
        s.push_str("<w:tr>");
        // trPr / tblPrEx precede the cells; preserved verbatim.
        for raw in &row.raw_props {
            s.push_str(raw);
        }
        for cell in &row.cells {
            write_cell(s, cell);
        }
        s.push_str("</w:tr>");
    }
    s.push_str("</w:tbl>");
}

fn write_cell(s: &mut String, cell: &Cell) {
    s.push_str("<w:tc>");
    if let Some(raw) = &cell.raw_tcpr {
        // The original tcPr (already carries gridSpan/vMerge) — re-emit as-is so
        // borders/shading/width/vAlign survive.
        s.push_str(raw);
    } else if cell.grid_span > 1 || cell.v_merge != VMerge::None {
        // A cell created in-editor: synthesize tcPr from the model.
        s.push_str("<w:tcPr>");
        if cell.grid_span > 1 {
            s.push_str(&format!("<w:gridSpan w:val=\"{}\"/>", cell.grid_span));
        }
        match cell.v_merge {
            VMerge::None => {}
            VMerge::Restart => s.push_str("<w:vMerge w:val=\"restart\"/>"),
            VMerge::Continue => s.push_str("<w:vMerge/>"),
        }
        s.push_str("</w:tcPr>");
    }
    if cell.blocks.is_empty() {
        // A table cell must contain at least one block to be valid OOXML.
        s.push_str("<w:p></w:p>");
    } else {
        for b in &cell.blocks {
            write_block(s, b);
        }
    }
    s.push_str("</w:tc>");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::{Relationships, parse_document_xml, parse_rels_xml};

    fn roundtrip(doc: &Document, rels: &Relationships) -> Document {
        let xml = document_to_xml(doc);
        parse_document_xml(&xml, rels)
    }

    fn run(text: &str, props: RunProps) -> Inline {
        Inline::Run(Run {
            text: text.to_string(),
            props,
        })
    }
    fn para(props: ParProps, content: Vec<Inline>) -> Block {
        Block::Paragraph(Paragraph { props, content })
    }

    #[test]
    fn plain_paragraph_roundtrips() {
        let d = Document {
            body: vec![para(
                ParProps::default(),
                vec![run("Hello world", RunProps::default())],
            )],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn preserves_unmodeled_para_table_and_cell_props() {
        // A paragraph carrying shading + spacing (both unmodeled), and a table
        // whose tblPr / trPr / tcPr carry borders + shading — none of which the
        // model represents — must all survive a save round-trip instead of being
        // silently dropped (docxy gap D-1).
        let ppr = ParProps {
            raw_props: vec![
                "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"FFFF00\"/>".to_string(),
                "<w:spacing w:before=\"120\" w:after=\"120\"/>".to_string(),
            ],
            ..Default::default()
        };
        let cell = Cell {
            blocks: vec![para(ParProps::default(), vec![])],
            raw_tcpr: Some(
                "<w:tcPr><w:tcBorders><w:top w:val=\"single\" w:sz=\"4\"/></w:tcBorders>\
                 <w:shd w:val=\"clear\" w:fill=\"D9D9D9\"/></w:tcPr>"
                    .to_string(),
            ),
            ..Default::default()
        };
        let table = Table {
            grid: vec![100],
            rows: vec![Row {
                cells: vec![cell],
                raw_props: vec!["<w:trPr><w:trHeight w:val=\"300\"/></w:trPr>".to_string()],
            }],
            raw_tblpr: Some(
                "<w:tblPr><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\"/></w:tblBorders></w:tblPr>"
                    .to_string(),
            ),
        };
        let d = Document {
            body: vec![para(ppr, vec![]), Block::Table(table)],
        };
        let xml = document_to_xml(&d);
        assert!(
            xml.contains("w:fill=\"FFFF00\""),
            "paragraph shading dropped"
        );
        assert!(
            xml.contains("<w:spacing w:before=\"120\""),
            "paragraph spacing dropped"
        );
        assert!(xml.contains("<w:tblBorders>"), "table borders dropped");
        assert!(xml.contains("w:fill=\"D9D9D9\""), "cell shading dropped");
        assert!(xml.contains("<w:trPr>"), "row properties dropped");
        // And the whole thing round-trips to an identical model.
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn tracked_changes_visible_and_lossless() {
        // <w:ins>/<w:del> used to vanish into opaque Raw (invisible). Now the
        // inserted/deleted text is visible and the revision markup round-trips.
        let xml = "<w:document><w:body><w:p>\
            <w:r><w:t>keep </w:t></w:r>\
            <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>added</w:t></w:r></w:ins>\
            <w:del w:id=\"2\" w:author=\"A\"><w:r><w:delText>gone</w:delText></w:r></w:del>\
            </w:p></w:body></w:document>";
        let d = parse_document_xml(xml, &Relationships::default());
        let text = d.plain_text();
        assert!(text.contains("added"), "inserted text lost: {text:?}");
        assert!(text.contains("gone"), "deleted text lost: {text:?}");
        let out = document_to_xml(&d);
        assert!(out.contains("<w:ins w:id=\"1\""), "ins markup lost");
        assert!(out.contains("<w:del w:id=\"2\""), "del markup lost");
        assert!(out.contains("<w:delText>gone</w:delText>"), "delText lost");
        // Save is lossless: re-parsing the output yields the same model.
        assert_eq!(parse_document_xml(&out, &Relationships::default()), d);
    }

    #[test]
    fn footnote_reference_visible_and_lossless() {
        // A footnote/endnote reference used to be dropped (empty run → orphaned
        // notes part). Now it is modeled as a FootnoteRef, shown as a marker, and
        // the reference run survives a save.
        let xml = "<w:document><w:body><w:p>\
            <w:r><w:t>See note</w:t></w:r>\
            <w:r><w:rPr><w:rStyle w:val=\"FootnoteReference\"/></w:rPr>\
              <w:footnoteReference w:id=\"1\"/></w:r>\
            <w:r><w:t> and end</w:t></w:r>\
            <w:r><w:endnoteReference w:id=\"2\"/></w:r>\
            </w:p></w:body></w:document>";
        let d = parse_document_xml(xml, &Relationships::default());
        let refs: Vec<_> = match &d.body[0] {
            Block::Paragraph(p) => p
                .content
                .iter()
                .filter_map(|i| match i {
                    Inline::FootnoteRef { id, endnote, .. } => Some((*id, *endnote)),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        };
        assert_eq!(refs, vec![(1, false), (2, true)]);
        let out = document_to_xml(&d);
        assert!(
            out.contains("<w:footnoteReference w:id=\"1\"/>"),
            "footnote ref lost"
        );
        assert!(
            out.contains("<w:endnoteReference w:id=\"2\"/>"),
            "endnote ref lost"
        );
        assert_eq!(parse_document_xml(&out, &Relationships::default()), d);
    }

    #[test]
    fn run_properties_roundtrip() {
        let props = RunProps {
            bold: true,
            italic: true,
            underline: true,
            strike: true,
            code: false,
            caps: true,
            small_caps: true,
            vanish: true,
            vert_align: VertAlign::Superscript,
            color: Some("FF0000".to_string()),
            highlight: Some("yellow".to_string()),
            size_half_pts: Some(28),
            font: Some("Calibri".to_string()),
            style_id: Some("Emphasis".to_string()),
            ..Default::default()
        };
        let d = Document {
            body: vec![para(ParProps::default(), vec![run("styled", props)])],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn frame_pr_roundtrips() {
        // Floating placement must survive a save so we never corrupt the layout.
        let frame = FramePr {
            x: Some(6481),
            y: Some(2521),
            w: None,
            h: None,
            h_anchor: Some("page".to_string()),
            v_anchor: Some("page".to_string()),
            x_align: None,
            y_align: None,
        };
        let pp = ParProps {
            frame: Some(frame),
            ..Default::default()
        };
        let d = Document {
            body: vec![para(pp, vec![run("x", RunProps::default())])],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);

        // The align-keyword variant too.
        let frame2 = FramePr {
            h_anchor: Some("margin".to_string()),
            x_align: Some("right".to_string()),
            y_align: Some("bottom".to_string()),
            ..Default::default()
        };
        let pp2 = ParProps {
            frame: Some(frame2),
            ..Default::default()
        };
        let d2 = Document {
            body: vec![para(pp2, vec![run("y", RunProps::default())])],
        };
        assert_eq!(roundtrip(&d2, &Relationships::default()), d2);
    }

    #[test]
    fn paragraph_properties_roundtrip() {
        let pp = ParProps {
            style_id: Some("Quote".to_string()),
            align: Align::Center,
            heading_level: None,
            num_id: Some(3),
            ilvl: 1,
            rtl: true,
            frame: None,
            section_break: None,
            tabs: Vec::new(),
            borders: ParBorders {
                bottom: Some(BorderKind::Single),
                top: None,
            },
            indent: 720,
            first_line: -360,
            ..Default::default()
        };
        let d = Document {
            body: vec![para(pp, vec![run("x", RunProps::default())])],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn text_box_splices_edited_content_into_shape() {
        // Edited box content replaces the original txbxContent while the
        // surrounding shape XML is preserved verbatim.
        let raw = "<w:r><w:pict><v:shape><v:textbox><w:txbxContent>\
                   <w:p><w:r><w:t>old</w:t></w:r></w:p>\
                   </w:txbxContent></v:textbox></v:shape></w:pict></w:r>";
        let tb = Inline::TextBox {
            raw: raw.to_string(),
            blocks: vec![para(
                ParProps::default(),
                vec![run("new text", RunProps::default())],
            )],
        };
        let d = Document {
            body: vec![para(ParProps::default(), vec![tb])],
        };
        let xml = document_to_xml(&d);
        assert!(xml.contains("new text"), "edited content missing:\n{xml}");
        assert!(!xml.contains(">old<"), "stale content kept:\n{xml}");
        assert!(
            xml.contains("<v:shape><v:textbox>") && xml.contains("</v:shape>"),
            "shape markup not preserved:\n{xml}"
        );
        // And it reloads as a text box again.
        let back = roundtrip(&d, &Relationships::default());
        match &back.body[0] {
            Block::Paragraph(p) => match &p.content[0] {
                Inline::TextBox { blocks, .. } => {
                    assert_eq!(blocks[0].plain_text(), "new text")
                }
                other => panic!("expected TextBox, got {other:?}"),
            },
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn smartart_serializes_raw_verbatim() {
        // SmartArt carries the original run XML for lossless save; the extracted
        // node text is render-only and must not leak into the saved document.
        let raw = "<w:r><w:drawing><a:graphicData uri=\"x/diagram\">\
                   <dgm:relIds r:dm=\"rId5\"/></a:graphicData></w:drawing></w:r>";
        let d = Document {
            body: vec![para(
                ParProps::default(),
                vec![Inline::SmartArt {
                    raw: raw.to_string(),
                    text: vec!["Build".to_string(), "Ship".to_string()],
                }],
            )],
        };
        let xml = document_to_xml(&d);
        assert!(xml.contains(raw), "raw drawing not preserved:\n{xml}");
        assert!(!xml.contains("Build"), "render-only text leaked into save");
    }

    #[test]
    fn direct_tab_stops_roundtrip() {
        // A TOC-style paragraph with direct `w:tabs` (a left indent stop plus a
        // right-aligned dot-leader stop for the page number) must survive a save.
        let pp = ParProps {
            tabs: vec![
                TabStop {
                    pos: 960,
                    align: TabAlign::Left,
                    leader: TabLeader::None,
                },
                TabStop {
                    pos: 8630,
                    align: TabAlign::Right,
                    leader: TabLeader::Dot,
                },
            ],
            ..ParProps::default()
        };
        let d = Document {
            body: vec![para(pp, vec![run("x", RunProps::default())])],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn section_break_roundtrips() {
        // A mid-document section break (different page size/orientation) must
        // survive a save instead of being dropped.
        let pp = ParProps {
            section_break: Some(
                "<w:sectPr><w:pgSz w:w=\"15840\" w:h=\"12240\" w:orient=\"landscape\"/></w:sectPr>"
                    .to_string(),
            ),
            ..ParProps::default()
        };
        let d = Document {
            body: vec![para(pp, vec![run("x", RunProps::default())])],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn heading_roundtrips_via_style() {
        let pp = ParProps {
            style_id: Some("Heading2".to_string()),
            heading_level: Some(2),
            ..ParProps::default()
        };
        let d = Document {
            body: vec![para(pp, vec![run("Title", RunProps::default())])],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn breaks_and_tabs_roundtrip() {
        let d = Document {
            body: vec![para(
                ParProps::default(),
                vec![
                    run("a", RunProps::default()),
                    Inline::Tab(RunProps::default()),
                    run("b", RunProps::default()),
                    Inline::Break(BreakKind::Line),
                    Inline::Break(BreakKind::Page),
                ],
            )],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn underlined_tab_keeps_its_underline() {
        // A tab carries the run props of its run, so the footer "underlined tab =
        // a line" trick survives a load/save round-trip instead of dropping rPr.
        let d = Document {
            body: vec![para(
                ParProps::default(),
                vec![Inline::Tab(RunProps {
                    underline: true,
                    ..Default::default()
                })],
            )],
        };
        let back = roundtrip(&d, &Relationships::default());
        assert_eq!(back, d);
        assert!(matches!(&back.body[0], Block::Paragraph(p)
            if matches!(&p.content[0], Inline::Tab(rp) if rp.underline)));
    }

    #[test]
    fn special_characters_escape_roundtrip() {
        let d = Document {
            body: vec![para(
                ParProps::default(),
                vec![run("a < b & c > d \"q\"", RunProps::default())],
            )],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn hyperlink_roundtrips_with_rels() {
        let rels = parse_rels_xml(
            "<Relationships><Relationship Id=\"rId5\" Target=\"https://a.test/\" TargetMode=\"External\"/></Relationships>",
        );
        let h = Inline::Hyperlink(Hyperlink {
            target: Some("https://a.test/".to_string()),
            anchor: None,
            rel_id: Some("rId5".to_string()),
            runs: vec![Run {
                text: "click".to_string(),
                props: RunProps::default(),
            }],
        });
        let d = Document {
            body: vec![para(ParProps::default(), vec![h])],
        };
        assert_eq!(roundtrip(&d, &rels), d);
    }

    #[test]
    fn table_roundtrips() {
        let cell = |s: &str, span: u32| Cell {
            grid_span: span,
            v_merge: VMerge::None,
            blocks: vec![para(ParProps::default(), vec![run(s, RunProps::default())])],
            ..Default::default()
        };
        let t = Table {
            grid: vec![100, 200],
            rows: vec![
                Row {
                    cells: vec![cell("wide", 2)],
                    ..Default::default()
                },
                Row {
                    cells: vec![
                        Cell {
                            v_merge: VMerge::Restart,
                            ..cell("top", 1)
                        },
                        cell("b", 1),
                    ],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let d = Document {
            body: vec![Block::Table(t)],
        };
        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }
}
