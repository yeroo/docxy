//! Pure render engine: [`crate::model::Document`] -> styled terminal lines,
//! plus an optional caret map so an editor can place a cursor — including inside
//! table cells.
//!
//! Each rendered line carries a [`LineMap`] of zero or more [`LineSeg`]s. A
//! paragraph line has one segment; a table row line has one segment per editable
//! cell. Each segment ties screen columns to model character offsets for a
//! specific paragraph **path** (`[block]`, or `[table,row,cell,block,...]`).

use std::collections::HashMap;
use std::rc::Rc;

use crate::model::*;
use crate::styles::StyleSheet;

/// Reduced 16-color terminal palette (theme-friendly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    Gray,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

const PALETTE: [(Color, (u8, u8, u8)); 16] = [
    (Color::Black, (0, 0, 0)),
    (Color::Red, (205, 0, 0)),
    (Color::Green, (0, 205, 0)),
    (Color::Yellow, (205, 205, 0)),
    (Color::Blue, (0, 0, 238)),
    (Color::Magenta, (205, 0, 205)),
    (Color::Cyan, (0, 205, 205)),
    (Color::White, (229, 229, 229)),
    (Color::Gray, (127, 127, 127)),
    (Color::BrightRed, (255, 0, 0)),
    (Color::BrightGreen, (0, 255, 0)),
    (Color::BrightYellow, (255, 255, 0)),
    (Color::BrightBlue, (92, 92, 255)),
    (Color::BrightMagenta, (255, 0, 255)),
    (Color::BrightCyan, (0, 255, 255)),
    (Color::BrightWhite, (255, 255, 255)),
];

fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    if s.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// Quantize an RGB color to the nearest palette entry.
pub fn quantize(rgb: (u8, u8, u8)) -> Color {
    let mut best = Color::White;
    let mut best_d = u32::MAX;
    for (c, (r, g, b)) in PALETTE {
        let dr = r as i32 - rgb.0 as i32;
        let dg = g as i32 - rgb.1 as i32;
        let db = b as i32 - rgb.2 as i32;
        let d = (dr * dr + dg * dg + db * db) as u32;
        if d < best_d {
            best_d = d;
            best = c;
        }
    }
    best
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub dim: bool,
    /// Selected text (drawn reversed by the TUI).
    pub highlight: bool,
    pub color: Option<Color>,
}

fn dim_style() -> Style {
    Style {
        dim: true,
        ..Style::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn width(&self) -> usize {
        self.spans.iter().map(|s| s.text.chars().count()).sum()
    }
    pub fn plain(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
    fn text_span(text: String) -> Span {
        Span {
            text,
            style: Style::default(),
            link: None,
        }
    }
    fn dim_span(text: String) -> Span {
        Span {
            text,
            style: dim_style(),
            link: None,
        }
    }
}

/// One editable region on a visual line, tied to a paragraph path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSeg {
    /// Paragraph path (`[block]` or `[table,row,cell,block,...]`).
    pub path: Vec<usize>,
    /// Model char offset of the first char shown in this segment.
    pub start: usize,
    /// Absolute display column where the segment's text begins.
    pub col0: usize,
    /// `cols[k]` = column offset (relative to `col0`) of the k-th char; the last
    /// entry is just past the final char.
    pub cols: Vec<usize>,
}

impl LineSeg {
    pub fn nchars(&self) -> usize {
        self.cols.len().saturating_sub(1)
    }
    pub fn contains(&self, path: &[usize], offset: usize) -> bool {
        self.path == path && offset >= self.start && offset <= self.start + self.nchars()
    }
    pub fn col_for_offset(&self, offset: usize) -> Option<usize> {
        if offset < self.start {
            return None;
        }
        self.cols.get(offset - self.start).map(|c| self.col0 + c)
    }
    /// Nearest model offset for an absolute screen column.
    pub fn offset_for_col(&self, col: usize) -> usize {
        let rel = col.saturating_sub(self.col0);
        let mut best = 0usize;
        let mut best_d = usize::MAX;
        for (i, &c) in self.cols.iter().enumerate() {
            let d = c.abs_diff(rel);
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        self.start + best
    }
    /// Column span [first, last] this segment occupies on screen.
    pub fn col_range(&self) -> (usize, usize) {
        (
            self.col0,
            self.col0 + self.cols.last().copied().unwrap_or(0),
        )
    }
}

/// The editable segments on a single visual line (empty if not editable).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineMap {
    pub segs: Vec<LineSeg>,
    /// In print layout, marks a hard page break: paginate starts a new page here
    /// and does not render this (placeholder) line.
    pub page_break: bool,
}

impl LineMap {
    fn one(seg: LineSeg) -> Self {
        LineMap {
            segs: vec![seg],
            page_break: false,
        }
    }
    /// The segment containing the caret (path + offset), if any.
    pub fn seg_for(&self, path: &[usize], offset: usize) -> Option<&LineSeg> {
        self.segs.iter().find(|s| s.contains(path, offset))
    }
    /// The editable segment nearest the given column (for vertical movement).
    pub fn nearest_seg(&self, col: usize) -> Option<&LineSeg> {
        self.segs.iter().min_by_key(|s| {
            let (a, b) = s.col_range();
            if col < a {
                a - col
            } else {
                col.saturating_sub(b)
            }
        })
    }
    pub fn is_editable(&self) -> bool {
        !self.segs.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub width: usize,
    pub show_invisibles: bool,
    pub page_view: bool,
    pub borderless_tables: bool,
    /// Selected ranges per paragraph: `(path, start_offset, end_offset)`.
    pub selection: Vec<(Vec<usize>, usize, usize)>,
    /// Resolved stylesheet (from `styles.xml`) for effective formatting.
    pub styles: Rc<StyleSheet>,
    /// Precomputed list markers per paragraph path (from `numbering.xml`).
    pub list_markers: Rc<HashMap<Vec<usize>, String>>,
    /// Page geometry, for projecting frame-positioned (floating) content.
    pub page: PageGeom,
    /// Header/footer block content (default / first-page / even-page variants),
    /// drawn in the page margins in print layout.
    pub headers: PageParts,
    pub footers: PageParts,
    /// First page uses the `first` variant (`<w:titlePg/>`).
    pub title_page: bool,
    /// Even pages use the `even` variant (`<w:evenAndOddHeaders/>`).
    pub even_odd: bool,
}

/// The three header (or footer) variants a section can define.
#[derive(Debug, Clone, Default)]
pub struct PageParts {
    pub default: Rc<Vec<Block>>,
    pub first: Rc<Vec<Block>>,
    pub even: Rc<Vec<Block>>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            width: 80,
            show_invisibles: false,
            page_view: false,
            borderless_tables: false,
            selection: Vec::new(),
            styles: Rc::new(StyleSheet::default()),
            list_markers: Rc::new(HashMap::new()),
            page: PageGeom::default(),
            headers: PageParts::default(),
            footers: PageParts::default(),
            title_page: false,
            even_odd: false,
        }
    }
}

/// Where an embedded image's placeholder box was laid out, so the app can
/// overlay real pixels onto it. Coordinates are in rendered cells: `row`/`col`
/// are the box's top-left; `cols`/`rows` are the **interior** size (excludes the
/// 1-cell border).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBox {
    /// Relationship id (`r:embed` / `r:id`) used to resolve the media bytes.
    pub rid: String,
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
}

/// The relationship id of an embedded image from its raw run XML — DrawingML
/// `<a:blip r:embed="..">` or VML `<v:imagedata r:id="..">`.
fn embed_rid(raw: &str) -> Option<String> {
    raw_attr_str(raw, "r:embed=").or_else(|| raw_attr_str(raw, "r:id="))
}

/// Render a document into styled lines.
pub fn render(doc: &Document, opts: &RenderOptions) -> Vec<Line> {
    render_mapped(doc, opts).0
}

/// Render a document into styled lines plus a caret map per line.
pub fn render_mapped(doc: &Document, opts: &RenderOptions) -> (Vec<Line>, Vec<LineMap>) {
    let (lines, maps, _imgs) = render_with_images(doc, opts);
    (lines, maps)
}

/// Like [`render_mapped`], plus the screen placement of every embedded image
/// box, so a TUI can draw real pixels over the placeholders.
pub fn render_with_images(
    doc: &Document,
    opts: &RenderOptions,
) -> (Vec<Line>, Vec<LineMap>, Vec<ImageBox>) {
    if !opts.page_view {
        let mut images = Vec::new();
        let pairs = render_blocks(&doc.body, &[], opts.width.max(8), opts, &mut images);
        let (lines, maps) = pairs.into_iter().unzip();
        return (lines, maps, images);
    }

    // Print layout: split the body into sections (each `section_break` paragraph
    // ends a section; the final body sectPr is the last section), then render and
    // paginate each at its own page geometry so different paper sizes/orientations
    // sit on different page boxes.
    let pl = page_lines(opts);
    let mut out: Vec<(Line, LineMap)> = Vec::new();
    let mut images: Vec<ImageBox> = Vec::new();
    for (start, end, geom) in sections(doc, opts.page) {
        let content_width = page_metrics(opts.width, geom).content_cols;
        let mut sec_imgs = Vec::new();
        let mut pairs = render_blocks(
            &doc.body[start..end],
            &[],
            content_width,
            opts,
            &mut sec_imgs,
        );
        // The slice rebased paragraph paths to 0; restore body-absolute paths.
        for (_, map) in &mut pairs {
            for seg in &mut map.segs {
                if let Some(first) = seg.path.first_mut() {
                    *first += start;
                }
            }
        }
        let pages = paginate(pairs, opts, geom, &mut sec_imgs, &pl);
        let base = out.len();
        for ib in &mut sec_imgs {
            ib.row += base;
        }
        out.extend(pages);
        images.extend(sec_imgs);
    }
    let (lines, maps) = out.into_iter().unzip();
    (lines, maps, images)
}

/// Body block ranges per section `(start, end_exclusive, geometry)`. A paragraph
/// carrying a `section_break` ends a section (using that break's geometry); the
/// remaining content forms the final section (using the trailing `sectPr`).
fn sections(doc: &Document, last: PageGeom) -> Vec<(usize, usize, PageGeom)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in doc.body.iter().enumerate() {
        if let Block::Paragraph(p) = b {
            if let Some(sect) = &p.props.section_break {
                out.push((start, i + 1, PageGeom::from_sect_pr(sect)));
                start = i + 1;
            }
        }
    }
    if start < doc.body.len() || out.is_empty() {
        out.push((start, doc.body.len(), last));
    }
    out
}

/// Render the header/footer variants to (non-editable) lines for the margins.
fn page_lines(opts: &RenderOptions) -> PageLines {
    let mut imgs = Vec::new();
    let to_lines = |blocks: &[Block], imgs: &mut Vec<ImageBox>| -> Vec<Line> {
        render_blocks(
            blocks,
            &[],
            page_metrics(opts.width, opts.page).content_cols,
            opts,
            imgs,
        )
        .into_iter()
        .map(|(l, _)| l)
        .collect()
    };
    PageLines {
        h: [
            to_lines(&opts.headers.default, &mut imgs),
            to_lines(&opts.headers.first, &mut imgs),
            to_lines(&opts.headers.even, &mut imgs),
        ],
        f: [
            to_lines(&opts.footers.default, &mut imgs),
            to_lines(&opts.footers.first, &mut imgs),
            to_lines(&opts.footers.even, &mut imgs),
        ],
        title_page: opts.title_page,
        even_odd: opts.even_odd,
    }
}

fn render_blocks(
    blocks: &[Block],
    base: &[usize],
    width: usize,
    opts: &RenderOptions,
    images: &mut Vec<ImageBox>,
) -> Vec<(Line, LineMap)> {
    // Frame-positioned (floating) images are page-anchored and independent of the
    // text flow, so gather *all* of them in this block list and project them onto
    // a single 2D canvas, emitted where the first one occurs.
    // The whole span from the first to the last floating image is one "figure
    // region": the images project to their page coordinates and any text flowing
    // among them (e.g. a heading) is composited at the page-text top, so it sits
    // level with the topmost image as on Word's page.
    let first = blocks.iter().position(|b| floating_image(b).is_some());
    let last = blocks.iter().rposition(|b| floating_image(b).is_some());

    let mut out = Vec::new();
    // Collect sub-render image boxes (rows relative to the sub-render), then
    // offset by where the sub-render lands in `out`.
    let absorb = |out: &mut Vec<(Line, LineMap)>,
                  sub: Vec<(Line, LineMap)>,
                  sub_imgs: Vec<ImageBox>,
                  images: &mut Vec<ImageBox>| {
        let base_row = out.len();
        for mut ib in sub_imgs {
            ib.row += base_row;
            images.push(ib);
        }
        out.extend(sub);
    };
    for (bi, b) in blocks.iter().enumerate() {
        if let (Some(f), Some(l)) = (first, last) {
            if bi >= f && bi <= l {
                if bi == f {
                    let region: Vec<&Block> = blocks[f..=l].iter().collect();
                    let mut sub_imgs = Vec::new();
                    let sub = render_floating_canvas(&region, width, opts, &mut sub_imgs);
                    absorb(&mut out, sub, sub_imgs, images);
                }
                continue; // the figure region is drawn as one canvas
            }
        }
        let mut path = base.to_vec();
        path.push(bi);
        match b {
            Block::Paragraph(p) => {
                let mut sub_imgs = Vec::new();
                let sub = render_paragraph(p, &path, width, opts, &mut sub_imgs);
                absorb(&mut out, sub, sub_imgs, images);
            }
            Block::Table(t) => out.extend(render_table(t, &path, width, opts)),
            Block::Raw(_) => out.push((
                Line {
                    spans: vec![Line::dim_span("⟨embedded content⟩".to_string())],
                },
                LineMap::default(),
            )),
        }
    }
    out
}

/// If a block is a frame-positioned paragraph holding an image, return its
/// `(FramePr, image pixel size)`. These are floated to absolute page coordinates.
fn floating_image(b: &Block) -> Option<(&FramePr, (u32, u32))> {
    let Block::Paragraph(p) = b else { return None };
    let frame = p.props.frame.as_ref()?;
    for item in &p.content {
        if let Inline::Raw(raw) = item {
            if let Some(size) = raw_image_extent(raw) {
                return Some((frame, size));
            }
        }
    }
    None
}

// ---- inline rendering ----

#[derive(Clone)]
struct Glyph {
    /// Semantic char (used for wrapping; spaces stay `' '`).
    ch: char,
    /// Display override (e.g. a space shown as `·`); falls back to `ch`.
    disp: Option<char>,
    style: Style,
    link: Option<Rc<str>>,
    src: Option<usize>,
}

/// Style for invisible-character marks: a muted gray.
fn invis_style() -> Style {
    Style {
        color: Some(Color::Gray),
        ..Style::default()
    }
}

fn style_from_run(p: &RunProps) -> Style {
    Style {
        bold: p.bold,
        italic: p.italic,
        underline: p.underline,
        strike: p.strike,
        dim: p.vanish,
        highlight: false,
        color: p.color.as_deref().and_then(parse_hex).map(quantize),
    }
}

/// A run of glyphs between hard breaks, plus the page/column break (if any) that
/// precedes it (so a separator line can be drawn before it).
struct Seg {
    glyphs: Vec<Glyph>,
    sep: Option<BreakKind>,
}

fn flatten_para(
    para: &Paragraph,
    opts: &RenderOptions,
    heading: bool,
    sel: &[(usize, usize)],
) -> Vec<Seg> {
    let inv = opts.show_invisibles;
    let mut segs: Vec<Seg> = vec![Seg {
        glyphs: Vec::new(),
        sep: None,
    }];
    let mut mc = 0usize;

    let sel_at = |mc: usize| sel.iter().any(|(s, e)| mc >= *s && mc < *e);

    // A normal text char, shown as `·` (gray) when it's a space and invisibles on.
    let make = |ch: char, style: &Style, link: Option<Rc<str>>, mc: usize| -> Glyph {
        let mut g = if inv && ch == ' ' {
            let mut st = invis_style();
            st.underline = style.underline; // keep link underline on a dotted link-space
            Glyph {
                ch: ' ',
                disp: Some('·'),
                style: st,
                link,
                src: Some(mc),
            }
        } else {
            Glyph {
                ch,
                disp: None,
                style: style.clone(),
                link,
                src: Some(mc),
            }
        };
        g.style.highlight = sel_at(mc);
        g
    };

    for item in &para.content {
        match item {
            Inline::Run(r) => {
                let eff = opts.styles.effective_run(
                    para.props.style_id.as_deref(),
                    r.props.style_id.as_deref(),
                    &r.props,
                );
                let mut st = style_from_run(&eff);
                if heading {
                    st.bold = true;
                }
                for ch in r.text.chars() {
                    segs.last_mut()
                        .unwrap()
                        .glyphs
                        .push(make(ch, &st, None, mc));
                    mc += 1;
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
                    let eff = opts.styles.effective_run(
                        para.props.style_id.as_deref(),
                        run.props.style_id.as_deref(),
                        &run.props,
                    );
                    let mut st = style_from_run(&eff);
                    st.underline = true;
                    st.color = Some(Color::Cyan);
                    if heading {
                        st.bold = true;
                    }
                    for ch in run.text.chars() {
                        segs.last_mut()
                            .unwrap()
                            .glyphs
                            .push(make(ch, &st, Some(rc.clone()), mc));
                        mc += 1;
                    }
                }
            }
            Inline::Tab => {
                let hl = sel_at(mc);
                let mut arrow = invis_style();
                arrow.highlight = hl;
                let plain = Style {
                    highlight: hl,
                    ..Style::default()
                };
                let seg = &mut segs.last_mut().unwrap().glyphs;
                if inv {
                    seg.push(Glyph {
                        ch: '→',
                        disp: None,
                        style: arrow,
                        link: None,
                        src: Some(mc),
                    });
                    for _ in 0..3 {
                        seg.push(Glyph {
                            ch: ' ',
                            disp: None,
                            style: plain.clone(),
                            link: None,
                            src: Some(mc),
                        });
                    }
                } else {
                    for _ in 0..4 {
                        seg.push(Glyph {
                            ch: ' ',
                            disp: None,
                            style: plain.clone(),
                            link: None,
                            src: Some(mc),
                        });
                    }
                }
                mc += 1;
            }
            Inline::Break(kind) => {
                match kind {
                    BreakKind::Line => {
                        if inv {
                            segs.last_mut().unwrap().glyphs.push(Glyph {
                                ch: '↵',
                                disp: None,
                                style: invis_style(),
                                link: None,
                                src: None,
                            });
                        }
                        segs.push(Seg {
                            glyphs: Vec::new(),
                            sep: None,
                        });
                    }
                    BreakKind::Page => segs.push(Seg {
                        glyphs: Vec::new(),
                        sep: Some(BreakKind::Page),
                    }),
                    BreakKind::Column => segs.push(Seg {
                        glyphs: Vec::new(),
                        sep: Some(BreakKind::Column),
                    }),
                }
                mc += 1;
            }
            Inline::Raw(_) => {} // zero-length, invisible (preserved for save only)
        }
    }
    if inv {
        segs.last_mut().unwrap().glyphs.push(Glyph {
            ch: '¶',
            disp: None,
            style: invis_style(),
            link: None,
            src: None,
        });
    }
    segs
}

/// A horizontal separator line for a page/column break (labeled when invisibles).
fn break_separator(kind: BreakKind, width: usize, inv: bool) -> Line {
    let label = match kind {
        BreakKind::Page => "Page Break",
        BreakKind::Column => "Column Break",
        BreakKind::Line => "",
    };
    let mid = if inv && !label.is_empty() {
        format!(" {label} ")
    } else {
        String::new()
    };
    let dashes = width.saturating_sub(mid.chars().count());
    let left = dashes / 2;
    let right = dashes - left;
    let text = format!("{}{}{}", "─".repeat(left), mid, "─".repeat(right));
    Line {
        spans: vec![Span {
            text,
            style: invis_style(),
            link: None,
        }],
    }
}

fn wrap_glyphs(glyphs: &[Glyph], width: usize) -> Vec<Vec<Glyph>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<Glyph>> = Vec::new();
    let mut cur: Vec<Glyph> = Vec::new();
    let mut last_space: Option<usize> = None;
    for g in glyphs {
        cur.push(g.clone());
        if g.ch == ' ' {
            last_space = Some(cur.len() - 1);
        }
        if cur.len() > width {
            if let Some(sp) = last_space {
                let rest = cur.split_off(sp + 1);
                while cur.last().map(|g| g.ch == ' ').unwrap_or(false) {
                    cur.pop();
                }
                lines.push(std::mem::take(&mut cur));
                cur = rest;
                last_space = cur.iter().rposition(|g| g.ch == ' ');
            } else {
                let last = cur.pop().unwrap();
                lines.push(std::mem::take(&mut cur));
                cur.push(last);
                last_space = None;
            }
        }
    }
    while cur.last().map(|g| g.ch == ' ').unwrap_or(false) {
        cur.pop();
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

fn glyphs_to_spans(glyphs: &[Glyph]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for g in glyphs {
        let gl = g.link.as_deref();
        let ch = g.disp.unwrap_or(g.ch);
        if let Some(last) = spans.last_mut() {
            if last.style == g.style && last.link.as_deref() == gl {
                last.text.push(ch);
                continue;
            }
        }
        spans.push(Span {
            text: ch.to_string(),
            style: g.style.clone(),
            link: g.link.as_ref().map(|r| r.to_string()),
        });
    }
    spans
}

/// Compute (start offset, column boundaries) for a wrapped line of glyphs.
fn glyph_extent(glyphs: &[Glyph]) -> (usize, Vec<usize>) {
    let mut cols = Vec::new();
    let mut start = None;
    let mut last_src = None;
    let mut col = 0usize;
    let mut last_end = 0usize;
    for g in glyphs {
        if let Some(s) = g.src {
            if last_src != Some(s) {
                if start.is_none() {
                    start = Some(s);
                }
                cols.push(col);
                last_src = Some(s);
            }
            last_end = col + 1;
        }
        col += 1;
    }
    cols.push(last_end);
    (start.unwrap_or(0), cols)
}

fn render_paragraph(
    para: &Paragraph,
    path: &[usize],
    width: usize,
    opts: &RenderOptions,
    images: &mut Vec<ImageBox>,
) -> Vec<(Line, LineMap)> {
    let heading = para.props.heading_level;
    let is_list = para.props.num_id.is_some();
    let (first_prefix, cont_prefix) = if is_list {
        let ind = " ".repeat(para.props.ilvl.max(0) as usize * 2);
        let marker = opts
            .list_markers
            .get(path)
            .map(|s| s.as_str())
            .unwrap_or("•");
        let first = format!("{ind}{marker} ");
        let cont = " ".repeat(first.chars().count());
        (first, cont)
    } else {
        (String::new(), String::new())
    };
    let prefix_w = first_prefix.chars().count();
    let avail = width.saturating_sub(prefix_w).max(4);

    let local_sel: Vec<(usize, usize)> = opts
        .selection
        .iter()
        .filter(|(p, _, _)| p == path)
        .map(|(_, s, e)| (*s, *e))
        .collect();
    let segs = flatten_para(para, opts, heading.is_some(), &local_sel);

    let mut out = Vec::new();
    let mut line_idx = 0usize;
    for seg in &segs {
        if let Some(kind) = seg.sep {
            if opts.page_view && kind == BreakKind::Page {
                // Print layout: a hard page break ends the page; emit a marker
                // (invisible) instead of a separator rule.
                out.push((
                    Line { spans: Vec::new() },
                    LineMap {
                        page_break: true,
                        ..LineMap::default()
                    },
                ));
            } else {
                out.push((
                    break_separator(kind, width, opts.show_invisibles),
                    LineMap::default(),
                ));
            }
        }
        for gl in wrap_glyphs(&seg.glyphs, avail) {
            let prefix = if line_idx == 0 {
                &first_prefix
            } else {
                &cont_prefix
            };
            let body_w = gl.len();
            let total = prefix.chars().count() + body_w;
            let align = opts
                .styles
                .effective_align(para.props.style_id.as_deref(), para.props.align);
            let lead = match align {
                Align::Center => width.saturating_sub(total) / 2,
                Align::Right => width.saturating_sub(total),
                _ => 0,
            };
            let mut line = Line::default();
            if lead > 0 {
                line.spans.push(Line::text_span(" ".repeat(lead)));
            }
            if !prefix.is_empty() {
                line.spans.push(Line::text_span(prefix.clone()));
            }
            line.spans.extend(glyphs_to_spans(&gl));
            let prefix_cols = lead + prefix.chars().count();
            let (start, cols) = glyph_extent(&gl);
            let lseg = LineSeg {
                path: path.to_vec(),
                start,
                col0: prefix_cols,
                cols,
            };
            out.push((line, LineMap::one(lseg)));
            line_idx += 1;
        }
    }
    // Drawings (images) inside the paragraph become a sized placeholder box,
    // emitted after the paragraph text. (Real pixels are overlaid by the app.)
    for item in &para.content {
        if let Inline::Raw(raw) = item {
            // A text box (`<w:txbxContent>` inside a drawing/VML shape): render its
            // text in a box rather than treating the shape as opaque/an image.
            if raw.contains("txbxContent") {
                let blocks = crate::load::parse_textbox_blocks(raw);
                if !blocks.is_empty() {
                    out.extend(text_box(&blocks, width, opts));
                    continue;
                }
            }
            if let Some((pw, ph)) = raw_image_extent(raw) {
                let (pw, ph) = (pw as usize, ph as usize);
                let cols = (pw / 8).clamp(10, width.saturating_sub(2).max(10));
                let rows = (ph / 16).clamp(2, 20);
                if let Some(rid) = embed_rid(raw) {
                    // Box top border is at the current row; interior starts at +1/+1.
                    images.push(ImageBox {
                        rid,
                        row: out.len() + 1,
                        col: 1,
                        cols,
                        rows,
                    });
                }
                out.extend(image_box(cols, rows, &format!("image {pw}×{ph}")));
            }
        }
    }
    if let Some(lvl) = heading {
        if lvl <= 2 {
            out.push((
                Line {
                    spans: vec![Line::dim_span("─".repeat(width))],
                },
                LineMap::default(),
            ));
        }
    }
    out
}

/// Parse an embedded image's display size in pixels (96 dpi) from raw run XML.
/// Handles both modern DrawingML (`wp:extent cx/cy`, in EMUs) and legacy VML
/// (`<v:shape style="width:…;height:…">` with a CSS length). Returns `None` when
/// the raw isn't an image (bookmarks, fields, plain content controls).
fn raw_image_extent(raw: &str) -> Option<(u32, u32)> {
    // DrawingML: EMUs, 9525 per pixel at 96 dpi.
    if raw.contains("drawing") {
        let cx = raw_attr_u32(raw, "cx=")?;
        let cy = raw_attr_u32(raw, "cy=")?;
        return Some(((cx / 9525).max(1), (cy / 9525).max(1)));
    }
    // VML: a <v:shape> that references image bytes, sized by a CSS style. The
    // "width:"/"height:" keys (lowercase, colon) appear only in that style.
    if raw.contains("imagedata") {
        let w = css_len_px(raw, "width:")?;
        let h = css_len_px(raw, "height:")?;
        return Some((w.max(1), h.max(1)));
    }
    None
}

fn raw_attr_u32(s: &str, key: &str) -> Option<u32> {
    let i = s.find(key)? + key.len();
    let rest = s[i..].trim_start_matches(['"', '\'']);
    let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    num.parse().ok()
}

/// The quoted string value of attribute `key` (e.g. `r:embed=`) in raw XML.
fn raw_attr_str(s: &str, key: &str) -> Option<String> {
    let i = s.find(key)? + key.len();
    let rest = &s[i..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

/// Convert the first CSS length after `key` (e.g. `width:192pt`) to pixels.
fn css_len_px(s: &str, key: &str) -> Option<u32> {
    let i = s.find(key)? + key.len();
    let rest = s[i..].trim_start();
    let num: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if num.is_empty() {
        return None;
    }
    let val: f32 = num.parse().ok()?;
    let unit = rest[num.len()..].trim_start();
    let px = if unit.starts_with("pt") {
        val * 96.0 / 72.0
    } else if unit.starts_with("in") {
        val * 96.0
    } else if unit.starts_with("cm") {
        val * 96.0 / 2.54
    } else if unit.starts_with("mm") {
        val * 96.0 / 25.4
    } else if unit.starts_with("pc") {
        val * 16.0
    } else {
        val // px or unitless
    };
    Some(px.round() as u32)
}

/// Render a text box's block content inside a dim bordered frame (non-editable).
fn text_box(blocks: &[Block], width: usize, opts: &RenderOptions) -> Vec<(Line, LineMap)> {
    let inner = width.saturating_sub(4).max(8);
    let content = render_blocks(blocks, &[], inner, opts, &mut Vec::new());
    let mut out = Vec::new();
    out.push((
        Line {
            spans: vec![Line::dim_span(format!("┌{}┐", "─".repeat(inner + 2)))],
        },
        LineMap::default(),
    ));
    for (ln, _) in content {
        let pad = inner.saturating_sub(ln.width());
        let mut spans = vec![Line::dim_span("│ ".to_string())];
        spans.extend(ln.spans);
        spans.push(Line::dim_span(format!("{} │", " ".repeat(pad))));
        out.push((Line { spans }, LineMap::default()));
    }
    out.push((
        Line {
            spans: vec![Line::dim_span(format!("└{}┘", "─".repeat(inner + 2)))],
        },
        LineMap::default(),
    ));
    out
}

/// A dim bordered box standing in for an image (the graphics fallback).
fn image_box(cols: usize, rows: usize, label: &str) -> Vec<(Line, LineMap)> {
    let cols = cols.max(8);
    let rows = rows.max(3);
    let mut out = Vec::new();
    out.push((
        Line {
            spans: vec![Line::dim_span(format!("┌{}┐", "─".repeat(cols)))],
        },
        LineMap::default(),
    ));
    let inner_rows = rows - 2;
    let label_row = inner_rows / 2;
    for r in 0..inner_rows {
        let inner = if r == label_row {
            let l: String = label.chars().take(cols).collect();
            let pad = cols.saturating_sub(l.chars().count());
            format!("{}{}{}", " ".repeat(pad / 2), l, " ".repeat(pad - pad / 2))
        } else {
            " ".repeat(cols)
        };
        out.push((
            Line {
                spans: vec![Line::dim_span(format!("│{inner}│"))],
            },
            LineMap::default(),
        ));
    }
    out.push((
        Line {
            spans: vec![Line::dim_span(format!("└{}┘", "─".repeat(cols)))],
        },
        LineMap::default(),
    ));
    out
}

/// One projected image placement on the canvas, in cells.
struct Place {
    rid: String,
    r: i32,
    c: i32,
    w: i32,
    h: i32,
    label: String,
}

/// Project a run of frame-positioned image paragraphs onto a 2D canvas at their
/// real page coordinates, scaled to the terminal width. Returns dim, read-only
/// lines (the graphics fallback; the app can overlay real pixels onto each box).
fn render_floating_canvas(
    blocks: &[&Block],
    width: usize,
    opts: &RenderOptions,
    images: &mut Vec<ImageBox>,
) -> Vec<(Line, LineMap)> {
    let pg = opts.page;
    let pw = (pg.w.max(1)) as f32;
    let ph = (pg.h.max(1)) as f32;
    let cols = width.max(8);
    // Horizontal: page width -> columns. Vertical: half rate, since a terminal
    // cell is ~2x taller than wide, so a square image stays roughly square.
    let cpt = cols as f32 / pw;
    let rpt = cols as f32 / (2.0 * pw);

    let mut places: Vec<Place> = Vec::new();
    for &b in blocks {
        let Some((frame, (px_w, px_h))) = floating_image(b) else {
            continue;
        };
        let rid = match b {
            Block::Paragraph(p) => p.content.iter().find_map(|it| match it {
                Inline::Raw(r) => embed_rid(r),
                _ => None,
            }),
            _ => None,
        }
        .unwrap_or_default();
        // Image display size: px -> twips (15 per px at 96 dpi) -> cells.
        let bw = (px_w as f32 * 15.0 * cpt).round().max(3.0) as i32;
        let bh = (px_h as f32 * 15.0 * rpt).round().max(3.0) as i32;
        let margin = frame.h_anchor.as_deref() == Some("margin");
        let origin_x = if margin { pg.ml as f32 } else { 0.0 };
        let c = if let Some(x) = frame.x {
            ((origin_x + x as f32) * cpt).round() as i32
        } else {
            match frame.x_align.as_deref() {
                Some("center") => (cols as i32 - bw) / 2,
                Some("right") | Some("outside") => ((pw - pg.mr as f32) * cpt).round() as i32 - bw,
                _ => (origin_x * cpt).round() as i32,
            }
        };
        let r = if let Some(y) = frame.y {
            (y as f32 * rpt).round() as i32
        } else {
            match frame.y_align.as_deref() {
                Some("bottom") | Some("outside") => ((ph - pg.mb as f32) * rpt).round() as i32 - bh,
                Some("center") => (ph * rpt / 2.0).round() as i32 - bh / 2,
                _ => (pg.mt.max(0) as f32 * rpt).round() as i32,
            }
        };
        places.push(Place {
            rid,
            r,
            c,
            w: bw,
            h: bh,
            label: format!("image {px_w}×{px_h}"),
        });
    }
    if places.is_empty() {
        return Vec::new();
    }

    // Text flowing among the frames (e.g. a heading) is composited at the page
    // text top-left, so it sits level with the topmost image rather than above
    // the whole block.
    let text_col = (pg.ml.max(0) as f32 * cpt).round().max(0.0) as i32;
    let text_w = (cols as i32 - text_col).max(8) as usize;
    let mut text_lines: Vec<String> = Vec::new();
    for &b in blocks {
        if floating_image(b).is_some() {
            continue;
        }
        if let Block::Paragraph(p) = b {
            for (line, _) in render_paragraph(p, &[0], text_w, opts, &mut Vec::new()) {
                let s = line.plain();
                // Skip leading blank lines so the heading hugs the top.
                if !s.trim().is_empty() || !text_lines.is_empty() {
                    text_lines.push(s);
                }
            }
        }
    }
    while text_lines
        .last()
        .map(|s| s.trim().is_empty())
        .unwrap_or(false)
    {
        text_lines.pop();
    }

    // Crop vertically to the content band (trim empty space above/below).
    let min_r = places.iter().map(|p| p.r).min().unwrap();
    let max_r = places.iter().map(|p| p.r + p.h).max().unwrap();
    let rows = ((max_r - min_r).max(text_lines.len() as i32)).clamp(1, 400) as usize;
    let mut grid: Vec<Vec<char>> = vec![vec![' '; cols]; rows];
    for p in &places {
        let top = p.r - min_r;
        draw_box_into(&mut grid, top, p.c, p.w, p.h, &p.label);
        // Interior rect (inside the border) for a real-pixel overlay.
        if !p.rid.is_empty() {
            images.push(ImageBox {
                rid: p.rid.clone(),
                row: (top + 1).max(0) as usize,
                col: (p.c + 1).max(0) as usize,
                cols: (p.w - 1).max(1) as usize,
                rows: (p.h - 1).max(1) as usize,
            });
        }
    }
    // Draw the flow text over the images so a heading stays readable.
    for (i, line) in text_lines.iter().enumerate() {
        for (j, ch) in line.chars().enumerate() {
            let (r, c) = (i, text_col as usize + j);
            if r < rows && c < cols && ch != ' ' {
                grid[r][c] = ch;
            }
        }
    }
    grid.into_iter()
        .map(|row| {
            let text: String = row.into_iter().collect();
            (
                Line {
                    spans: vec![Line::dim_span(text.trim_end().to_string())],
                },
                LineMap::default(),
            )
        })
        .collect()
}

/// Draw a bordered, labeled box into the char grid at (r0, c0), clipped to bounds.
fn draw_box_into(grid: &mut [Vec<char>], r0: i32, c0: i32, w: i32, h: i32, label: &str) {
    let rows = grid.len() as i32;
    let cols = grid.first().map(|r| r.len()).unwrap_or(0) as i32;
    let put = |g: &mut [Vec<char>], r: i32, c: i32, ch: char| {
        if r >= 0 && r < rows && c >= 0 && c < cols {
            g[r as usize][c as usize] = ch;
        }
    };
    let (w, h) = (w.max(2), h.max(2));
    for dc in 0..=w {
        put(grid, r0, c0 + dc, '─');
        put(grid, r0 + h, c0 + dc, '─');
    }
    for dr in 0..=h {
        put(grid, r0 + dr, c0, '│');
        put(grid, r0 + dr, c0 + w, '│');
    }
    put(grid, r0, c0, '┌');
    put(grid, r0, c0 + w, '┐');
    put(grid, r0 + h, c0, '└');
    put(grid, r0 + h, c0 + w, '┘');
    // Centered label on the middle row, clipped to the interior width.
    let inner = (w - 1).max(0) as usize;
    let l: String = label.chars().take(inner).collect();
    let start = c0 + 1 + ((inner.saturating_sub(l.chars().count())) / 2) as i32;
    for (i, ch) in l.chars().enumerate() {
        put(grid, r0 + h / 2, start + i as i32, ch);
    }
}

// ---- tables ----

/// Pick the box-drawing glyph for a junction from which of the four arms exist.
fn junction(u: bool, d: bool, l: bool, r: bool) -> char {
    match (u, d, l, r) {
        (false, false, false, false) => ' ',
        (false, true, false, true) => '┌',
        (false, true, true, false) => '┐',
        (true, false, false, true) => '└',
        (true, false, true, false) => '┘',
        (true, true, false, true) => '├',
        (true, true, true, false) => '┤',
        (false, true, true, true) => '┬',
        (true, false, true, true) => '┴',
        (true, true, true, true) => '┼',
        (false, false, _, _) => '─',
        (_, _, false, false) => '│',
    }
}

fn render_table(
    t: &Table,
    table_path: &[usize],
    width: usize,
    opts: &RenderOptions,
) -> Vec<(Line, LineMap)> {
    let nrows = t.rows.len();
    if nrows == 0 {
        return Vec::new();
    }
    // Logical column count = the widest row in grid columns (sum of gridSpans).
    let ncols = t
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| c.grid_span.max(1) as usize)
                .sum::<usize>()
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let borderless = opts.borderless_tables;

    let overhead = if borderless {
        (ncols - 1) * 2
    } else {
        3 * ncols + 1
    };
    let content_total = width.saturating_sub(overhead).max(ncols);
    let base = content_total / ncols;
    let rem = content_total - base * ncols;
    let cols: Vec<usize> = (0..ncols)
        .map(|i| (base + if i < rem { 1 } else { 0 }).max(1))
        .collect();

    // Occupancy grid: which origin cell covers each (row, col). gridSpan covers
    // columns; vMerge::Continue inherits the owner from the row above.
    let mut owner = vec![vec![None::<(usize, usize)>; ncols]; nrows];
    let mut origins: std::collections::HashMap<(usize, usize), Vec<(Line, LineMap)>> =
        std::collections::HashMap::new();
    for (r, row) in t.rows.iter().enumerate() {
        let mut c = 0usize;
        for (ci, cell) in row.cells.iter().enumerate() {
            if c >= ncols {
                break;
            }
            let span = (cell.grid_span.max(1) as usize).min(ncols - c);
            if cell.v_merge == VMerge::Continue && r > 0 {
                for k in 0..span {
                    owner[r][c + k] = owner[r - 1][c + k];
                }
            } else {
                for k in 0..span {
                    owner[r][c + k] = Some((r, c));
                }
                let cw = cols[c..c + span].iter().sum::<usize>()
                    + (span - 1) * (if borderless { 2 } else { 3 });
                let mut cell_path = table_path.to_vec();
                cell_path.push(r);
                cell_path.push(ci);
                // Images inside table cells aren't overlaid (v1): discard their boxes.
                origins.insert(
                    (r, c),
                    render_blocks(&cell.blocks, &cell_path, cw, opts, &mut Vec::new()),
                );
            }
            c += span;
        }
    }

    // A vertical border at grid-line `gc` on row `r` exists unless the cells on
    // each side are the same (horizontal merge). A horizontal segment at
    // grid-line `gr` over column `c` exists unless merged vertically.
    let vborder = |gc: usize, r: usize| gc == 0 || gc == ncols || owner[r][gc - 1] != owner[r][gc];
    let hseg = |gr: usize, c: usize| gr == 0 || gr == nrows || owner[gr - 1][c] != owner[gr][c];

    // Origin cell content lands in its origin row, so that row must be tall enough.
    let mut height = vec![1usize; nrows];
    for ((or, _), lines) in &origins {
        height[*or] = height[*or].max(lines.len().max(1));
    }

    let mut out: Vec<(Line, LineMap)> = Vec::new();
    for gr in 0..=nrows {
        if !borderless {
            let mut s = String::new();
            for gc in 0..=ncols {
                let up = gr > 0 && vborder(gc, gr - 1);
                let down = gr < nrows && vborder(gc, gr);
                let left = gc > 0 && hseg(gr, gc - 1);
                let right = gc < ncols && hseg(gr, gc);
                s.push(junction(up, down, left, right));
                if gc < ncols {
                    let glyph = if hseg(gr, gc) { "─" } else { " " };
                    s.push_str(&glyph.repeat(cols[gc] + 2));
                }
            }
            out.push((
                Line {
                    spans: vec![Line::dim_span(s)],
                },
                LineMap::default(),
            ));
        }
        if gr >= nrows {
            break;
        }

        let r = gr;
        for h in 0..height[r] {
            let mut line = Line::default();
            let mut map = LineMap::default();
            let mut col_cursor = 0usize;
            if !borderless {
                line.spans.push(Line::dim_span(
                    if vborder(0, r) { "│" } else { " " }.to_string(),
                ));
                col_cursor += 1;
            }
            let mut c = 0usize;
            while c < ncols {
                let own = owner[r][c];
                let mut span = 1usize;
                while c + span < ncols && owner[r][c + span] == own {
                    span += 1;
                }
                let cw = cols[c..c + span].iter().sum::<usize>()
                    + (span - 1) * (if borderless { 2 } else { 3 });

                if borderless {
                    if c > 0 {
                        line.spans.push(Line::text_span("  ".to_string()));
                        col_cursor += 2;
                    }
                } else {
                    line.spans.push(Line::text_span(" ".to_string()));
                    col_cursor += 1;
                }
                let content_start = col_cursor;
                // Content only for an origin cell whose origin row is this row.
                let content = match own {
                    Some((or, oc)) if or == r => {
                        origins.get(&(or, oc)).and_then(|lines| lines.get(h))
                    }
                    _ => None,
                };
                if let Some((cline, cmap)) = content {
                    let used = cline.width();
                    line.spans.extend(cline.spans.clone());
                    if cw > used {
                        line.spans.push(Line::text_span(" ".repeat(cw - used)));
                    }
                    for seg in &cmap.segs {
                        let mut seg = seg.clone();
                        seg.col0 += content_start;
                        map.segs.push(seg);
                    }
                } else {
                    line.spans.push(Line::text_span(" ".repeat(cw)));
                }
                col_cursor += cw;
                if !borderless {
                    line.spans.push(Line::text_span(" ".to_string()));
                    let edge = vborder(c + span, r);
                    line.spans
                        .push(Line::dim_span(if edge { "│" } else { " " }.to_string()));
                    col_cursor += 2;
                }
                c += span;
            }
            if opts.show_invisibles && h == height[r] - 1 {
                line.spans.push(Span {
                    text: " ¤".to_string(),
                    style: invis_style(),
                    link: None,
                });
            }
            out.push((line, map));
        }
    }
    out
}

// ---- page framing ----

/// Geometry of one printed page, projected to terminal cells.
struct PageMetrics {
    /// Text wrap width (cells), inside the margins.
    content_cols: usize,
    /// Text rows per page, inside the margins.
    content_rows: usize,
    /// Left/right margin widths (cells), inside the page border.
    ml: usize,
    mr: usize,
    /// Top/bottom margin heights (rows), inside the page border.
    mt: usize,
    mb: usize,
    /// Left offset to center the page on the terminal.
    center: usize,
}

fn page_metrics(width: usize, pg: PageGeom) -> PageMetrics {
    let pw = (pg.w.max(1)) as f32;
    let ph = (pg.h.max(1)) as f32;
    // Page width in cells (incl. borders), capped so the page stays readable and
    // centered on wide terminals.
    let total = width.saturating_sub(2).clamp(20, 90);
    let inner = total.saturating_sub(2).max(8); // page area inside the borders
    let cpt = inner as f32 / pw; // cells per twip (horizontal)
    let rpt = cpt / 2.0; // vertical: a terminal cell is ~2x taller than wide
    let ml = (pg.ml.max(0) as f32 * cpt).round() as usize;
    let mr = (pg.mr.max(0) as f32 * cpt).round() as usize;
    let mt = (pg.mt.max(0) as f32 * rpt).round() as usize;
    let mb = (pg.mb.max(0) as f32 * rpt).round() as usize;
    let content_cols = inner.saturating_sub(ml + mr).max(4);
    let page_rows = (ph * rpt).round() as usize;
    let content_rows = page_rows.saturating_sub(mt + mb).clamp(1, 200);
    PageMetrics {
        content_cols,
        content_rows,
        ml,
        mr,
        mt,
        mb,
        center: width.saturating_sub(inner + 2) / 2,
    }
}

/// Lay rendered lines out as discrete page boxes (Word "print layout"): each page
/// is a bordered rectangle with real margins and a page number, centered on the
/// terminal, with content flowing across pages.
/// Rendered header/footer lines for the three variants, plus the section flags
/// that select which one each page uses. Index 0=default, 1=first, 2=even.
struct PageLines {
    h: [Vec<Line>; 3],
    f: [Vec<Line>; 3],
    title_page: bool,
    even_odd: bool,
}

impl PageLines {
    fn variant(&self, page: usize) -> usize {
        if page == 0 && self.title_page {
            1 // first page
        } else if self.even_odd && page % 2 == 1 {
            2 // even page (page number 2, 4, … = 0-based 1, 3, …)
        } else {
            0 // default (odd pages, or no special variant)
        }
    }
    fn header(&self, page: usize) -> &[Line] {
        &self.h[self.variant(page)]
    }
    fn footer(&self, page: usize) -> &[Line] {
        &self.f[self.variant(page)]
    }
}

fn paginate(
    pairs: Vec<(Line, LineMap)>,
    opts: &RenderOptions,
    geom: PageGeom,
    images: &mut [ImageBox],
    pl: &PageLines,
) -> Vec<(Line, LineMap)> {
    let m = page_metrics(opts.width, geom);
    let inner_w = m.content_cols + m.ml + m.mr;
    let pad = |n: usize| " ".repeat(n);
    let lead = pad(m.center);
    // Frame a header/footer/content line into a page row (left margin + content
    // + right margin, between the borders). Non-editable (no caret map).
    let frame_line = |ln: &Line| -> (Line, LineMap) {
        let rpad = m.content_cols.saturating_sub(ln.width());
        let mut spans = vec![Line::dim_span(format!("{lead}│{}", pad(m.ml)))];
        spans.extend(ln.spans.clone());
        spans.push(Line::dim_span(format!("{}│", pad(rpad + m.mr))));
        (Line { spans }, LineMap::default())
    };
    let border = |l: char, r: char| -> (Line, LineMap) {
        (
            Line {
                spans: vec![Line::dim_span(format!(
                    "{lead}{l}{}{r}",
                    "─".repeat(inner_w)
                ))],
            },
            LineMap::default(),
        )
    };
    let margin_row = |label: Option<&str>| -> (Line, LineMap) {
        let inner = match label {
            Some(s) => {
                let s: String = s.chars().take(inner_w).collect();
                let p = inner_w.saturating_sub(s.chars().count());
                format!("{}{s}{}", pad(p / 2), pad(p - p / 2))
            }
            None => pad(inner_w),
        };
        (
            Line {
                spans: vec![Line::dim_span(format!("{lead}│{inner}│"))],
            },
            LineMap::default(),
        )
    };

    let col_off = m.center + 1 + m.ml; // border + left margin + centering
    let total_in = pairs.len();

    // Assign each content line to a page, honoring hard page breaks (the marker
    // lines force the next line onto a new page and are themselves dropped).
    let mut items: Vec<(usize, Line, LineMap, usize)> = Vec::new(); // (idx, line, map, page)
    let mut row = 0usize;
    let mut pg = 0usize;
    for (idx, (ln, map)) in pairs.into_iter().enumerate() {
        if map.page_break {
            if row > 0 {
                pg += 1;
                row = 0;
            }
            continue;
        }
        if row == m.content_rows {
            pg += 1;
            row = 0;
        }
        items.push((idx, ln, map, pg));
        row += 1;
    }
    let total_pages = pg + 1;

    let mut out: Vec<(Line, LineMap)> = Vec::new();
    let mut new_row = vec![usize::MAX; total_in];
    let mut it = items.into_iter().peekable();
    for page in 0..total_pages {
        out.push(border('┌', '┐'));
        // Top margin, with this page's header drawn into it.
        let header = pl.header(page);
        for r in 0..m.mt {
            match header.get(r) {
                Some(hl) => out.push(frame_line(hl)),
                None => out.push(margin_row(None)),
            }
        }
        let mut placed = 0usize;
        while it.peek().map(|t| t.3) == Some(page) {
            let (idx, ln, mut map, _) = it.next().unwrap();
            let rpad = m.content_cols.saturating_sub(ln.width());
            let mut spans = vec![Line::dim_span(format!("{lead}│{}", pad(m.ml)))];
            spans.extend(ln.spans);
            spans.push(Line::dim_span(format!("{}│", pad(rpad + m.mr))));
            for seg in &mut map.segs {
                seg.col0 += col_off;
            }
            new_row[idx] = out.len();
            out.push((Line { spans }, map));
            placed += 1;
        }
        for _ in placed..m.content_rows {
            out.push(margin_row(None)); // pad the page to full height
        }
        // Bottom margin: footer at the top of it, page number on the last row.
        let footer = pl.footer(page);
        let pageno = format!("Page {} of {total_pages}", page + 1);
        let last = m.mb.saturating_sub(1);
        for r in 0..m.mb {
            if r == last {
                out.push(margin_row(Some(pageno.as_str())));
            } else if let Some(fl) = footer.get(r) {
                out.push(frame_line(fl));
            } else {
                out.push(margin_row(None));
            }
        }
        out.push(border('└', '┘'));
        out.push((Line { spans: Vec::new() }, LineMap::default())); // gap between pages
    }

    // Remap floating-image placements into the paginated layout.
    for ib in images.iter_mut() {
        if let Some(&nr) = new_row.get(ib.row) {
            if nr != usize::MAX {
                ib.row = nr;
                ib.col += col_off;
            }
        }
    }
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
    fn opts(width: usize) -> RenderOptions {
        RenderOptions {
            width,
            ..RenderOptions::default()
        }
    }

    #[test]
    fn quantize_primaries() {
        assert_eq!(quantize((255, 0, 0)), Color::BrightRed);
        assert_eq!(quantize((0, 0, 0)), Color::Black);
        assert_eq!(quantize((0, 0, 238)), Color::Blue);
        assert_eq!(quantize((255, 255, 255)), Color::BrightWhite);
    }

    #[test]
    fn plain_text_renders_single_line() {
        let d = doc(vec![para(vec![run("hello world", RunProps::default())])]);
        let lines = render(&d, &opts(40));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].plain(), "hello world");
    }

    #[test]
    fn paragraph_style_makes_text_bold() {
        let ss = crate::styles::parse_styles_xml(
            r#"<w:styles><w:style w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style></w:styles>"#,
        );
        let mut pr = ParProps::default();
        pr.style_id = Some("Strong".to_string());
        let d = doc(vec![Block::Paragraph(Paragraph {
            props: pr,
            content: vec![run("hi", RunProps::default())],
        })]);
        let mut o = opts(40);
        o.styles = Rc::new(ss);
        let lines = render(&d, &o);
        assert!(
            lines[0].spans[0].style.bold,
            "style-derived bold not applied"
        );
    }

    #[test]
    fn bold_run_carries_style() {
        let bold = RunProps {
            bold: true,
            ..RunProps::default()
        };
        let d = doc(vec![para(vec![run("hi", bold)])]);
        let lines = render(&d, &opts(40));
        assert!(lines[0].spans[0].style.bold);
    }

    #[test]
    fn colored_run_is_quantized() {
        let red = RunProps {
            color: Some("FF0000".to_string()),
            ..RunProps::default()
        };
        let d = doc(vec![para(vec![run("x", red)])]);
        let lines = render(&d, &opts(40));
        assert_eq!(lines[0].spans[0].style.color, Some(Color::BrightRed));
    }

    #[test]
    fn selection_highlights_only_selected_glyphs() {
        let d = doc(vec![para(vec![run("abcd", RunProps::default())])]);
        let mut o = opts(40);
        o.selection = vec![(vec![0], 1, 3)]; // select "bc"
        let lines = render(&d, &o);
        let highlighted: String = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.highlight)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(highlighted, "bc");
    }

    #[test]
    fn long_paragraph_wraps_to_width() {
        let d = doc(vec![para(vec![run(
            "aaaa bbbb cccc dddd",
            RunProps::default(),
        )])]);
        let lines = render(&d, &opts(9));
        assert!(lines.len() >= 2);
        for l in &lines {
            assert!(l.width() <= 9);
        }
    }

    #[test]
    fn heading_is_bold_with_rule() {
        let mut p = Paragraph {
            props: ParProps::default(),
            content: vec![run("Title", RunProps::default())],
        };
        p.props.heading_level = Some(1);
        let d = doc(vec![Block::Paragraph(p)]);
        let lines = render(&d, &opts(20));
        assert!(lines[0].spans[0].style.bold);
        assert!(lines.last().unwrap().plain().starts_with('─'));
    }

    #[test]
    fn list_item_gets_bullet() {
        let mut p = Paragraph {
            props: ParProps::default(),
            content: vec![run("item", RunProps::default())],
        };
        p.props.num_id = Some(1);
        let d = doc(vec![Block::Paragraph(p)]);
        let lines = render(&d, &opts(20));
        assert!(lines[0].plain().starts_with("• "));
    }

    #[test]
    fn numbered_list_uses_marker() {
        let mut pr = ParProps::default();
        pr.num_id = Some(1);
        let d = doc(vec![Block::Paragraph(Paragraph {
            props: pr,
            content: vec![run("item", RunProps::default())],
        })]);
        let mut o = opts(40);
        let mut markers = HashMap::new();
        markers.insert(vec![0], "1.".to_string());
        o.list_markers = Rc::new(markers);
        let lines = render(&d, &o);
        assert!(
            lines[0].plain().starts_with("1. item"),
            "got {:?}",
            lines[0].plain()
        );
    }

    #[test]
    fn drawing_renders_as_image_box() {
        let raw = "<w:r><w:drawing><wp:inline><wp:extent cx=\"3048000\" cy=\"1524000\"/></wp:inline></w:drawing></w:r>";
        let d = doc(vec![para(vec![Inline::Raw(raw.to_string())])]);
        let joined: String = render(&d, &opts(60))
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains('┌') && joined.contains('┘'),
            "no image box: {joined}"
        );
        assert!(
            joined.contains("image 320×160"),
            "missing size label: {joined}"
        );

        // A non-drawing raw (e.g. a bookmark) stays invisible.
        let d2 = doc(vec![para(vec![Inline::Raw(
            "<w:bookmarkStart/>".to_string(),
        )])]);
        let joined2: String = render(&d2, &opts(60))
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined2.contains('┌'));
    }

    fn vml_img(w: u32, h: u32) -> String {
        format!(
            "<w:r><w:pict><v:shape style=\"width:{w}pt;height:{h}pt\"><v:imagedata r:id=\"r\"/></v:shape></w:pict></w:r>"
        )
    }
    fn framed_para(x: i32, y: i32, raw: String) -> Block {
        let frame = FramePr {
            x: Some(x),
            y: Some(y),
            h_anchor: Some("page".to_string()),
            v_anchor: Some("page".to_string()),
            ..Default::default()
        };
        Block::Paragraph(Paragraph {
            props: ParProps {
                frame: Some(frame),
                ..Default::default()
            },
            content: vec![Inline::Raw(raw)],
        })
    }

    #[test]
    fn floating_images_project_side_by_side() {
        // Two images at the same page y but far-apart x should land on the same
        // rows in different columns — projected, not stacked.
        let d = doc(vec![
            framed_para(1081, 2521, vml_img(72, 72)), // ~9% across
            framed_para(8000, 2521, vml_img(72, 72)), // ~65% across, same y
        ]);
        let lines = render(&d, &opts(100));
        let joined: String = lines
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains('┌'), "no projected boxes:\n{joined}");
        let row_with_two = lines.iter().any(|l| l.plain().matches('┌').count() == 2);
        assert!(
            row_with_two,
            "images were not placed side by side:\n{joined}"
        );
    }

    #[test]
    fn floating_images_report_overlay_placements() {
        // render_with_images must report a placement (rid + rect) per image so the
        // app can overlay real pixels.
        let d = doc(vec![
            framed_para(1081, 2521, vml_img(72, 72)),
            framed_para(8000, 2521, vml_img(72, 72)),
        ]);
        let (_l, _m, imgs) = render_with_images(&d, &opts(100));
        assert_eq!(imgs.len(), 2, "expected one placement per image");
        assert!(
            imgs.iter().all(|i| i.rid == "r"),
            "rid not resolved: {imgs:?}"
        );
        assert!(imgs.iter().all(|i| i.cols > 0 && i.rows > 0));
        assert_ne!(
            imgs[0].col, imgs[1].col,
            "projected images should differ in column"
        );
    }

    #[test]
    fn inline_image_reports_placement() {
        let raw = "<w:r><w:drawing><wp:inline><wp:extent cx=\"1905000\" cy=\"952500\"/>\
            <a:blip r:embed=\"rId9\"/></wp:inline></w:drawing></w:r>";
        let d = doc(vec![para(vec![Inline::Raw(raw.to_string())])]);
        let (_l, _m, imgs) = render_with_images(&d, &opts(60));
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].rid, "rId9");
    }

    #[test]
    fn text_among_floating_images_sits_at_canvas_top() {
        // A heading flowing among floating images is composited at the page-text
        // top — level with the topmost image (small line index), never below the
        // block (the original bug, where it rendered after all the pictures).
        let d = doc(vec![
            framed_para(6481, 2521, vml_img(72, 72)), // top image
            para(vec![run("Graphics", RunProps::default())]),
            framed_para(1081, 9361, vml_img(72, 72)), // a lower image
        ]);
        let texts: Vec<String> = render(&d, &opts(100)).iter().map(|l| l.plain()).collect();
        let heading = texts
            .iter()
            .position(|s| s.contains("Graphics"))
            .expect("heading line");
        let last_box = texts
            .iter()
            .rposition(|s| s.contains('┌'))
            .expect("a box line");
        assert!(
            heading <= 1,
            "heading should be at the very top, got line {heading}"
        );
        assert!(
            last_box > heading,
            "the lower image should be below the heading"
        );
    }

    #[test]
    fn floating_image_right_align_sits_right_of_left_one() {
        // A right-aligned floating image should project to a higher column than a
        // left-anchored one.
        let left = framed_para(1081, 2521, vml_img(72, 72));
        let right_frame = FramePr {
            y: Some(2521),
            h_anchor: Some("margin".to_string()),
            x_align: Some("right".to_string()),
            y_align: Some("top".to_string()),
            ..Default::default()
        };
        let right = Block::Paragraph(Paragraph {
            props: ParProps {
                frame: Some(right_frame),
                ..Default::default()
            },
            content: vec![Inline::Raw(vml_img(72, 72))],
        });
        let lines = render(&doc(vec![left, right]), &opts(100));
        // The rightmost box corner (by column) should be far to the right.
        let max_c = lines
            .iter()
            .flat_map(|l| {
                let s = l.plain();
                s.match_indices('┌')
                    .map(|(i, _)| s[..i].chars().count())
                    .collect::<Vec<_>>()
            })
            .max()
            .unwrap_or(0);
        assert!(
            max_c > 50,
            "right-aligned image should be far right, got col {max_c}"
        );
    }

    #[test]
    fn text_box_content_renders() {
        let raw = "<w:r><w:pict><v:shape><v:textbox><w:txbxContent>\
                   <w:p><w:r><w:t>boxed text</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r>";
        let d = doc(vec![para(vec![Inline::Raw(raw.to_string())])]);
        let joined: String = render(&d, &opts(40))
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("boxed text"),
            "text box text missing:\n{joined}"
        );
        assert!(
            joined.contains('┌') && joined.contains('└'),
            "no box frame:\n{joined}"
        );
    }

    #[test]
    fn vml_image_renders_as_image_box() {
        // Legacy VML image: size comes from the shape's CSS style (192pt × 2in).
        // 192pt = 256px, 2in = 192px.
        let raw = "<w:r><w:pict><v:shape id=\"i\" type=\"#t75\" style=\"width:192pt;height:2in\">\
            <v:imagedata r:id=\"rId7\" o:title=\"\"/></v:shape></w:pict></w:r>";
        let d = doc(vec![para(vec![Inline::Raw(raw.to_string())])]);
        let joined: String = render(&d, &opts(60))
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains('┌') && joined.contains('┘'),
            "no image box: {joined}"
        );
        assert!(
            joined.contains("image 256×192"),
            "missing size label: {joined}"
        );
    }

    #[test]
    fn centered_paragraph_is_padded() {
        let mut p = Paragraph {
            props: ParProps::default(),
            content: vec![run("hi", RunProps::default())],
        };
        p.props.align = Align::Center;
        let d = doc(vec![Block::Paragraph(p)]);
        let lines = render(&d, &opts(10));
        assert!(lines[0].plain().starts_with(' '));
        assert_eq!(lines[0].plain().trim(), "hi");
    }

    #[test]
    fn hyperlink_span_has_target_and_underline() {
        let h = Inline::Hyperlink(Hyperlink {
            target: Some("https://x.test/".to_string()),
            anchor: None,
            rel_id: None,
            runs: vec![Run {
                text: "link".to_string(),
                props: RunProps::default(),
            }],
        });
        let d = doc(vec![para(vec![h])]);
        let lines = render(&d, &opts(40));
        let s = &lines[0].spans[0];
        assert_eq!(s.link.as_deref(), Some("https://x.test/"));
        assert!(s.style.underline);
        assert_eq!(s.style.color, Some(Color::Cyan));
    }

    #[test]
    fn invisibles_show_pilcrow_and_tab() {
        let mut o = opts(40);
        o.show_invisibles = true;
        let d = doc(vec![para(vec![
            run("a", RunProps::default()),
            Inline::Tab,
            run("b", RunProps::default()),
        ])]);
        let lines = render(&d, &o);
        let text = lines[0].plain();
        assert!(text.contains('→'));
        assert!(text.ends_with('¶'));
    }

    #[test]
    fn invisibles_dot_spaces_in_gray() {
        let mut o = opts(40);
        o.show_invisibles = true;
        let d = doc(vec![para(vec![run("a b", RunProps::default())])]);
        let lines = render(&d, &o);
        let text = lines[0].plain();
        assert!(text.contains('·'), "space should be dotted: {text:?}");
        assert!(text.ends_with('¶'));
        let dot = lines[0]
            .spans
            .iter()
            .find(|s| s.text.contains('·'))
            .unwrap();
        assert_eq!(dot.style.color, Some(Color::Gray));
    }

    #[test]
    fn page_break_renders_labeled_separator() {
        let d = doc(vec![para(vec![
            run("a", RunProps::default()),
            Inline::Break(BreakKind::Page),
            run("b", RunProps::default()),
        ])]);
        let mut o = opts(24);
        o.show_invisibles = true;
        let lines = render(&d, &o);
        let joined: String = lines
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("Page Break"),
            "missing page-break label: {joined}"
        );
        // content before and after the break is present
        assert!(joined.contains('a') && joined.contains('b'));
    }

    #[test]
    fn invisibles_show_table_row_end_marker() {
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let t = Table {
            grid: vec![100, 100],
            rows: vec![Row {
                cells: vec![cell("A"), cell("B")],
            }],
        };
        let d = doc(vec![Block::Table(t)]);
        let mut o = opts(30);
        o.show_invisibles = true;
        let lines = render(&d, &o);
        let joined: String = lines
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains('¤'), "row-end marker missing: {joined}");
    }

    #[test]
    fn table_renders_with_borders() {
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let t = Table {
            grid: vec![100, 100],
            rows: vec![
                Row {
                    cells: vec![cell("A"), cell("B")],
                },
                Row {
                    cells: vec![cell("c"), cell("d")],
                },
            ],
        };
        let d = doc(vec![Block::Table(t)]);
        let lines = render(&d, &opts(30));
        let joined: String = lines
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains('┌') && joined.contains('┐'));
        assert!(joined.contains('│'));
        assert!(joined.contains('└') && joined.contains('┘'));
        assert!(joined.contains('A') && joined.contains('d'));
    }

    #[test]
    fn gridspan_cell_fills_row_width_with_one_segment() {
        let cell = |s: &str, span: u32| Cell {
            grid_span: span,
            v_merge: VMerge::None,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let t = Table {
            grid: vec![100, 100],
            rows: vec![
                Row {
                    cells: vec![cell("A", 1), cell("B", 1)],
                },
                Row {
                    cells: vec![cell("wide", 2)],
                }, // spans both columns
            ],
        };
        let d = doc(vec![Block::Table(t)]);
        let (lines, maps) = render_mapped(&d, &opts(30));

        // The merged row contributes exactly one editable segment (its path),
        // not a phantom empty second cell.
        let merged_idx = maps
            .iter()
            .position(|m| m.segs.len() == 1 && m.segs[0].path == vec![0, 1, 0, 0])
            .expect("merged cell segment");
        assert_eq!(maps[merged_idx].segs.len(), 1);
        // It spans the full table width (same as a normal two-cell row).
        let normal_idx = maps
            .iter()
            .position(|m| m.segs.iter().any(|s| s.path == vec![0, 0, 0, 0]))
            .unwrap();
        assert_eq!(lines[merged_idx].width(), lines[normal_idx].width());
    }

    #[test]
    fn borderless_table_has_no_box_glyphs() {
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let t = Table {
            grid: vec![100, 100],
            rows: vec![Row {
                cells: vec![cell("A"), cell("B")],
            }],
        };
        let d = doc(vec![Block::Table(t)]);
        let mut o = opts(30);
        o.borderless_tables = true;
        let lines = render(&d, &o);
        let joined: String = lines
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined.contains('│') && !joined.contains('┌'));
        assert!(joined.contains('A') && joined.contains('B'));
    }

    #[test]
    fn horizontal_merge_has_no_internal_tick() {
        let t = Table {
            grid: vec![100, 100],
            rows: vec![Row {
                cells: vec![Cell {
                    grid_span: 2,
                    v_merge: VMerge::None,
                    blocks: vec![para(vec![run("wide", RunProps::default())])],
                }],
            }],
        };
        let d = doc(vec![Block::Table(t)]);
        let joined: String = render(&d, &opts(30))
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !joined.contains('┬') && !joined.contains('┴') && !joined.contains('┼'),
            "stray tick: {joined}"
        );
        assert!(joined.contains('┌') && joined.contains('┐') && joined.contains("wide"));
    }

    #[test]
    fn vertical_merge_renders_once_and_is_navigable() {
        let cell = |s: &str, vm: VMerge| Cell {
            grid_span: 1,
            v_merge: vm,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let t = Table {
            grid: vec![100, 100],
            rows: vec![
                Row {
                    cells: vec![cell("M", VMerge::Restart), cell("a", VMerge::None)],
                },
                Row {
                    cells: vec![cell("", VMerge::Continue), cell("b", VMerge::None)],
                },
            ],
        };
        let d = doc(vec![Block::Table(t)]);
        let (lines, maps) = render_mapped(&d, &opts(30));
        let has = |p: Vec<usize>| maps.iter().any(|m| m.segs.iter().any(|s| s.path == p));
        assert!(has(vec![0, 0, 0, 0]), "merged origin not navigable");
        assert!(has(vec![0, 0, 1, 0]) && has(vec![0, 1, 1, 0]));
        let joined: String = lines
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            joined.matches('M').count(),
            1,
            "merged content should render once"
        );
    }

    #[test]
    fn page_view_frames_content() {
        let d = doc(vec![para(vec![run("hi", RunProps::default())])]);
        let mut o = opts(40);
        o.page_view = true;
        let lines = render(&d, &o);
        let plain: Vec<String> = lines.iter().map(|l| l.plain()).collect();
        // First line is the top page border, last is the bottom border.
        assert!(plain[0].trim_start().starts_with('┌'), "{:?}", plain[0]);
        assert!(plain.iter().any(|l| l.trim_start().starts_with('└')));
        // The content "hi" appears inside the page, and a page number is printed.
        assert!(plain.iter().any(|l| l.contains("hi")));
        assert!(plain.iter().any(|l| l.contains("Page 1 of 1")));
    }

    #[test]
    fn explicit_page_break_starts_new_page() {
        // A hard page break must start a second page even with little content.
        let p = para(vec![
            run("before", RunProps::default()),
            Inline::Break(BreakKind::Page),
            run("after", RunProps::default()),
        ]);
        let mut o = opts(60);
        o.page_view = true;
        let plain: Vec<String> = render(&doc(vec![p]), &o)
            .iter()
            .map(|l| l.plain())
            .collect();
        let tops = plain
            .iter()
            .filter(|l| l.trim_start().starts_with('┌'))
            .count();
        assert_eq!(tops, 2, "page break should create a second page");
        assert!(plain.iter().any(|l| l.contains("before")));
        assert!(plain.iter().any(|l| l.contains("after")));
        assert!(plain.iter().any(|l| l.contains("Page 2 of 2")));
    }

    #[test]
    fn print_layout_draws_header_and_footer() {
        let mut o = opts(70);
        o.page_view = true;
        o.headers.default =
            std::rc::Rc::new(vec![para(vec![run("THE HEADER", RunProps::default())])]);
        o.footers.default =
            std::rc::Rc::new(vec![para(vec![run("THE FOOTER", RunProps::default())])]);
        let d = doc(vec![para(vec![run("body text", RunProps::default())])]);
        let plain: Vec<String> = render(&d, &o).iter().map(|l| l.plain()).collect();
        let h = plain
            .iter()
            .position(|l| l.contains("THE HEADER"))
            .expect("header drawn");
        let b = plain
            .iter()
            .position(|l| l.contains("body text"))
            .expect("body drawn");
        let f = plain
            .iter()
            .position(|l| l.contains("THE FOOTER"))
            .expect("footer drawn");
        assert!(
            h < b && b < f,
            "header/body/footer order wrong: h={h} b={b} f={f}"
        );
    }

    #[test]
    fn print_layout_switches_page_size_per_section() {
        // Section 1 ends with a landscape section break (wider, so shorter page);
        // section 2 uses the default trailing portrait geometry (taller page).
        let break_para = Block::Paragraph(Paragraph {
            props: ParProps {
                section_break: Some(
                    "<w:sectPr><w:pgSz w:w=\"15840\" w:h=\"12240\"/></w:sectPr>".to_string(),
                ),
                ..Default::default()
            },
            content: vec![run("end of section one", RunProps::default())],
        });
        let body = vec![
            para(vec![run("section one body", RunProps::default())]),
            break_para,
            para(vec![run("section two body", RunProps::default())]),
        ];
        let mut o = opts(100);
        o.page_view = true;
        let plain: Vec<String> = render(&doc(body), &o).iter().map(|l| l.plain()).collect();
        let tops: Vec<usize> = plain
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim_start().starts_with('┌'))
            .map(|(i, _)| i)
            .collect();
        let bots: Vec<usize> = plain
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim_start().starts_with('└'))
            .map(|(i, _)| i)
            .collect();
        assert!(
            tops.len() >= 2 && bots.len() >= 2,
            "expected ≥2 page boxes (2 sections)"
        );
        let h1 = bots[0] - tops[0]; // landscape (shorter)
        let h2 = bots[1] - tops[1]; // portrait (taller)
        assert!(
            h1 < h2,
            "landscape section page should be shorter than portrait: {h1} vs {h2}"
        );
    }

    #[test]
    fn print_layout_uses_first_and_even_header_variants() {
        // With titlePg + evenAndOdd: page 1 = first, page 2 = even, page 3 = default.
        let body: Vec<Block> = (0..120)
            .map(|i| para(vec![run(&format!("line {i}"), RunProps::default())]))
            .collect();
        let mut o = opts(50);
        o.page_view = true;
        o.title_page = true;
        o.even_odd = true;
        o.headers.default =
            std::rc::Rc::new(vec![para(vec![run("ODD-HEADER", RunProps::default())])]);
        o.headers.first =
            std::rc::Rc::new(vec![para(vec![run("FIRST-HEADER", RunProps::default())])]);
        o.headers.even =
            std::rc::Rc::new(vec![para(vec![run("EVEN-HEADER", RunProps::default())])]);
        let plain: Vec<String> = render(&doc(body), &o).iter().map(|l| l.plain()).collect();
        let first = plain
            .iter()
            .position(|l| l.contains("FIRST-HEADER"))
            .expect("first header");
        let even = plain
            .iter()
            .position(|l| l.contains("EVEN-HEADER"))
            .expect("even header");
        let odd = plain
            .iter()
            .position(|l| l.contains("ODD-HEADER"))
            .expect("default header");
        assert!(
            first < even && even < odd,
            "variant order: first={first} even={even} odd={odd}"
        );
    }

    #[test]
    fn print_layout_paginates_long_documents() {
        // Many short paragraphs must span more than one page box.
        let body: Vec<Block> = (0..200)
            .map(|i| para(vec![run(&format!("line {i}"), RunProps::default())]))
            .collect();
        let mut o = opts(60);
        o.page_view = true;
        let plain: Vec<String> = render(&doc(body), &o).iter().map(|l| l.plain()).collect();
        let tops = plain
            .iter()
            .filter(|l| l.trim_start().starts_with('┌'))
            .count();
        assert!(tops >= 2, "expected multiple pages, got {tops}");
        assert!(plain.iter().any(|l| l.contains("Page 2 of")));
    }

    // ---- caret map tests ----

    fn seg(map: &LineMap) -> &LineSeg {
        &map.segs[0]
    }

    #[test]
    fn map_locates_offsets_on_single_line() {
        let d = doc(vec![para(vec![run("hello", RunProps::default())])]);
        let (_lines, maps) = render_mapped(&d, &opts(40));
        let s = seg(&maps[0]);
        assert_eq!(s.path, vec![0]);
        assert_eq!(s.start, 0);
        assert_eq!(s.nchars(), 5);
        assert_eq!(s.col_for_offset(0), Some(0));
        assert_eq!(s.col_for_offset(5), Some(5));
        assert_eq!(s.offset_for_col(3), 3);
    }

    #[test]
    fn map_tracks_offsets_across_wrap() {
        let d = doc(vec![para(vec![run("aaaa bbbb", RunProps::default())])]);
        let (_lines, maps) = render_mapped(&d, &opts(5));
        assert_eq!(maps.len(), 2);
        assert_eq!(seg(&maps[0]).start, 0);
        assert_eq!(seg(&maps[1]).start, 5);
        assert_eq!(seg(&maps[1]).col_for_offset(5), Some(0));
    }

    #[test]
    fn map_accounts_for_list_prefix() {
        let mut p = Paragraph {
            props: ParProps::default(),
            content: vec![run("x", RunProps::default())],
        };
        p.props.num_id = Some(1);
        let d = doc(vec![Block::Paragraph(p)]);
        let (_lines, maps) = render_mapped(&d, &opts(20));
        assert_eq!(seg(&maps[0]).col0, 2);
        assert_eq!(seg(&maps[0]).col_for_offset(0), Some(2));
    }

    #[test]
    fn map_handles_empty_paragraph() {
        let d = doc(vec![Block::Paragraph(Paragraph::default())]);
        let (_lines, maps) = render_mapped(&d, &opts(20));
        let s = seg(&maps[0]);
        assert_eq!(s.path, vec![0]);
        assert_eq!(s.nchars(), 0);
        assert_eq!(s.col_for_offset(0), Some(0));
    }

    #[test]
    fn table_row_line_maps_each_cell_to_its_path() {
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let t = Table {
            grid: vec![100, 100],
            rows: vec![Row {
                cells: vec![cell("A"), cell("B")],
            }],
        };
        let d = doc(vec![Block::Table(t)]);
        let (_lines, maps) = render_mapped(&d, &opts(30));
        // Find the one row line that has two editable segments.
        let row = maps
            .iter()
            .find(|m| m.segs.len() == 2)
            .expect("a row line with two cells");
        assert_eq!(row.segs[0].path, vec![0, 0, 0, 0]); // table0, row0, cell0, para0
        assert_eq!(row.segs[1].path, vec![0, 0, 1, 0]);
        // Cell B's segment starts to the right of cell A's.
        assert!(row.segs[1].col0 > row.segs[0].col0);
    }
}
