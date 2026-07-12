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
//! headings, lists, hyperlink annotations, and pagination. Tables are flattened
//! to text rows; real bordered tables and images in PDF come in a later phase.

use std::rc::Rc;

use crate::model::*;
use crate::styles::StyleSheet;

#[derive(Debug, Clone)]
pub struct PdfOptions {
    pub page_width: f32,
    pub page_height: f32,
    pub margin: f32,
    pub base_font_size: f32,
    /// Resolved stylesheet for effective run formatting.
    pub styles: Rc<StyleSheet>,
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
        }
    }
}

/// Render a document to PDF bytes.
pub fn to_pdf(doc: &Document, opts: &PdfOptions) -> Vec<u8> {
    let pages = Layout::new(opts).run(doc);
    write_pdf(&pages, opts)
}

// ---- layout ----

#[derive(Clone)]
struct PCell {
    ch: char,
    font: u8, // 0=regular 1=bold 2=oblique 3=bold-oblique
    color: (f32, f32, f32),
    underline: bool,
    strike: bool,
    link: Option<Rc<str>>,
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
}

#[derive(Default, Clone)]
struct Page {
    frags: Vec<Frag>,
    links: Vec<((f32, f32, f32, f32), String)>,
}

struct Layout<'a> {
    opts: &'a PdfOptions,
    pages: Vec<Page>,
    cur: Page,
    y: f32,
}

fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn run_color(p: &RunProps) -> (f32, f32, f32) {
    match p.color.as_deref().and_then(parse_hex) {
        Some((r, g, b)) => (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0),
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

impl<'a> Layout<'a> {
    fn new(opts: &'a PdfOptions) -> Self {
        Layout {
            opts,
            pages: Vec::new(),
            cur: Page::default(),
            y: opts.page_height - opts.margin,
        }
    }

    fn content_width(&self) -> f32 {
        self.opts.page_width - 2.0 * self.opts.margin
    }

    fn run(mut self, doc: &Document) -> Vec<Page> {
        for block in &doc.body {
            match block {
                Block::Paragraph(p) => self.paragraph(p),
                Block::Table(t) => self.table(t),
                Block::Raw(_) => {}
            }
        }
        self.pages.push(std::mem::take(&mut self.cur));
        self.pages
    }

    fn heading_size(&self, p: &Paragraph) -> f32 {
        let base = self.opts.base_font_size;
        match p.props.heading_level {
            Some(1) => base * 1.8,
            Some(2) => base * 1.5,
            Some(3) => base * 1.3,
            Some(4) => base * 1.15,
            Some(_) => base * 1.05,
            None => base,
        }
    }

    fn paragraph(&mut self, p: &Paragraph) {
        let size = self.heading_size(p);
        let mut segs = flatten_segments(p, p.props.heading_level.is_some(), &self.opts.styles);
        if p.props.num_id.is_some() {
            let ind = (p.props.ilvl.max(0) as usize) * 2;
            let mut bullet: Vec<PCell> = Vec::new();
            for _ in 0..ind {
                bullet.push(plain_cell(' '));
            }
            bullet.push(plain_cell('•'));
            bullet.push(plain_cell(' '));
            bullet.extend(std::mem::take(&mut segs[0]));
            segs[0] = bullet;
        }
        self.emit_segments(segs, size, p.props.align);
    }

    fn table(&mut self, t: &Table) {
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
            let seg: Vec<PCell> = text.chars().map(plain_cell).collect();
            self.emit_segments(vec![seg], self.opts.base_font_size, Align::Left);
        }
    }

    fn emit_segments(&mut self, segs: Vec<Vec<PCell>>, size: f32, align: Align) {
        let line_height = size * 1.35;
        let advance = 0.6 * size;
        let cw = self.content_width();
        let cpl = ((cw / advance).floor() as usize).max(1);

        let mut lines: Vec<Vec<PCell>> = Vec::new();
        for seg in &segs {
            lines.extend(wrap_cells(seg, cpl));
        }
        if lines.is_empty() {
            lines.push(Vec::new());
        }

        for line in lines {
            self.newline(line_height);
            let line_w = line.len() as f32 * advance;
            let off = match align {
                Align::Center => (cw - line_w).max(0.0) / 2.0,
                Align::Right => (cw - line_w).max(0.0),
                _ => 0.0,
            };
            let x0 = self.opts.margin + off;

            let mut i = 0;
            while i < line.len() {
                let start = i;
                let c0 = line[start].clone();
                while i < line.len() && same_style(&line[i], &c0) {
                    i += 1;
                }
                let text: String = line[start..i].iter().map(|c| c.ch).collect();
                let fx = x0 + start as f32 * advance;
                let span_w = (i - start) as f32 * advance;
                if let Some(link) = &c0.link {
                    self.cur.links.push((
                        (fx, self.y - 2.0, fx + span_w, self.y + size * 0.85),
                        link.to_string(),
                    ));
                }
                self.cur.frags.push(Frag {
                    x: fx,
                    y: self.y,
                    text,
                    size,
                    font: c0.font,
                    color: c0.color,
                    underline: c0.underline,
                    strike: c0.strike,
                });
            }
        }
        // paragraph spacing
        self.y -= size * 0.4;
    }

    fn newline(&mut self, line_height: f32) {
        self.y -= line_height;
        if self.y < self.opts.margin {
            self.pages.push(std::mem::take(&mut self.cur));
            self.y = self.opts.page_height - self.opts.margin - line_height;
        }
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
    }
}

fn same_style(a: &PCell, b: &PCell) -> bool {
    a.font == b.font
        && a.color == b.color
        && a.underline == b.underline
        && a.strike == b.strike
        && a.link.as_deref() == b.link.as_deref()
}

fn flatten_segments(p: &Paragraph, heading: bool, styles: &StyleSheet) -> Vec<Vec<PCell>> {
    let pstyle = p.props.style_id.as_deref();
    let mut segs: Vec<Vec<PCell>> = vec![Vec::new()];
    for item in &p.content {
        match item {
            Inline::Run(r) => {
                let eff = styles.effective_run(pstyle, r.props.style_id.as_deref(), &r.props);
                let font = font_index(eff.bold || heading, eff.italic);
                let color = run_color(&eff);
                for ch in r.text.chars() {
                    segs.last_mut().unwrap().push(PCell {
                        ch,
                        font,
                        color,
                        underline: eff.underline,
                        strike: eff.strike,
                        link: None,
                    });
                }
            }
            Inline::Hyperlink(h) => {
                let target = h
                    .target
                    .clone()
                    .or_else(|| h.anchor.as_ref().map(|a| format!("#{a}")))
                    .unwrap_or_default();
                let rc: Rc<str> = Rc::from(target.as_str());
                for run in &h.runs {
                    let eff =
                        styles.effective_run(pstyle, run.props.style_id.as_deref(), &run.props);
                    let font = font_index(eff.bold || heading, eff.italic);
                    for ch in run.text.chars() {
                        segs.last_mut().unwrap().push(PCell {
                            ch,
                            font,
                            color: (0.0, 0.0, 0.55),
                            underline: true,
                            strike: eff.strike,
                            link: Some(rc.clone()),
                        });
                    }
                }
            }
            Inline::Tab(_) => {
                for _ in 0..4 {
                    segs.last_mut().unwrap().push(plain_cell(' '));
                }
            }
            Inline::Break(_) => segs.push(Vec::new()),
            // SmartArt: the terminal can't draw the diagram, so lay its node text
            // out as plain lines ("SmartArt" caption first) in the PDF too.
            Inline::SmartArt { text, .. } => {
                for line in std::iter::once("SmartArt").chain(text.iter().map(|s| s.as_str())) {
                    if !segs.last().map(|s| s.is_empty()).unwrap_or(true) {
                        segs.push(Vec::new());
                    }
                    for ch in line.chars() {
                        segs.last_mut().unwrap().push(plain_cell(ch));
                    }
                    segs.push(Vec::new());
                }
            }
            // A chart: lay its text bar/pie view out on its own lines.
            Inline::Chart { chart, .. } => {
                for line in crate::chart::render_chart(chart, 80) {
                    if !segs.last().map(|s| s.is_empty()).unwrap_or(true) {
                        segs.push(Vec::new());
                    }
                    for ch in line.chars() {
                        segs.last_mut().unwrap().push(plain_cell(ch));
                    }
                    segs.push(Vec::new());
                }
            }
            // A decoded equation (or a field's result) flows inline as plain text.
            Inline::Equation { text, .. } | Inline::Field { text, .. } => {
                for ch in text.chars() {
                    segs.last_mut().unwrap().push(plain_cell(ch));
                }
            }
            // A tracked change: lay its inner text out inline.
            Inline::Revision { content, .. } => {
                for ch in content
                    .iter()
                    .flat_map(|i| i.text().chars().collect::<Vec<_>>())
                {
                    segs.last_mut().unwrap().push(plain_cell(ch));
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
                    if !segs.last().map(|s| s.is_empty()).unwrap_or(true) {
                        segs.push(Vec::new());
                    }
                    for ch in line.chars() {
                        segs.last_mut().unwrap().push(plain_cell(ch));
                    }
                    segs.push(Vec::new());
                }
            }
            Inline::Raw(_) => {}
        }
    }
    segs
}

fn wrap_cells(cells: &[PCell], width: usize) -> Vec<Vec<PCell>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<PCell>> = Vec::new();
    let mut cur: Vec<PCell> = Vec::new();
    let mut last_space: Option<usize> = None;
    for c in cells {
        cur.push(c.clone());
        if c.ch == ' ' {
            last_space = Some(cur.len() - 1);
        }
        if cur.len() > width {
            if let Some(sp) = last_space {
                let rest = cur.split_off(sp + 1);
                while cur.last().map(|c| c.ch == ' ').unwrap_or(false) {
                    cur.pop();
                }
                lines.push(std::mem::take(&mut cur));
                cur = rest;
                last_space = cur.iter().rposition(|c| c.ch == ' ');
            } else {
                let last = cur.pop().unwrap();
                lines.push(std::mem::take(&mut cur));
                cur.push(last);
                last_space = None;
            }
        }
    }
    while cur.last().map(|c| c.ch == ' ').unwrap_or(false) {
        cur.pop();
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
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

fn build_content(page: &Page) -> Vec<u8> {
    let mut s: Vec<u8> = Vec::new();
    for f in &page.frags {
        let n_chars = f.text.chars().count() as f32;
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
        s.extend(pdf_string_body(&f.text));
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
        let content = build_content(page);
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
        for (rect, uri) in &page.links {
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
            opts.page_width, opts.page_height
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
}
