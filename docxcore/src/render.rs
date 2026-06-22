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
        self.spans.iter().map(|s| str_width(&s.text)).sum()
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
    /// Marks an image placeholder line. Pagination keeps a contiguous run of these
    /// together on one page when the image fits, rather than cutting it.
    pub image: bool,
    /// A wide-table line that is allowed to extend past the page's right border
    /// (instead of being clipped) when the table doesn't fit the page width.
    pub overflow: bool,
}

impl LineMap {
    fn one(seg: LineSeg) -> Self {
        LineMap {
            segs: vec![seg],
            page_break: false,
            image: false,
            overflow: false,
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
/// 1-cell border). A tall image split across page boundaries yields one
/// `ImageBox` per page, each a vertical slice of the same source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBox {
    /// Relationship id (`r:embed` / `r:id`) used to resolve the media bytes.
    pub rid: String,
    pub row: usize,
    pub col: usize,
    pub cols: usize,
    /// Height of *this* slice in cells (the whole image when not split).
    pub rows: usize,
    /// Cell-rows of the image lying above this slice (0 for the first/only slice).
    /// The app crops the source from here so each page shows its own band.
    pub src_row: usize,
    /// The image's full height in cells across all slices (== `rows` when whole).
    /// The app scales the decoded source to this height before cropping.
    pub full_rows: usize,
    /// The document defines a visible outline for this image. The app draws a
    /// border around the picture; otherwise pictures are borderless and a box is
    /// drawn only as the fallback when the pixels can't be rendered.
    pub bordered: bool,
    /// Fallback caption (e.g. `image 320×160`) drawn in the box when the picture
    /// can't be rendered.
    pub label: String,
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
        let m = page_metrics(opts.width, geom);
        let content_width = m.content_cols;
        // Newspaper columns: render the section at the narrower column width, then
        // flow the resulting lines into N side-by-side columns per page.
        let ncols = geom.cols.max(1) as usize;
        let col_layout = (ncols > 1).then(|| {
            let text_tw = (geom.w - geom.ml - geom.mr).max(1) as f32;
            let gap = ((geom.col_space as f32) * content_width as f32 / text_tw).round() as usize;
            let gap = gap.clamp(1, content_width / 4);
            let col_w = (content_width.saturating_sub((ncols - 1) * gap) / ncols).max(6);
            (ncols, gap, col_w, m.content_rows)
        });
        let render_w = col_layout.map(|(_, _, w, _)| w).unwrap_or(content_width);

        let mut sec_imgs = Vec::new();
        // The slice rebases paragraph paths to 0, so the path-keyed selection and
        // list markers must be rebased to match (else they silently miss every
        // paragraph after the first section break).
        let sec_opts = section_opts(opts, start, end);
        let mut pairs = render_blocks(
            &doc.body[start..end],
            &[],
            render_w,
            &sec_opts,
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
        if let Some((n, gap, col_w, rows)) = col_layout {
            // Pixel-image overlays can't follow the column reflow; drop them (the
            // placeholder space remains). Text/charts flow as normal content.
            sec_imgs.clear();
            pairs = columnize(pairs, n, gap, col_w, rows);
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

/// A copy of `opts` with path-keyed inputs (selection, list markers) rebased to a
/// section slice `[start, end)`: the top-level path index is shifted down by
/// `start` and entries outside the section are dropped, so they line up with the
/// slice's local (0-based) paragraph paths.
fn section_opts(opts: &RenderOptions, start: usize, end: usize) -> RenderOptions {
    let rebase = |p: &[usize]| -> Option<Vec<usize>> {
        let first = *p.first()?;
        (start..end).contains(&first).then(|| {
            let mut lp = p.to_vec();
            lp[0] = first - start;
            lp
        })
    };
    let selection = opts
        .selection
        .iter()
        .filter_map(|(p, s, e)| rebase(p).map(|lp| (lp, *s, *e)))
        .collect();
    let mut markers = HashMap::new();
    for (p, m) in opts.list_markers.iter() {
        if let Some(lp) = rebase(p) {
            markers.insert(lp, m.clone());
        }
    }
    let mut so = opts.clone();
    so.selection = selection;
    so.list_markers = Rc::new(markers);
    so
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
    /// Set on the first cell of a small inline image (e.g. an equation) embedded
    /// in the text flow. The remaining cells are blank fillers reserving its width.
    img: Option<Rc<InlineImg>>,
}

/// A small inline picture placed within a line of text (its width is reserved by
/// blank filler glyphs; the app paints the pixels over them).
struct InlineImg {
    rid: String,
    cols: usize,
    rows: usize,
    bordered: bool,
    label: String,
}

/// Display width of a character in terminal cells: 2 for East Asian wide /
/// fullwidth glyphs (CJK, Hangul, kana, fullwidth forms, most emoji), 0 for
/// combining marks, 1 otherwise. A compact approximation of `unicode-width`.
fn char_width(c: char) -> usize {
    let u = c as u32;
    if u == 0 {
        return 0;
    }
    if (0x0300..=0x036F).contains(&u) || (0x200B..=0x200F).contains(&u) {
        return 0; // combining marks / zero-width
    }
    let wide = matches!(u,
        0x1100..=0x115F   // Hangul Jamo
        | 0x2E80..=0x303E // CJK radicals, Kangxi, CJK punctuation
        | 0x3041..=0x33FF // kana, CJK symbols
        | 0x3400..=0x4DBF // CJK ext A
        | 0x4E00..=0x9FFF // CJK unified
        | 0xA000..=0xA4CF // Yi
        | 0xAC00..=0xD7A3 // Hangul syllables
        | 0xF900..=0xFAFF // CJK compatibility
        | 0xFE10..=0xFE19 // vertical forms
        | 0xFE30..=0xFE6F // CJK compatibility / small forms
        | 0xFF00..=0xFF60 // fullwidth forms
        | 0xFFE0..=0xFFE6 // fullwidth signs
        | 0x1F300..=0x1FAFF // emoji & pictographs
        | 0x20000..=0x3FFFD // CJK ext B and beyond
    );
    if wide { 2 } else { 1 }
}

/// Display width of a string in terminal cells.
fn str_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Display width of a glyph (its shown char), in terminal cells.
fn glyph_w(g: &Glyph) -> usize {
    char_width(g.disp.unwrap_or(g.ch))
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

/// Visible width (chars) of the inline content following index `from`, up to the
/// next tab or break — used to right/center-align text at a tab stop.
fn following_inline_width(content: &[Inline], from: usize) -> usize {
    let mut w = 0;
    for it in &content[(from + 1).min(content.len())..] {
        match it {
            Inline::Run(r) => w += str_width(&r.text),
            Inline::Hyperlink(h) => w += h.runs.iter().map(|r| str_width(&r.text)).sum::<usize>(),
            Inline::Tab | Inline::Break(_) => break,
            Inline::Equation { text, .. } => w += str_width(text),
            Inline::SmartArt { .. }
            | Inline::Chart { .. }
            | Inline::TextBox { .. }
            | Inline::Raw(_) => {}
        }
    }
    w
}

fn flatten_para(
    para: &Paragraph,
    opts: &RenderOptions,
    heading: bool,
    sel: &[(usize, usize)],
    avail: usize,
) -> Vec<Seg> {
    let inv = opts.show_invisibles;
    // Tab stops (direct `w:tabs`, else the paragraph style's) projected to
    // columns. The page text width maps to `avail` cells, so a right tab near the
    // margin lands near the right edge in either view.
    let tab_stops = if para.props.tabs.is_empty() {
        opts.styles.effective_tabs(para.props.style_id.as_deref())
    } else {
        para.props.tabs.clone()
    };
    let text_twips = (opts.page.w - opts.page.ml - opts.page.mr).max(1) as f32;
    // With invisibles on, a trailing `¶` (and `↵` at line breaks) takes a cell, so
    // a right tab must stop one column short of the edge or its right-aligned text
    // (e.g. a TOC page number) overflows and wraps to the next row.
    let tab_limit = avail.saturating_sub(if inv { 1 } else { 0 });
    let tab_col = |pos: i32| {
        ((pos.max(0) as f32 * (avail as f32 / text_twips)).round() as usize).min(tab_limit)
    };
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
                img: None,
            }
        } else {
            Glyph {
                ch,
                disp: None,
                style: style.clone(),
                link,
                src: Some(mc),
                img: None,
            }
        };
        g.style.highlight = sel_at(mc);
        g
    };

    for (idx, item) in para.content.iter().enumerate() {
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
                let cur = segs.last().unwrap().glyphs.len();
                // Next tab stop strictly beyond the current column, else a default
                // stop every 8 cells.
                let stop = tab_stops
                    .iter()
                    .map(|t| (tab_col(t.pos), t.align, t.leader))
                    .filter(|(col, _, _)| *col > cur)
                    .min_by_key(|(col, _, _)| *col)
                    .unwrap_or(((cur / 8 + 1) * 8, TabAlign::Left, TabLeader::None));
                let (target, align, leader) = stop;
                let fw = following_inline_width(&para.content, idx);
                // Right/center tabs right-align the following text to the stop.
                let fill_to = match align {
                    TabAlign::Right => target.saturating_sub(fw),
                    TabAlign::Center => target.saturating_sub(fw / 2),
                    TabAlign::Left => target,
                };
                let fill_ch = match leader {
                    TabLeader::Dot => '.',
                    TabLeader::Hyphen => '-',
                    TabLeader::Underscore => '_',
                    TabLeader::None => ' ',
                };
                let end = fill_to.max(cur + 1).min(avail.max(cur + 1));
                let seg = &mut segs.last_mut().unwrap().glyphs;
                for col in cur..end {
                    let (ch, mut style) = if inv && col == cur {
                        ('→', invis_style())
                    } else if leader == TabLeader::None {
                        (' ', Style::default())
                    } else {
                        (fill_ch, dim_style())
                    };
                    style.highlight = hl;
                    seg.push(Glyph {
                        ch,
                        disp: None,
                        style,
                        link: None,
                        src: Some(mc),
                        img: None,
                    });
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
                                img: None,
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
            // A small inline picture (e.g. an equation) flows in the text: reserve
            // its width with blank, non-breaking filler cells and tag the first so
            // the line emitter can place the overlay. Larger images and SmartArt are
            // drawn as blocks after the paragraph instead.
            Inline::Raw(raw) => {
                if let Some(img) = inline_image(raw) {
                    let rc = Rc::new(img);
                    let w = rc.cols;
                    for i in 0..w.max(1) {
                        segs.last_mut().unwrap().glyphs.push(Glyph {
                            ch: '\u{00a0}', // non-breaking: keep the image on one line
                            disp: Some(' '),
                            style: Style::default(),
                            link: None,
                            src: None,
                            img: (i == 0).then(|| rc.clone()),
                        });
                    }
                }
            }
            // A decoded equation flows as ordinary (non-editable) text at body size.
            Inline::Equation { text, .. } => {
                let st = Style::default();
                for ch in text.chars() {
                    segs.last_mut().unwrap().glyphs.push(Glyph {
                        ch,
                        disp: None,
                        style: st.clone(),
                        link: None,
                        src: None,
                        img: None,
                    });
                }
            }
            // Rendered as a box after the paragraph, not in the inline flow.
            Inline::SmartArt { .. } | Inline::Chart { .. } | Inline::TextBox { .. } => {}
        }
    }
    if inv {
        segs.last_mut().unwrap().glyphs.push(Glyph {
            ch: '¶',
            disp: None,
            style: invis_style(),
            link: None,
            src: None,
            img: None,
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
    let mut cur_w = 0usize; // display width of `cur`
    let mut last_space: Option<usize> = None;
    let row_w = |gs: &[Glyph]| gs.iter().map(glyph_w).sum::<usize>();
    for g in glyphs {
        cur.push(g.clone());
        cur_w += glyph_w(g);
        if g.ch == ' ' {
            last_space = Some(cur.len() - 1);
        }
        if cur_w > width {
            if let Some(sp) = last_space {
                let rest = cur.split_off(sp + 1);
                while cur.last().map(|g| g.ch == ' ').unwrap_or(false) {
                    cur.pop();
                }
                lines.push(std::mem::take(&mut cur));
                cur = rest;
                cur_w = row_w(&cur);
                last_space = cur.iter().rposition(|g| g.ch == ' ');
            } else {
                let last = cur.pop().unwrap();
                lines.push(std::mem::take(&mut cur));
                cur_w = glyph_w(&last);
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
        let w = glyph_w(g);
        if let Some(s) = g.src {
            if last_src != Some(s) {
                if start.is_none() {
                    start = Some(s);
                }
                cols.push(col);
                last_src = Some(s);
            }
            last_end = col + w;
        }
        col += w;
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
    // Honor the requested width down to a single cell — narrow table columns rely
    // on wrapping to their exact width, else cells overflow and break the borders.
    let avail = width.saturating_sub(prefix_w).max(1);

    let local_sel: Vec<(usize, usize)> = opts
        .selection
        .iter()
        .filter(|(p, _, _)| p == path)
        .map(|(_, s, e)| (*s, *e))
        .collect();
    let segs = flatten_para(para, opts, heading.is_some(), &local_sel, avail);

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
            let body_w = gl.iter().map(glyph_w).sum::<usize>();
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
            // Place any inline images sitting on this line and reserve the rows
            // their pixels extend below the text baseline.
            let row = out.len();
            let mut reserve = 0usize;
            for (i, g) in gl.iter().enumerate() {
                if let Some(img) = &g.img {
                    images.push(ImageBox {
                        rid: img.rid.clone(),
                        row,
                        col: prefix_cols + i,
                        cols: img.cols,
                        rows: img.rows,
                        src_row: 0,
                        full_rows: img.rows,
                        bordered: img.bordered,
                        label: img.label.clone(),
                    });
                    reserve = reserve.max(img.rows.saturating_sub(1));
                }
            }
            out.push((line, LineMap::one(lseg)));
            for _ in 0..reserve {
                out.push((Line { spans: Vec::new() }, LineMap::default()));
            }
            line_idx += 1;
        }
    }
    // Drawings (images) inside the paragraph become a sized placeholder box,
    // emitted after the paragraph text. (Real pixels are overlaid by the app.)
    for (idx, item) in para.content.iter().enumerate() {
        // A SmartArt diagram: the shapes can't be drawn in a terminal, so show the
        // diagram's node text in a labeled (non-editable) box.
        if let Inline::SmartArt { text, .. } = item {
            let blocks = smartart_blocks(text);
            out.extend(text_box(&blocks, None, width, opts, images));
            continue;
        }
        // A chart: drawn as a text bar/pie view in a (non-editable) box.
        if let Inline::Chart { chart, .. } = item {
            let lines = crate::chart::render_chart(chart, width.saturating_sub(6).max(16));
            let blocks = chart_blocks(&lines);
            out.extend(text_box(&blocks, None, width, opts, images));
            continue;
        }
        // A text box: its content is editable, addressed by the host paragraph's
        // path plus this inline index, so the box's text is selectable.
        if let Inline::TextBox { blocks, .. } = item {
            let mut base = path.to_vec();
            base.push(idx);
            out.extend(text_box(blocks, Some(&base), width, opts, images));
            continue;
        }
        if let Inline::Raw(raw) = item {
            // Small inline pictures (equations) were already placed in the text
            // flow above; only larger images get their own block box here.
            if inline_image(raw).is_some() {
                continue;
            }
            if let Some((pw, ph)) = raw_image_extent(raw) {
                let (pw, ph) = (pw as usize, ph as usize);
                let (cols, rows) = image_box_cells(pw, ph, width);
                let bordered = raw_has_outline(raw);
                let label = format!("image {pw}×{ph}");
                // A bordered picture's pixels sit inside the document's outline; a
                // borderless one fills the whole region. The box is always reported
                // (even with no rid) so the app can draw a fallback for what it can't
                // render. The overlay is aligned so pixels never spill onto the frame.
                let (row, col, ocols, orows) = if bordered {
                    (out.len() + 1, 1, cols, rows.saturating_sub(2).max(1))
                } else {
                    (out.len(), 0, cols + 2, rows)
                };
                images.push(ImageBox {
                    rid: embed_rid(raw).unwrap_or_default(),
                    row,
                    col,
                    cols: ocols,
                    rows: orows,
                    src_row: 0,
                    full_rows: orows,
                    bordered,
                    label: label.clone(),
                });
                // The placeholder text carries a box only when the document defines
                // one; otherwise it is blank and the app draws the pixels (or a
                // fallback box if it can't render them).
                let mut boxed = if bordered {
                    image_box(cols, rows, &label)
                } else {
                    blank_box(rows)
                };
                for (_, map) in &mut boxed {
                    map.image = true;
                }
                out.extend(boxed);
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
/// Turn a SmartArt diagram's node text into renderable blocks: a "SmartArt"
/// caption followed by one paragraph per node, so `text_box` can frame it.
fn smartart_blocks(text: &[String]) -> Vec<Block> {
    use crate::model::{Inline, Paragraph, Run, RunProps};
    let para = |s: &str, bold: bool| {
        Block::Paragraph(Paragraph {
            props: Default::default(),
            content: vec![Inline::Run(Run {
                text: s.to_string(),
                props: RunProps {
                    bold,
                    ..Default::default()
                },
            })],
        })
    };
    let mut blocks = vec![para("SmartArt", true)];
    blocks.extend(text.iter().map(|t| para(t, false)));
    blocks
}

/// Build the box content for a chart: the heading line bold, the bars below it
/// as monospace rows (a leading run keeps spacing intact).
fn chart_blocks(lines: &[String]) -> Vec<Block> {
    use crate::model::{Inline, Paragraph, Run, RunProps};
    lines
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Block::Paragraph(Paragraph {
                props: Default::default(),
                content: vec![Inline::Run(Run {
                    text: s.clone(),
                    props: RunProps {
                        bold: i == 0,
                        ..Default::default()
                    },
                })],
            })
        })
        .collect()
}

/// Render `blocks` inside a dim border. When `base` is `Some`, the content keeps
/// its caret map (rebased to that path prefix and shifted right past the `│ `
/// frame) so the box's text is editable/selectable; `None` makes it display-only
/// (e.g. a SmartArt caption, whose paragraphs aren't part of the model).
fn text_box(
    blocks: &[Block],
    base: Option<&[usize]>,
    width: usize,
    opts: &RenderOptions,
    images: &mut Vec<ImageBox>,
) -> Vec<(Line, LineMap)> {
    const FRAME: usize = 2; // the leading "│ "
    let inner = width.saturating_sub(4).max(8);
    let content = render_blocks(blocks, base.unwrap_or(&[]), inner, opts, images);
    let mut out = Vec::new();
    out.push((
        Line {
            spans: vec![Line::dim_span(format!("┌{}┐", "─".repeat(inner + 2)))],
        },
        LineMap::default(),
    ));
    for (ln, mut map) in content {
        let pad = inner.saturating_sub(ln.width());
        let mut spans = vec![Line::dim_span("│ ".to_string())];
        spans.extend(ln.spans);
        spans.push(Line::dim_span(format!("{} │", " ".repeat(pad))));
        // Keep the content editable only for a real (modeled) text box.
        let map = if base.is_some() {
            for seg in &mut map.segs {
                seg.col0 += FRAME;
            }
            map
        } else {
            LineMap::default()
        };
        out.push((Line { spans }, map));
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
/// Size a block image's placeholder box (interior `cols`×`rows`, in cells) from
/// its pixel size and the available width. A terminal cell is ~8×16px (2:1
/// tall:wide), so `pw/8` × `ph/16` preserves aspect ratio. Only larger pictures
/// reach this path; small ones (e.g. equations) flow inline via [`inline_image`].
fn image_box_cells(pw: usize, ph: usize, width: usize) -> (usize, usize) {
    let max_cols = width.saturating_sub(2).max(10);
    let cols = (pw / 8).clamp(10, max_cols);
    let rows = (ph / 16).clamp(3, 24);
    (cols, rows)
}

/// A small inline picture (e.g. an equation) that should flow within the text
/// line, at its natural size (a cell is ~8×16px). Returns `None` for images too
/// tall to sit inline, which are drawn as their own block instead.
fn inline_image(raw: &str) -> Option<InlineImg> {
    const INLINE_MAX_ROWS: usize = 3;
    let (pw, ph) = raw_image_extent(raw)?;
    let (pw, ph) = (pw as f32, ph as f32);
    let rows = (ph / 16.0).round().max(1.0) as usize;
    if rows > INLINE_MAX_ROWS {
        return None;
    }
    // Width preserves aspect at that height (a cell is ~2× taller than wide).
    let cols = ((pw / ph) * rows as f32 * 2.0).round().clamp(1.0, 40.0) as usize;
    Some(InlineImg {
        rid: embed_rid(raw).unwrap_or_default(),
        cols,
        rows,
        bordered: raw_has_outline(raw),
        label: format!("image {}×{}", pw as usize, ph as usize),
    })
}

/// Whether the document gives a picture a visible outline. Conservative: when in
/// doubt, treat it as borderless (the default we want). VML shapes are bordered
/// only with an explicit `stroked="t"`; DrawingML pictures only with an `<a:ln>`
/// that has a real fill (not `<a:noFill/>`).
fn raw_has_outline(raw: &str) -> bool {
    if raw.contains("stroked=\"t\"") {
        return true;
    }
    if let Some(i) = raw.find("<a:ln") {
        let seg = &raw[i..(i + 400).min(raw.len())];
        return seg.contains("solidFill") && !seg.contains("noFill");
    }
    false
}

/// A blank, borderless image placeholder of `rows` lines. It only reserves the
/// vertical space; the app paints the picture (or a fallback box) over it.
fn blank_box(rows: usize) -> Vec<(Line, LineMap)> {
    (0..rows.max(1))
        .map(|_| (Line { spans: Vec::new() }, LineMap::default()))
        .collect()
}

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
    bordered: bool,
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
        let raw = match b {
            Block::Paragraph(p) => p.content.iter().find_map(|it| match it {
                Inline::Raw(r) => Some(r.as_str()),
                _ => None,
            }),
            _ => None,
        };
        let rid = raw.and_then(embed_rid).unwrap_or_default();
        let bordered = raw.map(raw_has_outline).unwrap_or(false);
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
            bordered,
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
        // Only stamp an outline into the canvas when the document defines one;
        // otherwise the picture is borderless (the app paints it, or a fallback
        // box if it can't).
        if p.bordered {
            draw_box_into(&mut grid, top, p.c, p.w, p.h, &p.label);
        }
        {
            let (row, col, w, h) = if p.bordered {
                // Inside the outline.
                (top + 1, p.c + 1, (p.w - 1).max(1), (p.h - 1).max(1))
            } else {
                (top, p.c, p.w.max(1), p.h.max(1))
            };
            let rows = h.max(0) as usize;
            images.push(ImageBox {
                rid: p.rid.clone(),
                row: row.max(0) as usize,
                col: col.max(0) as usize,
                cols: w as usize,
                rows,
                src_row: 0,
                full_rows: rows,
                bordered: p.bordered,
                label: p.label.clone(),
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
    // In page layout, a top-level table that can't give each column a readable
    // width is rendered wider than the page and allowed to extend past the right
    // border (capped at the terminal), rather than crammed into 1-2 cell columns.
    const MIN_COL: usize = 8;
    let top_level = table_path.len() <= 1;
    let eff_width = if opts.page_view && top_level {
        let desired = ncols * MIN_COL + overhead;
        desired.clamp(width, opts.width.saturating_sub(2).max(width))
    } else {
        width
    };
    let overflow = eff_width > width;
    let content_total = eff_width.saturating_sub(overhead).max(ncols);
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
                    // Clip the cell content to the cell width so nothing — e.g. a
                    // nested table too wide to fit — can push the outer grid out
                    // of alignment.
                    let (clipped, used) = clip_to_cols(cline.spans.clone(), cw);
                    line.spans.extend(clipped);
                    if cw > used {
                        line.spans.push(Line::text_span(" ".repeat(cw - used)));
                    }
                    for seg in &cmap.segs {
                        // Drop segments that fall entirely past the clip (their text
                        // isn't shown), and keep the rest aligned to the cell.
                        if seg.col0 >= cw {
                            continue;
                        }
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
    // Wide table: let pagination know these lines may extend past the page border.
    if overflow {
        for (_, m) in &mut out {
            m.overflow = true;
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

/// Truncate a line's spans to at most `max` display columns, returning the
/// clipped spans and their width. Keeps the page frame rigid: content that
/// overruns the text width (e.g. an invisibles `¶`/`¤` marker spilling out of a
/// table cell or text box) is cut at the margin instead of pushing the border.
fn clip_to_cols(spans: Vec<Span>, max: usize) -> (Vec<Span>, usize) {
    let mut out = Vec::new();
    let mut used = 0;
    for sp in spans {
        if used >= max {
            break;
        }
        let w = str_width(&sp.text);
        if used + w <= max {
            used += w;
            out.push(sp);
        } else {
            // Partial span: keep whole chars while they fit (never split a
            // double-width glyph across the boundary).
            let mut text = String::new();
            for ch in sp.text.chars() {
                let cw = char_width(ch);
                if used + cw > max {
                    break;
                }
                text.push(ch);
                used += cw;
            }
            out.push(Span {
                text,
                style: sp.style,
                link: sp.link,
            });
            break;
        }
    }
    (out, used)
}

/// Flow a section's single tall column of lines into `n` newspaper columns of
/// `col_w` cells each, `gap` cells apart, `rows` lines tall per column. Column
/// `k` of a page sits at x-offset `k*(col_w+gap)`; columns fill top-to-bottom
/// then left-to-right, advancing to a new page after `n` full columns. Each
/// output line is one merged screen row (its map carries every column's
/// segments, shifted to their column's x), so the caret still maps correctly.
fn columnize(
    pairs: Vec<(Line, LineMap)>,
    n: usize,
    gap: usize,
    col_w: usize,
    rows: usize,
) -> Vec<(Line, LineMap)> {
    if n <= 1 || rows == 0 || pairs.is_empty() {
        return pairs;
    }
    let total = pairs.len();
    let per_page = rows * n;
    let pages = total.div_ceil(per_page);
    let mut out: Vec<(Line, LineMap)> = Vec::new();
    for page in 0..pages {
        let base = page * per_page;
        for r in 0..rows {
            let mut spans: Vec<Span> = Vec::new();
            let mut segs: Vec<LineSeg> = Vec::new();
            for k in 0..n {
                let x_off = k * (col_w + gap);
                let li = base + k * rows + r;
                if li < total {
                    let (line, map) = &pairs[li];
                    let (clipped, w) = clip_to_cols(line.spans.clone(), col_w);
                    spans.extend(clipped);
                    if w < col_w {
                        spans.push(Line::text_span(" ".repeat(col_w - w)));
                    }
                    for seg in &map.segs {
                        let mut s = seg.clone();
                        s.col0 += x_off;
                        segs.push(s);
                    }
                } else {
                    spans.push(Line::text_span(" ".repeat(col_w)));
                }
                if k + 1 < n {
                    spans.push(Line::text_span(" ".repeat(gap)));
                }
            }
            out.push((
                Line { spans },
                LineMap {
                    segs,
                    ..Default::default()
                },
            ));
        }
    }
    out
}

fn paginate(
    pairs: Vec<(Line, LineMap)>,
    opts: &RenderOptions,
    geom: PageGeom,
    images: &mut Vec<ImageBox>,
    pl: &PageLines,
) -> Vec<(Line, LineMap)> {
    let m = page_metrics(opts.width, geom);
    let inner_w = m.content_cols + m.ml + m.mr;
    let pad = |n: usize| " ".repeat(n);
    let lead = pad(m.center);
    // Frame a header/footer/content line into a page row (left margin + content
    // + right margin, between the borders). Non-editable (no caret map).
    let frame_line = |ln: &Line| -> (Line, LineMap) {
        let (clipped, w) = clip_to_cols(ln.spans.clone(), m.content_cols);
        let rpad = m.content_cols - w;
        let mut spans = vec![Line::dim_span(format!("{lead}│{}", pad(m.ml)))];
        spans.extend(clipped);
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

    // The height of the contiguous image run starting at each line (0 if the line
    // is not the first of an image run), so a whole image that fits on a page can
    // be kept together rather than cut at the boundary.
    let img_flags: Vec<bool> = pairs.iter().map(|(_, m)| m.image).collect();
    let run_height = |i: usize| -> usize {
        if !img_flags[i] || (i > 0 && img_flags[i - 1]) {
            return 0; // not the start of an image run
        }
        let mut h = 0;
        while i + h < img_flags.len() && img_flags[i + h] {
            h += 1;
        }
        h
    };

    // Assign each content line to a page, honoring hard page breaks (the marker
    // lines force the next line onto a new page and are themselves dropped) and
    // keeping page-sized images intact.
    let mut items: Vec<(usize, Line, LineMap, usize)> = Vec::new(); // (idx, line, map, page)
    let mut line_page = vec![usize::MAX; total_in];
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
        // An image that fits on a page but not in the space left starts the next
        // page instead of being split. Taller-than-page images fall through and
        // are cut across pages below.
        let h = run_height(idx);
        if h > 0 && h <= m.content_rows && row > 0 && row + h > m.content_rows {
            pg += 1;
            row = 0;
        }
        if row == m.content_rows {
            pg += 1;
            row = 0;
        }
        line_page[idx] = pg;
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
            let mut spans = vec![Line::dim_span(format!("{lead}│{}", pad(m.ml)))];
            if map.overflow {
                // A wide table line: keep it whole and let it run past the right
                // border (no clip, no right frame) — the page extends rightward.
                spans.extend(ln.spans);
            } else {
                let (clipped, w) = clip_to_cols(ln.spans, m.content_cols);
                let rpad = m.content_cols - w;
                spans.extend(clipped);
                spans.push(Line::dim_span(format!("{}│", pad(rpad + m.mr))));
            }
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

    // Remap image placements into the paginated layout. An image whose interior
    // lines landed on more than one page is emitted as one slice per page (each a
    // vertical band of the same source) so it is cut cleanly at the boundary and
    // never drawn over a page border.
    let mut placed_imgs: Vec<ImageBox> = Vec::new();
    for ib in images.iter() {
        let mut seg_start = ib.row;
        let end = ib.row + ib.rows;
        while seg_start < end {
            let Some(page) = line_page
                .get(seg_start)
                .copied()
                .filter(|&p| p != usize::MAX)
            else {
                seg_start += 1;
                continue;
            };
            // Extend the slice while lines stay on the same page and are placed.
            let mut seg_end = seg_start + 1;
            while seg_end < end
                && line_page.get(seg_end).copied() == Some(page)
                && new_row.get(seg_end).copied().unwrap_or(usize::MAX) != usize::MAX
            {
                seg_end += 1;
            }
            if let Some(&nr) = new_row.get(seg_start) {
                if nr != usize::MAX {
                    placed_imgs.push(ImageBox {
                        rid: ib.rid.clone(),
                        row: nr,
                        col: ib.col + col_off,
                        cols: ib.cols,
                        rows: seg_end - seg_start,
                        src_row: ib.src_row + (seg_start - ib.row),
                        full_rows: ib.full_rows,
                        bordered: ib.bordered,
                        label: ib.label.clone(),
                    });
                }
            }
            seg_start = seg_end;
        }
    }
    *images = placed_imgs;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_chars_are_double_width_and_clip_safely() {
        assert_eq!(char_width('A'), 1);
        assert_eq!(char_width('哈'), 2);
        assert_eq!(str_width("a哈b"), 4);
        // clip by display width, never splitting a wide glyph
        let spans = vec![Line::text_span("哈巴谷".to_string())];
        let (out, w) = clip_to_cols(spans, 3);
        assert_eq!(w, 2); // one wide char fits; the next needs 2 more cells
        assert_eq!(out[0].text, "哈");
    }

    #[test]
    fn columnize_lays_lines_into_columns_with_shifted_maps() {
        // six single-char lines, each an editable segment
        let mk = |c: char| {
            let line = Line {
                spans: vec![Line::text_span(c.to_string())],
            };
            let map = LineMap::one(LineSeg {
                path: vec![0],
                start: 0,
                col0: 0,
                cols: vec![0, 1],
            });
            (line, map)
        };
        let pairs: Vec<_> = "abcdef".chars().map(mk).collect();
        // 2 columns, gap 1, col width 3, 3 rows per page → one page, a/b/c | d/e/f
        let out = columnize(pairs, 2, 1, 3, 3);
        assert_eq!(out.len(), 3);
        let r0 = out[0].0.plain();
        assert!(r0.starts_with('a') && r0.contains('d'), "row0: {r0:?}");
        // the two columns map to different screen x (0 and col_w+gap = 4)
        assert_eq!(out[0].1.segs.len(), 2);
        assert_eq!(out[0].1.segs[0].col0, 0);
        assert_eq!(out[0].1.segs[1].col0, 4);
        assert!(out[1].0.plain().contains('b') && out[1].0.plain().contains('e'));
        assert!(out[2].0.plain().contains('c') && out[2].0.plain().contains('f'));
    }

    #[test]
    fn page_geom_parses_column_count() {
        let g = crate::model::PageGeom::from_sect_pr(
            "<w:sectPr><w:cols w:num=\"3\" w:space=\"425\"/></w:sectPr>",
        );
        assert_eq!(g.cols, 3);
        assert_eq!(g.col_space, 425);
        // absent → single column
        assert_eq!(crate::model::PageGeom::from_sect_pr("<w:sectPr/>").cols, 1);
    }

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
        // The picture is reported as an image region (with a fallback caption) but,
        // having no document-defined outline, draws no border in the text itself.
        let (lines, _m, imgs) = render_with_images(&d, &opts(60));
        let joined: String = lines
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(imgs.len(), 1, "expected one image region");
        assert!(!imgs[0].bordered);
        assert_eq!(imgs[0].label, "image 320×160");
        assert!(
            !joined.contains('┌'),
            "borderless image should draw no box: {joined:?}"
        );

        // A non-drawing raw (e.g. a bookmark) is not an image at all.
        let d2 = doc(vec![para(vec![Inline::Raw(
            "<w:bookmarkStart/>".to_string(),
        )])]);
        let (_l, _m, imgs2) = render_with_images(&d2, &opts(60));
        assert!(imgs2.is_empty());
    }

    #[test]
    fn drawing_with_outline_keeps_its_border() {
        // A picture the document outlines (VML stroked) keeps a drawn border.
        let raw = "<w:r><w:pict><v:shape style=\"width:240pt;height:120pt\" stroked=\"t\">\
                   <v:imagedata r:id=\"r\"/></v:shape></w:pict></w:r>";
        let d = doc(vec![para(vec![Inline::Raw(raw.to_string())])]);
        let (lines, _m, imgs) = render_with_images(&d, &opts(60));
        let joined: String = lines
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(imgs[0].bordered);
        assert!(
            joined.contains('┌') && joined.contains('┘'),
            "outline not drawn: {joined:?}"
        );
    }

    #[test]
    fn small_inline_equation_flows_in_the_text() {
        // A small inline object (an equation ~93×27px) sits within the line: text
        // before it, a reserved gap for the picture, then text after it — not a
        // detached block below the paragraph.
        let eq = "<w:r><w:object><v:shape style=\"width:69.75pt;height:20.25pt\">\
                  <v:imagedata r:id=\"rEq\"/></v:shape></w:object></w:r>";
        let p = Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content: vec![
                run("If ", RunProps::default()),
                Inline::Raw(eq.to_string()),
                run(", then x.", RunProps::default()),
            ],
        });
        let (lines, _m, imgs) = render_with_images(&doc(vec![p]), &opts(60));
        assert_eq!(imgs.len(), 1, "one inline image");
        let ib = &imgs[0];
        assert_eq!(ib.rid, "rEq");
        assert!(ib.rows <= 3, "inline image should stay small: {ib:?}");
        // It sits on the first line (with the text), not on a block after it.
        let text_row = lines.iter().position(|l| l.plain().contains("If")).unwrap();
        assert_eq!(ib.row, text_row, "image should share the text's line");
        // The picture's column falls between "If " and the following ", then x.".
        assert!(ib.col >= 3, "image should follow 'If ': {ib:?}");
        assert!(
            lines[text_row].plain().contains(", then x."),
            "text continues on the same line after the picture"
        );
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
        let (_l, _m, imgs) = render_with_images(&d, &opts(100));
        assert_eq!(imgs.len(), 2, "expected two image regions: {imgs:?}");
        assert_eq!(imgs[0].row, imgs[1].row, "same y should share a row");
        assert_ne!(
            imgs[0].col, imgs[1].col,
            "far-apart x should differ in column"
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
        let (lines, _m, imgs) = render_with_images(&d, &opts(100));
        let texts: Vec<String> = lines.iter().map(|l| l.plain()).collect();
        let heading = texts
            .iter()
            .position(|s| s.contains("Graphics"))
            .expect("heading line");
        let last_box = imgs.iter().map(|i| i.row).max().expect("an image region");
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
        let (_l, _m, imgs) = render_with_images(&doc(vec![left, right]), &opts(100));
        // The right-aligned image should project to a far-right column.
        let max_c = imgs.iter().map(|i| i.col).max().unwrap_or(0);
        assert!(
            max_c > 50,
            "right-aligned image should be far right, got col {max_c}"
        );
    }

    #[test]
    fn text_box_content_renders_and_is_editable() {
        let raw = "<w:r><w:pict><v:shape><v:textbox><w:txbxContent>\
                   <w:p><w:r><w:t>boxed text</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r>"
            .to_string();
        let tb = Inline::TextBox {
            raw,
            blocks: vec![para(vec![run("boxed text", RunProps::default())])],
        };
        let d = doc(vec![para(vec![tb])]);
        let (lines, maps, _i) = render_with_images(&d, &opts(40));
        let joined: String = lines
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("boxed text"), "text missing:\n{joined}");
        assert!(
            joined.contains('┌') && joined.contains('└'),
            "no frame:\n{joined}"
        );
        // The box's text line carries an editable segment whose path descends into
        // the text box: [host paragraph 0, inline 0, inner paragraph 0].
        let seg = lines
            .iter()
            .zip(maps.iter())
            .find(|(l, _)| l.plain().contains("boxed text"))
            .and_then(|(_, m)| m.segs.first())
            .expect("an editable segment for the box text");
        assert_eq!(seg.path, vec![0, 0, 0]);
    }

    #[test]
    fn vml_image_renders_as_image_box() {
        // Legacy VML image: size comes from the shape's CSS style (192pt × 2in).
        // 192pt = 256px, 2in = 192px.
        let raw = "<w:r><w:pict><v:shape id=\"i\" type=\"#t75\" style=\"width:192pt;height:2in\">\
            <v:imagedata r:id=\"rId7\" o:title=\"\"/></v:shape></w:pict></w:r>";
        let d = doc(vec![para(vec![Inline::Raw(raw.to_string())])]);
        let (_l, _m, imgs) = render_with_images(&d, &opts(60));
        // No explicit stroke → borderless, but reported with rid and caption.
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].rid, "rId7");
        assert!(!imgs[0].bordered);
        assert_eq!(imgs[0].label, "image 256×192");
    }

    #[test]
    fn tab_stop_with_dot_leader_right_aligns() {
        // A TOC-style paragraph: text + tab + page number, with a right tab and
        // dot leader → dots fill and the number sits at the right.
        let styles = crate::styles::parse_styles_xml(
            "<w:styles><w:style w:styleId=\"TOC1\"><w:pPr>\
             <w:tabs><w:tab w:val=\"right\" w:leader=\"dot\" w:pos=\"8630\"/></w:tabs>\
             </w:pPr></w:style></w:styles>",
        );
        let mut o = opts(50);
        o.styles = std::rc::Rc::new(styles);
        let p = Block::Paragraph(Paragraph {
            props: ParProps {
                style_id: Some("TOC1".to_string()),
                ..Default::default()
            },
            content: vec![
                run("Title", RunProps::default()),
                Inline::Tab,
                run("9", RunProps::default()),
            ],
        });
        let line = render(&doc(vec![p]), &o)[0].plain();
        assert!(line.starts_with("Title"), "{line:?}");
        assert!(line.contains("...."), "no dot leader: {line:?}");
        assert!(
            line.trim_end().ends_with('9'),
            "page number not right-aligned: {line:?}"
        );
    }

    #[test]
    fn direct_ppr_tabs_render_dot_leader() {
        // A deeper TOC entry (e.g. "1.1 Motivation") carries its tab stops on the
        // paragraph's own `pPr`, not its style. The direct stops must drive the
        // dot leader and right-aligned page number even with no style tabs.
        let p = Block::Paragraph(Paragraph {
            props: ParProps {
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
                ..Default::default()
            },
            content: vec![
                run("1.1", RunProps::default()),
                Inline::Tab,
                run("Motivation", RunProps::default()),
                Inline::Tab,
                run("9", RunProps::default()),
            ],
        });
        let line = render(&doc(vec![p]), &opts(50))[0].plain();
        assert!(line.contains("...."), "no dot leader: {line:?}");
        assert!(
            line.trim_end().ends_with('9'),
            "page number not right-aligned: {line:?}"
        );
    }

    #[test]
    fn toc_page_number_stays_on_one_line_with_invisibles() {
        // The trailing ¶ must not push a right-aligned page number onto the next
        // row when invisibles are shown.
        let p = Block::Paragraph(Paragraph {
            props: ParProps {
                tabs: vec![TabStop {
                    pos: 8630,
                    align: TabAlign::Right,
                    leader: TabLeader::Dot,
                }],
                ..Default::default()
            },
            content: vec![
                run("1 Introduction", RunProps::default()),
                Inline::Tab,
                run("9", RunProps::default()),
            ],
        });
        let mut o = opts(60);
        o.show_invisibles = true;
        let lines = render(&doc(vec![p]), &o);
        assert_eq!(lines.len(), 1, "page number wrapped to a second line");
        let plain = lines[0].plain();
        // Both the number and the pilcrow sit on the one line.
        assert!(plain.contains('9') && plain.contains('¶'), "{plain:?}");
    }

    #[test]
    fn smartart_renders_node_text_in_box() {
        // A diagram's shapes can't be drawn in a terminal, so its node text shows
        // in a labeled box.
        let sa = Inline::SmartArt {
            raw: "<w:r><w:drawing/></w:r>".to_string(),
            text: vec!["Plan".to_string(), "Build".to_string(), "Ship".to_string()],
        };
        let joined: String = render(&doc(vec![para(vec![sa])]), &opts(40))
            .iter()
            .map(|l| l.plain())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("SmartArt"), "missing caption:\n{joined}");
        assert!(
            joined.contains("Plan") && joined.contains("Build") && joined.contains("Ship"),
            "missing node text:\n{joined}"
        );
        assert!(
            joined.contains('┌') && joined.contains('└'),
            "not boxed:\n{joined}"
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
    fn selection_highlights_after_a_section_break() {
        // In print layout each section is rendered from a 0-based slice, so a
        // selection keyed by an absolute paragraph path must still highlight a
        // paragraph that sits after a section break.
        let mut sect = ParProps::default();
        sect.section_break = Some("<w:sectPr/>".to_string());
        let first = Block::Paragraph(Paragraph {
            props: sect,
            content: vec![run("first section", RunProps::default())],
        });
        let second = para(vec![run("pick me", RunProps::default())]);
        let mut o = opts(50);
        o.page_view = true;
        o.selection = vec![(vec![1], 0, 7)]; // all of paragraph index 1
        let (lines, _maps, _imgs) = render_with_images(&doc(vec![first, second]), &o);
        let hit = lines
            .iter()
            .find(|l| l.plain().contains("pick me"))
            .expect("second section line");
        assert!(
            hit.spans.iter().any(|s| s.style.highlight),
            "selection should highlight across the section break"
        );
    }

    #[test]
    fn invisibles_keep_the_page_border_rigid() {
        // A table's row-end `¤` (and paragraph `¶`) must not spill past the text
        // width and push the page border out when invisibles are shown.
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let t = Table {
            grid: vec![100, 100],
            rows: vec![Row {
                cells: vec![cell("Apple"), cell("Banana")],
            }],
        };
        let d = doc(vec![
            Block::Table(t),
            para(vec![run("body text", RunProps::default())]),
        ]);
        let mut o = opts(60);
        o.page_view = true;
        o.show_invisibles = true;
        let right_borders: Vec<usize> = render(&d, &o)
            .iter()
            .filter_map(|l| {
                let s = l.plain();
                s.rfind('│').map(|b| s[..b].chars().count())
            })
            .collect();
        let first = right_borders[0];
        assert!(
            right_borders.iter().all(|&c| c == first),
            "page border not rigid: {right_borders:?}"
        );
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
    fn wide_table_overflows_the_page_instead_of_cramming() {
        // A 10-column table can't fit a small page width; in page-view it should
        // widen and extend past the right border rather than cram to 1-cell cells.
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let mkrow = || Row {
            cells: (0..10).map(|i| cell(&format!("c{i}"))).collect(),
        };
        let t = Table {
            grid: vec![100; 10],
            rows: vec![mkrow(), mkrow()],
        };
        let d = doc(vec![Block::Table(t)]);
        let o = RenderOptions {
            width: 100,
            page_view: true,
            ..RenderOptions::default()
        };
        let (lines, _) = render_mapped(&d, &o);
        let content_cols = page_metrics(o.width, o.page).content_cols;
        // some table row must be wider than the page content area (it overflows)
        let widest = lines.iter().map(|l| l.width()).max().unwrap();
        assert!(
            widest > content_cols + 2,
            "table did not overflow: widest {widest}, content {content_cols}"
        );
        // and every column label still shows in full (not crammed/clipped away)
        let joined = lines.iter().map(|l| l.plain()).collect::<String>();
        for i in 0..10 {
            assert!(joined.contains(&format!("c{i}")), "missing c{i}");
        }
    }

    #[test]
    fn nested_table_too_wide_does_not_break_the_outer_grid() {
        // A bordered nested table needs more cells than the narrow outer cell can
        // give; it must be clipped to the cell, not overflow and shift the grid.
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let nested = Table {
            grid: vec![100, 100],
            rows: vec![Row {
                cells: vec![cell("a"), cell("b")],
            }],
        };
        let nested_cell = Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![Block::Table(nested)],
        };
        let t = Table {
            grid: vec![100, 100, 100],
            rows: vec![Row {
                cells: vec![cell("X"), nested_cell, cell("Y")],
            }],
        };
        let d = doc(vec![Block::Table(t)]);
        let lines = render(&d, &opts(40));
        let first = lines[0].width();
        for (i, l) in lines.iter().enumerate() {
            assert_eq!(l.width(), first, "row {i} width differs: {:?}", l.plain());
        }
    }

    #[test]
    fn narrow_table_keeps_borders_aligned() {
        // At a small width the columns are < 4 cells; cell text must wrap to the
        // column instead of overflowing it (which used to break the borders).
        let cell = |s: &str| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![para(vec![run(s, RunProps::default())])],
        };
        let t = Table {
            grid: vec![100, 100, 100, 100],
            rows: vec![
                Row {
                    cells: vec![
                        cell("Release"),
                        cell("Disk"),
                        cell("Media"),
                        cell("Product"),
                    ],
                },
                Row {
                    cells: vec![
                        cell("09/29/95"),
                        cell("Disk1"),
                        cell("1.44mb"),
                        cell("Office"),
                    ],
                },
            ],
        };
        let d = doc(vec![Block::Table(t)]);
        let lines = render(&d, &opts(24));
        let first = lines[0].width();
        for (i, l) in lines.iter().enumerate() {
            assert_eq!(l.width(), first, "row {i} width differs: {:?}", l.plain());
        }
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

    #[test]
    fn small_image_flows_inline_not_as_block() {
        // A short, wide equation (~114×24px) is handled inline, so the block-box
        // sizer is never asked to magnify it.
        let raw = "<w:r><w:pict><v:shape style=\"width:85.5pt;height:18pt\">\
                   <v:imagedata r:id=\"r\"/></v:shape></w:pict></w:r>";
        let img = inline_image(raw).expect("small image flows inline");
        assert!(img.rows <= 3, "inline image too tall: {}", img.rows);
        assert!(img.cols > img.rows, "one-line formula stays wide");
    }

    #[test]
    fn large_image_keeps_natural_cell_size() {
        // A comfortably-sized image (320×160px) is left at its natural projection.
        assert_eq!(image_box_cells(320, 160, 90), (40, 10));
        assert!(
            inline_image(
                "<w:r><w:drawing><wp:inline><wp:extent cx=\"3048000\" cy=\"1524000\"/>\
             </wp:inline></w:drawing></w:r>"
            )
            .is_none()
        );
    }

    /// `n` blank lines, all tagged as image-placeholder lines.
    fn image_pairs(n: usize) -> Vec<(Line, LineMap)> {
        (0..n)
            .map(|_| {
                (
                    Line { spans: Vec::new() },
                    LineMap {
                        image: true,
                        ..LineMap::default()
                    },
                )
            })
            .collect()
    }

    #[test]
    fn tall_image_is_split_into_tiling_slices() {
        let o = opts(80);
        let geom = o.page;
        let cr = page_metrics(o.width, geom).content_rows;
        let pl = page_lines(&o);
        let n = cr + 3; // taller than a single page → must be cut
        let mut imgs = vec![ImageBox {
            rid: "r".to_string(),
            row: 0,
            col: 0,
            cols: 5,
            rows: n,
            src_row: 0,
            full_rows: n,
            bordered: false,
            label: String::new(),
        }];
        paginate(image_pairs(n), &o, geom, &mut imgs, &pl);

        assert!(imgs.len() >= 2, "tall image should be cut across pages");
        assert!(imgs.iter().all(|i| i.rid == "r" && i.full_rows == n));
        // Slices tile the image top-to-bottom with no gaps or overlap.
        let mut acc = 0;
        for s in &imgs {
            assert_eq!(s.src_row, acc, "slice not contiguous: {imgs:?}");
            acc += s.rows;
        }
        assert_eq!(acc, n, "slices must cover the whole image");
    }

    #[test]
    fn page_sized_image_moves_to_next_page_intact() {
        let o = opts(80);
        let geom = o.page;
        let cr = page_metrics(o.width, geom).content_rows;
        assert!(cr >= 4, "test needs a few content rows (got {cr})");
        let pl = page_lines(&o);
        let img_h = cr - 1; // fits on a page, but not after the leading text
        let mut pairs = vec![
            (
                Line {
                    spans: vec![Line::dim_span("x".to_string())],
                },
                LineMap::default(),
            ),
            (
                Line {
                    spans: vec![Line::dim_span("y".to_string())],
                },
                LineMap::default(),
            ),
        ];
        pairs.extend(image_pairs(img_h));
        let mut imgs = vec![ImageBox {
            rid: "r".to_string(),
            row: 2,
            col: 0,
            cols: 5,
            rows: img_h,
            src_row: 0,
            full_rows: img_h,
            bordered: false,
            label: String::new(),
        }];
        paginate(pairs, &o, geom, &mut imgs, &pl);

        // Kept whole (one slice) rather than cut at the page boundary.
        assert_eq!(imgs.len(), 1, "image should not be split: {imgs:?}");
        assert_eq!(imgs[0].rows, img_h);
        assert_eq!(imgs[0].src_row, 0);
    }
}
