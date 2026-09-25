//! The **rich document model** the editable-HTML page renders as real DOM
//! (`docx_doc`), in place of the character grid [`crate::bridge::Session::view_json`]
//! paints for the VS Code webview.
//!
//! Every paragraph carries its editor path (`"0"`, or `"3.1.0.0"` inside a
//! table cell) and its caret length, and every inline becomes a *segment* with
//! its editor offset `o` and width `w`:
//!
//! - text runs (`"k":"t"`) are editable; `w` is their character count, and the
//!   page converts between the browser's UTF-16 offsets and these scalar
//!   offsets;
//! - everything else is **atomic** (`contenteditable=false` on the page) with
//!   the width the editor gives it — one for a tab or break, zero for fields,
//!   tracked changes, drawings and other anchors the editor cannot enter.
//!
//! Widths come from [`docxcore::editor::inline_len`] (and paragraph lengths
//! from `para_text_len`), the editor's own functions, so the page and the
//! editor can never disagree about where an offset is. Formatting is the *effective*
//! formatting (styles resolved), since that is what the page must draw.

use std::collections::HashMap;

use docxcore::editor::{inline_len, para_text_len};
use docxcore::load::Relationships;
use docxcore::model::{
    Align, Block, BreakKind, Document, Inline, PageGeom, Paragraph, RevisionKind, RunProps, Table,
    VMerge, VertAlign,
};
use docxcore::styles::StyleSheet;

use crate::json;

/// What the renderer needs besides the document.
pub struct Ctx<'a> {
    pub styles: &'a StyleSheet,
    pub markers: &'a HashMap<Vec<usize>, String>,
    pub rels: &'a Relationships,
}

/// `[3, 1, 0, 0]` → `"3.1.0.0"`.
pub fn path_str(path: &[usize]) -> String {
    path.iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// `"3.1.0.0"` → `[3, 1, 0, 0]`; `None` for anything else (empty included).
pub fn parse_path(s: &str) -> Option<Vec<usize>> {
    if s.is_empty() {
        return None;
    }
    s.split('.').map(|p| p.parse().ok()).collect()
}

/// The body as JSON: `{"page":{…},"blocks":[…]}`.
pub fn doc_json(doc: &Document, page: &PageGeom, ctx: &Ctx<'_>) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str(&format!(
        "{{\"page\":{{\"w\":{},\"h\":{},\"top\":{},\"right\":{},\"bottom\":{},\"left\":{}}},\"blocks\":",
        page.w, page.h, page.mt, page.mr, page.mb, page.ml
    ));
    let mut prefix = Vec::new();
    push_blocks(&mut out, &doc.body, &mut prefix, ctx);
    out.push('}');
    out
}

fn push_blocks(out: &mut String, blocks: &[Block], prefix: &mut Vec<usize>, ctx: &Ctx<'_>) {
    out.push('[');
    let mut first = true;
    for (i, block) in blocks.iter().enumerate() {
        prefix.push(i);
        match block {
            Block::Paragraph(p) => {
                if !first {
                    out.push(',');
                }
                first = false;
                push_paragraph(out, p, prefix, ctx);
            }
            Block::Table(t) => {
                if !first {
                    out.push(',');
                }
                first = false;
                push_table(out, t, prefix, ctx);
            }
            Block::SectionProperties(_) | Block::Raw(_) => {}
        }
        prefix.pop();
    }
    out.push(']');
}

fn push_table(out: &mut String, t: &Table, prefix: &mut Vec<usize>, ctx: &Ctx<'_>) {
    out.push_str("{\"t\":\"tbl\",\"grid\":[");
    out.push_str(
        &t.grid
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push_str("],\"rows\":[");
    for (ri, row) in t.rows.iter().enumerate() {
        if ri > 0 {
            out.push(',');
        }
        out.push('[');
        for (ci, cell) in row.cells.iter().enumerate() {
            if ci > 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"span\":{}", cell.grid_span.max(1)));
            match cell.v_merge {
                VMerge::Restart => out.push_str(",\"vm\":\"restart\""),
                VMerge::Continue => out.push_str(",\"vm\":\"continue\""),
                VMerge::None => {}
            }
            out.push_str(",\"blocks\":");
            prefix.push(ri);
            prefix.push(ci);
            push_blocks(out, &cell.blocks, prefix, ctx);
            prefix.pop();
            prefix.pop();
            out.push('}');
        }
        out.push(']');
    }
    out.push_str("]}");
}

pub fn align_name(a: Align) -> &'static str {
    match a {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
        Align::Justify => "justify",
    }
}

fn push_paragraph(out: &mut String, p: &Paragraph, path: &[usize], ctx: &Ctx<'_>) {
    let props = &p.props;
    let len = para_text_len(p);
    out.push_str("{\"t\":\"p\",\"p\":");
    json::push_str(out, &path_str(path));
    out.push_str(&format!(",\"len\":{len}"));
    if let Some(s) = &props.style_id {
        out.push_str(",\"style\":");
        json::push_str(out, s);
    }
    if let Some(h) = props.heading_level {
        out.push_str(&format!(",\"head\":{h}"));
    }
    let align = ctx
        .styles
        .effective_align(props.style_id.as_deref(), props.align);
    if align != Align::Left {
        out.push_str(",\"align\":\"");
        out.push_str(align_name(align));
        out.push('"');
    }
    if props.indent != 0 || props.first_line != 0 || props.indent_right != 0 {
        out.push_str(&format!(
            ",\"ind\":{{\"left\":{},\"right\":{},\"first\":{}}}",
            props.indent, props.indent_right, props.first_line
        ));
    }
    let sp = &props.spacing;
    if !sp.is_empty() {
        out.push_str(",\"spacing\":{");
        let mut parts = Vec::new();
        if let Some(v) = sp.before {
            parts.push(format!("\"before\":{v}"));
        }
        if let Some(v) = sp.after {
            parts.push(format!("\"after\":{v}"));
        }
        if let Some(v) = sp.line {
            parts.push(format!("\"line\":{v}"));
        }
        if let Some(r) = &sp.line_rule {
            parts.push(format!("\"rule\":{}", json::quote(r)));
        }
        out.push_str(&parts.join(","));
        out.push('}');
    }
    if let Some(label) = ctx.markers.get(path) {
        out.push_str(",\"list\":");
        json::push_str(out, label);
        out.push_str(&format!(",\"level\":{}", props.ilvl.max(0)));
    }
    if props.rtl {
        out.push_str(",\"rtl\":true");
    }
    let borders = ctx
        .styles
        .effective_borders(props.style_id.as_deref(), props.borders);
    if borders.bottom.is_some() {
        out.push_str(",\"borderBottom\":true");
    }
    if borders.top.is_some() {
        out.push_str(",\"borderTop\":true");
    }
    out.push_str(",\"segs\":[");
    let mut w = SegWriter {
        out,
        first: true,
        offset: 0,
        para_style: props.style_id.as_deref(),
        ctx,
    };
    for inline in &p.content {
        w.inline(inline);
    }
    debug_assert_eq!(w.offset, len, "segment widths must sum to the caret length");
    out.push_str("]}");
}

struct SegWriter<'a, 'o> {
    out: &'o mut String,
    first: bool,
    offset: usize,
    para_style: Option<&'a str>,
    ctx: &'a Ctx<'a>,
}

impl SegWriter<'_, '_> {
    fn open(&mut self, kind: &str, width: usize) {
        if !self.first {
            self.out.push(',');
        }
        self.first = false;
        self.out.push_str(&format!(
            "{{\"k\":\"{kind}\",\"o\":{},\"w\":{width}",
            self.offset
        ));
        self.offset += width;
    }

    fn text_field(&mut self, text: &str) {
        self.out.push_str(",\"x\":");
        json::push_str(self.out, text);
    }

    fn run(&mut self, text: &str, direct: &RunProps, href: Option<&str>) {
        let width = text.chars().count();
        if width == 0 {
            return;
        }
        self.open("t", width);
        self.text_field(text);
        let eff =
            self.ctx
                .styles
                .effective_run(self.para_style, direct.style_id.as_deref(), direct);
        push_props(self.out, &eff, direct);
        if let Some(h) = href {
            self.out.push_str(",\"href\":");
            json::push_str(self.out, h);
        }
        self.out.push('}');
    }

    /// An atomic segment with display text.
    fn atom(&mut self, kind: &str, width: usize, text: &str, extra: &str) {
        self.open(kind, width);
        if !text.is_empty() {
            self.text_field(text);
        }
        self.out.push_str(extra);
        self.out.push('}');
    }

    fn inline(&mut self, inline: &Inline) {
        let width = inline_len(inline);
        match inline {
            Inline::Run(r) => self.run(&r.text, &r.props, None),
            Inline::Hyperlink(h) => {
                let href = h
                    .target
                    .clone()
                    .or_else(|| h.anchor.as_ref().map(|a| format!("#{a}")));
                for r in &h.runs {
                    self.run(&r.text, &r.props, href.as_deref());
                }
                // Links carrying revisions or other markup keep that content
                // outside the editor's offsets: show it, uneditable.
                if !h.content.is_empty() {
                    let text: String = h.content.iter().map(Inline::text).collect();
                    self.atom("link", 0, &text, "");
                }
            }
            Inline::Tab(_) => self.atom("tab", width, "", ""),
            Inline::Break(kind) => {
                let k = match kind {
                    BreakKind::Line => "br",
                    BreakKind::Page => "pagebreak",
                    BreakKind::Column => "colbreak",
                };
                self.atom(k, width, "", "");
            }
            Inline::Revision {
                kind,
                metadata,
                content,
                ..
            } => {
                let text: String = content.iter().map(Inline::text).collect();
                let mut extra = String::from(",\"rev\":");
                extra.push_str(match kind {
                    RevisionKind::Insert => "\"ins\"",
                    RevisionKind::Delete => "\"del\"",
                });
                if let Some(a) = &metadata.author {
                    extra.push_str(",\"author\":");
                    json::push_str(&mut extra, a);
                }
                self.atom("rev", width, &text, &extra);
            }
            Inline::Field { text, .. } => self.atom("field", width, text, ""),
            Inline::Equation { text, .. } => self.atom("eq", width, text, ""),
            Inline::SmartArt { text, .. } => self.atom("art", width, &text.join(" · "), ""),
            Inline::Chart { chart, .. } => {
                let title = chart.title.clone().unwrap_or_else(|| "Chart".into());
                self.atom("chart", width, &title, "");
            }
            Inline::TextBox { blocks, .. } => {
                let text = blocks
                    .iter()
                    .map(Block::plain_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.atom("box", width, &text, "");
            }
            Inline::FootnoteRef { id, endnote, .. } => {
                let extra = if *endnote { ",\"endnote\":true" } else { "" };
                self.atom("note", width, &id.to_string(), extra);
            }
            Inline::UnsupportedRevision { .. } => {}
            Inline::Raw(xml) => {
                if let Some(extra) = image_extra(xml, self.ctx.rels) {
                    self.atom("img", width, "", &extra);
                } else if let Some(id) = attr(xml, "w:commentReference", "w:id") {
                    let mut extra = String::from(",\"id\":");
                    json::push_str(&mut extra, &id);
                    self.atom("comment", width, "", &extra);
                }
            }
        }
    }
}

fn push_props(out: &mut String, eff: &RunProps, direct: &RunProps) {
    let flag = |out: &mut String, on: bool, key: &str| {
        if on {
            out.push_str(&format!(",\"{key}\":true"));
        }
    };
    flag(out, eff.bold, "b");
    flag(out, eff.italic, "i");
    flag(out, eff.underline, "u");
    flag(out, eff.strike, "s");
    flag(out, eff.code, "code");
    flag(out, eff.caps, "caps");
    flag(out, eff.small_caps, "smallCaps");
    flag(out, eff.vanish, "hidden");
    flag(out, eff.rtl, "rtl");
    // Review cues baked into the run by an enclosing revision.
    flag(out, direct.revision_cues.insertions > 0, "ins");
    flag(out, direct.revision_cues.deletions > 0, "del");
    match eff.vert_align {
        VertAlign::Superscript => out.push_str(",\"va\":\"sup\""),
        VertAlign::Subscript => out.push_str(",\"va\":\"sub\""),
        VertAlign::Baseline => {}
    }
    if let Some(sz) = eff.size_half_pts {
        out.push_str(&format!(",\"sz\":{sz}"));
    }
    if let Some(c) = &eff.color {
        out.push_str(",\"color\":");
        json::push_str(out, c);
    }
    if let Some(h) = &eff.highlight {
        out.push_str(",\"hl\":");
        json::push_str(out, h);
    }
    if let Some(f) = &eff.font {
        out.push_str(",\"font\":");
        json::push_str(out, f);
    }
}

/// `,"rid":…,"cx":…,"cy":…` for a drawing/VML picture run whose media the
/// relationships resolve; `None` for any other raw XML.
fn image_extra(xml: &str, rels: &Relationships) -> Option<String> {
    let rid = attr_value(xml, "r:embed=").or_else(|| attr_value(xml, "r:id="))?;
    rels.target(&rid)?;
    let mut extra = String::from(",\"rid\":");
    json::push_str(&mut extra, &rid);
    if let Some(ext) = xml.find("<wp:extent") {
        let el = &xml[ext..];
        if let (Some(cx), Some(cy)) = (attr_value(el, "cx="), attr_value(el, "cy=")) {
            if let (Ok(cx), Ok(cy)) = (cx.parse::<u64>(), cy.parse::<u64>()) {
                extra.push_str(&format!(",\"cx\":{cx},\"cy\":{cy}"));
            }
        }
    }
    Some(extra)
}

/// The value of `name` on the first `<tag …>` element in `xml`.
fn attr(xml: &str, tag: &str, name: &str) -> Option<String> {
    let at = xml.find(&format!("<{tag}"))?;
    let end = xml[at..].find('>')? + at;
    attr_value(&xml[at..end], &format!("{name}="))
}

/// The quoted value after `key` (which ends in `=`).
fn attr_value(s: &str, key: &str) -> Option<String> {
    let i = s.find(key)? + key.len();
    let rest = &s[i..];
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let rest = &rest[1..];
    Some(rest[..rest.find(q)?].to_string())
}
