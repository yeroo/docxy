//! Serialize the [`crate::model`] document tree back to `word/document.xml`.
//!
//! This is a *semantic* serializer: it re-emits the structure and properties we
//! model (paragraphs, runs + rPr, tables, lists, hyperlinks). It is designed so
//! that `parse_document_xml(document_to_xml(&doc)) == doc` for everything we
//! model — see the round-trip tests. Unknown body content remains raw, while the
//! body-level final `sectPr` is modeled so its revision can be reviewed.

use crate::model::*;
use crate::xml::{Event, XmlParser};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
const MC_NS: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const W15_NS: &str = "http://schemas.microsoft.com/office/word/2012/wordml";

/// Serialize a document to the bytes of `word/document.xml`.
pub fn document_to_xml(doc: &Document) -> String {
    let mut body = String::new();
    for block in &doc.body {
        write_block(&mut body, block);
    }

    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    // `m:` supports equations authored from Markdown. Row-level repeating
    // sections use the Office 2013 `w15:` vocabulary, which must stay bound
    // when their captured properties are placed in the new document root.
    s.push_str(&format!(
        "<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:m=\"{M_NS}\""
    ));
    let uses_w15 = xml_uses_prefix(&body, "w15");
    let uses_mc = uses_w15 || xml_uses_prefix(&body, "mc");
    if uses_mc {
        s.push_str(&format!(" xmlns:mc=\"{MC_NS}\""));
    }
    if uses_w15 {
        s.push_str(&format!(" xmlns:w15=\"{W15_NS}\" mc:Ignorable=\"w15\""));
    }
    s.push_str("><w:body>");
    s.push_str(&body);
    s.push_str("</w:body></w:document>");
    s
}

fn xml_uses_prefix(xml: &str, prefix: &str) -> bool {
    let qualified_prefix = format!("{prefix}:");
    let namespace_name = format!("xmlns:{prefix}");
    let mut parser = XmlParser::new(xml);
    loop {
        match parser.next() {
            Event::Start => {
                let mut used = parser.name().starts_with(&qualified_prefix);
                for attr in parser.attrs() {
                    if !attr.name.starts_with("xmlns") && attr.name.starts_with(&qualified_prefix) {
                        used = true;
                    }
                    let local_name = attr
                        .name
                        .split_once(':')
                        .map_or(attr.name, |(_, local)| local);
                    if matches!(
                        local_name,
                        "Ignorable"
                            | "ProcessContent"
                            | "PreserveElements"
                            | "PreserveAttributes"
                            | "Requires"
                    ) && attr.value.split_whitespace().any(|token| {
                        token == prefix
                            || token
                                .strip_prefix(prefix)
                                .is_some_and(|suffix| suffix.starts_with(':'))
                    }) {
                        used = true;
                    }
                }
                if used
                    && !parser
                        .namespace_attrs()
                        .iter()
                        .any(|attr| attr.name == namespace_name)
                {
                    return true;
                }
            }
            Event::Eof => return false,
            Event::End | Event::Text => {}
        }
    }
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
        Block::SectionProperties(section) => s.push_str(&with_property_change(
            &section.raw,
            "w:sectPr",
            section.property_change.as_ref(),
        )),
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
    if local == "pPrChange" {
        return u32::MAX;
    }
    ORDER
        .iter()
        .position(|&e| e == local)
        .map_or(u32::MAX - 1, |i| i as u32)
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

/// Append a tracked-property record as the final child of its current property
/// container. Every `*PrChange` is last in the corresponding CT_*Pr schema.
/// Parsed containers have already had this child separated by the loader; the
/// containment check is a guard for manually-constructed legacy model values.
fn with_property_change(
    raw: &str,
    container_name: &str,
    change: Option<&PropertyChange>,
) -> String {
    let Some(change) = change else {
        return raw.to_string();
    };
    if raw.contains(&change.raw) {
        return raw.to_string();
    }

    let mut parser = XmlParser::new(raw);
    if parser.next() != Event::Start || parser.name() != container_name {
        return format!("<{container_name}>{}</{container_name}>", change.raw);
    }
    let opening_end = parser.pos();
    let opening = &raw[..opening_end];
    if opening.trim_end().ends_with("/>") {
        let Some(slash) = opening.rfind("/>") else {
            return raw.to_string();
        };
        let mut out =
            String::with_capacity(raw.len() + change.raw.len() + container_name.len() + 2);
        out.push_str(&raw[..slash]);
        out.push('>');
        out.push_str(&change.raw);
        out.push_str("</");
        out.push_str(container_name);
        out.push('>');
        out.push_str(&raw[opening_end..]);
        return out;
    }

    let close = format!("</{container_name}>");
    let Some(close_at) = raw.rfind(&close) else {
        return raw.to_string();
    };
    let mut out = String::with_capacity(raw.len() + change.raw.len());
    out.push_str(&raw[..close_at]);
    out.push_str(&change.raw);
    out.push_str(&raw[close_at..]);
    out
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
        || !props.raw_props.is_empty()
        || props.property_change.is_some()
        || props.section_property_change.is_some();
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
        let name = local_name(raw);
        let superseded = matches!(name, "jc" if props.align != Align::Left)
            || matches!(name, "bidi" if props.rtl)
            || matches!(name, "pPrChange" if props.property_change.is_some());
        if !superseded {
            parts.push((ppr_rank(name), raw.clone()));
        }
    }
    if let Some(sect) = &props.section_break {
        parts.push((
            ppr_rank("sectPr"),
            with_property_change(sect, "w:sectPr", props.section_property_change.as_ref()),
        ));
    } else if let Some(change) = &props.section_property_change {
        parts.push((
            ppr_rank("sectPr"),
            format!("<w:sectPr>{}</w:sectPr>", change.raw),
        ));
    }
    if let Some(change) = &props.property_change {
        parts.push((ppr_rank("pPrChange"), change.raw.clone()));
    }

    parts.sort_by_key(|(rank, _)| *rank);
    s.push_str("<w:pPr>");
    for (_, x) in &parts {
        s.push_str(x);
    }
    s.push_str("</w:pPr>");
}

#[derive(Clone, Copy)]
enum RunTextKind {
    Normal,
    Deleted,
}

fn write_inline(s: &mut String, item: &Inline) {
    write_inline_with_text_kind(s, item, RunTextKind::Normal);
}

fn write_inline_with_text_kind(s: &mut String, item: &Inline, text_kind: RunTextKind) {
    match item {
        Inline::Run(r) => write_run(s, r, text_kind),
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
            if let Some(raw) = &h.raw
                && !h.content_changed
            {
                s.push_str(raw);
                return;
            }
            if let Some(raw) = &h.raw {
                let mut parser = XmlParser::new(raw);
                if parser.next() == Event::Start && parser.name() == "w:hyperlink" {
                    let opening = &raw[..parser.pos()];
                    if opening.trim_end().ends_with("/>") {
                        if let Some(slash) = opening.rfind("/>") {
                            s.push_str(&opening[..slash]);
                            s.push('>');
                        } else {
                            s.push_str(opening);
                        }
                    } else {
                        s.push_str(opening);
                    }
                } else {
                    s.push_str("<w:hyperlink>");
                }
                for run in &h.runs {
                    write_run(s, run, text_kind);
                }
                for item in &h.content {
                    write_inline_with_text_kind(s, item, text_kind);
                }
                s.push_str("</w:hyperlink>");
                return;
            }
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
                write_run(s, r, text_kind);
            }
            for item in &h.content {
                write_inline_with_text_kind(s, item, text_kind);
            }
            s.push_str("</w:hyperlink>");
        }
        Inline::SmartArt { raw, .. } => s.push_str(raw),
        Inline::Chart { raw, .. } => s.push_str(raw),
        Inline::Equation { raw, .. } => s.push_str(raw),
        Inline::Field { raw, .. } => s.push_str(raw),
        // Untouched tracked changes remain byte-faithful. If a descendant was
        // acted on, retain the exact wrapper start tag/metadata and rebuild only
        // its inline payload so the nested action survives save/reload.
        Inline::Revision {
            kind,
            raw,
            content,
            content_changed,
            ..
        } => {
            if *content_changed {
                write_changed_revision(s, *kind, raw, content);
            } else {
                s.push_str(raw);
            }
        }
        Inline::UnsupportedRevision { raw, .. } => s.push_str(raw),
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

fn write_changed_revision(s: &mut String, kind: RevisionKind, raw: &str, content: &[Inline]) {
    let mut parser = XmlParser::new(raw);
    if parser.next() != Event::Start {
        s.push_str(raw);
        return;
    }
    let name = parser.name().to_string();
    let opening_end = parser.pos();
    let opening = &raw[..opening_end];
    if opening.trim_end().ends_with("/>") {
        if let Some(slash) = opening.rfind("/>") {
            s.push_str(&opening[..slash]);
            s.push('>');
        } else {
            s.push_str(opening);
        }
    } else {
        s.push_str(opening);
    }

    let text_kind = match kind {
        RevisionKind::Insert => RunTextKind::Normal,
        RevisionKind::Delete => RunTextKind::Deleted,
    };
    for item in content {
        write_inline_with_text_kind(s, item, text_kind);
    }
    s.push_str("</");
    s.push_str(&name);
    s.push('>');
}

fn write_run(s: &mut String, r: &Run, text_kind: RunTextKind) {
    s.push_str("<w:r>");
    write_rpr(s, &r.props);
    match text_kind {
        RunTextKind::Normal => s.push_str("<w:t xml:space=\"preserve\">"),
        RunTextKind::Deleted => s.push_str("<w:delText xml:space=\"preserve\">"),
    }
    esc_text(&r.text, s);
    match text_kind {
        RunTextKind::Normal => s.push_str("</w:t></w:r>"),
        RunTextKind::Deleted => s.push_str("</w:delText></w:r>"),
    }
}

fn rpr_rank(local: &str) -> u32 {
    const ORDER: [&str; 41] = [
        "rStyle",
        "rFonts",
        "b",
        "bCs",
        "i",
        "iCs",
        "caps",
        "smallCaps",
        "strike",
        "dstrike",
        "outline",
        "shadow",
        "emboss",
        "imprint",
        "noProof",
        "snapToGrid",
        "vanish",
        "webHidden",
        "color",
        "spacing",
        "w",
        "kern",
        "position",
        "sz",
        "szCs",
        "highlight",
        "u",
        "effect",
        "bdr",
        "shd",
        "fitText",
        "vertAlign",
        "rtl",
        "cs",
        "em",
        "lang",
        "eastAsianLayout",
        "specVanish",
        "oMath",
        "rPrChange",
        "ins",
    ];
    if local == "rPrChange" {
        return u32::MAX;
    }
    ORDER
        .iter()
        .position(|&element| element == local)
        .map_or(u32::MAX - 1, |index| index as u32)
}

fn write_rpr(s: &mut String, p: &RunProps) {
    // Revision underline/strike are renderer cues, never direct OOXML
    // properties. Preserve a genuine direct value, but omit a cue that was
    // introduced only by an enclosing insertion/deletion wrapper.
    let underline = p.underline && !p.revision_cues.underline_added;
    let strike = p.strike && !p.revision_cues.strike_added;
    let has_any = p.bold
        || p.italic
        || underline
        || strike
        || p.code
        || p.caps
        || p.small_caps
        || p.vanish
        || p.rtl
        || p.vert_align != VertAlign::Baseline
        || p.color.is_some()
        || p.highlight.is_some()
        || p.size_half_pts.is_some()
        || p.font.is_some()
        || p.style_id.is_some()
        || !p.raw_props.is_empty()
        || p.property_change.is_some();
    if !has_any {
        return;
    }
    let mut parts: Vec<(u32, String)> = Vec::new();
    // Inline code carries the "Code" character style unless a more specific
    // character style is already set (which then implies the code styling).
    let rstyle = p
        .style_id
        .as_deref()
        .or(if p.code { Some("Code") } else { None });
    if let Some(st) = rstyle {
        let mut xml = String::from("<w:rStyle w:val=\"");
        esc_attr(st, &mut xml);
        xml.push_str("\"/>");
        parts.push((rpr_rank("rStyle"), xml));
    }
    if let Some(f) = &p.font {
        let mut xml = String::from("<w:rFonts w:ascii=\"");
        esc_attr(f, &mut xml);
        xml.push_str("\"/>");
        parts.push((rpr_rank("rFonts"), xml));
    }
    if p.bold {
        parts.push((rpr_rank("b"), "<w:b/>".to_string()));
    }
    if p.italic {
        parts.push((rpr_rank("i"), "<w:i/>".to_string()));
    }
    if p.caps {
        parts.push((rpr_rank("caps"), "<w:caps/>".to_string()));
    }
    if p.small_caps {
        parts.push((rpr_rank("smallCaps"), "<w:smallCaps/>".to_string()));
    }
    if strike {
        parts.push((rpr_rank("strike"), "<w:strike/>".to_string()));
    }
    if p.vanish {
        parts.push((rpr_rank("vanish"), "<w:vanish/>".to_string()));
    }
    if let Some(c) = &p.color {
        let mut xml = String::from("<w:color w:val=\"");
        esc_attr(c, &mut xml);
        xml.push_str("\"/>");
        parts.push((rpr_rank("color"), xml));
    }
    if let Some(sz) = p.size_half_pts {
        parts.push((rpr_rank("sz"), format!("<w:sz w:val=\"{sz}\"/>")));
    }
    if let Some(h) = &p.highlight {
        let mut xml = String::from("<w:highlight w:val=\"");
        esc_attr(h, &mut xml);
        xml.push_str("\"/>");
        parts.push((rpr_rank("highlight"), xml));
    }
    if underline {
        parts.push((rpr_rank("u"), "<w:u w:val=\"single\"/>".to_string()));
    }
    match p.vert_align {
        VertAlign::Baseline => {}
        VertAlign::Superscript => parts.push((
            rpr_rank("vertAlign"),
            "<w:vertAlign w:val=\"superscript\"/>".to_string(),
        )),
        VertAlign::Subscript => parts.push((
            rpr_rank("vertAlign"),
            "<w:vertAlign w:val=\"subscript\"/>".to_string(),
        )),
    }
    if p.rtl {
        parts.push((rpr_rank("rtl"), "<w:rtl/>".to_string()));
    }
    // Explicit-off toggles are retained as raw children so they remain distinct
    // from absent/style-derived values. A later direct edit to the same primary
    // property wins without emitting a contradictory duplicate.
    for raw in &p.raw_props {
        let name = local_name(raw);
        let superseded = matches!(name, "b" if p.bold)
            || matches!(name, "i" if p.italic)
            || matches!(name, "caps" if p.caps)
            || matches!(name, "smallCaps" if p.small_caps)
            || matches!(name, "strike" if strike)
            || matches!(name, "vanish" if p.vanish)
            || matches!(name, "color" if p.color.is_some())
            || matches!(name, "sz" if p.size_half_pts.is_some())
            || matches!(name, "highlight" if p.highlight.is_some())
            || matches!(name, "u" if underline)
            || matches!(name, "vertAlign" if p.vert_align != VertAlign::Baseline)
            || matches!(name, "rtl" if p.rtl)
            || matches!(name, "rPrChange" if p.property_change.is_some());
        if !superseded {
            parts.push((rpr_rank(name), raw.clone()));
        }
    }
    if let Some(change) = &p.property_change {
        parts.push((rpr_rank("rPrChange"), change.raw.clone()));
    }
    parts.sort_by_key(|(rank, _)| *rank);
    s.push_str("<w:rPr>");
    for (_, xml) in parts {
        s.push_str(&xml);
    }
    s.push_str("</w:rPr>");
}

fn write_table(s: &mut String, t: &Table) {
    s.push_str("<w:tbl");
    for (name, value) in &t.namespace_declarations {
        s.push(' ');
        s.push_str(name);
        s.push_str("=\"");
        esc_attr(value, s);
        s.push('"');
    }
    for (name, value) in &t.markup_compatibility_attributes {
        s.push(' ');
        s.push_str(name);
        s.push_str("=\"");
        esc_attr(value, s);
        s.push('"');
    }
    s.push('>');
    // tblPr is the first tbl child; preserved verbatim when present.
    if let Some(raw) = &t.raw_tblpr {
        s.push_str(&with_property_change(
            raw,
            "w:tblPr",
            t.property_change.as_ref(),
        ));
    } else if let Some(change) = &t.property_change {
        s.push_str(&format!("<w:tblPr>{}</w:tblPr>", change.raw));
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
                        let content_needs_close = content_stack
                            .pop()
                            .expect("validated row boundaries have a matching SDT open");
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
    ContentOpen(usize),
    ContentEmpty(usize),
    MissingContent(usize),
    Invalid,
}

/// Return the byte length ending after the last lexically complete XML token.
/// This is deliberately narrower than XML validation: it lets the structural
/// SDT checks below retain complete captured markup while dropping a final tag,
/// comment, CDATA section, or processing instruction cut off at EOF.
fn complete_xml_prefix_len(raw: &str) -> usize {
    let bytes = raw.as_bytes();
    let mut pos = 0;
    while pos < raw.len() {
        let Some(relative_start) = raw[pos..].find('<') else {
            return raw.len();
        };
        let start = pos + relative_start;
        let tail = &raw[start..];
        let end = if tail.starts_with("<!--") {
            tail.find("-->").map(|offset| start + offset + 3)
        } else if tail.starts_with("<![CDATA[") {
            tail.find("]]>").map(|offset| start + offset + 3)
        } else if tail.starts_with("<?") {
            tail.find("?>").map(|offset| start + offset + 2)
        } else {
            let mut quote = None;
            let mut end = None;
            for (offset, byte) in bytes[start + 1..].iter().copied().enumerate() {
                match byte {
                    b'\'' | b'"' if quote.is_none() => quote = Some(byte),
                    _ if quote == Some(byte) => quote = None,
                    b'>' if quote.is_none() => {
                        end = Some(start + offset + 2);
                        break;
                    }
                    _ => {}
                }
            }
            end
        };
        let Some(end) = end else {
            return start;
        };
        pos = end;
    }
    raw.len()
}

/// Inspect the captured SDT prefix without changing it. A valid row-control
/// prefix ends with either an open or self-closing direct `w:sdtContent` child.
fn sdt_open_shape(raw: &str) -> SdtOpenShape {
    let complete_len = complete_xml_prefix_len(raw);
    let mut parser = XmlParser::new(&raw[..complete_len]);
    if parser.next() != Event::Start || parser.name() != "w:sdt" {
        return SdtOpenShape::Invalid;
    }

    let mut stack = vec!["w:sdt".to_string()];
    let mut safe_end = parser.pos();
    loop {
        match parser.next() {
            Event::Start if stack.len() == 1 && parser.name() == "w:sdtContent" => {
                let tag = parser.raw_slice(parser.start_pos(), parser.pos());
                return if tag.trim_end().ends_with("/>") {
                    SdtOpenShape::ContentEmpty(parser.pos())
                } else {
                    SdtOpenShape::ContentOpen(parser.pos())
                };
            }
            Event::Start => stack.push(parser.name().to_string()),
            Event::End if stack.len() == 1 => {
                return SdtOpenShape::MissingContent(safe_end);
            }
            Event::End => {
                if stack.last().map(String::as_str) != Some(parser.name()) {
                    return SdtOpenShape::MissingContent(safe_end);
                }
                stack.pop();
                if stack.len() == 1 {
                    safe_end = parser.pos();
                }
            }
            Event::Eof => {
                if stack.len() == 1 {
                    safe_end = complete_len;
                }
                return SdtOpenShape::MissingContent(safe_end);
            }
            Event::Text if stack.len() == 1 => safe_end = parser.pos(),
            Event::Text => {}
        }
    }
}

/// Emit an SDT prefix and return whether its content element still needs a
/// closing tag. Valid captured prefixes are copied exactly; recovery adds only
/// structural tags absent from malformed/truncated source.
fn write_sdt_open(s: &mut String, raw: &str, is_empty_control: bool) -> bool {
    match sdt_open_shape(raw) {
        SdtOpenShape::ContentOpen(end) => {
            s.push_str(&raw[..end]);
            true
        }
        SdtOpenShape::ContentEmpty(end) => {
            let complete = &raw[..end];
            if is_empty_control {
                s.push_str(complete);
                false
            } else {
                // A self-closing content tag cannot own rows. This can only
                // arise in a manually-mutated model; open that exact captured
                // tag and let the matching close boundary finish it.
                let slash = complete
                    .rfind("/>")
                    .expect("ContentEmpty shape has a self-closing tag");
                s.push_str(&complete[..slash]);
                s.push('>');
                s.push_str(&complete[slash + 2..]);
                true
            }
        }
        SdtOpenShape::MissingContent(end) => {
            s.push_str(&raw[..end]);
            s.push_str("<w:sdtContent>");
            true
        }
        SdtOpenShape::Invalid => {
            s.push_str("<w:sdt><w:sdtContent>");
            true
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SdtCloseShape {
    safe_end: usize,
    content_close_ranges: Vec<(usize, usize)>,
    sdt_close_range: Option<(usize, usize)>,
}

fn sdt_close_shape(raw: &str) -> SdtCloseShape {
    let complete_len = complete_xml_prefix_len(raw);
    let mut parser = XmlParser::new(&raw[..complete_len]);
    let mut shape = SdtCloseShape {
        safe_end: 0,
        content_close_ranges: Vec::new(),
        sdt_close_range: None,
    };
    let mut stack = Vec::new();
    loop {
        let event_start = parser.pos();
        match parser.next() {
            Event::Start => {
                if stack.is_empty() {
                    shape.safe_end = parser.start_pos();
                }
                stack.push(parser.name().to_string());
            }
            Event::End if !stack.is_empty() => {
                if stack.last().map(String::as_str) != Some(parser.name()) {
                    return shape;
                }
                stack.pop();
                if stack.is_empty() {
                    shape.safe_end = parser.pos();
                }
            }
            Event::End if parser.name() == "w:sdtContent" => {
                shape.content_close_ranges.push((event_start, parser.pos()));
                shape.safe_end = parser.pos();
            }
            Event::End if parser.name() == "w:sdt" => {
                shape.sdt_close_range = Some((event_start, parser.pos()));
                shape.safe_end = parser.pos();
                return shape;
            }
            Event::End => return shape,
            Event::Text if stack.is_empty() => shape.safe_end = parser.pos(),
            Event::Text => {}
            Event::Eof => {
                if stack.is_empty() {
                    shape.safe_end = complete_len;
                }
                return shape;
            }
        }
    }
}

fn write_sdt_close(s: &mut String, raw: &str, content_needs_close: bool) {
    let shape = sdt_close_shape(raw);
    let expected_content_closes = if content_needs_close { 1 } else { 0 };
    if shape.content_close_ranges.len() == expected_content_closes
        && shape.sdt_close_range.is_some()
    {
        s.push_str(&raw[..shape.safe_end]);
        return;
    }

    // Contradictory/duplicate structural closes are unsafe to copy verbatim.
    // Retain every other complete suffix token, but emit exactly the closes
    // required by the normalized opening shape.
    if content_needs_close {
        s.push_str("</w:sdtContent>");
    }

    let mut structural_ranges = shape.content_close_ranges;
    if let Some(range) = shape.sdt_close_range {
        structural_ranges.push(range);
    }
    structural_ranges.sort_unstable_by_key(|(start, _)| *start);
    let mut copied_through = 0;
    for (start, end) in structural_ranges {
        if copied_through < start {
            s.push_str(&raw[copied_through..start]);
        }
        copied_through = copied_through.max(end);
    }
    if copied_through < shape.safe_end {
        s.push_str(&raw[copied_through..shape.safe_end]);
    }
    s.push_str("</w:sdt>");
}

fn write_row(s: &mut String, row: &Row) {
    s.push_str("<w:tr>");
    // trPr / tblPrEx precede the cells; preserved verbatim.
    let mut wrote_change = false;
    for raw in &row.raw_props {
        if local_name(raw) == "trPr" {
            s.push_str(&with_property_change(
                raw,
                "w:trPr",
                row.property_change.as_ref(),
            ));
            wrote_change = row.property_change.is_some();
        } else {
            s.push_str(raw);
        }
    }
    if !wrote_change {
        if let Some(change) = &row.property_change {
            s.push_str(&format!("<w:trPr>{}</w:trPr>", change.raw));
        }
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
        s.push_str(&with_property_change(
            raw,
            "w:tcPr",
            cell.property_change.as_ref(),
        ));
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
        if let Some(change) = &cell.property_change {
            s.push_str(&change.raw);
        }
        s.push_str("</w:tcPr>");
    } else if let Some(change) = &cell.property_change {
        s.push_str(&format!("<w:tcPr>{}</w:tcPr>", change.raw));
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
    fn generated_repeating_section_roundtrips_without_importing_root_namespaces() {
        let d = Document {
            body: vec![Block::Table(Table {
                row_boundaries: vec![
                    TableRowBoundary::sdt_open(
                        0,
                        "<w:sdt><w:sdtPr><w15:repeatingSection/></w:sdtPr><w:sdtContent>",
                    ),
                    TableRowBoundary::sdt_close(0, "</w:sdtContent></w:sdt>"),
                ],
                ..Default::default()
            })],
        };

        assert_eq!(roundtrip(&d, &Relationships::default()), d);
    }

    #[test]
    fn visible_w15_text_does_not_add_extension_namespaces() {
        let d = Document {
            body: vec![para(
                ParProps::default(),
                vec![run("visible w15:text", RunProps::default())],
            )],
        };

        let xml = document_to_xml(&d);
        assert!(!xml.contains("xmlns:w15="));
        assert!(!xml.contains("mc:Ignorable="));
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
                property_change: None,
            }],
            namespace_declarations: vec![],
            markup_compatibility_attributes: vec![],
            row_boundaries: vec![],
            raw_tblpr: Some(
                "<w:tblPr><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\"/></w:tblBorders></w:tblPr>"
                    .to_string(),
            ),
            property_change: None,
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
            rtl: true,
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
            ..Hyperlink::default()
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
        let children = format!(
            "{tbl_pr}{grid}{outer_open}{}{inner_open}{}{close}{close}{empty_open}{close}{group_open}{}{unknown_child}{}{close}",
            row_xml("Привет 🌍"),
            row_xml("東京"),
            row_xml("α"),
            row_xml("β")
        );
        let source = format!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:mc=\"{MC_NS}\" xmlns:w15=\"{W15_NS}\"><w:body><w:tbl>{children}</w:tbl></w:body></w:document>"
        );

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
    fn truncated_control_before_content_is_normalized_to_balanced_xml() {
        let source = table_xml("<w:sdt><w:sdtPr/>");
        let parsed = parse_document_xml(&source, &Relationships::default());
        let Block::Table(table) = &parsed.body[0] else {
            panic!("expected table");
        };
        assert_eq!(table.row_boundaries.len(), 2);
        assert!(table.validate_row_boundaries().is_ok());

        let saved = document_to_xml(&parsed);
        assert!(saved.contains("<w:sdt><w:sdtPr/><w:sdtContent></w:sdtContent></w:sdt></w:tbl>"));
        assert_eq!(saved.matches("<w:sdt>").count(), 1);
        assert_eq!(saved.matches("</w:sdt>").count(), 1);
        assert_eq!(saved.matches("<w:sdtContent>").count(), 1);
        assert_eq!(saved.matches("</w:sdtContent>").count(), 1);

        let reparsed = parse_document_xml(&saved, &Relationships::default());
        let Block::Table(table) = &reparsed.body[0] else {
            panic!("expected table");
        };
        assert!(table.validate_row_boundaries().is_ok());
    }

    #[test]
    fn lexically_truncated_boundary_tags_are_removed_before_balancing() {
        let partial_open = "<w:document><w:body><w:tbl><w:sdt><w:sdtPr";
        let parsed = parse_document_xml(partial_open, &Relationships::default());
        let saved = document_to_xml(&parsed);
        assert!(saved.contains("<w:tbl><w:sdt><w:sdtContent></w:sdtContent></w:sdt></w:tbl>"));
        assert!(!saved.contains("<w:sdtPr<"));

        let mismatched_open = "<w:document><w:body><w:tbl><w:sdt><w:sdtPr></w:future>";
        let parsed = parse_document_xml(mismatched_open, &Relationships::default());
        let saved = document_to_xml(&parsed);
        assert!(saved.contains("<w:tbl><w:sdt><w:sdtContent></w:sdtContent></w:sdt></w:tbl>"));
        assert!(!saved.contains("<w:sdtPr></w:future>"));

        let mismatched_closed = table_xml("<w:sdt><w:sdtPr></w:future></w:sdt>");
        let parsed = parse_document_xml(&mismatched_closed, &Relationships::default());
        let saved = document_to_xml(&parsed);
        assert!(saved.contains("<w:tbl><w:sdt><w:sdtContent></w:sdtContent></w:sdt></w:tbl>"));
        assert!(!saved.contains("<w:sdtPr></w:future>"));

        let open = "<w:sdt><w:sdtPr><w:alias w:val=\"tail\"/></w:sdtPr><w:sdtContent>";
        let partial_close = format!(
            "<w:document><w:body><w:tbl>{open}{}</w:sdtContent><w:future",
            row_xml("visible")
        );
        let parsed = parse_document_xml(&partial_close, &Relationships::default());
        let saved = document_to_xml(&parsed);
        assert!(!saved.contains("<w:future"));
        assert!(
            saved
                .contains("visible</w:t></w:r></w:p></w:tc></w:tr></w:sdtContent></w:sdt></w:tbl>")
        );
        assert_eq!(
            parse_document_xml(&saved, &Relationships::default()).plain_text(),
            "visible\n"
        );
    }

    #[test]
    fn truncated_unknown_table_children_are_not_emitted_as_raw_xml() {
        let inside_control = format!(
            "<w:document><w:body><w:tbl><w:sdt><w:sdtContent>{}<w:customXml>",
            row_xml("visible")
        );
        let parsed = parse_document_xml(&inside_control, &Relationships::default());
        let saved = document_to_xml(&parsed);
        assert!(!saved.contains("<w:customXml>"));
        assert!(saved.contains("</w:sdtContent></w:sdt></w:tbl>"));
        assert_eq!(
            parse_document_xml(&saved, &Relationships::default()).plain_text(),
            "visible\n"
        );

        let table_child = "<w:document><w:body><w:tbl><w:customXml>";
        let saved = document_to_xml(&parse_document_xml(table_child, &Relationships::default()));
        assert!(!saved.contains("<w:customXml>"));
        assert!(saved.contains("<w:tbl></w:tbl>"));
    }

    #[test]
    fn nested_sdt_in_truncated_tail_does_not_mask_missing_outer_close() {
        let open = "<w:sdt><w:sdtPr><w:alias w:val=\"outer\"/></w:sdtPr><w:sdtContent>";
        let source = format!(
            "<w:document><w:body><w:tbl>{open}{}</w:sdtContent><w:customXml><w:sdt/></w:customXml>",
            row_xml("visible")
        );
        let parsed = parse_document_xml(&source, &Relationships::default());
        let saved = document_to_xml(&parsed);
        assert!(
            saved.contains("</w:sdtContent><w:customXml><w:sdt/></w:customXml></w:sdt></w:tbl>")
        );
        assert_eq!(saved.matches("</w:sdt>").count(), 1);

        let reparsed = parse_document_xml(&saved, &Relationships::default());
        let Block::Table(table) = &reparsed.body[0] else {
            panic!("expected table");
        };
        assert!(table.validate_row_boundaries().is_ok());
        assert_eq!(reparsed.plain_text(), "visible\n");
    }

    #[test]
    fn row_control_keeps_namespaces_inherited_from_body_and_table() {
        let open = "<w:sdt><w:sdtPr><mc:AlternateContent><mc:Choice Requires=\"w15\"><w:alias w:val=\"choice\"/></mc:Choice><mc:Fallback/></mc:AlternateContent><ux:bodyProperty/><tv:tableProperty/></w:sdtPr><w:sdtContent>";
        let source = format!(
            "<w:document xmlns:w=\"{W_NS}\"><w:body xmlns:mc=\"{MC_NS}\" xmlns:w15=\"{W15_NS}\" xmlns:ux=\"urn:body\" mc:Ignorable=\"ux w15\"><w:tbl xmlns:tv=\"urn:table\" mc:Ignorable=\"tv\" mc:PreserveElements=\"tv:tableProperty\">{open}{}</w:sdtContent></w:sdt></w:tbl></w:body></w:document>",
            row_xml("visible")
        );
        let parsed = parse_document_xml(&source, &Relationships::default());
        let Block::Table(table) = &parsed.body[0] else {
            panic!("expected table");
        };
        assert_eq!(
            table.namespace_declarations,
            vec![
                ("xmlns:mc".to_string(), MC_NS.to_string()),
                ("xmlns:w15".to_string(), W15_NS.to_string()),
                ("xmlns:ux".to_string(), "urn:body".to_string()),
                ("xmlns:tv".to_string(), "urn:table".to_string()),
            ]
        );
        assert_eq!(
            table.markup_compatibility_attributes,
            vec![
                ("mc:Ignorable".to_string(), "ux w15 tv".to_string()),
                (
                    "mc:PreserveElements".to_string(),
                    "tv:tableProperty".to_string()
                ),
            ]
        );

        let saved = document_to_xml(&parsed);
        assert!(saved.contains(&format!(
            "<w:tbl xmlns:mc=\"{MC_NS}\" xmlns:w15=\"{W15_NS}\" xmlns:ux=\"urn:body\" xmlns:tv=\"urn:table\" mc:Ignorable=\"ux w15 tv\" mc:PreserveElements=\"tv:tableProperty\">"
        )));
        assert!(saved.contains(open), "captured control properties changed");
        assert_eq!(
            parse_document_xml(&saved, &Relationships::default()),
            parsed
        );
    }

    #[test]
    fn row_control_keeps_document_scoped_markup_compatibility_attributes() {
        let open = "<w:sdt><w:sdtPr><ux:property/></w:sdtPr><w:sdtContent>";
        let source = format!(
            "<w:document xmlns:w=\"{W_NS}\" xmlns:mc=\"{MC_NS}\" xmlns:ux=\"urn:document-extension\" mc:Ignorable=\"ux\" mc:PreserveElements=\"ux:property\"><w:body><w:tbl>{open}{}</w:sdtContent></w:sdt></w:tbl></w:body></w:document>",
            row_xml("visible")
        );
        let parsed = parse_document_xml(&source, &Relationships::default());
        let Block::Table(table) = &parsed.body[0] else {
            panic!("expected table");
        };
        assert_eq!(
            table.markup_compatibility_attributes,
            vec![
                ("mc:Ignorable".to_string(), "ux".to_string()),
                ("mc:PreserveElements".to_string(), "ux:property".to_string()),
            ]
        );

        let saved = document_to_xml(&parsed);
        let root_start = saved.find("<w:document").expect("document root");
        let root_end = root_start
            + saved[root_start..]
                .find('>')
                .expect("document root terminator");
        assert!(saved[root_start..root_end].contains(&format!("xmlns:mc=\"{MC_NS}\"")));
        assert!(saved.contains(
            "<w:tbl xmlns:ux=\"urn:document-extension\" mc:Ignorable=\"ux\" mc:PreserveElements=\"ux:property\">"
        ));
        assert!(saved.contains(open));
        assert_eq!(
            parse_document_xml(&saved, &Relationships::default()),
            parsed
        );
    }

    #[test]
    fn visible_row_after_premature_content_close_is_repaired_inside_wrapper() {
        let open = "<w:sdt><w:sdtPr><w:alias w:val=\"recovered\"/></w:sdtPr><w:sdtContent>";
        let metadata = "<w:customXml w:uri=\"urn:after-close\"/>";
        let source = table_xml(&format!(
            "{open}{}</w:sdtContent>{metadata}{}</w:sdt>",
            row_xml("before"),
            row_xml("recovered")
        ));

        let parsed = parse_document_xml(&source, &Relationships::default());
        assert_eq!(parsed.plain_text(), "before\nrecovered\n");

        let saved = document_to_xml(&parsed);
        assert_eq!(saved.matches("<w:sdtContent>").count(), 1);
        assert_eq!(saved.matches("</w:sdtContent>").count(), 1);
        assert_eq!(saved.matches("</w:sdt>").count(), 1);
        assert!(saved.contains(metadata), "intervening metadata was dropped");
        assert!(
            saved.find(metadata).unwrap() < saved.find("recovered</w:t>").unwrap(),
            "intervening metadata moved after the recovered row"
        );

        let reparsed = parse_document_xml(&saved, &Relationships::default());
        let Block::Table(table) = &reparsed.body[0] else {
            panic!("expected table");
        };
        assert_eq!(table.row_control_owners(), Ok(vec![vec![0], vec![0]]));
        assert_eq!(reparsed.plain_text(), "before\nrecovered\n");

        let nested_open = "<w:sdt><w:sdtPr><w:alias w:val=\"nested\"/></w:sdtPr><w:sdtContent>";
        let nested_source = table_xml(&format!(
            "{open}{}</w:sdtContent>{nested_open}{}{}</w:sdt></w:sdt>",
            row_xml("before"),
            row_xml("nested recovered"),
            "</w:sdtContent>"
        ));
        let nested = parse_document_xml(&nested_source, &Relationships::default());
        let nested_saved = document_to_xml(&nested);
        assert_eq!(nested_saved.matches("<w:sdtContent>").count(), 2);
        assert_eq!(nested_saved.matches("</w:sdtContent>").count(), 2);
        assert_eq!(nested_saved.matches("</w:sdt>").count(), 2);
        let nested_reparsed = parse_document_xml(&nested_saved, &Relationships::default());
        let Block::Table(table) = &nested_reparsed.body[0] else {
            panic!("expected table");
        };
        assert_eq!(table.row_control_owners(), Ok(vec![vec![0], vec![0, 1]]));
        assert_eq!(nested_reparsed.plain_text(), "before\nnested recovered\n");
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

    #[test]
    fn malformed_rows_after_self_closing_content_are_recovered_without_panic() {
        let source = table_xml(&format!(
            "<w:sdt><w:sdtContent/>{}</w:sdt>",
            row_xml("direct")
        ));
        let parsed = parse_document_xml(&source, &Relationships::default());
        assert_eq!(parsed.plain_text(), "direct\n");
        let saved = document_to_xml(&parsed);
        assert!(saved.contains("<w:sdtContent><w:tr>"));
        assert!(saved.contains("</w:tr></w:sdtContent></w:sdt>"));
        let reparsed = parse_document_xml(&saved, &Relationships::default());
        let Block::Table(table) = &reparsed.body[0] else {
            panic!("expected table");
        };
        assert_eq!(table.row_control_owners(), Ok(vec![vec![0]]));

        let nested_source = table_xml(&format!(
            "<w:sdt><w:sdtContent><w:sdt><w:sdtContent/>{}</w:sdt></w:sdtContent></w:sdt>",
            row_xml("nested")
        ));
        let nested = parse_document_xml(&nested_source, &Relationships::default());
        assert_eq!(nested.plain_text(), "nested\n");
        let nested_saved = document_to_xml(&nested);
        assert_eq!(nested_saved.matches("<w:sdtContent>").count(), 2);
        assert_eq!(nested_saved.matches("</w:sdtContent>").count(), 2);
        let nested_reparsed = parse_document_xml(&nested_saved, &Relationships::default());
        let Block::Table(table) = &nested_reparsed.body[0] else {
            panic!("expected table");
        };
        assert_eq!(table.row_control_owners(), Ok(vec![vec![0, 1]]));
    }

    #[test]
    fn contradictory_and_duplicate_content_closes_are_normalized() {
        let self_closing = table_xml("<w:sdt><w:sdtContent/></w:sdtContent></w:sdt>");
        let parsed = parse_document_xml(&self_closing, &Relationships::default());
        let saved = document_to_xml(&parsed);
        assert!(saved.contains("<w:sdt><w:sdtContent/></w:sdt>"));
        assert_eq!(saved.matches("</w:sdtContent>").count(), 0);
        let reparsed = parse_document_xml(&saved, &Relationships::default());
        let Block::Table(table) = &reparsed.body[0] else {
            panic!("expected table");
        };
        assert!(table.validate_row_boundaries().is_ok());

        let duplicate = table_xml("<w:sdt><w:sdtContent></w:sdtContent></w:sdtContent></w:sdt>");
        let parsed = parse_document_xml(&duplicate, &Relationships::default());
        let saved = document_to_xml(&parsed);
        assert!(saved.contains("<w:sdt><w:sdtContent></w:sdtContent></w:sdt>"));
        assert_eq!(saved.matches("</w:sdtContent>").count(), 1);
        let reparsed = parse_document_xml(&saved, &Relationships::default());
        let Block::Table(table) = &reparsed.body[0] else {
            panic!("expected table");
        };
        assert!(table.validate_row_boundaries().is_ok());
    }
}
