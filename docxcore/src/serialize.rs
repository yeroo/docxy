//! Serialize the [`crate::model`] document tree back to `word/document.xml`.
//!
//! This is a *semantic* serializer: it re-emits the structure and properties we
//! model (paragraphs, runs + rPr, tables, lists, hyperlinks). It is designed so
//! that `parse_document_xml(document_to_xml(&doc)) == doc` for everything we
//! model — see the round-trip tests. Body content we do not model (e.g.
//! `sectPr`, bookmarks) is preserved separately by the package layer, not here.

use crate::model::*;
use crate::xml::{Event, XmlParser};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const W15_NS: &str = "http://schemas.microsoft.com/office/word/2012/wordml";

/// Serialize a document to the bytes of `word/document.xml`.
pub fn document_to_xml(doc: &Document) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    // `m:` supports equations authored from Markdown. Row-level repeating
    // sections use the Office 2013 `w15:` vocabulary, which must stay bound
    // when their captured properties are placed in the new document root.
    s.push_str(&format!(
        "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:m=\"{M_NS}\" \
         xmlns:mc=\"{MC_NS}\" xmlns:w15=\"{W15_NS}\" mc:Ignorable=\"w15\"><w:body>"
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

/// Ordinal of a `CT_PPr` child element (local name, no `w:` prefix) in the
/// WordprocessingML schema's fixed child order. Unknown elements sort last but
/// keep their relative order. Word (and strict validators) reject `<w:pPr>`
/// children out of this order, so both modeled and preserved (`raw_props`)
/// children are emitted through this rank.
fn ppr_rank(local: &str) -> u32 {
    const ORDER: [&str; 36] = [
        "pStyle",
        "keepNext",
        "keepLines",
        "pageBreakBefore",
        "framePr",
        "widowControl",
        "numPr",
        "suppressLineNumbers",
        "pBdr",
        "shd",
        "tabs",
        "suppressAutoHyphens",
        "kinsoku",
        "wordWrap",
        "overflowPunct",
        "topLinePunct",
        "autoSpaceDE",
        "autoSpaceDN",
        "bidi",
        "adjustRightInd",
        "snapToGrid",
        "spacing",
        "ind",
        "contextualSpacing",
        "mirrorIndents",
        "suppressOverlap",
        "jc",
        "textDirection",
        "textAlignment",
        "textboxTightWrap",
        "outlineLvl",
        "divId",
        "cnfStyle",
        "rPr",
        "sectPr",
        "pPrChange",
    ];
    ORDER
        .iter()
        .position(|&e| e == local)
        .map_or(u32::MAX, |i| i as u32)
}

/// The local element name of a serialized child (`"<w:spacing …/>"` → `spacing`),
/// used to rank preserved `raw_props` against the modeled children.
fn local_name(raw: &str) -> &str {
    let t = raw.trim_start();
    let Some(rest) = t.strip_prefix('<') else {
        return "";
    };
    let end = rest
        .find([' ', '/', '>', '\t', '\n', '\r'])
        .unwrap_or(rest.len());
    let name = &rest[..end];
    name.rsplit(':').next().unwrap_or(name)
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
        || props.indent_right != 0
        || props.first_line != 0
        || !props.spacing.is_empty()
        || !props.tabs.is_empty()
        || !props.raw_props.is_empty();
    if !has_any {
        return;
    }

    // Assemble children as (schema rank, xml) then stable-sort, so modeled and
    // preserved children interleave in the order `CT_PPr` requires (e.g. a
    // preserved `w:spacing`/`w:shd`/`w:bidi` lands before the modeled
    // `w:ind`/`w:jc`, and a paragraph-mark `w:rPr` stays just before `sectPr`).
    let mut parts: Vec<(u32, String)> = Vec::new();

    if let Some(st) = &style {
        let mut x = String::from("<w:pStyle w:val=\"");
        esc_attr(st, &mut x);
        x.push_str("\"/>");
        parts.push((ppr_rank("pStyle"), x));
    }
    if let Some(f) = &props.frame {
        let mut x = String::from("<w:framePr");
        if let Some(v) = f.w {
            x.push_str(&format!(" w:w=\"{v}\""));
        }
        if let Some(v) = f.h {
            x.push_str(&format!(" w:h=\"{v}\""));
        }
        if let Some(a) = &f.h_anchor {
            x.push_str(" w:hAnchor=\"");
            esc_attr(a, &mut x);
            x.push('"');
        }
        if let Some(a) = &f.v_anchor {
            x.push_str(" w:vAnchor=\"");
            esc_attr(a, &mut x);
            x.push('"');
        }
        if let Some(a) = &f.x_align {
            x.push_str(" w:xAlign=\"");
            esc_attr(a, &mut x);
            x.push('"');
        }
        if let Some(a) = &f.y_align {
            x.push_str(" w:yAlign=\"");
            esc_attr(a, &mut x);
            x.push('"');
        }
        if let Some(v) = f.x {
            x.push_str(&format!(" w:x=\"{v}\""));
        }
        if let Some(v) = f.y {
            x.push_str(&format!(" w:y=\"{v}\""));
        }
        x.push_str("/>");
        parts.push((ppr_rank("framePr"), x));
    }
    if let Some(num) = props.num_id {
        parts.push((
            ppr_rank("numPr"),
            format!(
                "<w:numPr><w:ilvl w:val=\"{}\"/><w:numId w:val=\"{}\"/></w:numPr>",
                props.ilvl, num
            ),
        ));
    }
    if props.borders.top.is_some() || props.borders.bottom.is_some() {
        let mut x = String::from("<w:pBdr>");
        for (tag, side) in [
            ("w:top", props.borders.top),
            ("w:bottom", props.borders.bottom),
        ] {
            if let Some(k) = side {
                x.push_str(&format!(
                    "<{tag} w:val=\"{}\" w:sz=\"6\" w:space=\"1\" w:color=\"auto\"/>",
                    k.to_val()
                ));
            }
        }
        x.push_str("</w:pBdr>");
        parts.push((ppr_rank("pBdr"), x));
    }
    if !props.tabs.is_empty() {
        let mut x = String::from("<w:tabs>");
        for t in &props.tabs {
            let val = match t.align {
                TabAlign::Center => "center",
                TabAlign::Right => "right",
                TabAlign::Left => "left",
            };
            x.push_str(&format!("<w:tab w:val=\"{val}\""));
            let leader = match t.leader {
                TabLeader::Dot => Some("dot"),
                TabLeader::Hyphen => Some("hyphen"),
                TabLeader::Underscore => Some("underscore"),
                TabLeader::None => None,
            };
            if let Some(l) = leader {
                x.push_str(&format!(" w:leader=\"{l}\""));
            }
            x.push_str(&format!(" w:pos=\"{}\"/>", t.pos));
        }
        x.push_str("</w:tabs>");
        parts.push((ppr_rank("tabs"), x));
    }
    if !props.spacing.is_empty() {
        let sp = &props.spacing;
        let mut x = String::from("<w:spacing");
        // Emitted in CT_Spacing schema order (Word ignores attribute order, but
        // matching the schema keeps diffs against real files small).
        if let Some(v) = sp.before {
            x.push_str(&format!(" w:before=\"{v}\""));
        }
        if let Some(v) = sp.before_lines {
            x.push_str(&format!(" w:beforeLines=\"{v}\""));
        }
        if let Some(a) = &sp.before_auto {
            x.push_str(" w:beforeAutospacing=\"");
            esc_attr(a, &mut x);
            x.push('"');
        }
        if let Some(v) = sp.after {
            x.push_str(&format!(" w:after=\"{v}\""));
        }
        if let Some(v) = sp.after_lines {
            x.push_str(&format!(" w:afterLines=\"{v}\""));
        }
        if let Some(a) = &sp.after_auto {
            x.push_str(" w:afterAutospacing=\"");
            esc_attr(a, &mut x);
            x.push('"');
        }
        if let Some(v) = sp.line {
            x.push_str(&format!(" w:line=\"{v}\""));
        }
        if let Some(r) = &sp.line_rule {
            x.push_str(" w:lineRule=\"");
            esc_attr(r, &mut x);
            x.push('"');
        }
        x.push_str("/>");
        parts.push((ppr_rank("spacing"), x));
    }
    if props.indent != 0 || props.first_line != 0 || props.indent_right != 0 {
        let mut x = String::from("<w:ind");
        if props.indent != 0 {
            x.push_str(&format!(" w:left=\"{}\"", props.indent));
        }
        if props.indent_right != 0 {
            x.push_str(&format!(" w:right=\"{}\"", props.indent_right));
        }
        // firstLine and hanging are mutually exclusive; both are non-negative.
        match props.first_line.cmp(&0) {
            std::cmp::Ordering::Greater => {
                x.push_str(&format!(" w:firstLine=\"{}\"", props.first_line))
            }
            std::cmp::Ordering::Less => {
                x.push_str(&format!(" w:hanging=\"{}\"", -props.first_line))
            }
            std::cmp::Ordering::Equal => {}
        }
        x.push_str("/>");
        parts.push((ppr_rank("ind"), x));
    }
    match props.align {
        Align::Left => {}
        Align::Center => parts.push((ppr_rank("jc"), "<w:jc w:val=\"center\"/>".into())),
        Align::Right => parts.push((ppr_rank("jc"), "<w:jc w:val=\"right\"/>".into())),
        Align::Justify => parts.push((ppr_rank("jc"), "<w:jc w:val=\"both\"/>".into())),
    }
    if props.rtl {
        parts.push((ppr_rank("bidi"), "<w:bidi/>".into()));
    }
    // Preserved unmodeled pPr children (paragraph-mark `w:rPr`, `outlineLvl`,
    // shading, spacing, …), each ranked by its own element name.
    for raw in &props.raw_props {
        parts.push((ppr_rank(local_name(raw)), raw.clone()));
    }
    if let Some(sect) = &props.section_break {
        parts.push((ppr_rank("sectPr"), sect.clone()));
    }

    parts.sort_by_key(|(rank, _)| *rank);
    s.push_str("<w:pPr>");
    for (_, x) in &parts {
        s.push_str(x);
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
    // A boundary-aware table is a mixed child sequence. tblPr/tblGrid remain
    // first as required by CT_Tbl; each gap's invisible children are then
    // emitted immediately before the visible row anchored at that gap.
    //
    // `document_to_xml` cannot report a model-validation error. If a caller
    // constructs an invalid boundary sequence, omit the whole sequence and
    // retain every visible row instead of writing unbalanced XML. Loaders and
    // row-edit helpers maintain the invariant, so this recovery affects only
    // malformed manually-constructed models.
    if t.validate_row_boundaries().is_ok() {
        let mut boundary_index = 0;
        let mut content_stack = Vec::new();
        for row_index in 0..=t.rows.len() {
            while boundary_index < t.row_boundaries.len()
                && t.row_boundaries[boundary_index].at == row_index
            {
                match &t.row_boundaries[boundary_index].kind {
                    TableRowBoundaryKind::SdtOpen(raw) => {
                        let is_empty_control = matches!(
                            t.row_boundaries.get(boundary_index + 1),
                            Some(TableRowBoundary {
                                at,
                                kind: TableRowBoundaryKind::SdtClose(_),
                            }) if *at == row_index
                        );
                        content_stack.push(write_sdt_open(s, raw, is_empty_control));
                    }
                    TableRowBoundaryKind::SdtClose(raw) => {
                        let content_needs_close = content_stack.pop().unwrap_or(true);
                        write_sdt_close(s, raw, content_needs_close);
                    }
                    TableRowBoundaryKind::Raw(raw) => s.push_str(raw),
                }
                boundary_index += 1;
            }
            if let Some(row) = t.rows.get(row_index) {
                write_row(s, row);
            }
        }
        debug_assert!(content_stack.is_empty());
        debug_assert_eq!(boundary_index, t.row_boundaries.len());
    } else {
        for row in &t.rows {
            write_row(s, row);
        }
    }
    s.push_str("</w:tbl>");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SdtOpenShape {
    ContentOpen,
    ContentEmpty,
    MissingContent,
    Invalid,
}

/// Inspect the captured SDT prefix without changing it. A valid row-control
/// prefix ends with either an open or self-closing direct `w:sdtContent` child.
fn sdt_open_shape(raw: &str) -> SdtOpenShape {
    let mut parser = XmlParser::new(raw);
    if parser.next() != Event::Start || parser.name() != "w:sdt" {
        return SdtOpenShape::Invalid;
    }

    let mut depth = 1usize;
    loop {
        match parser.next() {
            Event::Start if depth == 1 && parser.name() == "w:sdtContent" => {
                let tag = parser.raw_slice(parser.start_pos(), parser.pos());
                return if tag.trim_end().ends_with("/>") {
                    SdtOpenShape::ContentEmpty
                } else {
                    SdtOpenShape::ContentOpen
                };
            }
            Event::Start => depth += 1,
            Event::End if depth == 1 => return SdtOpenShape::Invalid,
            Event::End => depth -= 1,
            Event::Eof => return SdtOpenShape::MissingContent,
            Event::Text => {}
        }
    }
}

/// Emit an SDT prefix and return whether its content element still needs a
/// closing tag. Valid captured prefixes are copied exactly; recovery adds only
/// structural tags absent from malformed/truncated source.
fn write_sdt_open(s: &mut String, raw: &str, is_empty_control: bool) -> bool {
    match sdt_open_shape(raw) {
        SdtOpenShape::ContentOpen => {
            s.push_str(raw);
            true
        }
        SdtOpenShape::ContentEmpty => {
            if is_empty_control {
                s.push_str(raw);
                false
            } else if let Some(slash) = raw.rfind("/>") {
                // A self-closing content tag cannot own rows. This can only
                // arise in a manually-mutated model; open that exact captured
                // tag and let the matching close boundary finish it.
                s.push_str(&raw[..slash]);
                s.push('>');
                s.push_str(&raw[slash + 2..]);
                true
            } else {
                s.push_str("<w:sdt><w:sdtContent>");
                true
            }
        }
        SdtOpenShape::MissingContent => {
            s.push_str(raw);
            s.push_str("<w:sdtContent>");
            true
        }
        SdtOpenShape::Invalid => {
            s.push_str("<w:sdt><w:sdtContent>");
            true
        }
    }
}

fn sdt_close_tags(raw: &str) -> (bool, bool) {
    let mut parser = XmlParser::new(raw);
    let mut content_close = false;
    let mut sdt_close = false;
    loop {
        match parser.next() {
            Event::End if parser.name() == "w:sdtContent" => content_close = true,
            Event::End if parser.name() == "w:sdt" => sdt_close = true,
            Event::Eof => return (content_close, sdt_close),
            _ => {}
        }
    }
}

fn write_sdt_close(s: &mut String, raw: &str, content_needs_close: bool) {
    let (has_content_close, has_sdt_close) = sdt_close_tags(raw);
    if content_needs_close && !has_content_close {
        s.push_str("</w:sdtContent>");
    }
    s.push_str(raw);
    if !has_sdt_close {
        s.push_str("</w:sdt>");
    }
}

fn write_row(s: &mut String, row: &Row) {
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
        // A paragraph carrying shading (unmodeled) + spacing (modeled), and a
        // table whose tblPr / trPr / tcPr carry borders + shading — none of which
        // the model represents — must all survive a save round-trip instead of
        // being silently dropped (docxy gap D-1).
        let ppr = ParProps {
            raw_props: vec![
                "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"FFFF00\"/>".to_string(),
            ],
            spacing: crate::model::Spacing {
                before: Some(120),
                after: Some(120),
                ..Default::default()
            },
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
            row_boundaries: vec![],
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
    fn ppr_children_emitted_in_schema_order() {
        // Modeled ind/jc/bidi and preserved shd/spacing must interleave in the
        // CT_PPr order: shd(9) < bidi(18) < spacing(21) < ind(22) < jc(26) — even
        // though the raw_props are supplied out of order.
        let ppr = ParProps {
            indent: 100,
            align: Align::Center,
            rtl: true,
            raw_props: vec![
                "<w:spacing w:before=\"120\"/>".to_string(),
                "<w:shd w:val=\"clear\" w:fill=\"FFFF00\"/>".to_string(),
            ],
            ..Default::default()
        };
        let d = Document {
            body: vec![para(ppr, vec![])],
        };
        let xml = document_to_xml(&d);
        let at = |needle: &str| {
            xml.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        let (shd, bidi, spacing, ind, jc) = (
            at("<w:shd"),
            at("<w:bidi/>"),
            at("<w:spacing"),
            at("<w:ind"),
            at("<w:jc"),
        );
        assert!(
            shd < bidi && bidi < spacing && spacing < ind && ind < jc,
            "pPr children out of schema order: shd={shd} bidi={bidi} spacing={spacing} ind={ind} jc={jc}"
        );
        // Reordering is a normalization: re-parsing then re-serializing is stable.
        let d2 = roundtrip(&d, &Relationships::default());
        assert_eq!(document_to_xml(&d2), xml);
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
    fn symbol_run_becomes_glyph_and_lossless() {
        // A <w:sym> used to be preserved-but-invisible; now its font code point
        // renders as a Unicode glyph while the run round-trips.
        let xml = "<w:document><w:body><w:p>\
            <w:r><w:sym w:font=\"Symbol\" w:char=\"F062\"/></w:r>\
            <w:r><w:sym w:font=\"Symbol\" w:char=\"F0B7\"/></w:r>\
            <w:r><w:sym w:font=\"Wingdings\" w:char=\"F04A\"/></w:r>\
            </w:p></w:body></w:document>";
        let d = parse_document_xml(xml, &Relationships::default());
        let text = d.plain_text();
        assert!(text.contains('β'), "Symbol 'b' → beta; got {text:?}");
        assert!(text.contains('•'), "Symbol 0xB7 → bullet; got {text:?}");
        assert!(
            text.contains('□'),
            "unknown font → placeholder; got {text:?}"
        );
        let out = document_to_xml(&d);
        assert_eq!(out.matches("<w:sym").count(), 3, "sym runs lost");
        assert_eq!(parse_document_xml(&out, &Relationships::default()), d);
    }

    #[test]
    fn internal_anchor_link_navigable_toc_keeps_tabs() {
        // A plain internal link (a cross-reference) becomes a navigable Hyperlink.
        let xr = "<w:document><w:body><w:p>\
            <w:hyperlink w:anchor=\"_Ref1\"><w:r><w:t>see Section 3</w:t></w:r></w:hyperlink>\
            </w:p></w:body></w:document>";
        let d = parse_document_xml(xr, &Relationships::default());
        match &d.body[0] {
            Block::Paragraph(p) => match &p.content[0] {
                Inline::Hyperlink(h) => {
                    assert_eq!(h.anchor.as_deref(), Some("_Ref1"));
                    assert!(h.target.is_none());
                }
                other => panic!("expected navigable Hyperlink, got {other:?}"),
            },
            _ => panic!("no paragraph"),
        }
        // Round-trips (the w:anchor survives).
        assert_eq!(
            parse_document_xml(&document_to_xml(&d), &Relationships::default()),
            d
        );

        // A TOC entry (internal link carrying a tab) keeps the tab at top level
        // (so its leader dots still render) AND stays navigable: the text on each
        // side of the tab becomes a Hyperlink segment carrying the anchor.
        let toc = "<w:document><w:body><w:p>\
            <w:hyperlink w:anchor=\"_Toc1\"><w:r><w:t>Intro</w:t></w:r>\
              <w:r><w:tab/></w:r><w:r><w:t>9</w:t></w:r></w:hyperlink>\
            </w:p></w:body></w:document>";
        let d2 = parse_document_xml(toc, &Relationships::default());
        match &d2.body[0] {
            Block::Paragraph(p) => {
                assert!(
                    p.content.iter().any(|i| matches!(i, Inline::Tab(_))),
                    "TOC tab dropped"
                );
                let links = p
                    .content
                    .iter()
                    .filter(|i| matches!(i, Inline::Hyperlink(h) if h.anchor.as_deref() == Some("_Toc1")))
                    .count();
                assert_eq!(
                    links, 2,
                    "TOC text should be navigable on both sides of the tab"
                );
            }
            _ => panic!("no paragraph"),
        }
        // Round-trips within the model.
        assert_eq!(
            parse_document_xml(&document_to_xml(&d2), &Relationships::default()),
            d2
        );
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

    fn table_xml(children: &str) -> String {
        format!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body><w:tbl>{children}</w:tbl></w:body></w:document>"
        )
    }

    fn row_xml(text: &str) -> String {
        format!("<w:tr><w:tc><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc></w:tr>")
    }

    #[test]
    fn row_controls_roundtrip_nested_empty_unknown_unicode_and_multiple_groups() {
        let tbl_pr = "<w:tblPr><w:tblStyle w:val=\"TableGrid\"/></w:tblPr>";
        let grid = "<w:tblGrid><w:gridCol w:w=\"2400\"/></w:tblGrid>";
        let outer_open = "<w:sdt data-source=\"orders\"><w:sdtPr><w:alias w:val=\"Заказы\"/><w15:repeatingSection w15:sectionTitle=\"Order\"/></w:sdtPr><w:sdtEndPr><w:rPr><w:b/></w:rPr></w:sdtEndPr><w:sdtContent>";
        let inner_open = "<w:sdt><w:sdtPr><w:tag w:val=\"nested\"/></w:sdtPr><w:sdtContent>";
        let empty_open = "<w:sdt><w:sdtPr><w:alias w:val=\"empty\"/></w:sdtPr><w:sdtContent>";
        let group_open = "<w:sdt xmlns:ux=\"urn:docxy:fixture\"><w:sdtPr><ux:unknown ux:val=\"kept\"/></w:sdtPr><w:sdtContent>";
        let unknown_child =
            "<w:customXml w:uri=\"urn:rows\"><w:future w:val=\"preserve\"/></w:customXml>";
        let close = "</w:sdtContent></w:sdt>";
        let source = table_xml(&format!(
            "{tbl_pr}{grid}{outer_open}{}{inner_open}{}{close}{close}{empty_open}{close}{group_open}{}{unknown_child}{}{close}",
            row_xml("Привет 🌍"),
            row_xml("東京"),
            row_xml("α"),
            row_xml("β")
        ));

        let parsed = parse_document_xml(&source, &Relationships::default());
        let saved = document_to_xml(&parsed);

        for raw in [
            outer_open,
            inner_open,
            empty_open,
            group_open,
            unknown_child,
        ] {
            assert!(saved.contains(raw), "captured wrapper XML changed: {raw}");
        }
        assert_eq!(saved.matches(close).count(), 4);
        assert!(
            saved.contains("xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\"")
        );
        let at = |needle: &str| {
            saved
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        assert!(
            at(tbl_pr) < at(grid) && at(grid) < at(outer_open) && at(outer_open) < at("<w:tr>"),
            "table properties, grid, boundaries, and rows are out of schema order"
        );

        let reparsed = parse_document_xml(&saved, &Relationships::default());
        assert_eq!(reparsed, parsed);
        assert_eq!(reparsed.plain_text(), "Привет 🌍\n東京\nα\nβ\n");
    }

    #[test]
    fn editing_controlled_row_changes_only_row_payload() {
        let open = "<w:sdt data-origin=\"fixture\"><w:sdtPr><w:alias w:val=\"Repeat\"/><w:tag w:val=\"rows\"/></w:sdtPr><w:sdtEndPr><w:rPr><w:i/></w:rPr></w:sdtEndPr><w:sdtContent>";
        let close = "</w:sdtContent><w:future w:val=\"tail\"/></w:sdt>";
        let source = table_xml(&format!("{open}{}{close}", row_xml("before")));
        let mut parsed = parse_document_xml(&source, &Relationships::default());

        let Block::Table(table) = &mut parsed.body[0] else {
            panic!("expected table");
        };
        let Block::Paragraph(paragraph) = &mut table.rows[0].cells[0].blocks[0] else {
            panic!("expected paragraph");
        };
        let Inline::Run(run) = &mut paragraph.content[0] else {
            panic!("expected run");
        };
        run.text = "after ✓".to_string();

        let saved = document_to_xml(&parsed);
        assert!(saved.contains(open), "opening wrapper changed");
        assert!(saved.contains(close), "closing wrapper changed");
        assert!(saved.contains("after ✓</w:t>"));
        assert!(!saved.contains(">before<"));
        assert_eq!(
            parse_document_xml(&saved, &Relationships::default()),
            parsed
        );
    }

    #[test]
    fn invalid_boundary_sequences_cannot_emit_unbalanced_wrappers() {
        let row = Row {
            cells: vec![Cell {
                blocks: vec![para(
                    ParProps::default(),
                    vec![run("visible", RunProps::default())],
                )],
                ..Default::default()
            }],
            ..Default::default()
        };
        let invalid_sequences = [
            vec![TableRowBoundary::sdt_open(0, "<w:sdt><w:sdtContent>")],
            vec![TableRowBoundary::sdt_close(1, "</w:sdtContent></w:sdt>")],
        ];

        for row_boundaries in invalid_sequences {
            let document = Document {
                body: vec![Block::Table(Table {
                    rows: vec![row.clone()],
                    row_boundaries,
                    ..Default::default()
                })],
            };
            let saved = document_to_xml(&document);
            assert!(!saved.contains("<w:sdt>"));
            assert!(!saved.contains("</w:sdt>"));
            let reparsed = parse_document_xml(&saved, &Relationships::default());
            assert_eq!(reparsed.plain_text(), "visible\n");
        }
    }

    #[test]
    fn truncated_boundary_fragments_receive_only_missing_closing_tags() {
        let open = "<w:sdt><w:sdtPr><w:alias w:val=\"truncated\"/></w:sdtPr><w:sdtContent>";
        let source = format!("<w:document><w:body><w:tbl>{open}{}", row_xml("survives"));
        let parsed = parse_document_xml(&source, &Relationships::default());
        let saved = document_to_xml(&parsed);

        assert!(saved.contains(open), "captured prefix changed");
        assert!(
            saved.contains(
                "survives</w:t></w:r></w:p></w:tc></w:tr></w:sdtContent></w:sdt></w:tbl>"
            )
        );
        let reparsed = parse_document_xml(&saved, &Relationships::default());
        let Block::Table(table) = &reparsed.body[0] else {
            panic!("expected table");
        };
        assert!(table.validate_row_boundaries().is_ok());
        assert_eq!(reparsed.plain_text(), "survives\n");
    }

    #[test]
    fn self_closing_content_is_opened_when_a_mutated_model_assigns_it_rows() {
        let document = Document {
            body: vec![Block::Table(Table {
                rows: vec![Row {
                    cells: vec![Cell {
                        blocks: vec![para(
                            ParProps::default(),
                            vec![run("owned", RunProps::default())],
                        )],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                row_boundaries: vec![
                    TableRowBoundary::sdt_open(
                        0,
                        "<w:sdt><w:sdtPr><w:alias w:val=\"was-empty\"/></w:sdtPr><w:sdtContent/>",
                    ),
                    TableRowBoundary::sdt_close(1, "</w:sdt>"),
                ],
                ..Default::default()
            })],
        };

        let saved = document_to_xml(&document);
        assert!(saved.contains("<w:sdtContent><w:tr>"));
        assert!(saved.contains("</w:tr></w:sdtContent></w:sdt>"));
        let reparsed = parse_document_xml(&saved, &Relationships::default());
        let Block::Table(table) = &reparsed.body[0] else {
            panic!("expected table");
        };
        assert_eq!(table.row_control_owners(), Ok(vec![vec![0]]));
    }
}
